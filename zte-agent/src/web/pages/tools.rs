//! Tools › Device Info (the app's `DeviceInfoView`) and Tools › Connected
//! Devices (`ClientsView`).
//!
//! - **Device Info** is read at page load (the app reads it when the screen
//!   opens and on pull to refresh): `wan` and `signal` through the hub's
//!   readers, the IMEI and the LAN address in-process. Only the IMEI's
//!   last 4 digits ever reach the renderer (owner decision; the app shows it
//!   whole). The SIM read the app makes and never shows is not made.
//! - **Connected Devices** is live from the `clients` topic, with the rows
//!   and masking of Wi-Fi & devices (`pages::wifi`): name and IPv4, the MAC
//!   only masked to its last 2 octets, no IPv6. The search (the app's
//!   search bar) is posted to the page, so its text is in no URL; it
//!   matches what is shown (the masked name and MAC, the IPv4 address), and
//!   a searched page is a snapshot (its stream would show every device).

use maud::{html, Markup};
use serde_json::{json, Value};

use crate::events::Topic;
use crate::handlers::AppState;
use crate::web::kit::{self, Icon};
use crate::web::render::{self, Fragment, Views};
use crate::web::screen::{PageData, Read};

use super::wifi::device_list;

pub const DEVICE_INFO: &str = "/ui/tools/device-info";
pub const DEVICES: &str = "/ui/tools/devices";

// --- Device Info ---

/// A route's `data`, or `Null`.
fn get(app: &AppState, path: &'static str) -> Value {
    Read::Api(path).run(app, None).unwrap_or(Value::Null)
}

/// What the page shows of the IMEI and LAN reads: "…" and the IMEI's last
/// 4 digits ("" without 4 digits), the LAN address.
fn info(imei: &Value, lan: &Value) -> Value {
    let digits: Vec<char> = imei["imei"].as_str().unwrap_or("").chars().filter(char::is_ascii_digit).collect();
    let tail = match digits.len() {
        n if n >= 4 => format!("\u{2026}{}", digits[n - 4..].iter().collect::<String>()),
        _ => String::new(),
    };
    json!({"imei": tail, "lan": lan["ipv4-address"][0]["address"].as_str().unwrap_or("")})
}

/// The page's read besides the topics (a test may stand in for both).
pub fn info_read(app: &AppState, _: Option<&str>) -> Result<Value, String> {
    #[cfg(test)]
    if let Some(v) = super::fake::get(app) {
        return Ok(info(&v, &v));
    }
    Ok(info(&get(app, "/api/sim/imei"), &get(app, "/api/network/lan-status")))
}

/// A row: the label secondary, the value ("--" when empty), as the app's
/// `infoRow`.
fn info_row(label: &str, value: &str) -> Markup {
    html! { li.it { span.muted { (label) } span.num { @if value.is_empty() { "--" } @else { (value) } } } }
}

/// The app's colour for `bars` of 5: 60 % and up green, 40 % and up yellow
/// (orange here), else red; none: secondary.
fn bars_class(bars: i64) -> &'static str {
    match bars * 10 / 5 {
        _ if bars <= 0 => "tn muted",
        r if r >= 6 => "tn c-green",
        r if r >= 4 => "tn c-orange",
        _ => "tn c-red",
    }
}

/// "Device" (IMEI), "Network" (roaming, signal bars), "WAN" (IPv4, each
/// global IPv6 address), "LAN" (gateway).
pub fn device_info(pd: &PageData) -> Markup {
    let d = pd.read(0);
    let s = |k: &str| d.and_then(|d| d[k].as_str()).unwrap_or("").to_owned();
    let sig = pd.views.value(Topic::Signal);
    let roaming = sig.is_some_and(|s| crate::web::fmt::int(&s["simcard_roam"]) == Some(1));
    let bars = sig.and_then(|s| crate::web::fmt::int(&s["signalbar"])).unwrap_or(0);
    let wan = pd.views.value(Topic::Wan);
    let v4 = wan.and_then(|w| w["wan"]["ipv4-address"][0]["address"].as_str()).unwrap_or("");
    let v6: Vec<&str> = wan
        .and_then(|w| w["wan6"]["ipv6-address"].as_array())
        .map(|a| a.iter().filter_map(|e| e["address"].as_str()).filter(|a| !a.starts_with("fe80")).collect())
        .unwrap_or_default();
    html! {
        (kit::section(Some("Device"), None, info_row("IMEI", &s("imei"))))
        (kit::section(Some("Network"), None, html! {
            li.it {
                span.muted { "Roaming" }
                @if roaming { span.c-orange { "Roaming" } } @else { span.c-green { "Home" } }
            }
            li.it {
                span.muted { "Signal" }
                span class=(bars_class(bars)) { (kit::icon(Icon::Bars)) }
                span.muted.num { (bars) "/5" }
            }
        }))
        (kit::section(Some("WAN"), None, html! {
            (info_row("IPv4", v4))
            @for a in &v6 { (info_row("IPv6", a)) }
            @if v6.is_empty() { (info_row("IPv6", "")) }
        }))
        (kit::section(Some("LAN"), None, info_row("Gateway", &s("lan"))))
    }
}

// --- Enable ADB ---

/// The app's `EnableADBView`: icon, title, text, the result line and
/// "Enable ADB", which asks first (owner decision).
pub fn adb(pd: &PageData) -> Markup {
    html! {
        div.adb {
            (kit::empty_state(Icon::Cable, "Enable ADB Debug", "Sets the USB mode to debug, enabling ADB access over USB-C."))
            @if let Some(n) = &pd.notice { (n) }
            p.ctr { a.btn.btn-wide href=(super::confirm::ADB_ENABLE) { (kit::icon(Icon::Power)) " Enable ADB" } }
        }
    }
}

// --- Connected Devices ---

pub const FRAGMENTS: [Fragment; 1] = [Fragment { id: "cd-list", topics: &[Topic::Clients], render: |v| list(v, "") }];

/// A read error first (the app's red row), "<n> Devices" (the app's
/// header, unpluralised) and a row per device that matches `q`
/// (case-insensitive, in what is shown); else the app's empty states.
fn list(v: &Views, q: &str) -> Markup {
    let all = v.value(Topic::Clients).map(device_list).unwrap_or_default();
    let q = q.to_lowercase();
    let shown: Vec<_> = all
        .iter()
        .filter(|d| [&d.name, &d.ip, &d.mac].iter().any(|s| s.to_lowercase().contains(&q)))
        .collect();
    let error = v.slot(Topic::Clients).is_some_and(|s| s.error.is_some());
    html! {
        div #cd-list {
            (render::error_line(v, &[Topic::Clients]))
            @if !shown.is_empty() {
                (kit::section(Some(&format!("{} Devices", shown.len())), None, html! {
                    @for d in &shown {
                        li.it {
                            span {
                                b.dn { (d.name) }
                                // The icon secondary, as the app's label.
                                small { span.tn { (kit::icon(Icon::Network)) } " " @if d.ip.is_empty() { "--" } @else { (d.ip) } " " span.mono { (d.mac) } }
                            }
                        }
                    }
                }))
            } @else if !q.is_empty() && !all.is_empty() {
                (kit::empty_state(Icon::Search, &format!("No Results for \u{201c}{q}\u{201d}"), "Check the spelling or try a new search."))
            } @else if !error {
                (kit::empty_state(Icon::Wifi, "No Devices", "Nothing is connected to the router right now."))
            }
        }
    }
}

/// The search (posted to this page) and the list: live, or filtered by a
/// search as a snapshot.
pub fn devices(pd: &PageData) -> Markup {
    let q = pd.form.and_then(|f| f.value("q")).unwrap_or("");
    html! {
        (render::live_snapshot(DEVICES))
        form.srch method="post" action=(DEVICES) role="search" {
            (pd.csrf())
            input type="search" name="q" value=(q) placeholder="Search devices" aria-label="Search devices" autocomplete="off" maxlength="64";
            button.btn.sm type="submit" { "Search" }
            @if !q.is_empty() { a.nl href=(DEVICES) { "Cancel" } }
        }
        (list(pd.views, q))
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

    fn views(slots: &[(Topic, Value)]) -> Views {
        let slot = |v: &Value| SlotView { version: 1, at: NOW, value_at: NOW, value: Some(Arc::new(v.clone())), error: None };
        Views::new(NOW, slots.iter().map(|(t, v)| (*t, slot(v))).collect())
    }

    /// The IMEI's last 4 digits only; nothing without 4 digits.
    #[test]
    fn only_the_imei_tail_is_kept() {
        let v = info(&json!({"imei": "000000000000042"}), &json!({"ipv4-address": [{"address": "192.168.0.1"}]}));
        assert_eq!(v, json!({"imei": "\u{2026}0042", "lan": "192.168.0.1"}));
        assert!(!v.to_string().contains("000000000000042"));
        assert_eq!(info(&json!({"imei": "12a"}), &Value::Null), json!({"imei": "", "lan": ""}));
        assert_eq!(info(&Value::Null, &Value::Null)["imei"], "");
    }

    #[test]
    fn signal_bar_colours_follow_the_app() {
        for (bars, class) in [(0, "tn muted"), (1, "tn c-red"), (2, "tn c-orange"), (3, "tn c-green"), (5, "tn c-green"), (-1, "tn muted")] {
            assert_eq!(bars_class(bars), class, "{bars}");
        }
    }

    /// Rows per search: what is shown matches (the masked MAC, never the
    /// hidden octets); no hits: "No Results for …"; none at all: "No
    /// Devices".
    #[test]
    fn devices_search_what_is_shown() {
        let c = json!({
            "hosts": {
                "AA:BB:CC:DD:EE:01": {"name": "tv", "ipaddrs": ["192.168.0.10"]},
                "AA:BB:CC:DD:EE:02": {"name": "phone", "ipaddrs": ["192.168.0.11"]},
                "AA:BB:CC:DD:EE:03": {},
            },
        });
        let v = views(&[(Topic::Clients, c)]);
        let s = list(&v, "").into_string();
        assert!(s.contains("<h2>3 Devices</h2>") && s.contains("<b class=\"dn\">tv</b>"), "{s}");
        // A nameless device shows its masked MAC; one without an address "--".
        assert!(s.contains("<b class=\"dn\">••:••:••:••:EE:03</b><small><span class=\"tn\"><svg"), "{s}");
        assert!(s.contains("</svg></span> -- <span class=\"mono\">••:••:••:••:EE:03</span>"), "{s}");
        assert!(!s.contains("AA:BB"));
        let s = list(&v, "PHO").into_string();
        assert!(s.contains("<h2>1 Devices</h2>") && s.contains("phone") && !s.contains(">tv<"), "{s}");
        assert!(list(&v, "ee:02").into_string().contains("<h2>1 Devices</h2>"));
        assert!(list(&v, "0.10").into_string().contains(">tv<"));
        // The hidden octets never match.
        let s = list(&v, "aa:bb").into_string();
        assert!(s.contains("No Results for \u{201c}aa:bb\u{201d}") && !s.contains("Devices</h2>"), "{s}");
        // A failed read: the error row first.
        let slot = SlotView { version: 1, at: NOW, value_at: NOW, value: None, error: Some("ubus timed out".into()) };
        let s = list(&Views::new(NOW, vec![(Topic::Clients, slot)]), "").into_string();
        assert!(s.starts_with("<div id=\"cd-list\"><p") && s.contains("timed out"), "{s}");
        let none = views(&[(Topic::Clients, json!({"hosts": {}}))]);
        assert!(list(&none, "").into_string().contains("No Devices"));
        assert!(list(&none, "x").into_string().contains("No Devices"));
    }
}
