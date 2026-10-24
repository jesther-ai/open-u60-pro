use std::net::Ipv4Addr;

use serde_json::{json, Value};

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
    let ip = ubus::uci_get("network.lan.ipaddr").unwrap_or_default();
    let mask = ubus::uci_get("network.lan.netmask").unwrap_or_default();
    let ignore = ubus::uci_get("dhcp.lan.ignore").unwrap_or_default();
    let start = ubus::uci_get("dhcp.lan.start").unwrap_or_default();
    let limit = ubus::uci_get("dhcp.lan.limit").unwrap_or_default();
    let lease = ubus::uci_get("dhcp.lan.leasetime").unwrap_or_default();
    let end = compute_dhcp_end(&ip, &mask, &start, &limit);
    (200, json!({"ok": true, "data": {
        "lan_ipaddr": ip, "lan_netmask": mask,
        "dhcp_enable": if ignore == "1" { "0" } else { "1" },
        "dhcp_start": start, "dhcp_end": end,
        "dhcp_lease_time": lease
    }}))
}

/// The DHCP range's last address from UCI's `start` offset and `limit`:
/// the router's network (its address and netmask) plus `start + limit - 1`,
/// at most the subnet's last host (as OpenWrt's `ipcalc.sh` keeps the range
/// inside the subnet). On a /24 this is the old answer (the last octet
/// replaced) for every range that fits the subnet. Without a usable address
/// or netmask (/8 to /30), the old answer.
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
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match ubus::call("zwrt_router.api", "router_set_portforward", Some(&parsed.to_string())) {
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
