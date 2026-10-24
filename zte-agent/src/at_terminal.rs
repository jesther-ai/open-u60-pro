use std::time::Instant;

use serde_json::{json, Value};

use crate::at_cmd;
use crate::handlers::AppState;

/// The command as it is sent: shell-dangerous characters stripped. The web
/// UI checks this form of a command (its dangerous-command question).
pub(crate) fn sanitize(command: &str) -> String {
    command.chars().filter(|c| !matches!(c, '\'' | '`' | '$' | ';' | '|' | '&')).collect()
}

/// POST /api/at/send — execute an AT command
/// Body: {"command": "AT+CSQ", "timeout": 3}
pub fn at_send(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    let command = match parsed["command"].as_str() {
        Some(c) => c,
        None => return (400, json!({"ok": false, "error": "missing 'command' field"})),
    };

    // Validate: must start with "AT" (case-insensitive)
    if !command.to_ascii_uppercase().starts_with("AT") {
        return (400, json!({"ok": false, "error": "command must start with 'AT'"}));
    }

    // Parse timeout: optional, 1-30, default 3
    let timeout: u64 = parsed["timeout"]
        .as_u64()
        .unwrap_or(3)
        .max(1)
        .min(30);

    let sanitized = sanitize(command);

    if sanitized.is_empty() {
        return (400, json!({"ok": false, "error": "command is empty after sanitization"}));
    }

    let start = Instant::now();
    match at_cmd::send(&state.at_port, &sanitized, timeout) {
        Ok(response) => {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let port = state.at_port.detect().unwrap_or_default();
            (200, json!({
                "ok": true,
                "data": {
                    "command": sanitized,
                    "response": response,
                    "port": port,
                    "elapsed_ms": elapsed_ms,
                }
            }))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// GET /api/at/port — return detected port info
pub fn at_port(state: &AppState) -> (u16, Value) {
    match state.at_port.detect() {
        Some(port) => (200, json!({
            "ok": true,
            "data": {
                "port": port,
                "available": true,
            }
        })),
        None => (200, json!({
            "ok": true,
            "data": {
                "port": null,
                "available": false,
            }
        })),
    }
}
