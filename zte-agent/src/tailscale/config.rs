use serde::{Deserialize, Serialize};

const CONFIG_PATH: &str = "/data/local/tmp/tailscale_config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsConfig {
    pub enabled: bool,
    /// Set true after a successful enrollment, false on logout. Distinguishes
    /// "never enrolled" from "enrolled but key invalid" — the state file alone
    /// can't (tailscaled creates it before first login).
    #[serde(default)]
    pub enrolled: bool,
    #[serde(default = "default_hostname")]
    pub hostname: String,
    #[serde(default)]
    pub advertise_routes: Vec<String>,
    #[serde(default = "default_true")]
    pub hold_wakelock: bool,
    #[serde(default)]
    pub tailscale_ssh: bool,
    #[serde(default)]
    pub pinned_version: String,
}

fn default_hostname() -> String {
    "u60-pro".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for TsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            enrolled: false,
            hostname: default_hostname(),
            advertise_routes: Vec::new(),
            hold_wakelock: true,
            tailscale_ssh: false,
            pinned_version: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TsConfigPatch {
    pub hostname: Option<String>,
    pub advertise_routes: Option<Vec<String>>,
    pub hold_wakelock: Option<bool>,
    pub tailscale_ssh: Option<bool>,
}

impl TsConfig {
    pub fn apply_patch(&mut self, patch: TsConfigPatch) -> Result<(), String> {
        if let Some(v) = patch.hostname {
            let v = v.trim().to_string();
            if v.is_empty() || v.len() > 63 || !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err("hostname must be 1-63 chars, alphanumeric or '-'".into());
            }
            self.hostname = v;
        }
        if let Some(v) = patch.advertise_routes {
            for route in &v {
                if !valid_cidr(route) {
                    return Err(format!("invalid CIDR route: {route}"));
                }
            }
            self.advertise_routes = v;
        }
        if let Some(v) = patch.hold_wakelock {
            self.hold_wakelock = v;
        }
        if let Some(v) = patch.tailscale_ssh {
            self.tailscale_ssh = v;
        }
        Ok(())
    }
}

/// Minimal IPv4 CIDR validation (e.g. "192.168.0.0/24").
fn valid_cidr(s: &str) -> bool {
    let mut parts = s.splitn(2, '/');
    let (addr, prefix) = match (parts.next(), parts.next()) {
        (Some(a), Some(p)) => (a, p),
        _ => return false,
    };
    let prefix_ok = prefix.parse::<u8>().map(|p| p <= 32).unwrap_or(false);
    let octets: Vec<&str> = addr.split('.').collect();
    let addr_ok = octets.len() == 4 && octets.iter().all(|o| o.parse::<u8>().is_ok());
    prefix_ok && addr_ok
}

pub fn load() -> TsConfig {
    std::fs::read_to_string(CONFIG_PATH)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(config: &TsConfig) -> Result<(), String> {
    let json = serde_json::to_string_pretty(config).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(CONFIG_PATH, json).map_err(|e| format!("write {CONFIG_PATH}: {e}"))
}
