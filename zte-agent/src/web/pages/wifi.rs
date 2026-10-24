//! Wi-Fi & devices (the app's sheet: `WiFiCardView`, "Share Wi-Fi",
//! `DevicesCardView`) and the Share Wi-Fi page (`WiFiShareCardView`).
//!
//! - The Wi-Fi card is read at page load (no topic: the status read runs
//!   several commands and carries the keys). The read keeps only key-free
//!   fields, so no key ever reaches the renderer.
//! - Connected Devices is live from the `clients` topic, inside a
//!   `<details open>` that sits outside its fragments. Each device shows
//!   its name and IPv4 address; the MAC address only masked to its last 2
//!   octets (owner decision; the app shows it whole), also where a name
//!   carries it, and no IPv6 address.
//! - Share Wi-Fi is a page of its own (the app expands a card): network
//!   name, the password masked until "Show password" (`?show=1`, the only
//!   way its text reaches the page) and the join QR code. It is never live;
//!   like every page it is `no-store`, and the password is never in a URL.

use maud::{html, Markup};
use serde_json::{json, Map, Value};

use crate::events::Topic;
use crate::handlers::AppState;
use crate::web::fmt;
use crate::web::kit::{self, Icon};
use crate::web::qr;
use crate::web::render::{self, Fragment, Views};
use crate::web::screen::PageData;
use crate::web::session::query_param;

use super::dashboard::DASH;

pub const PATH: &str = "/ui/wifi";
pub const SHARE: &str = "/ui/wifi/share";

// --- Wi-Fi card ---

/// What the Wi-Fi card shows of `GET /api/wifi/status`: never the keys.
const CARD_FIELDS: [&str; 18] = [
    "wifi_onoff",
    "wifi6_switch",
    "radio2_disabled",
    "radio5_disabled",
    "ssid_2g",
    "ssid_5g",
    "channel_2g",
    "channel_5g",
    "actual_channel_2g",
    "actual_channel_5g",
    "encryption_2g",
    "encryption_5g",
    "hidden_2g",
    "hidden_5g",
    "clients_total",
    "guest_disabled_2g",
    "guest_disabled_5g",
    "guest_ssid",
];

/// A route's `data`, or its error.
fn data((status, mut v): (u16, Value)) -> Result<Value, String> {
    if status < 400 {
        Ok(v["data"].take())
    } else {
        Err(v["error"].as_str().unwrap_or("error").to_owned())
    }
}

/// Only `CARD_FIELDS` of a status read.
pub fn card_fields(status: &Value) -> Value {
    let mut out = Map::new();
    for key in CARD_FIELDS {
        if let Some(v) = status.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    Value::Object(out)
}

/// `GET /api/wifi/status`'s data, keys included (a test may stand in).
pub(crate) fn status(app: &AppState) -> Result<Value, String> {
    #[cfg(test)]
    if let Some(v) = super::fake::get(app) {
        return Ok(v);
    }
    data(crate::wifi::wifi_status(app))
}

/// The guest network's data, key included (a test may stand in).
pub(crate) fn guest_status(app: &AppState) -> Result<Value, String> {
    #[cfg(test)]
    if let Some(v) = super::fake::get(app) {
        return Ok(v);
    }
    data(crate::wifi::guest_status(app))
}

/// The page's read: the Wi-Fi status without its keys.
pub fn card_read(app: &AppState, _: Option<&str>) -> Result<Value, String> {
    status(app).map(|d| card_fields(&d))
}

fn s<'a>(d: &'a Value, key: &str) -> &'a str {
    d[key].as_str().unwrap_or("")
}

fn yes(d: &Value, key: &str) -> bool {
    s(d, key) == "1"
}

/// Either guest band not disabled (the app's `guestEnabled`).
fn guest_on(d: &Value) -> bool {
    !yes(d, "guest_disabled_2g") || !yes(d, "guest_disabled_5g")
}

/// The app's names for the encryption modes (`formatEncryption`).
pub fn encryption_label(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "psk2" | "psk2+ccmp" => "WPA2".into(),
        "sae" => "WPA3".into(),
        "sae-mixed" | "sae+psk2" => "WPA2/3".into(),
        "psk-mixed" | "psk+psk2" => "WPA/2".into(),
        "psk" => "WPA".into(),
        "none" | "" => "Open".into(),
        _ => raw.to_uppercase(),
    }
}

/// A band row: "Disabled", or "(Hidden)", the name, the encryption and
/// the channel (the radio's actual one when known).
fn band(d: &Value, label: &str, b: &str) -> Markup {
    let actual = s(d, &format!("actual_channel_{b}"));
    let channel = if actual.is_empty() { s(d, &format!("channel_{b}")) } else { actual };
    let radio = if b == "2g" { "radio2_disabled" } else { "radio5_disabled" };
    let ssid = s(d, &format!("ssid_{b}"));
    html! {
        div {
            dt { (kit::icon(Icon::Wifi)) " " (label) }
            @if yes(d, radio) {
                dd.muted { "Disabled" }
            } @else {
                dd {
                    @if yes(d, &format!("hidden_{b}")) { small.c-orange { "(Hidden)" } " " }
                    @if !ssid.is_empty() { b { (ssid) } " " }
                    small.muted { (encryption_label(s(d, &format!("encryption_{b}")))) }
                    @if !channel.is_empty() { " " small.muted { "CH " (channel) } }
                }
            }
        }
    }
}

/// The app's Wi-Fi card; while Wi-Fi is on, a link to Share Wi-Fi (the
/// app's card opens sharing when tapped then). An unreadable status shows
/// why, not "Off".
pub fn wifi_card(read: Result<&Value, &str>) -> Markup {
    let d = read.ok();
    let on = d.is_some_and(|d| yes(d, "wifi_onoff"));
    let clients = d.and_then(|d| fmt::int(&d["clients_total"])).unwrap_or(0);
    let body = html! {
            div.wh {
                h2 { "WiFi" }
                @if on && d.is_some_and(|d| yes(d, "wifi6_switch")) { span.badge.bl { "WiFi 7" } }
                @if on && clients > 0 { span.cap { (clients) " client" @if clients != 1 { "s" } } }
                @if d.is_some() {
                    @if on { span.c-green { "On" } } @else { span.c-red { "Off" } }
                }
            }
            @match (d, read) {
                (Some(d), _) => {
                    @if on {
                        dl.kv.tk {
                            (band(d, "2.4G", "2g"))
                            (band(d, "5G", "5g"))
                            @if guest_on(d) {
                                div { dt { (kit::icon(Icon::People)) " Guest" } dd { b { (s(d, "guest_ssid")) } } }
                            }
                        }
                    }
                }
                (None, Err(e)) => p.age.err { "Couldn't update: " (fmt::truncate(e, 120)) },
                (None, Ok(_)) => {},
            }
    };
    html! {
        @if on { a.card.wf href=(SHARE) { (body) } } @else { section.card.wf { (body) } }
    }
}

// --- Connected devices ---

pub const FRAGMENTS: [Fragment; 2] = [
    Fragment { id: "wf-count", topics: &[Topic::Clients], render: device_count },
    Fragment { id: "wf-devices", topics: &[Topic::Clients], render: devices },
];

/// `••:••:••:••:3a:7f`: only the last 2 octets of a MAC address (anything
/// that is not one is masked whole).
pub fn mask_mac(mac: &str) -> String {
    let parts: Vec<&str> = mac.split(':').collect();
    let octet = |p: &&str| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit());
    if parts.len() == 6 && parts.iter().all(octet) {
        format!("\u{2022}\u{2022}:\u{2022}\u{2022}:\u{2022}\u{2022}:\u{2022}\u{2022}:{}:{}", parts[4], parts[5])
    } else {
        "\u{2022}\u{2022}:\u{2022}\u{2022}:\u{2022}\u{2022}:\u{2022}\u{2022}:\u{2022}\u{2022}:\u{2022}\u{2022}".into()
    }
}

/// `name` with every part that spells 3 or more octets of `mac` (as 6+ hex
/// digits, or octets joined by `:` or `-`; makers put them in host names,
/// such as `shellyplus1pm-02032ab12345`) masked, so a name shows no more of
/// the MAC address than `mask_mac`.
pub fn mask_name(name: &str, mac: &str) -> String {
    let octets: Vec<&str> = mac.split(':').collect();
    if octets.len() != 6 || !octets.iter().all(|o| o.len() == 2 && o.bytes().all(|b| b.is_ascii_hexdigit())) {
        return name.to_owned();
    }
    let hex = octets.concat().to_ascii_lowercase();
    let mut parts: Vec<String> = Vec::new();
    for start in 0..=6 {
        parts.extend((start + 6..=12).map(|end| hex[start..end].to_owned()));
    }
    for sep in [":", "-"] {
        for start in 0..=3 {
            parts.extend((start + 3..=6).map(|end| octets[start..end].join(sep).to_ascii_lowercase()));
        }
    }
    // ASCII lower case keeps every byte where it was.
    let low = name.to_ascii_lowercase();
    let mut hide = vec![false; name.len()];
    for part in &parts {
        for (i, _) in low.match_indices(part.as_str()) {
            hide[i..i + part.len()].fill(true);
        }
    }
    name.char_indices().map(|(i, c)| if hide[i] { '\u{2022}' } else { c }).collect()
}

/// A dotted-quad IPv4 address as a number, for sorting.
fn ipv4_key(ip: &str) -> Option<u32> {
    let mut key = 0u32;
    let mut n = 0;
    for part in ip.split('.') {
        if n == 4 || part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        key = key << 8 | part.parse::<u8>().ok()? as u32;
        n += 1;
    }
    (n == 4).then_some(key)
}

/// One device: (name, masked MAC, IPv4).
pub struct Device {
    pub name: String,
    pub mac: String,
    pub ip: String,
}

/// The app's list (`parseHostHints`, `enrichWithDHCP`): the DHCP host name,
/// else the host-hint name, else the (masked) MAC; sorted by IPv4 address,
/// devices without one first (the app's string order puts "" first).
pub fn device_list(c: &Value) -> Vec<Device> {
    let leases = c["dhcp_leases"].as_array().map(Vec::as_slice).unwrap_or_default();
    let hostname = |mac: &str| {
        leases
            .iter()
            .filter(|l| l["macaddr"].as_str().is_some_and(|m| m.eq_ignore_ascii_case(mac)))
            .filter_map(|l| l["hostname"].as_str())
            .next_back()
            .filter(|h| !h.is_empty())
    };
    let mut out: Vec<(Option<u32>, Device)> = c["hosts"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, h)| h.is_object())
        .map(|(mac, h)| {
            let ip = h["ipaddrs"].as_array().and_then(|a| a.first()).and_then(Value::as_str).unwrap_or("").to_owned();
            let masked = mask_mac(mac);
            let hint = h["name"].as_str().filter(|n| !n.is_empty());
            let name = hostname(mac).or(hint).map_or_else(|| masked.clone(), |n| mask_name(n, mac));
            (ipv4_key(&ip), Device { name, mac: masked, ip })
        })
        .collect();
    out.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.ip.cmp(&y.ip)).then_with(|| x.name.cmp(&y.name)));
    out.into_iter().map(|(_, d)| d).collect()
}

/// How many devices the list has (entries that are not objects are no
/// device, as in the app's parse).
pub fn host_count(clients: &Value) -> Option<usize> {
    clients["hosts"].as_object().map(|h| h.values().filter(|h| h.is_object()).count())
}

/// The count beside "Connected Devices" (in the summary, so it updates).
fn device_count(v: &Views) -> Markup {
    let n = v.value(Topic::Clients).and_then(host_count);
    html! {
        b #wf-count .big { @match n { Some(n) => (crate::web::roll::roll(v, "dev", &n.to_string())), None => (DASH) } }
    }
}

/// Name (bold), masked MAC and IPv4; nothing when no device is connected
/// (the app's card shows only its count).
fn devices(v: &Views) -> Markup {
    let list = v.value(Topic::Clients).map(device_list).unwrap_or_default();
    html! {
        div #wf-devices {
            @if !list.is_empty() {
                ul.devs {
                    @for d in &list {
                        li {
                            b { (d.name) }
                            div.row { span.mono { (d.mac) } @if !d.ip.is_empty() { span.mono { (d.ip) } } }
                        }
                    }
                }
            }
            (render::error_line(v, &[Topic::Clients]))
        }
    }
}

/// The sheet: the Wi-Fi card, "Share Wi-Fi" (a page here; the app expands
/// a card, also when Wi-Fi is off), Connected Devices (open, as the app's).
pub fn page(pd: &PageData) -> Markup {
    let read = match pd.reads.first() {
        Some(Ok(d)) => Ok(d),
        Some(Err(e)) => Err(e.as_str()),
        None => Err("no read"),
    };
    html! {
        (render::live_snapshot(PATH))
        div.dash {
            (wifi_card(read))
            p.ctr { a.nl href=(SHARE) { "Share Wi-Fi" } }
            details.card.devc open {
                summary.ch { (kit::icon(Icon::Laptop)) span { "Connected Devices" } (device_count(pd.views)) }
                (devices(pd.views))
            }
        }
    }
}

// --- Share Wi-Fi ---

/// The share page's read: the network's name, password and encryption
/// (the 2.4 GHz network, or 5 GHz when that radio is off; the guest
/// network with `?net=guest` while it is on), and whether there is a guest
/// network. Rendered only by `share`, never logged or streamed.
pub fn share_read(app: &AppState, query: Option<&str>) -> Result<Value, String> {
    let d = status(app)?;
    let guest_on = guest_on(&d);
    let guest = guest_on && query_param(query, "net").as_deref() == Some("guest");
    let creds = |d: &Value, ssid: &str, key: &str, enc: &str| {
        let enc = d[enc].as_str().unwrap_or("psk2+ccmp");
        json!({"ssid": s(d, ssid), "key": s(d, key), "encryption": enc, "guest": guest, "guest_on": guest_on})
    };
    Ok(if guest {
        creds(&guest_status(app)?, "ssid", "key", "encryption")
    } else if !yes(&d, "radio2_disabled") {
        creds(&d, "ssid_2g", "key_2g", "encryption_2g")
    } else {
        creds(&d, "ssid_5g", "key_5g", "encryption_5g")
    })
}

/// "Main" / "Guest" (only with a guest network), the name, the password
/// ("•" per character, at most 12, until "Show password") and the QR code.
pub fn share(pd: &PageData) -> Markup {
    let d = match pd.reads.first() {
        Some(Ok(d)) => d,
        Some(Err(e)) => return kit::notice(false, &format!("Failed to load Wi-Fi credentials: {}", fmt::truncate(e, 120))),
        None => return html! {},
    };
    let guest = d["guest"].as_bool() == Some(true);
    let (ssid, key, enc) = (s(d, "ssid"), s(d, "key"), s(d, "encryption"));
    let show = pd.param("show").as_deref() == Some("1");
    let here = if guest { "/ui/wifi/share?net=guest" } else { SHARE };
    let toggle = if guest { "/ui/wifi/share?net=guest&show=1" } else { "/ui/wifi/share?show=1" };
    let code = qr::encode(&qr::payload(ssid, key, enc));
    html! {
        section.card.ctr.share {
            @if d["guest_on"].as_bool() == Some(true) {
                nav.seg aria-label="Network" {
                    a href=(SHARE) aria-current=[(!guest).then_some("page")] { "Main" }
                    a href="/ui/wifi/share?net=guest" aria-current=[guest.then_some("page")] { "Guest" }
                }
            }
            p { b { (ssid) } }
            @if !key.is_empty() {
                p.pw {
                    @if show {
                        // Never sent to a page translation service.
                        span.mono translate="no" { (key) } " " a href=(here) { "Hide password" }
                    } @else {
                        span.mono role="img" aria-label="Password hidden" { ("\u{2022}".repeat(key.chars().count().min(12))) }
                        " " a href=(toggle) { "Show password" }
                    }
                }
            }
            @match &code {
                Some(c) => (qr::svg(c, "Wi-Fi QR code")),
                None => p.cap { "This network name and password are too long for a QR code." },
            }
            p.cap { "Scan with any phone camera" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boottime::BootInstant;
    use crate::events::hub::SlotView;
    use std::sync::Arc;
    use std::time::Duration;

    const NOW: BootInstant = BootInstant::from_duration(Duration::from_secs(10_000));
    const KEY: &str = "WIFIKEY-7f3a";

    /// A status as the agent returns it, keys included.
    fn status() -> Value {
        json!({
            "wifi_onoff": "1", "wifi6_switch": "1", "radio2_disabled": "0", "radio5_disabled": "0",
            "channel_2g": "auto", "channel_5g": "36", "actual_channel_2g": "6", "actual_channel_5g": "",
            "ssid_2g": "<b>Home</b>", "ssid_5g": "Home-5G", "key_2g": KEY, "key_5g": "WIFIKEY-5g-9c1d",
            "encryption_2g": "psk2+ccmp", "encryption_5g": "sae-mixed", "hidden_2g": "0", "hidden_5g": "1",
            "clients_total": 3, "guest_disabled_2g": "0", "guest_disabled_5g": "1", "guest_ssid": "Guests",
            "txpower_2g": "100", "htmode_2g": "HE40", "country_code": "DE",
        })
    }

    fn views(clients: Value) -> Views {
        let slot = SlotView { version: 1, at: NOW, value_at: NOW, value: Some(Arc::new(clients)), error: None };
        Views::new(NOW, vec![(Topic::Clients, slot)])
    }

    #[test]
    fn encryption_labels_follow_the_app() {
        for (raw, label) in [
            ("psk2", "WPA2"), ("psk2+ccmp", "WPA2"), ("PSK2+CCMP", "WPA2"), ("sae", "WPA3"), ("sae-mixed", "WPA2/3"),
            ("sae+psk2", "WPA2/3"), ("psk-mixed", "WPA/2"), ("psk+psk2", "WPA/2"), ("psk", "WPA"), ("none", "Open"),
            ("", "Open"), ("wpa3-192", "WPA3-192"),
        ] {
            assert_eq!(encryption_label(raw), label, "{raw}");
        }
    }

    /// The card's read keeps no key; the card is the app's.
    #[test]
    fn the_card_has_no_key_and_follows_the_app() {
        let d = card_fields(&status());
        let text = d.to_string();
        assert!(!text.contains("WIFIKEY") && !text.contains("key_") && !text.contains("txpower"), "{text}");
        let s = wifi_card(Ok(&d)).into_string();
        for want in [
            "<h2>WiFi</h2><span class=\"badge bl\">WiFi 7</span><span class=\"cap\">3 clients</span><span class=\"c-green\">On</span>",
            // 2.4G: the actual channel; the name escaped.
            "2.4G</dt><dd><b>&lt;b&gt;Home&lt;/b&gt;</b> <small class=\"muted\">WPA2</small> <small class=\"muted\">CH 6</small></dd>",
            // 5G: hidden, no actual channel: the configured one.
            "5G</dt><dd><small class=\"c-orange\">(Hidden)</small> <b>Home-5G</b> <small class=\"muted\">WPA2/3</small> <small class=\"muted\">CH 36</small></dd>",
            "Guest</dt><dd><b>Guests</b></dd>",
        ] {
            assert!(s.contains(want), "{want} in {s}");
        }
        assert!(!s.contains("WIFIKEY"));
        // On: the card opens Share Wi-Fi (the app's tap); off, it is no link.
        assert!(s.starts_with("<a class=\"card wf\" href=\"/ui/wifi/share\"><div class=\"wh\">"), "{s}");
        // One client; no WiFi 7; a disabled radio; no guest network.
        let mut d = status();
        for (k, v) in [("clients_total", json!(1)), ("wifi6_switch", json!("0")), ("radio5_disabled", json!("1")), ("guest_disabled_2g", json!("1"))] {
            d[k] = v;
        }
        let s = wifi_card(Ok(&card_fields(&d))).into_string();
        assert!(s.contains("<span class=\"cap\">1 client</span>") && !s.contains("WiFi 7") && !s.contains("Guest"), "{s}");
        assert!(s.contains("5G</dt><dd class=\"muted\">Disabled</dd>"), "{s}");
        // Off: "Off" in red, no client count, no bands (as the app).
        d["wifi_onoff"] = json!("0");
        let s = wifi_card(Ok(&card_fields(&d))).into_string();
        assert!(s.contains("<span class=\"c-red\">Off</span>") && !s.contains("client") && !s.contains("2.4G"), "{s}");
        assert!(s.starts_with("<section class=\"card wf\">") && !s.contains("href"), "{s}");
        // No clients: no count.
        let mut d = status();
        d["clients_total"] = json!(0);
        assert!(!wifi_card(Ok(&d)).into_string().contains("client"));
        // Unreadable: why, not "Off".
        let s = wifi_card(Err("uci: <x>")).into_string();
        assert!(s.contains("Couldn't update: uci: &lt;x&gt;") && !s.contains("Off"), "{s}");
    }

    #[test]
    fn macs_are_masked_to_the_last_two_octets() {
        assert_eq!(mask_mac("AA:BB:CC:DD:3A:7F"), "••:••:••:••:3A:7F");
        assert_eq!(mask_mac("aa:bb:cc:dd:ee:42"), "••:••:••:••:ee:42");
        for odd in ["", "aa:bb", "aa:bb:cc:dd:ee:ff:00", "<b>:bb:cc:dd:ee:ff", "aa-bb-cc-dd-ee-ff"] {
            assert_eq!(mask_mac(odd), "••:••:••:••:••:••", "{odd}");
        }
        assert_eq!((ipv4_key("192.168.0.10"), ipv4_key("10.0.0.1")), (Some(0xC0A8_000A), Some(0x0A00_0001)));
        // A name that spells 3 or more octets of the MAC: those are masked
        // (contiguous in any case, or joined by ":" or "-"); fewer, or other
        // hex, stay.
        let mac = "02:03:2A:B1:23:45";
        for (name, masked) in [
            ("shellyplus1pm-02032ab12345", "shellyplus1pm-••••••••••••"),
            ("ESP_B12345", "ESP_••••••"),
            ("tasmota-2AB123-4567", "tasmota-••••••-4567"),
            ("dev-2a:b1:23:45", "dev-•••••••••••"),
            ("02-03-2A-x", "••••••••-x"),
            ("Shelly-2345", "Shelly-2345"),
            ("cafe-deadbeef", "cafe-deadbeef"),
            ("Küche-b12345", "Küche-••••••"),
        ] {
            assert_eq!(mask_name(name, mac), masked, "{name}");
        }
        assert_eq!(mask_name("x-02032ab12345", "not-a-mac"), "x-02032ab12345");
        for bad in ["", "1.2.3", "1.2.3.4.5", "256.1.1.1", "a.b.c.d", "1..2.3", "fe80::1"] {
            assert_eq!(ipv4_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn devices_list_names_ipv4_and_masked_macs() {
        let c = json!({
            "hosts": {
                "AA:BB:CC:DD:EE:01": {"name": "hint-one", "ipaddrs": ["192.168.0.10"], "ip6addrs": ["2001:db8::1"]},
                "AA:BB:CC:DD:EE:02": {"name": "", "ipaddrs": ["192.168.0.9"]},
                "AA:BB:CC:DD:EE:03": {"name": "<script>x</script>"},
                "AA:BB:CC:DD:EE:04": {"ipaddrs": ["192.168.0.100"]},
            },
            "dhcp_leases": [{"macaddr": "aa:bb:cc:dd:ee:01", "hostname": "alice-laptop"}],
        });
        let v = views(c);
        let s = devices(&v).into_string();
        // The DHCP name wins; a nameless device is its masked MAC; no IPv6.
        let rows: Vec<&str> = s.split("<li>").skip(1).collect();
        assert_eq!(rows.len(), 4, "{s}");
        assert!(rows[0].starts_with("<b>&lt;script&gt;x&lt;/script&gt;</b><div class=\"row\"><span class=\"mono\">••:••:••:••:EE:03</span></div>"), "{s}");
        assert!(rows[1].starts_with("<b>••:••:••:••:EE:02</b>") && rows[1].contains(">192.168.0.9<"), "{s}");
        assert!(rows[2].starts_with("<b>alice-laptop</b><div class=\"row\"><span class=\"mono\">••:••:••:••:EE:01</span><span class=\"mono\">192.168.0.10</span>"), "{s}");
        assert!(rows[3].contains(">192.168.0.100<") && rows[3].starts_with("<b>••:••:••:••:EE:04</b>"), "{s}");
        for gone in ["AA:BB:CC", "aa:bb:cc", "2001:db8", "hint-one", "<script"] {
            assert!(!s.contains(gone), "{gone} in {s}");
        }
        assert_eq!(device_count(&v).into_string(), "<b class=\"big\" id=\"wf-count\">4</b>");
        // Entries that are no device are not counted (as the app's parse).
        let odd = views(json!({"hosts": {"AA:BB:CC:DD:EE:01": {}, "x": 1, "y": null}}));
        assert_eq!(device_count(&odd).into_string(), "<b class=\"big\" id=\"wf-count\">1</b>");
        assert_eq!(devices(&odd).into_string().matches("<li>").count(), 1);
        // Nothing connected: the count only (the app's card).
        let none = views(json!({"hosts": {}}));
        assert_eq!(devices(&none).into_string(), "<div id=\"wf-devices\"></div>");
        assert_eq!(device_count(&Views::new(NOW, vec![])).into_string(), "<b class=\"big\" id=\"wf-count\">—</b>");
    }

    fn share_html(read: Value, query: Option<&str>) -> String {
        use crate::auth::{Kind, PeerKey};
        let app = AppState::new();
        app.auth.set_password("pw");
        let issued = app.auth.login("pw", Kind::Browser, PeerKey::UNKNOWN, &[]).unwrap();
        let p = app.auth.authenticate(Kind::Browser, issued.token.expose()).unwrap();
        let v = Views::new(NOW, vec![]);
        let reads = [Ok(read)];
        share(&PageData { app: &app, p: &p, views: &v, reads: &reads, remote: false, form: None, query, once: None, at: None, ussd: None, notice: None, token: Default::default(), prefs: Default::default(), secure: false }).into_string()
    }

    /// The password's text is in the page only after "Show password"; the
    /// QR code always encodes it (the app shows the code at once).
    #[test]
    fn the_password_shows_only_when_asked() {
        let read = json!({"ssid": "Home;Net", "key": "p@ss\"word:1234567", "encryption": "psk2+ccmp", "guest": false, "guest_on": true});
        let hidden = share_html(read.clone(), None);
        assert!(!hidden.contains("p@ss") && !hidden.contains("1234567"), "{hidden}");
        assert!(hidden.contains("<span class=\"mono\" role=\"img\" aria-label=\"Password hidden\">••••••••••••</span> <a href=\"/ui/wifi/share?show=1\">Show password</a>"), "{hidden}");
        for want in [
            "<nav class=\"seg\" aria-label=\"Network\"><a href=\"/ui/wifi/share\" aria-current=\"page\">Main</a><a href=\"/ui/wifi/share?net=guest\">Guest</a></nav>",
            "<p><b>Home;Net</b></p>", "<svg class=\"qr\"", "Scan with any phone camera",
        ] {
            assert!(hidden.contains(want), "{want} in {hidden}");
        }
        for q in [Some("show=0"), Some("show=true"), Some("net=main&show=")] {
            assert!(!share_html(read.clone(), q).contains("p@ss"), "{q:?}");
        }
        let shown = share_html(read.clone(), Some("show=1"));
        assert!(shown.contains("<span class=\"mono\" translate=\"no\">p@ss&quot;word:1234567</span> <a href=\"/ui/wifi/share\">Hide password</a>"), "{shown}");
        // The same code either way, encoding the app's payload.
        let code = qr::svg(&qr::encode(r#"WIFI:T:WPA;S:Home\;Net;P:p@ss\"word\:1234567;;"#).unwrap(), "Wi-Fi QR code").into_string();
        assert!(hidden.contains(&code) && shown.contains(&code));
        assert!(!hidden.contains("style=") && !hidden.contains("<form"));
        // A short password: as many dots as characters; guest: its own links.
        let guest = json!({"ssid": "Guests", "key": "abc", "encryption": "sae", "guest": true, "guest_on": true});
        let s = share_html(guest, None);
        assert!(s.contains(">•••</span> <a href=\"/ui/wifi/share?net=guest&amp;show=1\">Show password</a>"), "{s}");
        assert!(s.contains("<a href=\"/ui/wifi/share?net=guest\" aria-current=\"page\">Guest</a>"), "{s}");
        // No guest network: no Main/Guest; an open one: no password row.
        let open = json!({"ssid": "Cafe", "key": "", "encryption": "none", "guest": false, "guest_on": false});
        let s = share_html(open, Some("show=1"));
        assert!(!s.contains("seg") && !s.contains("password") && s.contains("<svg class=\"qr\""), "{s}");
    }
}
