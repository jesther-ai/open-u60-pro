mod at_cmd;
mod at_terminal;
mod auth;
mod cell;
mod charge_policy;
mod device_ext;
pub mod doh;
mod event_bus;
mod handlers;
mod lan_test;
mod modem_ext;
mod network_ext;
mod router;
mod scheduler;
mod server;
mod sim;
mod sms;
mod sms_forward;
mod speedtest;
mod system;
mod tailscale;
mod telephony;
mod ubus;
mod usb;
mod wifi;

use std::sync::Arc;

use event_bus::EventBus;
use handlers::AppState;

const DEFAULT_BIND: &str = "0.0.0.0:9090";
// 4 workers: tailscale setup/logout are legitimately slow (up to ~1-2 min
// against the control plane) and must not starve the rest of the API.
const DEFAULT_THREADS: usize = 4;

fn main() {
    let bind = std::env::var("ZTE_AGENT_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let threads: usize = std::env::var("ZTE_AGENT_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_THREADS);

    let state = Arc::new(AppState::new());

    // Set password from environment if provided
    if let Ok(pw) = std::env::var("ZTE_AGENT_PASSWORD") {
        state.auth.set_password(&pw);
    }

    // Event bus: single `ubus listen` process dispatches to subscribers
    let event_bus = EventBus::new();
    let sms_rx = event_bus.subscribe("zwrt_wms_status_event");
    let charger_rx = event_bus.subscribe("BSP_CHARGER_EVENT");
    let service_rx = event_bus.subscribe("zwrt_servicestatus");
    let wan_status_rx = event_bus.subscribe("router_event_wan_connect_status");
    let ts_wan_rx = event_bus.subscribe("router_event_wan_connect_status");
    event_bus.start();

    state.doh.auto_start();
    state.scheduler.start(Arc::clone(&state));
    state.charge_limit.start(charger_rx);
    state.sms_forward.start(sms_rx, service_rx, wan_status_rx);
    state.tailscale.auto_start();
    state.tailscale.start_wan_watch(ts_wan_rx);

    server::start(&bind, threads, state);
}
