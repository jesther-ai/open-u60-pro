//! The page shell (head, nav bar, tab bar), live fragments, form helpers,
//! the live-status line and error pages. Maud escapes every interpolated
//! value.

use axum::body::Body;
use axum::http::header::{HeaderValue, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::Response;
use maud::{html, Markup, DOCTYPE};

use serde_json::Value;

use super::assets::{APP_CSS, DATASTAR_JS, ICON_180, ICON_SVG, THEME_DARK_CSS, THEME_JS, THEME_LIGHT_CSS};
use super::fmt;
use super::kit::{self, Tab};
use super::ops::Op;
use super::prefs::Theme;
use crate::auth::{AuthState, Principal};
use crate::boottime::BootInstant;
use crate::events::history::HistPoint;
use crate::events::hub::SlotView;
use crate::events::Topic;

/// What a page or a live fragment is rendered from: the hub's view of each
/// topic it shows, and the time. A fragment renders from this alone, so a
/// live patch equals the same fragment in a reloaded page.
pub struct Views {
    pub now: BootInstant,
    slots: Vec<(Topic, SlotView)>,
    /// The signal readings of the last minutes (signal page chart), oldest first.
    pub history: Vec<HistPoint>,
    /// The operation an operation page follows.
    pub op: Option<Op>,
    /// The session dismissed the USB-C card for the cable attached now
    /// (`pages::usb`): set by the dashboard's page load and stream from
    /// the session's flag, never by a fragment.
    pub usb_dismissed: bool,
    /// The page's `Screen::arg` as at page load (also in its stream URL).
    pub arg: u64,
    /// The eSIM profile names last read (`Ctx::esim_names`), for pages and
    /// streams with `esim_switch`.
    pub esim_names: Option<std::sync::Arc<crate::web::pages::esim::Names>>,
    /// A live stream's numbers as last drawn (`roll::roll`); `None` on a
    /// page load, which draws plain numbers.
    pub roll: Option<std::cell::RefCell<super::roll::Rolls>>,
}

impl Views {
    pub fn new(now: BootInstant, slots: Vec<(Topic, SlotView)>) -> Self {
        Views { now, slots, history: Vec::new(), op: None, usb_dismissed: false, arg: 0, esim_names: None, roll: None }
    }

    pub fn slot(&self, t: Topic) -> Option<&SlotView> {
        self.slots.iter().find(|(topic, _)| *topic == t).map(|(_, s)| s)
    }

    /// Replace (or add) a topic's view.
    pub fn set(&mut self, t: Topic, view: SlotView) {
        match self.slots.iter_mut().find(|(topic, _)| *topic == t) {
            Some(slot) => slot.1 = view,
            None => self.slots.push((t, view)),
        }
    }

    #[cfg(test)]
    pub fn slots(&self) -> Vec<(Topic, SlotView)> {
        self.slots.clone()
    }

    /// Newer read times for a topic whose value did not change.
    pub fn set_at(&mut self, t: Topic, at: BootInstant, value_at: BootInstant) {
        if let Some((_, s)) = self.slots.iter_mut().find(|(topic, _)| *topic == t) {
            s.at = at;
            s.value_at = value_at;
        }
    }

    /// The topic's last good value.
    pub fn value(&self, t: Topic) -> Option<&Value> {
        self.slot(t)?.value.as_deref()
    }
}

/// A part of a page that live updates replace by id: which topics it is
/// rendered from and how.
#[derive(Copy, Clone)]
pub struct Fragment {
    pub id: &'static str,
    pub topics: &'static [Topic],
    pub render: fn(&Views) -> Markup,
}

/// The fragment `id` of `fragments`, rendered (nothing if there is none).
pub fn fragment(fragments: &[Fragment], id: &str, v: &Views) -> Markup {
    fragments.iter().find(|f| f.id == id).map(|f| (f.render)(v)).unwrap_or_default()
}

/// The first read error of `topics` under a card (a web addition: the app
/// shows none), else nothing.
pub fn error_line(v: &Views, topics: &[Topic]) -> Markup {
    match topics.iter().find_map(|&t| v.slot(t)?.error.as_deref()) {
        Some(err) => html! { p.age.err { "Couldn't update: " (fmt::truncate(err, 120)) } },
        None => html! {},
    }
}

/// A sheet's age line: the first read error of `topics`, or how old the
/// data is once a sampled topic is 3 cadences behind (the seconds below
/// 10, then in 5 s steps); else nothing.
pub fn age_line(v: &Views, topics: &[Topic]) -> Markup {
    let slots = || topics.iter().filter_map(|&t| Some((t, v.slot(t)?)));
    if slots().any(|(_, s)| s.error.is_some()) {
        return error_line(v, topics);
    }
    let stale = slots()
        .filter(|(_, s)| s.value.is_some())
        .filter_map(|(t, s)| {
            let age = v.now.saturating_duration_since(s.at);
            (age >= 3 * t.spec().cadence?).then_some(age.as_secs())
        })
        .max();
    match stale {
        Some(secs) if secs < 10 => html! { p.age { "Updated " (secs) " s ago" } },
        Some(secs) if secs < 120 => html! { p.age { "Updated " (secs / 5 * 5) " s ago" } },
        Some(secs) => html! { p.age { "Updated " (secs / 60) " min ago" } },
        None => html! {},
    }
}

#[derive(Default)]
pub struct PageCtx<'a> {
    /// The `<title>` (with " · U60 Pro"), and the nav bar's heading when
    /// signed in. Never contains an identifier.
    pub title: &'a str,
    /// Signed in: the nav bar and the tab bar with this tab current.
    /// Signed out (login, errors): a plain bar, no tabs.
    pub tab: Option<Tab>,
    /// This response's CSP nonce.
    pub nonce: &'a str,
    /// Live pages: the Datastar stream URL (a server constant and this
    /// page load's random key, `live::url`).
    pub live: Option<&'a str>,
    /// Without JavaScript, reload every 3 s (a running operation, a speed
    /// test), to `refresh_to` if given.
    pub refresh: bool,
    pub refresh_to: Option<&'a str>,
    /// Open this page of the UI at once, with or without JavaScript (the
    /// marker hop, `hop_page`; a `session::without_session` target only).
    pub go: Option<&'a str>,
    /// The nav bar's leading and trailing items.
    pub lead: Markup,
    pub trail: Markup,
    /// A heading other than `title` (the operation page's live `#op-title`);
    /// signed out, what replaces the brand.
    pub heading: Option<Markup>,
}

/// A live stream URL: `/ui/live/` and then only `[a-z0-9/]`. It is put
/// inside a JavaScript string in `data-init`, where HTML escaping does not
/// help, so nothing else may ever get there.
fn is_live_url(url: &str) -> bool {
    url.strip_prefix("/ui/live/")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'/')))
}

/// The `data-init` expression of a live page.
fn live_init(url: &str) -> String {
    format!(
        "document.hidden ? document.addEventListener('visibilitychange', () => @get('{url}'), {{once: true}}) : @get('{url}')"
    )
}

pub fn page(ctx: &PageCtx, body: Markup) -> Markup {
    debug_assert!(ctx.live.is_none_or(is_live_url), "not a live stream URL: {:?}", ctx.live);
    // A page without a valid stream URL is the plain snapshot. Only live
    // pages carry the nonce in the markup (Datastar reads and removes it).
    let live = ctx.live.filter(|url| is_live_url(url));
    html! {
        (DOCTYPE)
        html lang="en" data-nonce=[live.map(|_| ctx.nonce)] {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover";
                meta name="apple-mobile-web-app-capable" content="yes";
                meta name="mobile-web-app-capable" content="yes";
                meta name="apple-mobile-web-app-title" content="U60 Pro";
                title { (ctx.title) " · U60 Pro" }
                link rel="stylesheet" href=(APP_CSS);
                // The system's scheme; a browser's theme replaces these
                // tags (`prefs::themed`).
                (scheme_tags(Theme::System, ctx.nonce))
                link rel="manifest" href="/ui/manifest.webmanifest";
                link rel="icon" type="image/svg+xml" href=(ICON_SVG);
                link rel="apple-touch-icon" href=(ICON_180);
                @if ctx.refresh {
                    noscript { meta http-equiv="refresh" content=(ctx.refresh_to.map_or_else(|| "3".to_owned(), |u| format!("3; url={u}"))); }
                }
                @if let Some(url) = ctx.go {
                    meta http-equiv="refresh" content=(format!("0; url={url}"));
                }
                @if live.is_some() {
                    script type="module" src=(DATASTAR_JS) nonce=(ctx.nonce) {}
                }
            }
            body {
                a.skip href="#main" { "Skip to content" }
                @if ctx.tab.is_some() {
                    header.bar {
                        div.lead { (ctx.lead) }
                        @match &ctx.heading { Some(h) => (h), None => h1 { (ctx.title) } }
                        div.trail { (ctx.trail) }
                    }
                } @else {
                    // Signed out: the brand, or the page's own (login: none,
                    // as the app's login has no title over its header).
                    header.bar { @match &ctx.heading { Some(h) => (h), None => span.brand { "U60 Pro" } } }
                }
                // Opened once the page is visible: Datastar itself only
                // reacts to visibility changes, so a page loaded hidden (a
                // background tab) would otherwise stream to nobody.
                @if let Some(url) = live {
                    div #live hidden data-init=(live_init(url)) {}
                }
                main #main { (body) }
                // After the content in the DOM; CSS places it.
                @if let Some(tab) = ctx.tab { (kit::tab_bar(tab)) }
            }
        }
    }
}

/// The colour-scheme tags of the page shell: for the system's scheme both
/// (and the nav bar colour of each) and the small script that makes Safari
/// take the bar colours again when the system's scheme changes with the
/// page open (`assets/theme.js`, owner decision 60; it does nothing until
/// then, and the page is the same without it), else the theme's, with its
/// stylesheet after `app.css` (Settings › Appearance). What each browser
/// colours from them (and from the page's background, which Safari 26
/// samples): docs/web-ui.md, "Theme and the browser's own colours".
fn scheme_tags(theme: Theme, nonce: &str) -> Markup {
    let one = |scheme: &str, color: &str, css: &str| {
        html! {
            meta name="color-scheme" content=(scheme);
            meta name="theme-color" content=(color);
            link rel="stylesheet" href=(css);
        }
    };
    match theme {
        Theme::System => html! {
            meta name="color-scheme" content="light dark";
            meta name="theme-color" media="(prefers-color-scheme: light)" content="#f2f2f7";
            meta name="theme-color" media="(prefers-color-scheme: dark)" content="#000000";
            script src=(THEME_JS) nonce=(nonce) defer {}
        },
        Theme::Light => one("light", "#f2f2f7", THEME_LIGHT_CSS),
        Theme::Dark => one("dark", "#000000", THEME_DARK_CSS),
    }
}

/// `scheme_tags` as a page with this CSP nonce has them.
pub fn scheme(theme: Theme, nonce: &str) -> String {
    scheme_tags(theme, nonce).into_string()
}

/// The hidden CSRF field; always the first field of a POST form.
pub fn csrf_field(auth: &AuthState, p: &Principal) -> Markup {
    html! { input type="hidden" name="csrf" value=(auth.csrf_token(p)); }
}

/// A form's single-use token (`store::Once`); `autocomplete="off"`, so the
/// browser does not restore it into an old copy of the form.
pub fn once_field(token: &str) -> Markup {
    html! { input type="hidden" name="once" value=(token) autocomplete="off"; }
}

/// `#live-status` as rendered with the page: a snapshot whose age labels
/// appear after 1 and 5 minutes through CSS alone, and a Refresh link.
pub fn live_snapshot(refresh: &str) -> Markup {
    html! {
        p #live-status .snap {
            "Snapshot from page load"
            span.ages {
                span.age1 { " · over 1 min old" }
                span.age5 { " · over 5 min old" }
            }
            " · "
            a href=(refresh) { "Refresh" }
        }
    }
}

/// `#live-status` as the dashboard's nav bar pill, without JavaScript: a
/// compact "Snapshot" capsule that reloads the page (its age is
/// `snapshot_age`, in the age badge's place).
pub fn status_pill(refresh: &str) -> Markup {
    html! {
        p #live-status .snap .ps {
            a href=(refresh) { "Snapshot" span.sr { " from page load. Refresh" } }
        }
    }
}

/// Beside the age badge, which is hidden in a snapshot (it would never
/// move): how old the snapshot is, after 1 and 5 minutes, by CSS alone;
/// hidden once live.
pub fn snapshot_age() -> Markup {
    html! { span.agebadge.sa.ages { span.age1 { "over 1 min old" } span.age5 { "over 5 min old" } } }
}

/// A full page with a heading, a message and a way back.
pub fn error_page(title: &str, msg: &str, nonce: &str, retry: Option<&str>) -> Markup {
    let body = html! {
        div.narrow {
            h1 { (title) }
            p { (msg) }
            p.actions {
                @if let Some(href) = retry {
                    a.btn href=(href) { "Try again" }
                }
                @if retry != Some("/ui/") {
                    a.btn.btn-quiet[retry.is_some()] href="/ui/" { "Back to dashboard" }
                }
            }
        }
    };
    page(&PageCtx { title, nonce, ..PageCtx::default() }, body)
}

/// The marker hop (`session::without_session`, owner decision 61): this
/// page opens `target` (the request's own page, flagged) at once. It is a
/// navigation the page starts itself, so the browser counts it as
/// same-site and sends the `SameSite=Strict` session cookie that a launch
/// from outside the site (an installed app, another app's link) left
/// behind. A link for a browser that does not follow the refresh.
pub fn hop_page(target: &str, nonce: &str) -> Response {
    let body = html! {
        div.narrow.vc {
            p.actions.ctr { a.btn href=(target) { "Continue" } }
        }
    };
    html(200, page(&PageCtx { title: "Opening", nonce, go: Some(target), ..PageCtx::default() }, body))
}

/// `text/html` with `status` (the header middleware adds the security headers).
pub fn html(status: u16, markup: Markup) -> Response {
    let mut resp = Response::new(Body::from(markup.into_string()));
    *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    resp
}

pub fn error(status: u16, title: &str, msg: &str, nonce: &str) -> Response {
    html(status, error_page(title, msg, nonce, None))
}

pub fn bad_request(nonce: &str) -> Response {
    error(400, "Bad request", "The request could not be understood.", nonce)
}

pub fn not_found(nonce: &str) -> Response {
    error(404, "Page not found", "There is no page at this address.", nonce)
}

/// A post that failed the origin or CSRF check; `form` is the page that
/// shows the form again (a standalone window has no Back button).
pub fn form_refused(nonce: &str, form: &str) -> Response {
    let msg = "This form expired or came from another site. Open it again and retry.";
    html(403, error_page("Form not accepted", msg, nonce, Some(form)))
}

/// No lane permit in time: the device is busy with other calls.
pub fn busy(nonce: &str, retry: &str) -> Response {
    let msg = "The router is busy. Try again in a few seconds.";
    html(503, error_page("Router busy", msg, nonce, Some(retry)))
}

pub fn internal(nonce: &str) -> Response {
    error(500, "Something went wrong", "The page could not be built. Try again.", nonce)
}

/// Every `/ui` page while the agent has no password.
pub fn no_password(nonce: &str) -> Response {
    let body = html! {
        div.narrow {
            h1 { "Set an agent password" }
            p {
                "The web interface needs an agent password. Add "
                code { "ZTE_AGENT_PASSWORD=<password>" }
                " to the agent's start script on the router, then restart the agent."
            }
            // A standalone window has no reload button.
            p.actions { a.btn href="/ui/" { "Try again" } }
        }
    };
    html(403, page(&PageCtx { title: "Set an agent password", nonce, ..PageCtx::default() }, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_page_carries_the_nonce_in_three_places() {
        let ctx = PageCtx { title: "T", nonce: "00ff", live: Some("/ui/live/x"), ..PageCtx::default() };
        let s = page(&ctx, html! { p { "x" } }).into_string();
        assert!(s.starts_with("<!DOCTYPE html><html lang=\"en\" data-nonce=\"00ff\">"), "{s}");
        assert!(s.contains(&format!(r#"<script type="module" src="{DATASTAR_JS}" nonce="00ff"></script>"#)));
        // Opened only once the page is visible (Datastar itself does not
        // check at the start); the URL is the only value in the expression.
        assert!(s.contains(
            "<div id=\"live\" hidden data-init=\"document.hidden ? document.addEventListener('visibilitychange', \
             () =&gt; @get('/ui/live/x'), {once: true}) : @get('/ui/live/x')\"></div>"
        ), "{s}");
        // Signed out: no tab bar, no heading in the bar.
        assert!(!s.contains("<nav") && !s.contains("<h1"));
        // Signed in: the title is the nav bar's heading, the tab bar follows `main`.
        let tabs = PageCtx { title: "T", nonce: "00ff", tab: Some(Tab::Router), ..PageCtx::default() };
        let s = page(&tabs, html! {}).into_string();
        assert!(s.contains("<header class=\"bar\"><div class=\"lead\"></div><h1>T</h1><div class=\"trail\"></div></header>"), "{s}");
        assert!(s.contains("</main><nav class=\"tabs\" aria-label=\"Tabs\">"));
        assert!(s.contains("<a href=\"/ui/router\" aria-current=\"page\">"));
        assert_eq!(s.matches("aria-current").count(), 1);
        // Without live updates: only the System theme's script, which carries
        // the nonce; no Datastar, no `data-nonce`, no `data-init`.
        let ctx = PageCtx { live: None, ..ctx };
        let s = page(&ctx, html! {}).into_string();
        assert_eq!(s.matches("<script").count(), 1, "{s}");
        assert!(s.contains(&format!(r#"<script src="{THEME_JS}" nonce="00ff" defer></script>"#)), "{s}");
        assert!(!s.contains("datastar") && !s.contains("data-init") && !s.contains("data-nonce") && s.matches("00ff").count() == 1, "{s}");
        assert!(s.starts_with("<!DOCTYPE html><html lang=\"en\">"));
    }

    #[test]
    fn live_urls_are_server_constants_only() {
        for ok in ["/ui/live/dashboard", "/ui/live/signal", "/ui/live/op/0123456789abcdef"] {
            assert!(is_live_url(ok), "{ok}");
        }
        for bad in ["/ui/live/", "/ui/live/x')+alert(1)+('", "/ui/live/X", "/ui/live/a b", "/ui/signal", "/ui/live/\\x"] {
            assert!(!is_live_url(bad), "{bad}");
        }
    }

    #[test]
    fn error_pages_offer_a_way_on() {
        let s = error_page("Router busy", "m", "00", Some("/ui/")).into_string();
        assert_eq!(s.matches("href=\"/ui/\"").count(), 1, "one link per target: {s}");
        let s = error_page("Form not accepted", "m", "00", Some("/ui/login")).into_string();
        assert!(s.contains("href=\"/ui/login\"") && s.contains("Back to dashboard"));
    }

    #[test]
    fn snapshot_status_line() {
        assert_eq!(
            live_snapshot("/ui/").into_string(),
            "<p class=\"snap\" id=\"live-status\">Snapshot from page load<span class=\"ages\">\
             <span class=\"age1\"> · over 1 min old</span><span class=\"age5\"> · over 5 min old</span></span> · \
             <a href=\"/ui/\">Refresh</a></p>"
        );
    }
}
