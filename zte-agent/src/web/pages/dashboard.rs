//! `GET /ui/`: the dashboard, a copy of the app's (`DashboardView`): banners,
//! the USB-C card while a cable is attached (the app's USB Mode sheet,
//! `pages::usb`), then "Connection & signal", "Live traffic", a note for
//! the app's Live signal control, "Battery" beside "CPU & heat", and
//! "Wi-Fi & devices".
//! The cards open their sheets (`pages::sheets`, `pages::signal`,
//! `pages::wifi`), as the app's. Without JavaScript it is a
//! snapshot (the nav bar's "Snapshot" pill reloads it). Each card is one
//! fragment with a stable id, rendered from topic data alone (`FRAGMENTS`),
//! so live updates can replace it by id; the nav bar's age badge is one
//! too. The Wi-Fi on/off word is read only at page load and sits outside
//! every fragment. Only a device count is shown, never MAC addresses or
//! names; SIM data is used for the PIN/PUK banners only.

use maud::{html, Markup};
use serde_json::Value;

use crate::boottime::BootInstant;
use crate::events::Topic;
use crate::handlers::AppState;
use crate::ubus;
use crate::web::fmt;
use crate::web::kit::{self, Icon};
use crate::web::pages::usb;
use crate::web::render::{self, Fragment, Views};
use crate::web::roll::{roll, roll_up_labelled};
use crate::web::screen::PageData;
use crate::web::signal::display_type;

/// Everything the dashboard shows.
pub const TOPICS: [Topic; 10] = [
    Topic::Signal,
    Topic::Traffic,
    Topic::Battery,
    Topic::Charger,
    Topic::BatteryInfo,
    Topic::Health,
    Topic::Clients,
    Topic::Sim,
    Topic::Modem,
    Topic::Usb,
];

pub const FRAGMENTS: [Fragment; 8] = [
    Fragment { id: "banners", topics: &[Topic::Sim, Topic::Modem], render: banners },
    // Its forms are outside it, in the page (`usb::card`).
    Fragment { id: "card-usb", topics: &[Topic::Usb, Topic::Charger], render: usb::fragment },
    // In airplane mode the firmware keeps reporting the last readings: blanked.
    Fragment { id: "card-conn", topics: &[Topic::Signal, Topic::Modem], render: card_conn },
    Fragment { id: "card-traffic", topics: &[Topic::Traffic], render: card_traffic },
    Fragment { id: "card-battery", topics: &[Topic::Battery, Topic::Charger, Topic::BatteryInfo], render: card_battery },
    Fragment { id: "card-health", topics: &[Topic::Health, Topic::Battery], render: card_health },
    Fragment { id: "card-devices", topics: &[Topic::Clients], render: card_devices },
    // The nav bar's trailing badge (`screen::Bar::Status`).
    Fragment { id: "age", topics: &[Topic::Signal], render: age_badge },
];

fn fragment(id: &str, v: &Views) -> Markup {
    render::fragment(&FRAGMENTS, id, v)
}

/// The dashboard's content (`tabs::dashboard`): read 0 is the Wi-Fi switch.
pub fn block(pd: &PageData) -> Markup {
    body(pd.views, pd.read(0).and_then(Value::as_bool), &pd.csrf())
}

/// `wifi_on` as a page-load read.
pub fn wifi_read(_: &AppState, _: Option<&str>) -> Result<Value, String> {
    wifi_on().map(Value::Bool).ok_or_else(|| "Wi-Fi state unreadable".to_owned())
}

/// The master Wi-Fi switch (1-2 `uci get`, no keys): the same option and
/// fallback the Wi-Fi API reads. `None` if unreadable.
fn wifi_on() -> Option<bool> {
    let v = ubus::uci_get("wireless.zte_mbb.wifi_onoff")
        .or_else(|_| ubus::uci_get("zte_mbb.wifi.wifi_onoff"))
        .ok()?;
    match v.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

pub const DASH: &str = "\u{2014}";

/// `csrf`: the session's field, for the USB-C card's forms.
pub fn body(v: &Views, wifi: Option<bool>, csrf: &Markup) -> Markup {
    // Unknown: no state word (the app shows "Wi-Fi off" until its first read).
    let (wifi_text, wifi_class) = match wifi {
        Some(true) => ("Wi-Fi on", "c-green"),
        Some(false) => ("Wi-Fi off", "muted"),
        None => ("", "muted"),
    };
    let wifi_body = html! {
        div.row {
            p #wifi-state class=(wifi_class) { (wifi_text) }
            (fragment("card-devices", v))
        }
        p.cap { "Networks, sharing & connected devices" }
    };
    // The app's Lock Screen "Live signal" control has no web counterpart.
    let live_signal = html! { p.cap { "Lock Screen live signal is in the iOS app." } };
    html! {
        div.dash {
            (fragment("banners", v))
            (usb::card(v, csrf))
            (fragment("card-conn", v))
            (fragment("card-traffic", v))
            (kit::card(None, None, Icon::Antenna, "Live signal", live_signal))
            div.pair {
                (fragment("card-battery", v))
                (fragment("card-health", v))
            }
            (kit::card(None, Some("/ui/wifi"), Icon::Wifi, "Wi-Fi & devices", wifi_body))
        }
    }
}

/// `#age`: how old the signal reading is (the app's "Ns ago" badge),
/// re-rendered with every reading and every heartbeat. The dashboard's
/// counts from the last good reading (the app's moves only when a refresh
/// answered).
pub fn age_badge(v: &Views) -> Markup {
    let s = v.slot(Topic::Signal).filter(|s| s.value.is_some());
    badge(v, s.map(|s| s.value_at))
}

/// The signal sheet's badge counts from the last read, failed or not (the
/// app's sheet sets its time on an error too).
pub fn read_badge(v: &Views) -> Markup {
    let s = v.slot(Topic::Signal).filter(|s| s.value.is_some() || s.error.is_some());
    badge(v, s.map(|s| s.at))
}

/// The badge's text rolls up on a live page (the app's `LastUpdatedView`);
/// its label is what screen readers read.
fn badge(v: &Views, at: Option<BootInstant>) -> Markup {
    let (text, label) = match at.map(|at| v.now.saturating_duration_since(at).as_secs()) {
        None => ("--".to_owned(), "Never updated".to_owned()),
        Some(s) if s < 60 => (format!("{s}s ago"), format!("Updated {s} seconds ago")),
        Some(s) => (format!("{}m {}s ago", s / 60, s % 60), format!("Updated {} minutes {} seconds ago", s / 60, s % 60)),
    };
    html! { span #age .agebadge role="img" aria-label=(label) { (roll_up_labelled(v, "age", &text)) } }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

/// Airplane mode: the modem's operate mode is known and not `ONLINE`.
pub fn airplane(v: &Views) -> bool {
    let mode = v.value(Topic::Modem).map(|m| str_of(&m["status"], "operate_mode")).unwrap_or("");
    !mode.is_empty() && mode != "ONLINE"
}

/// The app's banners, in its order.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Banner {
    Puk,
    Pin,
    Airplane,
    DataOff,
}

/// The banner conditions, in display order. "Mobile data off" uses an
/// exact `connect_status` (the app's substring test reads "disconnected"
/// as connected, so it never shows the banner).
fn banner_list(v: &Views) -> Vec<Banner> {
    let mut out = Vec::new();
    if let Some(sim) = v.value(Topic::Sim) {
        let state = str_of(sim, "sim_states").to_ascii_lowercase();
        let main = str_of(sim, "modem_main_state").to_ascii_lowercase();
        if state == "wait puk" || main == "modem_waitpuk" {
            out.push(Banner::Puk);
        }
        if state == "wait pin" || main == "modem_waitpin" {
            out.push(Banner::Pin);
        }
    }
    let airplane = airplane(v);
    if airplane {
        out.push(Banner::Airplane);
    }
    if let Some(data) = v.value(Topic::Modem).map(|m| &m["data"]).filter(|d| d.is_object()) {
        let connected = matches!(
            str_of(data, "connect_status"),
            "ipv4_connected" | "ipv6_connected" | "ipv4_ipv6_connected"
        );
        if !airplane && fmt::int(&data["enable"]) == Some(0) && !connected {
            out.push(Banner::DataOff);
        }
    }
    out
}

pub fn banners(v: &Views) -> Markup {
    html! {
        div #banners role="status" {
            @for b in banner_list(v) {
                @match b {
                    Banner::Puk => (kit::banner("b-red", Icon::LockWarn, "SIM PUK Required", html! {
                        "Too many wrong PIN attempts. Go to " a href="/ui/router/sim" { "Router > SIM Card" } " to enter your PUK."
                    })),
                    Banner::Pin => (kit::banner("b-orange", Icon::Lock, "SIM PIN Required", html! {
                        "Your SIM card is locked. Go to " a href="/ui/router/sim" { "Router > SIM Card" } " to enter your PIN."
                    })),
                    Banner::Airplane => (kit::banner("b-blue", Icon::Airplane, "Airplane Mode", html! {
                        "Cellular radio is off. The modem is powered down \u{2014} no signal or data."
                    })),
                    Banner::DataOff => (kit::banner("b-orange", Icon::AntennaSlash, "Mobile Data Off", html! {
                        "Cellular radio is on but data is disabled. Go to "
                        a href="/ui/router/mobile-network" { "Router > Mobile Network" }
                        " to enable it."
                    })),
                }
            }
        }
    }
}

/// Operator and network type, then the serving RSRP: no bars, no quality
/// word, no colour (as the app). Blank in airplane mode or with no reading.
pub fn card_conn(v: &Views) -> Markup {
    let sig = v.value(Topic::Signal).filter(|_| !airplane(v));
    let operator = sig.and_then(|s| s["network_provider"].as_str()).map(str::trim).filter(|o| !o.is_empty());
    let kind = sig.map(display_type).filter(|t| !t.is_empty());
    let nr = sig.and_then(|s| fmt::rsrp(s, "nr5g_rsrp"));
    let (label, rsrp) = match nr {
        Some(_) => ("NR RSRP", nr),
        None => ("LTE RSRP", sig.and_then(|s| fmt::rsrp(s, "lte_rsrp"))),
    };
    let roaming = sig.is_some_and(|s| fmt::int(&s["simcard_roam"]) == Some(1));
    kit::card(Some("card-conn"), Some("/ui/signal"), Icon::Antenna, "Connection & signal", html! {
        div.row {
            b.big { (operator.unwrap_or("No operator")) }
            span.rat { (kind.as_deref().unwrap_or(DASH)) }
        }
        div.row.cap {
            span {
                (label) " "
                // The app's `Int(rsrp)`: cut off, not rounded.
                @match rsrp { Some(r) => b.num { (roll(v, "rsrp", &format!("{} dBm", r.trunc() as i64))) }, None => (DASH) }
            }
            span {
                @if roaming { span.roam { "Roaming" } " " }
                "Radio details"
            }
        }
        (render::error_line(v, &[Topic::Signal]))
    })
}

/// The traffic caption (iOS `trafficCaption`): stale after 5 s or on a read
/// error (the age is the last good sample's), in place of the age line. The app's "Estimated cellular traffic"
/// variant never applies: the web reads only the modem's own rates
/// (`network/speeds`) and has no fallback to the app's other sources
/// (`speed`, `traffic`, `rmnet`); without them the card says "Live traffic
/// unavailable".
fn traffic_caption(v: &Views) -> String {
    match v.slot(Topic::Traffic).filter(|s| s.value.is_some()) {
        None => "Live traffic unavailable".into(),
        Some(s) => {
            let age = v.now.saturating_duration_since(s.value_at).as_secs();
            if s.error.is_some() || age >= 5 {
                format!("Last traffic sample \u{b7} {age}s ago \u{b7} waiting for device")
            } else {
                "Live cellular traffic \u{b7} all devices".into()
            }
        }
    }
}

/// The traffic card, opening the Live traffic sheet.
pub fn card_traffic(v: &Views) -> Markup {
    traffic_card(v, Some("/ui/traffic"))
}

/// The same card on the Live traffic sheet itself.
pub fn traffic_sheet(v: &Views) -> Markup {
    traffic_card(v, None)
}

fn traffic_card(v: &Views, href: Option<&str>) -> Markup {
    let t = v.value(Topic::Traffic);
    let num = |key| t.and_then(|t| fmt::float(&t[key]));
    let rate = |key| num(key).map(fmt::rate).unwrap_or_else(|| DASH.into());
    let total = |key| num(key).map(|b| fmt::bytes(b.max(0.0) as u64)).unwrap_or_else(|| DASH.into());
    kit::card(Some("card-traffic"), href, Icon::Arrows, "Live traffic", html! {
        div.split {
            div { (kit::icon(Icon::ArrowDown)) b.big { (roll(v, "rx", &rate("real_rx_speed"))) } span.lbl { "Download" } }
            div { (kit::icon(Icon::ArrowUp)) b.big { (roll(v, "tx", &rate("real_tx_speed"))) } span.lbl { "Upload" } }
        }
        p.cap { (traffic_caption(v)) }
        dl.kv.tot {
            div { dt { "Total DL" } dd { (roll(v, "dl", &total("real_rx_bytes"))) } }
            div { dt { "Total UL" } dd { (roll(v, "ul", &total("real_tx_bytes"))) } }
        }
    })
}

/// Capacity, the status line with the power, and "Power details" (the
/// temperature and times are on the battery page).
pub fn card_battery(v: &Views) -> Markup {
    let b = v.value(Topic::Battery);
    let state = fmt::charge(b, v.value(Topic::Charger));
    let capacity = b.and_then(fmt::capacity);
    let info = v.value(Topic::BatteryInfo);
    let minutes = |key| info.and_then(|i| fmt::int(&i[key]));
    let current = b.is_some_and(|b| fmt::int(&b["current_ua"]).is_some());
    let (status, class) =
        fmt::status_line(state, capacity, current, minutes("battery_time_to_full"), minutes("battery_time_to_empty"));
    // The power takes the status colour; "Unavailable" the level's.
    let power_class = if state == fmt::Charge::Unknown { fmt::level_class(capacity) } else { class };
    kit::card(Some("card-battery"), Some("/ui/battery"), kit::battery_icon(capacity), "Battery", html! {
        p.big { (capacity.map(|c| format!("{c}%")).unwrap_or_else(|| DASH.into())) }
        p.st {
            span class=(class) { (status) }
            @if let Some(p) = b.and_then(fmt::power) { span.muted { " \u{b7} " } span class=(power_class) { (roll(v, "pw", &p)) } }
        }
        p.cap { "Power details" }
        (render::error_line(v, &[Topic::Battery, Topic::Charger, Topic::BatteryInfo]))
    })
}

/// A good reading of `t` younger than 10 s (temperatures show "—" for
/// anything older, as the app's). A failed read is not fresh: its time is
/// the failure's, not the value's.
pub fn fresh(v: &Views, t: Topic) -> bool {
    v.slot(t).is_some_and(|s| s.error.is_none() && v.now.saturating_duration_since(s.at).as_secs() < 10)
}

/// CPU usage in percent, if the reading is plausible.
pub fn cpu_usage(v: &Views) -> Option<f64> {
    v.value(Topic::Health).and_then(|h| fmt::float(&h["cpu"]["overall"])).filter(|c| (0.0..=100.0).contains(c))
}

/// "40.8%" or "—".
pub fn percent(v: Option<f64>) -> String {
    v.map(|c| format!("{c:.1}%")).unwrap_or_else(|| DASH.into())
}

pub fn card_health(v: &Views) -> Markup {
    let h = v.value(Topic::Health);
    let cpu = cpu_usage(v);
    let cpu_temp = h.and_then(|h| fmt::cpu_temp(&h["thermal"])).filter(|_| fresh(v, Topic::Health));
    let bat_temp = v.value(Topic::Battery).and_then(fmt::battery_celsius).filter(|_| fresh(v, Topic::Battery));
    let temp = |key, t: Option<f64>| roll(v, key, &t.map(fmt::celsius).unwrap_or_else(|| DASH.into()));
    kit::card(Some("card-health"), Some("/ui/health"), Icon::Cpu, "CPU & heat", html! {
        p.big { (roll(v, "cpu", &percent(cpu))) }
        dl.kv {
            div { dt { "CPU" } dd { (temp("tc", cpu_temp)) } }
            div { dt { "Battery" } dd { (temp("tb", bat_temp)) } }
        }
        (render::error_line(v, &[Topic::Health, Topic::Battery]))
    })
}

/// The number of connected devices (the count the apps show); no names or
/// addresses.
pub fn card_devices(v: &Views) -> Markup {
    let count = v.value(Topic::Clients).and_then(super::wifi::host_count);
    html! {
        span #card-devices {
            @match count { Some(n) => b.big { (roll(v, "dev", &n.to_string())) }, None => b.big { (DASH) } }
            " " span.lbl { "devices" }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::boottime::BootInstant;
    use crate::events::hub::SlotView;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    const NOW: BootInstant = BootInstant::from_duration(Duration::from_secs(10_000));

    fn slot(value: Value) -> SlotView {
        SlotView { version: 1, at: NOW, value_at: NOW, value: Some(Arc::new(value)), error: None }
    }

    fn views(pairs: Vec<(Topic, Value)>) -> Views {
        Views::new(NOW, pairs.into_iter().map(|(t, v)| (t, slot(v))).collect())
    }

    /// Every topic, with distinctive identifiers that must never be shown.
    pub(crate) fn fixture() -> Views {
        views(vec![
            (Topic::Signal, json!({
                "network_provider": "<script>alert(1)</script>", "network_type": "ENDC",
                "nr5g_rsrp": "-77", "lte_rsrp": "-85", "simcard_roam": "1",
                "sim_iccid": "8900100000009998877", "imsi": "001019999911111", "imei": "000000000000042",
            })),
            (Topic::Traffic, json!({"real_rx_speed": 52, "real_tx_speed": "69", "real_rx_bytes": 1_992_294, "real_tx_bytes": 121_856})),
            (Topic::Battery, json!({"status": "Charging", "capacity": 69, "voltage_uv": 4_435_000, "current_ua": 5_680_000, "temperature": 260})),
            (Topic::Charger, json!({"charger_connect": 1, "charge_status": 1, "direct_power_supply_mode": "disable"})),
            (Topic::BatteryInfo, json!({"battery_time_to_full": 37, "battery_time_to_empty": 1240})),
            (Topic::Health, json!({"cpu": {"overall": 40.84, "cores": [1.0]}, "thermal": {"cpuss_temp": 37.2, "sensors": []}})),
            (Topic::Clients, json!({
                "hosts": {"aa:bb:cc:dd:ee:42": {"name": "alice-laptop-42", "ipaddrs": ["192.168.0.42"]},
                          "aa:bb:cc:dd:ee:43": {"name": "phone"}},
                "dhcp_leases": [{"macaddr": "aa:bb:cc:dd:ee:42", "hostname": "alice-laptop-42"}],
            })),
            (Topic::Wan, json!({
                "wan": {"ipv4-address": [{"address": "10.20.30.40", "mask": 32}]},
                "wan6": {"ipv6-address": [{"address": "fe80::1"}, {"address": "2001:db8::42"}]},
            })),
            (Topic::Sim, json!({
                "sim_states": "Ready", "modem_main_state": "modem_init_complete",
                "sim_iccid": "8900100000009998877", "sim_imsi": "001019999911111", "msisdn": "+10000000042",
                "imei": "000000000000042",
            })),
            (Topic::Modem, json!({
                "status": {"operate_mode": "ONLINE", "imei": "000000000000042"},
                "data": {"enable": 1, "connect_status": "ipv4_ipv6_connected", "key_2g": "WIFIKEY-7f3a"},
            })),
        ])
    }

    fn page_of(v: &Views) -> String {
        body(v, Some(true), &html! {}).into_string()
    }

    /// The whole page as the screen renders it: the nav bar's pill and age
    /// badge around the body.
    fn full_page(v: &Views, wifi: Option<bool>) -> String {
        use crate::web::kit::Tab;
        let pc = crate::web::render::PageCtx {
            title: "Dashboard",
            tab: Some(Tab::Dashboard),
            nonce: "00",
            lead: render::status_pill("/ui/"),
            trail: age_badge(v),
            ..Default::default()
        };
        render::page(&pc, body(v, wifi, &html! {})).into_string()
    }

    #[test]
    fn renders_the_ios_card_values() {
        let s = page_of(&fixture());
        for want in [
            "<a class=\"card\" id=\"card-conn\" href=\"/ui/signal\">", "Connection &amp; signal",
            "<span class=\"rat\">5G NSA</span>", "NR RSRP <b class=\"num\">-77 dBm</b>", "Roaming", "Radio details",
            "416.0 b/s", "552.0 b/s", "Download", "Upload", "1.90 MB", "119 KB", "Live cellular traffic · all devices",
            "Live signal", "Lock Screen live signal is in the iOS app.",
            ">69%<", "<span class=\"c-green\">Charging</span><span class=\"muted\"> · </span><span class=\"c-green\">25.2W</span>",
            "Power details", "<p class=\"big\">40.8%</p>", "37.2 °C", "26.0 °C", "<b class=\"big\">2</b>",
            "<span class=\"lbl\">devices</span>", "Networks, sharing &amp; connected devices", "Wi-Fi on",
        ] {
            assert!(s.contains(want), "missing {want:?} in {s}");
        }
        // Not on the app's dashboard: bars, quality words, the battery's
        // temperature and times, the WAN card, a coloured CPU value.
        for gone in [
            "Excellent", "bars", "Temperature", "Time to Full", "WAN addresses", "10.20.30.40", "2001:db8::42",
            "devices connected", "c-green\">40.8", "4G LTE", "No Service",
        ] {
            assert!(!s.contains(gone), "{gone} in {s}");
        }
        // Battery and CPU & heat side by side, after Live signal.
        let order = ["card-conn", "card-traffic", "Live signal", "class=\"pair\"", "card-battery", "card-health", "wifi-state"];
        let at: Vec<usize> = order.iter().map(|w| s.find(w).unwrap()).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{order:?} at {at:?}");
    }

    #[test]
    fn untrusted_text_is_escaped() {
        let s = page_of(&fixture());
        assert!(!s.contains("<script>"));
        assert!(s.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn no_identifier_reaches_the_page() {
        let full = full_page(&fixture(), Some(true));
        let fragments: String = FRAGMENTS.iter().map(|f| (f.render)(&fixture()).into_string()).collect();
        for secret in [
            "WIFIKEY-7f3a", "001019999911111", "+10000000042", "10000000042", "8900100000009998877", "9998877",
            "000000000000042", "aa:bb:cc:dd:ee:42", "AA:BB:CC", "alice-laptop-42", "192.168.0.42", "10.20.30.40",
            "2001:db8::42",
        ] {
            assert!(!full.contains(secret), "{secret} in page");
            assert!(!fragments.contains(secret), "{secret} in a fragment");
        }
    }

    #[test]
    fn every_fragment_is_in_the_page_byte_for_byte() {
        for v in [fixture(), views(vec![])] {
            let page = full_page(&v, Some(true));
            for f in &FRAGMENTS {
                let html = (f.render)(&v).into_string();
                assert!(html.contains(&format!("id=\"{}\"", f.id)), "{}", f.id);
                assert!(page.contains(&html), "{} differs from the page", f.id);
                assert!(!html.contains("<form") && !html.contains("csrf") && !html.contains("<details") && !html.contains("<input"));
            }
            for id in ["live-status", "wifi-state"] {
                assert!(page.contains(&format!("id=\"{id}\"")), "{id}");
            }
            // The pill leads and the age badge trails the nav bar's title.
            assert!(page.contains("<div class=\"lead\"><p class=\"snap ps\" id=\"live-status\">"), "{page}");
            assert!(page.contains("<h1>Dashboard</h1><div class=\"trail\"><span class=\"agebadge\" id=\"age\""), "{page}");
            // Read at page load only, so never inside a live fragment.
            let devices = card_devices(&v).into_string();
            assert!(!devices.contains("wifi-state") && !devices.contains("Wi-Fi"));
        }
        // The page reads and streams exactly what its fragments need.
        let mut need: Vec<Topic> = FRAGMENTS.iter().flat_map(|f| f.topics.iter().copied()).collect();
        need.dedup_by_key(|t| *t as u8);
        for t in TOPICS {
            assert!(need.contains(&t), "{t:?} unused");
        }
        assert!(need.iter().all(|t| TOPICS.contains(t)));
    }

    #[test]
    fn empty_and_failed_topics() {
        let mut failed = SlotView { version: 1, at: NOW, value_at: NOW, value: None, error: Some(Arc::from("ubus timeout <x>")) };
        let v = Views::new(NOW, vec![(Topic::Traffic, failed.clone())]);
        let s = card_traffic(&v).into_string();
        assert!(s.contains("Live traffic unavailable") && s.contains(">—</b>"), "{s}");
        // A last good value stays; the caption says it is waiting, aged
        // from that value (the app's last sample), not from the failed read.
        failed.value = Some(Arc::new(json!({"real_rx_speed": 1, "real_tx_speed": 1})));
        failed.value_at = at(12);
        let s = card_traffic(&Views::new(NOW, vec![(Topic::Traffic, failed)])).into_string();
        assert!(s.contains("8.0 b/s") && s.contains("Last traffic sample · 12s ago · waiting for device"), "{s}");
        // Other cards keep the error line under them.
        let bad = SlotView { version: 1, at: NOW, value_at: NOW, value: None, error: Some(Arc::from("no battery <y>")) };
        let s = card_battery(&Views::new(NOW, vec![(Topic::Battery, bad)])).into_string();
        assert!(s.contains("Couldn't update: no battery &lt;y&gt;") && s.contains("Unavailable"), "{s}");
        let s = page_of(&views(vec![]));
        for want in ["No operator", "LTE RSRP —", "Live traffic unavailable", "Unavailable", "<b class=\"big\">—</b>"] {
            assert!(s.contains(want), "{want}");
        }
        // An unknown Wi-Fi state shows no word.
        let unknown = body(&views(vec![]), None, &html! {}).into_string();
        assert!(unknown.contains("<p id=\"wifi-state\" class=\"muted\"></p>") && !unknown.contains("Wi-Fi o"), "{unknown}");
        assert!(body(&views(vec![]), Some(false), &html! {}).into_string().contains("Wi-Fi off"));
    }

    fn at(secs: u64) -> BootInstant {
        BootInstant::from_duration(Duration::from_secs(10_000 - secs))
    }

    #[test]
    fn traffic_caption_and_age_lines() {
        let traffic = |secs| {
            let s = SlotView { version: 1, at: at(secs), value_at: at(secs), value: Some(Arc::new(json!({"real_rx_speed": 1}))), error: None };
            card_traffic(&Views::new(NOW, vec![(Topic::Traffic, s)])).into_string()
        };
        assert!(traffic(4).contains("Live cellular traffic"));
        assert!(traffic(5).contains("Last traffic sample · 5s ago · waiting for device"));
        assert!(!traffic(600).contains("class=\"age\""), "the caption replaces the age line");
        // The cards say nothing about age (the app has only the nav bar's
        // badge); the sheets' age line shows after 3 cadences.
        let views_at = |t: Topic, secs| {
            let s = SlotView { version: 1, at: at(secs), value_at: at(secs), value: Some(Arc::new(json!({"capacity": 50}))), error: None };
            Views::new(NOW, vec![(t, s)])
        };
        assert!(!card_battery(&views_at(Topic::Battery, 600)).into_string().contains("class=\"age\""));
        assert!(!card_health(&views_at(Topic::Health, 600)).into_string().contains("class=\"age\""));
        let line = |t, secs| render::age_line(&views_at(t, secs), &[t]).into_string();
        // Battery every 5 s, health every 2 s: the seconds below 10, then 5 s steps.
        assert_eq!(line(Topic::Battery, 14), "");
        assert_eq!(line(Topic::Battery, 17), "<p class=\"age\">Updated 15 s ago</p>");
        assert_eq!(line(Topic::Battery, 600), "<p class=\"age\">Updated 10 min ago</p>");
        assert_eq!(line(Topic::Health, 5), "");
        assert_eq!(line(Topic::Health, 7), "<p class=\"age\">Updated 7 s ago</p>");
        // WAN has no cadence (event-driven): never "old".
        assert_eq!(line(Topic::Wan, 3600), "");
    }

    #[test]
    fn health_temperatures_blank_after_10_s() {
        let health = json!({"cpu": {"overall": 12.34}, "thermal": {"cpuss_temp": 50, "sensors": [{"name": "cpuss-1", "temperature_c": 55.5}]}});
        let card = |secs| {
            let s = SlotView { version: 1, at: at(secs), value_at: at(secs), value: Some(Arc::new(health.clone())), error: None };
            card_health(&Views::new(NOW, vec![(Topic::Health, s)])).into_string()
        };
        assert!(card(9).contains("<p class=\"big\">12.3%</p>") && card(9).contains("55.5 °C"));
        assert!(card(10).contains("12.3%") && !card(10).contains("°C"), "{}", card(10));
        let s = card_health(&views(vec![(Topic::Health, json!({"cpu": {"overall": 140}}))])).into_string();
        assert!(s.contains("<p class=\"big\">—</p>"));
    }

    #[test]
    fn age_badge_ticks_from_the_signal_read() {
        let badge = |secs| {
            let s = SlotView { version: 1, at: at(secs), value_at: at(secs), value: Some(Arc::new(json!({}))), error: None };
            age_badge(&Views::new(NOW, vec![(Topic::Signal, s)])).into_string()
        };
        assert_eq!(badge(0), "<span class=\"agebadge\" id=\"age\" role=\"img\" aria-label=\"Updated 0 seconds ago\">0s ago</span>");
        assert!(badge(59).contains(">59s ago<"));
        assert!(badge(65).contains("aria-label=\"Updated 1 minutes 5 seconds ago\">1m 5s ago<"));
        let never = SlotView { version: 0, at: NOW, value_at: NOW, value: None, error: None };
        let s = age_badge(&Views::new(NOW, vec![(Topic::Signal, never)])).into_string();
        assert!(s.contains("aria-label=\"Never updated\">--<"), "{s}");
        assert!(age_badge(&views(vec![])).into_string().contains(">--<"));
        // Reads failing since a good one 30 s ago: the dashboard's badge
        // keeps counting from the good one (the app's `lastUpdated`); the
        // signal sheet's counts from the failed read (its app sheet does).
        let failing = SlotView { version: 2, at: NOW, value_at: at(30), value: Some(Arc::new(json!({}))), error: Some(Arc::from("timeout")) };
        let v = Views::new(NOW, vec![(Topic::Signal, failing)]);
        assert!(age_badge(&v).into_string().contains(">30s ago<"));
        assert!(read_badge(&v).into_string().contains(">0s ago<"));
        let failed = SlotView { version: 1, at: NOW, value_at: NOW, value: None, error: Some(Arc::from("timeout")) };
        let v = Views::new(NOW, vec![(Topic::Signal, failed)]);
        assert!(age_badge(&v).into_string().contains(">--<") && read_badge(&v).into_string().contains(">0s ago<"));
    }

    #[test]
    fn battery_card_follows_the_app() {
        let card = |b: Value, info: Value| {
            card_battery(&views(vec![(Topic::Battery, b), (Topic::BatteryInfo, info)])).into_string()
        };
        let s = card(json!({"status": "Discharging", "capacity": 69, "voltage_uv": 3_900_000, "current_ua": -480_000}),
                      json!({"battery_time_to_empty": 1240}));
        assert!(s.contains("<span class=\"c-green\">20h 40m</span><span class=\"muted\"> · </span><span class=\"c-green\">1.9W</span>"), "{s}");
        let s = card(json!({"status": "Discharging", "capacity": 15}), json!({"battery_time_to_empty": 45}));
        assert!(s.contains("<span class=\"c-red\">45m left</span>") && !s.contains(" · "), "{s}");
        let s = card(json!({"status": "Charging", "capacity": 60, "current_ua": 120_000}), json!({"battery_time_to_full": 65}));
        assert!(s.contains(">Charging</span>") && s.contains(">+120mA<") && !s.contains("1h 5m"), "{s}");
        // The level icon (75 % and up full).
        assert!(s.contains("width=\"8.2\""), "{s}");
    }

    #[derive(Debug, PartialEq)]
    struct Shown(Vec<Banner>);

    fn banner_titles(pairs: Vec<(Topic, Value)>) -> Shown {
        Shown(banner_list(&views(pairs)))
    }

    #[test]
    fn banner_rules() {
        assert_eq!(banner_titles(vec![]), Shown(vec![]));
        assert!(banners(&views(vec![])).into_string().contains("id=\"banners\""));
        let sim = |state: &str, main: &str| (Topic::Sim, json!({"sim_states": state, "modem_main_state": main}));
        assert_eq!(banner_titles(vec![sim("Wait PUK", "")]), Shown(vec![Banner::Puk]));
        assert_eq!(banner_titles(vec![sim("", "MODEM_WAITPIN")]), Shown(vec![Banner::Pin]));
        let modem = |mode: &str, enable: Value, status: &str| {
            (Topic::Modem, json!({"status": {"operate_mode": mode}, "data": {"enable": enable, "connect_status": status}}))
        };
        assert_eq!(banner_titles(vec![modem("LPM", json!(0), "disconnected")]), Shown(vec![Banner::Airplane]));
        // "disconnected" is not connected (unlike a substring test).
        assert_eq!(banner_titles(vec![modem("ONLINE", json!("0"), "disconnected")]), Shown(vec![Banner::DataOff]));
        assert_eq!(banner_titles(vec![modem("ONLINE", json!(0), "ipv4_connected")]), Shown(vec![]));
        assert_eq!(banner_titles(vec![modem("ONLINE", json!(1), "disconnected")]), Shown(vec![]));
        // No "No Service" banner (the app has none).
        assert_eq!(banner_titles(vec![(Topic::Signal, json!({"network_type": "NO_SERVICE"}))]), Shown(vec![]));
        assert_eq!(
            banner_titles(vec![sim("wait puk", "modem_waitpin"), modem("ONLINE", json!(0), "")]),
            Shown(vec![Banner::Puk, Banner::Pin, Banner::DataOff])
        );
    }

    #[test]
    fn banner_copy_is_the_apps() {
        let all = views(vec![
            (Topic::Sim, json!({"sim_states": "wait puk", "modem_main_state": "modem_waitpin"})),
            (Topic::Modem, json!({"status": {"operate_mode": "LPM"}})),
        ]);
        let s = banners(&all).into_string();
        for (class, title, text) in [
            ("b-red", "SIM PUK Required", "Too many wrong PIN attempts. Go to <a href=\"/ui/router/sim\">Router &gt; SIM Card</a> to enter your PUK."),
            ("b-orange", "SIM PIN Required", "Your SIM card is locked. Go to <a href=\"/ui/router/sim\">Router &gt; SIM Card</a> to enter your PIN."),
            ("b-blue", "Airplane Mode", "Cellular radio is off. The modem is powered down — no signal or data."),
        ] {
            assert!(s.contains(&format!("<div class=\"banner {class}\"><svg")), "{class}");
            // PUK: the lock with "!" (the app's `exclamationmark.lock.fill`).
            let icon = if title == "SIM PUK Required" { Icon::LockWarn } else if title.starts_with("SIM") { Icon::Lock } else { Icon::Airplane };
            assert!(s.contains(&format!("{}<div><strong>{title}", kit::icon(icon).into_string())), "{title}");
            assert!(s.contains(&format!("<strong>{title}</strong><span>{text}</span>")), "{title} in {s}");
        }
        let off = views(vec![(Topic::Modem, json!({"status": {"operate_mode": "ONLINE"}, "data": {"enable": 0}}))]);
        assert!(banners(&off).into_string().contains(
            "<strong>Mobile Data Off</strong><span>Cellular radio is on but data is disabled. Go to \
             <a href=\"/ui/router/mobile-network\">Router &gt; Mobile Network</a> to enable it.</span>"
        ));
    }

    #[test]
    fn card_conn_is_blank_in_airplane_mode() {
        let mut v = fixture();
        v.set(Topic::Modem, slot(json!({"status": {"operate_mode": "LPM"}})));
        let s = card_conn(&v).into_string();
        assert!(s.contains("No operator") && s.contains("LTE RSRP —") && s.contains("Radio details"), "{s}");
        for gone in ["-77", "5G NSA", "Roaming", "&lt;script"] {
            assert!(!s.contains(gone), "{gone} in {s}");
        }
    }

    #[test]
    fn connection_card_falls_back_to_lte() {
        let v = views(vec![(Topic::Signal, json!({"network_type": "LTE", "lte_rsrp": -105, "nr5g_rsrp": "0", "network_provider": ""}))]);
        let s = card_conn(&v).into_string();
        for want in ["No operator", "<span class=\"rat\">4G</span>", "LTE RSRP <b class=\"num\">-105 dBm</b>"] {
            assert!(s.contains(want), "{want} in {s}");
        }
        for gone in ["Roaming", "Fair", "q-fair", "bars"] {
            assert!(!s.contains(gone), "{gone} in {s}");
        }
        // The app's `Int(rsrp)` cuts the decimals off.
        let v = views(vec![(Topic::Signal, json!({"network_type": "LTE", "lte_rsrp": "-105.7"}))]);
        assert!(card_conn(&v).into_string().contains("LTE RSRP <b class=\"num\">-105 dBm</b>"));
        let v = views(vec![(Topic::Signal, json!({"network_type": "NO_SERVICE"}))]);
        assert!(card_conn(&v).into_string().contains("<span class=\"rat\">NO_SERVICE</span>"));
        let v = views(vec![(Topic::Signal, json!({"network_type": ""}))]);
        assert!(card_conn(&v).into_string().contains("<span class=\"rat\">—</span>"));
    }
}
