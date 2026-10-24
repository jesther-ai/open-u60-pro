use std::process::Command;

use serde_json::Value;

/// Call `ubus call <object> <method> [<params>]` and parse JSON output.
pub fn call(object: &str, method: &str, params: Option<&str>) -> Result<Value, String> {
    crate::httpd::gate::debug_assert_may_block();
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

/// `ubus send <event> <payload>`: broadcasts an agent event to other processes on the router.
pub fn send(event: &str, payload: &Value) -> Result<(), String> {
    crate::httpd::gate::debug_assert_may_block();
    let output = send_command(event, payload)
        .output()
        .map_err(|e| format!("ubus exec: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "ubus send {event} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn send_command(event: &str, payload: &Value) -> Command {
    let mut cmd = Command::new("ubus");
    cmd.args(["-t", "3", "send", event, &payload.to_string()]);
    cmd
}

/// Run `uci get <key>` and return the value.
pub fn uci_get(key: &str) -> Result<String, String> {
    crate::httpd::gate::debug_assert_may_block();
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

/// Run `uci -q show <target>`: one fork for a whole section, as
/// `config.section.option='value'` lines.
pub fn uci_show(target: &str) -> Result<String, String> {
    crate::httpd::gate::debug_assert_may_block();
    let output = Command::new("uci")
        .args(["-q", "show", target])
        .output()
        .map_err(|e| format!("uci exec: {e}"))?;
    if !output.status.success() {
        return Err(format!("uci show {target}: {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run `uci set <key>=<value>` followed by `uci commit <config>`.
#[allow(dead_code)]
pub fn uci_set(key: &str, value: &str) -> Result<(), String> {
    crate::httpd::gate::debug_assert_may_block();
    uci_set_no_commit(key, value)?;
    let config = key.split('.').next().unwrap_or(key);
    uci_commit(config)
}

/// Run `uci set <key>=<value>` without committing.
/// Parse `uci show <config>` output into `key -> value`, undoing uci's quoting
/// (`'...'` with embedded quotes written as `'\''`). Section headers
/// (`wireless.wifi0=wifi-device`) are included unquoted; lists keep their raw
/// `'a' 'b'` form.
pub fn uci_show_map(show: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in show.lines() {
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        map.insert(key.to_string(), uci_unquote(raw));
    }
    map
}

fn uci_unquote(raw: &str) -> String {
    let raw = raw.trim_end();
    if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        // `uci show` prints a list option as `'a' 'b'`. Neutralise escaped
        // quotes first so a scalar like `'x'\'' '\''y'` (value `x' 'y`) is not
        // mistaken for a list; if a `' '` separator survives, keep the raw
        // form rather than mangling it into `a' 'b`.
        if raw.replace("'\\''", "\u{0}").contains("' '") {
            return raw.to_string();
        }
        raw[1..raw.len() - 1].replace("'\\''", "'")
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod uci_show_tests {
    use super::uci_show_map;

    #[test]
    fn parses_values_sections_and_escaped_quotes() {
        let show = "wireless.wifi0=wifi-device\nwireless.wifi0.disabled='1'\nwireless.main_5g.ssid='Bob'\\''s Wi-Fi'\nwireless.main_5g.key=''\nwireless.zte_mbb.wifi_onoff='1'\n";
        let m = uci_show_map(show);
        assert_eq!(m["wireless.wifi0"], "wifi-device");
        assert_eq!(m["wireless.wifi0.disabled"], "1");
        assert_eq!(m["wireless.main_5g.ssid"], "Bob's Wi-Fi");
        assert_eq!(m["wireless.main_5g.key"], "");
        assert_eq!(m["wireless.zte_mbb.wifi_onoff"], "1");
        assert!(!m.contains_key("wireless.missing"));
    }

    #[test]
    fn lists_keep_raw_form_and_scalars_still_unescape() {
        let show = "wireless.x.list='a' 'b'\nwireless.x.list2='a'\\''b' 'c'\nwireless.x.ssid='Bob'\\''s Wi-Fi'\nwireless.x.tricky=''\\'' '\\'''\nwireless.x.spaced='a b'\n";
        let m = uci_show_map(show);
        assert_eq!(m["wireless.x.list"], "'a' 'b'");
        assert_eq!(m["wireless.x.list2"], "'a'\\''b' 'c'");
        assert_eq!(m["wireless.x.ssid"], "Bob's Wi-Fi");
        assert_eq!(m["wireless.x.tricky"], "' '");
        assert_eq!(m["wireless.x.spaced"], "a b");
    }

    #[test]
    fn send_command_passes_event_and_compact_json() {
        let cmd = super::send_command("zte_agent.sim_identity", &serde_json::json!({"phase": "online", "epoch": 1}));
        assert_eq!(cmd.get_program(), "ubus");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["-t", "3", "send", "zte_agent.sim_identity", r#"{"epoch":1,"phase":"online"}"#]);
    }
}

pub fn uci_set_no_commit(key: &str, value: &str) -> Result<(), String> {
    crate::httpd::gate::debug_assert_may_block();
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
pub fn uci_commit(config: &str) -> Result<(), String> {
    crate::httpd::gate::debug_assert_may_block();
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
