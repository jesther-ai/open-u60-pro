//! The upstream to reconnect after the agent starts again (an update, a crash
//! or a reboot). Written when the owner's connect succeeds with a credential
//! the router already keeps (an open network, or a remembered one with the
//! same password), removed when the owner connects elsewhere or disconnects.
//! A dropped link or a failed restore keeps it. Never holds a password.

use serde::{Deserialize, Serialize};
use std::path::Path;

use super::{net, Network};

const FILE: &str = "resume.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Resume {
    pub ssid: String,
    pub security: String,
    pub bssid: String,
    pub ipv6: bool,
}

pub(super) fn load(root: &Path) -> Option<Resume> {
    let data = std::fs::read(root.join(FILE)).ok()?;
    serde_json::from_slice(&data).ok()
}

pub(super) fn save(root: &Path, network: &Network, ipv6: bool) -> Result<(), String> {
    let resume = Resume { ssid: network.ssid.clone(), security: network.security.into(), bssid: network.bssid.clone(), ipv6 };
    let data = serde_json::to_vec(&resume).map_err(|_| "Cannot encode the relay to restore")?;
    net::write_private(&root.join(FILE), &data).map_err(|_| "Cannot save the relay to restore after a restart".into())
}

pub(super) fn clear(root: &Path) -> Result<(), String> {
    match std::fs::remove_file(root.join(FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err("Cannot clear the relay to restore".into()),
        _ => Ok(()),
    }
}

/// The scanned network to rejoin: the same SSID and security, on the same
/// BSS while it is still there, else the strongest one (mesh, a new AP).
pub(super) fn pick<'a>(resume: &Resume, scans: &'a [Network]) -> Option<&'a Network> {
    let same = scans.iter().filter(|n| n.supported && n.ssid == resume.ssid && n.security == resume.security);
    same.clone().find(|n| n.bssid == resume.bssid).or_else(|| same.max_by_key(|n| n.signal))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(ssid: &str, bssid: &str, signal: i32, security: &'static str) -> Network {
        Network { id: format!("{ssid}-{bssid}"), ssid: ssid.into(), frequency: 2437, signal, security, supported: true, bssid: bssid.into() }
    }

    #[test]
    fn saved_target_survives_reload_privately_without_a_password() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("u60-relay-resume-{}", std::process::id()));
        assert_eq!(load(&root), None);
        save(&root, &network("Home", "aa:bb:cc:dd:ee:01", -50, "wpa2"), false).unwrap();
        let resume = load(&root).unwrap();
        assert_eq!(resume, Resume { ssid: "Home".into(), security: "wpa2".into(), bssid: "aa:bb:cc:dd:ee:01".into(), ipv6: false });
        assert_eq!(std::fs::metadata(root.join(FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!std::fs::read_to_string(root.join(FILE)).unwrap().contains("password"));
        clear(&root).unwrap();
        clear(&root).unwrap();
        assert_eq!(load(&root), None);
        std::fs::write(root.join(FILE), b"{").unwrap();
        assert_eq!(load(&root), None, "a damaged file restores nothing");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejoins_the_same_bss_else_the_strongest_with_the_same_security() {
        let resume = Resume { ssid: "Home".into(), security: "wpa2".into(), bssid: "aa:bb:cc:dd:ee:01".into(), ipv6: true };
        let mut unsupported = network("Home", "aa:bb:cc:dd:ee:04", -20, "wpa2");
        unsupported.supported = false;
        let scans = [
            network("Home", "aa:bb:cc:dd:ee:02", -40, "wpa2"),
            network("Home", "aa:bb:cc:dd:ee:01", -70, "wpa2"),
            network("Home", "aa:bb:cc:dd:ee:03", -30, "open"),
            unsupported,
            network("Other", "aa:bb:cc:dd:ee:05", -10, "wpa2"),
        ];
        assert_eq!(pick(&resume, &scans).unwrap().bssid, "aa:bb:cc:dd:ee:01");
        assert!(pick(&resume, &scans[2..]).is_none(), "an open twin never takes a WPA2 target");
        let moved = Resume { bssid: "aa:bb:cc:dd:ee:09".into(), ..resume };
        assert_eq!(pick(&moved, &scans).unwrap().bssid, "aa:bb:cc:dd:ee:02");
    }
}
