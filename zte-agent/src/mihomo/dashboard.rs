//! Zashboard, the pinned Mihomo dashboard, and its restricted same-origin
//! bridge to the loopback core controller. The responsive console opens it in
//! a new tab after trading its bearer token for a short-lived pass cookie
//! (`POST /api/mihomo/dashboard`): no credential goes into a URL, and the core
//! secret never leaves the router.

use std::path::Component;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header::*, HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use futures_util::StreamExt;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{API, CONTROLLER, DASHBOARD};
use crate::auth::{SessionCheck, SessionId};
use crate::boottime::BootInstant;
use crate::handlers::AppState;
use crate::httpd::gate::{GateError, Lane};
use crate::httpd::{api, Ctx};

mod adapter;

/// `zte_ui_*` so the HTTPS listener maps it to a `__Host-` cookie by itself.
const PASS_COOKIE: &str = "zte_ui_zashboard";
const PASS_PATH: &str = "/ui/mihomo/";
const PASS_LIFE: Duration = Duration::from_secs(12 * 3600);
const PASS_CAP: usize = 8;
/// Well-formed pass cookies looked at per request: a page on this host's port
/// 80 can set more of them, and browsers send those first.
const PASS_COOKIES: usize = 8;
const SIGN_IN: &str = "/console/#network/proxies";

/// Dashboard passes, kept in RAM. A pass belongs to the console session that
/// asked for it and stops working with it (logout, expiry, password change).
pub struct Passes {
    entries: Mutex<Vec<Pass>>,
    changed: tokio::sync::watch::Sender<()>,
}

impl Default for Passes {
    fn default() -> Self {
        Self { entries: Mutex::new(Vec::new()), changed: tokio::sync::watch::channel(()).0 }
    }
}

struct Pass {
    digest: [u8; 32],
    owner: SessionId,
    born: BootInstant,
}

fn digest(token: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"zashboard-pass\0");
    hash.update(token.as_bytes());
    hash.finalize().into()
}

impl Passes {
    fn lock(&self) -> MutexGuard<'_, Vec<Pass>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One pass per console session: the browser's cookie is replaced anyway.
    fn issue(&self, token: &str, owner: SessionId, now: BootInstant) {
        let mut passes = self.lock();
        passes.retain(|pass| pass.owner != owner && now.saturating_duration_since(pass.born) < PASS_LIFE);
        if passes.len() >= PASS_CAP {
            passes.remove(0);
        }
        passes.push(Pass { digest: digest(token), owner, born: now });
        drop(passes);
        self.changed.send_replace(());
    }

    fn owner(&self, headers: &HeaderMap, now: BootInstant) -> Option<SessionId> {
        self.key(headers, now).map(|(_, owner)| owner)
    }

    fn key(&self, headers: &HeaderMap, now: BootInstant) -> Option<([u8; 32], SessionId)> {
        let presented: Vec<[u8; 32]> = pass_cookies(headers).into_iter().map(digest).collect();
        if presented.is_empty() {
            return None;
        }
        let mut passes = self.lock();
        passes.retain(|pass| now.saturating_duration_since(pass.born) < PASS_LIFE);
        presented.iter().find_map(|wanted| passes.iter().find(|pass| pass.digest == *wanted).map(|pass| (pass.digest, pass.owner)))
    }
}

impl Passes {
    fn remaining(&self, key: ([u8; 32], SessionId), now: BootInstant) -> Option<Duration> {
        let passes = self.lock();
        let pass = passes.iter().find(|pass| (pass.digest, pass.owner) == key)?;
        PASS_LIFE.checked_sub(now.saturating_duration_since(pass.born)).filter(|left| !left.is_zero())
    }

    fn revoke(&self, owner: SessionId) {
        self.lock().retain(|pass| pass.owner != owner);
        self.changed.send_replace(());
    }
}

pub fn revoke(app: &AppState, owner: SessionId) -> Response {
    app.mihomo.passes.revoke(owner);
    let mut reply = api::json_reply(200, &json!({"ok": true}));
    reply.headers_mut().insert(SET_COOKIE, HeaderValue::from_static("zte_ui_zashboard=; Path=/ui/mihomo/; HttpOnly; SameSite=Strict; Max-Age=0"));
    reply.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    reply
}

fn pass_cookies(headers: &HeaderMap) -> Vec<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name.trim() == PASS_COOKIE).then(|| value.trim())
        })
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
        .take(PASS_COOKIES)
        .collect()
}

/// `POST /api/mihomo/dashboard` for a console session (already authenticated
/// by the API layer): a pass cookie that only `/ui/mihomo/` ever receives.
pub fn grant(app: &AppState, owner: SessionId) -> Response {
    if !app.mihomo.running() {
        return api::json_reply(409, &crate::server::error_body("Clash is off"));
    }
    if !matches!(app.auth.touch_session(owner), SessionCheck::Live { .. }) {
        return api::json_reply(401, &crate::server::error_body("unauthorized"));
    }
    let Ok(token) = app.auth.random_hex(32) else {
        return api::json_reply(503, &crate::server::error_body("No randomness available"));
    };
    app.mihomo.passes.issue(&token, owner, app.auth.now());
    let mut reply = api::json_reply(200, &json!({"ok": true, "data": {"url": dashboard_link()}}));
    let headers = reply.headers_mut();
    if let Ok(cookie) = HeaderValue::from_str(&format!("{PASS_COOKIE}={token}; Path={PASS_PATH}; HttpOnly; SameSite=Strict")) {
        headers.append(SET_COOKIE, cookie);
    }
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    reply
}

fn signed_in(app: &AppState, headers: &HeaderMap) -> Option<SessionId> {
    let owner = app.mihomo.passes.owner(headers, app.auth.now())?;
    matches!(app.auth.touch_session(owner), SessionCheck::Live { .. }).then_some(owner)
}

pub fn dashboard_link() -> String {
    // The pinned dashboard adapter selects this browser's same-origin bridge
    // before its router mounts. No setup query or credentials are needed.
    format!("{DASHBOARD}#/proxies")
}

pub fn router() -> Router<Ctx> {
    Router::new()
        .route("/ui/mihomo/dashboard", any(page))
        .route("/ui/mihomo/dashboard/", any(page))
        .route("/ui/mihomo/dashboard/{*file}", any(page))
        .route("/ui/mihomo/api", any(page))
        .route("/ui/mihomo/api/{*rest}", any(page))
}

async fn page(State(ctx): State<Ctx>, req: Request) -> Response {
    if !crate::console::host_allowed(&ctx, &req) {
        return refuse(req, plain(403, "Host not allowed")).await;
    }
    let Some(owner) = signed_in(&ctx.app, req.headers()) else {
        let navigation = matches!(*req.method(), Method::GET | Method::HEAD) && req.uri().path().starts_with(DASHBOARD.trim_end_matches('/'));
        let reply = if navigation { see_other(SIGN_IN) } else { plain(401, "Open Zashboard from the router console") };
        return refuse(req, reply).await;
    };
    harden(serve(ctx, req, owner).await)
}

/// A reply that leaves the request body unread.
async fn refuse(req: Request, reply: Response) -> Response {
    let (parts, body) = req.into_parts();
    harden(api::finish_early(&parts.headers, body.into_data_stream(), 0, reply).await)
}

/// Nothing here may be embedded by another origin, and the entry page gets
/// the dashboard's own CSP (the agent's nonced page policy is not relaxed).
fn harden(mut reply: Response) -> Response {
    let html = reply.headers().get(CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(|value| value.starts_with("text/html"));
    let headers = reply.headers_mut();
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    if html {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        secure_dashboard(&mut reply);
    }
    reply
}

fn plain(status: u16, message: &'static str) -> Response {
    let mut r = Response::new(Body::from(message));
    *r.status_mut() = StatusCode::from_u16(status).unwrap();
    r.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    r.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn busy() -> Response {
    let mut reply = plain(503, "Router busy, retry");
    reply.headers_mut().insert(RETRY_AFTER, HeaderValue::from(1));
    reply
}

fn see_other(location: &str) -> Response {
    let mut reply = Response::new(Body::empty());
    *reply.status_mut() = StatusCode::SEE_OTHER;
    if let Ok(value) = HeaderValue::from_str(location) {
        reply.headers_mut().insert(LOCATION, value);
    }
    reply.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    reply
}

/// Everything after sign-in: the dashboard's files and the bridge.
pub async fn serve(ctx: Ctx, mut req: Request, owner: SessionId) -> Response {
    let path = req.uri().path().to_owned();
    // Firewall transactions can hold inner for seconds: never wait on it here.
    let secret = match core_secret(&ctx).await {
        Ok(Some(secret)) => secret,
        Ok(None) => return plain(409, "Mihomo is off"),
        Err(_) => return busy(),
    };
    if path == DASHBOARD.trim_end_matches('/') {
        return see_other(DASHBOARD);
    }
    if let Some(file) = path.strip_prefix(DASHBOARD) {
        if !matches!(*req.method(), Method::GET | Method::HEAD) {
            return plain(405, "GET only");
        }
        let file = if file.is_empty() { "index.html" } else { file };
        // Service-worker caches would survive logout/off and are unnecessary
        // for a device-local panel whose resources are already offline.
        if file == "sw.js" {
            return plain(404, "Service worker disabled");
        }
        if file == "registerSW.js" {
            let mut r = Response::new(Body::from("/* Device-local dashboard: service worker intentionally disabled. */"));
            r.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/javascript; charset=utf-8"));
            return r;
        }
        let relative = std::path::Path::new(file);
        if file.contains(['%', '\\']) || relative.components().any(|c| !matches!(c, Component::Normal(_))) {
            return plain(404, "Not found");
        }
        let (root, file, head) = (ctx.app.mihomo.dashboard_root(), file.to_owned(), req.method() == Method::HEAD);
        return match ctx.gate.run(Lane::General, move || dashboard_file(&root, &file, head)).await {
            Ok(reply) => reply,
            Err(GateError::Busy) => busy(),
            Err(GateError::Panicked) => plain(500, "Internal error"),
        };
    }
    let endpoint = path.strip_prefix(API).unwrap_or("");
    if !allowed(req.method(), endpoint) {
        return plain(403, "This core operation is managed by the router");
    }
    let h = api::req_headers(req.headers());
    // Same-site is not enough: the vendor UI on port 80 is a different origin.
    // Require an exact Origin or Referer for API reads as well as writes.
    if !same_origin(&h) {
        return plain(403, "Same-origin dashboard requests only");
    }
    let query = bridge_query(req.uri().query());
    if req.headers().get(UPGRADE).is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket")) {
        if req.method() != Method::GET || !["/traffic", "/memory", "/connections", "/logs"].contains(&endpoint) {
            return plain(403, "WebSocket endpoint not allowed");
        }
        return websocket(&ctx, &mut req, &format!("{endpoint}{query}"), &secret, owner).await;
    }
    let method = req.method().clone();
    let (parts, body) = req.into_parts();
    let bytes = match api::read_capped(&parts.headers, body, 64 * 1024, ctx.body_wait).await {
        Ok(v) => v,
        Err(_) => return plain(413, "Dashboard request too large"),
    };
    if method == Method::PATCH && endpoint == "/configs" && !safe_patch(&bytes) {
        return plain(403, "Only mode and log-level can be changed here");
    }
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap()
    });
    let mut request = client.request(method, format!("http://{CONTROLLER}{endpoint}{query}")).bearer_auth(secret).body(bytes);
    if let Some(ct) = parts.headers.get(CONTENT_TYPE) {
        request = request.header(CONTENT_TYPE, ct);
    }
    let Ok(response) = request.send().await else {
        return plain(502, "Mihomo did not answer");
    };
    let status = response.status();
    let ct = response.headers().get(CONTENT_TYPE).cloned();
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return plain(502, "Mihomo response was interrupted");
        };
        if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
            return plain(502, "Mihomo response too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    // /configs includes the core secret. A dashboard needs neither that key
    // nor listening addresses; never reflect them back to the browser.
    if endpoint == "/configs" && status.is_success() {
        let Some(redacted) = dashboard_config(&bytes) else { return plain(502, "Invalid Mihomo configuration response"); };
        bytes = redacted;
    }
    let mut r = Response::new(Body::from(bytes));
    *r.status_mut() = status;
    if let Some(ct) = ct {
        r.headers_mut().insert(CONTENT_TYPE, ct);
    }
    r.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}


fn dashboard_file(root: &std::path::Path, file: &str, head: bool) -> Response {
    let Ok(canonical) = root.join(file).canonicalize() else {
        return plain(404, "Dashboard file not found");
    };
    let Ok(root) = root.canonicalize() else {
        return plain(404, "Dashboard not installed");
    };
    if !canonical.starts_with(root) {
        return plain(404, "Not found");
    }
    let Ok(data) = std::fs::read(canonical) else {
        return plain(404, "Not found");
    };
    if data.len() > 8 * 1024 * 1024 {
        return plain(413, "Dashboard file too large");
    }
    let data = match adapter::asset(file, data) {
        Ok(data) => data,
        Err(e) => return plain(503, e),
    };
    let mime = match std::path::Path::new(file).extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" | "webmanifest" => "application/json",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => "application/octet-stream",
    };
    let mut r = Response::new(if head { Body::empty() } else { Body::from(data) });
    r.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static(mime));
    r.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn dashboard_config(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object_mut()?;
    for key in ["secret", "external-controller", "external-controller-tls", "external-ui", "authentication", "listeners"] {
        object.remove(key);
    }
    serde_json::to_vec(&value).ok()
}

fn same_origin(h: &crate::httpd::guard::ReqHeaders<'_>) -> bool {
    use crate::httpd::guard::{self, OriginVerdict};
    !guard::fetch_site_foreign(h)
        && match guard::origin_verdict(h) {
            OriginVerdict::Same => true,
            OriginVerdict::Absent => guard::referer_verdict(h) == OriginVerdict::Same,
            OriginVerdict::Cross => false,
        }
}

fn bridge_query(query: Option<&str>) -> String {
    // Mihomo prefers a nonempty WebSocket token query to Authorization. The
    // browser has no core credential; even a stale saved token must not win.
    let parts: Vec<_> = query
        .into_iter()
        .flat_map(|s| s.split('&'))
        .filter(|part| !part.is_empty() && crate::httpd::api::percent_decode(part.split('=').next().unwrap_or("")) != "token")
        .collect();
    if parts.is_empty() { String::new() } else { format!("?{}", parts.join("&")) }
}

fn allowed(method: &Method, path: &str) -> bool {
    let decoded = crate::httpd::api::percent_decode(path);
    if decoded.contains(['%', '\\']) || decoded.chars().any(char::is_control) || decoded.split('/').any(|s| s == "." || s == "..") {
        return false;
    }
    match *method {
        Method::GET => {
            path.is_empty()
                || path == "/"
                || [
                    "/version",
                    "/configs",
                    "/rules",
                    "/proxies",
                    "/group",
                    "/providers/proxies",
                    "/providers/rules",
                    "/storage/zashboard",
                    "/connections",
                    "/traffic",
                    "/memory",
                    "/logs",
                ]
                .contains(&path)
                || path.starts_with("/proxies/")
                || path.starts_with("/group/")
                || path.starts_with("/providers/proxies/")
                || path.starts_with("/providers/rules/")
        }
        Method::PATCH => path == "/configs",
        Method::PUT => path.starts_with("/proxies/") || path.starts_with("/providers/proxies/") || path.starts_with("/providers/rules/"),
        Method::DELETE => path == "/connections" || path.starts_with("/connections/"),
        Method::POST => path == "/cache/dns/flush" || path == "/cache/fakeip/flush",
        _ => false,
    }
}

fn safe_patch(bytes: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else { return false };
    let Some(m) = v.as_object() else { return false };
    !m.is_empty()
        && m.iter().all(|(k, v)| match k.as_str() {
            "mode" => v.as_str().is_some_and(|s| ["rule", "global", "direct"].contains(&s)),
            "log-level" => v.as_str().is_some_and(|s| ["silent", "error", "warning", "info", "debug"].contains(&s)),
            _ => false,
        })
}

async fn websocket(ctx: &Ctx, req: &mut Request, path: &str, secret: &str, owner: crate::auth::SessionId) -> Response {
    let mut changed = ctx.app.mihomo.passes.changed.subscribe();
    let Some(pass) = ctx.app.mihomo.passes.key(req.headers(), ctx.app.auth.now()).filter(|(_, id)| *id == owner) else {
        return plain(401, "Open Zashboard from the router console");
    };
    let Some(lifetime) = req.extensions().get::<crate::httpd::ConnectionLifetime>().cloned() else {
        return plain(503, "Dashboard stream requires a live listener connection");
    };
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let Ok(permit) = SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(8))).clone().try_acquire_owned() else {
        return plain(503, "Too many dashboard streams");
    };
    let key = match req.headers().get("sec-websocket-key").and_then(|v| v.to_str().ok()) {
        Some(k) => k,
        None => return plain(400, "Missing WebSocket key"),
    };
    let Ok(Ok(mut upstream)) = tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(CONTROLLER)).await else {
        return plain(502, "Mihomo did not answer");
    };
    let handshake = format!("GET {path} HTTP/1.1\r\nHost: {CONTROLLER}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nAuthorization: Bearer {secret}\r\n\r\n");
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        upstream.write_all(handshake.as_bytes()).await?;
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
            head.push(upstream.read_u8().await?);
        }
        Ok::<_, std::io::Error>(head)
    })
    .await;
    let Ok(Ok(head)) = result else {
        return plain(502, "Mihomo WebSocket handshake failed");
    };
    let head = String::from_utf8_lossy(&head);
    if !head.starts_with("HTTP/1.1 101 ") {
        return plain(502, "Mihomo refused the WebSocket");
    }
    let accept = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("sec-websocket-accept").then_some(v.trim())
    });
    let Some(accept) = accept.and_then(|v| HeaderValue::from_str(v).ok()) else {
        return plain(502, "Invalid WebSocket handshake");
    };
    let upgrade = hyper::upgrade::on(req);
    let ctx = ctx.clone();
    tokio::spawn(lifetime.until_stopped(async move {
        let _permit = permit;
        let Ok(client) = upgrade.await else { return };
        let mut client = hyper_util::rt::TokioIo::new(client);
        let relay = tokio::io::copy_bidirectional(&mut client, &mut upstream);
        tokio::pin!(relay);
        loop {
            if !matches!(core_secret(&ctx).await, Ok(Some(_))) { break; }
            let Some(remaining) = ctx.app.mihomo.passes.remaining(pass, ctx.app.auth.now()) else { break };
            if !matches!(ctx.app.auth.touch_session(owner), SessionCheck::Live { .. }) { break; }
            tokio::select! {
                _ = &mut relay => break,
                _ = changed.changed() => {},
                _ = tokio::time::sleep(remaining.min(Duration::from_secs(15))) => {},
            }
        }
    }));
    let mut r = Response::new(Body::empty());
    *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    r.headers_mut().insert(CONNECTION, HeaderValue::from_static("upgrade"));
    r.headers_mut().insert(UPGRADE, HeaderValue::from_static("websocket"));
    r.headers_mut().insert("sec-websocket-accept", accept);
    r
}

async fn core_secret(ctx: &Ctx) -> Result<Option<String>, GateError> {
    let core = Arc::clone(&ctx.app.mihomo);
    ctx.gate.run(Lane::General, move || core.secret()).await
}

/// Zashboard is a trusted, separately version-pinned SPA. Give only its
/// entry page its own CSP; do not relax the agent's nonced page policy.
pub fn secure_dashboard(resp: &mut Response) {
    resp.headers_mut().insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(
        "default-src 'self'; script-src 'self' 'sha256-ZS0vkhcl113ly98eGGUNDhkTSpJH5poUYTIXHqXmIts='; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; worker-src 'self' blob:; base-uri 'self'; frame-ancestors 'none'; form-action 'self'"));
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_download_credentials_never_reach_dashboard() {
        let bytes = dashboard_config(br#"{"mode":"rule","secret":"core-key","authentication":["proxy-key"],"listeners":[{"users":[{"password":"private-download-key"}]}]}"#).unwrap();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),serde_json::json!({"mode":"rule"}));
        assert!(dashboard_config(b"[]").is_none());
    }

    #[test]
    fn dashboard_cannot_reload_core_config_or_open_listeners() {
        assert!(!allowed(&Method::PUT, "/configs"));
        assert!(!allowed(&Method::POST, "/upgrade"));
        assert!(!allowed(&Method::PUT, "/restart"));
        assert!(allowed(&Method::GET, "/storage/zashboard"));
        assert!(!allowed(&Method::PUT, "/storage/zashboard"));
        assert!(!safe_patch(br#"{"mode":"rule","external-controller":"0.0.0.0:9090"}"#));
        assert!(safe_patch(br#"{"mode":"rule"}"#));
        assert!(!allowed(&Method::GET, "/providers/proxies/../../configs"));
    }

    #[test]
    fn link_contains_no_credentials_and_points_to_same_origin_bridge() {
        assert_eq!(dashboard_link(), "/ui/mihomo/dashboard/#/proxies");
        assert!(!dashboard_link().contains('?'));
    }

    #[test]
    fn browser_token_queries_never_override_the_server_only_core_secret() {
        assert_eq!(bridge_query(Some("token=outdated&level=info&%74oken=another&limit=7")), "?level=info&limit=7");
        assert_eq!(bridge_query(Some("token=")), "");
        assert_eq!(bridge_query(None), "");
    }

    use crate::auth::{Kind, PeerKey};
    use crate::httpd::{app, Peer};
    use crate::mihomo::Mihomo;
    use std::net::SocketAddr;
    use tower::ServiceExt;

    const PW: &str = "test-password";
    const HOST: &str = "192.168.0.1:9090";

    fn at(secs: u64) -> BootInstant {
        BootInstant::from_duration(Duration::from_secs(secs))
    }

    fn cookie_header(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_str(value).unwrap());
        headers
    }

    fn token(letter: char) -> String {
        letter.to_string().repeat(64)
    }

    #[test]
    fn a_pass_belongs_to_its_session_and_expires() {
        let passes = Passes::default();
        let owner = SessionId::for_test(7);
        passes.issue(&token('a'), owner, at(10));
        let wanted = cookie_header(&format!("{PASS_COOKIE}={}", token('a')));
        assert_eq!(passes.owner(&wanted, at(11)), Some(owner));
        assert_eq!(passes.owner(&cookie_header(&format!("{PASS_COOKIE}={}", token('b'))), at(11)), None);
        assert_eq!(passes.owner(&HeaderMap::new(), at(11)), None);
        assert_eq!(passes.owner(&wanted, at(10 + PASS_LIFE.as_secs())), None, "twelve hours is the end");
    }

    #[test]
    fn a_new_pass_replaces_the_sessions_old_one_and_the_table_is_bounded() {
        let passes = Passes::default();
        passes.issue(&token('a'), SessionId::for_test(1), at(1));
        passes.issue(&token('b'), SessionId::for_test(1), at(2));
        assert_eq!(passes.owner(&cookie_header(&format!("{PASS_COOKIE}={}", token('a'))), at(3)), None);
        assert_eq!(passes.owner(&cookie_header(&format!("{PASS_COOKIE}={}", token('b'))), at(3)), Some(SessionId::for_test(1)));
        for n in 0..(PASS_CAP as u64 + 4) {
            passes.issue(&format!("{n:064x}"), SessionId::for_test(100 + n), at(10 + n));
        }
        assert!(passes.lock().len() <= PASS_CAP);
        assert_eq!(passes.owner(&cookie_header(&format!("{PASS_COOKIE}={:064x}", PASS_CAP as u64 + 3)), at(40)), Some(SessionId::for_test(100 + PASS_CAP as u64 + 3)));
    }

    #[test]
    fn stream_pass_identity_expires_and_is_revoked_on_replacement_eviction_and_logout() {
        let passes = Passes::default();
        let owner = SessionId::for_test(1);
        let key = (digest(&token('a')), owner);
        let mut changed = passes.changed.subscribe();
        passes.issue(&token('a'), owner, at(1));
        changed.borrow_and_update();
        assert_eq!(passes.remaining(key, at(2)), Some(PASS_LIFE - Duration::from_secs(1)));
        assert_eq!(passes.remaining(key, at(1 + PASS_LIFE.as_secs())), None);
        passes.issue(&token('b'), owner, at(3));
        assert!(changed.has_changed().unwrap());
        assert_eq!(passes.remaining(key, at(4)), None);
        let key = (digest(&token('b')), owner);
        passes.revoke(owner);
        assert_eq!(passes.remaining(key, at(4)), None);
        passes.issue(&token('a'), owner, at(5));
        for n in 0..PASS_CAP {
            passes.issue(&format!("{n:064x}"), SessionId::for_test(100 + n as u64), at(6));
        }
        assert_eq!(passes.remaining((digest(&token('a')), owner), at(7)), None);
    }

    #[tokio::test]
    async fn dashboard_state_lock_wait_never_blocks_the_runtime() {
        let rig = rig("runtime-lock", true, 9);
        let (_, cookie) = grant_pass(&rig, 9).await;
        for (method, uri) in [("GET", "/ui/mihomo/dashboard/assets/app.css"), ("POST", "/api/mihomo/dashboard")] {
            let core = Arc::clone(&rig.ctx.app.mihomo);
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let _held = core.inner.lock().unwrap();
                ready_tx.send(()).unwrap();
                let _ = release_rx.recv_timeout(Duration::from_secs(2));
            });
            ready_rx.recv().unwrap();
            let start = std::time::Instant::now();
            let req = request(method, uri, 9, 20).header("authorization", format!("Bearer {}", rig.bearer)).header("cookie", cookie.clone()).body(Body::empty()).unwrap();
            let timer = async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let elapsed = start.elapsed();
                let _ = release_tx.send(());
                assert!(elapsed < Duration::from_secs(1), "runtime waited for the state mutex: {elapsed:?}");
            };
            let (_, reply) = tokio::join!(timer, send(&rig.ctx, req));
            assert_eq!(reply.status, 200);
            worker.join().unwrap();
        }
    }

    #[test]
    fn only_well_formed_cookies_of_this_name_are_considered() {
        let good = token('c');
        let headers = cookie_header(&format!("other={good}; {PASS_COOKIE}=short; {PASS_COOKIE}={good}; {PASS_COOKIE}={}; zte_ui_session={good}", "G".repeat(64)));
        assert_eq!(pass_cookies(&headers), [good.as_str()]);
        let many = (0..20).map(|n| format!("{PASS_COOKIE}={n:064x}")).collect::<Vec<_>>().join("; ");
        assert_eq!(pass_cookies(&cookie_header(&many)).len(), PASS_COOKIES);
    }

    struct Rig {
        ctx: Ctx,
        bearer: String,
    }

    fn peer(test: u8, n: u8) -> Peer {
        Peer(SocketAddr::from(([10, 1, test, n], 5000)))
    }

    fn rig(name: &str, running: bool, test: u8) -> Rig {
        let mut state = AppState::new();
        state.auth.set_password(PW);
        if running {
            let root = std::env::temp_dir().join(format!("u60-dashboard-{name}-{}", std::process::id()));
            std::fs::create_dir_all(root.join("ui/assets")).unwrap();
            std::fs::write(root.join("ui/assets/app.css"), "body{}").unwrap();
            std::fs::write(root.join("ui/index.html"), "<html>not the pinned page</html>").unwrap();
            state.mihomo = Arc::new(Mihomo::for_bridge(root, Arc::clone(&state.network_lock), "private-core-key".into()));
        }
        let ctx = Ctx::new(Arc::new(state), 2);
        let bearer = ctx.app.auth.login(PW, Kind::Bearer, PeerKey::from(peer(test, 250).0), &[]).unwrap().token.expose().to_owned();
        Rig { ctx, bearer }
    }

    struct Reply {
        status: u16,
        headers: HeaderMap,
        body: String,
    }

    impl Reply {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers.get(name).and_then(|v| v.to_str().ok())
        }
    }

    async fn send(ctx: &Ctx, request: axum::http::Request<Body>) -> Reply {
        let resp = app(ctx.clone()).oneshot(request).await.unwrap();
        let (status, headers) = (resp.status().as_u16(), resp.headers().clone());
        let body = String::from_utf8_lossy(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).into_owned();
        Reply { status, headers, body }
    }

    fn request(method: &str, uri: &str, test: u8, n: u8) -> axum::http::request::Builder {
        axum::http::Request::builder().method(method).uri(uri).header("host", HOST).extension(peer(test, n))
    }

    async fn grant_pass(rig: &Rig, test: u8) -> (Reply, String) {
        let reply = send(&rig.ctx, request("POST", "/api/mihomo/dashboard", test, 1).header("authorization", format!("Bearer {}", rig.bearer)).header("origin", format!("http://{HOST}")).body(Body::empty()).unwrap()).await;
        let cookie = reply.header("set-cookie").and_then(|v| v.split(';').next()).unwrap_or("").to_owned();
        (reply, cookie)
    }

    #[tokio::test]
    async fn a_console_session_gets_a_pass_cookie_that_only_the_dashboard_path_receives() {
        let rig = rig("grant", true, 1);
        let (reply, cookie) = grant_pass(&rig, 1).await;
        assert_eq!(reply.status, 200);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&reply.body).unwrap(), json!({"ok": true, "data": {"url": "/ui/mihomo/dashboard/#/proxies"}}));
        let set = reply.header("set-cookie").unwrap();
        assert!(set.starts_with(&format!("{PASS_COOKIE}=")) && set.ends_with("; Path=/ui/mihomo/; HttpOnly; SameSite=Strict"), "{set}");
        assert_eq!(cookie.len(), PASS_COOKIE.len() + 1 + 64);
        assert!(!reply.body.contains(cookie.split('=').nth(1).unwrap()), "the pass is in the cookie only");
        assert_eq!(reply.header("cache-control"), Some("no-store"));
    }

    #[tokio::test]
    async fn the_pass_request_needs_a_live_console_login_and_a_running_core() {
        let rig_on = rig("refusals", true, 2);
        let no_token = send(&rig_on.ctx, request("POST", "/api/mihomo/dashboard", 2, 2).body(Body::empty()).unwrap()).await;
        assert_eq!(no_token.status, 401);
        assert!(no_token.header("set-cookie").is_none());
        let cross = send(&rig_on.ctx, request("POST", "/api/mihomo/dashboard", 2, 3).header("authorization", format!("Bearer {}", rig_on.bearer)).header("origin", "http://evil.invalid").body(Body::empty()).unwrap()).await;
        assert_eq!(cross.status, 403);
        assert!(cross.header("set-cookie").is_none());
        let get = send(&rig_on.ctx, request("GET", "/api/mihomo/dashboard", 2, 4).header("authorization", format!("Bearer {}", rig_on.bearer)).body(Body::empty()).unwrap()).await;
        assert_ne!(get.status, 200);

        let off = rig("off", false, 3);
        let (reply, cookie) = grant_pass(&off, 3).await;
        assert_eq!((reply.status, cookie.as_str()), (409, ""));
        assert!(reply.body.contains("Clash is off"));
    }

    #[tokio::test]
    async fn the_pass_opens_the_dashboard_and_keeps_the_bridge_restricted() {
        let rig = rig("serve", true, 4);
        let (_, cookie) = grant_pass(&rig, 4).await;
        let with = |method: &str, uri: &str, n: u8| request(method, uri, 4, n).header("cookie", cookie.clone());

        let css = send(&rig.ctx, with("GET", "/ui/mihomo/dashboard/assets/app.css", 2).body(Body::empty()).unwrap()).await;
        assert_eq!((css.status, css.body.as_str(), css.header("content-type")), (200, "body{}", Some("text/css; charset=utf-8")));
        assert_eq!(css.header("x-frame-options"), Some("DENY"));
        assert_eq!(css.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(css.header("cross-origin-resource-policy"), Some("same-origin"));
        let index = send(&rig.ctx, with("GET", "/ui/mihomo/dashboard/", 3).body(Body::empty()).unwrap()).await;
        assert_eq!(index.status, 503, "an unpinned dashboard page fails closed");
        assert!(index.body.contains("version mismatch"));
        assert_eq!(send(&rig.ctx, with("GET", "/ui/mihomo/dashboard", 4).body(Body::empty()).unwrap()).await.header("location"), Some("/ui/mihomo/dashboard/"));
        for escape in ["/ui/mihomo/dashboard/%2e%2e/secret", "/ui/mihomo/dashboard/..%2f..%2fsecret", "/ui/mihomo/dashboard/assets/..%5cx"] {
            assert_eq!(send(&rig.ctx, with("GET", escape, 5).body(Body::empty()).unwrap()).await.status, 404, "{escape}");
        }
        assert_eq!(send(&rig.ctx, with("GET", "/ui/mihomo/dashboard/sw.js", 6).body(Body::empty()).unwrap()).await.status, 404);
        assert_eq!(send(&rig.ctx, with("POST", "/ui/mihomo/dashboard/assets/app.css", 7).body(Body::empty()).unwrap()).await.status, 405);

        let same = format!("http://{HOST}");
        for (method, uri) in [("PUT", "/ui/mihomo/api/configs"), ("POST", "/ui/mihomo/api/upgrade"), ("PUT", "/ui/mihomo/api/restart"), ("GET", "/ui/mihomo/api/../configs")] {
            assert_eq!(send(&rig.ctx, with(method, uri, 8).header("origin", same.clone()).body(Body::empty()).unwrap()).await.status, 403, "{method} {uri}");
        }
        for origin in ["http://evil.invalid", "null"] {
            assert_eq!(send(&rig.ctx, with("GET", "/ui/mihomo/api/version", 9).header("origin", origin).body(Body::empty()).unwrap()).await.status, 403, "{origin}");
        }
        assert_eq!(send(&rig.ctx, with("GET", "/ui/mihomo/api/version", 10).body(Body::empty()).unwrap()).await.status, 403, "a read needs Origin or Referer too");
        assert_eq!(send(&rig.ctx, with("PATCH", "/ui/mihomo/api/configs", 11).header("origin", same).body(Body::from(r#"{"external-controller":"0.0.0.0:9090"}"#)).unwrap()).await.status, 403);
    }

    #[tokio::test]
    async fn without_a_live_pass_nothing_is_served_and_the_pass_is_no_console_credential() {
        let rig = rig("signed-out", true, 5);
        let (_, cookie) = grant_pass(&rig, 5).await;

        let page = send(&rig.ctx, request("GET", "/ui/mihomo/dashboard/", 5, 2).body(Body::empty()).unwrap()).await;
        assert_eq!((page.status, page.header("location")), (303, Some(SIGN_IN)));
        assert_eq!(send(&rig.ctx, request("GET", "/ui/mihomo/api/version", 5, 3).header("origin", format!("http://{HOST}")).body(Body::empty()).unwrap()).await.status, 401);
        let junk = send(&rig.ctx, request("GET", "/ui/mihomo/dashboard/assets/app.css", 5, 4).header("cookie", format!("{PASS_COOKIE}={}", token('f'))).body(Body::empty()).unwrap()).await;
        assert_eq!(junk.status, 303);
        let session_cookie = send(&rig.ctx, request("GET", "/ui/mihomo/dashboard/assets/app.css", 5, 5).header("cookie", format!("zte_ui_session={}", rig.bearer)).body(Body::empty()).unwrap()).await;
        assert_eq!(session_cookie.status, 303);
        let api = send(&rig.ctx, request("GET", "/api/device", 5, 6).header("cookie", cookie.clone()).body(Body::empty()).unwrap()).await;
        assert_eq!(api.status, 401, "the JSON API never reads cookies");
        let wrong_host = send(&rig.ctx, axum::http::Request::builder().uri("/ui/mihomo/dashboard/").header("host", "evil.invalid").header("cookie", cookie.clone()).extension(peer(5, 7)).body(Body::empty()).unwrap()).await;
        assert_eq!(wrong_host.status, 403);

        assert_eq!(send(&rig.ctx, request("GET", "/ui/mihomo/dashboard/assets/app.css", 5, 8).header("cookie", cookie.clone()).body(Body::empty()).unwrap()).await.status, 200);
        let logout = send(&rig.ctx, request("POST", "/api/auth/logout", 5, 10).header("authorization", format!("Bearer {}", rig.bearer)).header("origin", format!("http://{HOST}")).body(Body::empty()).unwrap()).await;
        assert_eq!(logout.status, 200);
        assert!(logout.header("set-cookie").unwrap().contains("Max-Age=0"));
        let after = send(&rig.ctx, request("GET", "/ui/mihomo/dashboard/assets/app.css", 5, 9).header("cookie", cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(after.status, 303, "signing out of the console ends the dashboard session");
    }
}
