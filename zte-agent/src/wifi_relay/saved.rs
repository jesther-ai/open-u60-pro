//! Opt-in remembered upstream credentials, separate from disposable sessions.
//! Matching includes security: an open SSID cannot inherit a WPA2 credential.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

use super::{net, Network, JOURNAL};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Tuning {
    #[default]
    Off,
    Auto,
}

impl Tuning {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
        }
    }
    pub(super) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "off" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            _ => Err("Choose negotiated bandwidth or automatic tuning".into()),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    ssid: String,
    security: String,
    password: String,
    #[serde(default)]
    width_mode: Tuning,
}

pub(super) fn key(ssid: &str, security: &str) -> String {
    let hash = Sha256::digest(format!("{security}\0{ssid}"));
    hash[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn load(root: &Path) -> Result<Vec<Saved>, String> {
    let file = match std::fs::File::open(root.join("saved-wifi.json")) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("Cannot read remembered Wi-Fi networks".into()),
    };
    let mut data = Vec::new();
    file.take(16 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(|_| "Cannot read remembered Wi-Fi networks")?;
    if data.len() > 16 * 1024 {
        return Err("Remembered Wi-Fi store is too large".into());
    }
    serde_json::from_slice(&data).map_err(|_| "Remembered Wi-Fi store is invalid".into())
}

pub(super) fn password(network: &Network) -> Result<Option<String>, String> {
    Ok(load(Path::new(JOURNAL))?
        .into_iter()
        .find(|n| n.ssid == network.ssid && n.security == network.security)
        .map(|n| n.password))
}

pub(super) fn tuning(network: &Network) -> Result<Tuning, String> {
    Ok(load(Path::new(JOURNAL))?
        .into_iter()
        .find(|n| n.ssid == network.ssid && n.security == network.security)
        .map_or(Tuning::Off, |n| n.width_mode))
}

/// Persist the per-network choice even when the owner does not save a password.
/// An unchecked Remember never erases a previously saved credential.
pub(super) fn remember(
    network: &Network,
    password: Option<&str>,
    width_mode: Tuning,
) -> Result<(), String> {
    remember_at(Path::new(JOURNAL), network, password, width_mode)
}

fn remember_at(
    root: &Path,
    network: &Network,
    password: Option<&str>,
    width_mode: Tuning,
) -> Result<(), String> {
    let mut saved = load(root)?;
    let password = password
        .map(str::to_owned)
        .or_else(|| {
            saved
                .iter()
                .find(|n| n.ssid == network.ssid && n.security == network.security)
                .map(|n| n.password.clone())
        })
        .unwrap_or_default();
    saved.retain(|n| n.ssid != network.ssid || n.security != network.security);
    if saved.len() >= 32 {
        return Err("At most 32 Wi-Fi network preferences can be saved".into());
    }
    saved.push(Saved {
        ssid: network.ssid.clone(),
        security: network.security.into(),
        password,
        width_mode,
    });
    write(root, &saved)
}

pub(super) fn set_tuning(id: &str, width_mode: Tuning) -> Result<(), String> {
    let root = Path::new(JOURNAL);
    let mut saved = load(root)?;
    let entry = saved
        .iter_mut()
        .find(|n| key(&n.ssid, &n.security) == id)
        .ok_or("Saved network not found")?;
    entry.width_mode = width_mode;
    write(root, &saved)
}

fn write(root: &Path, saved: &[Saved]) -> Result<(), String> {
    let data = serde_json::to_vec(saved).map_err(|_| "Cannot encode remembered Wi-Fi")?;
    net::write_private(&root.join("saved-wifi.json"), &data)
        .map_err(|_| "Cannot save remembered Wi-Fi".into())
}

pub(super) fn forget(id: &str) -> Result<(), String> {
    let root = Path::new(JOURNAL);
    let mut saved = load(root)?;
    let len = saved.len();
    saved.retain(|n| key(&n.ssid, &n.security) != id);
    if len == saved.len() {
        return Err("Remembered Wi-Fi not found".into());
    }
    write(root, &saved)
}

pub(super) fn annotate(status: &mut Value) {
    match load(Path::new(JOURNAL)) {
        Ok(saved) => annotate_with(status, &saved),
        Err(e) => {
            status["saved_error"] = json!(e);
        }
    }
}

fn annotate_with(status: &mut Value, saved: &[Saved]) {
    status["saved_networks"] = json!(saved
        .iter()
        .map(|n| json!({"id":key(&n.ssid,&n.security),"ssid":n.ssid,"security":n.security,"width_mode":n.width_mode,"remembered":!n.password.is_empty() || n.security == "open"}))
        .collect::<Vec<_>>());
    if let Some(networks) = status["networks"].as_array_mut() {
        for network in networks {
            let entry = saved.iter().find(|n| {
                network["ssid"].as_str() == Some(&n.ssid)
                    && network["security"].as_str() == Some(&n.security)
            });
            network["remembered"] =
                json!(entry.is_some_and(|n| !n.password.is_empty() || n.security == "open"));
            network["width_mode"] = json!(entry.map_or(Tuning::Off, |n| n.width_mode));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tuning_migrates_old_credentials_and_is_private_per_network() {
        let old: Vec<Saved> = serde_json::from_str(
            r#"[{"ssid":"Home","security":"wpa2","password":"private-password"}]"#,
        )
        .unwrap();
        assert_eq!(old[0].width_mode, Tuning::Off);
        let root = std::env::temp_dir().join(format!("u60-tuning-{}", std::process::id()));
        write(&root, &old).unwrap();
        let network = Network {
            id: "scan".into(),
            ssid: "Home".into(),
            security: "wpa2",
            bssid: "02:00:00:00:00:01".into(),
            frequency: 5260,
            supported: true,
            signal: -40,
        };
        remember_at(&root, &network, None, Tuning::Auto).unwrap();
        let other = Network {
            ssid: "Other".into(),
            ..network.clone()
        };
        remember_at(&root, &other, None, Tuning::Off).unwrap();
        let stored = load(&root).unwrap();
        assert_eq!(
            stored[0].password, "private-password",
            "changing mode preserves saved credentials"
        );
        assert_eq!(stored[0].width_mode, Tuning::Auto);
        assert!(
            stored[1].password.is_empty(),
            "preference alone must not opt into credential storage"
        );
        let mut status = json!({"networks":[{"ssid":"Home","security":"wpa2"},{"ssid":"Home","security":"open"},{"ssid":"Other","security":"wpa2"}]});
        annotate_with(&mut status, &stored);
        assert_eq!(status["networks"][0]["width_mode"], "auto");
        assert_eq!(status["networks"][1]["width_mode"], "off");
        assert_eq!(status["networks"][2]["remembered"], false);
        assert!(!status.to_string().contains("private-password"));
        assert!(
            Tuning::parse("observe").is_err(),
            "web choice has only two stable values"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn remembered_metadata_never_contains_password_or_matches_open_twin() {
        let saved = vec![Saved {
            ssid: "Hotel".into(),
            security: "wpa2".into(),
            password: "private-password".into(),
            width_mode: Tuning::Auto,
        }];
        let mut status = json!({"networks":[{"ssid":"Hotel","security":"wpa2"},{"ssid":"Hotel","security":"open"}]});
        annotate_with(&mut status, &saved);
        assert_eq!(status["networks"][0]["remembered"], true);
        assert_eq!(status["networks"][1]["remembered"], false);
        assert!(!status.to_string().contains("private-password"));
    }

    #[test]
    fn saved_credentials_survive_reload_with_private_permissions_and_can_be_forgotten() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("u60-remembered-{}", std::process::id()));
        let saved = vec![Saved {
            ssid: "Test".into(),
            security: "wpa2".into(),
            password: "qa-password".into(),
            width_mode: Tuning::Off,
        }];
        write(&root, &saved).unwrap();
        assert_eq!(load(&root).unwrap()[0].password, "qa-password");
        assert_eq!(
            std::fs::metadata(root.join("saved-wifi.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        write(&root, &[]).unwrap();
        assert!(load(&root).unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
