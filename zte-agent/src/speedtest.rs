use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::events::{self, Topic};
use crate::handlers::AppState;

// --- Data types ---

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Latency,
    Download,
    Upload,
    Complete,
    Cancelled,
    Error,
}

#[derive(Serialize, Clone)]
pub struct SpeedTestProgress {
    phase: Phase,
    progress: u8,
    live_speed_mbps: f64,
    ping_ms: Option<f64>,
    jitter_ms: Option<f64>,
    download_mbps: Option<f64>,
    upload_mbps: Option<f64>,
    download_bytes: u64,
    upload_bytes: u64,
    server: String,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct TestServer {
    pub id: u64,
    pub name: String,
    pub sponsor: String,
    pub country: String,
    pub host: String,
    pub url: String,
    #[serde(skip)]
    pub base_url: String,
}

pub struct SpeedTest {
    progress: Arc<Mutex<SpeedTestProgress>>,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    servers_cache: Arc<Mutex<(Vec<TestServer>, Instant)>>,
}

const CACHE_TTL: Duration = Duration::from_secs(300);
const DOWNLOAD_DURATION: Duration = Duration::from_secs(15);
const UPLOAD_SIZE: usize = 1_000_000; // 1 MB
const UPLOAD_ROUNDS: usize = 10;
const PING_COUNT: usize = 10;
const BUF_SIZE: usize = 16384; // 16 KB

/// Change the progress, then mark the `speedtest` topic stale: the hub
/// re-reads `GET /api/speedtest/progress` (at most twice a second) only
/// while a live stream watches it.
fn update(progress: &Mutex<SpeedTestProgress>, f: impl FnOnce(&mut SpeedTestProgress)) {
    f(&mut progress.lock().unwrap_or_else(PoisonError::into_inner));
    events::touch(Topic::Speedtest);
}

impl SpeedTest {
    pub fn new() -> Self {
        Self {
            progress: Arc::new(Mutex::new(SpeedTestProgress {
                phase: Phase::Idle,
                progress: 0,
                live_speed_mbps: 0.0,
                ping_ms: None,
                jitter_ms: None,
                download_mbps: None,
                upload_mbps: None,
                download_bytes: 0,
                upload_bytes: 0,
                server: String::new(),
                error: None,
            })),
            cancel: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(false)),
            servers_cache: Arc::new(Mutex::new((Vec::new(), Instant::now() - CACHE_TTL))),
        }
    }

    /// True while a test is in progress (the modem policies hold off).
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

// --- Server fetching ---

fn fetch_servers() -> Result<Vec<TestServer>, String> {
    // Tests never reach speedtest.net (they fill the cache: `set_servers`).
    if cfg!(test) {
        return Err("no server list in tests".into());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();

    let resp = agent
        .get("https://www.speedtest.net/api/js/servers?engine=js&limit=20")
        .call()
        .map_err(|e| format!("fetch servers: {e}"))?;

    let data: Vec<Value> = resp
        .into_body()
        .read_json()
        .map_err(|e| format!("parse servers: {e}"))?;

    let mut servers = Vec::new();
    for entry in &data {
        let url = entry["url"].as_str().unwrap_or_default();
        // base_url: everything up to and including the last '/'
        let base_url = match url.rfind('/') {
            Some(i) => &url[..=i],
            None => continue,
        };

        servers.push(TestServer {
            id: entry["id"].as_u64()
                .or_else(|| entry["id"].as_str().and_then(|s| s.parse().ok()))
                .unwrap_or(0),
            name: entry["name"].as_str().unwrap_or("").to_string(),
            sponsor: entry["sponsor"].as_str().unwrap_or("").to_string(),
            country: entry["country"].as_str().unwrap_or("").to_string(),
            host: entry["host"].as_str().unwrap_or("").to_string(),
            url: url.to_string(),
            base_url: base_url.to_string(),
        });
    }
    Ok(servers)
}

fn get_servers(cache: &Arc<Mutex<(Vec<TestServer>, Instant)>>) -> Result<Vec<TestServer>, String> {
    let guard = cache.lock().unwrap();
    if !guard.0.is_empty() && guard.1.elapsed() < CACHE_TTL {
        return Ok(guard.0.clone());
    }
    drop(guard);

    let servers = fetch_servers()?;
    let mut guard = cache.lock().unwrap();
    guard.0 = servers.clone();
    guard.1 = Instant::now();
    Ok(servers)
}

// --- Background test logic ---

fn run_test(
    server: &TestServer,
    progress: &Arc<Mutex<SpeedTestProgress>>,
    cancel: &Arc<AtomicBool>,
) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();

    let ping_agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .into();

    // --- Latency phase ---
    update(progress, |p| {
        p.phase = Phase::Latency;
        p.progress = 0;
    });

    let ping_url = format!("{}latency.txt", server.base_url);
    let mut rtts = Vec::with_capacity(PING_COUNT);

    for i in 0..PING_COUNT {
        if cancel.load(Ordering::Relaxed) {
            set_cancelled(progress);
            return;
        }

        let start = Instant::now();
        match ping_agent.get(&ping_url).call() {
            Ok(resp) => {
                let _ = resp.into_body().read_to_vec();
                rtts.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            Err(_) => {} // skip failed pings
        }

        let pct = ((i + 1) as u8 * 20) / PING_COUNT as u8;
        update(progress, |p| p.progress = pct);
    }

    if rtts.is_empty() {
        set_error(progress, "all ping attempts failed");
        return;
    }

    rtts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_ping = rtts[rtts.len() / 2];
    let jitter = if rtts.len() > 1 {
        let diffs: Vec<f64> = rtts.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        diffs.iter().sum::<f64>() / diffs.len() as f64
    } else {
        0.0
    };

    // --- Download phase ---
    update(progress, |p| {
        p.ping_ms = Some(round2(median_ping));
        p.jitter_ms = Some(round2(jitter));
        p.phase = Phase::Download;
        p.progress = 20;
    });

    let download_url = format!("{}random4000x4000.jpg", server.base_url);
    let dl_start = Instant::now();
    let mut dl_bytes: u64 = 0;
    let mut buf = [0u8; BUF_SIZE];

    // Download for DOWNLOAD_DURATION, re-fetching the file if it finishes early
    'dl_outer: while dl_start.elapsed() < DOWNLOAD_DURATION {
        if cancel.load(Ordering::Relaxed) {
            set_cancelled(progress);
            return;
        }

        let resp = match agent.get(&download_url).call() {
            Ok(r) => r,
            Err(_) => break,
        };

        let mut body = resp.into_body();
        let mut reader = body.as_reader();
        loop {
            if cancel.load(Ordering::Relaxed) {
                set_cancelled(progress);
                return;
            }
            if dl_start.elapsed() >= DOWNLOAD_DURATION {
                break 'dl_outer;
            }

            match reader.read(&mut buf) {
                Ok(0) => break, // EOF, re-fetch
                Ok(n) => {
                    dl_bytes += n as u64;
                    let elapsed = dl_start.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.0 {
                        (dl_bytes as f64 * 8.0) / (elapsed * 1_000_000.0)
                    } else {
                        0.0
                    };
                    let pct = 20 + ((dl_start.elapsed().as_secs_f64() / DOWNLOAD_DURATION.as_secs_f64()) * 40.0).min(40.0) as u8;
                    // Every 16 KB read updates the figures; the topic is
                    // touched once per percent (about every 0.4 s).
                    let moved = {
                        let mut guard = progress.lock().unwrap_or_else(PoisonError::into_inner);
                        let moved = guard.progress != pct;
                        guard.live_speed_mbps = round2(speed);
                        guard.download_bytes = dl_bytes;
                        guard.progress = pct;
                        moved
                    };
                    if moved {
                        events::touch(Topic::Speedtest);
                    }
                }
                Err(_) => break,
            }
        }
    }

    let dl_elapsed = dl_start.elapsed().as_secs_f64();
    let dl_speed = if dl_elapsed > 0.0 {
        (dl_bytes as f64 * 8.0) / (dl_elapsed * 1_000_000.0)
    } else {
        0.0
    };

    // --- Upload phase ---
    update(progress, |p| {
        p.download_mbps = Some(round2(dl_speed));
        p.download_bytes = dl_bytes;
        p.progress = 60;
        p.phase = Phase::Upload;
        p.live_speed_mbps = 0.0;
    });

    let upload_buf = vec![0u8; UPLOAD_SIZE];
    let ul_start = Instant::now();
    let mut ul_bytes: u64 = 0;

    for i in 0..UPLOAD_ROUNDS {
        if cancel.load(Ordering::Relaxed) {
            set_cancelled(progress);
            return;
        }

        match agent.post(&server.url).send(&upload_buf[..]) {
            Ok(_) => {
                ul_bytes += UPLOAD_SIZE as u64;
            }
            Err(_) => {} // continue on error
        }

        let elapsed = ul_start.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            (ul_bytes as f64 * 8.0) / (elapsed * 1_000_000.0)
        } else {
            0.0
        };
        let pct = upload_pct(i);
        update(progress, |p| {
            p.live_speed_mbps = round2(speed);
            p.upload_bytes = ul_bytes;
            p.progress = pct;
        });
    }

    let ul_elapsed = ul_start.elapsed().as_secs_f64();
    let ul_speed = if ul_elapsed > 0.0 {
        (ul_bytes as f64 * 8.0) / (ul_elapsed * 1_000_000.0)
    } else {
        0.0
    };

    update(progress, |p| {
        p.upload_mbps = Some(round2(ul_speed));
        p.upload_bytes = ul_bytes;
        p.phase = Phase::Complete;
        p.progress = 100;
        p.live_speed_mbps = 0.0;
    });
}

fn set_cancelled(progress: &Arc<Mutex<SpeedTestProgress>>) {
    update(progress, |p| {
        p.phase = Phase::Cancelled;
        p.live_speed_mbps = 0.0;
    });
}

fn set_error(progress: &Arc<Mutex<SpeedTestProgress>>, msg: &str) {
    update(progress, |p| {
        p.phase = Phase::Error;
        p.error = Some(msg.to_string());
        p.live_speed_mbps = 0.0;
    });
}

/// The progress after upload round `i` (0-based): 60 to 100. Counted in
/// `usize`: `(i + 1) * 40` passes 255 from the 7th round.
fn upload_pct(i: usize) -> u8 {
    (60 + (i + 1).min(UPLOAD_ROUNDS) * 40 / UPLOAD_ROUNDS) as u8
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// --- Handler functions ---

pub fn servers(state: &AppState) -> (u16, Value) {
    match get_servers(&state.speedtest.servers_cache) {
        Ok(list) => (200, json!({"ok": true, "data": list})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn start(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    let server_id = parsed["server_id"].as_u64();

    // A running test is refused before anything is touched (a double start
    // must not wipe its progress or clear a pending stop).
    if state.speedtest.is_running() {
        return (409, json!({"ok": false, "error": "test already running"}));
    }

    let server_list = match get_servers(&state.speedtest.servers_cache) {
        Ok(s) => s,
        Err(e) => return (503, json!({"ok": false, "error": e})),
    };

    let server = if let Some(id) = server_id {
        match server_list.iter().find(|s| s.id == id) {
            Some(s) => s.clone(),
            None => return (404, json!({"ok": false, "error": "server not found"})),
        }
    } else {
        match server_list.into_iter().next() {
            Some(s) => s,
            None => return (503, json!({"ok": false, "error": "no servers available"})),
        }
    };

    // Atomically set running=true; if already true, another start won the
    // race while the server list was read. Only the winner resets the state.
    if state.speedtest.running.compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed).is_err() {
        return (409, json!({"ok": false, "error": "test already running"}));
    }

    // A Stop in the few microseconds between the exchange and this store
    // is lost (as before this order): the test runs on until stopped again.
    state.speedtest.cancel.store(false, Ordering::Relaxed);
    update(&state.speedtest.progress, |p| {
        *p = SpeedTestProgress {
            phase: Phase::Idle,
            progress: 0,
            live_speed_mbps: 0.0,
            ping_ms: None,
            jitter_ms: None,
            download_mbps: None,
            upload_mbps: None,
            download_bytes: 0,
            upload_bytes: 0,
            server: format!("{} ({})", server.sponsor, server.name),
            error: None,
        };
    });

    let progress = Arc::clone(&state.speedtest.progress);
    let cancel = Arc::clone(&state.speedtest.cancel);
    let running = Arc::clone(&state.speedtest.running);

    std::thread::spawn(move || {
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_test(&server, &progress, &cancel);
        }));
        // A panic would leave a running phase behind for good.
        if run.is_err() {
            set_error(&progress, "speed test failed");
        }
        running.store(false, Ordering::Relaxed);
    });

    (200, json!({"ok": true, "data": {"status": "started"}}))
}

pub fn progress(state: &AppState) -> (u16, Value) {
    let guard = state.speedtest.progress.lock().unwrap_or_else(PoisonError::into_inner);
    (200, json!({"ok": true, "data": *guard}))
}

pub fn stop(state: &AppState, _body: &[u8]) -> (u16, Value) {
    if !state.speedtest.running.load(Ordering::Relaxed) {
        return (200, json!({"ok": true, "data": {"status": "not_running"}}));
    }
    state.speedtest.cancel.store(true, Ordering::Relaxed);
    (200, json!({"ok": true, "data": {"status": "stopping"}}))
}

#[cfg(test)]
impl SpeedTest {
    /// A server list as if fetched now (tests never reach speedtest.net).
    pub fn set_servers(&self, servers: Vec<TestServer>) {
        *self.servers_cache.lock().unwrap() = (servers, Instant::now());
    }

    /// One server whose URLs point at `base` (a local address in tests).
    pub fn test_server(id: u64, base: &str) -> TestServer {
        TestServer {
            id,
            name: "Testville".into(),
            sponsor: "Example Net".into(),
            country: "Nowhere".into(),
            host: "127.0.0.1".into(),
            url: format!("{base}upload.php"),
            base_url: base.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::test_sink;

    fn phase(app: &AppState) -> Value {
        progress(app).1["data"].clone()
    }

    /// Every change of the progress marks the `speedtest` topic stale.
    #[test]
    fn updates_touch_the_topic() {
        let app = AppState::new();
        test_sink::take();
        update(&app.speedtest.progress, |p| p.progress = 7);
        set_cancelled(&app.speedtest.progress);
        assert_eq!(test_sink::take(), [Topic::Speedtest, Topic::Speedtest]);
        assert_eq!(phase(&app)["phase"], "cancelled");
        assert_eq!(phase(&app)["progress"], 7);
    }

    /// The topic costs nothing unwatched: a change only marks it; a
    /// stream's subscription reads it once, then each change (no cadence).
    #[test]
    fn the_topic_is_read_only_while_watched() {
        use crate::auth::SessionId;
        use crate::boottime::BootClock;
        use crate::events::hub::{Hub, SubscribeReq};
        use crate::events::{readers, Lane};
        let app = AppState::new();
        let hub = Arc::new(Hub::new(Arc::new(BootClock)));
        let far = Instant::now() + Duration::from_secs(3600);
        test_sink::with_hub(&hub, || {
            update(&app.speedtest.progress, |p| p.progress = 5);
            assert_eq!(hub.due_job(Lane::Fast, far), Err(None));
            let topics = [Topic::Speedtest].into_iter().collect();
            let _sub = hub.subscribe(SubscribeReq { owner: SessionId::for_test(1), topics, signal_hint: None, replace_same: None }).unwrap();
            assert_eq!(hub.due_job(Lane::Fast, Instant::now()), Ok(Topic::Speedtest));
            let started = Instant::now();
            let out = readers::read(Topic::Speedtest, &app, &hub).unwrap();
            assert_eq!(out.value.as_ref().unwrap(), &phase(&app));
            hub.store(Topic::Speedtest, out, started);
            assert_eq!(hub.due_job(Lane::Fast, far), Err(None));
            update(&app.speedtest.progress, |p| p.progress = 6);
            assert_eq!(hub.due_job(Lane::Fast, Instant::now() + Duration::from_millis(600)), Ok(Topic::Speedtest));
        });
    }

    /// A second start while a test runs is refused before anything is
    /// changed: the running test keeps its progress and a pending stop.
    #[test]
    fn a_second_start_keeps_the_running_test() {
        let app = AppState::new();
        app.speedtest.set_servers(vec![SpeedTest::test_server(7, "http://127.0.0.1:9/")]);
        update(&app.speedtest.progress, |p| {
            p.phase = Phase::Download;
            p.progress = 42;
            p.server = "Example Net (Testville)".into();
        });
        app.speedtest.running.store(true, Ordering::SeqCst);
        app.speedtest.cancel.store(true, Ordering::SeqCst);
        test_sink::take();
        let (status, v) = start(&app, br#"{"server_id":7}"#);
        assert_eq!((status, v["error"].as_str()), (409, Some("test already running")));
        let p = phase(&app);
        assert_eq!((p["phase"].as_str(), p["progress"].as_u64()), (Some("download"), Some(42)));
        assert!(app.speedtest.cancel.load(Ordering::SeqCst), "the stop was cleared");
        assert!(test_sink::take().is_empty());
        app.speedtest.running.store(false, Ordering::SeqCst);
        assert_eq!(start(&app, br#"{"server_id":8}"#).0, 404);
        assert_eq!(phase(&app)["progress"], 42);
    }

    /// The upload rounds move the bar from 60 to 100 without wrapping.
    #[test]
    fn upload_progress_climbs_to_100() {
        let pcts: Vec<u8> = (0..UPLOAD_ROUNDS).map(upload_pct).collect();
        assert_eq!(pcts, [64, 68, 72, 76, 80, 84, 88, 92, 96, 100]);
    }

    /// Starts at the same moment: exactly one wins the compare-exchange,
    /// the others answer 409 (the test holds in its latency phase against
    /// a local server that accepts and never answers).
    #[test]
    fn concurrent_starts_start_one() {
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", l.local_addr().unwrap());
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for s in l.incoming().flatten() {
                held.push(s);
            }
        });
        for round in 0..10 {
            let app = Arc::new(AppState::new());
            app.speedtest.set_servers(vec![SpeedTest::test_server(7, &base)]);
            let barrier = Arc::new(std::sync::Barrier::new(8));
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let (app, b) = (Arc::clone(&app), Arc::clone(&barrier));
                    std::thread::spawn(move || {
                        b.wait();
                        start(&app, br#"{"server_id":7}"#).0
                    })
                })
                .collect();
            let codes: Vec<u16> = threads.into_iter().map(|t| t.join().unwrap()).collect();
            assert_eq!(codes.iter().filter(|c| **c == 200).count(), 1, "round {round}: {codes:?}");
            assert_eq!(codes.iter().filter(|c| **c == 409).count(), 7, "round {round}: {codes:?}");
            app.speedtest.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// A test against a server that cannot be reached ends as an error
    /// and is no longer running.
    #[test]
    fn an_unreachable_server_ends_in_an_error() {
        let app = AppState::new();
        app.speedtest.set_servers(vec![SpeedTest::test_server(7, "http://127.0.0.1:9/")]);
        assert_eq!(start(&app, br#"{"server_id":7}"#).0, 200);
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.speedtest.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let p = phase(&app);
        assert!(!app.speedtest.is_running());
        assert_eq!((p["phase"].as_str(), p["error"].as_str()), (Some("error"), Some("all ping attempts failed")));
        assert_eq!(p["server"], "Example Net (Testville)");
    }
}
