//! Router › Cell Lock, Smart Tower Connect and Signal Detection (iOS
//! `CellLockView`, `STCView`, `SignalDetectView`).
//!
//! - **Cell Lock**: "Current Cell" from the `signal` reading at page load
//!   (the signal page's keys: `nr5g_pci`, `nr5g_action_channel`,
//!   `nr5g_action_band`, `lte_pci`, `wan_active_channel`; the app reads
//!   keys the firmware does not send), "Locked" from `cell_lock_status`
//!   ("—" when the reading has none, and then "Unlock Cell" is offered:
//!   the app hides it unless it saw a lock); right after a lock or an
//!   unlock, what it did (`?locked=1` / `0`, as the app sets its state).
//!   A failed read says "Failed to load cell info: …" (the app's). "Lock NR" / "Lock LTE" are GET
//!   forms of their questions (a lock to a cell that cannot be used cuts
//!   service; the app asks nothing): PCI 0–1007 (LTE 0–503), NR-ARFCN
//!   0–3279165 (EARFCN 0–262143), band 1–1024 (optional), else the form
//!   again with a message per field. The bodies use the firmware's names
//!   (`ubus -v list`: `lock_nr_pci`, `lock_nr_earfcn`, `lock_nr_cell_band`,
//!   `lock_lte_pci`, `lock_lte_earfcn`; the app sends `pci`/`earfcn`/`band`,
//!   which the firmware's handler does not name). "Scan Neighbors"
//!   (single-use token) starts the scan, waits 3 s as the app, and leads to
//!   `?nbr=1`, which reads both neighbour lists ("Found N neighbor
//!   cell(s)" / "No neighbors found").
//! - **Smart Tower Connect**: "Status", "Parameters" (whole numbers 0–99999,
//!   sent as the firmware's `stc_*` names) and "Controls" (Enable, Disable,
//!   Reset Whitelist, each asked first: the web's addition). Right after
//!   one, the state it set (`?stc=1` / `0`, as the app); a state that
//!   cannot be read is "Unavailable" (the app shows "Disabled").
//! - **Signal Detection**: the `signal_detect` topic (progress, and the
//!   results once a sweep ends), streamed only while a sweep runs; "Start
//!   Signal Detection" and "Stop Detection" carry single-use tokens. The
//!   page's watch point (`since`: the start, or `stop`: the stop, boot
//!   clock in ms) is its `arg`: a 100 % reading within 6 s of the start is
//!   the previous sweep's (the app's rule), a started sweep counts as
//!   running below 100 % for at most 10 minutes, and results show only
//!   after a sweep this page started, stopped or found running (a fresh
//!   page shows no old one, as the app's; one it re-attaches to ends with
//!   "Detection complete" and its results, as the app's). For 6 s after
//!   Stop the device's running flag is not believed (the app sets its own).

use std::time::Duration;

use maud::{html, Markup};
use serde_json::{json, Value};

use super::confirm::{asking, Bad, Input, Kind, Next, Question};
use crate::boottime::BootInstant;
use crate::events::Topic;
use crate::handlers::AppState;
use crate::web::action::Valid;
use crate::web::kit::{self, Icon};
use crate::web::render::{self, once_field, Fragment, Views};
use crate::web::screen::{FormState, PageData};
use crate::web::session::{percent_encode, query_param};
use crate::web::store::Claim;
use crate::web::fmt;

pub const CELL: &str = "/ui/router/cell-lock";
pub const LOCK_NR: &str = "/ui/router/cell-lock/nr";
pub const LOCK_LTE: &str = "/ui/router/cell-lock/lte";
pub const UNLOCK: &str = "/ui/router/cell-lock/reset";
pub const NEIGHBORS: &str = "/ui/router/cell-lock/scan";
pub const STC: &str = "/ui/router/stc";
pub const STC_PARAMS: &str = "/ui/router/stc/params";
pub const STC_ON: &str = "/ui/router/stc/enable";
pub const STC_OFF: &str = "/ui/router/stc/disable";
pub const STC_RESET: &str = "/ui/router/stc/reset";
pub const SD: &str = "/ui/router/signal-detect";
pub const SD_START: &str = "/ui/router/signal-detect/start";
pub const SD_STOP: &str = "/ui/router/signal-detect/stop";

/// A firmware value as text (strings and numbers).
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The app's `asBool`: "1", "true", "on" or a number other than 0.
fn truthy(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => Some(n.as_f64().is_some_and(|f| f != 0.0)),
        Value::String(s) => Some(matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on")),
        _ => None,
    }
}

fn dash(s: String) -> String {
    if s.is_empty() {
        "\u{2014}".into()
    } else {
        s
    }
}

/// A row of the app's text fields: the label is the placeholder (as the
/// app's), and for screen readers the label; shown above the field too
/// when the field starts filled (`shown`: STC's, whose values would
/// otherwise stand unnamed).
fn field(name: &str, label: &str, value: &str, err: Option<&str>, required: bool, shown: bool) -> Markup {
    let err_id = format!("{name}-err");
    html! {
        li.it.fld {
            label.sr[!shown] for=(name) { (label) }
            input #(name) type="text" name=(name) value=(value) placeholder=(label) inputmode="numeric" autocomplete="off"
                maxlength="12" required[required] aria-invalid=[err.map(|_| "true")] aria-describedby=[err.map(|_| err_id.as_str())];
            @if let Some(e) = err { p.ferr id=(err_id) { (e) } }
        }
    }
}

/// A field's value: as typed after a refused post, else as the question's
/// Cancel brought it back (`?nr_pci=…`), else empty.
fn typed(pd: &PageData, name: &str) -> String {
    pd.form
        .and_then(|f| f.value(name).map(str::to_owned))
        .or_else(|| pd.param(name).filter(|v| v.len() <= 12 && v.bytes().all(|b| b.is_ascii_digit())))
        .unwrap_or_default()
}

// --- Cell Lock ---

/// With `?nbr=1` (the scan's way back): both neighbour lists (`cell_list`
/// of `pci`, `earfcn`, `band`, `rsrp`), NR first; a list that cannot be read
/// adds nothing (as the app's).
pub fn neighbors(app: &AppState, query: Option<&str>) -> Result<Value, String> {
    if query_param(query, "nbr").as_deref() != Some("1") {
        return Ok(Value::Null);
    }
    let mut cells = Vec::new();
    for (kind, path) in [("NR", "/api/cell/neighbors/nr"), ("LTE", "/api/cell/neighbors/lte")] {
        if let Some(list) = super::api_get(app, path).ok().and_then(|d| d["cell_list"].as_array().cloned()) {
            for c in list {
                cells.push(json!({"type": kind, "pci": text(&c["pci"]), "earfcn": text(&c["earfcn"]), "band": text(&c["band"]), "rsrp": text(&c["rsrp"])}));
            }
        }
    }
    Ok(Value::Array(cells))
}

pub fn cell_lock(pd: &PageData) -> Markup {
    let sig = pd.views.value(Topic::Signal);
    let v = |k: &str| dash(sig.map(|s| text(&s[k])).unwrap_or_default());
    // What a lock or an unlock just did, else the reading.
    let set = pd.param("locked").and_then(|l| match l.as_str() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    });
    let locked = set.or_else(|| sig.and_then(|s| truthy(&s["cell_lock_status"])));
    let failed = pd.views.slot(Topic::Signal).and_then(|s| s.error.as_deref());
    let found = pd.read(0).and_then(Value::as_array);
    let err = |k: &str| pd.form.and_then(|f| f.error(k));
    html! {
        @if let Some(cells) = found {
            @if cells.is_empty() {
                p.msg.ok role="status" { "No neighbors found" }
            } @else {
                p.msg.ok role="status" { "Found " (cells.len()) " neighbor cell(s)" }
            }
        }
        @if let Some(e) = failed { p.msg.err role="alert" { "Failed to load cell info: " (fmt::truncate(e, 120)) } }
        (kit::section(Some("Current Cell"), None, html! {
            (kit::kv_row(None, "NR PCI", &v("nr5g_pci")))
            (kit::kv_row(None, "NR EARFCN", &v("nr5g_action_channel")))
            (kit::kv_row(None, "NR Band", &v("nr5g_action_band")))
            (kit::kv_row(None, "LTE PCI", &v("lte_pci")))
            (kit::kv_row(None, "LTE EARFCN", &v("wan_active_channel")))
            li.it {
                span { "Locked" }
                @match locked {
                    Some(true) => span.v.c-orange { "Yes" },
                    Some(false) => span.v.c-green { "No" },
                    None => span.v { "\u{2014}" },
                }
            }
        }))
        form.rf.req method="get" action=(LOCK_NR) {
            (kit::section(Some("Lock NR Cell"), None, html! {
                (field("nr_pci", "PCI", &typed(pd, "nr_pci"), err("nr_pci"), true, false))
                (field("nr_earfcn", "EARFCN", &typed(pd, "nr_earfcn"), err("nr_earfcn"), true, false))
                (field("nr_band", "Band (optional)", &typed(pd, "nr_band"), err("nr_band"), false, false))
                li { button.it.cv.snd type="submit" { "Lock NR" } }
            }))
        }
        form.rf.req method="get" action=(LOCK_LTE) {
            (kit::section(Some("Lock LTE Cell"), None, html! {
                (field("lte_pci", "PCI", &typed(pd, "lte_pci"), err("lte_pci"), true, false))
                (field("lte_earfcn", "EARFCN", &typed(pd, "lte_earfcn"), err("lte_earfcn"), true, false))
                li { button.it.cv.snd type="submit" { "Lock LTE" } }
            }))
        }
        (kit::section(Some("Neighbor Scan"), None, html! {
            li {
                form.sf method="post" action=(NEIGHBORS) {
                    (pd.csrf())
                    (once_field(&pd.once()))
                    input type="hidden" name="confirm" value="1";
                    button.it.cv.bt type="submit" { "Scan Neighbors" }
                }
            }
            @for c in found.into_iter().flatten() {
                @let f = |k: &str| c[k].as_str().unwrap_or("").to_owned();
                li.it.nb {
                    span.cvb {
                        span.l1 { b { (f("type")) } span.muted { "Band " (f("band")) } }
                        small { "PCI: " (f("pci")) "\u{a0}\u{a0}EARFCN: " (f("earfcn")) "\u{a0}\u{a0}RSRP: " (f("rsrp")) }
                    }
                }
            }
        }))
        @if locked != Some(false) {
            (kit::section(None, None, html! { li { a.it.bt.danger href=(UNLOCK) { "Unlock Cell" } } }))
        }
    }
}

/// A whole number in `range` typed into `name`, or its message.
fn int_field(pairs: &[(String, String)], name: &'static str, range: std::ops::RangeInclusive<u32>, required: bool, fs: &mut FormState) -> String {
    let raw = pairs.iter().find(|(k, _)| k == name).map_or("", |(_, v)| v.as_str()).trim();
    fs.values.push((name, raw.chars().take(12).collect()));
    if raw.is_empty() {
        if required {
            fs.errors.push((name, "PCI and EARFCN are required".into()));
        }
        return String::new();
    }
    match raw.parse::<u32>().ok().filter(|n| raw.bytes().all(|b| b.is_ascii_digit()) && range.contains(n)) {
        Some(n) => n.to_string(),
        None => {
            fs.errors.push((name, format!("Enter a whole number from {} to {}.", range.start(), range.end())));
            String::new()
        }
    }
}

pub fn parse(kind: Kind, pairs: &[(String, String)]) -> Result<Vec<(&'static str, String)>, Bad> {
    let mut fs = FormState::default();
    let vals = match kind {
        Kind::CellNr => vec![
            ("nr_pci", int_field(pairs, "nr_pci", 0..=1007, true, &mut fs)),
            ("nr_earfcn", int_field(pairs, "nr_earfcn", 0..=3_279_165, true, &mut fs)),
            ("nr_band", int_field(pairs, "nr_band", 1..=1024, false, &mut fs)),
        ],
        Kind::CellLte => vec![
            ("lte_pci", int_field(pairs, "lte_pci", 0..=503, true, &mut fs)),
            ("lte_earfcn", int_field(pairs, "lte_earfcn", 0..=262_143, true, &mut fs)),
        ],
        _ => Vec::new(),
    };
    if fs.errors.is_empty() {
        Ok(vals.into_iter().filter(|(_, v)| !v.is_empty()).collect())
    } else {
        Err(Bad::Fields(fs))
    }
}

pub fn question(kind: Kind, i: &Input, remote: bool) -> Option<Question> {
    let band = if i.val("nr_band").is_empty() { String::new() } else { format!(", band n{}", i.val("nr_band")) };
    let rereg = "It may re-register, which drops the connection for a moment.";
    Some(match kind {
        Kind::CellNr => asking(
            "Lock this NR cell?",
            vec![
                format!("The modem will use only the 5G NR cell with PCI {} on NR-ARFCN {}{band}.", i.val("nr_pci"), i.val("nr_earfcn")),
                "If that cell can't be used here, the router can lose 5G, or all service, until you unlock the cell.".into(),
            ],
            "Lock NR",
            remote,
            true,
        ),
        Kind::CellLte => asking(
            "Lock this LTE cell?",
            vec![
                format!("The modem will use only the LTE cell with PCI {} on EARFCN {}.", i.val("lte_pci"), i.val("lte_earfcn")),
                "If that cell can't be used here, the router can lose its connection until you unlock the cell.".into(),
            ],
            "Lock LTE",
            remote,
            true,
        ),
        Kind::CellReset => asking(
            "Unlock the cell?",
            vec![
                format!("The modem may use any cell again. {rereg}"),
                "The firmware resets band and cell settings together, so band locks may be undone too.".into(),
            ],
            "Unlock Cell",
            remote,
            true,
        ),
        Kind::StcOn => asking(
            "Turn on Smart Tower Connect?",
            vec!["The modem keeps to the cells on its whitelist. If those cells can't be used, the router can lose service until you turn it off.".into()],
            "Enable STC",
            remote,
            true,
        ),
        Kind::StcOff => asking("Turn off Smart Tower Connect?", vec![format!("The modem may use any cell again. {rereg}")], "Disable STC", remote, true),
        Kind::StcReset => asking(
            "Reset the STC whitelist?",
            vec!["The collected cells are removed and Smart Tower Connect turns off.".into(), "The modem may re-register, which drops the connection for a moment.".into()],
            "Reset Whitelist",
            remote,
            true,
        ),
        _ => return None,
    })
}

/// The pause after a neighbour scan before its lists are read (the app's).
const NEIGHBOR_WAIT: Duration = if cfg!(test) { Duration::from_millis(10) } else { Duration::from_secs(3) };

pub fn send(app: &AppState, kind: Kind, i: &Input, claim: &mut Claim) -> Result<(), (u16, String)> {
    let (method, path) = kind.call();
    let body = match kind {
        Kind::CellNr => {
            let mut b = json!({"lock_nr_pci": i.val("nr_pci"), "lock_nr_earfcn": i.val("nr_earfcn")});
            if !i.val("nr_band").is_empty() {
                b["lock_nr_cell_band"] = i.val("nr_band").into();
            }
            b
        }
        Kind::CellLte => json!({"lock_lte_pci": i.val("lte_pci"), "lock_lte_earfcn": i.val("lte_earfcn")}),
        _ => json!({}),
    };
    claim.disarm();
    let prefix = match kind {
        Kind::Neighbors => "Scan failed: ",
        Kind::Detect => "Failed to start: ",
        Kind::DetectStop => "Failed to stop: ",
        _ => "Failed: ",
    };
    super::call(app, method, path, &body).map_err(|(status, e)| (status, format!("{prefix}{}", fmt::truncate(&e, 120))))?;
    if kind == Kind::Neighbors {
        std::thread::sleep(NEIGHBOR_WAIT);
    }
    Ok(())
}

pub fn back(kind: Kind, i: &Input) -> String {
    match kind {
        Kind::CellNr | Kind::CellLte => {
            let q: Vec<String> = i.vals.iter().map(|(k, v)| format!("{k}={}", percent_encode(v))).collect();
            if q.is_empty() {
                CELL.into()
            } else {
                format!("{CELL}?{}", q.join("&"))
            }
        }
        Kind::CellReset | Kind::Neighbors => CELL.into(),
        Kind::StcOn | Kind::StcOff | Kind::StcReset => STC.into(),
        _ => SD.into(),
    }
}

pub fn next(kind: Kind, i: &Input, r: Result<(), &str>, now: BootInstant) -> Next {
    let sep = |to: String| if to.contains('?') { format!("{to}&") } else { format!("{to}?") };
    let ok = match kind {
        Kind::CellNr => "NR cell locked",
        Kind::CellLte => "LTE cell locked",
        Kind::CellReset => "Cell lock reset",
        Kind::StcOn => "STC enabled",
        Kind::StcOff => "STC disabled",
        Kind::StcReset => "STC whitelist reset",
        Kind::Detect => "Detection started",
        _ => "Detection stopped",
    };
    match (kind, r) {
        (_, Err(msg)) => Next::Line(back(kind, i), Some(false), msg.to_owned()),
        (Kind::Neighbors, Ok(())) => Next::To(format!("{CELL}?nbr=1")),
        // The page watches the sweep from its start, or from the stop.
        (Kind::Detect, Ok(())) => Next::Line(format!("{SD}?since={:x}", now.as_millis().max(1)), Some(true), ok.into()),
        (Kind::DetectStop, Ok(())) => Next::Line(format!("{SD}?stop={:x}", now.as_millis().max(1)), Some(true), ok.into()),
        // The state it set, as the app's (`?locked=`, `?stc=`).
        (Kind::CellNr | Kind::CellLte, Ok(())) => Next::Line(format!("{}locked=1", sep(back(kind, i))), Some(true), ok.into()),
        (Kind::CellReset, Ok(())) => Next::Line(format!("{CELL}?locked=0"), Some(true), ok.into()),
        (Kind::StcOn, Ok(())) => Next::Line(format!("{STC}?stc=1"), Some(true), ok.into()),
        (_, Ok(())) => Next::Line(format!("{STC}?stc=0"), Some(true), ok.into()),
    }
}

// --- Smart Tower Connect ---

/// The parameters' fields: (field, label, the firmware's name, the app's).
pub const STC_FIELDS: [(&str, &str, &str, &str); 4] = [
    ("lte_timer", "LTE Collect Timer", "stc_lte_collect_timer", "lte_collect_timer"),
    ("nrsa_timer", "NRSA Collect Timer", "stc_nrsa_collect_timer", "nrsa_collect_timer"),
    ("lte_max", "LTE Whitelist Max", "stc_lte_white_list_num_max", "lte_whitelist_max"),
    ("nrsa_max", "NRSA Whitelist Max", "stc_nrsa_white_list_num_max", "nrsa_whitelist_max"),
];

/// `PUT /api/cell/stc/params`: the four whole numbers, as strings under the
/// firmware's names.
pub fn stc_body(v: &Valid, _: &[Value]) -> Option<Value> {
    let mut b = serde_json::Map::new();
    for (field, _, fw, _) in STC_FIELDS {
        b.insert(fw.into(), v.get(field).into());
    }
    Some(Value::Object(b))
}

pub fn stc(pd: &PageData) -> Markup {
    let params = pd.read(0);
    let status = pd.read(1);
    let read = [params.map(|p| truthy(&p["stc_enable"])), status.map(|s| truthy(&s["stc_enable"])), status.map(|s| truthy(&s["status"]))]
        .into_iter()
        .flatten()
        .flatten()
        .reduce(|a, b| a || b);
    // What a switch or a reset just did, else the reading.
    let set = pd.param("stc").and_then(|v| match v.as_str() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    });
    let enabled = set.or(read);
    let saved = |fw: &str, app: &str| params.map(|p| Some(text(&p[fw])).filter(|s| !s.is_empty()).unwrap_or_else(|| text(&p[app]))).unwrap_or_default();
    let ctl = |label: &'static str, href: &'static str, on: bool, danger: bool| {
        html! {
            li {
                @if on {
                    a.it.bt.danger[danger] href=(href) { (label) }
                } @else {
                    span.it.bt.off aria-disabled="true" { (label) }
                }
            }
        }
    };
    html! {
        @if let Some(Err(e)) = pd.reads.first() {
            p.msg.err role="alert" { "Failed to load STC: " (fmt::truncate(e, 120)) }
        }
        (kit::section(Some("Status"), None, html! {
            li.it {
                span { "STC" }
                @match enabled {
                    Some(true) => span.v.c-green { "Enabled" },
                    Some(false) => span.v { "Disabled" },
                    None => span.v { "Unavailable" },
                }
            }
        }))
        form.rf.req method="post" action=(STC_PARAMS) {
            (pd.csrf())
            (kit::section(Some("Parameters"), None, html! {
                @for (name, label, fw, app) in STC_FIELDS {
                    @let value = pd.form.and_then(|f| f.value(name)).map_or_else(|| saved(fw, app), str::to_owned);
                    (field(name, label, &value, pd.form.and_then(|f| f.error(name)), true, true))
                }
                li { button.it.cv.snd type="submit" { "Apply Parameters" } }
            }))
        }
        (kit::section(Some("Controls"), None, html! {
            (ctl("Enable STC", STC_ON, enabled != Some(true), false))
            (ctl("Disable STC", STC_OFF, enabled != Some(false), false))
            (ctl("Reset Whitelist", STC_RESET, true, true))
        }))
    }
}

// --- Signal Detection ---

const T: &[Topic] = &[Topic::SignalDetect];

pub const FRAGMENTS: [Fragment; 3] = [
    Fragment { id: "sd-msg", topics: T, render: message },
    Fragment { id: "sd-run", topics: T, render: controls },
    Fragment { id: "sd-res", topics: T, render: results },
];

/// A 100 % reading this soon after the start is the previous sweep's.
const GRACE: Duration = Duration::from_secs(6);
/// A started sweep counts as running below 100 % for at most this long.
const WATCH: Duration = Duration::from_secs(10 * 60);

/// The watch point (`Screen::arg`): the start (`since`) or the stop
/// (`stop`, plus 1), boot clock in ms, shifted left; a time still to come
/// (an old URL after a reboot) counts as none. A fresh page that finds a
/// sweep running re-attaches to it: watched as if started 6 s ago, so its
/// 100 % is believed at once (the app's `resumeIfRunning`); else 0.
pub fn sd_arg(v: &Views, query: Option<&str>) -> u64 {
    let now = v.now.as_millis();
    let at = |k: &str| query_param(query, k).filter(|s| s.len() <= 12).and_then(|s| u64::from_str_radix(&s, 16).ok()).filter(|t| *t <= now);
    if query_param(query, "stop").is_some() {
        return at("stop").unwrap_or(0) << 1 | 1;
    }
    match at("since") {
        Some(t) if t > 0 => t << 1,
        _ if reading_sweeps(v) => now.saturating_sub(GRACE.as_millis() as u64).max(1) << 1,
        _ => 0,
    }
}

/// The device reports a sweep below 100 %.
fn reading_sweeps(v: &Views) -> bool {
    let (pct, flag) = reading(v);
    flag && pct < 100
}

/// The progress reading: its percentage and the device's running flag.
fn reading(v: &Views) -> (u64, bool) {
    let p = v.value(Topic::SignalDetect).map(|d| &d["progress"]);
    let pct = p.and_then(|p| fmt::int(&p["progress"])).unwrap_or(0).clamp(0, 100) as u64;
    (pct, p.and_then(|p| truthy(&p["running"])).unwrap_or(false))
}

/// Since this page's watch point (`None`: it has none).
fn since_start(v: &Views) -> Option<Duration> {
    let started = v.arg >> 1;
    (started > 0).then(|| Duration::from_millis(v.now.as_millis().saturating_sub(started)))
}

/// The page Stop led to.
fn stopped(v: &Views) -> bool {
    v.arg & 1 == 1
}

/// A sweep runs: the device says so below 100 % (the app re-attaching;
/// not within 6 s of a Stop, which the app takes as stopped), or this page
/// started one: within 6 s of its start, or below 100 % for 10 minutes
/// (the app's polling).
pub fn running(v: &Views) -> bool {
    let (pct, _) = reading(v);
    let age = since_start(v);
    let device = reading_sweeps(v) && !(stopped(v) && age.is_some_and(|a| a < GRACE));
    let ours = age.filter(|_| !stopped(v));
    device || ours.is_some_and(|age| age < GRACE || (pct < 100 && age < WATCH))
}

/// Without JavaScript, reload every 3 s while a sweep runs, keeping the
/// result line, the watch point and the page's unused token (`t`).
pub fn sd_refresh(pd: &PageData) -> Option<String> {
    running(pd.views).then(|| {
        let hex = |v: &String| (1..=16).contains(&v.len()) && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        let mut url = format!("{SD}?");
        if let Some(r) = pd.param("r").filter(hex) {
            url += &format!("r={r}&");
        }
        let token = pd.once();
        if !token.is_empty() {
            url += &format!("t={token}&");
        }
        let point = if stopped(pd.views) { "stop" } else { "since" };
        format!("{url}{point}={:x}", pd.views.arg >> 1)
    })
}

/// `#sd-msg`: "Detection complete" once this page's sweep reached 100 %.
fn message(v: &Views) -> Markup {
    let done = since_start(v).is_some() && !stopped(v) && !running(v) && reading(v).0 >= 100;
    html! {
        div #sd-msg {
            @if done { p.msg.ok role="status" { "Detection complete" } }
        }
    }
}

/// `#sd-run`: "Progress", "n%" and its bar while a sweep runs (class
/// `run`, which shows "Stop Detection" instead of Start); else empty.
fn controls(v: &Views) -> Markup {
    if !running(v) {
        return html! { li #sd-run .it {} };
    }
    // The previous sweep's 100 % during the grace: this one's 0 %.
    let pct = Some(reading(v).0).filter(|p| *p < 100).unwrap_or(0);
    html! {
        li #sd-run .it .fld .run {
            span.row { span { "Progress" } span.muted.num { (pct) "%" } }
            progress max="100" value=(pct) aria-label="Detection progress" {}
        }
    }
}

/// `#sd-res`: "Results" after a sweep this page started or stopped (the
/// app shows its own sweep's only), when there are any.
fn results(v: &Views) -> Markup {
    let list = v.value(Topic::SignalDetect).and_then(|d| d["results"]["record_list"].as_array()).filter(|l| !l.is_empty());
    let Some(list) = list.filter(|_| v.arg != 0 && !running(v)) else {
        return html! { div #sd-res {} };
    };
    html! {
        div #sd-res {
            (kit::section(Some("Results"), None, html! {
                @for r in list {
                    @let f = |k: &str| text(&r[k]);
                    li.it.nb {
                        span.cvb {
                            span.l1 { b { (f("type")) } span.muted { "Band " (f("band")) } }
                            span.sq {
                                span { (kit::icon(Icon::Antenna)) span.sr { "RSRP " } (f("rsrp")) }
                                span { (kit::icon(Icon::Waveform)) span.sr { "SINR " } (f("sinr")) }
                            }
                            small { "PCI: " (f("pci")) "\u{a0}\u{a0}EARFCN: " (f("earfcn")) }
                        }
                    }
                }
            }))
        }
    }
}

pub fn signal_detect(pd: &PageData) -> Markup {
    let v = pd.views;
    // One token for both forms (CSS shows one): each post leads to a new page.
    let token = pd.once();
    html! {
        (render::live_snapshot(SD))
        (render::error_line(v, T))
        (message(v))
        @if let Some(n) = &pd.notice { (n) }
        section.group {
            h2 { "Controls" }
            ul.list {
                (controls(v))
                li.go {
                    form.sf method="post" action=(SD_START) {
                        (pd.csrf())
                        (once_field(&token))
                        input type="hidden" name="confirm" value="1";
                        button.it.cv.snd type="submit" { "Start Signal Detection" }
                    }
                }
                li.halt {
                    form.sf method="post" action=(SD_STOP) {
                        (pd.csrf())
                        (once_field(&token))
                        input type="hidden" name="confirm" value="1";
                        button.it.cv.snd.danger type="submit" { "Stop Detection" }
                    }
                }
            }
        }
        (results(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::hub::SlotView;
    use std::sync::Arc;

    fn views(d: Value, now_ms: u64, arg: u64) -> Views {
        let now = BootInstant::from_duration(Duration::from_millis(now_ms));
        let slot = SlotView { version: 1, at: now, value_at: now, value: Some(Arc::new(d)), error: None };
        let mut v = Views::new(now, vec![(Topic::SignalDetect, slot)]);
        v.arg = arg;
        v
    }

    fn sweep(pct: Value, running: Value) -> Value {
        json!({"progress": {"progress": pct, "running": running}, "results": {"record_list": [
            {"type": "LTE", "band": "3", "earfcn": "1300", "pci": "178", "rsrp": "-90", "rsrq": "-9", "sinr": "12"},
        ]}})
    }

    /// The app's rules: re-attach to a sweep the device runs; a page's own
    /// sweep runs below 100 %, a 100 % within 6 s of its start is the old
    /// one's; results and "Detection complete" only for a watched sweep.
    #[test]
    fn a_sweep_runs_as_the_app_sees_it() {
        let start = 100_000u64;
        let ours = start << 1;
        // Fresh page: only the device's flag counts.
        assert!(running(&views(sweep(json!(40), json!(1)), start, 0)));
        assert!(!running(&views(sweep(json!(40), json!(0)), start, 0)));
        assert!(!running(&views(sweep(json!(100), json!(1)), start, 0)));
        // Our sweep: the old 100 % for 6 s, then believed.
        assert!(running(&views(sweep(json!("100"), json!(0)), start + 5_999, ours)));
        assert!(!running(&views(sweep(json!("100"), json!(0)), start + 6_000, ours)));
        assert!(running(&views(sweep(json!("40"), json!(0)), start + 60_000, ours)));
        assert!(!running(&views(sweep(json!("40"), json!(0)), start + 601_000, ours)));
        let v = views(sweep(json!(100), json!(0)), start + 8_000, ours);
        assert!(message(&v).into_string().contains("Detection complete"));
        let r = results(&v).into_string();
        for want in ["<b>LTE</b><span class=\"muted\">Band 3</span>", "-90", "12", "PCI: 178\u{a0}\u{a0}EARFCN: 1300"] {
            assert!(r.contains(want), "{want} in {r}");
        }
        // A fresh page: no results, no message; after Stop: results, no message.
        let fresh = views(sweep(json!(100), json!(0)), start, 0);
        assert_eq!((message(&fresh).into_string(), results(&fresh).into_string()), ("<div id=\"sd-msg\"></div>".into(), "<div id=\"sd-res\"></div>".into()));
        let stopped = views(sweep(json!(40), json!(0)), start, 1);
        assert!(results(&stopped).into_string().contains("Results") && !message(&stopped).into_string().contains("complete"));
        // Running: the bar (the old 100 % reads 0 %).
        let c = controls(&views(sweep(json!(100), json!(0)), start + 1_000, ours)).into_string();
        assert!(c.contains("class=\"it fld run\"") && c.contains("<progress max=\"100\" value=\"0\""), "{c}");
        assert_eq!(sd_arg(&fresh, Some("since=186a0&r=ab")), ours);
        assert_eq!(sd_arg(&fresh, Some("stop=1")), 3);
        assert_eq!(sd_arg(&fresh, Some("stop=186a0")), ours | 1);
        // A watch point still to come (an old URL after a reboot, or made
        // up) is none: the page does not run forever.
        assert_eq!(sd_arg(&fresh, Some("since=ffffffffffff")), 0);
        assert_eq!(sd_arg(&fresh, Some("stop=ffffffffffff")), 1);
    }

    /// A fresh page that finds a sweep running re-attaches to it, and its
    /// end shows "Detection complete" and the results (the app's
    /// `resumeIfRunning`); after Stop the device's flag waits 6 s.
    #[test]
    fn a_running_sweep_is_reattached_and_stop_is_believed() {
        let start = 100_000u64;
        let attached = sd_arg(&views(sweep(json!(40), json!(1)), start, 0), None);
        assert_eq!(attached, (start - 6_000) << 1);
        assert!(running(&views(sweep(json!(40), json!(1)), start, attached)));
        // Its 100 % is believed at once (no grace: the device's own counter).
        let end = views(sweep(json!(100), json!(0)), start + 2_000, attached);
        assert!(!running(&end) && message(&end).into_string().contains("Detection complete") && results(&end).into_string().contains("Results"));
        // Nothing running: no watch point.
        assert_eq!(sd_arg(&views(sweep(json!(100), json!(0)), start, 0), None), 0);
        assert_eq!(sd_arg(&views(sweep(json!(40), json!(0)), start, 0), None), 0);
        // Stop at `start`: the device's "running" is not believed for 6 s.
        let stop = start << 1 | 1;
        let just = views(sweep(json!(40), json!(1)), start + 2_000, stop);
        assert!(!running(&just) && results(&just).into_string().contains("Results") && !message(&just).into_string().contains("complete"));
        assert!(running(&views(sweep(json!(40), json!(1)), start + 6_000, stop)));
    }

    #[test]
    fn cell_fields_are_checked() {
        let p = |kind, q: &str| parse(kind, crate::web::session::Form::parse(q.as_bytes()).pairs());
        assert_eq!(p(Kind::CellNr, "nr_pci=774&nr_earfcn=513000&nr_band=").unwrap(), [("nr_pci", "774".to_owned()), ("nr_earfcn", "513000".to_owned())]);
        assert_eq!(p(Kind::CellLte, "lte_pci=0&lte_earfcn=262143").unwrap().len(), 2);
        let Err(Bad::Fields(fs)) = p(Kind::CellNr, "nr_pci=1008&nr_earfcn=&nr_band=x") else { panic!() };
        assert_eq!(fs.error("nr_pci"), Some("Enter a whole number from 0 to 1007."));
        assert_eq!(fs.error("nr_earfcn"), Some("PCI and EARFCN are required"));
        assert_eq!(fs.error("nr_band"), Some("Enter a whole number from 1 to 1024."));
        assert_eq!(fs.value("nr_pci"), Some("1008"));
        assert!(matches!(p(Kind::CellLte, "lte_pci=504&lte_earfcn=1"), Err(Bad::Fields(_))));
        let i = Input::of(&[("nr_pci", "774"), ("nr_earfcn", "513000"), ("nr_band", "41")]);
        assert_eq!(back(Kind::CellNr, &i), "/ui/router/cell-lock?nr_pci=774&nr_earfcn=513000&nr_band=41");
        let q = question(Kind::CellNr, &i, true).unwrap();
        assert_eq!(q.lines[0], "The modem will use only the 5G NR cell with PCI 774 on NR-ARFCN 513000, band n41.");
        assert_eq!(q.lines.last().unwrap(), super::super::confirm::REMOTE);
        assert!(question(Kind::Neighbors, &i, true).is_none() && question(Kind::Detect, &i, true).is_none());
    }

    #[test]
    fn stc_writes_the_firmware_names() {
        let v = Valid::of(&[("lte_timer", "30"), ("nrsa_timer", "40"), ("lte_max", "5"), ("nrsa_max", "6")]);
        assert_eq!(
            stc_body(&v, &[]).unwrap(),
            json!({"stc_lte_collect_timer": "30", "stc_nrsa_collect_timer": "40", "stc_lte_white_list_num_max": "5", "stc_nrsa_white_list_num_max": "6"})
        );
    }
}
