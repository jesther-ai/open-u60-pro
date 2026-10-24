//! Live updates: one hub holding the latest value of each topic, fed by
//! firmware and agent events and by a sampler that runs only while someone
//! is subscribed. `GET /api/events` (and the web UI's live fragments)
//! stream a snapshot on connect and then only changes.
//!
//! Nothing runs when nobody watches: with zero subscribers the sampler lanes
//! park without a timeout. Bus events (service status, power, SMS, SIM
//! identity) are handled on the event bus's own thread, which wakes for them
//! anyway; nothing forks.

pub mod history;
pub mod hub;
pub mod readers;
pub mod sampler;
pub mod sse;
pub mod stream;
pub mod watched;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;

use crate::event_bus::EventBus;
use crate::handlers::AppState;
pub use hub::Hub;

/// Everything a client can subscribe to. `data` is always the `data` of the
/// matching GET route (see `docs/events.md`).
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Topic {
    Signal,
    Traffic,
    Battery,
    Health,
    Clients,
    Wan,
    Sim,
    Modem,
    Charger,
    BatteryInfo,
    SimIdentity,
    Service,
    PowerState,
    Sms,
    EsimSwitch,
    EsimDownload,
    Processes,
    Usb,
    Speedtest,
    Netscan,
    SignalDetect,
    Stk,
}

/// Which sampler thread reads a topic. `/proc` and sysfs reads (Fast) never
/// wait behind a hung `ubus` call (Ubus); None is never read, only pushed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Lane {
    Fast,
    Ubus,
    None,
}

pub struct TopicSpec {
    pub lane: Lane,
    /// Sampled topics are re-read this often while demanded.
    pub cadence: Option<Duration>,
    /// Every read is a change (the signal chart wants every point).
    pub always_bump: bool,
    /// Pull topics: a subscriber gets a fresh read if the value is older (the cache TTLs).
    pub max_age_on_subscribe: Option<Duration>,
}

const fn spec(lane: Lane, cadence_ms: u64, max_age_s: u64) -> TopicSpec {
    TopicSpec {
        lane,
        cadence: if cadence_ms > 0 { Some(Duration::from_millis(cadence_ms)) } else { None },
        always_bump: false,
        max_age_on_subscribe: if max_age_s > 0 { Some(Duration::from_secs(max_age_s)) } else { None },
    }
}

/// Indexed by `Topic as usize`.
const SPECS: [TopicSpec; Topic::COUNT] = [
    // Signal: 2 s by default; subscribers may ask for 1-10 s (`signal_ms`).
    TopicSpec { always_bump: true, ..spec(Lane::Ubus, 2_000, 0) },
    spec(Lane::Ubus, 1_000, 0),     // Traffic (zwrt_data native rates)
    spec(Lane::Fast, 5_000, 0),     // Battery
    spec(Lane::Fast, 2_000, 0),     // Health (the app's 2 s health loop; /proc and sysfs only)
    spec(Lane::Ubus, 10_000, 0),    // Clients
    spec(Lane::Ubus, 0, 30),        // Wan
    spec(Lane::Ubus, 0, 30),        // Sim
    spec(Lane::Ubus, 0, 30),        // Modem
    spec(Lane::Ubus, 10_000, 10),   // Charger
    spec(Lane::Ubus, 30_000, 10),   // BatteryInfo
    spec(Lane::Fast, 0, 0),         // SimIdentity: read once, then events
    spec(Lane::Ubus, 0, 30),        // Service: events; read on subscribe if older (a missed event)
    spec(Lane::None, 0, 0),         // PowerState
    spec(Lane::None, 0, 0),         // Sms
    spec(Lane::Fast, 0, 0),         // EsimSwitch: re-read when touched
    spec(Lane::Fast, 0, 0),         // EsimDownload: re-read when touched
    spec(Lane::Fast, 3_000, 0),     // Processes (the app's 3 s list; /proc only, the hub's own tracker)
    spec(Lane::Ubus, 10_000, 10),   // Usb (the app's 10 s cable check; also touched on charger events)
    spec(Lane::Fast, 0, 0),         // Speedtest: in memory, re-read when the test touches it
    spec(Lane::Ubus, 2_000, 0),     // Netscan (the app's 2 s scan/register polls)
    spec(Lane::Ubus, 2_000, 0),     // SignalDetect (the app's 2 s progress poll)
    spec(Lane::Fast, 0, 0),         // Stk: the toolkit worker's memory, re-read when the worker touches it
];

/// Every topic name, in `Topic` order; `NAME_END[i]` ends name `i`. (One
/// string and byte offsets instead of a table of `&str`, which would need
/// 22 relocated pointers.)
const NAMES: &str = "signaltrafficbatteryhealthclientswansimmodemchargerbattery_info\
                     sim_identityservicepower_statesmsesim_switchesim_downloadprocessesusbspeedtest\
                     netscansignal_detectstk";
const NAME_END: [u8; Topic::COUNT] = [6, 13, 20, 26, 33, 36, 39, 44, 51, 63, 75, 82, 93, 96, 107, 120, 129, 132, 141, 148, 161, 164];

impl Topic {
    pub const COUNT: usize = 22;
    pub const ALL: [Topic; Topic::COUNT] = [
        Topic::Signal,
        Topic::Traffic,
        Topic::Battery,
        Topic::Health,
        Topic::Clients,
        Topic::Wan,
        Topic::Sim,
        Topic::Modem,
        Topic::Charger,
        Topic::BatteryInfo,
        Topic::SimIdentity,
        Topic::Service,
        Topic::PowerState,
        Topic::Sms,
        Topic::EsimSwitch,
        Topic::EsimDownload,
        Topic::Processes,
        Topic::Usb,
        Topic::Speedtest,
        Topic::Netscan,
        Topic::SignalDetect,
        Topic::Stk,
    ];

    pub fn name(self) -> &'static str {
        let i = self as usize;
        let start = if i == 0 { 0 } else { NAME_END[i - 1] as usize };
        NAMES.get(start..NAME_END[i] as usize).unwrap_or_default()
    }

    pub fn from_name(s: &str) -> Option<Topic> {
        Topic::ALL.into_iter().find(|t| t.name() == s)
    }

    pub fn spec(self) -> &'static TopicSpec {
        &SPECS[self as usize]
    }
}

/// A set of topics (bit per `Topic`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicSet(u32);

impl TopicSet {
    pub fn insert(&mut self, t: Topic) {
        self.0 |= 1 << t as u32;
    }

    pub fn contains(self, t: Topic) -> bool {
        self.0 & (1 << t as u32) != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// In `Topic` order.
    pub fn iter(self) -> impl Iterator<Item = Topic> {
        Topic::ALL.into_iter().filter(move |&t| self.contains(t))
    }
}

impl FromIterator<Topic> for TopicSet {
    fn from_iter<I: IntoIterator<Item = Topic>>(iter: I) -> Self {
        let mut set = TopicSet::default();
        iter.into_iter().for_each(|t| set.insert(t));
        set
    }
}

/// Bus events the hub handles. The three firmware names are already in the
/// `ubus listen` filter for other subscribers, so it gains no names; the hub
/// keeps `zwrt_device_power_state_event` registered on its own.
pub const BUS_EVENTS: [&str; 4] = [
    "zwrt_servicestatus",
    "zwrt_device_power_state_event",
    "zwrt_wms_status_event",
    crate::sim_identity::EVENT,
];

/// A JSON object from `(key, value)` pairs: one shared function instead of a
/// `json!` expansion at every site.
pub(crate) fn object(pairs: &mut [(&str, Value)]) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in pairs {
        map.insert((*key).into(), std::mem::take(value));
    }
    Value::Object(map)
}

static HUB: OnceLock<Arc<Hub>> = OnceLock::new();

fn install(h: Arc<Hub>) {
    let _ = HUB.set(h);
}

/// A watched value changed (see `watched.rs`): mark the topic stale, so it
/// is re-read if someone is subscribed. A no-op before `start` and inside
/// the hub's own reads (a reader that takes `&mut`, such as
/// `esim::download_status`, would otherwise re-trigger itself forever).
pub fn touch(t: Topic) {
    if readers::in_hub_read() {
        return;
    }
    #[cfg(test)]
    {
        test_sink::record(t);
        if let Some(h) = test_sink::hub() {
            h.touch(t, None);
            return;
        }
    }
    if let Some(h) = HUB.get() {
        h.touch(t, None);
    }
}

/// Wire the hub to its sources and start the sampler. Call before
/// `bus.start()`: the hub's firmware events must be in the `ubus listen` filter.
pub fn start(state: &Arc<AppState>, bus: &EventBus) {
    wire(state, bus);
    sampler::spawn(Arc::clone(&state.hub), Arc::new(sampler::AppReaders(Arc::clone(state))));
}

/// `start` without the sampler threads: events, cache invalidations, eSIM
/// changes, revocations and toolkit changes reach the hub.
pub(crate) fn wire(state: &Arc<AppState>, bus: &EventBus) {
    install(Arc::clone(&state.hub));
    let h = Arc::clone(&state.hub);
    // Runs on the bus's reader (or publisher) thread: the hub only updates
    // its slots and wakes what is waiting; it never blocks or publishes.
    bus.subscribe_fn(&BUS_EVENTS, Arc::new(move |name: &str, payload: &Value| h.on_bus_event(name, payload)));
    let h = Arc::clone(&state.hub);
    state.cache.set_observer(Box::new(move |k, s| h.on_cache_invalidated(k, s)));
    let h = Arc::clone(&state.hub);
    state.auth.set_revoke_hook(Box::new(move |ids| h.end_sessions(ids)));
    // The toolkit worker's state changed: re-read `stk` if someone watches.
    let h = Arc::clone(&state.hub);
    state.stk.on_change(Arc::new(move || h.touch(Topic::Stk, None)));
}

/// Test-only view of `touch`: what was touched on this thread, and an
/// optional per-thread hub instead of the process-wide one.
#[cfg(test)]
pub(crate) mod test_sink {
    use super::{Hub, Topic};
    use std::cell::RefCell;
    use std::sync::Arc;

    thread_local! {
        static TOUCHED: RefCell<Vec<Topic>> = const { RefCell::new(Vec::new()) };
        static HUB: RefCell<Option<Arc<Hub>>> = const { RefCell::new(None) };
    }

    pub fn record(t: Topic) {
        TOUCHED.with(|v| v.borrow_mut().push(t));
    }

    /// Touches recorded on this thread since the last call.
    pub fn take() -> Vec<Topic> {
        TOUCHED.with(|v| std::mem::take(&mut *v.borrow_mut()))
    }

    pub fn hub() -> Option<Arc<Hub>> {
        HUB.with(|h| h.borrow().clone())
    }

    /// Route this thread's touches to `hub` while `f` runs.
    pub fn with_hub<R>(hub: &Arc<Hub>, f: impl FnOnce() -> R) -> R {
        HUB.with(|h| *h.borrow_mut() = Some(Arc::clone(hub)));
        let r = f();
        HUB.with(|h| *h.borrow_mut() = None);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::hub::{EndReason, ReadOut, SubscribeReq, Wake};
    use super::*;
    use crate::auth::{Kind, PeerKey};
    use futures_util::FutureExt;
    use serde_json::json;
    use std::time::Instant;

    /// The production wiring: bus, cache and auth each reach the hub.
    #[test]
    fn wire_connects_bus_cache_and_auth() {
        let state = Arc::new(AppState::new());
        state.auth.set_password("pw");
        let bus = EventBus::new();
        wire(&state, &bus);
        let hub = &state.hub;
        // The firmware names are in the `ubus listen` filter.
        let patterns = bus.listen_patterns();
        assert!(BUS_EVENTS[..3].iter().all(|n| patterns.iter().any(|p| p == n)), "{patterns:?}");
        bus.publish("zwrt_wms_status_event", &json!({"wms_status": "new_sms_received"}));
        assert_eq!(hub.peek(Topic::Sms).value.as_deref(), Some(&json!({"received": 1})));

        let id = state.auth.login("pw", Kind::Bearer, PeerKey::UNKNOWN, &[]).unwrap().id;
        let topics = [Topic::Wan].into_iter().collect();
        let mut sub = hub.subscribe(SubscribeReq { owner: id, topics, signal_hint: None, replace_same: None }).unwrap();
        hub.store(Topic::Wan, ReadOut { value: Some(json!({})), error: None }, Instant::now());
        let later = Instant::now() + Duration::from_secs(3600);
        assert_eq!(hub.due_job(Lane::Ubus, later), Err(None));
        state.cache.invalidate(crate::cache::WAN);
        assert_eq!(hub.due_job(Lane::Ubus, Instant::now() + Duration::from_millis(500)), Ok(Topic::Wan));

        assert!(state.auth.logout_id(id));
        assert_eq!(hub.demand(Topic::Wan), 0);
        assert_eq!(sub.changed().now_or_never(), Some(Wake::Ended(EndReason::LoggedOut)));
    }

    #[test]
    fn names_round_trip_and_specs_line_up() {
        for t in Topic::ALL {
            assert_eq!(Topic::from_name(t.name()), Some(t));
            assert_eq!(Topic::ALL[t as usize], t);
        }
        assert_eq!(Topic::from_name("wifi"), None);
        assert_eq!(Topic::from_name("Signal"), None);
        assert_eq!(Topic::Signal.spec().cadence, Some(Duration::from_secs(2)));
        assert!(Topic::Signal.spec().always_bump);
        assert_eq!(Topic::Traffic.spec().cadence, Some(Duration::from_secs(1)));
        assert_eq!(Topic::Wan.spec().max_age_on_subscribe, Some(Duration::from_secs(30)));
        assert_eq!(Topic::Charger.spec().max_age_on_subscribe, Some(Duration::from_secs(10)));
        assert_eq!((Topic::Sms.spec().lane, Topic::PowerState.spec().lane), (Lane::None, Lane::None));
        assert_eq!(Topic::Health.spec().lane, Lane::Fast);
        assert_eq!(Topic::Health.spec().cadence, Some(Duration::from_secs(2)));
        assert_eq!(Topic::Battery.spec().cadence, Some(Duration::from_secs(5)));
        assert_eq!((Topic::Processes.spec().lane, Topic::Processes.spec().cadence), (Lane::Fast, Some(Duration::from_secs(3))));
        let usb = Topic::Usb.spec();
        assert_eq!((usb.lane, usb.cadence, usb.max_age_on_subscribe), (Lane::Ubus, Some(Duration::from_secs(10)), Some(Duration::from_secs(10))));
        // Wi-Fi status forks and carries keys: never a topic.
        assert!(!NAMES.contains("wifi"));
        let all: Vec<_> = Topic::ALL.iter().map(|t| t.name()).collect();
        assert_eq!(
            all,
            [
                "signal", "traffic", "battery", "health", "clients", "wan", "sim", "modem", "charger",
                "battery_info", "sim_identity", "service", "power_state", "sms", "esim_switch", "esim_download",
                "processes", "usb", "speedtest", "netscan", "signal_detect", "stk",
            ]
        );
        let st = Topic::Speedtest.spec();
        assert_eq!((st.lane, st.cadence, st.max_age_on_subscribe), (Lane::Fast, None, None));
        // The web UI's scan and signal detection pages: ubus, 2 s, only while watched.
        for t in [Topic::Netscan, Topic::SignalDetect] {
            let s = t.spec();
            assert_eq!((s.lane, s.cadence, s.max_age_on_subscribe, s.always_bump), (Lane::Ubus, Some(Duration::from_secs(2)), None, false));
        }
        // SIM Services: the toolkit worker's memory, re-read only when it changes.
        let stk = Topic::Stk.spec();
        assert_eq!((stk.lane, stk.cadence, stk.max_age_on_subscribe), (Lane::Fast, None, None));
        assert_eq!(NAME_END[Topic::COUNT - 1] as usize, NAMES.len());
    }

    /// `processes` scans `/proc` on hub-fast every 3 s, and only while a
    /// stream wants it: nothing is due (no timer) before or after.
    #[test]
    fn processes_are_sampled_only_while_subscribed() {
        let hub = Arc::new(Hub::new(Arc::new(crate::boottime::BootClock)));
        let far = Instant::now() + Duration::from_secs(3600);
        assert_eq!(hub.due_job(Lane::Fast, far), Err(None));
        let topics = [Topic::Processes].into_iter().collect();
        let owner = crate::auth::SessionId::for_test(1);
        let sub = hub.subscribe(SubscribeReq { owner, topics, signal_hint: None, replace_same: None }).unwrap();
        assert_eq!(hub.due_job(Lane::Fast, Instant::now()), Ok(Topic::Processes));
        assert_eq!(hub.due_job(Lane::Ubus, far), Err(None));
        let read = Instant::now();
        hub.store(Topic::Processes, ReadOut { value: Some(json!({"processes": []})), error: None }, read);
        match hub.due_job(Lane::Fast, Instant::now()) {
            Err(Some(next)) => assert_eq!(next.saturating_duration_since(read).as_secs(), 3),
            other => panic!("{other:?}"),
        }
        drop(sub);
        assert_eq!(hub.demand(Topic::Processes), 0);
        assert_eq!(hub.due_job(Lane::Fast, far), Err(None));
    }

    #[test]
    fn topic_set() {
        let set: TopicSet = [Topic::Sms, Topic::Signal, Topic::Sms].into_iter().collect();
        assert!(set.contains(Topic::Signal) && set.contains(Topic::Sms) && !set.contains(Topic::Wan));
        assert_eq!(set.iter().collect::<Vec<_>>(), [Topic::Signal, Topic::Sms]);
        assert!(TopicSet::default().is_empty() && !set.is_empty());
    }
}
