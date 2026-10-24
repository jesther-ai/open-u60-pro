//! Router › SIM Card (iOS `SIMView` and its sheets `EnterPinSheet`,
//! `ChangePinSheet`, `EnterPukSheet`, `UnlockSIMSheet`).
//!
//! - **SIM Card** is live from `sim` (re-read after SIM card events, each
//!   SIM identity phase and, every 2 s for 20 s, after a PIN, PUK or SIM
//!   lock action): the app's banners ("SIM PIN Required", "SIM PUK
//!   Required"), "SIM Card" (Status, ICCID, IMSI, MSISDN, SPN, MCC/MNC,
//!   Operator, SIM Slot) and "PIN Management". The SIM identity comes as
//!   the reading's `sim_epoch` stamp (a page-load read of the
//!   `sim_identity` topic would replace the hub's last event with the
//!   stamp's shorter form, which API streams would then see).
//!   Identifiers are masked (owner decision; the app shows them whole):
//!   ICCID and MSISDN as their last 4 digits ("…1234"), the IMSI as its
//!   network and last 4 digits ("001/01 …1234"). "SIM Lock" (`GET
//!   /api/sim/lock-trials`) is read at page load; when the SIM identity
//!   changes while the page is open, a Refresh is offered (the app re-reads
//!   both).
//! - Each sheet is its own page with "Cancel": Enter PIN, Enable / Disable
//!   PIN Lock, Change PIN, Enter PUK, SIM Unlock. A sheet is also its own
//!   confirmation, so a PIN is typed once and posted once (never carried
//!   through a second page): what happens, the attempts left, the last
//!   attempt and, for what can cut service, the remote line stand above
//!   its button. PINs, PUKs and unlock codes are in the POST body only:
//!   never in a URL, never shown again (a refused or failed post shows
//!   empty fields), never logged. A single-use token makes a double click
//!   send once; its digest is keyed (`AuthState::opaque_key`), so the
//!   token store holds no plain hash of a PIN.
//! - A post re-reads the SIM first (not from the cache) and sends nothing
//!   when the SIM no longer fits (a PIN for a SIM that is not waiting for
//!   one, the lock turned on when it is on, no PUK or unlock attempts
//!   left, a PUK or unlock count that cannot be read) or when the attempts
//!   left differ from those the sheet was drawn for (its hidden `left`):
//!   the sheet comes back with the count and warnings as they are now, so
//!   the last attempt is never spent without its warning. Attempts are
//!   shown where the app shows them (Enter PIN, Enter PUK, SIM Unlock).

use std::sync::Arc;

use axum::extract::Request;
use axum::http::Method as HttpMethod;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::{json, Value};

use crate::auth::SessionId;
use crate::events::Topic;
use crate::handlers::AppState;
use crate::httpd::Ctx;
use crate::server::Method;
use crate::web::job::{self, OnceJob, Sent};
use crate::web::kit::{self, Icon};
use crate::web::render::{self, once_field, Fragment, Views};
use crate::web::screen::{self, FormState, Notice, PageData, Req, Screen};
use crate::web::session::{self, Form};
use crate::web::store::{Flash, Token, SETTLE_SLOW};
use crate::web::tabs::router;
use crate::web::{fmt, CspNonce};

pub const PATH: &str = "/ui/router/sim";
pub const PIN: &str = "/ui/router/sim/pin";
pub const LOCK_ON: &str = "/ui/router/sim/pin-lock/enable";
pub const LOCK_OFF: &str = "/ui/router/sim/pin-lock/disable";
pub const CHANGE: &str = "/ui/router/sim/change-pin";
pub const PUK: &str = "/ui/router/sim/puk";
pub const NCK: &str = "/ui/router/sim/nck";
pub const TRIALS: &str = "/api/sim/lock-trials";

const DASH: &str = "--";

// --- What the page shows of `GET /api/sim/info` ---

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().map_or("", str::trim)
}

/// The app's status: `sim_states`, or from `modem_main_state` when empty.
fn state(sim: &Value) -> String {
    let raw = text(sim, "sim_states");
    if !raw.is_empty() {
        return raw.to_owned();
    }
    match text(sim, "modem_main_state").to_ascii_lowercase().as_str() {
        "modem_waitpin" => "wait pin",
        "modem_waitpuk" => "wait puk",
        "modem_init_complete" => "sim ready",
        _ => "",
    }
    .to_owned()
}

/// The app's label and colour for a status.
fn status(raw: &str) -> (String, Option<&'static str>) {
    let (label, class) = match raw.to_ascii_lowercase().as_str() {
        "" | "unknown" => (DASH, None),
        "sim ready" => ("Ready", Some("c-green")),
        "sim undetected" => ("No SIM", Some("c-red")),
        "wait pin" => ("PIN Required", Some("c-orange")),
        "wait puk" => ("PUK Required", Some("c-orange")),
        "sim destroy" => ("SIM Destroyed", Some("c-red")),
        "error" | "sim_error" => ("Error", Some("c-red")),
        _ => return (raw.to_owned(), None),
    };
    (label.to_owned(), class)
}

fn pin_locked(sim: &Value) -> bool {
    text(sim, "sim_states").eq_ignore_ascii_case("wait pin") || text(sim, "modem_main_state").eq_ignore_ascii_case("modem_waitpin")
}

fn puk_locked(sim: &Value) -> bool {
    text(sim, "sim_states").eq_ignore_ascii_case("wait puk") || text(sim, "modem_main_state").eq_ignore_ascii_case("modem_waitpuk")
}

/// `pin_status` "1" (the app's; a number too).
fn pin_enabled(sim: &Value) -> bool {
    text(sim, "pin_status") == "1" || sim["pin_status"].as_i64() == Some(1)
}

/// "…" and the last 4 digits ("" with fewer).
fn tail(s: &str) -> String {
    let digits: Vec<char> = s.chars().filter(char::is_ascii_digit).collect();
    match digits.len() {
        n if n >= 4 => format!("\u{2026}{}", digits[n - 4..].iter().collect::<String>()),
        _ => String::new(),
    }
}

/// The IMSI as its network and last 4 digits ("001/01 …1111"): the card's
/// MCC and MNC when they lead it, else its first 3 and 2 digits.
fn imsi(imsi: &str, mcc: &str, mnc: &str) -> String {
    let d: String = imsi.chars().filter(char::is_ascii_digit).collect();
    if d.len() < 10 {
        return tail(&d);
    }
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let own = mcc.len() == 3 && (2..=3).contains(&mnc.len()) && digits(mcc) && digits(mnc) && d.starts_with(&format!("{mcc}{mnc}"));
    let (mcc, mnc) = if own { (mcc, mnc) } else { (&d[..3], &d[3..5]) };
    format!("{mcc}/{mnc} {}", tail(&d))
}

/// `spn_name_data`: UCS-2 hex, zero units skipped (the app's `decodeSpn`).
fn spn(hex: &str) -> String {
    let hex = hex.trim();
    if hex.is_empty() || !hex.len().is_multiple_of(4) || !hex.is_ascii() {
        return String::new();
    }
    let units: Vec<u16> = hex
        .as_bytes()
        .chunks(4)
        .filter_map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .filter(|u| *u != 0)
        .collect();
    String::from_utf16_lossy(&units).chars().filter(|c| !c.is_control()).collect()
}

fn or_dash(s: String) -> String {
    if s.is_empty() {
        DASH.to_owned()
    } else {
        s
    }
}

fn row(label: &str, value: &str, tone: Option<&str>) -> Markup {
    let class = tone.map_or_else(|| "v".to_owned(), |t| format!("v {t}"));
    html! { li.it { span { (label) } span class=(class) { (value) } } }
}

/// A row that opens a sheet (the app's labelled buttons).
fn action(href: &str, i: Icon, label: &str) -> Markup {
    html! { li { a.it.bt href=(href) { (kit::icon(i)) span { (label) } } } }
}

// --- SIM Card ---

const T: &[Topic] = &[Topic::Sim];

pub const FRAGMENTS: [Fragment; 4] = [
    Fragment { id: "sim-chg", topics: T, render: changed },
    Fragment { id: "sim-ban", topics: T, render: banners },
    Fragment { id: "sim-card", topics: T, render: card },
    Fragment { id: "sim-pin", topics: T, render: pins },
];

/// The SIM identity's epoch (the reading's `sim_epoch` stamp) plus 1 (0:
/// none), the page's `Screen::arg`.
pub fn epoch(v: &Views, _: Option<&str>) -> u64 {
    v.value(Topic::Sim).and_then(|s| s["sim_epoch"].as_u64()).map_or(0, |e| e.saturating_add(1))
}

/// `#sim-chg`: the SIM changed since the page was loaded (its SIM Lock
/// read is the old SIM's).
fn changed(v: &Views) -> Markup {
    let now = epoch(v, None);
    html! {
        div #sim-chg {
            @if v.arg != 0 && now != 0 && now != v.arg {
                p.msg.info role="status" { "The SIM changed. " a href="" { "Refresh" } }
            }
        }
    }
}

fn banner(i: Icon, tone: &str, title: &str, text: &str, href: &str, label: &str) -> Markup {
    kit::section(None, None, html! {
        li.it {
            b class=(tone) { (kit::icon(i)) }
            span { b { (title) } small { (text) } }
        }
        li { a.it.bt.snd href=(href) { (label) } }
    })
}

/// `#sim-ban`: a failed read (the app's "Failed to load SIM info: …"),
/// then the app's PIN and PUK banners with their buttons.
fn banners(v: &Views) -> Markup {
    let err = v.slot(Topic::Sim).and_then(|s| s.error.as_deref());
    let sim = v.value(Topic::Sim);
    html! {
        div #sim-ban {
            @if let Some(e) = err { p.msg.err role="alert" { "Failed to load SIM info: " (fmt::truncate(e, 120)) } }
            @if let Some(sim) = sim {
                @if pin_locked(sim) {
                    (banner(Icon::Lock, "c-orange", "SIM PIN Required", "Enter your PIN to unlock the SIM card", PIN, "Enter PIN"))
                }
                @if puk_locked(sim) {
                    (banner(Icon::LockWarn, "c-red", "SIM PUK Required", "Too many wrong PIN attempts. Enter PUK to unlock.", PUK, "Enter PUK"))
                }
            }
        }
    }
}

/// `#sim-card`: status and the masked identifiers.
fn card(v: &Views) -> Markup {
    let sim = v.value(Topic::Sim).unwrap_or(&Value::Null);
    let (label, tone) = status(&state(sim));
    let (mcc, mnc) = (text(sim, "mdm_mcc"), text(sim, "mdm_mnc"));
    let network = if mcc.is_empty() && mnc.is_empty() { DASH.to_owned() } else { format!("{mcc}/{mnc}") };
    let operator = text(sim, "Operator");
    html! {
        section.group #sim-card {
            h2 { "SIM Card" }
            ul.list {
                (row("Status", &label, tone))
                (row("ICCID", &or_dash(tail(text(sim, "sim_iccid"))), None))
                (row("IMSI", &or_dash(imsi(text(sim, "sim_imsi"), mcc, mnc)), None))
                (row("MSISDN", &or_dash(tail(text(sim, "msisdn"))), None))
                (row("SPN", &or_dash(spn(text(sim, "spn_name_data"))), None))
                (row("MCC/MNC", &network, None))
                @if !operator.is_empty() { (row("Operator", operator, None)) }
                (row("SIM Slot", &or_dash(text(sim, "current_sim_slot").to_owned()), None))
            }
        }
    }
}

/// `#sim-pin`: "PIN Management"; its sheets while the SIM is not locked.
fn pins(v: &Views) -> Markup {
    let sim = v.value(Topic::Sim).unwrap_or(&Value::Null);
    let on = pin_enabled(sim);
    let pin = fmt::int(&sim["pinnumber"]).unwrap_or(0);
    let puk = fmt::int(&sim["puknumber"]).unwrap_or(0);
    html! {
        section.group #sim-pin {
            h2 { "PIN Management" }
            ul.list {
                (row("PIN Lock", if on { "Enabled" } else { "Disabled" }, on.then_some("c-green")))
                (row("PIN Attempts", &pin.to_string(), (pin == 1).then_some("c-red")))
                (row("PUK Attempts", &puk.to_string(), (1..=3).contains(&puk).then_some("c-orange")))
                @if !pin_locked(sim) && !puk_locked(sim) {
                    @if on { (action(LOCK_OFF, Icon::LockOpen, "Disable PIN Lock")) } @else { (action(LOCK_ON, Icon::Lock, "Enable PIN Lock")) }
                    (action(CHANGE, Icon::Pencil, "Change PIN"))
                }
            }
        }
    }
}

fn trials(read: Option<&Value>) -> Option<i64> {
    read.and_then(|t| fmt::int(&t["available_trials"]))
}

pub fn page(pd: &PageData) -> Markup {
    let v = pd.views;
    let left = trials(pd.read(0)).unwrap_or(0);
    html! {
        (render::live_snapshot(PATH))
        (changed(v))
        (banners(v))
        (card(v))
        (pins(v))
        (kit::section(Some("SIM Lock"), None, html! {
            (row("Unlock Attempts", &left.to_string(), None))
            @if left > 0 { (action(NCK, Icon::LockOpen, "Enter Unlock Code")) }
        }))
    }
}

// --- The sheets ---

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Sheet {
    Pin,
    LockOn,
    LockOff,
    Change,
    Puk,
    Nck,
}

const SHEETS: [Sheet; 6] = [Sheet::Pin, Sheet::LockOn, Sheet::LockOff, Sheet::Change, Sheet::Puk, Sheet::Nck];

impl Sheet {
    pub fn path(self) -> &'static str {
        match self {
            Sheet::Pin => PIN,
            Sheet::LockOn => LOCK_ON,
            Sheet::LockOff => LOCK_OFF,
            Sheet::Change => CHANGE,
            Sheet::Puk => PUK,
            Sheet::Nck => NCK,
        }
    }

    pub fn of(path: &str) -> Option<Sheet> {
        SHEETS.into_iter().find(|s| s.path() == path)
    }

    fn screen(self) -> &'static Screen {
        match self {
            Sheet::Pin => &router::SIM_PIN,
            Sheet::LockOn => &router::SIM_LOCK_ON,
            Sheet::LockOff => &router::SIM_LOCK_OFF,
            Sheet::Change => &router::SIM_CHANGE,
            Sheet::Puk => &router::SIM_PUK,
            Sheet::Nck => &router::SIM_NCK,
        }
    }

    fn call(self) -> &'static str {
        match self {
            Sheet::Pin | Sheet::Puk => "/api/sim/pin/verify",
            Sheet::LockOn | Sheet::LockOff => "/api/sim/pin/mode",
            Sheet::Change => "/api/sim/pin/change",
            Sheet::Nck => "/api/sim/unlock",
        }
    }

    /// The app's success message.
    fn done(self) -> &'static str {
        match self {
            Sheet::Pin => "PIN verified",
            Sheet::LockOn => "PIN lock enabled",
            Sheet::LockOff => "PIN lock disabled",
            Sheet::Change => "PIN changed successfully",
            Sheet::Puk => "PUK verified, new PIN set",
            Sheet::Nck => "SIM unlocked successfully",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Sheet::LockOn => "Enable",
            Sheet::LockOff => "Disable",
            Sheet::Change => "Change PIN",
            _ => "Unlock",
        }
    }

    /// A mistake can cut the mobile connection (a PIN lock that takes
    /// effect at the next start, attempts of an unlocked SIM, the PUK's
    /// last ones): a red button, and the remote line for a remote browser.
    fn cuts(self) -> bool {
        matches!(self, Sheet::LockOn | Sheet::LockOff | Sheet::Change | Sheet::Puk)
    }
}

const UNREAD: &str = "The attempts left can't be read right now, so nothing can be sent. Try again in a moment.";

/// The attempts this sheet spends from, as read.
fn attempts(k: Sheet, sim: Option<&Value>, unlocks: Option<i64>) -> Option<i64> {
    match k {
        Sheet::Puk => sim.and_then(|s| fmt::int(&s["puknumber"])),
        Sheet::Nck => unlocks,
        _ => sim.and_then(|s| fmt::int(&s["pinnumber"])),
    }
}

/// Enter PIN once the SIM wants its PUK (the last wrong PIN locked it):
/// said with a way on to Enter PUK.
const NEEDS_PUK: &str = "The SIM is locked after too many wrong PINs and now needs its PUK.";

/// Why this sheet has nothing to do for the SIM as read. A PIN sheet
/// whose SIM could not be read leaves it to the device; a PUK or unlock
/// code is never sent without its attempts known (a wrong last one blocks
/// the SIM, or the router's network lock, for good).
fn refusal(k: Sheet, sim: Option<&Value>, unlocks: Option<i64>) -> Option<&'static str> {
    if k == Sheet::Nck {
        return match unlocks {
            Some(0) => Some("No unlock attempts are left."),
            None => Some(UNREAD),
            Some(_) => None,
        };
    }
    let Some(sim) = sim else { return (k == Sheet::Puk).then_some(UNREAD) };
    let locked = pin_locked(sim) || puk_locked(sim);
    match k {
        Sheet::Pin if puk_locked(sim) => Some(NEEDS_PUK),
        Sheet::Pin if !pin_locked(sim) => Some("The SIM is not waiting for its PIN."),
        Sheet::Puk if !puk_locked(sim) => Some("The SIM is not waiting for its PUK."),
        Sheet::Puk => match attempts(k, Some(sim), None) {
            Some(0) => Some("No PUK attempts are left."),
            None => Some(UNREAD),
            Some(_) => None,
        },
        Sheet::LockOn | Sheet::LockOff | Sheet::Change if locked => Some("Unlock the SIM first."),
        Sheet::LockOn if pin_enabled(sim) => Some("The PIN lock is already on."),
        Sheet::LockOff if !pin_enabled(sim) => Some("The PIN lock is already off."),
        _ => None,
    }
}

/// What a sheet says above its button (web additions: the app asks
/// nothing): what happens, what a mistake costs, the remote line.
fn lines(k: Sheet, left: Option<i64>, remote: bool) -> Vec<&'static str> {
    let mut out = Vec::new();
    match k {
        Sheet::LockOn => out.push("The SIM will ask for this PIN every time the router starts. Until it is entered, the router has no mobile connection."),
        Sheet::LockOff => out.push("The SIM will no longer ask for a PIN when the router starts."),
        _ => {}
    }
    let last = left == Some(1);
    match k {
        Sheet::Pin if last => out.push("This is the last attempt: a wrong PIN locks the SIM until its PUK is entered."),
        Sheet::LockOn | Sheet::LockOff | Sheet::Change if last => {
            out.push("This is the last attempt: a wrong PIN locks the SIM, and the router loses its mobile connection until the PUK is entered.")
        }
        Sheet::LockOn | Sheet::LockOff | Sheet::Change => {
            out.push("A wrong PIN uses an attempt. With none left, the SIM locks and the router loses its mobile connection until the PUK is entered.")
        }
        Sheet::Puk if last => out.push("This is the last attempt: a wrong PUK blocks the SIM for good."),
        Sheet::Puk => out.push("A wrong PUK uses an attempt. With none left, the SIM is blocked for good."),
        Sheet::Nck if last => out.push("This is the last attempt: a wrong code can leave the router locked to its network for good."),
        _ => {}
    }
    if remote && k.cuts() {
        out.push(super::confirm::REMOTE);
    }
    out
}

/// A PIN or PUK field (the app's secure fields): never filled, and marked
/// as a one-time code so a browser neither offers to save it as (or
/// generate) a password nor fills in the router's.
fn secret(name: &str, label: &str, max: u8, err: Option<&str>) -> Markup {
    let err_id = format!("{name}-err");
    html! {
        li.it.fld {
            label.sr for=(name) { (label) }
            input #(name) type="password" name=(name) placeholder=(label) inputmode="numeric" maxlength=(max) required
                autocomplete="one-time-code" "data-1p-ignore" data-lpignore="true" data-bwignore
                aria-invalid=[err.map(|_| "true")] aria-describedby=[err.map(|_| err_id.as_str())];
            @if let Some(e) = err { p.ferr id=(err_id) { (e) } }
        }
    }
}

pub fn pin_sheet(pd: &PageData) -> Markup {
    sheet(pd, Sheet::Pin)
}

pub fn lock_on_sheet(pd: &PageData) -> Markup {
    sheet(pd, Sheet::LockOn)
}

pub fn lock_off_sheet(pd: &PageData) -> Markup {
    sheet(pd, Sheet::LockOff)
}

pub fn change_sheet(pd: &PageData) -> Markup {
    sheet(pd, Sheet::Change)
}

pub fn puk_sheet(pd: &PageData) -> Markup {
    sheet(pd, Sheet::Puk)
}

pub fn nck_sheet(pd: &PageData) -> Markup {
    sheet(pd, Sheet::Nck)
}

/// A sheet: its fields (always empty), the attempts left (where the app
/// shows them), what happens, a failure, the button; the count it was
/// drawn for in `left`. Nothing to do for this SIM: why, and only Cancel.
fn sheet(pd: &PageData, k: Sheet) -> Markup {
    let sim = pd.views.value(Topic::Sim);
    let unlocks = if k == Sheet::Nck { trials(pd.read(0)) } else { None };
    if let Some(why) = refusal(k, sim, unlocks) {
        return html! {
            @if let Some(n) = &pd.notice { (n) }
            @if why == UNREAD { p.msg.err role="alert" { (why) } } @else { p.msg.info role="status" { (why) } }
            @if why == NEEDS_PUK { p.actions { a.btn href=(PUK) { "Enter PUK" } } }
        };
    }
    let left = attempts(k, sim, unlocks);
    let err = |name: &str| pd.form.and_then(|f| f.error(name));
    let count = left.filter(|_| matches!(k, Sheet::Pin | Sheet::Puk | Sheet::Nck));
    let footer = html! {
        @if let Some(n) = count { p.foot { (n) @if n == 1 { " attempt remaining" } @else { " attempts remaining" } } }
        @for l in lines(k, left, pd.remote) { p.foot { (l) } }
        @if let Some(n) = &pd.notice { (n) }
    };
    let new_pin = html! {
        (secret("new", "New PIN", 8, err("new")))
        (secret("new2", "Confirm New PIN", 8, err("new2")))
    };
    html! {
        form.rf.req method="post" action=(k.path()) {
            (pd.csrf())
            (once_field(&pd.once()))
            input type="hidden" name="confirm" value="1";
            input type="hidden" name="left" value=(left.map(|n| n.to_string()).unwrap_or_default());
            @match k {
                Sheet::Change => {
                    (kit::section(Some("Current PIN"), None, secret("old", "Old PIN", 8, err("old"))))
                    (kit::section(Some("New PIN"), Some(footer), new_pin))
                }
                Sheet::Puk => {
                    (kit::section(None, Some(footer), secret("puk", "PUK Code", 8, err("puk"))))
                    (kit::section(Some("New PIN"), None, new_pin))
                }
                Sheet::Nck => (kit::section(None, Some(footer), html! {
                    li.it.fld {
                        label.sr for="nck" { "Unlock Code (NCK)" }
                        input #nck type="text" name="nck" placeholder="Unlock Code (NCK)" inputmode="numeric" maxlength="16" required
                            autocomplete="off" autocapitalize="off" autocorrect="off" spellcheck="false"
                            aria-invalid=[err("nck").map(|_| "true")] aria-describedby=[err("nck").map(|_| "nck-err")];
                        @if let Some(e) = err("nck") { p.ferr #nck-err { (e) } }
                    }
                })),
                _ => (kit::section(None, Some(footer), secret("pin", "PIN", 8, err("pin")))),
            }
            (kit::section(None, None, html! {
                li { button.it.cv.snd.danger[k.cuts()] type="submit" { (k.label()) } }
            }))
        }
    }
}

// --- Posts ---

const CHANGED: &str = "The attempts left have changed, so nothing was sent. Read the sheet again before trying.";

/// The checked fields (PINs and codes: in RAM for this post only).
type Vals = Vec<(&'static str, String)>;

fn val<'a>(vals: &'a Vals, name: &str) -> &'a str {
    vals.iter().find(|(k, _)| *k == name).map_or("", |(_, v)| v.as_str())
}

fn digits(v: &str) -> bool {
    !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())
}

/// A PIN: the app's message when shorter than 4, then 4–8 digits.
fn pin(form: &Form, name: &'static str, short: &str, fs: &mut FormState) -> String {
    let v = form.get(name).unwrap_or("").trim();
    if v.chars().count() < 4 {
        fs.errors.push((name, short.into()));
    } else if v.len() > 8 || !digits(v) {
        fs.errors.push((name, "A PIN is 4 to 8 digits.".into()));
    }
    v.to_owned()
}

/// The checks (the app's messages, and digits only); a refused form comes
/// back with its messages and no values.
fn parse(k: Sheet, form: &Form) -> Result<Vals, FormState> {
    let mut fs = FormState::default();
    let short = if k == Sheet::Puk { "New PIN must be at least 4 digits" } else { "PIN must be at least 4 digits" };
    let mut vals = Vec::new();
    match k {
        Sheet::Pin | Sheet::LockOn | Sheet::LockOff => vals.push(("pin", pin(form, "pin", short, &mut fs))),
        Sheet::Change => vals.push(("old", pin(form, "old", short, &mut fs))),
        Sheet::Puk => {
            let puk = form.get("puk").unwrap_or("").trim();
            if puk.chars().count() < 8 {
                fs.errors.push(("puk", "PUK must be at least 8 digits".into()));
            } else if puk.len() != 8 || !digits(puk) {
                fs.errors.push(("puk", "A PUK is 8 digits.".into()));
            }
            vals.push(("puk", puk.to_owned()));
        }
        Sheet::Nck => {
            let code = form.get("nck").unwrap_or("").trim();
            if code.is_empty() {
                fs.errors.push(("nck", "Unlock code is required".into()));
            } else if code.len() > 16 || !digits(code) {
                fs.errors.push(("nck", "The unlock code is digits only, at most 16.".into()));
            }
            vals.push(("nck", code.to_owned()));
        }
    }
    if matches!(k, Sheet::Change | Sheet::Puk) {
        let new = pin(form, "new", short, &mut fs);
        if fs.error("new").is_none() && form.get("new2").unwrap_or("").trim() != new {
            fs.errors.push(("new2", "The new PINs do not match.".into()));
        }
        vals.push(("new", new));
    }
    if fs.errors.is_empty() {
        Ok(vals)
    } else {
        Err(fs)
    }
}

/// The app's bodies.
fn body(k: Sheet, v: &Vals) -> Value {
    match k {
        Sheet::Pin => json!({"pin_num": val(v, "pin"), "puk_num": "", "pin_encode_flag": "0"}),
        Sheet::LockOn | Sheet::LockOff => {
            json!({"pin_num_m": val(v, "pin"), "pin_mode": i32::from(k == Sheet::LockOn), "pin_encode_flag": "0"})
        }
        Sheet::Change => json!({"pin_num": val(v, "old"), "new_pin_num": val(v, "new"), "pin_encode_flag": "0"}),
        Sheet::Puk => json!({"pin_num": val(v, "new"), "puk_num": val(v, "puk"), "pin_encode_flag": "0"}),
        Sheet::Nck => json!({"nck": val(v, "nck")}),
    }
}

/// An answer whose `result` says it failed (inferred: what the firmware
/// answers to a wrong PIN is not verified on the device; the app takes
/// every 2xx as success).
fn checked(data: Value) -> Result<(), (u16, String)> {
    match data["result"].as_str() {
        Some(r) if r.to_ascii_lowercase().contains("fail") => Err((502, fmt::truncate(r, 120))),
        _ => Ok(()),
    }
}

/// The SIM and its unlock attempts as read now (`None`: unread), the SIM
/// past the cache (whose entry can be 30 s old).
fn now(app: &AppState, k: Sheet) -> (Option<Value>, Option<i64>) {
    app.cache.invalidate(crate::cache::SIM_INFO);
    let views = super::read_topics(app, &[Topic::Sim]);
    let sim = views.slot(Topic::Sim).filter(|s| s.error.is_none()).and_then(|s| s.value.as_deref().cloned());
    let unlocks = if k == Sheet::Nck { trials(super::api_get(app, TRIALS).ok().as_ref()) } else { None };
    (sim, unlocks)
}

/// 303 to SIM Card with `text` for this session (`?r=<id>`).
fn line(app: &AppState, flash: &Flash, owner: SessionId, ok: Option<bool>, text: &str) -> Response {
    let Ok(id) = app.auth.random_hex(8) else { return session::see_other(PATH) };
    flash.insert(id.clone(), owner, (ok, text.to_owned()), app.auth.now());
    session::see_other(&format!("{PATH}?r={id}"))
}

/// The attempts a sheet was drawn for (`left`; "" none, anything else a
/// count no SIM has).
fn shown_left(form: &Form) -> Option<i64> {
    match form.get("left").unwrap_or("") {
        "" => None,
        v => Some(v.parse().ok().filter(|_| digits(v)).unwrap_or(-1)),
    }
}

/// `POST <sheet>`: the session, the form and its CSRF token, the fields,
/// then (single-use token) the SIM as it is now, and the call. Success:
/// SIM Card with the app's line; a failure: the sheet again, empty, with
/// "Failed: …" and the attempts left as read after it.
pub async fn post(ctx: Ctx, req: Request, k: Sheet) -> Response {
    let nonce = CspNonce::of(&req);
    let mut r = Req { query: None, ..Req::of(&ctx, &req) };
    let shown = Req { query: None, ..Req::of(&ctx, &req) };
    let (parts, body_in) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, k.path(), None);
        return session::reply_unread(&parts.headers, body_in, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body_in, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&nonce, k.path());
    }
    // The sheet is the question: a post that did not come from it goes there.
    if form.get("confirm") != Some("1") {
        return session::see_other(k.path());
    }
    let s = k.screen();
    let vals = match parse(k, &form) {
        Ok(v) => v,
        Err(fs) => {
            r.typed = Some(fs);
            return screen::show(&ctx, p, r, s, None, 400).await;
        }
    };
    let token = form.get("once").unwrap_or("").to_owned();
    let left = shown_left(&form);
    // Keyed: the token store never holds a plain hash of a PIN.
    let secret: Vec<&str> = vals.iter().map(|(_, v)| v.as_str()).collect();
    let keyed = ctx.app.auth.opaque_key("sim-once", &secret.join("\n")).unwrap_or_default();
    let digest = job::digest(&[k.path(), &keyed]);
    let (flash, owner, pw) = (Arc::clone(&ctx.flash), p.id, p.clone());
    let work: OnceJob = Box::new(move |app: &AppState, claim| {
        let (sim, unlocks) = now(app, k);
        if refusal(k, sim.as_ref(), unlocks).is_some() {
            // Nothing sent (the token goes back); the sheet says why.
            return screen::render(app, s, &pw, &r, None, None, 409);
        }
        // Other attempts left than the sheet showed (another tab, the app,
        // the touchscreen): nothing sent; the sheet as it is now. (A PIN
        // sheet whose SIM could not be read leaves it to the device.)
        let now_left = attempts(k, sim.as_ref(), unlocks);
        if now_left != left && (now_left.is_some() || matches!(k, Sheet::Puk | Sheet::Nck)) {
            let note = Notice::Flash(Some(false), CHANGED.to_owned());
            return screen::render(app, s, &pw, &r, Some(note), None, 409);
        }
        claim.disarm();
        let out = super::call(app, Method::Post, k.call(), &body(k, &vals)).and_then(checked);
        claim.set(match &out {
            Ok(()) => Token::Sent(claim.digest()),
            Err((status, e)) => Token::Failed(claim.digest(), *status, e.clone()),
        });
        match out {
            Ok(()) => line(app, &flash, owner, Some(true), k.done()),
            Err((status, e)) => {
                // The sheet again for the SIM as it is now (the last wrong
                // PIN leaves it wanting its PUK).
                app.cache.invalidate(crate::cache::SIM_INFO);
                screen::render(app, s, &pw, &r, Some(Notice::Failed(e)), None, status)
            }
        }
    });
    let lane = crate::server::lane(Method::Post, k.call());
    let again = |text: &str| Some(Notice::Flash(Some(false), text.to_owned()));
    match job::once(&ctx, owner, &nonce, &token, digest, lane, SETTLE_SLOW, work).await {
        Sent::Ran(resp) => resp,
        Sent::Done(Ok(())) => line(&ctx.app, &ctx.flash, owner, Some(true), k.done()),
        Sent::Done(Err((status, e))) => screen::show(&ctx, p, shown, s, Some(Notice::Failed(e)), status).await,
        Sent::Expired => screen::show(&ctx, p, shown, s, again("This page expired, so nothing was sent. Try again."), 409).await,
        Sent::Busy => screen::show(&ctx, p, shown, s, again("Failed: the router is busy. Try again in a few seconds."), 503).await,
        Sent::Unknown => line(&ctx.app, &ctx.flash, owner, None, "The router may still be working on it. Check before trying again."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boottime::BootInstant;
    use crate::events::hub::SlotView;

    fn views(sim: Value) -> Views {
        let now = BootInstant::ZERO;
        let slot = SlotView { version: 1, at: now, value_at: now, value: Some(Arc::new(sim)), error: None };
        Views::new(now, vec![(Topic::Sim, slot)])
    }

    /// The app's status labels and colours; identifiers masked.
    #[test]
    fn the_card_follows_the_app_with_identifiers_masked() {
        let sim = json!({
            "sim_states": "sim ready", "sim_iccid": "8900100000009998877F", "sim_imsi": "001019999911111",
            "msisdn": "+10000000042", "mdm_mcc": "001", "mdm_mnc": "01", "Operator": "Test Net",
            "current_sim_slot": "1", "spn_name_data": "0054006500730074", "pin_status": "1", "pinnumber": "1", "puknumber": 3,
        });
        let s = card(&views(sim.clone())).into_string();
        for want in [
            "<span>Status</span><span class=\"v c-green\">Ready</span>", "<span>ICCID</span><span class=\"v\">…8877</span>",
            "<span>IMSI</span><span class=\"v\">001/01 …1111</span>", "<span>MSISDN</span><span class=\"v\">…0042</span>",
            "<span>SPN</span><span class=\"v\">Test</span>", "<span>MCC/MNC</span><span class=\"v\">001/01</span>",
            "<span>Operator</span><span class=\"v\">Test Net</span>", "<span>SIM Slot</span><span class=\"v\">1</span>",
        ] {
            assert!(s.contains(want), "{want} in {s}");
        }
        for secret in ["8900100000009998877", "9998877", "001019999911111", "10000000042"] {
            assert!(!s.contains(secret), "{secret}");
        }
        let p = pins(&views(sim)).into_string();
        for want in [
            "<span class=\"v c-green\">Enabled</span>", "<span>PIN Attempts</span><span class=\"v c-red\">1</span>",
            "<span>PUK Attempts</span><span class=\"v c-orange\">3</span>", "href=\"/ui/router/sim/pin-lock/disable\"",
            "href=\"/ui/router/sim/change-pin\"",
        ] {
            assert!(p.contains(want), "{want} in {p}");
        }
        // Empty values: the app's "--"; a status from the modem state.
        let s = card(&views(json!({"modem_main_state": "modem_waitpuk"}))).into_string();
        assert!(s.contains("<span class=\"v c-orange\">PUK Required</span>") && s.contains("<span>ICCID</span><span class=\"v\">--</span>"), "{s}");
        assert!(!s.contains("Operator"));
        assert_eq!(status("sim undetected"), ("No SIM".into(), Some("c-red")));
        assert_eq!(status("Something"), ("Something".into(), None));
        // An IMSI whose network the card does not name: 3 + 2 digits.
        assert_eq!(imsi("310260000000042", "", ""), "310/26 …0042");
        assert_eq!(imsi("310260000000042", "310", "260"), "310/260 …0042");
        assert_eq!(imsi("12345", "", ""), "…2345");
        assert_eq!(spn("00"), "");
        assert_eq!(spn("004100000042"), "AB");
    }

    /// Locked SIMs: the banners with their sheets; no lock or change rows.
    #[test]
    fn locked_sims_show_the_banners() {
        let v = views(json!({"sim_states": "wait pin", "pin_status": "1"}));
        let b = banners(&v).into_string();
        assert!(b.contains("<b>SIM PIN Required</b><small>Enter your PIN to unlock the SIM card</small>"), "{b}");
        assert!(b.contains("<a class=\"it bt snd\" href=\"/ui/router/sim/pin\">Enter PIN</a>") && !b.contains("PUK"));
        assert!(!pins(&v).into_string().contains("<a "));
        let v = views(json!({"sim_states": "", "modem_main_state": "MODEM_WAITPUK"}));
        assert!(banners(&v).into_string().contains("href=\"/ui/router/sim/puk\">Enter PUK</a>"));
    }

    /// The checks never keep a value; PINs 4–8 digits, PUK 8, NCK digits.
    #[test]
    fn fields_are_checked_and_never_kept() {
        let f = |q: &str| Form::parse(q.as_bytes());
        assert_eq!(parse(Sheet::Pin, &f("pin=1234")).unwrap(), [("pin", "1234".to_owned())]);
        let fs = parse(Sheet::Pin, &f("pin=123")).unwrap_err();
        assert_eq!((fs.error("pin"), fs.values.len()), (Some("PIN must be at least 4 digits"), 0));
        assert_eq!(parse(Sheet::LockOn, &f("pin=12a4")).unwrap_err().error("pin"), Some("A PIN is 4 to 8 digits."));
        assert_eq!(parse(Sheet::Change, &f("old=1234&new=5678&new2=5679")).unwrap_err().error("new2"), Some("The new PINs do not match."));
        assert_eq!(parse(Sheet::Change, &f("old=1234&new=5678&new2=5678")).unwrap().len(), 2);
        let fs = parse(Sheet::Puk, &f("puk=1234567&new=12&new2=12")).unwrap_err();
        assert_eq!((fs.error("puk"), fs.error("new")), (Some("PUK must be at least 8 digits"), Some("New PIN must be at least 4 digits")));
        assert_eq!(parse(Sheet::Puk, &f("puk=123456789&new=1234&new2=1234")).unwrap_err().error("puk"), Some("A PUK is 8 digits."));
        assert_eq!(parse(Sheet::Nck, &f("nck=")).unwrap_err().error("nck"), Some("Unlock code is required"));
        assert!(parse(Sheet::Nck, &f("nck=0000000000000042")).is_ok());
        let v: Vals = vec![("new", "5678".into()), ("puk", "12345678".into())];
        assert_eq!(body(Sheet::Puk, &v), json!({"pin_num": "5678", "puk_num": "12345678", "pin_encode_flag": "0"}));
        let v: Vals = vec![("pin", "1234".into())];
        assert_eq!(body(Sheet::LockOff, &v), json!({"pin_num_m": "1234", "pin_mode": 0, "pin_encode_flag": "0"}));
        assert_eq!(body(Sheet::LockOn, &v)["pin_mode"], 1);
        assert!(checked(json!({"result": "failure"})).is_err() && checked(json!({"result": "success"})).is_ok() && checked(Value::Null).is_ok());
    }

    /// A sheet that does not fit the SIM sends nothing; the questions name
    /// what a mistake costs, the last attempt, and the remote line.
    #[test]
    fn sheets_fit_the_sim_and_say_what_happens() {
        let ready = json!({"sim_states": "sim ready", "pin_status": "0"});
        assert_eq!(refusal(Sheet::Pin, Some(&ready), None), Some("The SIM is not waiting for its PIN."));
        assert_eq!(refusal(Sheet::Pin, Some(&json!({"sim_states": "wait puk", "puknumber": 10})), None), Some(NEEDS_PUK));
        assert_eq!(refusal(Sheet::LockOff, Some(&ready), None), Some("The PIN lock is already off."));
        assert_eq!(refusal(Sheet::LockOn, Some(&ready), None), None);
        assert_eq!(refusal(Sheet::Change, Some(&json!({"sim_states": "wait pin"})), None), Some("Unlock the SIM first."));
        assert_eq!(refusal(Sheet::Pin, None, None), None);
        assert_eq!(refusal(Sheet::Nck, None, Some(0)), Some("No unlock attempts are left."));
        // A PUK or unlock code is never sent without its attempts known.
        assert_eq!(refusal(Sheet::Nck, None, None), Some(UNREAD));
        assert_eq!(refusal(Sheet::Nck, None, Some(2)), None);
        let puk = |n: Value| json!({"sim_states": "wait puk", "puknumber": n});
        assert_eq!(refusal(Sheet::Puk, None, None), Some(UNREAD));
        assert_eq!(refusal(Sheet::Puk, Some(&puk(json!(0))), None), Some("No PUK attempts are left."));
        assert_eq!(refusal(Sheet::Puk, Some(&puk(Value::Null)), None), Some(UNREAD));
        assert_eq!(refusal(Sheet::Puk, Some(&puk(json!("2"))), None), None);
        assert_eq!(attempts(Sheet::Puk, Some(&puk(json!("2"))), None), Some(2));
        let f = |q: &str| shown_left(&Form::parse(q.as_bytes()));
        assert_eq!((f("left=3"), f("left="), f("x=1"), f("left=-1"), f("left=2a")), (Some(3), None, None, Some(-1), Some(-1)));
        let l = lines(Sheet::LockOn, Some(1), true);
        assert!(l[0].starts_with("The SIM will ask for this PIN") && l[1].starts_with("This is the last attempt") && l[2] == super::super::confirm::REMOTE);
        assert_eq!(lines(Sheet::Pin, Some(3), true), Vec::<&str>::new());
        assert_eq!(lines(Sheet::Nck, Some(1), true).len(), 1);
    }
}
