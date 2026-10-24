//! Router › eSIM Profiles › Add eSIM (iOS `RouterESimAddView`, the download
//! part of `RouterESimViewModel`, `RouterESimActivationCode`), on the
//! agent's download API (`zte-agent/docs/esim.md`, Download).
//!
//! - **Entry** (no download running): the app's header, its "no mobile data"
//!   note, then one form (`multipart/form-data`, `web::upload`): "Scan QR
//!   code" (a file input with `capture="environment"`: a phone's browser
//!   opens the camera), "Choose QR from Photos" (a file input), "Enter code
//!   manually" (a password field) and the operator's confirmation code (a
//!   password field the app shows only for codes that need one: here a
//!   `<details>`, opened when the code asked for it), and "Read profile
//!   details". A photo is decoded on the router (`qr_decode`, one at a time,
//!   in the post's slow-lane task); the codes in it and a pasted code are
//!   checked as the app does (`parse`). Then `POST /api/esim/download` with
//!   `preview: true` (single-use token; its digest is keyed, so the token
//!   store holds no plain hash of a code).
//! - **Codes** (activation and confirmation) are only ever in the post's
//!   body and the agent's request: never rendered (the fields are always
//!   empty, so a refused post asks to scan or enter the code again), put in a
//!   URL, a fragment, a result line or a log, and never kept.
//! - **Download** (the app's download card; also with `?dl=1` once it has
//!   finished, as the eSIM Profiles card opens it): live from
//!   `esim_download` while it runs, waits for the preview answer or
//!   cancels (with the nav bar's "Close", "Done" once it ends); without
//!   JavaScript the page reloads every 3 s while it runs (not while it
//!   waits for the answer). The preview is the question:
//!   "Install <name>?" with "Install profile" (`POST
//!   /api/esim/download/confirm {"accept":true}`), which the agent cancels
//!   after 2 minutes; "Cancel download" (`POST /api/esim/download/cancel`)
//!   while the app offers it. Both are buttons of a form outside the live
//!   fragment (`#eaddf`, as SIM Services'), each post with the page's
//!   single-use token. Done: "Switch to it now" (the eSIM Profiles switch
//!   question, `?new=1`) or "Later"; failed: the error (never a part shaped
//!   like an activation code) and "Try again", with the code field open.
//! - The agent runs one eSIM operation at a time and needs the router's own
//!   mobile data; its refusals show in the app's "Can’t start this
//!   download" card.

use std::time::Duration;

use axum::extract::Request;
use axum::http::Method as HttpMethod;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::{json, Value};

use super::esim::{self, BUSY, DL, PATH};
use crate::auth::SessionId;
use crate::events::Topic;
use crate::handlers::AppState;
use crate::httpd::gate::Lane;
use crate::httpd::guard::ui_post_allowed;
use crate::httpd::{api, Ctx};
use crate::server::{self, Method};
use crate::web::job::{self, OnceJob, Sent};
use crate::web::kit::{self, Icon};
use crate::web::ops::is_op_id;
use crate::web::qr_decode::{self, Fail};
use crate::web::render::{self, once_field, Fragment, Views};
use crate::web::screen::{self, FormState, Notice, PageData, Req};
use crate::web::store::{Flash, Token, SETTLE_SLOW};
use crate::web::{fmt, session, upload, CspNonce};

pub const ADD: &str = esim::ADD;
/// Add eSIM with "Enter code manually" open.
const MANUAL: &str = "/ui/router/esim/add?manual=1";
pub const INSTALL: &str = "/ui/router/esim/add/install";
pub const CANCEL: &str = "/ui/router/esim/add/cancel";
const DOWNLOAD: &str = "/api/esim/download";
const CONFIRM_API: &str = "/api/esim/download/confirm";
const CANCEL_API: &str = "/api/esim/download/cancel";
/// The form of the download card's buttons (outside the live fragment).
const FORM: &str = "eaddf";

/// A photo's decode, at most (it takes well under a second on the router;
/// the decoding process is stopped after it).
const DECODE_LIMIT: Duration = if cfg!(test) { Duration::from_secs(4) } else { Duration::from_secs(15) };

// The app's input errors (`RouterESimAddView`, `RouterESimActivationCode`).
const INVALID: &str = "This QR code isn't an eSIM activation code.";
const ONE_CODE: &str = "Choose a photo with one eSIM QR code.";
const UNREADABLE: &str = "Couldn\u{2019}t read this photo. Choose another image or enter the code manually.";
const NEED_CC: &str = "Enter the confirmation code from your operator (up to 64 characters).";
// Web additions: no input, two, an image the router does not read, the
// decoder busy; and what a refused post asks (the code is never kept).
const NOTHING: &str = "Scan or choose a photo of the QR code, or enter the code manually.";
const TWO: &str = "Use one photo or the code entered manually, not both.";
const NOT_IMAGE: &str = "Choose a JPEG or PNG photo, or enter the code manually.";
const TOO_LARGE: &str = "This photo is too large to read. Choose a smaller image or enter the code manually.";
const DECODER_BUSY: &str = "The router is reading another photo. Try again in a moment.";
const AGAIN: &str = "Your code isn\u{2019}t kept, so scan, choose or enter it again.";
const NO_DATA: &str = "Your router has no mobile data connection. Connect it before downloading; your phone\u{2019}s own data can\u{2019}t be used.";

// --- The app's activation code check (`RouterESimActivationCode.parse`) ---

/// A checked activation code. Kept only for the post that read it.
pub struct Code {
    pub value: String,
    pub needs_cc: bool,
}

/// `LPA:1$<server>$<matching ID>[$<OID>[$1]]`, as the app accepts it:
/// trimmed, at most 512 bytes, no whitespace or control character, 3 to 5
/// `$` parts, the first `1`, the server `[A-Za-z0-9.:-]+`, the matching ID
/// `[A-Za-z0-9-]*`; a fifth part `1` asks for a confirmation code.
pub fn parse(input: &str) -> Option<Code> {
    let code = input.trim();
    if code.len() > 512 || code.chars().any(|c| c.is_whitespace() || control(c)) {
        return None;
    }
    let rest = code.get(..4).filter(|p| p.eq_ignore_ascii_case("lpa:")).map(|_| &code[4..])?;
    let parts: Vec<&str> = rest.split('$').collect();
    let ok = (3..=5).contains(&parts.len())
        && parts[0] == "1"
        && !parts[1].is_empty()
        && parts[1].bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
        && parts[2].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    ok.then(|| Code { value: format!("LPA:{rest}"), needs_cc: parts.len() == 5 && parts[4] == "1" })
}

/// The app's control characters (`CharacterSet.controlCharacters`): Cc,
/// and the invisible format characters a copied code may carry.
fn control(c: char) -> bool {
    c.is_control() || matches!(c, '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{feff}')
}

/// The app's `validConfirmationCode`: 1 to 64 bytes, no control character.
fn cc_ok(cc: &str) -> bool {
    !cc.is_empty() && cc.len() <= 64 && !cc.chars().any(control)
}

/// The one activation code among a photo's QR codes (each once).
fn one_code(found: &[String]) -> Result<Code, &'static str> {
    let mut valid: Vec<Code> = Vec::new();
    for c in found.iter().filter_map(|s| parse(s)) {
        if !valid.iter().any(|v| v.value == c.value) {
            valid.push(c);
        }
    }
    match valid.len() {
        0 => Err(INVALID),
        1 => Ok(valid.remove(0)),
        _ => Err(ONE_CODE),
    }
}

fn fail_text(f: Fail) -> &'static str {
    match f {
        Fail::NotImage => NOT_IMAGE,
        Fail::TooLarge => TOO_LARGE,
        Fail::Busy => DECODER_BUSY,
        Fail::Unreadable | Fail::TimedOut => UNREADABLE,
    }
}

// --- The page ---

fn download(v: &Views) -> Option<&Value> {
    v.value(Topic::EsimDownload)
}

/// While a download runs, waits for its answer or cancels: the stream
/// stays open (`Screen::live_while`).
pub fn live(v: &Views) -> bool {
    esim::downloading(esim::state(download(v)))
}

/// `#eadd-bar`: the nav bar's "Close" while a download runs, else "Done"
/// (the app's toolbar), live: it reads "Done" once the download ends.
pub fn bar(v: &Views) -> Markup {
    html! { a #eadd-bar .nl href=(PATH) { (if live(v) { "Close" } else { "Done" }) } }
}

/// The download card rather than the entry: while one runs, or `?dl=1`
/// and there is one to show.
fn shows_download(pd: &PageData) -> bool {
    let st = esim::state(download(pd.views));
    live(pd.views) || (pd.param("dl").as_deref() == Some("1") && st != "idle")
}

/// Without JavaScript, reload every 3 s while the download runs or
/// cancels (not while it waits for "Install profile"), keeping the result
/// line and the page's unused token (`t`).
pub fn refresh(pd: &PageData) -> Option<String> {
    let st = esim::state(download(pd.views));
    matches!(st, "running" | "cancelling").then(|| {
        let mut url = DL.to_owned();
        for k in ["r", "t"] {
            let v = if k == "t" { Some(pd.once()) } else { pd.param(k) };
            if let Some(v) = v.filter(|v| is_op_id(v)) {
                url.push_str(&format!("&{k}={v}"));
            }
        }
        url
    })
}

pub const FRAGMENTS: [Fragment; 2] = [
    Fragment { id: "eadd-dl", topics: &[Topic::EsimDownload], render: card },
    Fragment { id: "eadd-bar", topics: &[Topic::EsimDownload], render: bar },
];

/// An error of the operator's server as shown: its words, but never a part
/// that looks like an activation code (a server could echo the code).
fn scrub(e: &str) -> String {
    let words: Vec<&str> = e
        .split(char::is_whitespace)
        .map(|w| if w.contains('$') || w.to_ascii_lowercase().contains("lpa:") { "\u{2026}" } else { w })
        .collect();
    fmt::truncate(&words.join(" "), 200)
}

/// The app's name for the profile: provider, else name, else "eSIM profile".
fn display_name(d: &Value) -> String {
    let name = [&d["provider"], &d["name"]].iter().filter_map(|v| v.as_str()).map(str::trim).find(|s| !s.is_empty());
    fmt::truncate(name.unwrap_or("eSIM profile"), 64)
}

/// `#eadd-dl`: the app's download card. Its buttons belong to `#eaddf`
/// (outside the fragment: no token in a fragment).
fn card(v: &Views) -> Markup {
    let d = download(v).cloned().unwrap_or(Value::Null);
    let st = esim::state(Some(&d));
    let step = d["step"].as_str().unwrap_or("");
    let installing = step.starts_with("es10b_load_bound_profile_package");
    let can_cancel = matches!(st, "running" | "confirm") && !installing;
    let spin = matches!(st, "running" | "cancelling");
    let good = matches!(st, "done" | "confirm");
    let name = display_name(&d);
    let error = d["error"].as_str().map(str::trim).filter(|e| !e.is_empty());
    html! {
        section #eadd-dl .card.eprog.ewarn[!spin && !good] aria-live="polite" {
            div.ehead {
                @if spin {
                    span.eic { span.spin {} }
                } @else {
                    span.eic { (kit::icon(match st { "confirm" => Icon::SimCard, "done" => Icon::Check, _ => Icon::Alert })) }
                }
                b { @if st == "idle" { "No download in progress" } @else { (esim::download_text(&d)) } }
            }
            @match st {
                "confirm" => {
                    h2 { "Install " (name) "?" }
                    @if let Some(n) = d["name"].as_str().map(str::trim).filter(|n| !n.is_empty() && *n != name) { p.cap { (fmt::truncate(n, 64)) } }
                    p.det { "The profile will be installed on your router\u{2019}s eSIM and stay inactive until you switch to it. Confirm within 2 minutes." }
                    p.actions { button.btn.btn-wide.teal type="submit" form=(FORM) formaction=(INSTALL) { "Install profile" } }
                }
                "done" => {
                    p.det { "Added to your router. Your current profile is still active." }
                    p.actions {
                        @if d["iccid"].as_str().is_some_and(|i| !i.is_empty()) {
                            a.btn.teal href=(format!("{}?new=1", esim::SWITCH)) { "Switch to it now" }
                        } @else {
                            a.btn.btn-quiet href=(PATH) { "Refresh profiles" }
                        }
                        a.btn.btn-quiet href=(PATH) { "Later" }
                    }
                }
                "failed" => {
                    p { (error.map_or_else(|| "The operator couldn\u{2019}t add this profile.".to_owned(), scrub)) }
                    // The app's Try again opens the code field.
                    p.actions { a.btn href=(MANUAL) { "Try again" } }
                }
                "cancelled" | "idle" => {
                    @if error.is_some_and(|e| e.contains("2 minutes")) { p.det { "It wasn\u{2019}t confirmed within 2 minutes." } }
                    p.actions { a.btn.btn-quiet href=(ADD) { "Add eSIM" } }
                }
                _ => p.det {
                    (if installing {
                        "Keep the router powered on. Installation can\u{2019}t be cancelled at this stage."
                    } else {
                        "Keep your router connected to the internet. You can close this screen and return to check progress."
                    })
                },
            }
            @if can_cancel {
                p.actions { button.btn.btn-quiet.bad type="submit" form=(FORM) formaction=(CANCEL) { "Cancel download" } }
            }
        }
    }
}

/// A result line of Install profile or Cancel download (the app's
/// download note, orange, with "Check status").
pub fn note(n: Notice) -> Markup {
    let text = match n {
        Notice::Saved(m) => m.to_owned(),
        Notice::Flash(_, m) => m,
        Notice::Failed(e) => format!("Failed: {}", fmt::truncate(&e, 120)),
    };
    html! {
        div.eerr role="alert" {
            b { (kit::icon(Icon::Alert)) (text) }
            a.btn.btn-quiet.sm href=(DL) { "Check status" }
        }
    }
}

/// The form an input error or a refusal came back with.
fn flag(pd: &PageData, k: &str) -> Option<String> {
    pd.form.and_then(|f| f.error(k)).map(str::to_owned)
}

/// The router's data connection, read at page load: down (known and not
/// connected).
fn offline(pd: &PageData) -> bool {
    pd.read(0).and_then(|d| d["connect_status"].as_str()).is_some_and(|s| !s.trim().is_empty() && !esim::connected(s))
}

/// The app's entry view: what it does, one form for the photo (camera or
/// file) or the code, then the notes.
fn entry(pd: &PageData) -> Markup {
    let error = flag(pd, "input");
    let refusal = flag(pd, "note");
    let switching = pd.read(1).is_some_and(|s| esim::running(esim::state(Some(s))));
    let ignore = "true";
    html! {
        div.ahead {
            span.eic.ebig { (kit::icon(Icon::QrScan)) }
            h2 { "Add a profile to your router" }
            p.cap { "Use the eSIM QR code from your operator. You\u{2019}ll review the profile before installing it." }
        }
        @if offline(pd) { p.enote { (kit::icon(Icon::Wifi)) span { (NO_DATA) } } }
        @if let Some(n) = &pd.notice { (n) }
        @if switching && refusal.is_none() { p.ein role="status" { (kit::icon(Icon::Clock)) span { (BUSY) } } }
        @if let Some(e) = &error {
            div.ein role="alert" { (kit::icon(Icon::Alert)) span { (e) small { (AGAIN) } } }
        }
        @if let Some(n) = &refusal {
            section.card.emsg.ebad role="alert" {
                span.eic { (kit::icon(Icon::Alert)) }
                div {
                    b { "Can\u{2019}t start this download" }
                    p { (n) }
                    @if n.contains("no mobile data") && !offline(pd) { p.cap { (NO_DATA) } }
                    @if n.contains("agent password required") { p.cap { "Set the router agent password in Agent Settings, then try again." } }
                    p.cap { (AGAIN) }
                }
            }
        }
        form.card.eform method="post" action=(ADD) enctype="multipart/form-data" autocomplete="off" {
            (pd.csrf())
            (once_field(&pd.once()))
            label.fbtn.prim {
                span { (kit::icon(Icon::Camera)) "Scan QR code" }
                input type="file" name="scan" accept="image/jpeg,image/png" capture="environment";
            }
            label.fbtn {
                span { (kit::icon(Icon::Photo)) "Choose QR from Photos" }
                input type="file" name="photo" accept="image/jpeg,image/png";
            }
            details.eopt open[flag(pd, "manual").is_some() || pd.param("manual").as_deref() == Some("1")] {
                summary { (kit::icon(Icon::Keyboard)) "Enter code manually" }
                div.fld {
                    label for="code" { "Activation code" }
                    input #code type="password" name="code" placeholder="LPA:1$\u{2026}" maxlength="512" autocomplete="one-time-code"
                        autocapitalize="off" spellcheck="false" "data-1p-ignore" data-lpignore=(ignore) data-bwignore;
                }
            }
            details.eopt open[flag(pd, "cc").is_some()] {
                summary { (kit::icon(Icon::LockShield)) "Operator confirmation code (if you have one)" }
                div.fld {
                    label for="cc" { "Confirmation code" }
                    input #cc type="password" name="cc" placeholder="Operator confirmation code" maxlength="64" autocomplete="one-time-code"
                        autocapitalize="off" spellcheck="false" "data-1p-ignore" data-lpignore=(ignore) data-bwignore;
                    small.cap { "Enter the separate confirmation code supplied by your operator (up to 64 characters)." }
                }
            }
            button.btn.btn-wide.teal type="submit" disabled[switching] { "Read profile details" }
        }
        ul.hints {
            li { (kit::icon(Icon::Wifi)) "Your router needs internet to download the profile. Keep its Wi-Fi connected. Downloads usually take 10\u{2013}60 seconds." }
            li { (kit::icon(Icon::Lock)) "Your activation code is private and single-use. It is sent only to your router and isn\u{2019}t saved by OpenU60." }
        }
    }
}

/// Add eSIM.
pub fn page(pd: &PageData) -> Markup {
    if !shows_download(pd) {
        return html! { div.esim.eadd { (entry(pd)) } };
    }
    html! {
        div.esim.eadd {
            (render::live_snapshot(DL))
            @if let Some(n) = &pd.notice { (n) }
            // Its buttons are in the card while it can show them.
            @if live(pd.views) {
                form #eaddf method="post" action=(INSTALL) {
                    (pd.csrf())
                    (once_field(&pd.once()))
                }
            }
            (card(pd.views))
        }
    }
}

// --- Posts ---

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Post {
    /// Read profile details: the photo or the pasted code.
    Add,
    Install,
    Cancel,
}

impl Post {
    pub fn of(path: &str) -> Option<Post> {
        [Post::Add, Post::Install, Post::Cancel].into_iter().find(|k| k.path() == path)
    }

    pub fn path(self) -> &'static str {
        match self {
            Post::Add => ADD,
            Post::Install => INSTALL,
            Post::Cancel => CANCEL,
        }
    }
}

pub async fn post(ctx: Ctx, req: Request, k: Post) -> Response {
    match k {
        Post::Add => add(ctx, req).await,
        _ => decide(ctx, req, k).await,
    }
}

/// What a post reads the code from (a photo with the upload slot, held
/// until it is decoded).
enum Input {
    Photo(Vec<u8>, upload::Held),
    Code(String),
}

/// The entry again (`400` and the like) with an input error (`input`) or
/// the agent's refusal (`note`), and the parts of the form to open
/// (`manual`, `cc`).
fn again(key: &'static str, text: &str, open: &[&'static str]) -> FormState {
    let mut errors = vec![(key, text.to_owned())];
    errors.extend(open.iter().map(|o| (*o, String::new())));
    FormState { values: Vec::new(), errors }
}

/// The agent's refusal as the card says it: its "busy" 409s as the app's
/// busy line, else its words (scrubbed as a server's error).
fn refusal(status: u16, e: &str) -> String {
    if esim::busy_error(status, e) {
        BUSY.to_owned()
    } else {
        scrub(e)
    }
}

/// `POST /ui/router/esim/add`: the session and same origin first (before
/// reading up to 8 MiB), the upload and its CSRF token, which input, the
/// checks the app makes, then the single-use token's work in the slow
/// lane: the photo's decode, the checks of its code, and the download's
/// start.
async fn add(ctx: Ctx, req: Request) -> Response {
    let nonce = CspNonce::of(&req);
    let r = Req { query: None, ..Req::of(&ctx, &req) };
    let shown = Req { query: None, ..Req::of(&ctx, &req) };
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, ADD, None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    if !ui_post_allowed(&api::req_headers(&parts.headers)) {
        return session::reply_unread(&parts.headers, body, render::form_refused(&nonce, ADD)).await;
    }
    let up = match upload::read(&ctx, &parts.headers, body, &nonce, ADD).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, up.form.get("csrf")) {
        return render::form_refused(&nonce, ADD);
    }
    let s = &crate::web::tabs::router::ESIM_ADD;
    let token = up.form.get("once").unwrap_or("").to_owned();
    let cc = up.form.get("cc").unwrap_or("").trim().to_owned();
    let pasted = up.form.get("code").unwrap_or("").trim().to_owned();
    let show = |r: Req, fs: FormState, status: u16| {
        let ctx = ctx.clone();
        let p = p.clone();
        async move { screen::show(&ctx, p, Req { typed: Some(fs), ..r }, s, None, status).await }
    };
    let files = up.files().len();
    // The photo (with the upload slot), else the upload goes now.
    let photo = if files == 1 && pasted.is_empty() {
        up.into_file()
    } else {
        drop(up);
        None
    };
    // The app's checks of what can be checked now: one input, the
    // confirmation code, a pasted code (a photo's once it is read). A
    // refused post drops the photo before its page is drawn.
    let checked = match (files, pasted.is_empty(), photo) {
        (0, true, _) => Err(again("input", NOTHING, &[])),
        (_, no_code, _) if !cc.is_empty() && !cc_ok(&cc) => Err(again("input", NEED_CC, if no_code { &["cc"] } else { &["manual", "cc"] })),
        (1, true, Some((bytes, held))) => Ok((format!("photo:{}", bytes.len()), Input::Photo(bytes, held))),
        (0, false, _) => match parse(&pasted) {
            None => Err(again("input", INVALID, &["manual"])),
            Some(code) if code.needs_cc && cc.is_empty() => Err(again("input", NEED_CC, &["manual", "cc"])),
            Some(code) => Ok((code.value, Input::Code(pasted))),
        },
        _ => Err(again("input", TWO, &[])),
    };
    let (what, input) = match checked {
        Ok(found) => found,
        Err(fs) => return show(shown, fs, 400).await,
    };
    // Keyed: the token store never holds a plain hash of a code.
    let keyed = ctx.app.auth.opaque_key("esim-add", &format!("{what}\n{cc}")).unwrap_or_default();
    drop(what);
    let digest = job::digest(&[ADD, &keyed]);
    let pw = p.clone();
    let work: OnceJob = Box::new(move |app: &AppState, claim| {
        let refused = |fs: FormState, status: u16| screen::render(app, s, &pw, &r, None, Some(&fs), status);
        let code = match input {
            Input::Code(c) => parse(&c),
            Input::Photo(bytes, held) => {
                let found = qr_decode::codes(bytes, DECODE_LIMIT);
                drop(held);
                match found.map_err(fail_text).and_then(|f| one_code(&f)) {
                    Ok(code) => Some(code),
                    Err(text) => return refused(again("input", text, &[]), 400),
                }
            }
        };
        let Some(code) = code else { return refused(again("input", INVALID, &["manual"]), 400) };
        if code.needs_cc && cc.is_empty() {
            return refused(again("input", NEED_CC, &["cc"]), 400);
        }
        // One eSIM operation at a time (the token goes back).
        if esim::busy_now(app) {
            return refused(again("note", BUSY, &[]), 409);
        }
        let mut body = json!({"activation_code": code.value, "preview": true});
        if !cc.is_empty() {
            body["confirmation_code"] = json!(cc);
        }
        claim.disarm();
        let out = super::call(app, Method::Post, DOWNLOAD, &body);
        drop((body, code));
        match out {
            Ok(_) => {
                claim.set(Token::Sent(claim.digest()));
                session::see_other(DL)
            }
            Err((status, e)) => {
                let text = refusal(status, &e);
                claim.set(Token::Failed(claim.digest(), status, text.clone()));
                refused(again("note", &text, &[]), status)
            }
        }
    });
    match job::once(&ctx, p.id, &nonce, &token, digest, Lane::Slow, SETTLE_SLOW, work).await {
        Sent::Ran(resp) => resp,
        Sent::Done(Ok(())) | Sent::Unknown => session::see_other(DL),
        Sent::Done(Err((status, e))) => show(shown, again("note", &e, &[]), status).await,
        Sent::Expired => show(shown, again("input", "This page expired, so nothing was sent. Try again.", &[]), 409).await,
        Sent::Busy => show(shown, again("input", "Failed: the router is busy. Try again in a few seconds.", &[]), 503).await,
    }
}

/// `POST` of Install profile or Cancel download (`#eaddf`): the session,
/// the form and its CSRF token, then, with the single-use token, the
/// download's state read again (nothing is sent when it no longer waits
/// for the answer or can no longer be cancelled) and the agent's call.
async fn decide(ctx: Ctx, req: Request, k: Post) -> Response {
    let nonce = CspNonce::of(&req);
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, DL, None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&nonce, DL);
    }
    let (api_path, body) = match k {
        Post::Install => (CONFIRM_API, json!({"accept": true})),
        _ => (CANCEL_API, json!({})),
    };
    let token = form.get("once").unwrap_or("").to_owned();
    let (flash, owner, token_w) = (std::sync::Arc::clone(&ctx.flash), p.id, token.clone());
    let work: OnceJob = Box::new(move |app: &AppState, claim| {
        let d = match super::api_get(app, DOWNLOAD) {
            Ok(d) => d,
            Err(e) => return line(app, &flash, owner, None, format!("Failed: {}", fmt::truncate(&e, 120))),
        };
        let st = esim::state(Some(&d));
        let installing = d["step"].as_str().is_some_and(|s| s.starts_with("es10b_load_bound_profile_package"));
        let fits = match k {
            Post::Install => st == "confirm",
            _ => matches!(st, "running" | "confirm") && !installing,
        };
        // It moved on (the 2 minutes ran out, another browser or the app
        // answered): the card shows where it is; the token goes back.
        if !fits {
            return session::see_other(DL);
        }
        claim.disarm();
        match super::call(app, Method::Post, api_path, &body) {
            Ok(_) => {
                claim.set(Token::Sent(claim.digest()));
                session::see_other(DL)
            }
            Err((status, e)) => {
                let text = fmt::truncate(&e, 120);
                claim.set(Token::Failed(claim.digest(), status, text.clone()));
                line(app, &flash, owner, Some(&token_w), text)
            }
        }
    });
    let lane = server::lane(Method::Post, api_path);
    let note = |text: &str| line(&ctx.app, &ctx.flash, owner, None, text.to_owned());
    let digest = job::digest(&[k.path()]);
    match job::once(&ctx, owner, &nonce, &token, digest, lane, SETTLE_SLOW, work).await {
        Sent::Ran(resp) => resp,
        Sent::Done(Err(_)) if ctx.flash.get(&token, owner, ctx.app.auth.now()).is_some() => session::see_other(&format!("{DL}&r={token}")),
        Sent::Done(Err((_, e))) => note(&e),
        Sent::Done(Ok(())) | Sent::Unknown => session::see_other(DL),
        Sent::Expired => note("This page expired, so nothing was sent. Try again."),
        Sent::Busy => note("Failed: the router is busy. Try again in a few seconds."),
    }
}

/// 303 to the download card with `text` for this session (the app's
/// download note), kept under the post's token when it sent something (a
/// replay finds it), else under a random id.
fn line(app: &AppState, flash: &Flash, owner: SessionId, token: Option<&str>, text: String) -> Response {
    let id = match token.filter(|t| is_op_id(t) && flash.get(t, owner, app.auth.now()).is_none()) {
        Some(t) => t.to_owned(),
        None => match app.auth.random_hex(8) {
            Ok(id) => id,
            Err(_) => return session::see_other(DL),
        },
    };
    flash.insert(id.clone(), owner, (Some(false), text), app.auth.now());
    session::see_other(&format!("{DL}&r={id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app's code check, fixtures obviously fake.
    #[test]
    fn codes_are_checked_as_the_app_does() {
        let c = parse("  LPA:1$example.com$FAKE-MATCHING-ID \n").unwrap();
        assert_eq!((c.value.as_str(), c.needs_cc), ("LPA:1$example.com$FAKE-MATCHING-ID", false));
        let c = parse("lpa:1$smdp.example.com:443$FAKE-0000$1.3.6.1.4.1.31746$1").unwrap();
        assert_eq!((c.value.as_str(), c.needs_cc), ("LPA:1$smdp.example.com:443$FAKE-0000$1.3.6.1.4.1.31746$1", true));
        assert!(!parse("LPA:1$example.com$FAKE$$0").unwrap().needs_cc);
        assert!(parse("LPA:1$example.com$").is_some(), "an empty matching ID, as the app");
        for bad in [
            "", "LPA:2$example.com$FAKE", "LPA:1$$FAKE", "LPA:1$example.com", "LPA:1$a$b$c$1$x", "LPA:1$evil.example/x$FAKE",
            "LPA:1$example.com$FAKE_ID", "LPA:1$example.com$FAKE ID", "LPA:1$example.com$FAKE\u{7}", "WIFI:T:WPA;S:x;;",
            "https://example.com/esim", "LPA:1$exämple.com$FAKE", "LPA:1$example.com$FAKE$1.3\u{200b}$1",
        ] {
            assert!(parse(bad).is_none(), "{bad}");
        }
        assert!(parse(&format!("LPA:1$example.com${}", "A".repeat(512 - 17))).is_none());
        assert!(parse(&format!("LPA:1$example.com${}", "A".repeat(512 - 18))).is_some());
        assert!(cc_ok("12345678") && cc_ok(&"x".repeat(64)) && !cc_ok("") && !cc_ok(&"x".repeat(65)) && !cc_ok("1\n2") && !cc_ok("12\u{feff}"));
    }

    /// A photo's codes: the one activation code; none or several refused
    /// with the app's words (the same code twice is one).
    #[test]
    fn a_photo_needs_one_activation_code() {
        let code = "LPA:1$example.com$FAKE-MATCHING-ID".to_owned();
        let other = "LPA:1$smdp.example.com$FAKE-0000-0000-MATCHING-ID".to_owned();
        assert_eq!(one_code(std::slice::from_ref(&code)).unwrap().value, code);
        assert_eq!(one_code(&["WIFI:S:x;;".into(), code.clone()]).unwrap().value, code);
        assert_eq!(one_code(&[code.clone(), "x".into(), code.clone()]).unwrap().value, code);
        assert_eq!(one_code(&[]).err(), Some(INVALID));
        assert_eq!(one_code(&["https://example.com".into()]).err(), Some(INVALID));
        assert_eq!(one_code(&[code, other]).err(), Some(ONE_CODE));
    }
}
