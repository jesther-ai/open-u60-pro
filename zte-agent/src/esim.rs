//! eSIM profile management (GSMA SGP.22 LPA: ES10 to the card over QMI-over-QRTR, ES9+ to the
//! operator's SM-DP+ over HTTPS).
//!
//! Everything runs in-process on the LGPL-2.1 `euicc` crate (a Rust translation of lpac's
//! libeuicc) over the QRTR UIM transport: card commands and receipts in `esim_native`, profile
//! download in `esim_download`. Card commands keep lpac's names and result format (`chip info`,
//! `profile list`, `profile enable <iccid> 1`, `profile delete <iccid>`,
//! `profile nickname <iccid> [name]`, `notification list`, `notification process -r <seq>`), the
//! output the views below were verified against. One command at a time touches the card, with a
//! hard time limit; each holds the agent's flag and a `flock` on /tmp/esim.lock, shared with
//! `zte-agent esim-native` runs by hand.
//!
//! A download runs in the background. With preview on (the default) it stops after the
//! operator's server has sent the profile metadata; the app shows the operator/profile name and
//! answers through `/download/confirm`.
//!
//! Verified on a U60 Pro with a 9eSIM B1 (2026-10-18): listing leaves the connection alone;
//! enabling another profile (refresh flag set) makes the modem re-read the SIM, the firmware
//! picks the APN from its ICCID list, and WAN is down for roughly 20–45 s while it reattaches.
//!
//! While the modem reloads the SIM (a few seconds after `enable`) the eUICC's management
//! application cannot be selected: opening the channel (`euicc_init`) fails with QMI error 80
//! (`SimFileNotFound`), seen live 3 times in 14 switches. So the agent keeps the last good
//! profile list, answers reads from it while a switch reloads the SIM, and re-reads the card
//! itself once the SIM is ready. Each switch is then verified step by step: card, modem
//! identity (`get_sim_info`), data connection.

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cache;
use crate::conn_watchdog::unix_now;
use crate::esim_download::DownloadCtl;
use crate::events::watched::Watched;
use crate::events::Topic;
use crate::handlers::AppState;

/// Every card-only command observed on the router finished in under 4 s.
const CARD_LIMIT: Duration = Duration::from_secs(30);
/// Downloads talk to the operator's server, wait for the user's confirmation and write the
/// profile to the card.
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(300);
/// An unanswered preview is cancelled (reason timeout) so the server session ends cleanly.
const PREVIEW_LIMIT: Duration = Duration::from_secs(120);
/// A finished download's result is reported for this long, then the status reads `idle` again.
pub(crate) const RESULT_KEEP_SECS: u64 = 600;
/// After a switch the WAN is down while the modem reattaches; the connectivity policies hold off.
pub const SWITCH_GUARD_SECS: u64 = 300;
/// The firmware's SIM reload and radio restart take about 3–8 s after `enable`.
const RELOAD_SETTLE: Duration = Duration::from_secs(5);
/// How long the card may stay unreadable after a switch before it is reported unavailable.
const CARD_CONFIRM_LIMIT: Duration = Duration::from_secs(90);
/// How long the modem has to load the new identity and connect data after a switch.
const SWITCH_VERIFY_LIMIT: Duration = Duration::from_secs(150);
const SWITCH_POLL: Duration = Duration::from_secs(3);
/// A busy-refused enable is retried this often, for this long. With a Klook travel profile
/// enabled the card refused with catBusy for about two minutes from the first attempt, twice.
const CARD_BUSY_GAP: Duration = Duration::from_secs(10);
const CARD_BUSY_LIMIT: Duration = Duration::from_secs(240);
/// One receipt (ES9+ HandleNotification) to one operator server.
const RECEIPT_LIMIT: Duration = Duration::from_secs(45);
/// Pause between "data connected" after a switch and sending its receipts (DNS and routes settle).
const RECEIPT_AFTER_SWITCH: Duration = Duration::from_secs(5);
/// How long receipts wait for the router's default route after the modem reports data (the
/// route arrived 3-23 s later in the router's logs).
const WAN_READY_LIMIT: Duration = Duration::from_secs(40);
/// Second try for receipts a switch left that failed on the first.
const RECEIPT_SECOND_TRY: Duration = Duration::from_secs(30);
/// Pending receipts are retried this often while the router has mobile data.
const RECEIPT_RETRY: Duration = Duration::from_secs(600);
/// After this many failed attempts a receipt is left on the card and reported, not retried.
const RECEIPT_MAX_ATTEMPTS: u32 = 10;
/// Retry rounds before the background retry stops (about 4 hours); "send now" restarts it.
const RECEIPT_MAX_ROUNDS: u32 = 24;
/// SGP.22 profileNickname is a UTF8String of at most 64 bytes.
const NICKNAME_MAX_BYTES: usize = 64;
/// Shared with `zte-agent esim-native` runs by hand.
const DEFAULT_LOCK_PATH: &str = "/tmp/esim.lock";

pub struct Esim {
    inner: Arc<Inner>,
}

struct Inner {
    /// A command holds a logical channel to the card while it runs: never two at once.
    busy: AtomicBool,
    /// `mono_secs()` of the last successful profile switch (0 = none).
    switched_at: AtomicU64,
    download: Watched<Download>,
    /// The running download's decisions (preview answer, cancel).
    control: DownloadControl,
    /// Last good profile list, and whether it may be out of date.
    inventory: Mutex<Inventory>,
    /// The last profile switch, verified card → modem → data.
    switch: Watched<Switch>,
    /// Identifies this agent run (start time, ms): clients must drop cached state when it changes.
    instance: u64,
    /// Operator receipts (SGP.22 notifications) waiting on the card.
    receipts: Mutex<Receipts>,
    /// File lock shared with `zte-agent esim-native` runs outside the agent.
    lock_path: PathBuf,
}

/// Pending operator receipts: install/enable/disable/delete notifications the card keeps until
/// they reach the operator's server. Each receipt is sent on its own and counts its own attempts.
#[derive(Debug, Default)]
struct Receipts {
    /// As last listed from the card (`None` = not listed yet).
    pending: Option<Vec<Receipt>>,
    listed_at: u64,
    last_attempt_at: u64,
    last_sent_at: u64,
    /// A background retry loop is running.
    retrying: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Receipt {
    seq: u64,
    operation: String,
    address: String,
    iccid: Option<String>,
    attempts: u32,
    last_error: Option<String>,
}

impl Receipts {
    /// Replaces the list with a fresh card read, keeping attempt counts for receipts still there.
    fn merge(&mut self, fresh: Vec<Receipt>, now: u64) {
        let old = self.pending.take().unwrap_or_default();
        let merged = fresh
            .into_iter()
            .map(|mut r| {
                let same = |o: &&Receipt| {
                    o.seq == r.seq && o.address == r.address && o.operation == r.operation && o.iccid == r.iccid
                };
                if let Some(o) = old.iter().find(same) {
                    r.attempts = o.attempts;
                    r.last_error = o.last_error.clone();
                }
                r
            })
            .collect();
        self.pending = Some(merged);
        self.listed_at = now;
    }

    fn retryable(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.iter().any(|r| r.attempts < RECEIPT_MAX_ATTEMPTS))
    }

    fn view(&self, stale: bool) -> Value {
        let items: Vec<Value> = self
            .pending
            .iter()
            .flatten()
            .map(|r| {
                json!({
                    "seq": r.seq,
                    "operation": r.operation,
                    "address": r.address,
                    "iccid": r.iccid,
                    "attempts": r.attempts,
                    "last_error": r.last_error,
                    "gave_up": r.attempts >= RECEIPT_MAX_ATTEMPTS,
                })
            })
            .collect();
        json!({
            "count": self.pending.as_ref().map(Vec::len),
            "pending": items,
            "listed_at": self.listed_at,
            "last_attempt_at": self.last_attempt_at,
            "last_sent_at": self.last_sent_at,
            "retrying": self.retrying,
            "stale": stale,
        })
    }
}

fn receipt_views(data: &Value) -> Vec<Receipt> {
    data.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|n| {
                    Some(Receipt {
                        seq: n["seqNumber"].as_u64()?,
                        operation: n["profileManagementOperation"].as_str().unwrap_or("").to_string(),
                        address: n["notificationAddress"].as_str().unwrap_or("").to_string(),
                        iccid: n["iccid"].as_str().map(str::to_string),
                        attempts: 0,
                        last_error: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Last successful profile list.
#[derive(Debug, Default)]
struct Inventory {
    profiles: Option<Vec<Value>>,
    observed_at: u64,
    /// Counts good reads, so clients can order lists without the wall clock (it jumps by hours
    /// when the network sets the time on attach).
    observed_seq: u64,
    /// Bumped whenever the card's contents or selection change (switch, delete, download).
    generation: u64,
    /// A switch is reloading the SIM: reads are answered from this list without touching the card.
    reloading: bool,
    error: Option<ReadError>,
    /// EID of the card this list belongs to (learned from `chip info`).
    eid: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct ReadError {
    /// "sim_reloading", "management_unavailable", "read_failed", "busy" or "not_configured".
    kind: &'static str,
    message: String,
    at: u64,
}

impl Inventory {
    /// Records the card's EID. A different EID means the card was swapped: the list and its
    /// error belong to the old card and are dropped. Returns true in that case.
    fn note_card(&mut self, eid: &str) -> bool {
        if eid.is_empty() {
            return false;
        }
        match &self.eid {
            Some(known) if known != eid => {
                *self = Inventory {
                    eid: Some(eid.to_string()),
                    generation: self.generation + 1,
                    ..Inventory::default()
                };
                true
            }
            Some(_) => false,
            None => {
                self.eid = Some(eid.to_string());
                false
            }
        }
    }

    fn record(&mut self, profiles: Vec<Value>, now: u64) {
        self.profiles = Some(profiles);
        self.observed_at = now;
        self.observed_seq += 1;
        self.error = None;
    }

    /// `updating`: a newer list is coming (switch reload or another eSIM operation running).
    /// `stale`: the list is older than the latest attempt to read the card.
    fn view(&self, instance: u64, busy: bool) -> Value {
        let profiles = self.profiles.clone().unwrap_or_default();
        json!({
            "profiles": profiles,
            "enabled_iccid": enabled_iccid(&profiles),
            "observed_at": self.observed_at,
            "observed_seq": self.observed_seq,
            "generation": self.generation,
            "agent_instance": instance,
            "updating": self.reloading || busy,
            "stale": self.reloading || busy || self.error.is_some(),
            "error": self.error.as_ref().map(|e| json!({"kind": e.kind, "message": e.message, "at": e.at})),
        })
    }
}

/// The last profile switch. `state` moves reloading → card_confirmed → modem_confirmed and ends
/// as "connected", "no_data" (applied, no data connection), "modem_unconfirmed" (card switched,
/// modem kept the old identity), "card_unconfirmed" (card unreadable) or "not_applied".
#[derive(Clone, Debug, Default, PartialEq)]
struct Switch {
    state: &'static str,
    target: String,
    previous: Option<String>,
    generation: u64,
    started_at: u64,
    card_confirmed_at: u64,
    modem_confirmed_at: u64,
    connected_at: u64,
    finished_at: u64,
    note: Option<String>,
    /// Why the switch didn't complete, when known: `roaming_off` or `data_off` (no data), or
    /// `card_busy` (not applied: the eSIM kept refusing).
    reason: Option<&'static str>,
}

impl Switch {
    fn finish(&mut self, state: &'static str, now: u64) {
        self.state = state;
        self.finished_at = now;
    }

    fn view(&self) -> Value {
        json!({
            "state": if self.state.is_empty() { "idle" } else { self.state },
            "target_iccid": if self.target.is_empty() { Value::Null } else { json!(self.target) },
            "previous_iccid": self.previous,
            "generation": self.generation,
            "started_at": self.started_at,
            "card_confirmed_at": self.card_confirmed_at,
            "modem_confirmed_at": self.modem_confirmed_at,
            "connected_at": self.connected_at,
            "finished_at": self.finished_at,
            "note": self.note,
            "reason": self.reason,
        })
    }
}

/// Decisions for the running download; empty when none runs.
#[derive(Default)]
struct DownloadControl {
    tx: Mutex<Option<Sender<DownloadCtl>>>,
}

impl DownloadControl {
    /// Hands `ctl` to the running download; false when none is running.
    fn send(&self, ctl: DownloadCtl) -> bool {
        self.tx.lock().unwrap().as_ref().is_some_and(|tx| tx.send(ctl).is_ok())
    }
}

/// State of the last profile download, polled by the app.
#[derive(Clone, Debug, Default, PartialEq)]
struct Download {
    /// "idle", "running", "confirm" (waiting for the user), "cancelling", "done", "failed" or
    /// "cancelled".
    state: &'static str,
    /// Last progress step (lpac's names), e.g. `es9p_get_bound_profile_package`.
    step: String,
    provider: Option<String>,
    name: Option<String>,
    iccid: Option<String>,
    error: Option<String>,
    started_at: u64,
    finished_at: u64,
}

impl Download {
    fn is_finished(&self) -> bool {
        matches!(self.state, "done" | "failed" | "cancelled")
    }

    /// Forgets a finished result once it is older than `RESULT_KEEP_SECS`.
    fn expire(&mut self, now: u64) {
        if self.is_finished() && now.saturating_sub(self.finished_at) >= RESULT_KEEP_SECS {
            *self = Download::default();
        }
    }

    fn view(&self) -> Value {
        json!({
            "state": if self.state.is_empty() { "idle" } else { self.state },
            "step": self.step,
            "provider": self.provider,
            "name": self.name,
            "iccid": self.iccid,
            "error": self.error,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
        })
    }

    /// Records a progress step; `es8p_metadata_parse` carries the operator's profile metadata.
    fn progress(&mut self, step: &str, data: &Value) {
        // Cancelling the session is the download's cleanup after a failure or cancel: keep the
        // step that failed, so "failed at" names it.
        if !step.ends_with("_cancel_session") {
            self.step = step.to_string();
            if step == "preview" && self.state == "running" {
                self.state = "confirm";
            }
        }
        if data.is_object() {
            if let Some(p) = data["serviceProviderName"].as_str() {
                self.provider = Some(p.to_string());
            }
            if let Some(n) = data["profileName"].as_str() {
                self.name = Some(n.to_string());
            }
        }
    }
}

/// Released on drop, including when a download thread ends. Holds the shared file lock too,
/// so a `zte-agent esim-native` run by hand and the agent never overlap.
struct BusyGuard {
    inner: Arc<Inner>,
    _lock: Option<std::fs::File>,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        drop(self._lock.take());
        *HELD_LOCK.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.inner.busy.store(false, Ordering::Release);
    }
}

/// A second descriptor for the lock the current guard holds. `flock` belongs to the open file, so
/// a card worker that outlives its caller keeps the card locked against CLI runs by holding its
/// own copy (see `card`).
static HELD_LOCK: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// Seconds on a monotonic clock (+1, so 0 can mean "never"). The router's wall clock jumps by
/// hours when the network sets the time on attach; guards must not.
fn mono_secs() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs() + 1
}

impl Esim {
    pub fn new() -> Self {
        Self::with_lock_path(lock_path())
    }

    fn with_lock_path(lock_path: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                busy: AtomicBool::new(false),
                switched_at: AtomicU64::new(0),
                download: Watched::new(Download::default(), Topic::EsimDownload),
                control: DownloadControl::default(),
                inventory: Mutex::new(Inventory::default()),
                switch: Watched::new(Switch::default(), Topic::EsimSwitch),
                instance: crate::sim_identity::agent_instance(),
                receipts: Mutex::new(Receipts::default()),
                lock_path,
            }),
        }
    }

    fn acquire(&self) -> Option<BusyGuard> {
        acquire(&self.inner)
    }

    fn reloading(&self) -> bool {
        self.inner.inventory.lock().unwrap().reloading
    }

    /// Whether a profile switch happened within `SWITCH_GUARD_SECS` (busy guard for policies),
    /// on the monotonic clock.
    pub fn recently_switched(&self) -> bool {
        switched_within(self.inner.switched_at.load(Ordering::Relaxed), mono_secs())
    }
}

fn switched_within(at: u64, now: u64) -> bool {
    at != 0 && now.saturating_sub(at) < SWITCH_GUARD_SECS
}

fn error(status: u16, message: &str) -> (u16, Value) {
    (status, json!({"ok": false, "error": message}))
}

fn acquire(inner: &Arc<Inner>) -> Option<BusyGuard> {
    inner
        .busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .ok()?;
    let lock = match file_lock(&inner.lock_path) {
        Ok(lock) => lock,
        Err(()) => {
            // Someone else (a CLI run by hand) holds the card.
            inner.busy.store(false, Ordering::Release);
            return None;
        }
    };
    *HELD_LOCK.lock().unwrap_or_else(|e| e.into_inner()) = lock.as_ref().and_then(|f| f.try_clone().ok());
    Some(BusyGuard { inner: Arc::clone(inner), _lock: lock })
}

/// Non-blocking exclusive `flock`. `Ok(None)` when the lock file cannot be created at all (the
/// agent then relies on its own flag only); `Err` when another process holds the lock.
fn file_lock(path: &Path) -> Result<Option<std::fs::File>, ()> {
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    else {
        return Ok(None);
    };
    // SAFETY: `file` is an open descriptor owned by this function.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(Some(file))
    } else {
        Err(())
    }
}

fn busy() -> (u16, Value) {
    error(409, "eSIM manager busy; retry shortly")
}

fn reloading_reply() -> (u16, Value) {
    error(409, "the SIM is still reloading after the last switch; try again in a moment")
}

/// A failed card read: `euicc_init` / channel-open failures mean the eUICC's management
/// application could not be selected, which is expected while a switch reloads the SIM.
fn classify(message: &str, recently_switched: bool) -> &'static str {
    let channel = message.contains("euicc_init")
        || message.contains("open logical channel")
        || message.contains("SimFileNotFound");
    match (channel, recently_switched) {
        (true, true) => "sim_reloading",
        (true, false) => "management_unavailable",
        _ => "read_failed",
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/esim/profiles — a fresh read of the card, or the last good list (marked `updating`
/// / `stale`, with the read error) while a switch reloads the SIM, another eSIM operation runs,
/// or the read fails.
pub fn profiles(state: &AppState) -> (u16, Value) {
    let inner = &state.esim.inner;
    if state.esim.reloading() {
        return (200, json!({"ok": true, "data": inner.inventory.lock().unwrap().view(inner.instance, false)}));
    }
    let check = SwitchCheck::read(state);
    let Some(_guard) = state.esim.acquire() else {
        let inv = inner.inventory.lock().unwrap();
        return if inv.profiles.is_some() {
            let mut view = inv.view(inner.instance, true);
            check.annotate(&mut view);
            (200, json!({"ok": true, "data": view}))
        } else {
            busy()
        };
    };
    let result = card(&["profile", "list"], CARD_LIMIT);
    if result.is_ok() && inner.receipts.lock().unwrap().pending.is_none() {
        // First good read since the agent started: learn whether receipts are waiting.
        let _ = list_receipts(inner);
        ensure_receipt_retry(inner);
    }
    let now = unix_now();
    let mut inv = inner.inventory.lock().unwrap();
    match result {
        Ok(data) => inv.record(profile_views(&data), now),
        Err(e) => {
            let kind = match &e {
                CardError::Config(_) => "not_configured",
                CardError::NotStarted(_) => "busy",
                CardError::Failed(m) => classify(m, state.esim.recently_switched()),
            };
            inv.error = Some(ReadError { kind, message: e.message(), at: now });
            if inv.profiles.is_none() {
                let (status, mut body) = e.reply();
                body["kind"] = json!(kind);
                return (status, body);
            }
        }
    }
    let mut view = inv.view(inner.instance, false);
    drop(inv);
    view["pending_receipts"] = json!(inner.receipts.lock().unwrap().pending.as_ref().map(Vec::len));
    check.annotate(&mut view);
    (200, json!({"ok": true, "data": view}))
}

/// What switching to a profile would run into, shown before the user confirms. Read-only: the
/// agent never changes the APN or roaming setting itself.
#[derive(Debug, Default)]
struct SwitchCheck {
    /// The enabled manual APN (name, APN) when the router is in manual APN mode.
    manual_apn: Option<(String, String)>,
    roaming_off: bool,
    loaded_iccid: Option<String>,
    sim_mcc: Option<String>,
    net_mcc: Option<String>,
}

impl SwitchCheck {
    /// One uncached APN-mode read (and the manual APN list only in manual mode); the SIM, WAN
    /// and network values come from the shared cache.
    fn read(state: &AppState) -> Self {
        let manual = crate::ubus::call("zwrt_apn_object", "get_apn_mode", Some("{}"))
            .ok()
            .and_then(|v| int(&v["apn_mode"]))
            == Some(1);
        let manual_apn = manual
            .then(|| crate::ubus::call("zwrt_apn_object", "get_manu_apn_list", Some("{}")).ok())
            .flatten()
            .and_then(|list| enabled_manual_apn(&list));
        let roaming_off = crate::modem_ext::wwan(state).ok().and_then(|w| int(&w["roam_enable"])) == Some(0);
        let sim = crate::sim::info(state).unwrap_or_default();
        let text = |v: &Value| match v {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        };
        SwitchCheck {
            manual_apn,
            roaming_off,
            loaded_iccid: text(&sim["sim_iccid"]),
            sim_mcc: text(&sim["mdm_mcc"]),
            // Unregistered, the firmware reports MCC 0: no network country to compare with.
            net_mcc: state
                .cache
                .netinfo(cache::NETINFO_TTL)
                .ok()
                .and_then(|n| text(&n["rmcc"]))
                .filter(|m| real_mcc(m)),
        }
    }

    /// Codes for switching to `target`: `manual_apn` (any target; it will dial that APN too) and
    /// `roaming_off` (the target looks like a profile from another country).
    fn warnings(&self, target: &str) -> Vec<&'static str> {
        let mut codes = Vec::new();
        if self.manual_apn.is_some() {
            codes.push("manual_apn");
        }
        let roams = target_roams(
            target,
            self.loaded_iccid.as_deref(),
            self.sim_mcc.as_deref(),
            self.net_mcc.as_deref(),
        );
        if self.roaming_off && roams == Some(true) {
            codes.push("roaming_off");
        }
        codes
    }

    /// `switch_warnings` on each profile that can be switched to, and the top-level `manual_apn`.
    fn annotate(&self, view: &mut Value) {
        if let Some(profiles) = view["profiles"].as_array_mut() {
            for p in profiles {
                if p["state"] == "disabled" && p["class"] == "operational" {
                    let codes = self.warnings(p["iccid"].as_str().unwrap_or(""));
                    p["switch_warnings"] = json!(codes);
                }
            }
        }
        view["manual_apn"] = match &self.manual_apn {
            Some((name, apn)) => json!({"name": name, "apn": apn}),
            None => Value::Null,
        };
    }
}

/// A real 3-digit MCC (not the 0 the firmware reports while unregistered).
fn real_mcc(mcc: &str) -> bool {
    mcc.len() == 3 && mcc.bytes().all(|b| b.is_ascii_digit()) && mcc != "000"
}

/// A firmware number that may come as a number or a string.
fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
}

/// The enabled entry of `get_manu_apn_list` as (name, APN); its user name and password are
/// never copied.
fn enabled_manual_apn(list: &Value) -> Option<(String, String)> {
    list["apnListArray"].as_array()?.iter().find_map(|a| {
        let enabled = a["isEnable"] == true || int(&a["isEnable"]) == Some(1);
        let apn = a["wanapn"].as_str().map(str::trim).filter(|s| !s.is_empty())?;
        enabled.then(|| {
            let name = a["profilename"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(apn);
            (name.to_string(), apn.to_string())
        })
    })
}

/// The ITU country code in an ICCID (`89` + E.164 code, ITU-T E.118). E.164 codes are
/// prefix-free: 1 and 7 are one digit, the assigned two-digit ones are listed, the rest are three.
/// Two issuer conventions differ: US issuers pad the code (`8901…`) and Canadian ones use their
/// MCC (`89302…`); other leading zeros are not guessed at.
fn iccid_country(iccid: &str) -> Option<&str> {
    const TWO: [&str; 44] = [
        "20", "27", "30", "31", "32", "33", "34", "36", "39", "40", "41", "43", "44", "45", "46", "47",
        "48", "49", "51", "52", "53", "54", "55", "56", "57", "58", "60", "61", "62", "63", "64", "65",
        "66", "81", "82", "84", "86", "90", "91", "92", "93", "94", "95", "98",
    ];
    let digits = iccid.trim().strip_prefix("89")?;
    if digits.len() < 3 || !digits[..3].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.starts_with("01") {
        return Some("1");
    }
    if digits.starts_with('0') {
        return None;
    }
    if digits.starts_with("302") {
        return Some("302");
    }
    Some(match &digits[..1] {
        "1" | "7" => &digits[..1],
        _ if TWO.contains(&&digits[..2]) => &digits[..2],
        _ => &digits[..3],
    })
}

/// Whether `target` would roam where the router is now, judged from ICCID countries: next to a
/// home SIM, another country roams; next to a roaming SIM, only its own country is known to roam.
/// `None` when it can't be told.
fn target_roams(target: &str, loaded: Option<&str>, sim_mcc: Option<&str>, net_mcc: Option<&str>) -> Option<bool> {
    let target = iccid_country(target)?;
    let loaded = iccid_country(loaded?)?;
    let home = sim_mcc? == net_mcc?;
    match (home, target == loaded) {
        (true, same) => Some(!same),
        (false, true) => Some(true),
        (false, false) => None,
    }
}

/// GET /api/esim/switch — progress and verified outcome of the last profile switch.
pub fn switch_status(state: &AppState) -> (u16, Value) {
    let mut view = state.esim.inner.switch.lock().unwrap().view();
    if let Some(obj) = view.as_object_mut() {
        state.sim_identity.stamp(obj);
    }
    (200, json!({"ok": true, "data": view}))
}

/// GET /api/esim/chip — card identity and capacity.
pub fn chip(state: &AppState) -> (u16, Value) {
    let inner = &state.esim.inner;
    if state.esim.reloading() {
        return reloading_reply();
    }
    let Some(_guard) = state.esim.acquire() else {
        return busy();
    };
    match card(&["chip", "info"], CARD_LIMIT) {
        Ok(data) => {
            let changed = inner
                .inventory
                .lock()
                .unwrap()
                .note_card(data["eidValue"].as_str().unwrap_or(""));
            let mut view = chip_view(&data);
            view["card_changed"] = json!(changed);
            (200, json!({"ok": true, "data": view}))
        }
        Err(e) => e.reply(),
    }
}

/// Reads the card's EID (guard held) and refuses to continue if it is not the card the saved
/// list belongs to.
fn check_card(inner: &Arc<Inner>) -> Result<Value, (u16, Value)> {
    let data = card(&["chip", "info"], CARD_LIMIT).map_err(CardError::reply)?;
    let eid = data["eidValue"].as_str().unwrap_or("");
    if inner.inventory.lock().unwrap().note_card(eid) {
        eprintln!("[esim] a different eSIM card is in the slot; saved list dropped");
        return Err(error(409, "a different eSIM card is in the slot; reload the profile list"));
    }
    Ok(data)
}

/// Parses `{"iccid": "..."}` for the mutating profile actions.
fn iccid_body(body: &[u8]) -> Result<String, (u16, Value)> {
    let parsed: Value = serde_json::from_slice(body).map_err(|_| error(400, "invalid JSON"))?;
    parsed["iccid"]
        .as_str()
        .and_then(normalize_iccid)
        .ok_or_else(|| error(400, "iccid must be 18–20 digits"))
}

/// POST /api/esim/enable — body `{"iccid": "..."}`. Switches the enabled profile; the WAN drops
/// while the modem reattaches with the new one.
pub fn enable(state: &AppState, body: &[u8]) -> (u16, Value) {
    if !state.auth.has_password() {
        return error(403, "agent password required");
    }
    let iccid = match iccid_body(body) {
        Ok(i) => i,
        Err(reply) => return reply,
    };
    if state.esim.reloading() {
        return reloading_reply();
    }
    if state.esim.inner.switch.lock().unwrap().state == "card_busy" {
        return error(409, "the router is still retrying the last switch while the eSIM is busy");
    }
    let mode = crate::ubus::uci_get("zte_nwinfo.sys_info.operate_mode").ok();
    let policy = state.oos_policy.status();
    if radio_blocks_switch(mode.as_deref(), policy["state"].as_str()) {
        return error(
            409,
            "the modem is in airplane / low-power mode; turn it back on before switching profiles",
        );
    }
    let Some(guard) = state.esim.acquire() else {
        return busy();
    };
    if let Err(reply) = check_card(&state.esim.inner) {
        return reply;
    }
    let list = match card(&["profile", "list"], CARD_LIMIT) {
        Ok(data) => profile_views(&data),
        Err(e) => return e.reply(),
    };
    let previous = enabled_iccid(&list);
    if let Err((status, message)) = check_enable_target(&list, &iccid) {
        return error(status, message);
    }
    state.sim_identity.agent_switch_arm();
    let result = card(&["profile", "enable", &iccid, "1"], CARD_LIMIT);
    drop(guard);
    if let Err(CardError::NotStarted(m)) = &result {
        state.sim_identity.agent_switch_disarm();
        return error(409, m); // the card was never asked: nothing to verify
    }
    if let Err(e) = &result {
        if card_busy(&e.message()) {
            state.sim_identity.agent_switch_disarm();
            return retry_while_busy(state, iccid, previous, list, e.message());
        }
    }
    let inner = &state.esim.inner;
    let now = unix_now();
    // Even a failed or timed-out enable may have switched the card: the background check reads
    // the card once the SIM settles and reports what actually happened.
    let generation = {
        let mut inv = inner.inventory.lock().unwrap();
        inv.record(list, now);
        inv.generation += 1;
        inv.reloading = true;
        inv.generation
    };
    *inner.switch.lock().unwrap() = Switch {
        state: "reloading",
        target: iccid.clone(),
        previous: previous.clone(),
        generation,
        started_at: now,
        note: result.as_ref().err().map(|e| format!("enable error: {}", e.message())),
        ..Switch::default()
    };
    inner.switched_at.store(mono_secs(), Ordering::Relaxed);
    state.sim_identity.agent_switch_started(generation, previous.as_deref());
    state
        .cache
        .invalidate_with_settle(cache::SIM_INFO, cache::SETTLE_WINDOW);
    let worker = Arc::clone(inner);
    let target = iccid.clone();
    if crate::system::try_spawn_named("esim-switch", move || verify_switch(worker, target, generation)).is_err() {
        let mut inv = inner.inventory.lock().unwrap();
        inv.reloading = false;
        inner.switch.lock().unwrap().finish("card_unconfirmed", now);
    }
    match result {
        Ok(_) => {
            eprintln!(
                "[esim] enabled profile {} (was {})",
                mask(&iccid),
                previous.as_deref().map(mask).unwrap_or_else(|| "none".into())
            );
            (
                200,
                json!({"ok": true, "data": {
                    "enabled_iccid": iccid,
                    "previous_iccid": previous,
                    "reconnecting": true,
                    "generation": generation,
                }}),
            )
        }
        Err(e) => {
            eprintln!("[esim] enable {} uncertain: {}", mask(&iccid), e.message());
            let (status, mut body) = e.reply();
            body["outcome"] = json!("unknown");
            body["hint"] = json!("the agent is checking whether the switch happened: GET /api/esim/switch");
            (status, body)
        }
    }
}

/// The card answered the enable with a refusal that passes: SGP.22 catBusy (5, its SIM Toolkit
/// is in a proactive session), undefinedError (127), or a code without a name. Nothing
/// changed; seen with a roaming travel profile enabled, accepted again a couple of minutes later.
fn card_busy(message: &str) -> bool {
    message.contains("es10c_enable_profile")
        && ["catBusy", "undefinedError", "unknown result"].iter().any(|r| message.contains(r))
}

/// Answers a busy refusal with 202 and keeps trying in the background (`card_busy`), so the user
/// doesn't have to. The SIM didn't reload, so the saved list stays current.
fn retry_while_busy(
    state: &AppState,
    iccid: String,
    previous: Option<String>,
    list: Vec<Value>,
    message: String,
) -> (u16, Value) {
    let inner = &state.esim.inner;
    let now = unix_now();
    let generation = {
        let mut inv = inner.inventory.lock().unwrap();
        inv.record(list, now);
        inv.generation += 1;
        inv.generation
    };
    *inner.switch.lock().unwrap() = Switch {
        state: "card_busy",
        target: iccid.clone(),
        previous: previous.clone(),
        generation,
        started_at: now,
        note: Some(format!("the eSIM refused while busy ({message}); retrying")),
        ..Switch::default()
    };
    eprintln!("[esim] enable {} refused while busy ({message}); retrying", mask(&iccid));
    let worker = Arc::clone(inner);
    let cache = Arc::clone(&state.cache);
    let identity = Arc::clone(&state.sim_identity);
    let target = iccid.clone();
    let outgoing = previous.clone();
    let spawned = crate::system::try_spawn_named("esim-switch", move || {
        retry_enable(worker, target, outgoing, cache, identity, generation);
    });
    if spawned.is_err() {
        let mut sw = inner.switch.lock().unwrap();
        sw.reason = Some("card_busy");
        sw.finish("not_applied", now);
    }
    (
        202,
        json!({"ok": true, "data": {
            "enabled_iccid": null,
            "previous_iccid": previous,
            "reconnecting": false,
            "retrying": true,
            "generation": generation,
        }}),
    )
}

/// Retries a busy-refused enable every `CARD_BUSY_GAP` for `CARD_BUSY_LIMIT`, then verifies the
/// switch like a first-time success; a failure other than "busy" may still have switched the
/// card, so it is verified too.
fn retry_enable(
    inner: Arc<Inner>,
    target: String,
    previous: Option<String>,
    cache: Arc<crate::cache::UbusCache>,
    identity: Arc<crate::sim_identity::SimIdentity>,
    generation: u64,
) {
    let started = Instant::now();
    let mut attempt = 1;
    let mut last = String::new();
    while started.elapsed() < CARD_BUSY_LIMIT {
        std::thread::sleep(CARD_BUSY_GAP);
        attempt += 1;
        let result = {
            let Some(_guard) = acquire_soon(&inner) else { continue };
            identity.agent_switch_arm();
            card(&["profile", "enable", &target, "1"], CARD_LIMIT)
        };
        if !current(&inner, generation) {
            identity.agent_switch_disarm();
            return; // another switch took over
        }
        let note = match result {
            Ok(_) => format!("the eSIM was busy; switched on attempt {attempt}"),
            Err(CardError::NotStarted(m)) => {
                identity.agent_switch_disarm();
                last = m;
                continue;
            }
            Err(e) if card_busy(&e.message()) => {
                identity.agent_switch_disarm();
                last = e.message();
                continue;
            }
            Err(e) => format!("enable error: {}", e.message()),
        };
        inner.inventory.lock().unwrap().reloading = true;
        inner.switched_at.store(mono_secs(), Ordering::Relaxed);
        identity.agent_switch_started(generation, previous.as_deref());
        cache.invalidate_with_settle(cache::SIM_INFO, cache::SETTLE_WINDOW);
        {
            let mut sw = inner.switch.lock().unwrap();
            sw.state = "reloading";
            sw.note = Some(note);
        }
        eprintln!("[esim] enable {} accepted on attempt {attempt}", mask(&target));
        verify_switch(inner, target, generation);
        return;
    }
    let mut sw = inner.switch.lock().unwrap();
    if sw.generation != generation {
        return;
    }
    sw.reason = Some("card_busy");
    sw.note = Some(format!("the eSIM stayed busy for {} s ({last})", CARD_BUSY_LIMIT.as_secs()));
    sw.finish("not_applied", unix_now());
    eprintln!("[esim] switch to {} not applied: the eSIM stayed busy", mask(&target));
}

/// Whether `generation` is still the switch being reported (a newer switch supersedes it).
fn current(inner: &Arc<Inner>, generation: u64) -> bool {
    inner.switch.lock().unwrap().generation == generation
}

/// Background check after `enable`: waits for the SIM to reload, re-reads the card (retrying
/// while the management application is not selectable), then checks that the modem loaded the
/// new identity and that data connects. Writes only while `generation` is still the current
/// switch, and never holds the switch lock across a ubus call.
fn verify_switch(inner: Arc<Inner>, target: String, generation: u64) {
    let started = Instant::now();
    std::thread::sleep(RELOAD_SETTLE);
    let mut list = None;
    let mut last_error = String::from("the SIM did not report ready");
    while started.elapsed() < CARD_CONFIRM_LIMIT {
        if !current(&inner, generation) {
            return;
        }
        if sim_ready() != Some(false) {
            if let Some(_guard) = acquire(&inner) {
                match card(&["profile", "list"], CARD_LIMIT) {
                    Ok(data) => {
                        list = Some(profile_views(&data));
                        break;
                    }
                    Err(e) => last_error = e.message(),
                }
            }
        }
        std::thread::sleep(SWITCH_POLL);
    }
    let now = unix_now();
    let modem_has_target = || modem_iccid().is_some_and(|m| same_iccid(&m, &target));
    match list {
        Some(list) => {
            let enabled = enabled_iccid(&list);
            {
                let mut inv = inner.inventory.lock().unwrap();
                inv.reloading = false;
                inv.record(list, now);
            }
            if enabled.as_deref() != Some(target.as_str()) {
                eprintln!("[esim] switch check: {} is not enabled on the card", mask(&target));
                let mut sw = inner.switch.lock().unwrap();
                if sw.generation == generation {
                    sw.finish("not_applied", now);
                }
                return;
            }
            let mut sw = inner.switch.lock().unwrap();
            if sw.generation != generation {
                return;
            }
            sw.card_confirmed_at = now;
            sw.state = "card_confirmed";
        }
        None => {
            {
                let mut inv = inner.inventory.lock().unwrap();
                inv.reloading = false;
                inv.error = Some(ReadError { kind: "management_unavailable", message: last_error, at: now });
            }
            // The card stayed unreadable, but the modem loading the target is proof enough to
            // keep checking data; otherwise report it and still look after the receipts.
            if !modem_has_target() {
                eprintln!("[esim] switch check: card unreadable for {} s", CARD_CONFIRM_LIMIT.as_secs());
                let mut sw = inner.switch.lock().unwrap();
                if sw.generation != generation {
                    return;
                }
                sw.finish("card_unconfirmed", now);
                drop(sw);
                receipts_after_switch(&inner, false);
                return;
            }
            eprintln!("[esim] switch check: card unreadable, but the modem loaded {}", mask(&target));
        }
    }
    let connected = loop {
        // Ask the firmware first, then publish under the lock.
        let modem_ok = modem_has_target();
        let data_ok = data_connected();
        let out_of_time = started.elapsed() >= SWITCH_VERIFY_LIMIT;
        let blocker = if out_of_time { data_blocker() } else { None };
        let now = unix_now();
        let mut sw = inner.switch.lock().unwrap();
        if sw.generation != generation {
            return;
        }
        if sw.modem_confirmed_at == 0 && modem_ok {
            sw.modem_confirmed_at = now;
            sw.state = "modem_confirmed";
        }
        if sw.modem_confirmed_at != 0 && data_ok {
            sw.connected_at = now;
            sw.finish("connected", now);
            eprintln!("[esim] switch to {} verified: card, modem, data", mask(&target));
            break true;
        }
        if out_of_time {
            let outcome = if sw.modem_confirmed_at != 0 { "no_data" } else { "modem_unconfirmed" };
            if outcome == "no_data" {
                sw.reason = blocker;
            }
            sw.finish(outcome, now);
            eprintln!("[esim] switch to {} ended as {outcome}", mask(&target));
            break false;
        }
        drop(sw);
        std::thread::sleep(SWITCH_POLL);
    };
    receipts_after_switch(&inner, connected);
}

/// The switch left an enable and a disable receipt on the card. With data up, send them once
/// traffic flows; otherwise list them so the count is right and leave them to the retry loop.
fn receipts_after_switch(inner: &Arc<Inner>, connected: bool) {
    if !connected {
        if let Some(_guard) = acquire_soon(inner) {
            let _ = list_receipts(inner);
        }
        ensure_receipt_retry(inner);
        return;
    }
    // The modem reports data before the router has its default route, DNS and firewall.
    wait_for_default_route(WAN_READY_LIMIT);
    std::thread::sleep(RECEIPT_AFTER_SWITCH);
    if inner.inventory.lock().unwrap().reloading {
        // A newer switch is reloading the SIM; its own check sends everything pending.
        ensure_receipt_retry(inner);
        return;
    }
    let sent_all = acquire_soon(inner).is_some_and(|_guard| send_receipts(inner));
    // Right after the attach a request can still stall: one more try before the 10-minute loop.
    if !sent_all {
        std::thread::sleep(RECEIPT_SECOND_TRY);
        if data_connected() {
            if let Some(_guard) = acquire_soon(inner) {
                send_receipts(inner);
            }
        }
    }
    ensure_receipt_retry(inner);
}

/// Waits until the kernel has an IPv4 default route (or `limit` passes).
fn wait_for_default_route(limit: Duration) {
    let started = Instant::now();
    while started.elapsed() < limit {
        if std::fs::read_to_string("/proc/net/route").is_ok_and(|t| has_default_route(&t)) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// `/proc/net/route` has a row with destination 00000000 that is up (flag 0x1).
pub(crate) fn has_default_route(table: &str) -> bool {
    table.lines().skip(1).any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() > 3 && f[1] == "00000000" && u32::from_str_radix(f[3], 16).is_ok_and(|flags| flags & 1 == 1)
    })
}

/// Background work takes the card when it's free within 10 s; the app usually re-reads the
/// list as a switch completes.
fn acquire_soon(inner: &Arc<Inner>) -> Option<BusyGuard> {
    (0..10).find_map(|i| {
        if i > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        acquire(inner)
    })
}

/// `Some(true)` when the modem reports the SIM ready; `None` if the firmware did not answer.
fn sim_ready() -> Option<bool> {
    let info = crate::ubus::call("zwrt_zte_mdm.api", "get_sim_info", Some("{}")).ok()?;
    sim_states_ready(&info)
}

/// `get_sim_info` `sim_states` says the SIM is ready.
pub(crate) fn sim_states_ready(info: &Value) -> Option<bool> {
    let s = info["sim_states"].as_str()?.to_ascii_lowercase();
    Some(s.contains("ready") && !s.contains("not"))
}

/// ICCID of the subscription the modem has loaded.
fn modem_iccid() -> Option<String> {
    let info = crate::ubus::call("zwrt_zte_mdm.api", "get_sim_info", Some("{}")).ok()?;
    info["sim_iccid"].as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn data_connected() -> bool {
    crate::ubus::call(
        "zwrt_data",
        "get_wwaniface",
        Some(r#"{"source_module":"zte_topsw_data","cid":1}"#),
    )
    .ok()
    .and_then(|v| v["connect_status"].as_str().map(str::to_string))
    .is_some_and(|s| s.contains("connected") && !s.contains("disconnected"))
}

/// Why data is down when the firmware says so: a profile from another country (SIM and network
/// MCC differ, as with travel eSIMs) while data roaming is off, or mobile data switched off.
pub(crate) fn data_blocker() -> Option<&'static str> {
    let wwan = crate::ubus::call(
        "zwrt_data",
        "get_wwaniface",
        Some(r#"{"source_module":"zte_topsw_data","cid":1}"#),
    )
    .ok()?;
    let sim_mcc = crate::ubus::call("zwrt_zte_mdm.api", "get_sim_info", Some("{}"))
        .ok()
        .and_then(|v| v["mdm_mcc"].as_str().map(str::to_string));
    let net_mcc = crate::ubus::call("zte_nwinfo_api", "nwinfo_get_netinfo", None)
        .ok()
        .map(|v| v["rmcc"].to_string().trim_matches('"').to_string());
    blocker(&wwan, sim_mcc.as_deref(), net_mcc.as_deref())
}

fn blocker(wwan: &Value, sim_mcc: Option<&str>, net_mcc: Option<&str>) -> Option<&'static str> {
    let flag = |k: &str| wwan[k].as_i64().or_else(|| wwan[k].as_str().and_then(|s| s.parse().ok()));
    let abroad = matches!((sim_mcc, net_mcc), (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() && b != "null" && a.trim() != b.trim());
    if abroad && flag("roam_enable") == Some(0) {
        Some("roaming_off")
    } else if flag("enable") == Some(0) {
        Some("data_off")
    } else {
        None
    }
}

/// A switch while the user parked the modem would change the card but never attach. When the
/// no-service policy parked it (`lpm` / `probing`), switching to another network is the way out,
/// and the firmware's SIM reload brings the radio back online. Unknown mode: allowed.
fn radio_blocks_switch(operate_mode: Option<&str>, policy_state: Option<&str>) -> bool {
    let online = operate_mode.is_none_or(|m| m.trim().eq_ignore_ascii_case("ONLINE"));
    !online && !matches!(policy_state, Some("lpm" | "probing"))
}

/// The firmware and the card may differ in a trailing filler `F`.
pub(crate) fn same_iccid(a: &str, b: &str) -> bool {
    let trim = |s: &str| s.trim().trim_end_matches(['F', 'f']).to_string();
    !a.trim().is_empty() && trim(a) == trim(b)
}

/// POST /api/esim/delete — body `{"iccid": "..."}`. Permanently removes a disabled profile.
pub fn delete(state: &AppState, body: &[u8]) -> (u16, Value) {
    if !state.auth.has_password() {
        return error(403, "agent password required");
    }
    let iccid = match iccid_body(body) {
        Ok(i) => i,
        Err(reply) => return reply,
    };
    if state.esim.reloading() {
        return reloading_reply();
    }
    let Some(_guard) = state.esim.acquire() else {
        return busy();
    };
    if let Err(reply) = check_card(&state.esim.inner) {
        return reply;
    }
    let list = match card(&["profile", "list"], CARD_LIMIT) {
        Ok(data) => profile_views(&data),
        Err(e) => return e.reply(),
    };
    if let Err((status, message)) = check_delete_target(&list, &iccid) {
        return error(status, message);
    }
    if let Err(e) = card(&["profile", "delete", &iccid], CARD_LIMIT) {
        return e.reply();
    }
    eprintln!("[esim] deleted profile {}", mask(&iccid));
    {
        let mut inv = state.esim.inner.inventory.lock().unwrap();
        let remaining = list.into_iter().filter(|p| p["iccid"] != iccid.as_str()).collect();
        inv.record(remaining, unix_now());
        inv.generation += 1;
    }
    let notified = send_receipts(&state.esim.inner);
    ensure_receipt_retry(&state.esim.inner);
    (
        200,
        json!({"ok": true, "data": {"deleted_iccid": iccid, "operator_notified": notified}}),
    )
}

/// POST /api/esim/download — body `{"activation_code": "LPA:1$...", "confirmation_code": "..."}`.
/// Starts the download in the background (it can take a minute); poll GET /api/esim/download.
pub fn download_start(state: &AppState, body: &[u8]) -> (u16, Value) {
    if !state.auth.has_password() {
        return error(403, "agent password required");
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return error(400, "invalid JSON"),
    };
    let code = parsed["activation_code"].as_str().unwrap_or("").trim();
    let preview = parsed["preview"].as_bool().unwrap_or(true);
    let confirmation = parsed["confirmation_code"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let needs_confirmation = match check_activation_code(code) {
        Ok(needs) => needs,
        Err(message) => return error(400, message),
    };
    if let Some(c) = confirmation {
        if c.len() > 64 || c.chars().any(|ch| ch.is_control()) {
            return error(400, "confirmation code is too long or contains control characters");
        }
    } else if needs_confirmation {
        return error(400, "this activation code needs a confirmation code from the operator");
    }
    if state.esim.reloading() {
        return reloading_reply();
    }
    if !data_connected() {
        return error(
            409,
            "the router has no mobile data connection; downloads use the router's own internet",
        );
    }
    let Some(guard) = state.esim.acquire() else {
        return busy();
    };
    let free_nvm = match check_card(&state.esim.inner) {
        Ok(chip) => chip["EUICCInfo2"]["extCardResource"]["freeNonVolatileMemory"].clone(),
        Err(reply) => return reply,
    };
    let inner = Arc::clone(&state.esim.inner);
    *inner.download.lock().unwrap() = Download {
        state: "running",
        step: "starting".into(),
        started_at: unix_now(),
        ..Download::default()
    };
    let code = code.to_string();
    let confirmation = confirmation.map(str::to_string);
    let spawned = crate::system::try_spawn_named("esim-download", move || {
        let _guard = guard;
        let result = download(
            &code,
            confirmation.as_deref(),
            preview,
            DOWNLOAD_LIMIT,
            &|step, data| {
                let asks = {
                    let mut d = inner.download.lock().unwrap();
                    d.progress(step, data);
                    d.state == "confirm"
                };
                if asks {
                    arm_preview_timeout(&inner);
                }
            },
            &inner.control,
        );
        // An install that started leaves an installation-result receipt, failed or not.
        let installing = result.is_ok() || installing_step(&inner.download.lock().unwrap().step);
        let notified = installing && send_receipts(&inner);
        if installing {
            inner.inventory.lock().unwrap().generation += 1;
            ensure_receipt_retry(&inner);
        }
        let mut d = inner.download.lock().unwrap();
        d.finished_at = unix_now();
        match result {
            Ok(data) => {
                d.state = "done";
                d.error = None; // a Cancel that came too late to stop the install
                d.iccid = data["iccid"].as_str().map(str::to_string);
                eprintln!(
                    "[esim] downloaded profile {} (operator notified: {notified})",
                    d.iccid.as_deref().map(mask).unwrap_or_else(|| "?".into())
                );
            }
            // The download says itself whether it stopped on a user decision.
            Err(e) if e.message().starts_with("cancelled") => {
                d.state = "cancelled";
                d.error = d.error.take().or_else(|| Some(e.message()));
                eprintln!("[esim] download cancelled at {}", d.step);
            }
            Err(e) => {
                d.state = "failed";
                d.error = Some(e.message());
                eprintln!("[esim] download failed at {}", d.step);
            }
        }
    });
    if spawned.is_err() {
        let mut d = state.esim.inner.download.lock().unwrap();
        d.state = "failed";
        d.error = Some("cannot start the download thread".into());
        return error(503, "cannot start the download thread");
    }
    let mut view = state.esim.inner.download.lock().unwrap().view();
    view["free_nvm_bytes"] = free_nvm;
    (202, json!({"ok": true, "data": view}))
}

/// POST /api/esim/download/confirm — body `{"accept": true|false}`, while the state is "confirm".
pub fn download_confirm(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return error(400, "invalid JSON"),
    };
    let Some(accept) = parsed["accept"].as_bool() else {
        return error(400, "accept must be true or false");
    };
    let inner = &state.esim.inner;
    let mut d = inner.download.lock().unwrap();
    if d.state != "confirm" {
        return error(409, "no download is waiting for confirmation");
    }
    if !inner.control.send(if accept { DownloadCtl::Accept } else { DownloadCtl::Decline }) {
        return error(409, "the download is no longer waiting");
    }
    if accept {
        d.state = "running";
    } else {
        d.state = "cancelling";
        d.error = Some("declined".into());
    }
    (200, json!({"ok": true, "data": d.view()}))
}

/// The install step (a prefix, so a sub-step such as `…:result` counts too).
fn installing_step(step: &str) -> bool {
    step.starts_with("es10b_load_bound_profile_package")
}

/// POST /api/esim/download/cancel — stops a running download at its next step; an ES9+ request
/// already running ends within its timeout. At the preview it postpones, so the QR code stays
/// usable.
pub fn download_cancel(state: &AppState) -> (u16, Value) {
    let inner = &state.esim.inner;
    let mut d = inner.download.lock().unwrap();
    if d.state == "running" && installing_step(&d.step) {
        return error(409, "the profile is being installed and can't be cancelled now");
    }
    if !matches!(d.state, "confirm" | "running") {
        return error(409, "no download is running");
    }
    if !inner.control.send(DownloadCtl::Cancel) {
        return error(409, "the download cannot be cancelled now");
    }
    d.state = "cancelling";
    d.error = Some("cancelled".into());
    (200, json!({"ok": true, "data": d.view()}))
}

/// Cancels the preview (reason timeout) if the user has not answered within `PREVIEW_LIMIT`.
fn arm_preview_timeout(inner: &Arc<Inner>) {
    let inner = Arc::clone(inner);
    let started = inner.download.lock().unwrap().started_at;
    let _ = crate::system::try_spawn_named("esim-preview", move || {
        std::thread::sleep(PREVIEW_LIMIT);
        let mut d = inner.download.lock().unwrap();
        if d.state == "confirm" && d.started_at == started && inner.control.send(DownloadCtl::PreviewTimeout) {
            d.state = "cancelling";
            d.error = Some("not confirmed within 2 minutes".into());
        }
    });
}

/// GET /api/esim/download — state of the last download (a finished result for 10 minutes).
pub fn download_status(state: &AppState) -> (u16, Value) {
    let mut d = state.esim.inner.download.lock().unwrap();
    d.expire(unix_now());
    (200, json!({"ok": true, "data": d.view()}))
}

/// DELETE /api/esim/download — dismisses a finished result (the app's "close" on the result card).
pub fn download_dismiss(state: &AppState) -> (u16, Value) {
    let mut d = state.esim.inner.download.lock().unwrap();
    if !d.is_finished() && !d.state.is_empty() && d.state != "idle" {
        return error(409, "a download is still in progress");
    }
    *d = Download::default();
    (200, json!({"ok": true, "data": d.view()}))
}

/// Lists the receipts waiting on the card (guard held) and merges them into the saved state.
fn list_receipts(inner: &Arc<Inner>) -> Result<usize, String> {
    let data = card(&["notification", "list"], CARD_LIMIT).map_err(|e| e.message())?;
    let fresh = receipt_views(&data);
    let n = fresh.len();
    inner.receipts.lock().unwrap().merge(fresh, unix_now());
    Ok(n)
}

/// Sends each pending receipt on its own (guard held) and removes the ones the operator's
/// server accepted. Returns true when nothing is left pending. Without internet they stay on
/// the card for the retry loop.
fn send_receipts(inner: &Arc<Inner>) -> bool {
    if list_receipts(inner).is_err() {
        return false;
    }
    let now = unix_now();
    let todo: Vec<(u64, String)> = {
        let mut r = inner.receipts.lock().unwrap();
        r.last_attempt_at = now;
        r.pending
            .iter()
            .flatten()
            .filter(|x| x.attempts < RECEIPT_MAX_ATTEMPTS)
            .map(|x| (x.seq, x.address.clone()))
            .collect()
    };
    // A server that failed (or hung until the limit) is skipped for the rest of this round, so
    // one unreachable operator can't hold the card for 45 s per receipt.
    let mut failed_servers = std::collections::HashSet::new();
    let mut sent = 0;
    for (seq, address) in todo {
        if failed_servers.contains(&address) {
            continue;
        }
        let arg = seq.to_string();
        let result = card(&["notification", "process", "-r", &arg], RECEIPT_LIMIT);
        if let Err(CardError::NotStarted(m)) = &result {
            // Nothing was attempted: stop the round without counting it against the receipt.
            eprintln!("[esim] operator receipts postponed: {m}");
            break;
        }
        let mut r = inner.receipts.lock().unwrap();
        match result {
            Ok(_) => {
                if let Some(p) = r.pending.as_mut() {
                    p.retain(|x| x.seq != seq);
                }
                r.last_sent_at = unix_now();
                sent += 1;
            }
            Err(e) => {
                if let Some(x) = r.pending.iter_mut().flatten().find(|x| x.seq == seq) {
                    x.attempts += 1;
                    x.last_error = Some(e.message());
                }
                eprintln!("[esim] operator receipt {seq} to {address} not sent: {}", e.message());
                failed_servers.insert(address);
            }
        }
    }
    if sent > 0 {
        eprintln!("[esim] sent {sent} operator receipt(s)");
    }
    let _ = list_receipts(inner);
    inner.receipts.lock().unwrap().pending.as_ref().is_some_and(Vec::is_empty)
}

/// Starts the background retry if receipts are waiting and no retry loop runs. It tries every
/// `RECEIPT_RETRY` while the router has mobile data and stops when nothing retryable is left.
fn ensure_receipt_retry(inner: &Arc<Inner>) {
    {
        let mut r = inner.receipts.lock().unwrap();
        if r.retrying || !r.retryable() {
            return;
        }
        r.retrying = true;
    }
    let worker = Arc::clone(inner);
    let spawned = crate::system::try_spawn_named("esim-receipts", move || {
        for _ in 0..RECEIPT_MAX_ROUNDS {
            std::thread::sleep(RECEIPT_RETRY);
            if !worker.inventory.lock().unwrap().reloading && data_connected() {
                if let Some(_guard) = acquire(&worker) {
                    send_receipts(&worker);
                }
            }
            if !worker.receipts.lock().unwrap().retryable() {
                break;
            }
        }
        worker.receipts.lock().unwrap().retrying = false;
    });
    if spawned.is_err() {
        inner.receipts.lock().unwrap().retrying = false;
    }
}

/// GET /api/esim/notifications — receipts waiting on the card for the operators' servers.
pub fn notifications(state: &AppState) -> (u16, Value) {
    let inner = &state.esim.inner;
    if state.esim.reloading() {
        return (200, json!({"ok": true, "data": inner.receipts.lock().unwrap().view(true)}));
    }
    let Some(_guard) = state.esim.acquire() else {
        return (200, json!({"ok": true, "data": inner.receipts.lock().unwrap().view(true)}));
    };
    let stale = list_receipts(inner).is_err();
    (200, json!({"ok": true, "data": inner.receipts.lock().unwrap().view(stale)}))
}

/// POST /api/esim/notifications/process — sends pending receipts now (in the background) and
/// restarts the retry loop, including receipts that had given up.
pub fn notifications_process(state: &AppState) -> (u16, Value) {
    if !state.auth.has_password() {
        return error(403, "agent password required");
    }
    if state.esim.reloading() {
        return reloading_reply();
    }
    if !data_connected() {
        return error(409, "the router has no mobile data connection");
    }
    let Some(guard) = state.esim.acquire() else {
        return busy();
    };
    let inner = Arc::clone(&state.esim.inner);
    if let Some(p) = inner.receipts.lock().unwrap().pending.as_mut() {
        for x in p.iter_mut() {
            x.attempts = 0;
        }
    }
    let worker = Arc::clone(&inner);
    let spawned = crate::system::try_spawn_named("esim-receipts-now", move || {
        let _guard = guard;
        send_receipts(&worker);
        drop(_guard);
        ensure_receipt_retry(&worker);
    });
    if spawned.is_err() {
        return error(503, "cannot start the receipt thread");
    }
    (202, json!({"ok": true, "data": inner.receipts.lock().unwrap().view(true)}))
}

/// POST /api/esim/nickname — body `{"iccid": "...", "nickname": "..."}`; an empty nickname
/// clears it.
pub fn nickname(state: &AppState, body: &[u8]) -> (u16, Value) {
    if !state.auth.has_password() {
        return error(403, "agent password required");
    }
    let iccid = match iccid_body(body) {
        Ok(i) => i,
        Err(reply) => return reply,
    };
    let parsed: Value = serde_json::from_slice(body).unwrap_or_default();
    let Some(raw) = parsed.get("nickname").and_then(Value::as_str) else {
        return error(400, "nickname must be a string (\"\" clears it)");
    };
    let name = raw.trim().to_string();
    if let Err(message) = check_nickname(&name) {
        return error(400, message);
    }
    if state.esim.reloading() {
        return reloading_reply();
    }
    let Some(_guard) = state.esim.acquire() else {
        return busy();
    };
    if let Err(reply) = check_card(&state.esim.inner) {
        return reply;
    }
    let list = match card(&["profile", "list"], CARD_LIMIT) {
        Ok(data) => profile_views(&data),
        Err(e) => return e.reply(),
    };
    if !list.iter().any(|p| p["iccid"] == iccid.as_str()) {
        return error(404, "no profile with this ICCID on the eSIM");
    }
    let mut args = vec!["profile", "nickname", iccid.as_str()];
    if !name.is_empty() {
        args.push(name.as_str());
    }
    if let Err(e) = card(&args, CARD_LIMIT) {
        return e.reply();
    }
    let updated: Vec<Value> = list
        .into_iter()
        .map(|mut p| {
            if p["iccid"] == iccid.as_str() {
                p["nickname"] = if name.is_empty() { Value::Null } else { json!(name) };
            }
            p
        })
        .collect();
    let generation = {
        let mut inv = state.esim.inner.inventory.lock().unwrap();
        inv.record(updated, unix_now());
        inv.generation += 1;
        inv.generation
    };
    eprintln!("[esim] renamed profile {}", mask(&iccid));
    (200, json!({"ok": true, "data": {"iccid": iccid, "nickname": name, "generation": generation}}))
}

fn check_nickname(name: &str) -> Result<(), &'static str> {
    if name.len() > NICKNAME_MAX_BYTES {
        return Err("nickname is longer than 64 bytes");
    }
    if name.chars().any(char::is_control) {
        return Err("nickname contains control characters");
    }
    Ok(())
}

fn enabled_iccid(list: &[Value]) -> Option<String> {
    list.iter()
        .find(|p| p["state"] == "enabled")
        .and_then(|p| p["iccid"].as_str())
        .map(str::to_string)
}

/// The profile must exist, be disabled and be an ordinary operator profile.
fn check_enable_target(list: &[Value], iccid: &str) -> Result<(), (u16, &'static str)> {
    let Some(target) = list.iter().find(|p| p["iccid"] == iccid) else {
        return Err((404, "no profile with this ICCID on the eSIM"));
    };
    if target["state"] != "disabled" {
        return Err((409, "this profile is already enabled"));
    }
    if target["class"] != "operational" {
        return Err((400, "only operational profiles can be enabled here"));
    }
    Ok(())
}

/// Deleting is permanent; never the profile the router is using.
fn check_delete_target(list: &[Value], iccid: &str) -> Result<(), (u16, &'static str)> {
    let Some(target) = list.iter().find(|p| p["iccid"] == iccid) else {
        return Err((404, "no profile with this ICCID on the eSIM"));
    };
    if target["state"] != "disabled" {
        return Err((409, "switch to another profile before deleting the active one"));
    }
    Ok(())
}

/// `LPA:1$<SM-DP+ address>$<matching ID>[$<OID>[$<confirmation code required flag>]]`
/// (SGP.22 §4.1). Returns whether the code says a confirmation code is required.
fn check_activation_code(code: &str) -> Result<bool, &'static str> {
    const BAD: &str = "activation code must look like LPA:1$<server>$<matching ID>";
    if code.len() > 512 || code.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(BAD);
    }
    let Some(rest) = code.get(..4).filter(|p| p.eq_ignore_ascii_case("LPA:")).map(|_| &code[4..]) else {
        return Err(BAD);
    };
    let parts: Vec<&str> = rest.split('$').collect();
    if parts.len() < 3 || parts[0] != "1" {
        return Err(BAD);
    }
    let server_ok = !parts[1].is_empty()
        && parts[1]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'));
    if !server_ok || parts[1].len() > 255 {
        return Err("the activation code's server address is invalid");
    }
    // Matching IDs use only letters, digits and '-' (SGP.22 section 4.1).
    if !parts[2].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err("the activation code's matching ID is invalid");
    }
    Ok(parts.get(4) == Some(&"1"))
}

/// The card's form of an ICCID: digits only. The modem pads an odd-length ICCID with `F`, which
/// is accepted and dropped (a 20-digit ICCID never has one).
fn normalize_iccid(s: &str) -> Option<String> {
    let digits = s.strip_suffix(['F', 'f']).unwrap_or(s);
    let range = if digits.len() == s.len() { 18..=20 } else { 18..=19 };
    (range.contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.to_string())
}

/// Last four digits only, for logs.
fn mask(iccid: &str) -> String {
    let tail: String = iccid.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{tail}")
}

// ---------------------------------------------------------------------------
// Card command results → API views
// ---------------------------------------------------------------------------

/// Keeps the fields the app needs; drops icons (base64 images, often several KB).
fn profile_views(data: &Value) -> Vec<Value> {
    data.as_array()
        .map(|items| {
            items
                .iter()
                .map(|p| {
                    json!({
                        "iccid": p["iccid"],
                        "isdp_aid": p["isdpAid"],
                        "state": p["profileState"],
                        "nickname": p["profileNickname"],
                        "provider": p["serviceProviderName"],
                        "name": p["profileName"],
                        // SGP.22 ProfileClass DEFAULT operational: an absent tag means operational.
                        "class": p["profileClass"].as_str().unwrap_or("operational"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn chip_view(data: &Value) -> Value {
    let info = &data["EUICCInfo2"];
    json!({
        "eid": data["eidValue"],
        "firmware": info["euiccFirmwareVer"],
        "sgp22_version": info["svn"],
        "profile_version": info["profileVersion"],
        "free_nvm_bytes": info["extCardResource"]["freeNonVolatileMemory"],
        "free_ram_bytes": info["extCardResource"]["freeVolatileMemory"],
    })
}

// ---------------------------------------------------------------------------
// Running card commands
// ---------------------------------------------------------------------------

enum CardError {
    /// `ZTE_AGENT_ESIM_QRTR_NODE` or `ZTE_AGENT_ESIM_UIM_SLOT` is invalid.
    Config(String),
    Failed(String),
    /// The command never reached the card (an earlier one still holds it, or no worker thread):
    /// not a card outcome.
    NotStarted(String),
}

impl CardError {
    fn message(&self) -> String {
        match self {
            CardError::Config(m) | CardError::Failed(m) | CardError::NotStarted(m) => m.clone(),
        }
    }

    fn reply(self) -> (u16, Value) {
        match self {
            CardError::Config(m) => error(503, &m),
            CardError::Failed(m) => error(502, &m),
            CardError::NotStarted(m) => error(409, &m),
        }
    }
}

/// The card lock (`ZTE_AGENT_ESIM_LOCK`, default /tmp/esim.lock), shared with the CLI.
pub(crate) fn lock_path() -> PathBuf {
    PathBuf::from(std::env::var("ZTE_AGENT_ESIM_LOCK").unwrap_or_else(|_| DEFAULT_LOCK_PATH.into()))
}

/// QRTR node of the modem's UIM service and the SIM slot holding the eSIM
/// (`ZTE_AGENT_ESIM_QRTR_NODE`, default 3; `ZTE_AGENT_ESIM_UIM_SLOT`, default 1).
pub(crate) fn card_target() -> Result<(u32, u8), String> {
    let number = |name: &str, default: &str| {
        let v = std::env::var(name).unwrap_or_else(|_| default.into());
        Some(v).filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())).and_then(|v| v.parse::<u32>().ok())
    };
    let node = number("ZTE_AGENT_ESIM_QRTR_NODE", "3").ok_or("ZTE_AGENT_ESIM_QRTR_NODE must be a number")?;
    let slot = number("ZTE_AGENT_ESIM_UIM_SLOT", "1")
        .and_then(|s| u8::try_from(s).ok())
        .filter(|s| *s >= 1)
        .ok_or("ZTE_AGENT_ESIM_UIM_SLOT must be 1-255")?;
    Ok((node, slot))
}

/// Runs a profile download (`esim_download`) on the caller's (download) thread, which holds the
/// card for the whole session. `progress(step, data)` gets each step; decisions arrive through
/// `control`.
fn download(
    code: &str,
    confirmation: Option<&str>,
    preview: bool,
    limit: Duration,
    progress: &dyn Fn(&str, &Value),
    control: &DownloadControl,
) -> Result<Value, CardError> {
    let (node, slot) = card_target().map_err(CardError::Config)?;
    // A card command that outlived its limit may still hold the card: nothing else may start.
    if CARD_WORKER_RUNNING.load(Ordering::SeqCst) {
        return Err(CardError::NotStarted("the previous eSIM command is still finishing".into()));
    }
    let code = crate::esim_download::parse_activation_code(code).map_err(|e| CardError::Failed(e.to_string()))?;
    let (tx, rx) = std::sync::mpsc::channel();
    // Clears the control channel however the download ends.
    struct Clear<'a>(&'a DownloadControl);
    impl Drop for Clear<'_> {
        fn drop(&mut self) {
            *self.0.tx.lock().unwrap() = None;
        }
    }
    let _clear = Clear(control);
    *control.tx.lock().unwrap() = Some(tx);
    let deadline = Instant::now() + limit;
    // The card session outlives the procedure's deadline a little, so a stop at the deadline can
    // still cancel the card session and tell the server.
    let mut card = crate::esim_native::session(node, slot, deadline + CLEANUP_ALLOWANCE).map_err(CardError::Failed)?;
    let mut http = crate::esim_native::Es9pHttp { timeout: Duration::from_secs(30) };
    let mut report = |step: &str, data: Value| progress(step, &data);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::esim_download::download(&mut card, &mut http, &code, confirmation, preview, &rx, &mut report, deadline)
    }))
    .unwrap_or_else(|_| Err("the eSIM download stopped unexpectedly".into()))
    .map_err(CardError::Failed)
}

/// Time after a download's deadline for cancelling the sessions.
const CLEANUP_ALLOWANCE: Duration = Duration::from_secs(30);

/// Set while a card command's worker thread runs.
static CARD_WORKER_RUNNING: AtomicBool = AtomicBool::new(false);
/// Past the command's own limit before the caller stops waiting for a worker.
const WORKER_GRACE: Duration = Duration::from_secs(5);

/// Runs one card command (`esim_native::run`) on a worker thread, so a stalled modem (QRTR flow
/// control can block a send indefinitely) can't hold the caller past `limit`.
fn card(args: &[&str], limit: Duration) -> Result<Value, CardError> {
    crate::httpd::gate::debug_assert_may_block();
    let (node, slot) = card_target().map_err(CardError::Config)?;
    if CARD_WORKER_RUNNING.swap(true, Ordering::SeqCst) {
        return Err(CardError::NotStarted("the previous eSIM command is still finishing".into()));
    }
    // Clears the flag however the worker ends, panics included.
    struct Running;
    impl Drop for Running {
        fn drop(&mut self) {
            CARD_WORKER_RUNNING.store(false, Ordering::SeqCst);
        }
    }
    let running = Running;
    // The worker's own copy of the card lock: if the caller stops waiting, CLI runs stay locked
    // out until the worker really ends.
    let lock = HELD_LOCK.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|f| f.try_clone().ok());
    let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = crate::system::try_spawn_named("esim-card", move || {
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::esim_native::run(&refs, node, slot, limit)
        }))
        .unwrap_or_else(|_| Err("the eSIM worker stopped unexpectedly".into()));
        // The card session is closed: release the claims before the caller can start the next
        // command, so it never sees "still finishing".
        drop(lock);
        drop(running);
        let _ = tx.send(result);
    });
    if spawned.is_err() {
        // The closure (and with it `running`) was dropped, which cleared the flag.
        return Err(CardError::NotStarted("could not start the eSIM worker".into()));
    }
    let op = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
    match rx.recv_timeout(limit + WORKER_GRACE) {
        Ok(Ok(data)) => Ok(data),
        Ok(Err(e)) => {
            eprintln!("[esim] {op} failed: {e}");
            Err(CardError::Failed(e))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            eprintln!("[esim] {op}: worker stopped without an answer");
            Err(CardError::Failed("the eSIM worker stopped unexpectedly".into()))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            eprintln!("[esim] {op} did not finish within {} s", limit.as_secs());
            Err(CardError::Failed(format!("eSIM command did not finish within {} s", limit.as_secs())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"{"type":"lpa","payload":{"code":0,"message":"success","data":[{"iccid":"8963000000000000001","isdpAid":"A0000005591010FFFFFFFF8900001000","profileState":"enabled","profileNickname":"Smart","serviceProviderName":"SMART","profileName":"Operational","iconType":"png","icon":"iVBORw0KGgo=","profileClass":"operational"},{"iccid":"8963000000000000002","isdpAid":"A0000005591010FFFFFFFF8900001100","profileState":"disabled","profileNickname":null,"serviceProviderName":"GOMO","profileName":"Globe Philippines ODC","iconType":"none","icon":null,"profileClass":"operational"},{"iccid":"8963000000000000003","isdpAid":"A0000005591010FFFFFFFF8900001200","profileState":"disabled","profileNickname":null,"serviceProviderName":"Test","profileName":"Test","iconType":"none","icon":null,"profileClass":"test"}]}}"#;

    /// The data of a result line in lpac's format, which `esim_native` reproduces.
    fn data(line: &str) -> Value {
        serde_json::from_str::<Value>(line).unwrap()["payload"]["data"].clone()
    }

    fn list() -> Vec<Value> {
        profile_views(&data(LIST))
    }

    #[test]
    fn profile_list_maps_fields_and_drops_icons() {
        let p = list();
        assert_eq!(p.len(), 3);
        assert_eq!(p[0]["iccid"], "8963000000000000001");
        assert_eq!(p[0]["state"], "enabled");
        assert_eq!(p[0]["nickname"], "Smart");
        assert_eq!(p[1]["provider"], "GOMO");
        assert_eq!(p[1]["nickname"], Value::Null);
        assert!(p.iter().all(|v| v.get("icon").is_none()));
        assert_eq!(enabled_iccid(&p).as_deref(), Some("8963000000000000001"));
    }

    #[test]
    fn enable_target_must_be_a_disabled_operational_profile() {
        let p = list();
        assert!(check_enable_target(&p, "8963000000000000002").is_ok());
        assert_eq!(check_enable_target(&p, "8963000000000000001").unwrap_err().0, 409);
        assert_eq!(check_enable_target(&p, "8963000000000000003").unwrap_err().0, 400);
        assert_eq!(check_enable_target(&p, "8963000000000000009").unwrap_err().0, 404);
    }

    #[test]
    fn delete_never_removes_the_active_profile() {
        let p = list();
        assert!(check_delete_target(&p, "8963000000000000002").is_ok());
        assert!(check_delete_target(&p, "8963000000000000003").is_ok());
        assert_eq!(check_delete_target(&p, "8963000000000000001").unwrap_err().0, 409);
        assert_eq!(check_delete_target(&p, "8963000000000000009").unwrap_err().0, 404);
    }

    #[test]
    fn activation_codes() {
        assert_eq!(check_activation_code("LPA:1$smdp.example.com$ABC-123"), Ok(false));
        assert_eq!(check_activation_code("lpa:1$smdp.example.com:443$ABC123"), Ok(false));
        assert_eq!(check_activation_code("LPA:1$smdp.example.com$ABC123$1.3.6.1$1"), Ok(true));
        assert_eq!(check_activation_code("LPA:1$smdp.example.com$$$"), Ok(false));
        assert!(check_activation_code("LPA:2$smdp.example.com$ABC").is_err());
        assert!(check_activation_code("smdp.example.com$ABC").is_err());
        assert!(check_activation_code("LPA:1$smdp.example.com").is_err());
        assert!(check_activation_code("LPA:1$smdp.example.com$AB C").is_err());
        assert!(check_activation_code("LPA:1$smdp.example.com$AB_C").is_err());
        assert!(check_activation_code("LPA:1$sm dp$ABC").is_err());
        assert!(check_activation_code("LPA:1$;reboot$ABC").is_err());
        assert!(check_activation_code("").is_err());
    }

    #[test]
    fn download_progress_tracks_step_and_metadata() {
        let mut d = Download { state: "running", ..Download::default() };
        d.progress("es9p_initiate_authentication", &json!("smdp.example.com"));
        assert_eq!(d.step, "es9p_initiate_authentication");
        d.progress(
            "es8p_metadata_parse",
            &json!({"iccid":"8963000000000000004","serviceProviderName":"GOMO","profileName":"Globe"}),
        );
        assert_eq!(d.provider.as_deref(), Some("GOMO"));
        assert_eq!(d.name.as_deref(), Some("Globe"));
        d.progress("es9p_authenticate_client", &Value::Null);
        d.progress("es10b_cancel_session", &Value::Null);
        d.progress("es9p_cancel_session", &Value::Null);
        assert_eq!(d.step, "es9p_authenticate_client", "cleanup does not hide the failing step");
        assert_eq!(Download::default().view()["state"], "idle");
    }

    #[test]
    fn preview_prompt_moves_a_running_download_to_confirm() {
        let mut d = Download { state: "running", ..Download::default() };
        d.progress("preview", &Value::Null);
        assert_eq!(d.state, "confirm");
        assert_eq!(d.view()["state"], "confirm");
    }

    #[test]
    fn finished_results_expire_but_running_downloads_do_not() {
        let mut d = Download { state: "cancelled", finished_at: 1_000, ..Download::default() };
        d.expire(1_000 + RESULT_KEEP_SECS - 1);
        assert_eq!(d.state, "cancelled");
        d.expire(1_000 + RESULT_KEEP_SECS);
        assert_eq!(d.view()["state"], "idle");
        let mut running = Download { state: "running", started_at: 1, ..Download::default() };
        running.expire(1_000_000);
        assert_eq!(running.state, "running");
    }

    #[test]
    fn decisions_without_a_running_download_go_nowhere() {
        let control = DownloadControl::default();
        assert!(!control.send(DownloadCtl::Accept));
        assert!(!control.send(DownloadCtl::Cancel));
    }

    #[test]
    fn iccid_validation() {
        assert!(normalize_iccid("8963000000000000001").is_some());
        assert!(normalize_iccid("896300000000000000F").is_some());
        assert!(normalize_iccid("89630000000000000012").is_some());
        assert!(normalize_iccid("89630000000000001").is_none());
        assert!(normalize_iccid("8963000000000000001; reboot").is_none());
        assert!(normalize_iccid("").is_none());
    }

    #[test]
    fn chip_view_picks_identity_and_memory() {
        let data = json!({"eidValue":"89000000000000000000000000000001","EUICCInfo2":{"profileVersion":"2.3.1","svn":"2.2.2","euiccFirmwareVer":"4.2.0","extCardResource":{"installedApplication":0,"freeNonVolatileMemory":972020,"freeVolatileMemory":16619}}});
        let v = chip_view(&data);
        assert_eq!(v["firmware"], "4.2.0");
        assert_eq!(v["sgp22_version"], "2.2.2");
        assert_eq!(v["free_nvm_bytes"], 972020);
    }

    fn temp_lock(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("zte-agent-lock-{}-{tag}", std::process::id()))
    }

    #[test]
    fn the_file_lock_keeps_a_second_holder_out() {
        let path = temp_lock("shared");
        let a = Esim::with_lock_path(path.clone());
        let b = Esim::with_lock_path(path.clone());
        let g = a.acquire().expect("first holder");
        assert!(b.acquire().is_none(), "another holder of the file lock is refused");
        assert!(!b.inner.busy.load(Ordering::Relaxed), "a refused acquire leaves no flag set");
        drop(g);
        // A child being spawned by another thread holds a copy of the descriptor until its
        // exec, so the lock can take a moment to come free.
        let freed = (0..200).any(|_| b.acquire().is_some() || {
            std::thread::sleep(Duration::from_millis(5));
            false
        });
        assert!(freed);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn switching_is_refused_only_when_the_user_parked_the_radio() {
        assert!(!radio_blocks_switch(Some("ONLINE"), Some("idle")));
        assert!(!radio_blocks_switch(Some(" online "), None));
        assert!(!radio_blocks_switch(None, None), "unknown mode does not block");
        assert!(radio_blocks_switch(Some("LPM"), Some("idle")));
        assert!(radio_blocks_switch(Some("OFFLINE"), None));
        assert!(!radio_blocks_switch(Some("LPM"), Some("lpm")), "parked by the no-service policy");
        assert!(!radio_blocks_switch(Some("LPM"), Some("probing")));
    }

    #[test]
    fn a_different_card_drops_the_saved_list() {
        let mut inv = Inventory::default();
        assert!(!inv.note_card("89000000000000000000000000000001"));
        inv.record(list(), 10);
        inv.generation = 4;
        assert!(!inv.note_card("89000000000000000000000000000001"));
        assert!(!inv.note_card(""), "an empty EID proves nothing");
        assert!(inv.profiles.is_some());
        assert!(inv.note_card("89000000000000000000000000000002"));
        assert!(inv.profiles.is_none());
        assert_eq!(inv.generation, 5);
        assert_eq!(inv.eid.as_deref(), Some("89000000000000000000000000000002"));
    }

    #[test]
    fn receipts_keep_attempt_counts_across_reads() {
        let data = json!([
            {"seqNumber": 7, "profileManagementOperation": "install", "notificationAddress": "smdp.example.com", "iccid": "8963000000000000004"},
            {"seqNumber": 8, "profileManagementOperation": "delete", "notificationAddress": "smdp.example.net", "iccid": null},
            {"profileManagementOperation": "install"}
        ]);
        let fresh = receipt_views(&data);
        assert_eq!(fresh.len(), 2, "entries without a sequence number are skipped");
        let mut r = Receipts::default();
        assert!(!r.retryable());
        r.merge(fresh.clone(), 100);
        r.pending.as_mut().unwrap()[0].attempts = RECEIPT_MAX_ATTEMPTS;
        r.pending.as_mut().unwrap()[1].attempts = 2;
        r.merge(fresh, 200);
        let p = r.pending.as_ref().unwrap();
        assert_eq!((p[0].attempts, p[1].attempts), (RECEIPT_MAX_ATTEMPTS, 2));
        assert!(r.retryable());
        let v = r.view(false);
        assert_eq!(v["count"], 2);
        assert_eq!(v["pending"][0]["gave_up"], true);
        assert_eq!(v["pending"][1]["operation"], "delete");
        r.merge(vec![], 300);
        assert!(!r.retryable());
        assert_eq!(r.view(false)["count"], 0);
        assert_eq!(Receipts::default().view(true)["count"], Value::Null, "not listed yet");
    }

    #[test]
    fn nickname_limits() {
        assert!(check_nickname("").is_ok());
        assert!(check_nickname("Travel – Globe").is_ok());
        assert!(check_nickname(&"x".repeat(64)).is_ok());
        assert!(check_nickname(&"x".repeat(65)).is_err());
        assert!(check_nickname(&"é".repeat(33)).is_err(), "64 bytes, not 64 characters");
        assert!(check_nickname("line\nbreak").is_err());
    }

    #[test]
    fn busy_guard_is_exclusive_and_released_on_drop() {
        let e = Esim::with_lock_path(temp_lock("busy"));
        let g = e.acquire().expect("first");
        assert!(e.acquire().is_none());
        drop(g);
        assert!(e.acquire().is_some());
    }

    /// The card commands' data and errors go through the views' parsers.
    #[test]
    fn card_output_feeds_the_views() {
        use crate::esim_native::{info2_json, notification_json, profile_json};
        let p = euicc::es10c::ProfileInfo {
            iccid: "8963000000000000001".into(),
            isdp_aid: "a0000005591010ffffffff8900001000".into(),
            profile_state: Some("enabled"),
            profile_nickname: None,
            service_provider_name: Some("SMART".into()),
            profile_name: Some("Operational".into()),
            profile_class: Some("operational"),
            ..Default::default()
        };
        let views = profile_views(&json!([profile_json(&p)]));
        assert_eq!(views[0]["iccid"], "8963000000000000001");
        assert_eq!(views[0]["state"], "enabled");
        assert_eq!(views[0]["provider"], "SMART");
        assert_eq!(views[0]["class"], "operational");
        assert_eq!(views[0]["nickname"], Value::Null);
        assert_eq!(enabled_iccid(&views).as_deref(), Some("8963000000000000001"));

        let info = euicc::info2::EuiccInfo2 {
            euicc_firmware_ver: Some("4.2.0".into()),
            svn: Some("2.2.2".into()),
            profile_version: Some("2.3.1".into()),
            ext_card_resource: Some(euicc::info2::ExtCardResource {
                installed_application: 3,
                free_non_volatile_memory: 971752,
                free_volatile_memory: 16574,
            }),
            ..Default::default()
        };
        let chip = chip_view(&json!({"eidValue": "89000000000000000000000000000001", "EUICCInfo2": info2_json(&info)}));
        assert_eq!(chip["eid"], "89000000000000000000000000000001");
        assert_eq!(chip["firmware"], "4.2.0");
        assert_eq!(chip["free_nvm_bytes"], 971752);
        let no_info = chip_view(&json!({"eidValue": "89000000000000000000000000000001", "EUICCInfo2": null}));
        assert_eq!(no_info["eid"], "89000000000000000000000000000001", "card check still works without card details");

        let n = euicc::es10b::NotificationMetadata {
            seq_number: 131,
            profile_management_operation: Some("enable"),
            notification_address: Some(" glo.prod.ondemandconnectivity.com\n".into()),
            iccid: Some("8963000000000000001".into()),
        };
        let receipts = receipt_views(&json!([notification_json(&n)]));
        assert_eq!(receipts[0].seq, 131);
        assert_eq!(receipts[0].operation, "enable");
        assert_eq!(receipts[0].address, "glo.prod.ondemandconnectivity.com", "address trimmed");

        // Errors: a SIM reload while opening the channel, and a busy card.
        let reload = "euicc_init: open logical channel failed: QMI error 80 SimFileNotFound";
        assert_eq!(classify(reload, true), "sim_reloading");
        assert_eq!(classify(reload, false), "management_unavailable");
        assert!(card_busy("es10c_enable_profile: catBusy"));
    }

    #[test]
    fn audit_fixes_for_iccids_receipts_routes_and_class() {
        // The modem's padded form is accepted and looked up as the card's form.
        assert_eq!(normalize_iccid("8963000000000000005F").as_deref(), Some("8963000000000000005"));
        assert_eq!(normalize_iccid("89630000000000000027").as_deref(), Some("89630000000000000027"));
        assert!(normalize_iccid("89630000000000000027F").is_none(), "20 digits never carry a filler");
        assert!(normalize_iccid("8963000000000000x01").is_none());
        // A profile without a class tag is operational (SGP.22 default) and can be enabled.
        let views = profile_views(&json!([{"iccid": "8963000000000000001", "profileState": "disabled"}]));
        assert_eq!(views[0]["class"], "operational");
        assert!(check_enable_target(&views, "8963000000000000001").is_ok());
        // Receipts keep attempt counts only for the same receipt, not a reused sequence number.
        let receipt = |seq, address: &str| Receipt {
            seq,
            operation: "enable".into(),
            address: address.into(),
            iccid: None,
            attempts: 0,
            last_error: None,
        };
        let mut r = Receipts::default();
        r.merge(vec![receipt(5, "a.example")], 1);
        r.pending.as_mut().unwrap()[0].attempts = 7;
        r.merge(vec![receipt(5, "b.example")], 2);
        assert_eq!(r.pending.as_ref().unwrap()[0].attempts, 0, "another card's receipt 5");
        // The router's default route.
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\n\
                     rmnet_data0\t00000000\t0100A8C0\t0003\t0\t0\t0\t00000000\n";
        assert!(has_default_route(table));
        assert!(!has_default_route("Iface\tDestination\tGateway \tFlags\nbr-lan\t0000A8C0\t00000000\t0001\n"));
    }

    #[test]
    fn busy_refusals_are_retried_and_other_refusals_are_not() {
        assert!(card_busy("es10c_enable_profile: catBusy"));
        assert!(card_busy("es10c_enable_profile: undefinedError"));
        assert!(card_busy("es10c_enable_profile: unknown result 6"));
        assert!(!card_busy("es10c_enable_profile: disallowedByPolicy"));
        assert!(!card_busy("es10c_enable_profile: unexpected card response (no result code)"));
        assert!(!card_busy("es10c_delete_profile: catBusy"));
        assert!(!card_busy("eSIM command did not finish within 30 s"));
    }

    #[test]
    fn a_roaming_profile_with_roaming_off_is_named_as_the_reason() {
        let wwan = |enable: i64, roam: i64| json!({"enable": enable, "roam_enable": roam});
        // Travel eSIM: Singapore SIM on a Philippine network, roaming off (firmware turns data off).
        assert_eq!(blocker(&wwan(0, 0), Some("525"), Some("515")), Some("roaming_off"));
        assert_eq!(blocker(&wwan(1, 1), Some("525"), Some("515")), None);
        // Home network: roaming doesn't matter, data switched off does.
        assert_eq!(blocker(&wwan(1, 0), Some("515"), Some("515")), None);
        assert_eq!(blocker(&wwan(0, 0), Some("515"), Some("515")), Some("data_off"));
        // Unknown network MCC: no roaming claim.
        assert_eq!(blocker(&wwan(1, 0), Some("525"), None), None);
        assert_eq!(blocker(&json!({"enable": "1", "roam_enable": "0"}), Some("525"), Some("515")), Some("roaming_off"));
    }

    #[test]
    fn switch_guard_expires() {
        let e = Esim::new();
        assert!(!e.recently_switched());
        e.inner.switched_at.store(mono_secs(), Ordering::Relaxed);
        assert!(e.recently_switched());
        assert!(switched_within(1_000, 1_000 + SWITCH_GUARD_SECS - 1));
        assert!(!switched_within(1_000, 1_000 + SWITCH_GUARD_SECS));
        assert!(!switched_within(0, 5), "never switched");
    }

    #[test]
    fn channel_failures_are_reloading_right_after_a_switch_and_unavailable_otherwise() {
        let msg = "euicc_init (error: open logical channel failed: QMI protocol error (80): 'SimFileNotFound')";
        assert_eq!(classify(msg, true), "sim_reloading");
        assert_eq!(classify(msg, false), "management_unavailable");
        assert_eq!(classify("es10c_enable_profile: unknown", true), "read_failed");
    }

    #[test]
    fn inventory_view_marks_old_lists() {
        let mut inv = Inventory::default();
        assert_eq!(inv.view(7, false)["profiles"], json!([]));
        inv.record(list(), 100);
        let v = inv.view(7, false);
        assert_eq!((v["stale"].as_bool(), v["updating"].as_bool()), (Some(false), Some(false)));
        assert_eq!(v["enabled_iccid"], "8963000000000000001");
        assert_eq!(v["agent_instance"], 7);
        inv.reloading = true;
        assert_eq!(inv.view(7, false)["updating"], true);
        inv.reloading = false;
        inv.error = Some(ReadError { kind: "management_unavailable", message: "x".into(), at: 200 });
        let v = inv.view(7, false);
        assert_eq!(v["stale"], true);
        assert_eq!(v["error"]["kind"], "management_unavailable");
        assert_eq!(v["observed_at"], 100, "a failed read keeps the time of the last good one");
        inv.record(list(), 300);
        assert_eq!(inv.view(7, false)["error"], Value::Null, "a good read clears the error");
        assert_eq!(inv.view(7, true)["updating"], true);
    }

    #[test]
    fn iccid_comparison_ignores_filler() {
        assert!(same_iccid("8963000000000000001F", "8963000000000000001"));
        assert!(same_iccid(" 8963000000000000001 ", "8963000000000000001"));
        assert!(!same_iccid("8963000000000000002", "8963000000000000001"));
        assert!(!same_iccid("", ""));
    }

    #[test]
    fn switch_view_starts_idle() {
        assert_eq!(Switch::default().view()["state"], "idle");
        assert_eq!(Switch::default().view()["target_iccid"], Value::Null);
        let mut sw = Switch { state: "reloading", target: "8963000000000000001".into(), ..Switch::default() };
        sw.finish("connected", 9);
        assert_eq!((sw.view()["state"].as_str(), sw.finished_at), (Some("connected"), 9));
    }

    #[test]
    fn logs_show_only_the_last_four_digits() {
        assert_eq!(mask("8963000000000000001"), "…0001");
    }

    #[test]
    fn inventory_seq_orders_reads_without_the_wall_clock() {
        let mut inv = Inventory::default();
        inv.record(list(), 1_000 + 28_800);
        let first = inv.view(1, false);
        inv.record(list(), 1_000);
        let second = inv.view(1, false);
        assert_eq!((first["observed_seq"].as_u64(), second["observed_seq"].as_u64()), (Some(1), Some(2)));
        assert!(second["observed_at"].as_u64() < first["observed_at"].as_u64(), "the clock went back");
        inv.error = Some(ReadError { kind: "read_failed", message: "x".into(), at: 2_000 });
        assert_eq!(inv.view(1, false)["observed_seq"], 2, "a failed read doesn't count");
    }

    #[test]
    fn iccid_country_codes_are_prefix_free() {
        assert_eq!(iccid_country("8963000000000000001"), Some("63"));
        assert_eq!(iccid_country("8965010000000000001"), Some("65"));
        assert_eq!(iccid_country("8914800000000000001"), Some("1"));
        assert_eq!(iccid_country("8970000000000000001"), Some("7"));
        assert_eq!(iccid_country("8985212000000000001"), Some("852"));
        assert_eq!(iccid_country("8988216000000000001"), Some("882"));
        assert_eq!(iccid_country("8901410000000000001"), Some("1"), "AT&T pads the code");
        assert_eq!(iccid_country("8901260000000000001"), Some("1"), "so does T-Mobile US");
        assert_eq!(iccid_country("8930272000000000001"), Some("302"), "Canadian issuers use the MCC");
        assert_eq!(iccid_country("8900100000000000001"), None);
        assert_eq!(iccid_country("8863000000000000001"), None);
        assert_eq!(iccid_country("89"), None);
        assert_eq!(iccid_country("89x1000000000000001"), None);
    }

    #[test]
    fn target_roams_from_iccid_country() {
        let (ph, sg) = ("8963000000000000001", "8965000000000000001");
        assert_eq!(target_roams(sg, Some(ph), Some("515"), Some("515")), Some(true));
        assert_eq!(target_roams(ph, Some(ph), Some("515"), Some("515")), Some(false));
        // Next to a roaming SG SIM, a PH profile may well be home: unknown.
        assert_eq!(target_roams(ph, Some(sg), Some("525"), Some("515")), None);
        assert_eq!(target_roams(sg, Some(sg), Some("525"), Some("515")), Some(true));
        assert_eq!(target_roams(sg, Some(ph), None, Some("515")), None);
        // Unregistered (rmcc 0): no network country, no guess either way.
        assert!(!real_mcc("0") && !real_mcc("000") && real_mcc("515"));
        assert_eq!(target_roams(ph, Some(ph), Some("515"), None), None);
    }

    #[test]
    fn switch_warnings_codes() {
        let (ph, sg) = ("8963000000000000001", "8965000000000000001");
        let check = SwitchCheck {
            manual_apn: Some(("GOMO".into(), "gomo.ph".into())),
            roaming_off: true,
            loaded_iccid: Some(ph.into()),
            sim_mcc: Some("515".into()),
            net_mcc: Some("515".into()),
        };
        assert_eq!(check.warnings(sg), ["manual_apn", "roaming_off"]);
        assert_eq!(check.warnings(ph), ["manual_apn"]);
        let roaming_on = SwitchCheck { roaming_off: false, ..check };
        assert_eq!(roaming_on.warnings(sg), ["manual_apn"]);
        let auto = SwitchCheck { manual_apn: None, ..roaming_on };
        assert!(auto.warnings(ph).is_empty());
    }

    #[test]
    fn enabled_manual_apn_keeps_credentials_out() {
        let list = |enabled: Value| {
            json!({"apnListArray": [
                {"profilename": "Default", "wanapn": "", "isEnable": false},
                {"profilename": "e-ideas", "wanapn": "e-ideas", "username": "user", "password": "secret", "isEnable": enabled},
            ]})
        };
        for enabled in [json!(true), json!(1), json!("1")] {
            let apn = enabled_manual_apn(&list(enabled)).unwrap();
            assert_eq!(apn, ("e-ideas".to_string(), "e-ideas".to_string()));
            assert!(!format!("{apn:?}").contains("secret"));
        }
        assert_eq!(enabled_manual_apn(&list(json!(false))), None);
    }

    #[test]
    fn profiles_view_gets_switch_warnings() {
        let mut inv = Inventory::default();
        inv.record(list(), 1);
        let mut view = inv.view(1, false);
        let check = SwitchCheck {
            manual_apn: Some(("GOMO".into(), "gomo.ph".into())),
            ..SwitchCheck::default()
        };
        check.annotate(&mut view);
        let p = view["profiles"].as_array().unwrap();
        assert!(p[0].get("switch_warnings").is_none(), "the enabled profile");
        assert_eq!(p[1]["switch_warnings"], json!(["manual_apn"]));
        assert!(p[2].get("switch_warnings").is_none(), "a test profile can't be switched to");
        assert_eq!(view["manual_apn"], json!({"name": "GOMO", "apn": "gomo.ph"}));
        let mut view = inv.view(1, false);
        SwitchCheck::default().annotate(&mut view);
        assert_eq!(view["manual_apn"], Value::Null);
        assert_eq!(view["profiles"][1]["switch_warnings"], json!([]));
    }
}
