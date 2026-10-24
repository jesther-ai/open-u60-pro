//! Dashboard tab: the dashboard with its USB-C card's actions, the sheets
//! it opens (Connection & signal, Live traffic, Battery Details, CPU &
//! temperatures and, over it, CPU & Memory and Processes with its kills;
//! Wi-Fi & devices and Share Wi-Fi), and Network Mode, pushed from
//! Connection & signal and also mounted under Router. Their cards and
//! panels are drawn in `pages::dashboard`, `pages::usb`, `pages::signal`,
//! `pages::sheets`, `pages::processes` and `pages::wifi`.

use maud::{html, Markup};
use serde_json::{json, Value};

use crate::events::Topic;
use crate::server::Method;
use crate::web::action::{Action, After, Confirm, Field, Kind, Need, Valid};
use crate::web::fmt;
use crate::web::kit::{self, Tab};
use crate::web::ops::{NET_MODE_ROUTER, NET_MODE_SIGNAL};
use crate::web::pages::dashboard::DASH;
use crate::web::pages::{dashboard, processes, sheets, signal, usb, wifi};
use crate::web::screen::{Bar, Block, PageData, Read, Screen};

pub static DASHBOARD: Screen = Screen {
    path: "/ui/",
    id: "dashboard",
    title: "Dashboard",
    tab: Tab::Dashboard,
    bar: Bar::Status(dashboard::age_badge),
    topics: &dashboard::TOPICS,
    reads: &[Read::Fn(dashboard::wifi_read)],
    live: &dashboard::FRAGMENTS,
    blocks: &[Block::Custom(dashboard::block)],
    ..Screen::BLANK
};

/// The USB-C card's "Fast Charging" (the app asks nothing). The one value
/// is the whitelist; the body is built. Sent only while a cable is
/// attached (a page shown before it was unplugged sends nothing).
pub static POWERBANK: Action = Action {
    path: usb::POWERBANK,
    screen: &DASHBOARD,
    fields: &[Field { name: "state", kind: Kind::Choice(&["1"]) }],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: &[Read::Fn(usb::cable_read)],
    call: (Method::Put, "/api/usb/powerbank"),
    body: usb::powerbank_body,
    after: After::Flash(usb::switched),
    again: None,
    once: false,
};

/// "Dismiss": this session's flag only, nothing is sent (`call` unused).
pub static USB_DISMISS: Action = Action {
    path: usb::DISMISS,
    screen: &DASHBOARD,
    fields: &[],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: &[],
    call: (Method::Post, usb::DISMISS),
    body: |_, _| None,
    after: After::Session(usb::dismiss),
    again: None,
    once: false,
};

pub static SIGNAL: Screen = Screen {
    path: "/ui/signal",
    id: "signal",
    title: "Connection & signal",
    tab: Tab::Dashboard,
    bar: Bar::AgeDone(dashboard::read_badge, "/ui/"),
    topics: &signal::TOPICS,
    live: &signal::FRAGMENTS,
    history: true,
    blocks: &[Block::Custom(signal::block)],
    ..Screen::BLANK
};

pub static TRAFFIC: Screen = Screen {
    path: "/ui/traffic",
    id: "traffic",
    title: "Live traffic",
    tab: Tab::Dashboard,
    bar: Bar::Done("/ui/"),
    topics: &[Topic::Traffic],
    live: &sheets::TRAFFIC,
    blocks: &[Block::Custom(sheets::traffic)],
    ..Screen::BLANK
};

pub static BATTERY: Screen = Screen {
    path: "/ui/battery",
    id: "battery",
    title: "Battery Details",
    tab: Tab::Dashboard,
    bar: Bar::Done("/ui/"),
    topics: &[Topic::Battery, Topic::Charger, Topic::BatteryInfo],
    live: &sheets::BATTERY,
    blocks: &[Block::Custom(sheets::battery)],
    ..Screen::BLANK
};

pub static HEALTH: Screen = Screen {
    path: "/ui/health",
    id: "health",
    title: "CPU & temperatures",
    tab: Tab::Dashboard,
    bar: Bar::Done("/ui/"),
    topics: &[Topic::Health, Topic::Battery],
    live: &sheets::HEALTH,
    blocks: &[Block::Custom(sheets::health)],
    ..Screen::BLANK
};

/// A sheet over CPU & temperatures (the app's is swiped away; here "‹").
pub static CPU: Screen = Screen {
    path: "/ui/health/cpu",
    id: "cpu",
    title: "CPU & Memory",
    tab: Tab::Dashboard,
    bar: Bar::Back("/ui/health", "CPU & temperatures"),
    topics: &[Topic::Health],
    // The bloat summary as the app reads it: the API's tracker, once.
    reads: &[Read::Api("/api/device/system"), Read::Api("/api/system/top")],
    live: &sheets::CPU,
    blocks: &[Block::Custom(sheets::cpu)],
    ..Screen::BLANK
};

// --- Wi-Fi & devices ---

pub static WIFI: Screen = Screen {
    path: wifi::PATH,
    id: "wifi",
    title: "Wi-Fi & devices",
    tab: Tab::Dashboard,
    bar: Bar::Done("/ui/"),
    topics: &[Topic::Clients],
    reads: &[Read::Fn(wifi::card_read)],
    live: &wifi::FRAGMENTS,
    blocks: &[Block::Custom(wifi::page)],
    ..Screen::BLANK
};

/// Never live: the password and its QR code are rendered only here.
pub static SHARE: Screen = Screen {
    path: wifi::SHARE,
    id: "wifishare",
    title: "WiFi Credentials",
    tab: Tab::Dashboard,
    bar: Bar::Back(wifi::PATH, "Wi-Fi & devices"),
    reads: &[Read::Fn(wifi::share_read)],
    blocks: &[Block::Custom(wifi::share)],
    ..Screen::BLANK
};

// --- Processes ---

/// A sheet over CPU & Memory: "Done" leads back to it, "Kill All Bloat"
/// trails while there is bloat.
pub static PROCESSES: Screen = Screen {
    path: processes::PATH,
    id: "processes",
    title: "Processes",
    tab: Tab::Dashboard,
    bar: Bar::DoneTrail("/ui/health/cpu", processes::kill_all_link),
    topics: &[Topic::Processes],
    live: &processes::FRAGMENTS,
    blocks: &[Block::Custom(processes::page)],
    ..Screen::BLANK
};

/// The confirmations read the process names, so they are screens; their
/// forms post to the actions of the same path.
const KILL_CONFIRM: Screen = Screen { title: "Confirm", bar: Bar::Back(processes::PATH, "Processes"), ..Screen::BLANK };

pub static KILL_ONE: Screen = Screen {
    path: processes::KILL,
    id: "prkill",
    reads: &[Read::Fn(processes::process_read)],
    blocks: &[Block::Custom(processes::kill_page)],
    ..KILL_CONFIRM
};

pub static KILL_ALL: Screen = Screen {
    path: processes::KILL_ALL,
    id: "prkillall",
    reads: &[Read::Fn(processes::bloat_read)],
    blocks: &[Block::Custom(processes::kill_all_page)],
    ..KILL_CONFIRM
};

const KILL_BLOAT: (Method, &str) = (Method::Post, "/api/system/kill-bloat");

/// One PID and the daemon the question named (the kill waits until the
/// PID still runs it).
pub static KILL: Action = Action {
    path: processes::KILL,
    screen: &PROCESSES,
    fields: &[
        Field { name: "pid", kind: Kind::Id(processes::PID_MAX) },
        Field { name: "name", kind: Kind::Choice(processes::NAMES) },
    ],
    confirm: &[],
    confirm_at: Some(|v| format!("{}?pid={}", processes::KILL, v.int("pid"))),
    unchanged: None,
    reads: &[],
    call: KILL_BLOAT,
    body: processes::kill_body,
    after: After::Flash(processes::killed_one),
    again: None,
    once: false,
};

pub static KILL_ALL_BLOAT: Action = Action {
    path: processes::KILL_ALL,
    screen: &PROCESSES,
    fields: &[],
    confirm: &[],
    confirm_at: Some(|_| processes::KILL_ALL.to_owned()),
    unchanged: None,
    // The PIDs as of now (a daemon may have restarted since the question).
    reads: &[Read::Fn(processes::targets_read)],
    call: KILL_BLOAT,
    body: processes::kill_all_body,
    after: After::Flash(processes::killed_all),
    again: None,
    once: false,
};

// --- Network Mode ---

/// The app's modes (`NetworkModeConfig.netSelectOptions`): value, label.
pub const NET_MODES: [(&str, &str); 6] = [
    ("WL_AND_5G", "Auto"),
    ("LTE_AND_5G", "5G NSA (LTE + NR)"),
    ("Only_5G", "5G SA Only"),
    ("Only_LTE", "4G Only"),
    ("WCDMA_AND_LTE", "4G + 3G"),
    ("Only_WCDMA", "3G Only"),
];
const MODE_VALUES: [&str; 6] = ["WL_AND_5G", "LTE_AND_5G", "Only_5G", "Only_LTE", "WCDMA_AND_LTE", "Only_WCDMA"];

const REREGISTER: &str = "The modem re-registers on the network. Mobile data drops for a few seconds.";

/// Every mode is confirmed (the app has no dialog): it re-registers the
/// modem, and a mode without coverage here can strand a remote browser.
macro_rules! mode_confirm {
    ($value:literal, $label:literal) => {
        Confirm {
            when: ("mode", $value),
            need: Need::Always,
            title: concat!("Change network mode to ", $label, "?"),
            lines: &[REREGISTER],
            remote: Some(&[
                REREGISTER,
                concat!(
                    "You are connected remotely: if ",
                    $label,
                    " has no coverage here, this page can\u{2019}t be reached until the mode is changed on the router."
                ),
            ]),
            label: "Change mode",
        }
    };
}

static MODE_CONFIRMS: [Confirm; 6] = [
    mode_confirm!("WL_AND_5G", "Auto"),
    mode_confirm!("LTE_AND_5G", "5G NSA (LTE + NR)"),
    mode_confirm!("Only_5G", "5G SA Only"),
    mode_confirm!("Only_LTE", "4G Only"),
    mode_confirm!("WCDMA_AND_LTE", "4G + 3G"),
    mode_confirm!("Only_WCDMA", "3G Only"),
];

/// The two mounts differ in path, tab, way back and form target.
const NET_MODE_BASE: Screen = Screen { title: "Network Mode", topics: &[Topic::Signal], ..Screen::BLANK };

pub static NET_MODE: Screen = Screen {
    path: "/ui/signal/network-mode",
    id: "netmode",
    bar: Bar::Back("/ui/signal", "Connection & signal"),
    blocks: &[Block::Custom(|pd| net_mode(pd, &SET_NET_MODE))],
    ..NET_MODE_BASE
};

pub static ROUTER_NET_MODE: Screen = Screen {
    path: "/ui/router/network-mode",
    id: "routernetmode",
    tab: Tab::Router,
    bar: Bar::Back("/ui/router", "Router"),
    blocks: &[Block::Custom(|pd| net_mode(pd, &SET_ROUTER_NET_MODE))],
    ..NET_MODE_BASE
};

const MODE: &[Field] = &[Field { name: "mode", kind: Kind::Choice(&MODE_VALUES) }];
const PUT_MODE: (Method, &str) = (Method::Put, "/api/modem/network-mode");

/// Built here from the checked value: the agent hands the body to ubus as is.
fn mode_body(v: &Valid, _: &[Value]) -> Option<Value> {
    Some(json!({"net_select": v.get("mode")}))
}

/// The checked mode as the operation's target.
fn target(v: &Valid) -> &'static str {
    MODE_VALUES.into_iter().find(|m| *m == v.get("mode")).unwrap_or("")
}

pub static SET_NET_MODE: Action = Action {
    path: "/ui/signal/network-mode",
    screen: &NET_MODE,
    fields: MODE,
    confirm: &MODE_CONFIRMS,
    confirm_at: None,
    unchanged: Some(("mode", "cur")),
    reads: &[],
    call: PUT_MODE,
    body: mode_body,
    after: After::Op(|v| (&NET_MODE_SIGNAL, target(v))),
    again: None,
    once: false,
};

pub static SET_ROUTER_NET_MODE: Action = Action {
    path: "/ui/router/network-mode",
    screen: &ROUTER_NET_MODE,
    fields: MODE,
    confirm: &MODE_CONFIRMS,
    confirm_at: None,
    unchanged: Some(("mode", "cur")),
    reads: &[],
    call: PUT_MODE,
    body: mode_body,
    after: After::Op(|v| (&NET_MODE_ROUTER, target(v))),
    again: None,
    once: false,
};

/// iOS `NetworkModeView`: the current mode (its label, else the raw value,
/// else "—"), the picker and Apply. The form also posts the mode it showed
/// as current: applying that one changes nothing (the app's Apply is
/// disabled then).
fn net_mode(pd: &PageData, a: &Action) -> Markup {
    let slot = pd.views.slot(Topic::Signal);
    let cur = slot.and_then(|s| s.value.as_deref()).and_then(|v| v["net_select"].as_str()).map(str::trim).unwrap_or("");
    let label = NET_MODES.iter().find(|(v, _)| *v == cur).map(|(_, l)| *l);
    let current = label.map_or_else(|| if cur.is_empty() { DASH.into() } else { fmt::truncate(cur, 32) }, str::to_owned);
    html! {
        @if let Some(e) = slot.and_then(|s| s.error.as_deref()) {
            (kit::notice(false, &format!("Failed to load network mode: {}", fmt::truncate(e, 120))))
        }
        (kit::section(Some("Current"), None, kit::kv_row(None, "Network Mode", &current)))
        form method="post" action=(a.path) {
            (pd.csrf())
            input type="hidden" name="cur" value=(if label.is_some() { cur } else { "" });
            (kit::section(Some("Network Mode"), None, html! {
                li.it {
                    label for="mode" { "Mode" }
                    select #mode name="mode" {
                        @for (value, text) in NET_MODES {
                            option value=(value) selected[value == cur] { (text) }
                        }
                    }
                }
            }))
            p.actions { button.btn.btn-wide type="submit" { "Apply" } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_confirmations_and_body_line_up() {
        assert_eq!(MODE_VALUES, NET_MODES.map(|(v, _)| v));
        for ((value, label), c) in NET_MODES.iter().zip(&MODE_CONFIRMS) {
            assert_eq!(c.when, ("mode", *value));
            assert!(matches!(c.need, Need::Always));
            assert_eq!(c.title, format!("Change network mode to {label}?"));
            assert_eq!(c.lines, [REREGISTER]);
            assert!(c.remote.unwrap()[1].contains(&format!("You are connected remotely: if {label} has no coverage here,")));
            // The body carries the checked value only; the operation follows it.
            let v = Valid::of(&[("mode", value)]);
            assert_eq!(mode_body(&v, &[]), Some(json!({"net_select": value})));
            assert_eq!(target(&v), *value);
        }
        // Both mounts post to their own page and share everything else.
        for (a, s) in [(&SET_NET_MODE, &NET_MODE), (&SET_ROUTER_NET_MODE, &ROUTER_NET_MODE)] {
            assert!(std::ptr::eq(a.screen, s) && a.path == s.path && a.unchanged == Some(("mode", "cur")));
            assert_eq!(a.call, (Method::Put, "/api/modem/network-mode"));
        }
    }
}
