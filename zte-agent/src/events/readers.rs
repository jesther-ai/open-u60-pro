//! Topic reads through the existing handlers, so a topic's `data` always
//! equals its GET route's `data`. Called on the sampler threads (and, from
//! T4, inside a gate task for page loads); never on the async runtime thread.

use std::cell::Cell;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use super::hub::{Hub, ReadOut};
use super::{object, Topic};
use crate::handlers::{self, AppState};
use crate::system::{CpuTracker, ProcessTracker};
use crate::{cache, cell, device_ext, esim, modem_ext, network_ext, sim, speedtest, usb};

thread_local! {
    static IN_HUB_READ: Cell<bool> = const { Cell::new(false) };
}

/// Inside `read` on this thread: `events::touch` is suppressed.
pub fn in_hub_read() -> bool {
    IN_HUB_READ.with(Cell::get)
}

struct ReadScope(bool);

impl ReadScope {
    fn enter() -> Self {
        ReadScope(IN_HUB_READ.with(|c| c.replace(true)))
    }
}

impl Drop for ReadScope {
    fn drop(&mut self) {
        IN_HUB_READ.with(|c| c.set(self.0));
    }
}

/// One read of `t`; `None` for push-only topics (no reader).
pub fn read(t: Topic, app: &AppState, hub: &Hub) -> Option<ReadOut> {
    crate::httpd::gate::debug_assert_may_block();
    let _scope = ReadScope::enter();
    Some(match t {
        Topic::Signal => data_of(handlers::network_signal(app)),
        Topic::Traffic => data_of(network_ext::network_speeds(app)),
        Topic::Battery => data_of(handlers::battery(app)),
        Topic::Health => {
            let cpu = ReadOut { value: Some(hub.cpu.read()), error: None };
            composite([("cpu", cpu), ("thermal", data_of(device_ext::device_thermal(app)))])
        }
        Topic::Clients => data_of(network_ext::network_clients(app)),
        Topic::Wan => composite([
            ("wan", data_of(network_ext::network_wan(app))),
            ("wan6", data_of(network_ext::network_wan6(app))),
        ]),
        Topic::Sim => data_of(sim::sim_info(app)),
        Topic::Modem => composite([
            ("status", data_of(handlers::modem_status(app))),
            ("data", data_of(modem_ext::modem_data_get(app))),
        ]),
        Topic::Charger => data_of(device_ext::device_charger(app)),
        Topic::BatteryInfo => data_of(network_ext::network_battery_ubus(app)),
        Topic::SimIdentity => sim_identity_initial(app),
        // Shares the signal read's netinfo (same `network_type` vocabulary as the event).
        Topic::Service => match app.cache.netinfo(cache::NETINFO_TTL) {
            Ok(v) => ReadOut { value: Some(object(&mut [("network_type", v["network_type"].clone())])), error: None },
            Err(e) => ReadOut { value: None, error: Some(e) },
        },
        Topic::EsimSwitch => data_of(esim::switch_status(app)),
        Topic::EsimDownload => data_of(esim::download_status(app)),
        Topic::Processes => ReadOut { value: Some(hub.procs.read()), error: None },
        Topic::Usb => data_of(usb::usb_status(app)),
        Topic::Speedtest => data_of(speedtest::progress(app)),
        Topic::Netscan => composite([
            ("scan", data_of(modem_ext::modem_scan_status(app))),
            ("register", data_of(modem_ext::modem_register_result(app))),
        ]),
        Topic::SignalDetect => signal_detect(app),
        // The worker's state (`GET /api/stk/session`) from memory: never
        // starts the worker, connects, sends or waits for it.
        Topic::Stk => data_of(app.stk.session_view()),
        Topic::PowerState | Topic::Sms => return None,
    })
}

/// A handler's `(status, envelope)` as a read: `data` on 200, else `error`.
fn data_of((status, mut v): (u16, Value)) -> ReadOut {
    if status == 200 {
        ReadOut { value: Some(v["data"].take()), error: None }
    } else {
        ReadOut { value: None, error: Some(v["error"].as_str().unwrap_or("error").to_string()) }
    }
}

/// An object of parts, `null` for a failed part, with the first error; no
/// value at all if every part failed (the last good value stays).
fn composite<const N: usize>(parts: [(&str, ReadOut); N]) -> ReadOut {
    let mut obj = Map::new();
    let mut error = None;
    let mut any = false;
    for (key, part) in parts {
        any |= part.value.is_some();
        obj.insert(key.into(), part.value.unwrap_or(Value::Null));
        error = error.or(part.error);
    }
    ReadOut { value: any.then_some(Value::Object(obj)), error }
}

/// A signal detection sweep runs: the device says so and has not reached
/// 100 % (the app's rule when it re-attaches to a sweep).
pub fn sweeping(progress: &Value) -> bool {
    let truthy = match &progress["running"] {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "on"),
        _ => false,
    };
    let pct = match &progress["progress"] {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    };
    truthy && pct < 100.0
}

/// `{"progress": GET …/signal-detect/progress, "results": GET
/// …/signal-detect/results}`; the results are not read while a sweep runs
/// (`null`: they change only when it ends), so a watched sweep costs one
/// call per read.
fn signal_detect(app: &AppState) -> ReadOut {
    let progress = data_of(cell::cell_signal_detect_progress(app));
    let results = if progress.value.as_ref().is_some_and(sweeping) {
        ReadOut { value: Some(Value::Null), error: None }
    } else {
        data_of(cell::cell_signal_detect_results(app))
    };
    composite([("progress", progress), ("results", results)])
}

/// The `zte_agent.sim_identity` bus shape before the first event: phase and
/// epoch from the endpoints' stamp (`idle` = no transition since start).
fn sim_identity_initial(app: &AppState) -> ReadOut {
    let mut stamp = Map::new();
    app.sim_identity.stamp(&mut stamp);
    let mut v = Map::new();
    for (key, from) in [("phase", "sim_phase"), ("epoch", "sim_epoch"), ("agent_instance", "agent_instance")] {
        v.insert(key.into(), stamp.get(from).cloned().unwrap_or_default());
    }
    for key in ["source", "plmn", "operator", "iccid_tail", "since_leaving_ms"] {
        v.insert(key.into(), Value::Null);
    }
    ReadOut { value: Some(Value::Object(v)), error: None }
}

/// A figure measured over the time since the previous sample (CPU usage
/// for `health`, per-process CPU for `processes`) from a tracker of the
/// hub's own: the API's trackers are shared with every `GET /api/cpu` and
/// `GET /api/system/top`, which would shorten its window. After a gap it
/// primes first, so the first figure covers 250 ms rather than the whole
/// idle time. A read within 250 ms of the last sample (a page load while a
/// stream samples) gets that sample's figure: a shorter window reads 0 %.
pub struct Primed {
    sample: Box<dyn Fn() -> Value + Send + Sync>,
    /// When the last figure was sampled, and the figure.
    last: Mutex<Option<(Instant, Value)>>,
}

const PRIME_AFTER: Duration = Duration::from_secs(15);
const PRIME_WINDOW: Duration = Duration::from_millis(250);

impl Primed {
    fn new(sample: Box<dyn Fn() -> Value + Send + Sync>) -> Self {
        Primed { sample, last: Mutex::new(None) }
    }

    /// `{"cores": [...], "overall": n}` (`GET /api/cpu`).
    pub fn cpu() -> Self {
        let t = CpuTracker::new();
        Primed::new(Box::new(move || serde_json::to_value(t.sample()).unwrap_or_default()))
    }

    /// The data of `GET /api/system/top`.
    pub fn processes() -> Self {
        let t = ProcessTracker::new();
        Primed::new(Box::new(move || serde_json::to_value(t.sample()).unwrap_or_default()))
    }

    /// May sleep 250 ms: hub-fast or a gate task only.
    pub fn read(&self) -> Value {
        self.read_with(Instant::now(), &*self.sample, std::thread::sleep)
    }

    fn read_with(&self, now: Instant, sample: &dyn Fn() -> Value, pause: impl Fn(Duration)) -> Value {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let gap = last.as_ref().map(|(at, _)| now.saturating_duration_since(*at));
        if let Some((_, figure)) = last.as_ref().filter(|_| gap.is_some_and(|g| g < PRIME_WINDOW)) {
            return figure.clone();
        }
        let mut at = now;
        if gap.is_none_or(|g| g > PRIME_AFTER) {
            sample();
            pause(PRIME_WINDOW);
            at += PRIME_WINDOW;
        }
        let figure = sample();
        *last = Some((at, figure.clone()));
        figure
    }

    /// When the last figure was sampled.
    #[cfg(test)]
    pub fn last_at(&self) -> Option<Instant> {
        self.last.lock().unwrap().as_ref().map(|(at, _)| *at)
    }
}

#[cfg(test)]
mod tests {
    use super::super::hub::SubscribeReq;
    use super::super::{test_sink, Lane};
    use super::*;
    use crate::auth::SessionId;
    use crate::boottime::BootClock;
    use serde_json::json;
    use std::cell::RefCell;
    use std::sync::Arc;

    #[test]
    fn hub_read_never_self_touches() {
        let app = AppState::new();
        let hub = Arc::new(Hub::new(Arc::new(BootClock)));
        let topics = [Topic::EsimDownload].into_iter().collect();
        let req = SubscribeReq { owner: SessionId::for_test(1), topics, signal_hint: None, replace_same: None };
        let _sub = hub.subscribe(req).unwrap();
        test_sink::with_hub(&hub, || {
            test_sink::take();
            // A plain GET takes `&mut` (expire) and touches.
            esim::download_status(&app);
            assert_eq!(test_sink::take(), [Topic::EsimDownload]);
            let started = Instant::now();
            let out = read(Topic::EsimDownload, &app, &hub).unwrap();
            assert_eq!(out.value.as_ref().unwrap()["state"], "idle");
            hub.store(Topic::EsimDownload, out, started);
            // The hub's own read touched nothing, so nothing is due.
            assert!(test_sink::take().is_empty());
            assert!(!in_hub_read());
            let far = Instant::now() + Duration::from_secs(3600);
            assert_eq!(hub.due_job(Lane::Fast, far), Err(None));
        });
    }

    #[test]
    fn sim_identity_initial_has_bus_shape() {
        let app = AppState::new();
        let hub = Hub::new(Arc::new(BootClock));
        let v = read(Topic::SimIdentity, &app, &hub).unwrap().value.unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            ["agent_instance", "epoch", "iccid_tail", "operator", "phase", "plmn", "since_leaving_ms", "source"]
        );
        assert_eq!((v["phase"].as_str(), v["epoch"].as_u64()), (Some("idle"), Some(0)));
        assert_eq!(v["agent_instance"], crate::sim_identity::agent_instance());
        assert!(v["source"].is_null() && v["iccid_tail"].is_null() && v["since_leaving_ms"].is_null());
        // Push-only topics have no reader.
        assert!(read(Topic::Sms, &app, &hub).is_none());
        assert!(read(Topic::PowerState, &app, &hub).is_none());
    }

    #[test]
    fn readers_return_the_get_route_data() {
        let app = AppState::new();
        let hub = Hub::new(Arc::new(BootClock));
        let (_, mut route) = crate::server::route(&crate::server::Method::Get, "/api/esim/switch", &app, b"");
        let out = read(Topic::EsimSwitch, &app, &hub).unwrap();
        assert_eq!(out.value.unwrap(), route["data"].take());
        assert_eq!(data_of((503, json!({"ok": false, "error": "no modem"}))).error.as_deref(), Some("no modem"));
    }

    /// The app's rule: running and below 100 %; numbers or strings.
    #[test]
    fn a_sweep_runs_below_100_percent() {
        assert!(sweeping(&json!({"running": 1, "progress": 40})));
        assert!(sweeping(&json!({"running": "true", "progress": "99"})));
        assert!(!sweeping(&json!({"running": 1, "progress": "100"})));
        assert!(!sweeping(&json!({"running": 0, "progress": 40})));
        assert!(!sweeping(&json!({"progress": 40})) && !sweeping(&Value::Null));
    }

    /// Both topics read their GET routes (which reach no device here):
    /// every part is named, and a failure is the read's error.
    #[test]
    fn scan_and_detection_topics_are_composites_of_their_routes() {
        let app = AppState::new();
        let hub = Hub::new(Arc::new(BootClock));
        let out = read(Topic::Netscan, &app, &hub).unwrap();
        let (_, route) = crate::server::route(&crate::server::Method::Get, "/api/modem/scan/status", &app, b"");
        assert_eq!(out.error.as_deref(), route["error"].as_str());
        let out = read(Topic::SignalDetect, &app, &hub).unwrap();
        let (_, route) = crate::server::route(&crate::server::Method::Get, "/api/cell/signal-detect/progress", &app, b"");
        assert_eq!(out.error.as_deref(), route["error"].as_str());
    }

    #[test]
    fn composite_reports_parts_and_first_error() {
        let ok = |v| ReadOut { value: Some(v), error: None };
        let bad = |e: &str| ReadOut { value: None, error: Some(e.into()) };
        let out = composite([("wan", ok(json!(1))), ("wan6", bad("down")), ("x", bad("later"))]);
        assert_eq!(out.value, Some(json!({"wan": 1, "wan6": null, "x": null})));
        assert_eq!(out.error.as_deref(), Some("down"));
        let out = composite([("wan", bad("a")), ("wan6", bad("b"))]);
        assert_eq!((out.value, out.error.as_deref()), (None, Some("a")));
    }

    #[test]
    fn primed_figures_prime_after_a_gap() {
        let cpu = Primed::cpu();
        let calls = RefCell::new(Vec::new());
        let sample = || {
            calls.borrow_mut().push("sample");
            json!({"cores": [], "overall": 1.0})
        };
        let pause = |d: Duration| calls.borrow_mut().push(if d == PRIME_WINDOW { "pause" } else { "?" });
        let t0 = Instant::now();
        // First read: prime, wait, sample.
        cpu.read_with(t0, &sample, pause);
        assert_eq!(calls.take(), ["sample", "pause", "sample"]);
        // Within 15 s of the last read: one sample covers the gap.
        cpu.read_with(t0 + Duration::from_secs(5), &sample, pause);
        assert_eq!(calls.take(), ["sample"]);
        // Older than 15 s: primed again.
        cpu.read_with(t0 + Duration::from_secs(21), &sample, pause);
        assert_eq!(calls.take(), ["sample", "pause", "sample"]);
    }

    #[test]
    fn primed_figures_never_cover_a_window_under_250_ms() {
        let cpu = Primed::cpu();
        let calls = RefCell::new(0);
        let figure = RefCell::new(40.8);
        let sample = || {
            *calls.borrow_mut() += 1;
            json!({"cores": [*figure.borrow()], "overall": *figure.borrow()})
        };
        let t0 = Instant::now();
        // Primed: the figure is sampled at t0 + 250 ms.
        assert_eq!(cpu.read_with(t0, &sample, |_| ())["overall"], 40.8);
        assert_eq!(calls.replace(0), 2);
        // A page load 1 ms after a stream's read: the same figure, no sample
        // (a 1 ms window would read 0 %); also during the priming pause.
        *figure.borrow_mut() = 0.0;
        for later in [Duration::from_millis(1), Duration::from_millis(251), Duration::from_millis(499)] {
            assert_eq!(cpu.read_with(t0 + later, &sample, |_| ()), json!({"cores": [40.8], "overall": 40.8}));
        }
        assert_eq!(*calls.borrow(), 0);
        // 250 ms after the sample: a new one.
        assert_eq!(cpu.read_with(t0 + Duration::from_millis(500), &sample, |_| ())["overall"], 0.0);
        assert_eq!(*calls.borrow(), 1);
    }

    /// The processes topic samples the hub's own tracker: `GET
    /// /api/system/top` (the apps, the CPU & Memory page) uses the API's
    /// and leaves the hub's window alone. Its data has the route's shape.
    #[test]
    fn processes_use_the_hubs_own_tracker() {
        let app = AppState::new();
        let hub = Hub::new(Arc::new(BootClock));
        assert_eq!(hub.procs.last_at(), None);
        let out = read(Topic::Processes, &app, &hub).unwrap();
        let at = hub.procs.last_at().expect("sampled by the hub's tracker");
        let (status, route) = crate::server::route(&crate::server::Method::Get, "/api/system/top", &app, b"");
        assert_eq!(status, 200);
        assert_eq!(hub.procs.last_at(), Some(at), "an API read moved the hub's window");
        let keys = |v: &Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        let data = out.value.unwrap();
        assert_eq!(keys(&data), keys(&route["data"]));
        assert_eq!(keys(&data), ["bloat_count", "bloat_cpu_pct", "bloat_rss_kb", "processes", "total_count"]);
    }
}
