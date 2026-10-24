//! The HTTPS listener end to end on 127.0.0.1: a rustls client that trusts
//! only the generated CA, the cookie and origin translations, hot swap,
//! reset and the shared caps.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::Ordering;
use std::time::Instant;

use rustls::pki_types::{CertificateDer, ServerName};

use super::*;
use crate::handlers::AppState;
use crate::httpd::{runtime, Limits, IDLE_TIMEOUT};
use crate::tls::tests::World;

const PW: &str = "test-password";
const DAY: i64 = 86_400;

fn now() -> i64 {
    crate::web::sms::now_unix()
}

struct Server {
    world: World,
    tls: Arc<Tls>,
    plain: SocketAddr,
    app: Arc<AppState>,
}

/// :9090 and the HTTPS supervisor on one runtime thread, sharing `conns`
/// connection slots, every peer admitted; HTTPS off until a test turns it
/// on. The fake world's clock is the real one (the client checks dates).
fn start(name: &str, conns: usize) -> Server {
    start_with(World::new(name), conns, || |_: SocketAddr, _: &tokio::net::TcpStream| true)
}

/// `start` in `world`, the HTTPS listener's peers checked by `side()`.
fn start_with<A: Admit + 'static>(world: World, conns: usize, side: impl Fn() -> A + Send + 'static) -> Server {
    world.wall.store(now(), Ordering::SeqCst);
    let tls = Arc::new(world.tls());
    let mut state = AppState::new();
    state.tls = Arc::clone(&tls);
    state.auth.set_password(PW);
    let app = Arc::new(state);
    let ctx = Ctx::new(Arc::clone(&app), 2);
    let caps = Caps::new(Limits { conns, per_peer: conns, idle: IDLE_TIMEOUT, reserved: 0, shared: IpAddr::is_loopback });
    let listener = listen("127.0.0.1:0").unwrap();
    let plain = listener.local_addr().unwrap();
    let t = Arc::clone(&tls);
    std::thread::spawn(move || {
        runtime(2).unwrap().block_on(async move {
            let plain = accept_loop_with(listener, ctx.clone(), caps.clone(), |_: SocketAddr, _: &tokio::net::TcpStream| true, super::super::Wire::Plain);
            futures_util::future::join(supervise(t, ctx, caps, side), plain).await
        })
    });
    Server { world, tls, plain, app }
}

/// Turn HTTPS on and wait for the listener.
fn on(s: &Server) -> SocketAddr {
    s.tls.turn_on().unwrap();
    listening(&s.tls)
}

fn listening(tls: &Tls) -> SocketAddr {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(a) = tls.status().listening {
            return a;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("not listening");
}

/// A client trusting only `ca`, without session resumption (every
/// connection sees and verifies the certificate).
fn client(ca: &[u8], versions: &[&'static rustls::SupportedProtocolVersion]) -> Arc<rustls::ClientConfig> {
    let mut c = Arc::unwrap_or_clone(resuming(ca, versions));
    c.resumption = rustls::client::Resumption::disabled();
    Arc::new(c)
}

fn resuming(ca: &[u8], versions: &[&'static rustls::SupportedProtocolVersion]) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(ca.to_vec())).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut c = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    c.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(c)
}

type Tls12 = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// A verified TLS connection to `addr` for `name` (the handshake done).
fn connect(addr: SocketAddr, cfg: Arc<rustls::ClientConfig>, name: &str) -> std::io::Result<Tls12> {
    let tcp = TcpStream::connect(addr)?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    let conn = rustls::ClientConnection::new(cfg, ServerName::try_from(name.to_owned()).unwrap()).unwrap();
    let mut s = rustls::StreamOwned::new(conn, tcp);
    while s.conn.is_handshaking() {
        s.conn.complete_io(&mut s.sock)?;
    }
    Ok(s)
}

#[derive(Debug)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn all(&self, name: &str) -> Vec<&str> {
        self.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect()
    }
}

/// One request on `io`; `head` without the final blank line.
fn request(io: &mut dyn ReadWrite, head: &str, body: &str) -> Reply {
    let req = format!("{head}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    io.write_all(req.as_bytes()).unwrap();
    io.flush().unwrap();
    let mut r = BufReader::new(io);
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).and_then(|s| s.parse().ok()).expect("status line");
    let mut headers = Vec::new();
    loop {
        line.clear();
        r.read_line(&mut line).unwrap();
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':').unwrap();
        headers.push((k.trim().to_owned(), v.trim().to_owned()));
    }
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).map_or(0, |(_, v)| v.parse().unwrap());
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).unwrap();
    Reply { status, headers, body: String::from_utf8_lossy(&body).into_owned() }
}

/// The value of `name=` in a `Set-Cookie` list.
fn cookie<'a>(r: &'a Reply, name: &str) -> Option<&'a str> {
    r.all("set-cookie").into_iter().find_map(|c| c.strip_prefix(name)?.strip_prefix('=')?.split(';').next())
}

/// Log in through the form on `io` (its origin `origin`, cookie names
/// `prefix`): the session cookie's value.
fn login(io: &mut dyn ReadWrite, host: &str, origin: &str, prefix: &str) -> String {
    let r = request(io, &format!("GET /ui/login HTTP/1.1\r\nHost: {host}"), "");
    assert_eq!(r.status, 200);
    let pre = cookie(&r, &format!("{prefix}zte_ui_pre")).expect("pre-login cookie").to_owned();
    let token = r.body.split("name=\"login_csrf\" value=\"").nth(1).unwrap().split('"').next().unwrap().to_owned();
    let form = format!("login_csrf={token}&next=%2Fui%2Fsettings&password={PW}");
    let head = format!(
        "POST /ui/login HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/x-www-form-urlencoded\r\nCookie: {prefix}zte_ui_pre={pre}"
    );
    let r = request(io, &head, &form);
    assert_eq!((r.status, r.header("location")), (303, Some("/ui/settings")), "{r:?}");
    cookie(&r, &format!("{prefix}zte_ui_session")).expect("session cookie").to_owned()
}

/// The UI over HTTPS with the generated certificate: verified by a client
/// that trusts only the router's CA, by the local name and by its address,
/// TLS 1.2 and 1.3, ALPN http/1.1, no HSTS; refused by name for another
/// name and by a client that trusts another CA.
#[test]
fn https_serves_the_ui_with_the_routers_certificate() {
    let s = start("https-ui", 16);
    let addr = on(&s);
    let ca = s.tls.ca_der().unwrap();
    let host = format!("u60.home.arpa:{}", addr.port());
    for versions in [&[&rustls::version::TLS13][..], &[&rustls::version::TLS12]] {
        let mut c = connect(addr, client(&ca, versions), "u60.home.arpa").unwrap();
        assert_eq!(c.conn.alpn_protocol(), Some(&b"http/1.1"[..]));
        assert_eq!(c.conn.protocol_version(), Some(versions[0].version));
        // The server certificate, then the issuing CA's.
        let served: Vec<Vec<u8>> = c.conn.peer_certificates().unwrap().iter().map(|c| c.as_ref().to_vec()).collect();
        assert_eq!(served, ["server.crt", "issuing.crt"].map(|f| std::fs::read(s.world.dir().join(f)).unwrap()));
        let r = request(&mut c, &format!("GET /ui/login HTTP/1.1\r\nHost: {host}"), "");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("Agent password"));
        assert_eq!(r.header("strict-transport-security"), None);
        let pre = r.all("set-cookie");
        assert!(pre.len() == 1 && pre[0].starts_with("__Host-zte_ui_pre=") && pre[0].ends_with("; Path=/; Max-Age=900; HttpOnly; SameSite=Strict; Secure"), "{pre:?}");
        // Keep-alive: a second request on the same connection.
        let r = request(&mut c, &format!("GET /api/lan/ping HTTP/1.1\r\nHost: {host}"), "");
        assert_eq!(r.status, 401);
    }
    // By its address (the certificate's IP SAN): 192.168.0.1 is the fake
    // LAN address; the TCP connection still goes to 127.0.0.1.
    assert!(connect(addr, client(&ca, &[&rustls::version::TLS13]), "192.168.0.1").is_ok());
    // Not for another name; not for a client trusting another CA.
    assert!(connect(addr, client(&ca, &[&rustls::version::TLS13]), "example.com").is_err());
    let other = crate::tls::x509::tests::test_authority();
    assert!(connect(addr, client(&other.root, &[&rustls::version::TLS13]), "u60.home.arpa").is_err());
    // The local name is no Host of the plain listener (another network's
    // DNS can answer for it over http), for the UI or the API.
    let mut p = TcpStream::connect(s.plain).unwrap();
    let r = request(&mut p, &format!("GET /ui/login HTTP/1.1\r\nHost: u60.home.arpa:{}", s.plain.port()), "");
    assert_eq!((r.status, r.body.as_str()), (403, "Host not allowed"));
    let r = request(&mut p, &format!("POST /api/auth/login HTTP/1.1\r\nHost: u60.home.arpa:{}\r\nOrigin: http://u60.home.arpa:{}", s.plain.port(), s.plain.port()), "{}");
    assert_eq!(r.status, 403);
    let mut c = connect(addr, client(&ca, &[&rustls::version::TLS13]), "u60.home.arpa").unwrap();
    let r = request(&mut c, &format!("POST /api/auth/login HTTP/1.1\r\nHost: {host}\r\nOrigin: https://{host}\r\nContent-Type: application/json"), "{\"password\":\"wrong\"}");
    assert_eq!(r.status, 401, "{r:?}");
    // DNS-rebinding protection otherwise, on both.
    let mut c = connect(addr, client(&ca, &[&rustls::version::TLS13]), "u60.home.arpa").unwrap();
    let r = request(&mut c, &format!("GET /ui/login HTTP/1.1\r\nHost: evil.example:{}", addr.port()), "");
    assert_eq!((r.status, r.body.as_str()), (403, "Host not allowed"));
    let r = request(&mut p, "GET /ui/login HTTP/1.1\r\nHost: u60.home.arpa.evil.example", "");
    assert_eq!(r.status, 403);
}

/// Each origin has its own cookies: a session made over HTTPS is a
/// `__Host-` cookie (Path=/, Secure) the http side never reads; the http
/// side's cookie, which browsers also send over HTTPS, is never read
/// there; the theme preference likewise.
#[test]
fn cookies_are_per_origin() {
    let s = start("https-cookies", 16);
    let addr = on(&s);
    let ca = s.tls.ca_der().unwrap();
    let host = format!("u60.home.arpa:{}", addr.port());
    let origin = format!("https://{host}");
    let mut c = connect(addr, client(&ca, &[&rustls::version::TLS13]), "u60.home.arpa").unwrap();
    let tok = login(&mut c, &host, &origin, "__Host-");
    let settings = |io: &mut dyn ReadWrite, host: &str, cookie: &str| {
        request(io, &format!("GET /ui/settings HTTP/1.1\r\nHost: {host}\r\nCookie: {cookie}"), "")
    };
    let r = settings(&mut c, &host, &format!("__Host-zte_ui_session={tok}"));
    assert_eq!(r.status, 200);
    assert!(r.body.contains("HTTPS &amp; Install as App"), "{}", r.body);
    // The same token under the http name: not read over HTTPS.
    let r = settings(&mut c, &host, &format!("zte_ui_session={tok}"));
    assert_eq!((r.status, r.header("location")), (303, Some("/ui/login?next=%2Fui%2Fsettings")));
    // Over :9090 the __Host- cookie is not read; its own login works there.
    let phost = format!("192.168.0.1:{}", s.plain.port());
    let mut p = TcpStream::connect(s.plain).unwrap();
    let r = settings(&mut p, &phost, &format!("__Host-zte_ui_session={tok}"));
    assert_eq!(r.status, 303);
    let ptok = login(&mut p, &phost, &format!("http://{phost}"), "");
    assert_ne!(ptok, tok);
    assert_eq!(settings(&mut p, &phost, &format!("zte_ui_session={ptok}")).status, 200);
    // Both cookies sent over HTTPS (as browsers do): only the __Host- one counts.
    let r = settings(&mut c, &host, &format!("zte_ui_session={ptok}; __Host-zte_ui_session={tok}"));
    assert_eq!(r.status, 200);
    let r = settings(&mut c, &host, &format!("zte_ui_session={ptok}"));
    assert_eq!(r.status, 303);
    // A preference set over HTTPS: a __Host- cookie.
    let csrf = |body: &str| body.split("name=\"csrf\" value=\"").nth(1).unwrap().split('"').next().unwrap().to_owned();
    let page = settings(&mut c, &host, &format!("__Host-zte_ui_session={tok}"));
    let post = |io: &mut dyn ReadWrite, origin: &str| {
        let head = format!(
            "POST /ui/settings/theme HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/x-www-form-urlencoded\r\nCookie: __Host-zte_ui_session={tok}"
        );
        request(io, &head, &format!("csrf={}&theme=dark", csrf(&page.body)))
    };
    let r = post(&mut c, &origin);
    assert_eq!(r.status, 303);
    assert_eq!(r.all("set-cookie"), ["__Host-zte_ui_theme=dark; Path=/; Max-Age=31536000; HttpOnly; SameSite=Strict; Secure"]);
    // The same form from the http origin of the same host and port, or
    // another site: refused.
    for bad in [format!("http://{host}"), "https://evil.example".to_owned(), "null".to_owned()] {
        let r = post(&mut c, &bad);
        assert_eq!(r.status, 403, "{bad}");
        assert!(r.all("set-cookie").is_empty());
    }
    // A logout clears the __Host- cookie (same name, path and Secure).
    let head = format!(
        "POST /ui/logout HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/x-www-form-urlencoded\r\nCookie: __Host-zte_ui_session={tok}"
    );
    let r = request(&mut c, &head, &format!("csrf={}", csrf(&page.body)));
    assert_eq!(r.status, 303);
    assert!(r.all("set-cookie").contains(&"__Host-zte_ui_session=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict; Secure"), "{r:?}");
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

/// A new certificate takes effect at the next handshake without a
/// restart; at a connection, a renewal the verified clock asks for is done
/// before the handshake; reset closes the port; on again listens with a
/// new CA.
#[test]
fn hot_swap_lazy_renewal_and_reset() {
    let s = start("https-swap", 16);
    // Issued 380 days ago (by the verified clock): 15 days left now.
    let real = now();
    *s.world.verified.lock().unwrap() = Some(real - 380 * DAY);
    let addr = on(&s);
    let ca = s.tls.ca_der().unwrap();
    let cfg = client(&ca, &[&rustls::version::TLS13]);
    let peer = |name: &str| {
        let c = connect(addr, Arc::clone(&cfg), name).unwrap();
        crate::tls::x509::parse(c.conn.peer_certificates().unwrap()[0].as_ref()).unwrap()
    };
    *s.world.verified.lock().unwrap() = Some(real);
    // The connection's check renews it before the handshake.
    let renewed = peer("u60.home.arpa");
    assert_eq!(renewed.not_before, real - 2 * DAY);
    // A new LAN address (outside the CA): the next handshake gets the
    // name-only certificate; back, the address again.
    s.tls.lan_changed(std::net::Ipv4Addr::new(10, 0, 0, 1));
    let moved = peer("u60.home.arpa");
    assert!(moved.ips.is_empty());
    assert_eq!(moved.not_after, renewed.not_after);
    assert!(connect(addr, Arc::clone(&cfg), "192.168.0.1").is_err());
    assert!(connect(addr, Arc::clone(&cfg), "10.0.0.1").is_err());
    s.tls.lan_changed(std::net::Ipv4Addr::new(192, 168, 0, 1));
    assert_eq!(peer("192.168.0.1").not_after, renewed.not_after);
    // An open connection keeps working across the swap.
    let mut held = connect(addr, Arc::clone(&cfg), "u60.home.arpa").unwrap();
    s.tls.lan_changed(std::net::Ipv4Addr::new(10, 0, 0, 2));
    let r = request(&mut held, &format!("GET /ui/login HTTP/1.1\r\nHost: u60.home.arpa:{}", addr.port()), "");
    assert_eq!(r.status, 200);
    // A client that resumes sessions (as browsers do), with one in hand.
    let resumed = resuming(&ca, &[&rustls::version::TLS13]);
    let mut r1 = connect(addr, Arc::clone(&resumed), "u60.home.arpa").unwrap();
    let _ = request(&mut r1, &format!("GET /ui/login HTTP/1.1\r\nHost: u60.home.arpa:{}", addr.port()), "");
    // Reset: the port closes.
    s.tls.reset().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while s.tls.status().listening.is_some() || TcpStream::connect(addr).is_ok_and(|_| connect(addr, Arc::clone(&cfg), "u60.home.arpa").is_ok()) {
        assert!(Instant::now() < deadline, "still serving after reset");
        std::thread::sleep(Duration::from_millis(20));
    }
    // On again: listening, with a new CA the old one does not verify, also
    // for a client holding a session from before the reset.
    let addr = on(&s);
    assert!(connect(addr, Arc::clone(&cfg), "u60.home.arpa").is_err());
    assert!(connect(addr, Arc::clone(&resumed), "u60.home.arpa").is_err());
    let fresh = client(&s.tls.ca_der().unwrap(), &[&rustls::version::TLS13]);
    assert!(connect(addr, fresh, "u60.home.arpa").is_ok());
}

/// The caps are shared with :9090: with every slot held by plain
/// connections an HTTPS connection is closed without a handshake; a freed
/// slot serves it.
#[test]
fn connection_caps_are_shared_with_the_plain_listener() {
    let s = start("https-caps", 2);
    let addr = on(&s);
    let cfg = client(&s.tls.ca_der().unwrap(), &[&rustls::version::TLS13]);
    let host = format!("Host: 192.168.0.1:{}", s.plain.port());
    let mut held: Vec<TcpStream> = (0..2)
        .map(|_| {
            let mut p = TcpStream::connect(s.plain).unwrap();
            assert_eq!(request(&mut p, &format!("GET /ui/login HTTP/1.1\r\n{host}"), "").status, 200);
            p
        })
        .collect();
    assert!(connect(addr, Arc::clone(&cfg), "u60.home.arpa").is_err());
    held.pop();
    let deadline = Instant::now() + Duration::from_secs(10);
    while connect(addr, Arc::clone(&cfg), "u60.home.arpa").is_err() {
        assert!(Instant::now() < deadline, "no slot after a connection closed");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The translations on their own.
#[test]
fn inbound_and_outbound_translations() {
    let mut h = HeaderMap::new();
    h.append(COOKIE, HeaderValue::from_static("zte_ui_session=aa; __Host-zte_ui_session=bb; other=1"));
    h.append(COOKIE, HeaderValue::from_static("__Host-zte_ui_theme=dark; __Host-other=2; __host-zte_ui_pre=x"));
    h.insert(ORIGIN, HeaderValue::from_static("https://u60.home.arpa:9443"));
    h.insert(REFERER, HeaderValue::from_static("https://u60.home.arpa:9443/ui/settings?x=1"));
    inbound(&mut h);
    assert_eq!(h.get_all(COOKIE).iter().collect::<Vec<_>>(), ["zte_ui_session=bb; zte_ui_theme=dark"]);
    assert_eq!(h[ORIGIN], "http://u60.home.arpa:9443");
    assert_eq!(h[REFERER], "http://u60.home.arpa:9443/ui/settings?x=1");
    let mut h = HeaderMap::new();
    h.insert(COOKIE, HeaderValue::from_static("zte_ui_session=aa"));
    h.insert(ORIGIN, HeaderValue::from_static("http://u60.home.arpa:9443"));
    h.insert(REFERER, HeaderValue::from_static("http://u60.home.arpa:9443/ui/"));
    inbound(&mut h);
    assert!(h.get(COOKIE).is_none());
    assert_eq!((h[ORIGIN].to_str().unwrap(), h[REFERER].to_str().unwrap()), ("null", "null"));
    let mut h = HeaderMap::new();
    for c in [
        "zte_ui_session=ab; Path=/ui/; Max-Age=604800; HttpOnly; SameSite=Strict",
        "zte_ui_pre=; Path=/ui/; Max-Age=0; HttpOnly; SameSite=Strict",
        "zte_ui_refresh=1500; Domain=x; Secure; HttpOnly",
        "foreign=1; Path=/",
    ] {
        h.append(SET_COOKIE, HeaderValue::from_static(c));
    }
    outbound(&mut h);
    assert_eq!(
        h.get_all(SET_COOKIE).iter().map(|v| v.to_str().unwrap()).collect::<Vec<_>>(),
        [
            "__Host-zte_ui_session=ab; Path=/; Max-Age=604800; HttpOnly; SameSite=Strict; Secure",
            "__Host-zte_ui_pre=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict; Secure",
            "__Host-zte_ui_refresh=1500; HttpOnly; Path=/; Secure",
            "foreign=1; Path=/",
        ]
    );
}

/// A live stream over TLS: small events arrive at once (each chunk is
/// flushed through the TLS layer), and a disconnect gives back its demand.
#[test]
fn event_stream_over_tls_flushes() {
    use crate::auth::{Kind, PeerKey};
    use crate::events::Topic;
    let s = start("https-sse", 16);
    let addr = on(&s);
    // The state the listener serves (`start` put this manager in it).
    let app = s.app.clone();
    let peer = PeerKey::from(SocketAddr::from(([10, 0, 99, 1], 5000)));
    let tok = app.auth.login(PW, Kind::Bearer, peer, &[]).unwrap().token.expose().to_owned();
    let mut c = connect(addr, client(&s.tls.ca_der().unwrap(), &[&rustls::version::TLS13]), "u60.home.arpa").unwrap();
    let head = format!("GET /api/events?topics=sms HTTP/1.1\r\nHost: u60.home.arpa:{}\r\nAuthorization: Bearer {tok}\r\n\r\n", addr.port());
    c.write_all(head.as_bytes()).unwrap();
    c.flush().unwrap();
    let mut seen = String::new();
    let mut buf = [0u8; 4096];
    let mut until = |c: &mut Tls12, seen: &mut String, needle: &str| {
        while !seen.contains(needle) {
            let n = c.read(&mut buf).expect("stream stalled");
            assert!(n > 0, "closed before {needle:?}: {seen}");
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    };
    until(&mut c, &mut seen, "\"received\":0}}");
    assert!(seen.starts_with("HTTP/1.1 200 OK\r\n"), "{seen}");
    assert_eq!(app.hub.demand(Topic::Sms), 1);
    let t = Instant::now();
    app.hub.on_bus_event("zwrt_wms_status_event", &serde_json::json!({"wms_status": "new_sms_received"}));
    until(&mut c, &mut seen, "\"received\":1}}");
    assert!(t.elapsed() < Duration::from_secs(2));
    drop(c);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.hub.demand(Topic::Sms) != 0 {
        assert!(Instant::now() < deadline, "demand not released");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The value of hidden input `name` in a page.
fn input(body: &str, name: &str) -> String {
    body.split(&format!("name=\"{name}\" value=\"")).nth(1).unwrap().split('"').next().unwrap().to_owned()
}

/// Reset ends what HTTPS has open: a live stream and an idle keep-alive
/// connection close at once, not at their own limits; the Reset posted
/// over HTTPS still gets its reply first.
#[test]
fn reset_ends_open_connections() {
    use crate::auth::{Kind, PeerKey};
    let s = start("https-reset-open", 16);
    let addr = on(&s);
    let cfg = client(&s.tls.ca_der().unwrap(), &[&rustls::version::TLS13]);
    let host = format!("u60.home.arpa:{}", addr.port());
    let origin = format!("https://{host}");
    // A live stream.
    let peer = PeerKey::from(SocketAddr::from(([10, 0, 99, 2], 5000)));
    let tok = s.app.auth.login(PW, Kind::Bearer, peer, &[]).unwrap().token.expose().to_owned();
    let mut sse = connect(addr, Arc::clone(&cfg), "u60.home.arpa").unwrap();
    sse.write_all(format!("GET /api/events?topics=sms HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {tok}\r\n\r\n").as_bytes()).unwrap();
    sse.flush().unwrap();
    let mut buf = [0u8; 4096];
    let mut seen = String::new();
    while !seen.contains("\"received\":0}}") {
        let n = sse.read(&mut buf).unwrap();
        assert!(n > 0);
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    // An idle keep-alive connection.
    let mut idle = connect(addr, Arc::clone(&cfg), "u60.home.arpa").unwrap();
    assert_eq!(request(&mut idle, &format!("GET /ui/login HTTP/1.1\r\nHost: {host}"), "").status, 200);
    // A signed-in browser resets over HTTPS.
    let mut c = connect(addr, Arc::clone(&cfg), "u60.home.arpa").unwrap();
    let sess = login(&mut c, &host, &origin, "__Host-");
    let ck = format!("Cookie: __Host-zte_ui_session={sess}");
    let q = request(&mut c, &format!("GET /ui/settings/https/reset HTTP/1.1\r\nHost: {host}\r\n{ck}"), "");
    assert_eq!(q.status, 200);
    let form = format!("csrf={}&once={}&confirm=1", input(&q.body, "csrf"), input(&q.body, "once"));
    let head = format!("POST /ui/settings/https/reset HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: application/x-www-form-urlencoded\r\n{ck}");
    let r = request(&mut c, &head, &form);
    assert_eq!(r.status, 303, "{r:?}");
    assert!(r.header("location").is_some_and(|l| l.starts_with("/ui/settings?r=")));
    assert!(!s.tls.is_on());
    // The stream and the idle connection end now (not after 30 min / 120 s).
    let t = Instant::now();
    let mut ended = |io: &mut Tls12| loop {
        match io.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    };
    ended(&mut sse);
    ended(&mut idle);
    assert!(t.elapsed() < Duration::from_secs(8), "open connections outlived the reset: {:?}", t.elapsed());
    assert!(TcpStream::connect(addr).is_err_and(|_| true) || connect(addr, Arc::clone(&cfg), "u60.home.arpa").is_err());
}

/// A bind that fails (the port taken) is shown, and Turn On again binds
/// again once the port is free (no retry on its own).
#[test]
fn a_failed_bind_is_shown_and_tried_again_at_turn_on() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut world = World::new("https-bind");
    world.bind = taken.local_addr().unwrap().to_string();
    let s = start_with(world, 16, || |_: SocketAddr, _: &tokio::net::TcpStream| true);
    s.tls.turn_on().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !s.tls.status().error.is_some_and(|e| e.starts_with("Could not listen on 127.0.0.1:")) {
        assert!(Instant::now() < deadline, "no bind error: {:?}", s.tls.status());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(s.tls.status().listening.is_none());
    let port = taken.local_addr().unwrap().port();
    drop(taken);
    s.tls.turn_on().unwrap();
    assert_eq!(listening(&s.tls).port(), port);
    assert_eq!(s.tls.status().error, None);
}

/// A listener that ends by itself (its socket could not be registered) is
/// shown as not listening and waits for the next change: no busy loop on
/// the one runtime thread.
#[test]
fn a_listener_that_ends_by_itself_waits_for_the_next_change() {
    use std::sync::atomic::AtomicUsize;
    let world = World::new("https-ended");
    let tls = Arc::new(world.tls());
    tls.turn_on().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let (t, r) = (Arc::clone(&tls), Arc::clone(&runs));
    std::thread::spawn(move || {
        runtime(2).unwrap().block_on(supervise_with(t, move |_, _| {
            r.fetch_add(1, Ordering::SeqCst);
            std::future::ready(())
        }))
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !tls.status().error.is_some_and(|e| e.contains("stopped")) {
        assert!(Instant::now() < deadline, "not reported");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(runs.load(Ordering::SeqCst), 1, "it spun");
    assert!(tls.status().listening.is_none());
    tls.turn_on().unwrap();
    while runs.load(Ordering::SeqCst) < 2 {
        assert!(Instant::now() < deadline + Duration::from_secs(5), "not tried again");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    drop(world);
}

/// The HTTPS listener asks the mobile-side check (`Guard::twin`, its own
/// table) before any TLS: a peer on the data call's subnet is closed
/// without a handshake, a LAN peer is served. The test's connections come
/// from 127.0.0.1 (always served), so the check sees the address the test
/// sets.
#[test]
fn the_mobile_side_is_refused_before_the_handshake() {
    use crate::httpd::mobile::{Guard, IfAddr, Lookup};
    let ifa = |name: &str, addr: &str, prefix| IfAddr { name: name.into(), addr: addr.parse().unwrap(), prefix, scope: 0 };
    let table = vec![ifa("lo", "127.0.0.1", 8), ifa("br-lan", "192.168.0.1", 24), ifa("rmnet_data0", "198.51.100.10", 24)];
    let lookup: Lookup = Arc::new(move || Ok(table.clone()));
    let plain = Guard::new(false, lookup, crate::boottime::FakeClock::at_secs(1000));
    let seen_as: Arc<std::sync::Mutex<SocketAddr>> = Arc::new(std::sync::Mutex::new("198.51.100.1:40000".parse().unwrap()));
    let who = Arc::clone(&seen_as);
    let side = move || {
        let mut g = plain.twin();
        let who = Arc::clone(&who);
        move |_: SocketAddr, _: &tokio::net::TcpStream| {
            let peer = *who.lock().unwrap();
            g.check(peer, || None).0
        }
    };
    let s = start_with(World::new("https-mobile"), 16, side);
    let addr = on(&s);
    let cfg = client(&s.tls.ca_der().unwrap(), &[&rustls::version::TLS13]);
    assert!(connect(addr, Arc::clone(&cfg), "u60.home.arpa").is_err(), "a mobile-side peer got a handshake");
    *seen_as.lock().unwrap() = "192.168.0.23:40000".parse().unwrap();
    let mut c = connect(addr, cfg, "u60.home.arpa").unwrap();
    assert_eq!(request(&mut c, &format!("GET /ui/login HTTP/1.1\r\nHost: u60.home.arpa:{}", addr.port()), "").status, 200);
}
