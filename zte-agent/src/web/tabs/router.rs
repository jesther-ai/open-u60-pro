//! Router tab: the list of the app's router screens (`RouterSettingsListView`)
//! and those the web UI has: Mobile Network (`pages::mobile`; with its
//! "Reboot Required" page), Network Mode (the dashboard's screen, mounted
//! here too), Cell Lock, Smart Tower Connect and Signal Detection
//! (`pages::cell`), SIM Card with its sheets (`pages::sim`), SIM Services
//! (`pages::services`), WiFi and Guest WiFi (`pages::wlan`), APN
//! (`pages::apn`), LAN / DHCP (`pages::lan`), DNS with its cache inspector
//! (`pages::dns`), Firewall with New Port Forward (`pages::firewall`),
//! Telemetry Blocker (`pages::telemetry`), VPN Passthrough and QoS
//! (`pages::vpnqos`; their changes are `pages::netcfg`'s), and Device
//! Controls with its Reboot Router and Factory Reset sheets
//! (`pages::devctl`). Every change is a
//! POST with the session's CSRF token, answered with 303; turning
//! something off that cuts the connection first goes through a
//! confirmation page. Mobile data and airplane mode continue on an
//! operation page that follows the modem until the change has settled;
//! the changes that hand out single-use tokens (Apply, a scan, a
//! registration, a cell lock, the STC switches, a detection) are
//! `pages::confirm`'s.

use std::time::Duration;

use maud::html;
use serde_json::json;

use crate::events::Topic;
use crate::server::Method;
use crate::web::action::{Action, After, Again, Confirm, Field, Kind, Need};
use crate::web::kit::{Icon, Tab};
use crate::web::ops::{AIRPLANE_OFF, AIRPLANE_ON, DATA_OFF, DATA_ON};
use crate::web::pages::{apn, cell, devctl, dns, esim, esim_add, firewall, lan, mobile, services, sim, telemetry, vpnqos, wlan};
use crate::web::screen::{Bar, Block, Item, Read, Screen, Search};

const fn to(href: &'static str, label: &'static str, icon: Icon) -> Item {
    Item { label, icon, href, sub: "" }
}

pub static ROUTER: Screen = Screen {
    path: "/ui/router",
    id: "router",
    title: "Router",
    tab: Tab::Router,
    blocks: &[
        Block::Links(
            "Cellular",
            &[
                to(mobile::PATH, "Mobile Network", Icon::Bars),
                to("/ui/router/network-mode", "Network Mode", Icon::Antenna),
                to(cell::CELL, "Cell Lock", Icon::Lock),
                to(cell::STC, "Smart Tower Connect", Icon::Antenna),
                to(cell::SD, "Signal Detection", Icon::Waveform),
                to(sim::PATH, "SIM Card", Icon::SimCard),
                to(esim::PATH, "eSIM Profiles", Icon::SimCard),
                to(services::PATH, "SIM Services", Icon::Phone),
            ],
        ),
        Block::Links(
            "Connectivity",
            &[
                to(wlan::WIFI, "WiFi", Icon::Wifi),
                to(wlan::GUEST, "Guest WiFi", Icon::People),
                to(apn::PATH, "APN", Icon::SimCard),
                to(lan::PATH, "LAN / DHCP", Icon::Network),
                to(dns::PATH, "DNS", Icon::Globe),
            ],
        ),
        Block::Links(
            "Security",
            &[
                to(firewall::PATH, "Firewall", Icon::Shield),
                to(telemetry::PATH, "Telemetry Blocker", Icon::EyeSlash),
                to(vpnqos::VPN, "VPN Passthrough", Icon::LockShield),
            ],
        ),
        Block::Links("Quality", &[to(vpnqos::QOS, "QoS", Icon::Gauge)]),
        Block::Links("System", &[to(devctl::PATH, "Device Controls", Icon::Power)]),
    ],
    ..Screen::BLANK
};

const BACK: Bar = Bar::Back("/ui/router", "Router");
const PUSHED: Screen = Screen { tab: Tab::Router, bar: BACK, ..Screen::BLANK };

/// Read at page load (the app reads once): the data call, the modem's
/// mode, the selection mode (`signal`) and, coming back from a scan, the
/// networks found.
pub static MOBILE_NETWORK: Screen = Screen {
    path: mobile::PATH,
    id: "mobilenetwork",
    title: "Mobile Network",
    topics: &[Topic::Signal],
    reads: &[Read::Fn(mobile::wwan), Read::Api("/api/modem/status"), Read::Fn(mobile::networks)],
    blocks: &[Block::Custom(mobile::page)],
    ..PUSHED
};

/// The app's "Reboot Required" alert, after airplane mode off failed twice
/// (its post: `pages::devctl::post`).
pub static REBOOT_REQUIRED: Screen = Screen {
    path: mobile::REBOOT,
    id: "rebootrequired",
    title: "Reboot Required",
    bar: Bar::Back(mobile::PATH, "Mobile Network"),
    blocks: &[Block::Custom(mobile::reboot_page)],
    ..PUSHED
};

/// Read at page load: the reading's cell (`signal`) and, coming back from
/// a neighbour scan, the neighbour lists.
pub static CELL_LOCK: Screen = Screen {
    path: cell::CELL,
    id: "celllock",
    title: "Cell Lock",
    topics: &[Topic::Signal],
    reads: &[Read::Fn(cell::neighbors)],
    blocks: &[Block::Custom(cell::cell_lock)],
    ..PUSHED
};

pub static STC: Screen = Screen {
    path: cell::STC,
    id: "stc",
    title: "Smart Tower Connect",
    reads: &[Read::Api("/api/cell/stc/params"), Read::Api("/api/cell/stc/status")],
    saved: &[("params", "STC parameters updated")],
    saved_ok: true,
    blocks: &[Block::Custom(cell::stc)],
    ..PUSHED
};

/// Live from `signal_detect` only while a sweep runs; its one message line
/// is drawn by the page.
pub static SIGNAL_DETECT: Screen = Screen {
    path: cell::SD,
    id: "signaldetect",
    title: "Signal Detection",
    topics: &[Topic::SignalDetect],
    live: &cell::FRAGMENTS,
    blocks: &[Block::Custom(cell::signal_detect)],
    arg: Some(cell::sd_arg),
    refresh: Some(cell::sd_refresh),
    own_notice: true,
    live_while: Some(cell::running),
    ..PUSHED
};

/// Live from `sim` (with its SIM identity stamp); SIM Lock is read at page load.
pub static SIM: Screen = Screen {
    path: sim::PATH,
    id: "sim",
    title: "SIM Card",
    topics: &[Topic::Sim],
    reads: &[Read::Api(sim::TRIALS)],
    live: &sim::FRAGMENTS,
    blocks: &[Block::Custom(sim::page)],
    arg: Some(sim::epoch),
    ..PUSHED
};

/// The app's sheets over SIM Card ("Cancel"), each its own page; the SIM
/// read at page load for the attempts left. Not live (forms).
const SHEET: Screen = Screen { tab: Tab::Router, bar: Bar::Cancel(sim::PATH), topics: &[Topic::Sim], own_notice: true, ..Screen::BLANK };

pub static SIM_PIN: Screen = Screen { path: sim::PIN, id: "simpin", title: "Enter PIN", blocks: &[Block::Custom(sim::pin_sheet)], ..SHEET };
pub static SIM_LOCK_ON: Screen =
    Screen { path: sim::LOCK_ON, id: "simlockon", title: "Enable PIN Lock", blocks: &[Block::Custom(sim::lock_on_sheet)], ..SHEET };
pub static SIM_LOCK_OFF: Screen =
    Screen { path: sim::LOCK_OFF, id: "simlockoff", title: "Disable PIN Lock", blocks: &[Block::Custom(sim::lock_off_sheet)], ..SHEET };
pub static SIM_CHANGE: Screen = Screen { path: sim::CHANGE, id: "simchange", title: "Change PIN", blocks: &[Block::Custom(sim::change_sheet)], ..SHEET };
pub static SIM_PUK: Screen = Screen { path: sim::PUK, id: "simpuk", title: "Enter PUK", blocks: &[Block::Custom(sim::puk_sheet)], ..SHEET };
pub static SIM_NCK: Screen =
    Screen { path: sim::NCK, id: "simnck", title: "SIM Unlock", reads: &[Read::Api(sim::TRIALS)], blocks: &[Block::Custom(sim::nck_sheet)], ..SHEET };

/// USSD and the SIM Toolkit: the toolkit session is opened before the
/// `stk` topic is read, then followed live (the topic costs nothing while
/// the worker is idle; not after a failed action, as the app); without
/// JavaScript the page reloads while the SIM works.
pub static SIM_SERVICES: Screen = Screen {
    path: services::PATH,
    id: "simservices",
    title: "SIM Services",
    topics: &[Topic::Stk],
    live: &services::FRAGMENTS,
    blocks: &[Block::Custom(services::page)],
    arg: Some(services::arg),
    refresh: Some(services::refresh),
    live_while: Some(services::live),
    before: Some(services::open),
    ..PUSHED
};

/// eSIM Profiles: the list and the chip read at page load (card commands,
/// the slow lane); live from the switch, the download, the data connection
/// and the signal reading (for the connection card: every 10 s, near the
/// app's 12 s idle check).
pub static ESIM: Screen = Screen {
    path: esim::PATH,
    id: "esim",
    title: "eSIM Profiles",
    bar: Bar::BackIcons("/ui/router", "Router", &[(esim::ADD, Icon::Plus, "Add eSIM"), (esim::PATH, Icon::Refresh, "Refresh profiles and connection")]),
    topics: &[Topic::EsimSwitch, Topic::EsimDownload, Topic::Modem, Topic::Signal],
    reads: &[Read::Api(esim::PROFILES), Read::Api(esim::CHIP)],
    live: &esim::FRAGMENTS,
    blocks: &[Block::Custom(esim::page)],
    arg: Some(esim::arg),
    own_notice: true,
    notice_view: Some(esim::message),
    signal_every: Some(std::time::Duration::from_secs(10)),
    ..PUSHED
};

/// The app's dialogs and sheets over eSIM Profiles, each its own page: the
/// list read at page load (to find the profile by its key), the switch and
/// download state (nothing is offered while either holds the eSIM).
const ESIM_PAGE: Screen = Screen {
    tab: Tab::Router,
    bar: Bar::Back(esim::PATH, "eSIM Profiles"),
    topics: &[Topic::EsimSwitch, Topic::EsimDownload],
    reads: &[Read::Api(esim::PROFILES)],
    status: Some(esim::status),
    ..Screen::BLANK
};

pub static ESIM_SWITCH: Screen = Screen { path: esim::SWITCH, id: "esimswitch", title: "Confirm", blocks: &[Block::Custom(esim::switch_page)], ..ESIM_PAGE };
pub static ESIM_DELETE: Screen = Screen { path: esim::DELETE, id: "esimdelete", title: "Confirm", blocks: &[Block::Custom(esim::delete_page)], ..ESIM_PAGE };
pub static ESIM_RENAME: Screen = Screen {
    path: esim::RENAME,
    id: "esimrename",
    title: "Rename profile",
    bar: Bar::Cancel(esim::PATH),
    topics: &[],
    blocks: &[Block::Custom(esim::rename_page)],
    own_notice: true,
    ..ESIM_PAGE
};

/// Add eSIM (the app's sheet over eSIM Profiles): the photo or the code,
/// then the download, live from `esim_download` while it runs or waits for
/// "Install profile"; the data connection and the switch read at page load
/// (the app's "no mobile data" note; nothing starts during a switch).
pub static ESIM_ADD: Screen = Screen {
    path: esim::ADD,
    id: "esimadd",
    title: "Add eSIM",
    tab: Tab::Router,
    bar: Bar::DoneAs(esim_add::bar),
    topics: &[Topic::EsimDownload],
    reads: &[Read::Api("/api/modem/data"), Read::Api("/api/esim/switch")],
    live: &esim_add::FRAGMENTS,
    blocks: &[Block::Custom(esim_add::page)],
    refresh: Some(esim_add::refresh),
    live_while: Some(esim_add::live),
    own_notice: true,
    notice_view: Some(esim_add::note),
    ..Screen::BLANK
};

/// WiFi Settings: read at page load (the status without its passwords).
pub static WIFI: Screen = Screen {
    path: wlan::WIFI,
    id: "wifisettings",
    title: "WiFi Settings",
    reads: &[Read::Fn(wlan::settings)],
    blocks: &[Block::Custom(wlan::page)],
    ..PUSHED
};

pub static GUEST: Screen = Screen {
    path: wlan::GUEST,
    id: "guestwifi",
    title: "Guest WiFi",
    reads: &[Read::Fn(wlan::guest_settings)],
    blocks: &[Block::Custom(wlan::guest_page)],
    ..PUSHED
};

/// APN Settings: the mode and both lists (without passwords).
pub static APN: Screen = Screen {
    path: apn::PATH,
    id: "apn",
    title: "APN Settings",
    reads: &[Read::Fn(apn::read)],
    blocks: &[Block::Custom(apn::page)],
    ..PUSHED
};

/// The app's sheets over APN Settings: New APN, Edit APN ("Cancel"),
/// Auto APN ("Done").
pub static APN_NEW: Screen = Screen {
    path: apn::NEW,
    id: "apnnew",
    title: "New APN",
    bar: Bar::Cancel(apn::PATH),
    blocks: &[Block::Custom(apn::form)],
    ..PUSHED
};

pub static APN_EDIT: Screen = Screen {
    path: apn::EDIT,
    id: "apnedit",
    title: "Edit APN",
    bar: Bar::Cancel(apn::PATH),
    reads: &[Read::Fn(apn::read)],
    blocks: &[Block::Custom(apn::edit_form)],
    status: Some(apn::edit_status),
    ..PUSHED
};

pub static APN_AUTO: Screen = Screen {
    path: apn::AUTO,
    id: "apnauto",
    title: "Auto APN",
    bar: Bar::Done(apn::PATH),
    reads: &[Read::Fn(apn::read)],
    blocks: &[Block::Custom(apn::auto_page)],
    status: Some(apn::auto_status),
    ..PUSHED
};

pub static LAN: Screen = Screen {
    path: lan::PATH,
    id: "lan",
    title: "LAN / DHCP",
    reads: &[Read::Api("/api/router/lan")],
    blocks: &[Block::Custom(lan::page)],
    ..PUSHED
};

/// DNS Settings: the router's DNS and the DoH status.
pub static DNS: Screen = Screen {
    path: dns::PATH,
    id: "dns",
    title: "DNS Settings",
    reads: &[Read::Api("/api/router/dns"), Read::Api("/api/doh/status")],
    blocks: &[Block::Custom(dns::page)],
    ..PUSHED
};

/// The DoH cache inspector (the app's sheet, "Done"): searched by a post.
pub static DNS_CACHE: Screen = Screen {
    path: dns::CACHE,
    id: "dnscache",
    title: "DNS Cache",
    // The app's "Done" leads (its cancellation action).
    bar: Bar::DoneTrail(dns::PATH, |_| html! {}),
    reads: &[Read::Api("/api/doh/cache"), Read::Api("/api/doh/status")],
    blocks: &[Block::Custom(dns::cache)],
    search: Search::Still,
    heading: Some(dns::cache_heading),
    ..PUSHED
};

/// Firewall: its settings, the port forwarding rules, the filters and
/// UPnP, each read at page load (the app's four reads).
pub static FIREWALL: Screen = Screen {
    path: firewall::PATH,
    id: "firewall",
    title: "Firewall",
    reads: &[Read::Api(firewall::API), Read::Api(firewall::PF_API), Read::Api(firewall::FILTER_API), Read::Api(firewall::UPNP_API)],
    blocks: &[Block::Custom(firewall::page)],
    ..PUSHED
};

/// The app's sheet over Firewall ("Cancel").
pub static PF_NEW: Screen = Screen {
    path: firewall::NEW,
    id: "portforwardnew",
    title: "New Port Forward",
    bar: Bar::Cancel(firewall::PATH),
    blocks: &[Block::Custom(firewall::rule_form)],
    ..PUSHED
};

pub static TELEMETRY: Screen = Screen {
    path: telemetry::PATH,
    id: "telemetry",
    title: "Telemetry Blocker",
    reads: &[Read::Api(telemetry::API)],
    blocks: &[Block::Custom(telemetry::page)],
    ..PUSHED
};

pub static VPN: Screen = Screen {
    path: vpnqos::VPN,
    id: "vpn",
    title: "VPN Passthrough",
    reads: &[Read::Api("/api/router/vpn")],
    blocks: &[Block::Custom(vpnqos::vpn_page)],
    ..PUSHED
};

pub static QOS: Screen = Screen {
    path: vpnqos::QOS,
    id: "qos",
    title: "QoS",
    reads: &[Read::Api("/api/router/qos")],
    blocks: &[Block::Custom(vpnqos::qos_page)],
    ..PUSHED
};

/// Device Controls: the app's four reads at page load (the power-save
/// mode's without a POST, `devctl::power_save`).
pub static DEVICE: Screen = Screen {
    path: devctl::PATH,
    id: "device",
    title: "Device Controls",
    reads: &[
        Read::Api("/api/device/charge-control"),
        Read::Fn(devctl::power_save),
        Read::Api(devctl::FAST_BOOT_API),
        Read::Api(devctl::AUTO_SLEEP_API),
    ],
    blocks: &[Block::Custom(devctl::page)],
    ..PUSHED
};

/// The app's password sheets over Device Controls ("Cancel"); their posts
/// are `devctl::post`'s.
pub static DEVICE_REBOOT: Screen = Screen {
    path: devctl::REBOOT,
    id: "reboot",
    title: "Reboot Router",
    bar: Bar::Cancel(devctl::PATH),
    blocks: &[Block::Custom(devctl::reboot_sheet)],
    ..PUSHED
};

pub static DEVICE_RESET: Screen = Screen {
    path: devctl::RESET,
    id: "factoryreset",
    title: "Factory Reset",
    bar: Bar::Cancel(devctl::PATH),
    blocks: &[Block::Custom(devctl::reset_sheet)],
    ..PUSHED
};

/// The remote line of a change that surely cuts the connection (the same
/// opening as `confirm::REMOTE`).
const CUT: &str = "You are connected remotely: this cuts the connection, and this page can\u{2019}t be reached until it is turned back on at the router.";
const AIRPLANE_LINE: &str = "The cellular radio turns off; calls, SMS and internet stop.";
const ENABLE: &[Field] = &[Field { name: "enable", kind: Kind::Choice(&["0", "1"]) }];

pub static MOBILE_DATA: Action = Action {
    path: "/ui/router/mobile-network/mobile-data",
    screen: &MOBILE_NETWORK,
    fields: ENABLE,
    confirm: &[
        Confirm {
            when: ("enable", "0"),
            need: Need::Always,
            title: "Turn off mobile data?",
            lines: &["All connected devices lose internet access. Wi-Fi and this page keep working."],
            remote: Some(&["All connected devices lose internet access. Wi-Fi keeps working.", CUT]),
            label: "Turn off mobile data",
        },
        Confirm {
            when: ("enable", "1"),
            need: Need::Never,
            title: "Turn on mobile data?",
            lines: &[],
            remote: None,
            label: "Turn on mobile data",
        },
    ],
    confirm_at: None,
    unchanged: None,
    reads: &[Read::Fn(mobile::wwan)],
    call: (Method::Put, "/api/modem/data"),
    body: |v, cur| Some(mobile::data_body(&cur[0], v.get("enable") == "1")),
    after: After::Op(|v| (if v.get("enable") == "1" { &DATA_ON } else { &DATA_OFF }, "")),
    again: None,
    once: true,
};

/// The app tries going online twice, 3 s apart, then shows "Reboot Required".
const ONLINE_PAUSE: Duration = if cfg!(test) { Duration::from_millis(20) } else { Duration::from_secs(3) };

/// Going online is an AT command of up to 8 s (the API's slow lane).
pub static AIRPLANE: Action = Action {
    path: "/ui/router/mobile-network/airplane",
    screen: &MOBILE_NETWORK,
    fields: &[Field { name: "mode", kind: Kind::Choice(&["LPM", "ONLINE"]) }],
    confirm: &[
        Confirm {
            when: ("mode", "LPM"),
            need: Need::Always,
            title: "Turn on airplane mode?",
            lines: &[AIRPLANE_LINE],
            remote: Some(&[AIRPLANE_LINE, CUT]),
            label: "Turn on airplane mode",
        },
        Confirm {
            when: ("mode", "ONLINE"),
            need: Need::Never,
            title: "Turn off airplane mode?",
            lines: &[],
            remote: None,
            label: "Turn off airplane mode",
        },
    ],
    confirm_at: None,
    unchanged: None,
    reads: &[],
    call: (Method::Post, "/api/modem/airplane"),
    body: |v, _| Some(json!({"operate_mode": v.get("mode")})),
    after: After::Op(|v| (if v.get("mode") == "LPM" { &AIRPLANE_ON } else { &AIRPLANE_OFF }, "")),
    again: Some(Again { when: ("mode", "ONLINE"), pause: ONLINE_PAUSE, then: mobile::REBOOT }),
    once: true,
};

/// Checked against the same ranges as the charge policy; the page's
/// single-use token (a double click saves once).
pub static CHARGE_LIMIT: Action = Action {
    path: devctl::CHARGE,
    screen: &DEVICE,
    fields: &[
        Field { name: "enabled", kind: Kind::Check },
        Field { name: "limit", kind: Kind::Int(50, 100) },
        Field { name: "hysteresis", kind: Kind::Int(1, 20) },
    ],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: &[],
    call: (Method::Put, "/api/device/charge-control"),
    body: |v, _| {
        Some(json!({"charge_limit_enabled": v.get("enabled") == "1", "charge_limit": v.int("limit"), "hysteresis": v.int("hysteresis")}))
    },
    // The page reads the new limit back (the app shows no line).
    after: After::Back(|_| String::new()),
    again: None,
    once: true,
};

const STC_NUMBER: Kind = Kind::Int(0, 99_999);

/// "Apply Parameters": whole numbers (the app checks nothing), sent as
/// strings under the firmware's names.
pub static STC_PARAMS: Action = Action {
    path: cell::STC_PARAMS,
    screen: &STC,
    fields: &[
        Field { name: "lte_timer", kind: STC_NUMBER },
        Field { name: "nrsa_timer", kind: STC_NUMBER },
        Field { name: "lte_max", kind: STC_NUMBER },
        Field { name: "nrsa_max", kind: STC_NUMBER },
    ],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: &[],
    call: (Method::Put, "/api/cell/stc/params"),
    body: cell::stc_body,
    after: After::Saved("params"),
    again: None,
    once: false,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// iOS `MobileNetworkView`: Connectivity, Connection Mode, Data
    /// Roaming, Network Selection, Apply; in airplane mode only Airplane
    /// Mode can be switched and the Apply form is disabled.
    #[test]
    fn mobile_network_order_and_airplane_mode() {
        use crate::auth::{Kind, PeerKey};
        use crate::handlers::AppState;
        use crate::web::render::Views;
        use crate::web::screen::PageData;
        use serde_json::Value;
        let app = AppState::new();
        app.auth.set_password("pw");
        let issued = app.auth.login("pw", Kind::Browser, PeerKey::UNKNOWN, &[]).unwrap();
        let p = app.auth.authenticate(Kind::Browser, issued.token.expose()).unwrap();
        let views = Views::new(crate::boottime::BootInstant::ZERO, vec![]);
        let page = |mode: &str| {
            let reads = [
                Ok(json!({"enable": 1, "roam_enable": 0, "connect_mode": 1, "connect_status": "disconnected"})),
                Ok(json!({"operate_mode": mode})),
                Ok(Value::Null),
            ];
            mobile::page(&PageData { app: &app, p: &p, views: &views, reads: &reads, remote: false, form: None, query: None, once: None, at: None, ussd: None, notice: None, token: Default::default(), prefs: Default::default(), secure: false }).into_string()
        };
        let s = page("ONLINE");
        let order = [
            "<h2>Connectivity</h2>", "Mobile Data", "<h2>Connection Mode</h2>", "Data Roaming", "Enabling roaming",
            "<h2>Network Selection</h2>", "Scan Networks", ">Apply</button>",
        ];
        let at: Vec<usize> = order.iter().map(|w| s.find(w).unwrap_or_else(|| panic!("{w} in {s}"))).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{order:?} at {at:?}");
        assert!(!s.contains("aria-disabled") && s.contains(&format!("href=\"{}/confirm?enable=0\"", MOBILE_DATA.path)));
        assert!(s.contains("<fieldset class=\"mnf\">") && s.contains("value=\"auto_select\" checked"));
        let s = page("LPM");
        // Mobile Data shown, not switchable; Airplane Mode is; Apply is off.
        assert_eq!(s.matches("aria-disabled=\"true\"").count(), 1, "{s}");
        assert!(s.contains("<span class=\"sw\" role=\"switch\" aria-checked=\"true\" aria-disabled=\"true\" aria-label=\"Mobile Data\">"), "{s}");
        assert!(!s.contains(MOBILE_DATA.path) && s.contains(AIRPLANE.path), "{s}");
        assert!(s.contains("<fieldset class=\"mnf\" disabled>"), "{s}");
    }
}
