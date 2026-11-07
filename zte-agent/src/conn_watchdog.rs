//! Connectivity watchdog (plan A5): "connected but no internet" recovery.
//!
//! The Live Activity recovery thread already probes the internet over TLS;
//! its result is the only signal here (no new timer while healthy). When the
//! WAN reports connected but the probe keeps failing, the watchdog climbs a
//! ladder of increasingly heavy remedies, each with its own cooldown:
//!
//! | rung | action | cooldown | cap |
//! |---|---|---|---|
//! | 1 | PDN reconnect (`zwrt_data set_wwaniface` enable 0 → 1) | 10 min | – |
//! | 2 | LPM → ONLINE (`nwinfo_set_mode LPM`, then `AT+CFUN=1`) | 20 min | – |
//! | 3 | modem reset `AT+CFUN=1,1` | 1 h | 3/day |
//! | 4 | reboot with fast boot off (opt-in `allow_reboot`) | 24 h | 1/day |
//!
//! The ladder only runs against a connected data session: a WAN drop that
//! outlives a rung's recovery window resets it, and the next session needs
//! a fresh trigger. Every rung taken is persisted (unix time) so cooldowns
//! and daily caps survive an agent restart or the rung-4 reboot.
//!
//! Layout: [`Machine`] is a pure state machine (`step(event, now) ->
//! Vec<Action>`), [`ConnWatchdog`] the runtime: one thread blocking on an
//! mpsc receiver (`recv_timeout` only while a deadline is pending) that
//! executes actions through the [`Effects`] trait. [`DeviceEffects`] is the
//! on-device implementation; tests use a mock. The effects never go through
//! the HTTP handlers (rung 2 sends `AT+CFUN=1` itself), so a handler
//! instrumented to report user activity cannot feed the watchdog's own
//! action back to it; the runtime also ignores
//! [`ConnWatchdog::set_user_activity`] calls made from its own thread.
//!
//! Off by default. Device effects are also safety-blocked until vendor user
//! intent can be distinguished from the watchdog's own temporary PDN-off.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

pub const CONFIG_PATH: &str = "/data/local/tmp/conn_watchdog.json";
/// Persisted watchdog state: the rungs taken in the last 24 h (cooldowns and
/// daily caps across restarts) and whether fast boot must be restored after
/// a rung-4 reboot.
pub const STATE_PATH: &str = "/data/local/tmp/conn_watchdog_state.json";

/// Rung 1 fires after this many consecutive probe failures...
pub const TRIGGER_FAILURES: u32 = 3;
/// ...spanning at least this long.
pub const TRIGGER_MIN_AGE: Duration = Duration::from_secs(10 * 60);
/// A rung must restore the probe within this long, else the next rung runs.
pub const RECOVERY_WINDOW: Duration = Duration::from_secs(5 * 60);
/// Probe failures this soon after an action are expected (PDN re-attach) and ignored.
pub const SETTLE: Duration = Duration::from_secs(60);
/// No action within this long after a user-initiated modem change.
pub const USER_ACTIVITY_GUARD: Duration = Duration::from_secs(120);
/// Retry delay when a call, USSD session or speed test blocks an action.
pub const GUARD_RETRY: Duration = Duration::from_secs(60);
const DAY: Duration = Duration::from_secs(24 * 3600);
const EXTERNAL_INTENT_UNAVAILABLE: &str =
    "Automatic modem recovery is unavailable: firmware does not expose authoritative external cellular-off intent";
/// Gap between LPM and ONLINE inside rung 2 (the firmware uses 5 s).
const LPM_ONLINE_GAP: Duration = Duration::from_secs(5);
/// `AT+CFUN=1` attempts inside rung 2: the AT port has no cross-thread lock
/// and can answer with nothing under contention, which must not leave the
/// modem parked in LPM.
const ONLINE_ATTEMPTS: u32 = 3;
/// After an unconfirmed `AT+CFUN=1`, how long `operate_mode` is polled for ONLINE.
const ONLINE_VERIFY_WAIT: Duration = Duration::from_secs(10);
const ONLINE_VERIFY_POLL: Duration = Duration::from_secs(1);
const UCI_OPERATE_MODE_KEY: &str = "zte_nwinfo.sys_info.operate_mode";
/// Attempts to restore fast boot after a watchdog reboot (ubus may not be up yet).
const FAST_BOOT_RESTORE_ATTEMPTS: u32 = 5;
const FAST_BOOT_RESTORE_DELAY: Duration = Duration::from_secs(5);
/// While the restore is still pending after those attempts, the watchdog
/// thread retries on every wakeup and at least this often.
const FAST_BOOT_RETRY_INTERVAL: Duration = Duration::from_secs(60);
/// Logged and returned by `config_set` when the watchdog is enabled on a
/// unit where nothing will ever feed it a probe result.
const NO_PROBE_WARNING: &str =
    "enabled but no probe source is attached (Live Activity provider + agent password required)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rung {
    PdnReconnect,
    LpmOnline,
    ModemReset,
    Reboot,
}

impl Rung {
    pub const ALL: [Rung; 4] = [
        Rung::PdnReconnect,
        Rung::LpmOnline,
        Rung::ModemReset,
        Rung::Reboot,
    ];

    pub fn number(self) -> u8 {
        match self {
            Rung::PdnReconnect => 1,
            Rung::LpmOnline => 2,
            Rung::ModemReset => 3,
            Rung::Reboot => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Rung::PdnReconnect => "PDN reconnect",
            Rung::LpmOnline => "LPM -> ONLINE",
            Rung::ModemReset => "modem reset (AT+CFUN=1,1)",
            Rung::Reboot => "reboot with fast boot off",
        }
    }

    pub fn cooldown(self) -> Duration {
        match self {
            Rung::PdnReconnect => Duration::from_secs(10 * 60),
            Rung::LpmOnline => Duration::from_secs(20 * 60),
            Rung::ModemReset => Duration::from_secs(3600),
            Rung::Reboot => DAY,
        }
    }

    pub fn daily_cap(self) -> Option<u32> {
        match self {
            Rung::PdnReconnect | Rung::LpmOnline => None,
            Rung::ModemReset => Some(3),
            Rung::Reboot => Some(1),
        }
    }

    pub fn next(self, allow_reboot: bool) -> Option<Rung> {
        match self {
            Rung::PdnReconnect => Some(Rung::LpmOnline),
            Rung::LpmOnline => Some(Rung::ModemReset),
            Rung::ModemReset if allow_reboot => Some(Rung::Reboot),
            Rung::ModemReset | Rung::Reboot => None,
        }
    }
}

// ── Config ──────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub allow_reboot: bool,
}

impl Config {
    /// Apply a partial JSON update (`{enabled, allow_reboot}`); unknown
    /// fields are ignored.
    pub fn patched(&self, patch: &Value) -> Result<Config, String> {
        let mut next = self.clone();
        if let Some(v) = patch.get("enabled") {
            next.enabled = v.as_bool().ok_or("enabled must be a boolean")?;
        }
        if let Some(v) = patch.get("allow_reboot") {
            next.allow_reboot = v.as_bool().ok_or("allow_reboot must be a boolean")?;
        }
        Ok(next)
    }

    fn load(path: &Path) -> Config {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Config>(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) {
        if let Ok(json) = serde_json::to_string(self) {
            let _ = fs::write(path, json);
        }
    }
}

// ── Inputs ──────────────────────────────────────────────────────────

/// `true` for the `connect_status`/`wan_status` values the stock scripts
/// treat as a cellular data session (`ipv4_connected`, `ipv6_connected`,
/// `ipv4_ipv6_connected`, `connected`).
pub fn wan_connected_from_status(status: &str) -> bool {
    status == "connected" || status.ends_with("_connected")
}

/// WAN state from a `router_event_wan_connect_status` payload.
pub fn wan_from_event(payload: &Value) -> Option<bool> {
    payload["wan_status"]
        .as_str()
        .map(wan_connected_from_status)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// WAN connect status: `true` = connected.
    Wan(bool),
    /// Result of one internet probe (the Live Activity recovery probe).
    Probe(bool),
    /// The pending deadline was reached.
    Timer,
    /// Configuration changed.
    Config(Config),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Act(Rung),
    Log(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Healthy, disabled, or counting failures; no deadline.
    Idle,
    /// `rung` ran at `acted_at`; waiting for the probe to recover.
    /// `fails` counts post-settle failures since then.
    Waiting {
        rung: Rung,
        acted_at: Instant,
        fails: u32,
    },
    /// `rung` is due but blocked (guard, cooldown or daily cap) until `until`.
    Cooling { rung: Rung, until: Instant },
}

// ── State machine ───────────────────────────────────────────────────

pub struct Machine {
    config: Config,
    state: State,
    deadline: Option<Instant>,
    wan_connected: Option<bool>,
    /// Consecutive probe failures while WAN is connected (rung 1 trigger).
    failures: u32,
    first_failure: Option<Instant>,
    /// Every action taken, for cooldowns and daily caps.
    history: Vec<(Rung, Instant)>,
    user_activity: Option<Instant>,
    /// Consulted only when a rung is about to run (see [`BusyCheck`]).
    busy: BusyCheck,
}

impl Machine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            state: State::Idle,
            deadline: None,
            wan_connected: None,
            failures: 0,
            first_failure: None,
            history: Vec::new(),
            user_activity: None,
            busy: Box::new(|| None),
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> &State {
        &self.state
    }

    /// The instant the runtime must deliver [`Event::Timer`] at, if any.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    #[cfg(test)]
    pub fn wan_connected(&self) -> Option<bool> {
        self.wan_connected
    }

    /// Install the guard that reports an active call, USSD session or speed
    /// test. It is called only when a rung is about to run, never on probe
    /// results that do not trigger an action.
    pub fn set_busy_check(&mut self, busy: BusyCheck) {
        self.busy = busy;
    }

    /// Last user-initiated modem change; actions wait [`USER_ACTIVITY_GUARD`].
    pub fn set_user_activity(&mut self, at: Option<Instant>) {
        self.user_activity = at;
    }

    /// Record an action taken outside this machine's lifetime (before the
    /// agent restarted) so cooldowns and caps account for it.
    pub fn record_action(&mut self, rung: Rung, at: Instant) {
        self.history.push((rung, at));
    }

    pub fn step(&mut self, event: Event, now: Instant) -> Vec<Action> {
        match event {
            Event::Wan(connected) => self.on_wan(connected, now),
            Event::Probe(ok) => self.on_probe(ok, now),
            Event::Timer => self.on_timer(now),
            Event::Config(config) => self.on_config(config),
        }
    }

    pub fn status(&self, now: Instant) -> Value {
        let (state, rung) = match self.state {
            State::Idle => ("idle", None),
            State::Waiting { rung, .. } => ("waiting", Some(rung)),
            State::Cooling { rung, .. } => ("cooling", Some(rung)),
        };
        let mut actions_24h = serde_json::Map::new();
        for r in Rung::ALL {
            let n = self
                .history
                .iter()
                .filter(|(h, t)| *h == r && now.saturating_duration_since(*t) < DAY)
                .count();
            actions_24h.insert(json!(r).as_str().unwrap_or("?").to_string(), json!(n));
        }
        let last_action = self.history.last().map(
            |(r, t)| json!({"rung": r, "secs_ago": now.saturating_duration_since(*t).as_secs()}),
        );
        json!({
            "state": state,
            "rung": rung,
            "failures": self.failures,
            "wan_connected": self.wan_connected,
            "next_deadline_secs": self.deadline.map(|d| d.saturating_duration_since(now).as_secs()),
            "actions_24h": actions_24h,
            "last_action": last_action,
        })
    }

    fn on_wan(&mut self, connected: bool, now: Instant) -> Vec<Action> {
        let prev = self.wan_connected.replace(connected);
        if connected {
            // A reconnect after a rung's recovery window is a new session
            // (the rung's own bounce comes back within the window); the old
            // ladder is stale and the new session needs a fresh trigger.
            if let State::Waiting { rung, acted_at, .. } = self.state {
                if prev != Some(true) && now >= acted_at + RECOVERY_WINDOW {
                    return self.reset(format!(
                        "WAN reconnected {} min after rung {}; ladder reset",
                        now.saturating_duration_since(acted_at).as_secs() / 60,
                        rung.number()
                    ));
                }
            }
            return Vec::new();
        }
        if prev == Some(false) {
            return Vec::new();
        }
        // A WAN drop ends the data session we were counting against. While a
        // rung is being evaluated the drop is part of the remedy; keep going
        // until the window ends.
        self.failures = 0;
        self.first_failure = None;
        match self.state {
            State::Idle | State::Waiting { .. } => Vec::new(),
            State::Cooling { rung, .. } => self.reset(format!(
                "WAN disconnected while waiting to run rung {}; ladder reset",
                rung.number()
            )),
        }
    }

    /// Back to [`State::Idle`] with a fresh trigger count.
    fn reset(&mut self, why: String) -> Vec<Action> {
        self.state = State::Idle;
        self.deadline = None;
        self.failures = 0;
        self.first_failure = None;
        vec![Action::Log(why)]
    }

    fn on_probe(&mut self, ok: bool, now: Instant) -> Vec<Action> {
        if ok {
            self.failures = 0;
            self.first_failure = None;
            let message = match self.state {
                State::Idle => return Vec::new(),
                State::Waiting { rung, acted_at, .. } => format!(
                    "probe ok {} s after rung {} ({}); connectivity restored",
                    now.saturating_duration_since(acted_at).as_secs(),
                    rung.number(),
                    rung.label()
                ),
                State::Cooling { rung, .. } => format!(
                    "probe ok while waiting to run rung {}; ladder reset",
                    rung.number()
                ),
            };
            self.state = State::Idle;
            self.deadline = None;
            return vec![Action::Log(message)];
        }
        if !self.config.enabled || self.wan_connected != Some(true) {
            return Vec::new();
        }
        match self.state {
            State::Idle => {
                self.failures += 1;
                let first = *self.first_failure.get_or_insert(now);
                if self.failures >= TRIGGER_FAILURES
                    && now.saturating_duration_since(first) >= TRIGGER_MIN_AGE
                {
                    let mut actions = vec![Action::Log(format!(
                        "WAN connected but {} probes failed over {} min; starting recovery ladder",
                        self.failures,
                        now.saturating_duration_since(first).as_secs() / 60
                    ))];
                    actions.extend(self.try_act(Rung::PdnReconnect, now));
                    return actions;
                }
                Vec::new()
            }
            State::Waiting {
                rung,
                acted_at,
                fails,
            } => {
                if now < acted_at + SETTLE {
                    return Vec::new();
                }
                let fails = fails + 1;
                self.state = State::Waiting {
                    rung,
                    acted_at,
                    fails,
                };
                if now >= acted_at + RECOVERY_WINDOW {
                    return self.escalate(rung, now);
                }
                Vec::new()
            }
            State::Cooling { .. } => Vec::new(),
        }
    }

    fn on_timer(&mut self, now: Instant) -> Vec<Action> {
        match self.deadline {
            Some(d) if now >= d => {}
            _ => return Vec::new(), // early or spurious wakeup
        }
        self.deadline = None;
        match self.state {
            State::Idle => Vec::new(),
            State::Waiting { rung, fails, .. } => {
                if self.wan_connected != Some(true) {
                    // The session the ladder was run against is gone; do not
                    // escalate against whatever comes up next.
                    self.reset(format!(
                        "WAN down at the end of rung {} window; ladder reset",
                        rung.number()
                    ))
                } else if fails > 0 {
                    self.escalate(rung, now)
                } else {
                    // No probe result since the action settled: the probe
                    // loop is backing off. The next failure escalates.
                    vec![Action::Log(format!(
                        "no probe result since rung {}; waiting for the next probe",
                        rung.number()
                    ))]
                }
            }
            State::Cooling { rung, .. } => self.try_act(rung, now),
        }
    }

    fn on_config(&mut self, config: Config) -> Vec<Action> {
        let was = std::mem::replace(&mut self.config, config);
        if self.config.enabled {
            return if was.enabled {
                Vec::new()
            } else {
                vec![Action::Log("watchdog enabled".into())]
            };
        }
        self.failures = 0;
        self.first_failure = None;
        self.state = State::Idle;
        self.deadline = None;
        if was.enabled {
            vec![Action::Log("watchdog disabled".into())]
        } else {
            Vec::new()
        }
    }

    /// `rung` did not restore the probe: move to the next one, or give up.
    fn escalate(&mut self, rung: Rung, now: Instant) -> Vec<Action> {
        let mut actions = vec![Action::Log(format!(
            "rung {} ({}) did not restore connectivity",
            rung.number(),
            rung.label()
        ))];
        match rung.next(self.config.allow_reboot) {
            Some(next) => actions.extend(self.try_act(next, now)),
            None => {
                self.state = State::Idle;
                self.deadline = None;
                self.failures = 0;
                self.first_failure = None;
                actions.push(Action::Log(
                    "ladder exhausted; waiting for a fresh trigger".into(),
                ));
            }
        }
        actions
    }

    /// Run `rung` now, or wait for the guard/cooldown/cap that blocks it.
    fn try_act(&mut self, rung: Rung, now: Instant) -> Vec<Action> {
        if let Some((until, reason)) = self.blocked_until(rung, now) {
            self.state = State::Cooling { rung, until };
            self.deadline = Some(until);
            return vec![Action::Log(format!(
                "rung {} ({}) deferred {} s: {reason}",
                rung.number(),
                rung.label(),
                until.saturating_duration_since(now).as_secs()
            ))];
        }
        self.history.push((rung, now));
        self.state = State::Waiting {
            rung,
            acted_at: now,
            fails: 0,
        };
        self.deadline = Some(now + RECOVERY_WINDOW);
        vec![Action::Act(rung)]
    }

    fn blocked_until(&self, rung: Rung, now: Instant) -> Option<(Instant, String)> {
        let mut block: Option<(Instant, String)> = None;
        let mut consider = |at: Instant, reason: String| {
            if at > now && block.as_ref().is_none_or(|(b, _)| at > *b) {
                block = Some((at, reason));
            }
        };
        if let Some(reason) = (self.busy)() {
            consider(now + GUARD_RETRY, format!("{reason} in progress"));
        }
        if let Some(at) = self.user_activity {
            consider(at + USER_ACTIVITY_GUARD, "recent user modem change".into());
        }
        let same: Vec<Instant> = self
            .history
            .iter()
            .filter(|(r, _)| *r == rung)
            .map(|(_, t)| *t)
            .collect();
        if let Some(last) = same.iter().max() {
            consider(
                *last + rung.cooldown(),
                format!("rung {} cooldown", rung.number()),
            );
        }
        if let Some(cap) = rung.daily_cap() {
            let mut today: Vec<Instant> = same
                .into_iter()
                .filter(|t| now.saturating_duration_since(*t) < DAY)
                .collect();
            if today.len() as u32 >= cap {
                today.sort();
                let oldest = today[today.len() - cap as usize];
                consider(
                    oldest + DAY,
                    format!("rung {} daily cap ({cap}/day)", rung.number()),
                );
            }
        }
        block
    }
}

// ── Side effects ────────────────────────────────────────────────────

pub trait Effects: Send {
    /// Internet probes describe the selected upstream, not necessarily SIM.
    fn recovery_blocked(&self) -> Option<&'static str> {
        None
    }
    /// `ubus call zwrt_data set_wwaniface {"cid":1,"enable":0}` then `enable:1`.
    fn pdn_reconnect(&self) -> Result<(), String>;
    /// `nwinfo_set_mode LPM`, then `AT+CFUN=1` sent directly through
    /// `crate::at_cmd::send` (the same command `handlers::modem_online`
    /// uses, but not through the handler: the wiring may count handler
    /// calls as user activity).
    fn lpm_online(&self) -> Result<(), String>;
    /// `AT+CFUN=1,1` via `crate::at_cmd::send`.
    fn modem_reset(&self) -> Result<(), String>;
    /// `quicken_power_on` via the fast-boot handler.
    fn set_fast_boot(&self, on: bool) -> Result<(), String>;
    /// `ubus call system reboot`.
    fn reboot(&self) -> Result<(), String>;
    /// Current WAN state, read once at thread start (the event only fires on
    /// change). `None` waits for the first event.
    fn read_wan_connected(&self) -> Option<bool> {
        None
    }
    /// Push a one-line status to the Live Activity if one is registered.
    fn notify(&self, _message: &str) {}
}

/// Returns `Some(reason)` while a call, USSD session or speed test is active.
/// Called only when a rung is about to run (a few times per incident), so it
/// may afford a quick `AT+CLCC` when a call flag is set; it is never called
/// on ordinary probe results or while idle.
pub type BusyCheck = Box<dyn Fn() -> Option<&'static str> + Send>;

pub struct DeviceEffects {
    app: Arc<AppState>,
}

impl DeviceEffects {
    pub fn new(app: Arc<AppState>) -> Self {
        Self { app }
    }

    fn cellular_enabled(&self) -> Result<bool, String> {
        ubus::call("zwrt_data", "get_wwaniface", Some(r#"{"cid":1}"#))
            .map(|v| v["enable"].as_u64() == Some(1) || v["enable"].as_str() == Some("1"))
    }

    fn network_guard(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        // B31 reports the same enable=0 for our PDN pause and an off request
        // from the official panel/screen. Do not guess permission to re-enable.
        automatic_recovery_allowed()?;
        let guard = self
            .app
            .network_lock
            .try_lock()
            .map_err(|_| "Network transition in progress")?;
        if self.app.wifi_relay.owns_network() {
            return Err("Wi-Fi relay owns the upstream; cellular recovery refused".into());
        }
        Ok(guard)
    }

    fn with_cellular_recovery(
        &self,
        action: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let _network = self.network_guard()?;
        let _control = self.app.cellular_control.lock();
        if !self.cellular_enabled()? {
            return Err("Cellular data is disabled; automatic recovery refused".into());
        }
        action()
    }
}

fn handler_result((status, body): (u16, Value), what: &str) -> Result<(), String> {
    if status == 200 {
        Ok(())
    } else {
        Err(format!(
            "{what}: {}",
            body["error"].as_str().unwrap_or("failed")
        ))
    }
}

fn automatic_recovery_allowed() -> Result<(), String> {
    Err(EXTERNAL_INTENT_UNAVAILABLE.into())
}

impl Effects for DeviceEffects {
    fn recovery_blocked(&self) -> Option<&'static str> {
        if automatic_recovery_allowed().is_err() {
            return Some(EXTERNAL_INTENT_UNAVAILABLE);
        }
        let Ok(_network) = self.network_guard() else {
            return Some("Wi-Fi relay or network transition");
        };
        let _control = self.app.cellular_control.lock();
        (!self.cellular_enabled().unwrap_or(false))
            .then_some("cellular data disabled or unavailable")
    }

    fn pdn_reconnect(&self) -> Result<(), String> {
        automatic_recovery_allowed()
    }

    fn lpm_online(&self) -> Result<(), String> {
        self.with_cellular_recovery(|| {
            ubus::call(
                "zte_nwinfo_api",
                "nwinfo_set_mode",
                Some(r#"{"operate_mode":"LPM"}"#),
            )?;
            std::thread::sleep(LPM_ONLINE_GAP);
            // Same command as handlers::modem_online, sent directly: going
            // through the handler would let an instrumented handler report the
            // watchdog's own action as a user modem change.
            bring_online(
                || crate::at_cmd::send(&self.app.at_port, "AT+CFUN=1", 8),
                || ubus::uci_get(UCI_OPERATE_MODE_KEY).ok(),
                || {
                    ubus::call(
                        "zte_nwinfo_api",
                        "nwinfo_set_mode",
                        Some(r#"{"operate_mode":"ONLINE"}"#),
                    )
                    .map(|_| ())
                },
                ONLINE_VERIFY_WAIT,
                ONLINE_VERIFY_POLL,
            )
        })
    }

    fn modem_reset(&self) -> Result<(), String> {
        self.with_cellular_recovery(|| {
            let resp = crate::at_cmd::send(&self.app.at_port, "AT+CFUN=1,1", 8)?;
            if resp.contains("OK") {
                Ok(())
            } else {
                Err(format!("AT+CFUN=1,1 failed: {}", resp.trim()))
            }
        })
    }

    fn set_fast_boot(&self, on: bool) -> Result<(), String> {
        let body: &[u8] = if on {
            br#"{"fast_boot":"1"}"#
        } else {
            br#"{"fast_boot":"0"}"#
        };
        handler_result(
            crate::device_ext::device_fast_boot_set(&self.app, body),
            "fast boot",
        )
    }

    fn reboot(&self) -> Result<(), String> {
        self.with_cellular_recovery(|| {
            handler_result(crate::device_ext::device_reboot(&self.app), "reboot")
        })
    }

    fn read_wan_connected(&self) -> Option<bool> {
        ubus::call(
            "zwrt_data",
            "get_wwaniface",
            Some(r#"{"source_module":"zte_topsw_data","cid":1}"#),
        )
        .ok()
        .and_then(|v| v["connect_status"].as_str().map(wan_connected_from_status))
    }
}

/// The ONLINE half of rung 2, with the modem already parked in LPM. Sends
/// `AT+CFUN=1` (`send_cfun`) up to [`ONLINE_ATTEMPTS`] times; an attempt
/// counts when the reply contains `OK` or `read_mode` (`operate_mode`)
/// reports ONLINE within `verify_wait`, polled every `verify_poll`. After
/// the last failed attempt `fallback` (`nwinfo_set_mode ONLINE`, known not
/// to leave LPM reliably) is tried and the mode checked once more. Never
/// returns silently with the modem unverified: `Err` comes only after all
/// of that, and is preceded by a "may be stuck in LPM" log line.
fn bring_online(
    mut send_cfun: impl FnMut() -> Result<String, String>,
    mut read_mode: impl FnMut() -> Option<String>,
    fallback: impl FnOnce() -> Result<(), String>,
    verify_wait: Duration,
    verify_poll: Duration,
) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 1..=ONLINE_ATTEMPTS {
        match send_cfun() {
            Ok(resp) if resp.contains("OK") => return Ok(()),
            Ok(resp) => last_error = format!("AT+CFUN=1 failed: {}", resp.trim()),
            Err(e) => last_error = format!("AT+CFUN=1: {e}"),
        }
        if mode_online_within(&mut read_mode, verify_wait, verify_poll) {
            eprintln!(
                "[conn_watchdog] AT+CFUN=1 attempt {attempt}/{ONLINE_ATTEMPTS} unconfirmed ({last_error}) but operate_mode reads ONLINE"
            );
            return Ok(());
        }
        eprintln!(
            "[conn_watchdog] AT+CFUN=1 attempt {attempt}/{ONLINE_ATTEMPTS} failed: {last_error}"
        );
    }
    match fallback() {
        Ok(()) => {
            if mode_online_within(&mut read_mode, verify_wait, verify_poll) {
                eprintln!(
                    "[conn_watchdog] nwinfo_set_mode ONLINE fallback brought the modem ONLINE"
                );
                return Ok(());
            }
            eprintln!(
                "[conn_watchdog] nwinfo_set_mode ONLINE fallback accepted but operate_mode is not ONLINE"
            );
        }
        Err(e) => eprintln!("[conn_watchdog] nwinfo_set_mode ONLINE fallback failed: {e}"),
    }
    eprintln!("[conn_watchdog] modem may be stuck in LPM");
    Err(format!(
        "{last_error} ({ONLINE_ATTEMPTS} attempts); modem may be stuck in LPM"
    ))
}

/// Poll `read_mode` until it reports ONLINE or `wait` has elapsed; one read
/// when `wait` is zero.
fn mode_online_within(
    read_mode: &mut impl FnMut() -> Option<String>,
    wait: Duration,
    poll: Duration,
) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if read_mode().is_some_and(|m| m.trim().eq_ignore_ascii_case("ONLINE")) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// One rung taken, by wall-clock time (the only clock that survives a restart).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ActionRecord {
    rung: Rung,
    at: u64,
}

/// On-disk state ([`STATE_PATH`]).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Persisted {
    /// Fast boot was turned off for a rung-4 reboot and must be turned back on.
    #[serde(default)]
    restore_fast_boot: bool,
    /// Rungs taken in the last 24 h (cooldowns and daily caps across restarts).
    #[serde(default)]
    actions: Vec<ActionRecord>,
}

impl Persisted {
    fn load(path: &Path) -> Option<Self> {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
    }

    /// Drop records older than a day (or from the future, after a clock step).
    fn prune(&mut self, unix: u64) {
        self.actions
            .retain(|r| r.at <= unix && unix - r.at < DAY.as_secs());
    }
}

/// Current unix time in seconds (0 if the clock is before the epoch).
pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Executes actions and owns the persisted state.
struct Executor {
    effects: Box<dyn Effects>,
    state_path: PathBuf,
    restore_delay: Duration,
}

impl Executor {
    fn new(effects: Box<dyn Effects>, state_path: PathBuf) -> Self {
        Self {
            effects,
            state_path,
            restore_delay: FAST_BOOT_RESTORE_DELAY,
        }
    }

    /// At thread start: turn fast boot back on after a rung-4 reboot and
    /// seed the machine's history with the rungs taken in the last 24 h.
    /// Returns `true` while the fast-boot restore is still pending (ubus
    /// not up yet); the caller keeps retrying with
    /// [`Executor::retry_restore_fast_boot`].
    fn restore_state(&self, machine: &mut Machine, now: Instant) -> bool {
        let Some(mut saved) = Persisted::load(&self.state_path) else {
            return false;
        };
        let before = saved.clone();
        let mut pending = false;
        if saved.restore_fast_boot {
            if self.restore_fast_boot() {
                eprintln!("[conn_watchdog] fast boot restored after watchdog reboot");
                saved.restore_fast_boot = false;
            } else {
                eprintln!(
                    "[conn_watchdog] fast boot left off; retrying every {} s",
                    FAST_BOOT_RETRY_INTERVAL.as_secs()
                );
                pending = true;
            }
        }
        let unix = unix_now();
        saved.prune(unix);
        for record in &saved.actions {
            let age = Duration::from_secs(unix.saturating_sub(record.at));
            machine.record_action(record.rung, now.checked_sub(age).unwrap_or(now));
        }
        if !saved.actions.is_empty() {
            eprintln!(
                "[conn_watchdog] {} rung(s) taken in the last 24 h before this start",
                saved.actions.len()
            );
        }
        if saved != before {
            if let Err(e) = saved.save(&self.state_path) {
                eprintln!("[conn_watchdog] {e}");
            }
        }
        pending
    }

    /// One more `set_fast_boot(true)` for a restore that was still pending
    /// after [`Executor::restore_state`]; clears the persisted flag on
    /// success. Returns `true` while the restore is still pending.
    fn retry_restore_fast_boot(&self) -> bool {
        match self.effects.set_fast_boot(true) {
            Ok(()) => {
                eprintln!("[conn_watchdog] fast boot restored after watchdog reboot");
                if let Err(e) = self.set_restore_flag(false) {
                    eprintln!("[conn_watchdog] {e}");
                }
                false
            }
            Err(e) => {
                eprintln!("[conn_watchdog] restore fast boot retry failed: {e}");
                true
            }
        }
    }

    /// Retry `set_fast_boot(true)` a few times (ubus may not be up yet right
    /// after boot).
    fn restore_fast_boot(&self) -> bool {
        for attempt in 1..=FAST_BOOT_RESTORE_ATTEMPTS {
            match self.effects.set_fast_boot(true) {
                Ok(()) => return true,
                Err(e) => {
                    eprintln!("[conn_watchdog] restore fast boot attempt {attempt}/{FAST_BOOT_RESTORE_ATTEMPTS} failed: {e}");
                    if attempt < FAST_BOOT_RESTORE_ATTEMPTS {
                        std::thread::sleep(self.restore_delay);
                    }
                }
            }
        }
        false
    }

    /// Append `rung` to the persisted history; `restore_fast_boot` is set
    /// in the same write for the reboot rung.
    fn record(&self, rung: Rung, restore_fast_boot: bool) -> Result<(), String> {
        let unix = unix_now();
        let mut saved = Persisted::load(&self.state_path).unwrap_or_default();
        saved.prune(unix);
        saved.actions.push(ActionRecord { rung, at: unix });
        saved.restore_fast_boot = restore_fast_boot;
        saved.save(&self.state_path)
    }

    fn set_restore_flag(&self, restore_fast_boot: bool) -> Result<(), String> {
        let mut saved = Persisted::load(&self.state_path).unwrap_or_default();
        saved.restore_fast_boot = restore_fast_boot;
        saved.save(&self.state_path)
    }

    fn perform(&self, action: Action) {
        match action {
            Action::Log(message) => eprintln!("[conn_watchdog] {message}"),
            Action::Act(rung) => {
                let message = format!("rung {}: {}", rung.number(), rung.label());
                eprintln!("[conn_watchdog] {message}");
                self.effects.notify(&message);
                // The reboot rung records itself together with the fast-boot flag.
                if rung != Rung::Reboot {
                    if let Err(e) = self.record(rung, false) {
                        eprintln!("[conn_watchdog] cannot persist rung {}: {e}", rung.number());
                    }
                }
                let result = match rung {
                    Rung::PdnReconnect => self.effects.pdn_reconnect(),
                    Rung::LpmOnline => self.effects.lpm_online(),
                    Rung::ModemReset => self.effects.modem_reset(),
                    Rung::Reboot => self.reboot_with_fast_boot_off(),
                };
                match result {
                    Ok(()) => eprintln!("[conn_watchdog] rung {} done", rung.number()),
                    Err(e) => eprintln!("[conn_watchdog] rung {} failed: {e}", rung.number()),
                }
            }
        }
    }

    /// Fast boot off, state written (so the next start turns it back on and
    /// counts the reboot), then reboot. If anything after the fast-boot
    /// write fails, fast boot is turned back on right away rather than at
    /// the next agent start.
    fn reboot_with_fast_boot_off(&self) -> Result<(), String> {
        self.effects.set_fast_boot(false)?;
        if let Err(e) = self.record(Rung::Reboot, true) {
            self.undo_fast_boot_off();
            return Err(format!("not rebooting: {e}"));
        }
        if let Err(e) = self.effects.reboot() {
            self.undo_fast_boot_off();
            return Err(e);
        }
        Ok(())
    }

    /// The reboot did not happen: restore fast boot now and clear the flag;
    /// if that fails the flag stays so the next agent start retries.
    fn undo_fast_boot_off(&self) {
        match self.effects.set_fast_boot(true) {
            Ok(()) => {
                eprintln!("[conn_watchdog] reboot aborted; fast boot restored");
                if let Err(e) = self.set_restore_flag(false) {
                    eprintln!("[conn_watchdog] {e}");
                }
            }
            Err(e) => {
                eprintln!("[conn_watchdog] reboot aborted and fast boot restore failed: {e}; will retry at next agent start");
                if let Err(e) = self.set_restore_flag(true) {
                    eprintln!("[conn_watchdog] {e}");
                }
            }
        }
    }
}

// ── Runtime ─────────────────────────────────────────────────────────

pub struct ConnWatchdog {
    config: Mutex<Config>,
    tx: Mutex<Option<mpsc::Sender<Event>>>,
    user_activity: Mutex<Option<Instant>>,
    status: Mutex<Value>,
    /// The watchdog thread; user-activity reports from it are the
    /// watchdog's own effects echoed back through an instrumented handler.
    thread: Mutex<Option<ThreadId>>,
    /// Set once a probe source (the Live Activity recovery loop) has
    /// attached or delivered a result; without one the watchdog is inert.
    probe_attached: AtomicBool,
    config_path: PathBuf,
    state_path: PathBuf,
}

impl ConnWatchdog {
    pub fn new() -> Self {
        Self::with_paths(CONFIG_PATH.into(), STATE_PATH.into())
    }

    pub fn with_paths(config_path: PathBuf, state_path: PathBuf) -> Self {
        let config = Config::load(&config_path);
        Self {
            config: Mutex::new(config),
            tx: Mutex::new(None),
            user_activity: Mutex::new(None),
            status: Mutex::new(json!({"state": "stopped"})),
            thread: Mutex::new(None),
            probe_attached: AtomicBool::new(false),
            config_path,
            state_path,
        }
    }

    /// True on the watchdog thread itself, i.e. inside one of its own effects.
    fn is_own_thread(&self) -> bool {
        *self.thread.lock().unwrap() == Some(std::thread::current().id())
    }

    /// Spawn the watchdog thread and a forwarder for the
    /// `router_event_wan_connect_status` subscription. Call once, after
    /// `EventBus::start()`. Probe results arrive via [`ConnWatchdog::probe_result`].
    pub fn start(
        self: &Arc<Self>,
        effects: Box<dyn Effects>,
        busy: BusyCheck,
        wan_rx: mpsc::Receiver<Value>,
    ) {
        let (tx, rx) = mpsc::channel();
        *self.tx.lock().unwrap() = Some(tx.clone());
        let _ = std::thread::Builder::new()
            .name("connwd-wan".into())
            .spawn(move || {
                for payload in wan_rx {
                    if let Some(connected) = wan_from_event(&payload) {
                        if tx.send(Event::Wan(connected)).is_err() {
                            break;
                        }
                    }
                }
            });
        let watchdog = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("conn-watchdog".into())
            .spawn(move || watchdog.run(rx, effects, busy));
    }

    fn run(&self, rx: mpsc::Receiver<Event>, effects: Box<dyn Effects>, busy: BusyCheck) {
        *self.thread.lock().unwrap() = Some(std::thread::current().id());
        let mut machine = Machine::new(self.config());
        machine.set_busy_check(busy);
        let executor = Executor::new(effects, self.state_path.clone());
        let mut fast_boot_pending = executor.restore_state(&mut machine, Instant::now());
        if self.config().enabled && !self.probe_attached() {
            eprintln!("[conn_watchdog] {NO_PROBE_WARNING}");
        }
        if let Some(connected) = executor.effects.read_wan_connected() {
            eprintln!("[conn_watchdog] initial WAN state: connected={connected}");
            self.apply(
                &mut machine,
                &executor,
                Event::Wan(connected),
                Instant::now(),
            );
        }
        self.publish(&machine);
        loop {
            // Wake at the machine's deadline, or (only while a fast-boot
            // restore is pending) within FAST_BOOT_RETRY_INTERVAL; the
            // machine ignores an early Timer.
            let mut wake = machine.next_deadline();
            if fast_boot_pending {
                let retry = Instant::now() + FAST_BOOT_RETRY_INTERVAL;
                wake = Some(wake.map_or(retry, |d| d.min(retry)));
            }
            let event = match wake {
                Some(deadline) => {
                    let wait = deadline.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(wait) {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Timeout) => Event::Timer,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                None => match rx.recv() {
                    Ok(event) => event,
                    Err(_) => break,
                },
            };
            if fast_boot_pending {
                fast_boot_pending = executor.retry_restore_fast_boot();
            }
            self.apply(&mut machine, &executor, event, Instant::now());
            self.publish(&machine);
        }
        eprintln!("[conn_watchdog] event channel closed; watchdog thread exiting");
    }

    fn apply(&self, machine: &mut Machine, executor: &Executor, event: Event, now: Instant) {
        machine.set_user_activity(*self.user_activity.lock().unwrap());
        if machine.config.enabled && executor.effects.recovery_blocked().is_some() {
            // Do not count Wi-Fi/portal failures against SIM or carry an old
            // escalation deadline across a change of upstream.
            machine.state = State::Idle;
            machine.deadline = None;
            machine.failures = 0;
            machine.first_failure = None;
            if matches!(event, Event::Probe(_) | Event::Timer) {
                return;
            }
        }
        for action in machine.step(event, now) {
            executor.perform(action);
        }
    }

    fn publish(&self, machine: &Machine) {
        *self.status.lock().unwrap() = machine.status(Instant::now());
    }

    fn send(&self, event: Event) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(event);
        }
    }

    /// Result of one internet probe; call from the Live Activity recovery
    /// loop after every `verified_server_time` attempt.
    pub fn probe_result(&self, ok: bool) {
        self.probe_attached.store(true, Ordering::Relaxed);
        self.send(Event::Probe(ok));
    }

    /// Declare that a probe source will call [`ConnWatchdog::probe_result`]
    /// (the Live Activity recovery loop, right before it is spawned).
    pub fn attach_probe(&self) {
        self.probe_attached.store(true, Ordering::Relaxed);
    }

    fn probe_attached(&self) -> bool {
        self.probe_attached.load(Ordering::Relaxed)
    }

    /// `"live_activity"` once a probe source has attached, else `"none"`.
    pub fn probe_source(&self) -> &'static str {
        if self.probe_attached() {
            "live_activity"
        } else {
            "none"
        }
    }

    /// Feed a `router_event_wan_connect_status` payload directly
    /// (alternative to the receiver passed to [`ConnWatchdog::start`]).
    #[allow(dead_code)]
    pub fn wan_event(&self, payload: &Value) {
        if let Some(connected) = wan_from_event(payload) {
            self.send(Event::Wan(connected));
        }
    }

    /// Record a user-initiated modem change (network mode, data toggle,
    /// airplane, cell lock...). No watchdog action within
    /// [`USER_ACTIVITY_GUARD`] of it.
    ///
    /// Ignored when called from the watchdog thread: that is one of the
    /// watchdog's own rungs echoed back through an instrumented handler,
    /// not a user change.
    pub fn set_user_activity(&self, at: Instant) {
        if self.is_own_thread() {
            eprintln!(
                "[conn_watchdog] ignoring user activity reported by the watchdog's own action"
            );
            return;
        }
        *self.user_activity.lock().unwrap() = Some(at);
    }

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    pub fn status(&self) -> Value {
        let mut status = self.status.lock().unwrap().clone();
        if let Some(obj) = status.as_object_mut() {
            obj.insert("probe_source".into(), json!(self.probe_source()));
            obj.insert(
                "safety_block".into(),
                json!(automatic_recovery_allowed().err()),
            );
        }
        status
    }

    /// Validate, persist and apply a partial config update.
    pub fn update(&self, patch: &Value) -> Result<Config, String> {
        let mut config = self.config.lock().unwrap();
        let next = config.patched(patch)?;
        if next != *config {
            *config = next.clone();
            config.save(&self.config_path);
            self.send(Event::Config(next.clone()));
        }
        Ok(next)
    }
}

// ── HTTP handlers ───────────────────────────────────────────────────

fn config_json(config: &Config, status: Value) -> Value {
    json!({"ok": true, "data": {
        "enabled": config.enabled,
        "allow_reboot": config.allow_reboot,
        "status": status,
    }})
}

/// GET /api/modem/conn-watchdog
pub fn config_get(watchdog: &ConnWatchdog) -> (u16, Value) {
    (200, config_json(&watchdog.config(), watchdog.status()))
}

/// PUT /api/modem/conn-watchdog — body: any of `{enabled, allow_reboot}`.
pub fn config_set(watchdog: &ConnWatchdog, body: &[u8]) -> (u16, Value) {
    let patch: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    match watchdog.update(&patch) {
        Ok(config) => {
            let mut body = config_json(&config, watchdog.status());
            if config.enabled && !watchdog.probe_attached() {
                eprintln!("[conn_watchdog] {NO_PROBE_WARNING}");
                body["data"]["warning"] = json!(NO_PROBE_WARNING);
            }
            if config.enabled {
                if let Err(reason) = automatic_recovery_allowed() {
                    body["data"]["warning"] = json!(reason);
                }
            }
            (200, body)
        }
        Err(e) => (400, json!({"ok": false, "error": e})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const MIN: Duration = Duration::from_secs(60);

    fn config(allow_reboot: bool) -> Config {
        Config {
            enabled: true,
            allow_reboot,
        }
    }

    #[test]
    fn external_cellular_intent_unknown_never_runs_automatic_modem_effects() {
        let effects = DeviceEffects::new(Arc::new(AppState::new()));
        assert_eq!(
            effects.recovery_blocked(),
            Some(EXTERNAL_INTENT_UNAVAILABLE)
        );
        for result in [
            effects.pdn_reconnect(),
            effects.lpm_online(),
            effects.modem_reset(),
            effects.reboot(),
        ] {
            assert_eq!(result.err().as_deref(), Some(EXTERNAL_INTENT_UNAVAILABLE));
        }
        assert!(effects.network_guard().is_err());
    }

    #[test]
    fn pending_relay_probe_failures_never_trigger_a_cellular_recovery() {
        struct Relay;
        impl Effects for Relay {
            fn recovery_blocked(&self) -> Option<&'static str> {
                Some("Wi-Fi relay owns the upstream")
            }
            fn pdn_reconnect(&self) -> Result<(), String> {
                panic!("relay probe must not bounce SIM")
            }
            fn lpm_online(&self) -> Result<(), String> {
                panic!("relay probe must not reset modem")
            }
            fn modem_reset(&self) -> Result<(), String> {
                panic!("relay probe must not reset modem")
            }
            fn set_fast_boot(&self, _: bool) -> Result<(), String> {
                panic!()
            }
            fn reboot(&self) -> Result<(), String> {
                panic!()
            }
        }
        let path = temp_path("relay-guard");
        let watchdog = ConnWatchdog::with_paths(temp_path("relay-config"), path.clone());
        let executor = Executor::new(Box::new(Relay), path);
        let mut m = Machine::new(config(false));
        let now = Instant::now();
        watchdog.apply(&mut m, &executor, Event::Wan(true), now);
        for i in 0..5 {
            watchdog.apply(&mut m, &executor, Event::Probe(false), now + i * 10 * MIN);
        }
        assert_eq!(m.failures, 0);
        assert_eq!(m.state(), &State::Idle);
        assert_eq!(m.wan_connected, Some(true));
        watchdog.apply(&mut m, &executor, Event::Config(Config::default()), now);
        assert!(!m.config.enabled);
    }

    fn acts(actions: Vec<Action>) -> Vec<Rung> {
        actions
            .into_iter()
            .filter_map(|a| match a {
                Action::Act(r) => Some(r),
                Action::Log(_) => None,
            })
            .collect()
    }

    /// WAN up, then three failures spanning ten minutes: rung 1 fires at `t0 + 10 min`.
    fn triggered(m: &mut Machine, t0: Instant) {
        assert!(m.step(Event::Wan(true), t0).is_empty());
        assert!(acts(m.step(Event::Probe(false), t0)).is_empty());
        assert!(acts(m.step(Event::Probe(false), t0 + MIN)).is_empty());
        // Third failure but only 2 min in: not yet.
        assert!(acts(m.step(Event::Probe(false), t0 + 2 * MIN)).is_empty());
        assert_eq!(m.state(), &State::Idle);
        assert_eq!(m.next_deadline(), None);
        let actions = m.step(Event::Probe(false), t0 + 10 * MIN);
        assert_eq!(acts(actions), vec![Rung::PdnReconnect]);
        assert_eq!(
            m.state(),
            &State::Waiting {
                rung: Rung::PdnReconnect,
                acted_at: t0 + 10 * MIN,
                fails: 0
            }
        );
        assert_eq!(m.next_deadline(), Some(t0 + 15 * MIN));
    }

    /// One failure after the settle time, then the window timer: escalates.
    fn fail_window(m: &mut Machine, acted_at: Instant) -> Vec<Action> {
        assert!(
            acts(m.step(Event::Probe(false), acted_at + 30 * Duration::from_secs(1))).is_empty()
        );
        assert!(acts(m.step(Event::Probe(false), acted_at + 2 * MIN)).is_empty());
        m.step(Event::Timer, acted_at + 5 * MIN)
    }

    #[test]
    fn parses_wan_status() {
        assert!(wan_connected_from_status("ipv4_connected"));
        assert!(wan_connected_from_status("ipv4_ipv6_connected"));
        assert!(wan_connected_from_status("ipv6_connected"));
        assert!(wan_connected_from_status("connected"));
        assert!(!wan_connected_from_status("disconnected"));
        assert!(!wan_connected_from_status("connecting"));
        assert_eq!(
            wan_from_event(&json!({"wan_status": "ipv4_connected"})),
            Some(true)
        );
        assert_eq!(
            wan_from_event(&json!({"wan_status": "disconnected"})),
            Some(false)
        );
        assert_eq!(wan_from_event(&json!({"other": 1})), None);
    }

    #[test]
    fn config_defaults_and_patch() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
        assert!(!c.enabled && !c.allow_reboot);
        let c = c.patched(&json!({"enabled": true})).unwrap();
        assert!(c.enabled && !c.allow_reboot);
        assert!(c.patched(&json!({"allow_reboot": 1})).is_err());
        assert_eq!(Rung::ModemReset.next(false), None);
        assert_eq!(Rung::ModemReset.next(true), Some(Rung::Reboot));
        assert_eq!(Rung::Reboot.next(true), None);
    }

    #[test]
    fn trigger_needs_three_failures_over_ten_minutes_with_wan_up() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        triggered(&mut m, t0);
    }

    #[test]
    fn failures_are_ignored_while_wan_is_down_or_disabled() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        for i in 0..5 {
            assert!(m.step(Event::Probe(false), t0 + i * 10 * MIN).is_empty());
        }
        assert_eq!(m.state(), &State::Idle);
        m.step(Event::Wan(false), t0);
        for i in 0..5 {
            assert!(m.step(Event::Probe(false), t0 + i * 10 * MIN).is_empty());
        }
        let mut off = Machine::new(Config::default());
        off.step(Event::Wan(true), t0);
        for i in 0..5 {
            assert!(off.step(Event::Probe(false), t0 + i * 10 * MIN).is_empty());
        }
        assert_eq!(off.state(), &State::Idle);
    }

    #[test]
    fn wan_drop_resets_failure_count() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        m.step(Event::Wan(true), t0);
        m.step(Event::Probe(false), t0);
        m.step(Event::Probe(false), t0 + MIN);
        m.step(Event::Wan(false), t0 + 2 * MIN);
        m.step(Event::Wan(true), t0 + 3 * MIN);
        // Only one failure since the new session, even though 10 min passed.
        assert!(acts(m.step(Event::Probe(false), t0 + 15 * MIN)).is_empty());
        assert_eq!(m.status(t0 + 15 * MIN)["failures"], 1);
    }

    #[test]
    fn probe_ok_resets_the_ladder() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        triggered(&mut m, t0);
        let actions = m.step(Event::Probe(true), t0 + 12 * MIN);
        assert_eq!(
            actions,
            vec![Action::Log(
                "probe ok 120 s after rung 1 (PDN reconnect); connectivity restored".into()
            )]
        );
        assert_eq!(m.state(), &State::Idle);
        assert_eq!(m.next_deadline(), None);
        assert_eq!(m.status(t0 + 12 * MIN)["failures"], 0);
    }

    #[test]
    fn ladder_escalates_and_exhausts_without_reboot() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        triggered(&mut m, t0);
        let actions = fail_window(&mut m, t0 + 10 * MIN);
        assert_eq!(acts(actions), vec![Rung::LpmOnline]);
        assert_eq!(m.next_deadline(), Some(t0 + 20 * MIN));
        let actions = fail_window(&mut m, t0 + 15 * MIN);
        assert_eq!(acts(actions), vec![Rung::ModemReset]);
        let actions = fail_window(&mut m, t0 + 20 * MIN);
        assert!(acts(actions.clone()).is_empty());
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Log(s) if s.contains("ladder exhausted"))));
        assert_eq!(m.state(), &State::Idle);
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn reboot_rung_only_when_allowed() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(true));
        triggered(&mut m, t0);
        fail_window(&mut m, t0 + 10 * MIN);
        fail_window(&mut m, t0 + 15 * MIN);
        let actions = fail_window(&mut m, t0 + 20 * MIN);
        assert_eq!(acts(actions), vec![Rung::Reboot]);
    }

    #[test]
    fn early_failures_settle_and_late_failure_escalates_without_timer() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        triggered(&mut m, t0);
        let acted = t0 + 10 * MIN;
        // Within the settle time: not counted.
        assert!(m
            .step(Event::Probe(false), acted + Duration::from_secs(30))
            .is_empty());
        // Window ends with no counted failure: wait for the next probe.
        let actions = m.step(Event::Timer, acted + 5 * MIN);
        assert!(acts(actions.clone()).is_empty());
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Log(s) if s.contains("waiting for the next probe"))));
        assert_eq!(m.next_deadline(), None);
        // The next failure after the window escalates directly.
        let actions = m.step(Event::Probe(false), acted + 7 * MIN);
        assert_eq!(acts(actions), vec![Rung::LpmOnline]);
    }

    #[test]
    fn cooldown_defers_a_rung_until_it_expires() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        triggered(&mut m, t0); // rung 1 at t0+10
        fail_window(&mut m, t0 + 10 * MIN); // rung 2 at t0+15
        fail_window(&mut m, t0 + 15 * MIN); // rung 3 at t0+20
        fail_window(&mut m, t0 + 20 * MIN); // exhausted at t0+25
                                            // Fresh trigger: three failures from t0+25 over ten minutes.
        m.step(Event::Probe(false), t0 + 25 * MIN);
        m.step(Event::Probe(false), t0 + 30 * MIN);
        let actions = m.step(Event::Probe(false), t0 + 35 * MIN);
        assert_eq!(acts(actions), vec![Rung::PdnReconnect]); // 25 min since rung 1: ok
        let actions = fail_window(&mut m, t0 + 35 * MIN);
        assert_eq!(acts(actions), vec![Rung::LpmOnline]); // 25 min since rung 2: ok
        let actions = fail_window(&mut m, t0 + 40 * MIN);
        // Rung 3 ran at t0+20 with a 1 h cooldown: wait until t0+80.
        assert!(acts(actions.clone()).is_empty());
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Log(s) if s.contains("rung 3 cooldown"))));
        assert_eq!(
            m.state(),
            &State::Cooling {
                rung: Rung::ModemReset,
                until: t0 + 80 * MIN
            }
        );
        assert_eq!(m.next_deadline(), Some(t0 + 80 * MIN));
        assert!(m.step(Event::Probe(false), t0 + 60 * MIN).is_empty());
        let actions = m.step(Event::Timer, t0 + 80 * MIN);
        assert_eq!(acts(actions), vec![Rung::ModemReset]);
    }

    #[test]
    fn probe_ok_or_wan_drop_while_cooling_resets() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        m.step(Event::Wan(true), t0);
        m.record_action(Rung::PdnReconnect, t0);
        m.step(Event::Probe(false), t0);
        m.step(Event::Probe(false), t0 + MIN);
        let actions = m.step(Event::Probe(false), t0 + 10 * MIN);
        // Rung 1 cooldown has not expired (10 min since t0 exactly is fine)...
        assert_eq!(acts(actions), vec![Rung::PdnReconnect]);
        let mut m = Machine::new(config(false));
        m.step(Event::Wan(true), t0);
        m.record_action(Rung::PdnReconnect, t0 + 5 * MIN);
        m.step(Event::Probe(false), t0);
        m.step(Event::Probe(false), t0 + MIN);
        let actions = m.step(Event::Probe(false), t0 + 10 * MIN);
        assert!(acts(actions).is_empty());
        assert!(matches!(
            m.state(),
            State::Cooling {
                rung: Rung::PdnReconnect,
                ..
            }
        ));
        let actions = m.step(Event::Wan(false), t0 + 11 * MIN);
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Log(s) if s.contains("ladder reset"))));
        assert_eq!(m.state(), &State::Idle);
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn daily_cap_blocks_fourth_modem_reset() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        m.step(Event::Wan(true), t0);
        for h in [1u32, 3, 5] {
            m.record_action(Rung::ModemReset, t0 + h * 60 * MIN);
        }
        let now = t0 + 7 * 60 * MIN;
        m.record_action(Rung::LpmOnline, now - 30 * MIN);
        // Escalate from rung 2 directly.
        let actions = m.escalate(Rung::LpmOnline, now);
        assert!(acts(actions.clone()).is_empty());
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Log(s) if s.contains("daily cap (3/day)"))));
        // Blocked until the oldest of the three ages out: t0 + 1 h + 24 h.
        assert_eq!(m.next_deadline(), Some(t0 + 25 * 60 * MIN));
        let s = m.status(now);
        assert_eq!(s["actions_24h"]["modem_reset"], 3);
        assert_eq!(s["state"], "cooling");
        assert_eq!(s["rung"], "modem_reset");
    }

    #[test]
    fn guards_block_actions() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        let checks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&checks);
        m.set_busy_check(Box::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            Some("speed test")
        }));
        m.step(Event::Wan(true), t0);
        m.step(Event::Probe(false), t0);
        m.step(Event::Probe(false), t0 + MIN);
        // Probe results that do not trigger a rung never consult the guard.
        assert_eq!(checks.load(Ordering::Relaxed), 0);
        let actions = m.step(Event::Probe(false), t0 + 10 * MIN);
        assert!(acts(actions.clone()).is_empty());
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::Log(s) if s.contains("speed test in progress"))));
        assert_eq!(checks.load(Ordering::Relaxed), 1);
        assert_eq!(m.next_deadline(), Some(t0 + 11 * MIN));
        // Still busy at the retry: deferred again.
        let actions = m.step(Event::Timer, t0 + 11 * MIN);
        assert!(acts(actions).is_empty());
        assert_eq!(m.next_deadline(), Some(t0 + 12 * MIN));
        // Busy cleared but the user just changed the modem: wait out 2 min.
        m.set_busy_check(Box::new(|| None));
        m.set_user_activity(Some(t0 + 11 * MIN + Duration::from_secs(30)));
        let actions = m.step(Event::Timer, t0 + 12 * MIN);
        assert!(acts(actions).is_empty());
        assert_eq!(
            m.next_deadline(),
            Some(t0 + 13 * MIN + Duration::from_secs(30))
        );
        let actions = m.step(Event::Timer, t0 + 13 * MIN + Duration::from_secs(30));
        assert_eq!(acts(actions), vec![Rung::PdnReconnect]);
    }

    #[test]
    fn wan_outage_at_window_end_resets_ladder() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        triggered(&mut m, t0); // rung 1 at t0+10, window until t0+15
                               // A counted failure, then the WAN drops and stays down.
        assert!(acts(m.step(Event::Probe(false), t0 + 12 * MIN)).is_empty());
        assert!(m.step(Event::Wan(false), t0 + 13 * MIN).is_empty());
        assert!(matches!(m.state(), State::Waiting { .. }));
        // The window timer fires with WAN down: no escalation, ladder reset.
        let actions = m.step(Event::Timer, t0 + 15 * MIN);
        assert!(acts(actions.clone()).is_empty());
        assert!(actions.iter().any(
            |a| matches!(a, Action::Log(s) if s.contains("WAN down at the end of rung 1 window"))
        ));
        assert_eq!(m.state(), &State::Idle);
        assert_eq!(m.next_deadline(), None);
        // Hours later a new session comes up: a single failure must not
        // escalate to rung 2; the full trigger is needed again.
        m.step(Event::Wan(true), t0 + 5 * 60 * MIN);
        assert!(acts(m.step(Event::Probe(false), t0 + 5 * 60 * MIN)).is_empty());
        assert_eq!(m.state(), &State::Idle);
        assert_eq!(m.status(t0 + 5 * 60 * MIN)["failures"], 1);
    }

    #[test]
    fn wan_reconnect_after_window_resets_ladder() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        triggered(&mut m, t0); // rung 1 at t0+10
                               // The window ends with no counted failure (probe loop backing off):
                               // state stays Waiting with no deadline.
        let actions = m.step(Event::Timer, t0 + 15 * MIN);
        assert!(acts(actions).is_empty());
        assert_eq!(m.next_deadline(), None);
        assert!(matches!(m.state(), State::Waiting { .. }));
        // WAN drops and comes back hours later: not the rung's own bounce.
        assert!(m.step(Event::Wan(false), t0 + 20 * MIN).is_empty());
        let actions = m.step(Event::Wan(true), t0 + 4 * 60 * MIN);
        assert!(actions.iter().any(
            |a| matches!(a, Action::Log(s) if s.contains("WAN reconnected 230 min after rung 1"))
        ));
        assert_eq!(m.state(), &State::Idle);
        // One failure on the new session: no rung 2.
        assert!(acts(m.step(Event::Probe(false), t0 + 4 * 60 * MIN)).is_empty());
        assert_eq!(m.state(), &State::Idle);

        // Inside the window the bounce is part of the remedy: keep waiting.
        let mut m = Machine::new(config(false));
        triggered(&mut m, t0);
        assert!(m
            .step(Event::Wan(false), t0 + 10 * MIN + Duration::from_secs(2))
            .is_empty());
        assert!(m
            .step(Event::Wan(true), t0 + 10 * MIN + Duration::from_secs(20))
            .is_empty());
        assert!(matches!(m.state(), State::Waiting { .. }));
        assert_eq!(m.next_deadline(), Some(t0 + 15 * MIN));
        let actions = fail_window(&mut m, t0 + 10 * MIN);
        assert_eq!(acts(actions), vec![Rung::LpmOnline]);
    }

    #[test]
    fn disabling_resets_everything() {
        let t0 = Instant::now();
        let mut m = Machine::new(config(false));
        triggered(&mut m, t0);
        let actions = m.step(Event::Config(Config::default()), t0 + 12 * MIN);
        assert_eq!(actions, vec![Action::Log("watchdog disabled".into())]);
        assert_eq!(m.state(), &State::Idle);
        assert_eq!(m.next_deadline(), None);
        assert!(m.step(Event::Probe(false), t0 + 30 * MIN).is_empty());
    }

    // ── executor ────────────────────────────────────────────────────

    struct Mock {
        calls: Arc<Mutex<Vec<String>>>,
        /// Shared so a test can bring "ubus" back up after the executor owns the mock.
        fail_fast_boot: Arc<AtomicBool>,
        fail_reboot: bool,
    }

    impl Mock {
        fn new(calls: Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                calls,
                fail_fast_boot: Arc::new(AtomicBool::new(false)),
                fail_reboot: false,
            }
        }
    }

    impl Effects for Mock {
        fn pdn_reconnect(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("pdn".into());
            Ok(())
        }
        fn lpm_online(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("lpm_online".into());
            Ok(())
        }
        fn modem_reset(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("cfun".into());
            Ok(())
        }
        fn set_fast_boot(&self, on: bool) -> Result<(), String> {
            if self.fail_fast_boot.load(Ordering::Relaxed) {
                return Err("ubus down".into());
            }
            self.calls
                .lock()
                .unwrap()
                .push(format!("fast_boot={}", on as u8));
            Ok(())
        }
        fn reboot(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("reboot".into());
            if self.fail_reboot {
                return Err("ubus call system reboot failed".into());
            }
            Ok(())
        }
        fn notify(&self, message: &str) {
            self.calls.lock().unwrap().push(format!("notify:{message}"));
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("connwd-{name}-{}-{n}", std::process::id()))
    }

    #[test]
    fn executor_runs_rungs_notifies_and_persists_them() {
        let state = temp_path("state");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ex = Executor::new(Box::new(Mock::new(Arc::clone(&calls))), state.clone());
        ex.perform(Action::Act(Rung::PdnReconnect));
        ex.perform(Action::Act(Rung::LpmOnline));
        ex.perform(Action::Act(Rung::ModemReset));
        ex.perform(Action::Log("x".into()));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "notify:rung 1: PDN reconnect",
                "pdn",
                "notify:rung 2: LPM -> ONLINE",
                "lpm_online",
                "notify:rung 3: modem reset (AT+CFUN=1,1)",
                "cfun",
            ]
        );
        let saved = Persisted::load(&state).unwrap();
        assert!(!saved.restore_fast_boot);
        assert_eq!(
            saved.actions.iter().map(|r| r.rung).collect::<Vec<_>>(),
            vec![Rung::PdnReconnect, Rung::LpmOnline, Rung::ModemReset]
        );
        assert!(saved.actions.iter().all(|r| r.at > 0));
        let _ = fs::remove_file(&state);
    }

    #[test]
    fn rung_history_survives_a_restart() {
        let state = temp_path("history");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ex = Executor::new(Box::new(Mock::new(Arc::clone(&calls))), state.clone());
        for _ in 0..3 {
            ex.perform(Action::Act(Rung::ModemReset));
        }
        // Records older than a day (or from the future) are dropped at load.
        let mut saved = Persisted::load(&state).unwrap();
        saved.actions.push(ActionRecord {
            rung: Rung::PdnReconnect,
            at: unix_now() - DAY.as_secs() - 1,
        });
        saved.actions.push(ActionRecord {
            rung: Rung::LpmOnline,
            at: unix_now() + 3600,
        });
        saved.save(&state).unwrap();

        // "Next start": the three resets count against the daily cap.
        calls.lock().unwrap().clear();
        let mut m = Machine::new(config(false));
        let now = Instant::now();
        ex.restore_state(&mut m, now);
        assert!(calls.lock().unwrap().is_empty());
        let s = m.status(now);
        assert_eq!(s["actions_24h"]["modem_reset"], 3);
        assert_eq!(s["actions_24h"]["pdn_reconnect"], 0);
        assert_eq!(s["actions_24h"]["lpm_online"], 0);
        assert!(m
            .blocked_until(Rung::ModemReset, now)
            .is_some_and(|(_, why)| why.contains("daily cap")));
        assert_eq!(Persisted::load(&state).unwrap().actions.len(), 3);
        let _ = fs::remove_file(&state);
    }

    #[test]
    fn reboot_writes_state_and_next_start_restores_fast_boot() {
        let state = temp_path("reboot");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ex = Executor::new(Box::new(Mock::new(Arc::clone(&calls))), state.clone());
        ex.perform(Action::Act(Rung::Reboot));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "notify:rung 4: reboot with fast boot off",
                "fast_boot=0",
                "reboot"
            ]
        );
        let saved = Persisted::load(&state).unwrap();
        assert!(saved.restore_fast_boot);
        assert_eq!(saved.actions.len(), 1);
        assert_eq!(saved.actions[0].rung, Rung::Reboot);
        assert!(saved.actions[0].at > 0);

        // "Next boot": fast boot goes back on, the reboot counts for the cap.
        calls.lock().unwrap().clear();
        let mut m = Machine::new(config(true));
        let now = Instant::now();
        assert!(!ex.restore_state(&mut m, now));
        assert_eq!(*calls.lock().unwrap(), vec!["fast_boot=1"]);
        let saved = Persisted::load(&state).unwrap();
        assert!(!saved.restore_fast_boot);
        assert_eq!(saved.actions.len(), 1);
        assert_eq!(m.status(now)["actions_24h"]["reboot"], 1);
        assert!(m.blocked_until(Rung::Reboot, now).is_some());

        // A second start does not touch fast boot again.
        calls.lock().unwrap().clear();
        ex.restore_state(&mut m, now);
        assert!(calls.lock().unwrap().is_empty());
        let _ = fs::remove_file(&state);
    }

    #[test]
    fn failed_reboot_restores_fast_boot_immediately() {
        let state = temp_path("noreboot");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut mock = Mock::new(Arc::clone(&calls));
        mock.fail_reboot = true;
        let ex = Executor::new(Box::new(mock), state.clone());
        ex.perform(Action::Act(Rung::Reboot));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "notify:rung 4: reboot with fast boot off",
                "fast_boot=0",
                "reboot",
                "fast_boot=1",
            ]
        );
        // Nothing left for the next start to undo; the attempt still counts.
        let saved = Persisted::load(&state).unwrap();
        assert!(!saved.restore_fast_boot);
        assert_eq!(saved.actions.len(), 1);
        let _ = fs::remove_file(&state);
    }

    #[test]
    fn failed_fast_boot_restore_keeps_flag_and_retries_later() {
        let state = temp_path("keep");
        Persisted {
            restore_fast_boot: true,
            actions: Vec::new(),
        }
        .save(&state)
        .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mock = Mock::new(Arc::clone(&calls));
        let ubus_down = Arc::clone(&mock.fail_fast_boot);
        ubus_down.store(true, Ordering::Relaxed);
        let mut ex = Executor::new(Box::new(mock), state.clone());
        ex.restore_delay = Duration::ZERO;
        let mut m = Machine::new(config(true));
        // ubus not up yet: the flag stays and the restore is reported pending.
        assert!(ex.restore_state(&mut m, Instant::now()));
        assert!(Persisted::load(&state).unwrap().restore_fast_boot);
        assert!(calls.lock().unwrap().is_empty());
        // Still down at the next wakeup: still pending, flag kept.
        assert!(ex.retry_restore_fast_boot());
        assert!(Persisted::load(&state).unwrap().restore_fast_boot);
        // ubus comes up: one call restores fast boot and clears the flag.
        ubus_down.store(false, Ordering::Relaxed);
        assert!(!ex.retry_restore_fast_boot());
        assert_eq!(*calls.lock().unwrap(), vec!["fast_boot=1"]);
        assert!(!Persisted::load(&state).unwrap().restore_fast_boot);
        let _ = fs::remove_file(&state);
    }

    /// Rung 2's ONLINE half must not give up on one empty AT reply (port
    /// contention) and leave the modem in LPM.
    #[test]
    fn online_retries_cfun_verifies_by_mode_and_falls_back() {
        let z = Duration::ZERO;
        // Empty replies until the third attempt answers OK: no fallback.
        let attempts = AtomicUsize::new(0);
        let fallbacks = AtomicUsize::new(0);
        let r = bring_online(
            || {
                let n = attempts.fetch_add(1, Ordering::Relaxed) + 1;
                Ok(if n == 3 {
                    "\r\nOK\r\n".into()
                } else {
                    String::new()
                })
            },
            || Some("LPM".into()),
            || {
                fallbacks.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            z,
            z,
        );
        assert!(r.is_ok());
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
        assert_eq!(fallbacks.load(Ordering::Relaxed), 0);

        // The reply is lost but uci says ONLINE: success after one attempt.
        let attempts = AtomicUsize::new(0);
        let r = bring_online(
            || {
                attempts.fetch_add(1, Ordering::Relaxed);
                Err("no serial port found".into())
            },
            || Some("ONLINE".into()),
            || panic!("fallback not expected"),
            z,
            z,
        );
        assert!(r.is_ok());
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        // Nothing works: three attempts, the fallback, then Err naming LPM.
        let attempts = AtomicUsize::new(0);
        let fallbacks = AtomicUsize::new(0);
        let r = bring_online(
            || {
                attempts.fetch_add(1, Ordering::Relaxed);
                Ok("\r\nERROR\r\n".into())
            },
            || Some("LPM".into()),
            || {
                fallbacks.fetch_add(1, Ordering::Relaxed);
                Err("ubus down".into())
            },
            z,
            z,
        );
        let err = r.unwrap_err();
        assert!(err.contains("AT+CFUN=1 failed: ERROR"), "{err}");
        assert!(err.contains("stuck in LPM"), "{err}");
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
        assert_eq!(fallbacks.load(Ordering::Relaxed), 1);

        // The fallback is accepted and the mode flips to ONLINE: Ok.
        let reads = AtomicUsize::new(0);
        let r = bring_online(
            || Ok(String::new()),
            || {
                // Three post-attempt reads say LPM; the post-fallback read says ONLINE.
                let n = reads.fetch_add(1, Ordering::Relaxed);
                Some(if n < 3 { "LPM" } else { "ONLINE" }.into())
            },
            || Ok(()),
            z,
            z,
        );
        assert!(r.is_ok());
        assert_eq!(reads.load(Ordering::Relaxed), 4);
    }

    /// Enabling the watchdog on a unit where nothing feeds it a probe must
    /// not be silent: the status names the probe source and `config_set`
    /// returns a warning until one attaches.
    #[test]
    fn status_reports_probe_source_and_enabling_without_one_warns() {
        let watchdog =
            ConnWatchdog::with_paths(temp_path("probe-config"), temp_path("probe-state"));
        assert_eq!(watchdog.status()["probe_source"], "none");
        let (status, body) = config_set(&watchdog, br#"{"enabled": true}"#);
        assert_eq!(status, 200);
        assert_eq!(body["data"]["enabled"], true);
        assert_eq!(body["data"]["status"]["probe_source"], "none");
        assert_eq!(body["data"]["warning"], EXTERNAL_INTENT_UNAVAILABLE);
        // Disabling never warns.
        let (_, body) = config_set(&watchdog, br#"{"enabled": false}"#);
        assert!(body["data"].get("warning").is_none());

        watchdog.attach_probe();
        assert_eq!(watchdog.status()["probe_source"], "live_activity");
        let (status, body) = config_set(&watchdog, br#"{"enabled": true}"#);
        assert_eq!(status, 200);
        assert_eq!(body["data"]["warning"], EXTERNAL_INTENT_UNAVAILABLE);
        assert_eq!(body["data"]["status"]["probe_source"], "live_activity");

        // A probe result alone also counts as an attached source.
        let other = ConnWatchdog::with_paths(temp_path("probe2-config"), temp_path("probe2-state"));
        other.probe_result(false);
        assert_eq!(other.status()["probe_source"], "live_activity");
        let _ = fs::remove_file(&watchdog.config_path);
        let _ = fs::remove_file(&other.config_path);
    }

    /// Regression: with a handler instrumented to call
    /// `conn_watchdog.set_user_activity`, rung 2 (LPM -> ONLINE) used to
    /// count as a user modem change and defer the next rung by 2 min. The
    /// runtime must ignore reports that originate from its own thread.
    #[test]
    fn own_thread_user_activity_is_ignored() {
        let watchdog = Arc::new(ConnWatchdog::with_paths(
            temp_path("own-config"),
            temp_path("own-state"),
        ));
        // This test thread plays the watchdog thread.
        *watchdog.thread.lock().unwrap() = Some(std::thread::current().id());
        watchdog.set_user_activity(Instant::now());
        assert_eq!(*watchdog.user_activity.lock().unwrap(), None);
        // A real user request from another thread is recorded.
        let from_http = Arc::clone(&watchdog);
        std::thread::spawn(move || from_http.set_user_activity(Instant::now()))
            .join()
            .unwrap();
        assert!(watchdog.user_activity.lock().unwrap().is_some());
    }

    #[test]
    fn handlers_roundtrip_config() {
        let watchdog = ConnWatchdog::with_paths(temp_path("config"), temp_path("marker"));
        let (status, body) = config_get(&watchdog);
        assert_eq!(status, 200);
        assert_eq!(body["data"]["enabled"], false);
        assert_eq!(body["data"]["allow_reboot"], false);
        let (status, body) = config_set(&watchdog, br#"{"enabled": true}"#);
        assert_eq!(status, 200);
        assert_eq!(body["data"]["enabled"], true);
        assert_eq!(Config::load(&watchdog.config_path), watchdog.config());
        let (status, _) = config_set(&watchdog, br#"{"allow_reboot": "no"}"#);
        assert_eq!(status, 400);
        let (status, _) = config_set(&watchdog, b"{");
        assert_eq!(status, 400);
        let _ = fs::remove_file(&watchdog.config_path);
    }

    #[test]
    fn runtime_feeds_probe_and_wan_events() {
        let watchdog = Arc::new(ConnWatchdog::with_paths(
            temp_path("rt-config"),
            temp_path("rt-state"),
        ));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (wan_tx, wan_rx) = mpsc::channel();
        watchdog.start(
            Box::new(Mock::new(Arc::clone(&calls))),
            Box::new(|| None),
            wan_rx,
        );
        watchdog.update(&json!({"enabled": true})).unwrap();
        wan_tx
            .send(json!({"wan_status": "ipv4_connected"}))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        // The WAN payload goes through the forwarder thread; wait for it so
        // the probes below are counted against a connected session.
        while watchdog.status()["wan_connected"] != true && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        watchdog.probe_result(false);
        watchdog.probe_result(false);
        while watchdog.status()["failures"] != 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let status = watchdog.status();
        assert_eq!(status["failures"], 2);
        assert_eq!(status["wan_connected"], true);
        assert_eq!(status["state"], "idle");
        assert!(calls.lock().unwrap().is_empty());
        let _ = fs::remove_file(&watchdog.config_path);
    }
}
