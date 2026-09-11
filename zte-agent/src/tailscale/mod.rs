pub mod config;

pub use config::TsConfig;

use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::handlers::AppState;
use crate::ubus;

const BIN_DIR: &str = "/data/tailscale/bin";
const TAILSCALED_BIN: &str = "/data/tailscale/bin/tailscaled";
const TAILSCALE_BIN: &str = "/data/tailscale/bin/tailscale";
const STATE_FILE: &str = "/data/tailscale/tailscaled.state";
const STATE_BAK: &str = "/data/tailscale/tailscaled.state.bak";
const RUN_DIR: &str = "/tmp/tailscale";
const SOCK: &str = "/tmp/tailscale/tailscaled.sock";
const LOG_FILE: &str = "/tmp/tailscaled.log";

const POLL_SECS: u64 = 30;
const CLI_TIMEOUT_SECS: u64 = 20;
/// Shorter than CLI_TIMEOUT_SECS so GET /status can't stall mobile clients
/// for long when the daemon is wedged.
const STATUS_CLI_TIMEOUT_SECS: u64 = 8;
const BACKOFF_MIN_SECS: u64 = 5;
const BACKOFF_MAX_SECS: u64 = 60;
const QUIESCE_TIMEOUT_SECS: u64 = 60;
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// 2025-01-01 UTC — clock earlier than this means NTP hasn't synced yet.
const SANE_CLOCK_EPOCH: i64 = 1_735_689_600;

pub struct TailscaleManager {
    config: Mutex<TsConfig>,
    /// Desired state: should tailscaled be running?
    running: AtomicBool,
    /// Update in progress — supervisor must quiesce so binaries can be swapped.
    paused: AtomicBool,
    /// The single supervisor thread has been spawned.
    supervisor_started: AtomicBool,
    /// True only while the supervisor is parked with no child — the only
    /// window where swapping binaries is safe.
    supervisor_idle: AtomicBool,
    updating: AtomicBool,
    login_in_progress: AtomicBool,
    /// One state-file restore attempt per enrollment (reset on setup/logout).
    state_restore_attempted: AtomicBool,
    child_pid: Mutex<Option<u32>>,
    /// Last computed status payload + unix timestamp it was computed at.
    last_status: Mutex<(Value, i64)>,
    /// Outcome of the most recent update attempt (surfaced in /status).
    last_update: Mutex<Option<Value>>,
    login_url: Mutex<Option<String>>,
    wakelock_held: AtomicBool,
}

fn unix_now() -> i64 {
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}

/// Run a command with a hard timeout. On timeout the process is SIGKILLed
/// and reaped by the waiter thread.
fn run_with_timeout(mut cmd: Command, timeout_secs: u64) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
        Ok(result) => result.map_err(|e| format!("wait: {e}")),
        Err(_) => {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            Err(format!("timed out after {timeout_secs}s"))
        }
    }
}

/// `tailscale --socket=... <args>` -> stdout. Mirrors ubus::call.
fn cli(args: &[&str], timeout_secs: u64) -> Result<String, String> {
    let mut cmd = Command::new(TAILSCALE_BIN);
    cmd.arg(format!("--socket={SOCK}"));
    cmd.args(args);
    let out = run_with_timeout(cmd, timeout_secs)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let msg = if stderr.trim().is_empty() { stdout } else { stderr };
        return Err(msg.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn cli_json(args: &[&str], timeout_secs: u64) -> Result<Value, String> {
    let stdout = cli(args, timeout_secs)?;
    serde_json::from_str(stdout.trim()).map_err(|e| format!("parse tailscale JSON: {e}"))
}

fn installed() -> bool {
    Path::new(TAILSCALED_BIN).exists() && Path::new(TAILSCALE_BIN).exists()
}

fn state_file_exists() -> bool {
    Path::new(STATE_FILE).exists()
}

/// A reboot mid-swap can leave `tailscaled` missing with `.prev`/`.new`
/// siblings present. Promote the known-good `.prev` first, else `.new`.
fn recover_binaries() {
    for name in ["tailscaled", "tailscale"] {
        let current = format!("{BIN_DIR}/{name}");
        if Path::new(&current).exists() {
            continue;
        }
        for suffix in [".prev", ".new"] {
            let candidate = format!("{current}{suffix}");
            if Path::new(&candidate).exists() {
                eprintln!("[tailscale] recovering {name} from {suffix} after interrupted update");
                if std::fs::rename(&candidate, &current).is_ok() {
                    break;
                }
            }
        }
    }
}

impl TailscaleManager {
    pub fn new() -> Self {
        recover_binaries();
        Self {
            config: Mutex::new(config::load()),
            running: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            supervisor_started: AtomicBool::new(false),
            supervisor_idle: AtomicBool::new(true),
            updating: AtomicBool::new(false),
            login_in_progress: AtomicBool::new(false),
            state_restore_attempted: AtomicBool::new(false),
            child_pid: Mutex::new(None),
            last_status: Mutex::new((Value::Null, 0)),
            last_update: Mutex::new(None),
            login_url: Mutex::new(None),
            wakelock_held: AtomicBool::new(false),
        }
    }

    /// Called once from main() — starts the daemon if previously enabled.
    pub fn auto_start(self: &Arc<Self>) {
        let cfg = self.config.lock().unwrap().clone();
        if !cfg.enabled || !installed() {
            return;
        }
        if cfg.hold_wakelock {
            self.acquire_wakelock();
        }
        self.running.store(true, Ordering::SeqCst);
        self.ensure_supervisor();
    }

    /// WAN-flap hook: on reconnect events, nudge tailscaled (rebind + restun)
    /// and escalate to a daemon restart if it stays unhealthy.
    pub fn start_wan_watch(self: &Arc<Self>, rx: mpsc::Receiver<Value>) {
        let mgr = Arc::clone(self);
        thread::spawn(move || {
            let mut last_handled: i64 = 0;
            for payload in rx.iter() {
                // Only react to reconnects, not disconnects (same event shape
                // sms_forward parses: "ipv4", "ipv4v6", ...)
                let wan_status = payload["wan_status"].as_str().unwrap_or("");
                if !wan_status.starts_with("ipv4") {
                    continue;
                }
                if !mgr.running.load(Ordering::SeqCst) || mgr.paused.load(Ordering::SeqCst) {
                    continue;
                }
                // Debounce bursts of WAN events
                if unix_now() - last_handled < 30 {
                    continue;
                }
                last_handled = unix_now();
                // Give the data path time to settle, then check health (fresh
                // status — the cache may predate the flap)
                thread::sleep(Duration::from_secs(20));
                if !mgr.running.load(Ordering::SeqCst) {
                    continue;
                }
                mgr.refresh_status();
                if mgr.is_unhealthy() {
                    eprintln!("[tailscale] unhealthy after WAN reconnect — rebind/restun");
                    mgr.nudge();
                    thread::sleep(Duration::from_secs(30));
                    mgr.refresh_status();
                    if mgr.running.load(Ordering::SeqCst) && mgr.is_unhealthy() {
                        eprintln!("[tailscale] still unhealthy — restarting tailscaled");
                        mgr.kill_child(libc::SIGTERM);
                    }
                }
            }
        });
    }

    // --- Supervisor ---

    /// Spawn the single long-lived supervisor thread (idles while disabled).
    fn ensure_supervisor(self: &Arc<Self>) {
        if self
            .supervisor_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let mgr = Arc::clone(self);
        thread::spawn(move || mgr.supervise());
    }

    fn supervise(&self) {
        let mut backoff = BACKOFF_MIN_SECS;
        loop {
            if !self.running.load(Ordering::SeqCst) || self.paused.load(Ordering::SeqCst) {
                self.supervisor_idle.store(true, Ordering::SeqCst);
                thread::sleep(Duration::from_secs(2));
                continue;
            }
            if !installed() {
                self.supervisor_idle.store(true, Ordering::SeqCst);
                thread::sleep(Duration::from_secs(10));
                continue;
            }
            self.supervisor_idle.store(false, Ordering::SeqCst);

            self.wait_for_clock_and_wan();
            if !self.running.load(Ordering::SeqCst) || self.paused.load(Ordering::SeqCst) {
                continue;
            }

            // Adopt-by-replace: a previous agent instance (e.g. redeploy via
            // killall zte-agent) leaves its tailscaled orphaned; a second
            // instance would fight over the socket and state file.
            self.kill_orphan_tailscaled();

            // Clean up leftovers from a previous crash (no-op in userspace mode)
            let mut cleanup = Command::new(TAILSCALED_BIN);
            cleanup.arg("--cleanup");
            let _ = run_with_timeout(cleanup, 10);

            // Last gate before spawn — an update may have paused us meanwhile
            if !self.running.load(Ordering::SeqCst) || self.paused.load(Ordering::SeqCst) {
                continue;
            }

            rotate_log_if_big();
            let _ = std::fs::create_dir_all(RUN_DIR);
            match self.spawn_tailscaled() {
                Ok(mut child) => {
                    eprintln!("[tailscale] tailscaled started (pid {})", child.id());
                    *self.child_pid.lock().unwrap() = Some(child.id());
                    self.apply_prefs_when_ready();
                    let clean_exit = self.monitor_until_exit(&mut child);
                    *self.child_pid.lock().unwrap() = None;
                    // AuthURL was in-memory in the dead daemon — a cached copy
                    // would be served as live and block re-enrollment
                    *self.login_url.lock().unwrap() = None;
                    if clean_exit {
                        backoff = BACKOFF_MIN_SECS;
                    }
                    eprintln!("[tailscale] tailscaled exited");
                }
                Err(e) => {
                    eprintln!("[tailscale] spawn failed: {e}");
                }
            }

            if !self.running.load(Ordering::SeqCst) {
                continue;
            }
            // Crash or spawn failure — back off before respawn
            let mut waited = 0;
            while waited < backoff
                && self.running.load(Ordering::SeqCst)
                && !self.paused.load(Ordering::SeqCst)
            {
                thread::sleep(Duration::from_secs(1));
                waited += 1;
            }
            backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
        }
    }

    fn spawn_tailscaled(&self) -> std::io::Result<std::process::Child> {
        let log_out = OpenOptions::new().create(true).append(true).open(LOG_FILE);
        let (stdout, stderr) = match log_out {
            Ok(f) => {
                let f2 = f.try_clone();
                (
                    Stdio::from(f),
                    f2.map(Stdio::from).unwrap_or_else(|_| Stdio::null()),
                )
            }
            Err(_) => (Stdio::null(), Stdio::null()),
        };
        Command::new(TAILSCALED_BIN)
            .args([
                "--tun=userspace-networking",
                &format!("--state={STATE_FILE}"),
                &format!("--statedir={RUN_DIR}"),
                &format!("--socket={SOCK}"),
                "--port=41641",
                "--no-logs-no-support",
            ])
            .env("GOGC", "25")
            .env("GOMEMLIMIT", "192MiB")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
    }

    /// Poll status while the child is alive. Detects suspend/resume via
    /// wall-clock jumps and nudges the daemon. Returns true if the exit was
    /// requested (disable/update), false on crash/forced restart.
    fn monitor_until_exit(&self, child: &mut std::process::Child) -> bool {
        let mut last_tick = unix_now();
        let mut bad_cycles = 0u32;
        let mut needs_login_cycles = 0u32;
        loop {
            // Requested stop?
            if !self.running.load(Ordering::SeqCst) || self.paused.load(Ordering::SeqCst) {
                self.graceful_stop(child);
                return true;
            }
            match child.try_wait() {
                Ok(Some(_)) => return false, // crashed / externally killed
                Ok(None) => {}
                Err(_) => return false,
            }

            // Suspend/resume detection: wall clock jumped past our sleep
            let now = unix_now();
            if now - last_tick > (POLL_SECS as i64) * 2 + 30 {
                eprintln!("[tailscale] wall-clock jump detected (suspend?) — rebind/restun");
                self.nudge();
            }
            last_tick = now;

            let payload = self.refresh_status();
            let cfg = self.config.lock().unwrap().clone();

            // Wakelock is load-bearing for reachability — keep retrying if the
            // ubus call failed earlier (silent sleep = dead tunnel).
            if cfg.enabled && cfg.hold_wakelock && !self.wakelock_held.load(Ordering::SeqCst) {
                self.acquire_wakelock();
            }

            match payload["state"].as_str().unwrap_or("") {
                "healthy" => {
                    bad_cycles = 0;
                    needs_login_cycles = 0;
                }
                // Real connectivity problems only — Health warnings alone do
                // NOT restart the daemon (they can be benign and permanent).
                "degraded" | "connecting" | "starting" | "unknown" => {
                    bad_cycles += 1;
                    if bad_cycles == 2 {
                        eprintln!("[tailscale] unhealthy for 2 cycles — rebind/restun");
                        self.nudge();
                    } else if bad_cycles >= 4 {
                        eprintln!("[tailscale] unhealthy for {bad_cycles} cycles — restarting");
                        self.graceful_stop(child);
                        return false; // backoff respawn
                    }
                }
                // Backend Stopped (WantRunning=false, e.g. after `down`) while
                // the user wants it enabled — self-heal with a full-pref `up`.
                // Makes POST /enable eventually consistent regardless of the
                // daemon-startup races. Never runs during interactive login.
                "paused" if cfg.enabled && !self.login_in_progress.load(Ordering::SeqCst) => {
                    eprintln!("[tailscale] backend Stopped while enabled — bringing it up");
                    let mut args = self.pref_args("up");
                    args.push("--timeout=30s".to_string());
                    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    if let Err(e) = cli(&arg_refs, 40) {
                        eprintln!("[tailscale] up after Stopped: {e}");
                    }
                    self.refresh_status();
                }
                // Spurious logout with an enrolled node: one restore attempt
                // from the post-enrollment state backup (report §4.6 ladder).
                "needs_login" if cfg.enrolled => {
                    needs_login_cycles += 1;
                    if needs_login_cycles >= 3
                        && Path::new(STATE_BAK).exists()
                        && !self.login_in_progress.load(Ordering::SeqCst)
                        && !self.state_restore_attempted.swap(true, Ordering::SeqCst)
                    {
                        eprintln!("[tailscale] enrolled node stuck in NeedsLogin — restoring state backup");
                        self.graceful_stop(child);
                        let _ = std::fs::copy(STATE_BAK, STATE_FILE);
                        return false; // respawn with restored state
                    }
                }
                _ => {
                    bad_cycles = 0;
                }
            }

            // Sleep in 1s slices so disable/update stay responsive
            let mut slept = 0;
            while slept < POLL_SECS {
                if !self.running.load(Ordering::SeqCst)
                    || self.paused.load(Ordering::SeqCst)
                    || matches!(child.try_wait(), Ok(Some(_)) | Err(_))
                {
                    break;
                }
                thread::sleep(Duration::from_secs(1));
                slept += 1;
            }
        }
    }

    fn graceful_stop(&self, child: &mut std::process::Child) {
        let pid = child.id();
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        for _ in 0..15 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.wait();
                return;
            }
            thread::sleep(Duration::from_secs(1));
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Signal the supervised child by pid, verifying via /proc that the pid
    /// still belongs to tailscaled (guards against pid recycling).
    fn kill_child(&self, sig: i32) {
        if let Some(pid) = *self.child_pid.lock().unwrap() {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            let comm = comm.trim();
            if comm.is_empty() || comm == "tailscaled" {
                unsafe { libc::kill(pid as i32, sig) };
            }
        }
    }

    /// SIGTERM (then SIGKILL) any tailscaled we don't own. On this device any
    /// tailscaled instance is ours — typically an orphan from a previous
    /// zte-agent process.
    fn kill_orphan_tailscaled(&self) {
        let pids = |out: &std::process::Output| -> Vec<i32> {
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect()
        };
        let found = match Command::new("pidof").arg("tailscaled").output() {
            Ok(o) if o.status.success() => pids(&o),
            _ => return,
        };
        if found.is_empty() {
            return;
        }
        eprintln!("[tailscale] terminating orphaned tailscaled instance(s): {found:?}");
        for pid in &found {
            unsafe { libc::kill(*pid, libc::SIGTERM) };
        }
        for _ in 0..10 {
            thread::sleep(Duration::from_secs(1));
            match Command::new("pidof").arg("tailscaled").output() {
                Ok(o) if o.status.success() && !pids(&o).is_empty() => continue,
                _ => return,
            }
        }
        if let Ok(o) = Command::new("pidof").arg("tailscaled").output() {
            for pid in pids(&o) {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    /// Gate startup on a sane clock (control-plane TLS fails with a 1970
    /// clock) and a connected WAN. Best-effort: proceeds after ~90s anyway —
    /// tailscaled retries on its own.
    fn wait_for_clock_and_wan(&self) {
        for _ in 0..90 {
            if !self.running.load(Ordering::SeqCst) || self.paused.load(Ordering::SeqCst) {
                return;
            }
            let clock_ok = unix_now() >= SANE_CLOCK_EPOCH;
            let wan_ok = match ubus::call(
                "zwrt_data",
                "get_wwaniface",
                Some(r#"{"source_module":"zte_topsw_data","cid":1}"#),
            ) {
                Ok(v) => v["connect_status"].as_str() == Some("connected"),
                Err(_) => true, // can't tell (e.g. dev machine) — don't block
            };
            if clock_ok && wan_ok {
                return;
            }
            thread::sleep(Duration::from_secs(1));
        }
    }

    /// Idempotent prefs, applied after every daemon start. Retries while the
    /// socket comes up. Failure is non-fatal (e.g. no profile before first
    /// enrollment).
    fn apply_prefs_when_ready(&self) {
        for _ in 0..15 {
            if !self.running.load(Ordering::SeqCst) || self.paused.load(Ordering::SeqCst) {
                return;
            }
            if cli(&["status", "--json", "--peers=false"], 5).is_ok() {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
        if self.paused.load(Ordering::SeqCst) {
            return;
        }
        if state_file_exists() {
            let args = self.pref_args("set");
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            if let Err(e) = cli(&arg_refs, CLI_TIMEOUT_SECS) {
                eprintln!("[tailscale] apply prefs: {e}");
            }
        }
        self.refresh_status();
    }

    /// Shared pref flags for `set` (steady state) and `up` (enrollment /
    /// re-activation). Never includes --auth-key (tailscale#16987).
    fn pref_args(&self, verb: &str) -> Vec<String> {
        let cfg = self.config.lock().unwrap().clone();
        pref_args_for(&cfg, verb)
    }

    /// `tailscale debug rebind` + `restun`: cheap recovery for stale NAT
    /// mappings after suspend or WAN flap, without a daemon restart.
    fn nudge(&self) {
        let _ = cli(&["debug", "rebind"], 10);
        let _ = cli(&["debug", "restun"], 10);
    }

    // --- Status ---

    /// Fetch `tailscale status --json` and cache the derived payload.
    fn refresh_status(&self) -> Value {
        let payload = self.build_status();
        *self.last_status.lock().unwrap() = (payload.clone(), unix_now());
        payload
    }

    fn build_status(&self) -> Value {
        let cfg = self.config.lock().unwrap().clone();
        let daemon_up = self.child_pid.lock().unwrap().is_some();

        let mut payload = json!({
            "installed": installed(),
            "enabled": cfg.enabled,
            "enrolled": cfg.enrolled,
            "daemon_running": daemon_up,
            "state": "unknown",
            "backend_state": Value::Null,
            "healthy": false,
            "health": [],
            "tailscale_ips": [],
            "dns_name": Value::Null,
            "relay": Value::Null,
            "direct": false,
            "key_expiry": Value::Null,
            "expired": false,
            "client_version": Value::Null,
            "login_url": *self.login_url.lock().unwrap(),
            "wakelock_held": self.wakelock_held.load(Ordering::SeqCst),
            "updating": self.updating.load(Ordering::SeqCst),
            "last_update": *self.last_update.lock().unwrap(),
            "hostname": cfg.hostname,
            "advertise_routes": cfg.advertise_routes,
            "tailscale_ssh": cfg.tailscale_ssh,
            "hold_wakelock": cfg.hold_wakelock,
            "pinned_version": cfg.pinned_version,
        });

        if !installed() {
            payload["state"] = json!("not_installed");
            return payload;
        }
        if self.updating.load(Ordering::SeqCst) {
            payload["state"] = json!("updating");
            return payload;
        }
        if !cfg.enabled && !daemon_up {
            payload["state"] = json!("disabled");
            return payload;
        }
        if !daemon_up {
            payload["state"] = json!("respawning");
            return payload;
        }

        let status = match cli_json(&["status", "--json", "--peers=false"], STATUS_CLI_TIMEOUT_SECS) {
            Ok(v) => v,
            Err(e) => {
                payload["state"] = json!("starting");
                payload["error"] = json!(e);
                return payload;
            }
        };

        let backend = status["BackendState"].as_str().unwrap_or("");
        let health: Vec<String> = status["Health"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let self_obj = &status["Self"];
        let online = self_obj["Online"].as_bool().unwrap_or(false);

        payload["backend_state"] = json!(backend);
        payload["health"] = json!(health);
        payload["tailscale_ips"] = status["TailscaleIPs"].clone();
        payload["dns_name"] = self_obj["DNSName"].clone();
        payload["relay"] = self_obj["Relay"].clone();
        payload["direct"] = json!(self_obj["CurAddr"].as_str().map(|s| !s.is_empty()).unwrap_or(false));
        payload["key_expiry"] = self_obj["KeyExpiry"].clone();
        payload["expired"] = json!(self_obj["Expired"].as_bool().unwrap_or(false));
        payload["client_version"] = status["Version"].clone();

        if backend == "Running" {
            // Enrollment done — any cached interactive-login URL is stale
            *self.login_url.lock().unwrap() = None;
            payload["login_url"] = Value::Null;
        } else if let Some(url) = status["AuthURL"].as_str() {
            if !url.is_empty() {
                payload["login_url"] = json!(url);
                *self.login_url.lock().unwrap() = Some(url.to_string());
            }
        }

        // Health warnings are informational (can be benign and permanent);
        // "degraded" is reserved for Running-but-offline, which is actionable.
        let state = match backend {
            "NeedsLogin" => {
                if cfg.enrolled {
                    "needs_login"
                } else {
                    "awaiting_setup"
                }
            }
            "NeedsMachineAuth" => "pending_approval",
            "Stopped" => "paused",
            "Starting" | "NoState" => "connecting",
            "Running" => {
                if online {
                    "healthy"
                } else {
                    "degraded"
                }
            }
            _ => "unknown",
        };
        payload["state"] = json!(state);
        payload["healthy"] = json!(state == "healthy");
        payload
    }

    fn is_unhealthy(&self) -> bool {
        let (status, _) = self.last_status.lock().unwrap().clone();
        matches!(
            status["state"].as_str().unwrap_or(""),
            "degraded" | "connecting" | "starting" | "unknown" | "respawning"
        )
    }

    // --- Wakelock ---

    fn acquire_wakelock(&self) {
        match ubus::call("zwrt_zte_sleep_faw.wakelock", "createWakelock", Some("{}")) {
            Ok(_) => self.wakelock_held.store(true, Ordering::SeqCst),
            Err(e) => eprintln!("[tailscale] createWakelock: {e}"),
        }
    }

    fn release_wakelock(&self) {
        if !self.wakelock_held.load(Ordering::SeqCst) {
            return;
        }
        match ubus::call("zwrt_zte_sleep_faw.wakelock", "destroyWakelock", Some("{}")) {
            Ok(_) => self.wakelock_held.store(false, Ordering::SeqCst),
            Err(e) => eprintln!("[tailscale] destroyWakelock: {e}"),
        }
    }

    // --- Lifecycle operations (called from handlers) ---

    fn ensure_daemon_ready(self: &Arc<Self>, wait_secs: u64) -> Result<(), String> {
        if !installed() {
            return Err("tailscale is not installed — POST /api/tailscale/update first".into());
        }
        if self.updating.load(Ordering::SeqCst) {
            return Err("update in progress".into());
        }
        self.running.store(true, Ordering::SeqCst);
        self.ensure_supervisor();
        for _ in 0..wait_secs {
            if cli(&["status", "--json", "--peers=false"], 5).is_ok() {
                return Ok(());
            }
            thread::sleep(Duration::from_secs(1));
        }
        Err("tailscaled did not become ready in time — check /api/tailscale/status".into())
    }

    /// Non-blocking: flags the desired state and returns. The supervisor
    /// spawns the daemon and the monitor loop self-heals a Stopped backend
    /// (`up` with full pref flags — a previous `tailscale down` persists
    /// WantRunning=false). Track progress via GET /api/tailscale/status.
    pub fn do_enable(self: &Arc<Self>) -> Result<(), String> {
        if !installed() {
            return Err("tailscale is not installed — POST /api/tailscale/update first".into());
        }
        if self.updating.load(Ordering::SeqCst) {
            return Err("update in progress".into());
        }
        let hold = {
            let mut cfg = self.config.lock().unwrap();
            cfg.enabled = true;
            config::save(&cfg)?;
            cfg.hold_wakelock
        };
        if hold {
            self.acquire_wakelock();
        }
        self.running.store(true, Ordering::SeqCst);
        self.ensure_supervisor();
        self.refresh_status();
        Ok(())
    }

    pub fn do_disable(&self) -> Result<(), String> {
        {
            let mut cfg = self.config.lock().unwrap();
            cfg.enabled = false;
            config::save(&cfg)?;
        }
        // Politely stop the backend so the node shows offline, then stop the daemon
        if self.child_pid.lock().unwrap().is_some() {
            let _ = cli(&["down"], 15);
        }
        self.running.store(false, Ordering::SeqCst);
        self.kill_child(libc::SIGTERM);
        self.release_wakelock();
        self.refresh_status();
        Ok(())
    }

    /// Mark enrollment success: persist flags, back up identity, wakelock.
    fn after_enrollment_success(&self) {
        let _ = std::fs::copy(STATE_FILE, STATE_BAK);
        self.state_restore_attempted.store(false, Ordering::SeqCst);
        let hold = {
            let mut cfg = self.config.lock().unwrap();
            cfg.enabled = true;
            cfg.enrolled = true;
            let _ = config::save(&cfg);
            cfg.hold_wakelock
        };
        if hold {
            self.acquire_wakelock();
        }
    }

    /// Pause the supervisor and wait until it is parked with no child.
    /// Returns false if it fails to quiesce within the budget.
    fn quiesce_supervisor(&self) -> bool {
        self.paused.store(true, Ordering::SeqCst);
        for i in 0..QUIESCE_TIMEOUT_SECS {
            if i % 5 == 0 {
                self.kill_child(libc::SIGTERM);
            }
            if self.supervisor_idle.load(Ordering::SeqCst)
                && self.child_pid.lock().unwrap().is_none()
            {
                return true;
            }
            thread::sleep(Duration::from_secs(1));
        }
        false
    }
}

fn pref_args_for(cfg: &TsConfig, verb: &str) -> Vec<String> {
    let mut args = vec![
        verb.to_string(),
        "--accept-dns=false".to_string(),
        "--accept-routes=false".to_string(),
        format!("--hostname={}", cfg.hostname),
        format!("--advertise-routes={}", cfg.advertise_routes.join(",")),
        format!("--ssh={}", cfg.tailscale_ssh),
    ];
    if verb == "up" {
        args.push("--reset".to_string());
    }
    args
}

fn rotate_log_if_big() {
    if let Ok(meta) = std::fs::metadata(LOG_FILE) {
        if meta.len() > LOG_MAX_BYTES {
            let _ = std::fs::rename(LOG_FILE, format!("{LOG_FILE}.old"));
        }
    }
}

/// Remote access without an agent password would expose an unauthenticated
/// management API to every tailnet device.
fn require_password(state: &AppState) -> Option<(u16, Value)> {
    if state.auth.has_password() {
        None
    } else {
        Some((
            403,
            json!({"ok": false, "error": "set an agent password (ZTE_AGENT_PASSWORD) before enabling remote access"}),
        ))
    }
}

// --- HTTP handlers (wired in server.rs) ---

/// GET /api/tailscale/status
pub fn status(state: &AppState) -> (u16, Value) {
    let mgr = &state.tailscale;
    // Serve cached status if fresh; refresh otherwise
    let (cached, at) = mgr.last_status.lock().unwrap().clone();
    let payload = if !cached.is_null() && unix_now() - at < 10 {
        cached
    } else {
        mgr.refresh_status()
    };
    (200, json!({"ok": true, "data": payload}))
}

/// POST /api/tailscale/setup — body: {"auth_key": "tskey-auth-..."}
/// The only code path that ever passes an auth key (see tailscale#16987).
pub fn setup(state: &AppState, body: &[u8]) -> (u16, Value) {
    let mgr = &state.tailscale;
    if let Some(resp) = require_password(state) {
        return resp;
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let key = match parsed["auth_key"].as_str() {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => return (400, json!({"ok": false, "error": "missing 'auth_key' field"})),
    };
    if mgr.updating.load(Ordering::SeqCst) {
        return (409, json!({"ok": false, "error": "update in progress"}));
    }
    if mgr.login_in_progress.load(Ordering::SeqCst) {
        return (409, json!({"ok": false, "error": "interactive login in progress"}));
    }

    if state_file_exists() {
        let current = mgr.refresh_status();
        if current["backend_state"].as_str() == Some("Running") {
            return (409, json!({"ok": false, "error": "already enrolled — logout first to re-enroll"}));
        }
    }

    if let Err(e) = mgr.ensure_daemon_ready(30) {
        return (503, json!({"ok": false, "error": e}));
    }

    // Key goes to tmpfs with 0600, never to /data, never into argv
    let keyfile = format!("{RUN_DIR}/authkey");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&keyfile);
        match file {
            Ok(mut f) => {
                if f.write_all(key.as_bytes()).is_err() {
                    let _ = std::fs::remove_file(&keyfile);
                    return (500, json!({"ok": false, "error": "failed to write auth key"}));
                }
            }
            Err(e) => return (500, json!({"ok": false, "error": format!("write auth key: {e}")})),
        }
    }

    let mut args = mgr.pref_args("up");
    args.push("--timeout=60s".to_string());
    args.push(format!("--auth-key=file:{keyfile}"));
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let result = cli(&arg_refs, 90);
    let _ = std::fs::remove_file(&keyfile);

    match result {
        Ok(_) => {
            mgr.after_enrollment_success();
            let payload = mgr.refresh_status();
            (200, json!({"ok": true, "data": payload}))
        }
        Err(e) => (502, json!({"ok": false, "error": format!("tailscale up: {e}")})),
    }
}

/// POST /api/tailscale/login-url — interactive enrollment alternative.
/// Kicks off `tailscale up --json` in the background and returns the AuthURL.
pub fn login_url(state: &AppState) -> (u16, Value) {
    let mgr = &state.tailscale;
    if let Some(resp) = require_password(state) {
        return resp;
    }
    if mgr.updating.load(Ordering::SeqCst) {
        return (409, json!({"ok": false, "error": "update in progress"}));
    }
    {
        let cfg = mgr.config.lock().unwrap().clone();
        if cfg.enrolled && !cfg.enabled {
            return (409, json!({"ok": false, "error": "tailscale is disabled — POST /api/tailscale/enable first"}));
        }
    }
    if let Err(e) = mgr.ensure_daemon_ready(30) {
        return (503, json!({"ok": false, "error": e}));
    }

    let current = mgr.refresh_status();
    if current["backend_state"].as_str() == Some("Running") {
        return (409, json!({"ok": false, "error": "already enrolled"}));
    }
    if let Some(url) = current["login_url"].as_str() {
        if !url.is_empty() {
            return (200, json!({"ok": true, "data": {"login_url": url, "pending": false}}));
        }
    }

    if mgr
        .login_in_progress
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        let mgr2 = Arc::clone(&state.tailscale);
        thread::spawn(move || {
            let mut args = mgr2.pref_args("up");
            args.push("--json".to_string());
            args.push("--timeout=300s".to_string());
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            // Blocks until the user completes login (or timeout); the AuthURL
            // surfaces via `tailscale status` which the monitor loop and the
            // poll below both read.
            let _ = cli(&arg_refs, 310);
            mgr2.login_in_progress.store(false, Ordering::SeqCst);
            let status = mgr2.refresh_status();
            if status["backend_state"].as_str() == Some("Running") {
                mgr2.after_enrollment_success();
                mgr2.refresh_status();
            }
        });
    }

    // Poll briefly for the AuthURL to appear — wall-clock bounded so a
    // wedged daemon (8s CLI timeout per probe) can't pin this worker thread
    let deadline = unix_now() + 6;
    while unix_now() < deadline {
        thread::sleep(Duration::from_millis(500));
        let status = mgr.refresh_status();
        if let Some(url) = status["login_url"].as_str() {
            if !url.is_empty() {
                return (200, json!({"ok": true, "data": {"login_url": url, "pending": false}}));
            }
        }
    }
    (202, json!({"ok": true, "data": {"login_url": Value::Null, "pending": true}}))
}

/// POST /api/tailscale/enable — returns immediately; the supervisor brings
/// the daemon up in the background. Poll /status for progress.
pub fn enable(state: &AppState) -> (u16, Value) {
    if let Some(resp) = require_password(state) {
        return resp;
    }
    match state.tailscale.do_enable() {
        Ok(()) => (200, json!({"ok": true, "data": {"status": "enabling"}})),
        Err(e) => (500, json!({"ok": false, "error": e})),
    }
}

/// POST /api/tailscale/disable
pub fn disable(state: &AppState) -> (u16, Value) {
    match state.tailscale.do_disable() {
        Ok(()) => (200, json!({"ok": true, "data": {"status": "disabled"}})),
        Err(e) => (500, json!({"ok": false, "error": e})),
    }
}

/// PUT /api/tailscale/config — body: any of
/// {"hostname", "advertise_routes", "hold_wakelock", "tailscale_ssh"}
pub fn config_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    let mgr = &state.tailscale;
    let patch: config::TsConfigPatch = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid config JSON: {e}")})),
    };

    // Apply to a candidate first; persist only after the daemon accepted it
    // (otherwise saved config silently diverges from live prefs).
    let mut candidate = mgr.config.lock().unwrap().clone();
    if let Err(e) = candidate.apply_patch(patch) {
        return (400, json!({"ok": false, "error": e}));
    }

    let daemon_up = mgr.child_pid.lock().unwrap().is_some();
    if candidate.enabled && daemon_up && state_file_exists() {
        let args = pref_args_for(&candidate, "set");
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        if let Err(e) = cli(&arg_refs, CLI_TIMEOUT_SECS) {
            return (502, json!({"ok": false, "error": format!("tailscale set: {e}")}));
        }
    }

    let (enabled, hold) = {
        let mut cfg = mgr.config.lock().unwrap();
        *cfg = candidate;
        if let Err(e) = config::save(&cfg) {
            return (500, json!({"ok": false, "error": e}));
        }
        (cfg.enabled, cfg.hold_wakelock)
    };

    if enabled {
        if hold {
            mgr.acquire_wakelock();
        } else {
            mgr.release_wakelock();
        }
    }
    let payload = mgr.refresh_status();
    (200, json!({"ok": true, "data": payload}))
}

/// POST /api/tailscale/logout — invalidates the node key and disables.
pub fn logout(state: &AppState) -> (u16, Value) {
    let mgr = &state.tailscale;
    if mgr.updating.load(Ordering::SeqCst) {
        return (409, json!({"ok": false, "error": "update in progress"}));
    }

    // The node key lives server-side too — start the daemon if needed so
    // `tailscale logout` can actually invalidate it.
    let mut warning: Option<String> = None;
    if installed() && state_file_exists() {
        if mgr.child_pid.lock().unwrap().is_none() {
            let _ = mgr.ensure_daemon_ready(20);
        }
        if mgr.child_pid.lock().unwrap().is_some() {
            if let Err(e) = cli(&["logout"], 30) {
                warning = Some(format!(
                    "tailscale logout failed ({e}) — remove this machine in the admin console"
                ));
            }
        } else {
            warning = Some(
                "daemon unavailable — node key not invalidated; remove this machine in the admin console"
                    .to_string(),
            );
        }
    }

    let _ = mgr.do_disable();
    let _ = std::fs::remove_file(STATE_FILE);
    let _ = std::fs::remove_file(STATE_BAK);
    *mgr.login_url.lock().unwrap() = None;
    mgr.state_restore_attempted.store(false, Ordering::SeqCst);
    {
        let mut cfg = mgr.config.lock().unwrap();
        cfg.enrolled = false;
        let _ = config::save(&cfg);
    }
    mgr.refresh_status();
    (200, json!({"ok": true, "data": {"status": "logged_out", "warning": warning}}))
}

/// POST /api/tailscale/update — body: {"version": "1.98.9", "sha256": "<hex>"}
/// Installs when absent; upgrades with rollback when present. Runs in the
/// background (multi-minute download on cellular) — poll /status for
/// "updating" and "last_update".
pub fn update(state: &AppState, body: &[u8]) -> (u16, Value) {
    let mgr = &state.tailscale;
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let version = match parsed["version"].as_str() {
        Some(v) if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.') => v.to_string(),
        _ => return (400, json!({"ok": false, "error": "missing or invalid 'version'"})),
    };
    let sha256 = match parsed["sha256"].as_str() {
        Some(s) if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) => s.to_lowercase(),
        _ => return (400, json!({"ok": false, "error": "missing or invalid 'sha256' (64 hex chars)"})),
    };

    if mgr
        .updating
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return (409, json!({"ok": false, "error": "update already in progress"}));
    }
    // Refresh so clients polling /status see "updating" immediately (the
    // cache is otherwise served for up to 10s)
    mgr.refresh_status();

    let mgr2 = Arc::clone(&state.tailscale);
    thread::spawn(move || {
        let result = run_update(&mgr2, &version, &sha256);
        if let Err(ref e) = result {
            eprintln!("[tailscale] update to {version} failed: {e}");
        }
        *mgr2.last_update.lock().unwrap() = Some(json!({
            "version": version,
            "ok": result.is_ok(),
            "error": result.err(),
            "finished_at": unix_now(),
        }));
        mgr2.updating.store(false, Ordering::SeqCst);
        mgr2.refresh_status();
    });
    (202, json!({"ok": true, "data": {"status": "updating"}}))
}

fn run_update(mgr: &Arc<TailscaleManager>, version: &str, sha256: &str) -> Result<(), String> {
    let tgz_path = "/tmp/tailscale-update.tgz";
    let extract_dir = "/tmp/tailscale-update";
    let result = run_update_inner(mgr, version, sha256, tgz_path, extract_dir);
    // Never leak tmpfs space, whatever the exit path
    let _ = std::fs::remove_file(tgz_path);
    let _ = std::fs::remove_dir_all(extract_dir);
    for name in ["tailscaled", "tailscale"] {
        let _ = std::fs::remove_file(format!("{BIN_DIR}/{name}.new"));
    }
    result
}

fn run_update_inner(
    mgr: &Arc<TailscaleManager>,
    version: &str,
    sha256: &str,
    tgz_path: &str,
    extract_dir: &str,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let url = format!("https://pkgs.tailscale.com/stable/tailscale_{version}_arm64.tgz");

    // 1. Download (streamed to tmpfs, 120 MB cap)
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into();
    let resp = agent.get(&url).call().map_err(|e| format!("download {url}: {e}"))?;
    let mut reader = resp.into_body().into_reader().take(120 * 1024 * 1024);
    let mut out = std::fs::File::create(tgz_path).map_err(|e| format!("create {tgz_path}: {e}"))?;
    std::io::copy(&mut reader, &mut out).map_err(|e| format!("download stream: {e}"))?;
    drop(out);

    // 2. Verify sha256
    let actual = sha256_file(tgz_path)?;
    if actual != sha256 {
        return Err(format!("sha256 mismatch: expected {sha256}, got {actual}"));
    }

    // 3. Extract via busybox tar
    let _ = std::fs::remove_dir_all(extract_dir);
    std::fs::create_dir_all(extract_dir).map_err(|e| format!("mkdir {extract_dir}: {e}"))?;
    let mut tar = Command::new("tar");
    tar.args(["-xzf", tgz_path, "-C", extract_dir]);
    let out = run_with_timeout(tar, 120)?;
    if !out.status.success() {
        return Err(format!("tar extract failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let src_dir = format!("{extract_dir}/tailscale_{version}_arm64");

    // 4. Stage new binaries next to the current ones (same filesystem for
    //    atomic rename) and smoke-test before touching the live install.
    std::fs::create_dir_all(BIN_DIR).map_err(|e| format!("mkdir {BIN_DIR}: {e}"))?;
    for name in ["tailscaled", "tailscale"] {
        let src = format!("{src_dir}/{name}");
        let staged = format!("{BIN_DIR}/{name}.new");
        std::fs::copy(&src, &staged).map_err(|e| format!("stage {name}: {e}"))?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {name}: {e}"))?;
    }
    let _ = std::fs::remove_file(tgz_path);
    let _ = std::fs::remove_dir_all(extract_dir);

    let mut smoke = Command::new(format!("{BIN_DIR}/tailscaled.new"));
    smoke.arg("--version");
    let smoke_out = run_with_timeout(smoke, 15)?;
    if !smoke_out.status.success() {
        return Err("staged tailscaled failed --version smoke test".into());
    }

    // 5. Quiesce the supervisor — a real handshake, not a best-effort wait:
    //    swapping binaries under a live or mid-spawn supervisor can leave the
    //    OLD daemon running while we validate and pin the new version.
    let was_running = mgr.running.load(Ordering::SeqCst);
    let enrolled = mgr.config.lock().unwrap().enrolled;
    if !mgr.quiesce_supervisor() {
        mgr.paused.store(false, Ordering::SeqCst);
        return Err("supervisor did not quiesce — update aborted".into());
    }

    let swap = || -> Result<(), String> {
        for name in ["tailscaled", "tailscale"] {
            let current = format!("{BIN_DIR}/{name}");
            let prev = format!("{current}.prev");
            let staged = format!("{current}.new");
            if Path::new(&current).exists() {
                std::fs::rename(&current, &prev).map_err(|e| format!("backup {name}: {e}"))?;
            }
            std::fs::rename(&staged, &current).map_err(|e| format!("install {name}: {e}"))?;
        }
        Ok(())
    };
    let rollback = || {
        for name in ["tailscaled", "tailscale"] {
            let current = format!("{BIN_DIR}/{name}");
            let prev = format!("{current}.prev");
            if Path::new(&prev).exists() {
                let _ = std::fs::rename(&prev, &current);
            }
        }
    };

    if let Err(e) = swap() {
        rollback();
        mgr.paused.store(false, Ordering::SeqCst);
        return Err(e);
    }

    // 6. Resume; if it was enrolled+enabled, require Running within 120s or
    //    roll back. Gate on `enrolled`, not the state file — tailscaled
    //    creates a state file before first login, and an unenrolled node
    //    sits in NeedsLogin forever (which is not a failed update).
    mgr.paused.store(false, Ordering::SeqCst);
    if was_running && enrolled {
        let mut healthy = false;
        for _ in 0..120 {
            thread::sleep(Duration::from_secs(1));
            if let Ok(v) = cli_json(&["status", "--json", "--peers=false"], 10) {
                if v["BackendState"].as_str() == Some("Running") {
                    healthy = true;
                    break;
                }
            }
        }
        if !healthy {
            eprintln!("[tailscale] update: not Running within 120s — rolling back");
            if !mgr.quiesce_supervisor() {
                mgr.paused.store(false, Ordering::SeqCst);
                return Err(
                    "updated tailscaled unhealthy and supervisor did not quiesce for rollback — manual intervention needed"
                        .into(),
                );
            }
            rollback();
            mgr.paused.store(false, Ordering::SeqCst);
            return Err("updated tailscaled did not reach Running in 120s — rolled back".into());
        }
    }

    let mut cfg = mgr.config.lock().unwrap();
    cfg.pinned_version = version.to_string();
    let _ = config::save(&cfg);
    Ok(())
}

fn sha256_file(path: &str) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("read {path}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
