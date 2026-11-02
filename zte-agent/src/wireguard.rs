//! Native WireGuard split-tunnel management for the U60 Linux kernel.
//!
//! The agent owns one fixed interface and two firewall chains. Imported
//! configurations never execute `wg-quick` hooks or shell fragments.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Command;
use std::str::FromStr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wireguard_control::{Backend, Device, DeviceUpdate, InterfaceName, Key, PeerConfigBuilder};

#[cfg(target_os = "linux")]
const BACKEND: Backend = Backend::Kernel;
#[cfg(not(target_os = "linux"))]
const BACKEND: Backend = Backend::Userspace;

const CONFIG_PATH: &str = "/data/local/tmp/wireguard_config.json";
const CONFIG_TEMP_PATH: &str = "/data/local/tmp/wireguard_config.json.tmp";
const INTERFACE: &str = "wg-u60";
const LAN_INTERFACE: &str = "br-lan";
const FORWARD_CHAIN: &str = "OPENU60_WG_FORWARD";
const NAT_CHAIN: &str = "OPENU60_WG_NAT";
const MAX_CONFIG_BYTES: usize = 64 * 1024;
const MAX_PEERS: usize = 16;
const MAX_ALLOWED_IPS: usize = 64;

/// Create, inspect and remove an unconfigured interface through the same
/// generic-netlink backend used in production. Existing interfaces are never
/// touched. This is intentionally a CLI-only installation diagnostic.
pub fn cli_smoke() -> i32 {
    const SMOKE_INTERFACE: &str = "wg-u60-smoke";
    let interface = match InterfaceName::from_str(SMOKE_INTERFACE) {
        Ok(interface) => interface,
        Err(error) => {
            eprintln!("wireguard smoke: invalid interface name: {error}");
            return 2;
        }
    };
    if Device::get(&interface, BACKEND).is_ok() {
        eprintln!("wireguard smoke: {SMOKE_INTERFACE} already exists; nothing changed");
        return 3;
    }
    if let Err(error) = DeviceUpdate::new().apply(&interface, BACKEND) {
        eprintln!("wireguard smoke: create failed: {error}");
        return 4;
    }
    let created = Device::get(&interface, BACKEND);
    let deleted = created
        .as_ref()
        .map_err(ToString::to_string)
        .and_then(|device| device.clone().delete().map_err(|error| error.to_string()));
    match (created, deleted) {
        (Ok(_), Ok(())) => {
            println!("wireguard kernel netlink smoke test passed");
            0
        }
        (Err(error), _) => {
            let _ = Command::new("ip")
                .args(["link", "delete", "dev", SMOKE_INTERFACE])
                .output();
            eprintln!("wireguard smoke: inspect failed: {error}");
            5
        }
        (_, Err(error)) => {
            eprintln!("wireguard smoke: cleanup failed: {error}");
            6
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Config {
    enabled: bool,
    private_key: String,
    addresses: Vec<String>,
    dns: Vec<String>,
    mtu: u16,
    listen_port: Option<u16>,
    peers: Vec<Peer>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Peer {
    name: String,
    public_key: String,
    preshared_key: Option<String>,
    endpoint: String,
    allowed_ips: Vec<String>,
    persistent_keepalive: Option<u16>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            private_key: String::new(),
            addresses: Vec::new(),
            dns: Vec::new(),
            mtu: 1420,
            listen_port: None,
            peers: Vec::new(),
        }
    }
}

impl Config {
    fn validate(&self) -> Result<(), String> {
        let private = parse_key(&self.private_key, "interface private key")?;
        if private == Key::zero() {
            return Err("interface private key cannot be all zeroes".into());
        }
        if self.addresses.is_empty() {
            return Err("the WireGuard interface needs at least one Address".into());
        }
        if self.addresses.len() > 8 {
            return Err("at most 8 interface addresses are supported".into());
        }
        for address in &self.addresses {
            parse_network(address, "interface address")?;
        }
        if !(1280..=1500).contains(&self.mtu) {
            return Err("MTU must be between 1280 and 1500".into());
        }
        if self.peers.is_empty() {
            return Err("the configuration needs at least one [Peer]".into());
        }
        if self.peers.len() > MAX_PEERS {
            return Err(format!("at most {MAX_PEERS} peers are supported"));
        }

        let local_public = private.get_public();
        let mut public_keys = HashSet::new();
        let mut route_owners: HashMap<IpNet, &str> = HashMap::new();
        for peer in &self.peers {
            let public = parse_key(&peer.public_key, &format!("{} public key", peer.name))?;
            if public == local_public {
                return Err(format!("{} uses the interface's own public key", peer.name));
            }
            if !public_keys.insert(public.to_base64()) {
                return Err(format!("{} repeats a peer public key", peer.name));
            }
            if let Some(key) = peer.preshared_key.as_deref() {
                parse_key(key, &format!("{} preshared key", peer.name))?;
            }
            parse_endpoint(&peer.endpoint)?;
            if peer.allowed_ips.is_empty() {
                return Err(format!("{} needs at least one AllowedIPs route", peer.name));
            }
            if peer.allowed_ips.len() > MAX_ALLOWED_IPS {
                return Err(format!(
                    "{} has more than {MAX_ALLOWED_IPS} AllowedIPs routes",
                    peer.name
                ));
            }
            for route in &peer.allowed_ips {
                let network = parse_network(route, &format!("{} AllowedIPs", peer.name))?;
                reject_unsafe_route(network)?;
                if let Some(owner) = route_owners.insert(network, &peer.name) {
                    return Err(format!(
                        "route {network} is assigned to both {owner} and {}",
                        peer.name
                    ));
                }
            }
        }
        Ok(())
    }

    fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("read saved WireGuard configuration: {error}"))?;
        let config: Self = serde_json::from_str(&text)
            .map_err(|error| format!("parse saved WireGuard configuration: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    fn save(&self, path: &Path, temp_path: &Path) -> Result<(), String> {
        let text = serde_json::to_vec(self)
            .map_err(|error| format!("serialize WireGuard configuration: {error}"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(temp_path)
            .map_err(|error| format!("open temporary WireGuard configuration: {error}"))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect WireGuard configuration: {error}"))?;
        file.write_all(&text)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("write WireGuard configuration: {error}"))?;
        fs::rename(temp_path, path)
            .map_err(|error| format!("commit WireGuard configuration: {error}"))
    }
}

struct State {
    config: Config,
    active: bool,
    last_error: Option<String>,
}

pub struct WireGuardManager {
    state: Mutex<State>,
}

impl WireGuardManager {
    pub fn new() -> Self {
        let (config, last_error) = if Path::new(CONFIG_PATH).exists() {
            match Config::load(Path::new(CONFIG_PATH)) {
                Ok(config) => (config, None),
                Err(error) => (Config::default(), Some(error)),
            }
        } else {
            (Config::default(), None)
        };
        Self {
            state: Mutex::new(State {
                config,
                active: false,
                last_error,
            }),
        }
    }

    /// Apply the saved state at startup and rebuild it after a WAN event so
    /// endpoint DNS and the carrier route cannot remain tied to an old bearer.
    pub fn start(self: &Arc<Self>, wan_events: mpsc::Receiver<Value>) {
        let manager = Arc::clone(self);
        crate::system::spawn_named("wireguard", move || {
            manager.reconcile("startup");
            for event in wan_events {
                if manager.enabled() && wan_connected(&event) {
                    manager.reconcile("WAN reconnect");
                }
            }
        });
    }

    fn enabled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .config
            .enabled
    }

    fn reconcile(&self, reason: &str) {
        let config = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .config
            .clone();
        let result = apply(&config);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match result {
            Ok(()) => {
                state.active = config.enabled;
                state.last_error = None;
            }
            Err(error) => {
                state.active = false;
                state.last_error = Some(error.clone());
                eprintln!("[wireguard] {reason} apply failed: {error}");
            }
        }
    }

    fn replace(&self, next: Config) -> Result<(), UpdateError> {
        next.validate().map_err(UpdateError::Invalid)?;
        let previous = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .config
            .clone();

        if let Err(error) = apply(&next) {
            let rollback = apply(&previous).err();
            self.record_failure(&previous, &error, rollback.as_deref());
            return Err(UpdateError::Apply(with_rollback(error, rollback)));
        }
        if let Err(error) = next.save(Path::new(CONFIG_PATH), Path::new(CONFIG_TEMP_PATH)) {
            let rollback = apply(&previous).err();
            self.record_failure(&previous, &error, rollback.as_deref());
            return Err(UpdateError::Apply(with_rollback(error, rollback)));
        }

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.config = next.clone();
        state.active = next.enabled;
        state.last_error = None;
        Ok(())
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), UpdateError> {
        let mut next = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .config
            .clone();
        if next.private_key.is_empty() {
            return Err(UpdateError::Invalid(
                "import a WireGuard configuration before enabling the tunnel".into(),
            ));
        }
        next.enabled = enabled;
        self.replace(next)
    }

    fn record_failure(&self, previous: &Config, error: &str, rollback: Option<&str>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.active = previous.enabled && rollback.is_none();
        state.last_error = Some(match rollback {
            Some(rollback) => format!("{error}; rollback also failed: {rollback}"),
            None => error.to_string(),
        });
    }

    fn status(&self) -> Value {
        let (config, remembered_active, last_error) = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            (state.config.clone(), state.active, state.last_error.clone())
        };
        public_status(&config, remembered_active, last_error)
    }
}

#[derive(Debug)]
enum UpdateError {
    Invalid(String),
    Apply(String),
}

#[derive(Deserialize)]
struct ImportRequest {
    config: String,
    #[serde(default)]
    enabled: bool,
}

pub fn status(manager: &WireGuardManager) -> (u16, Value) {
    (200, json!({"ok": true, "data": manager.status()}))
}

pub fn import(manager: &WireGuardManager, body: &[u8]) -> (u16, Value) {
    let request: ImportRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(_) => {
            return (
                400,
                json!({"ok": false, "error": "config and enabled are required"}),
            )
        }
    };
    let mut config = match parse_ini(&request.config) {
        Ok(config) => config,
        Err(error) => return (400, json!({"ok": false, "error": error})),
    };
    config.enabled = request.enabled;
    update_response(manager, manager.replace(config))
}

pub fn enable(manager: &WireGuardManager) -> (u16, Value) {
    update_response(manager, manager.set_enabled(true))
}

pub fn disable(manager: &WireGuardManager) -> (u16, Value) {
    update_response(manager, manager.set_enabled(false))
}

fn update_response(manager: &WireGuardManager, result: Result<(), UpdateError>) -> (u16, Value) {
    match result {
        Ok(()) => status(manager),
        Err(UpdateError::Invalid(error)) => (400, json!({"ok": false, "error": error})),
        Err(UpdateError::Apply(error)) => (503, json!({"ok": false, "error": error})),
    }
}

fn parse_ini(text: &str) -> Result<Config, String> {
    if text.len() > MAX_CONFIG_BYTES {
        return Err(format!(
            "WireGuard configuration exceeds {MAX_CONFIG_BYTES} bytes"
        ));
    }
    let mut config = Config::default();
    let mut section = Section::None;
    for (line_index, raw) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.eq_ignore_ascii_case("[Interface]") {
            if !matches!(section, Section::None) {
                return Err(format!(
                    "line {line_number}: [Interface] must appear once and first"
                ));
            }
            section = Section::Interface;
            continue;
        }
        if line.eq_ignore_ascii_case("[Peer]") {
            if matches!(section, Section::None) {
                return Err(format!(
                    "line {line_number}: [Interface] must appear before [Peer]"
                ));
            }
            if config.peers.len() >= MAX_PEERS {
                return Err(format!("at most {MAX_PEERS} peers are supported"));
            }
            config.peers.push(Peer {
                name: format!("Peer {}", config.peers.len() + 1),
                public_key: String::new(),
                preshared_key: None,
                endpoint: String::new(),
                allowed_ips: Vec::new(),
                persistent_keepalive: None,
            });
            section = Section::Peer(config.peers.len() - 1);
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .map(|(key, value)| (key.trim(), value.trim()))
            .ok_or_else(|| format!("line {line_number}: expected Key = value"))?;
        if value.is_empty() {
            return Err(format!("line {line_number}: {key} cannot be empty"));
        }
        match section {
            Section::None => return Err(format!("line {line_number}: add [Interface] first")),
            Section::Interface => parse_interface_line(&mut config, key, value, line_number)?,
            Section::Peer(index) => {
                parse_peer_line(&mut config.peers[index], key, value, line_number)?
            }
        }
    }
    if matches!(section, Section::None) {
        return Err("configuration is missing [Interface]".into());
    }
    config.validate()?;
    Ok(config)
}

#[derive(Clone, Copy)]
enum Section {
    None,
    Interface,
    Peer(usize),
}

fn parse_interface_line(
    config: &mut Config,
    key: &str,
    value: &str,
    line: usize,
) -> Result<(), String> {
    match key.to_ascii_lowercase().as_str() {
        "privatekey" => set_once(&mut config.private_key, value, "PrivateKey", line),
        "address" => {
            config.addresses.extend(split_list(value));
            Ok(())
        }
        "dns" => {
            config.dns.extend(split_list(value));
            Ok(())
        }
        "mtu" => {
            config.mtu = value
                .parse()
                .map_err(|_| format!("line {line}: MTU must be a number"))?;
            Ok(())
        }
        "listenport" => {
            config.listen_port = Some(
                value
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or_else(|| format!("line {line}: ListenPort must be 1-65535"))?,
            );
            Ok(())
        }
        "table" | "preup" | "postup" | "predown" | "postdown" | "saveconfig" => Err(format!(
            "line {line}: {key} is not accepted; the U60 agent owns routes and never executes hook commands"
        )),
        _ => Err(format!("line {line}: unsupported [Interface] key {key}")),
    }
}

fn parse_peer_line(peer: &mut Peer, key: &str, value: &str, line: usize) -> Result<(), String> {
    match key.to_ascii_lowercase().as_str() {
        "publickey" => set_once(&mut peer.public_key, value, "PublicKey", line),
        "presharedkey" => {
            if peer.preshared_key.is_some() {
                return Err(format!("line {line}: PresharedKey appears more than once"));
            }
            peer.preshared_key = Some(value.to_string());
            Ok(())
        }
        "endpoint" => set_once(&mut peer.endpoint, value, "Endpoint", line),
        "allowedips" => {
            peer.allowed_ips.extend(split_list(value));
            Ok(())
        }
        "persistentkeepalive" => {
            peer.persistent_keepalive = Some(
                value
                    .parse()
                    .map_err(|_| format!("line {line}: PersistentKeepalive must be 0-65535"))?,
            );
            Ok(())
        }
        _ => Err(format!("line {line}: unsupported [Peer] key {key}")),
    }
}

fn set_once(target: &mut String, value: &str, key: &str, line: usize) -> Result<(), String> {
    if !target.is_empty() {
        return Err(format!("line {line}: {key} appears more than once"));
    }
    *target = value.to_string();
    Ok(())
}

fn split_list(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
}

fn parse_key(value: &str, label: &str) -> Result<Key, String> {
    Key::from_base64(value.trim())
        .map_err(|_| format!("{label} must be a 32-byte base64 WireGuard key"))
}

fn parse_network(value: &str, label: &str) -> Result<IpNet, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("invalid {label}: {value}"))
}

fn parse_endpoint(value: &str) -> Result<(), String> {
    let (host, port) = endpoint_parts(value)?;
    if host.is_empty() || port.parse::<u16>().ok().filter(|port| *port != 0).is_none() {
        return Err(format!("invalid WireGuard endpoint: {value}"));
    }
    Ok(())
}

fn endpoint_parts(value: &str) -> Result<(&str, &str), String> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, rest) = rest
            .split_once(']')
            .ok_or_else(|| format!("invalid WireGuard endpoint: {value}"))?;
        let port = rest
            .strip_prefix(':')
            .ok_or_else(|| format!("invalid WireGuard endpoint: {value}"))?;
        return Ok((host, port));
    }
    value
        .rsplit_once(':')
        .ok_or_else(|| format!("WireGuard endpoint needs host:port: {value}"))
}

fn reject_unsafe_route(network: IpNet) -> Result<(), String> {
    if network.prefix_len() == 0 {
        return Err(format!(
            "default route {network} is blocked in the safe split-tunnel release"
        ));
    }
    match network.network() {
        IpAddr::V4(address)
            if address.is_loopback() || address.is_multicast() || address.is_link_local() =>
        {
            Err(format!("unsafe WireGuard route: {network}"))
        }
        IpAddr::V6(address)
            if address.is_loopback()
                || address.is_multicast()
                || address.is_unicast_link_local() =>
        {
            Err(format!("unsafe WireGuard route: {network}"))
        }
        _ => Ok(()),
    }
}

fn resolve_endpoint(endpoint: &str) -> Result<SocketAddr, String> {
    let resolved = endpoint
        .to_socket_addrs()
        .map_err(|error| format!("resolve WireGuard endpoint {endpoint}: {error}"))?
        .collect::<Vec<_>>();
    resolved
        .iter()
        .copied()
        .find(SocketAddr::is_ipv4)
        .or_else(|| resolved.first().copied())
        .ok_or_else(|| format!("WireGuard endpoint {endpoint} returned no address"))
}

fn apply(config: &Config) -> Result<(), String> {
    clear_owned_state()?;
    if !config.enabled {
        return Ok(());
    }
    config.validate()?;

    let interface = InterfaceName::from_str(INTERFACE)
        .map_err(|error| format!("invalid WireGuard interface name: {error}"))?;
    let private = parse_key(&config.private_key, "interface private key")?;
    let mut update = DeviceUpdate::new().set_private_key(private).replace_peers();
    update = match config.listen_port {
        Some(port) => update.set_listen_port(port),
        None => update.randomize_listen_port(),
    };
    let lan_address = crate::ubus::uci_get("network.lan.ipaddr")
        .ok()
        .and_then(|value| value.parse::<IpAddr>().ok());
    let mut resolved = Vec::with_capacity(config.peers.len());
    for peer in &config.peers {
        let endpoint = resolve_endpoint(&peer.endpoint)?;
        let public = parse_key(&peer.public_key, &format!("{} public key", peer.name))?;
        let mut builder = PeerConfigBuilder::new(&public)
            .set_endpoint(endpoint)
            .replace_allowed_ips();
        if let Some(key) = peer.preshared_key.as_deref() {
            builder =
                builder.set_preshared_key(parse_key(key, &format!("{} preshared key", peer.name))?);
        }
        if let Some(interval) = peer.persistent_keepalive.filter(|value| *value != 0) {
            builder = builder.set_persistent_keepalive_interval(interval);
        }
        for route in &peer.allowed_ips {
            let network = parse_network(route, &format!("{} AllowedIPs", peer.name))?;
            if network.contains(&endpoint.ip()) {
                return Err(format!(
                    "{} route {network} contains its own endpoint {}; this would loop the tunnel",
                    peer.name,
                    endpoint.ip()
                ));
            }
            if lan_address.is_some_and(|address| network.contains(&address)) {
                return Err(format!(
                    "{} route {network} contains the U60 LAN address; this would make the console unreachable",
                    peer.name
                ));
            }
            builder = builder.add_allowed_ip(network.network(), network.prefix_len());
        }
        update = update.add_peer(builder);
        resolved.push(endpoint);
    }

    if let Err(error) = update.apply(&interface, BACKEND) {
        let _ = clear_owned_state();
        return Err(format!("configure kernel WireGuard interface: {error}"));
    }
    let result = configure_interface(config);
    if let Err(error) = result {
        let _ = clear_owned_state();
        return Err(error);
    }
    Ok(())
}

fn configure_interface(config: &Config) -> Result<(), String> {
    run_required("ip", &["address", "flush", "dev", INTERFACE])?;
    for address in &config.addresses {
        let family = if matches!(parse_network(address, "interface address")?, IpNet::V4(_)) {
            "-4"
        } else {
            "-6"
        };
        run_required("ip", &[family, "address", "add", address, "dev", INTERFACE])?;
    }
    let mtu = config.mtu.to_string();
    run_required("ip", &["link", "set", "dev", INTERFACE, "mtu", &mtu, "up"])?;
    for peer in &config.peers {
        for route in &peer.allowed_ips {
            let family = if matches!(parse_network(route, "AllowedIPs")?, IpNet::V4(_)) {
                "-4"
            } else {
                "-6"
            };
            run_required(
                "ip",
                &[
                    family, "route", "replace", route, "dev", INTERFACE, "metric", "50",
                ],
            )?;
        }
    }
    install_firewall_rules()
}

fn install_firewall_rules() -> Result<(), String> {
    ensure_chain("iptables", "filter", FORWARD_CHAIN)?;
    run_required(
        "iptables",
        &["-w", "2", "-t", "filter", "-F", FORWARD_CHAIN],
    )?;
    run_required(
        "iptables",
        &[
            "-w",
            "2",
            "-t",
            "filter",
            "-A",
            FORWARD_CHAIN,
            "-i",
            LAN_INTERFACE,
            "-o",
            INTERFACE,
            "-j",
            "ACCEPT",
        ],
    )?;
    run_required(
        "iptables",
        &[
            "-w",
            "2",
            "-t",
            "filter",
            "-A",
            FORWARD_CHAIN,
            "-i",
            INTERFACE,
            "-o",
            LAN_INTERFACE,
            "-m",
            "conntrack",
            "--ctstate",
            "RELATED,ESTABLISHED",
            "-j",
            "ACCEPT",
        ],
    )?;
    ensure_jump("iptables", "filter", "FORWARD", FORWARD_CHAIN)?;

    ensure_chain("iptables", "nat", NAT_CHAIN)?;
    run_required("iptables", &["-w", "2", "-t", "nat", "-F", NAT_CHAIN])?;
    run_required(
        "iptables",
        &[
            "-w",
            "2",
            "-t",
            "nat",
            "-A",
            NAT_CHAIN,
            "-o",
            INTERFACE,
            "-j",
            "MASQUERADE",
        ],
    )?;
    ensure_jump("iptables", "nat", "POSTROUTING", NAT_CHAIN)
}

fn clear_owned_state() -> Result<(), String> {
    let mut errors = Vec::new();
    for (table, parent, chain) in [
        ("filter", "FORWARD", FORWARD_CHAIN),
        ("nat", "POSTROUTING", NAT_CHAIN),
    ] {
        if succeeds(
            "iptables",
            &["-w", "2", "-t", table, "-C", parent, "-j", chain],
        ) {
            if let Err(error) = run_required(
                "iptables",
                &["-w", "2", "-t", table, "-D", parent, "-j", chain],
            ) {
                errors.push(error);
            }
        }
        if succeeds("iptables", &["-w", "2", "-t", table, "-S", chain]) {
            if let Err(error) = run_required("iptables", &["-w", "2", "-t", table, "-F", chain]) {
                errors.push(error);
            }
            if let Err(error) = run_required("iptables", &["-w", "2", "-t", table, "-X", chain]) {
                errors.push(error);
            }
        }
    }
    let _ = Command::new("ip")
        .args(["link", "delete", "dev", INTERFACE])
        .output();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn ensure_chain(program: &str, table: &str, chain: &str) -> Result<(), String> {
    if succeeds(program, &["-w", "2", "-t", table, "-S", chain]) {
        Ok(())
    } else {
        run_required(program, &["-w", "2", "-t", table, "-N", chain])
    }
}

fn ensure_jump(program: &str, table: &str, parent: &str, chain: &str) -> Result<(), String> {
    if succeeds(
        program,
        &["-w", "2", "-t", table, "-C", parent, "-j", chain],
    ) {
        Ok(())
    } else {
        run_required(
            program,
            &["-w", "2", "-t", table, "-I", parent, "1", "-j", chain],
        )
    }
}

fn succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn run_required(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("run {program}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if detail.is_empty() {
        Err(format!("{program} exited with {}", output.status))
    } else {
        Err(format!("{program}: {detail}"))
    }
}

fn public_status(config: &Config, remembered_active: bool, last_error: Option<String>) -> Value {
    let interface = InterfaceName::from_str(INTERFACE).ok();
    let device = interface.and_then(|name| Device::get(&name, BACKEND).ok());
    let runtime_peers = device
        .as_ref()
        .map(|device| {
            device
                .peers
                .iter()
                .map(|peer| (peer.config.public_key.to_base64(), peer))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let public_key = Key::from_base64(&config.private_key)
        .ok()
        .map(|key| key.get_public().to_base64());
    let peers = config
        .peers
        .iter()
        .map(|peer| {
            let runtime = runtime_peers.get(&peer.public_key);
            json!({
                "name": peer.name,
                "public_key": peer.public_key,
                "preshared_key_configured": peer.preshared_key.is_some(),
                "endpoint": peer.endpoint,
                "allowed_ips": peer.allowed_ips,
                "persistent_keepalive": peer.persistent_keepalive,
                "last_handshake_at": runtime.and_then(|peer| peer.stats.last_handshake_time).and_then(system_time_secs),
                "rx_bytes": runtime.map_or(0, |peer| peer.stats.rx_bytes),
                "tx_bytes": runtime.map_or(0, |peer| peer.stats.tx_bytes)
            })
        })
        .collect::<Vec<_>>();
    json!({
        "supported": cfg!(target_os = "linux"),
        "mode": "split_tunnel",
        "interface": INTERFACE,
        "configured": !config.private_key.is_empty(),
        "enabled": config.enabled,
        "active": config.enabled && remembered_active && device.is_some(),
        "public_key": public_key,
        "addresses": config.addresses,
        "dns": config.dns,
        "dns_applied": false,
        "mtu": config.mtu,
        "listen_port": device.as_ref().and_then(|device| device.listen_port).or(config.listen_port),
        "peers": peers,
        "last_error": last_error
    })
}

fn system_time_secs(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn with_rollback(error: String, rollback: Option<String>) -> String {
    match rollback {
        Some(rollback) => format!("{error}; rollback also failed: {rollback}"),
        None => error,
    }
}

fn wan_connected(event: &Value) -> bool {
    let value = event
        .get("status")
        .or_else(|| event.get("connect_status"))
        .or_else(|| event.get("data").and_then(|data| data.get("status")));
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_i64().is_some_and(|value| value != 0),
        Some(Value::String(value)) => {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "up" | "connected" | "connect"
            )
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVATE: &str = "SGb+ojrRNDuMePufwtIYhXzA//k6wF3R21tEBgKlzlM=";
    const PUBLIC: &str = "Y/NG0R2i1Gtvollv5o/U3YaOlewG6HeyTLIUlrTrKg4=";

    fn valid_config(allowed: &str) -> String {
        format!(
            "[Interface]\nPrivateKey = {PRIVATE}\nAddress = 10.8.0.2/32\nDNS = 10.8.0.1\n\n[Peer]\nPublicKey = {PUBLIC}\nEndpoint = vpn.example.com:51820\nAllowedIPs = {allowed}\nPersistentKeepalive = 25\n"
        )
    }

    #[test]
    fn parser_accepts_standard_split_tunnel_config() {
        let config = parse_ini(&valid_config("10.8.0.0/24")).unwrap();
        assert_eq!(config.peers[0].allowed_ips, ["10.8.0.0/24"]);
    }

    #[test]
    fn parser_rejects_default_route() {
        let error = parse_ini(&valid_config("0.0.0.0/0")).err().unwrap();
        assert!(error.contains("safe split-tunnel"), "{error}");
    }

    #[test]
    fn parser_rejects_shell_hooks() {
        let input = valid_config("10.8.0.0/24").replace(
            "Address = 10.8.0.2/32",
            "Address = 10.8.0.2/32\nPostUp = reboot",
        );
        let error = parse_ini(&input).err().unwrap();
        assert!(error.contains("never executes hook commands"), "{error}");
    }

    #[test]
    fn public_status_never_contains_private_or_preshared_key() {
        let mut config = parse_ini(&valid_config("10.8.0.0/24")).unwrap();
        config.peers[0].preshared_key = Some(PRIVATE.to_string());
        let body = public_status(&config, false, None).to_string();
        assert!(!body.contains(PRIVATE), "secret leaked in status: {body}");
    }

    #[test]
    fn wan_event_recognizes_disconnect() {
        assert!(!wan_connected(&json!({"status": "disconnected"})));
    }
}
