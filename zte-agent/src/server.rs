use std::sync::Arc;

use serde_json::{json, Value};

use crate::at_terminal;
use crate::auth::{AuthState, Kind, PeerKey, SessionId};
use crate::cell;
use crate::conn_watchdog;
use crate::data_usage;
use crate::device_ext;
use crate::esim;
use crate::httpd::gate::Lane;
use crate::httpd::guard::{self, HostPolicy, ReqHeaders};
use crate::lan_test;
use crate::scheduler;
use crate::handlers::{self, AppState};
use crate::modem_ext;
use crate::network_ext;
use crate::oos_policy;
use crate::router;
use crate::sim;
use crate::sms;
use crate::sms_forward;
use crate::speedtest;
use crate::tailscale;
use crate::telephony;
use crate::usb;
use crate::wifi;

/// HTTP method as `route()` sees it, independent of the listener's types.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
    /// HEAD, OPTIONS, PATCH, ...: no route matches.
    Other,
}

impl From<&axum::http::Method> for Method {
    fn from(m: &axum::http::Method) -> Self {
        match m.as_str() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            _ => Method::Other,
        }
    }
}

/// Request body caps (413 JSON above them). No app request comes close.
pub const API_BODY_MAX: usize = 64 * 1024;
pub const LIVE_ACTIVITY_BODY_MAX: usize = 4096;
pub const LOGIN_BODY_MAX: usize = 1024;
const LAN_DOWNLOAD_DEFAULT: usize = 50 * 1024 * 1024;
const LAN_DOWNLOAD_MAX: usize = 200 * 1024 * 1024;

/// What `precheck` needs to know about a request; the body is not read yet.
pub struct ReqInfo<'a> {
    pub method: Method,
    /// Without the query string.
    pub path: &'a str,
    pub query: Option<&'a str>,
    /// The value after `Bearer ` (case-sensitive) of the first Authorization header.
    pub bearer: Option<&'a str>,
    pub headers: ReqHeaders<'a>,
    /// The peer is this device: Tailscale clients (userspace networking).
    pub loopback: bool,
    /// Over the LAN HTTPS listener (`httpd::tls`).
    pub secure: bool,
}

pub struct PreEnv<'a> {
    pub auth: &'a AuthState,
    pub hosts: &'a HostPolicy,
    pub ts_names: &'a dyn Fn() -> Vec<String>,
}

/// The listener's next step for a request.
#[derive(Debug, PartialEq)]
pub enum Pre {
    /// Final answer; the body is left unread.
    Reply(u16, Value),
    /// `POST /api/auth/login`: read at most `LOGIN_BODY_MAX`, needs the peer address.
    Login,
    LanDownload { size: usize },
    LanUpload,
    /// `GET /api/events`: a stream for this bearer token.
    Events { owner: SessionId },
    Dashboard,
    /// Read at most `body_limit` bytes, then `route()`.
    Dispatch { body_limit: usize },
}

/// Everything decided before the body is read: auth, Host and Origin checks,
/// the routes that bypass `route()`, and the body limit. Pure apart from the
/// auth lookups, so every listener shares the exact same behaviour.
pub fn precheck(req: &ReqInfo, env: &PreEnv) -> Pre {
    let has_password = env.auth.has_password();
    let mut principal = None;
    if req.path != "/api/auth/login" && has_password {
        principal = req.bearer.and_then(|t| env.auth.authenticate(Kind::Bearer, t));
        if principal.is_none() {
            return refuse(401, "unauthorized");
        }
    }
    // Refuse names a rebinding attack would use (any IP literal and the
    // device's own names pass): without a password for every request (the
    // API is open), with one for browser requests (`Origin` or
    // `Sec-Fetch-Site`; the apps send neither), so a rebound page cannot
    // try passwords at the login route or use up its rate limit.
    if !has_password || req.headers.origin.is_some() || req.headers.sec_fetch_site.is_some() {
        let Some(host) = req.headers.host else {
            return refuse(400, "missing host");
        };
        if !env.hosts.allows_via(host, req.loopback, req.secure, env.ts_names) {
            return refuse(403, "host not allowed");
        }
    }
    if guard::api_mutation_refused(req.method, &req.headers) {
        return refuse(403, "cross-origin request refused");
    }
    match (req.method, req.path) {
        (Method::Post, "/api/auth/login") => return Pre::Login,
        // Answered here, without a hand-off, so the RTT measurement stays honest.
        (Method::Get, "/api/lan/ping") => {
            let (status, body) = lan_test::ping();
            return Pre::Reply(status, body);
        }
        (Method::Get, "/api/lan/download") => {
            let size = parse_query_usize(req.query, "size").unwrap_or(LAN_DOWNLOAD_DEFAULT);
            return Pre::LanDownload { size: size.min(LAN_DOWNLOAD_MAX) };
        }
        (Method::Post, "/api/lan/upload") => return Pre::LanUpload,
        // Streams belong to a token (their expiry and per-token cap), so
        // they need a password.
        (Method::Get, "/api/dashboard/events") => {
            return if principal.is_some() { Pre::Dashboard } else { refuse(403,"agent password required") };
        }
        (Method::Get, "/api/events") => {
            return match principal {
                Some(p) => Pre::Events { owner: p.id },
                None => refuse(403, "agent password required"),
            };
        }
        _ => {}
    }
    let live_activity = req.path.starts_with("/api/live-activity");
    if live_activity && !has_password {
        return refuse(403, "agent password required");
    }
    Pre::Dispatch {
        body_limit: if live_activity { LIVE_ACTIVITY_BODY_MAX } else { API_BODY_MAX },
    }
}

/// `{"ok":false,"error":…}` built in one place (each `json!` site costs code size).
fn refuse(status: u16, error: &str) -> Pre {
    Pre::Reply(status, error_body(error))
}

pub(crate) fn error_body(error: &str) -> Value {
    json!({"ok": false, "error": error})
}

/// Serve the API on `bind`; never returns.
pub fn start(bind: &str, threads: usize, state: Arc<AppState>) {
    crate::httpd::serve(bind, threads, state);
}

/// A reply serialised for the wire: compact JSON, as before.
pub struct Encoded {
    pub status: u16,
    pub body: Vec<u8>,
    /// Seconds for a `Retry-After` header (429 replies with `retry_after`).
    pub retry_after: Option<u64>,
}

pub fn encode((status, body): (u16, Value)) -> Encoded {
    let retry_after = if status == 429 { body["retry_after"].as_u64() } else { None };
    Encoded {
        status,
        body: serde_json::to_vec(&body).unwrap_or_default(),
        retry_after,
    }
}

/// Which gate lane a `route()` call waits in. Slow: calls that can take tens
/// of seconds or minutes (eSIM card commands, Tailscale setup, AT/STK/USSD
/// waits, modem scans and mode changes, outbound HTTP, dnsmasq restarts), so
/// they can never use up the permits the quick reads need. The eSIM state
/// polls, the cached Tailscale status, the STK menu/session snapshots (the
/// STK worker's memory) and call control (one short AT command each, and a
/// hang-up must not wait behind a Tailscale setup) stay General.
pub fn lane(method: Method, path: &str) -> Lane {
    use Method::{Delete, Get, Post};
    let slow = match (method, path) {
        (Get, "/api/network/client-details") => true,
        (Get, "/api/esim/switch")
        | (Get | Delete, "/api/esim/download")
        | (Post, "/api/esim/download/confirm" | "/api/esim/download/cancel")
        | (Get, "/api/tailscale/status")
        | (Get, "/api/stk/menu" | "/api/stk/session") => false,
        (_, p) if p.starts_with("/api/esim/") || p.starts_with("/api/tailscale/") || p.starts_with("/api/https/") => true,
        (_, p) if p.starts_with("/api/ussd/") || p.starts_with("/api/stk/") => true,
        (Post, p) if p.starts_with("/api/sim/") || p.starts_with("/api/cell/") => true,
        (
            Post,
            "/api/at/send"
            | "/api/modem/online"
            | "/api/modem/airplane"
            | "/api/modem/scan"
            | "/api/modem/register"
            | "/api/sms/forward/test"
            | "/api/sms/forward/retry"
            | "/api/speedtest/start"
            | "/api/doh/enable"
            | "/api/doh/disable",
        ) => true,
        (Get, "/api/speedtest/servers") => true,
        _ => false,
    };
    if slow {
        Lane::Slow
    } else {
        Lane::General
    }
}

/// Same loop as before; `size` without `=` or with a bad number is `None`.
pub fn parse_query_usize(query: Option<&str>, key: &str) -> Option<usize> {
    for pair in query?.split('&') {
        let mut kv = pair.splitn(2, '=');
        if kv.next()? == key {
            return kv.next()?.parse().ok();
        }
    }
    None
}

fn route_inner(method: &Method, path: &str, state: &AppState, body: &[u8]) -> (u16, Value) {
    crate::httpd::gate::debug_assert_may_block();
    if *method != Method::Get {
        crate::httpd::gate::debug_assert_may_change(path);
    }
    match (method, path) {
        (&Method::Get, "/api/live-activity") => state.live_activity.status(),
        (&Method::Put, "/api/live-activity") => state.live_activity.register(body),
        (&Method::Delete, "/api/live-activity") => state.live_activity.unregister(),
        // Auth
        // Reached only in-process (the listener takes the Login path), so no peer address.
        (&Method::Post, "/api/auth/login") => handlers::login(state, body, PeerKey::UNKNOWN),
        // Device info (sysfs)
        (&Method::Get, "/api/device") => handlers::device(state),
        (&Method::Get, "/api/battery") => handlers::battery(state),
        (&Method::Get, "/api/cpu") => handlers::cpu(state),
        (&Method::Get, "/api/memory") => handlers::memory(state),
        // Network
        (&Method::Get, "/api/network/client-details") => { let reading=state.client_details.snapshot(); (if reading["ok"]==true {200} else {503},reading) },
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
        (&Method::Get, "/api/device/auto-sleep") => device_ext::device_auto_sleep_get(state),
        (&Method::Put, "/api/device/auto-sleep") => device_ext::device_auto_sleep_set(state, body),
        // System — process monitor
        (&Method::Get, "/api/system/top") => handlers::system_top(state),
        (&Method::Post, "/api/system/kill-bloat") => handlers::system_kill_bloat(state, body),
        // WiFi
        (&Method::Get, "/api/wifi/status") => wifi::wifi_status(state),
        (&Method::Put, "/api/wifi/settings") => wifi::wifi_set(state, body),
        (&Method::Get, "/api/wifi/guest") => wifi::guest_status(state),
        (&Method::Put, "/api/wifi/guest") => wifi::guest_set(state, body),
        // Modem
        (&Method::Get, "/api/data-usage") => data_usage::get(state),
        (&Method::Put, "/api/data-usage/limit") => data_usage::set_limit(state, body),
        (&Method::Put, "/api/data-usage/cycle") => data_usage::set_cycle(state, body),
        (&Method::Get, "/api/modem/status") => handlers::modem_status(state),
        (&Method::Post, "/api/modem/online") => modem_ext::modem_online_user(state),
        (&Method::Get, "/api/modem/data") => modem_ext::modem_data_get(state),
        (&Method::Put, "/api/modem/data") => modem_ext::modem_data_set(state, body),
        (&Method::Post, "/api/modem/airplane") => modem_ext::modem_airplane(state, body),
        (&Method::Put, "/api/modem/network-mode") => modem_ext::modem_network_mode_set(state, body),
        (&Method::Post, "/api/modem/scan") => modem_ext::modem_scan(state),
        (&Method::Get, "/api/modem/scan/status") => modem_ext::modem_scan_status(state),
        (&Method::Get, "/api/modem/scan/results") => modem_ext::modem_scan_results(state),
        (&Method::Post, "/api/modem/register") => modem_ext::modem_register(state, body),
        (&Method::Get, "/api/modem/register/result") => modem_ext::modem_register_result(state),
        (&Method::Get, "/api/modem/oos-policy") => oos_policy::config_get(&state.oos_policy),
        (&Method::Put, "/api/modem/oos-policy") => oos_policy::config_set(&state.oos_policy, body),
        (&Method::Get, "/api/modem/conn-watchdog") => {
            conn_watchdog::config_get(&state.conn_watchdog)
        }
        (&Method::Put, "/api/modem/conn-watchdog") => {
            conn_watchdog::config_set(&state.conn_watchdog, body)
        }
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
        (&Method::Get, "/api/stk/session") => state.stk.request("status", &[]),
        (&Method::Post, "/api/stk/respond") => state.stk.request("respond", body),
        (&Method::Post, "/api/stk/cancel") => state.stk.request("cancel", body),
        (&Method::Post, "/api/stk/back") => state.stk.request("back", body),
        // eSIM profiles
        (&Method::Get, "/api/esim/profiles") => esim::profiles(state),
        (&Method::Get, "/api/esim/chip") => esim::chip(state),
        (&Method::Post, "/api/esim/enable") => esim::enable(state, body),
        (&Method::Get, "/api/esim/switch") => esim::switch_status(state),
        (&Method::Post, "/api/esim/nickname") => esim::nickname(state, body),
        (&Method::Get, "/api/esim/notifications") => esim::notifications(state),
        (&Method::Post, "/api/esim/notifications/process") => esim::notifications_process(state),
        (&Method::Post, "/api/esim/delete") => esim::delete(state, body),
        (&Method::Post, "/api/esim/download") => esim::download_start(state, body),
        (&Method::Get, "/api/esim/download") => esim::download_status(state),
        (&Method::Delete, "/api/esim/download") => esim::download_dismiss_for(state, body),
        (&Method::Post, "/api/esim/download/confirm") => esim::download_confirm(state, body),
        (&Method::Post, "/api/esim/download/cancel") => esim::download_cancel_for(state, body),
        // DoH
        (&Method::Get, "/api/doh/status") => doh_status(state),
        (&Method::Put, "/api/doh/config") => doh_config_set(state, body),
        (&Method::Post, "/api/doh/enable") => doh_enable(state),
        (&Method::Post, "/api/doh/disable") => doh_disable(state),
        (&Method::Get, "/api/doh/cache") => doh_cache_list(state),
        (&Method::Post, "/api/doh/cache/clear") => doh_cache_clear(state),
        // Tailscale remote management
        (&Method::Get, "/api/tailscale/status") => tailscale::status(state),
        // LAN HTTPS (docs/web-ui.md, "LAN HTTPS"): read-only for the apps.
        (&Method::Get, "/api/https") => state.tls.status_json(),
        (&Method::Post, path @ ("/api/https/on" | "/api/https/renew" | "/api/https/reset")) => https_change(state, path, body),
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


pub fn route(method: &Method, path: &str, state: &AppState, body: &[u8]) -> (u16, Value) {
    let reply = route_inner(method,path,state,body);
    if reply.0 < 300 && matches!(method,Method::Put|Method::Post|Method::Delete) && path != "/api/auth/login" {
        state.dashboard.request_refresh();
    }
    reply
}

/// HTTPS changes use the existing authenticated API, never legacy browser cookies.
fn https_change(state: &AppState, path: &str, body: &[u8]) -> (u16, Value) {
    if !state.auth.has_password() {
        return (403, json!({"ok":false,"error":"agent password required"}));
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return (400, json!({"ok":false,"error":"invalid JSON"}));
    };
    if value["confirm"] != true {
        return (
            400,
            json!({"ok":false,"error":"explicit confirmation required"}),
        );
    }
    let result = match path {
        "/api/https/on" => state.tls.turn_on(),
        "/api/https/renew" => state.tls.renew(),
        "/api/https/reset" => state.tls.reset(),
        _ => return (404, json!({"ok":false,"error":"not found"})),
    };
    match result {
        Ok(()) => state.tls.status_json(),
        Err(error) => (503, json!({"ok":false,"error":error})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Kind;
    use std::net::SocketAddr;

    const PW: &str = "test-password";

    fn peer(n: u8) -> PeerKey {
        PeerKey::from(SocketAddr::from(([10, 0, 200, n], 5000)))
    }

    fn with_password() -> AuthState {
        let auth = AuthState::new();
        auth.set_password(PW);
        auth
    }

    fn ts_names() -> Vec<String> {
        vec!["u60-pro".into()]
    }

    /// A request from a Tailscale client (loopback); `lan` for one from the LAN.
    fn info<'a>(method: Method, path: &'a str, query: Option<&'a str>, bearer: Option<&'a str>, headers: ReqHeaders<'a>) -> ReqInfo<'a> {
        ReqInfo { method, path, query, bearer, headers, loopback: true, secure: false }
    }

    fn lan(req: ReqInfo<'_>) -> ReqInfo<'_> {
        ReqInfo { loopback: false, ..req }
    }

    fn host(h: &str) -> ReqHeaders<'_> {
        ReqHeaders { host: Some(h), ..ReqHeaders::default() }
    }

    fn run(auth: &AuthState, req: &ReqInfo) -> Pre {
        let hosts = HostPolicy::new(Vec::new());
        precheck(req, &PreEnv { auth, hosts: &hosts, ts_names: &ts_names })
    }

    fn reply(status: u16, error: &str) -> Pre {
        Pre::Reply(status, json!({"ok": false, "error": error}))
    }

    const DISPATCH: Pre = Pre::Dispatch { body_limit: API_BODY_MAX };

    #[test]
    fn precheck_with_password() {
        let auth = with_password();
        let token = auth.login(PW, Kind::Bearer, peer(1), &[]).unwrap();
        let t = Some(token.token.expose());
        let h = host("192.168.0.1:9090");
        // The login route is exempt from the token check.
        assert_eq!(run(&auth, &info(Method::Post, "/api/auth/login", None, None, h)), Pre::Login);
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, None, h)), reply(401, "unauthorized"));
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, Some("nope"), h)), reply(401, "unauthorized"));
        assert_eq!(run(&auth, &info(Method::Get, "/api/lan/ping", None, None, h)), reply(401, "unauthorized"));
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, t, h)), DISPATCH);
        // With a password the apps' Host is not checked (they may use any name; browsers: below).
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, t, host("evil.com"))), DISPATCH);
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, t, ReqHeaders::default())), DISPATCH);
        // A valid token does not make a cross-origin mutation acceptable.
        let cross = ReqHeaders { origin: Some("http://evil.com"), ..h };
        assert_eq!(
            run(&auth, &info(Method::Post, "/api/device/reboot", None, t, cross)),
            reply(403, "cross-origin request refused")
        );
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, t, cross)), DISPATCH);
        let site = ReqHeaders { sec_fetch_site: Some("cross-site"), ..h };
        assert_eq!(
            run(&auth, &info(Method::Put, "/api/wifi/settings", None, t, site)),
            reply(403, "cross-origin request refused")
        );
        assert_eq!(
            run(&auth, &info(Method::Get, "/api/live-activity", None, t, h)),
            Pre::Dispatch { body_limit: LIVE_ACTIVITY_BODY_MAX }
        );
        assert_eq!(
            run(&auth, &info(Method::Put, "/api/live-activity/x", None, t, h)),
            Pre::Dispatch { body_limit: LIVE_ACTIVITY_BODY_MAX }
        );
        assert_eq!(run(&auth, &info(Method::Get, "/api/lan/ping", None, t, h)), Pre::Reply(200, json!({"ok": true})));
        assert_eq!(run(&auth, &info(Method::Post, "/api/lan/upload", None, t, h)), Pre::LanUpload);
        assert_eq!(run(&auth, &info(Method::Other, "/", None, t, h)), DISPATCH);
        // Event streams carry the token's session.
        assert_eq!(
            run(&auth, &info(Method::Get, "/api/events", Some("topics=sms"), t, h)),
            Pre::Events { owner: token.id }
        );
        assert_eq!(run(&auth, &info(Method::Get, "/api/events", None, None, h)), reply(401, "unauthorized"));
        assert_eq!(run(&auth, &info(Method::Post, "/api/events", None, t, h)), DISPATCH);
    }

    #[test]
    fn precheck_with_password_host_checks_browser_requests() {
        let auth = with_password();
        let rebound = ReqHeaders {
            host: Some("evil.example:9090"),
            origin: Some("http://evil.example:9090"),
            ..ReqHeaders::default()
        };
        // A rebound page posting passwords: refused before the limiter.
        let login = lan(info(Method::Post, "/api/auth/login", None, None, rebound));
        assert_eq!(run(&auth, &login), reply(403, "host not allowed"));
        let fetch_site = ReqHeaders { origin: None, sec_fetch_site: Some("same-origin"), ..rebound };
        assert_eq!(run(&auth, &lan(info(Method::Post, "/api/auth/login", None, None, fetch_site))), reply(403, "host not allowed"));
        let no_host = ReqHeaders { host: None, ..rebound };
        assert_eq!(run(&auth, &lan(info(Method::Post, "/api/auth/login", None, None, no_host))), reply(400, "missing host"));
        for _ in 0..40 {
            assert_eq!(run(&auth, &login), reply(403, "host not allowed"));
        }
        assert!(auth.login(PW, Kind::Bearer, peer(9), &[]).is_ok(), "the limiter was touched");
        // The apps send no Origin: any name, as before.
        let app = ReqHeaders { origin: None, ..rebound };
        assert_eq!(run(&auth, &lan(info(Method::Post, "/api/auth/login", None, None, app))), Pre::Login);
        // A browser on an allowed name still gets the same-origin rules.
        let same = ReqHeaders { host: Some("192.168.0.1:9090"), origin: Some("http://192.168.0.1:9090"), ..rebound };
        assert_eq!(run(&auth, &lan(info(Method::Post, "/api/auth/login", None, None, same))), Pre::Login);
        // Tailscale names count only for Tailscale (loopback) peers.
        let ts = ReqHeaders { host: Some("u60-pro:9090"), origin: Some("http://u60-pro:9090"), ..rebound };
        assert_eq!(run(&auth, &info(Method::Post, "/api/auth/login", None, None, ts)), Pre::Login);
        assert_eq!(run(&auth, &lan(info(Method::Post, "/api/auth/login", None, None, ts))), reply(403, "host not allowed"));
    }

    #[test]
    fn precheck_without_password() {
        let auth = AuthState::new();
        let h = host("192.168.0.1:9090");
        // Open, but Host-checked.
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, None, h)), DISPATCH);
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, None, host("u60-pro:9090"))), DISPATCH);
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, None, host("localhost"))), DISPATCH);
        // The device's own names only from loopback (Tailscale) peers.
        assert_eq!(run(&auth, &lan(info(Method::Get, "/api/device", None, None, host("u60-pro:9090")))), reply(403, "host not allowed"));
        assert_eq!(run(&auth, &lan(info(Method::Get, "/api/device", None, None, host("localhost")))), reply(403, "host not allowed"));
        assert_eq!(run(&auth, &lan(info(Method::Get, "/api/device", None, None, h))), DISPATCH);
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, None, host("evil.com:9090"))), reply(403, "host not allowed"));
        assert_eq!(run(&auth, &info(Method::Get, "/api/device", None, None, ReqHeaders::default())), reply(400, "missing host"));
        assert_eq!(run(&auth, &info(Method::Post, "/api/auth/login", None, None, host("evil.com"))), reply(403, "host not allowed"));
        // Cross-origin mutations are refused; same-origin ones pass.
        let cross = ReqHeaders { origin: Some("http://evil.com"), ..h };
        assert_eq!(run(&auth, &info(Method::Post, "/api/device/reboot", None, None, cross)), reply(403, "cross-origin request refused"));
        let same = ReqHeaders { origin: Some("http://192.168.0.1:9090"), ..h };
        assert_eq!(run(&auth, &info(Method::Post, "/api/device/reboot", None, None, same)), DISPATCH);
        assert_eq!(run(&auth, &info(Method::Post, "/api/auth/login", None, None, h)), Pre::Login);
        assert_eq!(
            run(&auth, &info(Method::Get, "/api/live-activity", None, None, h)),
            reply(403, "agent password required")
        );
        assert_eq!(
            run(&auth, &info(Method::Delete, "/api/live-activity", None, None, h)),
            reply(403, "agent password required")
        );
        assert_eq!(run(&auth, &info(Method::Get, "/api/lan/ping", Some("x=1"), None, h)), Pre::Reply(200, json!({"ok": true})));
        // Wrong method: not special, falls through to route()'s 404.
        assert_eq!(run(&auth, &info(Method::Post, "/api/lan/ping", None, None, h)), DISPATCH);
        assert_eq!(
            run(&auth, &info(Method::Get, "/api/events", Some("topics=sms"), None, h)),
            reply(403, "agent password required")
        );
        assert_eq!(run(&auth, &info(Method::Get, "/api/events", None, None, host("evil.com"))), reply(403, "host not allowed"));
    }

    #[test]
    fn precheck_lan_download_size() {
        let auth = AuthState::new();
        let h = host("192.168.0.1");
        let size = |q| match run(&auth, &info(Method::Get, "/api/lan/download", q, None, h)) {
            Pre::LanDownload { size } => size,
            other => panic!("{other:?}"),
        };
        assert_eq!(size(None), 50 * 1024 * 1024);
        assert_eq!(size(Some("size=1024")), 1024);
        assert_eq!(size(Some("x=1&size=2048")), 2048);
        assert_eq!(size(Some("size=abc")), 50 * 1024 * 1024);
        assert_eq!(size(Some("size=-1")), 50 * 1024 * 1024);
        assert_eq!(size(Some("size")), 50 * 1024 * 1024);
        assert_eq!(size(Some("size=")), 50 * 1024 * 1024);
        assert_eq!(size(Some("size=999999999999")), 200 * 1024 * 1024);
        assert_eq!(size(Some("")), 50 * 1024 * 1024);
    }

    #[test]
    fn method_from_http() {
        use axum::http::Method as M;
        assert_eq!(Method::from(&M::GET), Method::Get);
        assert_eq!(Method::from(&M::POST), Method::Post);
        assert_eq!(Method::from(&M::PUT), Method::Put);
        assert_eq!(Method::from(&M::DELETE), Method::Delete);
        assert_eq!(Method::from(&M::HEAD), Method::Other);
        assert_eq!(Method::from(&M::PATCH), Method::Other);
        assert_eq!(Method::from(&M::from_bytes(b"get").unwrap()), Method::Other);
    }
}
