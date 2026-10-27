use std::net::Ipv4Addr;

use serde_json::{json, Value};
use std::sync::Mutex;

use crate::handlers::AppState;
use crate::ubus;

pub fn router_dns_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_router.api", "router_get_dns_para", Some("{}")) {
        Ok(data) => {
            // Firmware returns keys with "wan_" prefix (e.g. wan_dns_mode);
            // strip it so iOS client can use clean names (dns_mode, prefer_dns_manual, etc.)
            let mut cleaned = serde_json::Map::new();
            if let Some(obj) = data.as_object() {
                for (k, v) in obj {
                    let key = k.strip_prefix("wan_").unwrap_or(k).to_string();
                    cleaned.insert(key, v.clone());
                }
            }
            // Firmware bug: sometimes returns empty manual DNS values; fill from UCI
            if cleaned.get("dns_mode").and_then(|v| v.as_str()) == Some("manual") {
                if cleaned
                    .get("prefer_dns_manual")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    if let Ok(v) = ubus::uci_get("network.wan.dns") {
                        let mut parts = v.split_whitespace();
                        if let Some(primary) = parts.next() {
                            cleaned.insert("prefer_dns_manual".into(), Value::String(primary.to_string()));
                        }
                        if let Some(secondary) = parts.next() {
                            cleaned.insert("standby_dns_manual".into(), Value::String(secondary.to_string()));
                        }
                    }
                }
            }
            (200, json!({"ok": true, "data": Value::Object(cleaned)}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_dns_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_wan_dns", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_lan_get(_state: &AppState) -> (u16, Value) {
    let read = || -> Result<_, String> {
        let network = ubus::uci_show_map(&ubus::uci_show("network.lan")?);
        let dhcp = ubus::uci_show_map(&ubus::uci_show("dhcp.lan")?);
        let required = |map: &std::collections::HashMap<String, String>, key: &str| {
            map.get(key)
                .filter(|value| !value.is_empty())
                .cloned()
                .ok_or_else(|| format!("LAN setting {key} is unavailable"))
        };
        let ip = required(&network, "network.lan.ipaddr")?;
        let mask = required(&network, "network.lan.netmask")?;
        let start = required(&dhcp, "dhcp.lan.start")?;
        let limit = required(&dhcp, "dhcp.lan.limit")?;
        let lease = required(&dhcp, "dhcp.lan.leasetime")?;
        let end = strict_compute_dhcp_end(&ip, &mask, &start, &limit)
            .ok_or_else(|| "The reported DHCP pool is invalid".to_string())?;
        Ok((
            ip,
            mask,
            start,
            lease,
            end,
            dhcp.get("dhcp.lan.ignore").is_some_and(|v| v == "1"),
        ))
    };
    let (ip, mask, start, lease, end, ignored) = match read() {
        Ok(values) => values,
        Err(error) => return (503, json!({"ok":false,"error":error})),
    };
    (
        200,
        json!({"ok": true, "data": {
            "lan_ipaddr": ip, "lan_netmask": mask,
            "dhcp_enable": if ignored { "0" } else { "1" },
            "dhcp_start": start, "dhcp_end": end,
            "dhcp_lease_time": lease
        }}),
    )
}

/// The DHCP range's last address from UCI's `start` offset and `limit`:
/// the router's network (its address and netmask) plus `start + limit - 1`,
/// at most the subnet's last host (as OpenWrt's `ipcalc.sh` keeps the range
/// inside the subnet). On a /24 this is the old answer (the last octet
/// replaced) for every range that fits the subnet. Without a usable address
/// or netmask (/8 to /30), the old answer.
#[cfg(test)]
fn compute_dhcp_end(base_ip: &str, netmask: &str, start: &str, limit: &str) -> String {
    let start_num: u32 = start.parse().unwrap_or(100);
    let limit_num: u32 = limit.parse().unwrap_or(50);
    let end_host = start_num.saturating_add(limit_num).saturating_sub(1);
    let mask = netmask
        .parse::<Ipv4Addr>()
        .map(u32::from)
        .ok()
        .filter(|m| m.count_ones() == m.leading_ones() && (8..=30).contains(&m.leading_ones()));
    match (base_ip.parse::<Ipv4Addr>(), mask) {
        (Ok(ip), Some(m)) => {
            let net = u32::from(ip) & m;
            let last_host = (net | !m) - 1;
            Ipv4Addr::from(net.saturating_add(end_host).min(last_host)).to_string()
        }
        // Replace last octet of base IP with end_host
        _ => match base_ip.rfind('.') {
            Some(prefix) => format!("{}.{end_host}", &base_ip[..prefix]),
            None => format!("192.168.0.{end_host}"),
        },
    }
}

pub fn router_lan_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    lan_set_with(state, body, |params| ubus::call("zwrt_router.api", "router_set_lan_para", Some(params)))
}

/// `router_lan_set` with the firmware call as `apply` (tests swap it).
fn lan_set_with(state: &AppState, body: &[u8], apply: impl FnOnce(&str) -> Result<Value, String>) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match apply(&parsed.to_string()) {
        Ok(data) => {
            // LAN HTTPS: a certificate (and local name) for the new address.
            let ip = parsed["lan_ipaddr"].as_str().or(parsed["ipaddr"].as_str()).and_then(|s| s.parse().ok());
            if let Some(ip) = ip {
                state.tls.lan_changed(ip);
            }
            (200, json!({"ok": true, "data": data}))
        }
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_router.api", "router_get_firewall_para", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_switch_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_firewall_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_level_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_firewall_level", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_nat_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_nat_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_dmz_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_dmz", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_upnp_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_router.api", "router_get_upnp_switch", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_upnp_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_upnp_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_port_forward_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_router.api", "router_get_portforward_rule", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_port_forward_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let mut parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    // Rule ids can be array positions. Serialize agent writes and check the browser's
    // reviewed list immediately before deleting; native callers may omit this guard.
    static WRITES: Mutex<()> = Mutex::new(());
    let _guard = WRITES.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(expected) = parsed
        .as_object_mut()
        .and_then(|object| object.remove("expected_rules"))
    {
        if parsed["action"] != "delete" || !expected.is_array() {
            return (
                400,
                json!({"ok":false,"error":"expected_rules requires a delete and a rule array"}),
            );
        }
        let current = match ubus::call("zwrt_router.api", "router_get_portforward_rule", Some("{}"))
        {
            Ok(data) => data,
            Err(error) => return (503, json!({"ok":false,"error":error})),
        };
        if !reviewed_port_rules_match(&expected, &current, &parsed["id"]) {
            return (
                409,
                json!({"ok":false,"error":"Port-forward rules changed. Review the current list before deleting."}),
            );
        }
    }
    match ubus::call(
        "zwrt_router.api",
        "router_set_portforward",
        Some(&parsed.to_string()),
    ) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_port_forward_switch(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_portforward_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_firewall_filter_rules(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_router.api", "router_get_macipport_filter_rule", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_vpn_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_router.api", "router_get_alg_para", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_vpn_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_alg_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_qos_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_router.api", "router_get_qos_switch", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_qos_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_qos_switch", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_domain_filter_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_router.api", "router_get_domainfilter_rule", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_domain_filter_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_domain_filter", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_mode_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_apn_object", "get_apn_mode", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_mode_set(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_apn_object", "set_apn_mode", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_profiles_get(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_apn_object", "get_manu_apn_list", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_profiles_add(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_apn_object", "add_manu_apn", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_profiles_modify(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_apn_object", "modify_manu_apn", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_auto_profiles(_state: &AppState) -> (u16, Value) {
    match ubus::call("zwrt_apn_object", "get_auto_apn_list", Some("{}")) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_profiles_delete(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_apn_object", "delete_manu_apn", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

pub fn router_apn_profiles_activate(_state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_apn_object", "enable_manu_apn_id", Some(&parsed.to_string())) {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err(e) => (503, json!({"ok": false, "error": e})),
    }
}

#[cfg(test)]
mod tests {
    /// A LAN apply that the firmware took moves LAN HTTPS to the posted
    /// address (the hosts line follows); a refused one changes nothing.
    #[test]
    fn a_lan_apply_moves_https() {
        let w = crate::tls::tests::World::new("lan-apply");
        let mut state = crate::handlers::AppState::new();
        state.tls = std::sync::Arc::new(w.tls());
        state.tls.turn_on().unwrap();
        let body = br#"{"lan_ipaddr":"192.168.8.1","lan_netmask":"255.255.255.0"}"#;
        let (status, _) = super::lan_set_with(&state, body, |_| Err("refused (fake)".into()));
        assert_eq!((status, w.hosts_file().as_deref()), (503, Some("192.168.0.1 u60.home.arpa\n")));
        let sent = std::cell::RefCell::new(String::new());
        let (status, _) = super::lan_set_with(&state, body, |p| {
            *sent.borrow_mut() = p.to_owned();
            Ok(serde_json::json!({"result": "success"}))
        });
        assert_eq!(status, 200);
        assert!(sent.borrow().contains("\"lan_ipaddr\":\"192.168.8.1\""));
        assert_eq!(w.hosts_file().as_deref(), Some("192.168.8.1 u60.home.arpa\n"));
        assert!(state.tls.status().error.unwrap().contains("now 192.168.8.1"));
    }

    use super::compute_dhcp_end;

    /// The old answer: the last octet replaced.
    fn last_octet(ip: &str, n: u32) -> String {
        format!("{}.{n}", &ip[..ip.rfind('.').unwrap()])
    }

    #[test]
    fn dhcp_end_on_a_24_changes_only_past_the_subnet() {
        assert_eq!(compute_dhcp_end("192.168.0.1", "255.255.255.0", "100", "50"), "192.168.0.149");
        for start in 1..=254u32 {
            for limit in 1..=(255 - start) {
                assert_eq!(
                    compute_dhcp_end("192.168.0.1", "255.255.255.0", &start.to_string(), &limit.to_string()),
                    last_octet("192.168.0.1", start + limit - 1),
                    "{start} {limit}"
                );
            }
        }
        // A range past the subnet stops at its last host (before: ".299").
        assert_eq!(compute_dhcp_end("192.168.0.1", "255.255.255.0", "200", "100"), "192.168.0.254");
        assert_eq!(compute_dhcp_end("192.168.0.1", "255.255.255.0", "100", "156"), "192.168.0.254");
    }

    #[test]
    fn dhcp_end_from_the_network_and_netmask() {
        // The verifier's case: a /16 with start 200, limit 100.
        assert_eq!(compute_dhcp_end("10.0.0.1", "255.255.0.0", "200", "100"), "10.0.1.43");
        assert_eq!(compute_dhcp_end("10.0.0.1", "255.255.0.0", "100", "150"), "10.0.0.249");
        assert_eq!(compute_dhcp_end("10.0.5.1", "255.255.0.0", "100", "50"), "10.0.0.149");
        assert_eq!(compute_dhcp_end("172.16.3.1", "255.255.254.0", "300", "10"), "172.16.3.53");
        assert_eq!(compute_dhcp_end("10.0.0.1", "255.255.0.0", "65000", "1000"), "10.0.255.254");
        assert_eq!(compute_dhcp_end("192.168.8.1", "255.255.255.0", "", ""), "192.168.8.149");
        // No usable address or netmask: the old answer.
        assert_eq!(compute_dhcp_end("192.168.0.1", "", "100", "50"), "192.168.0.149");
        assert_eq!(compute_dhcp_end("192.168.0.1", "255.0.255.0", "200", "100"), "192.168.0.299");
        assert_eq!(compute_dhcp_end("", "", "", ""), "192.168.0.149");
        assert_eq!(compute_dhcp_end("192.168.0.1", "255.255.255.0", "0", "0"), "192.168.0.0");
    }
}

fn strict_compute_dhcp_end(base_ip: &str, netmask: &str, start: &str, limit: &str) -> Option<String> {
    let ip = u32::from(base_ip.parse::<Ipv4Addr>().ok()?);
    let mask = u32::from(netmask.parse::<Ipv4Addr>().ok()?);
    let inverse = !mask;
    if !(3..0x8000_0000).contains(&inverse) || inverse & (inverse + 1) != 0 {
        return None;
    }
    let network = ip & mask;
    let offset: u32 = start.parse().ok()?;
    let count: u32 = limit.parse().ok()?;
    let end = offset.checked_add(count.checked_sub(1)?)?;
    if offset == 0 || end >= inverse {
        return None;
    }
    Some(Ipv4Addr::from(network.checked_add(end)?).to_string())
}

fn reviewed_port_rules_match(expected: &Value, current: &Value, id: &Value) -> bool {
    let Some(rules) = current.get("rule_list").and_then(Value::as_array) else {
        return false;
    };
    if current.get("rule_list") != Some(expected) {
        return false;
    }
    let Some(id) = id.as_str() else {
        return false;
    };
    rules
        .iter()
        .enumerate()
        .filter(|(index, rule)| {
            let reported = rule.get("id");
            match reported {
                Some(Value::String(value)) => value == id,
                Some(Value::Number(value)) => value.to_string() == id,
                None | Some(Value::Null) => index.to_string() == id,
                _ => false,
            }
        })
        .count()
        == 1
}
#[cfg(test)]
mod console_routing_tests {
    use super::*;

    #[test]
    fn dhcp_end_crosses_octets_using_the_actual_subnet() {
        assert_eq!(
            strict_compute_dhcp_end("192.168.1.1", "255.255.254.0", "250", "30"),
            Some("192.168.1.23".into())
        );
    }

    #[test]
    fn dhcp_end_preserves_a_standard_pool() {
        assert_eq!(
            strict_compute_dhcp_end("192.168.0.1", "255.255.255.0", "100", "50"),
            Some("192.168.0.149".into())
        );
    }

    #[test]
    fn dhcp_end_rejects_invalid_configuration_without_inventing_defaults() {
        for (mask, start, count) in [
            ("255.255.255.0", "250", "10"),
            ("255.255.255.0", "100", "0"),
            ("255.0.255.0", "100", "50"),
            ("255.255.255.0", "4294967295", "2"),
            ("255.255.255.0", "bad", "50"),
        ] {
            assert_eq!(strict_compute_dhcp_end("192.168.0.1", mask, start, count), None);
        }
    }

    #[test]
    fn reviewed_rule_deletion_rejects_a_shifted_or_missing_list() {
        let reviewed = json!([{"name":"First"},{"name":"Second"}]);
        assert!(!reviewed_port_rules_match(
            &reviewed,
            &json!({"rule_list":[{"name":"Second"}]}),
            &json!("0")
        ));
        assert!(!reviewed_port_rules_match(
            &reviewed,
            &json!({}),
            &json!("0")
        ));
    }

    #[test]
    fn reviewed_rule_deletion_accepts_numeric_ids_and_position_fallbacks() {
        for (rules, id) in [
            (json!([{"name":"Position"}]), "0"),
            (json!([{"id":42,"name":"Explicit"}]), "42"),
        ] {
            assert!(reviewed_port_rules_match(
                &rules,
                &json!({"rule_list":rules}),
                &json!(id)
            ));
        }
    }

    #[test]
    fn reviewed_rule_deletion_rejects_duplicate_ids() {
        let rules = json!([{"id":"0","name":"First"},{"id":"0","name":"Second"}]);
        assert!(!reviewed_port_rules_match(
            &rules,
            &json!({"rule_list":rules}),
            &json!("0")
        ));
    }
}
