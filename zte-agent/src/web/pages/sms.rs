//! The SMS tab (iOS `SMSListView`, `SMSConversationView`,
//! `SMSComposeView`): Messages, a conversation, opening (mark read),
//! replying, New Message and deleting a conversation. The model is
//! `web::sms`.
//!
//! - **Messages** (`/ui/sms`, a table screen; this block reads the list):
//!   the search (on the server, posted: `POST /ui/sms`, so the text is in
//!   no URL), the storage filter, the storage row, "<n> Conversations". An
//!   unread row is a POST form to `/ui/sms/open`, which marks the
//!   conversation read and opens it (the app marks read on open; a GET
//!   never changes anything); a read row is a link.
//! - **A conversation** (`/ui/sms/t/<key>`, the key opaque): bubbles in a
//!   box that starts at its newest message (also on a reload), "Earlier
//!   messages" past 100, the reply bar (`#end`, with the new-message
//!   notice and the last failure). `<title>` is "Conversation": the number
//!   is only in the page.
//! - **Send** (`POST /ui/sms/send`, a reply with the conversation's key or
//!   New Message with a typed number): a single-use token, checked first
//!   and claimed only for valid fields, right before the device call,
//!   with a hash of what it sends; given back if nothing is sent (also
//!   when the request is dropped while the router is busy). A replay of
//!   the same send sends nothing: it waits for the first post and shows
//!   what it did (sent, its failure with the values kept, or, unknown, no
//!   "sent"); another send with a used token, an unknown or an expired one
//!   shows the form with "This form expired" and the values kept; every
//!   page with the form carries a new token. A failure keeps the values,
//!   as the app; with the router busy, a reply keeps its text on a page
//!   that reads nothing.
//! - **Delete** (the app's swipe): a question page naming the conversation
//!   (`/ui/sms/t/<key>/delete`, Android's words), whose form carries the
//!   conversation's fingerprint; the POST re-reads it and asks again if
//!   it changed, so a message that arrived meanwhile is never deleted
//!   unseen. While a store cannot be read, nothing is deleted (it could
//!   hold more of the conversation).
//! - **Live** (the `sms` topic, push only): "New message · Refresh" once
//!   the arrivals counter passes the page's (`Views::arg`, in the stream
//!   URL); the stream's links reload the page they are on. Numbers and
//!   text are never in a URL, a fragment id or a stream.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use axum::extract::Request;
use axum::http::Method;
use axum::response::Response;
use maud::{html, Markup};

use crate::auth::{AuthState, Principal, SessionId};
use crate::events::Topic;
use crate::handlers::AppState;
use crate::httpd::gate::{self, Lane};
use crate::httpd::Ctx;
use crate::web::action::{self, Ask};
use crate::web::job;
use crate::web::kit::{self, Icon, Tab};
use crate::web::render::{self, Fragment, PageCtx, Views};
use crate::web::screen::PageData;
use crate::web::session::{self, query_param, Form};
use crate::web::sms::{self, Conv, Msg, Store};
use crate::web::store::{settle, Claim, Once, Settled, Token, SETTLE};
use crate::web::{fmt, live, CspNonce};

pub const LIST: &str = "/ui/sms";
pub const FORWARD: &str = "/ui/sms/forward";
pub const NEW: &str = "/ui/sms/new";
const THREAD: &str = "/ui/sms/t/";
const OPEN: &str = "/ui/sms/open";
const SEND: &str = "/ui/sms/send";
const DELETE: &str = "/ui/sms/delete";
/// Messages per page of a conversation.
const PAGE: usize = 100;
const EXPIRED: &str = "This form expired, so the message was not sent. Send it again.";
const CHANGED: &str = "The conversation changed. Review and try again.";
/// The app's words for a send that failed without an error.
const NOT_SENT: &str = "Message not sent";
/// A replay of a reply whose first post's outcome is unknown.
const MAYBE: &str = "This message may not have been sent. Check the conversation before sending it again.";
const BUSY: &str = "Failed to send: the router is busy. Try again in a few seconds.";

/// `base`, then the store and `extra` as its query (none when empty).
fn url(base: &str, store: Store, extra: &str) -> String {
    let mut s = format!("{base}?{}{extra}", store.param());
    while s.ends_with(['?', '&']) {
        s.pop();
    }
    s
}

fn thread_url(key: &str, store: Store, extra: &str) -> String {
    url(&format!("{THREAD}{key}"), store, extra)
}

fn delete_url(key: &str, store: Store) -> String {
    url(&format!("{THREAD}{key}/delete"), store, "")
}

/// A page of this module (Messages itself is a screen).
pub enum Page {
    New,
    /// A conversation, by its key.
    Thread(String),
    /// A conversation's delete question.
    Delete(String),
}

impl Page {
    pub fn of(path: &str) -> Option<Page> {
        if path == NEW {
            return Some(Page::New);
        }
        let rest = path.strip_prefix(THREAD)?;
        match rest.strip_suffix("/delete") {
            Some(k) => sms::is_key(k).then(|| Page::Delete(k.to_owned())),
            None => sms::is_key(rest).then(|| Page::Thread(rest.to_owned())),
        }
    }
}

/// The page whose forms post to `path` (`routes::form_page`).
pub fn form_page(path: &str) -> Option<&'static str> {
    matches!(path, LIST | OPEN | SEND | DELETE).then_some(LIST)
}

/// The conversation `key` of `store` as it is now, and the note of a store
/// that could not be read (blocking).
fn find(app: &AppState, store: Store, key: &str) -> Result<(Option<Conv>, Option<String>), String> {
    sms::read(app, store).map(|l| (sms::find(app, sms::group(l.msgs, sms::now_unix()), key), l.partial))
}

/// A send form's hidden fields: the CSRF token and a new single-use token
/// (never restored by the browser into an old copy of the form).
fn send_fields(app: &AppState, p: &Principal, once: &Once) -> Markup {
    html! {
        (render::csrf_field(&app.auth, p))
        (render::once_field(&once.fresh(&app.auth, p.id)))
    }
}

// --- Live: "New message" ---

/// The SMS arrivals since the agent started (`sms` topic).
pub fn received(v: &Views) -> u64 {
    v.value(Topic::Sms).and_then(|x| x["received"].as_u64()).unwrap_or(0)
}

/// "Refresh" reloads the page as it is (`href=""`): no key or search in
/// the fragment.
fn new_notice(v: &Views) -> Markup {
    let n = received(v).saturating_sub(v.arg);
    html! {
        div #sms-new {
            @if n > 0 {
                p.msg.info role="status" {
                    @if n == 1 { "New message" } @else { (n) " new messages" }
                    " \u{b7} " a href="" { "Refresh" }
                }
            }
        }
    }
}

pub static FRAGMENTS: [Fragment; 1] = [Fragment { id: "sms-new", topics: &[Topic::Sms], render: new_notice }];

// --- Messages ---

/// The app's search bar: posted to this page (`#srch`), so the text is in
/// no URL, the history or `next`. During a search, "Cancel" (the app's)
/// leads back to the store's whole list.
fn search(store: Store, q: &str, csrf: &Markup) -> Markup {
    html! {
        form.srch #srch method="post" action=(url(LIST, store, "")) role="search" {
            (csrf)
            input type="search" name="q" value=(q) placeholder="Search messages" aria-label="Search messages"
                autocomplete="off" maxlength="64";
            button.btn.sm type="submit" { "Search" }
            @if !q.is_empty() { a.nl href=(url(LIST, store, "")) { "Cancel" } }
        }
    }
}

/// The "Storage" segmented control: links, or during a search the search
/// form posted to the other store's page (the search kept).
fn storage(store: Store, searching: bool) -> Markup {
    html! {
        nav.seg aria-label="Storage" {
            @for (s, _, label) in Store::OPTIONS {
                @let current = (s == store).then_some("true");
                @if searching {
                    button form="srch" formaction=(url(LIST, s, "")) aria-current=[current] { (label) }
                } @else {
                    a href=(url(LIST, s, "")) aria-current=[current] { (label) }
                }
            }
        }
    }
}

/// A conversation row: the number (bold while unread) and the time, the
/// latest message (2 lines) and the unread count; then "Delete" (the
/// app's swipe action).
fn row(app: &AppState, csrf: &Markup, c: &Conv, store: Store, now: i64, off: i64) -> Markup {
    let latest = c.latest();
    let body = html! {
        span.cvb {
            span.l1 { span.nm { (c.number) } span.tm { (latest.date.map(|t| sms::relative(t, now, off)).unwrap_or_default()) } }
            span.l2 {
                span.pv { (latest.content) }
                @if c.unread > 0 { span.ub { (c.unread) span.sr { " unread" } } }
            }
        }
    };
    let key = sms::url_key(app, c);
    html! {
        li.cr {
            @match &key {
                Some(k) if c.unread > 0 => form.sf method="post" action=(OPEN) {
                    (csrf)
                    input type="hidden" name="t" value=(k);
                    @if store != Store::All { input type="hidden" name="store" value=(store.value()); }
                    button.it.cv.ur type="submit" { (body) }
                },
                Some(k) => a.it.cv href=(format!("{}#end", thread_url(k, store, ""))) { (body) },
                None => div.it.cv.ur[c.unread > 0] { (body) },
            }
            @if let Some(k) = &key { a.del href=(delete_url(k, store)) { "Delete" span.sr { " conversation with " (c.number) } } }
        }
    }
}

/// Messages (`SMSListView`): reads the list and the capacity. The search
/// comes only from the search form (`found`), never from the URL.
pub fn list(pd: &PageData) -> Markup {
    let store = Store::parse(pd.param("store").as_deref()).unwrap_or(Store::All);
    let q = pd.form.and_then(|f| f.value("q")).unwrap_or("");
    let now = sms::now_unix();
    let off = sms::local_offset(now);
    let (convs, error, partial) = match sms::read(pd.app, store) {
        Ok(l) => (sms::group(l.msgs, now), None, l.partial),
        Err(e) => (Vec::new(), Some(e), None),
    };
    let capacity = sms::capacity(pd.app);
    let lower = q.to_lowercase();
    let shown: Vec<&Conv> = convs.iter().filter(|c| c.matches(&lower)).collect();
    let csrf = pd.csrf();
    html! {
        (render::live_snapshot(&url(LIST, store, "")))
        (render::fragment(&FRAGMENTS, "sms-new", pd.views))
        (search(store, q, &csrf))
        (storage(store, !q.is_empty()))
        @if let Some(e) = &error { (kit::notice(false, &format!("Failed: {}", fmt::truncate(e, 120)))) }
        @if let Some(p) = &partial { (kit::notice(false, &fmt::truncate(p, 160))) }
        // Both secondary, the count as a caption (the app's row).
        @if let Some((used, total)) = capacity {
            (kit::section(None, None, html! { li.it { span.muted { "Storage" } span.tm { (used) "/" (total) } } }))
        }
        // The app's header, always, unpluralised ("1 Conversations").
        (kit::section(Some(&format!("{} Conversations", shown.len())), None, html! {
            @for c in &shown { (row(pd.app, &csrf, c, store, now, off)) }
        }))
        @if convs.is_empty() && error.is_none() {
            (kit::empty_state(Icon::Message, "No Messages", "SMS messages will appear here"))
        } @else if shown.is_empty() && !q.is_empty() {
            (kit::empty_state(Icon::Search, &format!("No Results for \u{201c}{q}\u{201d}"), "Check the spelling or try a new search."))
        }
        p.foot { "Calls are only in the OpenU60 app." }
    }
}

// --- A conversation ---

/// A bubble: incoming grey on the left; sent blue, failed red ("Not
/// delivered"), draft orange on the right; the time under it (the
/// router's local time at that date; the date on hover).
fn bubble(m: &Msg) -> Markup {
    let class = match m.tag {
        sms::FAILED => "b out failed",
        sms::DRAFT => "b out draft",
        _ if m.incoming() => "b in",
        _ => "b out",
    };
    html! {
        li class=(class) {
            p { (m.content) }
            small {
                @if m.tag == sms::FAILED { span.nd role="img" aria-label="Not delivered" { (kit::icon(Icon::Alert)) } }
                @if let Some(t) = m.date {
                    @let off = sms::local_offset(t);
                    span title=(sms::day(t, off)) { (sms::clock(t, off)) }
                }
            }
        }
    }
}

/// What a conversation page is drawn for.
struct Thread<'a> {
    p: &'a Principal,
    nonce: &'a str,
    once: &'a Once,
    key: &'a str,
    store: Store,
    before: Option<u64>,
}

/// A send that did not happen: the text as typed, why, and the status.
#[derive(Clone)]
struct Failure {
    text: String,
    msg: String,
    status: u16,
}

/// A page pushed from Messages ("‹ Messages"): `title` in `<title>`, never
/// a number; `h1` in the nav bar.
fn shell(nonce: &str, title: &str, list: &str, h1: &str, live: Option<&str>, body: Markup, status: u16) -> Response {
    let pc = PageCtx {
        title,
        tab: Some(Tab::Sms),
        nonce,
        live,
        lead: kit::back(list, "Messages"),
        heading: Some(html! { h1 { (h1) } }),
        ..PageCtx::default()
    };
    render::html(status, render::page(&pc, body))
}

fn not_found(nonce: &str, list: &str) -> Response {
    let body = html! {
        (kit::empty_state(Icon::Message, "Conversation not found",
            "It may have been deleted. Links to conversations last until the agent restarts."))
        p.ctr { a.btn href=(list) { "Back to Messages" } }
    };
    shell(nonce, "Conversation", list, "Conversation", None, body, 404)
}

/// The reply bar (`#end`, where every way to a conversation lands, the
/// failed send's answer too): the new-message notice, why the last send
/// failed, the text field with a new token.
fn reply_bar(app: &AppState, t: &Thread, failure: Option<&Failure>, notice: Markup) -> Markup {
    html! {
        div #end {
            (notice)
            @if let Some(f) = failure { p.sfail role="alert" { (kit::icon(Icon::Warn)) span { (f.msg) } } }
            form.reply method="post" action=(format!("{SEND}#end")) {
                (send_fields(app, t.p, t.once))
                input type="hidden" name="t" value=(t.key);
                @if t.store != Store::All { input type="hidden" name="store" value=(t.store.value()); }
                textarea name="text" rows="1" placeholder="Message" aria-label="Message" maxlength="765" required {
                    (failure.map_or("", |f| f.text.as_str()))
                }
                button.send type="submit" aria-label="Send" { (kit::icon(Icon::ArrowUp)) }
            }
        }
    }
}

/// The conversation (blocking: reads the list), or with `read` false only
/// its reply bar (the router was busy). A GET has no failure and is live
/// (the status line, the new-message notice); the answer to a reply that
/// was not sent is not (its URL is the post's). A list that cannot be read
/// shows why and no reply bar (there is nothing to reply to), except
/// under a failed send, whose text it keeps. Every reply bar issues a new
/// single-use token.
fn thread_page(app: &AppState, t: &Thread, failure: Option<&Failure>, read: bool) -> Response {
    let list = url(LIST, t.store, "");
    if !read {
        let status = failure.map_or(200, |f| f.status);
        return shell(t.nonce, "Conversation", &list, "Conversation", None, reply_bar(app, t, failure, html! {}), status);
    }
    let mut views = super::read_topics(app, &[Topic::Sms]);
    views.arg = received(&views);
    let here = format!("{}#end", thread_url(t.key, t.store, ""));
    let (h1, status, content, live) = match sms::read(app, t.store) {
        Err(e) => {
            let why = kit::notice(false, &format!("Failed: {}", fmt::truncate(&e, 120)));
            if failure.is_none() {
                let body = html! { (why) p.ctr { a.btn href=(here) { "Try again" } } };
                return shell(t.nonce, "Conversation", &list, "Conversation", None, body, 503);
            }
            ("Conversation".to_owned(), 503, why, None)
        }
        Ok(l) => {
            let Some(c) = sms::find(app, sms::group(l.msgs, sms::now_unix()), t.key) else {
                return not_found(t.nonce, &list);
            };
            let live = failure.is_none().then(|| live::url(&app.auth, &format!("/ui/live/sms/{:x}", views.arg))).flatten();
            let end = t.before.and_then(|id| c.msgs.iter().position(|m| m.id == id)).unwrap_or(c.msgs.len());
            let start = end.saturating_sub(PAGE);
            let content = html! {
                @if live.is_some() { (render::live_snapshot(&here)) }
                @if let Some(p) = &l.partial { (kit::notice(false, &fmt::truncate(p, 160))) }
                // A web addition: a conversation reached by a link is not marked read.
                @if c.unread > 0 {
                    form.mr method="post" action=(OPEN) {
                        (render::csrf_field(&app.auth, t.p))
                        input type="hidden" name="t" value=(t.key);
                        @if t.store != Store::All { input type="hidden" name="store" value=(t.store.value()); }
                        button.btn.sm.btn-quiet type="submit" { "Mark as read" }
                    }
                }
                // Scrolls in a box that starts at its end: any load shows the newest.
                div.sc {
                    div {
                        @if start > 0 {
                            p.pg { a href=(thread_url(t.key, t.store, &format!("before={}", c.msgs[start].id))) { "Earlier messages" } }
                        }
                        ol.bb { @for m in &c.msgs[start..end] { (bubble(m)) } }
                        @if end < c.msgs.len() {
                            p.pg { a href=(here) { "Newest messages" } }
                        }
                    }
                }
            };
            (c.number, 200, content, live)
        }
    };
    let body = html! {
        (content)
        (reply_bar(app, t, failure, render::fragment(&FRAGMENTS, "sms-new", &views)))
    };
    shell(t.nonce, "Conversation", &list, &h1, live.as_deref(), body, failure.map_or(status, |f| f.status))
}

/// New Message (`SMSComposeView`): the number, the text with the app's
/// counter (computed when the page is drawn, "0/765 (0 SMS)" at first),
/// Send, then why the last send failed. Reads nothing.
fn compose(app: &AppState, p: &Principal, nonce: &str, once: &Once, number: &str, failure: Option<&Failure>) -> Response {
    let text = failure.map_or("", |f| f.text.as_str());
    let body = html! {
        form.cmp method="post" action=(SEND) {
            (send_fields(app, p, once))
            (kit::section(None, None, html! {
                li.it { input type="tel" name="number" value=(number) placeholder="Phone Number" aria-label="Phone Number"
                    autocomplete="off" maxlength="32" required autofocus; }
            }))
            (kit::section(None, Some(html! { p.foot { (sms::counter(text)) } }), html! {
                li.it { textarea name="text" rows="3" placeholder="Message" aria-label="Message" maxlength="765" required { (text) } }
            }))
            (kit::section(None, None, html! { li { button.it.cv.snd type="submit" { "Send" } } }))
            @if let Some(f) = failure { p.sfail role="alert" { span { (f.msg) } } }
        }
    };
    let pc = PageCtx {
        title: "New Message",
        tab: Some(Tab::Sms),
        nonce,
        lead: html! { a.nl href=(LIST) { "Cancel" } },
        ..PageCtx::default()
    };
    render::html(failure.map_or(200, |f| f.status), render::page(&pc, body))
}

/// The delete question (Android's words): how many messages from whom, in
/// the page only. The form carries the conversation's fingerprint;
/// `changed` asks again after it changed (409).
fn ask_delete(app: &AppState, p: &Principal, nonce: &str, key: &str, store: Store, c: &Conv, changed: bool) -> Response {
    let list = url(LIST, store, "");
    let fp = c.fingerprint();
    let line = format!("This will delete all {} messages from {}.", c.msgs.len(), c.number);
    let mut hidden = vec![("t", key), ("fp", fp.as_str())];
    if store != Store::All {
        hidden.push(("store", store.value()));
    }
    let q = Ask {
        title: "Delete conversation?",
        lines: &[&line],
        action: DELETE,
        hidden: &hidden,
        label: Some("Delete"),
        danger: true,
        cancel: &list,
    };
    let body = html! {
        @if changed { (kit::notice(false, CHANGED)) }
        (action::ask(render::csrf_field(&app.auth, p), &q))
    };
    shell(nonce, "Delete conversation?", &list, "Confirm", None, body, if changed { 409 } else { 200 })
}

/// `GET` of a page of this module: New Message at once; a conversation
/// (`?store=…&before=<id>`) or its delete question in a gate task.
pub async fn get(ctx: &Ctx, p: Principal, nonce: String, query: Option<String>, page: Page) -> Response {
    let query = query.as_deref();
    let store = Store::parse(query_param(query, "store").as_deref()).unwrap_or(Store::All);
    match page {
        Page::New => compose(&ctx.app, &p, &nonce, &ctx.once, "", None),
        Page::Thread(key) => {
            let before = query_param(query, "before").and_then(|b| b.parse().ok());
            let (retry, once, n) = (thread_url(&key, store, ""), Arc::clone(&ctx.once), nonce.clone());
            let job = Box::new(move |app: &AppState| {
                let t = Thread { p: &p, nonce: &n, once: &once, key: &key, store, before };
                gate::read_only(|| thread_page(app, &t, None, true))
            });
            job::run(ctx, Lane::General, &nonce, &retry, job).await
        }
        Page::Delete(key) => {
            let (retry, n) = (delete_url(&key, store), nonce.clone());
            let job = Box::new(move |app: &AppState| {
                let list = url(LIST, store, "");
                gate::read_only(|| match find(app, store, &key) {
                    Ok((Some(c), None)) => ask_delete(app, &p, &n, &key, store, &c, false),
                    Ok((None, _)) => not_found(&n, &list),
                    // A store that could not be read may hold more of it.
                    Err(e) | Ok((Some(_), Some(e))) => {
                        let body = kit::notice(false, &format!("Failed: {}", fmt::truncate(&e, 160)));
                        shell(&n, "Delete conversation?", &list, "Confirm", None, body, 503)
                    }
                })
            });
            job::run(ctx, Lane::General, &nonce, &retry, job).await
        }
    }
}

// --- Search, open, send, delete ---

pub enum Post {
    Open,
    Send,
    Delete,
}

impl Post {
    pub fn of(path: &str) -> Option<Post> {
        match path {
            OPEN => Some(Post::Open),
            SEND => Some(Post::Send),
            DELETE => Some(Post::Delete),
            _ => None,
        }
    }
}

/// `POST /ui/sms/open`, `/ui/sms/send` and
/// `/ui/sms/delete`: a session (else the login, nothing was changed), the
/// form, origin and CSRF token, the key and store (else 400); New Message
/// sends a number instead.
pub async fn post(ctx: Ctx, req: Request, which: Post) -> Response {
    let nonce = CspNonce::of(&req);
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&Method::POST, LIST, None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    let key = form.get("t").filter(|k| sms::is_key(k)).map(str::to_owned);
    let store = Store::parse(form.get("store"));
    let number = form.get("number").filter(|_| form.get("t").is_none() && matches!(which, Post::Send));
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        let back = match &key {
            Some(k) => thread_url(k, store.unwrap_or(Store::All), ""),
            None if number.is_some() => NEW.to_owned(),
            None => LIST.to_owned(),
        };
        return render::form_refused(&nonce, &back);
    }
    if let Some(n) = number {
        return send(ctx, p, nonce, To::Number(sms::clean_number(n)), &form).await;
    }
    let (Some(key), Some(store)) = (key, store) else {
        return render::bad_request(&nonce);
    };
    match which {
        Post::Open => open(ctx, nonce, key, store).await,
        Post::Delete => delete(ctx, p, nonce, key, store, &form).await,
        _ => send(ctx, p, nonce, To::Thread(key, store), &form).await,
    }
}

/// Mark the conversation's unread messages read (failures are silent, as
/// the app's), then open it.
async fn open(ctx: Ctx, nonce: String, key: String, store: Store) -> Response {
    let to = format!("{}#end", thread_url(&key, store, ""));
    let job = Box::new(move |app: &AppState| {
        if let Ok((Some(c), _)) = find(app, store, &key) {
            sms::mark_read(app, &c.unread_ids());
        }
        session::see_other(&to)
    });
    job::run(&ctx, Lane::General, &nonce, LIST, job).await
}

/// Where a send goes: a conversation (its key and store; the number is
/// resolved when sending) or a typed number (New Message, cleaned).
#[derive(Clone)]
enum To {
    Thread(String, Store),
    Number(String),
}

impl To {
    /// Where a sent message leads: the conversation, or Messages with
    /// "Message sent".
    fn done(&self) -> String {
        match self {
            To::Thread(key, store) => format!("{}#end", thread_url(key, *store, "")),
            To::Number(_) => format!("{LIST}?saved=sent"),
        }
    }

    /// What a claimed token sends: the destination and the text, hashed.
    fn digest(&self, text: &str) -> u64 {
        let mut h = DefaultHasher::new();
        match self {
            To::Thread(key, _) => (0u8, key, text).hash(&mut h),
            To::Number(n) => (1u8, n, text).hash(&mut h),
        }
        h.finish()
    }

    /// The form again with the values and why nothing was sent: New
    /// Message's; a conversation's (blocking: reads the list), or with
    /// `read` false its reply bar alone.
    fn again(&self, app: &AppState, p: &Principal, nonce: &str, once: &Once, f: &Failure, read: bool) -> Response {
        match self {
            To::Thread(key, store) => {
                let t = Thread { p, nonce, once, key, store: *store, before: None };
                gate::read_only(|| thread_page(app, &t, Some(f), read))
            }
            To::Number(number) => compose(app, p, nonce, once, number, Some(f)),
        }
    }
}

/// Test only: what a token claimed for this send records (`key` for a
/// reply, else New Message's cleaned number).
#[cfg(test)]
pub fn digest(key: Option<&str>, number: &str, text: &str) -> u64 {
    key.map_or(To::Number(number.into()), |k| To::Thread(k.into(), Store::All)).digest(text)
}

/// What a send post does.
enum Next {
    Send(Claim),
    /// A redirect (a replay of a sent message, or of one whose outcome is
    /// unknown).
    Go(String),
    /// The form again: why, the status.
    Show(String, u16),
}

/// A post of a token another post has claimed: nothing is sent. The same
/// send (a double click, a reload of its answer): once that post is done
/// (at most 20 s), what it did: sent, its failure, nothing sent, or
/// unknown (still sending, or the token gone: New Message leads to
/// Messages without "sent", a reply shows the conversation with the text
/// and a warning). Another destination or text: an expired form.
async fn replay(once: &Once, token: &str, owner: SessionId, digest: u64, auth: &AuthState, to: &To) -> Next {
    match settle(once, token, owner, digest, auth, SETTLE).await {
        Settled::Other => Next::Show(EXPIRED.to_owned(), 400),
        Settled::Sent => Next::Go(to.done()),
        Settled::Failed(status, msg) => Next::Show(msg, status),
        // That post sent nothing (the router was busy, the list failed).
        Settled::Fresh => Next::Show(NOT_SENT.to_owned(), 503),
        Settled::Unknown => match to {
            To::Number(_) => Next::Go(LIST.to_owned()),
            To::Thread(..) => Next::Show(MAYBE.to_owned(), 409),
        },
    }
}

/// A send. The token is checked first; claimed only for valid fields,
/// right before the send, and given back if nothing is sent. One gate task
/// sends or draws the conversation again; with the router busy, the form
/// again without reading anything.
async fn send(ctx: Ctx, p: Principal, nonce: String, to: To, form: &Form) -> Response {
    // As typed; browsers send line breaks as CR LF.
    let text = form.get("text").unwrap_or("").replace("\r\n", "\n");
    let token = form.get("once").unwrap_or("");
    let digest = to.digest(&text);
    let (once, auth) = (Arc::clone(&ctx.once), Arc::clone(&ctx.app.auth));
    let valid = || match &to {
        To::Number(n) => sms::check_number(n),
        To::Thread(..) => Ok(()),
    };
    let next = match once.get(token, p.id, auth.now()) {
        None => Next::Show(EXPIRED.to_owned(), 400),
        Some(Token::Fresh) => match valid().and_then(|()| sms::check_text(&text)) {
            Err(e) => Next::Show(e, 400),
            Ok(()) => match Claim::take(&once, &auth, token, p.id, digest) {
                Some(claim) => Next::Send(claim),
                None => replay(&once, token, p.id, digest, &auth, &to).await,
            },
        },
        Some(_) => replay(&once, token, p.id, digest, &auth, &to).await,
    };
    let (claim, shown) = match next {
        Next::Go(location) => return session::see_other(&location),
        Next::Show(msg, status) => (None, Failure { text, msg, status }),
        // Shown only if the router was busy (the send never ran).
        Next::Send(claim) => (Some(claim), Failure { text, msg: BUSY.to_owned(), status: 503 }),
    };
    if claim.is_none() && matches!(to, To::Number(_)) {
        return to.again(&ctx.app, &p, &nonce, &once, &shown, false);
    }
    let (o, n, who, dest, f) = (Arc::clone(&once), nonce.clone(), p.clone(), to.clone(), shown.clone());
    let job = Box::new(move |app: &AppState| {
        let f = match claim {
            Some(claim) => match deliver(app, &n, &dest, &f.text, claim) {
                Ok(resp) => return resp,
                Err(failed) => failed,
            },
            None => f,
        };
        dest.again(app, &who, &n, &o, &f, true)
    });
    match job::try_run(&ctx, Lane::General, &nonce, job).await {
        Some(resp) => resp,
        None => to.again(&ctx.app, &p, &nonce, &once, &shown, false),
    }
}

/// Send with a claimed token (blocking): where it leads, or why it was not
/// sent (the token given back unless the device was asked).
fn deliver(app: &AppState, nonce: &str, to: &To, text: &str, mut claim: Claim) -> Result<Response, Failure> {
    let failed = |msg: String, status: u16| Failure { text: text.to_owned(), msg, status };
    let number = match to {
        To::Number(number) => number.clone(),
        To::Thread(key, store) => match find(app, *store, key) {
            Ok((Some(c), _)) => c.number,
            Ok((None, _)) => return Ok(not_found(nonce, &url(LIST, *store, ""))),
            Err(e) => return Err(failed(format!("Failed to send: {}", fmt::truncate(&e, 120)), 503)),
        },
    };
    claim.disarm();
    match sms::send(app, &number, text) {
        Ok(()) => {
            claim.set(Token::Sent(claim.digest()));
            Ok(session::see_other(&to.done()))
        }
        Err((status, e)) => {
            let msg = format!("Failed to send: {}", fmt::truncate(&e, 120));
            claim.set(Token::Failed(claim.digest(), status, msg.clone()));
            Err(failed(msg, status))
        }
    }
}

/// Delete a conversation, confirmed on its question page: its messages as
/// they are now, if its fingerprint is still the one asked about (else
/// the question again) and every store could be read. Then Messages with
/// "Conversation deleted", or with "Delete failed: …" (the app's words; a
/// result line, `?r=`, so the page is a GET whose links and stream work).
async fn delete(ctx: Ctx, p: Principal, nonce: String, key: String, store: Store, form: &Form) -> Response {
    if form.get("confirm") != Some("1") {
        return session::see_other(&delete_url(&key, store));
    }
    let fp = form.get("fp").unwrap_or("").to_owned();
    let (flash, n) = (Arc::clone(&ctx.flash), nonce.clone());
    let job = Box::new(move |app: &AppState| {
        let failed = |e: &str| {
            let msg = format!("Delete failed: {}", fmt::truncate(e, 160));
            let Ok(id) = app.auth.random_hex(8) else { return render::error(503, "Delete failed", &msg, &n) };
            let to = url(LIST, store, &format!("r={id}"));
            flash.insert(id, p.id, (Some(false), msg), app.auth.now());
            session::see_other(&to)
        };
        match find(app, store, &key) {
            // A store that could not be read may hold more of it.
            Err(e) | Ok((Some(_), Some(e))) => failed(&e),
            Ok((None, _)) => not_found(&n, &url(LIST, store, "")),
            Ok((Some(c), None)) if c.fingerprint() != fp => ask_delete(app, &p, &n, &key, store, &c, true),
            Ok((Some(c), None)) => match sms::delete(app, &c.msgs.iter().map(|m| m.id).collect::<Vec<_>>()) {
                Ok(()) => session::see_other(&url(LIST, store, "saved=deleted")),
                Err(e) => failed(&e),
            },
        }
    });
    job::run(&ctx, Lane::General, &nonce, LIST, job).await
}
