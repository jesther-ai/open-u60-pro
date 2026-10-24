//! Questions that hand out a single-use token, and the posts that use it:
//! Speed Test's start (it uses mobile data; owner decision), Enable ADB
//! (debug access over USB; owner decision), Band Lock's apply and unlock
//! (a lock to bands the operator does not use here can cut service), and
//! the Router screens' changes (`pages::mobile`, `pages::cell`): Mobile
//! Network's Apply, scan and manual registration (a registration's
//! question reads the scan's results), Cell Lock's locks,
//! unlock and neighbour scan, Smart Tower Connect's switches and reset,
//! Signal Detection's start and stop; Device Controls' switches and idle
//! timeout (`pages::devctl`: only Auto Sleep on, for a remote browser, is
//! asked). The app asks none of them.
//!
//! - `GET <path>?<fields>` is the question (read-only; the speed test's
//!   reads its server list and progress). Its form posts the same fields,
//!   `confirm=1` and a new single-use token. A field no form sends is 400;
//!   a typed value out of range shows the screen's form again with a
//!   message per field (400). A kind with nothing to ask for these values
//!   (a scan, an Apply that cuts nothing) has no question: its page's form
//!   carries the token and posts at once.
//! - `POST <path>` without `confirm=1` goes to the question when there is
//!   one. The token is claimed for this post right before the device call
//!   and given back if nothing is sent; a double click sends once and the
//!   second post shows what the first did.
//! - Every outcome is a line on the screen for this session (`?r=<id>`),
//!   in the app's words, or an operation page (a scan, a registration,
//!   Apply) whose id is the token (so a replay finds it); a started speed
//!   test's page watches it from before the start (`since`), so it says how
//!   the test ends.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::Method as HttpMethod;
use axum::response::Response;
use maud::html;
use serde_json::{json, Value};

use super::{bandlock, cell, devctl, mobile, speedtest};
use crate::auth::{Principal, SessionId};
use crate::events::Topic;
use crate::handlers::AppState;
use crate::httpd::gate::{self, Lane};
use crate::httpd::{Ctx, Peer};
use crate::server::Method;
use crate::web::action::{self, Ask};
use crate::web::bands::{LOCK_LTE, LOCK_NR};
use crate::web::job::{self, OnceJob, Sent};
use crate::web::kit;
use crate::web::ops::{is_op_id, Op, OpSpec, OpTable};
use crate::web::render::{self, PageCtx};
use crate::web::screen::{self, FormState, Read, Req, Screen};
use crate::web::session::{self, Form};
use crate::web::store::{Claim, Flash, Once, Token, SETTLE, SETTLE_SLOW};
use crate::web::tabs::{router, tools};
use crate::web::{fmt, CspNonce};

pub const ADB: &str = "/ui/tools/adb";
pub const ADB_ENABLE: &str = "/ui/tools/adb/enable";

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    Speed,
    Adb,
    Nr,
    Lte,
    Unlock,
    /// Mobile Network (`pages::mobile`).
    Apply,
    Scan,
    Register,
    /// Cell Lock, Smart Tower Connect, Signal Detection (`pages::cell`).
    CellNr,
    CellLte,
    CellReset,
    Neighbors,
    StcOn,
    StcOff,
    StcReset,
    Detect,
    DetectStop,
    /// Device Controls (`pages::devctl`).
    PowerSave,
    FastBoot,
    AutoSleep,
}

const KINDS: [Kind; 20] = [
    Kind::Speed,
    Kind::Adb,
    Kind::Nr,
    Kind::Lte,
    Kind::Unlock,
    Kind::Apply,
    Kind::Scan,
    Kind::Register,
    Kind::CellNr,
    Kind::CellLte,
    Kind::CellReset,
    Kind::Neighbors,
    Kind::StcOn,
    Kind::StcOff,
    Kind::StcReset,
    Kind::Detect,
    Kind::DetectStop,
    Kind::PowerSave,
    Kind::FastBoot,
    Kind::AutoSleep,
];

/// Which module knows a Router kind's fields, question and call.
enum Of {
    Tools,
    Mobile,
    Cell,
    Device,
}

impl Kind {
    fn of(self) -> Of {
        match self {
            Kind::Speed | Kind::Adb | Kind::Nr | Kind::Lte | Kind::Unlock => Of::Tools,
            Kind::Apply | Kind::Scan | Kind::Register => Of::Mobile,
            Kind::PowerSave | Kind::FastBoot | Kind::AutoSleep => Of::Device,
            _ => Of::Cell,
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            Kind::Speed => speedtest::START,
            Kind::Adb => ADB_ENABLE,
            Kind::Nr => bandlock::NR,
            Kind::Lte => bandlock::LTE,
            Kind::Unlock => bandlock::UNLOCK,
            Kind::Apply => mobile::APPLY,
            Kind::Scan => mobile::SCAN,
            Kind::Register => mobile::REGISTER,
            Kind::CellNr => cell::LOCK_NR,
            Kind::CellLte => cell::LOCK_LTE,
            Kind::CellReset => cell::UNLOCK,
            Kind::Neighbors => cell::NEIGHBORS,
            Kind::StcOn => cell::STC_ON,
            Kind::StcOff => cell::STC_OFF,
            Kind::StcReset => cell::STC_RESET,
            Kind::Detect => cell::SD_START,
            Kind::DetectStop => cell::SD_STOP,
            Kind::PowerSave => devctl::POWER_SAVE,
            Kind::FastBoot => devctl::FAST_BOOT,
            Kind::AutoSleep => devctl::AUTO_SLEEP,
        }
    }

    pub fn by_path(path: &str) -> Option<Kind> {
        KINDS.into_iter().find(|k| k.path() == path)
    }

    /// The screen: the way back, Cancel and the result.
    pub fn screen(self) -> &'static Screen {
        match self {
            Kind::Speed => &tools::SPEEDTEST,
            Kind::Adb => &tools::ADB,
            Kind::Nr | Kind::Lte | Kind::Unlock => &tools::BAND_LOCK,
            Kind::Apply | Kind::Scan | Kind::Register => &router::MOBILE_NETWORK,
            Kind::CellNr | Kind::CellLte | Kind::CellReset | Kind::Neighbors => &router::CELL_LOCK,
            Kind::StcOn | Kind::StcOff | Kind::StcReset => &router::STC,
            Kind::Detect | Kind::DetectStop => &router::SIGNAL_DETECT,
            Kind::PowerSave | Kind::FastBoot | Kind::AutoSleep => &router::DEVICE,
        }
    }

    /// The (first) API call it makes.
    pub fn call(self) -> (Method, &'static str) {
        match self {
            Kind::Speed => (Method::Post, "/api/speedtest/start"),
            Kind::Adb => (Method::Put, "/api/usb/mode"),
            Kind::Nr => (Method::Post, "/api/cell/band/nr"),
            Kind::Lte => (Method::Post, "/api/cell/band/lte"),
            Kind::Unlock => (Method::Post, "/api/cell/band/reset"),
            Kind::Apply => (Method::Put, "/api/modem/data"),
            Kind::Scan => (Method::Post, "/api/modem/scan"),
            Kind::Register => (Method::Post, "/api/modem/register"),
            Kind::CellNr => (Method::Post, "/api/cell/lock/nr"),
            Kind::CellLte => (Method::Post, "/api/cell/lock/lte"),
            Kind::CellReset => (Method::Post, "/api/cell/lock/reset"),
            Kind::Neighbors => (Method::Post, "/api/cell/neighbors/scan"),
            Kind::StcOn => (Method::Post, "/api/cell/stc/enable"),
            Kind::StcOff => (Method::Post, "/api/cell/stc/disable"),
            Kind::StcReset => (Method::Post, "/api/cell/stc/reset"),
            Kind::Detect => (Method::Post, "/api/cell/signal-detect/start"),
            Kind::DetectStop => (Method::Post, "/api/cell/signal-detect/stop"),
            Kind::PowerSave => (Method::Put, devctl::POWER_SAVE_API),
            Kind::FastBoot => (Method::Put, devctl::FAST_BOOT_API),
            Kind::AutoSleep => (Method::Put, devctl::AUTO_SLEEP_API),
        }
    }

    /// The call's lane (`server::lane`): Enable ADB, Apply and Device
    /// Controls' are general, every other call is slow.
    fn lane(self) -> Lane {
        let (method, path) = self.call();
        crate::server::lane(method, path)
    }

    /// The band field of a lock (`nr`, `lte`) and its list.
    fn bands(self) -> Option<(&'static str, &'static [u16])> {
        match self {
            Kind::Nr => Some(("nr", &LOCK_NR)),
            Kind::Lte => Some(("lte", &LOCK_LTE)),
            _ => None,
        }
    }
}

/// The checked fields: the speed test's server, a lock's bands (sorted,
/// each once), a Router kind's fields (`vals`, from fixed names).
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Input {
    server: Option<u64>,
    bands: Vec<u16>,
    pub vals: Vec<(&'static str, String)>,
}

impl Input {
    /// A Router kind's field ("" if not sent).
    pub fn val(&self, name: &str) -> &str {
        self.vals.iter().find(|(k, _)| *k == name).map_or("", |(_, v)| v.as_str())
    }

    #[cfg(test)]
    pub fn of(vals: &[(&'static str, &str)]) -> Input {
        Input { vals: vals.iter().map(|(k, v)| (*k, (*v).to_owned())).collect(), ..Input::default() }
    }
}

/// Why fields were refused.
#[derive(Debug)]
pub enum Bad {
    /// A value no form sends: 400.
    Request,
    /// Typed values out of range: the screen's form again, as typed, with
    /// a message per field (400).
    Fields(FormState),
}

/// The fields of `kind` in a query or form; `Bad::Request` for a value no
/// form sends (a band not in the app's list, a server id that is not
/// digits).
fn parse(kind: Kind, pairs: &[(String, String)]) -> Result<Input, Bad> {
    match kind.of() {
        Of::Mobile => return mobile::parse(kind, pairs).map(|vals| Input { vals, ..Input::default() }),
        Of::Cell => return cell::parse(kind, pairs).map(|vals| Input { vals, ..Input::default() }),
        Of::Device => return devctl::parse(kind, pairs).map(|vals| Input { vals, ..Input::default() }),
        Of::Tools => {}
    }
    let digits = |v: &str| !v.is_empty() && v.len() <= 20 && v.bytes().all(|b| b.is_ascii_digit());
    let mut input = Input::default();
    for (k, v) in pairs {
        if kind == Kind::Speed && k == "server" && !v.is_empty() {
            input.server = Some(v.parse().ok().filter(|_| digits(v)).ok_or(Bad::Request)?);
        }
        if let Some((_, list)) = kind.bands().filter(|(name, _)| k == name) {
            let band = v.parse::<u16>().ok().filter(|b| digits(v) && list.contains(b)).ok_or(Bad::Request)?;
            if !input.bands.contains(&band) {
                input.bands.push(band);
            }
        }
    }
    input.bands.sort_unstable();
    Ok(input)
}

/// The fields as form pairs (the question's hidden fields, its URL).
fn fields(kind: Kind, input: &Input) -> Vec<(&'static str, String)> {
    match (kind, kind.bands()) {
        (Kind::Speed, _) => input.server.map(|s| ("server", s.to_string())).into_iter().collect(),
        (_, Some((name, _))) => input.bands.iter().map(|b| (name, b.to_string())).collect(),
        _ => input.vals.clone(),
    }
}

fn query(kind: Kind, input: &Input) -> String {
    let pairs: Vec<String> = fields(kind, input).iter().map(|(k, v)| format!("{k}={}", session::percent_encode(v))).collect();
    if pairs.is_empty() {
        String::new()
    } else {
        format!("?{}", pairs.join("&"))
    }
}

/// "1,3,78" (the app's band string) or "1, 3, 78".
fn band_list(bands: &[u16], sep: &str) -> String {
    bands.iter().map(u16::to_string).collect::<Vec<_>>().join(sep)
}

/// The screen, keeping a lock's choice, the speed test's server or what
/// was typed.
fn back(kind: Kind, input: &Input) -> String {
    match kind.of() {
        Of::Mobile => mobile::back(kind, input),
        Of::Cell => cell::back(kind, input),
        Of::Device => devctl::PATH.into(),
        Of::Tools => format!("{}{}", kind.screen().path, if kind == Kind::Unlock { String::new() } else { query(kind, input) }),
    }
}

/// The remote line: what a change that may cut this connection means for a
/// browser over Tailscale (every remote line opens the same way).
pub const REMOTE: &str = "You are connected remotely: if the connection drops, this page can\u{2019}t be reached until the router is back online.";

pub struct Question {
    pub title: String,
    pub lines: Vec<String>,
    /// The button; none: nothing to do, only Cancel.
    pub label: Option<&'static str>,
    pub danger: bool,
}

pub fn nothing(title: impl Into<String>, lines: Vec<String>) -> Question {
    Question { title: title.into(), lines, label: None, danger: false }
}

/// A question with a button; for a remote browser with `REMOTE` last
/// when `cuts` (the change can cut the mobile link the page comes over).
pub fn asking(title: impl Into<String>, mut lines: Vec<String>, label: &'static str, remote: bool, cuts: bool) -> Question {
    if remote && cuts {
        lines.push(REMOTE.into());
    }
    Question { title: title.into(), lines, label: Some(label), danger: true }
}

/// The question for `input`; `None` for a Router kind that has none for
/// these values (it posts from its page). What it reads (`read`): the
/// speed test's server list and progress, a registration's scan results
/// (`None`: only whether there is a question).
fn question(kind: Kind, input: &Input, remote: bool, read: Option<(&Result<Value, String>, &Value)>) -> Option<Question> {
    match kind.of() {
        Of::Mobile => return mobile::question(kind, input, remote, read.map(|(r, _)| r)),
        Of::Cell => return cell::question(kind, input, remote),
        Of::Device => return devctl::question(kind, input, remote),
        Of::Tools => {}
    }
    let ask = |title: &str, lines: Vec<String>, label, danger| {
        let mut q = asking(title, lines, label, remote, kind != Kind::Speed && kind != Kind::Adb);
        q.danger = danger;
        q
    };
    Some(match kind {
        Kind::Speed => {
            let Some(id) = input.server else { return Some(nothing("Select a server first", Vec::new())) };
            let (servers, progress) = read.unwrap_or((&Ok(Value::Null), &Value::Null));
            let servers = match servers {
                Ok(s) => s,
                Err(e) => return Some(nothing("Failed to load servers", vec![fmt::truncate(e, 120)])),
            };
            let Some(server) = servers.as_array().and_then(|a| a.iter().find(|s| speedtest::id_of(s) == Some(id))) else {
                return Some(nothing("This server is no longer in the list.", vec!["Choose another one.".into()]));
            };
            if speedtest::running(progress) {
                return Some(nothing("A speed test is already running.", Vec::new()));
            }
            let lines = vec![
                format!("Server: {}", speedtest::server_label(server)),
                "It downloads for 15 seconds and uploads 10 MB over the mobile connection: about 200 MB at 100 Mbit/s, more on a faster link.".into(),
                "Your plan may count this data.".into(),
            ];
            ask("Start the speed test?", lines, "Start Speed Test", false)
        }
        Kind::Adb => ask(
            "Enable ADB?",
            vec![
                "Anything plugged into the USB-C port gets a root shell on the router, without a password.".into(),
                "A computer that uses the USB-C cable for its network connection may lose it while the USB mode changes.".into(),
            ],
            "Enable ADB",
            true,
        ),
        Kind::Nr if input.bands.is_empty() => nothing("Select at least one NR band", Vec::new()),
        Kind::Lte if input.bands.is_empty() => nothing("Select at least one LTE band", Vec::new()),
        Kind::Nr => ask(
            "Apply NR Lock?",
            vec![
                format!("The modem will use only these 5G NR bands, for NSA and SA: {}.", band_list(&input.bands, ", ")),
                "If your operator has no 5G on these bands here, the router can lose 5G, or all service, until you unlock the bands.".into(),
            ],
            "Apply NR Lock",
            true,
        ),
        Kind::Lte => ask(
            "Apply LTE Lock?",
            vec![
                format!("The modem will use only these LTE bands: {}.", band_list(&input.bands, ", ")),
                "If your operator does not use these bands here, the router can lose its connection until you unlock the bands.".into(),
            ],
            "Apply LTE Lock",
            true,
        ),
        _ => ask(
            "Unlock all bands?",
            vec!["The modem may use every band again. It may re-register, which drops the connection for a moment.".into()],
            "Unlock All Bands",
            true,
        ),
    })
}

/// The question page; its form (if there is something to do) carries a
/// new single-use token. The nav bar leads back to the screen.
fn ask_page(app: &AppState, once: &Once, p: &Principal, nonce: &str, kind: Kind, input: &Input, q: &Question) -> Response {
    let token = q.label.map(|_| once.fresh(&app.auth, p.id)).unwrap_or_default();
    let mut hidden = fields(kind, input);
    hidden.push(("once", token));
    let hidden: Vec<(&str, &str)> = hidden.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let lines: Vec<&str> = q.lines.iter().map(String::as_str).collect();
    let cancel = back(kind, input);
    let a = Ask { title: &q.title, lines: &lines, action: kind.path(), hidden: &hidden, label: q.label, danger: q.danger, cancel: &cancel };
    let s = kind.screen();
    let pc = PageCtx {
        title: &q.title,
        tab: Some(s.tab),
        nonce,
        lead: kit::back(s.path, s.title),
        heading: Some(html! { h1 { "Confirm" } }),
        ..PageCtx::default()
    };
    render::html(200, render::page(&pc, action::ask(render::csrf_field(&app.auth, p), &a)))
}

/// The screen's form again, as typed, with a message per field (400).
async fn refused(ctx: &Ctx, p: Principal, mut r: Req, kind: Kind, fs: FormState) -> Response {
    r.typed = Some(fs);
    screen::show(ctx, p, r, kind.screen(), None, 400).await
}

/// `GET <path>?<fields>`: the question (none to ask: back to the screen).
/// The speed test's reads its server list and progress, a registration's
/// the scan's results, in a gate task.
pub async fn get(ctx: &Ctx, p: Principal, req: Request, kind: Kind) -> Response {
    let nonce = CspNonce::of(&req);
    let remote = session::is_remote(req.extensions().get::<Peer>());
    let input = match parse(kind, Form::parse(req.uri().query().unwrap_or("").as_bytes()).pairs()) {
        Ok(input) => input,
        Err(Bad::Request) => return render::bad_request(&nonce),
        Err(Bad::Fields(fs)) => return refused(ctx, p, Req::of(ctx, &req), kind, fs).await,
    };
    drop(req);
    let once = Arc::clone(&ctx.once);
    let reads = (kind == Kind::Speed && input.server.is_some()) || kind == Kind::Register;
    if !reads {
        return match question(kind, &input, remote, None) {
            Some(q) => ask_page(&ctx.app, &once, &p, &nonce, kind, &input, &q),
            None => session::see_other(&back(kind, &input)),
        };
    }
    // The server's name and whether a test runs (the list may be fetched
    // from speedtest.net: slow lane), or the networks the scan found.
    let (n, retry) = (nonce.clone(), format!("{}{}", kind.path(), query(kind, &input)));
    let lane = if kind == Kind::Register { crate::server::lane(Method::Get, mobile::RESULTS) } else { Lane::Slow };
    let job = Box::new(move |app: &AppState| {
        gate::read_only(|| {
            let (first, second) = if kind == Kind::Register {
                (super::api_get(app, mobile::RESULTS), Value::Null)
            } else {
                let progress = super::read_topics(app, &[Topic::Speedtest]).value(Topic::Speedtest).cloned().unwrap_or_default();
                (Read::Api(speedtest::SERVERS).run(app, None), progress)
            };
            let q = question(kind, &input, remote, Some((&first, &second))).unwrap_or_else(|| nothing("", Vec::new()));
            ask_page(app, &once, &p, &n, kind, &input, &q)
        })
    });
    job::run(ctx, lane, &nonce, &retry, job).await
}

/// The device calls of a confirmed post (disarming the claim right before
/// the first), and their data; a failure's line in the app's words
/// ("Failed to start: …", "Failed: …"). A Router kind that reads first
/// (Apply) sends nothing when the read fails, and the token is given back.
fn send(app: &AppState, kind: Kind, input: &Input, claim: &mut Claim) -> Result<(), (u16, String)> {
    match kind.of() {
        Of::Mobile => return mobile::send(app, kind, input, claim),
        Of::Cell => return cell::send(app, kind, input, claim),
        Of::Device => return devctl::send(app, kind, input, claim),
        Of::Tools => {}
    }
    claim.disarm();
    let (method, path) = kind.call();
    let call = |body: Value| super::call(app, method, path, &body).map(|_| ());
    let r = match kind {
        Kind::Speed => {
            let r = call(json!({"server_id": input.server.unwrap_or(0)}));
            return r.map_err(|(status, e)| (status, format!("Failed to start: {}", fmt::truncate(&e, 120))));
        }
        Kind::Adb => call(json!({"mode": "debug"})),
        // The app locks NSA, then SA, with the same bands; an SA failure
        // after NSA is locked says so (the app's "Failed: …" would read as
        // nothing changed).
        Kind::Nr => {
            let bands = band_list(&input.bands, ",");
            match call(json!({"nr5g_type": "nsa", "nr5g_band": bands})) {
                Ok(()) => {
                    return call(json!({"nr5g_type": "sa", "nr5g_band": bands})).map_err(|(status, e)| {
                        (status, format!("NR bands locked for NSA only: {bands}. SA lock failed: {}", fmt::truncate(&e, 120)))
                    })
                }
                Err(e) => Err(e),
            }
        }
        Kind::Lte => call(json!({"is_lte_band": "1", "lte_band_mask": band_list(&input.bands, ","), "is_gw_band": "0", "gw_band_mask": ""})),
        _ => call(json!({})),
    };
    r.map_err(|(status, e)| (status, format!("Failed: {}", fmt::truncate(&e, 120))))
}

/// Where a post leads.
pub enum Next {
    /// A line for this session (`?r=<id>`; `Some(true)` green, as the
    /// app's) at this URL (its query string kept).
    Line(String, Option<bool>, String),
    /// 303 here, without a line.
    To(String),
    /// This operation (and its target), whose progress page has the
    /// post's token as its id.
    Op(&'static OpSpec, &'static str),
}

/// What a post of `kind` comes to: `r` is the device's outcome (or the
/// first post's, on a replay), in the app's words.
fn next(kind: Kind, input: &Input, r: Result<(), &str>, app: &AppState) -> Next {
    match kind.of() {
        Of::Mobile => return mobile::next(kind, input, r),
        // The page's clock (`read_topics`): a sweep's watch point.
        Of::Cell => return cell::next(kind, input, r, crate::boottime::Clock::now(&crate::boottime::BootClock)),
        Of::Device => return devctl::next(r),
        Of::Tools => {}
    }
    let mut to = back(kind, input);
    let Err(msg) = r else {
        // A started test: its page watches from now (`speedtest::since`),
        // so a test that ends before the page loads still says how.
        if kind == Kind::Speed {
            let sep = if to.contains('?') { '&' } else { '?' };
            to = format!("{to}{sep}since={:x}", app.hub.peek(Topic::Speedtest).version << 1);
        }
        let line = match kind {
            Kind::Speed => "Speed test started".to_owned(),
            Kind::Adb => "ADB debug mode enabled. Connect USB-C cable to access the device.".to_owned(),
            Kind::Nr => format!("NR bands locked: {}", band_list(&input.bands, ",")),
            Kind::Lte => format!("LTE bands locked: {}", band_list(&input.bands, ",")),
            _ => "All bands unlocked".to_owned(),
        };
        return Next::Line(to, Some(true), line);
    };
    Next::Line(to, Some(false), msg.to_owned())
}

/// The stores a post's answer needs.
struct Stores {
    flash: Arc<Flash>,
    ops: Arc<OpTable>,
    owner: SessionId,
}

/// 303 to `to` with `line` for this session (`?r=<id>`).
fn line(app: &AppState, s: &Stores, to: &str, ok: Option<bool>, text: String) -> Response {
    // Without randomness the outcome stands; only its line is lost.
    let Ok(id) = app.auth.random_hex(8) else { return session::see_other(to) };
    s.flash.insert(id.clone(), s.owner, (ok, text), app.auth.now());
    let sep = if to.contains('?') { '&' } else { '?' };
    session::see_other(&format!("{to}{sep}r={id}"))
}

/// The answer to a post: its line, its redirect or its operation (kept
/// under the token, so a replay of the post finds the same page).
fn answer(app: &AppState, s: &Stores, token: &str, next: Next) -> Response {
    match next {
        Next::Line(to, ok, text) => line(app, s, &to, ok, text),
        Next::To(to) => session::see_other(&to),
        Next::Op(spec, arg) => {
            let now = app.auth.now();
            if is_op_id(token) && s.ops.get(token, s.owner, now).is_none() {
                s.ops.insert(token.to_owned(), s.owner, Op { spec, started: now, arg }, now);
            }
            session::see_other(&format!("/ui/op/{token}"))
        }
    }
}

/// Whether `kind` asks first for these values (a Router kind only for
/// some: a network scan only for a remote browser, Apply only for what can
/// cut service).
fn asks(kind: Kind, input: &Input, remote: bool) -> bool {
    matches!(kind.of(), Of::Tools) || question(kind, input, remote, None).is_some_and(|q| q.label.is_some())
}

/// `POST <path>`.
pub async fn post(ctx: Ctx, req: Request, kind: Kind) -> Response {
    let nonce = CspNonce::of(&req);
    let remote = session::is_remote(req.extensions().get::<Peer>());
    let r = Req { query: None, ..Req::of(&ctx, &req) };
    let s = kind.screen();
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, s.path, None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&nonce, s.path);
    }
    let input = match parse(kind, form.pairs()) {
        Ok(input) => input,
        Err(Bad::Request) => return render::bad_request(&nonce),
        Err(Bad::Fields(fs)) => return refused(&ctx, p, r, kind, fs).await,
    };
    // Apply with nothing changed: nothing to do (the app's Apply is disabled).
    if kind == Kind::Apply && !mobile::changed(&input) {
        return session::see_other(&back(kind, &input));
    }
    let empty = (kind == Kind::Speed && input.server.is_none()) || (kind.bands().is_some() && input.bands.is_empty());
    if (form.get("confirm") != Some("1") && asks(kind, &input, remote)) || empty {
        return session::see_other(&format!("{}{}", kind.path(), query(kind, &input)));
    }
    let token = form.get("once").unwrap_or("").to_owned();
    let digest = job::digest(&[kind.path(), &query(kind, &input)]);
    let stores = Stores { flash: Arc::clone(&ctx.flash), ops: Arc::clone(&ctx.ops), owner: p.id };
    let (work_input, work_token) = (input.clone(), token.clone());
    let work: OnceJob = Box::new(move |app: &AppState, claim| {
        let r = send(app, kind, &work_input, claim);
        if claim.sent() {
            claim.set(match &r {
                Ok(()) => Token::Sent(claim.digest()),
                Err((status, msg)) => Token::Failed(claim.digest(), *status, msg.clone()),
            });
        }
        let n = next(kind, &work_input, r.as_ref().map(|_| ()).map_err(|(_, m)| m.as_str()), app);
        answer(app, &stores, &work_token, n)
    });
    let limit = if kind.lane() == Lane::Slow { SETTLE_SLOW } else { SETTLE };
    let stores = Stores { flash: Arc::clone(&ctx.flash), ops: Arc::clone(&ctx.ops), owner: p.id };
    let to = back(kind, &input);
    let note = |ok: Option<bool>, text: &str| line(&ctx.app, &stores, &to, ok, text.to_owned());
    match job::once(&ctx, p.id, &nonce, &token, digest, kind.lane(), limit, work).await {
        Sent::Ran(resp) => resp,
        Sent::Done(r) => answer(&ctx.app, &stores, &token, next(kind, &input, r.as_ref().map(|_| ()).map_err(|(_, m)| m.as_str()), &ctx.app)),
        Sent::Expired => note(Some(false), "This page expired, so nothing was sent. Try again."),
        Sent::Unknown => note(None, "The router may still be working on it. Check before trying again."),
        Sent::Busy => note(Some(false), "Failed: the router is busy. Try again in a few seconds."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(q: &str) -> Vec<(String, String)> {
        Form::parse(q.as_bytes()).pairs().to_vec()
    }

    fn bad(r: Result<Input, Bad>) -> bool {
        matches!(r, Err(Bad::Request))
    }

    /// Only the app's bands and digits pass; bands sorted, each once.
    #[test]
    fn fields_are_checked() {
        assert_eq!(parse(Kind::Nr, &pairs("nr=78&nr=1&nr=78&x=1")).unwrap(), Input { server: None, bands: vec![1, 78], vals: vec![] });
        for q in ["nr=4", "nr=x", "nr=%2B1", "nr=", "nr=078a", "nr=99999"] {
            assert!(bad(parse(Kind::Nr, &pairs(q))), "{q}");
        }
        // LTE has band 4, NR does not; the other list's field is ignored.
        assert_eq!(parse(Kind::Lte, &pairs("lte=4&nr=4")).unwrap().bands, [4]);
        assert_eq!(parse(Kind::Speed, &pairs("server=12345")).unwrap().server, Some(12345));
        assert_eq!(parse(Kind::Speed, &pairs("server=")).unwrap().server, None);
        for q in ["server=-1", "server=1e3", "server=%201", "server=123456789012345678901"] {
            assert!(bad(parse(Kind::Speed, &pairs(q))), "{q}");
        }
        let i = Input { server: None, bands: vec![1, 3, 78], vals: vec![] };
        assert_eq!(query(Kind::Nr, &i), "?nr=1&nr=3&nr=78");
        assert_eq!(back(Kind::Unlock, &i), "/ui/tools/band-lock");
        assert_eq!(back(Kind::Nr, &i), "/ui/tools/band-lock?nr=1&nr=3&nr=78");
        let app = AppState::new();
        assert!(matches!(next(Kind::Nr, &i, Ok(()), &app), Next::Line(to, Some(true), l) if l == "NR bands locked: 1,3,78" && to == "/ui/tools/band-lock?nr=1&nr=3&nr=78"));
    }

    /// The lanes are the calls' (`server::lane`): only Enable ADB, Mobile
    /// Network's Apply and Device Controls' are not slow.
    #[test]
    fn lanes_follow_the_calls() {
        for k in KINDS {
            let general = matches!(k, Kind::Adb | Kind::Apply | Kind::PowerSave | Kind::FastBoot | Kind::AutoSleep);
            assert_eq!(k.lane(), if general { Lane::General } else { Lane::Slow }, "{k:?}");
            assert_eq!(Kind::by_path(k.path()), Some(k));
        }
    }

    /// The remote line goes on the band questions only (Enable ADB and the
    /// speed test do not cut the mobile link a remote page comes over).
    #[test]
    fn questions_name_what_happens() {
        let i = Input { server: None, bands: vec![3, 78], vals: vec![] };
        let q = question(Kind::Nr, &i, true, None).unwrap();
        assert_eq!((q.title.as_str(), q.label), ("Apply NR Lock?", Some("Apply NR Lock")));
        assert!(q.lines[0].ends_with("for NSA and SA: 3, 78.") && q.lines.last().unwrap() == REMOTE);
        assert!(!question(Kind::Nr, &i, false, None).unwrap().lines.contains(&REMOTE.to_owned()));
        assert_eq!(question(Kind::Lte, &Input::default(), false, None).unwrap().label, None);
        assert!(!question(Kind::Adb, &i, true, None).unwrap().lines.contains(&REMOTE.to_owned()));
        let servers = Ok(json!([{"id": 7, "sponsor": "Example Net", "name": "Testville", "country": "Nowhere"}]));
        let s = Input { server: Some(7), ..Input::default() };
        let q = question(Kind::Speed, &s, false, Some((&servers, &json!({"phase": "complete"})))).unwrap();
        assert_eq!((q.title.as_str(), q.lines[0].as_str()), ("Start the speed test?", "Server: Example Net - Testville, Nowhere"));
        assert!(!q.danger);
        let q = question(Kind::Speed, &s, false, Some((&servers, &json!({"phase": "upload"})))).unwrap();
        assert_eq!((q.title.as_str(), q.label), ("A speed test is already running.", None));
        let gone = Input { server: Some(8), ..Input::default() };
        assert_eq!(question(Kind::Speed, &gone, false, Some((&servers, &Value::Null))).unwrap().label, None);
        let failed = Err("fetch servers: timed out".to_owned());
        assert_eq!(question(Kind::Speed, &s, false, Some((&failed, &Value::Null))).unwrap().lines, ["fetch servers: timed out"]);
        // Every Tools question asks; a scan only a remote browser; a neighbour scan never.
        assert!(asks(Kind::Adb, &i, false) && !asks(Kind::Scan, &Input::default(), false) && asks(Kind::Scan, &Input::default(), true));
        assert!(!asks(Kind::Neighbors, &Input::default(), true));
    }
}
