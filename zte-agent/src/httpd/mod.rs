//! The HTTP listener: one current-thread tokio runtime on the main thread,
//! hyper http1 connections and an axum router whose fallback is the JSON API
//! (`api::api_entry`). Blocking work never runs on the runtime thread; it goes
//! through the `gate` lanes. Named `httpd` so it can never clash with the
//! `http` crate.

pub mod api;
pub mod gate;
pub mod guard;
pub mod mobile;
pub mod tls;

use std::io::ErrorKind;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::Body;
use axum::Router;
use tokio::sync::Semaphore;

use crate::handlers::AppState;
use crate::tailscale::TailscaleManager;
use gate::Gate;
use guard::HostPolicy;

/// Open connections; one more is accepted and closed at once, without a reply.
pub const MAX_CONNS: usize = 64;
/// Open connections from one LAN address, so one host (a leaky client, a
/// browser preconnecting) cannot take every slot. Loopback has no such cap:
/// every Tailscale client arrives as 127.0.0.1 (userspace networking).
pub const MAX_CONNS_PER_PEER: usize = 16;
/// The last slots of `MAX_CONNS` only loopback (Tailscale) peers may use, so
/// LAN hosts holding connections cannot lock out remote users.
pub const LOOPBACK_RESERVED: usize = 8;
/// hyper's header-read timeout: the first request's headers and every idle
/// keep-alive wait. Long enough that an app's next poll reuses the connection.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// A request body (at most 64 KiB) must arrive within this, and a LAN upload
/// may pause this long between reads; otherwise a stalled body would hold its
/// connection (and LAN) slot until TCP gives up, which can be never.
pub const BODY_WAIT: Duration = Duration::from_secs(15);
/// Unacknowledged data (or failed keepalive probes) for this long drops the
/// connection: a phone that vanished mid-reply. Linux only.
pub const TCP_USER_TIMEOUT_MS: u32 = 60_000;
/// Blocking calls in flight at once on the slow lane (the general lane has
/// `ZTE_AGENT_THREADS`).
pub const SLOW_PERMITS: usize = 2;
/// `ZTE_AGENT_THREADS` is clamped to 1..=this.
const THREADS_MAX: usize = 32;
/// Longest wait for a lane permit before 503 `agent busy, retry`; below the
/// iOS app's 10 s request timeout.
pub const GATE_WAIT: Duration = Duration::from_secs(7);
/// LAN speed-test streams at once; another one gets an immediate 503.
pub const LAN_STREAMS: usize = 2;
/// An early reply (401/403/413/LAN 503) reads and discards at most this much
/// of an unread request body, for at most `DRAIN_WAIT`, so the connection can
/// stay open; a longer body gets `Connection: close` instead.
pub const DRAIN_MAX: u64 = 1024 * 1024;
pub const DRAIN_WAIT: Duration = Duration::from_secs(2);
/// hyper's read buffer, which also caps the request head.
const MAX_BUF: usize = 64 * 1024;

/// The client's socket address, as a request extension.
#[derive(Copy, Clone, Debug)]
pub struct Peer(pub std::net::SocketAddr);

/// Shared by every request.
#[derive(Clone)]
pub struct Ctx {
    pub app: Arc<AppState>,
    pub gate: Arc<Gate>,
    /// LAN speed-test stream permits.
    pub lan: Arc<Semaphore>,
    pub hosts: Arc<HostPolicy>,
    pub ts: Arc<TsNames>,
    /// `BODY_WAIT` (tests shorten it).
    pub body_wait: Duration,
    /// Web UI operations in progress (`/ui/op/{id}`).
    pub ops: Arc<crate::web::ops::OpTable>,
    /// Web UI action results (`?r={id}`).
    pub flash: Arc<crate::web::store::Flash>,
    /// Web UI dismissals (the dashboard's USB-C card).
    pub dismissed: Arc<crate::web::store::Dismissed>,
    /// Web UI single-use form tokens (SMS send, forwarding rule forms).
    pub once: Arc<crate::web::store::Once>,
    /// Secrets typed into a forwarding rule form shown again, by its token.
    pub drafts: Arc<crate::web::store::Drafts>,
    /// AT Terminal history per session.
    pub at: Arc<crate::web::store::AtLog>,
    /// USSD answers per session (SIM Services).
    pub ussd: Arc<crate::web::store::UssdLog>,
    /// eSIM profile names last read (a switch's live card names its target).
    pub esim_names: Arc<crate::web::pages::esim::Names>,
}

impl Ctx {
    /// `threads` general-lane permits (the `ZTE_AGENT_THREADS` setting).
    pub fn new(app: Arc<AppState>, threads: usize) -> Self {
        Ctx {
            gate: Arc::new(Gate::new(threads, SLOW_PERMITS, GATE_WAIT)),
            lan: Arc::new(Semaphore::new(LAN_STREAMS)),
            // The LAN HTTPS name answers only over HTTPS (`allows_via`).
            hosts: Arc::new(HostPolicy::from_env_with(app.tls.name())),
            ts: Arc::new(TsNames::new(Arc::clone(&app.tailscale))),
            body_wait: BODY_WAIT,
            ops: Arc::default(),
            flash: Arc::default(),
            dismissed: Arc::default(),
            once: Arc::default(),
            drafts: Arc::default(),
            at: Arc::default(),
            ussd: Arc::default(),
            esim_names: Arc::default(),
            app,
        }
    }
}

/// `ZTE_AGENT_THREADS` within 1..=`THREADS_MAX`: 0 would leave the general
/// lane without permits (every call 503 while `HEAD /` still answers).
fn clamp_threads(threads: usize) -> usize {
    let clamped = threads.clamp(1, THREADS_MAX);
    if clamped != threads {
        eprintln!("[http] ZTE_AGENT_THREADS={threads} out of range, using {clamped}");
    }
    clamped
}

/// Bind, then run the listener on this thread; never returns. `threads` is
/// the number of general API calls that may block at once. Connections from
/// the mobile-data side are refused (`mobile`; `ZTE_AGENT_ALLOW_MOBILE=1`
/// serves them). The LAN HTTPS listener (`tls`) runs beside it on the same
/// runtime, with the same router, caps and check, while HTTPS is on.
pub fn serve(bind: &str, threads: usize, state: Arc<AppState>) {
    let threads = clamp_threads(threads);
    let side = mobile::Guard::from_env();
    let listener = match listen(bind) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[http] bind {bind}: {e}");
            std::process::exit(1);
        }
    };
    if let Ok(addr) = listener.local_addr() {
        HTTP_PORT.store(addr.port(), std::sync::atomic::Ordering::Relaxed);
    }
    // Seed the CPU tracker with initial sample (speed tracker is demand-driven)
    state.cpu.sample();
    let ctx = Ctx::new(state, threads);
    let rt = match runtime(threads) {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[http] runtime: {e}");
            std::process::exit(1);
        }
    };
    let caps = Caps::new(Limits::of(MAX_CONNS));
    rt.block_on(async move {
        let twin = side.twin();
        tokio::spawn(tls::supervise(Arc::clone(&ctx.app.tls), ctx.clone(), caps.clone(), move || twin.twin()));
        accept_loop_with(listener, ctx, caps, side, Wire::Plain).await
    });
    std::process::exit(1);
}

/// The plain-http listener's port (`ZTE_AGENT_BIND`), set once it is bound.
static HTTP_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(9090);

/// The port the plain-http UI and API answer on (9090 unless
/// `ZTE_AGENT_BIND` says otherwise), for texts that name it.
pub fn http_port() -> u16 {
    HTTP_PORT.load(std::sync::atomic::Ordering::Relaxed)
}

/// A bound, non-blocking listener with the keepalive settings.
pub fn listen(bind: &str) -> std::io::Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind(bind)?;
    set_tcp_keepalive(&listener);
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Current-thread runtime; its blocking pool has room for every gate permit
/// (so a permit holder never waits for a thread), each listener's
/// interface lookup (`mobile`, one at a time per listener: :9090 and
/// HTTPS), the HTTPS listener's certificate check (`tls::Tls::check`, at
/// most one a minute, which may outlive its wait) and one spare; its
/// threads exit after 10 s idle.
fn runtime(threads: usize) -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .max_blocking_threads(threads + SLOW_PERMITS + 4)
        .thread_name("http-blk")
        .thread_keep_alive(Duration::from_secs(10))
        .build()
}

/// Which peers are served: asked first for each connection, on the runtime
/// thread (it must not block; `mobile::Guard` reads its table on the
/// blocking pool, and only new connections wait for it).
trait Admit {
    async fn admit(&mut self, peer: std::net::SocketAddr, tcp: &tokio::net::TcpStream) -> bool;
}

impl Admit for mobile::Guard {
    async fn admit(&mut self, peer: std::net::SocketAddr, tcp: &tokio::net::TcpStream) -> bool {
        mobile::Guard::admit(self, peer, || tcp.local_addr().ok()).await
    }
}

/// A test's classifier.
#[cfg(test)]
impl<F: FnMut(std::net::SocketAddr, &tokio::net::TcpStream) -> bool> Admit for F {
    async fn admit(&mut self, peer: std::net::SocketAddr, tcp: &tokio::net::TcpStream) -> bool {
        self(peer, tcp)
    }
}

/// Connection caps and the idle timeout (tests shrink them).
#[derive(Copy, Clone)]
pub struct Limits {
    conns: usize,
    /// Per address, except `shared` ones.
    per_peer: usize,
    idle: Duration,
    /// Slots only `shared` addresses may take.
    reserved: usize,
    /// Addresses many clients arrive from (loopback: Tailscale).
    shared: fn(&IpAddr) -> bool,
}

impl Limits {
    /// The router's: `conns` in all, `MAX_CONNS_PER_PEER` per LAN address,
    /// the last `LOOPBACK_RESERVED` for loopback (Tailscale).
    fn of(conns: usize) -> Limits {
        Limits {
            conns,
            per_peer: MAX_CONNS_PER_PEER.min(conns),
            idle: IDLE_TIMEOUT,
            reserved: LOOPBACK_RESERVED.min(conns),
            shared: IpAddr::is_loopback,
        }
    }
}

/// The open-connection counts both listeners (:9090 and HTTPS) share, so
/// the caps hold for the two together.
#[derive(Clone)]
pub struct Caps {
    conns: Arc<Semaphore>,
    peers: Arc<PeerConns>,
    limits: Limits,
}

impl Caps {
    fn new(limits: Limits) -> Caps {
        Caps { conns: Arc::new(Semaphore::new(limits.conns)), peers: Arc::default(), limits }
    }
}

/// What a connection speaks: plain HTTP/1.1, or TLS first (`tls`).
#[derive(Clone)]
enum Wire {
    Plain,
    Tls(Arc<tls::Wire>),
}

/// hyper's HTTP/1.1 server on one connection, until `stop`: then a request
/// in progress may finish within `STOP_WAIT` (a Reset's own reply), and
/// keep-alive and live streams end.
async fn serve_http1<I, S>(io: I, svc: S, idle: Duration, stop: impl std::future::Future<Output = ()>)
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    S: hyper::service::HttpService<hyper::body::Incoming, ResBody = Body>,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    // `half_close` stays off: a client that closes its side is gone,
    // which ends a request still queued for a lane.
    let conn = hyper::server::conn::http1::Builder::new()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(idle)
        .max_buf_size(MAX_BUF)
        .serve_connection(io, svc);
    let mut conn = std::pin::pin!(conn);
    let stopped = matches!(futures_util::future::select(conn.as_mut(), std::pin::pin!(stop)).await, futures_util::future::Either::Right(_));
    if stopped {
        conn.as_mut().graceful_shutdown();
        let _ = tokio::time::timeout(STOP_WAIT, conn).await;
    }
}

/// How long a connection whose listener stopped may finish its request.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// `admit`: whether to serve a connection from this peer (see `Admit`).
async fn accept_loop_with(listener: std::net::TcpListener, ctx: Ctx, caps: Caps, mut admit: impl Admit, wire: Wire) {
    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[http] listener: {e}");
            return;
        }
    };
    let Caps { conns, peers, limits } = caps;
    let router = app(ctx);
    let mut failing = false;
    loop {
        let (tcp, addr) = match listener.accept().await {
            Ok(x) => {
                failing = false;
                x
            }
            // That connection died in the backlog; the next one is fine.
            Err(e) if is_connection_error(&e) => continue,
            Err(e) => {
                // EMFILE and friends: back off instead of spinning, and log
                // once per streak instead of ten lines a second.
                if !failing {
                    eprintln!("[http] accept: {e}");
                    failing = true;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        // From the mobile-data side: closed without a reply, before any cap.
        if !admit.admit(addr, &tcp).await {
            drop(tcp);
            continue;
        }
        // Over a cap: close without a reply.
        let Ok(permit) = Arc::clone(&conns).try_acquire_owned() else {
            drop(tcp);
            continue;
        };
        let ip = addr.ip().to_canonical();
        let shared = (limits.shared)(&ip);
        if !shared && conns.available_permits() < limits.reserved {
            drop(tcp);
            continue;
        }
        let cap = if shared { limits.conns } else { limits.per_peer };
        let Some(slot) = peers.take(ip, cap) else {
            drop(tcp);
            continue;
        };
        let _ = tcp.set_nodelay(true);
        set_tcp_user_timeout(&tcp);
        let router = router.clone();
        let held = (permit, slot);
        match &wire {
            Wire::Plain => {
                let svc = hyper::service::service_fn(
                    move |mut req: axum::http::Request<hyper::body::Incoming>| {
                        req.extensions_mut().insert(Peer(addr));
                        tower::ServiceExt::oneshot(router.clone(), req.map(Body::new))
                    },
                );
                tokio::spawn(async move {
                    let _held = held;
                    serve_http1(hyper_util::rt::TokioIo::new(tcp), svc, limits.idle, std::future::pending()).await;
                });
            }
            Wire::Tls(w) => {
                let w = Arc::clone(w);
                tokio::spawn(async move {
                    let _held = held;
                    w.serve(tcp, addr, router, limits.idle).await;
                });
            }
        }
    }
}

/// Errors `accept` reports for one failed connection, not for the listener.
fn is_connection_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset | ErrorKind::ConnectionRefused
    )
}

/// Open connections per peer address (at most `MAX_CONNS` entries); a leaf
/// lock held only for the update.
#[derive(Default)]
struct PeerConns(Mutex<Vec<(IpAddr, usize)>>);

impl PeerConns {
    fn take(self: &Arc<Self>, ip: IpAddr, cap: usize) -> Option<PeerSlot> {
        let mut open = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match open.iter_mut().find(|(a, _)| *a == ip) {
            Some((_, n)) if *n >= cap => return None,
            Some((_, n)) => *n += 1,
            None if cap == 0 => return None,
            None => open.push((ip, 1)),
        }
        Some(PeerSlot { peers: Arc::clone(self), ip })
    }
}

/// One open connection of `ip`; gives its slot back when the connection ends.
struct PeerSlot {
    peers: Arc<PeerConns>,
    ip: IpAddr,
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut open = self.peers.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = open.iter().position(|(a, _)| *a == self.ip) {
            open[i].1 -= 1;
            if open[i].1 == 0 {
                open.swap_remove(i);
            }
        }
    }
}

/// The web UI's routes (`/`, `/ui`, `/ui/**`); every other path goes to the
/// API fallback (no per-route registration, so there is no automatic 405
/// and no HEAD-to-GET routing for `/api`).
pub fn app(ctx: Ctx) -> Router {
    Router::new().merge(crate::web::router(&ctx)).merge(crate::console::router()).fallback(api::api_entry).with_state(ctx)
}

/// SO_KEEPALIVE with a 5 min idle / 60 s interval / 3 probes: a dead peer is
/// detected within ~8 min and its connection closed. Linux only; the values
/// are inherited by accepted sockets.
fn set_tcp_keepalive(listener: &std::net::TcpListener) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        set_opts(
            listener.as_raw_fd(),
            &[
                (libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1),
                (libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 300),
                (libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 60),
                (libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 3),
            ],
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = listener;
}

fn set_tcp_user_timeout(tcp: &tokio::net::TcpStream) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        set_opts(
            tcp.as_raw_fd(),
            &[(libc::IPPROTO_TCP, libc::TCP_USER_TIMEOUT, TCP_USER_TIMEOUT_MS as libc::c_int)],
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (tcp, TCP_USER_TIMEOUT_MS);
}

#[cfg(target_os = "linux")]
fn set_opts(fd: std::os::unix::io::RawFd, opts: &[(libc::c_int, libc::c_int, libc::c_int)]) {
    for &(level, name, value) in opts {
        // SAFETY: fd is a valid open socket owned by the caller; `value` outlives the call.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                level,
                name,
                &value as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            eprintln!("[http] setsockopt({level},{name}) failed: {}", std::io::Error::last_os_error());
        }
    }
}

/// Tailscale names for the Host allowlist without ever waiting on the
/// Tailscale manager's locks: when they are busy, the last answer is used.
pub struct TsNames {
    mgr: Arc<TailscaleManager>,
    last: Mutex<Vec<String>>,
}

impl TsNames {
    pub fn new(mgr: Arc<TailscaleManager>) -> Self {
        TsNames {
            mgr,
            last: Mutex::new(Vec::new()),
        }
    }

    pub fn get(&self) -> Vec<String> {
        let fresh = self.mgr.ui_host_names();
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        match fresh {
            Some(names) => {
                last.clone_from(&names);
                names
            }
            None => last.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tailscale::TsConfig;
    use serde_json::json;

    fn enabled() -> TsConfig {
        TsConfig {
            enabled: true,
            hostname: "U60-Pro".into(),
            ..TsConfig::default()
        }
    }

    #[test]
    fn ui_host_names_enabled_disabled() {
        let status = json!({"dns_name": "U60-Pro-1.Tailnet.ts.net."});
        let mgr = TailscaleManager::for_test(enabled(), status.clone());
        assert_eq!(
            mgr.ui_host_names().unwrap(),
            ["u60-pro", "u60-pro-1.tailnet.ts.net", "u60-pro-1"]
        );
        // No MagicDNS name yet (status not fetched): the hostname only.
        let mgr = TailscaleManager::for_test(enabled(), serde_json::Value::Null);
        assert_eq!(mgr.ui_host_names().unwrap(), ["u60-pro"]);
        // De-duplicated when the first label equals the hostname.
        let mgr = TailscaleManager::for_test(enabled(), json!({"dns_name": "u60-pro.tailnet.ts.net."}));
        assert_eq!(mgr.ui_host_names().unwrap(), ["u60-pro", "u60-pro.tailnet.ts.net"]);
        let mgr = TailscaleManager::for_test(TsConfig::default(), status);
        assert_eq!(mgr.ui_host_names().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn ts_names_keeps_last_answer_while_contended() {
        let mgr = Arc::new(TailscaleManager::for_test(
            enabled(),
            json!({"dns_name": "u60-pro-1.tailnet.ts.net."}),
        ));
        let names = TsNames::new(Arc::clone(&mgr));
        let fresh = names.get();
        assert_eq!(fresh.len(), 3);
        // A flash write holds `config`: no waiting, the last names are used.
        let held = mgr.lock_config_for_test();
        assert!(mgr.ui_host_names().is_none());
        assert_eq!(names.get(), fresh);
        drop(held);
        // Disabled since: the next uncontended lookup updates the answer.
        mgr.lock_config_for_test().enabled = false;
        assert!(names.get().is_empty());
    }
}

/// The listener over real sockets: framing, keep-alive, early replies with
/// unread bodies, the connection cap and the idle timeout.
#[cfg(test)]
mod wire_tests {
    use super::*;
    use crate::auth::{AuthConfig, AuthState, Kind, PeerKey, SeqRng};
    use crate::boottime::FakeClock;
    use crate::server::{API_BODY_MAX, LIVE_ACTIVITY_BODY_MAX, LOGIN_BODY_MAX};
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{Shutdown, SocketAddr, TcpStream};
    use std::time::Instant;

    const PW: &str = "test-password";
    /// A declared body length no one could ever allocate.
    const HUGE: &str = "9000000000000000000";

    fn state(password: Option<&str>) -> AppState {
        let state = AppState::new();
        if let Some(pw) = password {
            state.auth.set_password(pw);
        }
        state
    }

    /// A listener on 127.0.0.1:0 with its own runtime thread and AppState
    /// (so its own limiter), left running for the test process. Every test
    /// client is 127.0.0.1 (loopback: no per-peer cap, no reserved slots).
    fn start(ctx: Ctx, max_conns: usize, idle: Duration) -> SocketAddr {
        start_with(ctx, Limits { conns: max_conns, per_peer: max_conns, idle, reserved: 0, shared: IpAddr::is_loopback })
    }

    /// Caps as for LAN peers: the test clients (127.0.0.1) are not shared.
    fn lan_limits(conns: usize, per_peer: usize, reserved: usize) -> Limits {
        Limits { conns, per_peer, idle: IDLE_TIMEOUT, reserved, shared: |_| false }
    }

    /// `n` connections that each got an answer.
    fn hold(addr: SocketAddr, n: usize) -> Vec<Conn> {
        (0..n)
            .map(|_| {
                let mut c = Conn::open(addr);
                c.ping(None).assert_json(200, json!({"ok": true}));
                c
            })
            .collect()
    }

    fn refused(addr: SocketAddr) -> bool {
        let mut c = Conn::open(addr);
        let _ = c.stream.write_all(b"GET /api/lan/ping HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        c.closed()
    }

    fn start_with(ctx: Ctx, limits: Limits) -> SocketAddr {
        start_admitting(ctx, limits, |_: SocketAddr, _: &tokio::net::TcpStream| true)
    }

    /// As `start_with`, with `admit` deciding which peers are served.
    fn start_admitting(ctx: Ctx, limits: Limits, admit: impl Admit + Send + 'static) -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || runtime(2).unwrap().block_on(accept_loop_with(listener, ctx, Caps::new(limits), admit, Wire::Plain)));
        addr
    }

    /// Retries `f` on fresh connections until it succeeds: a slot is free
    /// again only once the server has seen the old connection close.
    fn eventually(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("{what}");
    }

    fn serve(password: Option<&str>) -> (SocketAddr, Ctx) {
        let ctx = Ctx::new(Arc::new(state(password)), 2);
        (start(ctx.clone(), MAX_CONNS, IDLE_TIMEOUT), ctx)
    }

    fn token(ctx: &Ctx) -> String {
        let peer = PeerKey::from(SocketAddr::from(([10, 0, 99, 1], 5000)));
        ctx.app.auth.login(PW, Kind::Bearer, peer, &[]).unwrap().token.expose().to_owned()
    }

    fn err(e: &str) -> Value {
        json!({"ok": false, "error": e})
    }

    struct Conn {
        stream: TcpStream,
        reader: BufReader<TcpStream>,
    }

    #[derive(Debug)]
    struct Reply {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Reply {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
        }

        fn json(&self) -> Value {
            serde_json::from_slice(&self.body).unwrap()
        }

        /// Status, the exact bytes (compact, sorted keys) and the old Content-Type.
        fn assert_json(&self, status: u16, expect: Value) {
            let body = String::from_utf8_lossy(&self.body);
            assert_eq!((self.status, body.as_ref()), (status, expect.to_string().as_str()));
            assert_eq!(self.header("content-type"), Some("application/json"));
        }

        fn closes(&self) -> bool {
            self.header("connection") == Some("close")
        }
    }

    impl Conn {
        fn open(addr: SocketAddr) -> Conn {
            let stream = TcpStream::connect(addr).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let reader = BufReader::new(stream.try_clone().unwrap());
            Conn { stream, reader }
        }

        /// One request with a Content-Length for `body`; `head` is the
        /// request line plus headers, without the final blank line.
        fn request(&mut self, head: &str, body: &[u8]) -> Reply {
            let req = format!("{head}\r\nContent-Length: {}\r\n\r\n", body.len());
            self.stream.write_all(req.as_bytes()).unwrap();
            self.stream.write_all(body).unwrap();
            self.reply(head.starts_with("HEAD ")).unwrap()
        }

        /// Head and body exactly as given (the caller writes the headers).
        fn raw(&mut self, head: &str, body: &[u8]) -> std::io::Result<Reply> {
            self.stream.write_all(format!("{head}\r\n\r\n").as_bytes())?;
            self.stream.write_all(body)?;
            self.reply(false)
        }

        fn reply(&mut self, head_request: bool) -> std::io::Result<Reply> {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Err(ErrorKind::UnexpectedEof.into());
            }
            let status = line.split(' ').nth(1).and_then(|s| s.parse().ok()).expect("status line");
            let mut headers = Vec::new();
            loop {
                line.clear();
                self.reader.read_line(&mut line)?;
                let l = line.trim_end();
                if l.is_empty() {
                    break;
                }
                let (k, v) = l.split_once(':').unwrap();
                headers.push((k.trim().to_owned(), v.trim().to_owned()));
            }
            let mut reply = Reply { status, headers, body: Vec::new() };
            if !head_request {
                let len: usize = reply.header("content-length").expect("content-length").parse().unwrap();
                reply.body = vec![0u8; len];
                self.reader.read_exact(&mut reply.body)?;
            }
            Ok(reply)
        }

        /// The server closed the connection (FIN or reset) without sending anything more.
        fn closed(&mut self) -> bool {
            let mut b = [0u8; 1];
            match self.reader.read(&mut b) {
                Ok(0) => true,
                Err(e) => e.kind() == ErrorKind::ConnectionReset,
                Ok(_) => false,
            }
        }

        fn ping(&mut self, token: Option<&str>) -> Reply {
            let auth = token.map(|t| format!("\r\nAuthorization: Bearer {t}")).unwrap_or_default();
            self.request(&format!("GET /api/lan/ping HTTP/1.1\r\nHost: 127.0.0.1{auth}"), b"")
        }

        fn login(&mut self, body: &[u8]) -> Reply {
            self.request("POST /api/auth/login HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json", body)
        }
    }

    /// The listener still answers on a new connection.
    fn assert_alive(addr: SocketAddr, token: Option<&str>) {
        Conn::open(addr).ping(token).assert_json(200, json!({"ok": true}));
    }

    #[test]
    fn head_then_second_request_on_one_connection() {
        let (addr, _) = serve(None);
        let mut c = Conn::open(addr);
        let r = c.request("HEAD /api/device HTTP/1.1\r\nHost: 127.0.0.1", b"");
        assert_eq!((r.status, r.header("content-length")), (404, Some("32")));
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.header("server"), None);
        // `/` redirects to the web UI; the connection stays usable.
        let r = c.request("HEAD / HTTP/1.1\r\nHost: 127.0.0.1", b"");
        assert_eq!((r.status, r.header("location"), r.header("content-length")), (303, Some("/ui/"), Some("0")));
        // Content-Length framing: the next reply starts right after.
        c.ping(None).assert_json(200, json!({"ok": true}));
        let r = c.request("GET /nope HTTP/1.1\r\nHost: 127.0.0.1", b"");
        assert_eq!((r.status, &r.body[..]), (404, &br#"{"error":"not found","ok":false}"#[..]));
    }

    #[test]
    fn head_with_password_is_401_and_keeps_the_connection() {
        let (addr, ctx) = serve(Some(PW));
        let mut c = Conn::open(addr);
        let r = c.request("HEAD /api/device HTTP/1.1\r\nHost: 127.0.0.1", b"");
        assert_eq!((r.status, r.header("content-length")), (401, Some("35")));
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert!(!r.closes());
        // The apps' reachability probe (`HEAD <base>`, any reply counts) now
        // gets the web UI's redirect.
        let r = c.request("HEAD / HTTP/1.1\r\nHost: 127.0.0.1", b"");
        assert_eq!((r.status, r.header("location")), (303, Some("/ui/")));
        assert!(!r.closes());
        c.ping(Some(&token(&ctx))).assert_json(200, json!({"ok": true}));
    }

    #[test]
    fn limits_follow_the_owner_decisions() {
        assert_eq!(IDLE_TIMEOUT, Duration::from_secs(120));
        assert_eq!((MAX_CONNS, MAX_CONNS_PER_PEER, LOOPBACK_RESERVED, LAN_STREAMS, SLOW_PERMITS), (64, 16, 8, 2, 2));
        assert_eq!((clamp_threads(0), clamp_threads(4), clamp_threads(usize::MAX)), (1, 4, THREADS_MAX));
    }

    #[test]
    fn lan_download_has_content_length() {
        let (addr, _) = serve(None);
        let mut c = Conn::open(addr);
        let r = c.request("GET /api/lan/download?size=100000 HTTP/1.1\r\nHost: 127.0.0.1", b"");
        assert_eq!(r.status, 200);
        assert_eq!(r.header("content-length"), Some("100000"));
        assert_eq!(r.header("content-type"), Some("application/octet-stream"));
        assert_eq!(r.header("transfer-encoding"), None);
        assert!(r.body.len() == 100_000 && r.body.iter().all(|&b| b == 0));
        c.ping(None).assert_json(200, json!({"ok": true}));
    }

    #[test]
    fn over_cap_body_sent_whole_gets_413_and_connection_stays_usable() {
        let (addr, ctx) = serve(Some(PW));
        let tok = token(&ctx);
        let mut c = Conn::open(addr);
        let head = format!("POST /api/scheduler/jobs HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {tok}");
        let r = c.request(&head, &vec![b'x'; API_BODY_MAX + 1]);
        r.assert_json(413, err("body too large"));
        assert!(!r.closes());
        c.ping(Some(&tok)).assert_json(200, json!({"ok": true}));
        // A small body behind an early 401 is drained too.
        let r = c.request("POST /api/device/reboot HTTP/1.1\r\nHost: 127.0.0.1", &[b'x'; 1000]);
        r.assert_json(401, err("unauthorized"));
        assert!(!r.closes());
        c.ping(Some(&tok)).assert_json(200, json!({"ok": true}));
    }

    #[test]
    fn early_401_with_large_declared_body_closes() {
        let (addr, ctx) = serve(Some(PW));
        let mut c = Conn::open(addr);
        // The client has only sent the head so far (a slow 5 MB upload).
        let head = "POST /api/device/reboot HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 5000000";
        let r = c.raw(head, b"").unwrap();
        r.assert_json(401, err("unauthorized"));
        assert!(r.closes());
        assert!(c.closed());
        assert_alive(addr, Some(&token(&ctx)));
    }

    #[test]
    fn sixth_bad_login_on_one_socket_is_429() {
        // A fake clock: no sleeping, and no timing window under load.
        let clock = FakeClock::at_secs(1000);
        let mut state = AppState::new();
        state.auth = Arc::new(AuthState::with_parts(clock.clone(), Box::new(SeqRng::new()), AuthConfig::default()));
        state.auth.set_password(PW);
        let addr = start(Ctx::new(Arc::new(state), 2), MAX_CONNS, IDLE_TIMEOUT);
        let mut c = Conn::open(addr);
        for _ in 0..5 {
            c.login(br#"{"password":"wrong"}"#).assert_json(401, err("invalid password"));
        }
        let r = c.login(br#"{"password":"wrong"}"#);
        r.assert_json(429, json!({"ok": false, "error": "too many login attempts", "retry_after": 1}));
        assert_eq!(r.header("retry-after"), Some("1"));
        // Blocked even with the right password, until the block ends.
        assert_eq!(c.login(format!(r#"{{"password":"{PW}"}}"#).as_bytes()).status, 429);
        clock.advance(Duration::from_secs(1));
        let r = c.login(format!(r#"{{"password":"{PW}"}}"#).as_bytes());
        assert_eq!(r.status, 200);
        let token = r.json()["data"]["token"].as_str().unwrap().to_owned();
        assert!(token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(r.header("retry-after").is_none());
    }

    #[test]
    fn connection_over_the_cap_is_closed_without_reply() {
        let addr = start(Ctx::new(Arc::new(state(None)), 2), 4, IDLE_TIMEOUT);
        let mut held: Vec<Conn> = (0..4)
            .map(|_| {
                let mut c = Conn::open(addr);
                c.ping(None).assert_json(200, json!({"ok": true}));
                c
            })
            .collect();
        let mut fifth = Conn::open(addr);
        let _ = fifth.stream.write_all(b"GET /api/lan/ping HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        assert!(fifth.closed());
        // A slot frees up once a held connection ends.
        drop(held.pop());
        eventually("no slot after a connection closed", || {
            let mut c = Conn::open(addr);
            c.raw("GET /api/lan/ping HTTP/1.1\r\nHost: 127.0.0.1", b"").is_ok_and(|r| r.status == 200)
        });
    }

    #[test]
    fn loopback_has_no_per_peer_cap_but_lan_keeps_off_the_reserved_slots() {
        // Tailscale clients (loopback): past the per-peer cap, into the reserve.
        let ctx = Ctx::new(Arc::new(state(None)), 2);
        let addr = start_with(ctx, Limits { shared: IpAddr::is_loopback, ..lan_limits(6, 2, 2) });
        let _held = hold(addr, 6);
        assert!(refused(addr));
        // LAN peers: the last two slots stay free.
        let ctx = Ctx::new(Arc::new(state(None)), 2);
        let addr = start_with(ctx, lan_limits(6, 6, 2));
        let mut held = hold(addr, 4);
        assert!(refused(addr));
        drop(held.pop());
        eventually("no slot after a connection closed", || {
            let mut c = Conn::open(addr);
            c.raw("GET /api/lan/ping HTTP/1.1\r\nHost: 127.0.0.1", b"").is_ok_and(|r| r.status == 200)
        });
    }

    /// A peer the check refuses (`mobile`; every test client is 127.0.0.1,
    /// so the test decides) is closed without a reply and holds no slot;
    /// the check sees the peer and the address it reached.
    #[test]
    fn refused_side_is_closed_without_reply_before_the_caps() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let refuse = Arc::new(AtomicBool::new(true));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (r, s) = (Arc::clone(&refuse), Arc::clone(&seen));
        let ctx = Ctx::new(Arc::new(state(None)), 2);
        let addr = start_admitting(ctx, lan_limits(1, 1, 0), move |peer: SocketAddr, tcp: &tokio::net::TcpStream| {
            s.lock().unwrap().push((peer.ip(), tcp.local_addr().ok()));
            !r.load(SeqCst)
        });
        for _ in 0..3 {
            assert!(refused(addr));
        }
        refuse.store(false, SeqCst);
        // The only slot is free: the refused connections took none.
        let mut c = Conn::open(addr);
        c.ping(None).assert_json(200, json!({"ok": true}));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert!(seen.iter().all(|(ip, local)| *ip == IpAddr::from([127, 0, 0, 1]) && *local == Some(addr)), "{seen:?}");
    }

    /// The router's guard on the loop: loopback (Tailscale) is always
    /// served, whatever the table (here: no LAN at all).
    #[test]
    fn the_mobile_guard_serves_loopback() {
        let lookup: mobile::Lookup = Arc::new(|| Ok(Vec::new()));
        let side = mobile::Guard::new(false, lookup, Arc::new(crate::boottime::BootClock));
        let ctx = Ctx::new(Arc::new(state(None)), 2);
        let addr = start_admitting(ctx, lan_limits(4, 4, 0), side);
        assert_alive(addr, None);
    }

    #[test]
    fn connection_over_the_per_peer_cap_is_closed_without_reply() {
        let ctx = Ctx::new(Arc::new(state(None)), 2);
        let addr = start_with(ctx, lan_limits(8, 2, 0));
        let mut held: Vec<Conn> = (0..2)
            .map(|_| {
                let mut c = Conn::open(addr);
                c.ping(None).assert_json(200, json!({"ok": true}));
                c
            })
            .collect();
        // Room overall, none for this address.
        let mut third = Conn::open(addr);
        let _ = third.stream.write_all(b"GET /api/lan/ping HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        assert!(third.closed());
        drop(held.pop());
        eventually("no per-peer slot after a connection closed", || {
            let mut c = Conn::open(addr);
            c.raw("GET /api/lan/ping HTTP/1.1\r\nHost: 127.0.0.1", b"").is_ok_and(|r| r.status == 200)
        });
    }

    #[test]
    fn idle_keep_alive_connection_closed_after_timeout() {
        let idle = Duration::from_millis(300);
        let addr = start(Ctx::new(Arc::new(state(None)), 2), MAX_CONNS, idle);
        // Both clocks start before the server's idle timer does.
        let t = Instant::now();
        let mut used = Conn::open(addr);
        used.ping(None).assert_json(200, json!({"ok": true}));
        let silent_t = Instant::now();
        let mut silent = Conn::open(addr);
        // Closed silently, without an unsolicited 408.
        assert!(used.closed());
        assert!(t.elapsed() >= idle);
        assert!(silent.closed());
        assert!(silent_t.elapsed() >= idle);
    }

    /// A body that stops arriving ends at `body_wait`: a stalled login or API
    /// body gets 400 and the connection closes; a LAN upload counts what came
    /// and frees its stream slot.
    #[test]
    fn stalled_bodies_end_after_body_wait() {
        let mut ctx = Ctx::new(Arc::new(state(None)), 2);
        ctx.body_wait = Duration::from_millis(300);
        let addr = start(ctx.clone(), MAX_CONNS, IDLE_TIMEOUT);
        for line in ["POST /api/auth/login", "POST /api/nowhere"] {
            let mut c = Conn::open(addr);
            let t = Instant::now();
            let r = c.raw(&format!("{line} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 100"), b"").unwrap();
            r.assert_json(400, err("incomplete body"));
            assert!(r.closes());
            assert!(t.elapsed() >= ctx.body_wait);
            assert!(c.closed());
        }
        let mut c = Conn::open(addr);
        let r = c.raw("POST /api/lan/upload HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 1000", &[0u8; 10]).unwrap();
        assert_eq!((r.status, r.json()["data"]["bytes"].as_u64()), (200, Some(10)));
        assert_eq!(ctx.lan.available_permits(), LAN_STREAMS);
        assert!(c.closed());
        assert_alive(addr, None);
    }

    /// As the apps run the test: a full 20 MB upload with a Content-Length,
    /// then the connection is reused.
    #[test]
    fn full_lan_upload_keeps_the_connection() {
        let (addr, _) = serve(None);
        let mut c = Conn::open(addr);
        let r = c.request("POST /api/lan/upload HTTP/1.1\r\nHost: 127.0.0.1", &vec![0u8; 20_000_000]);
        assert_eq!((r.status, r.json()["data"]["bytes"].as_u64()), (200, Some(20_000_000)));
        assert!(!r.closes());
        c.ping(None).assert_json(200, json!({"ok": true}));
    }

    /// Download slots come back when clients hang up mid-stream.
    #[test]
    fn abandoned_downloads_free_their_slots() {
        let (addr, ctx) = serve(None);
        let big = format!("GET /api/lan/download?size={} HTTP/1.1\r\nHost: 127.0.0.1", 200 << 20);
        let small = "GET /api/lan/download?size=10 HTTP/1.1\r\nHost: 127.0.0.1";
        let mut streams = Vec::new();
        for _ in 0..LAN_STREAMS {
            let mut c = Conn::open(addr);
            c.stream.write_all(format!("{big}\r\n\r\n").as_bytes()).unwrap();
            let mut first = [0u8; 4096];
            c.reader.read_exact(&mut first).unwrap();
            assert!(first.starts_with(b"HTTP/1.1 200 "));
            streams.push(c);
        }
        Conn::open(addr).request(small, b"").assert_json(503, err("agent busy, retry"));
        drop(streams);
        eventually("download slots not freed", || ctx.lan.available_permits() == LAN_STREAMS);
        let r = Conn::open(addr).request(small, b"");
        assert_eq!((r.status, r.body.len()), (200, 10));
    }

    /// A chunked body sent after `100 Continue` and refused past the cap is
    /// drained, not cut off: the 413 arrives and the connection stays.
    #[test]
    fn invited_body_over_the_cap_is_drained() {
        let (addr, ctx) = serve(Some(PW));
        let tok = token(&ctx);
        let mut c = Conn::open(addr);
        let head = format!(
            "POST /api/scheduler/jobs HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {tok}\r\n\
             Expect: 100-continue\r\nTransfer-Encoding: chunked\r\n\r\n"
        );
        c.stream.write_all(head.as_bytes()).unwrap();
        let mut line = String::new();
        c.reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("HTTP/1.1 100 "), "{line}");
        c.reader.read_line(&mut line).unwrap();
        let n = API_BODY_MAX + 1;
        c.stream.write_all(format!("{n:x}\r\n").as_bytes()).unwrap();
        c.stream.write_all(&vec![b'x'; n]).unwrap();
        c.stream.write_all(b"\r\n0\r\n\r\n").unwrap();
        let r = c.reply(false).unwrap();
        r.assert_json(413, err("body too large"));
        assert!(!r.closes());
        c.ping(Some(&tok)).assert_json(200, json!({"ok": true}));
    }

    /// Regression: tiny_http allocated the whole declared length when it
    /// discarded an unread body, so one request could abort the agent. Now
    /// nothing is sized from Content-Length.
    #[test]
    fn huge_declared_length_never_allocated() {
        let (addr, ctx) = serve(Some(PW));
        let tok = token(&ctx);
        let auth = format!("Authorization: Bearer {tok}");
        let cases: [(String, &[u8], u16, &str); 4] = [
            // Auth fails before the body.
            (format!("POST /api/device/reboot HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {HUGE}"), b"", 401, "unauthorized"),
            // Over a route's body cap, and over the login cap.
            (format!("POST /api/scheduler/jobs HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}\r\nContent-Length: {HUGE}"), b"", 413, "body too large"),
            (format!("POST /api/auth/login HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {HUGE}"), b"", 413, "body too large"),
            // A LAN route behind an auth failure.
            (format!("POST /api/lan/upload HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {HUGE}"), b"", 401, "unauthorized"),
        ];
        for (i, (head, body, status, error)) in cases.iter().enumerate() {
            let mut c = Conn::open(addr);
            let r = c.raw(head, body).unwrap();
            r.assert_json(*status, err(error));
            assert!(r.closes(), "case {i}");
            assert!(c.closed());
            assert_alive(addr, Some(&tok));
        }
        // Some body bytes already sent: the reply may be lost to a reset,
        // but the agent stays up.
        for head in [&cases[0].0, &cases[1].0] {
            let mut c = Conn::open(addr);
            if let Ok(r) = c.raw(head, &[b'x'; 100]) {
                assert!(r.closes() && (r.status == 401 || r.status == 413), "{r:?}");
            }
            assert_alive(addr, Some(&tok));
        }
        // LAN upload with a short body: counts what arrived.
        let mut c = Conn::open(addr);
        let head = format!("POST /api/lan/upload HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}\r\nContent-Length: {HUGE}\r\n\r\n");
        c.stream.write_all(head.as_bytes()).unwrap();
        c.stream.write_all(&[0u8; 1000]).unwrap();
        c.stream.shutdown(Shutdown::Write).unwrap();
        // Everything sent was read, so the reply cannot be lost to a reset.
        let r = c.reply(false).unwrap();
        assert_eq!((r.status, r.json()["data"]["bytes"].as_u64()), (200, Some(1000)));
        assert_alive(addr, Some(&tok));
        // Both LAN streams busy: an immediate 503.
        let held = (Arc::clone(&ctx.lan).try_acquire_owned().unwrap(), Arc::clone(&ctx.lan).try_acquire_owned().unwrap());
        for line in ["POST /api/lan/upload", "GET /api/lan/download?size=1000"] {
            let mut c = Conn::open(addr);
            let r = c.raw(&format!("{line} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}\r\nContent-Length: {HUGE}"), b"").unwrap();
            r.assert_json(503, err("agent busy, retry"));
            assert!(r.closes());
            assert_alive(addr, Some(&tok));
        }
        drop(held);
    }

    #[test]
    fn huge_declared_length_without_password() {
        let (addr, _) = serve(None);
        let cases = [
            ("POST /api/device/reboot HTTP/1.1", 400, "missing host"),
            ("POST /api/device/reboot HTTP/1.1\r\nHost: evil.example", 403, "host not allowed"),
            ("POST /api/device/reboot HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://evil.example", 403, "cross-origin request refused"),
            ("PUT /api/live-activity HTTP/1.1\r\nHost: 127.0.0.1", 403, "agent password required"),
        ];
        for (head, status, error) in cases {
            let mut c = Conn::open(addr);
            let r = c.raw(&format!("{head}\r\nContent-Length: {HUGE}"), b"").unwrap();
            r.assert_json(status, err(error));
            assert!(r.closes());
            assert_alive(addr, None);
        }
    }

    #[test]
    fn expect_continue_refused_body_is_never_invited() {
        let (addr, _) = serve(Some(PW));
        let mut c = Conn::open(addr);
        let head = "POST /api/device/reboot HTTP/1.1\r\nHost: 127.0.0.1\r\nExpect: 100-continue\r\nContent-Length: 10";
        let r = c.raw(head, b"").unwrap();
        // The final reply comes first, never a 100 Continue.
        r.assert_json(401, err("unauthorized"));
        assert!(r.closes());
    }

    // --- request handling carried over from the tiny_http wiring tests ---

    #[test]
    fn login_body_cap() {
        let (addr, _) = serve(Some(PW));
        let mut c = Conn::open(addr);
        let mut body = format!(r#"{{"password":"{PW}"}}"#).into_bytes();
        body.resize(LOGIN_BODY_MAX, b' ');
        assert_eq!(c.login(&body).status, 200);
        body.push(b' ');
        c.login(&body).assert_json(413, err("body too large"));
        // The connection is still usable.
        c.login(b"not json").assert_json(400, err("invalid JSON"));
        c.login(b"{}").assert_json(400, err("missing 'password' field"));
    }

    #[test]
    fn lan_ping_and_host_check_without_password() {
        let (addr, _) = serve(None);
        let mut c = Conn::open(addr);
        c.ping(None).assert_json(200, json!({"ok": true}));
        let r = c.request("GET /api/lan/ping?x=1 HTTP/1.1\r\nhost: localhost:9090", b"");
        r.assert_json(200, json!({"ok": true}));
        let r = c.request("GET /api/lan/ping HTTP/1.1\r\nHost: evil.example:9090", b"");
        r.assert_json(403, err("host not allowed"));
        let r = c.request("POST /api/x HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://evil.example", b"{}");
        r.assert_json(403, err("cross-origin request refused"));
        // HTTP/1.1 requires Host, but hyper passes a request without one on.
        let r = c.request("GET /api/lan/ping HTTP/1.1", b"");
        r.assert_json(400, err("missing host"));
    }

    #[test]
    fn bearer_prefix_case_sensitive_and_body_caps() {
        let (addr, _) = serve(Some(PW));
        let mut c = Conn::open(addr);
        let r = c.login(format!(r#"{{"password":"{PW}"}}"#).as_bytes());
        let token = r.json()["data"]["token"].as_str().unwrap().to_owned();
        let not_found = err("not found");
        // Header names are case-insensitive; the `Bearer ` prefix is not.
        let r = c.request(&format!("GET /api/nowhere HTTP/1.1\r\nHost: 127.0.0.1\r\nauthorization: Bearer {token}"), b"");
        r.assert_json(404, not_found.clone());
        let r = c.request(&format!("GET /api/nowhere HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: bearer {token}"), b"");
        r.assert_json(401, err("unauthorized"));
        let r = c.request("GET /api/nowhere HTTP/1.1\r\nHost: 127.0.0.1", b"");
        r.assert_json(401, err("unauthorized"));
        let auth = format!("POST /api/nowhere HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}");
        c.request(&auth, &vec![b'x'; API_BODY_MAX]).assert_json(404, not_found.clone());
        c.request(&auth, &vec![b'x'; API_BODY_MAX + 1]).assert_json(413, err("body too large"));
        // Live Activity: 4096 bytes reach route() (no handler on this path), 4097 do not.
        let live = format!("PUT /api/live-activity/nowhere HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}");
        c.request(&live, &vec![b'x'; LIVE_ACTIVITY_BODY_MAX]).assert_json(404, not_found);
        let live = format!("PUT /api/live-activity HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}");
        c.request(&live, &vec![b'x'; LIVE_ACTIVITY_BODY_MAX + 1]).assert_json(413, err("body too large"));
    }

    /// Reads until `needle` has arrived (or the 10 s read timeout fails).
    fn read_until(c: &mut Conn, seen: &mut String, needle: &str) {
        let mut buf = [0u8; 4096];
        while !seen.contains(needle) {
            let n = c.reader.read(&mut buf).expect("stream stalled");
            assert!(n > 0, "closed before {needle:?}: {seen}");
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }

    /// An event stream over a real socket: small events are not held back
    /// by buffering, and a client that disconnects gives back its demand.
    #[test]
    fn event_stream_flushes_and_disconnect_releases_demand() {
        use crate::events::Topic;
        let (addr, ctx) = serve(Some(PW));
        let tok = token(&ctx);
        let mut c = Conn::open(addr);
        let head = format!("GET /api/events?topics=sms,signal HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {tok}\r\n\r\n");
        c.stream.write_all(head.as_bytes()).unwrap();
        let mut seen = String::new();
        read_until(&mut c, &mut seen, "\"received\":0}}");
        assert!(seen.starts_with("HTTP/1.1 200 OK\r\n"), "{seen}");
        let lower = seen.to_ascii_lowercase();
        assert!(lower.contains("content-type: text/event-stream\r\n"));
        assert!(lower.contains("transfer-encoding: chunked\r\n"));
        assert!(seen.contains("event: hello\n"));
        assert_eq!((ctx.app.hub.demand(Topic::Sms), ctx.app.hub.demand(Topic::Signal)), (1, 1));
        // One small change arrives at once.
        ctx.app.hub.on_bus_event("zwrt_wms_status_event", &json!({"wms_status": "new_sms_received"}));
        let t = Instant::now();
        read_until(&mut c, &mut seen, "\"received\":1}}");
        assert!(t.elapsed() < Duration::from_secs(2));
        // Abrupt disconnect (no bye): the sampler's demand is released.
        drop(c);
        eventually("demand not released", || ctx.app.hub.demand(Topic::Signal) == 0);
        assert_eq!(ctx.app.hub.demand(Topic::Sms), 0);
        assert_alive(addr, Some(&tok));
    }

    /// hyper's header-read (idle) timer only runs while waiting for a
    /// request, so an open stream outlives it.
    #[test]
    fn event_stream_outlives_the_idle_timeout() {
        let idle = Duration::from_millis(100);
        let state = Arc::new(state(Some(PW)));
        let ctx = Ctx::new(state, 2);
        let addr = start(ctx.clone(), MAX_CONNS, idle);
        let tok = token(&ctx);
        let mut c = Conn::open(addr);
        let head = format!("GET /api/events?topics=sms HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {tok}\r\n\r\n");
        c.stream.write_all(head.as_bytes()).unwrap();
        let mut seen = String::new();
        read_until(&mut c, &mut seen, "\"received\":0}}");
        std::thread::sleep(idle * 3);
        ctx.app.hub.on_bus_event("zwrt_wms_status_event", &json!({"wms_status": "new_sms_received"}));
        read_until(&mut c, &mut seen, "\"received\":1}}");
        assert_eq!(ctx.app.hub.demand(crate::events::Topic::Sms), 1);
    }

    /// How streams end over a real socket, with the production wiring:
    /// `bye`, then the final chunk, and the connection serves the next request.
    #[test]
    fn event_streams_end_with_bye_and_the_connection_stays_usable() {
        use crate::events::Topic;
        let (addr, ctx) = serve(Some(PW));
        let bus = crate::event_bus::EventBus::new();
        crate::events::wire(&ctx.app, &bus);
        let open = |tok: &str| {
            let mut c = Conn::open(addr);
            let head = format!("GET /api/events?topics=sms HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {tok}\r\n\r\n");
            c.stream.write_all(head.as_bytes()).unwrap();
            let mut seen = String::new();
            read_until(&mut c, &mut seen, "\"received\":0}}");
            (c, seen)
        };
        let end = |c: &mut Conn, seen: &mut String, reason: &str| {
            read_until(c, seen, &format!("event: bye\ndata: {{\"reason\":\"{reason}\"}}\n\n"));
            read_until(c, seen, "\r\n0\r\n\r\n");
            assert!(seen.ends_with("\r\n0\r\n\r\n"), "{seen}");
        };
        // A reconnect on the same token replaces the old stream.
        let tok = token(&ctx);
        let (mut a, mut seen) = open(&tok);
        let (mut b, mut b_seen) = open(&tok);
        end(&mut a, &mut seen, "evicted");
        a.ping(Some(&tok)).assert_json(200, json!({"ok": true}));
        // A logout ends that token's stream at once, not at the next keep-alive.
        let other = token(&ctx);
        let (mut c, mut c_seen) = open(&other);
        let t = Instant::now();
        assert!(ctx.app.auth.logout(Kind::Bearer, &other));
        end(&mut c, &mut c_seen, "logged_out");
        assert!(t.elapsed() < Duration::from_secs(2));
        assert_eq!(ctx.app.hub.demand(Topic::Sms), 1);
        // Fast-boot standby ends every stream.
        bus.publish("zwrt_device_power_state_event", &json!({"action": "mode_power_off_quicken"}));
        end(&mut b, &mut b_seen, "standby");
        b.ping(Some(&tok)).assert_json(200, json!({"ok": true}));
        assert_eq!(ctx.app.hub.demand(Topic::Sms), 0);
    }

    #[test]
    fn incomplete_body_not_dispatched() {
        let (addr, _) = serve(None);
        let mut c = Conn::open(addr);
        // 1,500 of a declared 2,000 bytes, then the client stops sending.
        let head = "POST /api/nowhere HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2000\r\n\r\n";
        c.stream.write_all(head.as_bytes()).unwrap();
        c.stream.write_all(&[b' '; 1500]).unwrap();
        c.stream.shutdown(Shutdown::Write).unwrap();
        c.reply(false).unwrap().assert_json(400, err("incomplete body"));
    }
}
