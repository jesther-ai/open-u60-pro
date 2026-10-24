//! Router › LAN / DHCP (iOS `LANSettingsView`): the router's address and
//! subnet mask, the DHCP server and its range, read at page load (no
//! stream), applied through `pages::netcfg`: asked first (the app asks
//! nothing), since a new address or range drops every device, this browser
//! included, then a page with the new address.
//!
//! - The agent reads `dhcp_start` as UCI's offset ("100") and `dhcp_end` as
//!   an address it computes; the app shows and sends both as they come.
//!   The web shows both as addresses (the offset added to the router's
//!   network).
//! - The agent hands the body to the firmware's `router_set_lan_para`
//!   unchanged, whose `ubus -v list` names `ipaddr`, `netmask`, `ignore`
//!   (an integer: 1 is DHCP off), `zte_start`, `zte_end` and `leasetime`;
//!   it derives UCI's `start` and `limit` from the two addresses (its
//!   disassembly). The body carries the app's keys (`dhcp_start` as the
//!   offset, as the agent reads it) and the firmware's (`zte_start` and
//!   `zte_end` as addresses). Not verified on the device.
//! - Checks (the app's "LAN IP is required", then web additions): a private
//!   IPv4 address, written as Rust prints it (no spaces or leading zeros),
//!   that is not its subnet's network or broadcast address, a subnet mask
//!   of /8 to /30, a range inside the subnet without the router's address,
//!   start before end, and a lease time in seconds (at least 120) or as UCI
//!   writes it (`12h`, `30m`, `1d`). The range and the lease time are hidden
//!   while DHCP is off (as the app's; CSS, so a field with an error still
//!   shows); a hidden range left outside a new subnet moves into it (the
//!   same host numbers).

use std::net::Ipv4Addr;

use maud::{html, Markup};
use serde_json::{json, Value};

use super::confirm::{asking, nothing, Bad, Question};
use super::netcfg::{check as checkbox, failed, field, form_error, switch, text, text_row, truthy, val, Done, Vals};
use crate::handlers::AppState;
use crate::server::Method;
use crate::web::fmt;
use crate::web::kit;
use crate::web::render::once_field;
use crate::web::screen::{FormState, PageData};
use crate::web::store::Claim;

pub const PATH: &str = "/ui/router/lan";
pub const APPLY: &str = "/ui/router/lan/apply";
pub const APPLIED: &str = "/ui/router/lan/applied";
const API: &str = "/api/router/lan";

/// A dotted IPv4 address, exactly as Rust's parser takes it (no spaces,
/// no leading zeros), so a checked value is sent as it was checked.
pub fn ip(s: &str) -> Option<Ipv4Addr> {
    s.parse().ok()
}

/// A contiguous mask of /8 to /30.
pub fn mask(s: &str) -> Option<u32> {
    let m = u32::from(ip(s)?);
    let prefix = m.leading_ones();
    (m.count_ones() == prefix && (8..=30).contains(&prefix)).then_some(m)
}

/// The DHCP start as an address: UCI's offset added to the router's
/// network (the agent hands the offset over as it is), else as given.
pub fn start_address(start: &str, lan_ip: &str, netmask: &str) -> String {
    match (start.parse::<u32>(), ip(lan_ip), mask(netmask)) {
        (Ok(n), Some(a), Some(m)) if start.bytes().all(|b| b.is_ascii_digit()) => Ipv4Addr::from((u32::from(a) & m).wrapping_add(n)).to_string(),
        _ => start.to_owned(),
    }
}

/// Seconds of a lease time: digits, with UCI's `m`, `h` or `d`.
fn lease_secs(s: &str) -> Option<u64> {
    let (num, unit) = match s.char_indices().last()? {
        (i, 'm') => (&s[..i], 60),
        (i, 'h') => (&s[..i], 3600),
        (i, 'd') => (&s[..i], 86_400),
        _ => (s, 1),
    };
    if num.is_empty() || num.len() > 9 || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    num.parse::<u64>().ok().map(|n| n * unit)
}

pub fn page(pd: &PageData) -> Markup {
    let d = match pd.reads.first() {
        Some(Ok(d)) => d,
        Some(Err(e)) => return html! { p.msg.err role="alert" { "Failed to load LAN: " (fmt::truncate(e, 120)) } },
        None => return html! {},
    };
    let f = pd.form;
    let typed = |name: &str, router: String| f.and_then(|f| f.value(name)).map_or(router, str::to_owned);
    let err = |name: &str| f.and_then(|f| f.error(name));
    let (lan_ip, netmask) = (text(&d["lan_ipaddr"]), text(&d["lan_netmask"]));
    let dhcp = f.map_or(truthy(&d["dhcp_enable"]), |f| f.value("dhcp") == Some("1"));
    html! {
        (form_error(pd))
        form.rf.lnf method="post" action=(APPLY) autocomplete="off" {
            (pd.csrf())
            (once_field(&pd.once()))
            (kit::section(Some("LAN"), None, html! {
                (text_row("ip", "IP Address", &typed("ip", lan_ip.clone()), err("ip"), "it fld", 15, Some("decimal")))
                (text_row("mask", "Subnet Mask", &typed("mask", netmask.clone()), err("mask"), "it fld", 15, Some("decimal")))
            }))
            (kit::section(Some("DHCP Server"), None, html! {
                (switch("dhcp", "Enable DHCP", dhcp, "it", "sr"))
                // Hidden while Enable DHCP is off (CSS), unless refused.
                (text_row("start", "Start Address", &typed("start", start_address(&text(&d["dhcp_start"]), &lan_ip, &netmask)), err("start"), "it fld dh", 15, Some("decimal")))
                (text_row("end", "End Address", &typed("end", text(&d["dhcp_end"])), err("end"), "it fld dh", 15, Some("decimal")))
                (text_row("lease", "Lease Time (seconds)", &typed("lease", text(&d["dhcp_lease_time"])), err("lease"), "it fld dh", 10, None))
            }))
            (kit::section(None, None, html! {
                li { button.it.cv.snd type="submit" { "Apply" } }
            }))
        }
    }
}

/// The LAN form, checked (no device read).
pub fn check(pairs: &[(String, String)]) -> Result<Vals, Bad> {
    let get = |n: &str| field(pairs, n).unwrap_or("").trim().chars().take(16).collect::<String>();
    let (ip_s, mask_s, mut start_s, mut end_s, lease) = (get("ip"), get("mask"), get("start"), get("end"), get("lease"));
    let dhcp = checkbox(pairs, "dhcp");
    let mut errors: Vec<(&'static str, String)> = Vec::new();
    let a = ip(&ip_s);
    let m = mask(&mask_s);
    match a {
        _ if ip_s.is_empty() => errors.push(("ip", "LAN IP is required".into())),
        Some(a) if a.is_private() => {}
        _ => errors.push(("ip", "Enter a private IPv4 address, such as 192.168.0.1.".into())),
    }
    if m.is_none() {
        errors.push(("mask", "Enter a subnet mask such as 255.255.255.0.".into()));
    }
    if let (Some(a), Some(m)) = (a.filter(Ipv4Addr::is_private), m) {
        let (net, last) = (u32::from(a) & m, (u32::from(a) & m) | !m);
        let host = |x: u32| x != net && x != last && x & m == net;
        let (first, top) = (Ipv4Addr::from(net + 1), Ipv4Addr::from(last - 1));
        if !host(u32::from(a)) {
            errors.push(("ip", "This is the subnet\u{2019}s network or broadcast address; choose another.".into()));
        }
        let range = format!("Use an address from {first} to {top}.");
        // DHCP off: a hidden range left in the old subnet keeps its host
        // numbers in the new one.
        if dhcp != "1" {
            for s in [&mut start_s, &mut end_s] {
                if let Some(x) = ip(s).map(u32::from).filter(|x| !host(*x)) {
                    let moved = net | (x & !m);
                    if host(moved) {
                        *s = Ipv4Addr::from(moved).to_string();
                    }
                }
            }
        }
        let mut ends = [None, None];
        for (i, (name, s)) in [("start", &start_s), ("end", &end_s)].into_iter().enumerate() {
            match ip(s) {
                Some(x) if host(u32::from(x)) => ends[i] = Some(u32::from(x)),
                Some(_) => errors.push((name, range.clone())),
                None => errors.push((name, "Enter an IPv4 address, such as 192.168.0.100.".into())),
            }
        }
        if let [Some(s), Some(e)] = ends {
            if s > e {
                errors.push(("end", "The end address must not come before the start address.".into()));
            } else if (s..=e).contains(&u32::from(a)) {
                errors.push(("start", "The range must not include the router\u{2019}s address.".into()));
            }
        }
    }
    if !lease_secs(&lease).is_some_and(|s| (120..=31_536_000).contains(&s)) {
        errors.push(("lease", "Enter seconds (120 or more), or a time such as 12h.".into()));
    }
    let vals: Vals = vec![("ip", ip_s), ("mask", mask_s), ("dhcp", dhcp), ("start", start_s), ("end", end_s), ("lease", lease)];
    if errors.is_empty() {
        Ok(vals)
    } else {
        Err(Bad::Fields(FormState { values: vals, errors }))
    }
}

/// The start's offset in the subnet (UCI's `start`, as the agent reads it
/// and the app sends it back); the checked form's start is inside it.
fn offset(vals: &[(&'static str, String)]) -> String {
    match (mask(val(vals, "mask")), ip(val(vals, "start"))) {
        (Some(m), Some(s)) => (u32::from(s) & !m).to_string(),
        _ => val(vals, "start").to_owned(),
    }
}

/// The app's keys (`dhcp_start` the offset), plus the firmware's for
/// `router_set_lan_para` (`ignore` an integer, the range as addresses).
fn body(vals: &[(&'static str, String)]) -> Value {
    let on = val(vals, "dhcp") == "1";
    let (ip, mask, start, end, lease) = (val(vals, "ip"), val(vals, "mask"), val(vals, "start"), val(vals, "end"), val(vals, "lease"));
    json!({
        "lan_ipaddr": ip, "lan_netmask": mask, "dhcp_enable": if on { "1" } else { "0" },
        "dhcp_start": offset(vals), "dhcp_end": end, "dhcp_lease_time": lease,
        "ipaddr": ip, "netmask": mask, "ignore": i64::from(!on), "zte_start": start, "zte_end": end, "leasetime": lease,
    })
}

pub fn question(app: &AppState, vals: &[(&'static str, String)], port: &str, remote: bool) -> Question {
    let cur = match super::api_get(app, API) {
        Ok(c) => c,
        Err(e) => return nothing("Failed to load LAN", vec![fmt::truncate(&e, 120)]),
    };
    let (old, new) = (text(&cur["lan_ipaddr"]), val(vals, "ip"));
    let start0 = start_address(&text(&cur["dhcp_start"]), &old, &text(&cur["lan_netmask"]));
    let unchanged = old == new
        && text(&cur["lan_netmask"]) == val(vals, "mask")
        && truthy(&cur["dhcp_enable"]) == (val(vals, "dhcp") == "1")
        && start0 == val(vals, "start")
        && text(&cur["dhcp_end"]) == val(vals, "end")
        && text(&cur["dhcp_lease_time"]) == val(vals, "lease");
    if unchanged {
        return nothing("Nothing to apply", vec!["These are the router\u{2019}s settings already.".into()]);
    }
    let mut lines = Vec::new();
    if old != new && remote {
        lines.push(format!("The router\u{2019}s address on its network changes from {old} to {new}."));
    } else if old != new {
        lines.push(format!(
            "The router\u{2019}s address changes from {old} to {new}. This page stops working at the old address: open http://{new}{port}/ui/ afterwards and log in again."
        ));
    }
    if val(vals, "dhcp") != "1" {
        lines.push("The router stops handing out addresses: devices that join afterwards need an address set by hand.".into());
    } else if old != new || text(&cur["lan_netmask"]) != val(vals, "mask") || start0 != val(vals, "start") || text(&cur["dhcp_end"]) != val(vals, "end") {
        lines.push("Devices get addresses from the new range when they reconnect.".into());
    }
    lines.push(if remote {
        "The router\u{2019}s network restarts: every device on its WiFi or USB disconnects for a moment.".into()
    } else {
        "The router\u{2019}s network restarts: every device on its WiFi or USB disconnects for a moment, this browser too.".into()
    });
    asking("Apply LAN settings?", lines, "Apply", remote, true)
}

pub fn send(app: &AppState, vals: &[(&'static str, String)], claim: &mut Claim) -> Result<Done, (u16, String)> {
    let moved = super::api_get(app, API).map(|c| text(&c["lan_ipaddr"]) != val(vals, "ip")).map_err(|e| (503, failed(&e)))?;
    claim.disarm();
    super::call(app, Method::Put, API, &body(vals)).map_err(|(status, e)| (status, failed(&e)))?;
    Ok(Done::Page(if moved { vec![("moved", "1".to_owned())] } else { Vec::new() }))
}

/// After an Apply (from the sent draft): the new address, as a link
/// (another origin: a new login there).
pub fn applied(vals: &[(&'static str, String)], port: &str, remote: bool) -> (String, Markup) {
    let new = val(vals, "ip");
    let moved = val(vals, "moved") == "1";
    let link = format!("http://{new}{port}/ui/");
    let body = html! {
        p.msg.ok role="status" { "LAN settings updated" }
        div.narrow {
            @if moved && !new.is_empty() {
                ul.consequences {
                    li { "The router\u{2019}s address is now " b { (new) } "." }
                    li { "If this device was on the router\u{2019}s WiFi or USB, it may need to reconnect first. Then open the web UI at its new address and log in again." }
                    @if remote { li { "Over Tailscale the router keeps its own address, so this page usually keeps working." } }
                }
                p.actions {
                    a.btn href=(link) rel="noreferrer" { "Open " (new) }
                    a.btn.btn-quiet href=(PATH) { "Back to LAN / DHCP" }
                }
            } @else {
                ul.consequences { li { "Devices on the router may disconnect for a moment and get an address from the new range when they reconnect." } }
                p.actions { a.btn.btn-quiet href=(PATH) { "Back to LAN / DHCP" } }
            }
        }
    };
    ("LAN settings updated".to_owned(), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(q: &str) -> Vec<(String, String)> {
        crate::web::session::Form::parse(q.as_bytes()).pairs().to_vec()
    }

    fn errors(q: &str) -> Vec<(&'static str, String)> {
        match check(&pairs(q)) {
            Err(Bad::Fields(fs)) => fs.errors,
            Ok(_) => Vec::new(),
            Err(Bad::Request) => panic!("bad request"),
        }
    }

    const OK: &str = "ip=192.168.8.1&mask=255.255.255.0&dhcp=1&start=192.168.8.100&end=192.168.8.200&lease=43200";

    #[test]
    fn addresses_and_ranges_are_checked() {
        assert!(errors(OK).is_empty());
        assert!(errors(&OK.replace("43200", "12h")).is_empty());
        assert_eq!(errors(&OK.replace("ip=192.168.8.1", "ip=")), [("ip", "LAN IP is required".to_owned())]);
        for (q, field) in [
            (OK.replace("192.168.8.1&", "8.8.8.8&"), "ip"),
            (OK.replace("ip=192.168.8.1", "ip=192.168.8.0"), "ip"),
            (OK.replace("255.255.255.0", "255.0.255.0"), "mask"),
            (OK.replace("255.255.255.0", "255.255.255.254"), "mask"),
            (OK.replace("start=192.168.8.100", "start=192.168.9.100"), "start"),
            (OK.replace("end=192.168.8.200", "end=192.168.8.255"), "end"),
            (OK.replace("end=192.168.8.200", "end=192.168.8.50"), "end"),
            (OK.replace("start=192.168.8.100", "start=192.168.8.1"), "start"),
            (OK.replace("lease=43200", "lease=60"), "lease"),
            (OK.replace("lease=43200", "lease=12x"), "lease"),
            (OK.replace("start=192.168.8.100", "start=192.168.008.100"), "start"),
        ] {
            let e = errors(&q);
            assert!(e.iter().any(|(k, _)| *k == field), "{q}: {e:?}");
        }
        assert_eq!(start_address("100", "192.168.0.1", "255.255.255.0"), "192.168.0.100");
        assert_eq!(start_address("192.168.0.20", "192.168.0.1", "255.255.255.0"), "192.168.0.20");
        assert_eq!(start_address("5", "10.1.2.3", "255.255.0.0"), "10.1.0.5");
        assert_eq!(lease_secs("1d"), Some(86_400));
        assert_eq!(lease_secs("h"), None);
        let b = body(&check(&pairs(OK)).unwrap());
        assert_eq!(b, json!({"lan_ipaddr": "192.168.8.1", "lan_netmask": "255.255.255.0", "dhcp_enable": "1",
                             "dhcp_start": "100", "dhcp_end": "192.168.8.200", "dhcp_lease_time": "43200",
                             "ipaddr": "192.168.8.1", "netmask": "255.255.255.0", "ignore": 0,
                             "zte_start": "192.168.8.100", "zte_end": "192.168.8.200", "leasetime": "43200"}));
        let off = body(&check(&pairs(&OK.replace("dhcp=1&", ""))).unwrap());
        assert_eq!((&off["dhcp_enable"], &off["ignore"]), (&json!("0"), &json!(1)));
    }

    /// What is checked is what is sent: no spaces kept, no leading zeros.
    #[test]
    fn addresses_are_sent_as_checked() {
        for padded in ["ip=192.168.8.1%20%20%20%20%20x", "ip=%20192.168.8.1%20%20%20%20%20%20%20%20%20%20%20%20"] {
            let q = OK.replace("ip=192.168.8.1", padded);
            match check(&pairs(&q)) {
                Ok(v) => assert_eq!(val(&v, "ip"), "192.168.8.1", "{q}"),
                Err(Bad::Fields(fs)) => assert!(fs.errors.iter().any(|(k, _)| *k == "ip"), "{q}"),
                Err(Bad::Request) => panic!("bad request"),
            }
        }
        assert!(errors(&OK.replace("ip=192.168.8.1", "ip=192.168.8.1%20%20%20%20%20x")).iter().any(|(k, _)| *k == "ip"));
        assert_eq!(ip("192.168.8.1 "), None);
    }

    /// DHCP off: the range is hidden; one left in the old subnet moves into
    /// the new one with its host numbers, else it is refused (and shown).
    #[test]
    fn a_hidden_range_follows_a_new_subnet() {
        let q = "ip=10.1.2.1&mask=255.255.255.0&start=192.168.0.100&end=192.168.0.149&lease=12h";
        let v = check(&pairs(q)).unwrap();
        assert_eq!((val(&v, "start"), val(&v, "end"), val(&v, "dhcp")), ("10.1.2.100", "10.1.2.149", ""));
        // DHCP on: the visible range is refused, never moved.
        assert_eq!(errors(&format!("{q}&dhcp=1")).len(), 2);
        // A range that can't fit the new subnet is refused either way.
        assert!(!errors("ip=10.1.2.1&mask=255.255.255.252&start=192.168.0.100&end=192.168.0.149&lease=12h").is_empty());
    }
}
