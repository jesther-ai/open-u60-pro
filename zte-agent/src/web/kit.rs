//! Render primitives shared by every screen: the tab bar, nav bar items,
//! icons, card headings, inset-grouped lists and their rows, switches,
//! banners and notices. Maud escapes every value; nothing here carries an
//! inline style (CSP), and the icons are inline SVG drawn for this UI.

use maud::{html, Markup};

/// The app's five tabs (iOS `TabBarView`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Sms,
    Tools,
    Router,
    Settings,
}

const TABS: [(Tab, &str, &str, Icon); 5] = [
    (Tab::Dashboard, "/ui/", "Dashboard", Icon::Gauge),
    (Tab::Sms, "/ui/sms", "SMS", Icon::Message),
    (Tab::Tools, "/ui/tools", "Tools", Icon::Tools),
    (Tab::Router, "/ui/router", "Router", Icon::Router),
    (Tab::Settings, "/ui/settings", "Settings", Icon::Gear),
];

/// The tab bar (after `main` in the DOM; CSS floats it at the bottom on
/// phones and puts it under the nav bar on wide screens).
pub fn tab_bar(cur: Tab) -> Markup {
    html! {
        nav.tabs aria-label="Tabs" {
            @for (tab, href, label, i) in TABS {
                a href=(href) aria-current=[(tab == cur).then_some("page")] { (icon(i)) span { (label) } }
            }
        }
    }
}

/// Icons (24×24, stroked with the text colour).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    Gauge,
    Message,
    Tools,
    Router,
    Gear,
    Antenna,
    AntennaSlash,
    Arrows,
    ArrowDown,
    ArrowUp,
    /// The level icon of the app: 100, 75, 50, 25, or 0 for unknown.
    Battery(u8),
    Cpu,
    Wifi,
    Lock,
    /// A lock with "!" (the app's `exclamationmark.lock.fill`, PUK).
    LockWarn,
    /// The app's `lock.open`.
    LockOpen,
    /// The app's `pencil`.
    Pencil,
    Airplane,
    Bars,
    Waveform,
    SimCard,
    Phone,
    People,
    Network,
    Globe,
    Shield,
    EyeSlash,
    LockShield,
    Power,
    Clock,
    /// A clock in a circular arrow (the app's `clock.arrow.circlepath`).
    ClockArrow,
    Envelope,
    /// An envelope with a branching arrow (the app's
    /// `envelope.arrow.triangle.branch`, SMS Forwarding).
    EnvelopeBranch,
    Cable,
    /// A USB-C plug (the app's `cable.connector`).
    UsbC,
    Terminal,
    Info,
    Laptop,
    Doc,
    Number,
    /// Numbered lines (the app's `list.number`).
    ListNumber,
    Folder,
    Bolt,
    BoltCircle,
    BatteryBolt,
    Swap,
    Flame,
    Thermometer,
    Grid,
    Memory,
    Chart,
    Warn,
    /// Square and pencil (the app's `square.and.pencil`, New Message).
    Compose,
    Search,
    /// A circle with "!" (the app's `exclamationmark.circle.fill`).
    Alert,
    Plus,
    /// The app's `paperplane` (Test Destination).
    PaperPlane,
    /// The app's `checkmark.circle.fill` and `xmark.circle.fill` (a
    /// forward's result): a filled disc, the mark cut out.
    Check,
    Cross,
    /// The app's `arrow.clockwise` (refresh).
    Refresh,
    /// The app's `arrow.triangle.2.circlepath` (switch profile).
    Cycle,
    Eye,
    /// The app's `ellipsis` (a row's options).
    Ellipsis,
    /// The app's `qrcode.viewfinder`, `camera.fill`, `photo` and `keyboard`
    /// (Add eSIM).
    QrScan,
    Camera,
    Photo,
    Keyboard,
    /// The app's `trash` (Delete profile).
    Trash,
    /// The app's `circle` (a telemetry domain not blocked yet).
    Circle,
}

const ANTENNA: &str = "M12 12v8M12 10.5v.01M9 8.5a3.5 3.5 0 0 0 0 5M15 8.5a3.5 3.5 0 0 1 0 5\
                       M6 5.5a7.5 7.5 0 0 0 0 11M18 5.5a7.5 7.5 0 0 1 0 11";
const SHIELD: &str = "M12 3l7.5 3v5.5c0 4.5-3.2 8.3-7.5 9.5-4.3-1.2-7.5-5-7.5-9.5V6z";

impl Icon {
    fn path(self) -> &'static str {
        match self {
            Icon::Gauge => "M3 12a9 9 0 1 1 18 0 9 9 0 0 1-18 0M12 12l4-4M6.5 12H5M19 12h-1.5M12 6.5V5",
            Icon::Message => {
                "M12 4c5 0 9 3.1 9 7s-4 7-9 7c-1 0-2-.1-2.9-.4L4.5 20l1-3.7C4 15 3 13.1 3 11c0-3.9 4-7 9-7z"
            }
            Icon::Tools => {
                "M14.5 3.5a4 4 0 0 0-4.6 5.2L3.6 15a1.9 1.9 0 0 0 2.7 2.7l6.3-6.3a4 4 0 0 0 5.2-4.6L15.5 9 13 8.5 12.5 6z\
                 M4 4l2-.5 5 5M13.5 14.5l5 5a1.4 1.4 0 0 0 2-2l-5-5"
            }
            Icon::Router => "M3 14h18v6H3zM7 17h.01M10 17h.01M13 17h.01M9 10.5a4.5 4.5 0 0 1 6 0M6.5 8a8 8 0 0 1 11 0",
            Icon::Gear => {
                "M10.2 4.6 10.6 2.1 13.4 2.1 13.8 4.6 15.9 5.5 18 4 20 6 18.5 8.1 19.4 10.2 21.9 10.6 21.9 13.4 \
                 19.4 13.8 18.5 15.9 20 18 18 20 15.9 18.5 13.8 19.4 13.4 21.9 10.6 21.9 10.2 19.4 8.1 18.5 6 20 \
                 4 18 5.5 15.9 4.6 13.8 2.1 13.4 2.1 10.6 4.6 10.2 5.5 8.1 4 6 6 4 8.1 5.5zM15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0z"
            }
            Icon::Antenna => ANTENNA,
            Icon::AntennaSlash => "M12 12v8M9 8.5a3.5 3.5 0 0 0 0 5M15 8.5a3.5 3.5 0 0 1 0 5M6 5.5a7.5 7.5 0 0 0 0 11\
                                   M18 5.5a7.5 7.5 0 0 1 0 11M3 3l18 18",
            Icon::Arrows => "M8 20V4M4 8l4-4 4 4M16 4v16M12 16l4 4 4-4",
            Icon::ArrowDown => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7.5v9M8 12.5l4 4 4-4",
            Icon::ArrowUp => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 16.5v-9M8 11.5l4-4 4 4",
            Icon::Battery(_) => "M4.5 7h12A1.5 1.5 0 0 1 18 8.5v7a1.5 1.5 0 0 1-1.5 1.5h-12A1.5 1.5 0 0 1 3 15.5v-7A1.5 1.5 0 0 1 4.5 7zM20.5 10.5v3",
            Icon::Cpu => "M7 7h10v10H7zM10 10h4v4h-4zM9.5 3v4M14.5 3v4M9.5 17v4M14.5 17v4M3 9.5h4M3 14.5h4M17 9.5h4M17 14.5h4",
            Icon::Wifi => "M2.5 9a14 14 0 0 1 19 0M5.5 12.5a9.5 9.5 0 0 1 13 0M8.5 16a5 5 0 0 1 7 0M12 19.5h.01",
            Icon::Lock => "M6 11h12v10H6zM8.5 11V8a3.5 3.5 0 0 1 7 0v3M12 15v2",
            Icon::LockWarn => "M6 11h12v10H6zM8.5 11V8a3.5 3.5 0 0 1 7 0v3M12 13.5v3M12 18.5v.01",
            Icon::LockOpen => "M6 11h12v10H6zM8.5 11V8a3.5 3.5 0 0 1 6.9-1.1M12 15v2",
            Icon::Pencil => "M4 20l1.2-4.5L16 4.7a2 2 0 0 1 2.8 0l.5.5a2 2 0 0 1 0 2.8L8.5 18.8zM14.5 6.2l3.3 3.3",
            Icon::Airplane => {
                "M12 2.5c.8 0 1.5.7 1.5 1.5v5.5l7.5 4.5v2l-7.5-2.5v5l2 1.5v1.5l-3.5-1-3.5 1V20l2-1.5v-5L3 16v-2\
                 l7.5-4.5V4c0-.8.7-1.5 1.5-1.5z"
            }
            Icon::Bars => "M4 20v-3M9 20v-6M14 20v-9M19 20V5",
            Icon::Waveform => "M2 12h2.5l2-5 3 10 3-8 1.5 3M17 13a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM19.2 18.2 22 21",
            Icon::SimCard => "M7 3h7l4 4v14H7zM10 12h5v5h-5z",
            Icon::Phone => {
                "M5 4h3.5L10 8 8 9.5a11 11 0 0 0 6.5 6.5L16 14l4 1.5V19a1.5 1.5 0 0 1-1.5 1.5C10.5 20.5 3.5 13.5 3.5 5.5A1.5 1.5 0 0 1 5 4z"
            }
            Icon::People => "M9 11a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7zM3 20a6 6 0 0 1 12 0M16 4.5a3.2 3.2 0 0 1 0 6.2M18 14.5c2 .7 3 2.8 3 5.5",
            Icon::Network => "M10 3h4v4h-4zM3 17h4v4H3zM17 17h4v4h-4zM12 7v5M5 17v-5h14v5",
            Icon::Globe => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM3 12h18M12 3c2.4 2.5 3.5 5.5 3.5 9s-1.1 6.5-3.5 9c-2.4-2.5-3.5-5.5-3.5-9s1.1-6.5 3.5-9z",
            Icon::Shield => SHIELD,
            Icon::EyeSlash => "M2 12s3.5-6.5 10-6.5S22 12 22 12s-3.5 6.5-10 6.5S2 12 2 12zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM4 4l16 16",
            Icon::LockShield => "M12 3l7.5 3v5.5c0 4.5-3.2 8.3-7.5 9.5-4.3-1.2-7.5-5-7.5-9.5V6zM9.5 12h5v4h-5zM10.5 12v-1.5a1.5 1.5 0 0 1 3 0V12",
            Icon::Power => "M12 3v9M6.3 6.3a8 8 0 1 0 11.4 0",
            Icon::Clock => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3 2",
            Icon::ClockArrow => "M4.5 12a7.5 7.5 0 1 0 2.2-5.3L4.5 9M4.5 4.5V9H9M12 8v4l3 2",
            Icon::Envelope => "M3 6h18v12H3zM3 7l9 6 9-6",
            Icon::EnvelopeBranch => "M3 5h13v9H3zM3 6l6.5 4.5L16 6M12 21v-1.5a3 3 0 0 1 3-3h5M18 14.5l2 2-2 2",
            Icon::Cable => "M3 12h4M7 8h7v8H7zM14 10h4M14 14h4",
            Icon::UsbC => "M10.5 2.5h3v3h-3zM9 5.5h6v9a1.5 1.5 0 0 1-1.5 1.5h-3A1.5 1.5 0 0 1 9 14.5zM12 16v6",
            Icon::Terminal => "M3 5h18v14H3zM7 9l3 3-3 3M12 15h5",
            Icon::Info => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 11v6M12 7.5v.01",
            Icon::Laptop => "M4 6h11v8H4zM2 17h14M18 8h4v12h-4z",
            Icon::Doc => "M6 3h8l4 4v14H6zM14 3v4h4M9 13h6M9 17h6",
            Icon::Number => "M10 3 8 21M16 3l-2 18M4 8.5h17M3 15.5h17",
            Icon::ListNumber => "M10 6h10M10 12h10M10 18h10M4 4.5 5.5 3.5v5M4 10.5a1.5 1.5 0 0 1 3 0c0 1-3 2.5-3 3.5h3M4 15.5h3l-1.5 1.7a1.4 1.4 0 1 1-1.4 1.9",
            Icon::Folder => "M3 6h6l2 2h10v11H3z",
            Icon::Bolt => "M13.5 2.5 5 13.5h6l-1 8 8.5-11h-6z",
            Icon::BoltCircle => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM13 6.5l-4.5 6.3h3.3l-.8 4.7 4.5-6.3h-3.3z",
            Icon::BatteryBolt => {
                "M4.5 7h12A1.5 1.5 0 0 1 18 8.5v7a1.5 1.5 0 0 1-1.5 1.5h-12A1.5 1.5 0 0 1 3 15.5v-7A1.5 1.5 0 0 1 4.5 7z\
                 M20.5 10.5v3M11.5 8.5l-3 4h3l-1 3 3-4h-3z"
            }
            Icon::Swap => "M4 8h15M15 4l4 4-4 4M20 16H5M9 12l-4 4 4 4",
            Icon::Flame => "M12 3c1 3.5 5.5 5.5 5.5 10.5a5.5 5.5 0 0 1-11 0c0-2.5 1.5-4 2.5-5 .3 1.7 1.2 2.7 2.2 3C11.7 9.3 11.5 6 12 3z",
            Icon::Thermometer => "M10 4.5a2 2 0 0 1 4 0v9a4 4 0 1 1-4 0zM12 10v6.5M16.5 5h2M16.5 8h2M16.5 11h2",
            Icon::Grid => "M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z",
            Icon::Memory => "M6 5h12v14H6zM9 2.5V5M12 2.5V5M15 2.5V5M9 19v2.5M12 19v2.5M15 19v2.5M9 9h6v6H9z",
            Icon::Chart => "M4 20h16M6.5 17v-5M11 17V7M15.5 17v-8",
            Icon::Warn => "M12 3.5 2.5 20h19zM12 10v4.5M12 17.5v.01",
            Icon::Compose => "M11 4H5.5A1.5 1.5 0 0 0 4 5.5v13A1.5 1.5 0 0 0 5.5 20h13a1.5 1.5 0 0 0 1.5-1.5V13M18 3l3 3-9 9-4 1 1-4z",
            Icon::Search => "M10.5 4a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13zM15.5 15.5 20.5 20.5",
            Icon::Alert => "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7.5v6M12 16.5v.01",
            Icon::Plus => "M12 5v14M5 12h14",
            Icon::PaperPlane => "M21 3 3 10.5l7 2.5 2.5 7zM21 3 10 13",
            Icon::Check => "M8 12.5l2.7 2.7L16 9.8",
            Icon::Cross => "M9 9l6 6M15 9l-6 6",
            Icon::Refresh => "M19.5 12a7.5 7.5 0 1 1-2.6-5.7M17.5 2.5v4h-4",
            Icon::Cycle => "M4.5 12a7.5 7.5 0 0 1 13-5.1M19.5 12a7.5 7.5 0 0 1-13 5.1M17.5 3v4h-4M6.5 21v-4h4",
            Icon::Eye => "M2 12s3.5-6.5 10-6.5S22 12 22 12s-3.5 6.5-10 6.5S2 12 2 12zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z",
            Icon::Ellipsis => "M6 12h.01M12 12h.01M18 12h.01",
            Icon::QrScan => "M3.5 8V3.5H8M16 3.5h4.5V8M20.5 16v4.5H16M8 20.5H3.5V16M7.5 7.5h3.5v3.5H7.5zM13 7.5h3.5v3.5H13zM7.5 13h3.5v3.5H7.5zM13.5 13.5h.01M16 16h.01M13.5 16h.01M16 13.5h.01",
            Icon::Camera => "M3.5 8h3.5l1.5-2.5h7L17 8h3.5v11.5h-17zM12 10.5a3.25 3.25 0 1 0 0 6.5 3.25 3.25 0 0 0 0-6.5z",
            Icon::Photo => "M3.5 5h17v14h-17zM3.5 16l5-5 4 4 2.5-2.5 5.5 5M15.5 8.5h.01",
            Icon::Keyboard => "M2.5 6h19v12h-19zM6 9.5h.01M9 9.5h.01M12 9.5h.01M15 9.5h.01M18 9.5h.01M7.5 12.5h.01M10.5 12.5h.01M13.5 12.5h.01M16.5 12.5h.01M8 15.5h8",
            Icon::Trash => "M4.5 6.5h15M9.5 6.5V4h5v2.5M6.5 6.5l1 13.5h9l1-13.5M10 10v6.5M14 10v6.5",
            Icon::Circle => "M12 2.5a9.5 9.5 0 1 0 0 19 9.5 9.5 0 0 0 0-19z",
        }
    }
}

/// An icon, hidden from assistive technology (the text next to it says it).
pub fn icon(i: Icon) -> Markup {
    // The battery's fill: 11 units wide when full.
    let fill = match i {
        Icon::Battery(100) => Some("11"),
        Icon::Battery(75) => Some("8.2"),
        Icon::Battery(50) => Some("5.5"),
        Icon::Battery(25) => Some("2.8"),
        _ => None,
    };
    html! {
        svg.i viewBox="0 0 24 24" aria-hidden="true" {
            @if matches!(i, Icon::Check | Icon::Cross) { circle.f cx="12" cy="12" r="9.5" {} }
            path d=(i.path()) {}
            @if let Some(w) = fill { rect.f x="5" y="9" width=(w) height="6" rx="1" {} }
        }
    }
}

/// The app's `cellularbars` glyph for `bars` of 5 (four bars, as many
/// filled as the level reaches), named for screen readers.
pub fn signal_bars(bars: i64) -> Markup {
    let n = bars.clamp(0, 5);
    let lit = (n * 4 + 4) / 5;
    let label = format!("Signal {n} of 5");
    html! {
        svg.i.bars viewBox="0 0 24 24" role="img" aria-label=(label) {
            @for (i, (x, y, h)) in [("3", "15", "5"), ("8", "11.5", "8.5"), ("13", "8", "12"), ("18", "4.5", "15.5")].into_iter().enumerate() {
                rect.on[(i as i64) < lit] x=(x) y=(y) width="3.5" height=(h) rx="1" {}
            }
        }
    }
}

/// The battery icon for a level (iOS `batteryIcon`): 75 % and up full,
/// then three steps, none for an unknown level.
pub fn battery_icon(capacity: Option<i64>) -> Icon {
    Icon::Battery(match capacity {
        Some(c) if c >= 75 => 100,
        Some(c) if c >= 50 => 75,
        Some(c) if c >= 25 => 50,
        Some(_) => 25,
        None => 0,
    })
}

/// "‹ <parent title>": the nav bar's way back from a pushed page.
pub fn back(href: &str, label: &str) -> Markup {
    html! { a.nl.back href=(href) { (label) } }
}

/// A nav bar icon button (gear, compose): a link named by `label`.
pub fn icon_link(href: &str, i: Icon, label: &str) -> Markup {
    html! { a.nl.ic href=(href) aria-label=(label) { (icon(i)) } }
}

/// "Done": the nav bar's way back from a sheet.
pub fn done(href: &str) -> Markup {
    html! { a.nl href=(href) { "Done" } }
}

/// A card with an icon heading. With `href` the whole card is a link to
/// its detail page (the chevron is drawn by CSS).
pub fn card(id: Option<&str>, href: Option<&str>, i: Icon, title: &str, body: Markup) -> Markup {
    let head = html! { h2.ch { (icon(i)) span { (title) } } };
    html! {
        @match href {
            Some(h) => a.card id=[id] href=(h) { (head) (body) },
            None => section.card id=[id] { (head) (body) },
        }
    }
}

/// An inset-grouped section: optional header, the rows, optional footer.
pub fn section(title: Option<&str>, footer: Option<Markup>, rows: Markup) -> Markup {
    html! {
        section.group {
            @if let Some(t) = title { h2 { (t) } }
            ul.list { (rows) }
            @if let Some(f) = footer { (f) }
        }
    }
}

/// A row that opens another page (chevron by CSS).
pub fn link_row(href: &str, i: Option<Icon>, label: &str) -> Markup {
    html! { li { a.it href=(href) { @if let Some(i) = i { (icon(i)) } span { (label) } } } }
}

/// A label (with its icon) and a value.
pub fn kv_row(i: Option<Icon>, label: &str, value: &str) -> Markup {
    kv_row_m(i, label, html! { (value) })
}

/// `kv_row` with markup as its value (a rolling number, `roll`).
pub fn kv_row_m(i: Option<Icon>, label: &str, value: Markup) -> Markup {
    html! { li.it { @if let Some(i) = i { (icon(i)) } span { (label) } span.v { (value) } } }
}

/// What a control does: a POST of one hidden field, a link to a
/// confirmation page first, or nothing (disabled, as the app's control
/// in airplane mode).
pub enum Act<'a> {
    Post { action: &'a str, name: &'a str, value: &'a str },
    Link(&'a str),
    Off,
}

fn act(csrf: &Markup, a: &Act, class: &str, checked: Option<bool>, aria: &str, text: &str) -> Markup {
    let checked = checked.map(|c| if c { "true" } else { "false" });
    let role = checked.map(|_| "switch");
    match a {
        Act::Post { action, name, value } => html! {
            form.sf method="post" action=(action) {
                (csrf)
                input type="hidden" name=(name) value=(value);
                button class=(class) type="submit" role=[role] aria-checked=[checked] aria-label=(aria) { (text) }
            }
        },
        Act::Link(href) => html! {
            a class=(class) href=(href) role=[role] aria-checked=[checked] aria-label=(aria) { (text) }
        },
        Act::Off => html! {
            span class=(class) role=[role] aria-checked=[checked] aria-disabled="true" aria-label=(aria) { (text) }
        },
    }
}

/// A row with an on/off switch (`role=switch`) that does the opposite of
/// the current state. An unknown state offers both directions as buttons
/// ("On", "Off", read out as "Turn on <label>").
pub fn switch_row(csrf: &Markup, label: &str, state: Option<bool>, on: Act, off: Act) -> Markup {
    html! {
        li.it {
            span { (label) }
            @match state {
                Some(true) => (act(csrf, &off, "sw", Some(true), label, "")),
                Some(false) => (act(csrf, &on, "sw", Some(false), label, "")),
                None => {
                    span.v { "Unavailable" }
                    (act(csrf, &on, "btn sm", None, &format!("Turn on {label}"), "On"))
                    (act(csrf, &off, "btn sm", None, &format!("Turn off {label}"), "Off"))
                }
            }
        }
    }
}

/// A big centred icon, title and text (an empty list, a placeholder).
pub fn empty_state(i: Icon, title: &str, text: &str) -> Markup {
    html! { div.empty { (icon(i)) strong { (title) } p { (text) } } }
}

/// A banner: tinted by `class` (`b-red`, `b-orange`, `b-blue`).
pub fn banner(class: &str, i: Icon, title: &str, text: Markup) -> Markup {
    html! { div class=(format!("banner {class}")) { (icon(i)) div { strong { (title) } span { (text) } } } }
}

/// A result line above a page's content.
pub fn notice(ok: bool, text: &str) -> Markup {
    html! {
        @if ok { p.msg.info role="status" { (text) } } @else { p.msg.err role="alert" { (text) } }
    }
}
