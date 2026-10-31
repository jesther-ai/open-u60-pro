use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::doh::{self, DohConfig, EncryptedTransport};
use crate::event_bus::EventBus;
use crate::handlers::AppState;
use crate::ubus;

const FORWARDING_PATH: &str = "/tmp/dnsmasq.d/doh.conf";
const PROBE_NAME: &str = "example.com";
const PROXY_SERVER: &str = "127.0.0.1#5353";
const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const ROUTE_READY_LIMIT: Duration = Duration::from_secs(30);
const ROUTE_READY_POLL: Duration = Duration::from_millis(500);
const WAN_EVENT: &str = "router_event_wan_connect_status";
const NETWORK_INTERFACE_EVENT: &str = "network.interface";
const SERVICE_EVENT: &str = "zwrt_servicestatus";

static TRANSACTION: Mutex<()> = Mutex::new(());

/// Register one fast event-bus callback before `EventBus::start`. The callback
/// only copies events into a channel; all cache, proxy and dnsmasq work happens
/// on the dedicated worker below, never on the ubus reader thread.
pub fn subscribe_network_changes(bus: &EventBus) -> mpsc::Receiver<(String, Value)> {
    let (tx, rx) = mpsc::channel();
    bus.subscribe_fn(
        &[
            WAN_EVENT,
            NETWORK_INTERFACE_EVENT,
            SERVICE_EVENT,
            crate::sim_identity::EVENT,
        ],
        Arc::new(move |name, payload| {
            drop(tx.send((name.to_string(), payload.clone())));
        }),
    );
    rx
}

/// Restore the saved bootstrap resolver before the encrypted proxy starts.
/// This closes the boot-time window where a firmware-written carrier resolver
/// could otherwise be used to resolve the DoH/DoT hostname.
pub fn prepare_startup(state: &Arc<AppState>) -> Result<(), String> {
    let _guard = TRANSACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    preserve_current_bootstrap(state)?;
    if state.doh.config().enabled {
        restore_bootstrap_dns(state)?;
    }
    Ok(())
}

/// Keep DNS bound to the current mobile bearer. A transition in WAN state,
/// radio service or SIM identity clears both router cache layers and drops old
/// encrypted sessions. Reconnect also repairs the local forwarding file and
/// listener without ever falling back to carrier DNS.
pub fn start_network_watch(state: Arc<AppState>, events: mpsc::Receiver<(String, Value)>) {
    crate::system::spawn_named("dns-network", move || {
        if let Err(error) = migrate_bootstrap_dns(&state) {
            eprintln!("[dns] could not preserve the current bootstrap DNS: {error}");
        }
        let mut previous = None;
        let mut previous_service = None;
        for (name, payload) in events {
            let Some(action) = network_action(&name, &payload) else {
                continue;
            };
            let reconnect_clients = reconnect_clients_after_sim_change(&name, &payload);
            let reason = network_reason(&name, &payload);
            let result = match action {
                NetworkAction::Bearer { connected, force } => {
                    if !force && previous == Some(connected) {
                        continue;
                    }
                    reconcile_network(&state, connected).map(|()| previous = Some(connected))
                }
                NetworkAction::Service {
                    connected,
                    network_type,
                } => {
                    if previous_service.as_deref() == Some(network_type.as_str()) {
                        continue;
                    }
                    reconcile_network(&state, connected).map(|()| {
                        previous = Some(connected);
                        previous_service = Some(network_type);
                    })
                }
                NetworkAction::LocalRefresh => reconcile_local_path(&state),
            };
            if let Err(error) = result {
                // Keep the transition retryable: firmware often emits a short
                // burst of identical state events while routes and services
                // settle after a profile change.
                previous = None;
                eprintln!("[dns] {reason}: network synchronization failed: {error}");
            } else {
                if state.doh.config().enabled {
                    eprintln!("[dns] {reason}: encrypted DNS caches and sessions synchronized");
                } else {
                    eprintln!("[dns] {reason}: local DNS cache synchronized");
                }
                if reconnect_clients {
                    schedule_client_dns_refresh();
                }
            }
        }
    });
}

fn reconnect_clients_after_sim_change(name: &str, payload: &Value) -> bool {
    name == crate::sim_identity::EVENT
        && payload["phase"] == "online"
        && payload["identity_changed"] == true
}

fn schedule_client_dns_refresh() {
    if let Err(error) = crate::system::try_spawn_named("wifi-dns-refresh", || {
        match crate::wifi::reconnect_clients_for_dns_refresh() {
            Ok(result) => eprintln!(
                "[dns] eSIM changed: asked {} Wi-Fi client(s) to reconnect; {} failed",
                result.disconnected, result.failed
            ),
            Err(error) => eprintln!("[dns] eSIM changed: Wi-Fi client refresh failed: {error}"),
        }
    }) {
        eprintln!("[dns] eSIM changed: could not start Wi-Fi client refresh: {error}");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NetworkAction {
    Bearer {
        connected: bool,
        force: bool,
    },
    Service {
        connected: bool,
        network_type: String,
    },
    LocalRefresh,
}

fn network_action(name: &str, payload: &Value) -> Option<NetworkAction> {
    match name {
        WAN_EVENT => {
            crate::conn_watchdog::wan_from_event(payload).map(|connected| NetworkAction::Bearer {
                connected,
                force: false,
            })
        }
        NETWORK_INTERFACE_EVENT => {
            match (payload["interface"].as_str()?, payload["action"].as_str()?) {
                ("zte_wan" | "zte_wan6", "ifup") => Some(NetworkAction::Bearer {
                    connected: true,
                    force: true,
                }),
                ("zte_wan" | "zte_wan6", "ifdown") => Some(NetworkAction::Bearer {
                    connected: false,
                    force: true,
                }),
                ("lan", "ifup") => Some(NetworkAction::LocalRefresh),
                _ => None,
            }
        }
        SERVICE_EVENT => {
            let network_type = payload["network_type"].as_str()?;
            crate::oos_policy::service_from_event(payload).map(|connected| NetworkAction::Service {
                connected,
                network_type: network_type.to_string(),
            })
        }
        crate::sim_identity::EVENT => match payload["phase"].as_str()? {
            "leaving" | "changed" | "stalled" => Some(NetworkAction::Bearer {
                connected: false,
                force: true,
            }),
            "online" => Some(NetworkAction::Bearer {
                connected: true,
                force: true,
            }),
            _ => None,
        },
        _ => None,
    }
}

fn network_reason(name: &str, payload: &Value) -> String {
    if name == WAN_EVENT {
        format!(
            "WAN {}",
            payload["wan_status"].as_str().unwrap_or("state change")
        )
    } else if name == NETWORK_INTERFACE_EVENT {
        format!(
            "{} {}",
            payload["interface"].as_str().unwrap_or("network interface"),
            payload["action"].as_str().unwrap_or("change")
        )
    } else if name == SERVICE_EVENT {
        format!(
            "mobile service {}",
            payload["network_type"].as_str().unwrap_or("change")
        )
    } else {
        format!(
            "SIM identity {}",
            payload["phase"].as_str().unwrap_or("change")
        )
    }
}

fn reconcile_network(state: &Arc<AppState>, connected: bool) -> Result<(), String> {
    {
        let _guard = TRANSACTION
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Invalidate the old bearer immediately, before waiting for the new
        // default route. LAN clients can no longer receive cached answers or a
        // response that began on the previous SIM/APN.
        invalidate_network_bound_dns(state)?;
    }
    if !connected || !state.doh.config().enabled {
        return Ok(());
    }

    wait_for_mobile_route()?;
    let _guard = TRANSACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !state.doh.config().enabled {
        return Ok(());
    }
    // Queries may have arrived while the route was settling. Start the new
    // bearer with another empty cache and fresh encrypted sessions.
    invalidate_network_bound_dns(state)?;
    restore_bootstrap_dns(state)?;
    if !state.doh.is_running() {
        state.doh.start()?;
    }
    if ensure_forwarding_file()? {
        restart_dnsmasq()?;
        return Ok(());
    }
    Ok(())
}

fn invalidate_network_bound_dns(state: &AppState) -> Result<(), String> {
    state.doh.network_changed();
    clear_dnsmasq_cache().map(|_| ())
}

/// Drop all DNS state tied to the old radio preference after the modem accepts
/// a 3G/4G/5G mode change. Service and WAN events perform the post-attach pass.
pub fn network_preference_changed(state: &AppState) -> Result<(), String> {
    let _guard = TRANSACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    invalidate_network_bound_dns(state)
}

fn wait_for_mobile_route() -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < ROUTE_READY_LIMIT {
        let ipv4 = fs::read_to_string("/proc/net/route")
            .is_ok_and(|table| crate::esim::has_default_route(&table));
        let ipv6 = fs::read_to_string("/proc/net/ipv6_route")
            .is_ok_and(|table| has_ipv6_default_route(&table));
        if ipv4 || ipv6 {
            return Ok(());
        }
        thread::sleep(ROUTE_READY_POLL);
    }
    Err("mobile interface came up without a default route".to_string())
}

fn has_ipv6_default_route(table: &str) -> bool {
    table.lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        fields.len() >= 10
            && fields[0].bytes().all(|byte| byte == b'0')
            && fields[1] == "00"
            && matches!(fields.last(), Some(&"rmnet_data0" | &"rmnet_ipa0"))
    })
}

fn reconcile_local_path(state: &Arc<AppState>) -> Result<(), String> {
    let _guard = TRANSACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !state.doh.config().enabled {
        return Ok(());
    }
    if !state.doh.is_running() {
        state.doh.start()?;
    }
    if ensure_forwarding_file()? {
        restart_dnsmasq()?;
    }
    Ok(())
}

fn migrate_bootstrap_dns(state: &Arc<AppState>) -> Result<(), String> {
    let _guard = TRANSACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    preserve_current_bootstrap(state)
}

fn preserve_current_bootstrap(state: &Arc<AppState>) -> Result<(), String> {
    let config = state.doh.config();
    if !config.enabled || !config.bootstrap_primary.is_empty() {
        return Ok(());
    }
    let router = read_router_dns()?;
    if router.mode == "manual" && !router.primary.is_empty() {
        state
            .doh
            .remember_bootstrap(&router.primary, &router.secondary)?;
    }
    Ok(())
}

/// Firmware can replace `/etc/resolv.conf` while bringing up a new APN or SIM
/// profile. Restore only the saved bootstrap resolvers after the route is up;
/// LAN clients still use the local encrypted proxy through dnsmasq.
fn restore_bootstrap_dns(state: &Arc<AppState>) -> Result<(), String> {
    let mut config = state.doh.config();
    if config.bootstrap_primary.is_empty() {
        let current = read_router_dns()?;
        if current.mode != "manual" || current.primary.is_empty() {
            return Err("encrypted DNS has no saved bootstrap resolver".to_string());
        }
        state
            .doh
            .remember_bootstrap(&current.primary, &current.secondary)?;
        config.bootstrap_primary = current.primary;
        config.bootstrap_secondary = current.secondary;
    }
    let desired = RouterDns {
        mode: "manual".to_string(),
        primary: config.bootstrap_primary,
        secondary: config.bootstrap_secondary,
    };
    let current = read_router_dns()?;
    if current.mode != desired.mode
        || current.primary != desired.primary
        || current.secondary != desired.secondary
    {
        set_router_dns(&desired)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Automatic,
    Manual,
    Doh,
    Dot,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
            Self::Doh => "doh",
            Self::Dot => "dot",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    mode: String,
    #[serde(default)]
    primary: String,
    #[serde(default)]
    secondary: String,
    #[serde(default)]
    doh_url: String,
    #[serde(default)]
    dot_endpoint: String,
    #[serde(default)]
    dot_server_name: String,
}

#[derive(Debug, Clone)]
struct Candidate {
    mode: Mode,
    primary: Option<IpAddr>,
    secondary: Option<IpAddr>,
    encrypted: DohConfig,
}

#[derive(Debug, Clone)]
struct RouterDns {
    mode: String,
    primary: String,
    secondary: String,
}

#[derive(Debug, Clone)]
struct Snapshot {
    router: RouterDns,
    encrypted: DohConfig,
    proxy_running: bool,
}

#[derive(Debug)]
struct Probe {
    latency_ms: u128,
    detail: String,
}

pub fn status(state: &AppState) -> (u16, Value) {
    let router = match read_router_dns() {
        Ok(router) => router,
        Err(error) => return failure(503, error),
    };
    let encrypted = state.doh.config();
    let running = state.doh.is_running();
    let mode = if encrypted.enabled {
        match encrypted.transport {
            EncryptedTransport::Doh => Mode::Doh,
            EncryptedTransport::Dot => Mode::Dot,
        }
    } else if router.mode == "manual" {
        Mode::Manual
    } else {
        Mode::Automatic
    };
    let proxy = state.doh.status();
    (
        200,
        json!({"ok":true,"data":{
            "mode": mode.as_str(),
            "primary": router.primary,
            "secondary": router.secondary,
            "doh_url": encrypted.upstream_url,
            "dot_endpoint": encrypted.dot_endpoint,
            "dot_server_name": encrypted.dot_server_name,
            "proxy_running": running,
            "carrier_servers": carrier_servers(),
            "stats": proxy["stats"],
        }}),
    )
}

pub fn test(state: &AppState, body: &[u8]) -> (u16, Value) {
    let candidate = match parse_candidate(state, body) {
        Ok(candidate) => candidate,
        Err(error) => return failure(400, error),
    };
    match probe_candidate(&candidate) {
        Ok(probe) => (
            200,
            json!({"ok":true,"data":{
                "mode": candidate.mode.as_str(),
                "latency_ms": probe.latency_ms,
                "detail": probe.detail,
            }}),
        ),
        Err(error) => failure(422, format!("DNS test failed: {error}")),
    }
}

pub fn apply(state: &AppState, body: &[u8]) -> (u16, Value) {
    let candidate = match parse_candidate(state, body) {
        Ok(candidate) => candidate,
        Err(error) => return failure(400, error),
    };
    apply_guarded(state, candidate)
}

fn apply_guarded(state: &AppState, candidate: Candidate) -> (u16, Value) {
    let preflight = match probe_candidate(&candidate) {
        Ok(probe) => probe,
        Err(error) => {
            return failure(
                422,
                format!("DNS test failed; nothing was changed: {error}"),
            )
        }
    };
    let _guard = TRANSACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let snapshot = match snapshot(state) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return failure(
                503,
                format!("Could not save the current DNS state: {error}"),
            )
        }
    };
    if let Err(error) = apply_candidate(state, &candidate) {
        return match restore(state, &snapshot) {
            Ok(()) => (
                502,
                json!({"ok":false,"error":format!("{error}. Previous DNS settings were restored."),"data":{"rolled_back":true}}),
            ),
            Err(rollback) => (
                500,
                json!({"ok":false,"error":format!("{error}. Automatic rollback also failed: {rollback}"),"data":{"rolled_back":false}}),
            ),
        };
    }
    let (_, current) = status(state);
    (
        200,
        json!({"ok":true,"data":{
            "mode": candidate.mode.as_str(),
            "preflight_ms": preflight.latency_ms,
            "verified": true,
            "rolled_back": false,
            "current": current["data"],
        }}),
    )
}

/// Keep older native and embedded clients on the same guarded transaction as
/// the current DNS page. A manual update preserves encrypted DNS when it is
/// active; the legacy client's following disable request can then turn it off
/// without ever leaving dnsmasq pointed at a stopped listener.
pub fn legacy_router_apply(state: &AppState, body: &[u8]) -> (u16, Value) {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return failure(400, "invalid JSON".to_string()),
    };
    let old_mode = value["dns_mode"].as_str().unwrap_or_default();
    let config = state.doh.config();
    let mode = match old_mode {
        "auto" => Mode::Automatic,
        "manual" if config.enabled => match config.transport {
            EncryptedTransport::Doh => Mode::Doh,
            EncryptedTransport::Dot => Mode::Dot,
        },
        "manual" => Mode::Manual,
        _ => return failure(400, "dns_mode must be auto or manual".to_string()),
    };
    let request = json!({
        "mode": mode.as_str(),
        "primary": value["prefer_dns_manual"].as_str().unwrap_or_default(),
        "secondary": value["standby_dns_manual"].as_str().unwrap_or_default(),
        "doh_url": config.upstream_url,
        "dot_endpoint": config.dot_endpoint,
        "dot_server_name": config.dot_server_name,
    });
    apply(state, request.to_string().as_bytes())
}

pub fn legacy_doh_config(state: &AppState, body: &[u8]) -> (u16, Value) {
    let patch: crate::doh::config::DohConfigPatch = match serde_json::from_slice(body) {
        Ok(patch) => patch,
        Err(error) => return failure(400, format!("invalid config JSON: {error}")),
    };
    let mut next = state.doh.config();
    next.apply_patch(patch);
    if !next.enabled && !state.doh.is_running() {
        return match state.doh.update_config(body) {
            Ok(()) => (200, json!({"ok":true})),
            Err(error) => failure(400, error),
        };
    }
    match encrypted_candidate(state, next) {
        Ok(candidate) => apply_guarded(state, candidate),
        Err(error) => failure(400, error),
    }
}

pub fn legacy_doh_enable(state: &AppState) -> (u16, Value) {
    match encrypted_candidate(state, state.doh.config()) {
        Ok(candidate) => apply_guarded(state, candidate),
        Err(error) => failure(400, error),
    }
}

pub fn legacy_doh_disable(state: &AppState) -> (u16, Value) {
    let router = match read_router_dns() {
        Ok(router) => router,
        Err(error) => return failure(503, error),
    };
    let request = if router.mode == "manual" && !router.primary.is_empty() {
        json!({
            "mode":"manual",
            "primary":router.primary,
            "secondary":router.secondary,
        })
    } else {
        json!({"mode":"automatic"})
    };
    apply(state, request.to_string().as_bytes())
}

fn encrypted_candidate(state: &AppState, mut config: DohConfig) -> Result<Candidate, String> {
    let router = read_router_dns()?;
    let (primary, secondary) = if router.mode == "manual" && !router.primary.is_empty() {
        (router.primary, router.secondary)
    } else if !config.bootstrap_primary.is_empty() {
        (
            config.bootstrap_primary.clone(),
            config.bootstrap_secondary.clone(),
        )
    } else {
        let servers = carrier_servers();
        (
            servers.first().cloned().unwrap_or_default(),
            servers.get(1).cloned().unwrap_or_default(),
        )
    };
    let mode = match config.transport {
        EncryptedTransport::Doh => Mode::Doh,
        EncryptedTransport::Dot => Mode::Dot,
    };
    let request = json!({
        "mode":mode.as_str(),
        "primary":primary,
        "secondary":secondary,
        "doh_url":config.upstream_url,
        "dot_endpoint":config.dot_endpoint,
        "dot_server_name":config.dot_server_name,
    });
    let mut candidate = parse_candidate(state, request.to_string().as_bytes())?;
    config.enabled = true;
    config.bootstrap_primary = candidate
        .primary
        .map_or_else(String::new, |ip| ip.to_string());
    config.bootstrap_secondary = candidate
        .secondary
        .map_or_else(String::new, |ip| ip.to_string());
    candidate.encrypted = config;
    Ok(candidate)
}

pub fn cache(state: &AppState) -> (u16, Value) {
    (
        200,
        json!({"ok":true,"data":{
            "entries": state.doh.cache_entries(),
            "stats": state.doh.status()["stats"],
        }}),
    )
}

pub fn clear_cache(state: &AppState) -> (u16, Value) {
    state.doh.clear_cache();
    match clear_dnsmasq_cache() {
        Ok(signalled) => (
            200,
            json!({"ok":true,"data":{"cleared":true,"dnsmasq_processes":signalled}}),
        ),
        Err(error) => failure(
            503,
            format!("Agent cache cleared, but firmware cache clear failed: {error}"),
        ),
    }
}

fn parse_candidate(state: &AppState, body: &[u8]) -> Result<Candidate, String> {
    let request: Request =
        serde_json::from_slice(body).map_err(|error| format!("invalid DNS settings: {error}"))?;
    let mode = match request.mode.as_str() {
        "automatic" => Mode::Automatic,
        "manual" => Mode::Manual,
        "doh" => Mode::Doh,
        "dot" => Mode::Dot,
        _ => return Err("mode must be automatic, manual, doh or dot".to_string()),
    };
    let needs_servers = mode != Mode::Automatic;
    let primary = parse_server(&request.primary, "Primary DNS", needs_servers)?;
    let secondary = parse_server(&request.secondary, "Secondary DNS", false)?;
    if primary.is_some() && primary == secondary {
        return Err("Primary and secondary DNS must be different".to_string());
    }
    if let Ok(lan) = ubus::uci_get("network.lan.ipaddr").and_then(|value| {
        value
            .parse::<IpAddr>()
            .map_err(|error| format!("LAN address: {error}"))
    }) {
        if primary == Some(lan) || secondary == Some(lan) {
            return Err("A DNS server cannot be the router's own LAN address".to_string());
        }
    }

    let mut encrypted = state.doh.config();
    encrypted.enabled = matches!(mode, Mode::Doh | Mode::Dot);
    match mode {
        Mode::Doh => {
            validate_doh_url(&request.doh_url)?;
            encrypted.transport = EncryptedTransport::Doh;
            encrypted.upstream_url = request.doh_url.trim().to_string();
        }
        Mode::Dot => {
            let endpoint = normalize_dot_endpoint(&request.dot_endpoint)?;
            validate_server_name(&request.dot_server_name)?;
            encrypted.transport = EncryptedTransport::Dot;
            encrypted.dot_endpoint = endpoint;
            encrypted.dot_server_name = request.dot_server_name.trim().to_string();
        }
        Mode::Automatic | Mode::Manual => {}
    }
    if matches!(mode, Mode::Doh | Mode::Dot) {
        encrypted.bootstrap_primary = primary.map_or_else(String::new, |ip| ip.to_string());
        encrypted.bootstrap_secondary = secondary.map_or_else(String::new, |ip| ip.to_string());
    }
    Ok(Candidate {
        mode,
        primary,
        secondary,
        encrypted,
    })
}

fn parse_server(value: &str, label: &str, required: bool) -> Result<Option<IpAddr>, String> {
    let value = value.trim();
    if value.is_empty() {
        return if required {
            Err(format!("{label} is required"))
        } else {
            Ok(None)
        };
    }
    let address: IpAddr = value
        .parse()
        .map_err(|_| format!("{label} must be an IPv4 address"))?;
    if address.is_ipv6() {
        return Err(format!(
            "{label} must be IPv4 because this U60 firmware rejects IPv6 manual DNS settings"
        ));
    }
    let invalid = match address {
        IpAddr::V4(ip) => {
            ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                || ip == Ipv4Addr::BROADCAST
        }
        IpAddr::V6(ip) => ip.is_unspecified() || ip.is_loopback() || ip.is_multicast(),
    };
    if invalid {
        Err(format!("{label} is not a usable DNS server"))
    } else {
        Ok(Some(address))
    }
}

fn validate_doh_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value.trim()).map_err(|_| "Enter a valid DoH URL".to_string())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("DoH must use an https:// URL without credentials".to_string());
    }
    let host = url.host_str().unwrap_or_default();
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err("DoH cannot use localhost".to_string());
    }
    if host
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_unspecified() || ip.is_loopback() || ip.is_multicast())
    {
        return Err("DoH host is not a usable resolver".to_string());
    }
    Ok(())
}

fn normalize_dot_endpoint(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("DoT server is required".to_string());
    }
    let url = reqwest::Url::parse(&format!("https://{value}/")).map_err(|_| {
        "DoT server must be a host or IP, optionally followed by a port".to_string()
    })?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err("DoT server must contain only a host or IP and an optional port".to_string());
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "DoT server is invalid".to_string())?;
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err("DoT cannot use localhost".to_string());
    }
    if host
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_unspecified() || ip.is_loopback() || ip.is_multicast())
    {
        return Err("DoT host is not a usable resolver".to_string());
    }
    let port = url.port().unwrap_or(853);
    Ok(if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    })
}

fn validate_server_name(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() || value.parse::<IpAddr>().is_ok() {
        return Err("DoT TLS name must be the resolver's certificate hostname".to_string());
    }
    ServerName::try_from(value.to_string())
        .map(|_| ())
        .map_err(|_| "DoT TLS name is invalid".to_string())
}

fn snapshot(state: &AppState) -> Result<Snapshot, String> {
    Ok(Snapshot {
        router: read_router_dns()?,
        encrypted: state.doh.config(),
        proxy_running: state.doh.is_running(),
    })
}

fn read_router_dns() -> Result<RouterDns, String> {
    let data = ubus::call("zwrt_router.api", "router_get_dns_para", Some("{}"))?;
    let get = |key: &str| {
        data.get(key)
            .or_else(|| data.get(format!("wan_{key}")))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mode = get("dns_mode");
    if mode.is_empty() {
        return Err("router did not report its DNS mode".to_string());
    }
    let mut primary = get("prefer_dns_manual");
    let mut secondary = get("standby_dns_manual");
    if mode == "manual" && primary.is_empty() {
        if let Ok(uci) = ubus::uci_get("network.wan.dns") {
            let mut servers = uci.split_whitespace();
            primary = servers.next().unwrap_or_default().to_string();
            secondary = servers.next().unwrap_or_default().to_string();
        }
    }
    Ok(RouterDns {
        mode,
        primary,
        secondary,
    })
}

fn set_router_dns(router: &RouterDns) -> Result<(), String> {
    let payload = json!({
        "dns_mode": router.mode,
        "prefer_dns_manual": router.primary,
        "standby_dns_manual": router.secondary,
    });
    ubus::call(
        "zwrt_router.api",
        "router_set_wan_dns",
        Some(&payload.to_string()),
    )?;
    Ok(())
}

fn router_for(candidate: &Candidate) -> RouterDns {
    RouterDns {
        mode: if candidate.mode == Mode::Automatic {
            "auto".to_string()
        } else {
            "manual".to_string()
        },
        primary: candidate
            .primary
            .map_or_else(String::new, |ip| ip.to_string()),
        secondary: candidate
            .secondary
            .map_or_else(String::new, |ip| ip.to_string()),
    }
}

fn apply_candidate(state: &AppState, candidate: &Candidate) -> Result<(), String> {
    set_router_dns(&router_for(candidate))?;
    if matches!(candidate.mode, Mode::Doh | Mode::Dot) {
        state
            .doh
            .install_config(candidate.encrypted.clone(), true)?;
        set_forwarding(true)?;
    } else {
        let mut config = state.doh.config();
        config.enabled = false;
        state.doh.install_config(config, false)?;
        set_forwarding(false)?;
    }
    verify_effective(state, candidate)
}

fn restore(state: &AppState, snapshot: &Snapshot) -> Result<(), String> {
    set_router_dns(&snapshot.router)?;
    state
        .doh
        .install_config(snapshot.encrypted.clone(), snapshot.proxy_running)?;
    set_forwarding(snapshot.proxy_running)
}

fn set_forwarding(enabled: bool) -> Result<(), String> {
    if enabled {
        ensure_forwarding_file()?;
    } else if let Err(error) = fs::remove_file(FORWARDING_PATH) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(format!("remove encrypted DNS forwarding: {error}"));
        }
    }
    restart_dnsmasq()
}

/// Return whether dnsmasq's encrypted forwarding file had to be created or
/// repaired. A reconnect with an intact file needs only SIGHUP (cache flush),
/// while a repaired file needs a full restart so dnsmasq reads the new route.
fn ensure_forwarding_file() -> Result<bool, String> {
    let expected = format!("server={PROXY_SERVER}\nno-resolv\n");
    if fs::read_to_string(FORWARDING_PATH).is_ok_and(|current| current == expected) {
        return Ok(false);
    }
    fs::write(FORWARDING_PATH, expected)
        .map_err(|error| format!("write encrypted DNS forwarding: {error}"))?;
    Ok(true)
}

fn restart_dnsmasq() -> Result<(), String> {
    let output = Command::new("/etc/init.d/dnsmasq")
        .arg("restart")
        .output()
        .map_err(|error| format!("restart DNS service: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "restart DNS service failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn probe_candidate(candidate: &Candidate) -> Result<Probe, String> {
    let started = Instant::now();
    match candidate.mode {
        Mode::Automatic => {
            let servers = carrier_servers();
            if servers.is_empty() {
                return Err("the carrier supplied no DNS servers".to_string());
            }
            let mut errors = Vec::new();
            for (index, server) in servers.iter().enumerate() {
                match server.parse::<IpAddr>() {
                    Ok(server) => match query_udp(server, 0x6100 + index as u16) {
                        Ok(()) => {
                            return Ok(Probe {
                                latency_ms: started.elapsed().as_millis(),
                                detail: format!("Carrier DNS {server} answered"),
                            })
                        }
                        Err(error) => errors.push(error),
                    },
                    Err(_) => errors.push(format!("invalid carrier DNS {server}")),
                }
            }
            Err(errors.join("; "))
        }
        Mode::Manual => {
            let mut count = 0;
            for (index, server) in [candidate.primary, candidate.secondary]
                .into_iter()
                .flatten()
                .enumerate()
            {
                query_udp(server, 0x6200 + index as u16)?;
                count += 1;
            }
            Ok(Probe {
                latency_ms: started.elapsed().as_millis(),
                detail: format!(
                    "{count} manual DNS server{} answered",
                    if count == 1 { "" } else { "s" }
                ),
            })
        }
        Mode::Doh | Mode::Dot => {
            for (index, server) in [candidate.primary, candidate.secondary]
                .into_iter()
                .flatten()
                .enumerate()
            {
                query_udp(server, 0x6300 + index as u16)?;
            }
            let latency_ms = doh::probe_upstream(&candidate.encrypted)?;
            Ok(Probe {
                latency_ms,
                detail: format!(
                    "{} resolver returned a verified answer",
                    candidate.mode.as_str().to_uppercase()
                ),
            })
        }
    }
}

fn verify_effective(state: &AppState, candidate: &Candidate) -> Result<(), String> {
    if matches!(candidate.mode, Mode::Doh | Mode::Dot) {
        if !state.doh.is_running() {
            return Err("encrypted DNS proxy stopped after apply".to_string());
        }
        let listen: SocketAddr = candidate
            .encrypted
            .listen_addr
            .parse()
            .map_err(|_| "encrypted DNS listen address is invalid".to_string())?;
        query_udp_socket(listen, 0x6400)?;
    }
    let lan: IpAddr = ubus::uci_get("network.lan.ipaddr")?
        .parse()
        .map_err(|error| format!("router LAN address is invalid: {error}"))?;
    let target = SocketAddr::new(lan, 53);
    let mut last = String::new();
    for attempt in 0..4 {
        match query_udp_socket(target, 0x6500 + attempt) {
            Ok(()) => return Ok(()),
            Err(error) => last = error,
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!("router DNS verification failed: {last}"))
}

fn query_udp(server: IpAddr, id: u16) -> Result<(), String> {
    query_udp_socket(SocketAddr::new(server, 53), id)
}

fn query_udp_socket(server: SocketAddr, id: u16) -> Result<(), String> {
    let bind = if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind).map_err(|error| format!("DNS socket: {error}"))?;
    socket
        .set_read_timeout(Some(DNS_TIMEOUT))
        .map_err(|error| format!("DNS timeout: {error}"))?;
    socket
        .connect(server)
        .map_err(|error| format!("DNS server {server}: {error}"))?;
    let query = crate::doh::dns_packet::build_query(id, PROBE_NAME, 1)?;
    socket
        .send(&query)
        .map_err(|error| format!("DNS query to {server}: {error}"))?;
    let mut response = [0u8; 4096];
    let length = socket
        .recv(&mut response)
        .map_err(|error| format!("DNS server {server} did not answer: {error}"))?;
    if crate::doh::dns_packet::successful_response(&response[..length], id) {
        Ok(())
    } else {
        Err(format!("DNS server {server} returned no usable answer"))
    }
}

fn carrier_servers() -> Vec<String> {
    fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some("nameserver"))
                .then(|| words.next().map(str::to_string))
                .flatten()
        })
        .collect()
}

fn clear_dnsmasq_cache() -> Result<usize, String> {
    let mut signalled = 0;
    let entries = fs::read_dir("/proc").map_err(|error| format!("read processes: {error}"))?;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
            .filter(|pid| *pid > 1)
        else {
            continue;
        };
        let comm = fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        if comm.trim() != "dnsmasq" {
            continue;
        }
        // SAFETY: `pid` comes from /proc, is greater than one, and its current
        // command name was checked immediately before sending the harmless HUP.
        if unsafe { libc::kill(pid, libc::SIGHUP) } == 0 {
            signalled += 1;
        }
    }
    if signalled == 0 {
        Err("dnsmasq process not found".to_string())
    } else {
        Ok(signalled)
    }
}

fn failure(status: u16, error: String) -> (u16, Value) {
    (status, json!({"ok":false,"error":error}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState::new()
    }

    #[test]
    fn automatic_needs_no_server() {
        let candidate = parse_candidate(&state(), br#"{"mode":"automatic"}"#).unwrap();
        assert_eq!(candidate.mode, Mode::Automatic);
    }

    #[test]
    fn manual_rejects_loopback_and_duplicate_servers() {
        let local =
            parse_candidate(&state(), br#"{"mode":"manual","primary":"127.0.0.1"}"#).unwrap_err();
        assert!(local.contains("not a usable"), "{local}");
        let duplicate = parse_candidate(
            &state(),
            br#"{"mode":"manual","primary":"1.1.1.1","secondary":"1.1.1.1"}"#,
        )
        .unwrap_err();
        assert!(duplicate.contains("different"), "{duplicate}");
        let ipv6 = parse_candidate(
            &state(),
            br#"{"mode":"manual","primary":"2a07:a8c0::94:a9b1"}"#,
        )
        .unwrap_err();
        assert!(ipv6.contains("U60 firmware"), "{ipv6}");
    }

    #[test]
    fn encrypted_modes_validate_transport_fields() {
        let doh = parse_candidate(
            &state(),
            br#"{"mode":"doh","primary":"1.1.1.1","doh_url":"http://1.1.1.1/dns-query"}"#,
        )
        .unwrap_err();
        assert!(doh.contains("https://"), "{doh}");
        let dot = parse_candidate(
            &state(),
            br#"{"mode":"dot","primary":"1.1.1.1","dot_endpoint":"1.1.1.1","dot_server_name":"1.1.1.1"}"#,
        )
        .unwrap_err();
        assert!(dot.contains("certificate hostname"), "{dot}");

        let nextdns = parse_candidate(
            &state(),
            br#"{"mode":"doh","primary":"1.1.1.1","secondary":"1.0.0.1","doh_url":"https://dns.nextdns.io/test"}"#,
        )
        .unwrap();
        assert_eq!(nextdns.encrypted.bootstrap_primary, "1.1.1.1");
        assert_eq!(nextdns.encrypted.bootstrap_secondary, "1.0.0.1");
    }

    #[test]
    fn dot_endpoint_defaults_to_standard_port() {
        assert_eq!(normalize_dot_endpoint("9.9.9.9").unwrap(), "9.9.9.9:853");
        assert_eq!(
            normalize_dot_endpoint("[2620:fe::fe]:8853").unwrap(),
            "[2620:fe::fe]:8853"
        );
        assert!(normalize_dot_endpoint("user@example.com:853").is_err());
        assert!(normalize_dot_endpoint("example.com/path").is_err());
    }

    #[test]
    fn wan_events_classify_every_connected_variant_and_disconnect() {
        for status in [
            "connected",
            "ipv4_connected",
            "ipv6_connected",
            "ipv4_ipv6_connected",
        ] {
            assert_eq!(
                network_action(WAN_EVENT, &json!({"wan_status": status})),
                Some(NetworkAction::Bearer {
                    connected: true,
                    force: false,
                }),
                "{status}"
            );
        }
        for status in ["disconnected", "ipv4_disconnected", "connecting"] {
            assert_eq!(
                network_action(WAN_EVENT, &json!({"wan_status": status})),
                Some(NetworkAction::Bearer {
                    connected: false,
                    force: false,
                }),
                "{status}"
            );
        }
        assert_eq!(network_action(WAN_EVENT, &json!({})), None);
    }

    #[test]
    fn sim_identity_boundaries_flush_before_the_new_profile_is_online() {
        for phase in ["leaving", "changed", "stalled"] {
            assert_eq!(
                network_action(crate::sim_identity::EVENT, &json!({"phase": phase})),
                Some(NetworkAction::Bearer {
                    connected: false,
                    force: true,
                }),
                "{phase}"
            );
        }
        assert_eq!(
            network_action(crate::sim_identity::EVENT, &json!({"phase": "online"})),
            Some(NetworkAction::Bearer {
                connected: true,
                force: true,
            })
        );
        assert_eq!(
            network_action(crate::sim_identity::EVENT, &json!({"phase": "idle"})),
            None
        );
    }

    #[test]
    fn only_a_confirmed_new_sim_identity_reconnects_wifi_clients() {
        assert!(reconnect_clients_after_sim_change(
            crate::sim_identity::EVENT,
            &json!({"phase":"online","identity_changed":true,"epoch":4})
        ));
        assert!(!reconnect_clients_after_sim_change(
            crate::sim_identity::EVENT,
            &json!({"phase":"online","identity_changed":false,"epoch":4})
        ));
        assert!(!reconnect_clients_after_sim_change(
            WAN_EVENT,
            &json!({"phase":"online","identity_changed":true})
        ));
    }

    #[test]
    fn interface_route_events_force_resync_but_lan_only_repairs_local_path() {
        assert_eq!(
            network_action(
                NETWORK_INTERFACE_EVENT,
                &json!({"interface":"zte_wan","action":"ifup"})
            ),
            Some(NetworkAction::Bearer {
                connected: true,
                force: true,
            })
        );
        assert_eq!(
            network_action(
                NETWORK_INTERFACE_EVENT,
                &json!({"interface":"zte_wan6","action":"ifdown"})
            ),
            Some(NetworkAction::Bearer {
                connected: false,
                force: true,
            })
        );
        assert_eq!(
            network_action(
                NETWORK_INTERFACE_EVENT,
                &json!({"interface":"lan","action":"ifup"})
            ),
            Some(NetworkAction::LocalRefresh)
        );
        assert_eq!(
            network_action(
                NETWORK_INTERFACE_EVENT,
                &json!({"interface":"wlan0","action":"ifup"})
            ),
            None
        );
    }

    #[test]
    fn service_loss_flushes_even_without_a_wan_event() {
        assert_eq!(
            network_action(SERVICE_EVENT, &json!({"network_type":"NO_SERVICE"})),
            Some(NetworkAction::Service {
                connected: false,
                network_type: "NO_SERVICE".to_string(),
            })
        );
    }

    #[test]
    fn limited_sa_flushes_as_disconnected_service() {
        assert_eq!(
            network_action(SERVICE_EVENT, &json!({"network_type":"LIMITED_SERVICE_SA"})),
            Some(NetworkAction::Service {
                connected: false,
                network_type: "LIMITED_SERVICE_SA".to_string(),
            })
        );
    }

    #[test]
    fn a_new_radio_service_flushes_after_registration() {
        assert_eq!(
            network_action(SERVICE_EVENT, &json!({"network_type":"5G_SA"})),
            Some(NetworkAction::Service {
                connected: true,
                network_type: "5G_SA".to_string(),
            })
        );
    }

    #[test]
    fn empty_service_event_does_not_flush() {
        assert_eq!(
            network_action(SERVICE_EVENT, &json!({"network_type":""})),
            None
        );
    }

    #[test]
    fn ipv6_default_route_requires_zero_prefix_and_mobile_interface() {
        let default = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003 rmnet_data0";
        assert!(has_ipv6_default_route(default));
        assert!(!has_ipv6_default_route(
            &default.replace("rmnet_data0", "br-lan")
        ));
        assert!(!has_ipv6_default_route(&default.replacen("00 ", "40 ", 1)));
    }
}
