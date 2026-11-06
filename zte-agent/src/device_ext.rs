use std::fs;

use serde_json::{json, Value};

use crate::cache;
use crate::handlers::AppState;
use crate::ubus;

pub fn device_thermal(_state: &AppState) -> (u16, Value) {
    match read_thermal_zones(std::path::Path::new("/sys/class/thermal")) {
        Ok(sensors) => {
            // Keep the legacy field for older clients, now using the hottest CPU zone.
            let cpu = sensors.iter()
                .filter(|s| s["name"].as_str().is_some_and(|n| n.starts_with("cpuss-")))
                .filter_map(|s| s["temperature_c"].as_f64())
                .reduce(f64::max);
            // A single device reading should represent the hottest valid physical zone,
            // rather than implying that one CPU sensor describes the whole router.
            let device = hottest_temperature(&sensors);
            (200, json!({"ok": true, "data": {
                "device_temp": device,
                "cpuss_temp": cpu,
                "sensors": sensors,
            }}))
        }
        Err(e) => (503, json!({"ok": false, "error": e.to_string()})),
    }
}

fn hottest_temperature(sensors: &[Value]) -> Option<f64> {
    sensors
        .iter()
        .filter_map(|sensor| sensor["temperature_c"].as_f64())
        .reduce(f64::max)
}

fn read_thermal_zones(base: &std::path::Path) -> std::io::Result<Vec<Value>> {
    let mut zones: Vec<_> = fs::read_dir(base)?.filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("thermal_zone"))
        .collect();
    zones.sort_by_key(|e| e.file_name().to_string_lossy()
        .trim_start_matches("thermal_zone").parse::<u32>().unwrap_or(u32::MAX));
    Ok(zones.into_iter().filter_map(|zone| {
        let name = fs::read_to_string(zone.path().join("type")).ok()?;
        let name = name.trim();
        // zeroc is a reference zone, not a physical temperature sensor.
        if name.is_empty() || name == "zeroc" { return None; }
        let raw = fs::read_to_string(zone.path().join("temp")).ok()
            .and_then(|v| v.trim().parse::<i64>().ok());
        // Observed disconnected sensors return -273000 or -40960 millidegrees.
        let temperature = raw.filter(|v| (-40000..=150000).contains(v))
            .map(|v| v as f64 / 1000.0);
        Some(json!({"id": zone.file_name().to_string_lossy(), "name": name,
            "temperature_c": temperature}))
    }).collect())
}


pub fn device_charger(state: &AppState) -> (u16, Value) {
    match state.cache.charger_list(cache::CHARGER_TTL) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// `ubus call system info` without the fork: the same numbers procd takes
/// from `sysinfo(2)` and `/proc/meminfo`, in the same shape. Falls back to
/// the CLI only if `/proc` cannot be read or parsed.
pub fn device_system(_state: &AppState) -> (u16, Value) {
    let read = |path: &str| fs::read_to_string(path).ok();
    let synthesized = read("/proc/uptime")
        .zip(read("/proc/loadavg"))
        .zip(read("/proc/meminfo"))
        .and_then(|((uptime, loadavg), meminfo)| {
            system_info_from_proc(&uptime, &loadavg, &meminfo, local_epoch_now())
        });
    let result = match synthesized {
        Some(data) => Ok(data),
        None => ubus::call("system", "info", Some("{}")),
    };
    match result {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// Build procd's `system info` reply from `/proc/uptime`, `/proc/loadavg` and
/// `/proc/meminfo` text. `localtime` is `time(NULL) + tm_gmtoff`, as procd
/// reports it.
///
/// - `uptime`: `sysinfo(2)` rounds a fractional second up, so `203076.12`
///   reports as 203077.
/// - `load`: `sysinfo` exposes the averages as fixed point x65536; the text
///   file has two decimals, so the integers are the nearest representable.
/// - memory and swap: kB fields x1024, the same kernel counters `sysinfo`
///   returns (`Shmem` is `sharedram`, `Buffers` is `bufferram`).
fn system_info_from_proc(
    uptime: &str,
    loadavg: &str,
    meminfo: &str,
    localtime: u64,
) -> Option<Value> {
    let (secs, frac) = uptime.split_whitespace().next()?.split_once('.')?;
    let uptime = secs.parse::<u64>().ok()? + u64::from(frac.bytes().any(|b| b != b'0'));

    let load = loadavg
        .split_whitespace()
        .take(3)
        .map(|s| s.parse::<f64>().ok().map(|f| (f * 65536.0).round() as u64))
        .collect::<Option<Vec<u64>>>()?;
    if load.len() != 3 {
        return None;
    }

    let mem = |key: &str| -> Option<u64> {
        meminfo.lines().find_map(|line| {
            let (name, rest) = line.split_once(':')?;
            if name != key {
                return None;
            }
            rest.split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()?
                .checked_mul(1024)
        })
    };

    Some(json!({
        "localtime": localtime,
        "uptime": uptime,
        "load": load,
        "memory": {
            "total": mem("MemTotal")?,
            "free": mem("MemFree")?,
            "shared": mem("Shmem")?,
            "buffered": mem("Buffers")?,
            "available": mem("MemAvailable")?,
            "cached": mem("Cached")?,
        },
        "swap": {
            "total": mem("SwapTotal")?,
            "free": mem("SwapFree")?,
        },
    }))
}

/// `time(NULL) + tm_gmtoff`: seconds since the epoch shifted into the local
/// zone, which is what procd's `localtime` field carries. `time_t` and
/// `c_long` are both `i64` on every LP64 target this crate builds for.
fn local_epoch_now() -> u64 {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let gmtoff = if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        0
    } else {
        tm.tm_gmtoff
    };
    (now + gmtoff).max(0) as u64
}

pub fn device_reboot(_state: &AppState) -> (u16, Value) {
    match ubus::call("system", "reboot", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

// factory_reset is ZTE-specific (zwrt_bsp.power) — may require re-enabling that daemon
pub fn device_factory_reset(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_bsp.power", "factory_reset", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn charge_control_get(state: &AppState) -> (u16, Value) {
    let read_sysfs = |path: &str| -> String {
        fs::read_to_string(path)
            .unwrap_or_default()
            .trim()
            .to_string()
    };

    let battery_status = read_sysfs("/sys/class/power_supply/battery/status");
    let capacity: i64 = read_sysfs("/sys/class/power_supply/battery/capacity")
        .parse()
        .unwrap_or(0);

    let charging_stopped = state
        .cache
        .charger_list(cache::CHARGER_TTL)
        .ok()
        .and_then(|v| {
            v["direct_power_supply_mode"]
                .as_str()
                .map(|s| s == "enable")
        })
        .unwrap_or(false);

    let (limit_enabled, limit_pct, hysteresis, manual_override) = state.charge_limit.get();

    (
        200,
        json!({
            "ok": true,
            "data": {
                "charging_stopped": charging_stopped,
                "battery_status": battery_status,
                "capacity": capacity,
                "charge_limit_enabled": limit_enabled,
                "charge_limit": limit_pct,
                "hysteresis": hysteresis,
                "manual_override": manual_override,
            }
        }),
    )
}

pub fn charge_control_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    // Manual charge stop/resume via ubus (inverted: "enable" = stop, "disable" = start)
    if let Some(stopped) = parsed["charging_stopped"].as_bool() {
        let mode = if stopped { "enable" } else { "disable" };
        let params = format!(r#"{{"direct_power_supply_mode":"{mode}"}}"#);
        let result = ubus::call("zwrt_bsp.charger", "set", Some(&params));
        // The trailing charge_control_get must reflect this write, not the cache.
        state.cache.invalidate(cache::CHARGER);
        if let Err(e) = result {
            return (
                500,
                json!({"ok": false, "error": format!("charger ubus: {e}")}),
            );
        }
        // Set manual override so enforcer doesn't fight the user
        state.charge_limit.set_manual_override(stopped);
    }

    // Charge limit settings
    if parsed.get("charge_limit_enabled").is_some()
        || parsed.get("charge_limit").is_some()
        || parsed.get("hysteresis").is_some()
    {
        let (cur_enabled, cur_limit, cur_hysteresis, _) = state.charge_limit.get();
        let enabled = parsed["charge_limit_enabled"].as_bool().unwrap_or(cur_enabled);
        let limit = parsed["charge_limit"]
            .as_u64()
            .map(|v| v as u8)
            .unwrap_or(cur_limit);
        let hysteresis = parsed["hysteresis"]
            .as_u64()
            .map(|v| v as u8)
            .unwrap_or(cur_hysteresis);

        if let Err(e) = state.charge_limit.set(enabled, limit, hysteresis) {
            return (400, json!({"ok": false, "error": e}));
        }
        // `set` stops or resumes charging through its own ubus call, so the
        // trailing charge_control_get must not answer from the cache.
        state.cache.invalidate(cache::CHARGER);
    }

    // Return updated state
    charge_control_get(state)
}

pub fn device_power_save_get(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call(
        "zwrt_mc.device.manager",
        "get_device_info",
        Some(&parsed.to_string()),
    ) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn device_power_save_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call(
        "zwrt_mc.device.manager",
        "set_device_info",
        Some(&parsed.to_string()),
    ) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn device_fast_boot_get(_state: &AppState) -> (u16, Value) {
    let params = r#"{"deviceInfoList":["quicken_power_on"]}"#;
    match ubus::call("zwrt_mc.device.manager", "get_device_info", Some(params)) {
        Ok(data) => {
            let val = data["quicken_power_on"].as_str().unwrap_or("0");
            (200, json!({"ok": true, "data": {"fast_boot": val}}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn device_fast_boot_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let val = match parsed["fast_boot"].as_str() {
        Some(v @ ("0" | "1")) => v,
        _ => return (400, json!({"ok": false, "error": "fast_boot must be \"0\" or \"1\""})),
    };
    let params = format!(r#"{{"deviceInfoList":{{"quicken_power_on":"{val}"}}}}"#);
    match ubus::call("zwrt_mc.device.manager", "set_device_info", Some(&params)) {
        Ok(_) => (200, json!({"ok": true, "data": {"fast_boot": val}})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn device_auto_sleep_get(_state: &AppState) -> (u16, Value) {
    let timeout = ubus::uci_get("zwrt_sleep.ztmp_time.SysIdTime").unwrap_or_default();
    let switch = ubus::uci_get("zwrt_sleep.ztmp_switch.sleepSwitch").unwrap_or_default();
    let status = ubus::uci_get("zwrt_sleep.ztmp_status.sleepStatus").unwrap_or_default();
    (
        200,
        json!({
            "ok": true,
            "data": {
                "enabled": switch == "1",
                "timeout": timeout,
                "status": status,
            }
        }),
    )
}

pub fn device_auto_sleep_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    if let Some(enabled) = parsed["enabled"].as_bool() {
        let params = format!(r#"{{"switch":{}}}"#, enabled);
        if let Err(e) = ubus::call("zwrt_zte_sleep_faw.wakelock", "enableAutoSleep", Some(&params))
        {
            return (503, json!({"ok": false, "error": format!("enableAutoSleep: {e}")}));
        }
        // Persist to UCI so it survives reboots
        let val = if enabled { "1" } else { "0" };
        let _ = ubus::uci_set("zwrt_sleep.ztmp_switch.sleepSwitch", val);
    }

    if let Some(timeout) = parsed["timeout"].as_str() {
        let valid = timeout == "-1"
            || timeout
                .parse::<u32>()
                .map(|v| (1..=1440).contains(&v))
                .unwrap_or(false);
        if !valid {
            return (
                400,
                json!({"ok": false, "error": "timeout must be -1 or 1–1440 (minutes)"}),
            );
        }
        let params = format!(r#"{{"ufiSleepTime":"{timeout}"}}"#);
        if let Err(e) = ubus::call("zwrt_zte_sleep_faw.wakelock", "set_ufi_sleep", Some(&params)) {
            return (503, json!({"ok": false, "error": format!("set_ufi_sleep: {e}")}));
        }
    }

    device_auto_sleep_get(_state)
}

#[cfg(test)]
mod system_info_tests {
    use super::*;

    const UPTIME: &str = "203076.12 812345.67\n";
    const LOADAVG: &str = "0.52 0.58 0.59 1/160 12345\n";
    const MEMINFO: &str = "\
MemTotal:         463244 kB
MemFree:           91664 kB
MemAvailable:     266476 kB
Buffers:           11740 kB
Cached:           164512 kB
SwapCached:            0 kB
Active:            95988 kB
Shmem:              4172 kB
SwapTotal:             0 kB
SwapFree:              0 kB
";

    #[test]
    fn matches_procd_shape_and_units() {
        let info = system_info_from_proc(UPTIME, LOADAVG, MEMINFO, 1_760_000_000).unwrap();
        assert_eq!(
            info,
            json!({
                "localtime": 1_760_000_000u64,
                "uptime": 203077u64,
                "load": [34079u64, 38011u64, 38666u64],
                "memory": {
                    "total": 474_361_856u64,
                    "free": 93_863_936u64,
                    "shared": 4_272_128u64,
                    "buffered": 12_021_760u64,
                    "available": 272_871_424u64,
                    "cached": 168_460_288u64,
                },
                "swap": {"total": 0u64, "free": 0u64},
            })
        );
    }

    #[test]
    fn whole_second_uptime_is_not_rounded_up() {
        let info = system_info_from_proc("42.00 80.00\n", LOADAVG, MEMINFO, 0).unwrap();
        assert_eq!(info["uptime"], 42);
    }

    #[test]
    fn any_missing_field_falls_back() {
        assert!(system_info_from_proc("", LOADAVG, MEMINFO, 0).is_none());
        assert!(system_info_from_proc(UPTIME, "0.52 0.58\n", MEMINFO, 0).is_none());
        let no_available = MEMINFO.replace("MemAvailable", "MemElsewhere");
        assert!(system_info_from_proc(UPTIME, LOADAVG, &no_available, 0).is_none());
    }

    #[test]
    fn local_epoch_is_near_now() {
        let utc = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let local = local_epoch_now();
        // Within +-14 h of UTC (the widest real zone offsets) and monotone with it.
        assert!(
            local.abs_diff(utc) <= 14 * 3600 + 5,
            "local {local} vs utc {utc}"
        );
    }
}

#[cfg(test)]
mod thermal_tests {
    use super::*;

    #[test]
    fn reads_fresh_values_and_marks_disconnected_sensors_unavailable() {
        let base = std::env::temp_dir().join(format!("u60-thermal-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        for (id, name, temp) in [(16, "cpuss-0", "36800"), (3, "mmw1", "-273000"),
            (33, "sys-therm-5", "-40960"), (27, "zeroc", "0"), (39, "battery", "27000")] {
            let zone = base.join(format!("thermal_zone{id}"));
            fs::create_dir_all(&zone).unwrap();
            fs::write(zone.join("type"), name).unwrap();
            fs::write(zone.join("temp"), temp).unwrap();
        }
        let first = read_thermal_zones(&base).unwrap();
        assert_eq!(first.len(), 4);
        assert!(first[0]["temperature_c"].is_null());
        assert_eq!(first[1]["temperature_c"], 36.8);
        assert!(first[2]["temperature_c"].is_null());
        assert_eq!(hottest_temperature(&first), Some(36.8));
        fs::write(base.join("thermal_zone16/temp"), "42100").unwrap();
        let second = read_thermal_zones(&base).unwrap();
        assert_eq!(second[1]["temperature_c"], 42.1);
        assert_eq!(hottest_temperature(&second), Some(42.1));
        fs::remove_dir_all(base).unwrap();
    }
}
