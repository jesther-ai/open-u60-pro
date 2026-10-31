//! Opt-in IPv4 TTL and IPv6 hop-limit rewriting for cellular WAN traffic.
//!
//! The feature owns one chain in each legacy mangle table. It never changes
//! the router's boot scripts or restarts the firewall, and failures are
//! reported without preventing the agent from starting.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const CONFIG_PATH: &str = "/data/local/tmp/ttl_config.json";
const CONFIG_TEMP_PATH: &str = "/data/local/tmp/ttl_config.json.tmp";
const CHAIN: &str = "OPENU60_TTL";
const WAN_INTERFACE: &str = "rmnet_data0";
const DEFAULT_TTL: u8 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ttl")]
    pub ipv4_ttl: u8,
    #[serde(default = "default_ttl")]
    pub ipv6_hop_limit: u8,
}

const fn default_ttl() -> u8 {
    DEFAULT_TTL
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            ipv4_ttl: DEFAULT_TTL,
            ipv6_hop_limit: DEFAULT_TTL,
        }
    }
}

impl Config {
    fn validate(self) -> Result<Self, String> {
        if self.ipv4_ttl == 0 {
            return Err("ipv4_ttl must be 1-255".into());
        }
        if self.ipv6_hop_limit == 0 {
            return Err("ipv6_hop_limit must be 1-255".into());
        }
        Ok(self)
    }

    fn patched(self, patch: &Value) -> Result<Self, String> {
        let mut next = self;
        if let Some(value) = patch.get("enabled") {
            next.enabled = value
                .as_bool()
                .ok_or_else(|| "enabled must be a boolean".to_string())?;
        }
        if let Some(value) = patch.get("ipv4_ttl") {
            next.ipv4_ttl = parse_hop_value(value, "ipv4_ttl")?;
        }
        if let Some(value) = patch.get("ipv6_hop_limit") {
            next.ipv6_hop_limit = parse_hop_value(value, "ipv6_hop_limit")?;
        }
        next.validate()
    }

    fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
            .and_then(|config| config.validate().ok())
            .unwrap_or_default()
    }

    fn save(self, path: &Path, temp_path: &Path) -> Result<(), String> {
        let text = serde_json::to_string(&self)
            .map_err(|error| format!("serialize TTL configuration: {error}"))?;
        fs::write(temp_path, text).map_err(|error| format!("write TTL configuration: {error}"))?;
        fs::rename(temp_path, path).map_err(|error| format!("commit TTL configuration: {error}"))
    }
}

fn parse_hop_value(value: &Value, name: &str) -> Result<u8, String> {
    let value = value
        .as_u64()
        .ok_or_else(|| format!("{name} must be an integer from 1 to 255"))?;
    u8::try_from(value)
        .ok()
        .filter(|value| *value != 0)
        .ok_or_else(|| format!("{name} must be 1-255"))
}

#[derive(Debug)]
struct State {
    config: Config,
    active: bool,
    last_error: Option<String>,
}

pub struct TtlManager {
    state: Mutex<State>,
}

impl TtlManager {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                config: Config::load(Path::new(CONFIG_PATH)),
                active: false,
                last_error: None,
            }),
        }
    }

    /// Reconcile the saved configuration at startup, then reapply enabled
    /// rules after WAN reconnect events. The worker is non-fatal: a bad
    /// firewall state cannot stop the HTTP agent or the router's boot sequence.
    pub fn start(self: &Arc<Self>, wan_events: mpsc::Receiver<Value>) {
        let manager = Arc::clone(self);
        crate::system::spawn_named("ttl-rules", move || {
            manager.reconcile("startup");
            for _ in wan_events {
                if manager.enabled() {
                    manager.reconcile("WAN event");
                }
            }
        });
    }

    pub fn status(&self) -> Value {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        json!({
            "enabled": state.config.enabled,
            "ipv4_ttl": state.config.ipv4_ttl,
            "ipv6_hop_limit": state.config.ipv6_hop_limit,
            "active": state.active,
            "last_error": state.last_error,
            "wan_interface": WAN_INTERFACE,
            "scope": "cellular_software_path",
            "hardware_acceleration": "may_bypass"
        })
    }

    fn enabled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .config
            .enabled
    }

    fn update(&self, patch: &Value) -> Result<(), UpdateError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let previous = state.config;
        let next = previous.patched(patch).map_err(UpdateError::Invalid)?;

        if let Err(error) = apply(next) {
            let rollback = apply(previous).err();
            state.active = previous.enabled && rollback.is_none();
            state.last_error = Some(with_rollback(error.clone(), rollback));
            return Err(UpdateError::Apply(error));
        }

        if let Err(error) = next.save(Path::new(CONFIG_PATH), Path::new(CONFIG_TEMP_PATH)) {
            let rollback = apply(previous).err();
            state.active = previous.enabled && rollback.is_none();
            state.last_error = Some(with_rollback(error.clone(), rollback));
            return Err(UpdateError::Apply(error));
        }

        state.config = next;
        state.active = next.enabled;
        state.last_error = None;
        Ok(())
    }

    fn reconcile(&self, reason: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match apply(state.config) {
            Ok(()) => {
                state.active = state.config.enabled;
                state.last_error = None;
            }
            Err(error) => {
                state.active = false;
                state.last_error = Some(error.clone());
                eprintln!("[ttl] {reason} apply failed: {error}");
            }
        }
    }
}

fn with_rollback(error: String, rollback: Option<String>) -> String {
    match rollback {
        Some(rollback) => format!("{error}; rollback also failed: {rollback}"),
        None => error,
    }
}

#[derive(Debug)]
enum UpdateError {
    Invalid(String),
    Apply(String),
}

pub fn config_get(manager: &TtlManager) -> (u16, Value) {
    (200, json!({"ok": true, "data": manager.status()}))
}

pub fn config_set(manager: &TtlManager, body: &[u8]) -> (u16, Value) {
    let patch: Value = match serde_json::from_slice::<Value>(body) {
        Ok(value) if value.is_object() => value,
        Ok(_) => return (400, json!({"ok": false, "error": "JSON object required"})),
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match manager.update(&patch) {
        Ok(()) => config_get(manager),
        Err(UpdateError::Invalid(error)) => (400, json!({"ok": false, "error": error})),
        Err(UpdateError::Apply(error)) => (503, json!({"ok": false, "error": error})),
    }
}

fn apply(config: Config) -> Result<(), String> {
    if config.enabled {
        ensure_family("iptables", "TTL", "--ttl-set", config.ipv4_ttl)?;
        ensure_family("ip6tables", "HL", "--hl-set", config.ipv6_hop_limit)
    } else {
        clear_family("iptables")?;
        clear_family("ip6tables")
    }
}

fn ensure_family(program: &str, target: &str, set_option: &str, value: u8) -> Result<(), String> {
    if !succeeds(program, &["-w", "2", "-t", "mangle", "-S", CHAIN]) {
        run_required(program, &["-w", "2", "-t", "mangle", "-N", CHAIN])?;
    }
    run_required(program, &["-w", "2", "-t", "mangle", "-F", CHAIN])?;
    let value = value.to_string();
    run_required(
        program,
        &[
            "-w",
            "2",
            "-t",
            "mangle",
            "-A",
            CHAIN,
            "-o",
            WAN_INTERFACE,
            "-j",
            target,
            set_option,
            &value,
        ],
    )?;
    if !succeeds(
        program,
        &["-w", "2", "-t", "mangle", "-C", "POSTROUTING", "-j", CHAIN],
    ) {
        run_required(
            program,
            &[
                "-w",
                "2",
                "-t",
                "mangle",
                "-I",
                "POSTROUTING",
                "1",
                "-j",
                CHAIN,
            ],
        )?;
    }
    Ok(())
}

fn clear_family(program: &str) -> Result<(), String> {
    if !succeeds(program, &["-w", "2", "-t", "mangle", "-S", CHAIN]) {
        return Ok(());
    }
    if succeeds(
        program,
        &["-w", "2", "-t", "mangle", "-C", "POSTROUTING", "-j", CHAIN],
    ) {
        run_required(
            program,
            &["-w", "2", "-t", "mangle", "-D", "POSTROUTING", "-j", CHAIN],
        )?;
    }
    run_required(program, &["-w", "2", "-t", "mangle", "-F", CHAIN])?;
    run_required(program, &["-w", "2", "-t", "mangle", "-X", CHAIN])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_disabled_and_use_standard_hop_values() {
        assert_eq!(
            Config::default(),
            Config {
                enabled: false,
                ipv4_ttl: 64,
                ipv6_hop_limit: 64,
            }
        );
    }

    #[test]
    fn partial_patch_preserves_unspecified_values() {
        let updated = Config::default()
            .patched(&json!({"enabled": true, "ipv4_ttl": 65}))
            .unwrap();
        assert_eq!(
            updated,
            Config {
                enabled: true,
                ipv4_ttl: 65,
                ipv6_hop_limit: 64,
            }
        );
    }

    #[test]
    fn zero_hop_value_is_rejected() {
        let error = Config::default()
            .patched(&json!({"ipv6_hop_limit": 0}))
            .unwrap_err();
        assert!(error.contains("1-255"));
    }

    #[test]
    fn out_of_range_hop_value_is_rejected() {
        let error = Config::default()
            .patched(&json!({"ipv4_ttl": 256}))
            .unwrap_err();
        assert!(error.contains("1-255"));
    }
}
