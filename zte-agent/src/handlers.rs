use std::sync::Arc;

use serde_json::{json, Value};

use crate::at_cmd::AtPort;
use crate::auth::{self, AuthState, Kind, LoginError, PeerKey};
use crate::cache::{self, UbusCache};
use crate::charge_policy::ChargeLimitEnforcer;
use crate::scheduler::Scheduler;
use crate::sms_forward::SmsForwarder;
use crate::system::{self, CpuTracker, ProcessTracker, SpeedTracker};
use crate::ubus;

pub struct AppState {
    pub client_details: crate::client_details::ClientMonitor,
    pub dashboard: Arc<crate::dashboard::Dashboard>,
    pub live_activity: Arc<crate::live_activity::LiveActivity>,
    pub auth: Arc<AuthState>,
    pub cpu: CpuTracker,
    pub speed: SpeedTracker,
    pub proc_tracker: ProcessTracker,
    pub at_port: AtPort,
    pub stk: crate::stk::StkService,
    pub esim: crate::esim::Esim,
    pub doh: std::sync::Arc<crate::doh::DohProxy>,
    pub scheduler: Arc<Scheduler>,
    pub speedtest: crate::speedtest::SpeedTest,
    pub charge_limit: Arc<ChargeLimitEnforcer>,
    pub sms_forward: Arc<SmsForwarder>,
    pub tailscale: Arc<crate::tailscale::TailscaleManager>,
    pub ttl: Arc<crate::ttl::TtlManager>,
    /// Single-flight TTL cache in front of the `ubus` CLI (see cache.rs).
    pub cache: Arc<UbusCache>,
    pub oos_policy: Arc<crate::oos_policy::OosPolicy>,
    pub conn_watchdog: Arc<crate::conn_watchdog::ConnWatchdog>,
    /// A voice call is (believed to be) in progress; set by the telephony
    /// handlers, re-checked with `AT+CLCC` by the policies' busy guard.
    pub call_active: std::sync::atomic::AtomicBool,
    /// Unix seconds of the last USSD prompt that still needs a reply (0 = none).
    pub ussd_prompt_at: std::sync::atomic::AtomicU64,
    /// Tracks the active SIM's identity across profile switches and swaps (see sim_identity.rs).
    pub sim_identity: Arc<crate::sim_identity::SimIdentity>,
    /// Live updates for `GET /api/events` (see events/); no threads until `events::start`.
    pub hub: Arc<crate::events::Hub>,
    /// LAN HTTPS with the router's own CA (see tls/); nothing until `start` or Turn On.
    pub tls: Arc<crate::tls::Tls>,
}

impl AppState {
    pub fn new() -> Self {
        let live_activity = Arc::new(crate::live_activity::LiveActivity::new());
        Self {
            client_details: crate::client_details::ClientMonitor::default(),
            dashboard: Arc::new(crate::dashboard::Dashboard::default()),
            tls: Arc::new(crate::tls::Tls::router(Arc::clone(&live_activity))),
            live_activity,
            auth: Arc::new(AuthState::new()),
            cpu: CpuTracker::new(),
            speed: SpeedTracker::new(),
            proc_tracker: ProcessTracker::new(),
            at_port: AtPort::new(),
            stk: crate::stk::StkService::new(),
            esim: crate::esim::Esim::new(),
            doh: std::sync::Arc::new(crate::doh::DohProxy::new()),
            scheduler: Arc::new(Scheduler::new()),
            speedtest: crate::speedtest::SpeedTest::new(),
            charge_limit: Arc::new(ChargeLimitEnforcer::new()),
            sms_forward: Arc::new(SmsForwarder::new()),
            tailscale: Arc::new(crate::tailscale::TailscaleManager::new()),
            ttl: Arc::new(crate::ttl::TtlManager::new()),
            cache: Arc::new(UbusCache::new()),
            oos_policy: Arc::new(crate::oos_policy::OosPolicy::new()),
            conn_watchdog: Arc::new(crate::conn_watchdog::ConnWatchdog::new()),
            call_active: std::sync::atomic::AtomicBool::new(false),
            ussd_prompt_at: std::sync::atomic::AtomicU64::new(0),
            sim_identity: Arc::new(crate::sim_identity::SimIdentity::new()),
            hub: Arc::new(crate::events::Hub::new(Arc::new(crate::boottime::BootClock))),
        }
    }
}

/// POST /api/auth/login — body: {"password": "..."}. `peer` is the socket
/// address the login rate limit keys on.
pub fn login(state: &AppState, body: &[u8], peer: PeerKey) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let password = match parsed["password"].as_str() {
        Some(p) => p,
        None => return (400, json!({"ok": false, "error": "missing 'password' field"})),
    };
    match state.auth.login(password, Kind::Bearer, peer, &[]) {
        Ok(issued) => (200, json!({"ok": true, "data": {"token": issued.token.expose()}})),
        Err(LoginError::NoPassword | LoginError::BadPassword) => {
            (401, json!({"ok": false, "error": "invalid password"}))
        }
        // The listener copies `retry_after` into a Retry-After header.
        Err(LoginError::RateLimited { retry_after }) => (
            429,
            json!({"ok": false, "error": "too many login attempts",
                   "retry_after": auth::retry_after_secs(retry_after)}),
        ),
        Err(LoginError::Entropy) => (503, json!({"ok": false, "error": "entropy unavailable"})),
    }
}

/// GET /api/device
pub fn device(_state: &AppState) -> (u16, Value) {
    let info = system::read_device_info();
    (
        200,
        json!({"ok": true, "data": {
            "hostname": info.hostname,
            "uptime_secs": info.uptime_secs,
            "load_avg": info.load_avg,
            "kernel": info.kernel,
        }}),
    )
}

/// GET /api/battery
pub fn battery(_state: &AppState) -> (u16, Value) {
    match system::read_battery() {
        Some(b) => (200, json!({"ok": true, "data": b})),
        None => (
            503,
            json!({"ok": false, "error": "battery info not available"}),
        ),
    }
}

/// GET /api/cpu
pub fn cpu(state: &AppState) -> (u16, Value) {
    let usage = state.cpu.sample();
    (200, json!({"ok": true, "data": usage}))
}

/// GET /api/memory
pub fn memory(_state: &AppState) -> (u16, Value) {
    match system::read_meminfo() {
        Some(m) => (200, json!({"ok": true, "data": m})),
        None => (
            503,
            json!({"ok": false, "error": "memory info not available"}),
        ),
    }
}

/// GET /api/network/signal — `nwinfo_get_netinfo` plus `bearer`, the QoS the
/// network granted on the data session (see bearer.rs). Overlapping pollers
/// share one read for `NETINFO_TTL`.
pub fn network_signal(state: &AppState) -> (u16, Value) {
    match state.cache.netinfo(cache::NETINFO_TTL) {
        Ok(mut data) => {
            if let Some(obj) = data.as_object_mut() {
                obj.insert("bearer".into(), crate::bearer::read());
                state.sim_identity.stamp(obj);
            }
            (200, json!({"ok": true, "data": data}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// GET /api/network/traffic
pub fn network_traffic(_state: &AppState) -> (u16, Value) {
    let ifaces = system::read_network_traffic();
    (200, json!({"ok": true, "data": ifaces}))
}

/// GET /api/network/speed — server-computed speed with precise timing
pub fn network_speed(state: &AppState) -> (u16, Value) {
    let snap = state.speed.sample();
    (200, json!({"ok": true, "data": snap}))
}

/// GET /api/modem/status — `zte_nwinfo.sys_info.operate_mode` read the way
/// `uci get` reads it (committed file plus the uncommitted delta), without
/// the fork. No TTL: the daemon rewrites this key asynchronously after mode
/// toggles, so any cache would pin stale state. Falls back to `uci get` on
/// any miss so the result is always what the CLI would have printed.
pub fn modem_status(_state: &AppState) -> (u16, Value) {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let from_files = read(UCI_NWINFO_CONFIG).and_then(|config| {
        uci_operate_mode(&config, read(UCI_NWINFO_DELTA).as_deref().unwrap_or(""))
    });
    let mode = match from_files {
        Some(mode) => Ok(mode),
        None => ubus::uci_get(UCI_OPERATE_MODE_KEY),
    };
    match mode {
        Ok(mode) => (200, json!({"ok": true, "data": {"operate_mode": mode}})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

const UCI_NWINFO_CONFIG: &str = "/etc/config/zte_nwinfo";
const UCI_NWINFO_DELTA: &str = "/tmp/.uci/zte_nwinfo";
const UCI_OPERATE_MODE_KEY: &str = "zte_nwinfo.sys_info.operate_mode";

/// Resolve `zte_nwinfo.sys_info.operate_mode` from the committed config text
/// and the delta (`uci set` without `commit`) text. `None` means "not sure":
/// the option is absent, deleted, or the delta does something this reader
/// does not model (section add/rename), and the caller must ask `uci get`.
fn uci_operate_mode(config: &str, delta: &str) -> Option<String> {
    let mut value = uci_config_option(config, "sys_info", "operate_mode");
    for line in delta.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (lhs, rhs) = match line.split_once('=') {
            Some((lhs, rhs)) => (lhs, Some(rhs)),
            None => (line, None),
        };
        match lhs {
            // `uci set` of the key: last write wins.
            "zte_nwinfo.sys_info.operate_mode" => {
                let raw = rhs?.trim();
                if uci_value_torn(raw) {
                    return None;
                }
                value = Some(uci_unquote(raw));
            }
            // Deleting the option or the whole section removes it.
            "-zte_nwinfo.sys_info.operate_mode" | "-zte_nwinfo.sys_info" => value = None,
            // Section creation/rename or list ops touching sys_info: bail.
            _ if lhs.starts_with(['+', '@', '|', '~']) && line.contains("sys_info") => return None,
            _ => {}
        }
    }
    value
}

/// Value of `option <name>` inside the named section of a UCI config file.
fn uci_config_option(config: &str, section: &str, option: &str) -> Option<String> {
    let mut in_section = false;
    for line in config.lines() {
        let (keyword, rest) = split_first_word(line);
        let (second, rest) = split_first_word(rest);
        match keyword {
            // `config <type> '<name>'`; an anonymous section has no name.
            "config" => in_section = !rest.is_empty() && uci_unquote(rest) == section,
            "option" if in_section && second == option => return Some(uci_unquote(rest)),
            _ => {}
        }
    }
    None
}

/// `(first word, trimmed remainder)` of a line; both empty for a blank line.
fn split_first_word(line: &str) -> (&str, &str) {
    let line = line.trim();
    match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i..].trim()),
        None => (line, ""),
    }
}

/// `true` for an empty value or one whose opening quote never closes. `uci`
/// appends the delta under flock and this reader does not take it, so a torn
/// last line must fall back to the CLI instead of yielding `'LP`.
fn uci_value_torn(raw: &str) -> bool {
    match raw.chars().next() {
        None => true,
        Some(quote @ ('\'' | '"')) => raw.len() < 2 || !raw.ends_with(quote),
        Some(_) => false,
    }
}

/// Strip the quotes `uci` writes around a value and undo its `'\''` escape.
fn uci_unquote(raw: &str) -> String {
    let raw = raw.trim();
    if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        raw[1..raw.len() - 1].replace("'\\''", "'")
    } else if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        raw[1..raw.len() - 1].to_string()
    } else {
        raw.to_string()
    }
}

/// POST /api/modem/online
pub fn modem_online(state: &AppState) -> (u16, Value) {
    use crate::at_cmd;
    let result = at_cmd::send(&state.at_port, "AT+CFUN=1", 8);
    // The data session comes back asynchronously; keep the apps' post-write
    // confirmation polls fresh while it does.
    state
        .cache
        .invalidate_with_settle(cache::MODEM_DATA, cache::SETTLE_WINDOW);
    match result {
        Ok(resp) if resp.contains("OK") => (200, json!({"ok": true, "data": {"status": "ok"}})),
        Ok(resp) => (500, json!({"ok": false, "error": "AT+CFUN=1 failed", "raw": resp})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// GET /api/system/top
pub fn system_top(state: &AppState) -> (u16, Value) {
    let result = state.proc_tracker.sample();
    (200, json!({"ok": true, "data": result}))
}

/// POST /api/system/kill-bloat — body: {"all": true} or {"pids": [1, 2, 3]}
pub fn system_kill_bloat(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    let pids: Option<Vec<u32>> = if parsed["all"].as_bool() == Some(true) {
        None
    } else if let Some(arr) = parsed["pids"].as_array() {
        let ids: Vec<u32> = arr.iter().filter_map(|v| v.as_u64().map(|n| n as u32)).collect();
        if ids.is_empty() {
            return (400, json!({"ok": false, "error": "pids array is empty"}));
        }
        Some(ids)
    } else {
        return (400, json!({"ok": false, "error": "expected 'all' or 'pids'"}));
    };

    let result = system::kill_bloat(pids.as_deref());
    (200, json!({"ok": true, "data": result}))
}

#[cfg(test)]
mod uci_tests {
    use super::*;

    const CONFIG: &str = "
config sys_info 'sys_info'
	option net_select 'LTE_AND_5G'
	option operate_mode 'ONLINE'
	option ps_no_srv_check_timer_cfg '5'

config other 'other'
	option operate_mode 'DECOY'
";

    #[test]
    fn reads_committed_value_when_no_delta() {
        assert_eq!(uci_operate_mode(CONFIG, "").as_deref(), Some("ONLINE"));
    }

    #[test]
    fn delta_overrides_committed_value_last_write_wins() {
        let delta = "zte_nwinfo.sys_info.net_select='Only_5G'\n\
                     zte_nwinfo.sys_info.operate_mode='LPM'\n\
                     zte_nwinfo.sys_info.operate_mode='ONLINE'\n\
                     zte_nwinfo.sys_info.operate_mode='LPM'\n";
        assert_eq!(uci_operate_mode(CONFIG, delta).as_deref(), Some("LPM"));
        // Older uci wrote the delta unquoted.
        assert_eq!(
            uci_operate_mode(CONFIG, "zte_nwinfo.sys_info.operate_mode=LPM").as_deref(),
            Some("LPM")
        );
    }

    #[test]
    fn delta_removal_and_unmodelled_ops_force_the_cli_fallback() {
        assert_eq!(
            uci_operate_mode(CONFIG, "-zte_nwinfo.sys_info.operate_mode"),
            None
        );
        assert_eq!(uci_operate_mode(CONFIG, "-zte_nwinfo.sys_info"), None);
        assert_eq!(
            uci_operate_mode(CONFIG, "@zte_nwinfo.cfg0123=sys_info"),
            None
        );
        // Deltas for other sections are ignored.
        assert_eq!(
            uci_operate_mode(CONFIG, "-zte_nwinfo.other\n+zte_nwinfo.cfg9=foo").as_deref(),
            Some("ONLINE")
        );
    }

    #[test]
    fn torn_delta_value_forces_the_cli_fallback() {
        for delta in [
            "zte_nwinfo.sys_info.operate_mode='LP",
            "zte_nwinfo.sys_info.operate_mode='",
            "zte_nwinfo.sys_info.operate_mode=\"LP",
            "zte_nwinfo.sys_info.operate_mode=",
            "zte_nwinfo.sys_info.operate_mode",
            "zte_nwinfo.sys_info.operate_mode='LPM'\nzte_nwinfo.sys_info.operate_mode='ON",
        ] {
            assert_eq!(uci_operate_mode(CONFIG, delta), None, "{delta:?}");
        }
        // An empty but properly quoted value is complete, not torn.
        assert_eq!(
            uci_operate_mode(CONFIG, "zte_nwinfo.sys_info.operate_mode=''").as_deref(),
            Some("")
        );
    }

    #[test]
    fn missing_option_or_section_is_a_miss() {
        assert_eq!(
            uci_operate_mode("config sys_info 'sys_info'\n\toption x 'y'\n", ""),
            None
        );
        assert_eq!(
            uci_operate_mode("config sys_info\n\toption operate_mode 'ONLINE'\n", ""),
            None
        );
        assert_eq!(
            uci_operate_mode("", "zte_nwinfo.sys_info.operate_mode='LPM'").as_deref(),
            Some("LPM")
        );
    }

    #[test]
    fn unquotes_single_double_and_escaped_values() {
        assert_eq!(uci_unquote("'ONLINE'"), "ONLINE");
        assert_eq!(uci_unquote("\"ONLINE\""), "ONLINE");
        assert_eq!(uci_unquote("ONLINE"), "ONLINE");
        assert_eq!(uci_unquote("'it'\\''s'"), "it's");
        assert_eq!(
            uci_config_option("config s 's'\n  option  o   \"v w\"  \n", "s", "o").as_deref(),
            Some("v w")
        );
        assert_eq!(
            uci_config_option("config s s\n\toption o v\n", "s", "o").as_deref(),
            Some("v")
        );
    }
}

#[cfg(test)]
mod login_tests {
    use super::*;
    use crate::auth::{AuthConfig, FailRng, SeqRng};
    use crate::boottime::FakeClock;
    use std::net::SocketAddr;

    fn peer(n: u8) -> PeerKey {
        PeerKey::from(SocketAddr::from(([10, 0, 210, n], 5000)))
    }

    #[test]
    fn login_entropy_and_no_password_replies() {
        let body = br#"{"password":"x"}"#;
        let mut st = AppState::new();
        st.auth = Arc::new(AuthState::with_parts(FakeClock::at_secs(1), Box::new(FailRng), AuthConfig::default()));
        st.auth.set_password("x");
        assert_eq!(login(&st, body, peer(1)), (503, json!({"ok": false, "error": "entropy unavailable"})));

        // No password set: the same 401 as a wrong one, and never rate limited.
        let mut st = AppState::new();
        st.auth = Arc::new(AuthState::with_parts(FakeClock::at_secs(1), Box::new(SeqRng::new()), AuthConfig::default()));
        for _ in 0..10 {
            assert_eq!(login(&st, body, peer(2)), (401, json!({"ok": false, "error": "invalid password"})));
        }
    }
}
