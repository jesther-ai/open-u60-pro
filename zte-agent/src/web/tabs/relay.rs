//! Web-only Wi-Fi relay tab; the companion apps deliberately stay unchanged.

use serde_json::json;

use crate::server::Method;
use crate::web::action::{Action, After, Confirm, Need};
use crate::web::kit::Tab;
use crate::web::pages::network;
use crate::web::screen::{Block, Read, Screen};

pub static RELAY: Screen = Screen {
    path: network::RELAY,
    id: "relay",
    title: "Wi-Fi Relay",
    tab: Tab::Relay,
    reads: &[Read::Api("/api/wifi-relay/status")],
    blocks: &[Block::Custom(network::relay_page)],
    ..Screen::BLANK
};

pub static SCAN: Action = Action {
    path: network::SCAN,
    screen: &RELAY,
    fields: &[],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: &[],
    call: (Method::Post, "/api/wifi-relay/scan"),
    body: |_, _| Some(json!({})),
    after: After::Flash(|_, _| (Some(true), "Wi-Fi scan complete.".into())),
    again: None,
    once: true,
};

pub static DISCONNECT: Action = Action {
    path: network::DISCONNECT,
    screen: &RELAY,
    fields: &[],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: &[],
    call: (Method::Post, "/api/wifi-relay/disconnect"),
    body: |_, _| Some(json!({})),
    after: After::Flash(|_, _| (Some(true), "Relay disconnected. Previous hotspot settings restored; cellular data is used only if already enabled.".into())),
    again: None,
    once: true,
};

// Installation uses the generic action confirmation table on the setup page.
pub static INSTALL_CONFIRM: &[Confirm] = &[Confirm {
    when: ("install", "1"),
    need: Need::Always,
    title: "Install Mihomo and Zashboard?",
    lines: &[
        "Downloads about 22 MB from official GitHub releases. Cellular data may be used. The proxy remains off until you save a configuration and start it.",
    ],
    remote: None,
    label: "Download & install",
}];
