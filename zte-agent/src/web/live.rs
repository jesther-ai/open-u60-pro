//! Live updates for pages opened with JavaScript: `GET /ui/live/<screen id>`
//! (every screen with live fragments, `routes::SCREENS`) and
//! `/ui/live/op/<id>` are Datastar SSE streams of `datastar-patch-elements`
//! frames that replace the page's fragments by id.
//!
//! A stream opens only from the page's single `data-init`, once the page is
//! visible (Datastar closes it while the page is hidden and opens a new one
//! when it is shown again); it subscribes to the page's topics in the hub,
//! so the sampler runs only while some page is watching. Each page load has
//! its own stream URL (`…/<page key>`): a reconnect replaces its own earlier
//! stream at once (which may be dead but unnoticed), never another tab's.
//! Every stream starts with the full set of fragments (a reconnect is a
//! fresh snapshot, never a replay), then sends only fragments whose HTML
//! changed, a heartbeat every 5 s for the CSS watchdog, and ends with a
//! final status line (signed out, replaced, standby, 30 minutes), after
//! which Datastar does not reconnect. On the
//! dashboard the status is the nav bar's pill ("Connected", "Offline") and
//! an ending's text goes under the bar (`#live-note`).

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{HeaderValue, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::Response;
use maud::{html, Markup};

use super::pages::{op, usb};
use super::prefs::Prefs;
use super::render::{Fragment, Views};
use super::routes;
use super::screen::{Bar, Screen};
use super::session::{self, percent_encode};
use super::store::Dismissed;
use crate::auth::{AuthState, SessionId};
use crate::boottime::BootInstant;
use crate::events::hub::{Hub, Refused, SlotView, SubscribeReq, TopicAge};
use crate::events::sse::{self, ds_patch_elements, Frame};
use crate::events::stream::{drive, ByeReason, Emitter, StreamEnv};
use crate::events::{Topic, TopicSet};
use crate::httpd::Ctx;
use crate::web::ops::{is_op_id, Op};

const TICK: Duration = Duration::from_secs(5);
const FIRST_TICK: Duration = Duration::from_secs(1);
const MIN_GAP: Duration = Duration::from_millis(250);
const MAX_LIFE: Duration = Duration::from_secs(30 * 60);

/// What one page streams.
pub struct LivePage {
    /// The page itself (Refresh and Resume); empty: the page the link is
    /// on (`href=""`).
    pub page: String,
    /// The login's `next` after a sign-out (`None`: plain login).
    pub next: Option<&'static str>,
    pub topics: &'static [Topic],
    pub fragments: &'static [Fragment],
    /// The signal chart's history.
    pub history: bool,
    pub op: Option<Op>,
    /// The status is the nav bar's pill (`Bar::Status`).
    pub pill: bool,
    /// `Views::arg`, from the stream URL.
    pub arg: u64,
    /// `Screen::live_while`: the stream ends once this no longer holds.
    pub until: Option<fn(&Views) -> bool>,
    /// `Screen::signal_every` (`None`: the browser's refresh interval).
    pub signal_hint: Option<std::time::Duration>,
}

impl LivePage {
    /// The stream of a live screen. One with an argument streams to pages
    /// at several URLs (Messages with a filter or search, every
    /// conversation), so its links reload the page they are on.
    pub fn of(s: &'static Screen) -> LivePage {
        LivePage {
            page: if s.arg.is_some() { String::new() } else { s.path.into() },
            next: Some(s.path),
            topics: s.topics,
            fragments: s.live,
            history: s.history,
            op: None,
            pill: matches!(s.bar, Bar::Status(_)),
            arg: 0,
            until: s.live_while,
            signal_hint: s.signal_every,
        }
    }
}

/// `#live-status` while live: the heartbeat class `hb` changes with every
/// tick, which restarts the CSS animation that would reveal "No updates".
pub fn live_on(page: &str, hb: &str) -> Markup {
    html! {
        p #live-status .live .on .(hb) {
            span.ok { "Live" }
            span.lost { " No updates for 15 s \u{b7} " a href=(page) { "Refresh" } }
        }
    }
}

/// `#live-status` as the nav bar pill while live: "Connected" until 15 s
/// without a heartbeat, then "Offline", which reloads the page.
pub fn live_pill(page: &str, hb: &str) -> Markup {
    html! {
        p #live-status .live .on .ps .(hb) {
            span.ok { b { "Connected" } }
            span.lost { a href=(page) { "Offline" span.sr { ". Refresh" } } }
        }
    }
}

/// The patches of an ended stream (no `on`: the watchdog stays off): the
/// status line says why; with the pill, it says "Offline" and the reason
/// goes under the nav bar.
fn ending(pill: bool, msg: &str, link: Option<(&str, &str)>) -> Vec<String> {
    let note = html! {
        (msg)
        @if let Some((text, href)) = link { " " a href=(href) { (text) } }
    };
    if pill {
        vec![
            html! { p #live-status .live .ps { span.off { b { "Offline" } } } }.into_string(),
            html! { p #live-note { (note) } }.into_string(),
        ]
    } else {
        vec![html! { p #live-status .live { (note) } }.into_string()]
    }
}

fn login_href(next: Option<&str>) -> String {
    match next {
        Some(n) => format!("/ui/login?next={}", percent_encode(n)),
        None => "/ui/login".to_owned(),
    }
}

fn signed_out(pill: bool, next: Option<&str>) -> Vec<String> {
    ending(pill, "Signed out.", Some(("Log in", &login_href(next))))
}

/// One `datastar-patch-elements` frame for every element of a batch.
fn patch(parts: &[String]) -> Frame {
    ds_patch_elements(&parts.join("\n"))
}

fn digest(html: &str) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    html.hash(&mut h);
    h.finish()
}

/// Turns hub state into fragment patches for one page.
pub struct PageEmitter {
    hub: Arc<Hub>,
    page: LivePage,
    views: Views,
    /// Hash of the HTML last sent per fragment.
    sent: Vec<u64>,
    beat: bool,
    done: bool,
    /// A page with the `usb` topic: the dismissals and the stream's
    /// session, read into `views.usb_dismissed` before every render (a
    /// detached reading ends the session's dismissal, as a page load does).
    usb: Option<(Arc<Dismissed>, SessionId)>,
}

impl PageEmitter {
    pub fn new(hub: Arc<Hub>, page: LivePage, now: BootInstant) -> Self {
        let mut views = Views::new(now, Vec::new());
        views.op = page.op;
        views.arg = page.arg;
        // Numbers roll from the second time they are drawn (`roll`).
        views.roll = Some(Default::default());
        let sent = vec![0; page.fragments.len()];
        PageEmitter { hub, page, views, sent, beat: false, done: false, usb: None }
    }

    /// Fragments that depend on `changed` (all with `None`) and differ from
    /// what was sent.
    fn render(&mut self, changed: Option<&[Topic]>) -> Vec<String> {
        if let Some((store, owner)) = &self.usb {
            self.views.usb_dismissed = usb::dismissed(store, *owner, &self.views);
        }
        let mut out = Vec::new();
        for (i, f) in self.page.fragments.iter().enumerate() {
            if changed.is_some_and(|c| !f.topics.iter().any(|t| c.contains(t))) {
                continue;
            }
            let html = (f.render)(&self.views).into_string();
            let h = digest(&html);
            if self.sent[i] != h {
                self.sent[i] = h;
                out.push(html);
            }
        }
        out
    }

    fn take(&mut self, now: BootInstant, slots: Vec<(Topic, SlotView)>) -> Vec<Topic> {
        self.views.now = now;
        let topics: Vec<Topic> = slots.iter().map(|(t, _)| *t).collect();
        for (t, s) in slots {
            self.views.set(t, s);
        }
        topics
    }

    /// The batch, closed by the status line: the heartbeat (`hb`), or the
    /// end of an operation, after which the stream ends.
    fn batch(&mut self, mut parts: Vec<String>, hb: Option<&str>) -> Vec<Frame> {
        let over = self.page.until.is_some_and(|f| !f(&self.views));
        if over || op::status(&self.views).is_some_and(|s| s.finished()) {
            self.done = true;
            parts.push(html! { p #live-status .live {} }.into_string());
        } else if let Some(hb) = hb {
            let status = if self.page.pill { live_pill(&self.page.page, hb) } else { live_on(&self.page.page, hb) };
            parts.push(status.into_string());
        }
        if parts.is_empty() {
            Vec::new()
        } else {
            vec![patch(&parts)]
        }
    }
}

impl Emitter for PageEmitter {
    fn open(&mut self, snapshot: Vec<(Topic, SlotView)>, now: BootInstant) -> Vec<Frame> {
        self.take(now, snapshot);
        if self.page.history {
            self.views.history = self.hub.history();
        }
        let parts = self.render(None);
        self.batch(parts, Some("hb-o"))
    }

    fn changes(&mut self, changed: Vec<(Topic, SlotView)>, now: BootInstant) -> Vec<Frame> {
        let topics = self.take(now, changed);
        if self.page.history && topics.contains(&Topic::Signal) {
            self.views.history = self.hub.history();
        }
        let parts = self.render(Some(&topics));
        self.batch(parts, None)
    }

    /// The heartbeat, and every fragment whose age line (or, on an
    /// operation page, elapsed time and timeout) changed. Read times come
    /// from the hub: a re-read that found the same value is not a change.
    fn tick(&mut self, now: BootInstant, ages: &[TopicAge]) -> Vec<Frame> {
        self.views.now = now;
        for a in ages {
            self.views.set_at(a.topic, a.at, a.value_at);
        }
        let parts = self.render(None);
        self.beat = !self.beat;
        let hb = if self.beat { "hb-a" } else { "hb-b" };
        self.batch(parts, Some(hb))
    }

    fn bye(&mut self, reason: ByeReason) -> Vec<Frame> {
        let (page, pill) = (self.page.page.as_str(), self.page.pill);
        let parts = match reason {
            ByeReason::TokenExpired | ByeReason::LoggedOut => signed_out(pill, self.page.next),
            ByeReason::Lifetime => ending(pill, "Live updates paused after 30 minutes.", Some(("Resume", page))),
            ByeReason::Evicted => ending(pill, "Opened in another tab.", Some(("Refresh", page))),
            ByeReason::Standby => ending(pill, "The router went into standby.", Some(("Refresh", page))),
        };
        vec![patch(&parts)]
    }

    fn finished(&self) -> bool {
        self.done
    }
}

fn text(status: StatusCode, msg: &'static str) -> Response {
    let mut resp = Response::new(Body::from(msg));
    *resp.status_mut() = status;
    resp.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    resp
}

/// The stream URL of one page load: `base` and a random page key. `None`
/// without randomness (the page is then a snapshot).
pub fn url(auth: &AuthState, base: &str) -> Option<String> {
    auth.random_hex(8).ok().map(|key| format!("{base}/{key}"))
}

/// A page key: 16 lowercase hex characters (as an operation id).
fn page_key(s: &str) -> Option<u64> {
    is_op_id(s).then(|| u64::from_str_radix(s, 16).ok())?
}

/// A stream of one patch: Datastar applies it and, the stream having ended
/// cleanly, does not reconnect.
fn one(parts: Vec<String>) -> Response {
    sse::response(futures_util::stream::iter([Ok(patch(&parts))]))
}

/// An operation page whose operation is no longer kept (10 minutes; every
/// one has finished long before), or not this session's: the same answer,
/// which replaces a progress card left showing "Pending" while the page was
/// hidden (the nav bar keeps its way back).
fn op_gone() -> Response {
    one(vec![html! {
        section #op-progress .card {
            p.msg.info role="status" { "This change has finished." }
        }
        p #live-status .live {}
    }
    .into_string()])
}

/// The common steps: a Datastar request (else 400), a session (else the
/// signed-out patch), a place in the hub (else the busy patch), then the
/// stream, which replaces an earlier one of the same page load (`key`).
fn open(
    ctx: &Ctx,
    req: &Request,
    page: impl FnOnce(&Ctx, SessionId) -> Option<LivePage>,
    (next, pill): (Option<&'static str>, bool),
    key: u64,
) -> Response {
    if session_header(req) != Some("true") {
        return text(StatusCode::BAD_REQUEST, "Datastar request expected");
    }
    let Some(p) = session::find_session(&ctx.app.auth, req.headers()) else {
        return one(signed_out(pill, next));
    };
    let Some(page) = page(ctx, p.id) else {
        return op_gone();
    };
    let topics: TopicSet = page.topics.iter().copied().collect();
    let hub = Arc::clone(&ctx.app.hub);
    // The screen's own cadence, else this browser's refresh interval
    // (Settings › Polling); the hub uses it only with `signal`.
    let signal_hint = page.signal_hint.or_else(|| Prefs::of(req.headers()).signal_hint());
    let sub = match hub.subscribe(SubscribeReq { owner: p.id, topics, signal_hint, replace_same: Some(key) }) {
        Ok(sub) => sub,
        Err(Refused::Full) => {
            return one(ending(page.pill, "Live updates busy (too many open pages).", Some(("Refresh", &page.page))));
        }
    };
    let env = StreamEnv {
        auth: Arc::clone(&ctx.app.auth) as _,
        owner: p.id,
        tick: TICK,
        first_tick: FIRST_TICK,
        min_gap: MIN_GAP,
        max_life: Some(MAX_LIFE),
    };
    let usb = page.topics.contains(&Topic::Usb).then(|| (Arc::clone(&ctx.dismissed), p.id));
    let names = page.topics.contains(&Topic::EsimSwitch).then(|| Arc::clone(&ctx.esim_names));
    let mut em = PageEmitter::new(hub, page, ctx.app.auth.now());
    em.usb = usb;
    em.views.esim_names = names;
    sse::response(drive(env, sub, em))
}

fn session_header(req: &Request) -> Option<&str> {
    req.headers().get("datastar-request").and_then(|v| v.to_str().ok())
}

/// A screen's stream argument: 1 to 16 lowercase hex digits.
fn arg(s: &str) -> Option<u64> {
    let hex = (1..=16).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    hex.then(|| u64::from_str_radix(s, 16).ok())?
}

/// `GET /ui/live/{<screen id>[/<arg>],op/<id>}/<page key>`; `None` for any
/// other path (a screen with `Screen::arg` needs one, any other has none).
/// An operation's stream is only its owner's and ends with the operation.
pub fn stream(ctx: &Ctx, req: &Request) -> Option<Response> {
    let (path, key) = req.uri().path().rsplit_once('/')?;
    let key = page_key(key)?;
    let rest = path.strip_prefix("/ui/live/")?;
    let Some(id) = rest.strip_prefix("op/") else {
        let (id, n) = match rest.split_once('/') {
            Some((id, n)) => (id, Some(arg(n)?)),
            None => (rest, None),
        };
        let s = routes::live_screen(id).filter(|s| s.arg.is_some() == n.is_some())?;
        let pill = matches!(s.bar, Bar::Status(_));
        let page = move |_: &Ctx, _| Some(LivePage { arg: n.unwrap_or(0), ..LivePage::of(s) });
        return Some(open(ctx, req, page, (Some(s.path), pill), key));
    };
    let id = id.to_owned();
    let id = is_op_id(&id).then_some(id)?;
    let page = move |ctx: &Ctx, owner| {
        let op = ctx.ops.get(&id, owner, ctx.app.auth.now())?;
        Some(LivePage {
            page: format!("/ui/op/{id}"),
            next: None,
            topics: op.spec.topics,
            fragments: &op::FRAGMENTS,
            history: false,
            op: Some(op),
            pill: false,
            arg: 0,
            until: None,
            signal_hint: None,
        })
    };
    Some(open(ctx, req, page, (None, false), key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boottime::{BootClock, Clock};
    use crate::web::pages::dashboard::tests::fixture as dash_fixture;
    use crate::web::pages::{dashboard, signal};
    use crate::web::pages::signal::tests::fixture as signal_fixture;
    use serde_json::{json, Value};

    fn dash_page() -> LivePage {
        LivePage::of(&crate::web::tabs::dashboard::DASHBOARD)
    }

    fn signal_page(history: bool) -> LivePage {
        LivePage { history, ..LivePage::of(&crate::web::tabs::dashboard::SIGNAL) }
    }

    fn hub() -> Arc<Hub> {
        Arc::new(Hub::new(Arc::new(BootClock)))
    }

    /// The `elements` of one frame, one per patched element.
    fn elements(frames: &[Frame]) -> Vec<String> {
        assert!(frames.len() <= 1, "one frame per batch: {frames:?}");
        frames
            .iter()
            .flat_map(|f| {
                assert!(f.starts_with("event: datastar-patch-elements\n") && f.ends_with("\n\n"), "{f}");
                f.lines().skip(1).filter(|l| !l.is_empty()).map(|l| l.strip_prefix("data: elements ").unwrap().to_owned()).collect::<Vec<_>>()
            })
            .collect()
    }

    fn ids(parts: &[String]) -> Vec<String> {
        parts.iter().map(|p| p.split("id=\"").nth(1).unwrap().split('"').next().unwrap().to_owned()).collect()
    }

    #[test]
    fn open_sends_every_fragment_then_only_what_changed() {
        let v = dash_fixture();
        let mut em = PageEmitter::new(hub(), dash_page(), v.now);
        let open = elements(&em.open(v.slots(), v.now));
        let mut want: Vec<String> = dashboard::FRAGMENTS.iter().map(|f| f.id.to_owned()).collect();
        want.push("live-status".into());
        assert_eq!(ids(&open), want);
        assert!(open.last().unwrap().starts_with(
            "<p class=\"live on ps hb-o\" id=\"live-status\"><span class=\"ok\"><b>Connected</b></span>\
             <span class=\"lost\"><a href=\"/ui/\">Offline<span class=\"sr\">. Refresh</span></a></span></p>"
        ));
        // Byte-equal to the same fragment of a full page (the age badge is
        // the nav bar's).
        let page = dashboard::body(&v, Some(true), &html! {}).into_string() + &dashboard::age_badge(&v).into_string();
        for part in &open[..open.len() - 1] {
            assert!(page.contains(part.as_str()), "{part}");
        }
        // A new traffic reading: only its card.
        let t = SlotView { version: 9, at: v.now, value_at: v.now, value: Some(Arc::new(json!({"real_rx_speed": 1000}))), error: None };
        let parts = elements(&em.changes(vec![(Topic::Traffic, t.clone())], v.now));
        assert_eq!(ids(&parts), ["card-traffic"]);
        assert!(parts[0].contains("8.0 Kb/s"));
        // The same HTML again: nothing to send.
        assert!(em.changes(vec![(Topic::Traffic, t)], v.now).is_empty());
        // Airplane mode reaches the banners and the connection card.
        let lpm = SlotView { version: 10, at: v.now, value_at: v.now, value: Some(Arc::new(json!({"status": {"operate_mode": "LPM"}}))), error: None };
        assert_eq!(ids(&elements(&em.changes(vec![(Topic::Modem, lpm)], v.now))), ["banners", "card-conn"]);
    }

    #[test]
    fn ticks_beat_and_show_ages() {
        let v = dash_fixture();
        let mut em = PageEmitter::new(hub(), dash_page(), v.now);
        em.open(v.slots(), v.now);
        let later = v.now.saturating_add(Duration::from_secs(4));
        // The traffic topic was re-read just now (same value); the battery not.
        let ages = [TopicAge { topic: Topic::Traffic, at: later, value_at: later, error: false }];
        let parts = elements(&em.tick(later, &ages));
        // Battery (5 s cadence) is not 3 cadences old yet; traffic was
        // re-read; the signal reading is 4 s old now.
        assert_eq!(ids(&parts), ["age", "live-status"]);
        // The badge's digit rolls up from 0 (`roll`); its label is what
        // screen readers read.
        assert!(parts[0].contains("aria-label=\"Updated 4 seconds ago\"><span class=\"rl\" aria-hidden=\"true\"><i id=\"age04\" data-u=\"0\">4</i>s ago</span>"), "{}", parts[0]);
        assert!(parts[1].contains("class=\"live on ps hb-a\""));
        let much_later = v.now.saturating_add(Duration::from_secs(20));
        let parts = elements(&em.tick(much_later, &ages));
        // Traffic's caption ages from its last good sample and the
        // temperatures blank after 10 s; the battery card shows no age (the
        // app's cards have none), so it is not sent.
        assert_eq!(ids(&parts), ["card-traffic", "card-health", "age", "live-status"], "{parts:?}");
        assert!(parts[0].contains("Last traffic sample · 16s ago · waiting for device"));
        assert!(parts[1].contains("<dt>CPU</dt><dd>—</dd>") && !parts.iter().any(|p| p.contains("Updated 20 s ago")));
        assert!(parts.last().unwrap().contains("class=\"live on ps hb-b\""));
    }

    /// Every number of a topic value, moved (numbers and numeric strings).
    fn bump(v: &Value) -> Value {
        match v {
            Value::Number(n) if n.is_i64() => json!(n.as_i64().unwrap() + 7),
            Value::Number(n) => json!(n.as_f64().unwrap() + 1.37),
            Value::String(s) if s.parse::<i64>().is_ok() => json!((s.parse::<i64>().unwrap() + 3).to_string()),
            Value::Array(a) => Value::Array(a.iter().map(bump).collect()),
            Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), bump(v))).collect()),
            other => other.clone(),
        }
    }

    /// Values for every topic the rolling numbers read.
    fn every_topic() -> Vec<(Topic, SlotView)> {
        let mut slots = dash_fixture().slots();
        slots.retain(|(t, _)| *t != Topic::Health);
        slots.extend(signal_fixture().slots().into_iter().filter(|(t, _)| *t == Topic::Signal).map(|(_, s)| (Topic::Signal, s)));
        slots.retain(|(t, s)| *t != Topic::Signal || s.value.as_ref().is_some_and(|v| v["nr5g_pci"].is_string()));
        let now = dash_fixture().now;
        let slot = |v: Value| SlotView { version: 1, at: now, value_at: now, value: Some(Arc::new(v)), error: None };
        slots.push((Topic::Health, slot(json!({"cpu": {"overall": 40.84, "cores": [1.0, 2.0]}, "thermal": {"cpuss_temp": 37.2, "sensors": [
            {"id": "a", "name": "cpuss-0", "temperature_c": 41.5}, {"id": "b", "name": "usb", "temperature_c": 33.0},
            {"id": "c", "name": "sys-therm-1", "temperature_c": 30.5}]}}))));
        slots.push((Topic::Speedtest, slot(json!({"phase": "download", "progress": 42, "live_speed_mbps": 87.3, "server": "x"}))));
        slots
    }

    #[test]
    fn numbers_roll_only_on_later_frames_with_unique_digit_ids() {
        let mut rolled = 0;
        for s in crate::web::routes::SCREENS.iter().filter(|s| !s.live.is_empty()) {
            let first = every_topic();
            let now = dash_fixture().now;
            let mut em = PageEmitter::new(hub(), LivePage::of(s), now);
            // A first frame (and so a reconnect) draws every number plain.
            let open = elements(&em.open(first.clone(), now)).join("");
            assert!(!open.contains("class=\"rl\"") && !open.contains("<i id="), "{}: {open}", s.path);
            let moved: Vec<(Topic, SlotView)> = first
                .iter()
                .map(|(t, sv)| (*t, SlotView { version: 2, value: sv.value.as_deref().map(|v| Arc::new(bump(v))), ..sv.clone() }))
                .collect();
            let parts = elements(&em.changes(moved.clone(), now));
            let all = parts.join("");
            let ids: Vec<&str> = all.split("<i id=\"").skip(1).map(|r| r.split('"').next().unwrap()).collect();
            rolled += ids.len();
            let mut unique = ids.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), ids.len(), "{}: duplicate digit ids {ids:?}", s.path);
            for id in &ids {
                // Key letters, the place, the digit; never a fragment's id.
                let digits = id.trim_start_matches(|c: char| c.is_ascii_lowercase());
                assert!(digits.len() >= 2 && digits.bytes().all(|b| b.is_ascii_digit()), "{id}");
                assert!(s.live.iter().all(|f| f.id != *id), "{id}");
            }
            // Every rolled number is read once: its text in `.sr`, the
            // digits hidden.
            assert_eq!(all.matches("<span class=\"rl\" aria-hidden=\"true\">").count(), all.matches("class=\"rl\"").count());
            // The same values again: nothing to send, so nothing restarts
            // (a screen streamed only while something runs has ended).
            let again: Vec<(Topic, SlotView)> = moved.into_iter().map(|(t, sv)| (t, SlotView { version: 3, ..sv })).collect();
            let again = elements(&em.changes(again, now));
            assert!(again.is_empty() || s.live_while.is_some(), "{}: {again:?}", s.path);
        }
        assert!(rolled > 40, "{rolled}");
        // The dashboard: the changed digits of each card roll, by direction.
        let first = every_topic();
        let now = dash_fixture().now;
        let mut em = PageEmitter::new(hub(), dash_page(), now);
        em.open(first.clone(), now);
        let sig = |rsrp: &str| {
            let v = first.iter().find(|(t, _)| *t == Topic::Signal).unwrap().1.value.as_deref().unwrap().clone();
            let mut v = v;
            v["nr5g_rsrp"] = json!(rsrp);
            SlotView { version: 9, at: now, value_at: now, value: Some(Arc::new(v)), error: None }
        };
        let parts = elements(&em.changes(vec![(Topic::Signal, sig("-78"))], now));
        let conn = parts.iter().find(|p| p.contains("id=\"card-conn\"")).unwrap();
        assert!(
            conn.contains("<b class=\"num\"><span class=\"sr\">-78 dBm</span><span class=\"rl\" aria-hidden=\"true\">-7<i id=\"rsrp08\" data-d=\"7\">8</i> dBm</span></b>"),
            "{conn}"
        );
        let parts = elements(&em.changes(vec![(Topic::Signal, sig("-69"))], now));
        let conn = parts.iter().find(|p| p.contains("id=\"card-conn\"")).unwrap();
        assert!(conn.contains("-<i id=\"rsrp16\" data-u=\"7\">6</i><i id=\"rsrp09\" data-u=\"8\">9</i> dBm"), "{conn}");
    }

    #[test]
    fn bye_ends_with_a_status_line_without_the_watchdog() {
        let mut em = PageEmitter::new(hub(), signal_page(false), BootClock.now());
        assert_eq!(
            elements(&em.bye(ByeReason::LoggedOut)).join(""),
            "<p class=\"live\" id=\"live-status\">Signed out. <a href=\"/ui/login?next=%2Fui%2Fsignal\">Log in</a></p>"
        );
        // The dashboard: the pill says "Offline", the reason goes under the bar.
        let mut em = PageEmitter::new(hub(), dash_page(), BootClock.now());
        let note = |em: &mut PageEmitter, r| elements(&em.bye(r)).join("");
        for r in [ByeReason::TokenExpired, ByeReason::LoggedOut] {
            assert_eq!(
                note(&mut em, r),
                "<p class=\"live ps\" id=\"live-status\"><span class=\"off\"><b>Offline</b></span></p>\
                 <p id=\"live-note\">Signed out. <a href=\"/ui/login?next=%2Fui%2F\">Log in</a></p>"
            );
        }
        assert!(note(&mut em, ByeReason::Lifetime).contains("Live updates paused after 30 minutes. <a href=\"/ui/\">Resume</a>"));
        assert!(note(&mut em, ByeReason::Evicted).contains("Opened in another tab. <a href=\"/ui/\">Refresh</a>"));
        assert!(note(&mut em, ByeReason::Standby).contains("The router went into standby. <a href=\"/ui/\">Refresh</a>"));
        assert!(!note(&mut em, ByeReason::Standby).contains(" on"));
    }

    #[test]
    fn signal_frames_keep_the_chart_and_hidden_panels() {
        let h = hub();
        let v = signal_fixture();
        let mut em = PageEmitter::new(Arc::clone(&h), signal_page(true), v.now);
        let parts = elements(&em.open(v.slots(), v.now));
        let mut want: Vec<String> = signal::FRAGMENTS.iter().map(|f| f.id.to_owned()).collect();
        want.push("live-status".into());
        assert_eq!(ids(&parts), want);
        assert!(parts.iter().any(|p| p == "<section class=\"card\" id=\"sig-wcdma\" hidden></section>"));
        // Byte-equal to the page (the chart: from the hub's own history).
        let mut page_views = signal_fixture();
        page_views.history = h.history();
        let page = crate::web::pages::signal::tests::page(&page_views);
        for part in &parts[..parts.len() - 1] {
            assert!(page.contains(part.as_str()), "{part}");
        }
        // A new reading lands in the hub's history and moves the chart.
        h.offer(Topic::Signal, crate::events::hub::ReadOut { value: Some(json!({"nr5g_rsrp": "-90"})), error: None }, std::time::Instant::now());
        let changed = vec![(Topic::Signal, h.peek(Topic::Signal))];
        let parts = elements(&em.changes(changed, v.now));
        assert!(parts.iter().any(|p| p.starts_with("<figure class=\"card\" id=\"sig-chart\">") && p.contains("polyline")));
    }

    #[test]
    fn frames_carry_no_identifier_script_or_datastar_attribute() {
        let v = dash_fixture();
        let mut em = PageEmitter::new(hub(), dash_page(), v.now);
        let mut all = em.open(v.slots(), v.now);
        let sv = signal_fixture();
        all.extend(PageEmitter::new(hub(), signal_page(true), sv.now).open(sv.slots(), sv.now));
        let text = all.concat();
        for secret in [
            "WIFIKEY-7f3a", "001019999911111", "+10000000042", "10000000042", "8900100000009998877", "9998877",
            "000000000000042", "aa:bb:cc:dd:ee:42", "alice-laptop-42", "<script", "data-",
        ] {
            assert!(!text.contains(secret), "{secret} in a frame");
        }
    }

    /// Device text is not trusted (operator names, band and cell strings,
    /// addresses, read errors). With every string the pages read set to
    /// markup, a quote and a Datastar attribute, nothing can run: the only
    /// script element is the page's own Datastar tag, and the only `data-*`
    /// attributes are the page's `data-nonce` and `data-init`, in the full
    /// page and in every stream frame.
    #[test]
    fn hostile_device_text_stays_inert() {
        use crate::web::render::{page, PageCtx};
        use serde_json::Value;
        const HOSTILE: &str = "<script>x</script>\"' data-on:x=1";
        // Every string but the operate mode (hostile, it would be airplane
        // mode, which blanks the operator).
        fn poison(v: &mut Value) {
            match v {
                Value::String(s) => *s = HOSTILE.to_owned(),
                Value::Array(a) => a.iter_mut().for_each(poison),
                Value::Object(o) => o.iter_mut().filter(|(k, _)| *k != "operate_mode").for_each(|(_, v)| poison(v)),
                _ => {}
            }
        }
        let hostile = |v: Views, error: bool| {
            let slots = v
                .slots()
                .into_iter()
                .map(|(t, mut s)| {
                    let mut value = s.value.as_deref().cloned().unwrap_or(Value::Null);
                    poison(&mut value);
                    s.value = Some(Arc::new(value));
                    s.error = error.then(|| Arc::from(HOSTILE));
                    (t, s)
                })
                .collect();
            let mut out = Views::new(v.now, slots);
            out.history = v.history;
            out
        };
        let inert = |html: &str, scripts: usize, attrs: usize| {
            assert_eq!(html.matches("<script").count(), scripts, "{html}");
            // Shown as text: no raw quote closes an attribute before it.
            assert!(!html.contains("\"' data-on"), "{html}");
            assert_eq!(html.matches("data-").count() - html.matches(" data-on:x=1").count(), attrs, "{html}");
        };
        let live_page = |url: &str, body| {
            let ctx = PageCtx { title: "T", nonce: "00", live: Some(url), ..PageCtx::default() };
            page(&ctx, body).into_string()
        };
        let signal_page = || signal_page(false);
        for error in [false, true] {
            let v = hostile(dash_fixture(), error);
            let html = live_page("/ui/live/dashboard/0123456789abcdef", dashboard::body(&v, Some(true), &html! {}));
            assert!(html.contains("&lt;script&gt;x&lt;/script&gt;&quot;' data-on:x=1"), "{html}");
            inert(&html, 2, 2);
            inert(&PageEmitter::new(hub(), dash_page(), v.now).open(v.slots(), v.now).concat(), 0, 0);
            let mut v = hostile(signal_fixture(), error);
            let html = live_page("/ui/live/signal/0123456789abcdef", signal::body(&v));
            assert!(html.contains("&lt;script&gt;x&lt;/script&gt;&quot;' data-on:x=1"), "{html}");
            inert(&html, 2, 2);
            v.history = Vec::new();
            inert(&PageEmitter::new(hub(), signal_page(), v.now).open(v.slots(), v.now).concat(), 0, 0);
        }
    }

    /// Live fragments render from data alone: never a form, a CSRF token,
    /// an input or a `<details>` (a patch would reset what was typed or
    /// opened), on every live screen and the operation page.
    #[test]
    fn fragments_carry_no_client_state() {
        let empty = Views::new(BootClock.now(), Vec::new());
        let screens = crate::web::routes::SCREENS.iter().flat_map(|s| s.live.iter());
        for f in screens.chain(op::FRAGMENTS.iter()) {
            for v in [&empty, &dash_fixture(), &signal_fixture()] {
                let html = (f.render)(v).into_string();
                assert!(html.contains(&format!("id=\"{}\"", f.id)), "{}", f.id);
                for bad in ["<form", "csrf", "<input", "<details", "style="] {
                    assert!(!html.contains(bad), "{bad} in {}", f.id);
                }
            }
        }
    }

    /// Every interpolation in the UI goes through maud's escaping.
    #[test]
    fn web_code_has_no_unescaped_markup() {
        fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().map(Result::unwrap) {
                let p = e.path();
                if p.is_dir() {
                    rust_files(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        rust_files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/web"), &mut files);
        assert!(files.len() > 10, "{files:?}");
        let raw = concat!("PreEs", "caped");
        for f in files {
            assert!(!std::fs::read_to_string(&f).unwrap().contains(raw), "{} uses {raw}", f.display());
        }
    }

    #[test]
    fn page_keys_are_16_hex() {
        assert_eq!(page_key("00000000000000ff"), Some(255));
        for bad in ["", "ff", "00000000000000FF", "0000000000000000f", "000000000000000g"] {
            assert_eq!(page_key(bad), None, "{bad}");
        }
        let auth = crate::auth::AuthState::new();
        let url = url(&auth, "/ui/live/signal").unwrap();
        assert!(url.strip_prefix("/ui/live/signal/").is_some_and(|k| page_key(k).is_some()), "{url}");
    }

    /// Signal Detection streams while a sweep runs, and ends (status line
    /// cleared) with the update that shows it has ended.
    #[test]
    fn a_stream_while_a_sweep_runs_ends_with_it() {
        let now = BootClock.now();
        let page = LivePage::of(&crate::web::tabs::router::SIGNAL_DETECT);
        assert!(page.until.is_some());
        let mut em = PageEmitter::new(hub(), page, now);
        let sweep = |pct: u64, running: u64| SlotView {
            version: pct + 1,
            at: now,
            value_at: now,
            value: Some(Arc::new(json!({"progress": {"progress": pct, "running": running}, "results": null}))),
            error: None,
        };
        let parts = elements(&em.open(vec![(Topic::SignalDetect, sweep(40, 1))], now));
        assert!(parts.iter().any(|p| p.contains("40%")) && !em.finished());
        let parts = elements(&em.changes(vec![(Topic::SignalDetect, sweep(100, 0))], now));
        assert!(em.finished(), "{parts:?}");
        assert!(parts.last().unwrap().starts_with("<p class=\"live\" id=\"live-status\">"), "{parts:?}");
    }

    #[test]
    fn an_operation_stream_ends_with_the_operation() {
        let now = BootClock.now();
        let op = Op { spec: &crate::web::ops::AIRPLANE_ON, started: now, arg: "" };
        let page = LivePage {
            page: "/ui/op/0123456789abcdef".into(),
            next: None,
            topics: op.spec.topics,
            fragments: &op::FRAGMENTS,
            history: false,
            op: Some(op),
            pill: false,
            arg: 0,
            until: None,
            signal_hint: None,
        };
        let mut em = PageEmitter::new(hub(), page, now);
        let online = SlotView { version: 1, at: now, value_at: now, value: Some(Arc::new(json!({"status": {"operate_mode": "ONLINE"}}))), error: None };
        let parts = elements(&em.open(vec![(Topic::Modem, online.clone())], now));
        assert_eq!(parts[0], "<h1 id=\"op-title\">Turning airplane mode on</h1>");
        assert!(parts[1].contains("Turning the radio off") && parts[2].contains("hb-o"));
        assert!(!em.finished());
        // Byte-equal to the page's heading and progress card.
        let mut views = Views::new(now, vec![(Topic::Modem, online)]);
        views.op = em.page.op;
        assert_eq!(parts[0], op::heading(&views).into_string());
        assert_eq!(parts[1], op::progress(&views).into_string());
        let lpm = SlotView { version: 2, at: now, value_at: now, value: Some(Arc::new(json!({"status": {"operate_mode": "LPM"}}))), error: None };
        let parts = elements(&em.changes(vec![(Topic::Modem, lpm)], now));
        // The heading turns to the past tense with the outcome.
        assert_eq!(parts[0], "<h1 id=\"op-title\">Airplane mode turned on</h1>");
        assert!(parts[1].contains("Airplane mode is on. The radio is off."));
        assert_eq!(parts[2], "<p class=\"live\" id=\"live-status\"></p>");
        assert!(em.finished());
    }
}
