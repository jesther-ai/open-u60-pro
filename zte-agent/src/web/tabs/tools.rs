//! Tools tab: the app's tool list (`ToolsListView`), in its sections and
//! order, each row with the Android app's subtitle (the iOS rows have
//! none), and its screens: Automations (`pages::automations`), SMS
//! Forwarding (the SMS tab's screens, mounted here too: `tabs::sms`),
//! Speed Test (`pages::speedtest`), Enable ADB (`pages::tools`), USB Mode
//! (`pages::usb`), Band Lock (`pages::bandlock`), AT Terminal
//! (`pages::at`), Device Info and Connected Devices (`pages::tools`), the
//! app's placeholders for the shell tools, and notes for the two tools that
//! stay in the app (LAN Speed Test, Config Decrypt/Encrypt). The questions
//! of Speed Test, Enable ADB and Band Lock, which hand out single-use
//! tokens, are `pages::confirm`.

use crate::events::Topic;
use crate::server::Method;
use crate::web::action::{Action, After, Field, Kind};
use crate::web::kit::{Icon, Tab};
use crate::web::pages::{at, automations, bandlock, confirm, speedtest, tools, usb};
use crate::web::screen::{Bar, Block, Item, Read, Screen, Search};

const fn to(href: &'static str, label: &'static str, icon: Icon, sub: &'static str) -> Item {
    Item { label, icon, href, sub }
}

const SHELL: &str = "Requires ADB USB connection";

pub static TOOLS: Screen = Screen {
    path: "/ui/tools",
    id: "tools",
    title: "Tools",
    tab: Tab::Tools,
    blocks: &[
        Block::Links(
            "Automation",
            &[
                to(automations::LIST, "Automations", Icon::ClockArrow, "Schedule automated tasks"),
                to("/ui/tools/sms-forward", "SMS Forwarding", Icon::EnvelopeBranch, "Auto-forward SMS to Telegram, webhooks, etc."),
            ],
        ),
        Block::Links(
            "Network Tools",
            &[
                to(speedtest::PATH, "Speed Test", Icon::Gauge, "Test WAN throughput"),
                to("/ui/tools/lan-speedtest", "LAN Speed Test", Icon::Wifi, "Test WiFi link to router"),
                to(confirm::ADB, "Enable ADB", Icon::Cable, "Enable USB debug mode"),
                to(usb::MODE, "USB Mode", Icon::UsbC, "USB mode and powerbank control"),
                to(bandlock::PATH, "Band Lock", Icon::Lock, "Lock NR/LTE bands"),
                to(at::PATH, "AT Terminal", Icon::Terminal, "Send raw AT commands to modem"),
                to(tools::DEVICE_INFO, "Device Info", Icon::Info, "SIM, IMEI, WAN IPs"),
                to(tools::DEVICES, "Connected Devices", Icon::Laptop, "View connected clients and DHCP leases"),
            ],
        ),
        Block::Links("Config", &[to("/ui/tools/config", "Config Decrypt/Encrypt", Icon::Doc, "Offline ZXHN config file tool")]),
        Block::Links(
            "Shell Access Required",
            &[
                to("/ui/tools/ttl", "TTL Settings", Icon::Number, SHELL),
                to("/ui/tools/ssh", "Enable SSH", Icon::Terminal, SHELL),
                to("/ui/tools/explorer", "Device Explorer", Icon::Folder, SHELL),
            ],
        ),
    ],
    ..Screen::BLANK
};

const BACK: Bar = Bar::Back("/ui/tools", "Tools");
const PUSHED: Screen = Screen { tab: Tab::Tools, bar: BACK, ..Screen::BLANK };

/// The app's USB Mode, read at page load (as the app's, which reads it once).
pub static USB_MODE: Screen = Screen {
    path: usb::MODE,
    id: "usbmode",
    title: "USB Mode",
    topics: &[Topic::Usb, Topic::Charger],
    blocks: &[Block::Custom(usb::mode_page)],
    ..PUSHED
};

/// "Fast Charging (Powerbank)" on or off (the app asks nothing); the body
/// is built from the checked value.
pub static USB_POWERBANK: Action = Action {
    path: usb::MODE_POWERBANK,
    screen: &USB_MODE,
    fields: &[Field { name: "state", kind: Kind::Choice(&["0", "1"]) }],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: &[],
    call: (Method::Put, "/api/usb/powerbank"),
    body: usb::mode_body,
    after: After::Flash(usb::switched),
    again: None,
    once: false,
};

// --- Speed Test, Enable ADB, Band Lock, AT Terminal ---

/// Live from `speedtest` (re-read only when the test changes) from the
/// page's watch point; the server list is read at page load (slow lane: it
/// may come from speedtest.net). Its one message line is drawn by the page.
pub static SPEEDTEST: Screen = Screen {
    path: speedtest::PATH,
    id: "speedtest",
    title: "Speed Test",
    topics: &[Topic::Speedtest],
    reads: &[Read::Api(speedtest::SERVERS)],
    live: &speedtest::FRAGMENTS,
    blocks: &[Block::Custom(speedtest::page)],
    arg: Some(speedtest::since),
    refresh: Some(speedtest::refresh),
    own_notice: true,
    ..PUSHED
};

/// "Stop Test" (the app asks nothing), back at the chosen server (0: none).
pub static SPEEDTEST_STOP: Action = Action {
    path: speedtest::STOP,
    screen: &SPEEDTEST,
    fields: &[Field { name: "server", kind: Kind::Int(0, u32::MAX as i64) }],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: &[],
    call: (Method::Post, "/api/speedtest/stop"),
    body: speedtest::stop_body,
    after: After::FlashAt(speedtest::stop_at, speedtest::stopped),
    again: None,
    once: false,
};

/// Static; its result line is `?r=<id>` (`pages::confirm`), drawn above
/// the button as the app's.
pub static ADB: Screen = Screen {
    path: confirm::ADB,
    id: "adb",
    title: "Enable ADB",
    blocks: &[Block::Custom(tools::adb)],
    own_notice: true,
    ..PUSHED
};

/// Nothing is read, as in the app.
pub static BAND_LOCK: Screen = Screen {
    path: bandlock::PATH,
    id: "bandlock",
    title: "Band Lock",
    blocks: &[Block::Custom(bandlock::page)],
    ..PUSHED
};

/// The port is read at page load; the history is the session's.
pub static AT: Screen = Screen {
    path: at::PATH,
    id: "at",
    title: "AT Terminal",
    reads: &[Read::Api("/api/at/port")],
    blocks: &[Block::Custom(at::page)],
    ..PUSHED
};

/// "Clear": nothing is sent, the session's history goes.
pub static AT_CLEAR: Action = Action {
    path: at::CLEAR,
    screen: &AT,
    fields: &[],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: &[],
    call: (Method::Post, at::CLEAR),
    body: |_, _| None,
    after: After::Session(at::clear),
    again: None,
    once: false,
};

// --- Device Info, Connected Devices ---

/// Read at page load (the app reads it as the screen opens), never live.
pub static DEVICE_INFO: Screen = Screen {
    path: tools::DEVICE_INFO,
    id: "deviceinfo",
    title: "Device Info",
    topics: &[Topic::Wan, Topic::Signal],
    reads: &[Read::Fn(tools::info_read)],
    blocks: &[Block::Custom(tools::device_info)],
    ..PUSHED
};

/// Live from `clients`; a search is a snapshot.
pub static DEVICES: Screen = Screen {
    path: tools::DEVICES,
    id: "devices",
    title: "Connected Devices",
    topics: &[Topic::Clients],
    live: &tools::FRAGMENTS,
    blocks: &[Block::Custom(tools::devices)],
    search: Search::Still,
    ..PUSHED
};

// --- The app's placeholders and the tools that stay in the app ---

pub static LAN_SPEED: Screen = Screen {
    path: "/ui/tools/lan-speedtest",
    id: "lanspeed",
    title: "LAN Speed Test",
    blocks: &[Block::Empty(
        Icon::Wifi,
        "LAN Speed Test",
        "Measures WiFi link speed between this device and the router. Does not use internet data.",
        "LAN Speed Test is in the OpenU60 app.",
    )],
    ..PUSHED
};

/// The app's title for it is "Config Tool".
pub static CONFIG: Screen = Screen {
    path: "/ui/tools/config",
    id: "configtool",
    title: "Config Tool",
    blocks: &[Block::Empty(Icon::Doc, "Config Tool", "Offline ZXHN config file tool.", "Config Decrypt/Encrypt is in the OpenU60 app.")],
    ..PUSHED
};

pub static TTL: Screen = Screen {
    path: "/ui/tools/ttl",
    id: "ttl",
    title: "TTL Settings",
    blocks: &[Block::Empty(Icon::Number, "TTL Settings", "Set TTL override via iptables. Requires shell access.", "")],
    ..PUSHED
};

pub static SSH: Screen = Screen {
    path: "/ui/tools/ssh",
    id: "ssh",
    title: "Enable SSH",
    blocks: &[Block::Empty(Icon::Terminal, "Enable SSH", "Install and start dropbear SSH server. Requires ADB USB connection.", "")],
    ..PUSHED
};

pub static EXPLORER: Screen = Screen {
    path: "/ui/tools/explorer",
    id: "explorer",
    title: "Device Explorer",
    blocks: &[Block::Empty(Icon::Folder, "Device Explorer", "Browse filesystem and collect device info. Requires ADB USB connection.", "")],
    ..PUSHED
};

// --- Automations ---

const JOBS: &[Read] = &[Read::Api(automations::JOBS)];

pub static AUTOMATIONS: Screen = Screen {
    path: automations::LIST,
    id: "automations",
    title: "Automations",
    reads: JOBS,
    saved: &automations::SAVED,
    saved_ok: true,
    blocks: &[Block::Custom(automations::list)],
    ..PUSHED
};

/// The delete question reads the job's name, so it is a screen; its form
/// posts to the action of the same path.
pub static AUTO_DELETE: Screen = Screen {
    path: automations::DELETE,
    id: "autodelete",
    title: "Confirm",
    bar: Bar::Back(automations::LIST, "Automations"),
    reads: JOBS,
    blocks: &[Block::Custom(automations::ask_delete)],
    status: Some(automations::ask_status),
    ..PUSHED
};

const JOB_ID: Field = Field { name: "id", kind: Kind::Id(u32::MAX) };
/// The job's creation time: with the id, the job the page showed (the
/// agent reuses a deleted newest job's id).
const JOB_CREATED: Field = Field { name: "c", kind: Kind::Int(0, i64::MAX) };

/// A job's switch (`{"id","enabled"}`, as the app), for the job the list
/// showed (the jobs read first).
const TOGGLE_JOB: Action = Action {
    path: automations::TOGGLE,
    screen: &AUTOMATIONS,
    fields: &[JOB_ID, JOB_CREATED, Field { name: "enabled", kind: Kind::Choice(&["0", "1"]) }],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: JOBS,
    call: (Method::Put, "/api/scheduler/jobs/toggle"),
    body: automations::toggle_body,
    after: After::Back(automations::at_job),
    again: None,
    once: false,
};

pub static AUTO_TOGGLE: Action = TOGGLE_JOB;

/// Delete a job (the app's swipe deletes at once; the web asks first).
pub static AUTO_DELETE_JOB: Action = Action {
    path: automations::DELETE,
    fields: &[JOB_ID, JOB_CREATED],
    confirm_at: Some(|v| format!("{}?id={}", automations::DELETE, v.int("id"))),
    call: (Method::Delete, automations::JOBS),
    body: automations::delete_body,
    after: After::Saved("deleted"),
    ..TOGGLE_JOB
};
