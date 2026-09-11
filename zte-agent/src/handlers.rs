use std::sync::Arc;

use serde_json::{json, Value};

use crate::at_cmd::AtPort;
use crate::auth::AuthState;
use crate::charge_policy::ChargeLimitEnforcer;
use crate::scheduler::Scheduler;
use crate::sms_forward::SmsForwarder;
use crate::system::{self, CpuTracker, ProcessTracker, SpeedTracker};
use crate::ubus;

pub struct AppState {
    pub auth: AuthState,
    pub cpu: CpuTracker,
    pub speed: SpeedTracker,
    pub proc_tracker: ProcessTracker,
    pub at_port: AtPort,
    pub doh: std::sync::Arc<crate::doh::DohProxy>,
    pub scheduler: Arc<Scheduler>,
    pub speedtest: crate::speedtest::SpeedTest,
    pub charge_limit: Arc<ChargeLimitEnforcer>,
    pub sms_forward: Arc<SmsForwarder>,
    pub tailscale: Arc<crate::tailscale::TailscaleManager>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            auth: AuthState::new(),
            cpu: CpuTracker::new(),
            speed: SpeedTracker::new(),
            proc_tracker: ProcessTracker::new(),
            at_port: AtPort::new(),
            doh: std::sync::Arc::new(crate::doh::DohProxy::new()),
            scheduler: Arc::new(Scheduler::new()),
            speedtest: crate::speedtest::SpeedTest::new(),
            charge_limit: Arc::new(ChargeLimitEnforcer::new()),
            sms_forward: Arc::new(SmsForwarder::new()),
            tailscale: Arc::new(crate::tailscale::TailscaleManager::new()),
        }
    }
}

/// POST /api/auth/login — body: {"password": "..."}
pub fn login(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let password = match parsed["password"].as_str() {
        Some(p) => p,
        None => return (400, json!({"ok": false, "error": "missing 'password' field"})),
    };
    match state.auth.login(password) {
        Some(token) => (200, json!({"ok": true, "data": {"token": token}})),
        None => (401, json!({"ok": false, "error": "invalid password"})),
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

/// GET /api/network/signal
pub fn network_signal(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
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

/// GET /api/modem/status
pub fn modem_status(_state: &AppState) -> (u16, Value) {
    match ubus::uci_get("zte_nwinfo.sys_info.operate_mode") {
        Ok(mode) => (200, json!({"ok": true, "data": {"operate_mode": mode}})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// GET /api/data-usage
pub fn data_usage(_state: &AppState) -> (u16, Value) {
    let section = "zwrt_data_commit.wwancid1dst";

    let read_period = |prefix: &str| -> Value {
        let get = |suffix: &str| -> Value {
            let key = format!("{section}.{prefix}_{suffix}");
            match ubus::uci_get(&key) {
                Ok(v) => v.parse::<u64>().map(Value::from).unwrap_or(Value::from(v)),
                Err(_) => Value::Null,
            }
        };
        json!({
            "tx_bytes": get("tx_bytes"),
            "rx_bytes": get("rx_bytes"),
            "time_secs": get("time"),
            "tx_packets": get("tx_packets"),
            "rx_packets": get("rx_packets"),
        })
    };

    (
        200,
        json!({
            "ok": true,
            "data": {
                "day": read_period("day"),
                "month": read_period("month"),
                "total": read_period("total"),
            }
        }),
    )
}

/// POST /api/modem/online
pub fn modem_online(state: &AppState) -> (u16, Value) {
    use crate::at_cmd;
    match at_cmd::send(&state.at_port, "AT+CFUN=1", 8) {
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
