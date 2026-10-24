pub mod config;

pub use config::TsConfig;

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

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
/// WAN-reconnect events closer together than this get one health check.
const WAN_DEBOUNCE: Duration = Duration::from_secs(30);
const CLI_TIMEOUT_SECS: u64 = 20;
/// Shorter than CLI_TIMEOUT_SECS so GET /status can't stall mobile clients
/// for long when the daemon is wedged.
const STATUS_CLI_TIMEOUT_SECS: u64 = 8;
const BACKOFF_MIN_SECS: u64 = 5;
const BACKOFF_MAX_SECS: u64 = 60;
const QUIESCE_TIMEOUT_SECS: u64 = 60;
/// Supervisor park while disabled or paused. Every writer of `running` /
/// `paused` notifies `wake_cv`, so this bound is only a safety net.
const IDLE_PARK_SECS: u64 = 60;
/// How often the monitor checks for an externally killed tailscaled between
/// status polls (the respawn backoff is 5 s, so nothing finer is visible).
const CHILD_POLL_SECS: u64 = 5;
/// LocalAPI over the daemon socket: the same `ipnstate.Status` the CLI
/// prints, without forking the Go binary. HTTP/1.0 so the body ends at EOF
/// (Go never chunks a 1.0 response); the Host is the only one tailscaled
/// accepts over the socket.
const LOCALAPI_STATUS_REQUEST: &str =
    "GET /localapi/v0/status?peers=false HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n";
/// A peerless status is a few KB; anything larger is not tailscaled.
const LOCALAPI_MAX_BYTES: usize = 1024 * 1024;
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
    /// Supervisor wake-up: writers of `running` / `paused` notify so the
    /// thread parks instead of polling. The mutex guards no data; holding
    /// it while notifying orders the flag store before a parked thread's
    /// predicate re-check so a wake cannot be lost.
    wake_lock: Mutex<()>,
    wake_cv: Condvar,
}

/// Logged once per process: LocalAPI failing while the CLI works means the
/// fork-free path is broken, which would otherwise be invisible.
static LOCALAPI_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);

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

#[derive(Debug)]
enum LocalApiError {
    /// tailscaled accepted the connection but did not answer in time. The
    /// CLI talks to the same socket and would stall the same way.
    Timeout,
    Other(String),
}

/// `ipnstate.Status` (peers omitted) from tailscaled's LocalAPI socket.
fn localapi_status(timeout_secs: u64) -> Result<Value, LocalApiError> {
    localapi_status_at(Path::new(SOCK), timeout_secs)
}

fn localapi_status_at(sock: &Path, timeout_secs: u64) -> Result<Value, LocalApiError> {
    use std::io::Write;
    let io_err = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => LocalApiError::Timeout,
        _ => LocalApiError::Other(e.to_string()),
    };
    let timeout = Duration::from_secs(timeout_secs.max(1));
    let deadline = Instant::now() + timeout;
    let mut stream = UnixStream::connect(sock)
        .map_err(|e| LocalApiError::Other(format!("connect {}: {e}", sock.display())))?;
    stream.set_write_timeout(Some(timeout)).map_err(io_err)?;
    // Armed once, up front: tailscaled answers and closes in one go, and
    // setsockopt on a socket whose peer has already closed fails (EINVAL on
    // macOS). The socket timeout bounds each read; the deadline bounds the
    // total should the daemon trickle.
    stream.set_read_timeout(Some(timeout)).map_err(io_err)?;
    stream
        .write_all(LOCALAPI_STATUS_REQUEST.as_bytes())
        .map_err(io_err)?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > LOCALAPI_MAX_BYTES {
                    return Err(LocalApiError::Other(format!(
                        "response over {LOCALAPI_MAX_BYTES} bytes"
                    )));
                }
                if Instant::now() >= deadline {
                    return Err(LocalApiError::Timeout);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_err(e)),
        }
    }
    parse_localapi_response(&raw).map_err(LocalApiError::Other)
}

/// Body of a `200` HTTP/1.x response as JSON. Headers end at the first blank
/// line (CRLF or bare LF); a chunked body is refused rather than decoded
/// because tailscaled never sends one to an HTTP/1.0 request.
fn parse_localapi_response(raw: &[u8]) -> Result<Value, String> {
    let (head_len, sep_len) = (0..raw.len())
        .find_map(|i| {
            if raw[i..].starts_with(b"\r\n\r\n") {
                Some((i, 4))
            } else if raw[i..].starts_with(b"\n\n") {
                Some((i, 2))
            } else {
                None
            }
        })
        .ok_or_else(|| "incomplete HTTP response (no end of headers)".to_string())?;
    let head = String::from_utf8_lossy(&raw[..head_len]);
    let body = &raw[head_len + sep_len..];
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    let code = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(format!("not an HTTP response: {status_line:?}"));
    }
    if code != "200" {
        let text = String::from_utf8_lossy(body);
        return Err(format!("{status_line}: {}", text.trim()));
    }
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("transfer-encoding")
                && value.trim().eq_ignore_ascii_case("chunked")
            {
                return Err("chunked localapi response".into());
            }
        }
    }
    serde_json::from_slice(body).map_err(|e| format!("parse localapi JSON: {e}"))
}

/// `tailscale status --json --peers=false` without the fork: LocalAPI first,
/// the Go CLI on any failure except a timeout, so a LocalAPI problem costs a
/// fork, never a feature, and a wedged daemon still stalls GET /status for
/// at most `timeout_secs` (the CLI would only hit the same wall).
fn daemon_status(timeout_secs: u64) -> Result<Value, String> {
    let reason = match localapi_status(timeout_secs) {
        Ok(v) => return Ok(v),
        Err(LocalApiError::Timeout) => return Err(format!("timed out after {timeout_secs}s")),
        Err(LocalApiError::Other(e)) => e,
    };
    let result = cli_json(&["status", "--json", "--peers=false"], timeout_secs);
    if result.is_ok() && !LOCALAPI_FALLBACK_LOGGED.swap(true, Ordering::Relaxed) {
        eprintln!("[tailscale] localapi status failed ({reason}) — using the CLI");
    }
    result
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
            wake_lock: Mutex::new(()),
            wake_cv: Condvar::new(),
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
        self.wake_supervisor();
    }

    /// WAN-flap hook: on reconnect events, nudge tailscaled (rebind + restun)
    /// and escalate to a daemon restart if it stays unhealthy.
    pub fn start_wan_watch(self: &Arc<Self>, rx: mpsc::Receiver<Value>) {
        let mgr = Arc::clone(self);
        thread::spawn(move || {
            let mut last_handled: Option<Instant> = None;
            for payload in rx.iter() {
                // Only react to reconnects (any `*_connected`, IPv6-only
                // included), not disconnects.
                let wan_status = payload["wan_status"].as_str().unwrap_or("");
                if !wan_connected(wan_status) {
                    continue;
                }
                if !mgr.running.load(Ordering::SeqCst) || mgr.paused.load(Ordering::SeqCst) {
                    continue;
                }
                // Debounce bursts of WAN events
                if debounced(last_handled, Instant::now()) {
                    continue;
                }
                last_handled = Some(Instant::now());
                eprintln!("[tailscale] WAN reconnect ({wan_status}): checking health in 20 s");
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

    /// Wake the supervisor from any park. Callers store their flag first;
    /// taking the lock orders that store before the waiter's predicate
    /// re-check (see `wake_lock`).
    fn wake_supervisor(&self) {
        let _guard = self.wake_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.wake_cv.notify_all();
    }

    /// Park for `dur`, returning early once a disable or an update pause
    /// means the supervisor should stop what it is doing.
    fn park_while_active(&self, dur: Duration) {
        let guard = self.wake_lock.lock().unwrap_or_else(|e| e.into_inner());
        drop(
            self.wake_cv
                .wait_timeout_while(guard, dur, |_| {
                    self.running.load(Ordering::SeqCst) && !self.paused.load(Ordering::SeqCst)
                })
                .unwrap_or_else(|e| e.into_inner()),
        );
    }

    /// Undo `quiesce_supervisor`: the supervisor respawns at once instead
    /// of on its next idle tick.
    fn resume_supervisor(&self) {
        self.paused.store(false, Ordering::SeqCst);
        self.wake_supervisor();
    }

    fn supervise(&self) {
        let mut backoff = BACKOFF_MIN_SECS;
        loop {
            if !self.running.load(Ordering::SeqCst) || self.paused.load(Ordering::SeqCst) {
                // Park until enable/resume notifies (bounded as a safety net)
                self.supervisor_idle.store(true, Ordering::SeqCst);
                let guard = self.wake_lock.lock().unwrap_or_else(|e| e.into_inner());
                drop(
                    self.wake_cv
                        .wait_timeout_while(guard, Duration::from_secs(IDLE_PARK_SECS), |_| {
                            !self.running.load(Ordering::SeqCst)
                                || self.paused.load(Ordering::SeqCst)
                        })
                        .unwrap_or_else(|e| e.into_inner()),
                );
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
            self.park_while_active(Duration::from_secs(backoff));
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

            // Park until the next status poll. Disable/update/kill_child
            // notify the condvar; the child is re-checked every
            // CHILD_POLL_SECS. A wake never resets the deadline, so the
            // 30 s status cadence and the bad_cycles ladder are unchanged.
            let deadline = Instant::now() + Duration::from_secs(POLL_SECS);
            loop {
                if !self.running.load(Ordering::SeqCst)
                    || self.paused.load(Ordering::SeqCst)
                    || matches!(child.try_wait(), Ok(Some(_)) | Err(_))
                {
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let guard = self.wake_lock.lock().unwrap_or_else(|e| e.into_inner());
                drop(
                    self.wake_cv
                        .wait_timeout(guard, remaining.min(Duration::from_secs(CHILD_POLL_SECS)))
                        .unwrap_or_else(|e| e.into_inner()),
                );
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
        // The monitor would notice the exit within CHILD_POLL_SECS; wake it now
        self.wake_supervisor();
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
        let mut wan_ok = false;
        for tick in 0..90 {
            if !self.running.load(Ordering::SeqCst) || self.paused.load(Ordering::SeqCst) {
                return;
            }
            let clock_ok = unix_now() >= SANE_CLOCK_EPOCH;
            // Each probe forks `ubus`; the clock check is free and runs every tick
            if should_probe_wan(tick) {
                wan_ok = match ubus::call(
                    "zwrt_data",
                    "get_wwaniface",
                    Some(r#"{"source_module":"zte_topsw_data","cid":1}"#),
                ) {
                    Ok(v) => v["connect_status"].as_str().is_some_and(wan_connected),
                    Err(_) => true, // can't tell (e.g. dev machine) — don't block
                };
            }
            if clock_ok && wan_ok {
                return;
            }
            self.park_while_active(Duration::from_secs(1));
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
            if daemon_status(5).is_ok() {
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

        let status = match daemon_status(STATUS_CLI_TIMEOUT_SECS) {
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

    /// Host names the HTTP Host allowlist accepts for the tailnet: the
    /// configured hostname, the MagicDNS name and its first label (which
    /// differs from the hostname after a collision or an admin rename),
    /// lowercased and de-duplicated; empty while Tailscale is disabled.
    /// Never blocks: `None` if `config` (held across flash writes) or
    /// `last_status` is locked or poisoned; the caller keeps its last answer.
    pub fn ui_host_names(&self) -> Option<Vec<String>> {
        let hostname = {
            let cfg = self.config.try_lock().ok()?;
            if !cfg.enabled {
                return Some(Vec::new());
            }
            cfg.hostname.to_ascii_lowercase()
        };
        let dns_name = {
            let status = self.last_status.try_lock().ok()?;
            status.0["dns_name"].as_str().map(str::to_ascii_lowercase)
        };
        let mut names = vec![hostname];
        if let Some(dns) = dns_name {
            let dns = dns.strip_suffix('.').unwrap_or(&dns);
            names.push(dns.to_owned());
            names.push(dns.split('.').next().unwrap_or_default().to_owned());
        }
        let mut unique: Vec<String> = Vec::with_capacity(names.len());
        for n in names {
            if !n.is_empty() && !unique.contains(&n) {
                unique.push(n);
            }
        }
        Some(unique)
    }

    /// A manager with the given config and cached status, for tests of
    /// callers (no file is read, nothing is started).
    #[cfg(test)]
    pub(crate) fn for_test(cfg: TsConfig, status: Value) -> Self {
        let mgr = Self::new();
        *mgr.config.lock().unwrap() = cfg;
        *mgr.last_status.lock().unwrap() = (status, 0);
        mgr
    }

    /// Holds `config` as a flash write would, for contention tests.
    #[cfg(test)]
    pub(crate) fn lock_config_for_test(&self) -> std::sync::MutexGuard<'_, TsConfig> {
        self.config.lock().unwrap()
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
        self.wake_supervisor();
        for _ in 0..wait_secs {
            if daemon_status(5).is_ok() {
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
        self.wake_supervisor();
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
        self.wake_supervisor();
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
        self.wake_supervisor();
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

/// `connect_status` of `zwrt_data get_wwaniface` (or `wan_status` of
/// `router_event_wan_connect_status`) with a data session up. The firmware
/// reports `ipv4_connected` / `ipv6_connected` / `ipv4_ipv6_connected` (and
/// `disconnected` / `ipv4_disconnected` / `connecting`, which must not match);
/// older notes show a bare `connected`.
fn wan_connected(status: &str) -> bool {
    status == "connected" || status.ends_with("_connected")
}

/// WAN reconnects within [`WAN_DEBOUNCE`] of the last handled one share its
/// health check. Monotonic: the carrier's NITZ wall-clock jump (+8 h, landing
/// right at reconnect) must not suppress the checks that follow.
fn debounced(last: Option<Instant>, now: Instant) -> bool {
    last.is_some_and(|t| now.saturating_duration_since(t) < WAN_DEBOUNCE)
}

/// WAN probe schedule for the 90-tick startup gate: three quick probes, then
/// one every 10 s (11 forks per gate instead of 90 while WAN is down).
fn should_probe_wan(tick: u32) -> bool {
    tick < 3 || tick % 10 == 0
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
        mgr.resume_supervisor();
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
        mgr.resume_supervisor();
        return Err(e);
    }

    // 6. Resume; if it was enrolled+enabled, require Running within 120s or
    //    roll back. Gate on `enrolled`, not the state file — tailscaled
    //    creates a state file before first login, and an unenrolled node
    //    sits in NeedsLogin forever (which is not a failed update).
    mgr.resume_supervisor();
    if was_running && enrolled {
        let mut healthy = false;
        for _ in 0..120 {
            thread::sleep(Duration::from_secs(1));
            if let Ok(v) = daemon_status(10) {
                if v["BackendState"].as_str() == Some("Running") {
                    healthy = true;
                    break;
                }
            }
        }
        if !healthy {
            eprintln!("[tailscale] update: not Running within 120s — rolling back");
            if !mgr.quiesce_supervisor() {
                mgr.resume_supervisor();
                return Err(
                    "updated tailscaled unhealthy and supervisor did not quiesce for rollback — manual intervention needed"
                        .into(),
                );
            }
            rollback();
            mgr.resume_supervisor();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    /// Tab-indented like tailscaled's encoder (`SetIndent("", "\t")`).
    const STATUS_BODY: &str = "{\n\t\"Version\": \"1.86.2\",\n\t\"BackendState\": \"Running\",\n\t\"Self\": {\n\t\t\"Online\": true,\n\t\t\"DNSName\": \"u60-pro.tail.ts.net.\"\n\t},\n\t\"Health\": []\n}\n";

    #[test]
    fn wan_gate_accepts_every_connected_variant() {
        for s in [
            "connected",
            "ipv4_connected",
            "ipv6_connected",
            "ipv4_ipv6_connected",
        ] {
            assert!(wan_connected(s), "{s}");
        }
        for s in [
            "",
            "disconnected",
            "connecting",
            "init",
            "ipv4_disconnected",
        ] {
            assert!(!wan_connected(s), "{s}");
        }
    }

    #[test]
    fn wan_watch_debounce_is_monotonic() {
        let start = Instant::now();
        assert!(!debounced(None, start));
        assert!(debounced(Some(start), start + Duration::from_secs(10)));
        assert!(!debounced(Some(start), start + Duration::from_secs(31)));
        // A `now` that reads earlier than the stamp still debounces, never panics.
        assert!(debounced(Some(start + Duration::from_secs(1)), start));
    }

    #[test]
    fn wan_gate_probes_eleven_times_per_ninety_ticks() {
        let probes: Vec<u32> = (0..90).filter(|&t| should_probe_wan(t)).collect();
        assert_eq!(probes, [0, 1, 2, 10, 20, 30, 40, 50, 60, 70, 80]);
    }

    #[test]
    fn parses_a_200_with_crlf_headers() {
        let raw = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{STATUS_BODY}"
        );
        let v = parse_localapi_response(raw.as_bytes()).unwrap();
        assert_eq!(v["BackendState"], "Running");
        assert_eq!(v["Self"]["Online"], true);
        assert_eq!(v["Self"]["DNSName"], "u60-pro.tail.ts.net.");
        assert_eq!(v["Version"], "1.86.2");
    }

    #[test]
    fn parses_bare_lf_headers_and_an_http11_status_line() {
        let raw = format!("HTTP/1.1 200 OK\nContent-Type: application/json\n\n{STATUS_BODY}");
        let v = parse_localapi_response(raw.as_bytes()).unwrap();
        assert_eq!(v["BackendState"], "Running");
    }

    #[test]
    fn body_is_everything_after_the_first_blank_line() {
        let body = "{\n\t\"BackendState\": \"NeedsLogin\",\n\n\t\"AuthURL\": \"https://login.tailscale.com/a/abc\"\n}";
        let raw = format!("HTTP/1.0 200 OK\r\n\r\n{body}");
        let v = parse_localapi_response(raw.as_bytes()).unwrap();
        assert_eq!(v["AuthURL"], "https://login.tailscale.com/a/abc");
    }

    #[test]
    fn rejects_non_200_incomplete_chunked_and_non_http() {
        let forbidden =
            b"HTTP/1.0 403 Forbidden\r\nContent-Type: text/plain\r\n\r\ninvalid localapi request\n";
        let err = parse_localapi_response(forbidden).unwrap_err();
        assert!(
            err.contains("403") && err.contains("invalid localapi request"),
            "{err}"
        );
        assert!(
            parse_localapi_response(b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n")
                .unwrap_err()
                .contains("incomplete")
        );
        assert!(
            parse_localapi_response(b"HTTP/1.0 200 OK\r\n\r\n{\"Backend")
                .unwrap_err()
                .contains("parse localapi JSON")
        );
        let chunked = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{STATUS_BODY}");
        assert!(parse_localapi_response(chunked.as_bytes())
            .unwrap_err()
            .contains("chunked"));
        assert!(parse_localapi_response(b"").is_err());
        assert!(parse_localapi_response(b"garbage\r\n\r\n{}")
            .unwrap_err()
            .contains("not an HTTP response"));
    }

    fn sock_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("zte-ts-{}-{name}.sock", std::process::id()))
    }

    /// One-shot fake tailscaled: records the request head, then answers
    /// `response` and closes (HTTP/1.0 semantics) or, given an empty
    /// response, stays silent past the client's deadline.
    fn serve_once(path: &Path, response: &'static str) -> thread::JoinHandle<Vec<String>> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut head = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                head.push(line.trim_end().to_string());
            }
            if response.is_empty() {
                thread::sleep(Duration::from_secs(3));
            } else {
                let stream = reader.get_mut();
                stream.write_all(response.as_bytes()).unwrap();
                stream.shutdown(std::net::Shutdown::Both).unwrap();
            }
            head
        })
    }

    #[test]
    fn localapi_round_trip_over_a_unix_socket() {
        let path = sock_path("ok");
        let server = serve_once(
            &path,
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"BackendState\": \"Running\", \"Self\": {\"Online\": true}}\n",
        );
        let v = localapi_status_at(&path, 5).unwrap();
        assert_eq!(v["BackendState"], "Running");
        let head = server.join().unwrap();
        assert_eq!(head[0], "GET /localapi/v0/status?peers=false HTTP/1.0");
        assert!(
            head.contains(&"Host: local-tailscaled.sock".to_string()),
            "{head:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_silent_daemon_is_a_timeout_not_a_cli_fallback() {
        let path = sock_path("silent");
        let _server = serve_once(&path, "");
        let started = Instant::now();
        assert!(matches!(
            localapi_status_at(&path, 1),
            Err(LocalApiError::Timeout)
        ));
        assert!(started.elapsed() < Duration::from_secs(3));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_socket_is_an_ordinary_error() {
        let path = sock_path("missing");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(
            localapi_status_at(&path, 1),
            Err(LocalApiError::Other(_))
        ));
    }
}
