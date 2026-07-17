use std::sync::Arc;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::at_terminal;
use crate::cell;
use crate::device_ext;
use crate::lan_test;
use crate::scheduler;
use crate::handlers::{self, AppState};
use crate::modem_ext;
use crate::network_ext;
use crate::router;
use crate::sim;
use crate::sms;
use crate::sms_forward;
use crate::speedtest;
use crate::tailscale;
use crate::telephony;
use crate::usb;
use crate::wifi;

pub fn start(bind: &str, threads: usize, state: Arc<AppState>) {
    let server = match Server::http(bind) {
        Ok(s) => s,
        Err(_) => {
            std::process::exit(1);
        }
    };
    // Seed the CPU tracker with initial sample (speed tracker self-seeds)
    state.cpu.sample();

    let server = Arc::new(server);
    let mut handles = Vec::new();

    for _ in 0..threads {
        let server = Arc::clone(&server);
        let state = Arc::clone(&state);
        let handle = std::thread::spawn(move || {
            loop {
                let request = match server.recv() {
                    Ok(r) => r,
                    Err(_) => {
                        continue;
                    }
                };
                handle_request(request, &state);
            }
        });
        handles.push(handle);
    }

    for h in handles {
        let _ = h.join();
    }
}

fn handle_request(mut request: Request, state: &AppState) {
    let method = request.method().clone();
    let url = request.url().to_string();
    // Strip query string for routing
    let path = url.split('?').next().unwrap_or(&url);

    // Auth check — skip for login endpoint
    let needs_auth = path != "/api/auth/login";
    if needs_auth {
        let authorized = request
            .headers()
            .iter()
            .find(|h| h.field.as_str().to_ascii_lowercase() == "authorization")
            .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
            .map(|token| state.auth.validate(token))
            .unwrap_or(false);

        if !state.auth.has_password() {
            // No password set — allow unauthenticated access
        } else if !authorized {
            respond(request, 401, json!({"ok": false, "error": "unauthorized"}));
            return;
        }
    }

    // LAN test: download streams raw bytes, upload reads body incrementally.
    // Handle BEFORE the body is fully read into memory.
    match (&method, path) {
        (&Method::Get, "/api/lan/download") => {
            let size = parse_query_usize(&url, "size").unwrap_or(50 * 1024 * 1024);
            let size = size.min(200 * 1024 * 1024); // cap at 200 MB
            lan_test::download(request, size);
            return;
        }
        (&Method::Post, "/api/lan/upload") => {
            let (status, body_json) = lan_test::upload(&mut request);
            respond(request, status, body_json);
            return;
        }
        _ => {}
    }

    // Read body
    let mut body = Vec::new();
    let _ = std::io::Read::read_to_end(&mut request.as_reader(), &mut body);

    let (status, body_json) = route(&method, path, state, &body);
    respond(request, status, body_json);
}

fn parse_query_usize(url: &str, key: &str) -> Option<usize> {
    let query = url.split('?').nth(1)?;
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        if kv.next()? == key {
            return kv.next()?.parse().ok();
        }
    }
    None
}

pub fn route(method: &Method, path: &str, state: &AppState, body: &[u8]) -> (u16, Value) {
    match (method, path) {
        // Auth
        (&Method::Post, "/api/auth/login") => handlers::login(state, body),
        // Device info (sysfs)
        (&Method::Get, "/api/device") => handlers::device(state),
        (&Method::Get, "/api/battery") => handlers::battery(state),
        (&Method::Get, "/api/cpu") => handlers::cpu(state),
        (&Method::Get, "/api/memory") => handlers::memory(state),
        // Network
        (&Method::Get, "/api/network/signal") => handlers::network_signal(state),
        (&Method::Get, "/api/network/traffic") => handlers::network_traffic(state),
        (&Method::Get, "/api/network/speed") => handlers::network_speed(state),
        (&Method::Get, "/api/network/wan") => network_ext::network_wan(state),
        (&Method::Get, "/api/network/wan6") => network_ext::network_wan6(state),
        (&Method::Get, "/api/network/lan-status") => network_ext::network_lan_status(state),
        (&Method::Get, "/api/network/clients") => network_ext::network_clients(state),
        (&Method::Get, "/api/network/speeds") => network_ext::network_speeds(state),
        (&Method::Get, "/api/network/rmnet") => network_ext::network_rmnet(state),
        // Device (ubus)
        (&Method::Get, "/api/device/battery-info") => network_ext::network_battery_ubus(state),
        (&Method::Get, "/api/device/thermal") => device_ext::device_thermal(state),
        (&Method::Get, "/api/device/charger") => device_ext::device_charger(state),
        (&Method::Get, "/api/device/system") => device_ext::device_system(state),
        (&Method::Post, "/api/device/reboot") => device_ext::device_reboot(state),
        (&Method::Post, "/api/device/factory-reset") => device_ext::device_factory_reset(state),
        (&Method::Get, "/api/device/charge-control") => device_ext::charge_control_get(state),
        (&Method::Put, "/api/device/charge-control") => device_ext::charge_control_set(state, body),
        (&Method::Post, "/api/device/power-save") => device_ext::device_power_save_get(state, body),
        (&Method::Put, "/api/device/power-save") => device_ext::device_power_save_set(state, body),
        (&Method::Get, "/api/device/fast-boot") => device_ext::device_fast_boot_get(state),
        (&Method::Put, "/api/device/fast-boot") => device_ext::device_fast_boot_set(state, body),
        // System — process monitor
        (&Method::Get, "/api/system/top") => handlers::system_top(state),
        (&Method::Post, "/api/system/kill-bloat") => handlers::system_kill_bloat(state, body),
        // WiFi
        (&Method::Get, "/api/wifi/status") => wifi::wifi_status(state),
        (&Method::Put, "/api/wifi/settings") => wifi::wifi_set(state, body),
        (&Method::Get, "/api/wifi/guest") => wifi::guest_status(state),
        (&Method::Put, "/api/wifi/guest") => wifi::guest_set(state, body),
        // Modem
        (&Method::Get, "/api/data-usage") => handlers::data_usage(state),
        (&Method::Get, "/api/modem/status") => handlers::modem_status(state),
        (&Method::Post, "/api/modem/online") => handlers::modem_online(state),
        (&Method::Get, "/api/modem/data") => modem_ext::modem_data_get(state),
        (&Method::Put, "/api/modem/data") => modem_ext::modem_data_set(state, body),
        (&Method::Post, "/api/modem/airplane") => modem_ext::modem_airplane(state, body),
        (&Method::Put, "/api/modem/network-mode") => modem_ext::modem_network_mode_set(state, body),
        (&Method::Post, "/api/modem/scan") => modem_ext::modem_scan(state),
        (&Method::Get, "/api/modem/scan/status") => modem_ext::modem_scan_status(state),
        (&Method::Get, "/api/modem/scan/results") => modem_ext::modem_scan_results(state),
        (&Method::Post, "/api/modem/register") => modem_ext::modem_register(state, body),
        (&Method::Get, "/api/modem/register/result") => modem_ext::modem_register_result(state),
        // SMS
        (&Method::Post, "/api/sms/list") => sms::sms_list(state, body),
        (&Method::Get, "/api/sms/capacity") => sms::sms_capacity(state),
        (&Method::Post, "/api/sms/send") => sms::sms_send(state, body),
        (&Method::Post, "/api/sms/delete") => sms::sms_delete(state, body),
        (&Method::Post, "/api/sms/read") => sms::sms_mark_read(state, body),
        // SMS forwarding
        (&Method::Get, "/api/sms/forward/config") => sms_forward::config_get(state),
        (&Method::Put, "/api/sms/forward/config") => sms_forward::config_set(state, body),
        (&Method::Post, "/api/sms/forward/rules") => sms_forward::rules_create(state, body),
        (&Method::Put, "/api/sms/forward/rules") => sms_forward::rules_update(state, body),
        (&Method::Delete, "/api/sms/forward/rules") => sms_forward::rules_delete(state, body),
        (&Method::Put, "/api/sms/forward/rules/toggle") => sms_forward::rules_toggle(state, body),
        (&Method::Post, "/api/sms/forward/test") => sms_forward::test_forward(state, body),
        (&Method::Get, "/api/sms/forward/log") => sms_forward::log_get(state),
        (&Method::Post, "/api/sms/forward/log/clear") => sms_forward::log_clear(state),
        (&Method::Post, "/api/sms/forward/retry") => sms_forward::retry_forward(state, body),
        // SIM
        (&Method::Get, "/api/sim/info") => sim::sim_info(state),
        (&Method::Get, "/api/sim/imei") => sim::sim_imei(state),
        (&Method::Post, "/api/sim/pin/verify") => sim::sim_pin_verify(state, body),
        (&Method::Post, "/api/sim/pin/change") => sim::sim_pin_change(state, body),
        (&Method::Post, "/api/sim/pin/mode") => sim::sim_pin_mode(state, body),
        (&Method::Post, "/api/sim/unlock") => sim::sim_unlock(state, body),
        (&Method::Get, "/api/sim/lock-trials") => sim::sim_lock_trials(state),
        // Cell
        (&Method::Post, "/api/cell/lock/nr") => cell::cell_lock_nr(state, body),
        (&Method::Post, "/api/cell/lock/lte") => cell::cell_lock_lte(state, body),
        (&Method::Post, "/api/cell/lock/reset") => cell::cell_lock_reset(state),
        (&Method::Post, "/api/cell/neighbors/scan") => cell::cell_neighbors_scan(state),
        (&Method::Get, "/api/cell/neighbors/nr") => cell::cell_neighbors_nr(state),
        (&Method::Get, "/api/cell/neighbors/lte") => cell::cell_neighbors_lte(state),
        (&Method::Post, "/api/cell/band/nr") => cell::cell_band_nr(state, body),
        (&Method::Post, "/api/cell/band/lte") => cell::cell_band_lte(state, body),
        (&Method::Post, "/api/cell/band/reset") => cell::cell_band_reset(state),
        (&Method::Get, "/api/cell/stc/params") => cell::cell_stc_params_get(state),
        (&Method::Put, "/api/cell/stc/params") => cell::cell_stc_params_set(state, body),
        (&Method::Get, "/api/cell/stc/status") => cell::cell_stc_status(state),
        (&Method::Post, "/api/cell/stc/enable") => cell::cell_stc_enable(state),
        (&Method::Post, "/api/cell/stc/disable") => cell::cell_stc_disable(state),
        (&Method::Post, "/api/cell/stc/reset") => cell::cell_stc_reset(state),
        (&Method::Post, "/api/cell/signal-detect/start") => cell::cell_signal_detect_start(state),
        (&Method::Post, "/api/cell/signal-detect/stop") => cell::cell_signal_detect_stop(state),
        (&Method::Get, "/api/cell/signal-detect/results") => cell::cell_signal_detect_results(state),
        (&Method::Get, "/api/cell/signal-detect/progress") => cell::cell_signal_detect_progress(state),
        // Router
        (&Method::Get, "/api/router/dns") => router::router_dns_get(state),
        (&Method::Put, "/api/router/dns") => router::router_dns_set(state, body),
        (&Method::Get, "/api/router/lan") => router::router_lan_get(state),
        (&Method::Put, "/api/router/lan") => router::router_lan_set(state, body),
        (&Method::Get, "/api/router/firewall") => router::router_firewall_get(state),
        (&Method::Put, "/api/router/firewall/switch") => router::router_firewall_switch_set(state, body),
        (&Method::Put, "/api/router/firewall/level") => router::router_firewall_level_set(state, body),
        (&Method::Put, "/api/router/firewall/nat") => router::router_firewall_nat_set(state, body),
        (&Method::Put, "/api/router/firewall/dmz") => router::router_firewall_dmz_set(state, body),
        (&Method::Get, "/api/router/firewall/upnp") => router::router_firewall_upnp_get(state),
        (&Method::Put, "/api/router/firewall/upnp") => router::router_firewall_upnp_set(state, body),
        (&Method::Get, "/api/router/firewall/port-forward") => router::router_firewall_port_forward_get(state),
        (&Method::Post, "/api/router/firewall/port-forward") => router::router_firewall_port_forward_set(state, body),
        (&Method::Put, "/api/router/firewall/port-forward/switch") => router::router_firewall_port_forward_switch(state, body),
        (&Method::Get, "/api/router/firewall/filter-rules") => router::router_firewall_filter_rules(state),
        (&Method::Get, "/api/router/vpn") => router::router_vpn_get(state),
        (&Method::Put, "/api/router/vpn") => router::router_vpn_set(state, body),
        (&Method::Get, "/api/router/qos") => router::router_qos_get(state),
        (&Method::Put, "/api/router/qos") => router::router_qos_set(state, body),
        (&Method::Get, "/api/router/domain-filter") => router::router_domain_filter_get(state),
        (&Method::Put, "/api/router/domain-filter") => router::router_domain_filter_set(state, body),
        (&Method::Get, "/api/router/apn/mode") => router::router_apn_mode_get(state),
        (&Method::Put, "/api/router/apn/mode") => router::router_apn_mode_set(state, body),
        (&Method::Get, "/api/router/apn/profiles") => router::router_apn_profiles_get(state),
        (&Method::Post, "/api/router/apn/profiles") => router::router_apn_profiles_add(state, body),
        (&Method::Put, "/api/router/apn/profiles") => router::router_apn_profiles_modify(state, body),
        (&Method::Get, "/api/router/apn/auto-profiles") => router::router_apn_auto_profiles(state),
        (&Method::Post, "/api/router/apn/profiles/delete") => router::router_apn_profiles_delete(state, body),
        (&Method::Post, "/api/router/apn/profiles/activate") => router::router_apn_profiles_activate(state, body),
        // USB
        (&Method::Get, "/api/usb/status") => usb::usb_status(state),
        (&Method::Put, "/api/usb/mode") => usb::usb_mode_set(state, body),
        (&Method::Put, "/api/usb/powerbank") => usb::usb_powerbank_set(state, body),
        // Telephony — calls
        (&Method::Post, "/api/call/dial") => telephony::call_dial(state, body),
        (&Method::Post, "/api/call/hangup") => telephony::call_hangup(state),
        (&Method::Post, "/api/call/answer") => telephony::call_answer(state),
        (&Method::Get, "/api/call/status") => telephony::call_status(state),
        (&Method::Post, "/api/call/dtmf") => telephony::call_dtmf(state, body),
        (&Method::Post, "/api/call/mute") => telephony::call_mute(state, body),
        // Telephony — USSD
        (&Method::Post, "/api/ussd/send") => telephony::ussd_send(state, body),
        (&Method::Post, "/api/ussd/respond") => telephony::ussd_respond(state, body),
        (&Method::Post, "/api/ussd/cancel") => telephony::ussd_cancel(state),
        // Telephony — STK
        (&Method::Get, "/api/stk/menu") => telephony::stk_menu(state),
        (&Method::Post, "/api/stk/select") => telephony::stk_select(state, body),
        // DoH
        (&Method::Get, "/api/doh/status") => doh_status(state),
        (&Method::Put, "/api/doh/config") => doh_config_set(state, body),
        (&Method::Post, "/api/doh/enable") => doh_enable(state),
        (&Method::Post, "/api/doh/disable") => doh_disable(state),
        (&Method::Get, "/api/doh/cache") => doh_cache_list(state),
        (&Method::Post, "/api/doh/cache/clear") => doh_cache_clear(state),
        // Tailscale remote management
        (&Method::Get, "/api/tailscale/status") => tailscale::status(state),
        (&Method::Post, "/api/tailscale/setup") => tailscale::setup(state, body),
        (&Method::Post, "/api/tailscale/login-url") => tailscale::login_url(state),
        (&Method::Post, "/api/tailscale/enable") => tailscale::enable(state),
        (&Method::Post, "/api/tailscale/disable") => tailscale::disable(state),
        (&Method::Put, "/api/tailscale/config") => tailscale::config_set(state, body),
        (&Method::Post, "/api/tailscale/logout") => tailscale::logout(state),
        (&Method::Post, "/api/tailscale/update") => tailscale::update(state, body),
        // LAN test (download/upload handled above before body read)
        (&Method::Get, "/api/lan/ping") => lan_test::ping(),
        // Speed test
        (&Method::Get, "/api/speedtest/servers") => speedtest::servers(state),
        (&Method::Post, "/api/speedtest/start") => speedtest::start(state, body),
        (&Method::Get, "/api/speedtest/progress") => speedtest::progress(state),
        (&Method::Post, "/api/speedtest/stop") => speedtest::stop(state, body),
        // Scheduler
        (&Method::Get, "/api/scheduler/jobs") => scheduler::jobs_list(state),
        (&Method::Post, "/api/scheduler/jobs") => scheduler::jobs_create(state, body),
        (&Method::Put, "/api/scheduler/jobs") => scheduler::jobs_update(state, body),
        (&Method::Delete, "/api/scheduler/jobs") => scheduler::jobs_delete(state, body),
        (&Method::Put, "/api/scheduler/jobs/toggle") => scheduler::jobs_toggle(state, body),
        // AT terminal
        (&Method::Post, "/api/at/send") => at_terminal::at_send(state, body),
        (&Method::Get, "/api/at/port") => at_terminal::at_port(state),
        // Fallback
        _ => (404, json!({"ok": false, "error": "not found"})),
    }
}

// --- DoH handlers ---

fn doh_status(state: &AppState) -> (u16, Value) {
    (200, json!({"ok": true, "data": state.doh.status()}))
}

fn doh_config_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    match state.doh.update_config(body) {
        Ok(()) => (200, json!({"ok": true})),
        Err(e) => (400, json!({"ok": false, "error": e})),
    }
}

fn doh_enable(state: &AppState) -> (u16, Value) {
    if let Err(e) = state.doh.start() {
        return (500, json!({"ok": false, "error": e}));
    }
    // Write DoH forwarding config to dnsmasq.d drop-in
    // (UCI `set` creates a plain option, but dnsmasq init only reads `server` as a list — drop-in is reliable)
    let _ = std::fs::write("/tmp/dnsmasq.d/doh.conf", "server=127.0.0.1#5353\nno-resolv\n");
    let _ = std::process::Command::new("sh")
        .args(["-c", "/etc/init.d/dnsmasq restart"])
        .output();
    // Save config
    state.doh.set_enabled(true);
    (200, json!({"ok": true, "data": {"status": "enabled"}}))
}

fn doh_disable(state: &AppState) -> (u16, Value) {
    state.doh.stop();
    dnsmasq_restore_defaults();
    state.doh.set_enabled(false);
    (200, json!({"ok": true, "data": {"status": "disabled"}}))
}

/// Restore dnsmasq to default DNS resolution (remove DoH forwarding).
/// Safe to call even if dnsmasq isn't forwarding to DoH.
pub fn dnsmasq_restore_defaults() {
    let _ = std::process::Command::new("sh")
        .args(["-c", "rm -f /tmp/dnsmasq.d/doh.conf; uci delete dhcp.lan_dns.server 2>/dev/null; uci delete dhcp.lan_dns.noresolv 2>/dev/null; uci commit dhcp; /etc/init.d/dnsmasq restart"])
        .output();
}

fn doh_cache_list(state: &AppState) -> (u16, Value) {
    (200, json!({"ok": true, "data": state.doh.cache_entries()}))
}

fn doh_cache_clear(state: &AppState) -> (u16, Value) {
    state.doh.clear_cache();
    (200, json!({"ok": true}))
}

fn respond(request: Request, status: u16, body: Value) {
    let body_str = serde_json::to_string(&body).unwrap_or_default();
    let content_type = Header::from_bytes("Content-Type", "application/json").unwrap();
    let response = Response::from_string(body_str)
        .with_status_code(status)
        .with_header(content_type);
    let _ = request.respond(response);
}
