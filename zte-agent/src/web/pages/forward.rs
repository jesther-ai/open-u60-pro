//! SMS Forwarding (iOS `SMSForwardConfigView`, `SMSForwardRuleFormView`),
//! in two tabs: pushed from Messages' gear (`/ui/sms/forward`, "‹ Messages")
//! and listed under Tools › Automation (`/ui/tools/sms-forward`, "‹ Tools");
//! each mount has its tab, its way back and its forms' targets. The model
//! is `web::forward`.
//!
//! - **The page** (a table screen per mount, `tabs::sms`): "Settings" with
//!   the "Enabled" switch (applies at once) and the two after-forward
//!   switches with "Save Settings", the rules (a link to edit each, its
//!   switch, and "Delete", the app's swipe, which asks first), "New Rule",
//!   the Forward Log row and "Last Forwarded ID". The config is read
//!   without its secrets: a webhook shows its host only.
//! - **New Rule / Edit Rule** (`…/rules/new`, `…/rules/<id>`): name, filter,
//!   destination with each type's fields (CSS shows the chosen type's;
//!   the server reads only those), "Test Destination" and "Save". Secrets
//!   are fields that are never filled (empty keeps the saved one, for the
//!   same type and server: `forward::resolve`). Save and Test carry a
//!   single-use token (a double click creates one rule, sends one test);
//!   Test answers with the form again and its result, a new token and the
//!   typed secrets kept server-side for the next post (`store::Drafts`; a
//!   form whose draft is gone says so rather than fall back to the rule's).
//!   "Add Header" draws the form again with another header row. A test to
//!   an SMS destination costs an SMS: it is confirmed first.
//! - **Forward Log** (`…/log`, a table screen per mount): the entries
//!   without the messages' text, "Clear" (asked first: `…/log/clear`; held
//!   forwards stay) and "Retry" per failure that is not held: single-use,
//!   by the entry's fingerprint (`forward::Fingerprint`), in the slow
//!   lane; the result is a line on the log (`?r=`).

use std::sync::Arc;

use axum::extract::Request;
use axum::http::Method as HttpMethod;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::{json, Value};

use crate::auth::{AuthState, Principal, SessionId};
use crate::handlers::AppState;
use crate::httpd::gate::{self, Lane};
use crate::httpd::Ctx;
use crate::server::Method;
use crate::web::action::{self, Ask};
use crate::web::forward::{self, Dest, Errors, Fingerprint, Input, Kind, Secrets, DESTS, FILTERS, MAX_HEADERS};
use crate::web::kit::{self, Act, Icon, Tab};
use crate::web::render::{self, PageCtx};
use crate::web::screen::PageData;
use crate::web::session;
use crate::web::store::{settle, Claim, Draft, Drafts, Flash, Once, Settled, Token, SETTLE, SETTLE_SLOW};
use crate::web::{fmt, job, sms, CspNonce};

/// One place the forwarding screens are mounted.
/// (Its way back is its screens' nav bar, `tabs::sms`.)
pub struct Mount {
    pub base: &'static str,
    /// Its Forward Log.
    pub log: &'static str,
    pub tab: Tab,
}

pub static SMS: Mount = Mount { base: "/ui/sms/forward", log: "/ui/sms/forward/log", tab: Tab::Sms };
pub static TOOLS: Mount = Mount { base: "/ui/tools/sms-forward", log: "/ui/tools/sms-forward/log", tab: Tab::Tools };
static MOUNTS: [&Mount; 2] = [&SMS, &TOOLS];

impl Mount {
    fn at(&self, rest: &str) -> String {
        format!("{}{rest}", self.base)
    }

    fn form_url(&self, id: Option<u32>) -> String {
        match id {
            Some(id) => self.at(&format!("/rules/{id}")),
            None => self.at("/rules/new"),
        }
    }

    /// The page with this `?saved=` notice.
    fn saved(&self, key: &str) -> String {
        self.at(&format!("?saved={key}"))
    }
}

/// The page's notices (the app's words).
pub const SAVED: [(&str, &str); 4] = [
    ("settings", "Settings saved"),
    ("rule-created", "Rule created"),
    ("rule-updated", "Rule updated"),
    ("rule-deleted", "Rule deleted"),
];

// --- The page ---

/// A row with a switch that a form submits later (`form`): a checkbox.
fn check_switch(id: &str, label: &str, name: &str, on: bool, form: Option<&str>) -> Markup {
    html! {
        li.it {
            label.tl for=(id) {
                span { (label) }
                input.sr id=(id) type="checkbox" role="switch" name=(name) value="1" checked[on] form=[form];
                span.sw aria-hidden="true" {}
            }
        }
    }
}

/// A rule: its name, filter and destination (a link to edit it), its
/// switch, and "Delete" (the app's swipe; asked first).
fn rule_row(csrf: &Markup, m: &Mount, r: &Value) -> Markup {
    let id = r["id"].as_u64().unwrap_or(0);
    let name = r["name"].as_str().unwrap_or("");
    let on = r["enabled"].as_bool().unwrap_or(true);
    html! {
        li.cr.fr id=(format!("r{id}")) {
            a.it.cv href=(m.at(&format!("/rules/{id}"))) {
                span.cvb {
                    span.nm { (name) }
                    span.rs { (forward::filter_summary(&r["filter"])) }
                    span.rs.rd { (forward::dest_summary(&r["destination"])) }
                }
            }
            form.sf method="post" action=(m.at("/rules/toggle")) {
                (csrf)
                input type="hidden" name="id" value=(id);
                input type="hidden" name="enabled" value=(if on { "0" } else { "1" });
                button.sw type="submit" role="switch" aria-checked=(on) aria-label=(name) {}
            }
            a.del href=(m.at(&format!("/rules/delete?id={id}"))) { "Delete" span.sr { " rule " (name) } }
        }
    }
}

/// "New Rule", "Forward Log" and, once a message was forwarded, "Last
/// Forwarded ID".
fn tail(m: &Mount, last: u64) -> Markup {
    html! {
        (kit::section(None, None, html! {
            li { a.it.bt href=(m.at("/rules/new")) { (kit::icon(Icon::Plus)) span { "New Rule" } } }
        }))
        (kit::section(None, None, kit::link_row(m.log, Some(Icon::Doc), "Forward Log")))
        @if last > 0 {
            (kit::section(None, None, html! { li.it { span.muted { "Last Forwarded ID" } span.tm.num { (last) } } }))
        }
    }
}

/// The page (`SMSForwardConfigView`), from the config read without secrets.
pub fn page(pd: &PageData, m: &Mount) -> Markup {
    let cfg = match pd.reads.first() {
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            return html! {
                (kit::notice(false, &format!("Failed to load: {}", fmt::truncate(e, 120))))
                (tail(m, 0))
            }
        }
        None => return html! {},
    };
    let csrf = pd.csrf();
    let enabled = m.at("/enabled");
    let flag = |k: &str| cfg[k].as_bool().unwrap_or(false);
    let rules = cfg["rules"].as_array().map(Vec::as_slice).unwrap_or_default();
    html! {
        (kit::section(Some("Settings"), None, html! {
            (kit::switch_row(&csrf, "Enabled", Some(flag("enabled")),
                Act::Post { action: &enabled, name: "enabled", value: "1" },
                Act::Post { action: &enabled, name: "enabled", value: "0" }))
            (check_switch("fw-mr", "Mark Read After Forward", "mark_read", flag("mark_read_after_forward"), Some("fwset")))
            (check_switch("fw-da", "Delete After Forward", "delete_after", flag("delete_after_forward"), Some("fwset")))
            li {
                form #fwset method="post" action=(m.at("/settings")) {
                    (csrf)
                    button.it.cv.snd type="submit" { "Save Settings" }
                }
            }
        }))
        @if rules.is_empty() {
            (kit::empty_state(Icon::EnvelopeBranch, "No Rules",
                "Add forwarding rules to automatically forward SMS messages to Telegram, Discord, webhooks, and more."))
        } @else {
            (kit::section(Some("Rules"), None, html! { @for r in rules { (rule_row(&csrf, m, r)) } }))
        }
        (tail(m, cfg["last_forwarded_id"].as_u64().unwrap_or(0)))
    }
}

/// "Delete <name>?" (the app deletes at once): from the config read and
/// `?id=`. A rule that is gone (or a failed read) offers only Cancel.
pub fn page_delete(pd: &PageData, m: &Mount) -> Markup {
    let id = pd.param("id").and_then(|v| v.parse::<u64>().ok());
    let rule = asked(pd);
    let action = m.at("/rules/delete");
    let id_text = id.unwrap_or(0).to_string();
    let (title, note, label) = match (pd.reads.first(), rule) {
        (Some(Err(e)), _) => ("Delete rule?".to_owned(), Some(format!("Failed to load: {}", fmt::truncate(e, 120))), None),
        // As Edit Rule's.
        (_, None) => ("Delete rule?".to_owned(), Some("Rule not found. It may have been deleted.".to_owned()), None),
        (_, Some(r)) => (format!("Delete \u{201c}{}\u{201d}?", r["name"].as_str().unwrap_or("")), None, Some("Delete")),
    };
    let lines: &[&str] = if label.is_some() {
        &[
            "New messages are no longer forwarded with this rule.",
            "Forwards it holds until the connection returns are dropped; its log entries stay.",
        ]
    } else {
        &[]
    };
    let q = Ask { title: &title, lines, action: &action, hidden: &[("id", id_text.as_str())], label, danger: true, cancel: m.base };
    html! {
        @if let Some(n) = &note { (kit::notice(false, n)) }
        (action::ask(pd.csrf(), &q))
    }
}

/// The delete question's rule (`?id=`), if the read has it.
fn asked<'a>(pd: &'a PageData) -> Option<&'a Value> {
    let id = pd.param("id").and_then(|v| v.parse::<u64>().ok())?;
    pd.read(0)?["rules"].as_array()?.iter().find(|r| r["id"].as_u64() == Some(id))
}

/// The delete question's status, as Edit Rule's: 404 for a rule that is
/// gone, 503 when the rules cannot be read.
pub fn delete_status(pd: &PageData) -> u16 {
    match pd.reads.first() {
        Some(Err(_)) => 503,
        _ if asked(pd).is_none() => 404,
        _ => 200,
    }
}

// --- Forward Log ---

/// The log's notice: "Clear" answers on the log (the app shows it on SMS
/// Forwarding, behind its log screen).
pub const LOG_SAVED: [(&str, &str); 1] = [("log-cleared", "Log cleared")];

const RETRIED: &str = "Retry succeeded";
const LOG_CHANGED: &str = "The log changed. Try again.";
const LOG_EXPIRED: &str = "This page expired, so nothing was retried. Try again.";
const RETRY_MAYBE: &str = "The retry may still be running. Check the log before retrying again.";

/// An entry (`SMSForwardLogView`): the sender and the result ("Held, will
/// retry", or a check or cross), "<rule> → <type>", the preview, the
/// error (red; grey while held), the time (the router's zone at that
/// time, as the SMS pages); "Retry" (the app's swipe) for a failure that
/// is not held, with the page's single-use token.
fn log_row(csrf: &Markup, token: &str, m: &Mount, e: &Value) -> Markup {
    let s = |k: &str| e[k].as_str().unwrap_or("");
    let (ok, held) = (e["success"].as_bool().unwrap_or(false), e["pending"].as_bool().unwrap_or(false));
    let fp = Fingerprint::of(e);
    html! {
        li.it.lg {
            div.l1 {
                span.nm { (s("sender")) }
                @if held {
                    span.c-orange.hl { (kit::icon(Icon::ClockArrow)) "Held, will retry" }
                } @else if ok {
                    span.c-green role="img" aria-label="Forwarded" { (kit::icon(Icon::Check)) }
                } @else {
                    span.c-red role="img" aria-label="Failed" { (kit::icon(Icon::Cross)) }
                }
            }
            span.rs { (s("rule_name")) " \u{2192} " (s("destination_type")) }
            span.pv { (s("content_preview")) }
            @if let Some(err) = e["error"].as_str() { span.rs.er[!held] { (fmt::truncate(err, 200)) } }
            div.l2 {
                span.tm { (sms::stamp(fp.timestamp, sms::local_offset(fp.timestamp))) }
                @if !ok && !held {
                    form.sf method="post" action=(m.at("/log/retry")) {
                        (csrf)
                        (render::once_field(token))
                        input type="hidden" name="i" value=(fp.index);
                        input type="hidden" name="sms" value=(fp.sms_id);
                        input type="hidden" name="rule" value=(fp.rule_id);
                        input type="hidden" name="ts" value=(fp.timestamp);
                        button.btn.sm type="submit" { "Retry" span.sr { " forward from " (s("sender")) } }
                    }
                }
            }
        }
    }
}

/// Forward Log (`SMSForwardLogView`), newest first, from the log read
/// without the messages' text (`forward::public_log`). One single-use
/// token serves every "Retry" of the page (each answer is a new page).
pub fn log(pd: &PageData, m: &Mount) -> Markup {
    let entries = match pd.reads.first() {
        Some(Ok(v)) => v.as_array().map(Vec::as_slice).unwrap_or_default(),
        Some(Err(e)) => return kit::notice(false, &format!("Failed to load log: {}", fmt::truncate(e, 120))),
        None => return html! {},
    };
    if entries.is_empty() {
        return kit::empty_state(Icon::Doc, "No Log Entries", "Forwarded messages will appear here");
    }
    let retry = entries.iter().any(|e| e["success"] != true && e["pending"] != true);
    let token = if retry { pd.once() } else { String::new() };
    let csrf = pd.csrf();
    kit::section(None, None, html! { @for e in entries { (log_row(&csrf, &token, m, e)) } })
}

/// "Clear the forward log?" (the app clears at once).
pub fn ask_clear(pd: &PageData, m: &Mount) -> Markup {
    let action = m.at("/log/clear");
    let q = Ask {
        title: "Clear the forward log?",
        lines: &[
            "Forwarded and failed entries are removed, except failures of a message that is still held.",
            "Held forwards are kept and still sent once the connection returns.",
        ],
        action: &action,
        hidden: &[],
        label: Some("Clear"),
        danger: true,
        cancel: m.log,
    };
    action::ask(pd.csrf(), &q)
}

/// The log with this result line, kept for the session (`?r=<id>`).
fn logged(flash: &Flash, auth: &AuthState, owner: SessionId, m: &Mount, ok: bool, msg: String) -> Response {
    let Ok(id) = auth.random_hex(8) else { return session::see_other(m.log) };
    let to = format!("{}?r={id}", m.log);
    flash.insert(id, owner, (Some(ok), msg), auth.now());
    session::see_other(&to)
}

/// `POST …/log/retry`: a session (else the login, nothing was retried),
/// the form, origin and CSRF token, the entry's fingerprint (else 400),
/// then the page's single-use token (claimed for this entry; a double
/// click retries once and shows what the first did). In the slow lane:
/// the log as it is now, the entry by its fingerprint ("The log changed.
/// Try again." if it is gone or was retried meanwhile), the agent's retry
/// by its index now. The answer is the log with "Retry succeeded" or
/// "Retry failed: …" (the app's words).
async fn retry(ctx: Ctx, req: Request, m: &'static Mount) -> Response {
    let nonce = CspNonce::of(&req);
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, parts.uri.path(), None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&nonce, m.log);
    }
    let Some(fp) = Fingerprint::parse(&form) else {
        return render::bad_request(&nonce);
    };
    let (once, auth, flash) = (Arc::clone(&ctx.once), Arc::clone(&ctx.app.auth), Arc::clone(&ctx.flash));
    let (token, digest, owner) = (form.get("once").unwrap_or(""), fp.digest(), p.id);
    let done = |ok: bool, msg: &str| logged(&flash, &auth, owner, m, ok, msg.to_owned());
    let claim = match once.get(token, owner, auth.now()) {
        None => return done(false, LOG_EXPIRED),
        Some(Token::Fresh) => Claim::take(&once, &auth, token, owner, digest),
        Some(_) => None,
    };
    let claim = match claim {
        Some(c) => c,
        // A double click, a reload of the answer: what the first post did.
        None => match settle(&once, token, owner, digest, &auth, SETTLE_SLOW).await {
            Settled::Sent => return done(true, RETRIED),
            Settled::Failed(_, msg) => return done(false, &msg),
            Settled::Other => return done(false, LOG_EXPIRED),
            Settled::Unknown => return done(false, RETRY_MAYBE),
            Settled::Fresh => match Claim::take(&once, &auth, token, owner, digest) {
                Some(c) => c,
                None => return done(false, RETRY_MAYBE),
            },
        },
    };
    let (f, a) = (Arc::clone(&flash), Arc::clone(&auth));
    let job = Box::new(move |app: &AppState| {
        let (ok, msg) = run_retry(app, &fp, claim);
        logged(&f, &a, owner, m, ok, msg)
    });
    match job::try_run(&ctx, Lane::Slow, &nonce, job).await {
        Some(resp) => resp,
        // The claim went with the work: given back, nothing sent.
        None => done(false, &format!("Retry failed: {BUSY}")),
    }
}

/// The retry of the entry `fp` is for, with a claimed token (blocking).
/// The agent retries by index, so the index is the entry's in the log
/// read just before.
fn run_retry(app: &AppState, fp: &Fingerprint, mut claim: Claim) -> (bool, String) {
    let failed = |e: &str| format!("Retry failed: {}", fmt::truncate(&forward::redact(e), 120));
    let log = match forward::data(forward::api(app, Method::Get, forward::LOG, &Value::Null)) {
        Ok(log) => log,
        Err((_, e)) => return (false, failed(&e)),
    };
    let Some(index) = fp.find(&log) else {
        return (false, LOG_CHANGED.to_owned());
    };
    claim.disarm();
    match forward::data(forward::api(app, Method::Post, forward::RETRY, &json!({"index": index}))) {
        Ok(_) => {
            claim.set(Token::Sent(claim.digest()));
            (true, RETRIED.to_owned())
        }
        Err((status, e)) => {
            let msg = failed(&e);
            claim.set(Token::Failed(claim.digest(), status, msg.clone()));
            (false, msg)
        }
    }
}

// --- New Rule / Edit Rule ---

/// A page of the rule form: New Rule or Edit Rule (by id).
pub enum Page {
    New(&'static Mount),
    Edit(&'static Mount, u32),
}

/// A rule id in a path: digits, 1 and up.
fn rule_id(s: &str) -> Option<u32> {
    s.parse::<u32>().ok().filter(|n| *n > 0 && s.bytes().all(|b| b.is_ascii_digit()))
}

impl Page {
    pub fn of(path: &str) -> Option<Page> {
        MOUNTS.into_iter().find_map(|m| {
            let rest = path.strip_prefix(m.base)?.strip_prefix("/rules/")?;
            match rest {
                "new" => Some(Page::New(m)),
                id => rule_id(id).map(|id| Page::Edit(m, id)),
            }
        })
    }
}

/// The rule form's posts and the log's Retry.
pub enum Post {
    Save(&'static Mount),
    Test(&'static Mount),
    Retry(&'static Mount),
}

impl Post {
    pub fn of(path: &str) -> Option<Post> {
        MOUNTS.into_iter().find_map(|m| match path.strip_prefix(m.base)? {
            "/rules/save" => Some(Post::Save(m)),
            "/rules/test" => Some(Post::Test(m)),
            "/log/retry" => Some(Post::Retry(m)),
            _ => None,
        })
    }
}

/// The page whose form posts to `path` (`routes::form_page`).
pub fn form_page(path: &str) -> Option<&'static str> {
    Post::of(path).map(|p| match p {
        Post::Save(m) | Post::Test(m) => m.base,
        Post::Retry(m) => m.log,
    })
}

/// What a rule form is drawn with.
struct Draw<'a> {
    p: &'a Principal,
    nonce: &'a str,
    once: &'a Once,
    drafts: &'a Drafts,
    m: &'a Mount,
    /// The secrets of the rule being edited.
    stored: Option<&'a Secrets>,
}

/// A notice above the form: (success, text).
type Note = Option<(bool, String)>;

fn err_of<'e>(errors: &'e Errors, name: &str) -> Option<&'e str> {
    errors.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// A field's message under it.
fn field_err(id: &str, err: Option<&str>) -> Markup {
    html! { @if let Some(e) = err { p.ferr id=(format!("{id}-err")) { (e) } } }
}

/// A secret the form keeps when its field is left empty: whether it is the
/// rule's (else typed into this form before), and a secret URL's host.
type Kept = Option<(bool, Option<String>)>;

/// A password field that is never filled, and that password managers
/// leave alone (`new-password`: never filled with the router's password).
fn secret_input(id: Option<&str>, name: &str, label: Option<&str>, placeholder: &str, err: Option<&str>) -> Markup {
    let err_id = format!("{name}-err");
    html! {
        input id=[id] type="password" name=(name) aria-label=[label] placeholder=(placeholder) autocomplete="new-password"
            "data-1p-ignore" data-lpignore="true" data-bwignore aria-invalid=[err.map(|_| "true")]
            aria-describedby=[id.and(err).map(|_| err_id.as_str())];
    }
}

/// A text input with its label above it. A secret is never filled: a
/// token is a password field, a secret URL a plain one (the app shows it
/// as typed), with "Saved: <host>" or "Entered: <host>" when one is kept.
fn input_row(class: &str, f: &forward::Field, value: &str, kept: Kept, err: Option<&str>) -> Markup {
    let placeholder = match (&kept, f.kind) {
        (Some(_), _) => "Leave empty to keep",
        (None, Kind::Url) => f.default,
        _ if f.required => "Required",
        _ => "Optional",
    };
    let (ty, mode) = match f.kind {
        Kind::Url | Kind::SecretUrl => ("text", Some("url")),
        Kind::Tel => ("tel", None),
        _ => ("text", None),
    };
    let err_id = format!("{}-err", f.name);
    html! {
        li class=(format!("it fld {class}")) {
            label for=(f.name) { (f.label) }
            @if f.kind == Kind::Secret {
                (secret_input(Some(f.name), f.name, None, placeholder, err))
            } @else {
                input id=(f.name) type=(ty) name=(f.name) value=[(f.kind != Kind::SecretUrl).then_some(value)] inputmode=[mode]
                    placeholder=(placeholder) autocomplete="off" autocapitalize="off" spellcheck="false"
                    aria-invalid=[err.map(|_| "true")] aria-describedby=[err.map(|_| err_id.as_str())];
            }
            @if let Some((saved, Some(host))) = &kept { small { (if *saved { "Saved: " } else { "Entered: " }) (host) } }
            (field_err(f.name, err))
        }
    }
}

/// A radio group drawn as a wrapping segmented control (CSS shows the
/// chosen type's fields; without `:has()` every field shows).
fn chips(name: &str, opts: &[(&str, &str)], cur: &str) -> Markup {
    html! {
        li.it.fld {
            // Two columns for the four filters, three for the six types.
            fieldset class=(format!("chips c{}", if opts.len().is_multiple_of(3) { 3 } else { 2 })) {
                legend { "Type" }
                div {
                    @for (value, label) in opts {
                        label {
                            input.sr type="radio" id=(format!("{name}-{value}")) name=(name) value=(value) checked[*value == cur];
                            span { (label) }
                        }
                    }
                }
            }
        }
    }
}

/// The webhook's header rows: the name shown, the value a password field
/// (never filled; empty keeps the saved value of that name). As many rows
/// as the form had, one more than are filled and at least two; "Add
/// Header" draws the form again with one more.
fn header_rows(input: &Input, kept: &dyn Fn(&str) -> bool, errors: &Errors) -> Markup {
    let rows = input.rows.max(input.headers.len() + 1).clamp(2, MAX_HEADERS);
    html! {
        li.it.fld.dx.d-webhook { span.lb { "Headers" } small { "One header per row: its name and value." } }
        @for i in 0..rows {
            @let name = input.headers.get(i).map_or("", |h| h.0.as_str());
            @let (ne, ve) = (err_of(errors, &format!("wh_hn{i}")), err_of(errors, &format!("wh_hv{i}")));
            li.it.hr.dx.d-webhook {
                input type="text" name=(format!("wh_hn{i}")) value=(name) placeholder="Name" aria-label=(format!("Header {} name", i + 1))
                    autocomplete="off" autocapitalize="off" spellcheck="false" aria-invalid=[ne.map(|_| "true")];
                // After a URL change nothing is kept: "Value".
                (secret_input(None, &format!("wh_hv{i}"), Some(&format!("Header {} value", i + 1)),
                    if !name.is_empty() && kept(name) && ve != Some(forward::AGAIN_URL) { "Leave empty to keep" } else { "Value" }, ve))
                @if let Some(e) = ne.or(ve) { p.ferr { (e) } }
            }
        }
        @if rows < MAX_HEADERS {
            li.dx.d-webhook { button.it.bt type="submit" name="add" value="1" { (kit::icon(Icon::Plus)) span { "Add Header" } } }
        }
    }
}

/// One destination type's rows.
fn dest_rows(d: &Dest, input: &Input, sources: &[&Secrets], errors: &Errors) -> Markup {
    let class = format!("dx d-{}", d.key);
    // Whether an empty field keeps a secret (the rule's or typed before),
    // and the host a kept URL goes to.
    let kept = |name: &str| -> Kept {
        let s = sources.iter().find(|s| s.has(name))?;
        let url = forward::field(name).is_some_and(|f| f.kind == Kind::SecretUrl);
        Some((s.saved, s.get(name).filter(|_| url).map(forward::host).filter(|h| !h.is_empty())))
    };
    let kept_header = |name: &str| sources.iter().any(|s| s.has_header(name));
    html! {
        @for f in d.fields {
            @match f.kind {
                Kind::Check => li class=(format!("it {class}")) {
                    label.tl for=(f.name) {
                        span { (f.label) }
                        input.sr id=(f.name) type="checkbox" role="switch" name=(f.name) value="1" checked[input.value(f.name) == "1"];
                        span.sw aria-hidden="true" {}
                    }
                },
                Kind::Method => li class=(format!("it {class}")) {
                    label for=(f.name) { (f.label) }
                    select id=(f.name) name=(f.name) {
                        @for m in ["POST", "PUT"] { option value=(m) selected[input.value(f.name) == m] { (m) } }
                    }
                },
                Kind::Headers => (header_rows(input, &kept_header, errors)),
                _ => {
                    // After a server change nothing is kept ("Enter it again").
                    @let err = err_of(errors, f.name);
                    (input_row(&class, f, input.value(f.name),
                        kept(f.name).filter(|_| matches!(f.kind, Kind::Secret | Kind::SecretUrl) && err != Some(forward::AGAIN)), err))
                    @if f.name == "nt_token" && kept(f.name).is_some() {
                        li class=(format!("it {class}")) {
                            label.check { input type="checkbox" name="nt_token_clear" value="1" checked[input.clear_token]; " Remove the saved token" }
                        }
                    }
                }
            }
        }
    }
}

/// The rule form (`SMSForwardRuleFormView`), with a new single-use token;
/// `keep` (the secrets typed into this form) is stored under it, and the
/// form says it has a draft (`kd`, its type), so a post whose draft is
/// gone is refused rather than falling back to the rule's secrets. Nothing
/// secret is in the page: `input` is drawn without its secrets, and the
/// kept ones only say so.
fn form(app: &AppState, w: &Draw, input: &Input, keep: Option<Secrets>, note: Note, errors: &Errors, status: u16) -> Response {
    let m = w.m;
    let now = app.auth.now();
    let token = w.once.fresh(&app.auth, w.p.id);
    let keep = keep.filter(Secrets::any);
    let sources: Vec<&Secrets> = keep.iter().chain(w.stored).collect();
    let shown = input.shown();
    let title = if input.id.is_some() { "Edit Rule" } else { "New Rule" };
    let ferr = |name: &str| err_of(errors, name);
    let text_row = |class: &str, name: &str, label: &str, value: &str| {
        let e = ferr(name);
        html! {
            li class=(format!("it fld {class}")) {
                label for=(name) { (label) }
                input id=(name) type="text" name=(name) value=(value) placeholder="Optional" autocomplete="off" autocapitalize="off"
                    spellcheck="false" aria-invalid=[e.map(|_| "true")] aria-describedby=[e.map(|_| format!("{name}-err"))];
                (field_err(name, e))
            }
        }
    };
    let name_err = ferr("name");
    let body = html! {
        @match &note {
            Some((true, msg)) => p.msg.ok role="status" { (msg) },
            Some((false, msg)) => p.msg.err role="alert" { (msg) },
            None => {},
        }
        @if !errors.is_empty() && note.is_none() { (kit::notice(false, "Check the highlighted fields.")) }
        form.rf #rf method="post" action=(m.at("/rules/save")) {
            (render::csrf_field(&app.auth, w.p))
            (render::once_field(&token))
            @if let Some(k) = &keep { input type="hidden" name="kd" value=(k.dest); }
            @if let Some(id) = input.id { input type="hidden" name="id" value=(id); }
            // Enter in a field saves (the first submit button), never tests.
            button.sr type="submit" tabindex="-1" aria-hidden="true" { "Save" }
            (kit::section(Some("Name"), None, html! {
                li.it.fld {
                    input type="text" name="name" value=(shown.name) placeholder="Rule name" aria-label="Rule name" maxlength="100"
                        autocomplete="off" aria-invalid=[name_err.map(|_| "true")] aria-describedby=[name_err.map(|_| "name-err")];
                    (field_err("name", name_err))
                }
            }))
            (kit::section(Some("Filter"), None, html! {
                (chips("ft", &FILTERS, input.filter))
                (text_row("fx f-s", "patterns", "Sender patterns (comma-separated)", &shown.patterns))
                (text_row("fx f-k", "keywords", "Keywords (comma-separated)", &shown.keywords))
            }))
            (kit::section(Some("Destination"), None, html! {
                (chips("dt", &DESTS.iter().map(|d| (d.key, d.label)).collect::<Vec<_>>(), input.dest.key))
                @for d in &DESTS { (dest_rows(d, &shown, &sources, errors)) }
            }))
            (kit::section(None, None, html! {
                li { button.it.bt type="submit" formaction=(m.at("/rules/test")) { (kit::icon(Icon::PaperPlane)) span { "Test Destination" } } }
            }))
            (kit::section(None, None, html! { li { button.it.cv.snd type="submit" { "Save" } } }))
        }
    };
    if let (Some(k), false) = (keep, token.is_empty()) {
        w.drafts.insert(token, w.p.id, Draft::Rule(k), now);
    }
    let pc = PageCtx {
        title,
        tab: Some(m.tab),
        nonce: w.nonce,
        lead: html! { a.nl href=(m.base) { "Cancel" } },
        ..PageCtx::default()
    };
    render::html(status, render::page(&pc, body))
}

/// A page of this mount with a message and a way back (a rule that is
/// gone, a failed read).
fn message(nonce: &str, m: &Mount, title: &str, text: &str, status: u16) -> Response {
    let body = html! {
        (kit::empty_state(Icon::Envelope, title, text))
        p.ctr { a.btn href=(m.base) { "Back to SMS Forwarding" } }
    };
    let pc = PageCtx { title: "SMS Forwarding", tab: Some(m.tab), nonce, lead: kit::back(m.base, "SMS Forwarding"), ..PageCtx::default() };
    render::html(status, render::page(&pc, body))
}

/// The stored rule of `id` in a config read, and its secrets.
fn stored(data: &Value, id: Option<u32>) -> (Option<&Value>, Option<Secrets>) {
    let rule = id.and_then(|id| forward::rule(data, id));
    (rule, rule.map(|r| Secrets::of(&r["destination"])))
}

/// `GET` of New Rule (the blank form: it reads nothing, so it shows with
/// the router busy) or Edit Rule (reads the config: the rule's shown
/// values and which secrets it has).
pub async fn get(ctx: &Ctx, p: Principal, nonce: String, page: Page) -> Response {
    let (m, id) = match page {
        Page::New(m) => {
            let w = Draw { p: &p, nonce: &nonce, once: &ctx.once, drafts: &ctx.drafts, m, stored: None };
            return form(&ctx.app, &w, &Input::blank(), None, None, &Vec::new(), 200);
        }
        Page::Edit(m, id) => (m, id),
    };
    let (once, drafts, n) = (Arc::clone(&ctx.once), Arc::clone(&ctx.drafts), nonce.clone());
    let job = Box::new(move |app: &AppState| {
        gate::read_only(|| {
            let data = match forward::read(app) {
                Ok(d) => d,
                Err(e) => return message(&n, m, "Failed to load", &fmt::truncate(&e, 120), 503),
            };
            let (rule, s) = stored(&data, Some(id));
            let w = Draw { p: &p, nonce: &n, once: &once, drafts: &drafts, m, stored: s.as_ref() };
            match rule {
                Some(r) => form(app, &w, &Input::of_rule(&forward::public_rule(r)), None, None, &Vec::new(), 200),
                None => message(&n, m, "Rule not found", "It may have been deleted.", 404),
            }
        })
    });
    job::run(ctx, Lane::General, &nonce, &m.form_url(Some(id)), job).await
}

const BUSY: &str = "the router is busy. Try again in a few seconds.";

fn prefix(test: bool) -> &'static str {
    if test {
        "Test failed: "
    } else {
        "Failed: "
    }
}

fn expired(test: bool) -> &'static str {
    if test {
        "This form expired, so no test message was sent. Test again."
    } else {
        "This form expired, so the rule was not saved. Save it again."
    }
}

/// The secrets typed into the form (its draft) are gone: nothing is sent,
/// and nothing falls back to the rule's own.
fn gone(test: bool) -> &'static str {
    if test {
        "This form expired, so no test message was sent. Enter the secret fields again, then test."
    } else {
        "This form expired, so the rule was not saved. Enter the secret fields again, then save."
    }
}

fn unknown(test: bool) -> &'static str {
    if test {
        "The test may still be running. Check the destination before testing again."
    } else {
        "The rule may not have been saved. Check the rules before saving again."
    }
}

/// What a post of the rule form needs besides the form.
struct Req {
    ctx: Ctx,
    p: Principal,
    nonce: String,
    m: &'static Mount,
    input: Input,
    /// The token posted (its draft holds the secrets an earlier answer kept).
    token: String,
    test: bool,
    /// The form says it has a draft for the chosen type (`kd`).
    drafted: bool,
}

/// The draft of the token posted: the secrets an earlier answer of the
/// form kept; `Err` if the form had one (`kd`) that is gone.
fn draft_of(drafts: &Drafts, token: &str, owner: SessionId, app: &AppState, drafted: bool) -> Result<Option<Secrets>, ()> {
    match drafts.get(token, owner, app.auth.now()) {
        Some(Draft::Rule(d)) => Ok(Some(d)),
        _ if drafted => Err(()),
        _ => Ok(None),
    }
}

/// The form again with `note` (an Edit Rule's reads the config: its
/// secrets only say whether they are kept): after an expired token, a busy
/// router, "Cancel" on the SMS test question, "Add Header", a replay. A
/// draft that is gone says so.
async fn show(r: Req, note: Note, status: u16) -> Response {
    let (once, drafts, retry, id) = (Arc::clone(&r.ctx.once), Arc::clone(&r.ctx.drafts), r.m.form_url(r.input.id), r.input.id);
    let Req { ctx, p, nonce, m, input, token, test, drafted } = r;
    let n = nonce.clone();
    let draw = move |app: &AppState, data: Value| {
        let (_, stored) = stored(&data, input.id);
        let (draft, note) = match draft_of(&drafts, &token, p.id, app, drafted) {
            Ok(d) => (d, note),
            Err(()) => (None, Some((false, gone(test).to_owned()))),
        };
        let sources: Vec<&Secrets> = draft.iter().chain(stored.iter()).collect();
        let keep = forward::resolve(&input, &sources).secrets;
        let w = Draw { p: &p, nonce: &n, once: &once, drafts: &drafts, m, stored: stored.as_ref() };
        form(app, &w, &input, Some(keep), note, &Vec::new(), status)
    };
    if id.is_none() {
        return draw(&ctx.app, Value::Null);
    }
    let job = Box::new(move |app: &AppState| gate::read_only(|| draw(app, forward::read(app).unwrap_or(Value::Null))));
    job::run(&ctx, Lane::General, &nonce, &retry, job).await
}

/// `POST …/rules/save` and `…/rules/test`: a session (else the login,
/// nothing was changed), the form, origin and CSRF token, the fields (an
/// unknown type or id is 400), then the single-use token: checked first,
/// claimed right before the call for what was posted, given back if
/// nothing is sent. "Add Header" and the SMS question's "Cancel" only draw
/// the form again.
pub async fn post(ctx: Ctx, req: Request, which: Post) -> Response {
    let nonce = CspNonce::of(&req);
    let (m, test) = match which {
        Post::Save(m) => (m, false),
        Post::Test(m) => (m, true),
        Post::Retry(m) => return retry(ctx, req, m).await,
    };
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, parts.uri.path(), None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        let back = Input::parse(&form).map_or_else(|| m.base.to_owned(), |i| m.form_url(i.id));
        return render::form_refused(&nonce, &back);
    }
    let Some(input) = Input::parse(&form) else {
        return render::bad_request(&nonce);
    };
    let token = form.get("once").unwrap_or("").to_owned();
    let digest = forward::digest(test, &form);
    let drafted = form.get("kd") == Some(input.dest.key);
    let mut r = Req { ctx: ctx.clone(), p: p.clone(), nonce: nonce.clone(), m, input, token: token.clone(), test, drafted };
    // "Cancel" on the SMS test question: the form again, nothing sent.
    if test && form.get("back").is_some() {
        return show(r, None, 200).await;
    }
    // "Add Header": the form again with another row, nothing sent.
    if form.get("add").is_some() {
        r.input.rows = r.input.rows.max(r.input.headers.len()) + 1;
        return show(r, None, 200).await;
    }
    match ctx.once.get(&token, p.id, ctx.app.auth.now()) {
        None => return show(r, Some((false, expired(test).to_owned())), 400).await,
        Some(Token::Fresh) => {}
        Some(_) => return replayed(r, digest).await,
    }
    if test && r.input.dest.key == "sms" && form.get("confirm") != Some("1") {
        return ask_sms(r).await;
    }
    match Claim::take(&ctx.once, &ctx.app.auth, &token, p.id, digest) {
        Some(claim) => run(r, claim).await,
        None => replayed(r, digest).await,
    }
}

/// A post of a token another post claimed: nothing is sent. The same post
/// (a double click, a reload of its answer): once that one is done (at
/// most 20 s), what it did; if it sent nothing, this one goes ahead.
/// Another post with it: an expired form.
async fn replayed(r: Req, digest: u64) -> Response {
    let (once, auth, test) = (Arc::clone(&r.ctx.once), Arc::clone(&r.ctx.app.auth), r.test);
    let not_sure = Some((false, unknown(test).to_owned()));
    match settle(&once, &r.token, r.p.id, digest, &auth, if test { SETTLE_SLOW } else { SETTLE }).await {
        Settled::Other => show(r, Some((false, expired(test).to_owned())), 400).await,
        Settled::Sent if test => show(r, Some((true, "Test message sent".to_owned())), 200).await,
        Settled::Sent => session::see_other(&r.m.saved(if r.input.id.is_some() { "rule-updated" } else { "rule-created" })),
        Settled::Failed(status, msg) => show(r, Some((false, msg)), status).await,
        Settled::Fresh => match Claim::take(&once, &auth, &r.token, r.p.id, digest) {
            Some(claim) => run(r, claim).await,
            None => show(r, not_sure, 409).await,
        },
        Settled::Unknown => show(r, not_sure, 409).await,
    }
}

/// A checked post: the destination's wire body and (for a save) the
/// rule's name and filter.
type Checked = (Value, Option<(String, Value)>);

/// The checked parts of a post (or the errors) with the secrets it keeps.
fn check(input: &Input, sources: &[&Secrets], test: bool) -> (Result<Checked, Errors>, Secrets) {
    let res = forward::resolve(input, sources);
    let parts = if test { Ok(None) } else { forward::rule_parts(input).map(Some) };
    let checked = match (res.dest, parts) {
        (Ok(dest), Ok(parts)) => Ok((dest, parts)),
        (Err(mut a), Err(b)) => {
            a.extend(b);
            Err(a)
        }
        (Err(e), _) | (_, Err(e)) => Err(e),
    };
    (checked, res.secrets)
}

/// A test to an SMS destination costs an SMS: the question first (the
/// fields checked; they carry no secret). Its form posts them again with
/// `confirm=1`, or with `back=1` (Cancel) for the form as it was.
async fn ask_sms(r: Req) -> Response {
    let (once, drafts, retry) = (Arc::clone(&r.ctx.once), Arc::clone(&r.ctx.drafts), r.m.form_url(r.input.id));
    let Req { ctx, p, nonce, m, input, token, .. } = r;
    let n = nonce.clone();
    let job = Box::new(move |app: &AppState| {
        gate::read_only(|| {
            // An SMS destination has no secret: no draft is needed.
            let data = forward::read(app).unwrap_or(Value::Null);
            let (_, stored) = stored(&data, input.id);
            let draft = match drafts.get(&token, p.id, app.auth.now()) {
                Some(Draft::Rule(d)) => Some(d),
                _ => None,
            };
            let sources: Vec<&Secrets> = draft.iter().chain(stored.iter()).collect();
            let (checked, keep) = check(&input, &sources, true);
            let w = Draw { p: &p, nonce: &n, once: &once, drafts: &drafts, m, stored: stored.as_ref() };
            let (dest, _) = match checked {
                Ok(c) => c,
                Err(errors) => return form(app, &w, &input, Some(keep), None, &errors, 400),
            };
            let number = dest["forward_number"].as_str().unwrap_or("");
            let id = input.id.map(|id| id.to_string()).unwrap_or_default();
            let line = format!("This sends a real SMS to {number}, which your plan may charge.");
            let hidden = [
                ("once", token.as_str()),
                ("id", id.as_str()),
                ("name", input.name.as_str()),
                ("ft", input.filter),
                ("patterns", input.patterns.as_str()),
                ("keywords", input.keywords.as_str()),
                ("dt", "sms"),
                ("sms_number", input.value("sms_number")),
            ];
            let body = html! {
                div.narrow {
                    h2.q { "Send a test SMS?" }
                    ul.consequences { li { (line) } }
                    form.panel method="post" action=(m.at("/rules/test")) {
                        (render::csrf_field(&app.auth, &p))
                        @for (k, v) in hidden { @if !v.is_empty() { input type="hidden" name=(k) value=(v) autocomplete="off"; } }
                        p.actions {
                            button.btn type="submit" name="confirm" value="1" { "Send Test SMS" }
                            button.btn.btn-quiet type="submit" name="back" value="1" { "Cancel" }
                        }
                    }
                }
            };
            let pc = PageCtx {
                title: "Confirm",
                tab: Some(m.tab),
                nonce: &n,
                lead: kit::back(m.base, "SMS Forwarding"),
                heading: Some(html! { h1 { "Confirm" } }),
                ..PageCtx::default()
            };
            render::html(200, render::page(&pc, body))
        })
    });
    job::run(&ctx, Lane::General, &nonce, &retry, job).await
}

/// Save or test with a claimed token, in one gate task (the test in the
/// slow lane: it waits for the destination up to 15 s): read the config,
/// check and merge, then the call. Saved: the page with "Rule created" or
/// "Rule updated"; tested: the form with "Test message sent"; anything
/// else the form with why ("Failed: …", "Test failed: …"). With the router
/// busy the form again, nothing sent.
async fn run(r: Req, mut claim: Claim) -> Response {
    let lane = if r.test { Lane::Slow } else { Lane::General };
    let (once, drafts) = (Arc::clone(&r.ctx.once), Arc::clone(&r.ctx.drafts));
    let (p, nonce, m, input, token, test, drafted) = (r.p.clone(), r.nonce.clone(), r.m, r.input.clone(), r.token.clone(), r.test, r.drafted);
    let job = Box::new(move |app: &AppState| {
        let draft = draft_of(&drafts, &token, p.id, app, drafted);
        let data = match forward::read(app) {
            Ok(d) => d,
            Err(e) => {
                let w = Draw { p: &p, nonce: &nonce, once: &once, drafts: &drafts, m, stored: None };
                let (draft, note) = match draft {
                    Ok(d) => (d, format!("{}{}", prefix(test), fmt::truncate(&e, 120))),
                    Err(()) => (None, gone(test).to_owned()),
                };
                let keep = forward::resolve(&input, &draft.iter().collect::<Vec<_>>()).secrets;
                return form(app, &w, &input, Some(keep), Some((false, note)), &Vec::new(), 503);
            }
        };
        let (rule, stored) = stored(&data, input.id);
        let w = Draw { p: &p, nonce: &nonce, once: &once, drafts: &drafts, m, stored: stored.as_ref() };
        let Ok(draft) = draft else {
            return form(app, &w, &input, None, Some((false, gone(test).to_owned())), &Vec::new(), 400);
        };
        let sources: Vec<&Secrets> = draft.iter().chain(stored.iter()).collect();
        let (checked, keep) = check(&input, &sources, test);
        let (dest, parts) = match (checked, rule) {
            (Err(errors), _) => return form(app, &w, &input, Some(keep), None, &errors, 400),
            (Ok(_), None) if input.id.is_some() && !test => {
                return form(app, &w, &input, Some(keep), Some((false, "Failed: rule not found".to_owned())), &Vec::new(), 404);
            }
            (Ok(c), _) => c,
        };
        claim.disarm();
        let call = match (parts, input.id) {
            (None, _) => (Method::Post, forward::TEST, json!({"destination": dest})),
            (Some((name, filter)), Some(id)) => {
                let enabled = rule.and_then(|r| r["enabled"].as_bool()).unwrap_or(true);
                (Method::Put, forward::RULES, json!({"id": id, "name": name, "enabled": enabled, "filter": filter, "destination": dest}))
            }
            (Some((name, filter)), None) => {
                (Method::Post, forward::RULES, json!({"name": name, "filter": filter, "destination": dest, "enabled": true}))
            }
        };
        match forward::data(forward::api(app, call.0, call.1, &call.2)) {
            Ok(_) => {
                claim.set(Token::Sent(claim.digest()));
                if test {
                    form(app, &w, &input, Some(keep), Some((true, "Test message sent".to_owned())), &Vec::new(), 200)
                } else {
                    session::see_other(&m.saved(if input.id.is_some() { "rule-updated" } else { "rule-created" }))
                }
            }
            Err((status, e)) => {
                let msg = format!("{}{}", prefix(test), fmt::truncate(&forward::redact(&e), 120));
                claim.set(Token::Failed(claim.digest(), status, msg.clone()));
                form(app, &w, &input, Some(keep), Some((false, msg)), &Vec::new(), status)
            }
        }
    });
    match job::try_run(&r.ctx, lane, &r.nonce, job).await {
        Some(resp) => resp,
        // The claim went with the work: given back, nothing sent.
        None => {
            let msg = format!("{}{BUSY}", prefix(r.test));
            show(r, Some((false, msg)), 503).await
        }
    }
}

// --- The page's actions (tables in `tabs::sms`) ---

/// "Enabled" (the app sends the config's other values with it).
pub fn enabled_body(v: &crate::web::action::Valid, cur: &[Value]) -> Option<Value> {
    Some(config_body(&cur[0], v.get("enabled") == "1", None))
}

/// "Save Settings": the two switches, "Enabled" and the poll interval as
/// they are.
pub fn settings_body(v: &crate::web::action::Valid, cur: &[Value]) -> Option<Value> {
    let c = &cur[0];
    Some(config_body(c, c["enabled"].as_bool().unwrap_or(false), Some((v.get("mark_read") == "1", v.get("delete_after") == "1"))))
}

/// The app's config body; the poll interval is sent back unchanged (the
/// web has no field for it, as the app).
fn config_body(c: &Value, enabled: bool, flags: Option<(bool, bool)>) -> Value {
    let (mark, delete) =
        flags.unwrap_or((c["mark_read_after_forward"].as_bool().unwrap_or(false), c["delete_after_forward"].as_bool().unwrap_or(false)));
    let mut body = json!({"enabled": enabled, "mark_read_after_forward": mark, "delete_after_forward": delete});
    if let Some(poll) = c["poll_interval_secs"].as_u64() {
        body["poll_interval_secs"] = poll.into();
    }
    body
}

pub fn toggle_body(v: &crate::web::action::Valid, _: &[Value]) -> Option<Value> {
    Some(json!({"id": v.int("id"), "enabled": v.get("enabled") == "1"}))
}

pub fn delete_body(v: &crate::web::action::Valid, _: &[Value]) -> Option<Value> {
    Some(json!({"id": v.int("id")}))
}

/// After a rule's switch: back at its row.
pub fn at_rule(v: &crate::web::action::Valid) -> String {
    format!("#r{}", v.int("id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::action::Valid;

    /// The app's bodies: the config with its other values as they are
    /// (the poll interval sent back unchanged), `{"id","enabled"}`, `{"id"}`.
    #[test]
    fn action_bodies_follow_the_app() {
        let cur = [json!({"enabled": false, "poll_interval_secs": 45, "mark_read_after_forward": true, "delete_after_forward": false})];
        assert_eq!(
            enabled_body(&Valid::of(&[("enabled", "1")]), &cur),
            Some(json!({"enabled": true, "poll_interval_secs": 45, "mark_read_after_forward": true, "delete_after_forward": false}))
        );
        assert_eq!(
            settings_body(&Valid::of(&[("mark_read", ""), ("delete_after", "1")]), &cur),
            Some(json!({"enabled": false, "poll_interval_secs": 45, "mark_read_after_forward": false, "delete_after_forward": true}))
        );
        // Without a known interval none is sent (the agent keeps its own).
        let none = enabled_body(&Valid::of(&[("enabled", "0")]), &[json!({})]).unwrap();
        assert_eq!(none, json!({"enabled": false, "mark_read_after_forward": false, "delete_after_forward": false}));
        assert_eq!(toggle_body(&Valid::of(&[("id", "3"), ("enabled", "0")]), &[]), Some(json!({"id": 3, "enabled": false})));
        assert_eq!(delete_body(&Valid::of(&[("id", "3")]), &[]), Some(json!({"id": 3})));
        assert_eq!(at_rule(&Valid::of(&[("id", "3")])), "#r3");
    }

    #[test]
    fn paths() {
        assert!(matches!(Page::of("/ui/sms/forward/rules/new"), Some(Page::New(m)) if m.base == SMS.base));
        assert!(matches!(Page::of("/ui/tools/sms-forward/rules/12"), Some(Page::Edit(m, 12)) if m.base == TOOLS.base));
        for bad in ["/ui/sms/forward/rules/0", "/ui/sms/forward/rules/", "/ui/sms/forward/rules/1/x", "/ui/sms/forwardx/rules/new", "/ui/sms/forward/rules/99999999999"] {
            assert!(Page::of(bad).is_none(), "{bad}");
        }
        assert!(matches!(Post::of("/ui/tools/sms-forward/rules/test"), Some(Post::Test(m)) if m.base == TOOLS.base));
        assert_eq!(form_page("/ui/sms/forward/rules/save"), Some("/ui/sms/forward"));
        assert_eq!(form_page("/ui/sms/forward/rules/new"), None);
    }
}
