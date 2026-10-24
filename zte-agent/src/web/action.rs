//! Actions as data. An `Action` names its POST path, the screen that shows
//! its form, its fields and how they are checked, when it is confirmed
//! first, the API call it makes (body built here from checked values only,
//! never passed through) and where it goes after. One generic POST and one
//! confirmation page serve every action:
//!
//! 1. a session (else the login, "nothing was changed"), the form body,
//!    origin and CSRF token;
//! 2. the fields: a value outside a choice list is 400; a bad number sends
//!    the form back (400) as typed, with a message per field; a value the
//!    form showed as current changes nothing (303 back, no call);
//! 3. a confirmation where one is needed (`…/confirm?<field>=<value>`, a
//!    GET page whose form posts the same fields plus `confirm=1`; or a
//!    screen of its own when the question needs a device read);
//! 4. one gate task: reads, the call, then 303 to `?saved=`, to a result
//!    built from the answer (`?r=<id>`) or to the operation page; a device
//!    error shows the screen again with "Failed: <error>" and the call's
//!    status (a failed read: 503, nothing sent, no operation). An action
//!    with `once` needs the form's single-use token: a repeated post (a
//!    double click, a replay) sends nothing and lands where the first did
//!    (its operation, kept under the token, its line or its `?saved=`).

use std::sync::Arc;

use axum::extract::Request;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::Value;

use super::kit;
use super::ops::{Op, OpSpec};
use super::render::{self, PageCtx};
use super::screen::{self, FormState, Notice, Read, Req, Screen};
use super::job::{OnceJob, Sent};
use super::store::{Claim, Flash, Token, SETTLE, SETTLE_SLOW};
use super::session::{self, query_param, Form};
use super::{job, CspNonce};
use crate::httpd::gate::Lane;
use crate::auth::{Principal, SessionId};
use crate::handlers::AppState;
use crate::httpd::{Ctx, Peer};
use crate::server::{self, Method};

pub enum Kind {
    /// One of these values; anything else is a bad request.
    Choice(&'static [&'static str]),
    /// A whole number in `min..=max`.
    Int(i64, i64),
    /// A checkbox: "1" when sent, else "".
    Check,
    /// An id from a link (a PID): digits, `1..=max`; anything else is a
    /// bad request.
    Id(u32),
}

pub struct Field {
    pub name: &'static str,
    pub kind: Kind,
}

/// The checked values, by field name.
pub struct Valid(Vec<(&'static str, String)>);

impl Valid {
    pub fn get(&self, name: &str) -> &str {
        self.0.iter().find(|(k, _)| *k == name).map_or("", |(_, v)| v.as_str())
    }

    pub fn int(&self, name: &str) -> i64 {
        self.get(name).parse().unwrap_or(0)
    }

    #[cfg(test)]
    pub fn of(pairs: &[(&'static str, &str)]) -> Valid {
        Valid(pairs.iter().map(|(k, v)| (*k, (*v).to_owned())).collect())
    }
}

pub enum Need {
    /// The page exists (a link may lead to it); a POST does not wait for it.
    Never,
    Always,
}

/// A confirmation page for one value of a choice field.
pub struct Confirm {
    pub when: (&'static str, &'static str),
    pub need: Need,
    pub title: &'static str,
    pub lines: &'static [&'static str],
    /// The lines for a remote browser, if they differ (the change may cut
    /// the connection this page is reached over).
    pub remote: Option<&'static [&'static str]>,
    pub label: &'static str,
}

#[derive(Copy, Clone)]
pub enum After {
    /// 303 to the screen with this `?saved=` notice.
    Saved(&'static str),
    /// 303 to the screen with this line (`Some(true)`: it reports success,
    /// shown green as the app's; `Some(false)` red; `None` neutral), built
    /// from the checked values and the answer's `data` (`Null` when nothing
    /// was sent), kept for the session (`?r=<id>`).
    Flash(fn(&Valid, &Value) -> (Option<bool>, String)),
    /// The same, at the screen with this query string first ("?…&",
    /// from the checked values: the speed test's server).
    FlashAt(fn(&Valid) -> String, fn(&Valid, &Value) -> (Option<bool>, String)),
    /// Start this operation (with its target, from a fixed list, or "");
    /// 303 to its progress page.
    Op(fn(&Valid) -> (&'static OpSpec, &'static str)),
    /// Nothing is sent: a change to this session's own state in the web
    /// UI (the USB-C card's dismissal), then 303 to the screen.
    Session(fn(&Ctx, SessionId)),
    /// 303 back to the screen without a notice (a switch shows its new
    /// state, as the app's), at this `#anchor` ("" for none).
    Back(fn(&Valid) -> String),
}

/// A failed call that is tried once more after a pause, and where both
/// failures lead (the app's airplane mode off: two tries 3 s apart, then
/// "Reboot Required").
pub struct Again {
    /// For this field value only.
    pub when: (&'static str, &'static str),
    pub pause: std::time::Duration,
    /// 303 here with "Failed: <error>" for the session (`?r=<id>`).
    pub then: &'static str,
}

pub struct Action {
    pub path: &'static str,
    /// The screen of the form: shown again on an error, the Cancel target.
    pub screen: &'static Screen,
    pub fields: &'static [Field],
    pub confirm: &'static [Confirm],
    /// Always confirmed first on this page (a screen that reads what its
    /// question needs, such as a process name): a POST without
    /// `confirm=1` goes there.
    pub confirm_at: Option<fn(&Valid) -> String>,
    /// `(field, shown)`: the form posts the value it showed as current in
    /// the hidden field `shown`; the same value in `field` changes nothing
    /// (the app's Apply is disabled then). Only compared, never used.
    pub unchanged: Option<(&'static str, &'static str)>,
    /// Current state the body is merged with; a failed read sends nothing.
    pub reads: &'static [Read],
    pub call: (Method, &'static str),
    /// The request body; `None`: nothing to send (the action is done).
    pub body: fn(&Valid, &[Value]) -> Option<Value>,
    pub after: After,
    pub again: Option<Again>,
    /// Posts need the form's single-use token (`once`): sent once, however
    /// often posted (mobile data, airplane mode).
    pub once: bool,
}

enum Invalid {
    Bad,
    Fields(FormState),
}

fn validate(a: &Action, form: &Form) -> Result<Valid, Invalid> {
    let mut valid = Vec::new();
    let mut fs = FormState::default();
    for f in a.fields {
        let raw = form.get(f.name);
        match f.kind {
            Kind::Choice(set) => match set.iter().find(|v| Some(**v) == raw) {
                Some(v) => valid.push((f.name, (*v).to_owned())),
                None => return Err(Invalid::Bad),
            },
            Kind::Check => valid.push((f.name, if raw.is_some() { "1" } else { "" }.to_owned())),
            Kind::Id(max) => {
                let raw = raw.unwrap_or("");
                let n = raw.parse::<u32>().ok().filter(|n| raw.bytes().all(|b| b.is_ascii_digit()) && (1..=max).contains(n));
                match n {
                    Some(n) => valid.push((f.name, n.to_string())),
                    None => return Err(Invalid::Bad),
                }
            }
            Kind::Int(min, max) => {
                let typed = raw.unwrap_or("").trim();
                match typed.parse::<i64>().ok().filter(|n| (min..=max).contains(n)) {
                    Some(n) => valid.push((f.name, n.to_string())),
                    None => fs.errors.push((f.name, format!("Enter a whole number from {min} to {max}."))),
                }
                fs.values.push((f.name, raw.unwrap_or("").chars().take(8).collect()));
            }
        }
    }
    if fs.errors.is_empty() {
        Ok(Valid(valid))
    } else {
        // Checkboxes come back as sent.
        fs.values.extend(valid.into_iter().filter(|(k, _)| a.fields.iter().any(|f| f.name == *k && matches!(f.kind, Kind::Check))));
        Err(Invalid::Fields(fs))
    }
}

/// The confirmation case of these values, if the action has one.
fn case(a: &Action, valid: &Valid) -> Option<&'static Confirm> {
    a.confirm.iter().find(|c| valid.get(c.when.0) == c.when.1)
}

fn confirm_url(a: &Action, c: &Confirm) -> String {
    format!("{}/confirm?{}={}", a.path, c.when.0, c.when.1)
}

/// Why an action did not (surely) happen.
enum Fail {
    /// A state read failed: nothing was sent.
    Read(String),
    /// The call answered with this status and error.
    Call(u16, String),
}

/// A handler's `(status, envelope)` as a result: its `data`.
fn applied((status, mut v): (u16, Value)) -> Result<Value, Fail> {
    if status < 400 {
        Ok(v["data"].take())
    } else {
        Err(Fail::Call(status, v["error"].as_str().unwrap_or("error").to_owned()))
    }
}

/// A failed write whose outcome is unknown: the AT command got no "OK" in
/// time (500) or ubus timed out. The modem may still make the change, so
/// an operation page follows what it actually does.
pub fn unsure(status: u16, e: &str) -> bool {
    status == 500 || e.contains("timed out")
}

/// Deletes run one at a time, reads and call together: a double post
/// reads what the first one deleted.
static DELETING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The reads, then the call (blocking): the answer's `data`. A failed call
/// of a value `Action::again` names is made once more after its pause. A
/// single-use token's claim is disarmed right before the call.
fn call(app: &AppState, a: &Action, valid: &Valid, claim: Option<&mut Claim>) -> Result<Value, Fail> {
    let _one = (a.call.0 == Method::Delete).then(|| DELETING.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    let mut cur = Vec::with_capacity(a.reads.len());
    for r in a.reads {
        cur.push(r.run(app, None).map_err(Fail::Read)?);
    }
    let Some(body) = (a.body)(valid, &cur) else { return Ok(Value::Null) };
    if let Some(c) = claim {
        c.disarm();
    }
    let send = || {
        // A test's device answers instead (`pages::device`), when it has one.
        #[cfg(test)]
        if let Some(r) = super::pages::device::call(app, a.call.1, &body) {
            return r.map_err(|(status, e)| Fail::Call(status, e));
        }
        applied(server::route(&a.call.0, a.call.1, app, body.to_string().as_bytes()))
    };
    match (send(), &a.again) {
        (Err(Fail::Call(..)), Some(again)) if valid.get(again.when.0) == again.when.1 => {
            std::thread::sleep(again.pause);
            send()
        }
        (r, _) => r,
    }
}

/// `POST <action>`.
pub async fn post(ctx: Ctx, req: Request, a: &'static Action) -> Response {
    let r = Req { query: None, ..Req::of(&ctx, &req) };
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&parts.method, parts.uri.path(), None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &r.nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&r.nonce, a.screen.path);
    }
    let nonce = r.nonce.clone();
    let valid = match validate(a, &form) {
        Ok(v) => v,
        Err(Invalid::Bad) => return render::bad_request(&nonce),
        Err(Invalid::Fields(fs)) => {
            let job = Box::new(move |app: &AppState| screen::render(app, a.screen, &p, &r, None, Some(&fs), 400));
            return job::run(&ctx, a.screen.lane(), &nonce, a.screen.path, job).await;
        }
    };
    if a.unchanged.is_some_and(|(field, shown)| form.get(shown) == Some(valid.get(field))) {
        return session::see_other(a.screen.path);
    }
    if let Some(at) = a.confirm_at.filter(|_| form.get("confirm") != Some("1")) {
        return session::see_other(&at(&valid));
    }
    if let Some(c) = case(a, &valid) {
        if matches!(c.need, Need::Always) && form.get("confirm") != Some("1") {
            return session::see_other(&confirm_url(a, c));
        }
    }
    // An operation's id first: without randomness nothing is changed.
    let op = match a.after {
        After::Op(spec) => match ctx.app.auth.random_hex(8) {
            Ok(id) => Some((id, spec(&valid))),
            Err(_) => {
                let msg = "The router could not start this change right now. Nothing was changed.";
                return render::error(503, "Try again", msg, &nonce);
            }
        },
        After::Session(change) => {
            change(&ctx, p.id);
            return session::see_other(a.screen.path);
        }
        After::Saved(_) | After::Flash(_) | After::FlashAt(..) | After::Back(_) => None,
    };
    let (ops, flash) = (Arc::clone(&ctx.ops), Arc::clone(&ctx.flash));
    let lane = server::lane(a.call.0, a.call.1);
    if a.once {
        return once(&ctx, p, r, a, valid, form.get("once").unwrap_or("").to_owned(), op.map(|(_, spec)| spec)).await;
    }
    let job = Box::new(move |app: &AppState| {
        let done = call(app, a, &valid, None);
        // Failed twice (`Action::again`): its fallback page says so.
        if let (Err(Fail::Call(_, e)), Some(again)) = (&done, &a.again) {
            if valid.get(again.when.0) == again.when.1 {
                let line = (Some(false), format!("Failed: {}", super::fmt::truncate(e, 120)));
                return flashed(app, &flash, p.id, again.then, "?", line, None);
            }
        }
        // A failed read sent nothing: no operation to follow.
        let follow = match &done {
            Ok(_) => true,
            Err(Fail::Call(status, e)) => unsure(*status, e),
            Err(Fail::Read(_)) => false,
        };
        if let Some((id, (spec, arg))) = op.filter(|_| follow) {
            let now = app.auth.now();
            let location = format!("/ui/op/{id}");
            ops.insert(id, p.id, Op { spec, started: now, arg }, now);
            return session::see_other(&location);
        }
        let failed = |status, e| screen::render(app, a.screen, &p, &r, Some(Notice::Failed(e)), None, status);
        match (a.after, done) {
            (_, Err(Fail::Read(e))) => failed(503, e),
            (_, Err(Fail::Call(status, e))) => failed(status, e),
            (After::Saved(key), Ok(_)) => session::see_other(&format!("{}?saved={key}", a.screen.path)),
            (After::Flash(text), Ok(data)) => flashed(app, &flash, p.id, a.screen.path, "?", text(&valid, &data), None),
            (After::FlashAt(at, text), Ok(data)) => flashed(app, &flash, p.id, a.screen.path, &at(&valid), text(&valid, &data), None),
            (After::Back(anchor), Ok(_)) => session::see_other(&format!("{}{}", a.screen.path, anchor(&valid))),
            (After::Op(_) | After::Session(_), Ok(_)) => session::see_other(a.screen.path),
        }
    });
    job::run(&ctx, lane, &nonce, a.screen.path, job).await
}

/// 303 to `path` and `query` (ending in "?" or "&") with `line` for this
/// session (`r=<id>`): under the post's single-use `token` when it has one
/// (a replay finds it), else a random id.
fn flashed(app: &AppState, flash: &Flash, owner: SessionId, path: &str, query: &str, line: (Option<bool>, String), token: Option<&str>) -> Response {
    let id = match token.filter(|t| super::ops::is_op_id(t)) {
        Some(t) => t.to_owned(),
        // Without randomness the change still happened; only its message is lost.
        None => match app.auth.random_hex(8) {
            Ok(id) => id,
            Err(_) => return session::see_other(path),
        },
    };
    flash.insert(id.clone(), owner, line, app.auth.now());
    session::see_other(&format!("{path}{query}r={id}"))
}

/// Where a sent `once` action leads: its `?saved=` notice, else the screen.
fn done_url(a: &Action) -> String {
    match a.after {
        After::Saved(key) => format!("{}?saved={key}", a.screen.path),
        _ => a.screen.path.to_owned(),
    }
}

/// `post` of an action with `once`: the work with the token's claim; its
/// operation is kept under the token (a replay lands on it), as is the
/// fallback page's line (`Action::again`). A failed read sends nothing and
/// gives the token back.
async fn once(ctx: &Ctx, p: Principal, r: Req, a: &'static Action, valid: Valid, token: String, op: Option<(&'static OpSpec, &'static str)>) -> Response {
    let nonce = r.nonce.clone();
    let lane = server::lane(a.call.0, a.call.1);
    let fields: Vec<String> = valid.0.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let digest = job::digest(&[a.path, &fields.join("&")]);
    let (ops, flash, owner, token_w) = (Arc::clone(&ctx.ops), Arc::clone(&ctx.flash), p.id, token.clone());
    let work: OnceJob = Box::new(move |app: &AppState, claim| {
        let done = call(app, a, &valid, Some(&mut *claim));
        if claim.sent() {
            claim.set(match &done {
                Err(Fail::Call(status, e)) if !unsure(*status, e) => Token::Failed(claim.digest(), *status, e.clone()),
                _ => Token::Sent(claim.digest()),
            });
        }
        if let (Err(Fail::Call(_, e)), Some(again)) = (&done, &a.again) {
            if valid.get(again.when.0) == again.when.1 {
                let line = (Some(false), format!("Failed: {}", super::fmt::truncate(e, 120)));
                return flashed(app, &flash, owner, again.then, "?", line, Some(&token_w));
            }
        }
        let follow = match &done {
            Ok(_) => true,
            Err(Fail::Call(status, e)) => unsure(*status, e),
            Err(Fail::Read(_)) => false,
        };
        if let Some((spec, arg)) = op.filter(|_| follow && super::ops::is_op_id(&token_w)) {
            let now = app.auth.now();
            if ops.get(&token_w, owner, now).is_none() {
                ops.insert(token_w.clone(), owner, Op { spec, started: now, arg }, now);
            }
            return session::see_other(&format!("/ui/op/{token_w}"));
        }
        let failed = |status, e| screen::render(app, a.screen, &p, &r, Some(Notice::Failed(e)), None, status);
        match done {
            Err(Fail::Read(e)) => failed(503, e),
            Err(Fail::Call(status, e)) => failed(status, e),
            Ok(_) => session::see_other(&done_url(a)),
        }
    });
    let limit = if lane == Lane::Slow { SETTLE_SLOW } else { SETTLE };
    let now = || ctx.app.auth.now();
    let note = |line: (Option<bool>, String)| flashed(&ctx.app, &ctx.flash, owner, a.screen.path, "?", line, None);
    match job::once(ctx, owner, &nonce, &token, digest, lane, limit, work).await {
        Sent::Ran(resp) => resp,
        // A replay: where the first post went.
        Sent::Done(_) | Sent::Unknown if ctx.ops.get(&token, owner, now()).is_some() => session::see_other(&format!("/ui/op/{token}")),
        Sent::Done(_) | Sent::Unknown if ctx.flash.get(&token, owner, now()).is_some() => {
            let then = a.again.as_ref().map_or(a.screen.path, |g| g.then);
            session::see_other(&format!("{then}?r={token}"))
        }
        Sent::Done(Ok(())) => session::see_other(&done_url(a)),
        Sent::Done(Err((_, e))) => note((Some(false), format!("Failed: {}", super::fmt::truncate(&e, 120)))),
        Sent::Expired => note((Some(false), "This page expired, so nothing was sent. Try again.".to_owned())),
        Sent::Unknown => note((None, "The router may still be working on it. Check before trying again.".to_owned())),
        Sent::Busy => note((Some(false), "Failed: the router is busy. Try again in a few seconds.".to_owned())),
    }
}

/// A confirmation in a page: the question (it wraps), what will happen,
/// then a POST form with `hidden` plus `confirm=1` and the button `label`
/// (none: nothing to do, only the way back), and Cancel to `cancel`.
pub struct Ask<'a> {
    pub title: &'a str,
    pub lines: &'a [&'a str],
    pub action: &'a str,
    pub hidden: &'a [(&'a str, &'a str)],
    pub label: Option<&'a str>,
    /// A red button (destructive); else the plain one.
    pub danger: bool,
    pub cancel: &'a str,
}

pub fn ask(csrf: Markup, q: &Ask) -> Markup {
    html! {
        div.narrow {
            h2.q { (q.title) }
            @if !q.lines.is_empty() {
                ul.consequences {
                    @for line in q.lines { li { (line) } }
                }
            }
            @match q.label {
                Some(label) => form.panel method="post" action=(q.action) {
                    (csrf)
                    // A single-use token is never restored into an old copy.
                    @for (k, v) in q.hidden { input type="hidden" name=(k) value=(v) autocomplete=[(*k == "once").then_some("off")]; }
                    input type="hidden" name="confirm" value="1";
                    p.actions {
                        button.btn.btn-danger[q.danger] type="submit" { (label) }
                        a.btn.btn-quiet href=(q.cancel) { "Cancel" }
                    }
                },
                None => p.actions { a.btn.btn-quiet href=(q.cancel) { "Cancel" } },
            }
        }
    }
}

/// `GET <action>/confirm?<field>=<value>`: the question (in the page, where
/// it wraps), what will happen, then a POST form with the same field plus
/// `confirm=1`, and Cancel. The nav bar leads back to the form's screen.
pub fn confirm_page(ctx: &Ctx, p: &Principal, req: &Request, a: &'static Action) -> Response {
    let nonce = CspNonce::of(req);
    let query = req.uri().query();
    let Some(c) = a.confirm.iter().find(|c| query_param(query, c.when.0).as_deref() == Some(c.when.1)) else {
        return render::bad_request(&nonce);
    };
    let lines = c.remote.filter(|_| session::is_remote(req.extensions().get::<Peer>())).unwrap_or(c.lines);
    // An action with `once`: a fresh single-use token for this question.
    let token = if a.once { ctx.once.fresh(&ctx.app.auth, p.id) } else { String::new() };
    let with_token = [(c.when.0, c.when.1), ("once", token.as_str())];
    let q = Ask {
        title: c.title,
        lines,
        action: a.path,
        hidden: if a.once { &with_token } else { &with_token[..1] },
        label: Some(c.label),
        danger: !lines.is_empty(),
        cancel: a.screen.path,
    };
    let body = ask(render::csrf_field(&ctx.app.auth, p), &q);
    let pc = PageCtx {
        title: c.title,
        tab: Some(a.screen.tab),
        nonce: &nonce,
        lead: kit::back(a.screen.path, a.screen.title),
        heading: Some(html! { h1 { "Confirm" } }),
        ..PageCtx::default()
    };
    render::html(200, render::page(&pc, body))
}
