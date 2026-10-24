//! `/`, `/ui` and `/ui/**` through the whole router (`httpd::app`), as the
//! listener runs it. Every test that logs in builds its own state (its own
//! limiter) and uses its own peer addresses (`10.1.<test>.<n>`).

use std::io::Read;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::header::{HeaderMap, SET_COOKIE};
use axum::http::request::Builder;
use serde_json::Value;
use tower::ServiceExt;

use super::assets::{ASSETS, APP_CSS, ICON_180, MANIFEST_ETAG, THEME_JS};
use super::session::{CLEAR_MARKER, CLEAR_PRE, CLEAR_SESSION};
use crate::auth::{AuthConfig, AuthState, FailRng, Kind, PeerKey, SeqRng};
use crate::boottime::FakeClock;
use crate::handlers::AppState;
use crate::httpd::gate::Gate;
use crate::httpd::{app, Ctx, Peer};

const PW: &str = "test-password";
const HOST: &str = "192.168.0.1:9090";
const ORIGIN: &str = "http://192.168.0.1:9090";

fn peer(test: u8, n: u8) -> Peer {
    Peer(SocketAddr::from(([10, 1, test, n], 5000)))
}

fn ctx(password: Option<&str>) -> Ctx {
    let state = AppState::new();
    if let Some(pw) = password {
        state.auth.set_password(pw);
    }
    Ctx::new(Arc::new(state), 2)
}

/// With the password set and auth on a fake clock (deterministic tokens).
fn ctx_at(clock: Arc<FakeClock>) -> Ctx {
    let mut state = AppState::new();
    state.auth = Arc::new(AuthState::with_parts(clock, Box::new(SeqRng::new()), AuthConfig::default()));
    state.auth.set_password(PW);
    Ctx::new(Arc::new(state), 2)
}

fn req(method: &str, uri: &str, peer: Peer) -> Builder {
    axum::http::Request::builder().method(method).uri(uri).header("host", HOST).extension(peer)
}

struct Reply {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn cookies(&self) -> Vec<&str> {
        self.headers.get_all(SET_COOKIE).iter().map(|v| v.to_str().unwrap()).collect()
    }

    /// The value set for cookie `name`, if any.
    fn cookie(&self, name: &str) -> Option<String> {
        self.cookies().iter().find_map(|c| {
            let v = c.strip_prefix(name)?.strip_prefix('=')?;
            Some(v.split(';').next().unwrap().to_owned())
        })
    }

    /// An HTML page with every security header and a fresh CSP nonce. The
    /// markup carries the nonce only on its script tags (the System theme's
    /// `theme.js`, Datastar) and, on a live page (Datastar), as `data-nonce`.
    fn assert_secure_html(&self, status: u16) {
        assert_eq!(self.status, status, "{}", self.text());
        assert_eq!(self.header("content-type"), Some("text/html; charset=utf-8"));
        self.assert_security_headers();
        assert_eq!(self.header("cache-control"), Some("no-store"));
        let csp = self.header("content-security-policy").expect("CSP");
        let nonce = csp.split("'nonce-").nth(1).unwrap().split('\'').next().unwrap();
        assert_eq!(nonce.len(), 32);
        assert!(nonce.bytes().all(|b| b.is_ascii_hexdigit()));
        let html = self.text();
        let scripts = html.matches("<script").count();
        assert_eq!(html.matches(&format!(" nonce=\"{nonce}\"")).count(), scripts, "a script without this response's nonce");
        let live = html.contains("/ui/a/datastar-");
        assert_eq!(html.contains(&format!("data-nonce=\"{nonce}\"")), live, "data-nonce only on a live page");
        assert_eq!(html.matches(nonce).count(), scripts + usize::from(live), "the nonce elsewhere in the markup");
        // Scripts by nonce only (no 'self').
        assert!(csp.starts_with("default-src 'none'; script-src 'nonce-"));
        assert_eq!(self.header("cross-origin-opener-policy"), Some("same-origin"));
        assert!(csp.ends_with(
            "style-src 'self'; img-src 'self'; connect-src 'self'; form-action 'self'; \
             base-uri 'none'; frame-ancestors 'none'; manifest-src 'self'"
        ));
    }

    fn assert_security_headers(&self) {
        assert_eq!(self.header("x-frame-options"), Some("DENY"));
        assert_eq!(self.header("referrer-policy"), Some("same-origin"));
        assert_eq!(self.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(self.header("cross-origin-resource-policy"), Some("same-origin"));
    }

    fn assert_see_other(&self, location: &str) {
        assert_eq!((self.status, self.header("location")), (303, Some(location)), "{}", self.text());
    }

    /// The session's CSRF token, from this page or else the logout page.
    async fn input_or_logout(&self, c: &Ctx, cookie: &str) -> String {
        if self.text().contains("name=\"csrf\" value=\"") {
            return self.input("csrf");
        }
        get_with(c, "/ui/logout", peer(1, 1), cookie).await.input("csrf")
    }

    /// `value="…"` of the input named `name`.
    fn input(&self, name: &str) -> String {
        let html = self.text();
        let at = html.find(&format!("name=\"{name}\" value=\"")).unwrap_or_else(|| panic!("no {name} in {html}"));
        let rest = &html[at + name.len() + 15..];
        rest[..rest.find('"').unwrap()].to_owned()
    }
}

async fn send(ctx: &Ctx, req: axum::http::Request<Body>) -> Reply {
    let resp = app(ctx.clone()).oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec();
    Reply { status, headers, body }
}

async fn get(ctx: &Ctx, uri: &str, peer: Peer) -> Reply {
    send(ctx, req("GET", uri, peer).body(Body::empty()).unwrap()).await
}

async fn get_with(ctx: &Ctx, uri: &str, peer: Peer, cookie: &str) -> Reply {
    send(ctx, req("GET", uri, peer).header("cookie", cookie).body(Body::empty()).unwrap()).await
}

fn form(uri: &str, peer: Peer, cookie: &str, body: &str) -> Builder {
    let b = req("POST", uri, peer)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("content-length", body.len());
    if cookie.is_empty() {
        b
    } else {
        b.header("cookie", cookie)
    }
}

async fn post(ctx: &Ctx, uri: &str, peer: Peer, cookie: &str, body: &str) -> Reply {
    let r = form(uri, peer, cookie, body).header("origin", ORIGIN).body(Body::from(body.to_owned())).unwrap();
    send(ctx, r).await
}

/// The login form: (pre cookie, login_csrf).
async fn login_form(ctx: &Ctx, peer: Peer) -> (String, String) {
    let r = get(ctx, "/ui/login", peer).await;
    assert_eq!(r.status, 200);
    (r.cookie("zte_ui_pre").unwrap(), r.input("login_csrf"))
}

fn enc(s: &str) -> String {
    super::session::percent_encode(s)
}

/// Log in; returns the session cookie value.
async fn login(ctx: &Ctx, test: u8, n: u8) -> String {
    let (pre, token) = login_form(ctx, peer(test, n)).await;
    let body = format!("login_csrf={}&next=%2Fui%2F&password={}", enc(&token), enc(PW));
    let r = post(ctx, "/ui/login", peer(test, n), &format!("zte_ui_pre={pre}"), &body).await;
    r.assert_see_other("/ui/");
    r.cookie("zte_ui_session").unwrap()
}

/// The budget for the one stylesheet (owner decision: 48 KiB for the app copy).
#[test]
fn app_css_at_most_48_kib() {
    let css = include_str!("assets/app.css");
    assert!(css.len() <= 48 * 1024);
    // A snapshot (no JavaScript) hides the age badge, which would never
    // move, and shows its own age instead; live, the other way round.
    assert!(css.contains("body:has(#live-status.snap) #age, body:not(:has(#live-status.snap)) .sa { display: none; }"));
}

/// Rolling numbers are a `span.rl` (and a `span.sr`) inside their
/// element: a rule that sets `display` on every span of a container beats
/// `.rl { display: inline-block }` and drops the unit onto its own line
/// (Connection & signal's metric grids did). Labels are named by class.
/// A rolled number is an inline block, which gives the line a break point
/// before its unit: on Connection & signal's grids a 3-digit value put
/// "dBm" on its own line at phone widths (the panels grew from 48 to 71 px
/// while live). And its visible copy is `aria-hidden` next to the `.sr`
/// text: only the `.sr` text may be selected, so a copy gets it once.
#[test]
fn rolled_numbers_keep_their_unit_and_copy_once() {
    let css = include_str!("assets/app.css");
    assert!(css.contains("\n.metrics > div { white-space: nowrap; }\n"));
    let rl = css.lines().find(|l| l.starts_with(".rl {")).unwrap();
    assert!(rl.contains("-webkit-user-select: none;") && rl.contains(" user-select: none;"), "{rl}");
}

#[test]
fn no_rule_sets_display_on_every_span() {
    // Without comments.
    let css: String = include_str!("assets/app.css").split("/*").enumerate().map(|(i, part)| if i == 0 { part } else { part.split_once("*/").map_or("", |(_, rest)| rest) }).collect();
    let mut bad = Vec::new();
    let mut from = 0;
    while let Some(open) = css[from..].find('{').map(|i| from + i) {
        let start = css[..open].rfind(['{', '}', ';']).map_or(0, |i| i + 1);
        let close = css[open + 1..].find(['}', '{']).map_or(css.len(), |i| open + 1 + i);
        let (selectors, decls) = (&css[start..open], &css[open + 1..close]);
        if decls.contains("display:") {
            for sel in selectors.split(',').map(str::trim) {
                let last = sel.rsplit([' ', '>']).next().unwrap_or(sel);
                // Containers without live numbers: AT chips, the snapshot
                // ages, the live pill, cell signal squares, file buttons.
                if last == "span" && ![".chips span", ".ages > span", ".on > span", ".sq > span", ".fbtn span"].contains(&sel) {
                    bad.push(sel.to_owned());
                }
            }
        }
        from = open + 1;
    }
    assert!(bad.is_empty(), "{bad:?}");
}

#[tokio::test]
async fn host_allowlist_and_redirects() {
    let c = ctx(Some(PW));
    let evil = axum::http::Request::builder()
        .uri("/ui/login")
        .header("host", "evil.example")
        .extension(peer(1, 1))
        .body(Body::empty())
        .unwrap();
    let r = send(&c, evil).await;
    assert_eq!((r.status, r.text().as_str()), (403, "Host not allowed"));
    r.assert_security_headers();
    let none = axum::http::Request::builder().uri("/ui/").extension(peer(1, 2)).body(Body::empty()).unwrap();
    let r = send(&c, none).await;
    assert_eq!((r.status, r.text().as_str()), (400, "Missing Host"));
    // `localhost` (like the Tailscale names) only from this device: a LAN
    // browser never gets there for that name.
    let local = |p: Peer| {
        axum::http::Request::builder().uri("/ui/login").header("host", "localhost:9090").extension(p).body(Body::empty()).unwrap()
    };
    let r = send(&c, local(peer(1, 8))).await;
    assert_eq!((r.status, r.text().as_str()), (403, "Host not allowed"));
    send(&c, local(Peer(SocketAddr::from(([127, 0, 0, 1], 40002))))).await.assert_secure_html(200);
    get(&c, "/ui/login", peer(1, 3)).await.assert_secure_html(200);
    get(&c, "/", peer(1, 4)).await.assert_see_other("/ui/");
    get(&c, "/ui", peer(1, 5)).await.assert_see_other("/ui/");
    // Other methods on `/` stay the API's.
    let r = send(&c, req("POST", "/", peer(1, 6)).body(Body::empty()).unwrap()).await;
    assert_eq!((r.status, r.text().as_str()), (401, r#"{"error":"unauthorized","ok":false}"#));
    assert_eq!(r.header("content-type"), Some("application/json"));
    let open = ctx(None);
    let r = send(&open, req("POST", "/", peer(1, 7)).body(Body::empty()).unwrap()).await;
    assert_eq!((r.status, r.text().as_str()), (404, r#"{"error":"not found","ok":false}"#));
}

#[tokio::test]
async fn unknown_ui_paths_get_the_html_404() {
    let c = ctx(Some(PW));
    for uri in ["/ui/nope", "/ui/settings/", "/ui/a/x/y", "/ui/login/", "/ui/a/"] {
        let r = get(&c, uri, peer(2, 1)).await;
        r.assert_secure_html(404);
        assert!(r.text().contains("Page not found"));
    }
    // A posted body is read off, so the connection can stay.
    let r = send(&c, form("/ui/nope", peer(2, 4), "", "a=1").body(Body::from("a=1")).unwrap()).await;
    r.assert_secure_html(404);
    assert_eq!(r.header("connection"), None);
    // Unknown asset: plain text.
    let r = get(&c, "/ui/a/nope.css", peer(2, 2)).await;
    assert_eq!((r.status, r.header("content-type")), (404, Some("text/plain; charset=utf-8")));
    // The API is untouched.
    let r = get(&c, "/api/nope", peer(2, 3)).await;
    assert_eq!((r.status, r.header("content-type")), (401, Some("application/json")));
}

#[tokio::test]
async fn sessions_are_separate_from_bearer_tokens() {
    let c = ctx(Some(PW));
    get(&c, "/ui/", peer(3, 1)).await.assert_see_other("/ui/login?next=%2Fui%2F");
    let bearer = c.app.auth.login(PW, Kind::Bearer, PeerKey::from(peer(3, 2).0), &[]).unwrap();
    let r = send(
        &c,
        req("GET", "/ui/", peer(3, 3)).header("authorization", format!("Bearer {}", bearer.token.expose())).body(Body::empty()).unwrap(),
    )
    .await;
    r.assert_see_other("/ui/login?next=%2Fui%2F");
    // Not even as a cookie value.
    get_with(&c, "/ui/", peer(3, 4), &format!("zte_ui_session={}", bearer.token.expose())).await.assert_see_other("/ui/login?next=%2Fui%2F");
    let session = login(&c, 3, 5).await;
    let r = get_with(&c, "/api/device", peer(3, 6), &format!("zte_ui_session={session}")).await;
    assert_eq!(r.status, 401);
    // The cookie works for /ui; three cookies with the valid one last too.
    let cookie = format!("zte_ui_session={}; zte_ui_session=bad; zte_ui_session={session}", "0".repeat(64));
    let r = get_with(&c, "/ui/", peer(3, 7), &cookie).await;
    r.assert_secure_html(200);
    // The snapshot is complete without JavaScript; with it, the one
    // `data-init` opens the live stream.
    assert_eq!(r.text().matches("data-").count(), 2, "data-nonce and data-init only");
    assert!(r.text().contains("<div id=\"live\" hidden data-init=\"document.hidden ? "));
    assert!(r.text().contains(" : @get('/ui/live/dashboard/"));
    assert!(r.text().contains("<p class=\"snap ps\" id=\"live-status\"><a href=\"/ui/\">Snapshot<span class=\"sr\">"));
    assert!(r.text().contains("<span class=\"agebadge sa ages\"><span class=\"age1\">over 1 min old</span><span class=\"age5\">over 5 min old</span></span></div></header>"));
    for id in ["live-status", "live-note", "age", "banners", "card-conn", "card-traffic", "card-battery", "card-health", "wifi-state", "card-devices"] {
        assert!(r.text().contains(&format!("id=\"{id}\"")), "{id}");
    }
    assert!(!r.text().contains("card-wan") && !r.text().contains("WAN addresses"));
    assert!(r.text().contains("aria-current=\"page\""));
    // Junk is skipped; the 17th well-formed value is not looked at.
    let junk = |n: usize| (0..n).map(|i| format!("zte_ui_session={:064x}; ", i + 1)).collect::<String>();
    let cookie = format!("zte_ui_session=a; zte_ui_session=b; zte_ui_session=c; {}zte_ui_session={session}", junk(15));
    get_with(&c, "/ui/", peer(3, 8), &cookie).await.assert_secure_html(200);
    let cookie = format!("{}zte_ui_session={session}", junk(16));
    get_with(&c, "/ui/", peer(3, 8), &cookie).await.assert_see_other("/ui/login?next=%2Fui%2F");
}

/// Cookies are not port-scoped: a page on port 80 of the same host can plant
/// session cookies with deeper paths, which the browser sends first.
#[tokio::test]
async fn planted_deeper_path_cookies_do_not_lock_out_settings() {
    let (c, cookie, _) = signed_in(30).await;
    let csrf = get_with(&c, MN, peer(30, 2), &cookie).await.input("csrf");
    let planted: String = (1..=5).map(|i| format!("zte_ui_session={i:064x}; ")).collect();
    let r = post(&c, &format!("{MN}/mobile-data"), peer(30, 3), &format!("{planted}{cookie}"), &format!("csrf={csrf}&enable=0")).await;
    r.assert_see_other(&format!("{MN}/mobile-data/confirm?enable=0"));
}

#[tokio::test]
async fn login_form_and_pre_cookie() {
    let c = ctx(Some(PW));
    let r = get(&c, "/ui/login?next=%2Fui%2Fsignal&expired=1", peer(4, 1)).await;
    r.assert_secure_html(200);
    let pre = r.cookie("zte_ui_pre").unwrap();
    assert_eq!(r.cookies(), [format!("zte_ui_pre={pre}; Path=/ui/; Max-Age=900; HttpOnly; SameSite=Strict")]);
    assert_eq!(pre.len(), 64);
    assert_eq!(r.input("next"), "/ui/signal");
    assert!(r.text().contains("Your session expired. Nothing was changed."));
    // The CSRF field comes first in the form.
    let html = r.text();
    let form = &html[html.find("<form").unwrap()..];
    assert!(form[..form.find("<input").unwrap() + 40].contains("name=\"login_csrf\""));
    // A well-formed pre cookie is reused (second tab, back button).
    let r = get_with(&c, "/ui/login?next=//evil", peer(4, 2), &format!("zte_ui_pre={pre}")).await;
    assert_eq!(r.cookie("zte_ui_pre").as_deref(), Some(pre.as_str()));
    assert_eq!(r.input("next"), "/ui/");
    // A malformed one is replaced.
    let r = get_with(&c, "/ui/login", peer(4, 3), "zte_ui_pre=XYZ").await;
    assert_ne!(r.cookie("zte_ui_pre").as_deref(), Some("XYZ"));
    // Already logged in: straight to `next`.
    let session = login(&c, 4, 4).await;
    get_with(&c, "/ui/login?next=%2Fui%2Fsettings", peer(4, 5), &format!("zte_ui_session={session}")).await.assert_see_other("/ui/settings");
}

#[tokio::test]
async fn login_post_checks_in_order() {
    let c = ctx(Some(PW));
    let p = peer(5, 1);
    let (pre, token) = login_form(&c, p).await;
    let pre_cookie = format!("zte_ui_pre={pre}");
    let pw = format!("&password={}", enc(PW));
    // Missing or bad form token: 403 (not 400), with a fresh token.
    for body in [format!("next=%2Fui%2F{pw}"), format!("login_csrf=1.00{pw}"), format!("login_csrf={}x{pw}", enc(&token))] {
        let r = post(&c, "/ui/login", p, &pre_cookie, &body).await;
        r.assert_secure_html(403);
        assert!(r.text().contains("The login form expired. Try again."));
        assert!(!r.input("login_csrf").is_empty());
    }
    // A token bound to another pre cookie.
    let (other_pre, _) = login_form(&c, peer(5, 2)).await;
    let body = format!("login_csrf={}{pw}", enc(&token));
    let r = post(&c, "/ui/login", p, &format!("zte_ui_pre={other_pre}"), &body).await;
    assert_eq!(r.status, 403);
    // No pre cookie: cookies are off.
    let r = post(&c, "/ui/login", p, "", &body).await;
    r.assert_secure_html(403);
    assert!(r.text().contains("Cookies must be enabled to log in."));
    assert!(r.cookie("zte_ui_pre").is_some(), "a new pre cookie for the next try");
    // Cross-site or from the router's own web UI on port 80: refused before anything.
    for (origin, site) in [("http://evil.example", "cross-site"), ("http://192.168.0.1", "same-site")] {
        let r = form("/ui/login", p, &pre_cookie, &body)
            .header("origin", origin)
            .header("sec-fetch-site", site)
            .body(Body::from(body.clone()))
            .unwrap();
        let r = send(&c, r).await;
        r.assert_secure_html(403);
        assert!(r.text().contains("This form expired or came from another site."));
    }
    // Not a form: 400.
    let r = req("POST", "/ui/login", p).header("content-type", "application/json").header("cookie", &pre_cookie).body(Body::from("{}")).unwrap();
    send(&c, r).await.assert_secure_html(400);
    // Too long: 413.
    let long = format!("login_csrf={}&password={}", enc(&token), "a".repeat(9000));
    post(&c, "/ui/login", p, &pre_cookie, &long).await.assert_secure_html(413);
    // Right: session cookie, pre cookie cleared, to `next`.
    let body = format!("login_csrf={}&next=%2Fui%2Fsignal{pw}", enc(&token));
    let r = post(&c, "/ui/login", p, &pre_cookie, &body).await;
    r.assert_see_other("/ui/signal");
    let session = r.cookie("zte_ui_session").unwrap();
    assert_eq!(session.len(), 64);
    assert_eq!(
        r.cookies(),
        [
            format!("zte_ui_session={session}; Path=/ui/; Max-Age=604800; HttpOnly; SameSite=Strict"),
            // The marker hop's cookie (owner decision 61): no secret, Lax.
            "zte_ui_signed_in=1; Path=/ui/; Max-Age=604800; HttpOnly; SameSite=Lax".to_owned(),
            CLEAR_PRE.to_owned()
        ]
    );
    assert_eq!(CLEAR_PRE, "zte_ui_pre=; Path=/ui/; Max-Age=0; HttpOnly; SameSite=Strict");
    assert!(c.app.auth.authenticate(Kind::Browser, &session).is_some());
    // An unsafe `next` goes to the dashboard.
    let body = format!("login_csrf={}&next=%2F%2Fevil.example{pw}", enc(&token));
    post(&c, "/ui/login", peer(5, 3), &pre_cookie, &body).await.assert_see_other("/ui/");
}

#[tokio::test]
async fn wrong_password_then_rate_limit() {
    let clock = FakeClock::at_secs(1000);
    let c = ctx_at(clock.clone());
    let p = peer(6, 1);
    let (pre, token) = login_form(&c, p).await;
    let cookie = format!("zte_ui_pre={pre}");
    let body = format!("login_csrf={}&next=%2Fui%2Fsignal&password=Wrong-Secret-42", enc(&token));
    for _ in 0..5 {
        let r = post(&c, "/ui/login", p, &cookie, &body).await;
        r.assert_secure_html(401);
        assert!(r.text().contains("Wrong password."));
        assert!(!r.text().contains("Wrong-Secret-42"), "password echoed");
        assert_eq!(r.input("next"), "/ui/signal");
        assert!(c.app.auth.verify_login_form_token(&pre, &r.input("login_csrf")));
        assert!(r.cookie("zte_ui_session").is_none());
    }
    let r = post(&c, "/ui/login", p, &cookie, &body).await;
    r.assert_secure_html(429);
    assert_eq!(r.header("retry-after"), Some("1"));
    assert!(r.text().contains("Too many attempts. Try again in 1 s."));
    // The right password while blocked is still 429.
    let body = format!("login_csrf={}&password={}", enc(&token), enc(PW));
    assert_eq!(post(&c, "/ui/login", p, &cookie, &body).await.status, 429);
    // Once the wait is over it logs in.
    clock.advance(Duration::from_secs(1));
    post(&c, "/ui/login", p, &cookie, &body).await.assert_see_other("/ui/");
}

#[tokio::test]
async fn stale_form_without_its_cookie_is_expired() {
    let clock = FakeClock::at_secs(1000);
    let c = ctx_at(clock.clone());
    let (_, token) = login_form(&c, peer(15, 1)).await;
    let body = format!("login_csrf={}&password={}", enc(&token), enc(PW));
    // The browser dropped the pre cookie with the form: the form expired.
    clock.advance(Duration::from_secs(15 * 60 + 1));
    let r = post(&c, "/ui/login", peer(15, 1), "", &body).await;
    r.assert_secure_html(403);
    assert!(r.text().contains("The login form expired. Try again."), "{}", r.text());
    // The form token does not show the boot time (1000 s).
    assert!(!token.starts_with("1000."), "{token}");
}

#[tokio::test]
async fn login_rotates_presented_sessions() {
    let c = ctx(Some(PW));
    let old = login(&c, 7, 1).await;
    let (pre, token) = login_form(&c, peer(7, 2)).await;
    let body = format!("login_csrf={}&password={}", enc(&token), enc(PW));
    let r = post(&c, "/ui/login", peer(7, 2), &format!("zte_ui_session={old}; zte_ui_pre={pre}"), &body).await;
    let new = r.cookie("zte_ui_session").unwrap();
    assert_ne!(new, old);
    assert!(c.app.auth.authenticate(Kind::Browser, &old).is_none());
    assert!(c.app.auth.authenticate(Kind::Browser, &new).is_some());
}

#[tokio::test]
async fn logout() {
    let c = ctx(Some(PW));
    let session = login(&c, 8, 1).await;
    let cookie = format!("zte_ui_session={session}");
    let r = get_with(&c, "/ui/logout", peer(8, 2), &cookie).await;
    r.assert_secure_html(200);
    let csrf = r.input("csrf");
    // No CSRF field, a wrong one, cross-origin: 403, still logged in.
    for body in [String::new(), "csrf=00".into(), format!("csrf={}", "0".repeat(64))] {
        post(&c, "/ui/logout", peer(8, 3), &cookie, &body).await.assert_secure_html(403);
    }
    let r = form("/ui/logout", peer(8, 4), &cookie, &format!("csrf={csrf}"))
        .header("origin", "http://evil.example")
        .body(Body::from(format!("csrf={csrf}")))
        .unwrap();
    send(&c, r).await.assert_secure_html(403);
    let r = form("/ui/logout", peer(8, 4), &cookie, &format!("csrf={csrf}"))
        .header("referer", "http://192.168.0.1/index.html")
        .body(Body::from(format!("csrf={csrf}")))
        .unwrap();
    send(&c, r).await.assert_secure_html(403);
    assert!(c.app.auth.authenticate(Kind::Browser, &session).is_some());
    // Without Origin a same-origin Referer passes.
    let r = form("/ui/logout", peer(8, 5), &cookie, &format!("csrf={csrf}"))
        .header("referer", "http://192.168.0.1:9090/ui/logout")
        .body(Body::from(format!("csrf={csrf}")))
        .unwrap();
    let r = send(&c, r).await;
    r.assert_see_other("/ui/login");
    assert_eq!(r.cookies(), [CLEAR_SESSION, CLEAR_MARKER]);
    assert!(CLEAR_SESSION.contains("; Path=/ui/;"));
    assert!(c.app.auth.authenticate(Kind::Browser, &session).is_none());
    get_with(&c, "/ui/", peer(8, 6), &cookie).await.assert_see_other("/ui/login?next=%2Fui%2F");
    // Without a session: a stale cookie is cleared all the same.
    let r = post(&c, "/ui/logout", peer(8, 7), &cookie, "csrf=x").await;
    r.assert_see_other("/ui/login");
    assert_eq!(r.cookies(), [CLEAR_SESSION]);
    let r = post(&c, "/ui/logout", peer(8, 7), "", "csrf=x").await;
    r.assert_see_other("/ui/login");
    assert!(r.cookies().is_empty());
    // A cross-site post (which SameSite=Strict sends without the cookie)
    // must not delete the cookie: refused, with a way back to the form.
    let r = form("/ui/logout", peer(8, 9), "", "csrf=x")
        .header("origin", "http://evil.example")
        .header("sec-fetch-site", "cross-site")
        .body(Body::from("csrf=x"))
        .unwrap();
    let r = send(&c, r).await;
    r.assert_secure_html(403);
    assert!(r.cookies().is_empty());
    assert!(r.text().contains("href=\"/ui/logout\""));
    get(&c, "/ui/logout", peer(8, 8)).await.assert_see_other("/ui/login");
}

#[tokio::test]
async fn busy_dashboard_is_a_503_page() {
    let mut c = ctx(Some(PW));
    let session = login(&c, 9, 1).await;
    c.gate = Arc::new(Gate::new(0, 0, Duration::from_millis(20)));
    let r = get_with(&c, "/ui/", peer(9, 2), &format!("zte_ui_session={session}")).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("The router is busy. Try again in a few seconds."));
    assert!(r.text().contains("href=\"/ui/\""));
}

#[tokio::test]
async fn without_a_password_pages_explain_assets_load() {
    let c = ctx(None);
    for uri in ["/ui/", "/ui/login", "/ui/nope", "/ui"] {
        let r = get(&c, uri, peer(10, 1)).await;
        r.assert_secure_html(403);
        assert!(r.text().contains("ZTE_AGENT_PASSWORD"));
    }
    let r = send(&c, form("/ui/login", peer(10, 2), "", "x=1").body(Body::from("x=1")).unwrap()).await;
    r.assert_secure_html(403);
    // `/` still redirects; static files need no password.
    get(&c, "/", peer(10, 3)).await.assert_see_other("/ui/");
    for uri in [APP_CSS, ICON_180, "/ui/manifest.webmanifest"] {
        let r = get(&c, uri, peer(10, 4)).await;
        assert_eq!(r.status, 200, "{uri}");
        r.assert_security_headers();
        assert!(r.header("content-security-policy").is_none());
    }
}

#[tokio::test]
async fn entropy_failure_is_a_503() {
    let mut state = AppState::new();
    state.auth = Arc::new(AuthState::with_parts(FakeClock::at_secs(100), Box::new(FailRng), AuthConfig::default()));
    state.auth.set_password(PW);
    let c = Ctx::new(Arc::new(state), 2);
    let r = get(&c, "/ui/login", peer(11, 1)).await;
    assert_eq!((r.status, r.text().as_str()), (503, "Entropy unavailable"));
    r.assert_security_headers();
    // Static files need no nonce.
    assert_eq!(get(&c, APP_CSS, peer(11, 2)).await.status, 200);
}

fn inflate(gz: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(gz).read_to_end(&mut out).unwrap();
    out
}

#[tokio::test]
async fn assets_etags_and_encodings() {
    let c = ctx(Some(PW));
    let css = ASSETS.iter().find(|a| a.path == APP_CSS).unwrap();
    let send_asset = |ae: Option<&str>, inm: Option<&str>, method: &str| {
        let mut b = req(method, APP_CSS, peer(12, 1));
        if let Some(v) = ae {
            b = b.header("accept-encoding", v);
        }
        if let Some(v) = inm {
            b = b.header("if-none-match", v);
        }
        send(&c, b.body(Body::empty()).unwrap())
    };
    let gz = send_asset(Some("gzip, br"), None, "GET").await;
    assert_eq!(gz.status, 200);
    assert_eq!((gz.header("content-encoding"), gz.header("etag")), (Some("gzip"), Some(css.etag_gz)));
    assert_eq!(gz.header("vary"), Some("Accept-Encoding"));
    assert_eq!(gz.header("cache-control"), Some("public, max-age=31536000, immutable"));
    assert_eq!(gz.header("content-type"), Some("text/css; charset=utf-8"));
    gz.assert_security_headers();
    let plain = send_asset(Some("gzip;q=0, identity"), None, "GET").await;
    assert_eq!((plain.header("content-encoding"), plain.header("etag")), (None, Some(css.etag)));
    assert_eq!(plain.header("vary"), Some("Accept-Encoding"));
    assert_eq!(inflate(&gz.body), plain.body);
    assert!(String::from_utf8(plain.body.clone()).unwrap().contains(":root"));
    assert_ne!(css.etag, css.etag_gz);
    // 304 for a matching tag inside a list, and for a weak gzip tag.
    let etag_list = format!("\"x\", {}", css.etag_gz);
    let weak = format!("W/{}", css.etag_gz);
    for inm in [etag_list.as_str(), weak.as_str()] {
        let r = send_asset(Some("gzip"), Some(inm), "GET").await;
        assert_eq!((r.status, r.body.len()), (304, 0), "{inm}");
        assert_eq!(r.header("etag"), Some(css.etag_gz));
        assert_eq!(r.header("cache-control"), Some("public, max-age=31536000, immutable"));
        assert_eq!(r.header("vary"), Some("Accept-Encoding"));
    }
    // The gzip tag does not match the identity representation.
    assert_eq!(send_asset(None, Some(weak.as_str()), "GET").await.status, 200);
    // HEAD: headers only.
    let r = send_asset(Some("gzip"), None, "HEAD").await;
    assert_eq!((r.status, r.body.len(), r.header("etag")), (200, 0, Some(css.etag_gz)));
    // PNGs are stored as they are.
    let r = get(&c, ICON_180, peer(12, 2)).await;
    assert_eq!((r.header("content-type"), r.header("content-encoding"), r.header("vary")), (Some("image/png"), None, None));
    assert_eq!(&r.body[1..4], b"PNG");
}

#[tokio::test]
async fn manifest_route() {
    let c = ctx(Some(PW));
    let r = get(&c, "/ui/manifest.webmanifest", peer(13, 1)).await;
    assert_eq!((r.status, r.header("content-type")), (200, Some("application/manifest+json")));
    assert_eq!((r.header("cache-control"), r.header("etag")), (Some("no-cache"), Some(MANIFEST_ETAG)));
    let m: Value = serde_json::from_slice(&r.body).unwrap();
    for icon in m["icons"].as_array().unwrap() {
        let r = get(&c, icon["src"].as_str().unwrap(), peer(13, 2)).await;
        assert_eq!(r.status, 200);
    }
    let r = send(&c, req("GET", "/ui/manifest.webmanifest", peer(13, 3)).header("if-none-match", MANIFEST_ETAG).body(Body::empty()).unwrap()).await;
    assert_eq!((r.status, r.header("etag")), (304, Some(MANIFEST_ETAG)));
}

#[tokio::test]
async fn every_page_links_the_hashed_assets() {
    let c = ctx(Some(PW));
    let r = get(&c, "/ui/login", peer(14, 1)).await;
    let html = r.text();
    assert!(html.contains(&format!("<link rel=\"stylesheet\" href=\"{APP_CSS}\">")));
    assert!(html.contains("<link rel=\"manifest\" href=\"/ui/manifest.webmanifest\">"));
    assert!(html.contains(&format!("<link rel=\"apple-touch-icon\" href=\"{ICON_180}\">")));
    // Without live updates only the System theme's script (with the
    // response's nonce); no inline styles anywhere.
    assert_eq!(html.matches("<script").count(), 1, "{html}");
    assert!(html.contains(&format!("<script src=\"{THEME_JS}\" nonce=")) && !html.contains("datastar") && !html.contains("style="));
    // Each response has its own nonce.
    let other = get(&c, "/ui/login", peer(14, 2)).await;
    assert_ne!(r.header("content-security-policy"), other.header("content-security-policy"));
}

// --- Signal, settings, operations, live streams ---

/// A logged-in session on a fresh state: (ctx, cookie header, session id).
async fn signed_in(test: u8) -> (Ctx, String, crate::auth::SessionId) {
    let c = ctx(Some(PW));
    let session = login(&c, test, 1).await;
    let id = c.app.auth.authenticate(Kind::Browser, &session).unwrap().id;
    (c, format!("zte_ui_session={session}"), id)
}

/// Owner decision 61: Chrome for Android's installed web app (and links
/// from other apps) open the UI without the `SameSite=Strict` session
/// cookie. Login also sets a `Lax` marker; with it, a page without the
/// session hops once to itself (a same-site navigation) and finds it.
#[tokio::test]
async fn an_outside_launch_hops_once_and_lands_signed_in() {
    let c = ctx(Some(PW));
    let session = login(&c, 41, 1).await;
    let both = format!("zte_ui_session={session}; zte_ui_signed_in=1");
    let launch = |uri: &str, cookie: &str, site: &'static str| {
        req("GET", uri, peer(41, 2))
            .header("cookie", cookie)
            .header("sec-fetch-site", site)
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-dest", "document")
            .body(Body::empty())
            .unwrap()
    };
    // The screen route (`/ui/`), a `Session` extractor page and the login page.
    for (uri, hop) in [("/ui/", "/ui/?hop=1"), ("/ui/logout", "/ui/logout?hop=1"), ("/ui/login?next=%2Fui%2Fsignal", "/ui/login?next=%2Fui%2Fsignal&amp;hop=1")] {
        // From outside: only the marker came. A page that opens itself again.
        let r = send(&c, launch(uri, "zte_ui_signed_in=1", "none")).await;
        r.assert_secure_html(200);
        assert!(r.text().contains(&format!("<meta http-equiv=\"refresh\" content=\"0; url={hop}\">")), "{}", r.text());
        assert!(r.cookies().is_empty());
    }
    // The hop, same-site with both cookies: the page, without the flag.
    send(&c, launch("/ui/?hop=1", &both, "same-origin")).await.assert_see_other("/ui/");
    send(&c, launch("/ui/logout?hop=1", &both, "same-origin")).await.assert_see_other("/ui/logout");
    send(&c, launch("/ui/login?next=%2Fui%2Fsignal&hop=1", &both, "same-origin")).await.assert_see_other("/ui/signal");
    let r = send(&c, launch("/ui/", &both, "same-origin")).await;
    r.assert_secure_html(200);
    assert!(r.text().contains("Dashboard"));
    // The session is gone (a hop without it, or a stale cookie): login once,
    // and the marker is cleared, so the next launch does not hop.
    let r = send(&c, launch("/ui/?hop=1", "zte_ui_signed_in=1", "same-origin")).await;
    r.assert_see_other("/ui/login?next=%2Fui%2F");
    assert_eq!(r.cookies(), [CLEAR_MARKER]);
    let stale = format!("zte_ui_session={}; zte_ui_signed_in=1", "ab".repeat(32));
    let r = send(&c, launch("/ui/", &stale, "none")).await;
    r.assert_see_other("/ui/login?next=%2Fui%2F");
    assert_eq!(r.cookies(), [CLEAR_MARKER]);
    // Live streams and assets never hop.
    let r = send(&c, launch("/ui/live/dashboard/0123456789abcdef", "zte_ui_signed_in=1", "none")).await;
    assert_ne!(r.status, 200);
    assert!(!r.text().contains("http-equiv=\"refresh\""));
}

/// Mobile Network (mobile data, airplane mode, roaming) and Device Controls
/// (charge limit), under Router.
const MN: &str = "/ui/router/mobile-network";
const DEV: &str = "/ui/router/device";

#[tokio::test]
async fn signal_page_has_every_fragment_and_the_nav_works() {
    let (c, cookie, _) = signed_in(16).await;
    let r = get_with(&c, "/ui/signal", peer(16, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for f in super::pages::signal::FRAGMENTS.iter() {
        assert!(html.contains(&format!("id=\"{}\"", f.id)), "{}", f.id);
    }
    // The stream URL carries this page load's key.
    assert!(html.contains("id=\"live-status\"") && html.contains("@get('/ui/live/signal/"));
    // A sheet of the dashboard: its tab, "Done" back to it.
    assert!(html.contains("<a href=\"/ui/\" aria-current=\"page\">"));
    assert!(html.contains("<h1>Connection &amp; signal</h1><div class=\"trail\"><a class=\"nl\" href=\"/ui/\">Done</a>"));
    // The dashboard's "Radio details" and every tab exist.
    let dash = get_with(&c, "/ui/", peer(16, 3), &cookie).await.text();
    assert!(dash.contains("<a class=\"card\" id=\"card-conn\" href=\"/ui/signal\">") && dash.contains("Radio details"));
    for uri in ["/ui/", "/ui/sms", "/ui/tools", "/ui/router", "/ui/settings", "/ui/signal", MN, DEV] {
        assert_eq!(get_with(&c, uri, peer(16, 4), &cookie).await.status, 200, "{uri}");
    }
    get(&c, "/ui/signal", peer(16, 5)).await.assert_see_other("/ui/login?next=%2Fui%2Fsignal");
}

/// The dashboard's cards open their sheets; each sheet is live, has "Done"
/// back to the dashboard (CPU & Memory: "‹ CPU & temperatures"), and its
/// stream subscribes to its own topics only while it is open.
#[tokio::test]
async fn detail_sheets_open_from_the_dashboard() {
    use crate::events::Topic;
    let (c, cookie, _) = signed_in(30).await;
    let dash = get_with(&c, "/ui/", peer(30, 2), &cookie).await.text();
    for (id, href) in [("card-traffic", "/ui/traffic"), ("card-battery", "/ui/battery"), ("card-health", "/ui/health")] {
        assert!(dash.contains(&format!("<a class=\"card\" id=\"{id}\" href=\"{href}\">")), "{id}");
    }
    let done = "<div class=\"trail\"><a class=\"nl\" href=\"/ui/\">Done</a>";
    for (uri, id, title, nav) in [
        ("/ui/traffic", "traffic", "Live traffic", done),
        ("/ui/battery", "battery", "Battery Details", done),
        ("/ui/health", "health", "CPU &amp; temperatures", done),
        ("/ui/health/cpu", "cpu", "CPU &amp; Memory", "<a class=\"nl back\" href=\"/ui/health\">CPU &amp; temperatures</a>"),
    ] {
        let r = get_with(&c, uri, peer(30, 3), &cookie).await;
        r.assert_secure_html(200);
        let html = r.text();
        assert!(html.contains(&format!("<h1>{title}</h1>")) && html.contains(nav), "{uri}: {html}");
        assert!(html.contains(&format!("@get('/ui/live/{id}/")) && html.contains("id=\"live-status\""), "{uri}");
        assert!(html.contains("<a href=\"/ui/\" aria-current=\"page\">"), "{uri}");
    }
    // The health sheet streams health and battery, nothing else, and stops with the page.
    let resp = open_live(&c, &format!("/ui/live/health/{KEY}"), peer(30, 4), &cookie).await;
    assert_eq!((c.app.hub.demand(Topic::Health), c.app.hub.demand(Topic::Battery)), (1, 1));
    assert_eq!((c.app.hub.demand(Topic::Signal), c.app.hub.demand(Topic::Traffic)), (0, 0));
    let mut b = resp.into_body().into_data_stream();
    let first = frame(&mut b).await.unwrap();
    for f in super::pages::sheets::HEALTH.iter() {
        assert!(first.contains(&format!("id=\"{}\"", f.id)), "{}", f.id);
    }
    assert!(!first.contains("<details") && !first.contains("<form"));
    drop(b);
    for _ in 0..50 {
        if c.app.hub.demand(Topic::Health) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(c.app.hub.demand(Topic::Health), 0);
}

/// Network Mode in its two mounts: the app's six modes only, always
/// confirmed, the current mode a no-op, then an operation that follows
/// `net_select` and leads back to the mount it came from.
#[tokio::test]
async fn network_mode_in_both_mounts() {
    use crate::events::hub::ReadOut;
    use crate::events::Topic;
    const SIG: &str = "/ui/signal/network-mode";
    const RT: &str = "/ui/router/network-mode";
    let (c, cookie, _) = signed_in(31).await;
    let signal = |sel: &str| {
        let v = serde_json::json!({"net_select": sel, "network_type": "ENDC"});
        c.app.hub.offer(Topic::Signal, ReadOut { value: Some(v), error: None }, std::time::Instant::now());
    };
    // The page shows the current mode and preselects it (here: the hub's
    // reading; no device on the host).
    signal("Only_LTE");
    let r = get_with(&c, SIG, peer(31, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<a class=\"nl back\" href=\"/ui/signal\">Connection &amp; signal</a>", "<h1>Network Mode</h1>", "<h2>Current</h2>",
        "<span>Network Mode</span><span class=\"v\">4G Only</span>", "<h2>Network Mode</h2>", "<label for=\"mode\">Mode</label>",
        "<option value=\"WL_AND_5G\">Auto</option><option value=\"LTE_AND_5G\">5G NSA (LTE + NR)</option>\
         <option value=\"Only_5G\">5G SA Only</option><option value=\"Only_LTE\" selected>4G Only</option>\
         <option value=\"WCDMA_AND_LTE\">4G + 3G</option><option value=\"Only_WCDMA\">3G Only</option>",
        "<input type=\"hidden\" name=\"cur\" value=\"Only_LTE\">", "<button class=\"btn btn-wide\" type=\"submit\">Apply</button>",
        "<a href=\"/ui/\" aria-current=\"page\">",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let form = html.split("<form").nth(1).unwrap();
    assert!(form.starts_with(&format!(" method=\"post\" action=\"{SIG}\"><input type=\"hidden\" name=\"csrf\"")), "{form}");
    let csrf = r.input("csrf");
    // The other mount: Router's tab and way back, its own form target.
    let html = get_with(&c, RT, peer(31, 2), &cookie).await.text();
    assert!(html.contains("<a class=\"nl back\" href=\"/ui/router\">Router</a>") && html.contains(&format!("action=\"{RT}\"")));
    assert!(html.contains("<a href=\"/ui/router\" aria-current=\"page\">"));
    // A mode the app does not know is shown as reported, and never sent back as current.
    signal("<b>NR_ONLY</b>");
    let html = get_with(&c, RT, peer(31, 2), &cookie).await.text();
    assert!(html.contains("<span class=\"v\">&lt;b&gt;NR_ONLY&lt;/b&gt;</span>") && html.contains("name=\"cur\" value=\"\""), "{html}");
    assert!(!html.contains(" selected"));
    let p = |uri: &'static str, body: String| {
        let (c, cookie) = (c.clone(), cookie.clone());
        async move { post(&c, uri, peer(31, 3), &cookie, &body).await }
    };
    // Anything but the six values: 400, nothing sent (also when confirmed).
    for bad in ["AUTO", "only_lte", "Only_LTE%22%7D", "%3Cx%3E", "", "Only_5G%20"] {
        for mount in [SIG, RT] {
            p(mount, format!("csrf={csrf}&mode={bad}&confirm=1")).await.assert_secure_html(400);
        }
    }
    p(SIG, format!("csrf={csrf}&confirm=1")).await.assert_secure_html(400);
    // The mode shown as current: nothing to do, back to the page.
    p(SIG, format!("csrf={csrf}&mode=Only_LTE&cur=Only_LTE")).await.assert_see_other(SIG);
    p(RT, format!("csrf={csrf}&mode=Only_LTE&cur=Only_LTE&confirm=1")).await.assert_see_other(RT);
    // Every change is confirmed first, in both mounts.
    for (value, label) in super::tabs::dashboard::NET_MODES {
        for mount in [SIG, RT] {
            let to = format!("{mount}/confirm?mode={value}");
            p(mount, format!("csrf={csrf}&mode={value}&cur=")).await.assert_see_other(&to);
            let html = get_with(&c, &to, peer(31, 4), &cookie).await.text();
            assert!(html.contains(&format!("<h2 class=\"q\">Change network mode to {label}?</h2>")), "{html}");
            assert!(html.contains(&format!("<a class=\"nl back\" href=\"{mount}\">Network Mode</a></div><h1>Confirm</h1>")), "{html}");
            assert!(html.contains("The modem re-registers on the network. Mobile data drops for a few seconds."));
            assert!(html.contains(&format!("name=\"mode\" value=\"{value}\"")) && html.contains("name=\"confirm\" value=\"1\""));
            assert!(html.contains(&format!("href=\"{mount}\">Cancel")) && html.contains(">Change mode</button>"));
            assert!(!html.contains("connected remotely"));
        }
    }
    // Over Tailscale: what a mode without coverage does to this connection.
    let tailnet = Peer(SocketAddr::from(([127, 0, 0, 1], 40002)));
    let html = get_with(&c, &format!("{RT}/confirm?mode=Only_5G"), tailnet, &cookie).await.text();
    assert!(html.contains(
        "You are connected remotely: if 5G SA Only has no coverage here, this page can’t be reached until the mode is changed on the router."
    ), "{html}");
    get_with(&c, &format!("{RT}/confirm?mode=Only_4G"), peer(31, 4), &cookie).await.assert_secure_html(400);
    // Confirmed: the PUT runs (no ubus on the host, so it fails for sure):
    // the page again with the error, no operation.
    let r = p(SIG, format!("csrf={csrf}&mode=Only_5G&confirm=1")).await;
    assert!(r.status >= 500 && r.header("location").is_none(), "{}", r.status);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Failed: ubus exec"), "{}", r.text());
    assert!(r.text().contains("<h1>Network Mode</h1>"));
}

/// The operation follows `net_select` on the `signal` topic: done when the
/// modem reports the new mode, else the app's "Mode sent" after 10 s; each
/// mount's operation leads back to its own page.
#[tokio::test]
async fn network_mode_operation_settles_or_times_out() {
    use super::ops::{Op, NET_MODE_ROUTER, NET_MODE_SIGNAL};
    use crate::events::hub::ReadOut;
    use crate::events::Topic;
    let clock = FakeClock::at_secs(1000);
    let c = ctx_at(clock.clone());
    let session = login(&c, 32, 1).await;
    let id = c.app.auth.authenticate(Kind::Browser, &session).unwrap().id;
    let cookie = format!("zte_ui_session={session}");
    let now = c.app.auth.now();
    c.ops.insert("00000000000000a1".into(), id, Op { spec: &NET_MODE_ROUTER, started: now, arg: "Only_LTE" }, now);
    c.ops.insert("00000000000000a2".into(), id, Op { spec: &NET_MODE_SIGNAL, started: now, arg: "Only_5G" }, now);
    let sel = serde_json::json!({"net_select": "Only_LTE"});
    c.app.hub.offer(Topic::Signal, ReadOut { value: Some(sel), error: None }, std::time::Instant::now());
    let html = get_with(&c, "/ui/op/00000000000000a1", peer(32, 2), &cookie).await.text();
    assert!(html.contains("<h1 id=\"op-title\">Network mode updated</h1>") && html.contains("The modem reports the new mode."), "{html}");
    assert!(html.contains("<a href=\"/ui/router\" aria-current=\"page\">"));
    assert!(html.contains("<a class=\"nl back\" href=\"/ui/router/network-mode\">Network Mode</a>") && html.contains("Back to Network Mode"));
    assert!(!html.contains("http-equiv"), "a finished operation does not reload");
    // The other one is still waiting: it reloads without JS and streams the signal topic.
    let html = get_with(&c, "/ui/op/00000000000000a2", peer(32, 3), &cookie).await.text();
    assert!(html.contains("<h1 id=\"op-title\">Changing network mode</h1>") && html.contains("Switching network mode\u{2026}"), "{html}");
    assert!(html.contains("http-equiv=\"refresh\"") && html.contains("@get('/ui/live/op/00000000000000a2/"));
    assert!(html.contains("<a href=\"/ui/\" aria-current=\"page\">") && html.contains("href=\"/ui/signal/network-mode\""));
    clock.advance(Duration::from_secs(10));
    let html = get_with(&c, "/ui/op/00000000000000a2", peer(32, 4), &cookie).await.text();
    assert!(html.contains("<h1 id=\"op-title\">Mode sent \u{2014} router may still be switching</h1>"), "{html}");
    assert!(html.contains("The modem has not reported the new mode yet.") && !html.contains("http-equiv"));
    // Not an error (the app's "Mode sent" is not red): the mode was sent.
    assert!(html.contains("<p class=\"msg info\" role=\"status\">The modem has not") && !html.contains("role=\"alert\""), "{html}");
}

/// Every screen of the route table: signed in only, its tab marked, the
/// tab bar after `main`, no "Log out" in the header, no inline style.
#[tokio::test]
async fn every_screen_marks_its_tab() {
    use super::kit::Tab;
    let (c, cookie, _) = signed_in(26).await;
    let tab_href = |t: Tab| match t {
        Tab::Dashboard => "/ui/",
        Tab::Sms => "/ui/sms",
        Tab::Tools => "/ui/tools",
        Tab::Router => "/ui/router",
        Tab::Settings => "/ui/settings",
    };
    for s in super::routes::SCREENS {
        let r = get_with(&c, s.path, peer(26, 2), &cookie).await;
        // A question about something that is gone (here: no id) is a 404.
        r.assert_secure_html(if s.status.is_some() { 404 } else { 200 });
        let html = r.text();
        assert_eq!(html.matches("aria-current=\"page\"").count(), 1, "{}", s.path);
        assert!(html.contains(&format!("<a href=\"{}\" aria-current=\"page\">", tab_href(s.tab))), "{}", s.path);
        // A heading from what the page read starts with the title (DNS Cache (N)).
        let h1 = if s.heading.is_some() { format!("<h1>{} (", s.title) } else { format!("<h1>{}</h1>", s.title.replace('&', "&amp;")) };
        assert!(html.contains(&h1), "{}", s.path);
        let header = &html[html.find("<header").unwrap()..html.find("</header>").unwrap()];
        assert!(!header.contains("Log out") && !header.contains("logout"), "{}", s.path);
        assert!(html.contains("</main><nav class=\"tabs\" aria-label=\"Tabs\">") && !html.contains("style="), "{}", s.path);
        get(&c, s.path, peer(26, 3)).await.assert_see_other(&format!("/ui/login?next={}", enc(s.path)));
    }
    // The other signed-in pages: logout under Settings, an operation under Router.
    let r = get_with(&c, "/ui/logout", peer(26, 4), &cookie).await;
    assert!(r.text().contains("<a href=\"/ui/settings\" aria-current=\"page\">"));
    assert!(r.text().contains("href=\"/ui/settings\">Cancel"));
    // Live ids are [a-z0-9]+ and unique; paths are unique.
    let mut seen = Vec::new();
    for s in super::routes::SCREENS {
        assert!(!s.id.is_empty() && s.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()), "{}", s.id);
        assert!(!seen.contains(&s.id) && !seen.contains(&s.path), "{}", s.path);
        seen.extend([s.id, s.path]);
    }
}

#[tokio::test]
async fn tab_roots_follow_the_app() {
    let (c, cookie, _) = signed_in(27).await;
    let html = get_with(&c, "/ui/settings", peer(27, 2), &cookie).await.text();
    for want in [
        "<h2>Session</h2>", "Signed in in this browser", "href=\"/ui/logout\"><span>Log out</span>", "<h2>About</h2>",
        "OpenU60", "ZTE U60 Pro (MU5250)", "zte-agent web UI 0.1.0", "<h2>Legal</h2>", "not affiliated with",
        "href=\"https://open-u60-pro.vercel.app/privacy\" rel=\"noopener noreferrer\"",
    ] {
        assert!(html.contains(want), "{want}");
    }
    let html = get_with(&c, "/ui/router", peer(27, 3), &cookie).await.text();
    for (section, rows) in [
        ("Cellular", 8), ("Connectivity", 5), ("Security", 3), ("Quality", 1), ("System", 1),
    ] {
        let at = html.find(&format!("<h2>{section}</h2>")).unwrap_or_else(|| panic!("{section}"));
        let list = &html[at..at + html[at..].find("</ul>").unwrap()];
        assert_eq!(list.matches("<li").count(), rows, "{section}");
    }
    assert!(html.contains(&format!("<a class=\"it\" href=\"{MN}\">")) && html.contains(&format!("<a class=\"it\" href=\"{DEV}\">")));
    for href in [
        "/ui/router/network-mode", "/ui/router/cell-lock", "/ui/router/stc", "/ui/router/signal-detect", "/ui/router/sim",
        "/ui/router/sim-services", "/ui/router/esim", "/ui/router/wifi", "/ui/router/guest-wifi", "/ui/router/apn", "/ui/router/lan",
        "/ui/router/dns", "/ui/router/firewall", "/ui/router/telemetry", "/ui/router/vpn", "/ui/router/qos",
    ] {
        assert!(html.contains(&format!("<a class=\"it\" href=\"{href}\">")), "{href}");
    }
    assert_eq!(html.matches("Coming to the web UI").count(), 0);
    let html = get_with(&c, "/ui/tools", peer(27, 4), &cookie).await.text();
    for want in ["Automation", "Network Tools", "Config", "Shell Access Required", "Speed Test", "AT Terminal", "Device Explorer"] {
        assert!(html.contains(want), "{want}");
    }
    assert_eq!(html.matches("Coming to the web UI").count(), 0);
    for href in [
        "/ui/tools/automations", "/ui/tools/sms-forward", "/ui/tools/speedtest", "/ui/tools/lan-speedtest", "/ui/tools/adb",
        "/ui/tools/usb", "/ui/tools/band-lock", "/ui/tools/at", "/ui/tools/device-info", "/ui/tools/devices", "/ui/tools/config",
        "/ui/tools/ttl", "/ui/tools/ssh", "/ui/tools/explorer",
    ] {
        assert!(html.contains(&format!("<a class=\"it\" href=\"{href}\">")), "{href}");
    }
    // Messages: the gear (SMS Forwarding) leads, New Message trails.
    let html = get_with(&c, "/ui/sms", peer(27, 5), &cookie).await.text();
    assert!(html.contains(
        "<div class=\"lead\"><a class=\"nl ic\" href=\"/ui/sms/forward\" aria-label=\"SMS Forwarding\"><svg"
    ));
    assert!(html.contains("<h1>Messages</h1><div class=\"trail\"><a class=\"nl ic\" href=\"/ui/sms/new\" aria-label=\"New Message\">"));
    let html = get_with(&c, "/ui/sms/forward", peer(27, 5), &cookie).await.text();
    assert!(html.contains("<h2>Settings</h2>") && html.contains("href=\"/ui/sms\">Messages</a>"));
    // Static screens (and New Message, New Rule) read nothing: they render even with every permit taken.
    let mut busy = c.clone();
    busy.gate = Arc::new(Gate::new(0, 0, Duration::from_millis(20)));
    for uri in ["/ui/tools", "/ui/router", "/ui/settings", "/ui/sms/new", "/ui/tools/sms-forward/rules/new"] {
        assert_eq!(get_with(&busy, uri, peer(27, 6), &cookie).await.status, 200, "{uri}");
    }
    for uri in [MN, "/ui/sms", "/ui/sms/forward", "/ui/sms/forward/rules/1"] {
        assert_eq!(get_with(&busy, uri, peer(27, 6), &cookie).await.status, 503, "{uri}");
    }
}

/// Every action of the route table: refused without a session, without
/// the CSRF token or from another origin (nothing called); a confirmed
/// value never runs without `confirm=1`; every saved notice is the table's.
#[tokio::test]
async fn every_action_checks_session_csrf_origin_and_confirmation() {
    use super::action::{Kind, Need};
    let (c, cookie, _) = signed_in(29).await;
    // The session's token (the same in every form; some screens have none).
    let csrf = get_with(&c, "/ui/logout", peer(29, 2), &cookie).await.input("csrf");
    for a in super::routes::ACTIONS {
        let login = format!("/ui/login?next={}&expired=1", enc(a.screen.path));
        post(&c, a.path, peer(29, 3), "", &format!("csrf={csrf}")).await.assert_see_other(&login);
        for body in [String::new(), "csrf=00".to_owned()] {
            post(&c, a.path, peer(29, 3), &cookie, &body).await.assert_secure_html(403);
        }
        let r = form(a.path, peer(29, 3), &cookie, &format!("csrf={csrf}"))
            .header("origin", "http://evil.example")
            .body(Body::from(format!("csrf={csrf}")))
            .unwrap();
        send(&c, r).await.assert_secure_html(403);
        // A value outside a choice list is a bad request.
        if let Some(f) = a.fields.iter().find(|f| matches!(f.kind, Kind::Choice(_))) {
            post(&c, a.path, peer(29, 4), &cookie, &format!("csrf={csrf}&{}=%3Cx%3E", f.name)).await.assert_secure_html(400);
        }
        // A bad id is a bad request; any id is confirmed on its own page first.
        if let Some(f) = a.fields.iter().find(|f| matches!(f.kind, Kind::Id(_))) {
            for bad in ["0", "-1", "%2B1", "1%2C2", "x", "", "99999999999"] {
                let r = post(&c, a.path, peer(29, 4), &cookie, &format!("csrf={csrf}&{}={bad}&confirm=1", f.name)).await;
                r.assert_secure_html(400);
            }
        }
        if let Some(at) = a.confirm_at {
            let fields: Vec<(&'static str, &str)> =
                a.fields.iter().map(|f| (f.name, if let Kind::Choice(set) = f.kind { set[0] } else { "4194304" })).collect();
            let body: String = fields.iter().map(|(k, v)| format!("&{k}={v}")).collect();
            let to = at(&super::action::Valid::of(&fields));
            post(&c, a.path, peer(29, 4), &cookie, &format!("csrf={csrf}{body}")).await.assert_see_other(&to);
            // (A question about something that is gone may say so with 404.)
            let page = get_with(&c, &to, peer(29, 4), &cookie).await;
            page.assert_secure_html(if page.status == 404 { 404 } else { 200 });
            let back = format!("<div class=\"lead\"><a class=\"nl back\" href=\"{}\">{}</a></div><h1>Confirm</h1>", a.screen.path, a.screen.title);
            assert!(page.text().contains(&back), "{}", page.text());
        }
        for case in a.confirm.iter().filter(|c| matches!(c.need, Need::Always)) {
            let (k, v) = case.when;
            let r = post(&c, a.path, peer(29, 4), &cookie, &format!("csrf={csrf}&{k}={v}")).await;
            r.assert_see_other(&format!("{}/confirm?{k}={v}", a.path));
            let page = get_with(&c, &format!("{}/confirm?{k}={v}", a.path), peer(29, 4), &cookie).await;
            page.assert_secure_html(200);
            // The question in the page (it wraps); the nav bar leads back to the form.
            assert!(page.text().contains(&format!("<div class=\"narrow\"><h2 class=\"q\">{}</h2>", case.title)), "{}", case.title);
            let back = format!("<div class=\"lead\"><a class=\"nl back\" href=\"{}\">{}</a></div><h1>Confirm</h1>", a.screen.path, a.screen.title);
            assert!(page.text().contains(&back), "{}", page.text());
        }
    }
    for s in super::routes::SCREENS {
        // The app's own result lines are green.
        let class = if s.saved_ok { "msg ok" } else { "msg info" };
        for (key, msg) in s.saved {
            let html = get_with(&c, &format!("{}?saved={key}", s.path), peer(29, 5), &cookie).await.text();
            assert!(html.contains(&format!("<p class=\"{class}\" role=\"status\">{msg}</p>")), "{}?saved={key}", s.path);
        }
    }
    // (Questions only for a remote browser are `pages::confirm`'s: Mobile
    // Network's roaming off, `tests::router`.)
}

#[tokio::test]
async fn mobile_network_page_and_confirmations() {
    let (c, cookie, _) = signed_in(17).await;
    let r = get_with(&c, MN, peer(17, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in ["Airplane Mode", "Mobile Data", "Data Roaming", "<a class=\"nl back\" href=\"/ui/router\">Router</a>"] {
        assert!(html.contains(want), "{want}");
    }
    // No device here: the read's error, and the Apply form is disabled.
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed to load: ") && html.contains("<fieldset class=\"mnf\" disabled>"), "{html}");
    // Every POST form starts with the CSRF field; no page script; no form
    // inside a paragraph (the parser would close it there).
    for form in html.split("<form").skip(1) {
        assert!(form.split_once('>').unwrap().1.starts_with("<input type=\"hidden\" name=\"csrf\""), "{form}");
    }
    assert!(!html.contains("/ui/a/datastar-") && !html.contains("style=") && !html.contains("<p class=\"actions\"><form"));
    // The state is unknown here (no device): both directions, saying what they turn on or off.
    for label in ["Turn on Mobile Data", "Turn off Mobile Data", "Turn on Airplane Mode", "Turn off Airplane Mode"] {
        assert!(html.contains(&format!("aria-label=\"{label}\"")), "{label}");
    }
    // Unknown flash keys show nothing from the URL.
    let r = get_with(&c, &format!("{MN}?saved=%3Cb%3Eowned"), peer(17, 2), &cookie).await;
    assert!(!r.text().contains("owned") && !r.text().contains("role=\"status\""));
    // Confirmation pages: what happens, the same fields plus confirm=1.
    let r = get_with(&c, &format!("{MN}/mobile-data/confirm?enable=0"), peer(17, 3), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert!(html.contains("<h2 class=\"q\">Turn off mobile data?</h2>"));
    assert!(html.contains("All connected devices lose internet access. Wi-Fi and this page keep working."));
    assert!(html.contains(&format!("action=\"{MN}/mobile-data\"")));
    assert!(html.contains("name=\"enable\" value=\"0\"") && html.contains("name=\"confirm\" value=\"1\""));
    assert!(html.contains(&format!("href=\"{MN}\">Cancel")) && !html.contains("connected remotely"));
    assert!(html.contains("<a href=\"/ui/router\" aria-current=\"page\">"));
    let r = get_with(&c, &format!("{MN}/airplane/confirm?mode=LPM"), peer(17, 3), &cookie).await;
    assert!(r.text().contains("The cellular radio turns off; calls, SMS and internet stop."));
    // Roaming is Apply's (as the app's): turning it on is asked first.
    let apply = |roam: &str, roam0: &str| format!("{MN}/apply?cm=1&roam={roam}&sel=auto_select&cm0=1&roam0={roam0}&sel0=auto_select");
    let r = get_with(&c, &apply("1", "0"), peer(17, 3), &cookie).await;
    r.assert_secure_html(200);
    assert!(r.text().contains("<h2 class=\"q\">Turn on data roaming?</h2>"));
    assert!(r.text().contains("Your operator may charge for data used abroad."));
    assert!(r.text().contains("This also turns mobile data on if it is off."));
    assert!(r.text().contains("name=\"roam\" value=\"1\"") && r.text().contains("name=\"roam0\" value=\"0\""));
    // Roaming off is not asked on the LAN.
    get_with(&c, &apply("0", "1"), peer(17, 3), &cookie).await.assert_see_other(MN);
    // Over Tailscale (loopback peer): the warning, and nothing that says
    // this page keeps working.
    let tailnet = Peer(SocketAddr::from(([127, 0, 0, 1], 40000)));
    let r = get_with(&c, &format!("{MN}/airplane/confirm?mode=LPM"), tailnet, &cookie).await;
    assert!(r.text().contains("You are connected remotely: this cuts the connection, and this page can’t be reached until it is turned back on at the router."));
    let r = get_with(&c, &format!("{MN}/mobile-data/confirm?enable=0"), tailnet, &cookie).await;
    assert!(r.text().contains("All connected devices lose internet access. Wi-Fi keeps working."));
    assert!(r.text().contains("this cuts the connection") && !r.text().contains("this page keep working"));
    // Remotely, roaming off is asked too (abroad it cuts the connection).
    let r = get_with(&c, &apply("0", "1"), tailnet, &cookie).await;
    assert!(r.text().contains("You are connected remotely: if the router is abroad, this page can’t be reached until roaming is turned back on at the router.") && r.text().contains("btn-danger"));
    for q in ["mobile-data/confirm?enable=2", "mobile-data/confirm", "airplane/confirm?mode=lpm", "apply?cm=2&roam=1&sel=auto_select&cm0=1&roam0=0&sel0=auto_select", "apply?cm=1"] {
        get_with(&c, &format!("{MN}/{q}"), peer(17, 4), &cookie).await.assert_secure_html(400);
    }
    // The old settings paths are gone.
    for uri in ["/ui/settings/airplane/confirm?mode=LPM", "/ui/settings/charge-limit"] {
        get_with(&c, uri, peer(17, 4), &cookie).await.assert_secure_html(404);
    }
    get(&c, &format!("{MN}/airplane/confirm?mode=LPM"), peer(17, 5)).await.assert_see_other(
        "/ui/login?next=%2Fui%2Frouter%2Fmobile-network%2Fairplane%2Fconfirm%3Fmode%3DLPM",
    );
}

#[tokio::test]
async fn router_posts_check_session_csrf_confirm_and_values() {
    let (c, cookie, _) = signed_in(18).await;
    let csrf = get_with(&c, MN, peer(18, 2), &cookie).await.input("csrf");
    // Without a session: back to the login, nothing done.
    for (action, page) in [("apply", MN), ("mobile-data", MN), ("airplane", MN)] {
        post(&c, &format!("{MN}/{action}"), peer(18, 3), "", "csrf=x&enable=1").await.assert_see_other(&format!(
            "/ui/login?next={}&expired=1",
            enc(page)
        ));
    }
    post(&c, &format!("{DEV}/charge-limit"), peer(18, 3), "", "csrf=x").await.assert_see_other("/ui/login?next=%2Fui%2Frouter%2Fdevice&expired=1");
    // No CSRF field, a wrong one, cross-origin, the router's own port 80.
    for body in ["enable=1", "csrf=00&enable=1"] {
        let r = post(&c, &format!("{MN}/mobile-data"), peer(18, 4), &cookie, body).await;
        r.assert_secure_html(403);
        assert!(r.text().contains(&format!("href=\"{MN}\"")));
    }
    let body = format!("csrf={csrf}&enable=0");
    for (origin, site) in [("http://evil.example", "cross-site"), ("http://192.168.0.1", "same-site")] {
        let r = form(&format!("{MN}/mobile-data"), peer(18, 4), &cookie, &body)
            .header("origin", origin)
            .header("sec-fetch-site", site)
            .body(Body::from(body.clone()))
            .unwrap();
        send(&c, r).await.assert_secure_html(403);
    }
    // Confirmation first.
    let p = |uri: String, body: String| {
        let (c, cookie) = (c.clone(), cookie.clone());
        async move { post(&c, &uri, peer(18, 5), &cookie, &body).await }
    };
    let at = |a: &str| format!("{MN}/{a}");
    let fields = |roam: &str, roam0: &str| format!("cm=1&roam={roam}&sel=auto_select&cm0=1&roam0={roam0}&sel0=auto_select");
    p(at("apply"), format!("csrf={csrf}&{}", fields("1", "0"))).await.assert_see_other(&at(&format!("apply?{}", fields("1", "0"))));
    p(at("mobile-data"), format!("csrf={csrf}&enable=0")).await.assert_see_other(&at("mobile-data/confirm?enable=0"));
    p(at("airplane"), format!("csrf={csrf}&mode=LPM")).await.assert_see_other(&at("airplane/confirm?mode=LPM"));
    let tailnet = Peer(SocketAddr::from(([127, 0, 0, 1], 40001)));
    post(&c, &at("apply"), tailnet, &cookie, &format!("csrf={csrf}&{}", fields("0", "1"))).await.assert_see_other(&at(&format!("apply?{}", fields("0", "1"))));
    // Nothing changed: nothing to do.
    p(at("apply"), format!("csrf={csrf}&{}", fields("1", "1"))).await.assert_see_other(MN);
    // Bad values.
    p(at("apply"), format!("csrf={csrf}&cm=7&roam=1&sel=auto_select&cm0=1&roam0=0&sel0=auto_select")).await.assert_secure_html(400);
    p(at("airplane"), format!("csrf={csrf}&mode=OFF&confirm=1")).await.assert_secure_html(400);
    // Charge limit out of range: 400, the form as typed, one message per field.
    let csrf = get_with(&c, DEV, peer(18, 2), &cookie).await.input("csrf");
    let charge = format!("{DEV}/charge-limit");
    for (limit, hyst) in [("49", "5"), ("101", "5"), ("306", "5"), ("80", "0"), ("80", "21")] {
        let r = p(charge.clone(), format!("csrf={csrf}&enabled=1&limit={limit}&hysteresis={hyst}")).await;
        r.assert_secure_html(400);
        let html = r.text();
        // The app's values as lists; the value typed is shown, chosen.
        assert!(html.contains(&format!("<option value=\"{limit}\" selected>{limit}%</option>")), "{html}");
        assert!(html.contains(&format!("<option value=\"{hyst}\" selected>{hyst}%</option>")));
        assert!(html.contains("name=\"enabled\" value=\"1\" checked"));
        let bad_limit = limit != "80";
        assert_eq!(html.contains("Enter a whole number from 50 to 100."), bad_limit);
        assert_eq!(html.contains("Enter a whole number from 1 to 20."), !bad_limit);
        assert!(html.contains("aria-invalid=\"true\""));
    }
    for bad in ["5e1", "80.5", "-1", ""] {
        let r = p(charge.clone(), format!("csrf={csrf}&limit={bad}&hysteresis=5")).await;
        assert!(r.text().contains("Enter a whole number from 50 to 100."), "{bad}");
        assert!(!r.text().contains("name=\"enabled\" value=\"1\" checked"));
    }
    // What was typed and is no number is not shown.
    let r = p(charge.clone(), format!("csrf={csrf}&limit=%22%3E%3Cscript%3E&hysteresis=5")).await;
    r.assert_secure_html(400);
    let html = r.text();
    assert!(html.contains("Enter a whole number from 50 to 100.") && !html.contains("\"><script>") && !html.contains("&lt;script&gt;"), "{html}");
    // A valid charge limit (kept in memory here: no device files on the
    // host): saved, and the page shows the new values.
    // Without the page's single-use token nothing is saved.
    let r = p(charge.clone(), format!("csrf={csrf}&enabled=1&limit=85&hysteresis=7")).await;
    let html = get_with(&c, r.header("location").unwrap(), peer(18, 6), &cookie).await.text();
    assert!(html.contains("This page expired, so nothing was sent.") && !html.contains("<option value=\"85\" selected>"), "{html}");
    let once = get_with(&c, DEV, peer(18, 6), &cookie).await.input("once");
    let save = format!("csrf={csrf}&once={once}&enabled=1&limit=85&hysteresis=7");
    let (a, b) = tokio::join!(p(charge.clone(), save.clone()), p(charge.clone(), save));
    // Back to the page, which reads the new values (the app shows no line).
    a.assert_see_other(DEV);
    b.assert_see_other(DEV);
    let html = get_with(&c, DEV, peer(18, 6), &cookie).await.text();
    assert!(!html.contains("Charge limit saved.") && html.contains("<option value=\"85\" selected>85%</option>"));
    assert!(html.contains("<option value=\"7\" selected>7%</option>") && html.contains("name=\"enabled\" value=\"1\" checked"));
    assert!(html.contains("Charging stops at 85% and resumes at 78%."));
    // A confirmed change whose state cannot be read (no device here): the
    // page with the error; no operation is started, and the question's
    // single-use token is given back (nothing was sent).
    let token = get_with(&c, &at("mobile-data/confirm?enable=0"), peer(18, 5), &cookie).await.input("once");
    let r = p(at("mobile-data"), format!("csrf={csrf}&enable=0&confirm=1&once={token}")).await;
    assert!(r.status >= 500, "{}", r.status);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Failed: ") && r.header("location").is_none(), "{}", r.text());
    // With the state read: two posts at once send one write and land on one
    // operation (kept under the token); a replay finds it.
    let wwan = serde_json::json!({"enable": 1, "roam_enable": 0, "connect_mode": 1, "connect_status": "ipv4_connected"});
    let _dev = crate::web::pages::device::set(&c.app, crate::web::pages::device::Device { gets: vec![("/api/modem/data", Ok(wwan))], ..Default::default() });
    let body = format!("csrf={csrf}&enable=0&confirm=1&once={token}");
    let (a, b) = tokio::join!(p(at("mobile-data"), body.clone()), p(at("mobile-data"), body.clone()));
    a.assert_see_other(&format!("/ui/op/{token}"));
    b.assert_see_other(&format!("/ui/op/{token}"));
    p(at("mobile-data"), body).await.assert_see_other(&format!("/ui/op/{token}"));
    let sent = crate::web::pages::device::calls(&c.app);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, "/api/modem/data");
    // A question's token is its own: another page's is refused.
    let r = p(at("mobile-data"), format!("csrf={csrf}&enable=1&once=0123456789abcdef")).await;
    let html = get_with(&c, r.header("location").unwrap(), peer(18, 5), &cookie).await.text();
    assert!(html.contains("This page expired, so nothing was sent.") && crate::web::pages::device::calls(&c.app).len() == 1, "{html}");
}

#[tokio::test]
async fn operations_start_unless_the_write_surely_failed() {
    use super::action::unsure;
    assert!(unsure(500, "AT+CFUN=1 failed"));
    assert!(unsure(503, "ubus call zwrt_data set_wwaniface failed: Command failed: Request timed out"));
    assert!(!unsure(503, "Mobile data state unavailable") && !unsure(503, "no serial port found"));
    // Turning airplane mode off on the host: no AT port, which surely did
    // not change anything: the page with the error, no operation.
    let (c, cookie, _) = signed_in(28).await;
    let page = get_with(&c, MN, peer(28, 2), &cookie).await;
    let (csrf, token) = (page.input("csrf"), page.input("once"));
    let r = post(&c, &format!("{MN}/airplane"), peer(28, 3), &cookie, &format!("csrf={csrf}&mode=ONLINE&once={token}")).await;
    assert!(r.header("location").is_some_and(|l| l.starts_with("/ui/router/mobile-network/reboot?r=")), "{:?}", r.header("location"));
}

#[tokio::test]
async fn operation_page_is_its_owners_and_refreshes_without_js() {
    use super::ops::{Op, AIRPLANE_ON, DATA_ON};
    let (c, cookie, id) = signed_in(19).await;
    let now = c.app.auth.now();
    c.ops.insert("00000000000000aa".into(), id, Op { spec: &DATA_ON, started: now, arg: "" }, now);
    let other = crate::auth::SessionId::for_test(999);
    c.ops.insert("00000000000000bb".into(), other, Op { spec: &AIRPLANE_ON, started: now, arg: "" }, now);
    let r = get_with(&c, "/ui/op/00000000000000aa", peer(19, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert!(html.contains("<h1 id=\"op-title\">Turning mobile data on</h1>") && html.contains("id=\"op-progress\""));
    assert!(html.contains("<title>Turning mobile data on \u{b7} U60 Pro</title>"));
    assert!(html.contains("Turning on mobile data\u{2026}") && html.contains("Back to Mobile Network"));
    // Under Router, with the way back in the nav bar.
    assert!(html.contains("<a href=\"/ui/router\" aria-current=\"page\">"));
    assert!(html.contains(&format!("<a class=\"nl back\" href=\"{MN}\">Mobile Network</a>")));
    // Pending: refresh without JS, live with it.
    assert!(html.contains("<noscript><meta http-equiv=\"refresh\" content=\"3\"></noscript>"));
    assert!(html.contains("@get('/ui/live/op/00000000000000aa/"));
    for uri in ["/ui/op/00000000000000bb", "/ui/op/0000000000000000", "/ui/op/AA", "/ui/op/"] {
        get_with(&c, uri, peer(19, 3), &cookie).await.assert_secure_html(404);
    }
}

#[tokio::test]
async fn a_timed_out_operation_stops_refreshing() {
    use super::ops::{Op, AIRPLANE_ON};
    let clock = FakeClock::at_secs(1000);
    let c = ctx_at(clock.clone());
    let session = login(&c, 24, 1).await;
    let id = c.app.auth.authenticate(Kind::Browser, &session).unwrap().id;
    let now = c.app.auth.now();
    c.ops.insert("00000000000000cc".into(), id, Op { spec: &AIRPLANE_ON, started: now, arg: "" }, now);
    let cookie = format!("zte_ui_session={session}");
    let r = get_with(&c, "/ui/op/00000000000000cc", peer(24, 2), &cookie).await;
    assert!(r.text().contains("http-equiv=\"refresh\"") && r.text().contains("0 s so far"));
    // No modem state here: nothing is read, so it times out, saying so.
    clock.advance(Duration::from_secs(20));
    let r = get_with(&c, "/ui/op/00000000000000cc", peer(24, 3), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert!(html.contains("Couldn't read the modem state.") && html.contains("role=\"alert\""), "{html}");
    assert!(html.contains("<h1 id=\"op-title\">Change not confirmed</h1>"), "{html}");
    assert!(!html.contains("http-equiv") && !html.contains("/ui/a/datastar-") && !html.contains("data-init"));
    // Kept 10 minutes.
    clock.advance(Duration::from_secs(600));
    get_with(&c, "/ui/op/00000000000000cc", peer(24, 4), &cookie).await.assert_secure_html(404);
}

/// The next SSE frame of a streaming body, or None (ended or 5 s silence).
async fn frame(b: &mut axum::body::BodyDataStream) -> Option<String> {
    use futures_util::StreamExt;
    match tokio::time::timeout(Duration::from_secs(5), b.next()).await {
        Ok(Some(Ok(bytes))) => Some(String::from_utf8(bytes.to_vec()).unwrap()),
        _ => None,
    }
}

async fn open_live(c: &Ctx, uri: &str, peer: Peer, cookie: &str) -> axum::response::Response {
    let mut r = req("GET", uri, peer).header("datastar-request", "true");
    if !cookie.is_empty() {
        r = r.header("cookie", cookie);
    }
    app(c.clone()).oneshot(r.body(Body::empty()).unwrap()).await.unwrap()
}

/// A page key (`live::url`).
const KEY: &str = "0123456789abcdef";

#[tokio::test]
async fn live_endpoints_need_datastar_and_a_session() {
    let (c, cookie, _) = signed_in(20).await;
    let r = get_with(&c, &format!("/ui/live/dashboard/{KEY}"), peer(20, 2), &cookie).await;
    assert_eq!((r.status, r.header("content-type")), (400, Some("text/plain; charset=utf-8")));
    for (uri, status, link) in [
        (
            "/ui/live/dashboard",
            "<p class=\"live ps\" id=\"live-status\"><span class=\"off\"><b>Offline</b></span></p>\ndata: elements <p id=\"live-note\">",
            "/ui/login?next=%2Fui%2F",
        ),
        ("/ui/live/signal", "<p class=\"live\" id=\"live-status\">", "/ui/login?next=%2Fui%2Fsignal"),
        ("/ui/live/op/00000000000000aa", "<p class=\"live\" id=\"live-status\">", "/ui/login"),
    ] {
        let resp = open_live(&c, &format!("{uri}/{KEY}"), peer(20, 3), "").await;
        assert_eq!((resp.status().as_u16(), resp.headers()["content-type"].to_str().unwrap()), (200, "text/event-stream"));
        assert_eq!(resp.headers()["cache-control"], "no-store");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            format!("event: datastar-patch-elements\ndata: elements {status}Signed out. <a href=\"{link}\">Log in</a></p>\n\n")
        );
    }
    // Someone else's or an unknown (expired) operation: finished, so a page
    // left open does not keep showing "Pending".
    let resp = open_live(&c, &format!("/ui/live/op/00000000000000aa/{KEY}"), peer(20, 4), &cookie).await;
    assert_eq!(resp.status(), 200);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("<section class=\"card\" id=\"op-progress\">") && text.contains("This change has finished."), "{text}");
    assert!(text.contains("<p class=\"live\" id=\"live-status\"></p>"));
    // Without a page key, or with a malformed one or operation id: no stream.
    for uri in ["/ui/live/dashboard", "/ui/live/signal/0123", "/ui/live/op/xyz/0123456789abcdef", "/ui/live/op/00000000000000aa"] {
        assert_eq!(open_live(&c, uri, peer(20, 5), &cookie).await.status(), 404, "{uri}");
    }
    // HEAD neither subscribes nor replaces a stream.
    let r = req("HEAD", &format!("/ui/live/signal/{KEY}"), peer(20, 6))
        .header("cookie", &cookie)
        .header("datastar-request", "true")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&c, r).await.status, 404);
    assert_eq!(c.app.hub.demand(crate::events::Topic::Signal), 0);
}

#[tokio::test]
async fn live_dashboard_opens_with_every_fragment_and_ends_on_logout() {
    use crate::events::Topic;
    let (c, cookie, _) = signed_in(21).await;
    let resp = open_live(&c, &format!("/ui/live/dashboard/{KEY}"), peer(21, 2), &cookie).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(c.app.hub.demand(Topic::Traffic), 1);
    let mut b = resp.into_body().into_data_stream();
    let first = frame(&mut b).await.unwrap();
    assert!(first.starts_with("event: datastar-patch-elements\ndata: elements "), "{first}");
    for f in super::pages::dashboard::FRAGMENTS.iter() {
        assert!(first.contains(&format!("id=\"{}\"", f.id)), "{}", f.id);
    }
    assert!(first.contains("<p class=\"live on ps hb-o\" id=\"live-status\">"));
    assert!(!first.contains("<script") && !first.contains("data-"));
    // Logging out (in another tab) ends the stream with the signed-out line.
    let csrf = get_with(&c, "/ui/logout", peer(21, 3), &cookie).await.input("csrf");
    post(&c, "/ui/logout", peer(21, 3), &cookie, &format!("csrf={csrf}")).await.assert_see_other("/ui/login");
    let last = frame(&mut b).await.unwrap();
    assert!(last.contains("Signed out. <a href=\"/ui/login?next=%2Fui%2F\">Log in</a>"), "{last}");
    assert!(frame(&mut b).await.is_none(), "the stream ended");
    assert_eq!(c.app.hub.demand(Topic::Traffic), 0);
}

#[tokio::test]
async fn a_fourth_live_page_of_a_session_replaces_the_oldest() {
    let (c, cookie, _) = signed_in(22).await;
    let mut streams = Vec::new();
    for n in 0..4 {
        let resp = open_live(&c, &format!("/ui/live/signal/{:016x}", n + 1), peer(22, 2 + n), &cookie).await;
        streams.push(resp.into_body().into_data_stream());
    }
    let oldest = &mut streams[0];
    assert!(frame(oldest).await.unwrap().contains("hb-o"));
    let bye = frame(oldest).await.unwrap();
    assert!(bye.contains("Opened in another tab. <a href=\"/ui/signal\">Refresh</a>"), "{bye}");
    assert!(frame(oldest).await.is_none());
}

#[tokio::test]
async fn a_reconnect_replaces_only_its_own_page_stream() {
    let (c, cookie, _) = signed_in(25).await;
    let mut tabs: Vec<_> = Vec::new();
    for n in 1..=3u8 {
        let resp = open_live(&c, &format!("/ui/live/signal/{:016x}", n), peer(25, n), &cookie).await;
        tabs.push(resp.into_body().into_data_stream());
    }
    for t in &mut tabs {
        assert!(frame(t).await.unwrap().contains("hb-o"));
    }
    // The third page reconnects (its old connection may be dead but open):
    // its own earlier stream ends, the oldest tab keeps streaming.
    let again = open_live(&c, &format!("/ui/live/signal/{:016x}", 3), peer(25, 4), &cookie).await;
    let mut again = again.into_body().into_data_stream();
    assert!(frame(&mut again).await.unwrap().contains("hb-o"));
    assert!(frame(&mut tabs[2]).await.unwrap().contains("Opened in another tab."));
    assert!(frame(&mut tabs[2]).await.is_none());
    assert!(frame(&mut tabs[0]).await.unwrap().contains("hb-a"), "the first tab still beats");
}

#[tokio::test]
async fn a_finished_operation_streams_once_and_ends() {
    use super::ops::{Op, AIRPLANE_ON};
    use crate::events::hub::ReadOut;
    use crate::events::Topic;
    let (c, cookie, id) = signed_in(23).await;
    let now = c.app.auth.now();
    c.ops.insert("00000000000000dd".into(), id, Op { spec: &AIRPLANE_ON, started: now, arg: "" }, now);
    let lpm = serde_json::json!({"status": {"operate_mode": "LPM"}, "data": null});
    c.app.hub.offer(Topic::Modem, ReadOut { value: Some(lpm), error: None }, std::time::Instant::now());
    let resp = open_live(&c, &format!("/ui/live/op/00000000000000dd/{KEY}"), peer(23, 2), &cookie).await;
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("Airplane mode is on. The radio is off.") && text.contains("<p class=\"live\" id=\"live-status\"></p>"), "{text}");
    assert_eq!(text.matches("event: ").count(), 1);
    assert_eq!(c.app.hub.demand(Topic::Modem), 0);
}

/// Processes: a sheet over CPU & Memory ("Done" leads back, "Kill All
/// Bloat" trails while there is bloat), live from its own topic. A kill
/// runs only after its confirmation; the result built from the agent's
/// answer is shown on the list, to this session only. (On the build
/// machine no process has a bloat name: nothing is ever killed here.)
#[tokio::test]
async fn processes_kills_report_their_result_to_their_session() {
    use crate::events::Topic;
    let (c, cookie, _) = signed_in(46).await;
    let cpu = get_with(&c, "/ui/health/cpu", peer(46, 2), &cookie).await.text();
    assert!(cpu.contains("<a class=\"it\" href=\"/ui/health/processes\">"), "{cpu}");
    let r = get_with(&c, "/ui/health/processes", peer(46, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert!(html.contains(
        "<div class=\"lead\"><a class=\"nl\" href=\"/ui/health/cpu\">Done</a></div><h1>Processes</h1>\
         <div class=\"trail\"><span id=\"pr-all\"></span></div>"
    ), "{html}");
    assert!(html.contains("@get('/ui/live/processes/") && html.contains("id=\"pr-sum\"") && html.contains("id=\"pr-list\""));
    // Its stream samples the processes topic only, and only while open.
    let resp = open_live(&c, &live_url(&html, "processes"), peer(46, 3), &cookie).await;
    assert_eq!(c.app.hub.demand(Topic::Processes), 1);
    assert!(Topic::ALL.iter().filter(|t| **t != Topic::Processes).all(|t| c.app.hub.demand(*t) == 0));
    drop(resp);
    // One PID and the daemon the question named, confirmed. A PID that no
    // longer runs it (never a live PID here): nothing is sent, and the
    // result says so, in red (the app's green line is for a real kill).
    let csrf = r.input_or_logout(&c, &cookie).await;
    for bad in ["&pid=4194304", "&pid=4194304&name=init", "&pid=4194304&name=zte_topsw_diag2"] {
        post(&c, "/ui/health/processes/kill", peer(46, 4), &cookie, &format!("csrf={csrf}{bad}&confirm=1")).await.assert_secure_html(400);
    }
    let body = format!("csrf={csrf}&pid=4194304&name=zte_topsw_diag&confirm=1");
    let to = post(&c, "/ui/health/processes/kill", peer(46, 4), &cookie, &body).await;
    let location = to.header("location").unwrap().to_owned();
    assert_eq!(to.status, 303);
    assert!(location.starts_with("/ui/health/processes?r=") && super::ops::is_op_id(&location[23..]), "{location}");
    let shown = get_with(&c, &location, peer(46, 4), &cookie).await.text();
    let line = "<p class=\"msg err\" role=\"alert\">PID 4194304 is no longer zte_topsw_diag. Nothing was killed.</p>";
    assert!(shown.contains(line), "{shown}");
    // Another session, or an unknown id: nothing.
    let other = format!("zte_ui_session={}", login(&c, 46, 9).await);
    assert!(!get_with(&c, &location, peer(46, 9), &other).await.text().contains("Nothing was killed"));
    assert!(!get_with(&c, "/ui/health/processes?r=0123456789abcdef", peer(46, 4), &cookie).await.text().contains("killed"));
    // Kill All with nothing safe to kill: nothing is sent; a neutral line, never a success.
    let to = post(&c, "/ui/health/processes/kill-all", peer(46, 5), &cookie, &format!("csrf={csrf}&confirm=1")).await;
    let shown = get_with(&c, to.header("location").unwrap(), peer(46, 5), &cookie).await.text();
    let line = "<p class=\"msg info\" role=\"status\">No bloat daemons that are safe to kill are running. Nothing was killed.</p>";
    assert!(shown.contains(line) && !shown.contains("msg ok"), "{shown}");
    // The questions: a PID that is not running, and no safe bloat running.
    let q = get_with(&c, "/ui/health/processes/kill?pid=4194304", peer(46, 6), &cookie).await.text();
    assert!(q.contains("<h2 class=\"q\">This process is no longer running.</h2>") && !q.contains("<form"), "{q}");
    let q = get_with(&c, "/ui/health/processes/kill-all", peer(46, 6), &cookie).await.text();
    assert!(q.contains("<h2 class=\"q\">Kill all bloat daemons?</h2>") && q.contains("No bloat daemons that are safe to kill are running."));
    assert!(!q.contains("<form") && q.contains("<a class=\"btn btn-quiet\" href=\"/ui/health/processes\">Cancel</a>"), "{q}");
}

/// Wi-Fi & devices: opened from the dashboard's card, "Done" back to it;
/// live from `clients` only, with the device list inside an open
/// `<details>` outside its fragments. Share Wi-Fi is never live, never
/// cached, and nothing on it comes from the URL but `net` and `show`.
#[tokio::test]
async fn wifi_pages_link_stream_clients_and_never_stream_the_password() {
    use crate::events::Topic;
    let (c, cookie, _) = signed_in(47).await;
    let dash = get_with(&c, "/ui/", peer(47, 2), &cookie).await.text();
    assert!(dash.contains("<a class=\"card\" href=\"/ui/wifi\"><h2 class=\"ch\">"), "{dash}");
    let r = get_with(&c, "/ui/wifi", peer(47, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>Wi-Fi &amp; devices</h1><div class=\"trail\"><a class=\"nl\" href=\"/ui/\">Done</a>",
        "<p class=\"ctr\"><a class=\"nl\" href=\"/ui/wifi/share\">Share Wi-Fi</a></p>",
        "<details class=\"card devc\" open><summary class=\"ch\">", "<span>Connected Devices</span><b class=\"big\" id=\"wf-count\">",
        "<div id=\"wf-devices\">", "<h2>WiFi</h2>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let resp = open_live(&c, &live_url(&html, "wifi"), peer(47, 3), &cookie).await;
    assert!(Topic::ALL.iter().all(|t| (c.app.hub.demand(*t) > 0) == (*t == Topic::Clients)));
    drop(resp);
    for uri in ["/ui/wifi/share", "/ui/wifi/share?net=guest&show=1"] {
        let r = get_with(&c, uri, peer(47, 4), &cookie).await;
        r.assert_secure_html(200);
        let html = r.text();
        assert!(!html.contains("/ui/live/") && !html.contains("/ui/a/datastar-"), "{uri}");
        assert!(html.contains("<a class=\"nl back\" href=\"/ui/wifi\">Wi-Fi &amp; devices</a></div><h1>WiFi Credentials</h1>"), "{html}");
        // The build machine has no Wi-Fi configuration: the read's error.
        assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed to load Wi-Fi credentials: "), "{html}");
    }
    // No stream for the share page.
    let r = open_live(&c, &format!("/ui/live/wifishare/{KEY}"), peer(47, 5), &cookie).await;
    assert_eq!(r.status(), 404);
    // Signed out, the way back keeps only the page and its switches.
    get(&c, "/ui/wifi/share?net=guest&show=1", peer(47, 6)).await.assert_see_other("/ui/login?next=%2Fui%2Fwifi%2Fshare%3Fnet%3Dguest%26show%3D1");
}

// --- Table tests over every screen, action and stream (plan §3.9) ---

/// The USB-C card (the app's USB Mode sheet): on the dashboard while a
/// cable is attached, its forms outside the live fragment. "Fast Charging"
/// posts the one whitelisted value; "Dismiss" hides the card for this
/// session only, across reloads, until a detached reading: at a page load
/// or in the stream, after which the next attach shows it again.
#[tokio::test]
async fn usb_card_shows_while_attached_and_stays_dismissed_until_detached() {
    use crate::events::hub::ReadOut;
    use crate::events::Topic;
    let (c, cookie, _) = signed_in(47).await;
    let cable = |cc: &str| {
        let v = serde_json::json!({"typec_cc": cc, "mode": "rndis", "connect": 1});
        c.app.hub.offer(Topic::Usb, ReadOut { value: Some(v), error: None }, std::time::Instant::now());
    };
    const OFF: &str = "<div id=\"card-usb\"></div>";
    const ON: &str = "<div id=\"card-usb\" class=\"on\">";
    let dash = |cookie: String| {
        let c = c.clone();
        async move { get_with(&c, "/ui/", peer(47, 2), &cookie).await.text() }
    };
    // Never read (the host has no ubus) or no cable: no card; its forms
    // are in the page all the same (CSS hides them with the fragment).
    let html = dash(cookie.clone()).await;
    assert!(html.contains(OFF) && html.contains("action=\"/ui/usb/powerbank\""), "{html}");
    cable("no_cc");
    assert!(dash(cookie.clone()).await.contains(OFF));
    cable("cc1");
    let html = dash(cookie.clone()).await;
    assert!(html.contains(ON) && html.contains("USB-C Connected"), "{html}");
    // After the banners, before the first card.
    let at = |w: &str| html.find(w).unwrap();
    assert!(at("id=\"banners\"") < at("card-usb") && at("card-usb") < at("id=\"card-conn\""));
    for form in html.split("<form").skip(1) {
        assert!(form.split_once('>').unwrap().1.starts_with("<input type=\"hidden\" name=\"csrf\""), "{form}");
    }
    let csrf = get_with(&c, "/ui/logout", peer(47, 2), &cookie).await.input("csrf");
    // Only `state=1` is accepted; on the host the call fails, shown as the app's "Failed: …".
    for bad in ["", "&state=0", "&state=2", "&state=1%20", "&state=%3Cx%3E"] {
        post(&c, "/ui/usb/powerbank", peer(47, 3), &cookie, &format!("csrf={csrf}{bad}")).await.assert_secure_html(400);
    }
    let r = post(&c, "/ui/usb/powerbank", peer(47, 3), &cookie, &format!("csrf={csrf}&state=1&mode=x")).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Failed: "), "{}", r.text());
    // Dismissed: hidden on reload while attached, for this session only.
    post(&c, "/ui/usb/dismiss", peer(47, 3), &cookie, &format!("csrf={csrf}")).await.assert_see_other("/ui/");
    assert!(dash(cookie.clone()).await.contains(OFF));
    let other = format!("zte_ui_session={}", login(&c, 47, 4).await);
    assert!(dash(other).await.contains(ON));
    // A detached reading at a page load ends it.
    cable("no_cc");
    assert!(dash(cookie.clone()).await.contains(OFF));
    cable("cc1");
    assert!(dash(cookie.clone()).await.contains(ON));
    // In the stream: dismissed, then detached and attached again while
    // the page stays open; the card comes back, and stays on reload.
    post(&c, "/ui/usb/dismiss", peer(47, 3), &cookie, &format!("csrf={csrf}")).await.assert_see_other("/ui/");
    let page = dash(cookie.clone()).await;
    assert!(page.contains(OFF));
    let resp = open_live(&c, &live_url(&page, "dashboard"), peer(47, 5), &cookie).await;
    let mut b = resp.into_body().into_data_stream();
    assert!(frame(&mut b).await.unwrap().contains(OFF));
    cable("no_cc");
    // The stream reads the change before its next heartbeat (changes come
    // first); the card was off and stays off, so nothing else is patched.
    let beat = frame(&mut b).await.unwrap();
    assert!(beat.contains("id=\"live-status\"") && !beat.contains("card-usb"), "{beat}");
    cable("cc1");
    let mut shown = false;
    for _ in 0..6 {
        let Some(f) = frame(&mut b).await else { break };
        assert!(!f.contains("<form") && !f.contains("csrf"), "{f}");
        if f.contains(ON) {
            shown = true;
            break;
        }
    }
    assert!(shown, "the stream shows the card for the new attachment");
    assert!(dash(cookie.clone()).await.contains(ON));
}

/// Tools › USB Mode (the app's `USBModeView`): linked from Tools, read at
/// page load (no stream), the switch posts 0 or 1 only, and is disabled
/// without a cable. The dashboard's "Fast Charging" sends nothing unless a
/// cable is attached now.
#[tokio::test]
async fn usb_mode_turns_fast_charging_on_and_off() {
    use crate::events::Topic;
    let (c, cookie, _) = signed_in(48).await;
    let tools = get_with(&c, "/ui/tools", peer(48, 2), &cookie).await.text();
    assert!(tools.contains("<li><a class=\"it\" href=\"/ui/tools/usb\"><svg"), "{tools}");
    // The host has no ubus: unknown, so "Disconnected" and no switch to press.
    let r = get_with(&c, "/ui/tools/usb", peer(48, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert!(html.contains("<a class=\"nl back\" href=\"/ui/tools\">Tools</a></div><h1>USB Mode</h1>"), "{html}");
    assert!(html.contains("<span class=\"v\">Disconnected</span>") && !html.contains("<form class=\"sf\""), "{html}");
    assert!(!html.contains("/ui/live/") && Topic::ALL.iter().all(|t| c.app.hub.demand(*t) == 0));
    let attached = serde_json::json!({"typec_cc": "cc1", "mode": "rndis", "connect": 1, "otg_powerbank_state": 1});
    let fake = super::pages::fake::set(&c.app, attached);
    let html = get_with(&c, "/ui/tools/usb", peer(48, 2), &cookie).await.text();
    assert!(html.contains("<span class=\"c-green\">Connected</span>") && html.contains("name=\"state\" value=\"0\"><button class=\"sw\""), "{html}");
    let csrf = get_with(&c, "/ui/logout", peer(48, 2), &cookie).await.input("csrf");
    for bad in ["", "&state=2", "&state=%3Cx%3E", "&state=01"] {
        post(&c, "/ui/tools/usb/powerbank", peer(48, 3), &cookie, &format!("csrf={csrf}{bad}")).await.assert_secure_html(400);
    }
    // On the host the call fails: the screen again with "Failed: …".
    let r = post(&c, "/ui/tools/usb/powerbank", peer(48, 3), &cookie, &format!("csrf={csrf}&state=0")).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Failed: ") && r.text().contains("<h1>USB Mode</h1>"), "{}", r.text());
    drop(fake);
    // The card's button checks the cable first: the read fails here, so nothing is sent.
    let r = post(&c, "/ui/usb/powerbank", peer(48, 4), &cookie, &format!("csrf={csrf}&state=1")).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Failed: "), "{}", r.text());
}

/// The hub holds a value for every topic of every screen (a page load also
/// reads, which fails on the host and only adds an error).
fn offer_all(c: &Ctx, v: &Value) {
    use crate::events::hub::ReadOut;
    use crate::events::Topic;
    for t in Topic::ALL {
        c.app.hub.offer(t, ReadOut { value: Some(v.clone()), error: None }, std::time::Instant::now());
    }
}

/// The stream URL of a live page as served.
fn live_url(page: &str, id: &str) -> String {
    let key = page.split(&format!("@get('/ui/live/{id}/")).nth(1).unwrap().split('\'').next().unwrap();
    format!("/ui/live/{id}/{key}")
}

/// Every `id="…"` patched by an SSE frame, with its element.
fn frame_parts(frame: &str) -> Vec<(String, String)> {
    frame
        .lines()
        .filter_map(|l| l.strip_prefix("data: elements "))
        .map(|p| (p.split("id=\"").nth(1).unwrap().split('"').next().unwrap().to_owned(), p.to_owned()))
        .collect()
}

/// Every live screen: its stream subscribes to exactly the topics its
/// fragments are rendered from; the first frame patches each fragment,
/// and each one is in the GET page once and byte for byte (the status
/// line and the age badge, which differ by design or by the second, aside),
/// in states that show something (page loads read the same data).
#[tokio::test]
async fn every_live_screen_streams_its_fragments_as_in_the_page() {
    use crate::events::Topic;
    let (c, cookie, _) = signed_in(40).await;
    let all = serde_json::json!({
        "network_type": "ENDC", "network_provider": "Op", "nr5g_rsrp": "-77", "lte_rsrp": "-85", "net_select": "Only_LTE",
        "real_rx_speed": 52, "real_tx_speed": 69, "status": {"operate_mode": "ONLINE"}, "capacity": 69,
        "data": {"enable": 1, "connect_status": "ipv4_connected"}, "sim_states": "Ready",
        "hosts": {"aa:bb:cc:dd:ee:01": {"name": "tv", "ipaddrs": ["192.168.0.5"]}, "x": 1},
        "cpu": {"overall": 40.84, "cores": [1.0, 2.0]}, "thermal": {"cpuss_temp": 37.2, "sensors": [
            {"id": "z0", "name": "cpuss-0", "temperature_c": 40.1}, {"id": "z1", "name": "usb", "temperature_c": 33.0}]},
        "typec_cc": "cc1", "mode": "rndis", "otg_powerbank_state": 1,
        "processes": [{"pid": 812, "name": "zte_topsw_diag", "cpu_pct": 3.0, "rss_kb": 2048, "state": "sleeping", "is_bloat": true}],
        "bloat_count": 1, "bloat_cpu_pct": 3.0, "bloat_rss_kb": 2048,
        "phase": "upload", "progress": 80, "live_speed_mbps": 12.3, "server": "Example Net (Testville)",
    });
    // Signal Detection streams only while a sweep runs (its `progress` is
    // an object, the speed test's a number), Add eSIM while a download runs.
    let sweep = serde_json::json!({"progress": {"progress": 40, "running": 1}, "results": null, "state": "running", "step": "es9p_authenticate_client"});
    // What the visible states put in their fragments.
    let shows = [
        ("card-usb", "<div id=\"card-usb\" class=\"on pb\">"), ("card-devices", "<b class=\"big\">1</b>"),
        ("pr-all", "Kill All Bloat"), ("pr-sum", "Bloat Daemons"), ("pr-list", "PID 812"),
        ("wf-count", ">1</b>"), ("wf-devices", "<b>tv</b>"), ("cd-list", "<b class=\"dn\">tv</b>"),
        ("st-run", "<span>Uploading...</span><span class=\"muted num\">80%</span>"),
        ("sd-run", "<span>Progress</span><span class=\"muted num\">40%</span>"),
        ("eadd-dl", "<b>Connecting to the operator…</b>"),
    ];
    let mut shown = 0;
    for s in super::routes::SCREENS.iter().filter(|s| !s.live.is_empty()) {
        let fixture = if s.live_while.is_some() { &sweep } else { &all };
        let _fake = super::pages::fake::set(&c.app, fixture.clone());
        offer_all(&c, fixture);
        let mut union: Vec<Topic> = s.live.iter().flat_map(|f| f.topics.iter().copied()).collect();
        union.sort_by_key(|t| *t as u8);
        union.dedup();
        let mut topics = s.topics.to_vec();
        topics.sort_by_key(|t| *t as u8);
        assert_eq!(topics, union, "{}", s.path);
        let page = get_with(&c, s.path, peer(40, 2), &cookie).await.text();
        let resp = open_live(&c, &live_url(&page, s.id), peer(40, 3), &cookie).await;
        for t in Topic::ALL {
            assert_eq!(c.app.hub.demand(t) > 0, s.topics.contains(&t), "{} {t:?}", s.path);
        }
        let mut b = resp.into_body().into_data_stream();
        let parts = frame_parts(&frame(&mut b).await.unwrap());
        for f in s.live {
            assert_eq!(page.matches(&format!("id=\"{}\"", f.id)).count(), 1, "{} {}", s.path, f.id);
            let (_, html) = parts.iter().find(|(id, _)| id == f.id).unwrap_or_else(|| panic!("{} {}", s.path, f.id));
            assert!(f.id == "age" || page.contains(html.as_str()), "{} {} differs from the page", s.path, f.id);
            assert!(!html.contains("<form") && !html.contains("csrf") && !html.contains("<details") && !html.contains("<input"));
            if let Some((_, want)) = shows.iter().find(|(id, _)| *id == f.id) {
                assert!(html.contains(want), "{} {}: {want} in {html}", s.path, f.id);
                shown += 1;
            }
        }
        drop(b);
        for _ in 0..100 {
            if Topic::ALL.iter().all(|t| c.app.hub.demand(*t) == 0) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(Topic::ALL.iter().all(|t| c.app.hub.demand(*t) == 0), "{} left demand", s.path);
    }
    assert_eq!(shown, shows.len());
    // The other screens have no stream, and their pages subscribe nothing.
    for s in super::routes::SCREENS.iter().filter(|s| s.live.is_empty()) {
        let page = get_with(&c, s.path, peer(40, 4), &cookie).await.text();
        assert!(!page.contains("/ui/live/") && Topic::ALL.iter().all(|t| c.app.hub.demand(*t) == 0), "{}", s.path);
        let r = open_live(&c, &format!("/ui/live/{}/{KEY}", s.id), peer(40, 5), &cookie).await;
        assert_eq!(r.status(), 404, "{}", s.path);
    }
}

/// No identifier, key or host data reaches any screen or its first stream
/// frame, whatever the hub holds and page loads read: device names and
/// IPv4 addresses only on Wi-Fi & devices (MACs masked there, also inside
/// a name), the Wi-Fi password only on Share Wi-Fi after "Show password".
#[tokio::test]
async fn no_identifier_reaches_any_screen_or_stream() {
    let (c, cookie, _) = signed_in(41).await;
    let hostile = serde_json::json!({
        "wifi_onoff": "1", "radio2_disabled": "0", "ssid_2g": "Net-42", "key_5g": "WIFIKEY-5g", "encryption_2g": "psk2",
        "guest_disabled_2g": "1", "guest_disabled_5g": "1",
        "imei": "000000000000042", "imsi": "001019999911111", "sim_imsi": "001019999911111",
        "sim_iccid": "8900100000009998877", "iccid": "8900100000009998877", "msisdn": "+10000000042",
        "mac": "aa:bb:cc:dd:ee:42", "hostname": "alice-laptop-42", "key_2g": "WIFIKEY-7f3a",
        "hosts": {
            "aa:bb:cc:dd:ee:42": {"name": "alice-laptop-42", "ipaddrs": ["192.168.0.42"]},
            "aa:bb:cc:dd:ee:43": {"name": "shellyplus-AABBCCDDEE43", "ipaddrs": ["192.168.0.43"]},
        },
        "dhcp_leases": [{"macaddr": "aa:bb:cc:dd:ee:42", "hostname": "alice-laptop-42", "ipaddr": "192.168.0.42"}],
        "status": {"operate_mode": "ONLINE", "imei": "000000000000042"},
        "data": {"enable": 1, "connect_status": "ipv4_connected", "key_2g": "WIFIKEY-7f3a"},
        "network_type": "ENDC", "network_provider": "<script>x</script>", "nr5g_rsrp": "-77", "capacity": 50,
        "cpu": {"overall": 1.0, "cores": [1.0]}, "thermal": {"sensors": [{"id": "z", "name": "<b>usb</b>", "temperature_c": 30.0}]},
    });
    offer_all(&c, &hostile);
    let _fake = super::pages::fake::set(&c.app, hostile.clone());
    let secrets = [
        "000000000000042", "001019999911111", "8900100000009998877", "9998877", "10000000042", "aa:bb:cc:dd:ee:4",
        "AA:BB:CC", "aabbccddee4", "AABBCCDDEE4", "WIFIKEY-", "<script>x", "<b>usb",
    ];
    // Allowed on Wi-Fi & devices and Connected Devices only.
    let devices = ["alice-laptop-42", "192.168.0.42", "shellyplus-"];
    let lists = ["/ui/wifi", "/ui/tools/devices"];
    for s in super::routes::SCREENS {
        let page = get_with(&c, s.path, peer(41, 2), &cookie).await.text();
        let mut text = page.clone();
        // (Signal Detection streams only while a sweep runs.)
        if page.contains("@get('/ui/live/") {
            let resp = open_live(&c, &live_url(&page, s.id), peer(41, 3), &cookie).await;
            let mut b = resp.into_body().into_data_stream();
            text.push_str(&frame(&mut b).await.unwrap());
        }
        for x in secrets {
            assert!(!text.contains(x), "{} has {x}", s.path);
        }
        for x in devices {
            assert_eq!(text.contains(x), lists.contains(&s.path), "{} {x}", s.path);
        }
    }
    // The data did reach the pages: the devices with masked MACs, the
    // Wi-Fi card, and the password after "Show password" only.
    let wifi = get_with(&c, "/ui/wifi", peer(41, 4), &cookie).await.text();
    for want in ["<b>alice-laptop-42</b>", "••:••:••:••:ee:42", "<b>shellyplus-••••••••••••</b>", "<b>Net-42</b>", ">On<"] {
        assert!(wifi.contains(want), "{want} in {wifi}");
    }
    assert!(get_with(&c, "/ui/wifi/share", peer(41, 4), &cookie).await.text().contains("<b>Net-42</b>"));
    let shown = get_with(&c, "/ui/wifi/share?show=1", peer(41, 4), &cookie).await.text();
    assert!(shown.contains("<span class=\"mono\" translate=\"no\">WIFIKEY-7f3a</span>"), "{shown}");
}

/// Every live fragment of every screen, rendered from hostile data in
/// every topic (a page load on the build machine may replace the hub's
/// value): no identifier or key anywhere; device names and IPv4 addresses
/// only on Wi-Fi & devices, whose MAC addresses keep their last 2 octets.
#[test]
fn fragments_show_identifiers_only_where_allowed() {
    use crate::boottime::{BootClock, Clock};
    use crate::events::hub::SlotView;
    use crate::events::Topic;
    let hostile = serde_json::json!({
        "imei": "000000000000042", "sim_iccid": "8900100000009998877", "msisdn": "+10000000042", "key_2g": "WIFIKEY-7f3a",
        "hosts": {"aa:bb:cc:dd:ee:42": {"name": "alice-laptop-42", "ipaddrs": ["192.168.0.42"], "ip6addrs": ["2001:db8::42"]}},
        "dhcp_leases": [{"macaddr": "aa:bb:cc:dd:ee:42", "hostname": "alice-laptop-42"}],
        "processes": [{"pid": 7, "name": "<script>x</script>", "is_bloat": true}], "bloat_count": 1,
    });
    let now = BootClock.now();
    let slot = SlotView { version: 1, at: now, value_at: now, value: Some(Arc::new(hostile)), error: None };
    let v = super::render::Views::new(now, Topic::ALL.iter().map(|t| (*t, slot.clone())).collect());
    for s in super::routes::SCREENS {
        let html: String = s.live.iter().map(|f| (f.render)(&v).into_string()).collect();
        for x in ["000000000000042", "8900100000009998877", "10000000042", "WIFIKEY", "aa:bb:cc", "AA:BB:CC", "2001:db8", "<script"] {
            assert!(!html.contains(x), "{} has {x}", s.path);
        }
        let devices = html.contains("alice-laptop-42") && html.contains("192.168.0.42") && html.contains("••:••:••:••:ee:42");
        assert_eq!(devices, ["/ui/wifi", "/ui/tools/devices"].contains(&s.path), "{}", s.path);
    }
}

/// Every `/ui` link on every screen and confirmation page leads somewhere
/// (no 404 or 400); every form posts to an action.
#[tokio::test]
async fn every_link_resolves() {
    let (c, cookie, _) = signed_in(42).await;
    let mut pages: Vec<String> = super::routes::SCREENS.iter().map(|s| s.path.to_owned()).collect();
    for a in super::routes::ACTIONS {
        pages.extend(a.confirm.iter().map(|k| format!("{}/confirm?{}={}", a.path, k.when.0, k.when.1)));
    }
    pages.push("/ui/logout".into());
    let mut checked = Vec::new();
    for p in &pages {
        let html = get_with(&c, p, peer(42, 2), &cookie).await.text();
        for href in html.split("href=\"").skip(1).map(|s| s.split('"').next().unwrap().replace("&amp;", "&")) {
            if !href.starts_with("/ui") || href.starts_with("/ui/a/") || href == "/ui/manifest.webmanifest" || checked.contains(&href) {
                continue;
            }
            let status = get_with(&c, &href, peer(42, 3), &cookie).await.status;
            assert!(status != 404 && status != 400, "{p} -> {href}: {status}");
            checked.push(href);
        }
        // Every POST form (the SMS search posts to its own page).
        for form in html.split("<form").skip(1).map(|s| &s[..s.find('>').unwrap()]).filter(|f| f.contains("method=\"post\"")) {
            let action = form.split(" action=\"").nth(1).unwrap().split('"').next().unwrap();
            assert!(action == "/ui/logout" || super::routes::form_page(action).is_some(), "{p} posts to {action}");
        }
    }
    assert!(checked.len() >= 18, "{checked:?}");
}

/// Unknown `?saved=` keys show nothing, on every screen; a screen GET is
/// read-only (debug builds panic on a mutating call inside one, so every
/// screen answering 200 here and in `every_screen_marks_its_tab` checks it).
#[tokio::test]
async fn unknown_notices_show_nothing_and_pages_change_nothing() {
    let (c, cookie, _) = signed_in(43).await;
    for s in super::routes::SCREENS {
        let r = get_with(&c, &format!("{}?saved=%3Cb%3Eowned", s.path), peer(43, 2), &cookie).await;
        r.assert_secure_html(if s.status.is_some() { 404 } else { 200 });
        assert!(!r.text().contains("owned") && !r.text().contains("class=\"msg info\"") && !r.text().contains("class=\"msg ok\""), "{}", s.path);
    }
}

#[test]
#[should_panic(expected = "a page load changed the device: /api/modem/data")]
fn a_mutating_call_inside_a_page_load_panics() {
    let app = AppState::new();
    crate::httpd::gate::read_only(|| crate::server::route(&crate::server::Method::Put, "/api/modem/data", &app, b"{}"));
}

/// A state read that fails (even with "timed out") sends nothing: the form
/// again with the error, no operation page.
#[tokio::test]
async fn a_failed_state_read_starts_no_operation() {
    use super::action::{Action, After};
    use super::screen::Read;
    fn timed_out(_: &AppState, _: Option<&str>) -> Result<Value, String> {
        Err("ubus call zwrt_data get_wwaniface failed: Command failed: Request timed out".into())
    }
    static READ_TIMES_OUT: Action = Action {
        path: MN,
        screen: &super::tabs::router::MOBILE_NETWORK,
        fields: &[],
        confirm: &[],
        confirm_at: None,
        unchanged: None,
        reads: &[Read::Fn(timed_out)],
        // Never called: if it were, the answer would be its 404.
        call: (crate::server::Method::Put, "/api/no-such-route"),
        body: |_, _| None,
        after: After::Op(|_| (&super::ops::DATA_OFF, "")),
        again: None,
        once: false,
    };
    let (c, cookie, _) = signed_in(44).await;
    let csrf = get_with(&c, MN, peer(44, 2), &cookie).await.input("csrf");
    let body = format!("csrf={csrf}");
    let r = form(MN, peer(44, 3), &cookie, &body).header("origin", ORIGIN).body(Body::from(body.clone())).unwrap();
    let resp = super::action::post(c.clone(), r, &READ_TIMES_OUT).await;
    assert_eq!(resp.status(), 503);
    assert!(resp.headers().get("location").is_none());
    let html = String::from_utf8(axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed: ubus call zwrt_data get_wwaniface failed"), "{html}");
    // An action without confirmations has no confirmation page.
    for uri in ["/ui/router/device/charge-limit/confirm", "/ui/router/device/charge-limit/confirm?enabled=1"] {
        get_with(&c, uri, peer(44, 4), &cookie).await.assert_secure_html(404);
    }
}

mod devctl;
mod esim;
mod esim_add;
mod firewall;
mod forward;
mod https;
mod netcfg;
mod router;
mod sim;
mod sms;
mod tools;
mod tools_send;
