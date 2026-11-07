//! Temporary netifd data, not UCI deltas. Null values release typed odhcpd
//! overrides so it reads the vendor's CURRENT configuration on every reload.

use std::{fs, path::Path, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::{boot_id, reload_ra, Fault, JOURNAL};
use crate::net_tools::{self as net, Cmd};

const STATE: &str = "ra-runtime.json";
const OWNER: &str = "u60_relay_ra";

#[derive(Serialize, Deserialize)]
struct Overlay {
    boot: String,
    values: Map<String, Value>,
    #[serde(default)]
    withdrawing: bool,
}

impl Overlay {
    fn released(&self, current: &Map<String, Value>) -> Map<String, Value> {
        let owner_present = current.get(OWNER) == self.values.get(OWNER);
        self.values
            .iter()
            .filter(|(key, value)| {
                current.get(*key) == Some(*value)
                    || (self.withdrawing
                        && owner_present
                        && key.as_str() == "ra_lifetime"
                        && current.get(*key) == Some(&json!(0)))
            })
            .map(|(key, _)| (key.clone(), Value::Null))
            .collect()
    }

    fn save(&self) -> Result<(), String> {
        net::write_private(
            &Path::new(JOURNAL).join(STATE),
            &serde_json::to_vec(self).unwrap(),
        )
    }
}

pub(super) fn advertised_mtu(upstream: u32, lan: u32) -> Result<u32, String> {
    if !(1280..=1500).contains(&upstream) || lan < 1280 {
        return Err("IPv6 relay MTU is outside the supported link range".into());
    }
    Ok(upstream.min(lan))
}

pub(super) fn lan_mtu() -> Result<u32, String> {
    fs::read_to_string("/sys/class/net/br-lan/mtu")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .ok_or_else(|| "Cannot read the LAN link MTU".into())
}

fn options(ula: &str, upstream_mtu: u32, lan_mtu: u32) -> Result<Map<String, Value>, String> {
    // A 1280-byte QUIC datagram needs 1328 bytes with IPv6/UDP headers.
    // Do not impose IPv6's minimum when both links support larger packets.
    let mtu = advertised_mtu(upstream_mtu, lan_mtu)?;
    Ok(json!({
        "ra": "server", "dhcpv6": "disabled", "ndp": "disabled",
        "ra_slaac": true, "ra_default": 2, "ra_lifetime": 90,
        "ra_mininterval": 10, "ra_maxinterval": 30, "ra_flags": ["none"],
        "ra_mtu": mtu, "ra_useleasetime": false, "prefix_filter": ula,
        // odhcpd APPENDS runtime DNS to UCI DNS. Do not advertise stale SIM
        // RDNSS: dual-stack clients use existing DHCPv4 DNS for A AND AAAA.
        "ra_dns": false
    })
    .as_object()
    .unwrap()
    .clone())
}

fn interface() -> Result<Value, String> {
    let raw = Cmd::new("ubus", &["call", "network.interface.lan", "status", "{}"]).run()?;
    let status: Value = serde_json::from_str(&raw).map_err(|_| "Invalid LAN runtime status")?;
    Ok(status)
}

pub(super) fn data() -> Result<Map<String, Value>, String> {
    let status = interface()?;
    status["data"]
        .as_object()
        .cloned()
        .ok_or_else(|| "LAN runtime data is unavailable".into())
}

fn set(values: &Map<String, Value>) -> Result<(), String> {
    Cmd::new(
        "ubus",
        &[
            "call",
            "network.interface.lan",
            "set_data",
            &Value::Object(values.clone()).to_string(),
        ],
    )
    .run()
    .map(|_| ())
}

fn read() -> Result<Option<Overlay>, String> {
    match fs::read(Path::new(JOURNAL).join(STATE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "Invalid runtime RA ownership journal".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("Cannot read runtime RA ownership journal".into()),
    }
}

fn absent(data: &Map<String, Value>, key: &str) -> bool {
    data.get(key).is_none_or(Value::is_null)
}

pub(super) fn apply(ula: &str, mtu: u32) -> Result<(), String> {
    if read()?.is_some() {
        return Err("Runtime RA cleanup is pending".into());
    }
    let mut values = options(ula, mtu, lan_mtu()?)?;
    values.insert(
        OWNER.into(),
        json!({"boot": boot_id(), "owner": "zte-agent"}),
    );
    let status = interface()?;
    if status["up"] != true || status["l3_device"] != "br-lan" {
        return Err("LAN runtime interface is not ready for IPv6 relay".into());
    }
    let current = data()?;
    if values.keys().any(|key| !absent(&current, key)) {
        return Err(
            "LAN runtime RA options are owned by another service; IPv4 relay remains active".into(),
        );
    }
    let overlay = Overlay {
        boot: boot_id(),
        values,
        withdrawing: false,
    };
    // Journal before effects; a crash between set_data and its reply is safe.
    overlay.save()?;
    set(&overlay.values)?;
    maintain().map_err(Fault::message)
}

/// A failed netifd query or write is `Unreadable` (retried); a missing or
/// stale journal or another writer's overlay is a `Conflict`.
pub(super) fn maintain() -> Result<(), Fault> {
    let overlay = read()
        .map_err(Fault::Conflict)?
        .ok_or_else(|| Fault::Conflict("Runtime RA ownership journal is missing".into()))?;
    if overlay.boot != boot_id() {
        return Err(Fault::Conflict(
            "Runtime RA belongs to a previous boot".into(),
        ));
    }
    let current = data().map_err(Fault::Unreadable)?;
    if overlay
        .values
        .iter()
        .all(|(key, value)| current.get(key) == Some(value))
    {
        return Ok(());
    }
    // A LAN/netifd reinitialization discards all runtime data. Rebuild only
    // from this session's live lease; never adopt someone else's overlay.
    if overlay.values.keys().all(|key| absent(&current, key)) {
        set(&overlay.values).map_err(Fault::Unreadable)?;
        let current = data().map_err(Fault::Unreadable)?;
        if overlay
            .values
            .iter()
            .all(|(key, value)| current.get(key) == Some(value))
        {
            return Ok(());
        }
    }
    Err(Fault::Conflict(
        "LAN runtime RA changed externally; relay IPv6 is being released".into(),
    ))
}

pub(super) fn release() -> Result<(), String> {
    let Some(mut overlay) = read()? else {
        return Ok(());
    };
    let path = Path::new(JOURNAL).join(STATE);
    if overlay.boot != boot_id() {
        // Runtime data cannot cross a reboot. Never touch fresh vendor state.
        return fs::remove_file(path).map_err(|e| e.to_string());
    }
    let mut current = data()?;
    if current
        .get(OWNER)
        .is_some_and(|v| !v.is_null() && Some(v) != overlay.values.get(OWNER))
    {
        return Err("Runtime RA owner changed; cleanup journal retained".into());
    }
    let owner_present = current.get(OWNER) == overlay.values.get(OWNER);
    if owner_present
        && (current.get("ra_lifetime") == overlay.values.get("ra_lifetime")
            || (overlay.withdrawing && current.get("ra_lifetime") == Some(&json!(0))))
    {
        // A preexisting external zero is not ours. Persist provenance before
        // our own withdrawal so a crash/retry can release only that zero.
        overlay.withdrawing = true;
        overlay.save()?;
        set(&json!({"ra_lifetime": 0}).as_object().unwrap().clone())?;
        reload_ra()?;
        std::thread::sleep(Duration::from_millis(1200));
        current = data()?;
    }
    let release = overlay.released(&current);
    if !release.is_empty() {
        set(&release)?;
        let current = data()?;
        if release.keys().any(|key| !absent(&current, key)) {
            return Err("Cannot release owned runtime RA options; retry cleanup".into());
        }
    }
    // Retry the notification even if a previous attempt cleared the keys but
    // failed to reload. Keep the journal until the daemon accepts the reload.
    reload_ra()?;
    fs::remove_file(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_zero_lifetime_survives_but_journaled_withdrawal_is_released() {
        let mut overlay = Overlay {
            boot: "test".into(),
            values: options("fd12::/64", 1280, 1500).unwrap(),
            withdrawing: false,
        };
        overlay
            .values
            .insert(OWNER.into(), json!({"boot":"test","owner":"zte-agent"}));
        let mut current = overlay.values.clone();
        current.insert("ra_lifetime".into(), json!(0));
        assert!(!overlay.released(&current).contains_key("ra_lifetime"));
        overlay.withdrawing = true;
        let recovered: Overlay =
            serde_json::from_slice(&serde_json::to_vec(&overlay).unwrap()).unwrap();
        assert_eq!(
            recovered.released(&current).get("ra_lifetime"),
            Some(&Value::Null)
        );
        current.insert("ra_lifetime".into(), json!(42));
        assert!(!recovered.released(&current).contains_key("ra_lifetime"));
    }

    #[test]
    fn typed_overlay_never_copies_upstream_or_vendor_dns() {
        let values = options("fd12:3456:789a::/64", 1280, 1500).unwrap();
        assert_eq!(values["prefix_filter"], "fd12:3456:789a::/64");
        assert_eq!(values["ra_dns"], false);
        assert_eq!(values["ra_slaac"], true);
        assert_eq!(values["ra_flags"], json!(["none"]));
        assert_eq!(values["ra_mtu"], 1280);
        assert!(!values.contains_key("dns"));
    }

    #[test]
    fn advertised_mtu_follows_upstream_and_fits_unfragmented_hysteria2() {
        let values = options("fd12::/64", 1480, 1500).unwrap();
        assert_eq!(values["ra_mtu"], 1480);
        assert!(values["ra_mtu"].as_u64().unwrap() >= 1280 + 40 + 8);
    }

    #[test]
    fn advertised_mtu_never_exceeds_either_link_or_ipv6_limits() {
        for (upstream, lan, expected) in [
            (1500, 1400, 1400),
            (1480, 1500, 1480),
            (1280, 1500, 1280),
            (1500, 1280, 1280),
            (1500, 9000, 1500),
        ] {
            assert_eq!(
                options("fd12::/64", upstream, lan).unwrap()["ra_mtu"],
                expected
            );
        }
        for (upstream, lan) in [(1279, 1500), (1501, 1500), (1480, 1279), (0, 1500)] {
            assert!(options("fd12::/64", upstream, lan).is_err());
        }
    }
}
