use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::cache;
use crate::handlers::AppState;
use crate::system::{self, BatteryInfo};
use crate::ubus;

pub(super) const STATUS_MAX_AGE: Duration = Duration::from_secs(5);
const MIN_BATTERY_PERCENT: i64 = 30;
const MIN_SIGNAL_DBM: f64 = -125.0;

#[derive(Debug, PartialEq, Serialize)]
pub(super) struct InstallReadiness {
    pub ready: bool,
    pub reason: Option<String>,
    pub minimum_battery_percent: i64,
    pub battery_percent: Option<i64>,
    pub external_power: bool,
    pub network_type: Option<String>,
    pub signal_bars: Option<u8>,
    pub signal_dbm: Option<f64>,
    pub minimum_signal_dbm: f64,
    pub wan_ready: bool,
}

impl InstallReadiness {
    pub(super) fn blocking_reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

pub(super) fn read(state: &AppState, max_age: Duration) -> InstallReadiness {
    let battery = system::read_battery();
    let netinfo = state.cache.netinfo(max_age).ok();
    let wan = state
        .cache
        .get_or_fetch(cache::WAN, max_age, || {
            ubus::call("network.interface.zte_wan", "status", Some("{}"))
        })
        .ok();
    let wan6 = state
        .cache
        .get_or_fetch(cache::WAN6, max_age, || {
            ubus::call("network.interface.zte_wan6", "status", Some("{}"))
        })
        .ok();
    evaluate(
        battery.as_ref(),
        netinfo.as_ref(),
        wan.as_ref(),
        wan6.as_ref(),
    )
}

fn evaluate(
    battery: Option<&BatteryInfo>,
    netinfo: Option<&Value>,
    wan: Option<&Value>,
    wan6: Option<&Value>,
) -> InstallReadiness {
    let battery_percent = battery.map(|reading| reading.capacity);
    let external_power = battery.is_some_and(|reading| {
        matches!(
            reading.status.to_ascii_lowercase().as_str(),
            "charging" | "full"
        )
    });
    let battery_ready = battery_percent.is_some_and(|percent| {
        (0..=100).contains(&percent) && (external_power || percent >= MIN_BATTERY_PERCENT)
    });

    let network_type = netinfo
        .and_then(|reading| reading["network_type"].as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let service_ready = network_type
        .as_deref()
        .is_some_and(|kind| kind != "NO_SERVICE" && !kind.starts_with("LIMITED_SERVICE"));
    let signal_bars = netinfo
        .and_then(|reading| number(&reading["signalbar"]))
        .filter(|value| (0.0..=5.0).contains(value))
        .map(|value| value as u8);
    let signal_dbm = netinfo.and_then(strongest_signal);
    let signal_ready = signal_bars.is_some_and(|bars| bars >= 1)
        || signal_dbm.is_some_and(|dbm| dbm >= MIN_SIGNAL_DBM);
    let wan_ready = interface_ready(wan) || interface_ready(wan6);

    let reason = if battery.is_none() || battery_percent.is_some_and(|p| !(0..=100).contains(&p)) {
        Some("battery state is unavailable".to_string())
    } else if !battery_ready {
        Some(format!(
            "battery is {}%; connect power or charge to at least {MIN_BATTERY_PERCENT}%",
            battery_percent.unwrap_or_default()
        ))
    } else if netinfo.is_none() {
        Some("cellular status is unavailable".to_string())
    } else if !service_ready {
        Some("cellular service is not registered yet".to_string())
    } else if !signal_ready {
        Some(format!(
            "cellular signal is too weak; wait for at least one bar or {MIN_SIGNAL_DBM:.0} dBm"
        ))
    } else if !wan_ready {
        Some("cellular WAN is not online yet".to_string())
    } else {
        None
    };

    InstallReadiness {
        ready: reason.is_none(),
        reason,
        minimum_battery_percent: MIN_BATTERY_PERCENT,
        battery_percent,
        external_power,
        network_type,
        signal_bars,
        signal_dbm,
        minimum_signal_dbm: MIN_SIGNAL_DBM,
        wan_ready,
    }
}

fn strongest_signal(reading: &Value) -> Option<f64> {
    ["nr5g_rsrp", "lte_rsrp", "rscp"]
        .iter()
        .filter_map(|key| number(&reading[*key]))
        .filter(|value| (-160.0..=-20.0).contains(value))
        .max_by(f64::total_cmp)
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|value| value.is_finite())
}

fn interface_ready(status: Option<&Value>) -> bool {
    status.is_some_and(|value| {
        value["up"] == true
            && value["pending"] != true
            && ["ipv4-address", "ipv6-address"].iter().any(|key| {
                value[*key]
                    .as_array()
                    .is_some_and(|addresses| !addresses.is_empty())
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn battery(capacity: i64, status: &str) -> BatteryInfo {
        BatteryInfo {
            status: status.into(),
            capacity,
            voltage_uv: 0,
            current_ua: 0,
            temperature: 0,
        }
    }

    fn lte() -> Value {
        json!({"network_type":"LTE","signalbar":"3","lte_rsrp":-96,"nr5g_rsrp":0})
    }

    fn wan() -> Value {
        json!({"up":true,"pending":false,"ipv4-address":[{"address":"100.64.0.2"}]})
    }

    #[test]
    fn readiness_accepts_thirty_percent_on_battery() {
        let battery = battery(30, "Discharging");

        assert!(evaluate(Some(&battery), Some(&lte()), Some(&wan()), None).ready);
    }

    #[test]
    fn readiness_defers_below_thirty_percent_when_unplugged() {
        let battery = battery(29, "Discharging");
        let result = evaluate(Some(&battery), Some(&lte()), Some(&wan()), None);

        assert_eq!(
            result.blocking_reason(),
            Some("battery is 29%; connect power or charge to at least 30%")
        );
    }

    #[test]
    fn readiness_accepts_low_battery_while_charging() {
        let battery = battery(5, "Charging");

        assert!(evaluate(Some(&battery), Some(&lte()), Some(&wan()), None).ready);
    }

    #[test]
    fn readiness_defers_when_service_is_not_registered() {
        let battery = battery(80, "Discharging");
        let signal = json!({"network_type":"NO_SERVICE","signalbar":"4","lte_rsrp":-80});
        let result = evaluate(Some(&battery), Some(&signal), Some(&wan()), None);

        assert_eq!(
            result.blocking_reason(),
            Some("cellular service is not registered yet")
        );
    }

    #[test]
    fn readiness_defers_when_signal_is_too_weak() {
        let battery = battery(80, "Discharging");
        let signal = json!({"network_type":"LTE","signalbar":"0","lte_rsrp":-130});
        let result = evaluate(Some(&battery), Some(&signal), Some(&wan()), None);

        assert_eq!(
            result.blocking_reason(),
            Some("cellular signal is too weak; wait for at least one bar or -125 dBm")
        );
    }

    #[test]
    fn readiness_uses_nr_signal_when_lte_values_are_zero_in_sa() {
        let battery = battery(80, "Discharging");
        let signal =
            json!({"network_type":"NR5G_SA","signalbar":"0","lte_rsrp":0,"nr5g_rsrp":-112});

        assert!(evaluate(Some(&battery), Some(&signal), Some(&wan()), None).ready);
    }

    #[test]
    fn readiness_accepts_an_ipv6_only_wan() {
        let battery = battery(80, "Discharging");
        let wan6 = json!({"up":true,"pending":false,"ipv6-address":[{"address":"2001:db8::2"}]});

        assert!(evaluate(Some(&battery), Some(&lte()), None, Some(&wan6)).ready);
    }

    #[test]
    fn readiness_defers_when_wan_has_no_address() {
        let battery = battery(80, "Discharging");
        let offline = json!({"up":true,"pending":false,"ipv4-address":[]});
        let result = evaluate(Some(&battery), Some(&lte()), Some(&offline), None);

        assert_eq!(
            result.blocking_reason(),
            Some("cellular WAN is not online yet")
        );
    }
}
