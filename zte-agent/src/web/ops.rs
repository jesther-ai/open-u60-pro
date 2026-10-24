//! Operations a change starts and the progress page follows (mobile data,
//! airplane mode, network mode). The change itself runs once, in the POST;
//! the progress page and its live stream only read the device state, so a
//! reload or reconnect never repeats it.
//!
//! Each kind of operation is one `OpSpec`: its headings, steps, status,
//! the topics it follows, how it reads its state, and where it leads back.
//! Operations are kept in a `SessionStore`: at most 8 (the oldest of the
//! same session goes first), each for 10 minutes on the boot clock.

use std::time::Duration;

use serde_json::Value;

use super::fmt;
use super::kit::Tab;
use super::render::Views;
use super::store::SessionStore;
use crate::boottime::BootInstant;
use crate::events::Topic;

const CAP: usize = 8;
const TTL: Duration = Duration::from_secs(10 * 60);

/// One kind of operation.
pub struct OpSpec {
    /// The page heading while pending, when done, when timed out, when the
    /// device reports a failure.
    pub titles: [&'static str; 4],
    pub steps: &'static [&'static str],
    /// What the progress page reads and streams.
    pub topics: &'static [Topic],
    /// Where it stands after this long, given the operation's target (the
    /// function knows its timeout).
    pub status: fn(&str, Duration, &Views) -> OpStatus,
    /// The page it was started from: (path, title).
    pub back: (&'static str, &'static str),
    pub tab: Tab,
    /// A timeout is not a failure: the change was sent and may still
    /// happen (the app's "Mode sent" is not an error).
    pub timeout_ok: bool,
    /// Its own heading and progress card (the eSIM switch: the app's
    /// connection card); else the steps and the outcome.
    pub view: Option<OpView>,
}

/// An operation page drawn its own way: the heading and `#op-progress`,
/// from the views alone (they are its live fragments).
#[derive(Copy, Clone)]
pub struct OpView {
    pub title: fn(&Views) -> &'static str,
    pub progress: fn(&Views) -> maud::Markup,
}

/// A running operation (its id and owner are the store's).
#[derive(Copy, Clone)]
pub struct Op {
    pub spec: &'static OpSpec,
    pub started: BootInstant,
    /// What it sets, from a fixed list (a network mode), or "".
    pub arg: &'static str,
}

impl PartialEq for Op {
    fn eq(&self, other: &Op) -> bool {
        std::ptr::eq(self.spec, other.spec) && self.started == other.started && self.arg == other.arg
    }
}

impl std::fmt::Debug for Op {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Op({:?} at {:?})", self.spec.titles[0], self.started)
    }
}

impl Op {
    /// Where it stands at `v.now`.
    pub fn status(&self, v: &Views) -> OpStatus {
        (self.spec.status)(self.arg, v.now.saturating_duration_since(self.started), v)
    }

    /// The heading: what runs, then what happened.
    pub fn title(&self, st: OpStatus) -> &'static str {
        self.spec.titles[match st {
            OpStatus::Pending { .. } => 0,
            OpStatus::Done { .. } => 1,
            OpStatus::TimedOut { .. } => 2,
            OpStatus::Failed { .. } => 3,
        }]
    }
}

pub type OpTable = SessionStore<Op>;

impl Default for OpTable {
    fn default() -> Self {
        SessionStore::new(CAP, TTL)
    }
}

const MODEM: &[Topic] = &[Topic::Modem];
const MOBILE_NETWORK: (&str, &str) = ("/ui/router/mobile-network", "Mobile Network");
const UNCONFIRMED: &str = "Change not confirmed";

const fn modem_op(titles: [&'static str; 2], steps: &'static [&'static str], status: fn(&str, Duration, &Views) -> OpStatus) -> OpSpec {
    OpSpec {
        titles: [titles[0], titles[1], UNCONFIRMED, UNCONFIRMED],
        steps,
        topics: MODEM,
        status,
        back: MOBILE_NETWORK,
        tab: Tab::Router,
        timeout_ok: false,
        view: None,
    }
}

pub static DATA_ON: OpSpec = modem_op(
    ["Turning mobile data on", "Mobile data turned on"],
    &["Turning on mobile data\u{2026}", "Connecting\u{2026}"],
    |_, e, v| op_status(OpKind::MobileData { on: true }, e, v.value(Topic::Modem)),
);
pub static DATA_OFF: OpSpec = modem_op(
    ["Turning mobile data off", "Mobile data turned off"],
    &["Disconnecting\u{2026}"],
    |_, e, v| op_status(OpKind::MobileData { on: false }, e, v.value(Topic::Modem)),
);
pub static AIRPLANE_ON: OpSpec = modem_op(
    ["Turning airplane mode on", "Airplane mode turned on"],
    &["Turning the radio off\u{2026}"],
    |_, e, v| op_status(OpKind::Airplane { on: true }, e, v.value(Topic::Modem)),
);
pub static AIRPLANE_OFF: OpSpec = modem_op(
    ["Turning airplane mode off", "Airplane mode turned off"],
    &["Radio on, registering\u{2026}", "Connecting\u{2026}"],
    |_, e, v| op_status(OpKind::Airplane { on: false }, e, v.value(Topic::Modem)),
);

/// A network mode change (`PUT /api/modem/network-mode`), followed on the
/// `signal` topic until `net_select` is the new mode, for 10 s (the app
/// re-reads 5 times, 2 s apart). One spec per mount: it leads back to the
/// Network Mode page it was started from.
const fn net_mode_op(back: &'static str, tab: Tab) -> OpSpec {
    OpSpec {
        titles: ["Changing network mode", "Network mode updated", "Mode sent \u{2014} router may still be switching", "Mode sent \u{2014} router may still be switching"],
        steps: &["Switching network mode\u{2026}"],
        topics: &[Topic::Signal],
        status: net_mode_status,
        back: (back, "Network Mode"),
        tab,
        timeout_ok: true,
        view: None,
    }
}

pub static NET_MODE_SIGNAL: OpSpec = net_mode_op("/ui/signal/network-mode", Tab::Dashboard);
pub static NET_MODE_ROUTER: OpSpec = net_mode_op("/ui/router/network-mode", Tab::Router);

const NET_MODE_TIMEOUT: Duration = Duration::from_secs(10);

/// Done once the modem reports `target` as its `net_select`.
pub fn net_mode_status(target: &str, elapsed: Duration, v: &Views) -> OpStatus {
    let cur = v.value(Topic::Signal).and_then(|s| s["net_select"].as_str());
    match cur {
        Some(c) if c == target => OpStatus::Done { msg: "The modem reports the new mode." },
        _ if elapsed < NET_MODE_TIMEOUT => OpStatus::Pending { step: 0 },
        None => OpStatus::TimedOut { msg: "Couldn't read the modem state." },
        Some(_) => OpStatus::TimedOut { msg: "The modem has not reported the new mode yet. Check again in a moment." },
    }
}

/// Apply on Mobile Network (connection mode, data roaming): followed on
/// `modem` until `data.connect_mode` and `data.roam_enable` are the new
/// values, for 10 s (the app re-reads 5 times, 2 s apart). The target is
/// one of `APPLY_TARGETS`: "<connect mode>:<roaming>", "-" for a value
/// Apply left as it was.
pub static APPLY: OpSpec = OpSpec {
    titles: ["Applying settings", "Settings applied", "Settings sent \u{2014} router may still be applying", "Settings sent \u{2014} router may still be applying"],
    steps: &["Applying settings\u{2026}"],
    topics: MODEM,
    status: apply_status,
    back: MOBILE_NETWORK,
    tab: Tab::Router,
    timeout_ok: true,
    view: None,
};

pub const APPLY_TARGETS: [&str; 9] = ["0:0", "0:1", "1:0", "1:1", "0:-", "1:-", "-:0", "-:1", "-:-"];

const APPLY_TIMEOUT: Duration = Duration::from_secs(10);

pub fn apply_status(target: &str, elapsed: Duration, v: &Views) -> OpStatus {
    let data = v.value(Topic::Modem).map(|m| &m["data"]).filter(|d| d.is_object());
    let (cm, roam) = target.split_once(':').unwrap_or(("-", "-"));
    let shows = data.is_some_and(|d| {
        (cm == "-" || fmt::int(&d["connect_mode"]) == cm.parse().ok())
            && (roam == "-" || fmt::int(&d["roam_enable"]).map(|r| r != 0) == Some(roam == "1"))
    });
    match (data, shows) {
        (Some(_), true) => OpStatus::Done { msg: "The modem reports the new settings." },
        _ if elapsed < APPLY_TIMEOUT => OpStatus::Pending { step: 0 },
        (None, _) => OpStatus::TimedOut { msg: "Couldn't read the modem state." },
        _ => OpStatus::TimedOut { msg: "The modem has not reported the new settings yet. Check again in a moment." },
    }
}

/// A manual network scan (`POST /api/modem/scan`), followed on `netscan`
/// until the modem reports it done, for 60 s (the app polls 30 times, 2 s
/// apart). Its way back shows the networks found (`?scan=1`).
pub static NETSCAN: OpSpec = OpSpec {
    titles: ["Scanning networks", "Scan complete", "Scan timed out", "Scan failed"],
    steps: &["Scanning for networks\u{2026}"],
    topics: &[Topic::Netscan],
    status: |_, e, v| scan_status(e, v.value(Topic::Netscan)),
    back: ("/ui/router/mobile-network?scan=1", "Mobile Network"),
    tab: Tab::Router,
    timeout_ok: false,
    view: None,
};

/// A manual registration (`POST /api/modem/register`), followed on
/// `netscan` until the modem reports its result, for 30 s (the app polls
/// 15 times, 2 s apart).
pub static REGISTER: OpSpec = OpSpec {
    titles: ["Registering to network", "Registered to network", "Registration timed out", "Registration failed"],
    steps: &["Registering\u{2026}"],
    topics: &[Topic::Netscan],
    status: |_, e, v| register_status(e, v.value(Topic::Netscan)),
    back: MOBILE_NETWORK,
    tab: Tab::Router,
    timeout_ok: false,
    view: None,
};

/// The app reads its first answer 2 s after the request; an earlier one can
/// still be the previous scan's or registration's.
const FIRST_READ: Duration = Duration::from_secs(2);

/// A firmware value as text (strings and numbers).
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The scan status: the app's "done", "complete" or "2", or the firmware's
/// `manual_selected` (its strings; `manual_selecting` while it scans).
pub fn scan_done(status: &str) -> bool {
    matches!(status, "done" | "complete" | "2" | "manual_selected")
}

pub fn scan_status(elapsed: Duration, netscan: Option<&Value>) -> OpStatus {
    let st = netscan.map(|n| text(&n["scan"]["m_netselect_status"]));
    match st.as_deref() {
        _ if elapsed < FIRST_READ => OpStatus::Pending { step: 0 },
        Some(s) if scan_done(s) => OpStatus::Done { msg: "Choose a network under Network Selection." },
        Some(s) if s.contains("fail") => OpStatus::Failed { msg: "The modem reports that the scan failed." },
        _ if elapsed < Duration::from_secs(60) => OpStatus::Pending { step: 0 },
        None => OpStatus::TimedOut { msg: "Couldn't read the scan status." },
        Some(_) => OpStatus::TimedOut { msg: "The modem has not finished the scan after a minute." },
    }
}

/// The registration result: the app's "success"/"1" and "fail"/"0", and
/// any firmware word containing them.
pub fn register_status(elapsed: Duration, netscan: Option<&Value>) -> OpStatus {
    let r = netscan.map(|n| text(&n["register"]["m_netselect_result"]));
    match r.as_deref() {
        _ if elapsed < FIRST_READ => OpStatus::Pending { step: 0 },
        Some(r) if r == "1" || r.contains("success") => OpStatus::Done { msg: "The modem reports it is registered." },
        Some(r) if r == "0" || r.contains("fail") => OpStatus::Failed { msg: "The modem reports that the registration failed." },
        _ if elapsed < Duration::from_secs(30) => OpStatus::Pending { step: 0 },
        None => OpStatus::TimedOut { msg: "Couldn't read the registration result." },
        Some(_) => OpStatus::TimedOut { msg: "The modem has not reported a result after 30 seconds." },
    }
}

/// What a modem operation changes (`op_status`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OpKind {
    MobileData { on: bool },
    Airplane { on: bool },
}

impl OpKind {
    fn timeout(self) -> Duration {
        Duration::from_secs(match self {
            OpKind::MobileData { on: true } => 45,
            OpKind::MobileData { on: false } => 30,
            OpKind::Airplane { on: true } => 20,
            OpKind::Airplane { on: false } => 60,
        })
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OpStatus {
    /// Index into `OpSpec::steps`.
    Pending { step: usize },
    Done { msg: &'static str },
    TimedOut { msg: &'static str },
    /// The device reports that the change failed (a refused registration).
    Failed { msg: &'static str },
}

impl OpStatus {
    pub fn finished(self) -> bool {
        !matches!(self, OpStatus::Pending { .. })
    }
}

/// Where an operation stands, from the `modem` topic (`status.operate_mode`,
/// `data.enable`, `data.connect_status`) and the time since it started.
pub fn op_status(kind: OpKind, elapsed: Duration, modem: Option<&Value>) -> OpStatus {
    let mode = modem.and_then(|m| m["status"]["operate_mode"].as_str()).unwrap_or("");
    let data = modem.map(|m| &m["data"]).filter(|d| d.is_object());
    let connected = data.is_some_and(|d| {
        matches!(d["connect_status"].as_str(), Some("ipv4_connected" | "ipv6_connected" | "ipv4_ipv6_connected"))
    });
    let enable = data.and_then(|d| fmt::int(&d["enable"]));
    let late = elapsed >= kind.timeout();
    // Nothing known to report at the timeout.
    let unread = match kind {
        OpKind::MobileData { .. } => data.is_none(),
        OpKind::Airplane { .. } => mode.is_empty(),
    };
    match kind {
        _ if late && unread => OpStatus::TimedOut { msg: "Couldn't read the modem state." },
        OpKind::MobileData { on: true } if connected => OpStatus::Done { msg: "Mobile data is on and connected." },
        // The radio is off: nothing connects until airplane mode is off.
        OpKind::MobileData { on: true } if !mode.is_empty() && mode != "ONLINE" => {
            OpStatus::Done { msg: "Mobile data is on. It connects when airplane mode is turned off." }
        }
        OpKind::MobileData { on: true } if late => {
            OpStatus::TimedOut { msg: "Mobile data is on but not connected yet. Check signal and APN." }
        }
        OpKind::MobileData { on: true } => OpStatus::Pending { step: usize::from(enable == Some(1)) },
        OpKind::MobileData { on: false } if data.is_some() && !connected => OpStatus::Done { msg: "Mobile data is off." },
        OpKind::MobileData { on: false } if late => {
            OpStatus::TimedOut { msg: "Mobile data is still connected. Check again in a moment." }
        }
        OpKind::Airplane { on: true } if !mode.is_empty() && mode != "ONLINE" => {
            OpStatus::Done { msg: "Airplane mode is on. The radio is off." }
        }
        OpKind::Airplane { on: true } if late => OpStatus::TimedOut { msg: "The radio has not turned off yet." },
        OpKind::Airplane { on: false } if mode == "ONLINE" && connected => {
            OpStatus::Done { msg: "The radio is on and mobile data is connected." }
        }
        // Mobile data is off: there is no connection to wait for.
        OpKind::Airplane { on: false } if mode == "ONLINE" && enable == Some(0) => {
            OpStatus::Done { msg: "The radio is on. Mobile data is off." }
        }
        OpKind::Airplane { on: false } if late && mode == "ONLINE" => {
            OpStatus::TimedOut { msg: "The radio is on but there's no data connection yet." }
        }
        OpKind::Airplane { on: false } if late => OpStatus::TimedOut { msg: "The radio has not turned on yet." },
        OpKind::Airplane { on: false } => OpStatus::Pending { step: usize::from(mode == "ONLINE") },
        _ => OpStatus::Pending { step: 0 },
    }
}

/// A valid operation id: 16 lowercase hex characters.
pub fn is_op_id(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::SessionId;
    use serde_json::json;

    fn modem(mode: &str, enable: i64, status: &str) -> Value {
        json!({"status": {"operate_mode": mode}, "data": {"enable": enable, "connect_status": status}})
    }

    fn at(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn mobile_data_rows() {
        let on = OpKind::MobileData { on: true };
        assert_eq!(op_status(on, at(1), Some(&modem("ONLINE", 0, "disconnected"))), OpStatus::Pending { step: 0 });
        assert_eq!(op_status(on, at(3), Some(&modem("ONLINE", 1, "connecting"))), OpStatus::Pending { step: 1 });
        assert!(matches!(op_status(on, at(5), Some(&modem("ONLINE", 1, "ipv4_ipv6_connected"))), OpStatus::Done { .. }));
        assert_eq!(op_status(on, at(44), None), OpStatus::Pending { step: 0 });
        // Unread at the timeout: nothing is claimed.
        assert_eq!(op_status(on, at(45), None), OpStatus::TimedOut { msg: "Couldn't read the modem state." });
        // In airplane mode it cannot connect: done at once, and said so.
        assert_eq!(
            op_status(on, at(1), Some(&modem("LPM", 1, "disconnected"))),
            OpStatus::Done { msg: "Mobile data is on. It connects when airplane mode is turned off." }
        );
        assert_eq!(
            op_status(on, at(45), Some(&modem("ONLINE", 1, "disconnected"))),
            OpStatus::TimedOut { msg: "Mobile data is on but not connected yet. Check signal and APN." }
        );
        let off = OpKind::MobileData { on: false };
        assert_eq!(op_status(off, at(1), Some(&modem("ONLINE", 1, "ipv4_connected"))), OpStatus::Pending { step: 0 });
        // "disconnected" is not connected.
        assert!(matches!(op_status(off, at(2), Some(&modem("ONLINE", 0, "disconnected"))), OpStatus::Done { .. }));
        // An unreadable state is not "disconnected".
        assert_eq!(op_status(off, at(2), Some(&json!({"data": null}))), OpStatus::Pending { step: 0 });
        assert!(matches!(op_status(off, at(30), Some(&modem("ONLINE", 0, "ipv6_connected"))), OpStatus::TimedOut { .. }));
        assert_eq!(op_status(off, at(30), Some(&json!({"data": null}))), OpStatus::TimedOut { msg: "Couldn't read the modem state." });
    }

    #[test]
    fn airplane_rows() {
        let on = OpKind::Airplane { on: true };
        assert_eq!(op_status(on, at(1), Some(&modem("ONLINE", 1, "ipv4_connected"))), OpStatus::Pending { step: 0 });
        assert!(matches!(op_status(on, at(2), Some(&modem("LPM", 1, ""))), OpStatus::Done { .. }));
        assert!(matches!(op_status(on, at(20), Some(&modem("ONLINE", 1, ""))), OpStatus::TimedOut { .. }));
        let off = OpKind::Airplane { on: false };
        assert_eq!(op_status(off, at(1), Some(&modem("LPM", 1, ""))), OpStatus::Pending { step: 0 });
        assert_eq!(op_status(off, at(9), Some(&modem("ONLINE", 1, "disconnected"))), OpStatus::Pending { step: 1 });
        assert_eq!(
            op_status(off, at(12), Some(&modem("ONLINE", 1, "ipv4_connected"))),
            OpStatus::Done { msg: "The radio is on and mobile data is connected." }
        );
        assert_eq!(
            op_status(off, at(12), Some(&modem("ONLINE", 0, "disconnected"))),
            OpStatus::Done { msg: "The radio is on. Mobile data is off." }
        );
        assert_eq!(
            op_status(off, at(60), Some(&modem("ONLINE", 1, "disconnected"))),
            OpStatus::TimedOut { msg: "The radio is on but there's no data connection yet." }
        );
        assert_eq!(op_status(off, at(60), Some(&modem("LPM", 1, ""))), OpStatus::TimedOut { msg: "The radio has not turned on yet." });
        assert_eq!(op_status(on, at(20), None), OpStatus::TimedOut { msg: "Couldn't read the modem state." });
        assert!(!OpStatus::Pending { step: 0 }.finished() && OpStatus::Done { msg: "" }.finished());
    }

    #[test]
    fn titles_turn_past_tense_when_done() {
        let (pending, done, late) = (OpStatus::Pending { step: 0 }, OpStatus::Done { msg: "" }, OpStatus::TimedOut { msg: "" });
        let op = |spec| Op { spec, started: BootInstant::from_duration(at(1)), arg: "" };
        let data_on = op(&DATA_ON);
        assert_eq!((data_on.title(pending), data_on.title(done)), ("Turning mobile data on", "Mobile data turned on"));
        let lpm_off = op(&AIRPLANE_OFF);
        assert_eq!((lpm_off.title(pending), lpm_off.title(done)), ("Turning airplane mode off", "Airplane mode turned off"));
        assert_eq!(op(&DATA_OFF).title(late), "Change not confirmed");
        assert_eq!(op(&AIRPLANE_ON).spec.steps, ["Turning the radio off\u{2026}"]);
        // Every modem operation leads back to Mobile Network, under Router.
        for spec in [&DATA_ON, &DATA_OFF, &AIRPLANE_ON, &AIRPLANE_OFF] {
            assert_eq!((spec.back, spec.tab, spec.topics), (("/ui/router/mobile-network", "Mobile Network"), Tab::Router, MODEM));
        }
        // The status reads the modem topic at the view's time.
        let v = Views::new(BootInstant::from_duration(at(21)), vec![]);
        assert_eq!(op(&AIRPLANE_ON).status(&v), OpStatus::TimedOut { msg: "Couldn't read the modem state." });
        assert_eq!(op(&AIRPLANE_OFF).status(&v), OpStatus::Pending { step: 0 });
    }

    #[test]
    fn network_mode_follows_net_select_for_10_s() {
        let signal = |sel: &str| {
            let slot = crate::events::hub::SlotView {
                version: 1,
                at: BootInstant::from_duration(at(1)),
                value_at: BootInstant::from_duration(at(1)),
                value: Some(std::sync::Arc::new(json!({"net_select": sel}))),
                error: None,
            };
            Views::new(BootInstant::from_duration(at(1)), vec![(Topic::Signal, slot)])
        };
        let st = |sel: &str, secs| net_mode_status("Only_LTE", at(secs), &signal(sel));
        assert_eq!(st("WL_AND_5G", 2), OpStatus::Pending { step: 0 });
        assert_eq!(st("Only_LTE", 4), OpStatus::Done { msg: "The modem reports the new mode." });
        assert_eq!(st("Only_LTE", 30), OpStatus::Done { msg: "The modem reports the new mode." });
        assert_eq!(st("WL_AND_5G", 9), OpStatus::Pending { step: 0 });
        assert!(matches!(st("WL_AND_5G", 10), OpStatus::TimedOut { msg } if msg.contains("not reported the new mode")));
        let unread = Views::new(BootInstant::from_duration(at(1)), vec![]);
        assert_eq!(net_mode_status("Only_LTE", at(10), &unread), OpStatus::TimedOut { msg: "Couldn't read the modem state." });
        // The heading follows the app's messages; each mount leads back to its own page.
        let op = Op { spec: &NET_MODE_ROUTER, started: BootInstant::from_duration(at(1)), arg: "Only_LTE" };
        assert_eq!(op.title(OpStatus::Done { msg: "" }), "Network mode updated");
        assert_eq!(op.title(OpStatus::TimedOut { msg: "" }), "Mode sent \u{2014} router may still be switching");
        assert_eq!((NET_MODE_ROUTER.back, NET_MODE_ROUTER.tab), (("/ui/router/network-mode", "Network Mode"), Tab::Router));
        assert_eq!((NET_MODE_SIGNAL.back, NET_MODE_SIGNAL.tab), (("/ui/signal/network-mode", "Network Mode"), Tab::Dashboard));
        // The status reads the operation's own target.
        let mut v = signal("Only_LTE");
        v.now = BootInstant::from_duration(at(3));
        assert_eq!(op.status(&v), OpStatus::Done { msg: "The modem reports the new mode." });
        assert_eq!(Op { arg: "Only_5G", ..op }.status(&v), OpStatus::Pending { step: 0 });
    }

    #[test]
    fn scan_and_register_follow_the_app_after_its_first_read() {
        let n = |scan: &str, reg: &str| json!({"scan": {"m_netselect_status": scan}, "register": {"m_netselect_result": reg}});
        // The first 2 s: an old "done" is not believed.
        assert_eq!(scan_status(at(1), Some(&n("manual_selected", ""))), OpStatus::Pending { step: 0 });
        for done in ["done", "complete", "2", "manual_selected"] {
            assert!(matches!(scan_status(at(2), Some(&n(done, ""))), OpStatus::Done { .. }), "{done}");
        }
        let num = json!({"scan": {"m_netselect_status": 2}});
        assert!(matches!(scan_status(at(4), Some(&num)), OpStatus::Done { .. }));
        assert_eq!(scan_status(at(59), Some(&n("manual_selecting", ""))), OpStatus::Pending { step: 0 });
        assert_eq!(scan_status(at(60), Some(&n("manual_selecting", ""))), OpStatus::TimedOut { msg: "The modem has not finished the scan after a minute." });
        assert_eq!(scan_status(at(60), None), OpStatus::TimedOut { msg: "Couldn't read the scan status." });
        assert!(matches!(scan_status(at(3), Some(&n("manual_select_fail", ""))), OpStatus::Failed { .. }));
        assert_eq!(register_status(at(1), Some(&n("", "success"))), OpStatus::Pending { step: 0 });
        for ok in ["success", "1", "manual_success"] {
            assert!(matches!(register_status(at(2), Some(&n("", ok))), OpStatus::Done { .. }), "{ok}");
        }
        for bad in ["fail", "0", "manual_fail"] {
            assert_eq!(register_status(at(5), Some(&n("", bad))), OpStatus::Failed { msg: "The modem reports that the registration failed." });
        }
        assert_eq!(register_status(at(29), Some(&n("", ""))), OpStatus::Pending { step: 0 });
        assert_eq!(register_status(at(30), Some(&n("", ""))), OpStatus::TimedOut { msg: "The modem has not reported a result after 30 seconds." });
        assert_eq!(NETSCAN.back.0, "/ui/router/mobile-network?scan=1");
        let op = Op { spec: &REGISTER, started: BootInstant::from_duration(at(1)), arg: "" };
        // The app's words: a timeout is not called a failure.
        let st = [OpStatus::Done { msg: "" }, OpStatus::TimedOut { msg: "" }, OpStatus::Failed { msg: "" }];
        assert_eq!(st.map(|s| op.title(s)), ["Registered to network", "Registration timed out", "Registration failed"]);
        let scan = Op { spec: &NETSCAN, ..op };
        assert_eq!(st.map(|s| scan.title(s)), ["Scan complete", "Scan timed out", "Scan failed"]);
        assert!(OpStatus::Failed { msg: "" }.finished());
    }

    #[test]
    fn apply_follows_connect_mode_and_roaming_for_10_s() {
        let m = |cm: Value, roam: Value| json!({"data": {"connect_mode": cm, "roam_enable": roam}});
        assert!(matches!(apply_status("0:1", at(2), &views_of(&m(json!("0"), json!(1)))), OpStatus::Done { .. }));
        assert_eq!(apply_status("1:1", at(9), &views_of(&m(json!(1), json!(0)))), OpStatus::Pending { step: 0 });
        assert!(matches!(apply_status("1:1", at(10), &views_of(&m(json!(1), json!(0)))), OpStatus::TimedOut { msg } if msg.contains("not reported")));
        let none = Views::new(BootInstant::from_duration(at(1)), vec![]);
        assert_eq!(apply_status("1:0", at(10), &none), OpStatus::TimedOut { msg: "Couldn't read the modem state." });
        assert!(APPLY.timeout_ok && APPLY_TARGETS.contains(&"0:1"));
        // "-": a value Apply left as it was.
        assert!(matches!(apply_status("-:1", at(2), &views_of(&m(json!(0), json!("1")))), OpStatus::Done { .. }));
        assert_eq!(apply_status("1:-", at(2), &views_of(&m(json!(0), json!(1)))), OpStatus::Pending { step: 0 });
    }

    fn views_of(modem: &Value) -> Views {
        let t = BootInstant::from_duration(at(1));
        let slot = crate::events::hub::SlotView { version: 1, at: t, value_at: t, value: Some(std::sync::Arc::new(modem.clone())), error: None };
        Views::new(t, vec![(Topic::Modem, slot)])
    }

    #[test]
    fn table_is_owned_capped_and_expires() {
        let t = OpTable::default();
        let (me, other) = (SessionId::for_test(1), SessionId::for_test(2));
        let start = BootInstant::from_duration(at(1000));
        let id = |n: u64| format!("{n:016x}");
        let op = |n: u64| Op { spec: &AIRPLANE_ON, started: start.saturating_add(at(n)), arg: "" };
        for n in 0..9 {
            t.insert(id(n), me, op(n), start);
        }
        // The oldest of nine went.
        assert_eq!(t.get(&id(0), me, start), None);
        assert_eq!(t.get(&id(8), me, start), Some(op(8)));
        assert_eq!(t.get(&id(8), other, start), None);
        // Another session's operation makes room from this session's own.
        t.insert(id(99), other, op(99), start);
        t.insert(id(9), me, op(9), start);
        assert_eq!(t.get(&id(99), other, start), Some(op(99)));
        assert_eq!(t.get(&id(1), me, start), None);
        assert_eq!(t.get(&id(2), me, start), None);
        assert!(t.get(&id(3), me, start).is_some());
        assert!(t.get(&id(8), me, start.saturating_add(at(599))).is_some());
        assert!(t.get(&id(8), me, start.saturating_add(at(600))).is_none());
        assert!(is_op_id("0123456789abcdef") && !is_op_id("0123456789ABCDEF") && !is_op_id("0123"));
    }
}
