//! Qualcomm RIL socket transport verified on U60 Pro, 2026-09-14.
//! Initialization is verified live; menu/response flows are covered by protocol fixtures.
mod codec;

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{mpsc, Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codec::Command;
use serde_json::{json, Value};

const SOCKET: &str = "/dev/socket/qcrild/rild0";
const TIMEOUT: Duration = Duration::from_secs(8);
/// How long the SIM may stay silent after it acknowledged a selection before the session is dropped.
const WAITING_LIMIT: Duration = Duration::from_secs(30);
type Reply = (u16, Value);
type Sender = mpsc::Sender<Reply>;

struct Request {
    action: &'static str,
    body: Value,
    reply: Sender,
}

/// Handle to the `stk-ril` thread: the bounded request queue plus the write end of the self-pipe
/// that interrupts the worker's `poll()`. Dropping it disconnects the queue and hangs up the pipe.
struct Worker {
    tx: mpsc::SyncSender<Request>,
    wake: File,
}

/// Called on the worker thread when what `GET /api/stk/session` answers changes.
pub type OnChange = Arc<dyn Fn() + Send + Sync>;

/// What `GET /api/stk/session` answers, as the worker last published it.
type Shown = Arc<Mutex<Option<Reply>>>;

/// One owner of the RIL stream prevents competing readers and crossed STK sessions.
pub struct StkService {
    worker: Mutex<Option<Worker>>,
    on_change: OnceLock<OnChange>,
    shown: Shown,
}

impl StkService {
    pub fn new() -> Self {
        Self {
            worker: Mutex::new(None),
            on_change: OnceLock::new(),
            shown: Arc::new(Mutex::new(None)),
        }
    }

    /// What `GET /api/stk/session` answers, from memory: the worker's last
    /// published state (no worker yet: no session). Never starts the worker
    /// or waits for it (the live hub's `stk` topic reads this).
    #[allow(dead_code)] // the stk_probe example has no hub
    pub fn session_view(&self) -> Reply {
        self.shown.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_else(|| error(503, ""))
    }

    /// Tell `f` (the live hub's `stk` topic) whenever the session's state changes: a SIM
    /// command, a reply, the session ending. A status read changes nothing, so it never calls.
    /// Set once, before the first request.
    #[allow(dead_code)] // the stk_probe example has no hub
    pub fn on_change(&self, f: OnChange) {
        let _ = self.on_change.set(f);
    }

    pub fn request(&self, action: &'static str, body: &[u8]) -> Reply {
        let body = if body.is_empty() {
            json!({})
        } else {
            match serde_json::from_slice(body) {
                Ok(v) => v,
                Err(_) => return error(400, "invalid JSON"),
            }
        };
        let (reply, rx) = mpsc::channel();
        let Ok(mut guard) = self.worker.lock() else {
            return error(503, "STK worker unavailable");
        };
        if guard.is_none() {
            let (tx, rx) = mpsc::sync_channel(16);
            let (wake_rx, wake_tx) = match wake_pipe() {
                Ok(pipe) => pipe,
                Err(e) => {
                    eprintln!("[stk] cannot create worker wake pipe: {e}");
                    return error(503, "cannot start STK worker");
                }
            };
            let changed = self.on_change.get().cloned();
            let shown = Arc::clone(&self.shown);
            if std::thread::Builder::new()
                .name("stk-ril".into())
                .spawn(move || run(rx, wake_rx, SOCKET, changed, &shown))
                .is_err()
            {
                return error(503, "cannot start STK worker");
            }
            *guard = Some(Worker { tx, wake: wake_tx });
        }
        let Some(worker) = guard.as_ref() else {
            return error(503, "STK worker unavailable");
        };
        match worker.tx.try_send(Request {
            action,
            body,
            reply,
        }) {
            Ok(()) => {
                // Queue first, then wake: the worker drains the pipe before the queue, so a byte
                // written here is never consumed by a drain that ran before the request was queued.
                // A full pipe (EAGAIN) already guarantees a wakeup, so the result is irrelevant.
                let _ = (&worker.wake).write(&[1]);
            }
            Err(mpsc::TrySendError::Full(_)) => return error(503, "STK request queue busy"),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                *guard = None;
                *self.shown.lock().unwrap_or_else(PoisonError::into_inner) = None;
                return error(503, "STK worker stopped; retry loading the menu");
            }
        }
        drop(guard);
        rx.recv_timeout(TIMEOUT + Duration::from_secs(2))
            .unwrap_or_else(|_| {
                error(
                    504,
                    "STK request timed out; do not repeat a selection without reloading",
                )
            })
    }
}

fn error(status: u16, message: &str) -> Reply {
    (status, json!({"ok":false,"error":message}))
}

/// Non-blocking self-pipe `(read end, write end)`. Both ends are close-on-exec so the agent's
/// many child processes never inherit them.
fn wake_pipe() -> std::io::Result<(File, File)> {
    let mut fds: [libc::c_int; 2] = [-1, -1];
    #[cfg(target_os = "linux")]
    let rc = {
        // SAFETY: `fds` is writable storage for two descriptors for the duration of the call.
        unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) }
    };
    #[cfg(not(target_os = "linux"))]
    let rc = {
        // Host builds only (the router is Linux): emulate pipe2 with pipe + fcntl.
        // SAFETY: `fds` is writable storage for two descriptors; fcntl only touches those fds.
        unsafe {
            let rc = libc::pipe(fds.as_mut_ptr());
            if rc == 0 {
                for fd in fds {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
                }
            }
            rc
        }
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the pipe call succeeded, so both descriptors are open and owned by nobody else.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    Ok((File::from(read), File::from(write)))
}

/// Empties the wake pipe so the next `poll()` only returns for requests queued after this point.
fn drain_wake(mut wake: &File) {
    let mut sink = [0u8; 64];
    while matches!(wake.read(&mut sink), Ok(n) if n > 0) {}
}

/// `poll()` timeout in milliseconds for the next deadline, rounded up so the worker never wakes a
/// hair before the deadline and spins; `None` means block indefinitely (-1).
fn poll_timeout_ms(next_wake: Option<Instant>, now: Instant) -> libc::c_int {
    let Some(at) = next_wake else {
        return -1;
    };
    let ms = at
        .saturating_duration_since(now)
        .as_nanos()
        .div_ceil(1_000_000);
    ms.min(libc::c_int::MAX as u128) as libc::c_int
}

/// Blocks until the RIL socket has data (or hung up), the wake pipe is written, or `timeout_ms`
/// elapses (-1 = never). Returns whether the socket should be read.
fn wait_readable(stream: RawFd, wake: RawFd, timeout_ms: libc::c_int) -> Result<bool, String> {
    let mut fds = [
        libc::pollfd {
            fd: stream,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: `fds` is a valid array of two pollfd entries that outlives the call.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
    if rc < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(format!("RIL poll failed: {e}"));
    }
    if fds[0].revents & libc::POLLNVAL != 0 {
        return Err("RIL socket descriptor is invalid".into());
    }
    // POLLHUP / POLLERR are reported through the read (EOF or an error) so the messages stay the same.
    Ok(fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
}

struct Pending {
    deadline: Instant,
    reply: Option<Sender>,
    generation: u64,
    init: bool,
}

struct Session {
    stream: UnixStream,
    buffer: Vec<u8>,
    token: u64,
    generation: u64,
    root: Option<Command>,
    current: Option<Command>,
    phase: &'static str,
    reason: String,
    pending: HashMap<u64, Pending>,
    waiting_since: Option<Instant>,
}

fn record(id: i32, token: u64, payload: &[u8]) -> Vec<u8> {
    // Qualcomm's request header uses native little endian; the record and Marshal use big endian.
    let mut out = ((12 + payload.len()) as u32).to_be_bytes().to_vec();
    out.extend(id.to_le_bytes());
    out.extend(token.to_le_bytes());
    out.extend(payload);
    out
}

fn marshal_hex(raw: &[u8]) -> Vec<u8> {
    let hex = codec::hex(raw);
    let mut out = (hex.len() as u32).to_be_bytes().to_vec();
    out.extend(hex.as_bytes());
    out
}

fn number(data: &[u8], offset: usize) -> Result<u32, String> {
    let b = data
        .get(offset..offset + 4)
        .ok_or("truncated RIL integer")?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

impl Session {
    fn connect(path: &str) -> Result<Self, String> {
        let stream =
            UnixStream::connect(path).map_err(|e| format!("RIL connection failed: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        let generation = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut session = Self {
            stream,
            buffer: Vec::new(),
            token: 0,
            generation,
            root: None,
            current: None,
            phase: "initializing",
            reason: "Connecting to SIM Toolkit".into(),
            pending: HashMap::new(),
            waiting_since: None,
        };
        session.send(103, &[], None, true)?;
        Ok(session)
    }

    fn snapshot(&self) -> Reply {
        let mut value = self
            .current
            .as_ref()
            .map(Command::view)
            .unwrap_or_else(|| json!({"type":"status","items":[]}));
        value["source"] = json!("qualcomm_ril");
        // A working RIL backend is not proof that the inserted SIM has a toolkit application.
        value["supported"] = if self.root.is_some() || self.current.is_some() {
            json!(true)
        } else {
            Value::Null
        };
        value["transport_available"] = json!(true);
        value["status"] = json!(self.phase);
        value["reason"] = json!(self.reason);
        value["command_id"] = json!(self.generation.to_string());
        value["busy"] = json!(!self.pending.is_empty() || self.phase == "waiting");
        (200, json!({"ok":true,"data":value}))
    }

    fn send(
        &mut self,
        id: i32,
        raw: &[u8],
        reply: Option<Sender>,
        init: bool,
    ) -> Result<(), String> {
        self.token += 1;
        let payload = if id == 103 {
            Vec::new()
        } else {
            marshal_hex(raw)
        };
        if let Err(e) = self.stream.write_all(&record(id, self.token, &payload)) {
            if let Some(reply) = reply {
                let _ = reply.send(error(503, "RIL write failed; session invalidated"));
            }
            return Err(format!("RIL write failed: {e}"));
        }
        self.pending.insert(
            self.token,
            Pending {
                deadline: Instant::now() + TIMEOUT,
                reply,
                generation: self.generation,
                init,
            },
        );
        Ok(())
    }

    fn handle_request(&mut self, req: Request) -> Result<(), String> {
        if matches!(req.action, "menu" | "status") {
            if req.action == "menu" && self.phase == "menu_unavailable" && self.pending.is_empty() {
                self.phase = "initializing";
                self.reason = "Requesting the SIM Toolkit menu".into();
                self.send(103, &[], None, true)?;
            }
            let _ = req.reply.send(self.snapshot());
            return Ok(());
        }
        if !self.pending.is_empty() || self.phase == "waiting" {
            let _ = req
                .reply
                .send(error(409, "SIM request in progress; refresh the session"));
            return Ok(());
        }
        if req.body["command_id"].as_str() != Some(&self.generation.to_string()) {
            let _ = req.reply.send(error(
                409,
                "stale or missing command_id; reload the SIM menu",
            ));
            return Ok(());
        }
        let Some(cmd) = self.current.as_ref() else {
            let _ = req.reply.send(error(409, "no SIM command is available"));
            return Ok(());
        };
        let result = match req.action {
            "select" => req.body["item_id"]
                .as_u64()
                .and_then(|v| u8::try_from(v).ok())
                .ok_or_else(|| "item_id must be 1–255".to_string())
                .and_then(|id| cmd.select(id)),
            "respond" => cmd.input_response(&req.body).map(|raw| (70, raw)),
            "cancel" if cmd.details[1] != 0x25 => Ok((70, cmd.response(0x10, &[]))),
            "back" if cmd.details[1] != 0x25 => Ok((70, cmd.response(0x11, &[]))),
            _ => Err("no pending command for this action".into()),
        };
        match result {
            Ok((id, raw)) => self.send(id, &raw, Some(req.reply), false),
            Err(e) => {
                let _ = req.reply.send(error(400, &e));
                Ok(())
            }
        }
    }

    fn frame(&mut self, frame: &[u8]) -> Result<(), String> {
        match number(frame, 0)? {
            0 => {
                let token_bytes: [u8; 8] = frame
                    .get(4..12)
                    .ok_or("truncated RIL token")?
                    .try_into()
                    .map_err(|_| "invalid RIL token")?;
                let token = u64::from_be_bytes(token_bytes);
                let errno = number(frame, 12)?;
                let Some(p) = self.pending.remove(&token) else {
                    return Ok(());
                };
                if errno != 0 {
                    let message = format!("RIL rejected STK request (error {errno})");
                    if let Some(reply) = p.reply {
                        let _ = reply.send(error(502, &message));
                    }
                    return Err(message);
                }
                if p.init && self.current.is_none() {
                    self.phase = "menu_unavailable";
                    self.reason =
                        "The modem accepted SIM Toolkit initialization but has not supplied a menu"
                            .into();
                } else if p.reply.is_some() && self.generation == p.generation {
                    self.current = None;
                    self.generation += 1;
                    self.phase = "waiting";
                    self.reason = "Waiting for the SIM".into();
                    self.waiting_since = Some(Instant::now());
                }
                if let Some(reply) = p.reply {
                    let _ = reply.send(self.snapshot());
                }
            }
            1 => match number(frame, 4)? {
                1012 => {
                    // Session end: the root menu is cached, submenus are not.
                    self.current = self.root.clone();
                    self.generation += 1;
                    self.phase = if self.current.is_some() {
                        "ready"
                    } else {
                        "menu_unavailable"
                    };
                    self.reason = "SIM session ended".into();
                    self.waiting_since = None;
                }
                1013 => {
                    let len = number(frame, 8)? as usize;
                    if len > 16384 {
                        return Err("oversized proactive command".into());
                    }
                    let text = std::str::from_utf8(
                        frame
                            .get(12..12 + len)
                            .ok_or("truncated proactive command")?,
                    )
                    .map_err(|_| "invalid proactive text")?;
                    let cmd = Command::parse(&codec::unhex(text.trim_end_matches('\0'))?)?;
                    self.proactive(cmd)?;
                }
                // Refresh/removal changes the SIM identity or toolkit contents. Discard all cached menus.
                1017 => return Err("SIM refreshed; reload the toolkit session".into()),
                1014 | 1015 => {
                    // Event notifications are not proactive requests. Do not acknowledge or approve calls.
                    self.reason =
                        "SIM event received; no automatic service approval was sent".into();
                }
                _ => {} // Other RIL indications are deliberately ignored (and never logged).
            },
            _ => return Err("unknown RIL frame type".into()),
        }
        Ok(())
    }

    fn proactive(&mut self, cmd: Command) -> Result<(), String> {
        self.generation += 1;
        self.waiting_since = None;
        match cmd.details[1] {
            0x25 => {
                let response = cmd.response(0, &[]);
                self.root = if cmd.items.is_empty() {
                    None
                } else {
                    Some(cmd.clone())
                };
                self.current = self.root.clone();
                self.phase = if self.root.is_some() {
                    "ready"
                } else {
                    "menu_unavailable"
                };
                self.reason = if self.root.is_some() {
                    String::new()
                } else {
                    "SIM removed its toolkit menu".into()
                };
                self.send(70, &response, None, false)?;
            }
            0x21..=0x24 => {
                self.current = Some(cmd);
                self.phase = "ready";
                self.reason.clear();
            }
            _ => {
                // No automatic calls, SMS, USSD, browser launches, or channel opening.
                let response = cmd.response(0x30, &[]);
                self.phase = "unsupported_command";
                self.reason = format!("SIM command {:02X} is not implemented", cmd.details[1]);
                self.current = None;
                self.send(70, &response, None, false)?;
            }
        }
        Ok(())
    }

    /// One `read()` on the RIL socket (called once `poll()` reports it readable), then every
    /// complete record in the buffer is dispatched. A partial trailing record is kept for later.
    fn read_socket(&mut self) -> Result<(), String> {
        let mut bytes = [0; 8192];
        match self.stream.read(&mut bytes) {
            Ok(0) => return Err("RIL socket disconnected".into()),
            Ok(n) => self.buffer.extend(&bytes[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(format!("RIL read failed: {e}")),
        }
        while self.buffer.len() >= 4 {
            let n = number(&self.buffer, 0)? as usize;
            if !(4..=65536).contains(&n) {
                return Err("invalid RIL record length".into());
            }
            if self.buffer.len() < n + 4 {
                break;
            }
            let frame = self.buffer[4..n + 4].to_vec();
            self.buffer.drain(..n + 4);
            self.frame(&frame)?;
        }
        Ok(())
    }

    /// Fails the session when a request outlived `TIMEOUT` or the SIM stayed silent past
    /// `WAITING_LIMIT` after acknowledging a selection.
    fn check_deadlines(&self) -> Result<(), String> {
        let now = Instant::now();
        if self.pending.values().any(|p| now >= p.deadline) {
            return Err("STK response timed out; session invalidated, no automatic retry".into());
        }
        if self.waiting_since.is_some_and(|t| now >= t + WAITING_LIMIT) {
            return Err(
                "SIM did not return a command; reload the session before trying again".into(),
            );
        }
        Ok(())
    }

    /// Earliest instant at which `check_deadlines()` can fail; `None` when nothing is pending,
    /// so the worker may block in `poll()` without a timeout.
    fn next_wake(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|p| p.deadline)
            .chain(self.waiting_since.map(|t| t + WAITING_LIMIT))
            .min()
    }
}

/// Worker loop. Without a session it parks in `recv()`; with one it parks in `poll()` on the RIL
/// socket and the wake pipe, with a timeout only while a deadline is pending, so an idle toolkit
/// session costs no wakeups at all.
fn run(rx: mpsc::Receiver<Request>, wake: File, path: &str, changed: Option<OnChange>, shown: &Mutex<Option<Reply>>) {
    let mut session: Option<Session> = None;
    let mut last_error = String::new();
    let changed = changed.as_deref();
    loop {
        if session.is_none() {
            match rx.recv() {
                Ok(req) => serve(req, &mut session, &mut last_error, path, shown, changed),
                Err(_) => return,
            }
            continue;
        }
        // Drain the pipe before the queue: `request()` queues and then writes, so anything queued
        // after this drain still leaves its byte behind and the poll below returns at once.
        drain_wake(&wake);
        loop {
            match rx.try_recv() {
                Ok(req) => serve(req, &mut session, &mut last_error, path, shown, changed),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        let Some(s) = session.as_mut() else {
            continue;
        };
        let timeout = poll_timeout_ms(s.next_wake(), Instant::now());
        let result = wait_readable(s.stream.as_raw_fd(), wake.as_raw_fd(), timeout)
            .and_then(|readable| if readable { s.read_socket() } else { Ok(()) })
            .and_then(|()| s.check_deadlines());
        if let Err(e) = result {
            invalidate(&mut session, &mut last_error, e);
        }
        publish(&session, &last_error, shown, changed);
    }
}

/// One request, then the state published. A read's ("menu", "status")
/// answer, which is always immediate, goes out after the publish, so a
/// page that opened the menu and then reads the topic sees what it did.
fn serve(mut req: Request, session: &mut Option<Session>, last_error: &mut String, path: &str, shown: &Mutex<Option<Reply>>, changed: Option<&(dyn Fn() + Send + Sync)>) {
    let read = matches!(req.action, "menu" | "status").then(|| {
        let (tx, rx) = mpsc::channel();
        (std::mem::replace(&mut req.reply, tx), rx)
    });
    dispatch(req, session, last_error, path);
    publish(session, last_error, shown, changed);
    if let Some((reply, rx)) = read {
        if let Ok(answer) = rx.try_recv() {
            let _ = reply.send(answer);
        }
    }
}

/// What `GET /api/stk/session` answers now.
fn view(session: &Option<Session>, last_error: &str) -> Reply {
    match session {
        Some(s) => s.snapshot(),
        None => error(503, last_error),
    }
}

/// The hub's `stk` topic follows the worker (`StkService::on_change`): what
/// `GET /api/stk/session` answers is kept in `shown` (the topic reads it from memory,
/// `StkService::session_view`), and when it changes (a SIM command, a reply, the session
/// ending) the hub is told, so a watching stream re-reads the topic. A read changes nothing,
/// so it never calls again; with no stream open the hub reads nothing.
fn publish(session: &Option<Session>, last_error: &str, shown: &Mutex<Option<Reply>>, changed: Option<&(dyn Fn() + Send + Sync)>) {
    let now = view(session, last_error);
    let mut cur = shown.lock().unwrap_or_else(PoisonError::into_inner);
    if cur.as_ref() != Some(&now) {
        *cur = Some(now);
        drop(cur);
        if let Some(f) = changed {
            f();
        }
    }
}

/// Connect only for an explicit read. Never reconnect and replay a selection after a failure.
fn dispatch(req: Request, session: &mut Option<Session>, last_error: &mut String, path: &str) {
    if session.is_none() && req.action == "menu" {
        match Session::connect(path) {
            Ok(s) => *session = Some(s),
            Err(e) => *last_error = e,
        }
    }
    if let Some(s) = session.as_mut() {
        if let Err(e) = s.handle_request(req) {
            invalidate(session, last_error, e);
        }
    } else {
        let _ = req.reply.send(error(503, last_error));
    }
}

fn invalidate(session: &mut Option<Session>, last_error: &mut String, message: String) {
    if let Some(s) = session.take() {
        for (_, pending) in s.pending {
            if let Some(reply) = pending.reply {
                let _ = reply.send(error(503, &message));
            }
        }
    }
    *last_error = message;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initialization_matches_live_verified_packet() {
        assert_eq!(
            codec::hex(&record(103, 1, &[])),
            "0000000C670000000100000000000000"
        );
    }
    #[test]
    fn marshal_string_uses_big_endian_length_and_ascii_hex() {
        assert_eq!(marshal_hex(&[0xd3, 7]), b"\0\0\0\x04D307");
    }
    fn session() -> (Session, UnixStream) {
        let (stream, peer) = UnixStream::pair().unwrap();
        (
            Session {
                stream,
                buffer: vec![],
                token: 0,
                generation: 1,
                root: None,
                current: None,
                phase: "initializing",
                reason: String::new(),
                pending: HashMap::new(),
                waiting_since: None,
            },
            peer,
        )
    }
    /// The live topic reads the worker's state from memory: nothing before
    /// a worker exists (never started by the read); a read's answer goes out
    /// after the state is published, so what the page reads next matches it.
    #[test]
    fn the_session_view_is_read_from_memory() {
        let service = StkService::new();
        assert_eq!(service.session_view(), error(503, ""));
        assert!(service.worker.lock().unwrap().is_none());
        let (s, _peer) = session();
        let mut live = Some(s);
        let mut last_error = String::new();
        let (tx, rx) = mpsc::channel();
        serve(Request { action: "status", body: json!({}), reply: tx }, &mut live, &mut last_error, "", &service.shown, None);
        let answer = rx.recv().unwrap();
        assert_eq!((answer.0, service.session_view()), (200, answer.clone()));
        // A failed session: the route's answer, from memory.
        invalidate(&mut live, &mut last_error, "RIL socket disconnected".into());
        let (tx, rx) = mpsc::channel();
        serve(Request { action: "status", body: json!({}), reply: tx }, &mut live, &mut last_error, "", &service.shown, None);
        assert_eq!((rx.recv().unwrap(), service.session_view()), (view(&None, &last_error), view(&None, &last_error)));
    }

    #[test]
    fn successful_initialization_does_not_claim_sim_support() {
        let (mut s, _peer) = session();
        s.pending.insert(
            1,
            Pending {
                deadline: Instant::now() + TIMEOUT,
                reply: None,
                generation: 1,
                init: true,
            },
        );
        s.frame(&codec::unhex("00000000000000000000000100000000").unwrap())
            .unwrap();
        assert_eq!(s.snapshot().1["data"]["status"], "menu_unavailable");
        assert!(s.snapshot().1["data"]["supported"].is_null());
    }
    #[test]
    fn stale_selection_never_reaches_modem() {
        let (mut s, _peer) = session();
        let (tx, rx) = mpsc::channel();
        s.handle_request(Request {
            action: "select",
            body: json!({"item_id":1,"command_id":"old"}),
            reply: tx,
        })
        .unwrap();
        assert_eq!(rx.recv().unwrap().0, 409);
        assert!(s.pending.is_empty());
    }
    #[test]
    fn partial_socket_frames_are_preserved_across_reads() {
        let (mut s, mut peer) = session();
        s.pending.insert(
            1,
            Pending {
                deadline: Instant::now() + TIMEOUT,
                reply: None,
                generation: 1,
                init: true,
            },
        );
        let response = codec::unhex("0000001000000000000000000000000100000000").unwrap();
        peer.write_all(&response[..7]).unwrap();
        s.read_socket().unwrap();
        s.check_deadlines().unwrap();
        assert_eq!(s.phase, "initializing");
        peer.write_all(&response[7..]).unwrap();
        s.read_socket().unwrap();
        s.check_deadlines().unwrap();
        assert_eq!(s.phase, "menu_unavailable");
    }
    #[test]
    fn idle_session_blocks_in_poll_without_a_timeout() {
        let (s, _peer) = session();
        assert_eq!(s.next_wake(), None);
        assert_eq!(poll_timeout_ms(s.next_wake(), Instant::now()), -1);
        assert!(s.check_deadlines().is_ok());
    }
    #[test]
    fn pending_deadline_yields_a_bounded_poll_timeout() {
        let (mut s, _peer) = session();
        let now = Instant::now();
        s.pending.insert(
            1,
            Pending {
                deadline: now + TIMEOUT,
                reply: None,
                generation: 1,
                init: true,
            },
        );
        assert_eq!(s.next_wake(), Some(now + TIMEOUT));
        let ms = poll_timeout_ms(s.next_wake(), now);
        assert!(
            (1..=TIMEOUT.as_millis() as libc::c_int).contains(&ms),
            "{ms}"
        );
        // A silent SIM has its own, longer limit; the earliest deadline wins.
        s.waiting_since = Some(now);
        assert_eq!(s.next_wake(), Some(now + TIMEOUT));
        s.pending.clear();
        assert_eq!(s.next_wake(), Some(now + WAITING_LIMIT));
        assert_eq!(
            poll_timeout_ms(s.next_wake(), now),
            WAITING_LIMIT.as_millis() as libc::c_int
        );
    }
    #[test]
    fn poll_timeout_rounds_up_and_never_goes_negative() {
        let now = Instant::now();
        assert_eq!(poll_timeout_ms(Some(now), now), 0);
        assert_eq!(poll_timeout_ms(Some(now - Duration::from_secs(5)), now), 0);
        assert_eq!(
            poll_timeout_ms(Some(now + Duration::from_micros(1)), now),
            1
        );
        assert_eq!(
            poll_timeout_ms(Some(now + Duration::from_micros(1500)), now),
            2
        );
        assert_eq!(
            poll_timeout_ms(Some(now + Duration::from_secs(1 << 40)), now),
            libc::c_int::MAX
        );
    }
    #[test]
    fn expired_deadlines_fail_the_session() {
        let (mut s, _peer) = session();
        s.pending.insert(
            1,
            Pending {
                deadline: Instant::now() - Duration::from_millis(1),
                reply: None,
                generation: 1,
                init: false,
            },
        );
        assert!(s.check_deadlines().unwrap_err().contains("timed out"));
        s.pending.clear();
        s.waiting_since = Some(Instant::now() - WAITING_LIMIT - Duration::from_millis(1));
        assert!(s.check_deadlines().unwrap_err().contains("did not return"));
        s.waiting_since = Some(Instant::now());
        assert!(s.check_deadlines().is_ok());
    }
    #[test]
    fn wake_pipe_interrupts_poll_and_drains_clean() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let (wake_rx, wake_tx) = wake_pipe().unwrap();
        // Nothing pending: an immediate poll reports the socket idle.
        assert!(!wait_readable(stream.as_raw_fd(), wake_rx.as_raw_fd(), 0).unwrap());
        // A request wake returns from poll without claiming the socket is readable.
        (&wake_tx).write_all(&[1]).unwrap();
        assert!(!wait_readable(stream.as_raw_fd(), wake_rx.as_raw_fd(), 2_000).unwrap());
        drain_wake(&wake_rx);
        assert!(!wait_readable(stream.as_raw_fd(), wake_rx.as_raw_fd(), 0).unwrap());
        // Socket data is reported; a peer hangup is reported too so the read surfaces EOF.
        peer.write_all(&[0, 0, 0, 4]).unwrap();
        assert!(wait_readable(stream.as_raw_fd(), wake_rx.as_raw_fd(), 2_000).unwrap());
        drop(peer);
        assert!(wait_readable(stream.as_raw_fd(), wake_rx.as_raw_fd(), 2_000).unwrap());
    }
    #[test]
    fn asynchronous_menu_survives_late_request_ack() {
        let (mut s, _peer) = session();
        let (tx, rx) = mpsc::channel();
        s.pending.insert(
            1,
            Pending {
                deadline: Instant::now() + TIMEOUT,
                reply: Some(tx),
                generation: 0,
                init: false,
            },
        );
        let cmd =
            Command::parse(&codec::unhex("D00F8103092400820281828F04014F6E65").unwrap()).unwrap();
        s.proactive(cmd).unwrap();
        s.frame(&codec::unhex("00000000000000000000000100000000").unwrap())
            .unwrap();
        assert_eq!(rx.recv().unwrap().1["data"]["type"], "menu");
    }

    #[test]
    fn session_end_restores_root_but_sim_refresh_invalidates_it() {
        let (mut s, _peer) = session();
        s.root = Some(
            Command::parse(&codec::unhex("D00F8103012500820281828F04014F6E65").unwrap()).unwrap(),
        );
        s.phase = "waiting";
        s.frame(&codec::unhex("00000001000003F4").unwrap()).unwrap();
        assert_eq!(s.snapshot().1["data"]["root_menu"], true);
        assert!(s.frame(&codec::unhex("00000001000003F9").unwrap()).is_err());
    }

    #[test]
    fn duplicate_selection_is_busy_and_invalid_id_is_not_truncated() {
        let (mut s, _peer) = session();
        s.current = Some(
            Command::parse(&codec::unhex("D00F8103012500820281828F04014F6E65").unwrap()).unwrap(),
        );
        let (tx, rx) = mpsc::channel();
        s.handle_request(Request {
            action: "select",
            body: json!({"command_id":"1","item_id":257}),
            reply: tx,
        })
        .unwrap();
        assert_eq!(rx.recv().unwrap().0, 400);
        let (tx, _rx) = mpsc::channel();
        s.handle_request(Request {
            action: "select",
            body: json!({"command_id":"1","item_id":1}),
            reply: tx,
        })
        .unwrap();
        let (tx, rx) = mpsc::channel();
        s.handle_request(Request {
            action: "select",
            body: json!({"command_id":"1","item_id":1}),
            reply: tx,
        })
        .unwrap();
        assert_eq!(rx.recv().unwrap().0, 409);
        assert_eq!(s.pending.len(), 1);
    }

    /// The `stk` topic is told only when what `GET /api/stk/session` answers changes: a
    /// status read (the hub's own) never calls it.
    #[test]
    fn the_topic_follows_changes_of_the_session_answer_only() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = AtomicUsize::new(0);
        let f = || {
            calls.fetch_add(1, Ordering::Relaxed);
        };
        let taken = || calls.swap(0, Ordering::Relaxed);
        let shown = Mutex::new(None);
        let mut live: Option<Session> = None;
        publish(&live, "", &shown, Some(&f));
        publish(&live, "", &shown, Some(&f));
        assert_eq!(taken(), 1);
        let (mut s, _peer) = session();
        let (tx, rx) = mpsc::channel();
        s.handle_request(Request { action: "status", body: json!({}), reply: tx }).unwrap();
        assert_eq!(rx.recv().unwrap().0, 200);
        live = Some(s);
        publish(&live, "", &shown, Some(&f));
        let (tx, rx) = mpsc::channel();
        if let Some(s) = live.as_mut() {
            s.handle_request(Request { action: "status", body: json!({}), reply: tx }).unwrap();
        }
        assert_eq!(rx.recv().unwrap().0, 200);
        publish(&live, "", &shown, Some(&f));
        assert_eq!(taken(), 1);
        // A SIM command arrives: one call.
        let cmd =
            Command::parse(&codec::unhex("D00F8103092400820281828F04014F6E65").unwrap()).unwrap();
        if let Some(s) = live.as_mut() {
            s.proactive(cmd).unwrap();
        }
        publish(&live, "", &shown, Some(&f));
        publish(&live, "", &shown, Some(&f));
        assert_eq!(taken(), 1);
        // The session fails: one more.
        let mut last_error = String::new();
        invalidate(&mut live, &mut last_error, "RIL socket disconnected".into());
        publish(&live, &last_error, &shown, Some(&f));
        publish(&live, &last_error, &shown, Some(&f));
        assert_eq!(taken(), 1);
        // What the topic reads from memory is what the route would answer.
        assert_eq!(shown.lock().unwrap().clone(), Some(view(&live, &last_error)));
        // Without a hook nothing is called.
        publish(&None, "other", &shown, None);
    }

    #[test]
    fn failed_response_is_not_returned_as_success() {
        let (mut s, _peer) = session();
        let (tx, rx) = mpsc::channel();
        s.pending.insert(
            1,
            Pending {
                deadline: Instant::now() + TIMEOUT,
                reply: Some(tx),
                generation: 1,
                init: false,
            },
        );
        assert!(s
            .frame(&codec::unhex("00000000000000000000000100000006").unwrap())
            .is_err());
        assert_eq!(rx.recv().unwrap().0, 502);
    }
}
