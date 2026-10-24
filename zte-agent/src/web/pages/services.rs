//! Router › SIM Services (iOS `STKMenuView`): USSD and the SIM Toolkit,
//! through the agent's own routes only (`/api/ussd/*`, `/api/stk/*`; the
//! agent's toolkit worker owns the RIL socket, and nothing here looks
//! past it).
//!
//! - **USSD**: "USSD code (e.g. *100#)" and Send; the answer ("Response",
//!   the app's `response`, else `raw_response`) with "Reply" and "End
//!   Session" while the network keeps the session open. Every outcome is
//!   kept for the session in RAM (`store::UssdLog`, 10 minutes) under the
//!   post's single-use token and shown by `?u=<token>`, so a double click
//!   sends once and lands on the same answer (a failed send keeps the
//!   previous answer, as the app). The code may have digits, `*`, `#` and
//!   `+` only (spaces are dropped; the agent drops anything else); the SIM
//!   codes that carry a PIN or PUK (`**04*`, `**042*`, `**05*`, `**052*`)
//!   are refused, never sent over the air or shown again: SIM Card does
//!   that. Answers and toolkit texts are shown as they are, as the app's
//!   (balances, vouchers; the page is `no-store` and nothing is logged).
//! - **SIM Toolkit**: the page opens the toolkit session (`GET
//!   /api/stk/menu`, the app's load when the screen opens) on a plain load
//!   only: not on a USSD answer or a result line (`?u=`, `?r=`). After a
//!   failed action (`?held=1`) it shows the app's failure state: no menu,
//!   "Reload the SIM menu to reconnect." and "Reload SIM Menu", no stream,
//!   until reloaded. Otherwise it follows the `stk` topic (the worker's
//!   state, read only when the worker changes it): the title, reason and text, the menu items,
//!   Yes / No, Continue, Back, End SIM Session and Reload SIM Menu. Those
//!   are buttons of one form outside the fragments (`#stkf`: CSRF token,
//!   single-use token), each naming its command (`command_id`) and item,
//!   so a menu that arrives live works without a reload and an old page
//!   is refused by the agent ("stale or missing command_id"). A text the
//!   SIM asks for is a form outside the fragments, drawn for the command
//!   the page was loaded with (CSS hides it once another command shows);
//!   one that arrives live offers "Enter Response", which reloads. A
//!   stream only reads: a reconnect never repeats a selection. While the
//!   SIM works (`busy`), the buttons are dimmed and, without JavaScript,
//!   the page reloads every 3 s.

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
use crate::web::ops::is_op_id;
use crate::web::render::{self, once_field, Fragment, Views};
use crate::web::screen::{self, FormState, PageData, Req};
use crate::web::session::{self, query_param, Form};
use crate::web::store::{Flash, Token, Ussd, UssdLog, SETTLE_SLOW};
use crate::web::tabs::router::SIM_SERVICES;
use crate::web::{fmt, CspNonce};

pub const PATH: &str = "/ui/router/sim-services";
pub const USSD_SEND: &str = "/ui/router/sim-services/ussd/send";
pub const USSD_REPLY: &str = "/ui/router/sim-services/ussd/reply";
pub const USSD_END: &str = "/ui/router/sim-services/ussd/cancel";
pub const STK_SELECT: &str = "/ui/router/sim-services/stk/select";
pub const STK_RESPOND: &str = "/ui/router/sim-services/stk/respond";
pub const STK_BACK: &str = "/ui/router/sim-services/stk/back";
pub const STK_CANCEL: &str = "/ui/router/sim-services/stk/cancel";

/// The toolkit buttons' form (outside the fragments).
const FORM: &str = "stkf";
/// The longest USSD code or reply, and SIM response, accepted.
const CODE_MAX: usize = 64;
const TEXT_MAX: usize = 255;
/// The most of an answer kept.
const ANSWER_MAX: usize = 4096;

// --- The toolkit's state (`GET /api/stk/session`) ---

/// What the page shows of the worker's snapshot.
struct Stk<'a> {
    /// `command_id`: the worker's generation (0: none).
    id: u64,
    title: &'a str,
    kind: &'a str,
    reason: &'a str,
    text: &'a str,
    items: Vec<(u64, &'a str)>,
    busy: bool,
    root: bool,
    yes_no: bool,
    hidden: bool,
    digits: bool,
    min: u64,
    max: u64,
}

impl Stk<'_> {
    /// A text (or a single key) the SIM asks for: a form outside the fragments.
    fn typed(&self) -> bool {
        self.kind == "input" && !self.yes_no
    }
}

/// The snapshot; `Err` with the read's error when the last read failed
/// (no session: the worker's last error, which may be empty).
fn command(v: &Views) -> Result<Stk<'_>, &str> {
    let slot = v.slot(Topic::Stk);
    if let Some(e) = slot.and_then(|s| s.error.as_deref()) {
        return Err(e);
    }
    let d = v.value(Topic::Stk).ok_or("")?;
    let s = |k: &str| d[k].as_str().unwrap_or("");
    let input = &d["input"];
    let items = d["items"]
        .as_array()
        .map(|a| a.iter().filter_map(|i| Some((i["id"].as_u64()?, i["label"].as_str()?))).collect())
        .unwrap_or_default();
    Ok(Stk {
        id: s("command_id").parse().unwrap_or(0),
        title: s("title"),
        kind: d["type"].as_str().unwrap_or("status"),
        reason: s("reason"),
        text: s("text"),
        items,
        busy: d["busy"].as_bool().unwrap_or(false),
        root: d["root_menu"].as_bool().unwrap_or(true),
        yes_no: input["yes_no"].as_bool().unwrap_or(false),
        hidden: input["hidden"].as_bool().unwrap_or(false),
        digits: input["digits_only"].as_bool().unwrap_or(false),
        min: input["min"].as_u64().unwrap_or(0),
        max: input["max"].as_u64().unwrap_or(0),
    })
}

/// Before the topic is read: open the toolkit session (`GET
/// /api/stk/menu`) on a plain load of the page (`opens`).
pub fn open(app: &AppState, query: Option<&str>) {
    if opens(query) {
        // Its answer is the topic's next read; a failure is the topic's error.
        let _ = super::api_get(app, "/api/stk/menu");
    }
}

/// Whether a page load opens the toolkit session: the app's opening of the
/// screen and "Reload SIM Menu" (both the plain page); not a USSD answer,
/// a result line or the failure state.
fn opens(query: Option<&str>) -> bool {
    ["held", "u", "r"].iter().all(|k| query_param(query, k).is_none())
}

/// The failure state (`?held=1`).
fn held(v: &Views) -> bool {
    v.arg & 1 == 1
}

/// `Screen::arg`: the command the page was loaded with (its text input is
/// in the page), shifted left, plus 1 in the failure state.
pub fn arg(v: &Views, query: Option<&str>) -> u64 {
    command(v).map_or(0, |c| c.id) << 1 | u64::from(query_param(query, "held").as_deref() == Some("1"))
}

/// `Screen::live_while`: no stream in the failure state (the app stops
/// polling until "Reload SIM Menu").
pub fn live(v: &Views) -> bool {
    !held(v)
}

/// Without JavaScript, reload every 3 s while the SIM works (not in the
/// failure state), keeping the answer, the result line and the page's
/// unused token (`t`).
pub fn refresh(pd: &PageData) -> Option<String> {
    (!held(pd.views) && command(pd.views).is_ok_and(|c| c.busy)).then(|| {
        let mut q: Vec<String> = ["u", "r"].iter().filter_map(|k| pd.param(k).filter(|v| is_op_id(v)).map(|v| format!("{k}={v}"))).collect();
        let token = pd.once();
        if !token.is_empty() {
            q.push(format!("t={token}"));
        }
        if q.is_empty() {
            PATH.to_owned()
        } else {
            format!("{PATH}?{}", q.join("&"))
        }
    })
}

/// The app's toolkit section after a failed action (its `failSTK`): the
/// menu cleared, only the way to reconnect.
fn failure() -> Markup {
    html! {
        section.group #stk {
            h2 { "SIM Toolkit" }
            ul.list { li.it { span.muted { "Reload the SIM menu to reconnect." } } }
        }
    }
}

const T: &[Topic] = &[Topic::Stk];

pub const FRAGMENTS: [Fragment; 2] = [Fragment { id: "stk", topics: T, render: toolkit }, Fragment { id: "stk-nav", topics: T, render: nav }];

/// A button of `#stkf` naming its command (and item or answer).
fn button(action: &str, value: String, busy: bool, label: &str, class: &str) -> Markup {
    html! {
        button class=(class) type="submit" form=(FORM) formaction=(action) name="c" value=(value) disabled[busy] { (label) }
    }
}

/// `#stk`: the app's toolkit section (class `own` while the command the
/// page was loaded with asks for a text: its form follows).
fn toolkit(v: &Views) -> Markup {
    if held(v) {
        return failure();
    }
    let c = match command(v) {
        Ok(c) => c,
        Err(e) => {
            return html! {
                section.group #stk {
                    h2 { "SIM Toolkit" }
                    ul.list {
                        @if !e.is_empty() { li.it.danger { span.tx { (fmt::truncate(e, 120)) } } }
                        li.it { span.muted { "Reload the SIM menu to reconnect." } }
                    }
                }
            };
        }
    };
    let own = c.typed() && c.id == v.arg >> 1 && !c.busy;
    let title = if c.title.is_empty() { "SIM Toolkit" } else { c.title };
    let rows = !c.reason.is_empty() || !c.text.is_empty() || !c.items.is_empty() || matches!(c.kind, "input" | "display");
    html! {
        section.group #stk .own[own] {
            h2 { (title) }
            @if rows {
                ul.list {
                    @if !c.reason.is_empty() { li.it { span.muted { (c.reason) } } }
                    @if !c.text.is_empty() { li.it { span.tx { (c.text) } } }
                    @for (id, label) in &c.items {
                        li { (button(STK_SELECT, format!("{}.{id}", c.id), c.busy, label, "it bt")) }
                    }
                    @if c.kind == "input" && c.yes_no {
                        li.it.yn {
                            (button(STK_RESPOND, format!("{}.y", c.id), c.busy, "Yes", "lk"))
                            (button(STK_RESPOND, format!("{}.n", c.id), c.busy, "No", "lk"))
                        }
                    } @else if c.typed() && c.id != v.arg >> 1 {
                        li { a.it.bt href="" { "Enter Response" } }
                    } @else if c.kind == "display" {
                        li { (button(STK_RESPOND, c.id.to_string(), c.busy, "Continue", "it bt")) }
                    }
                }
            }
        }
    }
}

/// `#stk-nav`: Back and End SIM Session below a submenu, Reload SIM Menu
/// without a menu; the app's footer.
fn nav(v: &Views) -> Markup {
    let c = command(v).ok().filter(|_| !held(v));
    let back = c.as_ref().filter(|c| !c.root && c.kind != "status");
    let reload = held(v) || c.as_ref().is_none_or(|c| c.items.is_empty() && c.kind == "status");
    let busy = c.as_ref().is_some_and(|c| c.busy);
    html! {
        section.group #stk-nav {
            @if back.is_some() || reload {
                ul.list {
                    @if let Some(c) = back {
                        li { (button(STK_BACK, c.id.to_string(), c.busy, "Back", "it bt")) }
                        li { (button(STK_CANCEL, c.id.to_string(), c.busy, "End SIM Session", "it bt danger")) }
                    }
                    // The app's section is disabled while the SIM works.
                    @if reload && busy { li { span.it.bt.off aria-disabled="true" { "Reload SIM Menu" } } }
                    @else if reload { li { a.it.bt href=(PATH) { "Reload SIM Menu" } } }
                }
            }
            p.foot { "Carrier-provided services from your SIM card" }
        }
    }
}

/// The text the SIM asks for, for the command the page was loaded with
/// (`#stk` without `own` hides it): the app's field, "Enter min–max
/// characters" and "Send Response". A secret one is never filled (and is
/// a one-time code to the browser: never saved as a password).
fn answer_form(pd: &PageData, c: &Stk, token: &str) -> Markup {
    let kind = if c.hidden { "password" } else { "text" };
    html! {
        form.rf.stkin method="post" action=(STK_RESPOND) {
            (pd.csrf())
            (once_field(token))
            input type="hidden" name="c" value=(c.id);
            (kit::section(None, None, html! {
                li.it.fld {
                    label.sr for="stkr" { "SIM response" }
                    @if c.hidden {
                        input #stkr type=(kind) name="text" placeholder="SIM response" maxlength=(c.max.min(TEXT_MAX as u64))
                            inputmode=[c.digits.then_some("tel")] autocomplete="one-time-code" "data-1p-ignore" data-lpignore="true" data-bwignore;
                    } @else {
                        input #stkr type=(kind) name="text" placeholder="SIM response" maxlength=(c.max.min(TEXT_MAX as u64))
                            inputmode=[c.digits.then_some("tel")] autocomplete="off" autocapitalize="off" autocorrect="off" spellcheck="false";
                    }
                    small { "Enter " (c.min) "\u{2013}" (c.max) " characters" }
                }
                li { button.it.cv.snd type="submit" { "Send Response" } }
            }))
        }
    }
}

/// The USSD sections: the code, then the answer with Reply and End Session
/// while its session is open.
fn ussd(pd: &PageData, u: Option<&Ussd>, token: &str) -> Markup {
    let typed = pd.form.and_then(|f| f.value("code")).map(str::to_owned);
    let code = typed.or_else(|| u.map(|u| u.code.clone())).unwrap_or_default();
    let err = pd.form.and_then(|f| f.error("code"));
    let id = pd.param("u").filter(|v| is_op_id(v)).unwrap_or_default();
    html! {
        @if let Some(e) = err { p.msg.err role="alert" { (e) } }
        @if let Some((ok, text)) = u.and_then(|u| u.line.as_ref()) {
            @if *ok { p.msg.ok role="status" { (text) } } @else { p.msg.err role="alert" { (text) } }
        }
        form.rf method="post" action=(USSD_SEND) {
            (pd.csrf())
            (once_field(token))
            // The answer shown: kept if this send fails (the app's).
            @if !id.is_empty() && u.is_some_and(|u| u.answer.is_some()) { input type="hidden" name="u" value=(id); }
            (kit::section(Some("USSD"), Some(html! { p.foot { "Send carrier service codes to check balance, data plans, etc." } }), html! {
                li.it.uf {
                    label.sr for="ussd" { "USSD code" }
                    input #ussd type="text" name="code" value=(code) placeholder="USSD code (e.g. *100#)" inputmode="tel" maxlength=(CODE_MAX)
                        autocomplete="off" autocorrect="off" spellcheck="false" aria-invalid=[err.map(|_| "true")];
                    button.send type="submit" aria-label="Send" { (kit::icon(Icon::PaperPlane)) }
                }
            }))
        }
        @if let Some((answer, active)) = u.and_then(|u| u.answer.as_ref()) {
            (kit::section(Some("Response"), None, html! {
                li.it { span.tx { (answer) } }
                @if *active {
                    li.it {
                        form.rf.uf method="post" action=(USSD_REPLY) {
                            (pd.csrf())
                            (once_field(token))
                                            input type="hidden" name="u" value=(id);
                            input type="hidden" name="code" value=(code);
                            label.sr for="ussdr" { "Reply" }
                            input #ussdr type="text" name="reply" placeholder="Reply" inputmode="tel" maxlength=(CODE_MAX) autocomplete="off";
                            button.send type="submit" aria-label="Reply" { (kit::icon(Icon::ArrowUp)) }
                        }
                    }
                    li {
                        form.sf method="post" action=(USSD_END) {
                            (pd.csrf())
                            (once_field(token))
                                            input type="hidden" name="u" value=(id);
                            input type="hidden" name="code" value=(code);
                            button.it.cv.bt.danger type="submit" { (kit::icon(Icon::Cross)) span { "End Session" } }
                        }
                    }
                }
            }))
        }
    }
}

pub fn page(pd: &PageData) -> Markup {
    // One single-use token for every form of the page: each post leads to
    // a new page.
    let token = pd.once();
    let u = pd.param("u").filter(|v| is_op_id(v)).and_then(|id| pd.ussd?.get(&id, pd.p.id, pd.app.auth.now()));
    let v = pd.views;
    let asks = command(v).ok().filter(|c| c.typed() && !held(v));
    html! {
        (render::live_snapshot(""))
        (ussd(pd, u.as_ref(), &token))
        (toolkit(v))
        @if let Some(c) = &asks { (answer_form(pd, c, &token)) }
        (nav(v))
        form #stkf method="post" action=(STK_SELECT) {
            (pd.csrf())
            (once_field(&token))
        }
    }
}

// --- Posts ---

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Post {
    Send,
    Reply,
    End,
    Select,
    Respond,
    Back,
    Cancel,
}

const POSTS: [Post; 7] = [Post::Send, Post::Reply, Post::End, Post::Select, Post::Respond, Post::Back, Post::Cancel];

impl Post {
    fn path(self) -> &'static str {
        match self {
            Post::Send => USSD_SEND,
            Post::Reply => USSD_REPLY,
            Post::End => USSD_END,
            Post::Select => STK_SELECT,
            Post::Respond => STK_RESPOND,
            Post::Back => STK_BACK,
            Post::Cancel => STK_CANCEL,
        }
    }

    pub fn of(path: &str) -> Option<Post> {
        POSTS.into_iter().find(|p| p.path() == path)
    }

    fn call(self) -> &'static str {
        match self {
            Post::Send => "/api/ussd/send",
            Post::Reply => "/api/ussd/respond",
            Post::End => "/api/ussd/cancel",
            Post::Select => "/api/stk/select",
            Post::Respond => "/api/stk/respond",
            Post::Back => "/api/stk/back",
            Post::Cancel => "/api/stk/cancel",
        }
    }

    fn ussd(self) -> bool {
        matches!(self, Post::Send | Post::Reply | Post::End)
    }
}

/// A code or reply as the agent sends it: spaces dropped; `None` if
/// anything but digits, `*`, `#` and `+` is left, or too long.
fn code(s: &str) -> Option<String> {
    let c: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (c.len() <= CODE_MAX && c.bytes().all(|b| b.is_ascii_digit() || b"*#+".contains(&b))).then_some(c)
}

/// The standard SIM codes that carry a PIN or PUK (change PIN / PIN2,
/// unblock with the PUK / PUK2): a phone handles them itself, but sent as
/// USSD they would go to the network, PINs and all.
fn sim_code(c: &str) -> bool {
    ["**04*", "**042*", "**05*", "**052*"].iter().any(|p| c.starts_with(p))
}

const SIM_CODE: &str = "Use Router \u{203a} SIM Card to change the PIN or enter the PUK.";

/// What a post sends: the body and the text its digest is made of.
struct Send {
    body: Value,
    key: String,
}

/// The toolkit's `c`: "<command_id>" and, after a dot, an item (1–255) or
/// `y`/`n`; `None` for anything else.
fn command_field(c: &str) -> Option<(&str, Option<&str>)> {
    let (id, rest) = match c.split_once('.') {
        Some((id, rest)) => (id, Some(rest)),
        None => (c, None),
    };
    (!id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())).then_some((id, rest))
}

/// The checked post: what it sends, or why not. `Err(None)`: a value no
/// page sends (400); `Err(Some(msg))`: the USSD field's message.
fn parse(k: Post, form: &Form) -> Result<Send, Option<&'static str>> {
    match k {
        Post::Send => {
            let c = code(form.get("code").unwrap_or("")).ok_or(Some("A USSD code has only digits, *, # and +."))?;
            if c.is_empty() {
                return Err(Some("Enter a USSD code"));
            }
            if sim_code(&c) {
                return Err(Some(SIM_CODE));
            }
            Ok(Send { key: c.clone(), body: json!({"code": c}) })
        }
        Post::Reply => {
            let r = code(form.get("reply").unwrap_or("")).ok_or(Some("A reply has only digits, *, # and +."))?;
            if sim_code(&r) {
                return Err(Some(SIM_CODE));
            }
            Ok(Send { key: r.clone(), body: json!({"reply": r}) })
        }
        Post::End => Ok(Send { key: String::new(), body: json!({}) }),
        Post::Select => {
            let (id, item) = command_field(form.get("c").unwrap_or("")).ok_or(None)?;
            let item = item.and_then(|i| i.parse::<u8>().ok().filter(|n| *n > 0 && i.bytes().all(|b| b.is_ascii_digit()))).ok_or(None)?;
            Ok(Send { key: format!("{id}.{item}"), body: json!({"command_id": id, "item_id": item}) })
        }
        Post::Respond => {
            let (id, yes) = command_field(form.get("c").unwrap_or("")).ok_or(None)?;
            match yes {
                Some("y" | "n") => Ok(Send { key: format!("{id}.{}", yes.unwrap_or("")), body: json!({"command_id": id, "text": "", "yes": yes == Some("y")}) }),
                Some(_) => Err(None),
                None => {
                    let text = form.get("text").unwrap_or("");
                    if text.chars().count() > TEXT_MAX || text.chars().any(char::is_control) {
                        return Err(None);
                    }
                    Ok(Send { key: format!("{id}\n{text}"), body: json!({"command_id": id, "text": text}) })
                }
            }
        }
        Post::Back | Post::Cancel => {
            let (id, rest) = command_field(form.get("c").unwrap_or("")).ok_or(None)?;
            if rest.is_some() {
                return Err(None);
            }
            Ok(Send { key: id.to_owned(), body: json!({"command_id": id}) })
        }
    }
}

/// The answer as shown: `response`, else `raw_response` (as the app's),
/// cut to 4 KiB.
fn answer_text(data: &Value) -> String {
    let r = data["response"].as_str().filter(|s| !s.is_empty()).or_else(|| data["raw_response"].as_str()).unwrap_or("");
    let mut cut = ANSWER_MAX.min(r.len());
    while !r.is_char_boundary(cut) {
        cut -= 1;
    }
    r[..cut].to_owned()
}

/// 303 to `to` (ending in "?" or "&") with `text` for this session.
fn line(app: &AppState, flash: &Flash, owner: SessionId, to: &str, ok: Option<bool>, text: &str) -> Response {
    let Ok(id) = app.auth.random_hex(8) else { return session::see_other(PATH) };
    flash.insert(id.clone(), owner, (ok, text.to_owned()), app.auth.now());
    session::see_other(&format!("{to}r={id}"))
}

/// A USSD outcome for this session, under the post's token.
fn kept(ussd: &UssdLog, app: &AppState, owner: SessionId, token: &str, entry: Ussd) -> Response {
    ussd.insert(token.to_owned(), owner, entry, app.auth.now());
    session::see_other(&format!("{PATH}?u={token}"))
}

/// `POST …/ussd/{send,reply,cancel}`, `…/stk/{select,respond,back,cancel}`:
/// the session, the form and its CSRF token, the fields, then the call
/// with the page's single-use token.
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
    let now = ctx.app.auth.now();
    // The answer the reply or End Session belongs to (kept on a failure,
    // as the app keeps its response).
    let u = form.get("u").filter(|v| is_op_id(v)).map(str::to_owned);
    let before = u.as_deref().and_then(|id| ctx.ussd.get(id, p.id, now));
    let typed = form.get("code").and_then(code).unwrap_or_default();
    let send = match parse(k, &form) {
        Ok(s) => s,
        Err(None) => return render::bad_request(&nonce),
        Err(Some(msg)) => {
            // The USSD form again, as typed (a SIM code never: it holds a
            // PIN), with the app's message (400).
            let shown = form.get(if k == Post::Send { "code" } else { "reply" }).unwrap_or("").chars().take(CODE_MAX).collect();
            r.query = u.as_ref().map(|id| format!("u={id}"));
            let value = match k {
                _ if msg == SIM_CODE && k == Post::Send => String::new(),
                Post::Send => shown,
                _ => typed.clone(),
            };
            r.typed = Some(FormState { values: vec![("code", value)], errors: vec![("code", msg.into())] });
            return screen::show(&ctx, p, r, &SIM_SERVICES, None, 400).await;
        }
    };
    // A reply with nothing typed: nothing to send (the app's button is disabled).
    if k == Post::Reply && send.key.is_empty() {
        return session::see_other(&u.map_or_else(|| PATH.to_owned(), |id| format!("{PATH}?u={id}")));
    }
    let token = form.get("once").unwrap_or("").to_owned();
    // Keyed: a SIM response may be a secret; the store keeps no plain hash.
    let keyed = ctx.app.auth.opaque_key("sim-services-once", &send.key).unwrap_or_default();
    let digest = job::digest(&[k.path(), &keyed]);
    let (flash, ussd, owner, tok) = (Arc::clone(&ctx.flash), Arc::clone(&ctx.ussd), p.id, token.clone());
    let code_sent = if k == Post::Send { send.key.clone() } else { typed };
    let work: OnceJob = Box::new(move |app: &AppState, claim| {
        claim.disarm();
        let out = super::call(app, Method::Post, k.call(), &send.body);
        claim.set(match &out {
            Ok(_) => Token::Sent(claim.digest()),
            Err((status, e)) => Token::Failed(claim.digest(), *status, e.clone()),
        });
        let failed = |e: &str| format!("Failed: {}", fmt::truncate(e, 120));
        match (k, out) {
            (Post::Send | Post::Reply, Ok(data)) => {
                let active = data["session_active"].as_bool() == Some(true);
                kept(&ussd, app, owner, &tok, Ussd { code: code_sent, answer: Some((answer_text(&data), active)), line: None })
            }
            (Post::End, Ok(_)) => kept(&ussd, app, owner, &tok, Ussd { code: code_sent, answer: None, line: Some((true, "USSD session ended".into())) }),
            (Post::Send | Post::Reply | Post::End, Err((_, e))) => {
                let answer = before.and_then(|b| b.answer);
                kept(&ussd, app, owner, &tok, Ussd { code: code_sent, answer, line: Some((false, failed(&e))) })
            }
            // The toolkit shows what the SIM does next; a failure stops
            // there, as the app (no reconnect until Reload SIM Menu).
            (_, Ok(_)) => session::see_other(PATH),
            (_, Err((_, e))) => line(app, &flash, owner, &format!("{PATH}?held=1&"), Some(false), &failed(&e)),
        }
    });
    let lane = crate::server::lane(Method::Post, k.call());
    let at = if k.ussd() { format!("{PATH}?") } else { format!("{PATH}?held=1&") };
    match job::once(&ctx, owner, &nonce, &token, digest, lane, SETTLE_SLOW, work).await {
        Sent::Ran(resp) => resp,
        // The same post again: where the first one led.
        Sent::Done(_) if k.ussd() => session::see_other(&format!("{PATH}?u={token}")),
        Sent::Done(Ok(())) => session::see_other(PATH),
        Sent::Done(Err((_, e))) => line(&ctx.app, &ctx.flash, owner, &at, Some(false), &format!("Failed: {}", fmt::truncate(&e, 120))),
        Sent::Expired => line(&ctx.app, &ctx.flash, owner, &format!("{PATH}?"), Some(false), "This page expired, so nothing was sent. Try again."),
        Sent::Busy => line(&ctx.app, &ctx.flash, owner, &format!("{PATH}?"), Some(false), "Failed: the router is busy. Try again in a few seconds."),
        Sent::Unknown => line(&ctx.app, &ctx.flash, owner, &at, None, "The router may still be working on it. Check before trying again."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boottime::BootInstant;
    use crate::events::hub::SlotView;

    fn views(d: Option<Value>, error: Option<&str>, arg: u64) -> Views {
        let now = BootInstant::ZERO;
        let slot = SlotView { version: 1, at: now, value_at: now, value: d.map(Arc::new), error: error.map(Arc::from) };
        let mut v = Views::new(now, vec![(Topic::Stk, slot)]);
        // The command the page was loaded with (`arg`'s shift).
        v.arg = arg << 1;
        v
    }

    fn menu() -> Value {
        json!({"type": "menu", "title": "Test SIM", "text": "", "items": [{"id": 1, "label": "Balance"}, {"id": 2, "label": "Plans"}],
               "root_menu": true, "command_id": "1700000000001", "busy": false, "status": "ready", "reason": "",
               "input": {"min": 0, "max": 0, "yes_no": false, "hidden": false, "digits_only": true}})
    }

    /// The app's section: items as buttons of `#stkf` naming the command;
    /// a submenu adds Back and End SIM Session; no menu: Reload.
    #[test]
    fn the_toolkit_follows_the_app() {
        let s = toolkit(&views(Some(menu()), None, 1_700_000_000_001)).into_string();
        assert!(s.starts_with("<section class=\"group\" id=\"stk\"><h2>Test SIM</h2>"), "{s}");
        assert!(s.contains(
            "<button class=\"it bt\" type=\"submit\" form=\"stkf\" formaction=\"/ui/router/sim-services/stk/select\" name=\"c\" value=\"1700000000001.2\">Plans</button>"
        ), "{s}");
        let n = nav(&views(Some(menu()), None, 0)).into_string();
        assert!(!n.contains("Back") && !n.contains("Reload") && n.contains("Carrier-provided services from your SIM card"), "{n}");
        let mut sub = menu();
        sub["root_menu"] = json!(false);
        sub["busy"] = json!(true);
        let n = nav(&views(Some(sub.clone()), None, 0)).into_string();
        assert!(n.contains("value=\"1700000000001\" disabled>Back</button>") && n.contains("End SIM Session"), "{n}");
        assert!(toolkit(&views(Some(sub), None, 0)).into_string().contains("disabled>Balance"));
        // No session: the error, the app's line, Reload.
        let s = toolkit(&views(None, Some("RIL connection failed: <no socket>"), 0)).into_string();
        assert!(s.contains("RIL connection failed: &lt;no socket&gt;") && s.contains("Reload the SIM menu to reconnect."), "{s}");
        assert!(nav(&views(None, Some("x"), 0)).into_string().contains("<a class=\"it bt\" href=\"/ui/router/sim-services\">Reload SIM Menu</a>"));
        let status = json!({"type": "status", "items": [], "reason": "Waiting for the SIM", "busy": true, "command_id": "7"});
        let s = toolkit(&views(Some(status.clone()), None, 0)).into_string();
        assert!(s.contains("<h2>SIM Toolkit</h2>") && s.contains("Waiting for the SIM"), "{s}");
        // Busy: Reload is dimmed, as the app disables the section.
        assert!(nav(&views(Some(status), None, 0)).into_string().contains("<span class=\"it bt off\" aria-disabled=\"true\">Reload SIM Menu</span>"));
        // After a failed action (`held`): the app's failure state, not the
        // menu the worker still has; only Reload; no stream.
        let mut held = views(Some(menu()), None, 1_700_000_000_001);
        held.arg |= 1;
        let s = toolkit(&held).into_string();
        assert_eq!(s, "<section class=\"group\" id=\"stk\"><h2>SIM Toolkit</h2><ul class=\"list\"><li class=\"it\"><span class=\"muted\">Reload the SIM menu to reconnect.</span></li></ul></section>");
        let n = nav(&held).into_string();
        assert!(n.contains("<a class=\"it bt\" href=\"/ui/router/sim-services\">Reload SIM Menu</a>") && !n.contains("Back") && !n.contains("<button"), "{n}");
        assert!(!live(&held) && live(&views(Some(menu()), None, 0)));
        assert_eq!(arg(&views(Some(menu()), None, 0), Some("held=1")), 1_700_000_000_001 << 1 | 1);
    }

    /// Input commands: Yes / No buttons; a text for the page's own command
    /// is the form after the fragment (`own`), another one reloads.
    #[test]
    fn inputs_are_buttons_or_the_pages_form() {
        let yn = json!({"type": "input", "text": "Buy 1 GB?", "command_id": "9", "root_menu": false,
                        "input": {"min": 1, "max": 1, "yes_no": true}});
        let s = toolkit(&views(Some(yn), None, 9)).into_string();
        assert!(s.contains("value=\"9.y\">Yes</button>") && s.contains("value=\"9.n\">No</button>") && !s.contains("own"), "{s}");
        let text = json!({"type": "input", "text": "Enter PIN 0000000000000042", "command_id": "9", "input": {"min": 4, "max": 8, "hidden": true}});
        let s = toolkit(&views(Some(text.clone()), None, 9)).into_string();
        // The SIM's text as it is (the app's).
        assert!(s.contains("class=\"group own\"") && s.contains("Enter PIN 0000000000000042") && !s.contains("Enter Response"), "{s}");
        let s = toolkit(&views(Some(text), None, 8)).into_string();
        assert!(!s.contains("own") && s.contains("<a class=\"it bt\" href=\"\">Enter Response</a>"), "{s}");
        let display = json!({"type": "display", "text": "Done", "command_id": "10"});
        assert!(toolkit(&views(Some(display), None, 10)).into_string().contains("value=\"10\">Continue</button>"));
    }

    #[test]
    fn posts_send_only_checked_values() {
        let f = |q: &str| Form::parse(q.as_bytes());
        let s = parse(Post::Send, &f("code=*100%23")).unwrap_or_else(|_| panic!());
        assert_eq!(s.body, json!({"code": "*100#"}));
        assert_eq!(parse(Post::Send, &f("code=%20*1%2000%23%20")).ok().map(|s| s.body), Some(json!({"code": "*100#"})));
        assert_eq!(parse(Post::Send, &f("code=")).err(), Some(Some("Enter a USSD code")));
        assert_eq!(parse(Post::Send, &f("code=*100%23;reboot")).err(), Some(Some("A USSD code has only digits, *, # and +.")));
        assert_eq!(parse(Post::Select, &f("c=17.3")).ok().map(|s| s.body), Some(json!({"command_id": "17", "item_id": 3})));
        for bad in ["c=17", "c=17.0", "c=17.256", "c=x.1", "c=17.1x", "c="] {
            assert!(matches!(parse(Post::Select, &f(bad)), Err(None)), "{bad}");
        }
        assert_eq!(parse(Post::Respond, &f("c=17.y")).ok().map(|s| s.body), Some(json!({"command_id": "17", "text": "", "yes": true})));
        assert_eq!(parse(Post::Respond, &f("c=17&text=1234")).ok().map(|s| s.body), Some(json!({"command_id": "17", "text": "1234"})));
        assert!(matches!(parse(Post::Respond, &f("c=17.q")), Err(None)) && matches!(parse(Post::Respond, &f("c=17&text=a%0Ab")), Err(None)));
        assert!(matches!(parse(Post::Back, &f("c=17.1")), Err(None)));
        assert_eq!(parse(Post::Cancel, &f("c=17")).ok().map(|s| s.body), Some(json!({"command_id": "17"})));
        // The SIM codes that carry a PIN or PUK are refused.
        for pin in ["**04*1234*5678*5678%23", "**042*1234*5678*5678%23", "**05*12345678*1234*1234%23", "%20**052*1*2*2%23"] {
            assert_eq!(parse(Post::Send, &f(&format!("code={pin}"))).err(), Some(Some(SIM_CODE)), "{pin}");
            assert_eq!(parse(Post::Reply, &f(&format!("reply={pin}"))).err(), Some(Some(SIM_CODE)), "{pin}");
        }
        assert!(parse(Post::Send, &f("code=*04*1%23")).is_ok() && parse(Post::Send, &f("code=**21*1%23")).is_ok());
        // Answers: `response`, else `raw_response`, as they are (the app's).
        assert_eq!(answer_text(&json!({"response": "", "raw_response": "Voucher 00000000000000042 ok"})), "Voucher 00000000000000042 ok");
        assert_eq!(answer_text(&json!({"response": "Balance 5.00"})), "Balance 5.00");
        // The session is opened on a plain load only (the app's screen
        // opening and Reload): not on an answer, a result line or after a
        // failed action.
        assert!(opens(None) && opens(Some("t=0123456789abcdef")));
        for q in ["u=0123456789abcdef", "r=0123456789abcdef", "held=1&r=0123456789abcdef"] {
            assert!(!opens(Some(q)), "{q}");
        }
    }
}
