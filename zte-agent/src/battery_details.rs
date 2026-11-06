//! Additional read-only battery measurements, collected only for battery-page viewers.
use std::{fs, path::Path};

use crate::{device_ext, handlers::AppState, network_ext};
use serde_json::{json, Map, Value};

// Read measurements only; identifiers, writable controls and vendor debug files stay private.
const ATTRIBUTES: &[&str] = &[
    "type",
    "status",
    "health",
    "present",
    "online",
    "technology",
    "capacity",
    "capacity_level",
    "temp",
    "temp_ambient",
    "temp_min",
    "temp_max",
    "temp_alert_min",
    "temp_alert_max",
    "voltage_now",
    "voltage_avg",
    "voltage_ocv",
    "voltage_max",
    "voltage_min",
    "voltage_max_design",
    "voltage_min_design",
    "current_now",
    "current_avg",
    "current_max",
    "power_now",
    "power_avg",
    "charge_now",
    "charge_avg",
    "charge_counter",
    "charge_full",
    "charge_empty",
    "charge_full_design",
    "charge_empty_design",
    "energy_now",
    "energy_avg",
    "energy_full",
    "energy_empty",
    "energy_full_design",
    "energy_empty_design",
    "cycle_count",
    "charge_type",
    "time_to_full_now",
    "time_to_full_avg",
    "time_to_empty_now",
    "time_to_empty_avg",
    "usb_type",
    "input_current_limit",
    "input_voltage_limit",
    "input_power_limit",
    "constant_charge_current",
    "constant_charge_current_max",
    "constant_charge_voltage",
    "constant_charge_voltage_max",
    "charge_term_current",
];

fn supplies(base: &Path) -> std::io::Result<Value> {
    let mut result = Map::new();
    for supply in fs::read_dir(base)?.filter_map(Result::ok).take(16) {
        let mut fields = Map::new();
        for &name in ATTRIBUTES {
            let Ok(raw) = fs::read_to_string(supply.path().join(name)) else {
                continue;
            };
            let raw = raw.trim();
            if raw.is_empty() || raw.len() > 192 {
                continue;
            }
            let value = raw
                .parse::<i64>()
                .map_or_else(|_| Value::String(raw.to_owned()), Value::from);
            fields.insert(name.to_owned(), value);
        }
        // Some drivers expose an inconsistent power_now. V × I uses documented µV/µA units.
        if let (Some(voltage), Some(current)) = (
            fields
                .get("voltage_now")
                .and_then(Value::as_i64)
                .filter(|v| *v > 0),
            fields.get("current_now").and_then(Value::as_i64),
        ) {
            let watts = voltage as f64 * (current as f64).abs() / 1_000_000_000_000.0;
            fields.insert("calculated_power_w".into(), json!(watts));
        }
        if !fields.is_empty() {
            result.insert(
                supply.file_name().to_string_lossy().into_owned(),
                Value::Object(fields),
            );
        }
    }
    Ok(Value::Object(result))
}

pub(crate) fn snapshot(state: &AppState) -> Value {
    let sensors = match supplies(Path::new("/sys/class/power_supply")) {
        Ok(data) => json!({"ok":true,"data":data}),
        Err(_) => json!({"ok":false,"error":"Power-supply readings unavailable"}),
    };
    json!({"ok":true,"data":{
        "supplies":sensors,
        "firmware":network_ext::network_battery_ubus(state).1,
        "charger":device_ext::device_charger(state).1,
        "control":device_ext::charge_control_get(state).1,
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurements_preserve_zero_and_negative_values_without_exposing_identifiers() {
        let root = std::env::temp_dir().join(format!(
            "u60-battery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("battery")).unwrap();
        for (key, value) in [
            ("capacity", "0"),
            ("current_now", "-1000000"),
            ("voltage_now", "4000000"),
            ("power_now", "55522725"),
            ("cycle_count", "0"),
            ("time_to_empty_avg", "-1"),
            ("health", "Good"),
            ("serial_number", "private-identifier"),
        ] {
            fs::write(root.join("battery").join(key), value).unwrap();
        }
        let data = supplies(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_eq!(data["battery"]["calculated_power_w"], 4.0);
        assert_eq!(data["battery"]["capacity"], 0);
        assert_eq!(data["battery"]["cycle_count"], 0);
        assert_eq!(data["battery"]["time_to_empty_avg"], -1);
        assert!(data["battery"].get("serial_number").is_none());
        assert!(data["battery"].get("temp").is_none());
    }
}
