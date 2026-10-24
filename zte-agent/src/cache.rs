//! Per-key single-flight cache for `ubus` reads.
//!
//! Every `ubus call` forks the CLI (5-30 ms of CPU on this SoC) and the apps
//! poll a handful of slow-changing objects on every dashboard tick, so a short
//! TTL per key collapses overlapping and consecutive reads into one fork.
//!
//! - One fetch in flight per key: concurrent callers wait on the key's
//!   single-flight gate and share the result instead of forking again.
//! - `Err` is never cached (and neither is an empty `Null` reply).
//! - `invalidate(key)` drops the entry. `invalidate_with_settle(key, window)`
//!   additionally caps the effective TTL at [`SETTLE_TTL`] for `window`, so the
//!   2 s post-write confirmation loops in both apps keep tracking a daemon
//!   that applies the change asynchronously. Neither waits on an in-flight
//!   fetch: the fork runs under the gate, not the state lock, and a result
//!   that started before an invalidation is discarded instead of stored.
//! - The event threads started by [`UbusCache::start_event_invalidation`] map
//!   `ubus listen` events to the keys they make stale.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::ubus;

pub type Key = &'static str;

pub const CLIENTS: Key = "network/clients";
pub const CLIENTS_TTL: Duration = Duration::from_secs(5);
pub const SPEEDS: Key = "network/speeds";
/// Must stay below the fixed 1 s iOS traffic poll so that poller always misses.
pub const SPEEDS_TTL: Duration = Duration::from_millis(800);
pub const WAN: Key = "network/wan";
pub const WAN6: Key = "network/wan6";
pub const WAN_TTL: Duration = Duration::from_secs(30);
pub const BATTERY_INFO: Key = "device/battery-info";
pub const BATTERY_INFO_TTL: Duration = Duration::from_secs(10);
pub const CHARGER: Key = "device/charger";
pub const CHARGER_TTL: Duration = Duration::from_secs(10);
pub const MODEM_DATA: Key = "modem/data";
pub const MODEM_DATA_TTL: Duration = Duration::from_secs(30);
pub const SIM_INFO: Key = "sim/info";
pub const SIM_INFO_TTL: Duration = Duration::from_secs(30);
pub const NETINFO: Key = "network/signal";
/// Below the 1 s floor of the iOS signal poll slider.
pub const NETINFO_TTL: Duration = Duration::from_millis(500);

/// How long after a local write or a daemon event reads stay capped at
/// [`SETTLE_TTL`].
pub const SETTLE_WINDOW: Duration = Duration::from_secs(20);
pub const SETTLE_TTL: Duration = Duration::from_secs(1);

/// `ubus listen` events that invalidate cache keys; subscribe to each in
/// `main` before `EventBus::start` and hand the receivers to
/// [`UbusCache::start_event_invalidation`].
pub const EVENTS: &[Key] = &[
    "router_event_wan_connect_status",
    "network.interface",
    "BSP_CHARGER_EVENT",
    "BSP_POWERBANK_EVENT",
    "zwrt_card_state_event",
];

#[derive(Default)]
struct Slot {
    /// Value and the instant its fetch started.
    value: Option<(Instant, Value)>,
    settle_until: Option<Instant>,
    /// Bumped by every invalidation. A fetch stores its result only if the
    /// generation it started under is still current, so an invalidation never
    /// has to wait for the fork to finish.
    generation: u64,
}

impl Slot {
    fn effective_ttl(&self, ttl: Duration, now: Instant) -> Duration {
        match self.settle_until {
            Some(until) if now < until => ttl.min(SETTLE_TTL),
            _ => ttl,
        }
    }

    fn fresh(&self, ttl: Duration, now: Instant) -> Option<&Value> {
        let (at, value) = self.value.as_ref()?;
        (now.saturating_duration_since(*at) < self.effective_ttl(ttl, now)).then_some(value)
    }
}

/// Per-key state and single-flight gate. `inflight` is held across the fork
/// so concurrent readers share one fetch; `state` is only ever held for a few
/// instructions, so invalidation and fresh reads never block behind a fork.
/// Lock order: `inflight` before `state`, never the reverse.
#[derive(Default)]
struct Entry {
    state: Mutex<Slot>,
    inflight: Mutex<()>,
}

impl Entry {
    fn state(&self) -> MutexGuard<'_, Slot> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn invalidate(&self, settle: Option<Duration>) {
        let mut slot = self.state();
        slot.value = None;
        slot.generation = slot.generation.wrapping_add(1);
        if let Some(window) = settle {
            slot.settle_until = Some(Instant::now() + window);
        }
    }
}

/// Told about every invalidation (key, settle window), after the key's
/// lock is released (the event hub).
type Observer = Box<dyn Fn(Key, Option<Duration>) + Send + Sync>;

pub struct UbusCache {
    slots: Mutex<HashMap<Key, Arc<Entry>>>,
    observer: OnceLock<Observer>,
}

impl UbusCache {
    pub fn new() -> Self {
        UbusCache {
            slots: Mutex::new(HashMap::new()),
            observer: OnceLock::new(),
        }
    }

    /// Set once at startup; a second call is ignored.
    pub fn set_observer(&self, f: Observer) {
        let _ = self.observer.set(f);
    }

    fn observe(&self, key: Key, settle: Option<Duration>) {
        if let Some(f) = self.observer.get() {
            f(key, settle);
        }
    }

    fn slot(&self, key: Key) -> Arc<Entry> {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        Arc::clone(slots.entry(key).or_default())
    }

    /// Return the value cached under `key` if it is younger than `ttl`;
    /// otherwise run `fetch` (serialised per key) and cache its `Ok`.
    ///
    /// `Err` is handed back untouched and never stored, which also lets a
    /// caller return a value it does not want cached. `Duration::ZERO` always
    /// fetches but still refreshes the entry for other readers.
    pub fn get_or_fetch<E>(
        &self,
        key: Key,
        ttl: Duration,
        fetch: impl FnOnce() -> Result<Value, E>,
    ) -> Result<Value, E> {
        crate::httpd::gate::debug_assert_may_block();
        let entry = self.slot(key);
        if let Some(value) = entry.state().fresh(ttl, Instant::now()) {
            return Ok(value.clone());
        }
        // Serialise forks per key. Whoever waited here re-checks the slot, so
        // callers that overlapped an in-flight fetch share its result.
        let _gate = entry.inflight.lock().unwrap_or_else(|p| p.into_inner());
        let started = Instant::now();
        let generation = {
            let slot = entry.state();
            if let Some(value) = slot.fresh(ttl, started) {
                return Ok(value.clone());
            }
            slot.generation
        };
        let value = fetch()?;
        if !value.is_null() {
            let mut slot = entry.state();
            if slot.generation == generation {
                slot.value = Some((started, value.clone()));
            }
        }
        Ok(value)
    }

    /// Drop the entry so the next read forks. Does not wait for an in-flight
    /// fetch of the same key: its result is still returned to that caller but
    /// never stored, so a value that predates the trigger cannot outlive it.
    pub fn invalidate(&self, key: Key) {
        self.slot(key).invalidate(None);
        self.observe(key, None);
    }

    /// [`invalidate`](Self::invalidate) and cap the effective TTL at
    /// [`SETTLE_TTL`] for `window`.
    pub fn invalidate_with_settle(&self, key: Key, window: Duration) {
        self.slot(key).invalidate(Some(window));
        self.observe(key, Some(window));
    }

    /// `zte_nwinfo_api nwinfo_get_netinfo`, memoised for `max_age`. The
    /// Live Activity pusher passes `Duration::ZERO` so its sample time stays
    /// honest; HTTP readers pass [`NETINFO_TTL`].
    pub fn netinfo(&self, max_age: Duration) -> Result<Value, String> {
        self.get_or_fetch(NETINFO, max_age, || {
            ubus::call("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}"))
        })
    }

    /// `zwrt_bsp.charger list`, shared by the charger and charge-control
    /// readers. The charge-limit enforcer should pass `Duration::ZERO` so it
    /// always sees fresh state while still refreshing the entry.
    pub fn charger_list(&self, max_age: Duration) -> Result<Value, String> {
        self.get_or_fetch(CHARGER, max_age, || {
            ubus::call("zwrt_bsp.charger", "list", Some("{}"))
        })
    }

    /// Map a `ubus listen` event to the keys it makes stale.
    pub fn on_event(&self, name: &str, payload: &Value) {
        match name {
            "router_event_wan_connect_status" => {
                self.invalidate(WAN);
                self.invalidate(WAN6);
                self.invalidate_with_settle(MODEM_DATA, SETTLE_WINDOW);
            }
            "network.interface" => {
                let key = match payload["interface"].as_str() {
                    Some("zte_wan") => WAN,
                    Some("zte_wan6") => WAN6,
                    _ => return,
                };
                self.invalidate(key);
                self.invalidate_with_settle(MODEM_DATA, SETTLE_WINDOW);
            }
            "BSP_CHARGER_EVENT" => {
                self.invalidate(BATTERY_INFO);
                self.invalidate(CHARGER);
            }
            "BSP_POWERBANK_EVENT" => self.invalidate(CHARGER),
            "zwrt_card_state_event" => self.invalidate(SIM_INFO),
            _ => {}
        }
    }

    /// A SIM identity phase (`sim_identity`): the WAN and SIM values may belong to the old
    /// identity. After `changed` the SIM entry also settles, as the number and operator name
    /// fill in a few seconds late. Called before the phase becomes visible to the apps.
    pub fn on_sim_identity(&self, phase: &str) {
        for key in [MODEM_DATA, WAN, WAN6] {
            self.invalidate(key);
        }
        if phase == "changed" {
            self.invalidate_with_settle(SIM_INFO, SETTLE_WINDOW);
        } else {
            self.invalidate(SIM_INFO);
        }
    }

    /// One parked thread per subscription; each wakes only when its event
    /// fires and calls [`on_event`](Self::on_event).
    pub fn start_event_invalidation(self: Arc<Self>, events: Vec<(Key, Receiver<Value>)>) {
        for (name, rx) in events {
            let cache = Arc::clone(&self);
            let thread_name = format!("cache-{}", short_event_name(name));
            let spawned = std::thread::Builder::new()
                .name(thread_name)
                .spawn(move || {
                    for payload in rx.iter() {
                        cache.on_event(name, &payload);
                    }
                    eprintln!("[cache] {name} subscription closed");
                });
            if let Err(e) = spawned {
                eprintln!("[cache] failed to spawn {name} invalidator: {e}");
            }
        }
    }

    #[cfg(test)]
    fn peek(&self, key: Key) -> Option<Value> {
        let entry = self.slot(key);
        let slot = entry.state();
        slot.value.as_ref().map(|(_, v)| v.clone())
    }
}

/// Thread names are limited to 15 bytes on Linux.
fn short_event_name(name: &str) -> &'static str {
    match name {
        "router_event_wan_connect_status" => "wan",
        "network.interface" => "netif",
        "BSP_CHARGER_EVENT" => "charger",
        "BSP_POWERBANK_EVENT" => "powerbank",
        "zwrt_card_state_event" => "sim",
        _ => "event",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Fetch that counts its calls and returns the call number.
    fn counting(calls: &AtomicUsize) -> impl FnOnce() -> Result<Value, String> + '_ {
        move || Ok(json!(calls.fetch_add(1, Ordering::SeqCst) + 1))
    }

    #[test]
    fn serves_within_ttl_and_refetches_after_expiry() {
        let cache = UbusCache::new();
        let calls = AtomicUsize::new(0);
        let ttl = Duration::from_millis(60);
        let first = cache.get_or_fetch("k", ttl, counting(&calls)).unwrap();
        let second = cache.get_or_fetch("k", ttl, counting(&calls)).unwrap();
        assert_eq!(first, json!(1));
        assert_eq!(second, first);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        std::thread::sleep(Duration::from_millis(90));
        let third = cache.get_or_fetch("k", ttl, counting(&calls)).unwrap();
        assert_eq!(third, json!(2));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn zero_max_age_always_fetches_but_refreshes_for_others() {
        let cache = UbusCache::new();
        let calls = AtomicUsize::new(0);
        cache
            .get_or_fetch("k", Duration::ZERO, counting(&calls))
            .unwrap();
        cache
            .get_or_fetch("k", Duration::ZERO, counting(&calls))
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // A TTL reader right after sees the value the ZERO reader stored.
        let v = cache
            .get_or_fetch("k", Duration::from_secs(30), counting(&calls))
            .unwrap();
        assert_eq!(v, json!(2));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn errors_and_null_are_not_cached() {
        let cache = UbusCache::new();
        let ttl = Duration::from_secs(30);
        let err: Result<Value, String> = cache.get_or_fetch("k", ttl, || Err("boom".to_string()));
        assert_eq!(err, Err("boom".to_string()));
        assert_eq!(cache.peek("k"), None);

        let null: Result<Value, String> = cache.get_or_fetch("k", ttl, || Ok(Value::Null));
        assert_eq!(null, Ok(Value::Null));
        assert_eq!(cache.peek("k"), None);

        // An Err after a good value leaves the old entry (it is served until
        // it expires, exactly as if the failed call had never happened).
        let calls = AtomicUsize::new(0);
        cache.get_or_fetch("k", ttl, counting(&calls)).unwrap();
        let _: Result<Value, String> = cache.get_or_fetch("k", Duration::ZERO, || Err("x".into()));
        assert_eq!(cache.peek("k"), Some(json!(1)));
    }

    #[test]
    fn err_passthrough_returns_uncached_value() {
        // Callers can use Err to hand back a value they do not want cached.
        let cache = UbusCache::new();
        let v: Result<Value, Value> =
            cache.get_or_fetch("k", Duration::from_secs(5), || Err(json!({"hosts": null})));
        assert_eq!(v.unwrap_or_else(|v| v), json!({"hosts": null}));
        assert_eq!(cache.peek("k"), None);
    }

    #[test]
    fn concurrent_callers_share_one_fetch() {
        let cache = Arc::new(UbusCache::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let calls = Arc::clone(&calls);
                std::thread::spawn(move || {
                    cache
                        .get_or_fetch("k", Duration::from_secs(5), || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(50));
                            Ok::<_, String>(json!("shared"))
                        })
                        .unwrap()
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), json!("shared"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn keys_are_independent() {
        let cache = UbusCache::new();
        let a = AtomicUsize::new(0);
        let b = AtomicUsize::new(0);
        let ttl = Duration::from_secs(30);
        cache.get_or_fetch("a", ttl, counting(&a)).unwrap();
        cache.get_or_fetch("b", ttl, counting(&b)).unwrap();
        cache.invalidate("a");
        assert_eq!(cache.peek("a"), None);
        assert_eq!(cache.peek("b"), Some(json!(1)));
        cache.get_or_fetch("a", ttl, counting(&a)).unwrap();
        cache.get_or_fetch("b", ttl, counting(&b)).unwrap();
        assert_eq!((a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)), (2, 1));
    }

    #[test]
    fn settle_window_caps_effective_ttl() {
        let ttl = Duration::from_secs(30);
        let t0 = Instant::now();
        let mut slot = Slot {
            value: Some((t0, json!(1))),
            settle_until: None,
            generation: 0,
        };
        assert!(slot.fresh(ttl, t0 + Duration::from_secs(29)).is_some());
        assert!(slot.fresh(ttl, t0 + Duration::from_secs(30)).is_none());

        slot.settle_until = Some(t0 + SETTLE_WINDOW);
        assert!(slot.fresh(ttl, t0 + Duration::from_millis(999)).is_some());
        assert!(slot.fresh(ttl, t0 + Duration::from_millis(1001)).is_none());
        // A TTL already below the cap is unchanged inside the window.
        assert!(slot
            .fresh(Duration::from_millis(500), t0 + Duration::from_millis(400))
            .is_some());
        assert!(slot
            .fresh(Duration::from_millis(500), t0 + Duration::from_millis(600))
            .is_none());

        // Once the window has passed the full TTL applies again.
        let t1 = t0 + SETTLE_WINDOW + Duration::from_secs(1);
        slot.value = Some((t1, json!(2)));
        assert!(slot.fresh(ttl, t1 + Duration::from_secs(10)).is_some());
    }

    #[test]
    fn invalidate_with_settle_drops_value_then_caches_briefly() {
        let cache = UbusCache::new();
        let calls = AtomicUsize::new(0);
        let ttl = Duration::from_secs(30);
        cache.get_or_fetch("k", ttl, counting(&calls)).unwrap();
        cache.invalidate_with_settle("k", SETTLE_WINDOW);
        assert_eq!(cache.peek("k"), None);
        let v = cache.get_or_fetch("k", ttl, counting(&calls)).unwrap();
        assert_eq!(v, json!(2));
        // Back-to-back reads inside the 1 s cap still share the fetch.
        let v = cache.get_or_fetch("k", ttl, counting(&calls)).unwrap();
        assert_eq!(v, json!(2));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Start a `ttl` fetch of `key` on another thread and park it until the
    /// returned `release` sender fires. Returns once the fetch is in flight.
    fn parked_fetch(
        cache: &Arc<UbusCache>,
        key: Key,
        ttl: Duration,
        result: Value,
    ) -> (
        std::sync::mpsc::Sender<()>,
        std::thread::JoinHandle<Result<Value, String>>,
    ) {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let cache = Arc::clone(cache);
        let handle = std::thread::spawn(move || {
            cache.get_or_fetch(key, ttl, || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(result)
            })
        });
        started_rx.recv().unwrap();
        (release_tx, handle)
    }

    /// Run `f` on a thread and report whether it finished within `limit`.
    fn finishes_within(limit: Duration, f: impl FnOnce() + Send + 'static) -> bool {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            f();
            let _ = done_tx.send(());
        });
        done_rx.recv_timeout(limit).is_ok()
    }

    #[test]
    fn invalidate_does_not_wait_for_inflight_fetch_and_discards_its_result() {
        let cache = Arc::new(UbusCache::new());
        let (release, fetcher) = parked_fetch(&cache, "k", Duration::from_secs(30), json!("stale"));

        let c = Arc::clone(&cache);
        assert!(
            finishes_within(Duration::from_secs(1), move || c.invalidate("k")),
            "invalidate blocked behind the in-flight fetch"
        );

        // The parked caller still receives what it fetched, but the value
        // predates the invalidation and must not land in the cache.
        release.send(()).unwrap();
        assert_eq!(fetcher.join().unwrap(), Ok(json!("stale")));
        assert_eq!(cache.peek("k"), None);

        // The next reader forks again rather than seeing the stale result.
        let calls = AtomicUsize::new(0);
        let v = cache
            .get_or_fetch("k", Duration::from_secs(30), counting(&calls))
            .unwrap();
        assert_eq!(v, json!(1));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn invalidate_with_settle_does_not_wait_for_inflight_fetch() {
        let cache = Arc::new(UbusCache::new());
        let (release, fetcher) = parked_fetch(&cache, "k", Duration::from_secs(30), json!("stale"));

        let c = Arc::clone(&cache);
        assert!(
            finishes_within(Duration::from_secs(1), move || c
                .invalidate_with_settle("k", SETTLE_WINDOW)),
            "invalidate_with_settle blocked behind the in-flight fetch"
        );

        release.send(()).unwrap();
        assert_eq!(fetcher.join().unwrap(), Ok(json!("stale")));
        assert_eq!(cache.peek("k"), None);

        // The settle cap applies to the refetch that follows.
        let calls = AtomicUsize::new(0);
        let ttl = Duration::from_secs(30);
        cache.get_or_fetch("k", ttl, counting(&calls)).unwrap();
        cache.get_or_fetch("k", ttl, counting(&calls)).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        std::thread::sleep(SETTLE_TTL + Duration::from_millis(50));
        cache.get_or_fetch("k", ttl, counting(&calls)).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fresh_reads_are_served_while_a_fetch_is_in_flight() {
        // A ZERO-ttl reader (e.g. the Live Activity pusher) forking must not
        // stall TTL readers that already have a fresh value.
        let cache = Arc::new(UbusCache::new());
        let calls = AtomicUsize::new(0);
        cache
            .get_or_fetch("k", Duration::from_secs(30), counting(&calls))
            .unwrap();
        let (release, fetcher) = parked_fetch(&cache, "k", Duration::ZERO, json!("new"));

        let c = Arc::clone(&cache);
        assert!(
            finishes_within(Duration::from_secs(1), move || {
                let v: Value = c
                    .get_or_fetch("k", Duration::from_secs(30), || {
                        Err::<_, String>("x".into())
                    })
                    .unwrap();
                assert_eq!(v, json!(1));
            }),
            "fresh read blocked behind the in-flight fetch"
        );

        release.send(()).unwrap();
        assert_eq!(fetcher.join().unwrap(), Ok(json!("new")));
        assert_eq!(cache.peek("k"), Some(json!("new")));
    }

    #[test]
    fn events_invalidate_the_right_keys() {
        let cache = UbusCache::new();
        let all = [
            WAN,
            WAN6,
            MODEM_DATA,
            BATTERY_INFO,
            CHARGER,
            SIM_INFO,
            NETINFO,
        ];
        let fill = |cache: &UbusCache| {
            for key in all {
                let _: Result<Value, String> =
                    cache.get_or_fetch(key, Duration::from_secs(30), || Ok(json!(key)));
            }
        };
        let dropped = |cache: &UbusCache| -> Vec<Key> {
            all.iter()
                .copied()
                .filter(|k| cache.peek(k).is_none())
                .collect()
        };

        fill(&cache);
        cache.on_event(
            "network.interface",
            &json!({"interface": "zte_wan", "action": "ifup"}),
        );
        assert_eq!(dropped(&cache), vec![WAN, MODEM_DATA]);

        fill(&cache);
        cache.on_event(
            "network.interface",
            &json!({"interface": "lan", "action": "ifup"}),
        );
        assert_eq!(dropped(&cache), Vec::<Key>::new());

        fill(&cache);
        cache.on_event(
            "router_event_wan_connect_status",
            &json!({"wan_status": "disconnected"}),
        );
        assert_eq!(dropped(&cache), vec![WAN, WAN6, MODEM_DATA]);

        fill(&cache);
        cache.on_event("BSP_CHARGER_EVENT", &json!({"charger_connect": 1}));
        assert_eq!(dropped(&cache), vec![BATTERY_INFO, CHARGER]);

        fill(&cache);
        cache.on_event("BSP_POWERBANK_EVENT", &json!({}));
        assert_eq!(dropped(&cache), vec![CHARGER]);

        fill(&cache);
        cache.on_event("zwrt_card_state_event", &json!({"sim_states": "sim ready"}));
        assert_eq!(dropped(&cache), vec![SIM_INFO]);

        fill(&cache);
        cache.on_event("zwrt_wms_status_event", &json!({}));
        assert_eq!(dropped(&cache), Vec::<Key>::new());

        for phase in ["leaving", "changed", "online", "stalled"] {
            fill(&cache);
            cache.on_sim_identity(phase);
            assert_eq!(dropped(&cache), vec![WAN, WAN6, MODEM_DATA, SIM_INFO], "{phase}");
        }
        // After `changed` the SIM entry stays near-fresh while the number and SPN fill in.
        fill(&cache);
        let slot = cache.slot(SIM_INFO);
        assert!(slot.state().settle_until.is_some());
        cache.on_sim_identity("online");
        let _: Result<Value, String> = cache.get_or_fetch(WAN, Duration::from_secs(30), || Ok(json!(1)));
        assert_eq!(cache.peek(WAN), Some(json!(1)), "online doesn't settle the WAN entries");
    }

    #[test]
    fn event_threads_invalidate_from_the_channel() {
        let cache = Arc::new(UbusCache::new());
        let _: Result<Value, String> =
            cache.get_or_fetch(SIM_INFO, Duration::from_secs(30), || Ok(json!("sim")));
        let (tx, rx) = std::sync::mpsc::channel();
        Arc::clone(&cache).start_event_invalidation(vec![("zwrt_card_state_event", rx)]);
        tx.send(json!({"sim_hotswap": "sim hot remove"})).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while cache.peek(SIM_INFO).is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(cache.peek(SIM_INFO), None);
    }
    #[test]
    fn observer_runs_after_the_key_lock_is_released() {
        let cache = Arc::new(UbusCache::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (c, log) = (Arc::downgrade(&cache), Arc::clone(&seen));
        let reentered = std::sync::atomic::AtomicBool::new(false);
        cache.set_observer(Box::new(move |key, settle| {
            log.lock().unwrap().push((key, settle));
            // Re-entrant (once): the slot that was just invalidated is free again.
            if key == WAN && !reentered.swap(true, std::sync::atomic::Ordering::SeqCst) {
                if let Some(c) = c.upgrade() {
                    c.invalidate(WAN);
                }
            }
        }));
        cache.invalidate_with_settle(MODEM_DATA, SETTLE_WINDOW);
        cache.on_event("zwrt_card_state_event", &json!({}));
        cache.invalidate(WAN);
        assert_eq!(
            *seen.lock().unwrap(),
            [(MODEM_DATA, Some(SETTLE_WINDOW)), (SIM_INFO, None), (WAN, None), (WAN, None)]
        );
    }
}
