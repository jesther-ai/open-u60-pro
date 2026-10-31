use serde_json::{json, Value};

use crate::cache;
use crate::handlers::AppState;
use crate::ubus;

pub fn modem_data_get(state: &AppState) -> (u16, Value) {
    match wwan(state) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// `zwrt_data get_wwaniface` for the data call, through the shared cache.
pub(crate) fn wwan(state: &AppState) -> Result<Value, String> {
    let fetch = || ubus::call("zwrt_data", "get_wwaniface", Some(r#"{"cid":1}"#));
    state.cache.get_or_fetch(cache::MODEM_DATA, cache::MODEM_DATA_TTL, fetch)
}

/// The data daemon applies mode changes asynchronously, so after a local
/// write the modem/data entry is dropped and reads stay near-fresh for the
/// settle window (the apps run 2 s confirmation polls against it).
fn settle_modem_data(state: &AppState) {
    state
        .cache
        .invalidate_with_settle(cache::MODEM_DATA, cache::SETTLE_WINDOW);
}

/// A user-initiated modem change: the no-service policy and the connectivity
/// watchdog hold off for 2 min.
pub(crate) fn note_user_modem_change(state: &AppState) -> std::sync::MutexGuard<'_, ()> {
    let guard = state.esim.note_user_modem_change();
    let now = std::time::Instant::now();
    state.oos_policy.set_user_activity(now);
    state.conn_watchdog.set_user_activity(now);
    guard
}

/// POST /api/modem/online from the app: AT+CFUN=1 plus the user-activity
/// bookkeeping (the policies' own effects send AT+CFUN=1 directly and never
/// come through here).
pub fn modem_online_user(state: &AppState) -> (u16, Value) {
    let _command = note_user_modem_change(state);
    state.oos_policy.user_mode(true);
    crate::handlers::modem_online(state)
}

pub fn modem_data_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let _command = note_user_modem_change(state);
    let result = ubus::call("zwrt_data", "set_wwaniface", Some(&parsed.to_string()));
    settle_modem_data(state);
    match result {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_airplane(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let _command = note_user_modem_change(state);
    // Use AT+CFUN=1 for ONLINE (ubus nwinfo_set_mode ONLINE is broken for LPM→ONLINE recovery)
    if parsed["operate_mode"].as_str() == Some("ONLINE") {
        // modem_online settles the modem/data entry itself.
        state.oos_policy.user_mode(true);
        return crate::handlers::modem_online(state);
    }
    let result = ubus::call(
        "zte_nwinfo_api",
        "nwinfo_set_mode",
        Some(&parsed.to_string()),
    );
    settle_modem_data(state);
    match result {
        Ok(data) => {
            // The user parked the modem (LPM/OFFLINE): the no-service policy
            // stands down until the user goes ONLINE again.
            state.oos_policy.user_mode(false);
            (200, json!({"ok": true, "data": data}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_network_mode_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let _command = note_user_modem_change(state);
    let result = ubus::call(
        "zte_nwinfo_api",
        "nwinfo_set_netselect",
        Some(&parsed.to_string()),
    );
    settle_modem_data(state);
    match result {
        Ok(data) => {
            match crate::dns_control::network_preference_changed(state) {
                Ok(()) => eprintln!("[dns] network preference changed: caches and sessions reset"),
                Err(error) => {
                    eprintln!("[dns] network preference changed: immediate reset failed: {error}")
                }
            }
            (200, json!({"ok": true, "data": data}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_scan(state: &AppState) -> (u16, Value) {
    let _command = note_user_modem_change(state);
    match ubus::call("zte_nwinfo_api", "nwinfo_manual_scan", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_scan_status(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_m_netselect_status", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_scan_results(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_m_netselect_contents", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_register(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let _command = note_user_modem_change(state);
    match ubus::call("zte_nwinfo_api", "nwinfo_manual_register", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn modem_register_result(_state: &AppState) -> (u16, Value) {
    match ubus::call("zte_nwinfo_api", "nwinfo_m_netselect_result", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

