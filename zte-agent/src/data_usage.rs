use serde_json::{json, Map, Value};

use crate::handlers::AppState;
use crate::ubus;

const OBJECT: &str = "zwrt_data";
const SECTION: &str = "zwrt_data_commit.wwancid1dst";
const MAX_LIMIT_BYTES: u64 = 100 * 1024 * 1024 * 1024 * 1024;

fn base_payload() -> Value {
    json!({"source_module": "web", "cid": 1})
}

fn numeric(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn flag(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => value.as_u64().and_then(|value| match value {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }),
        Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "disabled" => Some(false),
            "1" | "true" | "enabled" => Some(true),
            _ => None,
        },
        _ => None,
    }
}

fn uci_number(suffix: &str) -> Option<u64> {
    ubus::uci_get(&format!("{SECTION}.{suffix}"))
        .ok()
        .and_then(|value| value.parse().ok())
}

fn period(source: Option<&Map<String, Value>>, prefix: &str) -> Value {
    let read = |suffix: &str| -> Value {
        let native = source
            .and_then(|source| source.get(&format!("{prefix}_{suffix}")))
            .and_then(|value| numeric(Some(value)));
        native
            .or_else(|| uci_number(&format!("{prefix}_{suffix}")))
            .map(Value::from)
            .unwrap_or(Value::Null)
    };
    json!({
        "tx_bytes": read("tx_bytes"),
        "rx_bytes": read("rx_bytes"),
        "time_secs": read("time"),
        "tx_packets": read("tx_packets"),
        "rx_packets": read("rx_packets"),
    })
}

fn read_limit() -> Result<Value, String> {
    let params = base_payload().to_string();
    let raw = ubus::call(OBJECT, "get_wwandst_monthlimit", Some(&params))?;
    let object = raw
        .as_object()
        .ok_or_else(|| "monthly limit response was not an object".to_string())?;
    let bytes = numeric(object.get("value"));
    let warning_percent = numeric(object.get("ratio"));
    let enabled = flag(object.get("enable"));
    let limit_type = numeric(object.get("type"));
    let auto_disconnect = flag(object.get("overflow_auto_disconn"))
        .or_else(|| uci_number("month_limit_overflow_auto_disconn_enable").map(|value| value == 1));
    Ok(json!({
        "available": enabled.is_some() && bytes.is_some() && warning_percent.is_some() && limit_type == Some(2),
        "enabled": enabled,
        "bytes": bytes,
        "warning_percent": warning_percent,
        "exceeded": flag(object.get("overflow")).unwrap_or(false),
        "auto_disconnect": auto_disconnect.unwrap_or(false),
        "type": limit_type,
    }))
}

fn read_cycle() -> Result<Value, String> {
    let params = base_payload().to_string();
    let raw = ubus::call(OBJECT, "get_wwandst_clearday", Some(&params))?;
    let object = raw
        .as_object()
        .ok_or_else(|| "billing cycle response was not an object".to_string())?;
    let enabled = flag(object.get("enable"));
    let reset_day = numeric(object.get("clearday"));
    Ok(json!({
        "available": enabled.is_some() && reset_day.is_some(),
        "enabled": enabled,
        "reset_day": reset_day,
    }))
}

fn snapshot() -> Value {
    let params = json!({"source_module": "web", "cid": 1, "type": 4}).to_string();
    let native = ubus::call(OBJECT, "get_wwandst", Some(&params)).ok();
    let source = native.as_ref().and_then(Value::as_object);
    let mut errors = Vec::new();
    let limit = read_limit().unwrap_or_else(|error| {
        errors.push(format!("monthly limit: {error}"));
        json!({"available": false})
    });
    let cycle = read_cycle().unwrap_or_else(|error| {
        errors.push(format!("billing cycle: {error}"));
        json!({"available": false})
    });
    json!({
        "day": period(source, "day"),
        "month": period(source, "month"),
        "total": period(source, "total"),
        "limit": limit,
        "cycle": cycle,
        "settings_errors": errors,
    })
}

/// GET /api/data-usage
pub fn get(_state: &AppState) -> (u16, Value) {
    (200, json!({"ok": true, "data": snapshot()}))
}

#[derive(Debug, PartialEq)]
struct LimitInput {
    enabled: bool,
    bytes: u64,
    warning_percent: u64,
    auto_disconnect: bool,
}

fn parse_object(body: &[u8]) -> Result<Map<String, Value>, String> {
    serde_json::from_slice::<Value>(body)
        .map_err(|_| "invalid JSON".to_string())?
        .as_object()
        .cloned()
        .ok_or_else(|| "request body must be an object".to_string())
}

fn parse_limit(body: &[u8]) -> Result<LimitInput, String> {
    let object = parse_object(body)?;
    let enabled =
        flag(object.get("enabled")).ok_or_else(|| "enabled must be true or false".to_string())?;
    let bytes = numeric(object.get("bytes"))
        .filter(|bytes| *bytes > 0 && *bytes <= MAX_LIMIT_BYTES)
        .ok_or_else(|| "bytes must be between 1 and 109951162777600".to_string())?;
    let warning_percent = numeric(object.get("warning_percent"))
        .filter(|ratio| (1..=100).contains(ratio))
        .ok_or_else(|| "warning_percent must be between 1 and 100".to_string())?;
    let auto_disconnect = flag(object.get("auto_disconnect"))
        .ok_or_else(|| "auto_disconnect must be true or false".to_string())?;
    Ok(LimitInput {
        enabled,
        bytes,
        warning_percent,
        auto_disconnect,
    })
}

/// PUT /api/data-usage/limit
pub fn set_limit(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let input = match parse_limit(body) {
        Ok(input) => input,
        Err(error) => return (400, json!({"ok": false, "error": error})),
    };
    let payload = json!({
        "source_module": "web",
        "cid": 1,
        "enable": u8::from(input.enabled),
        "type": 2,
        "value": input.bytes.to_string(),
        "ratio": input.warning_percent,
        "overflow_auto_disconn": u8::from(input.auto_disconnect),
    });
    if let Err(error) = ubus::call(OBJECT, "set_wwandst_monthlimit", Some(&payload.to_string())) {
        return (503, json!({"ok": false, "error": error}));
    }
    match read_limit() {
        Ok(limit)
            if flag(limit.get("enabled")) == Some(input.enabled)
                && numeric(limit.get("bytes")) == Some(input.bytes)
                && numeric(limit.get("warning_percent")) == Some(input.warning_percent)
                && flag(limit.get("auto_disconnect")) == Some(input.auto_disconnect) =>
        {
            (200, json!({"ok": true, "data": limit}))
        }
        Ok(limit) => (
            503,
            json!({"ok": false, "error": "the router did not confirm the monthly limit", "data": limit}),
        ),
        Err(error) => (
            503,
            json!({"ok": false, "error": format!("the router accepted the change but readback failed: {error}")}),
        ),
    }
}

#[derive(Debug, PartialEq)]
struct CycleInput {
    enabled: bool,
    reset_day: u64,
}

fn parse_cycle(body: &[u8]) -> Result<CycleInput, String> {
    let object = parse_object(body)?;
    let enabled =
        flag(object.get("enabled")).ok_or_else(|| "enabled must be true or false".to_string())?;
    let reset_day = numeric(object.get("reset_day"))
        .filter(|day| (1..=31).contains(day))
        .ok_or_else(|| "reset_day must be between 1 and 31".to_string())?;
    Ok(CycleInput { enabled, reset_day })
}

/// PUT /api/data-usage/cycle
pub fn set_cycle(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let input = match parse_cycle(body) {
        Ok(input) => input,
        Err(error) => return (400, json!({"ok": false, "error": error})),
    };
    let payload = json!({
        "source_module": "web",
        "cid": 1,
        "enable": u8::from(input.enabled),
        "clearday": input.reset_day,
    });
    if let Err(error) = ubus::call(OBJECT, "set_wwandst_clearday", Some(&payload.to_string())) {
        return (503, json!({"ok": false, "error": error}));
    }
    match read_cycle() {
        Ok(cycle)
            if flag(cycle.get("enabled")) == Some(input.enabled)
                && numeric(cycle.get("reset_day")) == Some(input.reset_day) =>
        {
            (200, json!({"ok": true, "data": cycle}))
        }
        Ok(cycle) => (
            503,
            json!({"ok": false, "error": "the router did not confirm the billing cycle", "data": cycle}),
        ),
        Err(error) => (
            503,
            json!({"ok": false, "error": format!("the router accepted the change but readback failed: {error}")}),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_numeric_strings_and_strict_flags() {
        assert_eq!(numeric(Some(&json!("123"))), Some(123));
        assert_eq!(numeric(Some(&json!(-1))), None);
        assert_eq!(flag(Some(&json!("enabled"))), Some(true));
        assert_eq!(flag(Some(&json!(2))), None);
    }

    #[test]
    fn validates_complete_monthly_limit() {
        assert_eq!(
            parse_limit(br#"{"enabled":true,"bytes":"107374182400","warning_percent":90,"auto_disconnect":false}"#),
            Ok(LimitInput {
                enabled: true,
                bytes: 107_374_182_400,
                warning_percent: 90,
                auto_disconnect: false,
            })
        );
        assert!(parse_limit(
            br#"{"enabled":true,"bytes":0,"warning_percent":90,"auto_disconnect":false}"#
        )
        .is_err());
        assert!(parse_limit(
            br#"{"enabled":true,"bytes":100,"warning_percent":101,"auto_disconnect":false}"#
        )
        .is_err());
        assert!(parse_limit(br#"{"enabled":true,"bytes":100,"warning_percent":90}"#).is_err());
    }

    #[test]
    fn validates_billing_cycle_day() {
        assert_eq!(
            parse_cycle(br#"{"enabled":false,"reset_day":"31"}"#),
            Ok(CycleInput {
                enabled: false,
                reset_day: 31,
            })
        );
        assert!(parse_cycle(br#"{"enabled":true,"reset_day":0}"#).is_err());
        assert!(parse_cycle(br#"{"enabled":true,"reset_day":32}"#).is_err());
    }
}
