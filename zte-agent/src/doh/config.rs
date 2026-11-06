use serde::{Deserialize, Serialize};

const CONFIG_PATH: &str = "/data/local/tmp/doh_config.json";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncryptedTransport {
    #[default]
    Doh,
    Dot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DohConfig {
    pub enabled: bool,
    pub listen_addr: String,
    pub transport: EncryptedTransport,
    pub upstream_url: String,
    pub dot_endpoint: String,
    pub dot_server_name: String,
    /// Plain IPv4 resolvers used only to bootstrap encrypted resolver hostnames.
    /// Persisted because some firmware paths replace `/etc/resolv.conf` when a
    /// WAN, APN or eSIM profile changes.
    pub bootstrap_primary: String,
    pub bootstrap_secondary: String,
    pub timeout_ms: u32,
    pub cache_enabled: bool,
    pub cache_max_entries: usize,
}

impl Default for DohConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_addr: "127.0.0.1:5353".to_string(),
            transport: EncryptedTransport::Doh,
            upstream_url: "https://1.1.1.1/dns-query".to_string(),
            dot_endpoint: "1.1.1.1:853".to_string(),
            dot_server_name: "one.one.one.one".to_string(),
            bootstrap_primary: String::new(),
            bootstrap_secondary: String::new(),
            timeout_ms: 3000,
            cache_enabled: true,
            cache_max_entries: 512,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DohConfigPatch {
    pub listen_addr: Option<String>,
    pub transport: Option<EncryptedTransport>,
    pub upstream_url: Option<String>,
    pub dot_endpoint: Option<String>,
    pub dot_server_name: Option<String>,
    pub bootstrap_primary: Option<String>,
    pub bootstrap_secondary: Option<String>,
    pub timeout_ms: Option<u32>,
    pub cache_enabled: Option<bool>,
    pub cache_max_entries: Option<usize>,
}

impl DohConfig {
    pub fn apply_patch(&mut self, patch: DohConfigPatch) {
        if let Some(v) = patch.listen_addr {
            self.listen_addr = v;
        }
        if let Some(v) = patch.transport {
            self.transport = v;
        }
        if let Some(v) = patch.upstream_url {
            self.upstream_url = v.trim().to_string();
        }
        if let Some(v) = patch.dot_endpoint {
            self.dot_endpoint = v.trim().to_string();
        }
        if let Some(v) = patch.dot_server_name {
            self.dot_server_name = v.trim().to_string();
        }
        if let Some(v) = patch.bootstrap_primary {
            self.bootstrap_primary = v.trim().to_string();
        }
        if let Some(v) = patch.bootstrap_secondary {
            self.bootstrap_secondary = v.trim().to_string();
        }
        if let Some(v) = patch.timeout_ms {
            self.timeout_ms = v;
        }
        if let Some(v) = patch.cache_enabled {
            self.cache_enabled = v;
        }
        if let Some(v) = patch.cache_max_entries {
            self.cache_max_entries = v;
        }
    }
}

pub fn load() -> DohConfig {
    std::fs::read_to_string(CONFIG_PATH)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(config: &DohConfig) -> Result<(), String> {
    let json = serde_json::to_string_pretty(config).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(CONFIG_PATH, json).map_err(|e| format!("write {CONFIG_PATH}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_without_bootstrap_fields_still_loads() {
        let config: DohConfig = serde_json::from_str(
            r#"{"enabled":true,"upstream_url":"https://dns.example/dns-query"}"#,
        )
        .unwrap();
        assert!(config.bootstrap_primary.is_empty());
        assert!(config.bootstrap_secondary.is_empty());
        assert_eq!(config.transport, EncryptedTransport::Doh);
    }

    #[test]
    fn bootstrap_patch_is_trimmed_and_persistable() {
        let mut config = DohConfig::default();
        let patch: DohConfigPatch = serde_json::from_str(
            r#"{"bootstrap_primary":" 1.1.1.1 ","bootstrap_secondary":" 1.0.0.1 "}"#,
        )
        .unwrap();
        config.apply_patch(patch);
        assert_eq!(config.bootstrap_primary, "1.1.1.1");
        assert_eq!(config.bootstrap_secondary, "1.0.0.1");
    }
}
