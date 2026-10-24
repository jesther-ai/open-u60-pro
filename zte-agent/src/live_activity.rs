//! Opt-in, direct APNs updates. Credentials stay on disk; activity tokens stay in RAM.
#[path = "live_activity_recovery.rs"]
mod recovery;
use crate::handlers::AppState;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use p256::{
    ecdsa::{signature::Signer, Signature, SigningKey},
    pkcs8::DecodePrivateKey,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    enabled: bool,
    team_id: String,
    key_id: String,
    bundle_id: String,
    key_file: String,
    environment: Environment,
    interval_seconds: u64,
    stale_after_seconds: u64,
    #[serde(default = "default_push_priority")]
    push_priority: u8,
}

fn default_push_priority() -> u8 {
    5
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Environment {
    Sandbox,
    Production,
}
impl Environment {
    fn host(self) -> &'static str {
        match self {
            Self::Sandbox => "https://api.sandbox.push.apple.com",
            Self::Production => "https://api.push.apple.com",
        }
    }
}
impl Config {
    fn validate(&self) -> Result<(), &'static str> {
        let identifier = |s: &str| {
            s.len() == 10
                && s.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        };
        if !identifier(&self.team_id) || !identifier(&self.key_id) {
            return Err("team_id and key_id must be 10 uppercase letters/digits");
        }
        if self.bundle_id.len() > 255
            || !self.bundle_id.contains('.')
            || !self
                .bundle_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            return Err("invalid bundle_id");
        }
        if !(15..=3600).contains(&self.interval_seconds)
            || self.stale_after_seconds < self.interval_seconds
            || self.stale_after_seconds > 7200
        {
            return Err(
                "interval must be 15..3600 seconds; stale interval must be >= interval and <= 7200",
            );
        }
        if !matches!(self.push_priority, 5 | 10) {
            return Err("push_priority must be 5 or 10");
        }
        if !Path::new(&self.key_file).is_absolute() {
            return Err("key_file must be absolute");
        }
        Ok(())
    }
}

fn private_file(path: &Path) -> Result<String, &'static str> {
    let meta = fs::symlink_metadata(path).map_err(|_| "private file unavailable")?;
    if !meta.is_file()
        || meta.mode() & 0o077 != 0
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.len() > 16384
    {
        return Err(
            "private file must be owned by agent user, regular, <=16 KiB, and mode 0600 or 0400",
        );
    }
    fs::read_to_string(path).map_err(|_| "cannot read private file")
}

struct Provider {
    config: Config,
    key: SigningKey,
}
fn load_provider() -> Result<Option<Provider>, &'static str> {
    let Ok(path) = std::env::var("ZTE_AGENT_LIVE_ACTIVITY_CONFIG") else {
        return Ok(None);
    };
    let config: Config = serde_json::from_str(&private_file(Path::new(&path))?)
        .map_err(|_| "invalid live activity config JSON")?;
    if !config.enabled {
        return Ok(None);
    }
    config.validate()?;
    let pem = private_file(Path::new(&config.key_file))?;
    let key = SigningKey::from_pkcs8_pem(&pem)
        .map_err(|_| "key must be an Apple ES256 PKCS#8 PEM .p8 key")?;
    Ok(Some(Provider { config, key }))
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    push_token: String,
    expires_at: u64,
}
impl Registration {
    fn validate(&self, now: u64) -> Result<(), &'static str> {
        if !(32..=1024).contains(&self.push_token.len())
            || !self.push_token.len().is_multiple_of(2)
            || !self.push_token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("push_token must be hexadecimal ActivityKit update token");
        }
        if self.expires_at <= now || self.expires_at > now.saturating_add(8 * 3600) {
            return Err("expires_at must be Unix seconds within the next 8 hours");
        }
        Ok(())
    }
}
#[derive(Default)]
struct State {
    activity: Option<Registration>,
    generation: u64,
    last_accepted_at: Option<u64>,
    last_error: Option<&'static str>,
    recovery: recovery::Recovery,
}
impl State {
    /// An activity is held and, once UTC is verified, has not expired. Reported by `status()`
    /// and used by the recovery thread to pick its probe cadence.
    fn registered(&self) -> bool {
        self.activity
            .as_ref()
            .is_some_and(|a| self.recovery.utc().is_none_or(|t| a.expires_at > t))
    }
}
pub struct LiveActivity {
    provider: Option<Provider>,
    state: Mutex<State>,
    changed: Condvar,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
fn error(code: u16, message: &str) -> (u16, Value) {
    (code, json!({"ok": false, "error": message}))
}
impl LiveActivity {
    pub fn new() -> Self {
        let (provider, last_error) = match load_provider() {
            Ok(p) => (p, None),
            Err(e) => (None, Some(e)),
        };
        Self {
            provider,
            changed: Condvar::new(),
            state: Mutex::new(State {
                last_error,
                ..State::default()
            }),
        }
    }
    /// UTC checked over TLS against a time server within the last 6 hours
    /// (unix seconds), if any: the LAN HTTPS certificates' dates (`tls`).
    pub fn verified_utc(&self) -> Option<u64> {
        self.trusted_now()
    }
    pub fn status(&self) -> (u16, Value) {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        (
            200,
            json!({"ok": true, "data": {
                "enabled": self.provider.is_some(),
                "registered": state.registered(),
                "expires_at": state.activity.as_ref().map(|a| a.expires_at),
                "last_accepted_at": state.last_accepted_at,
                "last_error": state.last_error,
                "interval_seconds": self.provider.as_ref().map(|p| p.config.interval_seconds),
                "push_priority": self.provider.as_ref().map(|p| p.config.push_priority),
                "recovery_state": state.recovery.label(),
                "clock_verified": state.recovery.utc().is_some(),
                "clock_error": state.recovery.error,
                "last_time_sync_at": state.recovery.last_sync,
                "last_recovery_event": state.recovery.last_event
            }}),
        )
    }
    pub fn register(&self, body: &[u8]) -> (u16, Value) {
        if self.provider.is_none() {
            return error(503, "live activity provider is disabled or invalid");
        }
        let registration: Registration = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(_) => return error(400, "invalid registration JSON"),
        };
        let Some(clock) = self.trusted_now() else {
            return error(
                503,
                "waiting for verified UTC; registration will need retry",
            );
        };
        if let Err(e) = registration.validate(clock) {
            return error(400, e);
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        // Foreground/resume can repeat PUT. Preserve backoff and acceptance history.
        if state
            .activity
            .as_ref()
            .is_some_and(|a| a.push_token == registration.push_token)
        {
            if let Some(current) = state.activity.as_mut() {
                current.expires_at = registration.expires_at;
            }
            self.changed.notify_all();
            return (200, json!({"ok": true}));
        }
        state.activity = Some(registration);
        state.generation = state.generation.wrapping_add(1);
        state.last_accepted_at = None;
        state.last_error = None;
        // While offline the probe backs off to 5 minutes; a new activity should not wait for it.
        if state.recovery.paused {
            state.recovery.reprobe();
        }
        self.changed.notify_all();
        (200, json!({"ok": true}))
    }
    pub fn unregister(&self) -> (u16, Value) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.activity = None;
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
        (200, json!({"ok": true}))
    }
    pub fn start(
        self: &Arc<Self>,
        app: Arc<AppState>,
        events: Vec<(&'static str, std::sync::mpsc::Receiver<Value>)>,
    ) {
        if self.provider.is_none() || !app.auth.has_password() {
            return;
        }
        // The recovery probe is the connectivity watchdog's only signal:
        // tell it one is coming so its status can say so.
        app.conn_watchdog.attach_probe();
        // Only the watchdog handle crosses into the recovery thread, not the
        // whole AppState (which owns this LiveActivity: no Arc cycle).
        self.start_recovery(events, Arc::clone(&app.conn_watchdog));
        let service = Arc::clone(self);
        let cache = Arc::clone(&app.cache);
        crate::system::spawn_named("la-push", move || service.run(cache));
    }
    /// Moves on every SIM/WAN recovery event: the push thread drops its APNs connection when
    /// this differs from the value it connected under.
    fn network_revision(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .recovery
            .revision()
    }
    /// Sleep on changes or the next deadline. No polling, signing, or modem reads while idle.
    fn wait_for_attempt(
        &self,
        next: &mut Instant,
        observed: &mut u64,
        failures: &mut u32,
    ) -> (Registration, u64, u64) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            let Some(clock) = state.recovery.utc() else {
                // The recovery thread notifies when UTC becomes verified.
                state = self.changed.wait(state).unwrap_or_else(|p| p.into_inner());
                continue;
            };
            if state
                .activity
                .as_ref()
                .is_some_and(|a| a.expires_at <= clock)
            {
                state.activity = None;
            }
            if state.generation != *observed {
                *observed = state.generation;
                *failures = 0;
                *next = Instant::now();
            }
            let Some(activity) = state.activity.as_ref() else {
                state = self.changed.wait(state).unwrap_or_else(|p| p.into_inner());
                continue;
            };
            if state.recovery.paused {
                // Resuming notifies; wake on our own only to drop the activity at its expiry.
                let delay = Duration::from_secs(activity.expires_at.saturating_sub(clock));
                state = self
                    .changed
                    .wait_timeout(state, delay)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
                continue;
            }
            if clock >= 1_700_000_000 && Instant::now() >= *next {
                return (activity.clone(), state.generation, clock);
            }
            // An unset wall clock must not cause a busy loop or invalid Apple signatures.
            let delay = if clock < 1_700_000_000 {
                Duration::from_secs(60)
            } else {
                next.saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(
                        activity.expires_at.saturating_sub(clock),
                    ))
            };
            state = self
                .changed
                .wait_timeout(state, delay)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    fn run(&self, cache: Arc<crate::cache::UbusCache>) {
        let Some(provider) = &self.provider else {
            return;
        };
        let mut client = match apns_client() {
            Ok(client) => client,
            Err(_) => {
                self.state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .last_error = Some("APNs client initialization failed");
                return;
            }
        };
        let mut jwt = String::new();
        let mut jwt_issued = 0;
        let mut next = Instant::now();
        let mut failures: u32 = 0;
        let mut observed_generation = 0;
        let mut client_revision = self.network_revision();
        let mut reconnect = false;
        loop {
            let (activity, generation, clock) =
                self.wait_for_attempt(&mut next, &mut observed_generation, &mut failures);
            // The pooled HTTP/2 connection outlives a SIM or WAN change on a socket that is now
            // black-holed; start a fresh one rather than wait minutes for it to time out.
            let revision = self.network_revision();
            if reconnect || revision != client_revision {
                if let Ok(fresh) = apns_client() {
                    client = fresh;
                    eprintln!("[live_activity] new APNs connection after network change");
                }
                client_revision = revision;
                reconnect = false;
            }
            if jwt.is_empty() || clock < jwt_issued || clock - jwt_issued >= 3000 {
                jwt = provider_token(provider, clock);
                jwt_issued = clock;
            }
            // Always a fresh read (max_age zero) so `sampledAt` stays honest;
            // going through the memo refreshes the entry the HTTP readers share.
            let result = match cache.netinfo(Duration::ZERO) {
                Ok(data) if data.is_object() => {
                    // Timestamp the read start conservatively; do not claim modem hardware sample time.
                    let payload = payload(&data, clock, provider.config.stale_after_seconds);
                    if self.trusted_now().is_none_or(|t| {
                        t >= clock.saturating_add(provider.config.stale_after_seconds)
                            || t >= activity.expires_at
                    }) {
                        Outcome::Retry("signal sample expired before send")
                    } else {
                        let valid = self
                            .state
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .generation
                            == generation;
                        if !valid {
                            continue;
                        }
                        send(&client, provider, &activity, &jwt, &payload)
                    }
                }
                _ => Outcome::Retry("signal read failed"),
            };
            reconnect = matches!(result, Outcome::Retry("APNs transport failed"));
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.generation != generation {
                continue;
            }
            match result {
                Outcome::Accepted => {
                    state.last_accepted_at = state.recovery.utc();
                    state.last_error = None;
                    failures = 0;
                }
                Outcome::Stop(message) => {
                    state.activity = None;
                    state.last_error = Some(message);
                    failures = 0;
                }
                Outcome::Retry(message) => {
                    state.last_error = Some(message);
                    failures = failures.saturating_add(1);
                }
            }
            next = Instant::now() + retry_delay(provider.config.interval_seconds, failures);
        }
    }
}
fn apns_client() -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .https_only(true)
        .http2_prior_knowledge()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        // No SO_KEEPALIVE: an idle APNs connection after a session must not wake the radio.
        .tcp_keepalive(None)
        .tcp_keepalive_interval(None)
        .tcp_keepalive_retries(None)
        .build()
}
fn retry_delay(interval: u64, failures: u32) -> Duration {
    // 30s, 60s, 120s ... up to one hour for the normal 15-second interval.
    Duration::from_secs(interval.saturating_mul(1u64 << failures.min(12)).min(3600))
}

fn provider_token(provider: &Provider, issued: u64) -> String {
    let header =
        URL_SAFE_NO_PAD.encode(json!({"alg": "ES256", "kid": provider.config.key_id}).to_string());
    let claims =
        URL_SAFE_NO_PAD.encode(json!({"iss": provider.config.team_id, "iat": issued}).to_string());
    let input = format!("{header}.{claims}");
    let signature: Signature = provider.key.sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}
pub(crate) fn number(data: &Value, key: &str, min: f64, max: f64) -> Option<f64> {
    data[key]
        .as_f64()
        .or_else(|| data[key].as_str()?.parse().ok())
        .filter(|v| v.is_finite() && *v >= min && *v <= max)
}
pub(crate) fn bars(rsrp: Option<f64>) -> Option<u8> {
    rsrp.map(|v| {
        if v >= -80.0 {
            4
        } else if v >= -90.0 {
            3
        } else if v >= -100.0 {
            2
        } else if v >= -110.0 {
            1
        } else {
            0
        }
    })
}
pub(crate) fn lte_carrier_count(data: &Value) -> Option<usize> {
    let mut carriers = std::collections::HashSet::new();
    for row in data["lteca"].as_str()?.split(';') {
        let columns: Vec<_> = row.split(',').collect();
        if columns.len() < 5 {
            continue;
        }
        let (Ok(pci), Ok(band), Ok(channel)) = (
            columns[0].parse::<u16>(),
            columns[1].parse::<u16>(),
            columns[3].parse::<u32>(),
        ) else {
            continue;
        };
        if pci <= 503 && band > 0 {
            carriers.insert((pci, band, channel));
        }
    }
    if carriers.is_empty() {
        None
    } else {
        Some(carriers.len())
    }
}

fn payload(data: &Value, sampled: u64, stale_after: u64) -> Value {
    let lte = number(data, "lte_rsrp", -160.0, -20.0);
    let nr = number(data, "nr5g_rsrp", -160.0, -20.0);
    json!({"aps": {"timestamp": sampled, "event": "update", "stale-date": sampled + stale_after,
    "content-state": {"schemaVersion": 1, "sampledAt": sampled,
        "networkType": data["network_type"].as_str(), "lteCarrierCount": lte_carrier_count(data),
        "lteRsrp": lte, "nrRsrp": nr, "lteBars": bars(lte), "nrBars": bars(nr),
        "lteSinr": number(data, "lte_snr", -50.0, 100.0), "nrSinr": number(data, "nr5g_snr", -50.0, 100.0)
    }}})
}
enum Outcome {
    Accepted,
    Retry(&'static str),
    Stop(&'static str),
}
fn send(
    client: &reqwest::blocking::Client,
    provider: &Provider,
    activity: &Registration,
    jwt: &str,
    payload: &Value,
) -> Outcome {
    let response = client
        .post(format!(
            "{}/3/device/{}",
            provider.config.environment.host(),
            activity.push_token
        ))
        .bearer_auth(jwt)
        .header(
            "apns-topic",
            format!("{}.push-type.liveactivity", provider.config.bundle_id),
        )
        .header("apns-push-type", "liveactivity")
        .header("apns-priority", provider.config.push_priority.to_string())
        // Keep only the latest sample until its freshness deadline, allowing brief delivery delays.
        .header(
            "apns-expiration",
            payload["aps"]["stale-date"]
                .as_u64()
                .unwrap_or(0)
                .min(activity.expires_at)
                .to_string(),
        )
        .header("apns-collapse-id", "openu60-signal")
        .json(payload)
        .send();
    let Ok(response) = response else {
        return Outcome::Retry("APNs transport failed");
    };
    if response.version() != reqwest::Version::HTTP_2 {
        return Outcome::Stop("APNs did not negotiate HTTP/2");
    }
    let status = response.status().as_u16();
    if status == 200 {
        return Outcome::Accepted;
    }
    let mut body = Vec::new();
    let _ = response.take(4096).read_to_end(&mut body);
    let reason = serde_json::from_slice::<Value>(&body).ok();
    classify_rejection(status, reason.as_ref().and_then(|v| v["reason"].as_str()))
}
fn classify_rejection(status: u16, reason: Option<&str>) -> Outcome {
    match reason {
        Some("BadDeviceToken" | "DeviceTokenNotForTopic" | "Unregistered") => Outcome::Stop("activity token invalid, expired, or wrong environment/topic"),
        _ if status == 429 || status >= 500 => Outcome::Retry("APNs throttled or unavailable"),
        _ => Outcome::Stop("APNs rejected configuration or payload; check Apple credentials, topic, environment and clock"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mode_payload_preserves_serving_mode_and_deduplicates_reported_carriers() {
        let data = json!({"network_type":"LTE-NSA", "lteca":"265,3,0,1775,15;265,1,2,150,20;265,1,2,150,20;bad;"});
        let p = payload(&data, 100, 30);
        assert_eq!(p["aps"]["content-state"]["networkType"], "LTE-NSA");
        assert_eq!(p["aps"]["content-state"]["lteCarrierCount"], 2);
        assert_eq!(lte_carrier_count(&json!({"lteca":"0,0,0,0,0;"})), None);
    }

    pub(super) fn test_service() -> LiveActivity {
        let config: Config = serde_json::from_value(json!({
            "enabled":true,"team_id":"ABCDE12345","key_id":"FGHIJ67890",
            "bundle_id":"test.app","key_file":"/test.p8","environment":"sandbox",
            "interval_seconds":15,"stale_after_seconds":30
        }))
        .unwrap();
        LiveActivity {
            provider: Some(Provider {
                config,
                key: SigningKey::from_slice(&[1; 32]).unwrap(),
            }),
            state: Mutex::new(State {
                recovery: recovery::Recovery::verified(now()),
                ..State::default()
            }),
            changed: Condvar::new(),
        }
    }

    #[test]
    fn repeated_registration_preserves_failure_state_and_generation() {
        let service = test_service();
        let body =
            serde_json::to_vec(&json!({"push_token":"ab".repeat(40),"expires_at":now()+3600}))
                .unwrap();
        assert_eq!(service.register(&body).0, 200);
        let generation = {
            let mut state = service.state.lock().unwrap();
            state.last_error = Some("APNs transport failed");
            state.last_accepted_at = Some(42);
            state.generation
        };
        for _ in 0..100 {
            assert_eq!(service.register(&body).0, 200);
        }
        let state = service.state.lock().unwrap();
        assert_eq!(state.generation, generation);
        assert_eq!(state.last_error, Some("APNs transport failed"));
        assert_eq!(state.last_accepted_at, Some(42));
    }

    #[test]
    fn offline_retries_are_bounded_and_success_restores_normal_cadence() {
        let delays: Vec<_> = (1..=10).map(|n| retry_delay(15, n).as_secs()).collect();
        assert_eq!(delays, [30, 60, 120, 240, 480, 960, 1920, 3600, 3600, 3600]);
        assert_eq!(retry_delay(15, u32::MAX).as_secs(), 3600);
        assert_eq!(retry_delay(15, 0).as_secs(), 15);
    }

    #[test]
    fn idle_worker_waits_and_registration_wakes_it() {
        use std::sync::mpsc;
        let service = Arc::new(test_service());
        let worker = Arc::clone(&service);
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let result = worker.wait_for_attempt(&mut Instant::now(), &mut 0, &mut 0);
            tx.send(result.1).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        service.register(
            &serde_json::to_vec(&json!({"push_token":"ab".repeat(40),"expires_at":now()+3600}))
                .unwrap(),
        );
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        handle.join().unwrap();
    }

    #[test]
    fn duplicate_registration_does_not_interrupt_backoff() {
        use std::sync::mpsc;
        let service = Arc::new(test_service());
        let body =
            serde_json::to_vec(&json!({"push_token":"ab".repeat(40),"expires_at":now()+3600}))
                .unwrap();
        service.register(&body);
        let worker = Arc::clone(&service);
        let deadline = Instant::now() + Duration::from_millis(250);
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            worker.wait_for_attempt(&mut { deadline }, &mut 1, &mut 3);
            tx.send(Instant::now()).unwrap();
        });
        service.register(&body);
        let fired = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(fired >= deadline);
        handle.join().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn offline_backoff_sleeps_with_negligible_thread_cpu() {
        fn cpu_ns() -> u64 {
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: ts points to writable timespec storage for this thread's clock.
            assert_eq!(
                unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) },
                0
            );
            ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
        }
        let service = test_service();
        service.register(
            &serde_json::to_vec(&json!({"push_token":"ab".repeat(40),"expires_at":now()+3600}))
                .unwrap(),
        );
        let mut next = Instant::now() + Duration::from_secs(2);
        let before = cpu_ns();
        service.wait_for_attempt(&mut next, &mut 1, &mut 4);
        let used = cpu_ns() - before;
        eprintln!(
            "Offline scheduler thread: {} microseconds CPU over 2 seconds waiting",
            used / 1000
        );
        assert!(
            used < 20_000_000,
            "backoff should consume under 1% of one CPU"
        );
    }

    #[test]
    fn unreachable_apple_returns_retry_without_unbounded_wait() {
        let service = test_service();
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .resolve("api.sandbox.push.apple.com", "127.0.0.1:9".parse().unwrap())
            .connect_timeout(Duration::from_millis(100))
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let before = Instant::now();
        let result = send(
            &client,
            service.provider.as_ref().unwrap(),
            &Registration {
                push_token: "ab".repeat(40),
                expires_at: now() + 60,
            },
            "test-only-not-a-real-credential",
            &payload(&json!({}), now(), 30),
        );
        assert!(matches!(result, Outcome::Retry("APNs transport failed")));
        assert!(before.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn apns_client_builds_and_only_network_events_move_the_revision() {
        assert!(apns_client().is_ok());
        let service = test_service();
        let body =
            serde_json::to_vec(&json!({"push_token":"ab".repeat(40),"expires_at":now()+3600}))
                .unwrap();
        assert_eq!(service.register(&body).0, 200);
        assert_eq!(service.register(&body).0, 200);
        assert_eq!(
            service.network_revision(),
            0,
            "registration keeps the connection"
        );
        service.recovery_event(
            "router_event_wan_connect_status",
            &json!({"wan_status":"disconnected"}),
        );
        assert_eq!(service.network_revision(), 1);
        service.recovery_event("zte_agent.sim_identity", &json!({"phase":"changed"}));
        service.recovery_event(
            "network.interface",
            &json!({"interface":"lan","action":"ifup"}),
        );
        assert_eq!(service.unregister().0, 200);
        assert_eq!(service.network_revision(), 1, "non-network events keep it");
        service.recovery_event("zte_agent.sim_identity", &json!({"phase":"leaving"}));
        assert_eq!(service.network_revision(), 2);
    }

    #[test]
    fn priority_defaults_to_low_and_rejects_invalid_values() {
        assert_eq!(default_push_priority(), 5);
        let raw = json!({"enabled":true,"team_id":"ABCDE12345","key_id":"FGHIJ67890",
            "bundle_id":"com.example.OpenU60","key_file":"/private/key.p8",
            "environment":"sandbox","interval_seconds":15,"stale_after_seconds":30});
        let mut config: Config = serde_json::from_value(raw).unwrap();
        assert_eq!(config.push_priority, 5);
        assert!(config.validate().is_ok());
        config.push_priority = 10;
        assert!(config.validate().is_ok());
        config.push_priority = 7;
        assert!(config.validate().is_err());
    }

    #[test]
    fn apple_rejections_stop_bad_tokens_and_retry_transient_failures() {
        assert!(matches!(
            classify_rejection(410, Some("Unregistered")),
            Outcome::Stop(_)
        ));
        assert!(matches!(
            classify_rejection(400, Some("DeviceTokenNotForTopic")),
            Outcome::Stop(_)
        ));
        assert!(matches!(
            classify_rejection(403, Some("InvalidProviderToken")),
            Outcome::Stop(_)
        ));
        assert!(matches!(
            classify_rejection(429, Some("TooManyRequests")),
            Outcome::Retry(_)
        ));
        assert!(matches!(classify_rejection(503, None), Outcome::Retry(_)));
    }
    #[test]
    fn private_files_reject_public_permissions_and_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let path =
            std::env::temp_dir().join(format!("zte-apns-permissions-{}", std::process::id()));
        let link = path.with_extension("link");
        fs::write(&path, "test-only").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(private_file(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(private_file(&path).unwrap(), "test-only");
        symlink(&path, &link).unwrap();
        assert!(private_file(&link).is_err());
        fs::remove_file(link).unwrap();
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn invalid_measurements_are_unknown_not_full_bars() {
        let p = payload(&json!({"lte_rsrp": "0", "nr5g_rsrp": "NaN"}), 100, 30);
        assert!(p["aps"]["content-state"]["lteBars"].is_null());
        assert!(p["aps"]["content-state"]["nrBars"].is_null());
    }
    #[test]
    fn payload_keeps_lte_nr_separate_and_timestamped() {
        let p = payload(&json!({"lte_rsrp": "-95", "nr5g_rsrp": -79}), 100, 30);
        assert_eq!(p["aps"]["content-state"]["lteBars"], 2);
        assert_eq!(p["aps"]["content-state"]["nrBars"], 4);
        assert_eq!(p["aps"]["stale-date"], 130);
    }
    #[test]
    fn registration_rejects_injection_and_unbounded_lifetime() {
        assert!(Registration {
            push_token: "a\r\nheader".into(),
            expires_at: 200
        }
        .validate(100)
        .is_err());
        assert!(Registration {
            push_token: "ab".repeat(32),
            expires_at: 30000
        }
        .validate(100)
        .is_err());
        assert!(Registration {
            push_token: "ab".repeat(32),
            expires_at: 200
        }
        .validate(100)
        .is_ok());
    }
    #[test]
    fn provider_jwt_is_verifiable_es256_with_raw_signature() {
        use p256::ecdsa::signature::Verifier;
        let key = SigningKey::from_slice(&[1u8; 32]).unwrap();
        let provider = Provider {
            key,
            config: Config {
                enabled: true,
                team_id: "ABCDEFGHIJ".into(),
                key_id: "1234567890".into(),
                bundle_id: "test.app".into(),
                key_file: "/test.p8".into(),
                environment: Environment::Sandbox,
                interval_seconds: 15,
                stale_after_seconds: 30,
                push_priority: 5,
            },
        };
        let token = provider_token(&provider, 100);
        let parts: Vec<_> = token.split('.').collect();
        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        provider
            .key
            .verifying_key()
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap()
                ["iat"],
            100
        );
    }
}
