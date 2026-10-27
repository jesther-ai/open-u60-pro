//! On-demand dashboard topics. These getters never start scans, send messages or run tests.
use crate::handlers::AppState;
use crate::{
    at_terminal, cell, device_ext, esim, handlers, modem_ext, network_ext, router, sms_forward,
    speedtest,
};
use serde_json::{json, Value};

pub(crate) fn read(state: &AppState, name: &str) -> Value {
    match name {
        "client_details" => state.client_details.snapshot(),
        "esim_status" => esim::dashboard_status(state),
        "operators" => json!({"ok":true,"data":{
            "status":modem_ext::modem_scan_status(state).1,
            "results":modem_ext::modem_scan_results(state).1,
            "registration":modem_ext::modem_register_result(state).1,
            "sampled_at":now_ms(),
        }}),
        "forwarding" => sms_forward::dashboard_status(state),
        "resources" => json!({"ok":true,"data":{
            "cpu":handlers::cpu(state).1,"memory":handlers::memory(state).1,
            "thermal":device_ext::device_thermal(state).1,
            "processes":handlers::system_top(state).1,
            "wan6":network_ext::network_wan6(state).1,
            "lan":network_ext::network_lan_status(state).1,
            "sampled_at":now_ms(),
        }}),
        "neighbors" => json!({"ok":true,"data":{
            "lte":cell::cell_neighbors_lte(state).1,
            "nr":cell::cell_neighbors_nr(state).1,"sampled_at":now_ms(),
        }}),
        "sweep" => json!({"ok":true,"data":{
            "progress":cell::cell_signal_detect_progress(state).1,
            "results":cell::cell_signal_detect_results(state).1,"sampled_at":now_ms(),
        }}),
        "speedtest" => speedtest::progress(state).1,
        "at_port" => at_terminal::at_port(state).1,
        "lan_config" => router::router_lan_get(state).1,
        "firewall_config" => router::router_firewall_get(state).1,
        "upnp_config" => router::router_firewall_upnp_get(state).1,
        "port_rules" => router::router_firewall_port_forward_get(state).1,
        "qos_config" => router::router_qos_get(state).1,
        _ => json!({"ok":false,"error":"unknown dashboard topic"}),
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
