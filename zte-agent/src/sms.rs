use std::process::Command;

use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

const SMS_DB_PATH: &str = "/etc_rw/ztembb/ztesms/sms_db/sms.db";

/// List SMS.
///
/// Once a key exists on the device, the firmware returns `number` and `content`
/// encrypted. They are resolved back to UCS-2 hex here, so clients see exactly
/// what they always did. Devices that return plaintext are untouched.
pub fn sms_list(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_wms", "zte_libwms_get_sms_data", Some(&parsed.to_string())) {
        Ok(mut data) => {
            resolve_message_fields(&mut data);
            (200, json!({"ok": true, "data": data}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// Resolve encrypted `number`/`content` in a `messages` array in place.
///
/// The on-disk WMS database stores both fields as plain UCS-2 hex — only the
/// ubus layer encrypts them on the way out. Reading the row back from SQLite is
/// therefore both simpler and *less invasive* than decrypting: the handshake
/// logs into the web UI, which kicks out whoever is using it, and their next
/// login invalidates the agent's key in turn.
///
/// SQLite is tried first for that reason; decryption stays as the fallback for
/// devices where the database isn't readable.
pub fn resolve_message_fields(data: &mut Value) {
    let Some(messages) = data.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };

    let encrypted_ids: Vec<i64> = messages
        .iter()
        .filter(|m| {
            ["number", "content"]
                .iter()
                .any(|f| m.get(f).and_then(|v| v.as_str()).is_some_and(looks_encrypted))
        })
        .filter_map(|m| {
            m.get("id")
                .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        })
        .collect();

    if encrypted_ids.is_empty() {
        return;
    }

    let from_db = db_fetch_fields(&encrypted_ids).unwrap_or_default();

    for msg in messages {
        let id = msg
            .get("id")
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));

        for (idx, field) in ["number", "content"].iter().enumerate() {
            let Some(raw) = msg.get(field).and_then(|v| v.as_str()) else {
                continue;
            };
            if !looks_encrypted(raw) {
                continue;
            }

            let resolved = id
                .and_then(|i| from_db.get(&i))
                .map(|pair: &(String, String)| {
                    if idx == 0 {
                        pair.0.clone()
                    } else {
                        pair.1.clone()
                    }
                })
                .unwrap_or_else(|| crate::web_crypto::maybe_decrypt(raw));

            msg[*field] = Value::String(resolved);
        }
    }
}

/// Same shape as the check in `web_crypto`, kept local so listing never has to
/// touch the crypto module (and therefore never triggers a handshake).
fn looks_encrypted(value: &str) -> bool {
    let v = value.trim();
    if v.len() < 40 || v.len() % 4 != 0 {
        return false;
    }
    if v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    v.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
}

/// Read `number`/`content` straight from the WMS database for the given ids.
fn db_fetch_fields(ids: &[i64]) -> Result<std::collections::HashMap<i64, (String, String)>, String> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let in_clause = ids
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT id, number, content FROM sms WHERE id IN ({in_clause});");
    let output = Command::new("/usr/bin/sqlite3")
        .args(["-cmd", ".timeout 2000", "-readonly", SMS_DB_PATH, &sql])
        .output()
        .map_err(|e| format!("spawn sqlite3: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let mut out = std::collections::HashMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        // sqlite3 default output separator is '|', and neither UCS-2 hex field
        // can contain it.
        let mut parts = line.splitn(3, '|');
        let (Some(id), Some(number), Some(content)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if let Ok(id) = id.trim().parse::<i64>() {
            out.insert(id, (number.to_string(), content.to_string()));
        }
    }
    Ok(out)
}

pub fn sms_capacity(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_wms", "zwrt_wms_get_wms_capacity", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// Send an SMS.
///
/// Firmware from the 2025-12 build requires `number` and `message_body` to be
/// encrypted (see `web_crypto`). Clients keep posting plaintext and the
/// encryption is applied here, so the HTTP API is unchanged.
///
/// The device-side key is global and gets overwritten by any web UI login,
/// hence the single retry with a fresh handshake.
pub fn sms_send(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    match send_via_ubus(&parsed) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

/// Send an SMS through ubus with encrypted parameters.
///
/// Public because both `/api/sms/send` and SMS forwarding use it. `params`
/// are passed in plaintext; this function applies the encryption.
pub fn send_via_ubus(params: &Value) -> Result<Value, String> {
    match send_encrypted(params) {
        Ok(data) => Ok(data),
        Err(first) => {
            // Any web UI login overwrites the key — one retry with a fresh
            // handshake before giving up.
            crate::web_crypto::global().invalidate();
            send_encrypted(params).map_err(|second| format!("{second} (first attempt: {first})"))
        }
    }
}

fn send_encrypted(parsed: &Value) -> Result<Value, String> {
    let mut params = parsed.clone();
    let obj = params
        .as_object_mut()
        .ok_or_else(|| "body must be an object".to_string())?;

    for field in ["number", "message_body"] {
        let plain = obj
            .get(field)
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("missing '{field}'"))?
            .to_string();
        // An already-encrypted value would get encrypted twice, but clients
        // always post plaintext so that shouldn't happen.
        let enc = crate::web_crypto::encrypt_field(&plain)?;
        obj.insert(field.to_string(), Value::String(enc));
    }

    ubus::call("zwrt_wms", "zte_libwms_send_sms", Some(&params.to_string()))
}

/// Delete one or more SMS by id.
///
/// Body shape (legacy ZTE format): `{"id": "3681;3682;"}` — semicolon-joined ids with trailing `;`.
///
/// Firmware bug: `zwrt_wms_delete_sms` works for NV-stored messages but silently returns
/// `{"result": 3}` without deleting SIM-stored rows. The daemon's listing reads from
/// `/etc_rw/ztembb/ztesms/sms_db/sms.db`, so we fall back to a direct SQLite DELETE for any
/// id that survived the ubus call.
pub fn sms_delete(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };

    let ids = match parse_ids(parsed.get("id")) {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => return (400, json!({"ok": false, "error": "no ids in 'id' field"})),
        Err(e) => return (400, json!({"ok": false, "error": e})),
    };

    let ubus_result = ubus::call("zwrt_wms", "zwrt_wms_delete_sms", Some(&parsed.to_string()));

    let survivors = match db_filter_existing(&ids) {
        Ok(v) => v,
        Err(e) => {
            return match ubus_result {
                Ok(data) => (200, json!({"ok": true, "data": data, "warning": format!("db check skipped: {e}")})),
                Err(ubus_err) => (503, json!({"ok": false, "error": format!("ubus: {ubus_err}; db: {e}")})),
            };
        }
    };

    if survivors.is_empty() {
        return (
            200,
            json!({"ok": true, "data": ubus_result.unwrap_or(Value::Null), "deleted_via": "ubus"}),
        );
    }

    match db_delete_ids(&survivors) {
        Ok(()) => (
            200,
            json!({"ok": true, "deleted_via": "sqlite", "ids": survivors}),
        ),
        Err(e) => (503, json!({"ok": false, "error": format!("sqlite delete failed: {e}")})),
    }
}

/// Parse the legacy ZTE id format (semicolon-joined with trailing `;`).
fn parse_ids(field: Option<&Value>) -> Result<Vec<i64>, String> {
    let raw = field.ok_or_else(|| "missing 'id' field".to_string())?;
    let s = raw.as_str().ok_or_else(|| "'id' must be a string".to_string())?;
    let mut out = Vec::new();
    for part in s.split(';') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let n: i64 = p
            .parse()
            .map_err(|_| format!("invalid id '{p}' (must be integer)"))?;
        out.push(n);
    }
    Ok(out)
}

/// Return the subset of `ids` still present in the WMS sms table.
fn db_filter_existing(ids: &[i64]) -> Result<Vec<i64>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let in_clause = ids
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT id FROM sms WHERE id IN ({in_clause});");
    let output = Command::new("/usr/bin/sqlite3")
        .args(["-cmd", ".timeout 2000", "-readonly", SMS_DB_PATH, &sql])
        .output()
        .map_err(|e| format!("spawn sqlite3: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Ok(n) = l.parse::<i64>() {
            out.push(n);
        }
    }
    Ok(out)
}

/// Direct DELETE bypassing the broken ubus path.
fn db_delete_ids(ids: &[i64]) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let in_clause = ids
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("DELETE FROM sms WHERE id IN ({in_clause});");
    let output = Command::new("/usr/bin/sqlite3")
        .args(["-cmd", ".timeout 2000", SMS_DB_PATH, &sql])
        .output()
        .map_err(|e| format!("spawn sqlite3: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

pub fn sms_mark_read(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_wms", "zwrt_wms_modify_tag", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}
