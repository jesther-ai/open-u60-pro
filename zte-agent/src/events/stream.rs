//! The stream driver shared by `GET /api/events` and the web UI's live
//! fragments: snapshot on open, then changes, a tick, and a final `bye`.
//!
//! Three deadlines persist across wakes (next tick, session expiry, stream
//! lifetime), so a busy topic never postpones the tick. Tokio timers only
//! hint when to look: they pause in suspend, so expiry and lifetime are
//! checked on the boot clock at every wake. The session is re-checked only
//! on this stream's own wakes, never on unrelated hub changes; a revoked
//! session's stream is ended by the hub. Dropping the stream (client gone)
//! drops the subscription and its demand.

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use futures_util::Stream;
use tokio::time::Instant;

use super::hub::{EndReason, SlotView, Subscription, TopicAge, Wake};
use super::sse::Frame;
use super::Topic;
use crate::auth::{AuthState, SessionCheck, SessionId};
use crate::boottime::BootInstant;

/// Why a stream ended (`event: bye`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ByeReason {
    TokenExpired,
    LoggedOut,
    /// A newer stream of the same token or session replaced this one.
    Evicted,
    /// Open too long (30 min): the client reconnects if it still needs it.
    Lifetime,
    Standby,
}

impl ByeReason {
    pub fn name(self) -> &'static str {
        match self {
            ByeReason::TokenExpired => "token_expired",
            ByeReason::LoggedOut => "logged_out",
            ByeReason::Evicted => "evicted",
            ByeReason::Lifetime => "lifetime",
            ByeReason::Standby => "standby",
        }
    }
}

impl From<EndReason> for ByeReason {
    fn from(r: EndReason) -> Self {
        match r {
            EndReason::Evicted => ByeReason::Evicted,
            EndReason::LoggedOut => ByeReason::LoggedOut,
            EndReason::Standby => ByeReason::Standby,
        }
    }
}

/// Turns hub state into frames (JSON for the API, HTML patches for the UI).
pub trait Emitter: Send + 'static {
    fn open(&mut self, snapshot: Vec<(Topic, SlotView)>, now: BootInstant) -> Vec<Frame>;
    fn changes(&mut self, changed: Vec<(Topic, SlotView)>, now: BootInstant) -> Vec<Frame>;
    fn tick(&mut self, now: BootInstant, ages: &[TopicAge]) -> Vec<Frame>;
    fn bye(&mut self, reason: ByeReason) -> Vec<Frame>;
    /// Nothing more to send (a finished operation): the stream ends after
    /// the frames already produced, without a `bye`.
    fn finished(&self) -> bool {
        false
    }
}

/// Where a stream checks its session, and the boot clock it is measured on.
pub trait Sessions: Send + Sync + 'static {
    fn touch_session(&self, id: SessionId) -> SessionCheck;
    fn now(&self) -> BootInstant;
}

impl Sessions for AuthState {
    fn touch_session(&self, id: SessionId) -> SessionCheck {
        AuthState::touch_session(self, id)
    }

    fn now(&self) -> BootInstant {
        AuthState::now(self)
    }
}

pub struct StreamEnv {
    pub auth: Arc<dyn Sessions>,
    pub owner: SessionId,
    /// Tick period (API: keep-alive comment; UI: heartbeat).
    pub tick: Duration,
    pub first_tick: Duration,
    /// Least time between two batches of changes (0: none).
    pub min_gap: Duration,
    pub max_life: Option<Duration>,
}

pub fn drive<E: Emitter>(
    env: StreamEnv,
    sub: Subscription,
    em: E,
) -> impl Stream<Item = Result<Frame, Infallible>> + Send + 'static {
    let now = Instant::now();
    let driver = Driver {
        next_tick: now + env.first_tick,
        // Set from the session at open.
        session_deadline: now,
        life_deadline: env.max_life.map(|d| now + d),
        opened_at: env.auth.now(),
        env,
        sub: Some(sub),
        em,
        outbox: Vec::new(),
        opened: false,
        last_batch: None,
        held: false,
    };
    futures_util::stream::unfold(driver, |mut d| async move {
        let ev = d.next().await?;
        Some((Ok(ev), d))
    })
}

struct Driver<E> {
    env: StreamEnv,
    /// `None` once the stream has ended (demand already given back).
    sub: Option<Subscription>,
    em: E,
    /// Frames not yet sent, oldest first (at most one batch and a `bye`).
    outbox: Vec<Frame>,
    opened: bool,
    opened_at: BootInstant,
    next_tick: Instant,
    session_deadline: Instant,
    life_deadline: Option<Instant>,
    last_batch: Option<Instant>,
    /// Changes are waiting for the end of the gap after the last batch.
    held: bool,
}

impl<E: Emitter> Driver<E> {
    async fn next(&mut self) -> Option<Frame> {
        loop {
            if !self.outbox.is_empty() {
                return Some(self.outbox.remove(0));
            }
            // Ended: the `bye` was the last frame.
            self.sub.as_ref()?;
            if !self.opened {
                self.opened = true;
                if self.check_session() {
                    let snapshot = sub_mut(&mut self.sub).snapshot();
                    let mut frames = self.em.open(snapshot, self.env.auth.now());
                    self.outbox.append(&mut frames);
                    self.end_if_finished();
                }
                continue;
            }
            let mut deadline = self.next_tick.min(self.session_deadline + Duration::from_secs(1));
            if let Some(life) = self.life_deadline {
                deadline = deadline.min(life);
            }
            // Held changes are not watched (they coalesce) until the gap ends.
            let held = self.held;
            if let Some(end) = self.gap_end().filter(|_| held) {
                deadline = deadline.min(end);
            }
            let wake = {
                let mut changed = std::pin::pin!(sub_mut(&mut self.sub).changed());
                let mut sleep = std::pin::pin!(tokio::time::sleep_until(deadline));
                // Changes first; the timer is only armed while nothing is ready.
                std::future::poll_fn(|cx| {
                    if !held {
                        if let Poll::Ready(w) = changed.as_mut().poll(cx) {
                            return Poll::Ready(Some(w));
                        }
                    }
                    sleep.as_mut().poll(cx).map(|()| None)
                })
                .await
            };
            self.after_wake(wake);
            self.end_if_finished();
        }
    }

    /// The emitter is done: give back the subscription; the stream ends once
    /// the outbox is sent.
    fn end_if_finished(&mut self) {
        if self.sub.is_some() && self.em.finished() {
            self.sub = None;
        }
    }

    fn after_wake(&mut self, wake: Option<Wake>) {
        if !self.check_session() {
            return;
        }
        if let Some(Wake::Ended(reason)) = wake {
            // What changed with it goes first (the `power_state` that
            // announced standby), ignoring the batch gap.
            let changed = sub_mut(&mut self.sub).changes();
            let mut frames = self.em.changes(changed, self.env.auth.now());
            self.outbox.append(&mut frames);
            return self.finish(reason.into());
        }
        let now = self.env.auth.now();
        // The boot clock never runs slower than tokio's; either one passing
        // the lifetime ends it (no spin on a timer that fired a hair early).
        let lived = now.saturating_duration_since(self.opened_at);
        if self.env.max_life.is_some_and(|max| lived >= max)
            || self.life_deadline.is_some_and(|d| Instant::now() >= d)
        {
            return self.finish(ByeReason::Lifetime);
        }
        if wake == Some(Wake::Changed) || self.held {
            // Inside the gap after the last batch, changes wait for its end;
            // whatever changed meanwhile goes into one batch then.
            self.held = self.gap_end().is_some_and(|end| Instant::now() < end);
            if !self.held {
                let changed = sub_mut(&mut self.sub).changes();
                let mut batch = self.em.changes(changed, self.env.auth.now());
                if !batch.is_empty() {
                    self.last_batch = Some(Instant::now());
                    self.outbox.append(&mut batch);
                }
            }
        }
        let t = Instant::now();
        if t >= self.next_tick {
            let ages = sub_mut(&mut self.sub).ages();
            let mut frames = self.em.tick(now, &ages);
            self.outbox.append(&mut frames);
            // No catch-up burst after a long pause.
            self.next_tick = (self.next_tick + self.env.tick).max(t + Duration::from_secs(1));
        }
    }

    /// When the next batch of changes may go out (`min_gap` after the last).
    fn gap_end(&self) -> Option<Instant> {
        self.last_batch.filter(|_| !self.env.min_gap.is_zero()).map(|last| last + self.env.min_gap)
    }

    /// Slides the session; false (and `bye`) if it expired or was revoked.
    fn check_session(&mut self) -> bool {
        match self.env.auth.touch_session(self.env.owner) {
            SessionCheck::Live { remaining } => {
                self.session_deadline = Instant::now() + remaining;
                true
            }
            SessionCheck::Expired => {
                self.finish(ByeReason::TokenExpired);
                false
            }
            SessionCheck::Revoked => {
                self.finish(ByeReason::LoggedOut);
                false
            }
        }
    }

    /// Give back the subscription now (even if the peer never reads the
    /// rest), then queue the `bye`.
    fn finish(&mut self, reason: ByeReason) {
        self.sub = None;
        let mut frames = self.em.bye(reason);
        self.outbox.append(&mut frames);
    }
}

/// Only called while the stream is running.
fn sub_mut(sub: &mut Option<Subscription>) -> &mut Subscription {
    sub.as_mut().expect("stream still running")
}

#[cfg(test)]
mod tests {
    use super::super::hub::{Hub, ReadOut, SubscribeReq};
    use super::super::sse::{frame as sse_frame, hello, response, JsonEmitter};
    use super::*;
    use crate::auth::{AuthConfig, Kind, PeerKey, SeqRng};
    use crate::boottime::Clock;
    use axum::body::BodyDataStream;
    use futures_util::StreamExt;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Mutex;

    const HOUR: Duration = Duration::from_secs(3600);

    /// The boot clock in tokio's (paused) time, plus suspended time that
    /// tokio never sees.
    struct TokioClock {
        base: Instant,
        suspended_ms: AtomicU64,
    }

    impl TokioClock {
        fn new() -> Arc<Self> {
            Arc::new(TokioClock { base: Instant::now(), suspended_ms: AtomicU64::new(0) })
        }

        fn suspend(&self, d: Duration) {
            self.suspended_ms.fetch_add(d.as_millis() as u64, Ordering::SeqCst);
        }
    }

    impl Clock for TokioClock {
        fn now(&self) -> BootInstant {
            let suspended = Duration::from_millis(self.suspended_ms.load(Ordering::SeqCst));
            BootInstant::from_duration(Duration::from_secs(1000) + self.base.elapsed() + suspended)
        }
    }

    /// Counts session checks; always live.
    struct Counting {
        clock: Arc<TokioClock>,
        touches: AtomicUsize,
    }

    impl Sessions for Counting {
        fn touch_session(&self, _: SessionId) -> SessionCheck {
            self.touches.fetch_add(1, Ordering::SeqCst);
            SessionCheck::Live { remaining: HOUR }
        }

        fn now(&self) -> BootInstant {
            self.clock.now()
        }
    }

    /// Frames that name what the driver asked for.
    struct Probe {
        ticks_ages: Arc<Mutex<Vec<Vec<TopicAge>>>>,
    }

    impl Emitter for Probe {
        fn open(&mut self, snapshot: Vec<(Topic, SlotView)>, _: BootInstant) -> Vec<Frame> {
            let names: Vec<_> = snapshot.iter().map(|(t, _)| t.name()).collect();
            vec![sse_frame("open", &names.join(","))]
        }

        fn changes(&mut self, changed: Vec<(Topic, SlotView)>, _: BootInstant) -> Vec<Frame> {
            changed.iter().map(|(t, s)| sse_frame("change", &format!("{}={}", t.name(), s.version))).collect()
        }

        fn tick(&mut self, _: BootInstant, ages: &[TopicAge]) -> Vec<Frame> {
            self.ticks_ages.lock().unwrap().push(ages.to_vec());
            vec![sse_frame("tick", "")]
        }

        fn bye(&mut self, reason: ByeReason) -> Vec<Frame> {
            vec![sse_frame("bye", reason.name())]
        }
    }

    fn probe() -> (Probe, Arc<Mutex<Vec<Vec<TopicAge>>>>) {
        let ages = Arc::new(Mutex::new(Vec::new()));
        (Probe { ticks_ages: Arc::clone(&ages) }, ages)
    }

    fn api_env(auth: Arc<dyn Sessions>, owner: SessionId) -> StreamEnv {
        let tick = Duration::from_secs(15);
        StreamEnv { auth, owner, tick, first_tick: tick, min_gap: Duration::ZERO, max_life: None }
    }

    fn ui_env(auth: Arc<dyn Sessions>, owner: SessionId) -> StreamEnv {
        StreamEnv {
            auth,
            owner,
            tick: Duration::from_secs(5),
            first_tick: Duration::from_secs(1),
            min_gap: Duration::from_millis(250),
            max_life: Some(Duration::from_secs(30 * 60)),
        }
    }

    fn subscribe(hub: &Arc<Hub>, owner: SessionId, topics: &[Topic]) -> Subscription {
        let topics = topics.iter().copied().collect();
        hub.subscribe(SubscribeReq { owner, topics, signal_hint: None, replace_same: None }).unwrap()
    }

    fn body<E: Emitter>(env: StreamEnv, sub: Subscription, em: E) -> BodyDataStream {
        response(drive(env, sub, em)).into_body().into_data_stream()
    }

    /// The next frame as text, or None if the stream ended or nothing came
    /// within `wait` (virtual time).
    async fn frame(b: &mut BodyDataStream, wait: Duration) -> Option<String> {
        match tokio::time::timeout(wait, b.next()).await {
            Ok(Some(Ok(bytes))) => Some(String::from_utf8(bytes.to_vec()).unwrap()),
            _ => None,
        }
    }

    /// Frame names (`event:` or the comment) until `until` (virtual time).
    async fn frames_until(b: &mut BodyDataStream, until: Instant) -> Vec<(Duration, String)> {
        let start = Instant::now();
        let mut out = Vec::new();
        while let Ok(Some(Ok(bytes))) = tokio::time::timeout_at(until, b.next()).await {
            let s = String::from_utf8(bytes.to_vec()).unwrap();
            out.push((start.elapsed(), s));
        }
        out
    }

    /// The `event:` name, else the first line (a comment).
    fn name(frame: &str) -> &str {
        frame.lines().find_map(|l| l.strip_prefix("event: ")).unwrap_or_else(|| frame.lines().next().unwrap_or(""))
    }

    fn counting() -> (Arc<Counting>, Arc<TokioClock>) {
        let clock = TokioClock::new();
        (Arc::new(Counting { clock: Arc::clone(&clock), touches: AtomicUsize::new(0) }), clock)
    }

    /// A real auth table on the tokio clock, with one bearer token.
    fn bearer_auth(clock: &Arc<TokioClock>) -> (Arc<AuthState>, SessionId) {
        let a = Arc::new(AuthState::with_parts(clock.clone(), Box::new(SeqRng::new()), AuthConfig::default()));
        a.set_password("pw");
        let issued = a.login("pw", Kind::Bearer, PeerKey::UNKNOWN, &[]).unwrap();
        (a, issued.id)
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_fires_every_5s_despite_changes_every_1s() {
        let (sessions, clock) = counting();
        let hub = Arc::new(Hub::new(clock.clone()));
        let owner = SessionId::for_test(1);
        let (em, _) = probe();
        let mut b = body(ui_env(sessions, owner), subscribe(&hub, owner, &[Topic::PowerState]), em);
        let start = Instant::now();
        let setter = {
            let hub = Arc::clone(&hub);
            tokio::spawn(async move {
                for i in 0.. {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    hub.set(Topic::PowerState, json!({"action": i}));
                }
            })
        };
        let frames = frames_until(&mut b, start + Duration::from_millis(21_500)).await;
        setter.abort();
        let ticks: Vec<_> = frames.iter().filter(|(_, f)| name(f) == "tick").map(|(t, _)| t.as_secs()).collect();
        assert_eq!(ticks, [1, 6, 11, 16, 21]);
        let changes = frames.iter().filter(|(_, f)| name(f) == "change").count();
        assert!(changes >= 18, "{changes}");
    }

    #[tokio::test(start_paused = true)]
    async fn api_keepalive_every_15s() {
        let (sessions, clock) = counting();
        let hub = Arc::new(Hub::new(clock.clone()));
        let owner = SessionId::for_test(1);
        let topics = [Topic::Sms].into_iter().collect();
        let em = JsonEmitter::new(hello(42, topics, &[], Duration::from_secs(15)));
        let mut b = body(api_env(sessions.clone(), owner), subscribe(&hub, owner, &[Topic::Sms]), em);
        let start = Instant::now();
        let frames = frames_until(&mut b, start + Duration::from_secs(46)).await;
        let got: Vec<_> = frames.iter().map(|(t, f)| (t.as_secs(), name(f))).collect();
        assert_eq!(
            got,
            [(0, "hello"), (0, "sms"), (15, ": keep-alive"), (30, ": keep-alive"), (45, ": keep-alive")]
        );
        assert!(frames[0].1.starts_with("retry: 3000\nevent: hello\n"));
        assert!(frames[1].1.starts_with("event: sms\ndata: {\"seq\":1,\"age_ms\":0,\"error\":null,\"data\":{\"received\":0}}"));
        // The session is checked at open and on each tick.
        assert_eq!(sessions.touches.load(Ordering::SeqCst), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn unrelated_topic_changes_do_not_wake_or_touch_session() {
        let (sessions, clock) = counting();
        let hub = Arc::new(Hub::new(clock.clone()));
        let owner = SessionId::for_test(1);
        let (em, _) = probe();
        let mut b = body(api_env(sessions.clone(), owner), subscribe(&hub, owner, &[Topic::Sms]), em);
        assert_eq!(name(&frame(&mut b, HOUR).await.unwrap()), "open");
        assert_eq!(sessions.touches.load(Ordering::SeqCst), 1);
        for i in 0..20 {
            hub.set(Topic::PowerState, json!({"action": i}));
            hub.store(Topic::Battery, ReadOut { value: Some(json!(i)), error: None }, std::time::Instant::now());
            assert!(frame(&mut b, Duration::from_millis(100)).await.is_none());
        }
        assert_eq!(sessions.touches.load(Ordering::SeqCst), 1);
        hub.on_bus_event("zwrt_wms_status_event", &json!({"wms_status": "new_sms_received"}));
        assert!(frame(&mut b, Duration::from_secs(1)).await.unwrap().contains("sms="));
        assert_eq!(sessions.touches.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn revocation_ends_with_bye_logged_out() {
        let clock = TokioClock::new();
        let (auth, id) = bearer_auth(&clock);
        let hub = Arc::new(Hub::new(clock.clone()));
        let h = Arc::clone(&hub);
        auth.set_revoke_hook(Box::new(move |ids| h.end_sessions(ids)));
        // Another token's stream keeps going.
        let other = auth.login("pw", Kind::Bearer, PeerKey::UNKNOWN, &[]).unwrap().id;
        let (em, _) = probe();
        let mut mine = body(api_env(auth.clone(), id), subscribe(&hub, id, &[Topic::Sms]), em);
        let (em, _) = probe();
        let mut theirs = body(api_env(auth.clone(), other), subscribe(&hub, other, &[Topic::Sms]), em);
        assert_eq!(name(&frame(&mut mine, HOUR).await.unwrap()), "open");
        assert_eq!(name(&frame(&mut theirs, HOUR).await.unwrap()), "open");
        assert!(auth.logout_id(id));
        assert_eq!(frame(&mut mine, Duration::from_secs(1)).await.unwrap(), "event: bye\ndata: logged_out\n\n");
        assert!(frame(&mut mine, HOUR).await.is_none(), "the stream ends after bye");
        assert!(frame(&mut theirs, Duration::from_secs(1)).await.is_none());
        assert_eq!(hub.demand(Topic::Sms), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn expiry_ends_with_token_expired_even_if_timer_late() {
        let clock = TokioClock::new();
        let (auth, id) = bearer_auth(&clock);
        let hub = Arc::new(Hub::new(clock.clone()));
        let (em, _) = probe();
        let mut b = body(api_env(auth.clone(), id), subscribe(&hub, id, &[Topic::Sms]), em);
        assert_eq!(name(&frame(&mut b, HOUR).await.unwrap()), "open");
        // The router slept 2 h: the boot clock moved, tokio's timers did not.
        clock.suspend(2 * HOUR);
        hub.on_bus_event("zwrt_wms_status_event", &json!({"wms_status": "new_sms_received"}));
        assert_eq!(frame(&mut b, Duration::from_secs(1)).await.unwrap(), "event: bye\ndata: token_expired\n\n");
        assert!(frame(&mut b, HOUR).await.is_none());
        assert_eq!(hub.demand(Topic::Sms), 0);

        // A quiet stream: its ticks slide the 1 h idle deadline, and the
        // session timer finds the 24 h absolute one.
        let (auth, id) = bearer_auth(&clock);
        let mut b = body(api_env(auth.clone(), id), subscribe(&hub, id, &[Topic::PowerState]), probe().0);
        let start = Instant::now();
        assert_eq!(name(&frame(&mut b, HOUR).await.unwrap()), "open");
        let frames = frames_until(&mut b, start + 25 * HOUR).await;
        let (at, last) = frames.last().unwrap();
        assert_eq!(last, "event: bye\ndata: token_expired\n\n");
        assert!(*at >= 24 * HOUR && *at <= 24 * HOUR + Duration::from_secs(2), "{at:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn eviction_sends_bye_evicted() {
        let (sessions, clock) = counting();
        let hub = Arc::new(Hub::new(clock.clone()));
        let owner = SessionId::for_test(9);
        let mut first = body(api_env(sessions.clone(), owner), subscribe(&hub, owner, &[Topic::Sms]), probe().0);
        assert_eq!(name(&frame(&mut first, HOUR).await.unwrap()), "open");
        let _more: Vec<_> = (0..3).map(|_| subscribe(&hub, owner, &[Topic::Sms])).collect();
        assert_eq!(frame(&mut first, Duration::from_secs(1)).await.unwrap(), "event: bye\ndata: evicted\n\n");
        assert!(frame(&mut first, HOUR).await.is_none());
        assert_eq!(hub.demand(Topic::Sms), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn standby_sends_bye_standby() {
        let (sessions, clock) = counting();
        let hub = Arc::new(Hub::new(clock.clone()));
        let owner = SessionId::for_test(1);
        let topics = [Topic::Signal, Topic::PowerState, Topic::Sms];
        let mut b = body(api_env(sessions, owner), subscribe(&hub, owner, &topics), probe().0);
        assert_eq!(name(&frame(&mut b, HOUR).await.unwrap()), "open");
        hub.on_bus_event("zwrt_device_power_state_event", &json!({"action": "mode_power_off", "parameter": "quicken"}));
        // The power event itself first, then the end.
        let version = hub.peek(Topic::PowerState).version;
        let change = frame(&mut b, Duration::from_secs(1)).await.unwrap();
        assert_eq!(change, format!("event: change\ndata: power_state={version}\n\n"));
        assert_eq!(frame(&mut b, Duration::from_secs(1)).await.unwrap(), "event: bye\ndata: standby\n\n");
        assert!(frame(&mut b, HOUR).await.is_none());
        assert_eq!((hub.demand(Topic::Signal), hub.demand(Topic::Sms)), (0, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn ui_lifetime_30min() {
        let (sessions, clock) = counting();
        let hub = Arc::new(Hub::new(clock.clone()));
        let owner = SessionId::for_test(1);
        let mut b = body(ui_env(sessions, owner), subscribe(&hub, owner, &[Topic::Sms]), probe().0);
        let start = Instant::now();
        let frames = frames_until(&mut b, start + HOUR).await;
        let (at, last) = frames.last().unwrap();
        assert_eq!(last, "event: bye\ndata: lifetime\n\n");
        assert_eq!(*at, Duration::from_secs(30 * 60));
        assert_eq!(hub.demand(Topic::Sms), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn api_stream_ends_with_lifetime() {
        let (sessions, clock) = counting();
        let hub = Arc::new(Hub::new(clock.clone()));
        let owner = SessionId::for_test(1);
        let env = crate::httpd::api::events_env(sessions, owner);
        let mut b = body(env, subscribe(&hub, owner, &[Topic::Sms]), probe().0);
        let start = Instant::now();
        let frames = frames_until(&mut b, start + HOUR).await;
        let (at, last) = frames.last().unwrap();
        assert_eq!(last, "event: bye\ndata: lifetime\n\n");
        assert_eq!(*at, Duration::from_secs(30 * 60));
        // Keep-alive ticks until then.
        assert!(frames.iter().filter(|(_, f)| f.contains("tick")).count() >= 100, "{frames:?}");
        assert_eq!(hub.demand(Topic::Sms), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn tick_uses_hub_ages_not_cached_view() {
        let (sessions, clock) = counting();
        let hub = Arc::new(Hub::new(clock.clone()));
        let owner = SessionId::for_test(1);
        let read = |v| ReadOut { value: Some(json!(v)), error: None };
        hub.store(Topic::Battery, read(80), std::time::Instant::now());
        let (em, ages) = probe();
        let mut b = body(ui_env(sessions, owner), subscribe(&hub, owner, &[Topic::Battery]), em);
        assert_eq!(name(&frame(&mut b, HOUR).await.unwrap()), "open");
        tokio::time::sleep(Duration::from_millis(500)).await;
        // A re-read with the same value: no change, but a new read time.
        hub.store(Topic::Battery, read(80), std::time::Instant::now());
        let reread_at = clock.now();
        assert_eq!(name(&frame(&mut b, Duration::from_secs(2)).await.unwrap()), "tick");
        let ages = ages.lock().unwrap();
        assert_eq!(ages[0], [TopicAge { topic: Topic::Battery, at: reread_at, value_at: reread_at, error: false }]);
    }

    #[tokio::test(start_paused = true)]
    async fn ui_batches_are_at_least_250ms_apart() {
        let (sessions, clock) = counting();
        let hub = Arc::new(Hub::new(clock.clone()));
        let owner = SessionId::for_test(1);
        let mut b = body(ui_env(sessions, owner), subscribe(&hub, owner, &[Topic::PowerState]), probe().0);
        assert_eq!(name(&frame(&mut b, HOUR).await.unwrap()), "open");
        hub.set(Topic::PowerState, json!({"action": 1}));
        let first = frame(&mut b, HOUR).await.unwrap();
        let t = Instant::now();
        for i in 2..10 {
            hub.set(Topic::PowerState, json!({"action": i}));
            tokio::task::yield_now().await;
        }
        // One coalesced batch with the latest version, after the gap.
        let second = frame(&mut b, HOUR).await.unwrap();
        assert!(t.elapsed() >= Duration::from_millis(250));
        assert_ne!(first, second);
        assert_eq!(second, format!("event: change\ndata: power_state={}\n\n", hub.peek(Topic::PowerState).version));
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_stream_releases_demand() {
        let (sessions, clock) = counting();
        let hub = Arc::new(Hub::new(clock.clone()));
        let owner = SessionId::for_test(1);
        let mut b = body(api_env(sessions, owner), subscribe(&hub, owner, &[Topic::Signal]), probe().0);
        assert_eq!(name(&frame(&mut b, HOUR).await.unwrap()), "open");
        assert_eq!(hub.demand(Topic::Signal), 1);
        drop(b);
        assert_eq!(hub.demand(Topic::Signal), 0);
    }
}
