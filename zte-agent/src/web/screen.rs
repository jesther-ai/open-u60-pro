//! Screens as data. A `Screen` names its URL, title, tab and nav bar items,
//! what it reads at page load (hub topics, in-process API routes or
//! functions, all in one gate task), its live fragments and its content
//! blocks; one generic GET renders every screen from that.

use std::sync::Arc;

use axum::extract::Request;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::Value;

use super::kit::{self, Icon, Tab};
use super::render::{self, Fragment, PageCtx, Views};
use super::session::{self, query_param};
use super::prefs::Prefs;
use super::store::{AtLog, Dismissed, Once, UssdLog};
use super::{job, live, CspNonce};
use crate::auth::Principal;
use crate::events::Topic;
use crate::handlers::AppState;
use crate::httpd::gate::{self, Lane};
use crate::httpd::{Ctx, Peer};
use crate::server::{self, Method};

/// The nav bar of a screen (iOS: tab root, push, sheet).
pub enum Bar {
    /// A tab root: the title only.
    Root,
    /// Pushed: "‹ parent" (path, parent title).
    Back(&'static str, &'static str),
    /// Pushed, with a text link trailing (path, its text): Forward Log's
    /// "Clear".
    BackLink(&'static str, &'static str, &'static str, &'static str),
    /// A sheet: "Done" back to the page that opened it.
    Done(&'static str),
    /// A modal form (the app's sheet with a "Cancel" button): "Cancel" back
    /// to the page that opened it.
    Cancel(&'static str),
    /// A sheet whose nav bar leads with this fragment (the age badge).
    AgeDone(fn(&Views) -> Markup, &'static str),
    /// A sheet with "Done" leading and this fragment trailing (Processes:
    /// "Kill All Bloat" while there is bloat).
    DoneTrail(&'static str, fn(&Views) -> Markup),
    /// The dashboard: the live status pill leads, this fragment (the age
    /// badge) trails.
    Status(fn(&Views) -> Markup),
    /// A tab root with icon buttons (href, icon, label) leading and
    /// trailing (Messages: the gear; New Message).
    Icons(&'static [NavIcon], &'static [NavIcon]),
    /// Pushed, with icon buttons trailing (eSIM Profiles: Add eSIM, Refresh).
    BackIcons(&'static str, &'static str, &'static [NavIcon]),
    /// A sheet whose trailing "Done" reads otherwise while something runs
    /// (Add eSIM: "Close" during a download): the link, with an id when it
    /// is one of the screen's live fragments.
    DoneAs(fn(&Views) -> Markup),
}

pub type NavIcon = (&'static str, Icon, &'static str);

fn icons(items: &[NavIcon]) -> Markup {
    html! { @for (href, i, label) in items { (kit::icon_link(href, *i, label)) } }
}

/// A page-load read besides the topics.
pub enum Read {
    /// `GET` of an API route, in-process: its `data`, or its error.
    Api(&'static str),
    /// A quick read (General lane) that changes nothing, given the page's
    /// query string (an action's reads get none).
    Fn(fn(&AppState, Option<&str>) -> Result<Value, String>),
}

impl Read {
    pub fn run(&self, app: &AppState, query: Option<&str>) -> Result<Value, String> {
        match self {
            Read::Api(path) => super::pages::api_get(app, path),
            Read::Fn(f) => f(app, query),
        }
    }
}

/// A row of a `Block::Links` list: a link; `sub` ("" for none) under the
/// label.
pub struct Item {
    pub label: &'static str,
    pub icon: Icon,
    pub href: &'static str,
    pub sub: &'static str,
}

pub enum Block {
    /// A grouped list of rows that open other screens.
    Links(&'static str, &'static [Item]),
    /// A grouped list of fixed label/value rows.
    Rows(&'static str, &'static [(&'static str, &'static str)]),
    /// A big icon, title and text (the app's placeholders), and a note
    /// under it ("" for none), centred in the screen.
    Empty(Icon, &'static str, &'static str, &'static str),
    Custom(fn(&PageData) -> Markup),
}

/// A screen searched by a POST to its own path (`search`): the text is in
/// the page only, never in a URL, the history or `next`.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Search {
    No,
    /// Its stream stays open during a search (it does not show the list).
    Live,
    /// A search shows a snapshot: its fragments would show the whole list.
    Still,
}

pub struct Screen {
    pub path: &'static str,
    /// `[a-z0-9]+`: the live stream's `/ui/live/<id>/…`.
    pub id: &'static str,
    pub title: &'static str,
    pub tab: Tab,
    pub bar: Bar,
    /// Read at page load and, on a live screen, streamed.
    pub topics: &'static [Topic],
    pub reads: &'static [Read],
    /// The live fragments (none: a snapshot page without a stream).
    pub live: &'static [Fragment],
    /// The signal chart's history.
    pub history: bool,
    /// The `?saved=` notices (a fixed table: nothing from the URL is shown).
    pub saved: &'static [(&'static str, &'static str)],
    /// The `saved` notices are the app's own result lines: green, as the
    /// app's (else the web UI's blue notice).
    pub saved_ok: bool,
    pub blocks: &'static [Block],
    /// A number the page and its stream compare against (`Views::arg`),
    /// taken at page load (from what it read and its query string): it
    /// travels in the stream URL as hex (the SMS arrivals counter, so "New
    /// message" shows only for later ones; the speed test's version).
    pub arg: Option<fn(&Views, Option<&str>) -> u64>,
    pub search: Search,
    /// The status of a page that renders (200 unless this says otherwise:
    /// 404 for a question about something that is gone).
    pub status: Option<fn(&PageData) -> u16>,
    /// Without JavaScript, reload every 3 s, to the URL this gives, while
    /// it gives one (a speed test that runs; it ends by itself).
    pub refresh: Option<fn(&PageData) -> Option<String>>,
    /// Its blocks draw the notice (`PageData::notice`) where the app shows
    /// its message; else it is above them.
    pub own_notice: bool,
    /// Draws the notice its own way (eSIM Profiles: the app's message card).
    pub notice_view: Option<fn(Notice) -> Markup>,
    /// A live screen that streams only while this holds (a signal detection
    /// sweep runs): checked at page load (no stream otherwise) and on every
    /// update (the stream ends once it no longer holds), so an idle page
    /// samples nothing.
    pub live_while: Option<fn(&Views) -> bool>,
    /// A read before the topics, given the query string, whose answer the
    /// topics then show (SIM Services: `GET /api/stk/menu` opens the
    /// toolkit session the `stk` topic reads). Read-only, as every read.
    pub before: Option<fn(&AppState, Option<&str>)>,
    /// The signal cadence its stream asks the hub for (`None`: the default
    /// 2 s; the fastest stream's wins).
    pub signal_every: Option<std::time::Duration>,
    /// A nav bar heading from what the page read (DNS Cache's "(N)"); the
    /// `<title>` stays `title`.
    pub heading: Option<fn(&PageData) -> String>,
}

impl Screen {
    /// Defaults for the tables (`..Screen::BLANK`).
    pub const BLANK: Screen = Screen {
        path: "",
        id: "",
        title: "",
        tab: Tab::Dashboard,
        bar: Bar::Root,
        topics: &[],
        reads: &[],
        live: &[],
        history: false,
        saved: &[],
        saved_ok: false,
        blocks: &[],
        arg: None,
        search: Search::No,
        status: None,
        refresh: None,
        own_notice: false,
        notice_view: None,
        live_while: None,
        before: None,
        signal_every: None,
        heading: None,
    };

    /// The gate lane of its page task: Slow if any read is a slow route
    /// (a page must not hold a General permit for a long call).
    pub fn lane(&self) -> Lane {
        let slow = self.reads.iter().any(|r| matches!(r, Read::Api(path) if server::lane(Method::Get, path) == Lane::Slow));
        if slow {
            Lane::Slow
        } else {
            Lane::General
        }
    }
}

/// A message above the content: a change was saved (a fixed text), an
/// action's result built from the device's answer (and whether it reports
/// success), or a failure.
pub enum Notice {
    Saved(&'static str),
    Flash(Option<bool>, String),
    Failed(String),
}

/// A form sent back as typed, with a message per invalid field.
#[derive(Default, Debug)]
pub struct FormState {
    pub values: Vec<(&'static str, String)>,
    pub errors: Vec<(&'static str, String)>,
}

impl FormState {
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    pub fn error(&self, name: &str) -> Option<&str> {
        self.errors.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }
}

/// What a block renders from.
pub struct PageData<'a> {
    pub app: &'a AppState,
    pub p: &'a Principal,
    pub views: &'a Views,
    /// `Screen::reads`, in order.
    pub reads: &'a [Result<Value, String>],
    /// Connected over Tailscale (`session::is_remote`).
    pub remote: bool,
    pub form: Option<&'a FormState>,
    /// The page's query string.
    pub query: Option<&'a str>,
    /// Where a form's single-use token is issued (`Ctx::once`).
    pub once: Option<&'a Once>,
    /// The AT Terminal's history (`Ctx::at`).
    pub at: Option<&'a AtLog>,
    /// USSD answers (`Ctx::ussd`).
    pub ussd: Option<&'a UssdLog>,
    /// The notice, on a screen that draws it itself (`Screen::own_notice`).
    pub notice: Option<Markup>,
    /// The page's single-use token, once issued (`PageData::once`).
    pub token: std::cell::OnceCell<String>,
    /// This browser's preferences (Settings).
    pub prefs: Prefs,
    /// The request came over HTTPS (`httpd::tls`).
    pub secure: bool,
}

impl PageData<'_> {
    /// The CSRF field for a POST form (never inside a live fragment).
    pub fn csrf(&self) -> Markup {
        render::csrf_field(&self.app.auth, self.p)
    }

    /// Read `i`, if it worked.
    pub fn read(&self, i: usize) -> Option<&Value> {
        self.reads.get(i)?.as_ref().ok()
    }

    /// The first value of `key` in the query string.
    pub fn param(&self, key: &str) -> Option<String> {
        query_param(self.query, key)
    }

    /// The page's single-use token, shared by all its forms (each post
    /// leads to a new page): issued on the first call ("" without a store).
    /// A page that reloads itself (`Screen::refresh`, `?t=`) keeps its
    /// token while it is unused, so a reload loop issues no new ones.
    pub fn once(&self) -> String {
        self.token
            .get_or_init(|| {
                let Some(o) = self.once else { return String::new() };
                let now = self.app.auth.now();
                match self.param("t").filter(|t| super::ops::is_op_id(t)) {
                    Some(t) if o.get(&t, self.p.id, now) == Some(super::store::Token::Fresh) => t,
                    _ => o.fresh(&self.app.auth, self.p.id),
                }
            })
            .clone()
    }
}

/// What a page needs from its request inside the gate task.
pub struct Req {
    pub nonce: String,
    pub query: Option<String>,
    pub remote: bool,
    /// The sessions' dismissals (`Ctx::dismissed`).
    pub dismissed: Arc<Dismissed>,
    /// Single-use form tokens (`Ctx::once`).
    pub once: Arc<Once>,
    /// The AT Terminal's history (`Ctx::at`).
    pub at: Arc<AtLog>,
    /// USSD answers (`Ctx::ussd`).
    pub ussd: Arc<UssdLog>,
    /// eSIM profile names (`Ctx::esim_names`).
    pub esim_names: Arc<crate::web::pages::esim::Names>,
    /// Values posted to the screen itself (the SMS search), never from the
    /// URL.
    pub typed: Option<FormState>,
    /// This browser's preferences (Settings).
    pub prefs: Prefs,
    /// Over HTTPS (`httpd::tls`).
    pub secure: bool,
}

impl Req {
    pub fn of(ctx: &Ctx, req: &Request) -> Req {
        Req {
            nonce: CspNonce::of(req),
            query: req.uri().query().map(str::to_owned),
            remote: session::is_remote(req.extensions().get::<Peer>()),
            dismissed: Arc::clone(&ctx.dismissed),
            once: Arc::clone(&ctx.once),
            at: Arc::clone(&ctx.at),
            ussd: Arc::clone(&ctx.ussd),
            esim_names: Arc::clone(&ctx.esim_names),
            typed: None,
            prefs: Prefs::of(req.headers()),
            secure: super::pages::https::is_secure(req),
        }
    }
}

/// `GET` of a screen. A screen that reads nothing from the device renders
/// at once; any other in one gate task.
pub async fn get(ctx: &Ctx, p: Principal, r: Req, s: &'static Screen) -> Response {
    let saved = query_param(r.query.as_deref(), "saved");
    let notice = s.saved.iter().find(|(k, _)| Some(*k) == saved.as_deref()).map(|(_, msg)| Notice::Saved(msg));
    // `?r=<id>`: an action's result, kept for this session only.
    let flash = || {
        let id = query_param(r.query.as_deref(), "r")?;
        ctx.flash.get(&id, p.id, ctx.app.auth.now()).map(|(ok, text)| Notice::Flash(ok, text))
    };
    let notice = notice.or_else(flash);
    if s.topics.is_empty() && s.reads.is_empty() && s.before.is_none() {
        return render(&ctx.app, s, &p, &r, notice, r.typed.as_ref(), 200);
    }
    let nonce = r.nonce.clone();
    job::run(ctx, s.lane(), &nonce, s.path, Box::new(move |app| render(app, s, &p, &r, notice, r.typed.as_ref(), 200))).await
}

/// Read and render `s` (blocking when it reads the device). Read-only:
/// debug builds panic if a read or block calls a mutating API route.
pub fn render(
    app: &AppState,
    s: &Screen,
    p: &Principal,
    r: &Req,
    notice: Option<Notice>,
    form: Option<&FormState>,
    status: u16,
) -> Response {
    gate::read_only(|| build(app, s, p, r, notice, form, status))
}

/// A notice as a line above the content.
fn notice_markup(s: &Screen, n: Notice) -> Markup {
    match n {
        Notice::Saved(msg) if s.saved_ok => html! { p.msg.ok role="status" { (msg) } },
        Notice::Saved(msg) => kit::notice(true, msg),
        // The app's result lines are green.
        Notice::Flash(Some(true), msg) => html! { p.msg.ok role="status" { (msg) } },
        Notice::Flash(ok, msg) => kit::notice(ok.is_none(), &msg),
        Notice::Failed(e) => kit::notice(false, &format!("Failed: {}", super::fmt::truncate(&e, 120))),
    }
}

fn build(
    app: &AppState,
    s: &Screen,
    p: &Principal,
    r: &Req,
    notice: Option<Notice>,
    form: Option<&FormState>,
    status: u16,
) -> Response {
    if let Some(f) = s.before {
        f(app, r.query.as_deref());
    }
    let mut views = super::pages::read_topics(app, s.topics);
    if s.history {
        views.history = app.hub.history();
    }
    if s.topics.contains(&Topic::Usb) {
        views.usb_dismissed = super::pages::usb::dismissed(&r.dismissed, p.id, &views);
    }
    if s.topics.contains(&Topic::EsimSwitch) {
        views.esim_names = Some(Arc::clone(&r.esim_names));
    }
    if let Some(arg) = s.arg {
        views.arg = arg(&views, r.query.as_deref());
    }
    let reads: Vec<_> = s.reads.iter().map(|rd| rd.run(app, r.query.as_deref())).collect();
    let quiet = s.live_while.is_some_and(|f| !f(&views));
    let live = match (s.live.is_empty() || quiet || (s.search == Search::Still && r.typed.is_some()), s.arg) {
        (true, _) => None,
        (false, None) => live::url(&app.auth, &format!("/ui/live/{}", s.id)),
        (false, Some(_)) => live::url(&app.auth, &format!("/ui/live/{}/{:x}", s.id, views.arg)),
    };
    let notice = notice.map(|n| match s.notice_view {
        Some(view) => view(n),
        None => notice_markup(s, n),
    });
    let (notice, own) = if s.own_notice { (None, notice) } else { (notice, None) };
    let pd = PageData {
        app,
        p,
        views: &views,
        reads: &reads,
        remote: r.remote,
        form,
        query: r.query.as_deref(),
        once: Some(&r.once),
        at: Some(&r.at),
        ussd: Some(&r.ussd),
        notice: own,
        token: std::cell::OnceCell::new(),
        prefs: r.prefs,
        secure: r.secure,
    };
    let (lead, trail) = match s.bar {
        Bar::Root => (html! {}, html! {}),
        Bar::Back(href, label) => (kit::back(href, label), html! {}),
        Bar::BackLink(href, label, to, text) => (kit::back(href, label), html! { a.nl href=(to) { (text) } }),
        Bar::Done(href) => (html! {}, kit::done(href)),
        Bar::Cancel(href) => (html! { a.nl href=(href) { "Cancel" } }, html! {}),
        Bar::AgeDone(age, href) => (age(&views), kit::done(href)),
        Bar::DoneTrail(href, trail) => (kit::done(href), trail(&views)),
        Bar::Status(trail) => (render::status_pill(s.path), html! { (trail(&views)) (render::snapshot_age()) }),
        Bar::Icons(lead, trail) => (icons(lead), icons(trail)),
        Bar::BackIcons(href, label, trail) => (kit::back(href, label), icons(trail)),
        Bar::DoneAs(link) => (html! {}, link(&views)),
    };
    let body = html! {
        // Where an ended stream says why (the pill only says "Offline").
        @if matches!(s.bar, Bar::Status(_)) { p #live-note {} }
        @if let Some(n) = notice { (n) }
        @for b in s.blocks {
            @match b {
                Block::Links(title, items) => (kit::section(Some(title), None, html! {
                    @for it in *items {
                        li { a.it href=(it.href) { (kit::icon(it.icon)) span { (it.label) @if !it.sub.is_empty() { small { (it.sub) } } } } }
                    }
                })),
                // Centred in the screen, as the app's placeholders.
                Block::Empty(i, title, text, note) => div.vc {
                    (kit::empty_state(*i, title, text))
                    @if !note.is_empty() { p.cap.ctr { (note) } }
                },
                Block::Rows(title, rows) => (kit::section(Some(title), None, html! {
                    @for (label, value) in *rows { (kit::kv_row(None, label, value)) }
                })),
                Block::Custom(f) => (f(&pd)),
            }
        }
    };
    let status = match s.status {
        Some(f) if status == 200 => f(&pd),
        _ => status,
    };
    let refresh = s.refresh.and_then(|f| f(&pd));
    let heading = s.heading.map(|f| html! { h1 { (f(&pd)) } });
    let ctx = PageCtx {
        title: s.title,
        tab: Some(s.tab),
        nonce: &r.nonce,
        live: live.as_deref(),
        refresh: refresh.is_some(),
        refresh_to: refresh.as_deref(),
        lead,
        trail,
        heading,
        go: None,
    };
    render::html(status, render::page(&ctx, body))
}

/// `s` with `notice` (and the typed values in `r`), drawn in a gate task
/// of its lane: the busy page if none is free.
pub async fn show(ctx: &Ctx, p: Principal, r: Req, s: &'static Screen, notice: Option<Notice>, status: u16) -> Response {
    let nonce = r.nonce.clone();
    let job: job::Job = Box::new(move |app: &AppState| render(app, s, &p, &r, notice, r.typed.as_ref(), status));
    if s.topics.is_empty() && s.reads.is_empty() && s.before.is_none() {
        return job(&ctx.app);
    }
    job::run(ctx, s.lane(), &nonce, s.path, job).await
}

/// `POST` of a searched screen (`Screen::search`): a session (else the
/// login), the form, origin and CSRF token; then the screen of the path
/// and query string filtered by the posted `q` (trimmed, at most 64
/// characters). Its links, stream and `next` carry no search.
pub async fn search(ctx: Ctx, req: Request, s: &'static Screen) -> Response {
    let mut r = Req::of(&ctx, &req);
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&parts.method, s.path, None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &r.nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&r.nonce, s.path);
    }
    let q: String = form.get("q").unwrap_or("").trim().chars().take(64).collect();
    r.typed = Some(FormState { values: vec![("q", q)], errors: Vec::new() });
    get(&ctx, p, r, s).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page waits in the lane of its slowest read: a slow route must not
    /// hold a General permit.
    #[test]
    fn a_page_takes_the_lane_of_its_slowest_read() {
        let slow = Screen { reads: &[Read::Api("/api/device/system"), Read::Api("/api/esim/profiles")], ..Screen::BLANK };
        assert_eq!(slow.lane(), Lane::Slow);
        let general = Screen { reads: &[Read::Api("/api/device/system"), Read::Fn(|_, _| Ok(Value::Null))], ..Screen::BLANK };
        assert_eq!(general.lane(), Lane::General);
        // Only the speed test's server list (speedtest.net) and the eSIM
        // pages' card reads are slow.
        for s in super::super::routes::SCREENS {
            let slow = s.path == "/ui/tools/speedtest" || (s.path.starts_with("/ui/router/esim") && s.path != "/ui/router/esim/add");
            let want = if slow { Lane::Slow } else { Lane::General };
            assert_eq!(s.lane(), want, "{}", s.path);
        }
    }
}
