use serde_json::{json, Value};

use crate::events::{self, Topic};
use crate::handlers::AppState;
use crate::ubus;

pub fn usb_status(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_bsp.usb", "list", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn usb_mode_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let result = ubus::call("zwrt_bsp.usb", "set", Some(&parsed.to_string()));
    events::touch(Topic::Usb);
    match result {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn usb_powerbank_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let result = ubus::call("zwrt_bsp.powerbank", "set", Some(&parsed.to_string()));
    // The charger view changes with powerbank mode; make the next read fresh
    // even if BSP_POWERBANK_EVENT lags.
    state.cache.invalidate(crate::cache::CHARGER);
    events::touch(Topic::Usb);
    match result {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::test_sink;

    /// A USB mode or powerbank change re-reads the `usb` topic (if someone
    /// watches); a body that is not JSON changes nothing.
    #[test]
    fn changes_touch_the_usb_topic() {
        let app = AppState::new();
        test_sink::take();
        assert_eq!(usb_mode_set(&app, b"not json").0, 400);
        assert!(test_sink::take().is_empty());
        usb_mode_set(&app, br#"{"mode":"x"}"#);
        assert_eq!(test_sink::take(), [Topic::Usb]);
        usb_powerbank_set(&app, br#"{"state":1}"#);
        assert_eq!(test_sink::take(), [Topic::Usb]);
    }
}
