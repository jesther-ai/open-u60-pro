//! Read-only, on-demand client inventory and Wi-Fi counter sampling.
//! AP transmit counters are device downloads; receive counters are device uploads.
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const MAX_DEVICES: usize = 256;
const GAP: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct ClientMonitor {
    inner: Mutex<Monitor>,
}

#[derive(Default)]
struct Monitor {
    cached: Option<(Instant, Value)>,
    windows: BTreeMap<String, Window>,
    qos: Option<(Instant, Value)>,
}

struct Window {
    last: Instant,
    since: u128,
    interface: String,
    connected_seconds: Option<u64>,
    rx: Option<u64>,
    tx: Option<u64>,
    upload_peak: Option<f64>,
    download_peak: Option<f64>,
}

#[derive(Default, Debug)]
struct Station {
    mac: String,
    interface: String,
    rx: Option<u64>,
    tx: Option<u64>,
    signal: Option<f64>,
    upload_link_mbps: Option<f64>,
    download_link_mbps: Option<f64>,
    connected_seconds: Option<u64>,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn mac(value: &str) -> Option<String> {
    let parts: Vec<_> = value.split(':').collect();
    (parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_hexdigit())))
    .then(|| value.to_ascii_lowercase())
}

fn interface(value: &str) -> bool {
    !value.is_empty()
        && value.len() < 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

fn read(object: &str, method: &str, args: &str) -> Result<Value, String> {
    let output = Command::new("ubus")
        .args(["-t", "2", "call", object, method, args])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("The router could not read this client source".into());
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "The router returned no client data".into())
}

fn records(data: &Value, key: &str) -> Result<Vec<Value>, String> {
    data.get(key)
        .and_then(Value::as_array)
        .filter(|rows| rows.iter().all(Value::is_object))
        .cloned()
        .ok_or_else(|| "The router returned no usable client list".into())
}

fn wireless() -> Result<Vec<Value>, String> {
    let mut all = Vec::new();
    for page in 0..26 {
        let args = json!({"start_id":page * 10 + 1,"end_id":page * 10 + 10});
        let rows = records(
            &read(
                "zwrt_router.api",
                "router_wireless_access_list",
                &args.to_string(),
            )?,
            "wireless_access_list_info",
        )?;
        let done = rows.len() < 10;
        all.extend(rows);
        if all.len() > MAX_DEVICES {
            return Err("The client list exceeds the supported inventory size".into());
        }
        if done {
            return Ok(all);
        }
    }
    Err("The router client list is incomplete".into())
}

fn parse_stations(text: &str, iface: &str) -> Vec<Station> {
    let mut result = Vec::new();
    let mut current: Option<Station> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix("Station ") {
            if let Some(previous) = current.take() {
                result.push(previous);
            }
            current = header
                .split_whitespace()
                .next()
                .and_then(mac)
                .map(|mac| Station {
                    mac,
                    interface: iface.into(),
                    ..Station::default()
                });
            continue;
        }
        let Some(station) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let token = value.split_whitespace().next().unwrap_or("");
        let number = token.parse::<f64>().ok().filter(|value| value.is_finite());
        match key {
            "rx bytes" | "rx bytes64" => station.rx = token.parse().ok(),
            "tx bytes" | "tx bytes64" => station.tx = token.parse().ok(),
            "signal" => station.signal = number.filter(|value| (-127.0..=0.0).contains(value)),
            "connected time" => station.connected_seconds = token.parse().ok(),
            "rx bitrate" => station.upload_link_mbps = link_rate(value),
            "tx bitrate" => station.download_link_mbps = link_rate(value),
            _ => {}
        }
    }
    if let Some(previous) = current {
        result.push(previous);
    }
    result.truncate(MAX_DEVICES);
    result
}

fn link_rate(value: &str) -> Option<f64> {
    let mut words = value.split_whitespace();
    let rate = words.next()?.parse::<f64>().ok()?;
    let multiplier = match words.next()? {
        "MBit/s" => 1.0,
        "GBit/s" => 1000.0,
        "kBit/s" => 0.001,
        _ => return None,
    };
    (rate.is_finite() && rate >= 0.0).then_some(rate * multiplier)
}

fn own_macs() -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("address")).ok())
        .filter_map(|value| mac(value.trim()))
        .collect()
}

fn inventory(rows: &[Value], kind: &str, devices: &mut BTreeMap<String, Value>) {
    for row in rows {
        let Some(address) = row.get("mac_address").and_then(Value::as_str).and_then(mac) else {
            continue;
        };
        let get = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
        let iface = get("connected_dev");
        devices.insert(address.clone(), json!({
            "mac":address,"name":get("hostname"),"ipv4":get("ip_address"),"ipv6":get("ipv6_address"),
            "connected":true,"connection_type":if kind == "wired" && (iface.starts_with("ecm") || iface.starts_with("rndis") || iface.starts_with("usb")) {"usb"} else {kind},
            "band":get("interface_type"),"interface":iface,"address_source":get("address_source"),
            "vendor":get("vendor"),"device_type":get("devtype"),
            "connected_seconds":row.get("duration_time").and_then(Value::as_u64),
            "limits":{"upload_bps":null,"download_bps":null,"status":"not_reported","scope":"device"},
            "traffic":{"upload_bps":null,"download_bps":null,"upload_bytes":null,"download_bytes":null,"scope":null},
            "peaks":{"upload_bps":null,"download_bps":null,"since":null},"link":{},
        }));
    }
}

fn rate(previous: Option<u64>, current: Option<u64>, seconds: f64) -> Option<f64> {
    (seconds >= 0.25 && seconds <= GAP.as_secs_f64()).then_some(())?;
    Some(current?.checked_sub(previous?)? as f64 * 8.0 / seconds)
}

impl Monitor {
    fn sample(&mut self, station: &Station, now: Instant, at: u128) -> Value {
        let reset = self.windows.get(&station.mac).is_none_or(|window| {
            now.duration_since(window.last) > GAP
                || window.interface != station.interface
                || station
                    .rx
                    .zip(window.rx)
                    .is_some_and(|(current, old)| current < old)
                || station
                    .tx
                    .zip(window.tx)
                    .is_some_and(|(current, old)| current < old)
                || station
                    .connected_seconds
                    .zip(window.connected_seconds)
                    .is_some_and(|(current, old)| current < old)
        });
        if reset {
            self.windows.insert(
                station.mac.clone(),
                Window {
                    last: now,
                    since: at,
                    interface: station.interface.clone(),
                    rx: None,
                    tx: None,
                    connected_seconds: station.connected_seconds,
                    upload_peak: None,
                    download_peak: None,
                },
            );
        }
        let Some(window) = self.windows.get_mut(&station.mac) else {
            return Value::Null;
        };
        let elapsed = now.duration_since(window.last).as_secs_f64();
        let upload = rate(window.rx, station.rx, elapsed);
        let download = rate(window.tx, station.tx, elapsed);
        if let Some(value) = upload {
            window.upload_peak = Some(window.upload_peak.unwrap_or(value).max(value));
        }
        if let Some(value) = download {
            window.download_peak = Some(window.download_peak.unwrap_or(value).max(value));
        }
        window.last = now;
        window.rx = station.rx;
        window.tx = station.tx;
        window.connected_seconds = station.connected_seconds;
        json!({"traffic":{"upload_bps":upload,"download_bps":download,"upload_bytes":station.rx,"download_bytes":station.tx,"scope":"wifi_link"},
            "peaks":{"upload_bps":window.upload_peak,"download_bps":window.download_peak,"since":window.since}})
    }
}

fn router_limits(data: &Value) -> Value {
    // Unit codes verified against the vendor's shipped QoS view. These are router-wide.
    let decode = |direction: &str| -> Option<f64> {
        let raw = data.get(format!("{direction}_total_limit_rate"))?;
        let rate = raw.as_f64().or_else(|| raw.as_str()?.parse().ok())?;
        let unit = data.get(format!("{direction}_total_limit_unit"))?;
        let unit = unit.as_u64().or_else(|| unit.as_str()?.parse().ok())?;
        let multiplier = match unit {
            3 => 1_000_000.0,
            4 => 10_000_000.0,
            6 => 1_000_000_000.0,
            _ => return None,
        };
        (rate.is_finite() && rate > 0.0).then_some(rate * multiplier)
    };
    json!({"scope":"router","upload_bps":decode("upload"),"download_bps":decode("download")})
}

impl ClientMonitor {
    pub fn snapshot(&self) -> Value {
        let mut monitor = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((at, value)) = &monitor.cached {
            if at.elapsed() < Duration::from_millis(1800) {
                return value.clone();
            }
        }
        let wifi = wireless();
        let wired = read("zwrt_router.api", "router_lan_access_list", "{}")
            .and_then(|data| records(&data, "lan_access_list_info"));
        if wifi.is_err() && wired.is_err() {
            monitor.windows.clear();
            return json!({"ok":false,"error":"Connected-device readings are unavailable"});
        }
        let mut devices = BTreeMap::new();
        if let Ok(rows) = &wifi {
            inventory(rows, "wifi", &mut devices);
        }
        if let Ok(rows) = &wired {
            inventory(rows, "wired", &mut devices);
        }
        let interfaces: BTreeSet<String> = wifi
            .as_ref()
            .into_iter()
            .flatten()
            .filter_map(|row| {
                row.get("connected_dev")
                    .and_then(Value::as_str)
                    .filter(|value| interface(value))
                    .map(str::to_owned)
            })
            .collect();
        let mut station_sources = BTreeMap::new();
        let mut sampled_macs = BTreeSet::new();
        let at = now_ms();
        for iface in interfaces.iter().take(16) {
            let output = Command::new("iw")
                .args(["dev", iface, "station", "dump"])
                .output();
            let Ok(output) = output else {
                station_sources.insert(iface.clone(), false);
                continue;
            };
            if !output.status.success() {
                station_sources.insert(iface.clone(), false);
                continue;
            }
            station_sources.insert(iface.clone(), true);
            for station in parse_stations(&String::from_utf8_lossy(&output.stdout), iface) {
                // A MAC seen on two interfaces during roaming is sampled once, never summed.
                if !sampled_macs.insert(station.mac.clone()) {
                    continue;
                }
                let sample = monitor.sample(&station, Instant::now(), at);
                let Some(device) = devices.get_mut(&station.mac) else {
                    continue;
                };
                device["traffic"] = sample["traffic"].clone();
                device["peaks"] = sample["peaks"].clone();
                device["link"] = json!({"signal_dbm":station.signal,"upload_mbps":station.upload_link_mbps,"download_mbps":station.download_link_mbps});
                if let Some(seconds) = station.connected_seconds {
                    device["connected_seconds"] = json!(seconds);
                }
            }
        }
        monitor
            .windows
            .retain(|address, _| sampled_macs.contains(address));
        if monitor
            .qos
            .as_ref()
            .is_none_or(|(when, _)| when.elapsed() >= Duration::from_secs(15))
        {
            let data = read("zwrt_router.api", "router_get_qos", "{}").unwrap_or(Value::Null);
            monitor.qos = Some((Instant::now(), router_limits(&data)));
        }
        let value = json!({"ok":true,"data":{
            "sampled_at":at,"sample_interval_seconds":2,"inventory_complete":wifi.is_ok() && wired.is_ok(),
            "sources":{"wireless":wifi.is_ok(),"wired":wired.is_ok(),"stations":station_sources},
            "devices":devices.into_values().take(MAX_DEVICES).collect::<Vec<_>>(),"router_macs":own_macs(),
            "router_limits":monitor.qos.as_ref().map(|(_, data)|data),
            "device_limits_status":"not_reported",
        }});
        monitor.cached = Some((Instant::now(), value.clone()));
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "Station aa:bb:cc:dd:ee:01 (on wlan2)\n\trx bytes:\t1451008\n\ttx bytes:\t1900314\n\tsignal: -45 dBm\n\ttx bitrate:\t2161.3 MBit/s 160MHz HE-MCS 10 HE-NSS 2\n\trx bitrate:\t1921.5 MBit/s 160MHz EHT-MCS 9\n\tconnected time: 149 seconds\nStation aa:bb:cc:dd:ee:02 (on wlan2)\n\trx bytes: 0\n\ttx bytes: 9007199254740993\n\tsignal: -48 dBm\n\tconnected time: 156 seconds\n";

    #[test]
    fn station_parser_preserves_direction_missing_fields_and_wide_counters() {
        let rows = parse_stations(DUMP, "wlan2");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].download_link_mbps, Some(2161.3));
        assert_eq!(rows[0].upload_link_mbps, Some(1921.5));
        assert_eq!(rows[1].rx, Some(0));
        assert_eq!(rows[1].tx, Some(9007199254740993));
        assert_eq!(rows[1].download_link_mbps, None);
    }

    #[test]
    fn samples_compute_device_rates_and_keep_peaks_without_counting_link_rates() {
        let mut monitor = Monitor::default();
        let mut station = parse_stations(DUMP, "wlan2").remove(0);
        let now = Instant::now();
        let first = monitor.sample(&station, now, 1000);
        assert!(first["traffic"]["upload_bps"].is_null());
        station.rx = station.rx.map(|value| value + 250000);
        station.tx = station.tx.map(|value| value + 2500000);
        let second = monitor.sample(&station, now + Duration::from_secs(2), 3000);
        assert_eq!(second["traffic"]["upload_bps"], 1_000_000.0);
        assert_eq!(second["traffic"]["download_bps"], 10_000_000.0);
        assert_eq!(second["peaks"]["since"], 1000);
        let idle = monitor.sample(&station, now + Duration::from_secs(4), 5000);
        assert_eq!(idle["traffic"]["download_bps"], 0.0);
        assert_eq!(idle["peaks"]["download_bps"], 10_000_000.0);
    }

    #[test]
    fn counter_resets_roaming_and_monitoring_gaps_start_a_new_peak_window() {
        for scenario in ["reset", "roam", "gap", "reassociate"] {
            let mut monitor = Monitor::default();
            let mut station = parse_stations(DUMP, "wlan2").remove(0);
            let now = Instant::now();
            monitor.sample(&station, now, 1000);
            station.rx = station.rx.map(|value| value + 1000);
            station.tx = station.tx.map(|value| value + 2000);
            monitor.sample(&station, now + Duration::from_secs(2), 3000);
            match scenario {
                "reset" => station.tx = Some(0),
                "roam" => station.interface = "wlan1".into(),
                "reassociate" => station.connected_seconds = Some(1),
                _ => {}
            }
            let delay = if scenario == "gap" { 20 } else { 4 };
            let sample = monitor.sample(&station, now + Duration::from_secs(delay), 9000);
            assert!(sample["traffic"]["download_bps"].is_null(), "{scenario}");
            assert!(sample["peaks"]["download_bps"].is_null(), "{scenario}");
            assert_eq!(sample["peaks"]["since"], 9000, "{scenario}");
        }
    }

    #[test]
    fn missing_counters_do_not_create_zero_rates_or_bridge_a_missing_interval() {
        let mut monitor = Monitor::default();
        let mut station = parse_stations(DUMP, "wlan2").remove(0);
        let now = Instant::now();
        monitor.sample(&station, now, 1000);
        station.rx = None;
        assert!(
            monitor.sample(&station, now + Duration::from_secs(2), 3000)["traffic"]["upload_bps"]
                .is_null()
        );
        station.rx = Some(9999999);
        assert!(
            monitor.sample(&station, now + Duration::from_secs(4), 5000)["traffic"]["upload_bps"]
                .is_null()
        );
    }

    #[test]
    fn router_caps_use_verified_units_and_never_become_device_caps() {
        for (unit, multiplier) in [(3, 1_000_000.0), (4, 10_000_000.0), (6, 1_000_000_000.0)] {
            let result =
                router_limits(&json!({"upload_total_limit_rate":2,"upload_total_limit_unit":unit}));
            assert_eq!(result["upload_bps"], 2.0 * multiplier);
            assert_eq!(result["scope"], "router");
            assert!(result["download_bps"].is_null());
        }
        assert!(router_limits(&json!({}))["upload_bps"].is_null());
        assert!(
            router_limits(&json!({"upload_total_limit_rate":0,"upload_total_limit_unit":3}))
                ["upload_bps"]
                .is_null()
        );
        assert!(
            router_limits(&json!({"upload_total_limit_rate":12,"upload_total_limit_unit":99}))
                ["upload_bps"]
                .is_null()
        );
    }

    #[test]
    fn inventory_is_allowlisted_and_does_not_invent_traffic_or_limits() {
        let mut devices = BTreeMap::new();
        inventory(
            &[
                json!({"mac_address":"AA:BB:CC:DD:EE:01","hostname":"Phone","connected_dev":"wlan2","interface_type":"5G","password":"secret","future_private_field":"secret"}),
            ],
            "wifi",
            &mut devices,
        );
        let value = &devices["aa:bb:cc:dd:ee:01"];
        assert_eq!(value["connected"], true);
        assert!(value["traffic"]["download_bps"].is_null());
        assert!(value["limits"]["upload_bps"].is_null());
        assert!(!value.to_string().contains("secret"));
        assert!(records(&json!({}), "wireless_access_list_info").is_err());
    }
}
