//! Router › eSIM Profiles (iOS `RouterESimView`, `RouterESimViewModel`,
//! `RouterESimModels`), on the agent's eSIM API (`zte-agent/docs/esim.md`).
//!
//! - **eSIM Profiles** reads `GET /api/esim/profiles` and `GET
//!   /api/esim/chip` at page load (card commands: the slow lane) and is live
//!   from `esim_switch`, `esim_download`, `modem` and `signal` (in memory,
//!   events, and the signal reading every 2 s while the page is open): the
//!   app's download card, its connection card (a switch's steps while one
//!   runs, else whether mobile data is up), "On your eSIM" with "Updating…"
//!   while a switch runs and a Refresh once the card changed (a switch, a
//!   SIM identity phase), the profiles (the active one first, then "Switch
//!   to another profile"), "Add eSIM", the operator receipts and the chip
//!   card. The list itself is not a hub topic: reading it is a card command
//!   (it would put card traffic on a sampler lane, next to the switch's own
//!   checks), and it changes only through the actions below, each of which
//!   leads to a fresh page, a switch, which the page follows, or a download,
//!   whose end it shows; after either "Changed · Refresh" reads it again.
//!   While a switch reloads the SIM the agent answers with the list read
//!   before it: the target shows as active once its enable went through (the
//!   app's local update after `enabled_iccid`). The connection card samples
//!   the signal every 10 s (the app's idle check is 12 s).
//! - While a switch or a download holds the eSIM the page's controls are
//!   dimmed and inert (the app disables them); their pages and posts refuse
//!   with the busy line anyway.
//! - Profiles are named in URLs by an opaque key (`AuthState::opaque_key`,
//!   per agent run), never their ICCID; pages show the ICCID's last 4
//!   digits. The EID is shown masked ("•••• •••• •••• …1234") and whole only
//!   on an explicit `?eid=1` (a GET, `no-store` as every page; never in a
//!   fragment or a URL).
//! - Switch, Delete and Rename are pages of their own (the app's dialogs and
//!   sheet), each with a single-use token; Switch asks with the app's words
//!   and its warnings (`manual_apn`, `roaming_off`) and, for a remote
//!   browser, says that the connection drops and nothing switches back by
//!   itself. A switch continues on an operation page that follows
//!   `esim_switch` (the app's steps and outcome notes); nothing ever
//!   switches back. Each post re-reads the list first and sends nothing when
//!   the profile is gone or no longer fits, or while a switch or a download
//!   runs (the app's "The eSIM is busy; try again in a moment."); the agent
//!   runs one card command at a time and its 409s read the same.
//! - Results are the app's message card (`?r=<token>`, kept for the session
//!   under the post's token, so a replay lands on the same line).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::Request;
use axum::http::Method as HttpMethod;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::{json, Value};

use crate::auth::SessionId;
use crate::events::Topic;
use crate::handlers::AppState;
use crate::httpd::gate::Lane;
use crate::httpd::Ctx;
use crate::server::Method;
use crate::web::action::{self, Ask};
use crate::web::job::{self, OnceJob, Sent};
use crate::web::kit::{self, Icon, Tab};
use crate::web::ops::{is_op_id, Op, OpSpec, OpStatus, OpView};
use crate::web::render::{self, once_field, Fragment, Views};
use crate::web::screen::{self, FormState, Notice, PageData, Req};
use crate::web::session::{self, Form};
use crate::web::store::{Flash, Token, SETTLE_SLOW};
use crate::web::{fmt, sms, CspNonce};

pub const PATH: &str = "/ui/router/esim";
pub const SWITCH: &str = "/ui/router/esim/switch";
pub const DELETE: &str = "/ui/router/esim/delete";
pub const RENAME: &str = "/ui/router/esim/rename";
pub const RECEIPTS: &str = "/ui/router/esim/receipts";
pub const ADD: &str = "/ui/router/esim/add";
/// Add eSIM showing the download, also once it has finished.
pub const DL: &str = "/ui/router/esim/add?dl=1";
pub const PROFILES: &str = "/api/esim/profiles";
pub const CHIP: &str = "/api/esim/chip";
const ENABLE: &str = "/api/esim/enable";
const DELETE_API: &str = "/api/esim/delete";
const NICKNAME: &str = "/api/esim/nickname";
const PROCESS: &str = "/api/esim/notifications/process";
const NOTIFICATIONS: &str = "/api/esim/notifications";

/// The app's notice when the card is in use (a switch or a download runs,
/// or the agent holds the card).
pub const BUSY: &str = "The eSIM is busy; try again in a moment.";
const GONE: &str = "This profile is no longer on your eSIM.";
const NICK_MAX: usize = 64;

// --- What the pages show of the agent's answers ---

/// One profile (`GET /api/esim/profiles`).
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub iccid: String,
    pub provider: String,
    pub name: String,
    pub nickname: String,
    pub enabled: bool,
    pub class: String,
    pub warnings: Vec<String>,
}

impl Profile {
    /// The nickname, else the profile name, else the provider (the app's
    /// `displayName`; "" when none).
    pub fn display(&self) -> &str {
        if !self.nickname.is_empty() {
            &self.nickname
        } else if !self.name.is_empty() {
            &self.name
        } else {
            &self.provider
        }
    }

    /// As the card shows it.
    pub fn shown(&self) -> &str {
        match self.display() {
            "" => "Unnamed profile",
            d => d,
        }
    }

    /// Only operator profiles can be switched to (the agent refuses others).
    pub fn can_enable(&self) -> bool {
        !self.enabled && self.class == "operational"
    }

    /// The card's second line.
    fn subtitle(&self) -> &str {
        let d = self.display();
        if !self.provider.is_empty() && !self.provider.eq_ignore_ascii_case(d) {
            &self.provider
        } else if !self.name.is_empty() && !self.name.eq_ignore_ascii_case(d) {
            &self.name
        } else if self.enabled {
            "Selected on your router"
        } else {
            "Ready to switch"
        }
    }
}

/// The list and how fresh it is.
#[derive(Debug, Default)]
pub struct Inventory {
    pub profiles: Vec<Profile>,
    pub observed_at: i64,
    pub updating: bool,
    /// A failed read behind the list: (kind, message).
    pub error: Option<(String, String)>,
    pub pending_receipts: Option<i64>,
    /// The manual APN the router dials while manual APN mode is on.
    pub manual_apn: Option<String>,
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().map_or_else(String::new, |s| s.trim().to_owned())
}

pub fn inventory(v: &Value) -> Inventory {
    let profiles = v["profiles"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|p| p["iccid"].as_str().is_some_and(|i| !i.is_empty()))
                .map(|p| Profile {
                    iccid: text(p, "iccid"),
                    provider: text(p, "provider"),
                    name: text(p, "name"),
                    nickname: text(p, "nickname"),
                    enabled: p["state"] == "enabled",
                    class: text(p, "class"),
                    warnings: p["switch_warnings"].as_array().map(|w| w.iter().filter_map(|c| c.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let error = v["error"].is_object().then(|| (text(&v["error"], "kind"), text(&v["error"], "message")));
    Inventory {
        profiles,
        observed_at: fmt::int(&v["observed_at"]).unwrap_or(0),
        updating: v["updating"] == true,
        error: error.filter(|(_, m)| !m.is_empty()),
        pending_receipts: fmt::int(&v["pending_receipts"]),
        manual_apn: v["manual_apn"]["apn"].as_str().map(str::to_owned).filter(|a| !a.is_empty()),
    }
}

impl Inventory {
    fn active(&self) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.enabled)
    }
}

/// The opaque key of a profile in URLs (16 hex, per agent run).
fn key(app: &AppState, iccid: &str) -> Option<String> {
    app.auth.opaque_key("esim", iccid)
}

fn find<'a>(app: &AppState, inv: &'a Inventory, k: &str) -> Option<&'a Profile> {
    inv.profiles.iter().find(|p| key(app, &p.iccid).as_deref() == Some(k))
}

/// The key in a query or form: 16 lowercase hex.
fn key_of(v: Option<&str>) -> Option<String> {
    v.filter(|k| is_op_id(k)).map(str::to_owned)
}

/// "…" and the last 4 digits.
fn tail(iccid: &str) -> String {
    let digits: Vec<char> = iccid.trim_end_matches(['F', 'f']).chars().filter(char::is_ascii_digit).collect();
    let n = digits.len();
    format!("\u{2026}{}", digits[n.saturating_sub(4)..].iter().collect::<String>())
}

/// The profile names last read, by ICCID (`Ctx::esim_names`, handed to
/// pages and streams with `esim_switch` as `Views::esim_names`), so a
/// switch's live card, which renders from the hub's `esim_switch` alone, can
/// name its target.
#[derive(Default)]
pub struct Names(Mutex<Vec<(String, String)>>);

impl Names {
    /// A list read (a failed or empty one keeps the names known).
    fn remember(&self, inv: &Inventory) {
        if !inv.profiles.is_empty() {
            let names = inv.profiles.iter().map(|p| (p.iccid.clone(), p.shown().to_owned())).collect();
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) = names;
        }
    }

    fn of(&self, iccid: &str) -> Option<String> {
        let names = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        names.iter().find(|(i, _)| crate::esim::same_iccid(i, iccid)).map(|(_, n)| n.clone())
    }
}

fn remember(v: &Views, inv: &Inventory) {
    if let Some(n) = &v.esim_names {
        n.remember(inv);
    }
}

/// "Switch to <name>" (else the ICCID's last 4 digits).
fn target(v: &Views, iccid: &str) -> String {
    let name = v.esim_names.as_ref().and_then(|n| n.of(iccid));
    format!("Switch to {}", name.unwrap_or_else(|| tail(iccid)))
}

/// The app's switch dialog: what happens, then its warnings.
fn switch_lines(inv: &Inventory, p: &Profile) -> Vec<String> {
    let name = p.shown();
    let from = inv.active().map_or("the current profile", Profile::shown);
    let mut lines = vec![format!(
        "The router disconnects from {from} and connects with {name}. Internet is down for about a minute. Wi-Fi stays on."
    )];
    for code in &p.warnings {
        match code.as_str() {
            "manual_apn" => {
                if let Some(apn) = &inv.manual_apn {
                    lines.push(format!(
                        "The router is set to the manual APN \u{201c}{apn}\u{201d}. If {name} needs a different APN, change it in Mobile Network after switching, or it may have no data."
                    ));
                }
            }
            "roaming_off" => lines.push(format!(
                "{name} looks like a travel (roaming) profile and Data Roaming is off, so it will have no data until you turn Data Roaming on."
            )),
            _ => {}
        }
    }
    lines
}

// --- Switch, download and connection state (hub topics) ---

pub fn state(v: Option<&Value>) -> &str {
    v.and_then(|s| s["state"].as_str()).unwrap_or("idle")
}

/// A switch the agent is still verifying.
pub fn running(state: &str) -> bool {
    matches!(state, "card_busy" | "reloading" | "card_confirmed" | "modem_confirmed")
}

fn finished(state: &str) -> bool {
    matches!(state, "connected" | "no_data" | "modem_unconfirmed" | "card_unconfirmed" | "not_applied")
}

/// A download that holds the eSIM (the app's `isBusy`).
pub fn downloading(state: &str) -> bool {
    matches!(state, "running" | "confirm" | "cancelling")
}

/// A switch or a download holds the eSIM (the app's `blocksSwitching`).
pub fn blocked(v: &Views) -> bool {
    running(state(v.value(Topic::EsimSwitch))) || downloading(state(v.value(Topic::EsimDownload)))
}

fn generation(v: Option<&Value>) -> u64 {
    v.and_then(|s| s["generation"].as_u64()).unwrap_or(0)
}

/// The connection card's phase (iOS `RouterESimConnectionPhase`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    Checking,
    Disconnected,
    Switching,
    Reloading,
    Registering,
    Connected,
    Waiting,
    Unavailable,
}

impl Phase {
    fn blocks(self) -> bool {
        matches!(self, Phase::Switching | Phase::Reloading | Phase::Registering)
    }

    fn attention(self) -> bool {
        matches!(self, Phase::Waiting | Phase::Unavailable)
    }

    fn title(self, switch: bool) -> &'static str {
        match self {
            Phase::Checking => "Checking connection",
            Phase::Switching => "Switching profile",
            Phase::Reloading => "Reloading SIM",
            Phase::Registering => "Registering on network",
            Phase::Connected => "Mobile data connected",
            Phase::Disconnected => "Mobile data offline",
            Phase::Waiting => "Connection needs attention",
            Phase::Unavailable if switch => "Switch needs attention",
            Phase::Unavailable => "Connection status unavailable",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Phase::Switching => "Selecting the new profile on your eSIM.",
            Phase::Reloading => "The modem is loading the SIM. The card may be unreadable for 5\u{2013}25 seconds.",
            Phase::Registering => "The new profile is loaded. Waiting for a mobile data connection.",
            Phase::Waiting => "Mobile data hasn\u{2019}t connected yet. Check the plan or coverage, or choose another profile below.",
            Phase::Disconnected => "Your router\u{2019}s Wi-Fi stays connected. You can still switch profiles.",
            Phase::Checking => "Reading live status from your router.",
            Phase::Unavailable => "Check again to read the router\u{2019}s connection status.",
            Phase::Connected => "Mobile data is up. Traffic may take a few more seconds.",
        }
    }

    /// The step in progress: Switch profile, Reload SIM, Connect to network.
    fn step(self) -> usize {
        match self {
            Phase::Switching | Phase::Unavailable => 0,
            Phase::Reloading => 1,
            _ => 2,
        }
    }
}

/// A switch state as the app's phase (`RouterESimSwitchStatus.phase`).
fn switch_phase(state: &str) -> Phase {
    match state {
        "card_busy" => Phase::Switching,
        "reloading" => Phase::Reloading,
        "card_confirmed" | "modem_confirmed" => Phase::Registering,
        "connected" => Phase::Connected,
        "no_data" | "modem_unconfirmed" | "card_unconfirmed" => Phase::Waiting,
        "not_applied" => Phase::Unavailable,
        _ => Phase::Checking,
    }
}

/// The app's notes: while the eSIM refuses for now, and why a finished
/// switch did not reach "connected".
fn switch_note(state: &str, reason: Option<&str>) -> Option<&'static str> {
    Some(match (state, reason) {
        ("card_busy", _) => "The eSIM is busy and refused the switch for now. The router keeps trying for up to 4 minutes.",
        ("no_data", Some("roaming_off")) => {
            "This profile roams on a local network and Data Roaming is off. Turn on Data Roaming in Mobile Network."
        }
        ("no_data", Some("data_off")) => "The profile is active, but Mobile Data is off. Turn it on in Mobile Network.",
        ("no_data", _) => "The profile is active, but mobile data hasn't connected. Check the plan or coverage.",
        ("modem_unconfirmed", _) => "The eSIM switched, but the router still uses the previous SIM. Restart the router to finish.",
        ("card_unconfirmed", _) => "The eSIM couldn't be read after the switch. Refresh in a moment.",
        ("not_applied", Some("card_busy")) => {
            "The eSIM stayed busy and didn't switch; the previous profile is still active. Try again in a few minutes."
        }
        ("not_applied", _) => "The switch didn't happen; the previous profile is still active.",
        _ => return None,
    })
}

/// The data call's `connect_status` says connected.
pub fn connected(status: &str) -> bool {
    matches!(status.trim().to_ascii_lowercase().as_str(), "connected" | "ipv4_connected" | "ipv6_connected" | "ipv4_ipv6_connected")
}

/// What the connection card shows.
struct Conn {
    phase: Phase,
    /// "Switch to <name>" while it follows a switch.
    target: Option<String>,
    note: Option<&'static str>,
    operator: String,
    bars: i64,
}

/// The operator, network type and bars of the `signal` reading.
fn network(v: &Views) -> (String, i64) {
    let Some(s) = v.value(Topic::Signal) else { return (String::new(), 0) };
    let operator = s["network_provider"].as_str().map_or("", str::trim);
    let kind = if s["network_type"].as_str().is_some_and(|t| !t.trim().is_empty()) { crate::web::signal::display_type(s) } else { String::new() };
    let line = [operator, kind.as_str()].iter().filter(|t| !t.is_empty()).copied().collect::<Vec<_>>().join(" \u{b7} ");
    (fmt::truncate(&line, 60), fmt::int(&s["signalbar"]).unwrap_or(0).clamp(0, 5))
}

/// The card of a switch the page follows.
fn following(v: &Views, unknown: Phase) -> Conn {
    let sw = v.value(Topic::EsimSwitch);
    let st = state(sw);
    let phase = if st == "idle" { unknown } else { switch_phase(st) };
    let reason = sw.and_then(|s| s["reason"].as_str());
    let target = sw.and_then(|s| s["target_iccid"].as_str()).filter(|t| !t.is_empty()).map(|t| target(v, t));
    let (operator, bars) = network(v);
    Conn { phase, target, note: switch_note(st, reason), operator, bars }
}

// `Screen::arg` of eSIM Profiles, by bits: 0 a switch ran at page load,
// 1-20 the SIM identity epoch, 21-41 the start of a finished download (its
// profile was added), 42-62 the last switch's generation, 63 set (nonzero).
const EPOCH: (u32, u64) = (1, (1 << 20) - 1);
const ADDED: (u32, u64) = (21, (1 << 21) - 1);
const GENERATION: (u32, u64) = (42, (1 << 21) - 1);

fn field(arg: u64, (at, mask): (u32, u64)) -> u64 {
    arg >> at & mask
}

/// `Screen::arg` of eSIM Profiles: what the page was loaded with.
pub fn arg(v: &Views, _: Option<&str>) -> u64 {
    let sw = v.value(Topic::EsimSwitch);
    let epoch = sw.and_then(|s| s["sim_epoch"].as_u64()).unwrap_or(0);
    let dl = v.value(Topic::EsimDownload).filter(|d| state(Some(d)) == "done");
    let added = dl.map_or(0, |d| (d["started_at"].as_u64().unwrap_or(0) & ADDED.1).max(1));
    let at = |n: u64, (bit, mask): (u32, u64)| (n & mask) << bit;
    1 << 63 | at(generation(sw), GENERATION) | at(added, ADDED) | at(epoch, EPOCH) | u64::from(running(state(sw)))
}

/// A switch started since the page was loaded.
fn switched_since(v: &Views) -> bool {
    v.arg != 0 && field(arg(v, None), GENERATION) != field(v.arg, GENERATION)
}

/// The card or the SIM changed since the page was loaded: a switch, a SIM
/// identity phase, or a download that added a profile.
fn changed_since(v: &Views) -> bool {
    let now = arg(v, None);
    let added = field(now, ADDED);
    v.arg != 0 && (switched_since(v) || field(now, EPOCH) != field(v.arg, EPOCH) || (added != 0 && added != field(v.arg, ADDED)))
}

/// The data call is up (`modem`).
fn data_up(v: &Views) -> bool {
    v.value(Topic::Modem).and_then(|m| m["data"]["connect_status"].as_str()).is_some_and(connected)
}

/// The list page's card: a switch that runs, or that ran while the page was
/// open (once an active profile without data gets it, as the app's next
/// check shows); else whether mobile data is up (`modem`).
fn list_conn(v: &Views) -> Conn {
    let st = state(v.value(Topic::EsimSwitch));
    if running(st) || (finished(st) && (switched_since(v) || v.arg & 1 == 1)) {
        let mut c = following(v, Phase::Checking);
        if st == "no_data" && data_up(v) {
            (c.phase, c.note) = (Phase::Connected, None);
        }
        return c;
    }
    let modem = v.slot(Topic::Modem);
    let status = v.value(Topic::Modem).and_then(|m| m["data"]["connect_status"].as_str());
    let phase = match status {
        Some(s) if connected(s) => Phase::Connected,
        Some("") => Phase::Unavailable,
        Some(_) => Phase::Disconnected,
        None if modem.is_some_and(|m| m.error.is_some()) => Phase::Unavailable,
        None => Phase::Checking,
    };
    let (operator, bars) = network(v);
    Conn { phase, target: None, note: None, operator, bars }
}

/// The app's connection card (`ESimConnectionCard`): a spinner, a check or
/// a warning; "Switch to …"; the title; the operator and bars when
/// connected; a switch's three steps; the detail (or the note); "Wi-Fi
/// stays connected"; "Check again" when it needs attention.
fn conn_card(c: &Conn, id: &str, check: Option<&str>, back: Option<(&str, &str)>) -> Markup {
    let progress = c.phase.blocks();
    let attention = c.phase.attention();
    let on = c.phase == Phase::Connected;
    let step = c.phase.step();
    let steps = [("Switch profile", Icon::SimCard), ("Reload SIM", Icon::Refresh), ("Connect to network", Icon::Antenna)];
    html! {
        section.card.econn.esw[c.target.is_some()].busy[progress].ewarn[attention] id=(id) {
            div.ehead {
                @if progress || c.phase == Phase::Checking {
                    span.eic { span.spin {} }
                } @else if on {
                    span.eic.eok { (kit::icon(Icon::Check)) }
                } @else if attention {
                    span.eic { (kit::icon(Icon::Alert)) }
                } @else {
                    span.eic { (kit::icon(Icon::AntennaSlash)) }
                }
                div {
                    @if let Some(t) = &c.target { small.cap { (t) } }
                    b { (c.phase.title(c.target.is_some())) }
                    @if on && !c.operator.is_empty() { small.cap { (c.operator) } }
                }
                @if on { (kit::signal_bars(c.bars)) }
            }
            @if c.target.is_some() && (progress || on) {
                ol.esteps {
                    @for (i, (label, icon)) in steps.into_iter().enumerate() {
                        @let done = on || i < step;
                        @let cur = i == step && !on;
                        li.done[done].cur[cur] aria-current=[cur.then_some("step")] {
                            span.dot { (kit::icon(if done { Icon::Check } else if cur && attention { Icon::Alert } else { icon })) }
                            span { (label) }
                            @if cur && progress { small { "In progress" } }
                            @if done { small { "Done" } }
                        }
                    }
                }
            }
            @if !on || c.target.is_some() { p.det { (c.note.unwrap_or(c.phase.detail())) } }
            @if progress { p.cap.wifi { (kit::icon(Icon::Wifi)) "Wi-Fi stays connected \u{b7} Usually 15\u{2013}45 seconds" } }
            @if attention {
                @if let Some(href) = check { p.actions { a.btn.btn-quiet.sm href=(href) { "Check again" } } }
            }
            @if let Some((href, label)) = back {
                p.actions { a.btn.btn-quiet[progress] href=(href) { "Back to " (label) } }
            }
        }
    }
}

// --- The list page ---

const SWDL: &[Topic] = &[Topic::EsimSwitch, Topic::EsimDownload];
const CONN: &[Topic] = &[Topic::EsimSwitch, Topic::Modem, Topic::Signal];
const DLT: &[Topic] = &[Topic::EsimDownload];

pub const FRAGMENTS: [Fragment; 3] = [
    Fragment { id: "esim-dl", topics: DLT, render: download },
    Fragment { id: "esim-conn", topics: CONN, render: conn },
    Fragment { id: "esim-upd", topics: SWDL, render: updating },
];

fn conn(v: &Views) -> Markup {
    conn_card(&list_conn(v), "esim-conn", Some(PATH), None)
}

/// The app's `progressText` of a download.
pub fn download_text(d: &Value) -> String {
    let st = state(Some(d));
    let step = d["step"].as_str().unwrap_or("");
    let name = [&d["provider"], &d["name"]].iter().filter_map(|v| v.as_str()).find(|s| !s.trim().is_empty()).unwrap_or("eSIM profile");
    let text = match st {
        "confirm" => "Ready to install",
        "cancelling" => "Cancelling\u{2026}",
        _ if step.ends_with("_cancel_session") => "Cancelling\u{2026}",
        "done" => return format!("{} added.", fmt::truncate(name, 64)),
        "failed" => "Couldn\u{2019}t add the profile",
        "cancelled" => "Download cancelled",
        _ => match step {
            "es9p_initiate_authentication" | "es10b_authenticate_server" | "es9p_authenticate_client" => "Connecting to the operator\u{2026}",
            "es8p_metadata_parse" => "Reading profile details\u{2026}",
            "es10b_prepare_download" | "es9p_get_bound_profile_package" => "Downloading profile\u{2026}",
            "es10b_load_bound_profile_package" => "Installing on the eSIM\u{2026}",
            _ => "Preparing eSIM\u{2026}",
        },
    };
    text.to_owned()
}

/// `#esim-dl`: a download that is not idle, which Add eSIM shows (`?dl=1`:
/// also a finished one, as the app's card opens the sheet as it is); `busy`
/// while it holds the eSIM (the page's controls are dimmed).
fn download(v: &Views) -> Markup {
    let d = v.value(Topic::EsimDownload);
    let st = state(d);
    html! {
        div #esim-dl .busy[downloading(st)] {
            @if let Some(d) = d.filter(|_| st != "idle") {
                a.card.edl href=(DL) {
                    (kit::icon(if downloading(st) { Icon::ArrowDown } else { Icon::SimCard }))
                    span { b { (download_text(d)) } small.cap { "Tap to view progress and next steps" } }
                }
            }
        }
    }
}

/// `#esim-upd`: "Updating…" while a switch runs; a Refresh once the card
/// or the SIM changed after the page was loaded (the list is a snapshot).
fn updating(v: &Views) -> Markup {
    let st = state(v.value(Topic::EsimSwitch));
    let changed = changed_since(v);
    html! {
        span #esim-upd {
            @if running(st) {
                span.spin {} "Updating\u{2026}"
            } @else if changed {
                "Changed \u{b7} " a href=(PATH) { "Refresh" }
            }
        }
    }
}

/// The app's message card (`ESimMessageCard`) for a result line.
pub fn message(n: Notice) -> Markup {
    let (ok, text) = match n {
        Notice::Saved(m) => (Some(true), m.to_owned()),
        Notice::Flash(ok, m) => (ok, m),
        Notice::Failed(e) => (Some(false), format!("Failed: {}", fmt::truncate(&e, 120))),
    };
    let lower = text.to_ascii_lowercase();
    // The busy notice only (the app compares the text); any other line
    // without an outcome reads as one that could not complete.
    let busy = text == BUSY;
    let receipts = lower.contains("receipts");
    let error = ok == Some(false) || (ok.is_none() && !busy);
    let (icon, title) = if busy {
        (Icon::Clock, "eSIM in use")
    } else if receipts {
        (Icon::PaperPlane, "Operator receipts")
    } else if error {
        (Icon::Alert, "Can\u{2019}t complete this right now")
    } else {
        (Icon::Check, "Profile updated")
    };
    let guidance = if lower.contains("agent password required") {
        Some("Set the router agent password in Agent Settings, then try your action again.")
    } else if lower.contains("airplane") || lower.contains("low-power") {
        Some("Open Router Settings \u{2192} Mobile Network to check the modem mode.")
    } else if lower.contains("different esim card") {
        Some("Reloading the profiles on the card now in your router. Review the list before trying again.")
    } else if lower.contains("404") || lower.contains("not found") {
        Some("This action may need a newer router agent. Your saved profiles are unchanged.")
    } else {
        None
    };
    html! {
        section.card.emsg.ebad[error && !receipts] role=(if error { "alert" } else { "status" }) {
            span.eic { (kit::icon(icon)) }
            div {
                b { (title) }
                p { (text) }
                @if let Some(g) = guidance { p.cap { (g) } }
                @if error && !receipts {
                    p.actions { a.btn.btn-quiet.sm href=(PATH) { "Reload profiles" } }
                    p.cap { "Reloading only reads the list. It doesn\u{2019}t repeat your action." }
                }
            }
        }
    }
}

/// The list read's failure, as the app words it.
fn list_error(inv: &Inventory) -> Option<String> {
    let (kind, msg) = inv.error.as_ref()?;
    let text = match kind.as_str() {
        "sim_reloading" => "The eSIM is still settling after the switch.".to_owned(),
        "management_unavailable" => format!("The eSIM's management app isn't responding ({}).", fmt::truncate(msg, 120)),
        _ => fmt::truncate(msg, 120),
    };
    let at = if inv.observed_at > 0 { sms::clock(inv.observed_at, sms::local_offset(inv.observed_at)) } else { "an earlier time".to_owned() };
    Some(format!("{text} Showing the list read at {at}."))
}

fn error_card(text: &str) -> Markup {
    html! {
        div.eerr {
            b { (kit::icon(Icon::Refresh)) "Couldn\u{2019}t refresh profiles" }
            p.cap { (text) }
            a.btn.btn-quiet.sm href=(PATH) { "Try again" }
        }
    }
}

fn row(app: &AppState, p: &Profile) -> Markup {
    let k = key(app, &p.iccid).unwrap_or_default();
    let name = p.shown();
    let body = html! {
        span.picon { (kit::icon(Icon::SimCard)) }
        span.ptext {
            @if p.enabled { small.active { (kit::icon(Icon::Check)) "Active" } }
            b { (name) }
            small.cap { (p.subtitle()) }
            small.cap.num { (tail(&p.iccid)) }
        }
        @if p.can_enable() {
            span.pgo aria-hidden="true" { (kit::icon(Icon::Cycle)) }
        } @else if !p.enabled {
            span.plock role="img" aria-label="Unavailable" { (kit::icon(Icon::Lock)) }
        }
    };
    html! {
        div.prow.eact[p.enabled] {
            @if p.can_enable() && !k.is_empty() {
                a.pmain href=(format!("{SWITCH}?p={k}")) { (body) }
            } @else {
                div.pmain { (body) }
            }
            @if !k.is_empty() {
                details.pmenu {
                    summary aria-label=(format!("Options for {name}")) { (kit::icon(Icon::Ellipsis)) }
                    div {
                        a href=(format!("{RENAME}?p={k}")) { (kit::icon(Icon::Pencil)) "Rename" }
                        @if !p.enabled { a.danger href=(format!("{DELETE}?p={k}")) { (kit::icon(Icon::Trash)) "Delete profile" } }
                    }
                }
            }
        }
    }
}

/// "Add eSIM": quiet under the list, teal in the empty card (the app's
/// bordered and prominent buttons).
fn add_button(quiet: bool) -> Markup {
    html! { a.btn.btn-wide.btn-quiet[quiet].teal[!quiet] href=(ADD) { (kit::icon(Icon::Plus)) "Add eSIM" } }
}

/// The operator receipts card (`ESimReceiptsCard`).
fn receipts(pd: &PageData, n: i64) -> Markup {
    html! {
        section.card.erc {
            div.ehead {
                span.eic.ebl { (kit::icon(Icon::PaperPlane)) }
                div { b { "Operator receipts" } small.cap { (n) " waiting \u{b7} Retried automatically" } }
            }
            details {
                summary { "Why are these waiting?" }
                p.cap {
                    "Your eSIM sends operators a receipt when a profile is added, switched or deleted. The router retries every 10 \
                     minutes while mobile data is connected. You don\u{2019}t need to keep this screen open."
                }
            }
            form method="post" action=(RECEIPTS) {
                (pd.csrf())
                (once_field(&pd.once()))
                button.btn.btn-quiet.sm type="submit" { (kit::icon(Icon::ArrowUp)) "Send now" }
            }
        }
    }
}

/// The chip card (`ESimChipCard`): firmware, free memory, the EID masked
/// unless this page load asked for it (`?eid=1`).
fn chip(pd: &PageData, c: &Value) -> Markup {
    let firmware = text(c, "firmware");
    let free = fmt::int(&c["free_nvm_bytes"]).filter(|n| *n >= 0);
    let eid = text(c, "eid");
    let reveal = pd.param("eid").as_deref() == Some("1") && !eid.is_empty();
    let last4: String = eid.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    html! {
        details #chip .card.chip open[reveal] {
            summary {
                span.eic.ebl { (kit::icon(Icon::SimCard)) }
                span { b { "eSIM card" } small.cap { "Card details & identifiers" } }
            }
            ul.list {
                @if !firmware.is_empty() { (kit::kv_row(None, "Firmware", &firmware)) }
                @if let Some(n) = free { (kit::kv_row(None, "Free memory", &fmt::bytes(n as u64))) }
                @if !eid.is_empty() {
                    li.it.eid {
                        span { (kit::icon(Icon::LockShield)) "EID" }
                        @if reveal {
                            span.v.mono translate="no" { (eid) }
                            a.ic href=(format!("{PATH}#chip")) aria-label="Hide EID" { (kit::icon(Icon::EyeSlash)) }
                        } @else {
                            span.v.mono { "\u{2022}\u{2022}\u{2022}\u{2022} \u{2022}\u{2022}\u{2022}\u{2022} \u{2022}\u{2022}\u{2022}\u{2022} \u{2026}" (last4) }
                            a.ic href=(format!("{PATH}?eid=1#chip")) aria-label="Reveal EID" { (kit::icon(Icon::Eye)) }
                        }
                    }
                }
            }
        }
    }
}

/// While a switch reloads the SIM the agent answers with the list read
/// before it: once the enable went through (no error noted), its target is
/// the active one, as the app shows it after the enable's `enabled_iccid`.
fn as_switched(inv: &mut Inventory, v: &Views) {
    let Some(sw) = v.value(Topic::EsimSwitch).filter(|s| state(Some(s)) == "reloading" && s["note"].is_null()) else { return };
    let Some(t) = sw["target_iccid"].as_str() else { return };
    if inv.profiles.iter().any(|p| crate::esim::same_iccid(&p.iccid, t)) {
        for p in &mut inv.profiles {
            p.enabled = crate::esim::same_iccid(&p.iccid, t);
        }
    }
}

/// eSIM Profiles.
pub fn page(pd: &PageData) -> Markup {
    let v = pd.views;
    let read = pd.reads.first();
    let mut inv = pd.read(0).map(inventory).unwrap_or_default();
    remember(v, &inv);
    as_switched(&mut inv, v);
    let failed = match read {
        Some(Err(e)) => Some(format!("Couldn't read eSIM profiles: {}", fmt::truncate(e, 120))),
        _ => list_error(&inv),
    };
    let n = inv.profiles.len();
    let count = if inv.updating {
        "Updating\u{2026}".to_owned()
    } else if failed.is_some() && n == 0 {
        "Not read yet".to_owned()
    } else if n == 1 {
        "1 profile".to_owned()
    } else {
        format!("{n} profiles")
    };
    let active = inv.active();
    let others: Vec<&Profile> = inv.profiles.iter().filter(|p| !p.enabled).collect();
    html! {
        div.esim.none[n == 0] {
            (render::live_snapshot(PATH))
            @if let Some(m) = &pd.notice { (m) }
            (download(v))
            (conn(v))
            div.ehdr {
                h2 { "On your eSIM" }
                (updating(v))
                span.cnt { (count) }
            }
            @if inv.observed_at > 0 {
                p.cap.last { "Last read " (sms::clock(inv.observed_at, sms::local_offset(inv.observed_at))) }
            }
            @if let Some(e) = &failed { (error_card(e)) }
            @if n == 0 {
                @if failed.is_none() && pd.notice.is_none() && !downloading(state(v.value(Topic::EsimDownload))) {
                    div.card.eempty {
                        span.eic.ebl { (kit::icon(Icon::SimCard)) }
                        b { "No profiles yet" }
                        p.cap { "Add one with the QR code from your operator." }
                        (add_button(false))
                    }
                }
            } @else {
                @if let Some(a) = active { (row(pd.app, a)) }
                @if !others.is_empty() {
                    @if active.is_some() { h2.esub { "Switch to another profile" } }
                    @for p in &others { (row(pd.app, p)) }
                }
                (add_button(true))
                ul.hints {
                    li { (kit::icon(Icon::Cycle)) "Select a profile to switch to it. Use \u{2022}\u{2022}\u{2022} for Rename and Delete." }
                    li { (kit::icon(Icon::Wifi)) "Switching pauses internet for about a minute. Your router\u{2019}s Wi-Fi stays on." }
                }
            }
            @if let Some(r) = inv.pending_receipts.filter(|r| *r > 0) { (receipts(pd, r)) }
            @if let Some(c) = pd.read(1).filter(|c| !text(c, "eid").is_empty() || !text(c, "firmware").is_empty()) { (chip(pd, c)) }
        }
    }
}

// --- The switch, delete and rename pages ---

/// The profile a page is about (`?p=<key>`, or `?new=1`: the one Add eSIM
/// just downloaded, its "Switch to it now"), from its read of the list.
enum Target {
    Read(String),
    Gone,
    Found(Inventory, String),
}

fn target_of(pd: &PageData) -> Target {
    let k = pd.query.and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("p="))).filter(|k| is_op_id(k));
    let new = k.is_none() && pd.param("new").as_deref() == Some("1");
    match (pd.reads.first(), k) {
        (Some(Err(e)), Some(_)) => Target::Read(e.clone()),
        (Some(Err(e)), None) if new => Target::Read(e.clone()),
        (Some(Ok(v)), Some(k)) => Target::Found(inventory(v), k.to_owned()),
        (Some(Ok(v)), None) if new => {
            let inv = inventory(v);
            match downloaded(pd.app, pd.views, &inv) {
                Some(k) => Target::Found(inv, k),
                None => Target::Gone,
            }
        }
        _ => Target::Gone,
    }
}

/// The key of the profile the last download added, if it is in the list.
fn downloaded(app: &AppState, v: &Views, inv: &Inventory) -> Option<String> {
    let d = v.value(Topic::EsimDownload).filter(|d| state(Some(d)) == "done")?;
    let iccid = d["iccid"].as_str()?;
    let p = inv.profiles.iter().find(|p| crate::esim::same_iccid(iccid, &p.iccid))?;
    key(app, &p.iccid)
}

/// 404 for a profile that is gone (or no key), 503 when the list cannot be
/// read.
pub fn status(pd: &PageData) -> u16 {
    match target_of(pd) {
        Target::Read(_) => 503,
        Target::Found(inv, k) if find(pd.app, &inv, &k).is_some() => 200,
        _ => 404,
    }
}

fn nothing(pd: &PageData, title: &str, lines: &[&str]) -> Markup {
    action::ask(pd.csrf(), &Ask { title, lines, action: PATH, hidden: &[], label: None, danger: false, cancel: PATH })
}

pub const REMOTE_SWITCH: &str = "The router does not switch back by itself if the new profile gets no data.";

/// "Switch to <name>?" (the app's dialog), with its warnings and, for a
/// remote browser, the remote lines.
pub fn switch_page(pd: &PageData) -> Markup {
    let (inv, k) = match target_of(pd) {
        Target::Read(e) => return nothing(pd, "Couldn\u{2019}t read eSIM profiles", &[&fmt::truncate(&e, 120)]),
        Target::Gone => return nothing(pd, GONE, &[]),
        Target::Found(inv, k) => (inv, k),
    };
    remember(pd.views, &inv);
    let Some(p) = find(pd.app, &inv, &k) else { return nothing(pd, GONE, &[]) };
    let name = p.shown();
    if p.enabled {
        return nothing(pd, &format!("{name} is already active."), &[]);
    }
    if !p.can_enable() {
        return nothing(pd, &format!("{name} can\u{2019}t be switched to here."), &["Only operator profiles can be switched to."]);
    }
    if blocked(pd.views) {
        return nothing(pd, BUSY, &[]);
    }
    let mut lines = switch_lines(&inv, p);
    if pd.remote {
        lines.push(super::confirm::REMOTE.to_owned());
        lines.push(REMOTE_SWITCH.to_owned());
    }
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (title, label, once) = (format!("Switch to {name}?"), format!("Switch to {name}"), pd.once());
    let hidden = [("p", k.as_str()), ("once", once.as_str())];
    action::ask(pd.csrf(), &Ask { title: &title, lines: &lines, action: SWITCH, hidden: &hidden, label: Some(&label), danger: false, cancel: PATH })
}

/// "Delete <name> permanently?" (inactive profiles only).
pub fn delete_page(pd: &PageData) -> Markup {
    let (inv, k) = match target_of(pd) {
        Target::Read(e) => return nothing(pd, "Couldn\u{2019}t read eSIM profiles", &[&fmt::truncate(&e, 120)]),
        Target::Gone => return nothing(pd, GONE, &[]),
        Target::Found(inv, k) => (inv, k),
    };
    let Some(p) = find(pd.app, &inv, &k) else { return nothing(pd, GONE, &[]) };
    let name = p.shown();
    if p.enabled {
        return nothing(pd, &format!("{name} is active."), &["Switch to another profile before deleting it."]);
    }
    if blocked(pd.views) {
        return nothing(pd, BUSY, &[]);
    }
    let from = if p.provider.is_empty() { name } else { p.provider.as_str() };
    let line = format!("This can\u{2019}t be undone. You\u{2019}d need a new QR code from {from} to get this eSIM back.");
    let (title, label, once) = (format!("Delete {name} permanently?"), format!("Delete {name}"), pd.once());
    let hidden = [("p", k.as_str()), ("once", once.as_str())];
    action::ask(pd.csrf(), &Ask { title: &title, lines: &[&line], action: DELETE, hidden: &hidden, label: Some(&label), danger: true, cancel: PATH })
}

/// A nickname as the app checks it: one line, at most 64 UTF-8 bytes
/// (trimmed; empty clears it).
fn nick_ok(s: &str) -> bool {
    s.trim().len() <= NICK_MAX && !s.chars().any(char::is_control)
}

/// "Rename profile" (the app's sheet).
pub fn rename_page(pd: &PageData) -> Markup {
    let (inv, k) = match target_of(pd) {
        Target::Read(e) => return nothing(pd, "Couldn\u{2019}t read eSIM profiles", &[&fmt::truncate(&e, 120)]),
        Target::Gone => return nothing(pd, GONE, &[]),
        Target::Found(inv, k) => (inv, k),
    };
    let Some(p) = find(pd.app, &inv, &k) else { return nothing(pd, GONE, &[]) };
    let typed = pd.form.and_then(|f| f.value("nickname")).unwrap_or(&p.nickname);
    let err = pd.form.and_then(|f| f.error("nickname"));
    let valid = err.is_none() && nick_ok(typed);
    let bytes = typed.trim().len();
    let placeholder = if p.name.is_empty() { &p.provider } else { &p.name };
    let footer = html! {
        p #nick-help .foot.ebytes.c-red[!valid] {
            span { (if valid { "Leave empty to use the operator\u{2019}s name." } else { "Use one line, up to 64 bytes." }) }
            span.num { (bytes) " / 64 bytes" }
        }
        p.foot { "Names with emoji or accented letters may use more than one byte per character." }
    };
    html! {
        @if let Some(n) = &pd.notice { (n) }
        form.rf method="post" action=(RENAME) {
            (pd.csrf())
            (once_field(&pd.once()))
            input type="hidden" name="p" value=(k.as_str());
            (kit::section(Some("Nickname"), Some(footer), html! {
                li.it { (kit::icon(Icon::SimCard)) b { (p.shown()) } }
                li.it.fld {
                    label.sr for="nickname" { "Nickname" }
                    input #nickname type="text" name="nickname" value=(typed) placeholder=(placeholder) maxlength="64"
                        autocomplete="off" spellcheck="false" aria-describedby="nick-help" aria-invalid=[(!valid).then_some("true")];
                }
            }))
            (kit::section(None, None, html! { li { button.it.cv.snd type="submit" { "Save" } } }))
        }
    }
}

// --- The switch's operation page ---

/// A switch, followed on `esim_switch` (and `signal` for the connected
/// card): its steps and the app's outcome notes. It never switches back.
pub static SWITCH_OP: OpSpec = OpSpec {
    titles: ["Switching profile", "Mobile data connected", "Connection status unavailable", "Connection needs attention"],
    steps: &["Switch profile", "Reload SIM", "Connect to network"],
    topics: &[Topic::EsimSwitch, Topic::Signal],
    status: op_status,
    back: (PATH, "eSIM Profiles"),
    tab: Tab::Router,
    timeout_ok: false,
    view: Some(OpView { title: op_title, progress: op_progress }),
};

/// Without a switch to show (the agent restarted): unknown after 15 s.
const OP_UNKNOWN: Duration = Duration::from_secs(15);

fn op_status(_: &str, elapsed: Duration, v: &Views) -> OpStatus {
    let sw = v.value(Topic::EsimSwitch);
    let st = state(sw);
    match switch_phase(st) {
        Phase::Switching => OpStatus::Pending { step: 0 },
        Phase::Reloading => OpStatus::Pending { step: 1 },
        Phase::Registering => OpStatus::Pending { step: 2 },
        Phase::Connected => OpStatus::Done { msg: Phase::Connected.detail() },
        Phase::Waiting | Phase::Unavailable => {
            OpStatus::Failed { msg: switch_note(st, sw.and_then(|s| s["reason"].as_str())).unwrap_or(Phase::Waiting.detail()) }
        }
        _ if elapsed < OP_UNKNOWN => OpStatus::Pending { step: 0 },
        _ => OpStatus::TimedOut { msg: Phase::Unavailable.detail() },
    }
}

fn op_phase(v: &Views) -> Phase {
    match v.op.as_ref().map(|op| op.status(v)) {
        Some(OpStatus::TimedOut { .. }) => Phase::Unavailable,
        _ => match switch_phase(state(v.value(Topic::EsimSwitch))) {
            Phase::Checking => Phase::Switching,
            p => p,
        },
    }
}

fn op_title(v: &Views) -> &'static str {
    op_phase(v).title(true)
}

/// The switch's card; "Check again" (when it needs attention) reloads the
/// operation page.
fn op_progress(v: &Views) -> Markup {
    let mut c = following(v, op_phase(v));
    c.phase = op_phase(v);
    conn_card(&c, "op-progress", Some(""), Some((PATH, "eSIM Profiles")))
}

// --- Posts ---

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Post {
    Switch,
    Delete,
    Rename,
    Receipts,
}

impl Post {
    pub fn of(path: &str) -> Option<Post> {
        [Post::Switch, Post::Delete, Post::Rename, Post::Receipts].into_iter().find(|k| k.path() == path)
    }

    pub fn path(self) -> &'static str {
        match self {
            Post::Switch => SWITCH,
            Post::Delete => DELETE,
            Post::Rename => RENAME,
            Post::Receipts => RECEIPTS,
        }
    }

    fn call(self) -> &'static str {
        match self {
            Post::Switch => ENABLE,
            Post::Delete => DELETE_API,
            Post::Rename => NICKNAME,
            Post::Receipts => PROCESS,
        }
    }
}

/// How long the app waits before reading what the receipts came to.
const RECEIPTS_WAIT: Duration = if cfg!(test) { Duration::from_millis(30) } else { Duration::from_secs(8) };

/// The agent's "one card command at a time" refusals.
pub fn busy_error(status: u16, e: &str) -> bool {
    status == 409 && ["busy", "reloading", "retrying"].iter().any(|w| e.contains(w))
}

/// A failed call as a result line: busy, or the agent's error (`prefix`
/// first, as the app's "Switch failed: …").
fn failure(status: u16, e: &str, prefix: &str) -> (Option<bool>, String) {
    if busy_error(status, e) {
        (None, BUSY.to_owned())
    } else {
        (Some(false), format!("{prefix}{}", fmt::truncate(e, 120)))
    }
}

/// 303 to eSIM Profiles with `line` for this session, kept under the
/// post's token when it sent something (`token`; a replay finds it), else
/// under a random id.
fn line(app: &AppState, flash: &Flash, owner: SessionId, token: Option<&str>, line: (Option<bool>, String)) -> Response {
    let id = if let Some(t) = token.filter(|t| is_op_id(t) && flash.get(t, owner, app.auth.now()).is_none()) {
        t.to_owned()
    } else {
        match app.auth.random_hex(8) {
            Ok(id) => id,
            Err(_) => return session::see_other(PATH),
        }
    };
    flash.insert(id.clone(), owner, line, app.auth.now());
    session::see_other(&format!("{PATH}?r={id}"))
}

/// The list as read now, and the profile of `k`; a line when the post must
/// send nothing (the token goes back).
fn current(app: &AppState, names: &Names, k: &str) -> Result<(Inventory, Profile), (Option<bool>, String)> {
    let inv = super::api_get(app, PROFILES).map_err(|e| (Some(false), format!("Couldn't read eSIM profiles: {}", fmt::truncate(&e, 120))))?;
    let inv = inventory(&inv);
    names.remember(&inv);
    let p = find(app, &inv, k).cloned().ok_or((Some(false), GONE.to_owned()))?;
    Ok((inv, p))
}

/// A switch or a download holds the eSIM now (read, not cached).
pub fn busy_now(app: &AppState) -> bool {
    blocked(&super::read_topics(app, &[Topic::EsimSwitch, Topic::EsimDownload]))
}

fn switch_generation(app: &AppState) -> u64 {
    generation(super::read_topics(app, &[Topic::EsimSwitch]).value(Topic::EsimSwitch))
}

/// Where a post's work leads.
enum Done {
    Line(Option<bool>, String),
    Op,
    /// Back to the list, no line (nothing to change).
    Back,
    /// Receipts handed to the agent: wait, then count them.
    Receipts,
}

/// The work of a confirmed post, in its gate task: the checks, then the
/// call (the claim disarmed right before it).
fn work(app: &AppState, names: &Names, k: Post, key: &str, nick: &str, claim: &mut crate::web::store::Claim) -> Done {
    let line = |(ok, text): (Option<bool>, String)| Done::Line(ok, text);
    if k == Post::Receipts {
        if busy_now(app) {
            return line((None, BUSY.to_owned()));
        }
        claim.disarm();
        return match super::call(app, Method::Post, PROCESS, &json!({})) {
            Ok(_) => Done::Receipts,
            Err((status, e)) => line(failure(status, &e, "")),
        };
    }
    let (_, p) = match current(app, names, key) {
        Ok(found) => found,
        Err(l) => return line(l),
    };
    let name = p.shown().to_owned();
    match k {
        Post::Switch if p.enabled => return line((Some(false), format!("{name} is already active."))),
        Post::Switch if !p.can_enable() => return line((Some(false), format!("{name} can\u{2019}t be switched to here."))),
        Post::Delete if p.enabled => return line((Some(false), "Switch to another profile before deleting the active one.".to_owned())),
        Post::Rename if nick == p.nickname => return Done::Back,
        _ => {}
    }
    if busy_now(app) {
        return line((None, BUSY.to_owned()));
    }
    let before = if k == Post::Switch { switch_generation(app) } else { 0 };
    let body = match k {
        Post::Rename => json!({"iccid": p.iccid, "nickname": nick}),
        _ => json!({"iccid": p.iccid}),
    };
    claim.disarm();
    let out = super::call(app, Method::Post, k.call(), &body);
    match (k, out) {
        (Post::Switch, Ok(_)) => Done::Op,
        // A failed or timed-out enable may still have switched: the agent
        // started verifying it (a new switch generation), so follow that.
        (Post::Switch, Err(_)) if switch_generation(app) != before => Done::Op,
        (Post::Switch, Err((status, e))) => line(failure(status, &e, "Switch failed: ")),
        (Post::Delete, Ok(d)) => {
            let note = if d["operator_notified"] == false { " The operator will be notified on the next eSIM change." } else { "" };
            line((Some(true), format!("{name} deleted.{note}")))
        }
        (Post::Rename, Ok(_)) if nick.is_empty() => line((Some(true), "Nickname cleared.".to_owned())),
        (Post::Rename, Ok(_)) => line((Some(true), format!("Renamed to {nick}."))),
        (_, Err((status, e))) => line(failure(status, &e, "")),
        (_, Ok(_)) => Done::Back,
    }
}

/// `POST` of Switch, Delete, Rename or Send now: the session, the form and
/// its CSRF token, the fields, the single-use token, then the work (slow
/// lane: card commands).
pub async fn post(ctx: Ctx, req: Request, k: Post) -> Response {
    let nonce = CspNonce::of(&req);
    let mut r = Req { query: None, ..Req::of(&ctx, &req) };
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, PATH, None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&nonce, PATH);
    }
    let key = key_of(form.get("p"));
    let key = match (k, key) {
        (Post::Receipts, _) => String::new(),
        (_, Some(key)) => key,
        (_, None) => return render::bad_request(&nonce),
    };
    // The questions are their pages: a post that did not come from one goes there.
    if matches!(k, Post::Switch | Post::Delete) && form.get("confirm") != Some("1") {
        return session::see_other(&format!("{}?p={key}", k.path()));
    }
    let nick = match k {
        Post::Rename => match nickname(&form) {
            Ok(n) => n,
            Err(fs) => {
                r.query = Some(format!("p={key}"));
                r.typed = Some(fs);
                return screen::show(&ctx, p, r, &crate::web::tabs::router::ESIM_RENAME, None, 400).await;
            }
        },
        _ => String::new(),
    };
    let token = form.get("once").unwrap_or("").to_owned();
    let digest = job::digest(&[k.path(), &key, &nick]);
    let (flash, ops, names, owner) = (Arc::clone(&ctx.flash), Arc::clone(&ctx.ops), Arc::clone(&ctx.esim_names), p.id);
    let handed = Arc::new(Mutex::new(false));
    let (handed_w, token_w) = (Arc::clone(&handed), token.clone());
    let job: OnceJob = Box::new(move |app: &AppState, claim| {
        let done = work(app, &names, k, &key, &nick, claim);
        if claim.sent() {
            claim.set(match &done {
                Done::Line(Some(false), text) => Token::Failed(claim.digest(), 409, text.clone()),
                _ => Token::Sent(claim.digest()),
            });
        }
        match done {
            Done::Line(ok, text) => line(app, &flash, owner, claim.sent().then_some(token_w.as_str()), (ok, text)),
            Done::Back => session::see_other(PATH),
            Done::Op => {
                let now = app.auth.now();
                if is_op_id(&token_w) && ops.get(&token_w, owner, now).is_none() {
                    ops.insert(token_w.clone(), owner, Op { spec: &SWITCH_OP, started: now, arg: "" }, now);
                }
                session::see_other(&format!("/ui/op/{token_w}"))
            }
            Done::Receipts => {
                *handed_w.lock().unwrap_or_else(PoisonError::into_inner) = true;
                session::see_other(PATH)
            }
        }
    });
    let note = |l: (Option<bool>, String)| line(&ctx.app, &ctx.flash, owner, None, l);
    let resp = match job::once(&ctx, owner, &nonce, &token, digest, Lane::Slow, SETTLE_SLOW, job).await {
        Sent::Ran(resp) => resp,
        // A replay: what the first post came to.
        Sent::Done(_) | Sent::Unknown if ctx.ops.get(&token, owner, ctx.app.auth.now()).is_some() => {
            return session::see_other(&format!("/ui/op/{token}"));
        }
        Sent::Done(_) if ctx.flash.get(&token, owner, ctx.app.auth.now()).is_some() => {
            return session::see_other(&format!("{PATH}?r={token}"));
        }
        Sent::Done(Ok(())) => return session::see_other(PATH),
        Sent::Done(Err((_, e))) => return note((Some(false), e)),
        Sent::Expired => return note((Some(false), "This page expired, so nothing was sent. Try again.".to_owned())),
        Sent::Busy => return note((Some(false), "Failed: the router is busy. Try again in a few seconds.".to_owned())),
        Sent::Unknown => return note((Some(false), "The router may still be working on it. Check before trying again.".to_owned())),
    };
    if !*handed.lock().unwrap_or_else(PoisonError::into_inner) {
        return resp;
    }
    // Send now: the agent sends in the background; the app reads the count
    // after 8 s.
    tokio::time::sleep(RECEIPTS_WAIT).await;
    let counted = Arc::new(Mutex::new(None));
    let out = Arc::clone(&counted);
    let read: job::Job = Box::new(move |app: &AppState| {
        let l = match super::api_get(app, NOTIFICATIONS) {
            Ok(d) if fmt::int(&d["count"]) == Some(0) => (Some(true), "Operator receipts sent.".to_owned()),
            Ok(_) => (Some(false), RETRYING.to_owned()),
            Err(e) => (Some(false), fmt::truncate(&e, 120)),
        };
        *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(l);
        Response::default()
    });
    // The router busy: the count is unknown, the receipts are still sent.
    let _ = job::try_run(&ctx, Lane::Slow, &nonce, read).await;
    let counted = counted.lock().unwrap_or_else(PoisonError::into_inner).take();
    line(&ctx.app, &ctx.flash, owner, Some(&token), counted.unwrap_or((Some(false), RETRYING.to_owned())))
}

const RETRYING: &str = "Some receipts couldn't be sent yet. The router keeps retrying.";

/// The posted nickname, trimmed and checked as the app does.
fn nickname(form: &Form) -> Result<String, FormState> {
    let raw = form.get("nickname").unwrap_or("");
    if nick_ok(raw) {
        return Ok(raw.trim().to_owned());
    }
    let shown: String = raw.chars().filter(|c| !c.is_control()).take(80).collect();
    Err(FormState { values: vec![("nickname", shown)], errors: vec![("nickname", "Use one line, up to 64 bytes.".to_owned())] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boottime::BootInstant;
    use crate::events::hub::SlotView;

    fn views(slots: Vec<(Topic, Value)>) -> Views {
        let now = BootInstant::ZERO;
        let s = slots
            .into_iter()
            .map(|(t, v)| (t, SlotView { version: 1, at: now, value_at: now, value: Some(Arc::new(v)), error: None }))
            .collect();
        Views::new(now, s)
    }

    fn profile(nick: &str, name: &str, provider: &str, enabled: bool) -> Profile {
        Profile {
            iccid: "8900000000000000101".into(),
            provider: provider.into(),
            name: name.into(),
            nickname: nick.into(),
            enabled,
            class: "operational".into(),
            warnings: vec![],
        }
    }

    /// The app's names and subtitles.
    #[test]
    fn names_follow_the_app() {
        let p = profile("", "Klook_app", "Travel - PHL", false);
        assert_eq!((p.shown(), p.subtitle()), ("Klook_app", "Travel - PHL"));
        let p = profile("GOMO u60", "", "GOMO", true);
        assert_eq!((p.shown(), p.subtitle()), ("GOMO u60", "GOMO"));
        let p = profile("", "", "SMART", true);
        assert_eq!((p.shown(), p.subtitle()), ("SMART", "Selected on your router"));
        let p = profile("", "", "", false);
        assert_eq!((p.shown(), p.subtitle(), p.can_enable()), ("Unnamed profile", "Ready to switch", true));
        assert!(!Profile { class: "test".into(), ..p }.can_enable());
        assert_eq!(tail("8900000000000000101F"), "\u{2026}0101");
    }

    /// The dialog's words and warnings (an unknown code is skipped).
    #[test]
    fn switch_lines_carry_the_warnings() {
        let mut p = profile("", "Travel", "Example Travel", false);
        p.warnings = vec!["manual_apn".into(), "roaming_off".into(), "new_code".into()];
        let inv = Inventory { profiles: vec![profile("Home", "", "", true), p.clone()], manual_apn: Some("example.apn".into()), ..Inventory::default() };
        let l = switch_lines(&inv, &p);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0], "The router disconnects from Home and connects with Travel. Internet is down for about a minute. Wi-Fi stays on.");
        assert!(l[1].starts_with("The router is set to the manual APN \u{201c}example.apn\u{201d}. If Travel needs"));
        assert!(l[2].starts_with("Travel looks like a travel (roaming) profile and Data Roaming is off"));
        // No manual APN known: that warning is left out; no active profile.
        let inv = Inventory { profiles: vec![p.clone()], ..Inventory::default() };
        let l = switch_lines(&inv, &p);
        assert!(l[0].starts_with("The router disconnects from the current profile and") && l.len() == 2);
    }

    /// Switch states as the app's phases, notes and the operation's status.
    #[test]
    fn switch_states_follow_the_app() {
        let at = |s: u64| Duration::from_secs(s);
        let sw = |state: &str, reason: Value| views(vec![(Topic::EsimSwitch, json!({"state": state, "reason": reason, "target_iccid": "8900000000000000909"}))]);
        assert_eq!(op_status("", at(1), &sw("card_busy", Value::Null)), OpStatus::Pending { step: 0 });
        assert_eq!(op_status("", at(1), &sw("reloading", Value::Null)), OpStatus::Pending { step: 1 });
        assert_eq!(op_status("", at(1), &sw("modem_confirmed", Value::Null)), OpStatus::Pending { step: 2 });
        assert!(matches!(op_status("", at(30), &sw("connected", Value::Null)), OpStatus::Done { .. }));
        assert_eq!(
            op_status("", at(150), &sw("no_data", json!("roaming_off"))),
            OpStatus::Failed { msg: "This profile roams on a local network and Data Roaming is off. Turn on Data Roaming in Mobile Network." }
        );
        assert_eq!(op_status("", at(150), &sw("no_data", json!("data_off"))), OpStatus::Failed { msg: "The profile is active, but Mobile Data is off. Turn it on in Mobile Network." });
        assert_eq!(
            op_status("", at(240), &sw("not_applied", json!("card_busy"))),
            OpStatus::Failed { msg: "The eSIM stayed busy and didn't switch; the previous profile is still active. Try again in a few minutes." }
        );
        assert_eq!(op_status("", at(5), &sw("idle", Value::Null)), OpStatus::Pending { step: 0 });
        assert!(matches!(op_status("", at(15), &sw("idle", Value::Null)), OpStatus::TimedOut { .. }));
        assert_eq!(switch_note("card_busy", None), Some("The eSIM is busy and refused the switch for now. The router keeps trying for up to 4 minutes."));
        assert_eq!(switch_phase("not_applied").title(true), "Switch needs attention");
        assert_eq!(switch_phase("no_data").title(true), "Connection needs attention");
        // The card: steps, In progress / Done, the note, no way to switch back.
        let mut v = sw("modem_confirmed", Value::Null);
        v.op = Some(Op { spec: &SWITCH_OP, started: BootInstant::ZERO, arg: "" });
        let html = op_progress(&v).into_string();
        for want in ["<b>Registering on network</b>", "Switch profile</span><small>Done</small>", "Reload SIM</span><small>Done</small>",
            "Connect to network</span><small>In progress</small>", "The new profile is loaded. Waiting for a mobile data connection.",
            "Wi-Fi stays connected · Usually 15–45 seconds", "Switch to …0909", "href=\"/ui/router/esim\">Back to eSIM Profiles</a>"] {
            assert!(html.contains(want), "{want} in {html}");
        }
        assert!(html.starts_with("<section class=\"card econn esw busy\" id=\"op-progress\">"), "{html}");
        assert_eq!(op_title(&v), "Registering on network");
        assert!(!html.to_ascii_lowercase().contains("switch back") && !html.contains("8900000000000000909"));
    }

    /// The list's card without a switch: the modem's data connection.
    #[test]
    fn list_card_reads_the_data_connection() {
        let v = views(vec![
            (Topic::Modem, json!({"data": {"connect_status": "ipv4_ipv6_connected"}})),
            (Topic::Signal, json!({"network_provider": "Example", "network_type": "LTE", "signalbar": "3"})),
            (Topic::EsimSwitch, json!({"state": "connected", "generation": 4})),
        ]);
        let mut v = v;
        v.arg = arg(&v, None);
        let html = conn(&v).into_string();
        assert!(html.contains("<b>Mobile data connected</b><small class=\"cap\">Example · 4G</small>"), "{html}");
        assert!(html.contains("aria-label=\"Signal 3 of 5\"") && !html.contains("esteps"), "{html}");
        // A switch since the page was loaded: followed.
        let mut later = views(vec![(Topic::EsimSwitch, json!({"state": "reloading", "generation": 5}))]);
        later.arg = v.arg;
        let html = conn(&later).into_string();
        assert!(html.contains("<b>Reloading SIM</b>") && html.contains("class=\"card econn busy\""), "{html}");
        assert!(updating(&later).into_string().contains("Updating"));
        later.set(Topic::EsimSwitch, SlotView { version: 2, at: BootInstant::ZERO, value_at: BootInstant::ZERO, value: Some(Arc::new(json!({"state": "no_data", "generation": 5}))), error: None });
        assert!(conn(&later).into_string().contains("mobile data hasn&#39;t connected") || conn(&later).into_string().contains("mobile data hasn't connected"));
        assert!(updating(&later).into_string().contains("Changed · <a href=\"/ui/router/esim\">Refresh</a>"));
        // Offline and unread.
        let off = views(vec![(Topic::Modem, json!({"data": {"connect_status": "disconnected"}}))]);
        assert!(conn(&off).into_string().contains("<b>Mobile data offline</b>"));
        let none = views(vec![(Topic::Modem, json!({"data": {"connect_status": ""}}))]);
        let html = conn(&none).into_string();
        assert!(html.contains("<b>Connection status unavailable</b>") && html.contains("Check again"), "{html}");
    }

    /// The app's message card titles and guidance.
    #[test]
    fn message_cards_follow_the_app() {
        let html = message(Notice::Flash(None, BUSY.into())).into_string();
        assert!(html.contains("<b>eSIM in use</b>") && !html.contains("Reload profiles"), "{html}");
        let html = message(Notice::Flash(Some(false), "Switch failed: the modem is in airplane / low-power mode".into())).into_string();
        assert!(html.contains("Can’t complete this right now") && html.contains("Open Router Settings → Mobile Network") && html.contains("Reload profiles"));
        let html = message(Notice::Flash(Some(false), "Some receipts couldn't be sent yet. The router keeps retrying.".into())).into_string();
        assert!(html.contains("<b>Operator receipts</b>") && !html.contains("Reload profiles"));
        assert!(message(Notice::Flash(Some(true), "Renamed to X.".into())).into_string().contains("<b>Profile updated</b>"));
        // Only the busy notice is "eSIM in use" (the app compares the text).
        let html = message(Notice::Flash(None, "The router may still be working on it. Check before trying again.".into())).into_string();
        assert!(html.contains("<b>Can’t complete this right now</b>") && html.contains("Reload profiles") && !html.contains("eSIM in use"), "{html}");
    }

    /// The switch's operation page offers "Check again" (a reload of
    /// itself) when the card needs attention, as the app's card does.
    #[test]
    fn the_switch_card_offers_check_again() {
        let mut v = views(vec![(Topic::EsimSwitch, json!({"state": "not_applied", "target_iccid": "8900000000000000909"}))]);
        v.op = Some(Op { spec: &SWITCH_OP, started: BootInstant::ZERO, arg: "" });
        let html = op_progress(&v).into_string();
        assert!(html.contains("<b>Switch needs attention</b>") && html.contains("<a class=\"btn btn-quiet sm\" href=\"\">Check again</a>"), "{html}");
        let mut v = views(vec![(Topic::EsimSwitch, json!({"state": "reloading", "target_iccid": "8900000000000000909"}))]);
        v.op = Some(Op { spec: &SWITCH_OP, started: BootInstant::ZERO, arg: "" });
        assert!(!op_progress(&v).into_string().contains("Check again"));
    }

    /// Names come from the page's store (per router), else the last digits.
    #[test]
    fn switch_targets_are_named_from_the_store() {
        let inv = Inventory { profiles: vec![Profile { iccid: "8900000000000000909".into(), ..profile("Travel", "", "", false) }], ..Inventory::default() };
        let mut v = views(vec![]);
        assert_eq!(target(&v, "8900000000000000909F"), "Switch to …0909");
        v.esim_names = Some(Arc::new(Names::default()));
        remember(&v, &inv);
        remember(&v, &Inventory::default());
        assert_eq!(target(&v, "8900000000000000909F"), "Switch to Travel");
    }

    /// Nicknames: one line, at most 64 bytes after trimming.
    #[test]
    fn nicknames_are_checked_in_bytes() {
        assert!(nick_ok("") && nick_ok(&"a".repeat(64)) && nick_ok(&format!("  {}  ", "a".repeat(64))));
        assert!(!nick_ok(&"a".repeat(65)) && !nick_ok("two\nlines") && !nick_ok("tab\there"));
        assert!(nick_ok(&"é".repeat(32)) && !nick_ok(&"é".repeat(33)));
    }

    #[test]
    fn download_texts_follow_the_app() {
        assert_eq!(download_text(&json!({"state": "confirm"})), "Ready to install");
        assert_eq!(download_text(&json!({"state": "running", "step": "es9p_authenticate_client"})), "Connecting to the operator…");
        assert_eq!(download_text(&json!({"state": "done", "provider": "Example", "name": "Plan"})), "Example added.");
        assert_eq!(download_text(&json!({"state": "done"})), "eSIM profile added.");
        assert_eq!(download_text(&json!({"state": "running", "step": "es10b_load_bound_profile_package"})), "Installing on the eSIM…");
    }
}
