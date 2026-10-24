//! Formatting and small interpretations of device data, ported from the iOS
//! app so both show the same text (`DeviceModels.swift`,
//! `SignalActivityMode.swift`, `ColorExtensions.swift`, `BatteryCardView.swift`).

use serde_json::Value;

use crate::live_activity;

/// An integer from a JSON number or a numeric string (the firmware uses both).
pub fn int(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
        .or_else(|| v.as_str()?.trim().parse().ok())
}

/// A float from a JSON number or a numeric string.
pub fn float(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()).filter(|f: &f64| f.is_finite())
}

fn round_to(v: f64, decimals: i32) -> f64 {
    let f = 10f64.powi(decimals);
    (v * f).round() / f
}

/// Bytes per second as bits per second in decimal steps, always one decimal;
/// a value that rounds up to 1000 is shown in the next unit.
pub fn rate(bytes_per_sec: f64) -> String {
    const UNITS: [&str; 4] = ["b/s", "Kb/s", "Mb/s", "Gb/s"];
    let mut raw = bytes_per_sec.max(0.0) * 8.0;
    let mut i = 0;
    while raw >= 1000.0 && i < UNITS.len() - 1 {
        raw /= 1000.0;
        i += 1;
    }
    let mut n = round_to(raw, 1);
    if n >= 1000.0 && i < UNITS.len() - 1 {
        i += 1;
        n = round_to(n / 1000.0, 1);
    }
    format!("{n:.1} {}", UNITS[i])
}

/// A byte count in binary steps with 2 / 1 / 0 decimals below 10 / 100 / above.
pub fn bytes(total: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut raw = total as f64;
    let mut i = 0;
    while raw >= 1024.0 && i < UNITS.len() - 1 {
        raw /= 1024.0;
        i += 1;
    }
    let dp = if raw < 10.0 {
        2
    } else if raw < 100.0 {
        1
    } else {
        0
    };
    format!("{:.*} {}", dp, round_to(raw, dp as i32), UNITS[i])
}

/// Minutes as "Xd Yh Zm", "Xh Ym" or "Xm".
pub fn eta(minutes: i64) -> String {
    let m = minutes.max(0);
    if m >= 1440 {
        format!("{}d {}h {}m", m / 1440, (m % 1440) / 60, m % 60)
    } else if m >= 60 {
        format!("{}h {}m", m / 60, m % 60)
    } else {
        format!("{m}m")
    }
}

/// Memory as the app's CPU & Memory sheet shows it: "x.x GB" from 1 GiB,
/// "N MB" from 1 MiB, else bytes.
pub fn memory(bytes: u64) -> String {
    const MIB: f64 = 1_048_576.0;
    let b = bytes as f64;
    if b >= 1024.0 * MIB {
        format!("{:.1} GB", b / (1024.0 * MIB))
    } else if b >= MIB {
        format!("{:.0} MB", b / MIB)
    } else {
        format!("{bytes} B")
    }
}

/// RSRP of `key` if it is a plausible reading (-160..=-20 dBm, the Live
/// Activity's filter).
pub fn rsrp(data: &Value, key: &str) -> Option<f64> {
    live_activity::number(data, key, -160.0, -20.0)
}

pub fn quality(rsrp: f64) -> &'static str {
    if rsrp >= -80.0 {
        "Excellent"
    } else if rsrp >= -100.0 {
        "Good"
    } else if rsrp >= -110.0 {
        "Fair"
    } else {
        "Poor"
    }
}

pub fn quality_class(rsrp: f64) -> &'static str {
    if rsrp >= -100.0 {
        "q-good"
    } else if rsrp >= -110.0 {
        "q-fair"
    } else {
        "q-poor"
    }
}

/// Standalone 5G from the firmware's `network_type` ("SA", "NR5G-SA",
/// "NR5G_SA", …; never from residual signal values). Limited service on
/// SA is not SA.
pub fn is_sa(signal: &Value) -> bool {
    let words = signal["network_type"].as_str().unwrap_or("").trim().to_ascii_uppercase().replace(['_', '-'], " ");
    !words.contains("LIMITED") && (words == "SA" || words.ends_with(" SA"))
}

/// At most `max` characters, with "…" when cut.
pub fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}\u{2026}", &s[..i]),
        None => s.to_owned(),
    }
}

/// "26.0 °C".
pub fn celsius(v: f64) -> String {
    format!("{v:.1} \u{b0}C")
}

/// A temperature in °C if it is plausible (-40..=150).
pub fn valid_celsius(v: Option<f64>) -> Option<f64> {
    v.filter(|t| (-40.0..=150.0).contains(t))
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Charge {
    Charging,
    Stopped,
    Discharging,
    Unknown,
}

/// The kernel's battery status, overridden by the charger when its data is
/// known: connected with direct power supply on means charging is stopped.
pub fn charge(battery: Option<&Value>, charger: Option<&Value>) -> Charge {
    let mut state = match battery.and_then(|b| b["status"].as_str()) {
        Some("Charging" | "Full") => Charge::Charging,
        Some("Not charging") => Charge::Stopped,
        Some("Discharging") => Charge::Discharging,
        _ => Charge::Unknown,
    };
    if let Some(c) = charger.filter(|c| c.is_object()) {
        let connected = int(&c["charger_connect"]) == Some(1);
        state = if connected && c["direct_power_supply_mode"].as_str() == Some("enable") {
            Charge::Stopped
        } else if int(&c["charge_status"]) == Some(1) {
            Charge::Charging
        } else {
            Charge::Discharging
        };
    }
    state
}

/// Battery percentage if it is 0-100.
pub fn capacity(battery: &Value) -> Option<i64> {
    int(&battery["capacity"]).filter(|c| (0..=100).contains(c))
}

/// The status label and its colour class.
pub fn charge_label(state: Charge, capacity: Option<i64>) -> (&'static str, &'static str) {
    match state {
        Charge::Stopped => ("Charge Stopped", "c-orange"),
        Charge::Charging if capacity == Some(100) => ("Full", "c-green"),
        Charge::Charging => ("Charging", "c-green"),
        Charge::Discharging => ("Discharging", level_class(capacity)),
        Charge::Unknown => ("Unavailable", "muted"),
    }
}

/// The battery card's status line and its colour class (iOS
/// `batteryStatusText`): with the current known, charging is just
/// "Charging" and discharging the time left without "left"; the time to
/// full only shows when the current is not known.
pub fn status_line(
    state: Charge,
    capacity: Option<i64>,
    current_known: bool,
    to_full: Option<i64>,
    to_empty: Option<i64>,
) -> (String, &'static str) {
    let (label, class) = charge_label(state, capacity);
    let text = match (state, to_full.filter(|m| *m > 0), to_empty.filter(|m| *m > 0)) {
        (Charge::Charging, Some(m), _) if capacity != Some(100) && !current_known => format!("Charging \u{b7} {}", eta(m)),
        (Charge::Discharging, _, Some(m)) if current_known => eta(m),
        (Charge::Discharging, _, Some(m)) => format!("{} left", eta(m)),
        _ => label.to_owned(),
    };
    (text, class)
}

/// After the status line: "25.2W" (no space, as the app's card) when
/// voltage and current are known, else the signed current ("+120mA").
pub fn power(battery: &Value) -> Option<String> {
    let ma = int(&battery["current_ua"])? / 1000;
    Some(match watts(battery) {
        Some(w) => format!("{w:.1}W"),
        None => format!("{ma:+}mA"),
    })
}

/// The hottest CPU zone (`cpuss-*` sensors), else `cpuss_temp` (iOS
/// `parseThermal`); plausible readings only.
pub fn cpu_temp(thermal: &Value) -> Option<f64> {
    let zones = thermal["sensors"].as_array().into_iter().flatten().filter(|s| {
        s["name"].as_str().is_some_and(|n| n.starts_with("cpuss-"))
    });
    let hottest = zones.filter_map(|s| valid_celsius(float(&s["temperature_c"]))).reduce(f64::max);
    hottest.or_else(|| valid_celsius(float(&thermal["cpuss_temp"])))
}

/// Battery level colour: 50 % and up green, 20 % and up orange, else red.
pub fn level_class(capacity: Option<i64>) -> &'static str {
    match capacity {
        Some(c) if c >= 50 => "c-green",
        Some(c) if c >= 20 => "c-orange",
        Some(_) => "c-red",
        None => "muted",
    }
}

/// Battery temperature (sysfs tenths of a degree).
pub fn battery_celsius(battery: &Value) -> Option<f64> {
    valid_celsius(float(&battery["temperature"]).map(|t| t / 10.0))
}

/// Power drawn or supplied: mV × |mA| / 1e6.
pub fn watts(battery: &Value) -> Option<f64> {
    let mv = int(&battery["voltage_uv"])? / 1000;
    let ma = int(&battery["current_ua"])? / 1000;
    Some(mv as f64 * ma.unsigned_abs() as f64 / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rates_promote_when_rounding_reaches_1000() {
        assert_eq!(rate(52.0), "416.0 b/s");
        assert_eq!(rate(0.0), "0.0 b/s");
        assert_eq!(rate(124.99), "999.9 b/s");
        // 999.95 Kb/s rounds to 1000.0 Kb/s: shown as 1.0 Mb/s.
        assert_eq!(rate(999_950.0 / 8.0), "1.0 Mb/s");
        assert_eq!(rate(999_940.0 / 8.0), "999.9 Kb/s");
        assert_eq!(rate(24_750_000.0), "198.0 Mb/s");
        assert_eq!(rate(2e12), "16000.0 Gb/s");
        assert_eq!(rate(-5.0), "0.0 b/s");
    }

    #[test]
    fn byte_totals_use_adaptive_decimals() {
        assert_eq!(bytes(0), "0.00 B");
        assert_eq!(bytes(119), "119 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.00 KB");
        assert_eq!(bytes(121_856), "119 KB");
        assert_eq!(bytes(1_992_294), "1.90 MB");
        assert_eq!(bytes(52_428_800), "50.0 MB");
        assert_eq!(bytes(5 * 1024u64.pow(4)), "5.00 TB");
    }

    #[test]
    fn eta_formats() {
        assert_eq!(eta(37), "37m");
        assert_eq!(eta(60), "1h 0m");
        assert_eq!(eta(1240), "20h 40m");
        assert_eq!(eta(1440), "1d 0h 0m");
        assert_eq!(eta(3001), "2d 2h 1m");
        assert_eq!(eta(-3), "0m");
    }

    #[test]
    fn memory_follows_the_app() {
        assert_eq!(memory(3_221_225_472), "3.0 GB");
        assert_eq!(memory(1_073_741_824), "1.0 GB");
        assert_eq!(memory(1_073_741_823), "1024 MB");
        assert_eq!(memory(424_673_280), "405 MB");
        assert_eq!(memory(1_048_575), "1048575 B");
        assert_eq!(memory(0), "0 B");
    }

    #[test]
    fn rsrp_validity_and_quality_boundaries() {
        let d = json!({"a": "-77", "b": -160, "c": -161, "d": "--", "e": 0, "f": -20.0, "g": -19.9});
        assert_eq!(rsrp(&d, "a"), Some(-77.0));
        assert_eq!(rsrp(&d, "b"), Some(-160.0));
        for k in ["c", "d", "e", "g", "missing"] {
            assert_eq!(rsrp(&d, k), None, "{k}");
        }
        assert_eq!(rsrp(&d, "f"), Some(-20.0));
        let cases = [
            (-80.0, "Excellent", "q-good"),
            (-80.1, "Good", "q-good"),
            (-100.0, "Good", "q-good"),
            (-100.1, "Fair", "q-fair"),
            (-110.0, "Fair", "q-fair"),
            (-110.1, "Poor", "q-poor"),
        ];
        for (v, q, c) in cases {
            assert_eq!((quality(v), quality_class(v)), (q, c), "{v}");
        }
    }

    #[test]
    fn sa_from_the_network_type() {
        let sa = |t: &str| is_sa(&json!({"network_type": t}));
        for t in ["SA", "NR5G-SA", "NR5G_SA", " sa "] {
            assert!(sa(t), "{t}");
        }
        for t in ["ENDC", "NR5G_NSA", "LTE", "LIMITED_SERVICE_SA", "NO_SERVICE", "", "WCDMA"] {
            assert!(!sa(t), "{t}");
        }
        assert!(!is_sa(&json!({})));
    }

    #[test]
    fn battery_status_and_charger_override() {
        let b = |status: &str| json!({"status": status, "capacity": 69});
        assert_eq!(charge(Some(&b("Charging")), None), Charge::Charging);
        assert_eq!(charge(Some(&b("Full")), None), Charge::Charging);
        assert_eq!(charge(Some(&b("Not charging")), None), Charge::Stopped);
        assert_eq!(charge(Some(&b("Discharging")), None), Charge::Discharging);
        assert_eq!(charge(Some(&b("Unknown")), None), Charge::Unknown);
        assert_eq!(charge(None, None), Charge::Unknown);
        let stopped = json!({"charger_connect": "1", "direct_power_supply_mode": "enable", "charge_status": 1});
        assert_eq!(charge(Some(&b("Charging")), Some(&stopped)), Charge::Stopped);
        let charging = json!({"charger_connect": 1, "direct_power_supply_mode": "disable", "charge_status": "1"});
        assert_eq!(charge(Some(&b("Discharging")), Some(&charging)), Charge::Charging);
        let unplugged = json!({"charger_connect": 0, "charge_status": 0});
        assert_eq!(charge(Some(&b("Charging")), Some(&unplugged)), Charge::Discharging);
        assert_eq!(charge_label(Charge::Charging, Some(100)), ("Full", "c-green"));
        assert_eq!(charge_label(Charge::Charging, Some(99)), ("Charging", "c-green"));
        assert_eq!(charge_label(Charge::Stopped, Some(10)), ("Charge Stopped", "c-orange"));
        assert_eq!(charge_label(Charge::Discharging, Some(50)).1, "c-green");
        assert_eq!(charge_label(Charge::Discharging, Some(20)).1, "c-orange");
        assert_eq!(charge_label(Charge::Discharging, Some(19)).1, "c-red");
        assert_eq!(charge_label(Charge::Unknown, None).0, "Unavailable");
    }

    #[test]
    fn battery_numbers() {
        let b = json!({"capacity": 101, "temperature": 260, "voltage_uv": 4_435_000, "current_ua": -5_680_000});
        assert_eq!(capacity(&b), None);
        assert_eq!(capacity(&json!({"capacity": "69"})), Some(69));
        assert_eq!(battery_celsius(&b).map(celsius).as_deref(), Some("26.0 \u{b0}C"));
        assert_eq!(battery_celsius(&json!({"temperature": 1600})), None);
        assert_eq!(format!("{:.1}", watts(&b).unwrap()), "25.2");
        assert_eq!(watts(&json!({"voltage_uv": 4_000_000})), None);
    }

    #[test]
    fn battery_status_line_follows_the_app() {
        let line = |state, cap, current, full, empty| status_line(state, cap, current, full, empty);
        assert_eq!(line(Charge::Stopped, Some(80), true, Some(30), None), ("Charge Stopped".into(), "c-orange"));
        assert_eq!(line(Charge::Charging, Some(100), false, Some(30), None), ("Full".into(), "c-green"));
        // With the current known (this device): no time to full.
        assert_eq!(line(Charge::Charging, Some(69), true, Some(65), None), ("Charging".into(), "c-green"));
        assert_eq!(line(Charge::Charging, Some(69), false, Some(65), None), ("Charging \u{b7} 1h 5m".into(), "c-green"));
        assert_eq!(line(Charge::Charging, Some(69), false, Some(0), None), ("Charging".into(), "c-green"));
        assert_eq!(line(Charge::Unknown, None, true, None, None), ("Unavailable".into(), "muted"));
        assert_eq!(line(Charge::Discharging, Some(69), true, None, Some(1240)), ("20h 40m".into(), "c-green"));
        assert_eq!(line(Charge::Discharging, Some(30), false, None, Some(90)), ("1h 30m left".into(), "c-orange"));
        assert_eq!(line(Charge::Discharging, Some(10), true, None, Some(-1)), ("Discharging".into(), "c-red"));
        let b = |mv: Option<i64>, ua: Option<i64>| power(&json!({"voltage_uv": mv, "current_ua": ua}));
        assert_eq!(b(Some(4_435_000), Some(5_680_000)).as_deref(), Some("25.2W"));
        assert_eq!(b(Some(3_900_000), Some(-480_000)).as_deref(), Some("1.9W"));
        assert_eq!(b(None, Some(120_000)).as_deref(), Some("+120mA"));
        assert_eq!(b(None, Some(-45_000)).as_deref(), Some("-45mA"));
        assert_eq!(b(Some(4_000_000), None), None);
    }

    #[test]
    fn hottest_cpu_zone() {
        let t = json!({"cpuss_temp": 37.2, "sensors": [
            {"name": "cpuss-0", "temperature_c": 40.1}, {"name": "cpuss-1", "temperature_c": 41.5},
            {"name": "cpuss-2", "temperature_c": 999}, {"name": "battery", "temperature_c": 50},
        ]});
        assert_eq!(cpu_temp(&t), Some(41.5));
        assert_eq!(cpu_temp(&json!({"cpuss_temp": "37.2", "sensors": []})), Some(37.2));
        assert_eq!(cpu_temp(&json!({"cpuss_temp": 200})), None);
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("abcdef", 3), "abc\u{2026}");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("ééé", 2), "éé\u{2026}");
    }
}
