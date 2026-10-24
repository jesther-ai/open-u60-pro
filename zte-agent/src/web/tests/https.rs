//! Settings › HTTPS & Install as App through the whole router: the
//! section, the questions and their single-use posts, the install page,
//! the public downloads and the http hints. The manager runs on a
//! temporary directory with fake clocks (`tls::tests::World`).

use std::sync::atomic::Ordering;

use super::*;
use crate::httpd::tls::Secure;
use crate::tls::tests::World;
use crate::tls::{CLOCK_FLOOR, DOWNLOAD_CRT, DOWNLOAD_PROFILE};

const SETTINGS: &str = "/ui/settings";
const ON: &str = "/ui/settings/https/on";
const RESET: &str = "/ui/settings/https/reset";
const RENEW: &str = "/ui/settings/https/renew";
const INSTALL: &str = "/ui/settings/https";

fn world(name: &str) -> World {
    let mut w = World::new(name);
    w.bind = "0.0.0.0:9443".into();
    w
}

fn ctx_with(w: &World, password: Option<&str>) -> Ctx {
    let mut state = AppState::new();
    state.tls = Arc::new(w.tls());
    if let Some(pw) = password {
        state.auth.set_password(pw);
    }
    Ctx::new(Arc::new(state), 2)
}

/// A GET as the HTTPS listener hands it on (`Secure`).
async fn get_secure(c: &Ctx, uri: &str, p: Peer, cookie: &str) -> Reply {
    send(c, req("GET", uri, p).header("cookie", cookie).extension(Secure).body(Body::empty()).unwrap()).await
}

/// The question's form posted: CSRF, its single-use token, `confirm=1`.
async fn confirm(c: &Ctx, uri: &str, p: Peer, cookie: &str) -> (Reply, String) {
    let q = get_with(c, uri, p, cookie).await;
    q.assert_secure_html(200);
    let body = format!("csrf={}&once={}&confirm=1", q.input("csrf"), q.input("once"));
    (post(c, uri, p, cookie, &body).await, body)
}

/// The line a 303 to Settings carries.
async fn line(c: &Ctx, r: &Reply, p: Peer, cookie: &str) -> String {
    let to = r.header("location").unwrap().to_owned();
    assert!(to.starts_with("/ui/settings?r="), "{to}");
    get_with(c, &to, p, cookie).await.text()
}

#[tokio::test]
async fn turn_on_from_settings_once() {
    let w = world("web-on");
    let c = ctx_with(&w, Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 230, 1).await);
    let p = peer(230, 2);
    let html = get_with(&c, SETTINGS, p, &cookie).await.text();
    for want in ["<h2>HTTPS &amp; Install as App</h2>", "<span>HTTPS</span><span class=\"v\">Off</span>", "href=\"/ui/settings/https/on\"", "Turn On HTTPS", "on port 9443"] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // The question: what happens, the router's clock (not checked), the token.
    let q = get_with(&c, ON, p, &cookie).await;
    q.assert_secure_html(200);
    let html = q.text();
    for want in [
        "Turn on HTTPS?",
        "its LAN address and u60.home.arpa",
        "never for other websites",
        "It could not be checked over the internet",
        "Plain http on port 9090 keeps working",
        "name=\"confirm\" value=\"1\"",
        ">Turn On HTTPS</button>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!q.input("once").is_empty());
    // Without confirm=1: back to the question; nothing done.
    let r = post(&c, ON, p, &cookie, &format!("csrf={}&once={}", q.input("csrf"), q.input("once"))).await;
    r.assert_see_other(ON);
    assert!(!c.app.tls.is_on());
    let (r, body) = confirm(&c, ON, p, &cookie).await;
    assert!(line(&c, &r, p, &cookie).await.contains("<p class=\"msg ok\" role=\"status\">HTTPS is on. Next: Install on Your Devices (a device that already trusts"));
    assert!(c.app.tls.is_on());
    let ca = c.app.tls.ca_der().unwrap();
    // A replay of the same post: the same line, nothing new.
    let again = post(&c, ON, p, &cookie, &body).await;
    assert!(line(&c, &again, p, &cookie).await.contains("HTTPS is on."));
    assert_eq!(c.app.tls.ca_der().unwrap(), ca);
    // Another session's token, or none: expired, nothing done.
    let r = post(&c, ON, p, &cookie, &format!("csrf={}&once=00112233aabbccdd&confirm=1", get_with(&c, SETTINGS, p, &cookie).await.input("csrf"))).await;
    assert!(line(&c, &r, p, &cookie).await.contains("This page expired, so nothing was changed."));
    // On: the addresses, Install on Your Devices, Reset; the http hint.
    let html = get_with(&c, SETTINGS, p, &cookie).await.text();
    for want in [
        "<span>HTTPS</span><span class=\"v\">On \u{b7} not listening</span>",
        "Certificate valid until",
        "href=\"https://192.168.0.1:9443/ui/\"",
        "href=\"https://u60.home.arpa:9443/ui/\"",
        "href=\"/ui/settings/https\"",
        "Install on Your Devices",
        "href=\"/ui/settings/https/reset\"",
        "This page is plain http. Once this device trusts the router\u{2019}s certificate authority (Install on Your Devices), open <a href=\"https://192.168.0.1:9443/ui/\" rel=\"noreferrer\">",
        ">Try Again</span>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains(RENEW));
    // Over HTTPS: no hint; over Tailscale (loopback): no hint either.
    let html = get_secure(&c, SETTINGS, p, &cookie).await.text();
    assert!(html.contains("You are using HTTPS.") && !html.contains("This page is plain http"));
    let html = get_with(&c, SETTINGS, Peer(SocketAddr::from(([127, 0, 0, 1], 40100))), &cookie).await.text();
    assert!(!html.contains("This page is plain http") && !html.contains("You are using HTTPS."));
    // On but not listening (a bind that failed): the question offers to
    // listen again, and its post tells the listener.
    let html = get_with(&c, ON, p, &cookie).await.text();
    assert!(html.contains("Listen again?") && html.contains(">Try Again</button>"), "{html}");
    let mut on = c.app.tls.subscribe();
    on.borrow_and_update();
    let (r, _) = confirm(&c, ON, p, &cookie).await;
    assert!(line(&c, &r, p, &cookie).await.contains("HTTPS is on."));
    assert!(on.has_changed().unwrap());
    assert_eq!(c.app.tls.ca_der().unwrap(), ca);
    // Listening: nothing to do.
    c.app.tls.set_listening(Ok("0.0.0.0:9443".parse().unwrap()));
    let html = get_with(&c, SETTINGS, p, &cookie).await.text();
    assert!(html.contains("<span class=\"v\">On</span>") && !html.contains("Try Again"));
    let html = get_with(&c, ON, p, &cookie).await.text();
    assert!(html.contains("HTTPS is already on") && !html.contains("name=\"once\""));
}

/// A CA kept at start (its server pair lost, no verified clock): Settings
/// says so, Turn On issues a pair from it (the same CA), and Reset can
/// delete the files while off.
#[tokio::test]
async fn a_kept_ca_is_turned_on_again() {
    let w = world("web-kept");
    w.tls().turn_on().unwrap();
    let ca = std::fs::read(w.dir().join("ca.crt")).unwrap();
    std::fs::write(w.dir().join("server.crt"), b"damaged").unwrap();
    let c = ctx_with(&w, Some(PW));
    c.app.tls.start();
    assert!(!c.app.tls.is_on());
    let cookie = format!("zte_ui_session={}", login(&c, 235, 1).await);
    let p = peer(235, 2);
    let html = get_with(&c, SETTINGS, p, &cookie).await.text();
    for want in ["is still on the router: Turn On HTTPS issues a new certificate from it", "issues a new one from the same certificate authority", "href=\"/ui/settings/https/reset\""] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let html = get_with(&c, ON, p, &cookie).await.text();
    assert!(html.contains("Devices that trust it need nothing new") && html.contains(">Turn On HTTPS</button>"), "{html}");
    let html = get_with(&c, RESET, p, &cookie).await.text();
    assert!(html.contains("This deletes the certificate files left on the router (U60 Pro local CA ") && html.contains(">Reset HTTPS</button>"), "{html}");
    let (r, _) = confirm(&c, ON, p, &cookie).await;
    assert!(line(&c, &r, p, &cookie).await.contains("HTTPS is on."));
    assert_eq!(c.app.tls.ca_der().unwrap(), ca);
}

#[tokio::test]
async fn questions_follow_the_clock_and_the_state() {
    let w = world("web-q");
    let c = ctx_with(&w, Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 231, 1).await);
    let p = peer(231, 2);
    // The clock is not set: no button.
    w.wall.store(CLOCK_FLOOR - 1, Ordering::SeqCst);
    let html = get_with(&c, ON, p, &cookie).await.text();
    assert!(html.contains("The router\u{2019}s clock isn\u{2019}t set yet") && !html.contains("name=\"once\""), "{html}");
    // Checked over the internet: said so.
    *w.verified.lock().unwrap() = Some(crate::tls::tests::NOW);
    let html = get_with(&c, ON, p, &cookie).await.text();
    assert!(html.contains("checked over the internet: Oct 26, 2026, 07:33 UTC."), "{html}");
    // Off: Renew and Reset have nothing to do.
    for uri in [RENEW, RESET] {
        let html = get_with(&c, uri, p, &cookie).await.text();
        assert!(html.contains("HTTPS is off") && !html.contains("name=\"once\""), "{uri}");
    }
    confirm(&c, ON, p, &cookie).await;
    // Renew shows once fewer than 30 days are left by the router's clock.
    let end = c.app.tls.status().not_after.unwrap();
    *w.verified.lock().unwrap() = None;
    w.wall.store(end - 10 * 86_400, Ordering::SeqCst);
    let html = get_with(&c, SETTINGS, p, &cookie).await.text();
    assert!(html.contains("Certificate expires") && html.contains("href=\"/ui/settings/https/renew\""), "{html}");
    let (r, _) = confirm(&c, RENEW, p, &cookie).await;
    assert!(line(&c, &r, p, &cookie).await.contains("Certificate renewed."));
    assert!(c.app.tls.status().not_after.unwrap() > end);
    // Reset: the question says what devices must do; over HTTPS that the
    // page stops working.
    let html = get_secure(&c, RESET, p, &cookie).await.text();
    for want in ["Reset HTTPS?", "Remove that certificate authority from every device", "must trust the new one", "This page is open over HTTPS", "btn btn-danger"] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let (r, _) = confirm(&c, RESET, p, &cookie).await;
    assert!(line(&c, &r, p, &cookie).await.contains("HTTPS was reset. Remove the old certificate authority from your devices."));
    assert!(!c.app.tls.is_on() && !w.dir().exists());
    // Signed out: a post goes to the login, back to Settings, nothing done.
    let r = post(&c, ON, p, "", "csrf=x&once=y&confirm=1").await;
    r.assert_see_other("/ui/login?next=%2Fui%2Fsettings&expired=1");
}

#[tokio::test]
async fn install_page_shows_the_ca_and_the_steps() {
    let w = world("web-install");
    let c = ctx_with(&w, Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 232, 1).await);
    let p = peer(232, 2);
    let r = get_with(&c, INSTALL, p, &cookie).await;
    r.assert_secure_html(200);
    assert!(r.text().contains("HTTPS is off"));
    c.app.tls.turn_on().unwrap();
    let s = c.app.tls.status();
    let r = get_with(&c, INSTALL, p, &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        &format!("<span class=\"v\">{}</span>", s.ca_name),
        "<span>Can vouch for</span><span class=\"v\">192.168.0.1 and u60.home.arpa</span>",
        &format!("<span class=\"sr\">{}</span><small class=\"mono fp\" translate=\"no\" aria-hidden=\"true\">", s.sha256),
        &s.sha256.replace(':', ":<wbr>"),
        "came over plain http, so someone on your network could change it",
        "sha256sum /data/zte-agent-tls/ca.crt",
        "letter case, spaces and colons do not matter",
        "More Details",
        "Firefox 120 and later uses the certificates you add",
        "View More",
        &s.sha1.replace(':', "").to_ascii_lowercase(),
        &format!("href=\"{DOWNLOAD_PROFILE}\""),
        &format!("href=\"{DOWNLOAD_CRT}\""),
        "<h2>iPhone and iPad</h2>",
        "Certificate Trust Settings",
        "<h2>Android</h2>",
        "Install a certificate",
        "<h2>Mac</h2>",
        "Always Trust",
        "<h2>Windows</h2>",
        "Trusted Root Certification Authorities",
        "href=\"https://u60.home.arpa:9443/ui/\"",
        "Add to Home Screen",
        "Private DNS",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // Over HTTPS the fingerprint came from the router itself.
    let html = get_secure(&c, INSTALL, p, &cookie).await.text();
    assert!(html.contains("This page came over HTTPS from the router") && !html.contains("came over plain http"));
    // Never a key.
    let key = std::fs::read(w.dir().join("issuing.key")).unwrap();
    use base64::Engine;
    assert!(!html.contains(&base64::engine::general_purpose::STANDARD.encode(key)));
}

/// The downloads: public (no session, no password needed), their content
/// types, never stored, behind the Host check; 404 while HTTPS is off.
#[tokio::test]
async fn downloads_are_public_with_their_types() {
    let w = world("web-dl");
    for password in [Some(PW), None] {
        let c = ctx_with(&w, password);
        for uri in [DOWNLOAD_CRT, DOWNLOAD_PROFILE] {
            let r = get(&c, uri, peer(233, 1)).await;
            assert_eq!((r.status, r.header("content-type")), (404, Some("text/plain; charset=utf-8")));
            r.assert_security_headers();
        }
        c.app.tls.turn_on().unwrap();
        let ca = c.app.tls.ca_der().unwrap();
        let r = get(&c, DOWNLOAD_CRT, peer(233, 2)).await;
        assert_eq!(r.status, 200);
        r.assert_security_headers();
        assert_eq!(r.header("content-type"), Some("application/x-x509-ca-cert"));
        assert_eq!(r.header("cache-control"), Some("no-store"));
        assert_eq!(r.header("content-disposition"), Some("attachment; filename=\"u60-pro-ca.crt\""));
        assert_eq!(r.body, ca);
        assert!(r.cookies().is_empty());
        let r = get(&c, DOWNLOAD_PROFILE, peer(233, 3)).await;
        assert_eq!((r.status, r.header("content-type")), (200, Some("application/x-apple-aspen-config")));
        assert_eq!((r.header("cache-control"), r.header("content-disposition")), (Some("no-store"), Some("attachment; filename=\"u60-pro-ca.mobileconfig\"")));
        assert!(r.text().contains("<string>com.apple.security.root</string>"));
        let r = send(&c, req("HEAD", DOWNLOAD_CRT, peer(233, 4)).body(Body::empty()).unwrap()).await;
        assert_eq!(r.status, 200);
        let evil = axum::http::Request::builder().uri(DOWNLOAD_CRT).header("host", "evil.example").extension(peer(233, 5)).body(Body::empty()).unwrap();
        assert_eq!(send(&c, evil).await.status, 403);
        // Posts are not downloads.
        let r = send(&c, form(DOWNLOAD_CRT, peer(233, 6), "", "").body(Body::empty()).unwrap()).await;
        assert_eq!((r.status, r.header("content-type")), (405, Some("text/plain; charset=utf-8")));
        c.app.tls.reset().unwrap();
    }
}

/// The login page over plain http points to the https address while
/// HTTPS is on; never over HTTPS or Tailscale.
#[tokio::test]
async fn login_page_hint() {
    let w = world("web-hint");
    let c = ctx_with(&w, Some(PW));
    let hint = "To install this web UI as an app, trust the router\u{2019}s certificate authority on this device (Settings \u{203a} HTTPS &amp; Install as App \u{203a} Install on Your Devices), then open <a href=\"https://192.168.0.1:9443/ui/\" rel=\"noreferrer\">https://192.168.0.1:9443/ui/</a>.";
    assert!(!get(&c, "/ui/login", peer(234, 1)).await.text().contains("To install"));
    c.app.tls.turn_on().unwrap();
    let r = get(&c, "/ui/login", peer(234, 2)).await;
    r.assert_secure_html(200);
    assert!(r.text().contains(hint), "{}", r.text());
    assert!(!get_secure(&c, "/ui/login", peer(234, 3), "").await.text().contains("To install"));
    assert!(!get(&c, "/ui/login", Peer(SocketAddr::from(([127, 0, 0, 1], 40101)))).await.text().contains("To install"));
    // The local name answers only over HTTPS (another network's DNS can
    // answer for it over http).
    let by_name = |secure: bool| {
        let b = axum::http::Request::builder().uri("/ui/login").header("host", "u60.home.arpa:9443").extension(peer(234, 5));
        if secure { b.extension(Secure) } else { b }.body(Body::empty()).unwrap()
    };
    let r = send(&c, by_name(false)).await;
    assert_eq!((r.status, r.text().as_str()), (403, "Host not allowed"));
    let r = send(&c, by_name(true)).await;
    assert!(r.status == 200 && !r.text().contains("To install"));
    // A failed login keeps it.
    let (pre, token) = login_form(&c, peer(234, 4)).await;
    let r = post(&c, "/ui/login", peer(234, 4), &format!("zte_ui_pre={pre}"), &format!("login_csrf={}&password=wrong", enc(&token))).await;
    assert_eq!(r.status, 401);
    assert!(r.text().contains(hint));
}

/// `GET /api/https` for the apps (in-process, as the gate runs it).
#[test]
fn api_status_route() {
    let w = world("web-api");
    let mut state = AppState::new();
    state.tls = Arc::new(w.tls());
    let (status, v) = crate::server::route(&crate::server::Method::Get, "/api/https", &state, b"");
    assert_eq!((status, v["data"]["enabled"].as_bool(), v["data"]["port"].as_u64()), (200, Some(false), Some(9443)));
    state.tls.turn_on().unwrap();
    let (_, v) = crate::server::route(&crate::server::Method::Get, "/api/https", &state, b"");
    assert_eq!(v["data"]["urls"], serde_json::json!(["https://192.168.0.1:9443/ui/", "https://u60.home.arpa:9443/ui/"]));
    assert_eq!(crate::server::lane(crate::server::Method::Get, "/api/https"), crate::httpd::gate::Lane::General);
}
