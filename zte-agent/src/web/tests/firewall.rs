//! Router › Firewall (with New Port Forward), Telemetry Blocker, VPN
//! Passthrough and QoS through the whole router: the pages, their
//! questions, the posts and where they lead. Page-load reads come from
//! `pages::device`'s `gets`, the changes go to `pages::device`. Every value
//! is a made-up fixture (documentation addresses, a locally administered
//! MAC).

use serde_json::json;

use super::*;
use crate::web::pages::device;

const FW: &str = "/ui/router/firewall";
const NEW: &str = "/ui/router/firewall/port-forward/new";
const ADD: &str = "/ui/router/firewall/port-forward/add";
const DEL: &str = "/ui/router/firewall/port-forward/delete";
const DMZ: &str = "/ui/router/firewall/dmz";
const TM: &str = "/ui/router/telemetry";
const VPN: &str = "/ui/router/vpn";
const QOS: &str = "/ui/router/qos";
const MAC: &str = "02:00:5e:10:3a:7f";

fn calls(c: &Ctx) -> Vec<(String, Value)> {
    device::calls(&c.app)
}

fn tailnet(port: u16) -> Peer {
    Peer(SocketAddr::from(([127, 0, 0, 1], port)))
}

/// `location` of a 303.
fn to(r: &Reply) -> String {
    assert_eq!(r.status, 303, "{}", r.text());
    r.header("location").unwrap().to_owned()
}

fn body(csrf: &str, once: &str, fields: &[(&str, &str)]) -> String {
    let mut out = format!("csrf={csrf}&once={once}");
    for (k, v) in fields {
        out.push_str(&format!("&{k}={}", enc(v)));
    }
    out
}

fn has(html: &str, wants: &[&str]) {
    for want in wants {
        assert!(html.contains(want), "{want} in {html}");
    }
}

fn rules() -> Value {
    json!({"rule_list": [
        {"id": "0", "name": "Web", "protocol": "tcp", "wan_port": "8080", "lan_ip": "192.168.0.50", "lan_port": "80", "enabled": "1"},
        {"section_id": "cfg0a1b2c", "comment": "", "proto": "udp", "src_dport": "3074", "dest_ip": "192.168.0.60", "dest_port": "3074"},
    ]})
}

/// The router: firewall on at Medium, NAT on, DMZ off, port forwarding on
/// with two rules, one MAC filter, UPnP on, the LAN 192.168.0.0/24.
fn router(fw: Value, pf: Value) -> device::Device {
    let filters = json!({"rule_list": [{"protocol": "tcp", "src_mac": MAC, "src_ip": "192.168.0.70", "src_port": "", "dest_ip": "203.0.113.9", "dest_port": "443", "enabled": "1"}]});
    device::Device {
        gets: vec![
            ("/api/router/firewall", Ok(fw)),
            ("/api/router/firewall/port-forward", Ok(pf)),
            ("/api/router/firewall/filter-rules", Ok(filters)),
            ("/api/router/firewall/upnp", Ok(json!({"upnp_switch": "1"}))),
            ("/api/router/lan", Ok(json!({"lan_ipaddr": "192.168.0.1", "lan_netmask": "255.255.255.0"}))),
        ],
        ..Default::default()
    }
}

fn firewall() -> Value {
    json!({"firewall_switch": "1", "nat_switch": "1", "dmz_enabled": "0", "dmz_ip": "", "firewall_level": "medium", "wan_ping_filter": "0", "port_forward_switch": "1"})
}

/// Firewall: the app's sections and rows, MACs masked; each switch and the
/// level are links to their questions; a confirmed post sends the app's
/// and the firmware's names once and shows the app's line; a remote
/// browser is warned where a change can lock it out.
#[tokio::test]
async fn firewall_page_switches_and_level() {
    let (c, cookie, _) = signed_in(210).await;
    let _dev = device::set(&c.app, router(firewall(), rules()));
    let r = get_with(&c, FW, peer(210, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    let g = html.split("&amp;g=").nth(1).unwrap()[..16].to_owned();
    has(&html, &[
        "<h1>Firewall</h1>", "href=\"/ui/router\">Router</a>", "<h2>Firewall</h2>", "<span>Status</span><span class=\"c-green\">Enabled</span>",
        "<a class=\"it bt\" href=\"/ui/router/firewall/switch?on=0\"><span>Disable Firewall</span></a>",
        "<span aria-current=\"true\">Medium</span>", "<a href=\"/ui/router/firewall/level?level=high\">High</a>",
        "<span>WAN Ping</span><span class=\"v\">Allowed</span>", "<h2>NAT / UPnP</h2>",
        "<a class=\"sw\" href=\"/ui/router/firewall/nat?on=0\" role=\"switch\" aria-checked=\"true\" aria-label=\"NAT\">",
        "<a class=\"sw\" href=\"/ui/router/firewall/upnp?on=0\" role=\"switch\" aria-checked=\"true\" aria-label=\"UPnP\">",
        "<form class=\"rf dmz\" method=\"post\" action=\"/ui/router/firewall/dmz\" autocomplete=\"off\">",
        "<input class=\"sr\" id=\"dmz\" type=\"checkbox\" role=\"switch\" name=\"dmz\" value=\"1\">", "DMZ Host IP", ">Apply DMZ</button>",
        "<h2>Port Forwarding</h2>", "<a class=\"it bt\" href=\"/ui/router/firewall/port-forward/new\">", "Add Rule",
        "<b class=\"nm\">Web</b><small class=\"c-green\">Active</small>", "TCP WAN:8080 \u{2192} 192.168.0.50:80",
        "<b class=\"nm\">Rule</b><small>Inactive</small>", "UDP WAN:3074 \u{2192} 192.168.0.60:3074",
        "href=\"/ui/router/firewall/port-forward/delete?id=cfg0a1b2c&amp;g=", "aria-label=\"Delete Web\"",
        "<h2>MAC/IP/Port Filters</h2>", "MAC: \u{2022}\u{2022}:\u{2022}\u{2022}:\u{2022}\u{2022}:\u{2022}\u{2022}:3a:7f",
        "192.168.0.70: \u{2192} 203.0.113.9:443",
    ]);
    assert!(!html.contains("02:00:5e") && !html.contains("Coming to the web UI"), "{html}");
    assert!(g.bytes().all(|b| b.is_ascii_hexdigit()), "{g}");
    let csrf = r.input("csrf");
    // Turn off: the question, then the confirmed post (both names, once).
    let q = get_with(&c, "/ui/router/firewall/switch?on=0", peer(210, 2), &cookie).await;
    q.assert_secure_html(200);
    has(&q.text(), &[
        "<h2 class=\"q\">Turn off the firewall?</h2>", "lose the firewall\u{2019}s protection from the internet",
        "The router\u{2019}s other services, such as SSH on port 2222 and the firmware\u{2019}s own web pages, may become reachable from the mobile network. This web UI and the agent\u{2019}s API keep refusing connections from it.",
        "<input type=\"hidden\" name=\"on\" value=\"0\">", ">Disable Firewall</button>", "href=\"/ui/router/firewall\">Cancel</a>",
    ]);
    assert!(!q.text().contains("connected remotely") && !q.text().contains("this web UI included"), "{}", q.text());
    let once = q.input("once");
    let fields = body(&csrf, &once, &[("on", "0")]);
    // Origin and CSRF first; a value no page sends; no confirm: the question.
    let bad = form("/ui/router/firewall/switch", peer(210, 2), &cookie, &fields).header("origin", "http://evil.example").body(Body::from(fields.clone())).unwrap();
    assert_eq!(send(&c, bad).await.status, 403);
    assert_eq!(post(&c, "/ui/router/firewall/switch", peer(210, 2), &cookie, &fields.replacen(&csrf, "0", 1)).await.status, 403);
    assert_eq!(post(&c, "/ui/router/firewall/switch", peer(210, 2), &cookie, &body(&csrf, &once, &[("on", "2"), ("confirm", "1")])).await.status, 400);
    assert_eq!(to(&post(&c, "/ui/router/firewall/switch", peer(210, 2), &cookie, &fields).await), "/ui/router/firewall/switch?on=0");
    assert!(calls(&c).is_empty());
    let confirmed = format!("{fields}&confirm=1");
    let done = to(&post(&c, "/ui/router/firewall/switch", peer(210, 2), &cookie, &confirmed).await);
    assert_eq!(done, format!("{FW}?r={once}"));
    assert_eq!(calls(&c), vec![("/api/router/firewall/switch".to_owned(), json!({"firewall_switch": "0", "enable": 0}))]);
    has(&get_with(&c, &done, peer(210, 2), &cookie).await.text(), &["<p class=\"msg ok\" role=\"status\">Firewall disabled</p>"]);
    // A replay sends nothing and lands on the same line.
    assert_eq!(to(&post(&c, "/ui/router/firewall/switch", peer(210, 2), &cookie, &confirmed).await), done);
    assert_eq!(calls(&c).len(), 1);
    // Turning it on or raising the level can lock a remote browser out.
    let q = get_with(&c, "/ui/router/firewall/switch?on=1", tailnet(41001), &cookie).await.text();
    has(&q, &["Turn on the firewall?", "which can cut access to the router", "You are connected remotely", ">Enable Firewall</button>"]);
    let q = get_with(&c, "/ui/router/firewall/level?level=high", tailnet(41002), &cookie).await.text();
    has(&q, &["<h2 class=\"q\">Set the firewall level to High?</h2>", "blocks more connections at High", "You are connected remotely", ">Set Level</button>"]);
    let q = get_with(&c, "/ui/router/firewall/level?level=low", tailnet(41003), &cookie).await.text();
    has(&q, &["Set the firewall level to Low?", "lets more connections from the internet through at Low", "such as SSH on port 2222", "This web UI and the agent\u{2019}s API keep refusing connections from it."]);
    assert!(!q.contains("connected remotely") && !q.contains("can cut access"), "{q}");
    let q = get_with(&c, "/ui/router/firewall/level?level=medium", peer(210, 2), &cookie).await.text();
    assert!(q.contains("The firewall level is already Medium") && !q.contains("name=\"confirm\""), "{q}");
    assert_eq!(get_with(&c, "/ui/router/firewall/level?level=max", peer(210, 2), &cookie).await.status, 400);
    // The level, NAT, UPnP and port forwarding: the app's and the firmware's names.
    for (path, field, value, sent, line) in [
        ("/ui/router/firewall/level", "level", "high", json!({"firewall_level": "high", "level": "high"}), "Firewall level set to high"),
        ("/ui/router/firewall/nat", "on", "0", json!({"nat_switch": "0", "enable": 0}), "NAT disabled"),
        ("/ui/router/firewall/upnp", "on", "1", json!({"upnp_switch": "1", "enable_upnp": 1}), "UPnP enabled"),
        ("/ui/router/firewall/port-forward/switch", "on", "0", json!({"port_forward_switch": "0", "portforward_enable": 0}), "Port forwarding disabled"),
    ] {
        let q = get_with(&c, &format!("{path}?{field}={value}"), peer(210, 2), &cookie).await;
        let once = q.input("once");
        let done = to(&post(&c, path, peer(210, 2), &cookie, &format!("{}&confirm=1", body(&csrf, &once, &[(field, value)]))).await);
        let api = format!("/api{}", path.trim_start_matches("/ui"));
        assert_eq!(calls(&c).last().unwrap(), &(api, sent), "{path}");
        has(&get_with(&c, &done, peer(210, 2), &cookie).await.text(), &[line]);
    }
    // A failed read shows only the error.
    let _dev = device::set(&c.app, device::Device { gets: vec![("/api/router/firewall", Err("ubus call failed: Method not found".into()))], ..Default::default() });
    let html = get_with(&c, FW, peer(210, 2), &cookie).await.text();
    assert!(html.contains("Failed to load firewall: ubus call failed: Method not found") && !html.contains("<form class=\"rf dmz\""), "{html}");
}

/// DMZ and New Port Forward: checked against the router's network and
/// rules (refused: the form again as typed), asked, then sent once with
/// both names; a device failure shows the form as typed.
#[tokio::test]
async fn dmz_and_new_port_forward() {
    let (c, cookie, _) = signed_in(211).await;
    let _dev = device::set(&c.app, router(firewall(), rules()));
    let page = get_with(&c, FW, peer(211, 2), &cookie).await;
    let (csrf, once) = (page.input("csrf"), page.input("once"));
    // The router's own address, then nothing to apply.
    let r = post(&c, DMZ, peer(211, 2), &cookie, &body(&csrf, &once, &[("dmz", "1"), ("ip", "192.168.0.1")])).await;
    r.assert_secure_html(400);
    has(&r.text(), &["This is the router\u{2019}s own address; use a device\u{2019}s address.", "name=\"ip\" value=\"192.168.0.1\"", "value=\"1\" checked"]);
    let r = post(&c, DMZ, peer(211, 2), &cookie, &body(&csrf, &once, &[("ip", "")])).await;
    let q = get_with(&c, &to(&r), peer(211, 2), &cookie).await.text();
    assert!(q.contains("Nothing to apply") && q.contains("DMZ is already off.") && !q.contains("name=\"confirm\""), "{q}");
    let r = post(&c, DMZ, peer(211, 2), &cookie, &body(&csrf, &once, &[("dmz", "1"), ("ip", "192.168.0.77")])).await;
    assert_eq!(to(&r), format!("{DMZ}?t={once}"));
    let q = get_with(&c, &to(&r), peer(211, 2), &cookie).await;
    has(&q.text(), &["<h2 class=\"q\">Make 192.168.0.77 the DMZ host?</h2>", "every port of that device can be reached from the internet", ">Apply DMZ</button>"]);
    let confirm = format!("csrf={csrf}&once={once}&d={}&confirm=1", q.input("d"));
    let done = to(&post(&c, DMZ, peer(211, 2), &cookie, &confirm).await);
    assert_eq!(calls(&c), vec![("/api/router/firewall/dmz".to_owned(), json!({"dmz_enabled": "1", "dmz_ip": "192.168.0.77", "dmz_enable": 1}))]);
    has(&get_with(&c, &done, peer(211, 2), &cookie).await.text(), &["DMZ settings updated"]);
    // New Port Forward: the app's sheet.
    let f = get_with(&c, NEW, peer(211, 3), &cookie).await;
    f.assert_secure_html(200);
    has(&f.text(), &[
        "<h1>New Port Forward</h1>", "<a class=\"nl\" href=\"/ui/router/firewall\">Cancel</a>", "<h2>Rule</h2>", "placeholder=\"Name\"",
        "<option value=\"tcp+udp\">TCP + UDP</option>", "<h2>WAN</h2>", "placeholder=\"WAN Port\"", "<h2>LAN</h2>", "placeholder=\"LAN IP\"",
        "placeholder=\"LAN Port\"", ">Add Rule</button>",
    ]);
    let once = f.input("once");
    let typed = |name: &'static str, proto: &'static str, wan: &'static str, ip: &'static str, lan: &'static str| {
        body(&csrf, &once, &[("pfname", name), ("proto", proto), ("wan", wan), ("ip", ip), ("lan", lan)])
    };
    let r = post(&c, ADD, peer(211, 3), &cookie, &typed("Cam<1>", "tcp", "", "10.9.9.9", "70000")).await;
    r.assert_secure_html(400);
    has(&r.text(), &[
        "Use up to 32 letters, digits, spaces, dots, hyphens and underscores.", "Enter a port from 1 to 65535.",
        "Use an address from 192.168.0.1 to 192.168.0.254.", "value=\"Cam&lt;1&gt;\"",
    ]);
    assert_eq!(post(&c, ADD, peer(211, 3), &cookie, &typed("Cam", "icmp", "9000", "192.168.0.80", "554")).await.status, 400);
    // A WAN port forwarded for the same protocol (UDP 8080 is free).
    let r = post(&c, ADD, peer(211, 3), &cookie, &typed("Cam", "tcp+udp", "8080", "192.168.0.80", "554")).await;
    assert!(r.text().contains("WAN port 8080 is already forwarded."), "{}", r.text());
    let r = post(&c, ADD, peer(211, 3), &cookie, &typed("Cam", "udp", "8080", "192.168.0.80", "554")).await;
    let q = get_with(&c, &to(&r), peer(211, 3), &cookie).await;
    has(&q.text(), &["<h2 class=\"q\">Add \u{201c}Cam\u{201d}?</h2>", "UDP WAN port 8080 goes to 192.168.0.80:554", ">Add Rule</button>"]);
    // A device failure: the form as typed, with the app's "Failed: …".
    let _fail = device::set(&c.app, device::Device {
        replies: vec![("/api/router/firewall/port-forward", (503, json!({"ok": false, "error": "ubus call zwrt_router.api router_set_portforward failed: Invalid argument"})))],
        ..router(firewall(), rules())
    });
    let confirm = format!("csrf={csrf}&once={once}&d={}&confirm=1", q.input("d"));
    let back = to(&post(&c, ADD, peer(211, 3), &cookie, &confirm).await);
    assert!(back.starts_with(&format!("{NEW}?t={once}&r=")), "{back}");
    has(&get_with(&c, &back, peer(211, 3), &cookie).await.text(), &[
        "Failed: ubus call zwrt_router.api router_set_portforward failed: Invalid argument", "name=\"pfname\" value=\"Cam\"", "name=\"wan\" value=\"8080\"",
    ]);
    assert_eq!(calls(&c).len(), 1);
    // Sent once with both names.
    let _ok = device::set(&c.app, router(firewall(), rules()));
    let f = get_with(&c, NEW, peer(211, 4), &cookie).await;
    let once = f.input("once");
    let r = post(&c, ADD, peer(211, 4), &cookie, &body(&csrf, &once, &[("pfname", ""), ("proto", "tcp"), ("wan", "9000"), ("ip", "192.168.0.80"), ("lan", "554")])).await;
    let q = get_with(&c, &to(&r), peer(211, 4), &cookie).await;
    assert!(q.text().contains("Add this port forward?"), "{}", q.text());
    let confirm = format!("csrf={csrf}&once={once}&d={}&confirm=1", q.input("d"));
    let done = to(&post(&c, ADD, peer(211, 4), &cookie, &confirm).await);
    assert_eq!(done, format!("{FW}?r={once}"));
    assert_eq!(calls(&c), vec![(
        "/api/router/firewall/port-forward".to_owned(),
        json!({"action": "add", "name": "", "protocol": "tcp", "wan_port": "9000", "lan_ip": "192.168.0.80", "lan_port": "554", "enabled": 1,
               "comment": "", "proto": "tcp", "src_dport": "9000", "dest_ip": "192.168.0.80"}),
    )]);
    has(&get_with(&c, &done, peer(211, 4), &cookie).await.text(), &["Port forward rule added"]);
    assert_eq!(to(&post(&c, ADD, peer(211, 4), &cookie, &confirm).await), done);
    assert_eq!(calls(&c).len(), 1);
}

/// A port forward's delete names the list it was chosen from: once the
/// list has changed, the question offers nothing and a post sends nothing
/// (the app's "out of date" line).
#[tokio::test]
async fn port_forward_delete_names_its_list() {
    let (c, cookie, _) = signed_in(212).await;
    let _dev = device::set(&c.app, router(firewall(), rules()));
    let html = get_with(&c, FW, peer(212, 2), &cookie).await.text();
    let at = html.find("href=\"/ui/router/firewall/port-forward/delete?id=0&amp;g=").unwrap();
    let href = html[at + 6..at + 6 + html[at + 6..].find('"').unwrap()].replace("&amp;", "&");
    let q = get_with(&c, &href, peer(212, 2), &cookie).await;
    q.assert_secure_html(200);
    has(&q.text(), &["<h2 class=\"q\">Delete \u{201c}Web\u{201d}?</h2>", "TCP WAN:8080 \u{2192} 192.168.0.50:80 stops", ">Delete</button>"]);
    let (csrf, once) = (q.input("csrf"), q.input("once"));
    let g = q.input("g");
    // Another rule appears: the question and the post refuse.
    let mut more = rules();
    more["rule_list"].as_array_mut().unwrap().insert(0, json!({"id": "9", "name": "New", "protocol": "tcp", "wan_port": "22", "lan_ip": "192.168.0.9", "lan_port": "22"}));
    let _changed = device::set(&c.app, router(firewall(), more));
    let stale = get_with(&c, &href, peer(212, 2), &cookie).await.text();
    assert!(stale.contains("The rule list changed") && !stale.contains("name=\"confirm\""), "{stale}");
    let confirm = body(&csrf, &once, &[("id", "0"), ("g", &g), ("confirm", "1")]);
    let line = to(&post(&c, DEL, peer(212, 2), &cookie, &confirm).await);
    has(&get_with(&c, &line, peer(212, 2), &cookie).await.text(), &["<p class=\"msg err\" role=\"alert\">The rule list was out of date and has been reloaded. Try again.</p>"]);
    assert!(calls(&c).is_empty());
    assert_eq!(get_with(&c, &format!("{DEL}?id=0"), peer(212, 2), &cookie).await.status, 400);
    assert_eq!(get_with(&c, &format!("{DEL}?id=a%20b&g={g}"), peer(212, 2), &cookie).await.status, 400);
    // The same list: deleted once.
    let _same = device::set(&c.app, router(firewall(), rules()));
    let once = get_with(&c, &href, peer(212, 2), &cookie).await.input("once");
    let confirm = body(&csrf, &once, &[("id", "0"), ("g", &g), ("confirm", "1")]);
    let done = to(&post(&c, DEL, peer(212, 2), &cookie, &confirm).await);
    assert_eq!(calls(&c), vec![("/api/router/firewall/port-forward".to_owned(), json!({"action": "delete", "id": "0", "section_id": ["0"]}))]);
    has(&get_with(&c, &done, peer(212, 2), &cookie).await.text(), &["Port forward rule deleted"]);
    assert_eq!(to(&post(&c, DEL, peer(212, 2), &cookie, &confirm).await), done);
    assert_eq!(calls(&c).len(), 1);
}

fn filter(list: Value) -> device::Device {
    device::Device { gets: vec![("/api/router/domain-filter", Ok(list))], ..Default::default() }
}

fn blocked() -> Value {
    json!({"enable": "1", "rule_list": [
        {"id": "1", "domain": "dclient.ztems.com", "enabled": "1"},
        {"id": "2", "domain": "ads.example.com", "enabled": "0"},
    ]})
}

/// Telemetry Blocker: the switch, Add and Block All post from the page
/// (once each); Add checks the name and skips one already blocked; Block
/// All adds only the missing known domains; a removal is asked and names
/// its list.
#[tokio::test]
async fn telemetry_blocker() {
    let (c, cookie, _) = signed_in(213).await;
    let _dev = device::set(&c.app, filter(blocked()));
    let r = get_with(&c, TM, peer(213, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    has(&html, &[
        "<h1>Telemetry Blocker</h1>", "<h2>Domain Filter</h2>", "<span class=\"c-green\">Enabled</span>",
        "<input type=\"hidden\" name=\"on\" value=\"0\"><button class=\"it bt\" type=\"submit\"><span>Disable Filter</span></button>",
        "<h2>Quick Actions</h2>", "<span>Block All ZTE Telemetry</span>",
        "<span class=\"dom\">dclient.ztems.com</span><span class=\"c-green\">", "<span class=\"sr\">Blocked</span>",
        "<span class=\"dom\">update.ztems.com</span><span class=\"v\">", "<h2>Add Custom Domain</h2>", "placeholder=\"Domain to block\"",
        "<h2>Blocked Domains</h2>", "<span class=\"dom\">ads.example.com</span><small>Inactive</small>", "aria-label=\"Remove ads.example.com\"",
    ]);
    assert_eq!(html.matches("Not blocked").count(), 4);
    let (csrf, once) = (r.input("csrf"), r.input("once"));
    // The switch: no question, once.
    let off = body(&csrf, &once, &[("on", "0")]);
    let done = to(&post(&c, "/ui/router/telemetry/switch", peer(213, 2), &cookie, &off).await);
    assert_eq!(done, format!("{TM}?r={once}"));
    assert_eq!(calls(&c), vec![("/api/router/domain-filter".to_owned(), json!({"enable": 0}))]);
    has(&get_with(&c, &done, peer(213, 2), &cookie).await.text(), &["Domain filter disabled"]);
    assert_eq!(to(&post(&c, "/ui/router/telemetry/switch", peer(213, 2), &cookie, &off).await), done);
    assert_eq!(calls(&c).len(), 1);
    // Add: refused as typed; one already blocked (any case) is not sent again.
    let once = get_with(&c, TM, peer(213, 2), &cookie).await.input("once");
    let r = post(&c, "/ui/router/telemetry/add", peer(213, 2), &cookie, &body(&csrf, &once, &[("domain", "not a domain")])).await;
    r.assert_secure_html(400);
    has(&r.text(), &["letters, digits, hyphens and dots", "value=\"not a domain\"", "aria-invalid=\"true\"", &format!("name=\"once\" value=\"{once}\"")]);
    let r = post(&c, "/ui/router/telemetry/add", peer(213, 2), &cookie, &body(&csrf, &once, &[("domain", "  ")])).await;
    assert!(r.text().contains("Enter a domain name<"), "{}", r.text());
    let line = to(&post(&c, "/ui/router/telemetry/add", peer(213, 2), &cookie, &body(&csrf, &once, &[("domain", "Ads.Example.com")])).await);
    has(&get_with(&c, &line, peer(213, 2), &cookie).await.text(), &["<p class=\"msg info\" role=\"status\">ads.example.com is already blocked</p>"]);
    assert_eq!(calls(&c).len(), 1);
    let done = to(&post(&c, "/ui/router/telemetry/add", peer(213, 2), &cookie, &body(&csrf, &once, &[("domain", "tracker.example.net")])).await);
    assert_eq!(calls(&c)[1], ("/api/router/domain-filter".to_owned(), json!({"action": "add", "domain": "tracker.example.net", "enabled": "1", "fqdn": "tracker.example.net"})));
    has(&get_with(&c, &done, peer(213, 2), &cookie).await.text(), &["Added tracker.example.net"]);
    // Block All: the four missing ones.
    let once = get_with(&c, TM, peer(213, 2), &cookie).await.input("once");
    let done = to(&post(&c, "/ui/router/telemetry/block-all", peer(213, 2), &cookie, &body(&csrf, &once, &[])).await);
    let added: Vec<Value> = calls(&c)[2..].iter().map(|(_, b)| b["domain"].clone()).collect();
    assert_eq!(added, ["dconfig.ztems.com", "iot.ztems.com", "mcs-cloud.ztems.com", "update.ztems.com"]);
    has(&get_with(&c, &done, peer(213, 2), &cookie).await.text(), &["Blocked 4 telemetry domains"]);
    // One add refused: the others still sent, and the line says so.
    let failing = device::Device {
        replies: vec![("/api/router/domain-filter#2", (503, json!({"ok": false, "error": "ubus call zwrt_router.api router_set_domain_filter failed: Invalid argument"})))],
        ..filter(blocked())
    };
    let _failing = device::set(&c.app, failing);
    let once = get_with(&c, TM, peer(213, 2), &cookie).await.input("once");
    let line = to(&post(&c, "/ui/router/telemetry/block-all", peer(213, 2), &cookie, &body(&csrf, &once, &[])).await);
    assert_eq!(calls(&c).len(), 4);
    has(&get_with(&c, &line, peer(213, 2), &cookie).await.text(), &[
        "<p class=\"msg info\" role=\"status\">Blocked 3 telemetry domains; 1 could not be added. Failed: ubus call zwrt_router.api router_set_domain_filter failed: Invalid argument</p>",
    ]);
    let _dev = device::set(&c.app, filter(blocked()));
    // A removal: asked, by its list.
    let html = get_with(&c, TM, peer(213, 2), &cookie).await.text();
    let at = html.find("href=\"/ui/router/telemetry/delete?id=2&amp;g=").unwrap();
    let href = html[at + 6..at + 6 + html[at + 6..].find('"').unwrap()].replace("&amp;", "&");
    let q = get_with(&c, &href, peer(213, 2), &cookie).await;
    has(&q.text(), &["<h2 class=\"q\">Remove ads.example.com?</h2>", "Devices on the router can reach ads.example.com again.", ">Remove</button>"]);
    let confirm = body(&csrf, &q.input("once"), &[("id", "2"), ("g", &q.input("g")), ("confirm", "1")]);
    let done = to(&post(&c, "/ui/router/telemetry/delete", peer(213, 2), &cookie, &confirm).await);
    assert_eq!(calls(&c).last().unwrap(), &("/api/router/domain-filter".to_owned(), json!({"action": "delete", "id": "2", "section_id": ["2"]})));
    has(&get_with(&c, &done, peer(213, 2), &cookie).await.text(), &["Removed ads.example.com"]);
    // Posted from the page: no question page; a failed read shows only the error.
    assert_eq!(to(&get_with(&c, "/ui/router/telemetry/add", peer(213, 2), &cookie).await), TM);
    let _dev = device::set(&c.app, device::Device { gets: vec![("/api/router/domain-filter", Err("ubus call failed: timed out".into()))], ..Default::default() });
    let html = get_with(&c, TM, peer(213, 2), &cookie).await.text();
    assert!(html.contains("Failed to load filters: ubus call failed: timed out") && !html.contains("Block All"), "{html}");
}

/// VPN Passthrough and QoS: the app's switches, posted from the page once
/// with the app's bodies; a failed read shows the error (QoS: the switch
/// "Unavailable", both directions offered; VPN: no form).
#[tokio::test]
async fn vpn_passthrough_and_qos() {
    let (c, cookie, _) = signed_in(214).await;
    let _dev = device::set(&c.app, device::Device {
        gets: vec![
            ("/api/router/vpn", Ok(json!({"l2tp_passthrough": "1", "pptp_passthrough": "0", "ipsec_passthrough": 1}))),
            ("/api/router/qos", Ok(json!({"qos_switch": "0"}))),
        ],
        ..Default::default()
    });
    let r = get_with(&c, VPN, peer(214, 2), &cookie).await;
    r.assert_secure_html(200);
    has(&r.text(), &[
        "<h1>VPN Passthrough</h1>", "<h2>VPN Passthrough</h2>", "name=\"l2tp\" value=\"1\" checked>", "name=\"pptp\" value=\"1\">",
        "name=\"ipsec\" value=\"1\" checked>", ">Apply</button>",
    ]);
    let (csrf, once) = (r.input("csrf"), r.input("once"));
    let apply = body(&csrf, &once, &[("pptp", "1")]);
    let done = to(&post(&c, "/ui/router/vpn/apply", peer(214, 2), &cookie, &apply).await);
    assert_eq!(calls(&c), vec![("/api/router/vpn".to_owned(), json!({"l2tp_passthrough": "0", "pptp_passthrough": "1", "ipsec_passthrough": "0"}))]);
    has(&get_with(&c, &done, peer(214, 2), &cookie).await.text(), &["VPN passthrough updated"]);
    assert_eq!(to(&post(&c, "/ui/router/vpn/apply", peer(214, 2), &cookie, &apply).await), done);
    assert_eq!(calls(&c).len(), 1);
    let r = get_with(&c, QOS, peer(214, 2), &cookie).await;
    r.assert_secure_html(200);
    has(&r.text(), &[
        "<h1>QoS</h1>", "<h2>Quality of Service</h2>", "action=\"/ui/router/qos/switch\"", "<input type=\"hidden\" name=\"on\" value=\"1\">",
        "role=\"switch\" aria-checked=\"false\" aria-label=\"Enable QoS\"",
    ]);
    let done = to(&post(&c, "/ui/router/qos/switch", peer(214, 2), &cookie, &body(&csrf, &r.input("once"), &[("on", "1")])).await);
    assert_eq!(calls(&c)[1], ("/api/router/qos".to_owned(), json!({"qos_switch": "1"})));
    has(&get_with(&c, &done, peer(214, 2), &cookie).await.text(), &["QoS enabled"]);
    assert_eq!(post(&c, "/ui/router/qos/switch", peer(214, 2), &cookie, &body(&csrf, &r.input("once"), &[("on", "x")])).await.status, 400);
    // The firmware has no such methods: the router's error.
    let missing = || Err("ubus call zwrt_router.api router_get_qos_switch failed: Method not found".to_owned());
    let _dev = device::set(&c.app, device::Device { gets: vec![("/api/router/vpn", missing()), ("/api/router/qos", missing())], ..Default::default() });
    let html = get_with(&c, VPN, peer(214, 2), &cookie).await.text();
    assert!(html.contains("Failed to load VPN: ubus call") && !html.contains("<form class=\"rf\""), "{html}");
    let html = get_with(&c, QOS, peer(214, 2), &cookie).await.text();
    has(&html, &["Failed to load QoS: ubus call", "<span class=\"v\">Unavailable</span>", ">On</button>", ">Off</button>"]);
}
