//! The hub: the latest value of every topic, who is subscribed to what, and
//! when the sampler should read next.
//!
//! There is no per-subscriber queue. Each slot carries a version (the hub's
//! sequence number at its last change); a subscriber remembers the versions
//! it has sent and, when woken, takes only the latest value of each changed
//! topic. A slow client therefore never grows memory, and the `watch`
//! channel coalesces wakeups.
//!
//! `state` is a leaf lock: nothing is called while it is held, and a
//! poisoned lock is recovered. The sampler lanes park on their condvars,
//! without a timeout when no demanded topic of their lane has a due time;
//! a lane is notified only when one of its topics is demanded.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::watch;

use super::history::{HistPoint, SignalHistory};
use super::readers::Primed;
use super::{object, Lane, Topic, TopicSet};
use crate::auth::SessionId;
use crate::boottime::{BootInstant, Clock};
use crate::cache;

/// Live (not ended) streams at once.
pub const MAX_STREAMS: usize = 8;
/// Live streams per session or token; a newer one ends the oldest.
pub const MAX_PER_OWNER: usize = 3;
/// Physical entries: an ended stream keeps its entry until its connection
/// lets go of it (a stalled peer may hold it until TCP gives up). Each
/// subscription is held by one open connection, so with an entry per
/// connection ended streams never crowd out a newcomer.
const ENTRY_TABLE: usize = crate::httpd::MAX_CONNS;

pub const SIGNAL_CADENCE: Duration = Duration::from_secs(2);
pub const SIGNAL_HINT_MIN: Duration = Duration::from_secs(1);
pub const SIGNAL_HINT_MAX: Duration = Duration::from_secs(10);
/// A touched topic is re-read this long after the first touch, so a burst
/// of invalidations costs one read.
const TOUCH_COALESCE: Duration = Duration::from_millis(500);
/// Re-read interval inside a settle window (a daemon applying a write).
const SETTLE_REREAD: Duration = Duration::from_secs(2);
const ERR_BACKOFF_MIN: Duration = Duration::from_secs(5);
const ERR_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// A topic's latest state as a subscriber sees it.
#[derive(Clone, Debug)]
pub struct SlotView {
    pub version: u64,
    /// Boot clock time of the read (or push) that produced it.
    pub at: BootInstant,
    /// Boot clock time of the last read (or push) that returned a value: a
    /// failed read that keeps the old value moves `at`, not this.
    pub value_at: BootInstant,
    pub value: Option<Arc<Value>>,
    pub error: Option<Arc<str>>,
}

/// One read's result. Composite topics may carry a value and the first error.
#[derive(Debug, Default)]
pub struct ReadOut {
    pub value: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// `MAX_STREAMS` live streams (or no free entry).
    Full,
}

/// Why the hub ended a stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// A newer stream of the same session or token took its place.
    Evicted,
    /// Its session or token was revoked (logout, pushed out of the table).
    LoggedOut,
    /// The router went into (fast-boot) standby.
    Standby,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Wake {
    /// A subscribed topic has a newer version.
    Changed,
    Ended(EndReason),
}

pub struct SubscribeReq {
    pub owner: SessionId,
    pub topics: TopicSet,
    /// Requested signal cadence (clamped to 1-10 s); the fastest wins.
    pub signal_hint: Option<Duration>,
    /// `Some(key)`: a stream of the same owner, topics and key (a reconnect)
    /// replaces its predecessor at once, whose connection may be dead but
    /// unnoticed (a tailnet peer is tailscaled, which acknowledges everything).
    /// API streams use one key; web pages one per page load, so a reconnect
    /// replaces only its own page's stream, never another tab's.
    pub replace_same: Option<u64>,
}

/// When a subscribed topic was last read, and whether that read failed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TopicAge {
    pub topic: Topic,
    pub at: BootInstant,
    pub value_at: BootInstant,
    pub error: bool,
}

struct Slot {
    version: u64,
    at: BootInstant,
    value_at: BootInstant,
    value: Option<Arc<Value>>,
    error: Option<Arc<str>>,
    /// Start of the read that produced the stored value or error.
    read_started: Option<Instant>,
    /// When that read was stored.
    last_read: Option<Instant>,
    /// Invalidated since the last read; re-read at `pull_after` if demanded.
    stale: bool,
    touched_at: Option<Instant>,
    pull_after: Option<Instant>,
    settle_until: Option<Instant>,
    err_backoff: Duration,
    /// The last read failed and is retried after `err_backoff`.
    err_retry: bool,
    recheck_at: Option<Instant>,
    /// A snapshot showed an old read: the next value is sent even if equal,
    /// so subscribers learn its real age.
    announce: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Slot {
            version: 0,
            at: BootInstant::ZERO,
            value_at: BootInstant::ZERO,
            value: None,
            error: None,
            read_started: None,
            last_read: None,
            stale: false,
            touched_at: None,
            pull_after: None,
            settle_until: None,
            err_backoff: Duration::ZERO,
            err_retry: false,
            recheck_at: None,
            announce: false,
        }
    }
}

impl Slot {
    fn view(&self) -> SlotView {
        SlotView {
            version: self.version,
            at: self.at,
            value_at: self.value_at,
            value: self.value.clone(),
            error: self.error.clone(),
        }
    }

    fn has_data(&self) -> bool {
        self.value.is_some() || self.error.is_some()
    }
}

struct StreamEntry {
    owner: SessionId,
    topics: TopicSet,
    signal_hint: Option<Duration>,
    replace_same: Option<u64>,
    /// Opening order (the oldest is evicted first).
    serial: u64,
    /// Ended streams no longer count against the caps and hold no demand.
    ended: Option<EndReason>,
}

struct State {
    seq: u64,
    slots: [Slot; Topic::COUNT],
    demand: [u16; Topic::COUNT],
    signal_cadence: Duration,
    history: SignalHistory,
    sms_received: u64,
    streams: [Option<StreamEntry>; ENTRY_TABLE],
    next_serial: u64,
    /// Test override of every sampled cadence.
    #[cfg(test)]
    test_cadence: Option<Duration>,
}

/// Lanes to notify once the lock is released.
#[derive(Copy, Clone, Default)]
struct Lanes {
    fast: bool,
    ubus: bool,
}

impl Lanes {
    fn add(&mut self, lane: Lane) {
        match lane {
            Lane::Fast => self.fast = true,
            Lane::Ubus => self.ubus = true,
            Lane::None => {}
        }
    }
}

impl State {
    fn new() -> Self {
        State {
            seq: 0,
            slots: std::array::from_fn(|_| Slot::default()),
            demand: [0; Topic::COUNT],
            signal_cadence: SIGNAL_CADENCE,
            history: SignalHistory::new(),
            sms_received: 0,
            streams: std::array::from_fn(|_| None),
            next_serial: 0,
            #[cfg(test)]
            test_cadence: None,
        }
    }

    fn slot(&mut self, t: Topic) -> &mut Slot {
        &mut self.slots[t as usize]
    }

    fn cadence(&self, t: Topic) -> Option<Duration> {
        let base = t.spec().cadence?;
        #[cfg(test)]
        if let Some(c) = self.test_cadence {
            return Some(c);
        }
        Some(if t == Topic::Signal { self.signal_cadence } else { base })
    }

    fn live(&self) -> impl Iterator<Item = (usize, &StreamEntry)> {
        self.streams
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().filter(|e| e.ended.is_none()).map(|e| (i, e)))
    }

    fn release(&mut self, topics: TopicSet, lanes: &mut Lanes) {
        for t in topics.iter() {
            let d = &mut self.demand[t as usize];
            *d = d.saturating_sub(1);
            lanes.add(t.spec().lane);
        }
    }

    fn end_entry(&mut self, i: usize, reason: EndReason, lanes: &mut Lanes) {
        let Some(e) = self.streams[i].as_mut().filter(|e| e.ended.is_none()) else {
            return;
        };
        e.ended = Some(reason);
        let topics = e.topics;
        self.release(topics, lanes);
    }

    /// The fastest requested signal cadence of the live streams, clamped.
    fn recompute_signal_cadence(&mut self) {
        self.signal_cadence = self
            .live()
            .filter(|(_, e)| e.topics.contains(Topic::Signal))
            .map(|(_, e)| e.signal_hint.unwrap_or(SIGNAL_CADENCE).clamp(SIGNAL_HINT_MIN, SIGNAL_HINT_MAX))
            .min()
            .unwrap_or(SIGNAL_CADENCE);
    }

    /// When the sampler should next read `t`, if ever.
    fn due_time(&self, t: Topic, now: Instant) -> Option<Instant> {
        let s = &self.slots[t as usize];
        let cadence = self.cadence(t);
        let mut due: Option<Instant> = None;
        let mut at = |d: Instant| due = Some(due.map_or(d, |x| x.min(d)));
        match s.last_read {
            None => at(now),
            Some(last) => {
                if let Some(c) = cadence {
                    at(last + c);
                }
                if s.settle_until.is_some_and(|u| u > now) {
                    at(last + SETTLE_REREAD);
                }
                if s.err_retry && cadence.is_none() {
                    at(last + s.err_backoff);
                }
            }
        }
        if s.stale {
            at(s.pull_after.unwrap_or(now));
        }
        if let Some(r) = s.recheck_at {
            at(r);
        }
        due
    }
}

/// The earliest due topic of `lane`; otherwise the earliest due time, or
/// `None` when no demanded topic of the lane will ever be due (park untimed).
fn due_job_locked(st: &State, lane: Lane, now: Instant) -> Result<Topic, Option<Instant>> {
    let mut best: Option<(Instant, Topic)> = None;
    for t in Topic::ALL {
        if lane == Lane::None || t.spec().lane != lane || st.demand[t as usize] == 0 {
            continue;
        }
        if let Some(due) = st.due_time(t, now) {
            if best.is_none_or(|(b, _)| due < b) {
                best = Some((due, t));
            }
        }
    }
    match best {
        Some((due, t)) if due <= now => Ok(t),
        Some((due, _)) => Err(Some(due)),
        None => Err(None),
    }
}

/// Seconds until an eSIM download result is forgotten (`esim::Download::expire`
/// works on the wall clock), for a finished `GET /api/esim/download` view.
fn result_expiry(view: &Value, unix_now: u64) -> Option<Duration> {
    if !matches!(view["state"].as_str(), Some("done" | "failed" | "cancelled")) {
        return None;
    }
    let finished = view["finished_at"].as_u64()? as i64;
    let left = finished + crate::esim::RESULT_KEEP_SECS as i64 - unix_now as i64;
    Some(Duration::from_secs(left.clamp(1, crate::esim::RESULT_KEEP_SECS as i64) as u64 + 1))
}

pub struct Hub {
    state: Mutex<State>,
    wake_fast: Condvar,
    wake_ubus: Condvar,
    /// The hub's sequence number; any thread may send, no runtime needed.
    changed: watch::Sender<u64>,
    clock: Arc<dyn Clock>,
    /// CPU usage for `health`, with its own sample window.
    pub cpu: Primed,
    /// The process list for `processes`, with its own sample window.
    pub procs: Primed,
    #[cfg(test)]
    notified: std::sync::atomic::AtomicUsize,
}

impl Hub {
    /// Inert until subscribed to: no threads (`events::start` adds them).
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let hub = Hub {
            state: Mutex::new(State::new()),
            wake_fast: Condvar::new(),
            wake_ubus: Condvar::new(),
            changed: watch::Sender::new(0),
            clock,
            cpu: Primed::cpu(),
            procs: Primed::processes(),
            #[cfg(test)]
            notified: std::sync::atomic::AtomicUsize::new(0),
        };
        hub.set(Topic::Sms, object(&mut [("received", 0.into())]));
        hub
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self, st: &mut State, t: Topic) {
        self.wake_streams(st);
        st.slots[t as usize].version = st.seq;
    }

    /// A new sequence number: every stream wakes and looks at what concerns it.
    fn wake_streams(&self, st: &mut State) {
        st.seq += 1;
        self.changed.send_replace(st.seq);
    }

    fn notify(&self, lanes: Lanes) {
        for (on, cv) in [(lanes.fast, &self.wake_fast), (lanes.ubus, &self.wake_ubus)] {
            if on {
                #[cfg(test)]
                self.notified.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                cv.notify_all();
            }
        }
    }

    /// A pushed value (events): stored as a fresh read; a change only if it differs.
    pub fn set(&self, t: Topic, data: Value) {
        self.put(&mut self.lock(), t, data);
    }

    /// `set` under the lock. The push time is taken under it too, so a
    /// sampler read that started earlier (`next_job`) is always dropped.
    fn put(&self, st: &mut State, t: Topic, data: Value) {
        let now = Instant::now();
        let at = self.clock.now();
        let slot = st.slot(t);
        slot.read_started = Some(now);
        slot.last_read = Some(now);
        slot.at = at;
        slot.value_at = at;
        slot.stale = false;
        slot.pull_after = None;
        slot.err_backoff = Duration::ZERO;
        slot.err_retry = false;
        let announce = std::mem::take(&mut slot.announce);
        if slot.error.is_some() || slot.value.as_deref() != Some(&data) || announce {
            slot.value = Some(Arc::new(data));
            slot.error = None;
            self.bump(st, t);
        }
    }

    /// The topic's source changed: re-read it 0.5 s after the first touch
    /// (so a burst costs one read, and a cache entry from before the change
    /// has expired), but only if someone is subscribed. `settle`: keep
    /// re-reading every 2 s for that long.
    pub fn touch(&self, t: Topic, settle: Option<Duration>) {
        let now = Instant::now();
        let demanded = {
            let mut st = self.lock();
            let slot = st.slot(t);
            slot.stale = true;
            slot.touched_at = Some(now);
            slot.pull_after.get_or_insert(now + TOUCH_COALESCE);
            if let Some(window) = settle {
                slot.settle_until = Some(now + window);
            }
            st.demand[t as usize] > 0
        };
        if demanded {
            let mut lanes = Lanes::default();
            lanes.add(t.spec().lane);
            self.notify(lanes);
        }
    }

    /// Sessions were revoked: their streams end (`bye logged_out`) and give
    /// back their demand now, even if a stalled peer never reads the `bye`.
    pub fn end_sessions(&self, ids: &[SessionId]) {
        let mut lanes = Lanes::default();
        {
            let mut st = self.lock();
            for i in 0..ENTRY_TABLE {
                if st.streams[i].as_ref().is_some_and(|e| ids.contains(&e.owner)) {
                    st.end_entry(i, EndReason::LoggedOut, &mut lanes);
                }
            }
            st.recompute_signal_cadence();
            self.wake_streams(&mut st);
        }
        self.notify(lanes);
    }

    /// End every live stream (`bye`); new subscriptions are still accepted.
    pub fn end_all(&self, reason: EndReason) {
        let mut lanes = Lanes::default();
        {
            let mut st = self.lock();
            for i in 0..ENTRY_TABLE {
                st.end_entry(i, reason, &mut lanes);
            }
            st.recompute_signal_cadence();
            self.wake_streams(&mut st);
        }
        self.notify(lanes);
    }

    /// A page load shares its fresh read (same rules as a sampler read).
    pub fn offer(&self, t: Topic, out: ReadOut, started: Instant) {
        self.store(t, out, started);
    }

    pub fn peek(&self, t: Topic) -> SlotView {
        self.lock().slots[t as usize].view()
    }

    pub fn history(&self) -> Vec<HistPoint> {
        self.lock().history.to_vec()
    }

    /// The cadence each sampled topic of `topics` is read at right now.
    pub fn cadences(&self, topics: TopicSet) -> Vec<(Topic, Duration)> {
        let st = self.lock();
        topics.iter().filter_map(|t| Some((t, st.cadence(t)?))).collect()
    }

    pub fn on_bus_event(&self, name: &str, payload: &Value) {
        match name {
            "zwrt_servicestatus" => {
                // One without a `network_type` says nothing about service
                // (as in `oos_policy::service_from_event`): keep the last one.
                if let Some(nt) = payload["network_type"].as_str().filter(|s| !s.is_empty()) {
                    self.set(Topic::Service, object(&mut [("network_type", nt.into())]));
                }
                self.touch(Topic::Modem, None);
                // Not at once: the signal read's netinfo is cached for 0.5 s,
                // so an immediate read would show the values from before.
                self.touch(Topic::Signal, None);
            }
            "zwrt_device_power_state_event" => {
                let action = payload["action"].as_str().unwrap_or_default();
                let parameter = payload["parameter"].clone();
                self.set(Topic::PowerState, object(&mut [("action", action.into()), ("parameter", parameter)]));
                // mode_power_off (quicken|normal), _quickening, _quicken,
                // _charger_quicken, _charger: the router is going down or into
                // fast-boot standby, and Wi-Fi with it.
                if action.starts_with("mode_power_off") {
                    self.end_all(EndReason::Standby);
                }
            }
            "zwrt_wms_status_event" if payload["wms_status"] == "new_sms_received" => {
                let mut st = self.lock();
                st.sms_received += 1;
                let n = st.sms_received;
                self.put(&mut st, Topic::Sms, object(&mut [("received", n.into())]));
            }
            crate::sim_identity::EVENT => {
                let mut data = payload.clone();
                if let Some(obj) = data.as_object_mut() {
                    obj.insert("agent_instance".into(), crate::sim_identity::agent_instance().into());
                }
                self.set(Topic::SimIdentity, data);
                // The switch view carries the identity stamp.
                self.touch(Topic::EsimSwitch, None);
            }
            _ => {}
        }
    }

    pub fn on_cache_invalidated(&self, key: cache::Key, settle: Option<Duration>) {
        let t = match key {
            cache::WAN | cache::WAN6 => Topic::Wan,
            cache::MODEM_DATA => Topic::Modem,
            cache::SIM_INFO => Topic::Sim,
            cache::CHARGER => {
                // Charger and powerbank events; whether attaching a USB-C
                // cable raises one is not verified (the 10 s read covers it).
                self.touch(Topic::Usb, settle);
                Topic::Charger
            }
            cache::BATTERY_INFO => Topic::BatteryInfo,
            _ => return,
        };
        self.touch(t, settle);
    }

    pub fn subscribe(self: &Arc<Self>, req: SubscribeReq) -> Result<Subscription, Refused> {
        let now = Instant::now();
        let mut lanes = Lanes::default();
        let sub = {
            let mut st = self.lock();
            let live = st.live().count();
            let oldest_own = st
                .live()
                .filter(|(_, e)| e.owner == req.owner)
                .min_by_key(|(_, e)| e.serial)
                .map(|(i, _)| i);
            let own = st.live().filter(|(_, e)| e.owner == req.owner).count();
            let same = st
                .live()
                .find(|(_, e)| {
                    req.replace_same.is_some()
                        && e.replace_same == req.replace_same
                        && e.owner == req.owner
                        && e.topics == req.topics
                })
                .map(|(i, _)| i);
            let evict = same.or(oldest_own.filter(|_| own >= MAX_PER_OWNER));
            if live - usize::from(evict.is_some()) >= MAX_STREAMS {
                return Err(Refused::Full);
            }
            let Some(entry) = st.streams.iter().position(Option::is_none) else {
                return Err(Refused::Full);
            };
            if let Some(i) = evict {
                st.end_entry(i, EndReason::Evicted, &mut lanes);
                self.wake_streams(&mut st);
            }
            st.next_serial += 1;
            st.streams[entry] = Some(StreamEntry {
                owner: req.owner,
                topics: req.topics,
                signal_hint: req.signal_hint,
                replace_same: req.replace_same,
                serial: st.next_serial,
                ended: None,
            });
            let boot_now = self.clock.now();
            for t in req.topics.iter() {
                st.demand[t as usize] += 1;
                let spec = t.spec();
                if spec.lane == Lane::None {
                    continue;
                }
                lanes.add(spec.lane);
                let cadence = st.cadence(t);
                let slot = st.slot(t);
                let age = boot_now.saturating_duration_since(slot.at);
                let too_old = spec.max_age_on_subscribe.is_some_and(|max| age > max);
                // The snapshot will show an old read (nobody sampled it for
                // a while): its fresh read is sent even if equal.
                if slot.has_data() && (too_old || cadence.is_some_and(|c| age > 2 * c)) {
                    slot.announce = true;
                }
                // A pending eSIM result expiry is checked again: it runs on
                // the wall clock, `recheck_at` on one that stops in standby.
                if slot.last_read.is_none() || too_old || slot.recheck_at.is_some() {
                    slot.stale = true;
                    slot.pull_after = Some(now);
                }
            }
            st.recompute_signal_cadence();
            Subscription {
                hub: Arc::clone(self),
                entry,
                topics: req.topics,
                seen: [0; Topic::COUNT],
                // Marks the current sequence as seen: a store between here and
                // `snapshot` causes one empty wake, nothing is lost.
                rx: self.changed.subscribe(),
            }
        };
        self.notify(lanes);
        Ok(sub)
    }

    /// Block until a topic of `lane` is due (sampler threads only); returns
    /// it with the read's start, taken under the lock: a push after it wins.
    pub(crate) fn next_job(&self, lane: Lane) -> (Topic, Instant) {
        let cv = if lane == Lane::Fast { &self.wake_fast } else { &self.wake_ubus };
        let mut st = self.lock();
        loop {
            let now = Instant::now();
            st = match due_job_locked(&st, lane, now) {
                Ok(t) => return (t, now),
                // Nothing demanded (or nothing ever due): park without a timeout.
                Err(None) => cv.wait(st).unwrap_or_else(PoisonError::into_inner),
                Err(Some(at)) => {
                    let wait = at.saturating_duration_since(now);
                    cv.wait_timeout(st, wait).unwrap_or_else(PoisonError::into_inner).0
                }
            };
        }
    }

    /// Pure scheduling, for tests: what `next_job` would do at `now`.
    #[cfg(test)]
    pub(crate) fn due_job(&self, lane: Lane, now: Instant) -> Result<Topic, Option<Instant>> {
        due_job_locked(&self.lock(), lane, now)
    }

    pub(crate) fn store(&self, t: Topic, out: ReadOut, started: Instant) {
        self.store_at(t, Some(out), started, Instant::now());
    }

    /// A topic without a reader: counts as read, nothing changes.
    pub(crate) fn read_skipped(&self, t: Topic, started: Instant) {
        self.store_at(t, None, started, Instant::now());
    }

    fn store_at(&self, t: Topic, out: Option<ReadOut>, started: Instant, now: Instant) {
        let mut guard = self.lock();
        let st = &mut *guard;
        let at = self.clock.now();
        let slot = &mut st.slots[t as usize];
        // A newer read (or push) already landed.
        if slot.read_started.is_some_and(|r| r > started) {
            return;
        }
        slot.read_started = Some(started);
        slot.last_read = Some(now);
        slot.at = at;
        // An invalidation that arrived during the read keeps the topic stale;
        // the re-read waits like a touch, so a storm costs a read per 0.5 s.
        if slot.touched_at.is_none_or(|x| x <= started) {
            slot.stale = false;
            slot.pull_after = None;
        } else {
            slot.pull_after = Some(now + TOUCH_COALESCE);
        }
        let Some(out) = out else {
            return;
        };
        let error = out.error.map(Arc::<str>::from);
        // Any error backs off, including a composite with one failed part:
        // a pull topic is re-read after it, never at once. A failed read is
        // retried; a partial one (`wan` without `wan6`) until the same error
        // repeats, then only an invalidation re-reads it.
        slot.err_backoff = match error {
            Some(_) => (slot.err_backoff * 2).clamp(ERR_BACKOFF_MIN, ERR_BACKOFF_MAX),
            None => Duration::ZERO,
        };
        slot.err_retry = error.is_some() && (out.value.is_none() || slot.error != error);
        // Without a value the error backoff is in charge (no busy recheck).
        if out.value.is_none() {
            slot.recheck_at = None;
        }
        if out.value.is_some() {
            slot.value_at = at;
        }
        let announce = out.value.is_some() && std::mem::take(&mut slot.announce);
        let changed = match out.value {
            Some(v) => {
                if t == Topic::Signal {
                    st.history.push(at, &v);
                }
                if t == Topic::EsimDownload {
                    let unix_now = crate::conn_watchdog::unix_now();
                    slot.recheck_at = result_expiry(&v, unix_now).map(|d| now + d);
                }
                let changed = slot.value.as_deref() != Some(&v) || slot.error != error;
                if changed {
                    slot.value = Some(Arc::new(v));
                    slot.error = error;
                }
                changed
            }
            // A failed read keeps the last good value.
            None if error.is_some() => {
                let changed = slot.error != error;
                slot.error = error;
                changed
            }
            None => false,
        };
        if changed || announce || t.spec().always_bump {
            self.bump(st, t);
        }
    }

    #[cfg(test)]
    pub(crate) fn cadence(&self, t: Topic) -> Option<Duration> {
        self.lock().cadence(t)
    }

    #[cfg(test)]
    pub(crate) fn demand(&self, t: Topic) -> u16 {
        self.lock().demand[t as usize]
    }

    #[cfg(test)]
    pub(crate) fn notified(&self) -> usize {
        self.notified.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn set_test_cadence(&self, c: Duration) {
        self.lock().test_cadence = Some(c);
    }
}

/// One stream's interest in the hub. Dropping it (the client went away)
/// gives back its demand, which stops the sampling nobody else needs.
pub struct Subscription {
    hub: Arc<Hub>,
    entry: usize,
    topics: TopicSet,
    seen: [u64; Topic::COUNT],
    rx: watch::Receiver<u64>,
}

impl Subscription {
    /// Every subscribed topic that has a value or an error.
    pub fn snapshot(&mut self) -> Vec<(Topic, SlotView)> {
        self.collect(true)
    }

    /// Topics changed since the last snapshot or changes.
    pub fn changes(&mut self) -> Vec<(Topic, SlotView)> {
        self.collect(false)
    }

    fn collect(&mut self, all: bool) -> Vec<(Topic, SlotView)> {
        let st = self.hub.lock();
        let mut out = Vec::new();
        for t in self.topics.iter() {
            let slot = &st.slots[t as usize];
            if slot.has_data() && (all || slot.version > self.seen[t as usize]) {
                out.push((t, slot.view()));
            }
            self.seen[t as usize] = slot.version;
        }
        out
    }

    /// The hub's current read time of every subscribed topic with data (a
    /// re-read that found the same value updates it without a change).
    pub fn ages(&self) -> Vec<TopicAge> {
        let st = self.hub.lock();
        self.topics
            .iter()
            .map(|t| (t, &st.slots[t as usize]))
            .filter(|(_, s)| s.has_data())
            .map(|(topic, s)| TopicAge { topic, at: s.at, value_at: s.value_at, error: s.error.is_some() })
            .collect()
    }

    /// Wait until this stream is ended or one of its own topics changed;
    /// other hub changes never return. Cancel-safe.
    pub async fn changed(&mut self) -> Wake {
        loop {
            {
                let st = self.hub.lock();
                if let Some(reason) = st.streams[self.entry].as_ref().and_then(|e| e.ended) {
                    return Wake::Ended(reason);
                }
                if self.topics.iter().any(|t| st.slots[t as usize].version > self.seen[t as usize]) {
                    return Wake::Changed;
                }
            }
            if self.rx.changed().await.is_err() {
                // The hub outlives every subscription; never spin.
                std::future::pending::<()>().await;
            }
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut lanes = Lanes::default();
        {
            let mut st = self.hub.lock();
            if let Some(e) = st.streams[self.entry].take() {
                if e.ended.is_none() {
                    st.release(e.topics, &mut lanes);
                }
            }
            st.recompute_signal_cadence();
        }
        // Lanes recompute (and park untimed when nothing is left).
        self.hub.notify(lanes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boottime::FakeClock;
    use serde_json::json;
    use std::future::Future;
    use std::task::{Context, Poll};

    fn hub() -> (Arc<Hub>, Arc<FakeClock>) {
        let clock = FakeClock::at_secs(1000);
        (Arc::new(Hub::new(clock.clone())), clock)
    }

    fn owner(n: u64) -> SessionId {
        SessionId::for_test(n)
    }

    fn req(n: u64, topics: &[Topic]) -> SubscribeReq {
        SubscribeReq { owner: owner(n), topics: topics.iter().copied().collect(), signal_hint: None, replace_same: None }
    }

    fn ok(v: Value) -> ReadOut {
        ReadOut { value: Some(v), error: None }
    }

    fn err(e: &str) -> ReadOut {
        ReadOut { value: None, error: Some(e.into()) }
    }

    fn version(h: &Hub, t: Topic) -> u64 {
        h.lock().slots[t as usize].version
    }

    /// `changed()` polled once without a runtime: Some(wake) if ready now.
    fn poll_once(sub: &mut Subscription) -> Option<Wake> {
        let fut = sub.changed();
        let mut fut = std::pin::pin!(fut);
        let mut cx = Context::from_waker(std::task::Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(w) => Some(w),
            Poll::Pending => None,
        }
    }

    #[test]
    fn set_100_times_one_change_latest_value() {
        let (h, _) = hub();
        let mut sub = h.subscribe(req(1, &[Topic::PowerState])).unwrap();
        assert!(sub.snapshot().is_empty(), "power_state has no value until the first event");
        for i in 0..100 {
            h.set(Topic::PowerState, json!({"action": format!("a{i}")}));
        }
        assert_eq!(poll_once(&mut sub), Some(Wake::Changed));
        let changes = sub.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].1.value.as_deref(), Some(&json!({"action": "a99"})));
        assert_eq!(poll_once(&mut sub), None);
        assert!(sub.changes().is_empty());
    }

    #[test]
    fn unchanged_value_no_version_bump_except_signal() {
        let (h, _) = hub();
        let t0 = Instant::now();
        h.store(Topic::Battery, ok(json!({"capacity": 50})), t0);
        let v = version(&h, Topic::Battery);
        h.store(Topic::Battery, ok(json!({"capacity": 50})), Instant::now());
        assert_eq!(version(&h, Topic::Battery), v);
        h.store(Topic::Battery, ok(json!({"capacity": 51})), Instant::now());
        assert!(version(&h, Topic::Battery) > v);
        h.store(Topic::Signal, ok(json!({"lte_rsrp": -90})), Instant::now());
        let s = version(&h, Topic::Signal);
        h.store(Topic::Signal, ok(json!({"lte_rsrp": -90})), Instant::now());
        assert!(version(&h, Topic::Signal) > s, "every signal read is a chart point");
        h.set(Topic::Service, json!({"network_type": "LTE"}));
        let s = version(&h, Topic::Service);
        h.set(Topic::Service, json!({"network_type": "LTE"}));
        assert_eq!(version(&h, Topic::Service), s);
    }

    #[test]
    fn error_keeps_last_value() {
        let (h, clock) = hub();
        h.store(Topic::Wan, ok(json!({"up": true})), Instant::now());
        let (v, good) = (version(&h, Topic::Wan), clock.now());
        clock.advance(Duration::from_secs(7));
        h.store(Topic::Wan, err("ubus timeout"), Instant::now());
        let view = h.peek(Topic::Wan);
        assert_eq!(view.value.as_deref(), Some(&json!({"up": true})));
        assert_eq!(view.error.as_deref(), Some("ubus timeout"));
        assert!(view.version > v);
        // The failed read is the latest read; the value is still the old one's.
        assert_eq!((view.at, view.value_at), (clock.now(), good));
        // The same error again is not a change; a good read clears it.
        h.store(Topic::Wan, err("ubus timeout"), Instant::now());
        assert_eq!(version(&h, Topic::Wan), view.version);
        h.store(Topic::Wan, ok(json!({"up": true})), Instant::now());
        let after = h.peek(Topic::Wan);
        assert_eq!(after.error, None);
        assert!(after.version > view.version);
    }

    #[test]
    fn demand_counts_and_drop_releases() {
        let (h, _) = hub();
        let a = h.subscribe(req(1, &[Topic::Signal, Topic::Sms])).unwrap();
        let b = h.subscribe(req(2, &[Topic::Signal])).unwrap();
        assert_eq!((h.demand(Topic::Signal), h.demand(Topic::Sms)), (2, 1));
        drop(a);
        assert_eq!((h.demand(Topic::Signal), h.demand(Topic::Sms)), (1, 0));
        drop(b);
        assert_eq!(h.demand(Topic::Signal), 0);
        assert!(h.lock().streams.iter().all(Option::is_none));
    }

    #[test]
    fn touch_during_read_rereads() {
        let (h, _) = hub();
        let _sub = h.subscribe(req(1, &[Topic::Wan])).unwrap();
        let started = Instant::now();
        assert_eq!(h.due_job(Lane::Ubus, started), Ok(Topic::Wan));
        // The WAN changes while the read is running.
        h.touch(Topic::Wan, None);
        h.store(Topic::Wan, ok(json!({"up": false})), started);
        let now = Instant::now();
        // Not back-to-back: a storm of invalidations costs a read per 0.5 s.
        assert!(matches!(h.due_job(Lane::Ubus, now), Err(Some(at)) if at > now));
        let due = h.due_job(Lane::Ubus, now + TOUCH_COALESCE);
        assert_eq!(due, Ok(Topic::Wan), "the read predates the invalidation");
        // A read that starts after the touch settles it.
        let started = Instant::now();
        h.store(Topic::Wan, ok(json!({"up": true})), started);
        assert_eq!(h.due_job(Lane::Ubus, started + Duration::from_secs(3600)), Err(None));
    }

    #[test]
    fn older_read_never_replaces_newer() {
        let (h, _) = hub();
        let early = Instant::now();
        let late = early + Duration::from_millis(5);
        h.offer(Topic::Sim, ok(json!({"n": "new"})), late);
        h.store(Topic::Sim, ok(json!({"n": "old"})), early);
        assert_eq!(h.peek(Topic::Sim).value.as_deref(), Some(&json!({"n": "new"})));
        // A push counts as a read that started at the push.
        let started = Instant::now() - Duration::from_millis(1);
        h.set(Topic::Service, json!({"network_type": "ENDC"}));
        h.store(Topic::Service, ok(json!({"network_type": "LTE"})), started);
        assert_eq!(h.peek(Topic::Service).value.as_deref(), Some(&json!({"network_type": "ENDC"})));
    }

    #[test]
    fn push_after_the_sampler_took_the_job_wins() {
        let (h, _) = hub();
        let _sub = h.subscribe(req(1, &[Topic::Service])).unwrap();
        // The lane takes the job (its start is taken under the hub lock) and
        // reads a cached value from before the event that lands meanwhile.
        let (t, started) = h.next_job(Lane::Ubus);
        assert_eq!(t, Topic::Service);
        h.on_bus_event("zwrt_servicestatus", &json!({"network_type": "NO_SERVICE"}));
        h.store(t, ok(json!({"network_type": "ENDC"})), started);
        assert_eq!(h.peek(Topic::Service).value.as_deref(), Some(&json!({"network_type": "NO_SERVICE"})));
        // The push settled the topic: nothing left to read.
        assert_eq!(h.due_job(Lane::Ubus, Instant::now() + Duration::from_secs(3600)), Err(None));
    }

    #[test]
    fn failed_pull_read_retries_with_backoff() {
        let (h, _) = hub();
        let _sub = h.subscribe(req(1, &[Topic::Sim])).unwrap();
        let mut expect = [5u64, 10, 20, 40, 60, 60].into_iter();
        for _ in 0..6 {
            let t0 = Instant::now();
            h.store_at(Topic::Sim, Some(err("no sim")), t0, t0);
            let secs = expect.next().unwrap();
            assert_eq!(h.due_job(Lane::Ubus, t0), Err(Some(t0 + Duration::from_secs(secs))));
            assert_eq!(h.due_job(Lane::Ubus, t0 + Duration::from_secs(secs)), Ok(Topic::Sim));
        }
        // Reset on success: a pull topic with a good value is never due again by itself.
        let t0 = Instant::now();
        h.store_at(Topic::Sim, Some(ok(json!({"state": "ready"}))), t0, t0);
        assert_eq!(h.due_job(Lane::Ubus, t0 + Duration::from_secs(3600)), Err(None));
        h.store_at(Topic::Sim, Some(err("no sim")), t0, t0);
        assert_eq!(h.due_job(Lane::Ubus, t0), Err(Some(t0 + Duration::from_secs(5))));
    }

    #[test]
    fn partial_read_retried_until_the_same_error_repeats() {
        let (h, _) = hub();
        let _sub = h.subscribe(req(1, &[Topic::Wan])).unwrap();
        // `wan` read, `wan6` failed: the value is stored, the topic is not re-read at once.
        let partial = |e: &str| ReadOut { value: Some(json!({"wan": {"up": true}, "wan6": null})), error: Some(e.into()) };
        let t0 = Instant::now();
        h.store_at(Topic::Wan, Some(partial("not found")), t0, t0);
        assert_eq!(h.due_job(Lane::Ubus, t0), Err(Some(t0 + Duration::from_secs(5))));
        // The same failure again (no `zte_wan6` on this router): no timer;
        // the next invalidation re-reads it.
        let t1 = t0 + Duration::from_secs(5);
        h.store_at(Topic::Wan, Some(partial("not found")), t1, t1);
        assert_eq!(h.due_job(Lane::Ubus, t1 + Duration::from_secs(3600)), Err(None));
        let view = h.peek(Topic::Wan);
        assert_eq!((view.value.unwrap()["wan6"].clone(), view.error.as_deref()), (Value::Null, Some("not found")));
        // A different failure is retried, backing off further.
        h.store_at(Topic::Wan, Some(partial("timeout")), t1, t1);
        assert_eq!(h.due_job(Lane::Ubus, t1), Err(Some(t1 + Duration::from_secs(20))));
        let t2 = t1 + Duration::from_secs(20);
        h.store_at(Topic::Wan, Some(ok(json!({"wan": {"up": true}, "wan6": {"up": true}}))), t2, t2);
        assert_eq!(h.due_job(Lane::Ubus, t2 + Duration::from_secs(3600)), Err(None));
    }

    #[test]
    fn pull_topic_older_than_ttl_due_on_subscribe() {
        let (h, clock) = hub();
        let t0 = Instant::now();
        h.store_at(Topic::Wan, Some(ok(json!({"up": true}))), t0, t0);
        // Fresh enough: a new subscriber gets the stored value, no read.
        let sub = h.subscribe(req(1, &[Topic::Wan])).unwrap();
        assert_eq!(h.due_job(Lane::Ubus, Instant::now()), Err(None));
        drop(sub);
        clock.advance(Duration::from_secs(31));
        let _sub = h.subscribe(req(1, &[Topic::Wan])).unwrap();
        assert_eq!(h.due_job(Lane::Ubus, Instant::now()), Ok(Topic::Wan));
    }

    #[test]
    fn no_condvar_notify_at_zero_demand() {
        let (h, _) = hub();
        for t in Topic::ALL {
            h.touch(t, Some(Duration::from_secs(20)));
            h.set(t, json!({"x": 1}));
        }
        for key in [cache::WAN, cache::WAN6, cache::MODEM_DATA, cache::SIM_INFO, cache::CHARGER, cache::BATTERY_INFO] {
            h.on_cache_invalidated(key, Some(Duration::from_secs(20)));
        }
        h.on_bus_event("zwrt_servicestatus", &json!({"network_type": "NO_SERVICE"}));
        h.on_bus_event(crate::sim_identity::EVENT, &json!({"phase": "online"}));
        h.on_bus_event("zwrt_device_power_state_event", &json!({"action": "mode_power_off", "parameter": "quicken"}));
        h.end_sessions(&[owner(1)]);
        assert_eq!(h.notified(), 0);
        // Nothing demanded: every lane would park without a timeout.
        let far = Instant::now() + Duration::from_secs(86_400);
        assert_eq!((h.due_job(Lane::Fast, far), h.due_job(Lane::Ubus, far)), (Err(None), Err(None)));
        let _sub = h.subscribe(req(1, &[Topic::Modem])).unwrap();
        let n = h.notified();
        assert_eq!(n, 1, "subscribe notifies the Ubus lane only");
        h.touch(Topic::Modem, None);
        assert_eq!(h.notified(), n + 1);
        h.touch(Topic::Wan, None);
        assert_eq!(h.notified(), n + 1, "an undemanded topic notifies nobody");
    }

    /// A charger event (a cable attached, powerbank mode) re-reads `usb`
    /// as well as `charger`, 0.5 s later, while someone watches it.
    #[test]
    fn charger_events_reread_usb() {
        let (h, _) = hub();
        let _sub = h.subscribe(req(1, &[Topic::Usb])).unwrap();
        let read = Instant::now();
        h.store(Topic::Usb, ok(json!({"typec_cc": "no_cc"})), read);
        assert!(matches!(h.due_job(Lane::Ubus, read), Err(Some(_))));
        h.on_cache_invalidated(cache::CHARGER, None);
        let soon = Instant::now() + Duration::from_millis(500);
        assert_eq!(h.due_job(Lane::Ubus, soon), Ok(Topic::Usb));
    }

    #[test]
    fn owner_cap_evicts_oldest() {
        let (h, _) = hub();
        let mut subs: Vec<_> = (0..3).map(|_| h.subscribe(req(7, &[Topic::Sms])).unwrap()).collect();
        let other = h.subscribe(req(8, &[Topic::Sms])).unwrap();
        subs.iter_mut().for_each(|s| drop(s.snapshot()));
        // A fourth stream for the same owner: newest wins.
        let mut fourth = h.subscribe(req(7, &[Topic::Sms])).unwrap();
        fourth.snapshot();
        assert_eq!(poll_once(&mut subs[0]), Some(Wake::Ended(EndReason::Evicted)));
        // The others only wake (no session re-check).
        assert_eq!(poll_once(&mut subs[1]), None);
        assert_eq!(poll_once(&mut fourth), None);
        assert_eq!(h.demand(Topic::Sms), 4, "the evicted stream gave its demand back");
        assert_eq!(h.lock().live().filter(|(_, e)| e.owner == owner(7)).count(), 3);
        // The next newcomer evicts the next oldest.
        let _fifth = h.subscribe(req(7, &[Topic::Sms])).unwrap();
        assert_eq!(poll_once(&mut subs[1]), Some(Wake::Ended(EndReason::Evicted)));
        drop(other);
        drop(subs);
        assert_eq!(h.demand(Topic::Sms), 2);
    }

    #[test]
    fn eviction_when_table_full() {
        let (h, _) = hub();
        let mut mine: Vec<_> = (0..3).map(|_| h.subscribe(req(1, &[Topic::Sms])).unwrap()).collect();
        let others: Vec<_> = (2..7).map(|n| h.subscribe(req(n, &[Topic::Sms])).unwrap()).collect();
        assert_eq!(h.lock().live().count(), MAX_STREAMS);
        // Full, but this owner's newcomer replaces its own oldest stream.
        // Ended entries no longer count: evictions keep fitting while the
        // evicted streams (stalled peers) still hold their entries.
        while h.lock().streams.iter().any(Option::is_none) {
            mine.push(h.subscribe(req(1, &[Topic::Sms])).unwrap());
        }
        assert_eq!(h.lock().live().count(), MAX_STREAMS);
        assert_eq!(mine.len(), ENTRY_TABLE - 5);
        // No physical entry left (one per connection, so only in this test):
        // refused, and nothing is evicted.
        assert_eq!(h.subscribe(req(1, &[Topic::Sms])).err(), Some(Refused::Full));
        assert_eq!(h.lock().live().count(), MAX_STREAMS);
        mine.remove(0);
        assert!(h.subscribe(req(1, &[Topic::Sms])).is_ok());
        drop((others, mine));
    }

    #[test]
    fn reconnect_with_the_same_topics_replaces_its_predecessor() {
        let (h, _) = hub();
        let keyed = |n, key| SubscribeReq { replace_same: Some(key), ..req(n, &[Topic::Signal, Topic::Sms]) };
        let same = |n| keyed(n, 0);
        let mut old = h.subscribe(same(1)).unwrap();
        let mut other_topics = h.subscribe(SubscribeReq { replace_same: Some(0), ..req(1, &[Topic::Sms]) }).unwrap();
        let mut other_owner = h.subscribe(same(2)).unwrap();
        let mut other_key = h.subscribe(keyed(1, 7)).unwrap();
        let mut new = h.subscribe(same(1)).unwrap();
        for s in [&mut other_topics, &mut other_owner, &mut other_key, &mut new] {
            s.snapshot();
        }
        // At once, not only past three: the old connection may be dead but unnoticed.
        assert_eq!(poll_once(&mut old), Some(Wake::Ended(EndReason::Evicted)));
        assert_eq!(h.demand(Topic::Signal), 3, "the replaced stream gave its demand back");
        for s in [&mut other_topics, &mut other_owner, &mut other_key, &mut new] {
            assert_eq!(poll_once(s), None);
        }
        // Without `replace_same` identical streams coexist.
        let _tab1 = h.subscribe(req(3, &[Topic::Sms])).unwrap();
        let mut tab2 = h.subscribe(req(3, &[Topic::Sms])).unwrap();
        tab2.snapshot();
        assert_eq!(h.lock().live().filter(|(_, e)| e.owner == owner(3)).count(), 2);
    }

    #[test]
    fn global_full_refused() {
        let (h, _) = hub();
        let subs: Vec<_> = (0..MAX_STREAMS as u64).map(|n| h.subscribe(req(n, &[Topic::Sms])).unwrap()).collect();
        assert_eq!(h.subscribe(req(99, &[Topic::Signal])).err(), Some(Refused::Full));
        assert_eq!(h.demand(Topic::Signal), 0);
        drop(subs);
        assert!(h.subscribe(req(99, &[Topic::Signal])).is_ok());
    }

    #[test]
    fn end_all_standby_ends_every_stream() {
        let (h, _) = hub();
        let mut a = h.subscribe(req(1, &[Topic::Signal])).unwrap();
        let mut b = h.subscribe(req(2, &[Topic::Sms])).unwrap();
        h.on_bus_event("zwrt_device_power_state_event", &json!({"action": "mode_power_on", "parameter": "quicken"}));
        assert_eq!(poll_once(&mut a), None);
        h.on_bus_event("zwrt_device_power_state_event", &json!({"action": "mode_power_off_quicken"}));
        for s in [&mut a, &mut b] {
            assert_eq!(poll_once(s), Some(Wake::Ended(EndReason::Standby)));
        }
        // The sampler stops at once, before the streams let go of their entries.
        assert_eq!((h.demand(Topic::Signal), h.demand(Topic::Sms)), (0, 0));
        assert_eq!(h.due_job(Lane::Ubus, Instant::now()), Err(None));
        assert_eq!(
            h.peek(Topic::PowerState).value.as_deref(),
            Some(&json!({"action": "mode_power_off_quicken", "parameter": null}))
        );
        // New streams are still accepted (Wi-Fi can come back during standby).
        let mut c = h.subscribe(req(3, &[Topic::Sms])).unwrap();
        c.snapshot();
        assert_eq!(poll_once(&mut c), None);
        drop((a, b));
        assert_eq!(h.demand(Topic::Sms), 1);
    }

    #[test]
    fn settle_window_rereads_every_2s() {
        let (h, _) = hub();
        let _sub = h.subscribe(req(1, &[Topic::Modem])).unwrap();
        let t0 = Instant::now();
        h.store_at(Topic::Modem, Some(ok(json!({"data": 1}))), t0, t0);
        assert_eq!(h.due_job(Lane::Ubus, t0 + Duration::from_secs(600)), Err(None));
        // A local write: invalidated with a 20 s settle window.
        h.on_cache_invalidated(cache::MODEM_DATA, Some(Duration::from_secs(20)));
        let t1 = Instant::now();
        assert_eq!(h.due_job(Lane::Ubus, t1 + TOUCH_COALESCE), Ok(Topic::Modem));
        let mut last = t1 + TOUCH_COALESCE;
        h.store_at(Topic::Modem, Some(ok(json!({"data": 2}))), last, last);
        for _ in 0..5 {
            assert_eq!(h.due_job(Lane::Ubus, last), Err(Some(last + SETTLE_REREAD)));
            last += SETTLE_REREAD;
            assert_eq!(h.due_job(Lane::Ubus, last), Ok(Topic::Modem));
            h.store_at(Topic::Modem, Some(ok(json!({"data": 2}))), last, last);
        }
        // After the window: no more reads.
        let after = t1 + Duration::from_secs(21);
        h.store_at(Topic::Modem, Some(ok(json!({"data": 2}))), after, after);
        assert_eq!(h.due_job(Lane::Ubus, after + Duration::from_secs(600)), Err(None));
    }

    #[test]
    fn signal_cadence_is_min_of_hints_clamped() {
        let (h, _) = hub();
        assert_eq!(h.cadence(Topic::Signal), Some(SIGNAL_CADENCE));
        let hint = |n, ms| SubscribeReq { signal_hint: Some(Duration::from_millis(ms)), ..req(n, &[Topic::Signal]) };
        let slow = h.subscribe(hint(1, 9_000)).unwrap();
        assert_eq!(h.cadence(Topic::Signal), Some(Duration::from_secs(9)));
        let default = h.subscribe(req(2, &[Topic::Signal])).unwrap();
        assert_eq!(h.cadence(Topic::Signal), Some(SIGNAL_CADENCE));
        let fast = h.subscribe(hint(3, 200)).unwrap();
        assert_eq!(h.cadence(Topic::Signal), Some(SIGNAL_HINT_MIN));
        drop(fast);
        drop(default);
        assert_eq!(h.cadence(Topic::Signal), Some(Duration::from_secs(9)));
        let huge = h.subscribe(hint(4, 60_000)).unwrap();
        drop(slow);
        assert_eq!(h.cadence(Topic::Signal), Some(SIGNAL_HINT_MAX));
        // The sampler follows: due one cadence after the last read.
        let t0 = Instant::now();
        h.store_at(Topic::Signal, Some(ok(json!({}))), t0, t0);
        assert_eq!(h.due_job(Lane::Ubus, t0), Err(Some(t0 + SIGNAL_HINT_MAX)));
        drop(huge);
        assert_eq!(h.cadence(Topic::Signal), Some(SIGNAL_CADENCE));
        assert_eq!(h.cadence(Topic::Wan), None);
    }

    #[test]
    fn history_ring_120_and_boottime_stamps() {
        let (h, clock) = hub();
        for i in 0..121 {
            clock.advance(Duration::from_secs(2));
            h.store(Topic::Signal, ok(json!({"nr5g_rsrp": -80 - i / 2, "lte_rsrp": "-95"})), Instant::now());
        }
        // A failed read adds no point.
        h.store(Topic::Signal, err("ubus"), Instant::now());
        let points = h.history();
        assert_eq!(points.len(), 120);
        assert_eq!(points[0].at_ms, 1_004_000);
        assert_eq!(points[119].at_ms, 1_242_000);
        assert_eq!((points[119].nr_rsrp, points[119].lte_rsrp), (-140, -95));
    }

    #[test]
    fn sms_counter_only_on_new_sms_received() {
        let (h, _) = hub();
        assert_eq!(h.peek(Topic::Sms).value.as_deref(), Some(&json!({"received": 0})));
        let mut sub = h.subscribe(req(1, &[Topic::Sms])).unwrap();
        assert_eq!(sub.snapshot().len(), 1);
        for status in ["sms_send_success", "sms_storage_full", ""] {
            h.on_bus_event("zwrt_wms_status_event", &json!({"wms_status": status}));
        }
        assert_eq!(poll_once(&mut sub), None);
        h.on_bus_event("zwrt_wms_status_event", &json!({"wms_status": "new_sms_received"}));
        h.on_bus_event("zwrt_wms_status_event", &json!({"wms_status": "new_sms_received"}));
        assert_eq!(poll_once(&mut sub), Some(Wake::Changed));
        let changes = sub.changes();
        assert_eq!(changes[0].1.value.as_deref(), Some(&json!({"received": 2})));
    }

    #[test]
    fn sim_identity_event_touches_esim_switch() {
        let (h, _) = hub();
        let _sub = h.subscribe(req(1, &[Topic::EsimSwitch])).unwrap();
        let t0 = Instant::now();
        h.store_at(Topic::EsimSwitch, Some(ok(json!({"state": "idle"}))), t0, t0);
        assert_eq!(h.due_job(Lane::Fast, t0 + Duration::from_secs(3600)), Err(None));
        let payload = json!({"phase": "changed", "epoch": 3, "source": "agent", "iccid_tail": "0042"});
        h.on_bus_event(crate::sim_identity::EVENT, &payload);
        let v = h.peek(Topic::SimIdentity).value.unwrap();
        assert_eq!((v["phase"].as_str(), v["iccid_tail"].as_str()), (Some("changed"), Some("0042")));
        assert_eq!(v["agent_instance"], crate::sim_identity::agent_instance());
        assert_eq!(h.due_job(Lane::Fast, Instant::now() + TOUCH_COALESCE), Ok(Topic::EsimSwitch));
    }

    #[test]
    fn service_status_pushes_and_rereads_signal_after_the_netinfo_ttl() {
        let (h, _) = hub();
        let _sig = h.subscribe(req(2, &[Topic::Signal])).unwrap();
        let t1 = Instant::now();
        h.store_at(Topic::Signal, Some(ok(json!({}))), t1, t1);
        h.on_bus_event("zwrt_servicestatus", &json!({"network_type": "LIMITED_SERVICE_LTE"}));
        // Not at once (that would hit the 0.5 s netinfo cache from before
        // the event), but well before the 2 s cadence.
        let now = Instant::now();
        assert!(matches!(h.due_job(Lane::Ubus, now), Err(Some(at)) if at > now));
        assert_eq!(h.due_job(Lane::Ubus, now + crate::cache::NETINFO_TTL), Ok(Topic::Signal));
        assert_eq!(h.peek(Topic::Service).value.as_deref(), Some(&json!({"network_type": "LIMITED_SERVICE_LTE"})));
        // No `network_type`: the last service value stays (the signal is still re-read).
        let t2 = Instant::now();
        h.store_at(Topic::Signal, Some(ok(json!({}))), t2, t2);
        assert!(matches!(h.due_job(Lane::Ubus, t2), Err(Some(_))), "nothing due right now");
        h.on_bus_event("zwrt_servicestatus", &json!({"network_type": ""}));
        h.on_bus_event("zwrt_servicestatus", &json!({}));
        assert_eq!(h.peek(Topic::Service).value.as_deref(), Some(&json!({"network_type": "LIMITED_SERVICE_LTE"})));
        assert_eq!(h.due_job(Lane::Ubus, Instant::now() + TOUCH_COALESCE), Ok(Topic::Signal));
        // The modem is touched too.
        let _modem = h.subscribe(req(3, &[Topic::Modem])).unwrap();
        let t3 = Instant::now();
        h.store_at(Topic::Modem, Some(ok(json!({}))), t3, t3);
        h.store_at(Topic::Signal, Some(ok(json!({}))), t3, t3);
        h.on_bus_event("zwrt_servicestatus", &json!({"network_type": "ENDC"}));
        let mut due = Vec::new();
        let at = Instant::now() + TOUCH_COALESCE;
        while let Ok(t) = h.due_job(Lane::Ubus, at) {
            due.push(t);
            h.store_at(t, Some(ok(json!({}))), at, at);
        }
        due.sort_by_key(|t| *t as u8);
        assert_eq!(due, [Topic::Signal, Topic::Modem]);
    }

    #[test]
    fn service_reread_on_subscribe_when_old() {
        let (h, clock) = hub();
        h.set(Topic::Service, json!({"network_type": "LTE"}));
        let sub = h.subscribe(req(1, &[Topic::Service])).unwrap();
        assert_eq!(h.due_job(Lane::Ubus, Instant::now()), Err(None), "fresh: no read");
        drop(sub);
        // A missed event (`ubus listen` restarting) is corrected by the next subscriber.
        clock.advance(Duration::from_secs(31));
        let _sub = h.subscribe(req(1, &[Topic::Service])).unwrap();
        assert_eq!(h.due_job(Lane::Ubus, Instant::now()), Ok(Topic::Service));
    }

    #[test]
    fn real_power_actions_and_standby() {
        // As broadcast by the router (2026-10-21).
        let ends = [
            ("mode_power_off", "quicken"),
            ("mode_power_off", "normal"),
            ("mode_power_off_quickening", ""),
            ("mode_power_off_quicken", ""),
            ("mode_power_off_charger_quicken", ""),
            ("mode_power_off_charger", ""),
        ];
        let keeps = [
            ("zwrt_mode_lcd_shutdown", ""),
            ("mode_startup", ""),
            ("mode_initial", ""),
            ("mode_power_on", "quicken"),
            ("mode_power_on", "normal"),
        ];
        for (actions, ends_streams) in [(&ends[..], true), (&keeps[..], false)] {
            for &(action, parameter) in actions {
                let (h, _) = hub();
                let mut sub = h.subscribe(req(1, &[Topic::Sms])).unwrap();
                sub.snapshot();
                let mut payload = json!({"action": action});
                if !parameter.is_empty() {
                    payload["parameter"] = parameter.into();
                }
                h.on_bus_event("zwrt_device_power_state_event", &payload);
                let expect = ends_streams.then_some(Wake::Ended(EndReason::Standby));
                assert_eq!(poll_once(&mut sub), expect, "{action} {parameter}");
                assert_eq!(h.peek(Topic::PowerState).value.unwrap()["action"], action);
            }
        }
    }

    #[test]
    fn esim_download_rechecked_at_result_expiry() {
        let now = 2_000_000_000;
        let view = |state: &str, finished: u64| json!({"state": state, "finished_at": finished});
        assert_eq!(result_expiry(&view("done", now - 590), now), Some(Duration::from_secs(11)));
        assert_eq!(result_expiry(&view("failed", now - 700), now), Some(Duration::from_secs(2)));
        // Wall clock jumped back: at most the full keep time.
        assert_eq!(result_expiry(&view("cancelled", now + 3600), now), Some(Duration::from_secs(601)));
        assert_eq!(result_expiry(&view("running", now), now), None);
        assert_eq!(result_expiry(&view("idle", 0), now), None);

        let (h, _) = hub();
        let _sub = h.subscribe(req(1, &[Topic::EsimDownload])).unwrap();
        let unix = crate::conn_watchdog::unix_now();
        let t0 = Instant::now();
        h.store_at(Topic::EsimDownload, Some(ok(view("done", unix - 595))), t0, t0);
        match h.due_job(Lane::Fast, t0) {
            Err(Some(at)) => {
                let d = at - t0;
                assert!(d >= Duration::from_secs(5) && d <= Duration::from_secs(7), "{d:?}");
                assert_eq!(h.due_job(Lane::Fast, at), Ok(Topic::EsimDownload));
            }
            other => panic!("{other:?}"),
        }
        // Expired (idle again): nothing more to check.
        h.store_at(Topic::EsimDownload, Some(ok(view("idle", 0))), t0, t0);
        assert_eq!(h.due_job(Lane::Fast, t0 + Duration::from_secs(3600)), Err(None));
    }

    #[test]
    fn failed_esim_download_read_backs_off_instead_of_spinning() {
        let (h, _) = hub();
        let _sub = h.subscribe(req(1, &[Topic::EsimDownload])).unwrap();
        let unix = crate::conn_watchdog::unix_now();
        let t0 = Instant::now();
        h.store_at(Topic::EsimDownload, Some(ok(json!({"state": "done", "finished_at": unix - 599}))), t0, t0);
        let Err(Some(recheck)) = h.due_job(Lane::Fast, t0) else { panic!("no recheck") };
        // The recheck fails (a poisoned lock, say): retried after the backoff, not at once.
        h.store_at(Topic::EsimDownload, Some(err("internal error")), recheck, recheck);
        assert_eq!(h.due_job(Lane::Fast, recheck), Err(Some(recheck + ERR_BACKOFF_MIN)));
        assert_eq!(h.peek(Topic::EsimDownload).value.unwrap()["state"], "done");
    }

    #[test]
    fn pending_esim_result_expiry_rechecked_on_subscribe() {
        let (h, _) = hub();
        let sub = h.subscribe(req(1, &[Topic::EsimDownload])).unwrap();
        let unix = crate::conn_watchdog::unix_now();
        let t0 = Instant::now();
        h.store_at(Topic::EsimDownload, Some(ok(json!({"state": "done", "finished_at": unix - 300}))), t0, t0);
        assert!(matches!(h.due_job(Lane::Fast, t0), Err(Some(_))));
        drop(sub);
        // After a standby the wall clock may have passed the expiry while
        // `Instant` stood still: a new subscriber gets a fresh read.
        let _sub = h.subscribe(req(1, &[Topic::EsimDownload])).unwrap();
        assert_eq!(h.due_job(Lane::Fast, Instant::now()), Ok(Topic::EsimDownload));
        // Without a pending result nothing is read.
        h.store_at(Topic::EsimDownload, Some(ok(json!({"state": "idle"}))), t0, t0);
        let _again = h.subscribe(req(2, &[Topic::EsimDownload])).unwrap();
        assert_eq!(h.due_job(Lane::Fast, Instant::now()), Err(None));
    }

    #[test]
    fn store_between_subscribe_and_snapshot_delivered_once() {
        let (h, _) = hub();
        let mut sub = h.subscribe(req(1, &[Topic::PowerState])).unwrap();
        h.set(Topic::PowerState, json!({"action": "mode_startup"}));
        let snap = sub.snapshot();
        assert_eq!(snap.len(), 1);
        // The watch saw a change, but it was already in the snapshot.
        assert!(sub.rx.has_changed().unwrap());
        assert_eq!(poll_once(&mut sub), None);
        assert!(sub.changes().is_empty());
    }

    #[test]
    fn unrelated_changes_do_not_wake() {
        let (h, _) = hub();
        let mut sub = h.subscribe(req(1, &[Topic::Sms])).unwrap();
        sub.snapshot();
        h.set(Topic::PowerState, json!({"action": "zwrt_mode_lcd_shutdown"}));
        h.store(Topic::Battery, ok(json!({"capacity": 3})), Instant::now());
        h.end_sessions(&[owner(2)]);
        assert_eq!(poll_once(&mut sub), None);
    }

    #[test]
    fn revoked_sessions_end_in_the_hub() {
        let (h, _) = hub();
        let mut a = h.subscribe(req(1, &[Topic::Signal])).unwrap();
        let mut b = h.subscribe(req(2, &[Topic::Signal, Topic::Sms])).unwrap();
        let mut c = h.subscribe(req(3, &[Topic::Sms])).unwrap();
        c.snapshot();
        h.end_sessions(&[owner(1), owner(2)]);
        // The demand goes now, before the streams are polled: a stalled peer
        // that never reads its `bye` no longer keeps the sampler running.
        assert_eq!((h.demand(Topic::Signal), h.demand(Topic::Sms)), (0, 1));
        assert_eq!(h.due_job(Lane::Ubus, Instant::now()), Err(None));
        for s in [&mut a, &mut b] {
            assert_eq!(poll_once(s), Some(Wake::Ended(EndReason::LoggedOut)));
        }
        assert_eq!(poll_once(&mut c), None);
    }

    #[test]
    fn subscribe_wakes_lanes_and_drop_parks_them() {
        let (h, _) = hub();
        let now = Instant::now();
        assert_eq!(h.due_job(Lane::Fast, now), Err(None));
        let sub = h.subscribe(req(1, &[Topic::Battery, Topic::Traffic])).unwrap();
        assert_eq!(h.due_job(Lane::Fast, Instant::now()), Ok(Topic::Battery));
        assert_eq!(h.due_job(Lane::Ubus, Instant::now()), Ok(Topic::Traffic));
        drop(sub);
        let now = Instant::now();
        assert_eq!((h.due_job(Lane::Fast, now), h.due_job(Lane::Ubus, now)), (Err(None), Err(None)));
    }

    #[test]
    fn old_snapshot_value_is_announced_by_the_fresh_read() {
        let (h, clock) = hub();
        let t0 = Instant::now();
        h.store_at(Topic::Wan, Some(ok(json!({"up": true}))), t0, t0);
        h.store_at(Topic::Battery, Some(ok(json!({"capacity": 80}))), t0, t0);
        // Three hours nobody watched: the snapshot shows old reads.
        clock.advance(Duration::from_secs(3 * 3600));
        let mut sub = h.subscribe(req(1, &[Topic::Wan, Topic::Battery])).unwrap();
        let ages: Vec<_> = sub.snapshot().iter().map(|(_, v)| clock.now().saturating_duration_since(v.at)).collect();
        assert_eq!(ages, [Duration::from_secs(3 * 3600); 2]);
        // The fresh reads find the same values, and are sent anyway (once).
        for t in [Topic::Wan, Topic::Battery] {
            let v = h.peek(t).value.unwrap();
            h.store(t, ok((*v).clone()), Instant::now());
        }
        assert_eq!(poll_once(&mut sub), Some(Wake::Changed));
        let changes = sub.changes();
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().all(|(_, v)| v.at == clock.now()));
        h.store(Topic::Battery, ok(json!({"capacity": 80})), Instant::now());
        assert_eq!(poll_once(&mut sub), None);
        // A value read within its cadence is not announced to a newcomer.
        let mut next = h.subscribe(req(2, &[Topic::Battery])).unwrap();
        next.snapshot();
        h.store(Topic::Battery, ok(json!({"capacity": 80})), Instant::now());
        assert_eq!(poll_once(&mut next), None);
    }

    #[test]
    fn ages_follow_rereads_with_the_same_value() {
        let (h, clock) = hub();
        let sub = h.subscribe(req(1, &[Topic::Battery, Topic::Wan])).unwrap();
        h.store(Topic::Battery, ok(json!({"capacity": 80})), Instant::now());
        let v = version(&h, Topic::Battery);
        clock.advance(Duration::from_secs(30));
        h.store(Topic::Battery, ok(json!({"capacity": 80})), Instant::now());
        assert_eq!(version(&h, Topic::Battery), v);
        let ages = sub.ages();
        assert_eq!(ages, [TopicAge { topic: Topic::Battery, at: clock.now(), value_at: clock.now(), error: false }]);
    }
}
