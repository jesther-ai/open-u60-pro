use std::process::Command;

use serde_json::Value;

/// Call `ubus call <object> <method> [<params>]` and parse JSON output.
pub fn call(object: &str, method: &str, params: Option<&str>) -> Result<Value, String> {
    let mut cmd = Command::new("ubus");
    cmd.args(["call", object, method]);
    if let Some(p) = params {
        cmd.arg(p);
    }
    let output = cmd.output().map_err(|e| format!("ubus exec: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = match stderr.find("\nUsage:") {
            Some(pos) => &stderr[..pos],
            None => &stderr,
        };
        return Err(format!(
            "ubus call {object} {method} failed: {}",
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(trimmed).map_err(|e| format!("ubus JSON parse: {e}"))
}

/// Run `uci get <key>` and return the value.
pub fn uci_get(key: &str) -> Result<String, String> {
    let output = Command::new("uci")
        .args(["get", key])
        .output()
        .map_err(|e| format!("uci exec: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("uci get {key}: {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run `uci set <key>=<value>` followed by `uci commit <config>`.
#[allow(dead_code)]
pub fn uci_set(key: &str, value: &str) -> Result<(), String> {
    uci_set_no_commit(key, value)?;
    let config = key.split('.').next().unwrap_or(key);
    uci_commit(config)
}

/// Run `uci set <key>=<value>` without committing.
pub fn uci_set_no_commit(key: &str, value: &str) -> Result<(), String> {
    let set_out = Command::new("uci")
        .args(["set", &format!("{key}={value}")])
        .output()
        .map_err(|e| format!("uci set: {e}"))?;
    if !set_out.status.success() {
        return Err(format!(
            "uci set {key}: {}",
            String::from_utf8_lossy(&set_out.stderr)
        ));
    }
    Ok(())
}

/// Run `uci commit <config>`.
/// Discard staged (uncommitted) uci changes for a config.
pub fn uci_revert(config: &str) -> Result<(), String> {
    let out = Command::new("uci")
        .args(["revert", config])
        .output()
        .map_err(|e| format!("uci revert: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "uci revert {config}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

pub fn uci_commit(config: &str) -> Result<(), String> {
    let commit_out = Command::new("uci")
        .args(["commit", config])
        .output()
        .map_err(|e| format!("uci commit: {e}"))?;
    if !commit_out.status.success() {
        return Err(format!(
            "uci commit {config}: {}",
            String::from_utf8_lossy(&commit_out.stderr)
        ));
    }
    Ok(())
}

