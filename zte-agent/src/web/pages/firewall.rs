//! Router › Firewall (iOS `FirewallSettingsView`, `PortForwardFormView`):
//! the firewall's switch and level, WAN Ping, NAT, UPnP, DMZ, port
//! forwarding (its switch, the rules, New Port Forward, a rule's delete)
//! and the MAC/IP/Port filters (read-only, as the app's), read at page load
//! (no stream). Every change is asked first (`pages::netcfg`; the app asks
//! none): each can open devices to the internet or cut access to them.
//!
//! - Reads under the app's names, else the firmware's (its `ubus -v list`
//!   and UCI options: `firewall_enable`, `nat_enable`, `dmz_enable`,
//!   `level`, `wan_ping_enable`, `portforward_enable`, `enable_upnp`; a
//!   rule's `section_id`, `comment`, `proto`, `src_dport`, `dest_ip`,
//!   `dest_port`). The bodies carry both: the app's keys, plus the
//!   firmware's for the same setting where `ubus -v list` names it
//!   (`router_set_firewall_switch {"enable"}`, `router_set_portforward
//!   {"action","section_id","src_dport","proto","dest_ip","enabled",
//!   "comment"}` …), its integers as integers. Not verified on the device.
//! - A rule's id is the firmware's, else its place in the list (the app's
//!   fallback), so a delete names the list it was chosen from (`g`) and
//!   sends nothing once the list has changed (the app's "The rule list was
//!   out of date and has been reloaded. Try again.").
//! - Checks (web additions; the app only needs the ports and the address
//!   typed): ports 1–65535; the LAN IP and the DMZ host a device's address
//!   on the router's network (not the router's own, which would open its
//!   own services to the internet); a WAN port another rule forwards for
//!   the same protocol is refused; a rule's name up to 32 letters, digits,
//!   spaces, dots, hyphens and underscores.
//! - Filter rules' MAC addresses show only their last 2 octets.

use std::net::Ipv4Addr;

use maud::{html, Markup};
use serde_json::{json, Value};

use super::confirm::{asking, nothing, Bad, Question};
use super::lan;
use super::netcfg::{check as checkbox, failed, field, first, form_error, generation, rule_id, select_row, switch, text, text_row, truthy, val, Done, Kind, Vals};
use super::wifi::mask_mac;
use crate::handlers::AppState;
use crate::server::Method;
use crate::web::fmt;
use crate::web::kit::{self, Act, Icon};
use crate::web::render::once_field;
use crate::web::screen::{FormState, PageData};
use crate::web::session::percent_encode;
use crate::web::store::Claim;

pub const PATH: &str = "/ui/router/firewall";
pub const SWITCH: &str = "/ui/router/firewall/switch";
pub const LEVEL: &str = "/ui/router/firewall/level";
pub const NAT: &str = "/ui/router/firewall/nat";
pub const UPNP: &str = "/ui/router/firewall/upnp";
pub const DMZ: &str = "/ui/router/firewall/dmz";
pub const PF_SWITCH: &str = "/ui/router/firewall/port-forward/switch";
pub const NEW: &str = "/ui/router/firewall/port-forward/new";
pub const PF_ADD: &str = "/ui/router/firewall/port-forward/add";
pub const PF_DELETE: &str = "/ui/router/firewall/port-forward/delete";

pub const API: &str = "/api/router/firewall";
pub const PF_API: &str = "/api/router/firewall/port-forward";
pub const FILTER_API: &str = "/api/router/firewall/filter-rules";
pub const UPNP_API: &str = "/api/router/firewall/upnp";
const LAN_API: &str = "/api/router/lan";

/// The app's levels (value, label).
const LEVELS: [(&str, &str); 3] = [("low", "Low"), ("medium", "Medium"), ("high", "High")];
/// The app's protocols (value, label).
const PROTOCOLS: [(&str, &str); 3] = [("tcp", "TCP"), ("udp", "UDP"), ("tcp+udp", "TCP + UDP")];

const LOCK: &str = "Connections the firewall doesn\u{2019}t allow are blocked, which can cut access to the router or to devices on it.";
/// The remote line where the firewall may block this browser's own way in.
const REMOTE_LOCK: &str = "You are connected remotely: if the firewall blocks this connection, change it back from a device on the router\u{2019}s Wi-Fi.";
const STALE: &str = "The rule list was out of date and has been reloaded. Try again.";
/// Less filtering: the agent refuses the mobile side itself
/// (`httpd::mobile`), the router's other services do not.
const EXPOSED: &str = "The router\u{2019}s other services, such as SSH on port 2222 and the firmware\u{2019}s own web pages, may become reachable from the mobile network. This web UI and the agent\u{2019}s API keep refusing connections from it.";
/// The same while the agent serves that side too (`ZTE_AGENT_ALLOW_MOBILE=1`,
/// or its interface lookup fails).
const EXPOSED_ALL: &str = "The router itself, this web UI included, may become reachable from the mobile network; only the agent password then protects it.";

/// What less filtering exposes.
fn exposed() -> &'static str {
    exposed_for(crate::httpd::mobile::mobile_served())
}

/// `served`: the agent serves the mobile side now.
fn exposed_for(served: bool) -> &'static str {
    if served {
        EXPOSED_ALL
    } else {
        EXPOSED
    }
}

/// The app's `FirewallConfig`.
struct Config {
    enabled: bool,
    nat: bool,
    dmz: bool,
    dmz_ip: String,
    level: String,
    ping_blocked: bool,
    pf: bool,
}

impl Config {
    fn of(d: &Value) -> Config {
        let on = |keys: &[&str]| truthy(first(d, keys));
        let level = text(first(d, &["firewall_level", "level"]));
        Config {
            enabled: on(&["firewall_switch", "firewall_enable"]),
            nat: on(&["nat_switch", "nat_enable"]),
            dmz: on(&["dmz_enabled", "dmz_enable"]),
            dmz_ip: text(&d["dmz_ip"]),
            // The app's default.
            level: if level.is_empty() { "medium".into() } else { level },
            // The app's `wan_ping_filter` (on: blocked), else the firmware's
            // `wan_ping_enable` (on: answered; inferred).
            ping_blocked: match &d["wan_ping_filter"] {
                Value::Null => d.get("wan_ping_enable").is_some_and(|v| !truthy(v)),
                v => truthy(v),
            },
            pf: on(&["port_forward_switch", "portforward_enable"]),
        }
    }
}

fn level_label(v: &str) -> &str {
    LEVELS.iter().find(|(k, _)| *k == v).map_or(v, |(_, l)| l)
}

/// A port forwarding rule (the app's `PortForwardRule`).
struct Rule {
    id: String,
    name: String,
    proto: String,
    wan: String,
    ip: String,
    lan: String,
    enabled: bool,
}

impl Rule {
    /// The app's row title.
    fn title(&self) -> &str {
        if self.name.is_empty() {
            "Rule"
        } else {
            &self.name
        }
    }

    /// The app's "{PROTO} WAN:{port} → {lanIP}:{lanPort}".
    fn route(&self) -> String {
        format!("{} WAN:{} \u{2192} {}:{}", self.proto.to_uppercase(), self.wan, self.ip, self.lan)
    }
}

fn rules(list: &Value) -> Vec<Rule> {
    let rows = list["rule_list"].as_array().map(Vec::as_slice).unwrap_or_default();
    rows.iter()
        .enumerate()
        .map(|(i, r)| {
            let s = |keys: &[&str]| text(first(r, keys));
            Rule {
                id: rule_id(r, i),
                name: s(&["name", "comment"]),
                proto: s(&["protocol", "proto"]),
                wan: s(&["wan_port", "src_dport"]),
                ip: s(&["lan_ip", "dest_ip"]),
                lan: s(&["lan_port", "dest_port"]),
                enabled: truthy(first(r, &["enabled", "enable"])),
            }
        })
        .collect()
}

/// Whether a protocol covers TCP and UDP (anything else: both).
fn protocols(p: &str) -> (bool, bool) {
    let p = p.to_ascii_lowercase();
    match (p.contains("tcp"), p.contains("udp")) {
        (false, false) => (true, true),
        both => both,
    }
}

fn rule_row(r: &Rule, g: &str) -> Markup {
    let href = format!("{PF_DELETE}?id={}&g={g}", percent_encode(&r.id));
    html! {
        li.it.pf {
            span.cvb {
                span.l1 { b.nm { (r.title()) } @if r.enabled { small.c-green { "Active" } } @else { small { "Inactive" } } }
                small { (r.route()) }
            }
            // The app's swipe to delete (asked first on the web).
            a.del href=(href) aria-label=(format!("Delete {}", r.title())) { (kit::icon(Icon::Trash)) }
        }
    }
}

/// The MAC/IP/Port filters (read-only, as the app's), MACs masked.
fn filter_rows(list: &Value) -> Markup {
    let rows = list["rule_list"].as_array().map(Vec::as_slice).unwrap_or_default();
    html! {
        @for r in rows {
            @let s = |keys: &[&str]| text(first(r, keys));
            @let (mac, src, dst) = (s(&["src_mac"]), s(&["src_ip"]), s(&["dest_ip"]));
            li.it {
                span.cvb {
                    span.l1 {
                        b.nm { (s(&["protocol", "proto"]).to_uppercase()) }
                        @if truthy(first(r, &["enabled", "enable"])) { small.c-green { "Active" } } @else { small { "Inactive" } }
                    }
                    @if !mac.is_empty() { small { "MAC: " (mask_mac(&mac)) } }
                    @if !src.is_empty() || !dst.is_empty() {
                        small { (format!("{src}:{} \u{2192} {dst}:{}", s(&["src_port"]), s(&["dest_port"]))) }
                    }
                }
            }
        }
    }
}

fn on_link(path: &str, on: bool) -> String {
    format!("{path}?on={}", u8::from(on))
}

pub fn page(pd: &PageData) -> Markup {
    let d = match pd.reads.first() {
        Some(Ok(d)) => d,
        Some(Err(e)) => return html! { p.msg.err role="alert" { "Failed to load firewall: " (fmt::truncate(e, 120)) } },
        None => return html! {},
    };
    let c = Config::of(d);
    // A failed read: "Unavailable" with both directions (the app shows off).
    let upnp = pd.read(3).map(|u| truthy(first(u, &["upnp_switch", "enable_upnp"])));
    let f = pd.form;
    let dmz_on = f.map_or(c.dmz, |f| f.value("dmz") == Some("1"));
    let dmz_ip = f.and_then(|f| f.value("ip")).map_or_else(|| c.dmz_ip.clone(), str::to_owned);
    let filters = pd.read(2).filter(|l| l["rule_list"].as_array().is_some_and(|a| !a.is_empty()));
    let none = html! {};
    html! {
        (kit::section(Some("Firewall"), None, html! {
            li.it { span { "Status" } @if c.enabled { span.c-green { "Enabled" } } @else { span.v { "Disabled" } } }
            li { a.it.bt href=(on_link(SWITCH, !c.enabled)) { span { (if c.enabled { "Disable Firewall" } else { "Enable Firewall" }) } } }
            @if c.enabled {
                // The app's picker: each level a link to its question.
                li.it.lv {
                    span { "Level" }
                    span.seg role="group" aria-label="Level" {
                        @for (v, l) in LEVELS {
                            @if v == c.level { span aria-current="true" { (l) } } @else { a href=(format!("{LEVEL}?level={v}")) { (l) } }
                        }
                    }
                }
            }
            (kit::kv_row(None, "WAN Ping", if c.ping_blocked { "Blocked" } else { "Allowed" }))
        }))
        (kit::section(Some("NAT / UPnP"), None, html! {
            (kit::switch_row(&none, "NAT", Some(c.nat), Act::Link(&on_link(NAT, true)), Act::Link(&on_link(NAT, false))))
            (kit::switch_row(&none, "UPnP", upnp, Act::Link(&on_link(UPNP, true)), Act::Link(&on_link(UPNP, false))))
        }))
        (form_error(pd))
        form.rf.dmz method="post" action=(DMZ) autocomplete="off" {
            (pd.csrf())
            (once_field(&pd.once()))
            (kit::section(Some("DMZ"), None, html! {
                (switch("dmz", "Enable DMZ", dmz_on, "it", "sr"))
                // Shown while Enable DMZ is on (CSS; without `:has()` always).
                (text_row("ip", "DMZ Host IP", &dmz_ip, f.and_then(|f| f.error("ip")), "it fld dip", 15, Some("decimal")))
                li { button.it.cv.snd type="submit" { "Apply DMZ" } }
            }))
        }
        (kit::section(Some("Port Forwarding"), None, html! {
            (kit::switch_row(&none, "Enable", Some(c.pf), Act::Link(&on_link(PF_SWITCH, true)), Act::Link(&on_link(PF_SWITCH, false))))
            @if c.pf {
                // The app's button (blue, no chevron).
                li { a.it.bt href=(NEW) { (kit::icon(Icon::Plus)) span { "Add Rule" } } }
                @match pd.reads.get(1) {
                    Some(Ok(list)) => {
                        @let g = generation(list);
                        @for r in rules(list) { (rule_row(&r, &g)) }
                    }
                    Some(Err(e)) => li.it.off { span { "Failed to load port forwarding rules: " (fmt::truncate(e, 120)) } },
                    None => {}
                }
            }
        }))
        @if let Some(list) = filters {
            (kit::section(Some("MAC/IP/Port Filters"), None, filter_rows(list)))
        }
    }
}

/// New Port Forward (the app's sheet; "Cancel" leads back).
pub fn rule_form(pd: &PageData) -> Markup {
    let f = pd.form;
    let typed = |n: &str| f.and_then(|f| f.value(n)).unwrap_or("").to_owned();
    let err = |n: &str| f.and_then(|f| f.error(n));
    let protos: Vec<(String, String)> = PROTOCOLS.iter().map(|(v, l)| ((*v).to_owned(), (*l).to_owned())).collect();
    let proto = f.and_then(|f| f.value("proto")).unwrap_or("tcp");
    html! {
        (form_error(pd))
        form.rf.pff method="post" action=(PF_ADD) autocomplete="off" {
            (pd.csrf())
            (once_field(&pd.once()))
            (kit::section(Some("Rule"), None, html! {
                (text_row("pfname", "Name", &typed("pfname"), err("pfname"), "it fld", 32, None))
                (select_row("proto", "Protocol", &protos, proto, None, "it"))
            }))
            (kit::section(Some("WAN"), None, text_row("wan", "WAN Port", &typed("wan"), err("wan"), "it fld", 5, Some("numeric"))))
            (kit::section(Some("LAN"), None, html! {
                (text_row("ip", "LAN IP", &typed("ip"), err("ip"), "it fld", 15, Some("decimal")))
                (text_row("lan", "LAN Port", &typed("lan"), err("lan"), "it fld", 5, Some("numeric")))
            }))
            (kit::section(None, None, html! { li { button.it.cv.snd type="submit" { "Add Rule" } } }))
        }
    }
}

// --- Changes (`pages::netcfg`) ---

pub fn level_fields(pairs: &[(String, String)]) -> Result<Vals, Bad> {
    match field(pairs, "level") {
        Some(v) if LEVELS.iter().any(|(k, _)| *k == v) => Ok(vec![("level", v.to_owned())]),
        _ => Err(Bad::Request),
    }
}

/// A port: digits, 1–65535.
fn port(s: &str) -> Option<u16> {
    if s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u16>().ok().filter(|p| *p >= 1)
}

/// The router's address and subnet mask, if they can be read.
fn lan_net(app: &AppState) -> Option<(Ipv4Addr, u32)> {
    let d = super::api_get(app, LAN_API).ok()?;
    Some((lan::ip(&text(&d["lan_ipaddr"]))?, lan::mask(&text(&d["lan_netmask"]))?))
}

/// Why `s` is not a device's address on the router's network (its subnet,
/// not the router's own, the network's or the broadcast address). Without
/// the subnet nothing is taken: the router's own address could not be told
/// apart (it would open the router's own services to the internet).
fn host_error(net: Option<(Ipv4Addr, u32)>, s: &str) -> Option<String> {
    let Some(a) = lan::ip(s) else { return Some("Enter an IPv4 address, such as 192.168.0.50.".into()) };
    let Some((router, m)) = net else {
        return Some("Couldn\u{2019}t read the router\u{2019}s network. Try again.".into());
    };
    let (x, net) = (u32::from(a), u32::from(router) & m);
    let last = net | !m;
    if a == router {
        Some("This is the router\u{2019}s own address; use a device\u{2019}s address.".into())
    } else if x & m != net || x == net || x == last {
        Some(format!("Use an address from {} to {}.", Ipv4Addr::from(net + 1), Ipv4Addr::from(last - 1)))
    } else {
        None
    }
}

/// The DMZ form, checked against the router's network (read now).
pub fn check_dmz(app: &AppState, pairs: &[(String, String)]) -> Result<Vals, Bad> {
    let on = checkbox(pairs, "dmz");
    let typed: String = field(pairs, "ip").unwrap_or("").trim().chars().take(16).collect();
    let mut errors: Vec<(&'static str, String)> = Vec::new();
    if on == "1" {
        if typed.is_empty() {
            errors.push(("ip", "Enter the IPv4 address of the DMZ host.".into()));
        } else if let Some(e) = host_error(lan_net(app), &typed) {
            errors.push(("ip", e));
        }
    }
    // Off: the field is hidden (as the app's); sent as typed when it is an
    // address, as the app sends what it shows.
    let ip = if on == "1" || lan::ip(&typed).is_some() { typed } else { String::new() };
    let vals: Vals = vec![("dmz", on), ("ip", ip)];
    if errors.is_empty() {
        Ok(vals)
    } else {
        Err(Bad::Fields(FormState { values: vals, errors }))
    }
}

/// New Port Forward's form, checked against the router's network and the
/// rules (read now; a list that can't be read checks nothing against it).
pub fn check_rule(app: &AppState, pairs: &[(String, String)]) -> Result<Vals, Bad> {
    let proto = match field(pairs, "proto") {
        Some(p) if PROTOCOLS.iter().any(|(v, _)| *v == p) => p.to_owned(),
        _ => return Err(Bad::Request),
    };
    let get = |n: &str, max: usize| field(pairs, n).unwrap_or("").trim().chars().take(max + 1).collect::<String>();
    let (name, wan, ip, lan) = (get("pfname", 32), get("wan", 6), get("ip", 16), get("lan", 6));
    let mut errors: Vec<(&'static str, String)> = Vec::new();
    if name.chars().count() > 32 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b" .-_".contains(&b)) {
        errors.push(("pfname", "Use up to 32 letters, digits, spaces, dots, hyphens and underscores.".into()));
    }
    let ports = [("wan", &wan), ("lan", &lan)].map(|(k, v)| {
        let p = port(v);
        if p.is_none() {
            errors.push((k, "Enter a port from 1 to 65535.".into()));
        }
        p
    });
    if ip.is_empty() {
        errors.push(("ip", "Enter the IPv4 address of the device.".into()));
    } else if let Some(e) = host_error(lan_net(app), &ip) {
        errors.push(("ip", e));
    }
    if let (Some(w), Ok(list)) = (ports[0], super::api_get(app, PF_API)) {
        let (t, u) = protocols(&proto);
        let taken = rules(&list).iter().any(|r| {
            let (rt, ru) = protocols(&r.proto);
            port(&r.wan) == Some(w) && ((t && rt) || (u && ru))
        });
        if taken {
            errors.push(("wan", format!("WAN port {w} is already forwarded.")));
        }
    }
    let shown = |p: Option<u16>, typed: &str| p.map_or_else(|| typed.chars().take(6).collect(), |p| p.to_string());
    let vals: Vals = vec![
        ("pfname", name.chars().take(32).collect()),
        ("proto", proto),
        ("wan", shown(ports[0], &wan)),
        ("ip", ip.chars().take(16).collect()),
        ("lan", shown(ports[1], &lan)),
    ];
    if errors.is_empty() {
        Ok(vals)
    } else {
        Err(Bad::Fields(FormState { values: vals, errors }))
    }
}

fn config(app: &AppState) -> Result<Config, Question> {
    super::api_get(app, API).map(|d| Config::of(&d)).map_err(|e| nothing("Failed to load firewall", vec![fmt::truncate(&e, 120)]))
}

pub fn question(kind: Kind, app: &AppState, vals: &[(&'static str, String)], remote: bool) -> Question {
    let on = val(vals, "on") == "1";
    // `locks`: the change can block this browser's own connection.
    let ask_lines = |title: String, mut lines: Vec<String>, label: &'static str, locks: bool| {
        if remote && locks {
            lines.push(REMOTE_LOCK.into());
        }
        asking(title, lines, label, false, false)
    };
    let ask = |title: &str, lines: &[&str], label: &'static str, locks: bool| {
        ask_lines(title.to_owned(), lines.iter().map(|l| (*l).to_owned()).collect(), label, locks)
    };
    match kind {
        Kind::FwSwitch if on => ask("Turn on the firewall?", &["Incoming connections are filtered again.", LOCK], "Enable Firewall", true),
        Kind::FwSwitch => ask(
            "Turn off the firewall?",
            &["Devices on the router lose the firewall\u{2019}s protection from the internet.", exposed()],
            "Disable Firewall",
            false,
        ),
        Kind::FwLevel => {
            let c = match config(app) {
                Ok(c) => c,
                Err(q) => return q,
            };
            let level = val(vals, "level");
            let label = level_label(level);
            let rank = |l: &str| LEVELS.iter().position(|(k, _)| *k == l).unwrap_or(1);
            if level == c.level {
                return nothing(format!("The firewall level is already {label}"), Vec::new());
            }
            let up = rank(level) > rank(&c.level);
            let lines = if up {
                vec![format!("The firewall blocks more connections at {label}."), LOCK.into()]
            } else {
                vec![format!("The firewall lets more connections from the internet through at {label}."), exposed().into()]
            };
            ask_lines(format!("Set the firewall level to {label}?"), lines, "Set Level", up)
        }
        Kind::FwNat if on => ask("Turn on NAT?", &["Devices on the router share its mobile connection through NAT again."], "Turn On NAT", false),
        Kind::FwNat => ask(
            "Turn off NAT?",
            &["Devices on the router lose internet access: without NAT they can\u{2019}t share the router\u{2019}s mobile connection."],
            "Turn Off NAT",
            false,
        ),
        Kind::FwUpnp if on => ask("Turn on UPnP?", &["Devices on the router can open ports to the internet by themselves."], "Turn On UPnP", false),
        Kind::FwUpnp => ask(
            "Turn off UPnP?",
            &["Ports that devices opened through UPnP close: games and apps that rely on them may stop accepting connections."],
            "Turn Off UPnP",
            false,
        ),
        Kind::FwPfSwitch if on => {
            let what = match super::api_get(app, PF_API).map(|l| rules(&l).len()) {
                Ok(0) => "Rules you add open ports from the internet to devices on the router.".to_owned(),
                Ok(1) => "1 rule opens a port from the internet to a device on the router.".to_owned(),
                Ok(n) => format!("{n} rules open ports from the internet to devices on the router."),
                Err(_) => "Its rules open ports from the internet to devices on the router.".to_owned(),
            };
            ask_lines("Turn on port forwarding?".into(), vec![what], "Turn On Port Forwarding", false)
        }
        Kind::FwPfSwitch => ask(
            "Turn off port forwarding?",
            &["The forwarded ports close: what devices serve through them can\u{2019}t be reached from the internet."],
            "Turn Off Port Forwarding",
            false,
        ),
        Kind::FwDmz => {
            let c = match config(app) {
                Ok(c) => c,
                Err(q) => return q,
            };
            let (on, ip) = (val(vals, "dmz") == "1", val(vals, "ip"));
            if on == c.dmz && (!on || ip == c.dmz_ip) {
                let line = if on { format!("{ip} is already the DMZ host.") } else { "DMZ is already off.".to_owned() };
                return nothing("Nothing to apply", vec![line]);
            }
            if on {
                let mut lines = vec![format!("Connections from the internet that no other rule takes go to {ip}: every port of that device can be reached from the internet.")];
                if c.dmz && !c.dmz_ip.is_empty() {
                    lines.push(format!("{} stops receiving them.", c.dmz_ip));
                }
                ask_lines(format!("Make {ip} the DMZ host?"), lines, "Apply DMZ", false)
            } else {
                let host = if c.dmz_ip.is_empty() { "The DMZ host" } else { c.dmz_ip.as_str() };
                ask_lines("Turn off DMZ?".into(), vec![format!("{host} no longer receives connections from the internet that no other rule takes.")], "Apply DMZ", false)
            }
        }
        Kind::FwPfAdd => {
            let proto = PROTOCOLS.iter().find(|(v, _)| *v == val(vals, "proto")).map_or("", |(_, l)| l);
            let (wan, ip, lan) = (val(vals, "wan"), val(vals, "ip"), val(vals, "lan"));
            let mut lines = vec![format!("{proto} WAN port {wan} goes to {ip}:{lan}: that port of the device can be reached from the internet.")];
            if super::api_get(app, API).is_ok_and(|d| !Config::of(&d).pf) {
                lines.push("Port forwarding is off: the rule applies once it is turned on.".into());
            }
            let name = val(vals, "pfname");
            let title = if name.is_empty() { "Add this port forward?".to_owned() } else { format!("Add \u{201c}{name}\u{201d}?") };
            ask_lines(title, lines, "Add Rule", false)
        }
        _ => {
            let list = match super::api_get(app, PF_API) {
                Ok(l) => l,
                Err(e) => return nothing("Failed to load port forwarding rules", vec![fmt::truncate(&e, 120)]),
            };
            if generation(&list) != val(vals, "g") {
                return nothing("The rule list changed", vec!["It changed since this page was loaded. Go back and choose the rule again.".into()]);
            }
            match rules(&list).into_iter().find(|r| r.id == val(vals, "id")) {
                None => nothing("Rule not found", vec!["It may have been deleted.".into()]),
                Some(r) => ask_lines(
                    format!("Delete \u{201c}{}\u{201d}?", r.title()),
                    vec![format!("{} stops: what it serves can\u{2019}t be reached from the internet any more.", r.route())],
                    "Delete",
                    false,
                ),
            }
        }
    }
}

pub fn send(kind: Kind, app: &AppState, vals: &[(&'static str, String)], claim: &mut Claim) -> Result<Done, (u16, String)> {
    let call = |method, path: &str, body: Value| super::call(app, method, path, &body).map_err(|(status, e)| (status, failed(&e)));
    let set = |on: bool| (if on { "1" } else { "0" }, i64::from(on), if on { "enabled" } else { "disabled" });
    let (s, n, state) = set(val(vals, "on") == "1");
    let line = |text: String| Ok(Done::Line(Some(true), text));
    match kind {
        Kind::FwSwitch => {
            claim.disarm();
            call(Method::Put, "/api/router/firewall/switch", json!({"firewall_switch": s, "enable": n}))?;
            line(format!("Firewall {state}"))
        }
        Kind::FwLevel => {
            let level = val(vals, "level");
            claim.disarm();
            call(Method::Put, "/api/router/firewall/level", json!({"firewall_level": level, "level": level}))?;
            line(format!("Firewall level set to {level}"))
        }
        Kind::FwNat => {
            claim.disarm();
            call(Method::Put, "/api/router/firewall/nat", json!({"nat_switch": s, "enable": n}))?;
            line(format!("NAT {state}"))
        }
        Kind::FwUpnp => {
            claim.disarm();
            call(Method::Put, UPNP_API, json!({"upnp_switch": s, "enable_upnp": n}))?;
            line(format!("UPnP {state}"))
        }
        Kind::FwDmz => {
            let (s, n, _) = set(val(vals, "dmz") == "1");
            claim.disarm();
            call(Method::Put, "/api/router/firewall/dmz", json!({"dmz_enabled": s, "dmz_ip": val(vals, "ip"), "dmz_enable": n}))?;
            line("DMZ settings updated".into())
        }
        Kind::FwPfSwitch => {
            claim.disarm();
            call(Method::Put, "/api/router/firewall/port-forward/switch", json!({"port_forward_switch": s, "portforward_enable": n}))?;
            line(format!("Port forwarding {state}"))
        }
        Kind::FwPfAdd => {
            let (name, proto, wan, ip) = (val(vals, "pfname"), val(vals, "proto"), val(vals, "wan"), val(vals, "ip"));
            let body = json!({
                "action": "add", "name": name, "protocol": proto, "wan_port": wan, "lan_ip": ip, "lan_port": val(vals, "lan"), "enabled": 1,
                "comment": name, "proto": proto, "src_dport": wan, "dest_ip": ip,
            });
            claim.disarm();
            call(Method::Post, PF_API, body)?;
            line("Port forward rule added".into())
        }
        _ => {
            let list = super::api_get(app, PF_API).map_err(|e| (503, failed(&e)))?;
            if generation(&list) != val(vals, "g") {
                return Err((409, STALE.into()));
            }
            let id = val(vals, "id");
            if !rules(&list).iter().any(|r| r.id == id) {
                return Err((404, "Rule not found".into()));
            }
            claim.disarm();
            call(Method::Post, PF_API, json!({"action": "delete", "id": id, "section_id": [id]}))?;
            line("Port forward rule deleted".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app's names first, then the firmware's; ids fall back to the
    /// place in the list; the generation follows the list.
    #[test]
    fn reads_both_names() {
        let c = Config::of(&json!({"firewall_enable": "1", "nat_enable": 0, "dmz_enable": "1", "dmz_ip": "192.168.0.50", "level": "high", "wan_ping_enable": "1"}));
        assert!(c.enabled && !c.nat && c.dmz && c.level == "high" && !c.ping_blocked && !c.pf);
        let c = Config::of(&json!({"firewall_switch": "0", "wan_ping_filter": "1", "port_forward_switch": "1"}));
        assert!(!c.enabled && c.level == "medium" && c.ping_blocked && c.pf);
        let list = json!({"rule_list": [
            {"name": "Web", "protocol": "tcp", "wan_port": "8080", "lan_ip": "192.168.0.50", "lan_port": "80", "enabled": "1"},
            {"section_id": "cfg0a1b2c", "comment": "Game", "proto": "udp", "src_dport": "3074", "dest_ip": "192.168.0.60"},
        ]});
        let r = rules(&list);
        assert_eq!((r[0].id.as_str(), r[0].route().as_str(), r[0].enabled), ("0", "TCP WAN:8080 \u{2192} 192.168.0.50:80", true));
        assert_eq!((r[1].id.as_str(), r[1].title(), r[1].route().as_str(), r[1].enabled), ("cfg0a1b2c", "Game", "UDP WAN:3074 \u{2192} 192.168.0.60:", false));
        assert_eq!(generation(&list), generation(&list.clone()));
        assert_ne!(generation(&list), generation(&json!({"rule_list": []})));
        assert_eq!(protocols("tcp+udp"), (true, true));
        assert_eq!(protocols("TCP"), (true, false));
        assert_eq!(protocols(""), (true, true));
    }

    /// The questions say this web UI stays out of reach only while the agent
    /// refuses the mobile side.
    #[test]
    fn exposure_follows_the_agents_refusal() {
        assert!(exposed_for(false) == EXPOSED && EXPOSED.contains("keep refusing connections from it"));
        assert!(exposed_for(true) == EXPOSED_ALL && EXPOSED_ALL.contains("this web UI included"));
    }

    #[test]
    fn hosts_and_ports() {
        let net = Some((Ipv4Addr::new(192, 168, 0, 1), 0xffff_ff00));
        assert_eq!(host_error(net, "192.168.0.50"), None);
        assert!(host_error(net, "192.168.0.1").unwrap().contains("router\u{2019}s own address"));
        assert_eq!(host_error(net, "192.168.1.5").unwrap(), "Use an address from 192.168.0.1 to 192.168.0.254.");
        assert!(host_error(net, "192.168.0.255").is_some() && host_error(net, "192.168.0.0").is_some());
        assert!(host_error(net, "nope").unwrap().starts_with("Enter an IPv4"));
        for unread in ["10.0.0.9", "192.168.0.1", "8.8.8.8"] {
            assert_eq!(host_error(None, unread).as_deref(), Some("Couldn\u{2019}t read the router\u{2019}s network. Try again."), "{unread}");
        }
        assert!(host_error(net, "192.168.0.50 ").unwrap().starts_with("Enter an IPv4"));
        assert_eq!(port("8080"), Some(8080));
        assert_eq!(port("080"), Some(80));
        for bad in ["", "0", "65536", "80-90", "+80", " 80", "123456"] {
            assert_eq!(port(bad), None, "{bad}");
        }
    }
}
