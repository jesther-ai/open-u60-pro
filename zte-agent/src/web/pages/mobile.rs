//! Router › Mobile Network (iOS `MobileNetworkView`): "Connectivity"
//! (Airplane Mode and Mobile Data switches, applied at once, as the app's),
//! then the app's Apply form: "Connection Mode" (Automatic / Manual),
//! "Data Roaming", "Network Selection" (Automatic / Manual; under Manual
//! "Scan Networks" and the networks found, each a way to register to it)
//! and "Apply". Read at page load (the app reads once); no stream.
//!
//! - Apply sends only what changed, as the app: `PUT /api/modem/data` for
//!   the connection mode or roaming (unchanged fields keep the router's
//!   current values; turning roaming on also turns mobile data on, as the
//!   app), `PUT /api/modem/network-mode {"net_select_mode":"auto_select"}`
//!   when Automatic selection is chosen again (the app's call; not verified
//!   on the device: the firmware's `nwinfo_set_netselect` may want another
//!   field, and a failure after the data write says the connection
//!   settings were applied; choosing Manual sends nothing, only registering
//!   does). Roaming on is asked first; for a remote browser also what can
//!   cut its connection: roaming off, Automatic selection, Manual
//!   connection mode. A changed mode or roaming continues on an operation
//!   page that follows the `modem` topic (the app's 5 confirmation reads).
//! - "Scan Networks" (single-use token; asked first for a remote browser)
//!   starts the modem's scan and follows it on an operation page (`netscan`
//!   topic, 60 s); its way back (`?scan=1`) reads the networks found, once
//!   the modem reports the scan done. A network's row asks "Register to
//!   …?" first (the app does not: another network can cut service); the
//!   question and the post re-read the scan's results, so only a network
//!   the last scan offers is registered to, under the name it gave. Then
//!   the result is followed (`netscan`, 30 s).
//! - Airplane mode off is tried twice, 3 s apart, as the app's; when both
//!   fail, "Reboot Required" (the app's alert) offers a reboot, which on
//!   the web needs the router password (checked on the router, counted by
//!   the login rate limit; the app reboots without one) and a single-use
//!   token: Device Controls' password post (`pages::devctl::post`).

use maud::{html, Markup};
use serde_json::{json, Value};

use super::confirm::{asking, nothing, Bad, Input, Kind, Next, Question, REMOTE};
use crate::events::Topic;
use crate::handlers::AppState;
use crate::server::Method;
use crate::web::kit::{self, Act, Icon};
use crate::web::ops::{self, APPLY_TARGETS};
use crate::web::render::once_field;
use crate::web::screen::PageData;
use crate::web::session::{percent_encode, query_param};
use crate::web::store::Claim;
use crate::web::tabs::router::{AIRPLANE, MOBILE_DATA};
use crate::web::fmt;

pub const PATH: &str = "/ui/router/mobile-network";
pub const APPLY: &str = "/ui/router/mobile-network/apply";
pub const SCAN: &str = "/ui/router/mobile-network/scan";
pub const REGISTER: &str = "/ui/router/mobile-network/register";
pub const REBOOT: &str = "/ui/router/mobile-network/reboot";

// --- The data call's state ---

/// `get_wwaniface` (`GET /api/modem/data`), only with the switches a write
/// keeps (an empty answer is no state, not the defaults: those could turn
/// roaming off).
pub fn wwan(app: &AppState, _: Option<&str>) -> Result<Value, String> {
    complete(super::api_get(app, "/api/modem/data")?)
}

pub fn complete(v: Value) -> Result<Value, String> {
    if fmt::int(&v["enable"]).is_some() && fmt::int(&v["roam_enable"]).is_some() {
        Ok(v)
    } else {
        Err("Mobile data state unavailable".into())
    }
}

/// A field of the current state, from a number or a numeric string.
pub fn num(v: &Value, key: &str, default: i64) -> i64 {
    fmt::int(&v[key]).unwrap_or(default)
}

fn connected(data: &Value) -> bool {
    matches!(data["connect_status"].as_str(), Some("ipv4_connected" | "ipv6_connected" | "ipv4_ipv6_connected"))
}

/// `set_wwaniface` for mobile data on or off (as the app writes it).
pub fn data_body(cur: &Value, on: bool) -> Value {
    let mut b = json!({
        "cid": 1,
        "connect_mode": if on { 1 } else { num(cur, "connect_mode", 1) },
        "roam_enable": num(cur, "roam_enable", 0),
        "enable": i64::from(on),
    });
    if !on {
        b["connect_status"] = "disconnected".into();
    }
    b
}

/// `set_wwaniface` for Apply (the app's `connectionBody`): the changed
/// connection mode and roaming, the router's current values for the
/// others; turning roaming on also turns mobile data on.
pub fn apply_body(cur: &Value, mode: Option<i64>, roaming: Option<bool>) -> Value {
    let roaming_now = num(cur, "roam_enable", 0) != 0;
    let roaming = roaming.unwrap_or(roaming_now);
    let enable = if roaming && !roaming_now { 1 } else { num(cur, "enable", 1) };
    json!({"cid": 1, "connect_mode": mode.unwrap_or_else(|| num(cur, "connect_mode", 1)), "roam_enable": i64::from(roaming), "enable": enable})
}

/// iOS `mobileDataFooter` (with an exact "connected" test: the app's
/// substring test reads "disconnected" as connected).
pub fn data_footer(d: &Value) -> String {
    let status = d["connect_status"].as_str().unwrap_or("");
    let on = num(d, "enable", 1) == 1;
    match (on, connected(d)) {
        (false, true) => "Mobile data setting is off, but the connection is still active.".into(),
        (false, false) => "Mobile data is disabled.".into(),
        (true, true) => {
            // Swift's `capitalized`: "ipv4_connected" -> "Ipv4 Connected".
            let words: Vec<String> = status
                .split('_')
                .map(|w| {
                    let mut c = w.chars();
                    c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect()).unwrap_or_default()
                })
                .collect();
            format!("Connected \u{2014} {}", words.join(" "))
        }
        (true, false) if !status.is_empty() => "Disconnected".into(),
        (true, false) => "Disabling mobile data will disconnect the cellular connection.".into(),
    }
}

// --- The networks a scan found ---

/// One network of a scan (iOS `NetworkOperator`).
#[derive(Debug, PartialEq, Eq)]
pub struct Network {
    pub name: String,
    pub plmn: String,
    pub rat: String,
    /// "available", "current" or "forbidden".
    pub status: &'static str,
}

/// A firmware value as text (strings and numbers).
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn status_word(s: &str) -> &'static str {
    match s {
        "current" | "2" => "current",
        "forbidden" | "3" => "forbidden",
        _ => "available",
    }
}

/// `m_netselect_contents`: the app's list of objects (`name` or
/// `operator_name`, `mcc_mnc` or `plmn`, `rat`, `status`), or the
/// firmware's "stat,name,plmn,rat;" string (stat as in AT+COPS: 1
/// available, 2 current, 3 forbidden); entries that fit neither are left out.
pub fn parse_networks(results: &Value) -> Vec<Network> {
    let contents = &results["m_netselect_contents"];
    if let Some(list) = contents.as_array() {
        return list
            .iter()
            .map(|n| {
                let pick = |a: &str, b: &str| Some(text(&n[a])).filter(|s| !s.is_empty()).unwrap_or_else(|| text(&n[b]));
                Network { name: pick("name", "operator_name"), plmn: pick("mcc_mnc", "plmn"), rat: text(&n["rat"]), status: status_word(&text(&n["status"])) }
            })
            .collect();
    }
    let digits = |s: &str, n: std::ops::RangeInclusive<usize>| n.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    text(contents)
        .split(';')
        .filter_map(|e| {
            let p: Vec<&str> = e.split(',').map(str::trim).collect();
            let fits = p.len() == 4 && digits(p[0], 1..=1) && digits(p[2], 5..=6) && digits(p[3], 1..=2);
            fits.then(|| Network { name: p[1].to_owned(), plmn: p[2].to_owned(), rat: p[3].to_owned(), status: status_word(p[0]) })
        })
        .collect()
}

/// Where a scan's networks are read.
pub const RESULTS: &str = "/api/modem/scan/results";

/// The network (`plmn`, `rat`) the scan's results offer: found and not
/// forbidden.
fn offered(results: &Value, plmn: &str, rat: &str) -> Option<Network> {
    parse_networks(results).into_iter().find(|n| n.plmn == plmn && n.rat == rat && n.status != "forbidden")
}

/// With `?scan=1` (the scan's way back): the networks found, once the
/// modem reports the scan done (else none: a scan that timed out shows
/// nothing, as the app's).
pub fn networks(app: &AppState, query: Option<&str>) -> Result<Value, String> {
    if query_param(query, "scan").as_deref() != Some("1") {
        return Ok(Value::Null);
    }
    let status = super::api_get(app, "/api/modem/scan/status")?;
    if !ops::scan_done(&text(&status["m_netselect_status"])) {
        return Ok(Value::Null);
    }
    let found = parse_networks(&super::api_get(app, RESULTS)?);
    Ok(Value::Array(found.into_iter().map(|n| json!({"name": n.name, "plmn": n.plmn, "rat": n.rat, "status": n.status})).collect()))
}

// --- The page ---

fn foot(text: &str) -> Markup {
    html! { p.foot { (text) } }
}

fn confirm_href(path: &str, field: &str, value: &str) -> String {
    format!("{path}/confirm?{field}={value}")
}

/// A segmented choice (`fieldset.chips.c2`, its legend hidden as the app's);
/// `data-i` marks the router's value (Apply is dimmed while nothing differs).
fn segmented(legend: &str, name: &str, options: [(&str, &str, &str); 2], cur: &str, router: &str) -> Markup {
    html! {
        li.it.fld {
            fieldset.chips.c2 {
                legend.sr { (legend) }
                div {
                    @for (id, value, label) in options {
                        label {
                            input.sr type="radio" id=(id) name=(name) value=(value) checked[value == cur] data-i[value == router];
                            span { (label) }
                        }
                    }
                }
            }
        }
    }
}

/// A network's row: its name, "<PLMN> · <RAT>", a green check for the
/// current one; a forbidden one is shown, not offered (as the app's).
fn network_row(n: &Value) -> Markup {
    let f = |k: &str| n[k].as_str().unwrap_or("");
    let (name, plmn, rat, status) = (f("name"), f("plmn"), f("rat"), f("status"));
    let shown = if name.is_empty() { plmn } else { name };
    let body = html! {
        span.cvb { span.nm { (shown) } small { (plmn) " \u{b7} " (rat) } }
        @if status == "current" { span.c-green { (kit::icon(Icon::Check)) } }
        @if status == "forbidden" { span.c-red { (kit::icon(Icon::Cross)) } }
    };
    let ok = register_fields(plmn, rat).is_some();
    html! {
        li.man {
            @if status == "forbidden" || !ok {
                span.it.off.net aria-disabled="true" { (body) }
            } @else {
                a.it.net href=(format!("{REGISTER}?plmn={}&rat={}", percent_encode(plmn), percent_encode(rat))) { (body) }
            }
        }
    }
}

pub fn page(pd: &PageData) -> Markup {
    let csrf = pd.csrf();
    let token = pd.once();
    let hidden = html! { (csrf) (once_field(&token)) };
    let data = pd.read(0);
    let mode = pd.read(1).and_then(|m| m["operate_mode"].as_str());
    // The app's switch: on while enabled or still connected.
    let data_on = data.map(|d| num(d, "enable", 1) == 1 || connected(d));
    let airplane = mode.map(|m| !m.is_empty() && m != "ONLINE");
    let (lpm, data_off) = (confirm_href(AIRPLANE.path, "mode", "LPM"), confirm_href(MOBILE_DATA.path, "enable", "0"));
    let post = |path: &'static str, name: &'static str, value: &'static str| Act::Post { action: path, name, value };
    // In airplane mode the app disables everything but Airplane Mode.
    let radio = airplane != Some(true);
    let (data_on_act, data_off_act) = if radio { (post(MOBILE_DATA.path, "enable", "1"), Act::Link(&data_off)) } else { (Act::Off, Act::Off) };
    let sel_now = pd
        .views
        .value(Topic::Signal)
        .and_then(|s| s["net_select_mode"].as_str())
        .filter(|s| *s == "manual_select")
        .unwrap_or("auto_select");
    let found = pd.read(2).and_then(Value::as_array);
    // Coming back from a scan (with the networks found) or from an Apply
    // that chose Manual: Manual.
    let sel = if found.is_some() || pd.param("sel").as_deref() == Some("manual") { "manual_select" } else { sel_now };
    let cm = data.map_or(1, |d| num(d, "connect_mode", 1));
    let roaming = data.is_some_and(|d| num(d, "roam_enable", 0) != 0);
    let off = !radio || data.is_none();
    html! {
        // The app's message row, above everything (its data read's error
        // replaces the airplane mode one).
        @match (pd.reads.first(), pd.reads.get(1)) {
            (Some(Err(e)), _) => p.msg.err role="alert" { "Failed to load: " (fmt::truncate(e, 120)) },
            (_, Some(Err(_))) => p.msg.err role="alert" { "Failed to load airplane mode status" },
            _ => {}
        }
        (kit::section(Some("Connectivity"), data.map(|d| foot(&data_footer(d))), html! {
            // Their posts carry the page's single-use token (sent once).
            (kit::switch_row(&hidden, "Airplane Mode", airplane, Act::Link(&lpm), post(AIRPLANE.path, "mode", "ONLINE")))
            (kit::switch_row(&hidden, "Mobile Data", data_on, data_on_act, data_off_act))
        }))
        // The scan's own form: its button sits under Network Selection
        // (a remote browser is asked first).
        form #mn-scan method="post" action=(SCAN) {
            (csrf)
            (once_field(&token))
            @if !pd.remote { input type="hidden" name="confirm" value="1"; }
        }
        form.mn method="post" action=(APPLY) {
            (csrf)
            (once_field(&token))
            // What the page showed: Apply sends only what changed.
            input type="hidden" name="cm0" value=(cm);
            input type="hidden" name="roam0" value=(u8::from(roaming));
            input type="hidden" name="sel0" value=(sel_now);
            fieldset.mnf disabled[off] {
                (kit::section(Some("Connection Mode"), None, segmented("Mode", "cm", [("cm-a", "1", "Automatic"), ("cm-m", "0", "Manual")], &cm.to_string(), &cm.to_string())))
                (kit::section(None, Some(foot("Enabling roaming may incur additional charges from your carrier.")), html! {
                    li.it {
                        label.tl for="roam" {
                            span { "Data Roaming" }
                            input.sr #roam type="checkbox" role="switch" name="roam" value="1" checked[roaming] data-i[roaming];
                            span.sw aria-hidden="true" {}
                        }
                    }
                }))
                (kit::section(Some("Network Selection"), None, html! {
                    (segmented("Mode", "sel", [("sel-a", "auto_select", "Automatic"), ("sel-m", "manual_select", "Manual")], sel, sel_now))
                    li.man { button.it.cv.bt type="submit" form="mn-scan" { "Scan Networks" } }
                    @if let Some(list) = found {
                        @for n in list { (network_row(n)) }
                        @if list.is_empty() { li.man.it.muted { "No networks found." } }
                    }
                }))
                (kit::section(None, None, html! { li { button.it.cv.snd type="submit" { "Apply" } } }))
            }
        }
    }
}

// --- Apply, Scan, Register (`pages::confirm`) ---

fn one_of(pairs: &[(String, String)], name: &str, set: &[&str], absent: Option<&'static str>) -> Result<String, Bad> {
    match pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()) {
        Some(v) if set.contains(&v) => Ok(v.to_owned()),
        None => absent.map(str::to_owned).ok_or(Bad::Request),
        Some(_) => Err(Bad::Request),
    }
}

/// A registration's fields: a 5 or 6 digit PLMN and the scan's RAT; `None`
/// if they do not fit. (The name shown is the scan's, never the URL's.)
fn register_fields(plmn: &str, rat: &str) -> Option<()> {
    let plmn_ok = (5..=6).contains(&plmn.len()) && plmn.bytes().all(|b| b.is_ascii_digit());
    (plmn_ok && rat_ok(rat)).then_some(())
}

/// A radio access technology as a scan names it: an AT+COPS access
/// technology number, 0 to 13 (the firmware's list), or a technology's name
/// (the app's list), in any case. Anything else is a bad request.
fn rat_ok(rat: &str) -> bool {
    const NAMES: [&str; 30] = [
        "GSM", "GPRS", "EDGE", "UMTS", "WCDMA", "HSPA", "HSPA+", "HSDPA", "HSUPA", "TD-SCDMA", "TDSCDMA", "CDMA", "LTE", "LTE-M", "CAT-M1",
        "NB-IOT", "E-UTRAN", "EUTRAN", "UTRAN", "GERAN", "NR", "NR5G", "NR5G-SA", "NR5G-NSA", "5G", "5G SA", "5G NSA", "2G", "3G", "4G",
    ];
    let act = (1..=2).contains(&rat.len()) && rat.bytes().all(|b| b.is_ascii_digit()) && rat.parse::<u8>().is_ok_and(|n| n <= 13);
    act || NAMES.iter().any(|n| n.eq_ignore_ascii_case(rat))
}

pub fn parse(kind: Kind, pairs: &[(String, String)]) -> Result<Vec<(&'static str, String)>, Bad> {
    let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map_or("", |(_, v)| v.as_str());
    match kind {
        Kind::Apply => {
            let sels = ["auto_select", "manual_select"];
            Ok(vec![
                ("cm", one_of(pairs, "cm", &["0", "1"], None)?),
                ("roam", one_of(pairs, "roam", &["0", "1"], Some("0"))?),
                ("sel", one_of(pairs, "sel", &sels, None)?),
                ("cm0", one_of(pairs, "cm0", &["0", "1"], None)?),
                ("roam0", one_of(pairs, "roam0", &["0", "1"], None)?),
                ("sel0", one_of(pairs, "sel0", &sels, None)?),
            ])
        }
        Kind::Register => {
            let (plmn, rat) = (get("plmn"), get("rat"));
            register_fields(plmn, rat).ok_or(Bad::Request)?;
            Ok(vec![("plmn", plmn.to_owned()), ("rat", rat.to_owned())])
        }
        _ => Ok(Vec::new()),
    }
}

/// Apply changes something (else the app's Apply is disabled).
pub fn changed(i: &Input) -> bool {
    ["cm", "roam", "sel"].iter().any(|k| i.val(k) != i.val(&format!("{k}0")))
}

/// Apply's question: roaming on always; for a remote browser also what can
/// cut its connection (roaming off, Automatic selection, Manual
/// connection mode). `None`: Apply posts at once.
fn apply_question(i: &Input, remote: bool) -> Option<Question> {
    let to = |k: &str, from: &str, to: &str| i.val(&format!("{k}0")) == from && i.val(k) == to;
    let (roam_on, roam_off) = (to("roam", "0", "1"), remote && to("roam", "1", "0"));
    let auto = remote && i.val("sel") == "auto_select" && i.val("sel0") != "auto_select";
    let manual = remote && to("cm", "1", "0");
    let mut head = None;
    let mut lines: Vec<String> = Vec::new();
    if roam_on {
        head = Some(("Turn on data roaming?", "Turn on data roaming"));
        lines.extend(["Your operator may charge for data used abroad.".into(), "This also turns mobile data on if it is off.".into()]);
    }
    if roam_off {
        head = head.or(Some(("Turn off data roaming?", "Turn off data roaming")));
        lines.extend(["Mobile data stops while the router is abroad.".into(), "You are connected remotely: if the router is abroad, this page can\u{2019}t be reached until roaming is turned back on at the router.".into()]);
    }
    if auto {
        head = head.or(Some(("Switch to automatic network selection?", "Apply")));
        lines.push("The modem chooses a network itself. It may re-register, which drops the connection for a moment.".into());
    }
    if manual {
        head = head.or(Some(("Switch to manual connection mode?", "Apply")));
        lines.push("Mobile data no longer reconnects by itself after the connection drops.".into());
    }
    let (title, label) = head?;
    Some(asking(title, lines, label, remote, auto || manual))
}

/// The question of `kind`; a registration's names the network the scan's
/// results (`found`, read for the question page; `None` when only asked
/// whether there is one) offer.
pub fn question(kind: Kind, i: &Input, remote: bool, found: Option<&Result<Value, String>>) -> Option<Question> {
    match kind {
        Kind::Apply => apply_question(i, remote),
        Kind::Scan if remote => Some(asking(
            "Scan for networks?",
            vec!["The modem searches for up to a minute. Some modems pause mobile data while they search.".into()],
            "Scan Networks",
            remote,
            true,
        )),
        Kind::Register => {
            let (plmn, rat) = (i.val("plmn"), i.val("rat"));
            let net = match found {
                None => None,
                Some(Err(e)) => return Some(nothing("Failed to load the networks", vec![fmt::truncate(e, 120)])),
                Some(Ok(r)) => match offered(r, plmn, rat) {
                    Some(n) => Some(n),
                    None => return Some(nothing("This network is not in the last scan.", vec!["Scan again to see the networks available now.".into()])),
                },
            };
            let name = net.map(|n| n.name).filter(|n| !n.is_empty()).unwrap_or_else(|| plmn.to_owned());
            Some(asking(
                format!("Register to {name}?"),
                vec![
                    format!("The modem leaves its current network and registers to {name} ({plmn})."),
                    "If this network does not accept your SIM, the router loses service until it registers to another one.".into(),
                ],
                "Register",
                remote,
                true,
            ))
        }
        _ => None,
    }
}

fn failed(prefix: &str) -> impl Fn((u16, String)) -> (u16, String) + '_ {
    move |(status, e)| (status, format!("{prefix}{}", fmt::truncate(&e, 120)))
}

pub fn send(app: &AppState, kind: Kind, i: &Input, claim: &mut Claim) -> Result<(), (u16, String)> {
    match kind {
        Kind::Apply => {
            // Merged with the state as it is now; unread: nothing is sent.
            let cur = wwan(app, None).map_err(|e| (503, format!("Failed: {}", fmt::truncate(&e, 120))))?;
            let mode = (i.val("cm") != i.val("cm0")).then(|| i64::from(i.val("cm") == "1"));
            let roaming = (i.val("roam") != i.val("roam0")).then(|| i.val("roam") == "1");
            let wrote = mode.is_some() || roaming.is_some();
            if wrote {
                claim.disarm();
                super::call(app, Method::Put, "/api/modem/data", &apply_body(&cur, mode, roaming)).map_err(failed("Failed: "))?;
            }
            if i.val("sel") == "auto_select" && i.val("sel0") != "auto_select" {
                claim.disarm();
                let body = json!({"net_select_mode": "auto_select"});
                // After the data write: say that part was applied.
                let prefix = if wrote { "Connection settings applied; Automatic selection failed: " } else { "Failed: " };
                super::call(app, Method::Put, "/api/modem/network-mode", &body).map_err(failed(prefix))?;
            }
            Ok(())
        }
        Kind::Scan => {
            claim.disarm();
            super::call(app, Method::Post, "/api/modem/scan", &json!({})).map(|_| ()).map_err(failed("Scan failed: "))
        }
        _ => {
            // Only a network the last scan offers (a crafted link sends nothing).
            let found = super::api_get(app, RESULTS).map_err(|e| (503, format!("Failed: {}", fmt::truncate(&e, 120))))?;
            if offered(&found, i.val("plmn"), i.val("rat")).is_none() {
                return Err((409, "This network is not in the last scan. Scan again.".into()));
            }
            claim.disarm();
            let body = json!({"m_mcc_mnc": i.val("plmn"), "m_rat": i.val("rat")});
            super::call(app, Method::Post, "/api/modem/register", &body).map(|_| ()).map_err(failed("Failed: "))
        }
    }
}

/// Apply's operation target: "<mode>:<roaming>", "-" for what it left.
fn apply_target(i: &Input) -> &'static str {
    let part = |k: &str| if i.val(k) != i.val(&format!("{k}0")) { i.val(k) } else { "-" };
    let t = format!("{}:{}", part("cm"), part("roam"));
    APPLY_TARGETS.into_iter().find(|x| *x == t).unwrap_or("-:-")
}

pub fn next(kind: Kind, i: &Input, r: Result<(), &str>) -> Next {
    match (kind, r) {
        (_, Err(msg)) => Next::Line(back(kind, i), Some(false), msg.to_owned()),
        (Kind::Apply, Ok(())) if apply_target(i) != "-:-" => Next::Op(&ops::APPLY, apply_target(i)),
        // Manual stays chosen (the router changes only when registering).
        (Kind::Apply, Ok(())) if i.val("sel") == "manual_select" => Next::Line(format!("{PATH}?sel=manual"), Some(true), "Settings applied".into()),
        (Kind::Apply, Ok(())) => Next::Line(PATH.into(), Some(true), "Settings applied".into()),
        (Kind::Scan, Ok(())) => Next::Op(&ops::NETSCAN, ""),
        (_, Ok(())) => Next::Op(&ops::REGISTER, ""),
    }
}

pub fn back(kind: Kind, _: &Input) -> String {
    match kind {
        Kind::Register => format!("{PATH}?scan=1"),
        _ => PATH.into(),
    }
}

// --- Reboot Required ---

/// The app's alert, as a page: its message (and the remote line), then
/// (unlike the app) the router password, "Reboot Now" (single-use token)
/// and Cancel; the post answers with the "Rebooting…" page.
pub fn reboot_page(pd: &PageData) -> Markup {
    let err = pd.form.and_then(|f| f.error("password"));
    html! {
        div.narrow {
            p { "Due to a firmware limitation, the cellular radio cannot be restored without rebooting. The router will restart (about 60 seconds)." }
            @if pd.remote { p { (REMOTE) } }
            form.panel method="post" action=(REBOOT) {
                (pd.csrf())
                (once_field(&pd.once()))
                div.field {
                    label for="rpw" { "Router Password" }
                    input #rpw type="password" name="password" autocomplete="current-password" required
                        aria-invalid=[err.map(|_| "true")] aria-describedby=[err.map(|_| "rpw-err")];
                    @if let Some(e) = err { p.ferr #rpw-err { (e) } }
                }
                p.actions {
                    button.btn.btn-danger type="submit" { "Reboot Now" }
                    a.btn.btn-quiet href=(PATH) { "Cancel" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_bodies_follow_the_app() {
        let cur = json!({"connect_mode": "0", "roam_enable": 1, "enable": "1", "connect_status": "ipv4_connected"});
        assert_eq!(
            data_body(&cur, false),
            json!({"cid": 1, "connect_mode": 0, "roam_enable": 1, "enable": 0, "connect_status": "disconnected"})
        );
        assert_eq!(data_body(&cur, true), json!({"cid": 1, "connect_mode": 1, "roam_enable": 1, "enable": 1}));
        // Missing fields take the app's defaults.
        assert_eq!(data_body(&json!({}), true), json!({"cid": 1, "connect_mode": 1, "roam_enable": 0, "enable": 1}));
        let off = json!({"connect_mode": 1, "roam_enable": 0, "enable": 0});
        // Roaming on with roaming off before also turns data on.
        assert_eq!(apply_body(&off, None, Some(true)), json!({"cid": 1, "connect_mode": 1, "roam_enable": 1, "enable": 1}));
        assert_eq!(apply_body(&off, Some(0), None), json!({"cid": 1, "connect_mode": 0, "roam_enable": 0, "enable": 0}));
        // What Apply did not change keeps the router's value.
        assert_eq!(apply_body(&cur, Some(1), None), json!({"cid": 1, "connect_mode": 1, "roam_enable": 1, "enable": 1}));
        assert_eq!(apply_body(&cur, None, Some(false)), json!({"cid": 1, "connect_mode": 0, "roam_enable": 0, "enable": 1}));
    }

    #[test]
    fn unread_states() {
        // An empty answer is no state (its defaults could turn roaming off).
        assert!(complete(Value::Null).is_err() && complete(json!({"enable": 1})).is_err());
        assert!(complete(json!({"enable": "1", "roam_enable": 0})).is_ok());
    }

    #[test]
    fn mobile_data_footer_follows_the_app() {
        let d = |enable: i64, status: &str| data_footer(&json!({"enable": enable, "connect_status": status}));
        assert_eq!(d(1, "ipv4_ipv6_connected"), "Connected \u{2014} Ipv4 Ipv6 Connected");
        assert_eq!(d(1, "disconnected"), "Disconnected");
        assert_eq!(d(1, ""), "Disabling mobile data will disconnect the cellular connection.");
        assert_eq!(d(0, "disconnected"), "Mobile data is disabled.");
        assert_eq!(d(0, "ipv6_connected"), "Mobile data setting is off, but the connection is still active.");
    }

    /// The app's list, or the firmware's "stat,name,plmn,rat;" string.
    #[test]
    fn networks_parse_both_shapes() {
        let app = json!({"m_netselect_contents": [
            {"name": "Test Net", "mcc_mnc": "00101", "rat": "LTE", "status": "current"},
            {"operator_name": "Other", "plmn": "001002", "rat": 7, "status": "forbidden"},
            {"name": "Third", "mcc_mnc": "00103", "rat": "NR"},
        ]});
        let n = parse_networks(&app);
        assert_eq!(n[0], Network { name: "Test Net".into(), plmn: "00101".into(), rat: "LTE".into(), status: "current" });
        assert_eq!((n[1].name.as_str(), n[1].plmn.as_str(), n[1].rat.as_str(), n[1].status), ("Other", "001002", "7", "forbidden"));
        assert_eq!(n[2].status, "available");
        let fw = json!({"m_netselect_contents": "2,Test Net,00101,7;1,Other Net,00102,12;3,Barred,00103,2;x,Bad,1,7;"});
        let n = parse_networks(&fw);
        assert_eq!(n.len(), 3);
        assert_eq!((n[0].status, n[1].status, n[2].status), ("current", "available", "forbidden"));
        assert_eq!((n[1].name.as_str(), n[1].plmn.as_str(), n[1].rat.as_str()), ("Other Net", "00102", "12"));
        assert!(parse_networks(&json!({})).is_empty());
    }

    #[test]
    fn apply_sends_what_changed() {
        let i = |cm: &str, roam: &str, sel: &str| {
            Input::of(&[("cm", cm), ("roam", roam), ("sel", sel), ("cm0", "1"), ("roam0", "0"), ("sel0", "manual_select")])
        };
        assert!(!changed(&i("1", "0", "manual_select")) && changed(&i("0", "0", "manual_select")));
        assert_eq!(apply_target(&i("0", "0", "manual_select")), "0:-");
        assert_eq!(apply_target(&i("1", "1", "manual_select")), "-:1");
        assert_eq!(apply_target(&i("1", "0", "auto_select")), "-:-");
        assert!(matches!(next(Kind::Apply, &i("1", "0", "auto_select"), Ok(())), Next::Line(to, Some(true), l) if l == "Settings applied" && to == PATH));
        // Manual alone: the app's line, and Manual stays chosen.
        assert!(matches!(next(Kind::Apply, &i("1", "0", "manual_select"), Ok(())), Next::Line(to, Some(true), l) if l == "Settings applied" && to == "/ui/router/mobile-network?sel=manual"));
        assert!(matches!(next(Kind::Apply, &i("0", "1", "manual_select"), Ok(())), Next::Op(_, "0:1")));
        // Roaming on asks; off only remotely; anything else does not.
        assert_eq!(question(Kind::Apply, &i("1", "1", "manual_select"), false, None).unwrap().title, "Turn on data roaming?");
        assert!(question(Kind::Apply, &i("0", "0", "manual_select"), false, None).is_none());
        let off = Input::of(&[("cm", "1"), ("roam", "0"), ("sel", "auto_select"), ("cm0", "1"), ("roam0", "1"), ("sel0", "auto_select")]);
        assert!(question(Kind::Apply, &off, false, None).is_none());
        assert_eq!(question(Kind::Apply, &off, true, None).unwrap().title, "Turn off data roaming?");
        // Remotely, also Automatic selection and Manual connection mode
        // (each can cut the connection), with the remote line.
        let q = question(Kind::Apply, &i("1", "0", "auto_select"), true, None).unwrap();
        assert_eq!((q.title.as_str(), q.lines.last().map(String::as_str)), ("Switch to automatic network selection?", Some(REMOTE)));
        assert!(question(Kind::Apply, &i("1", "0", "auto_select"), false, None).is_none());
        let q = question(Kind::Apply, &i("0", "0", "manual_select"), true, None).unwrap();
        assert_eq!((q.title.as_str(), q.lines.len()), ("Switch to manual connection mode?", 2));
        // Several at once: one question naming each.
        let q = question(Kind::Apply, &i("0", "1", "auto_select"), true, None).unwrap();
        assert_eq!((q.title.as_str(), q.lines.len()), ("Turn on data roaming?", 5));
        // A scan asks only remotely.
        assert!(question(Kind::Scan, &i("1", "0", "auto_select"), false, None).is_none());
        assert_eq!(question(Kind::Scan, &i("1", "0", "auto_select"), true, None).unwrap().label, Some("Scan Networks"));
    }

    /// The fields are checked; the name shown is the scan's, and only a
    /// network the last scan offers is asked about.
    #[test]
    fn registration_fields_are_checked() {
        let p = |q: &str| parse(Kind::Register, crate::web::session::Form::parse(q.as_bytes()).pairs());
        assert_eq!(p("plmn=00101&rat=7&name=Ignored").unwrap(), [("plmn", "00101".to_owned()), ("rat", "7".to_owned())]);
        assert!(p("plmn=00101&rat=lte").is_ok() && p("plmn=001010&rat=NR5G").is_ok() && p("plmn=00101&rat=13").is_ok());
        for bad in ["plmn=0010&rat=7", "plmn=0010a&rat=7", "plmn=00101&rat=", "plmn=00101&rat=%22x", "plmn=00101&rat=abc", "plmn=00101&rat=14", "plmn=00101&rat=007"] {
            assert!(matches!(p(bad), Err(Bad::Request)), "{bad}");
        }
        let i = Input::of(&[("plmn", "00101"), ("rat", "7")]);
        // Only asked whether there is a question: yes.
        assert_eq!(question(Kind::Register, &i, true, None).unwrap().label, Some("Register"));
        let found = Ok(json!({"m_netselect_contents": "1,Test Net,00101,7;3,Barred,00103,7;"}));
        let q = question(Kind::Register, &i, true, Some(&found)).unwrap();
        assert_eq!((q.title.as_str(), q.label), ("Register to Test Net?", Some("Register")));
        assert_eq!(q.lines.last().unwrap(), REMOTE);
        // Not offered (not found, forbidden, another RAT) or unread: nothing to do.
        for (plmn, rat) in [("99999", "7"), ("00103", "7"), ("00101", "12")] {
            let q = question(Kind::Register, &Input::of(&[("plmn", plmn), ("rat", rat)]), false, Some(&found)).unwrap();
            assert_eq!((q.title.as_str(), q.label), ("This network is not in the last scan.", None), "{plmn}");
        }
        let q = question(Kind::Register, &i, false, Some(&Err("ubus <down>".into()))).unwrap();
        assert_eq!((q.title.as_str(), q.label), ("Failed to load the networks", None));
    }
}
