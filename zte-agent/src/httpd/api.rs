//! The JSON API on the async listener: `server::precheck` decides, bodies are
//! read with a cap, and `server::route` runs on a gate lane. Replies keep the
//! exact envelope and `Content-Type: application/json` of the old listener.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, BodyDataStream};
use axum::extract::{Request, State};
use axum::http::header::{
    HeaderMap, HeaderValue, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, EXPECT, RETRY_AFTER,
    TRANSFER_ENCODING, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::StatusCode;
use axum::response::Response;
use futures_util::StreamExt;
use serde_json::Value;

use super::gate::{GateError, Lane};
use super::guard::ReqHeaders;
use super::{Ctx, Peer, DRAIN_MAX, DRAIN_WAIT};
use crate::auth::{PeerKey, SessionId};
use crate::events::hub::{Refused, SubscribeReq};
use crate::events::sse::{self, hello, JsonEmitter};
use crate::events::stream::{drive, Sessions, StreamEnv};
use crate::events::{Topic, TopicSet};
use crate::handlers;
use crate::lan_test;
use crate::server::{self, Encoded, Method, Pre, PreEnv, ReqInfo, LOGIN_BODY_MAX};

/// Fallback handler for every path.
pub async fn api_entry(State(ctx): State<Ctx>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let headers = &parts.headers;
    let method = Method::from(&parts.method);
    let path = parts.uri.path();
    let addr = parts.extensions.get::<Peer>().map(|p| p.0);
    let peer = addr.map_or(PeerKey::UNKNOWN, PeerKey::from);
    let pre = server::precheck(
        &ReqInfo {
            method,
            path,
            query: parts.uri.query(),
            bearer: header(headers, "authorization").and_then(|v| v.strip_prefix("Bearer ")),
            headers: req_headers(headers),
            loopback: addr.is_some_and(|a| a.ip().to_canonical().is_loopback()),
            secure: parts.extensions.get::<super::tls::Secure>().is_some(),
        },
        &PreEnv {
            auth: &ctx.app.auth,
            hosts: &ctx.hosts,
            ts_names: &|| ctx.ts.get(),
        },
    );

    match pre {
        Pre::Reply(status, v) => {
            finish_early(headers, body.into_data_stream(), 0, json_reply(status, &v)).await
        }
        Pre::Login => {
            let body = match read_capped(headers, body, LOGIN_BODY_MAX, ctx.body_wait).await {
                Ok(body) => body,
                Err(e) => return read_failed(headers, e).await,
            };
            let app = Arc::clone(&ctx.app);
            gated(
                ctx.gate
                    .run(Lane::General, move || server::encode(handlers::login(&app, &body, peer)))
                    .await,
            )
        }
        // LAN test: no gate (nothing blocks), its own stream limit.
        Pre::LanDownload { size } => match Arc::clone(&ctx.lan).try_acquire_owned() {
            Ok(permit) => {
                let mut resp = Response::new(lan_test::download_body(size, permit));
                let h = resp.headers_mut();
                h.insert(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
                h.insert(CONTENT_LENGTH, HeaderValue::from(size));
                resp
            }
            Err(_) => finish_early(headers, body.into_data_stream(), 0, busy()).await,
        },
        Pre::LanUpload => match Arc::clone(&ctx.lan).try_acquire_owned() {
            Ok(permit) => {
                server::encode(lan_test::upload(body, permit, ctx.body_wait).await).into_response()
            }
            Err(_) => finish_early(headers, body.into_data_stream(), 0, busy()).await,
        },
        // A stream: no gate, nothing blocks.
        Pre::Events { owner } => {
            let resp = events(&ctx, owner, parts.uri.query());
            finish_early(headers, body.into_data_stream(), 0, resp).await
        }
        Pre::Dashboard => {
            let token=header(headers,"authorization").and_then(|v|v.strip_prefix("Bearer ")).unwrap_or("").to_owned();
            let resp=crate::dashboard::response(Arc::clone(&ctx.app),parts.uri.to_string(),token);
            finish_early(headers,body.into_data_stream(),0,resp).await
        }
        Pre::Dispatch { body_limit } => {
            let body = match read_capped(headers, body, body_limit, ctx.body_wait).await {
                Ok(body) => body,
                Err(e) => return read_failed(headers, e).await,
            };
            // HEAD, OPTIONS, PATCH...: route() has no arm for them. Answered
            // here so a reachability probe never waits for a lane.
            if method == Method::Other {
                return json_reply(404, &server::error_body("not found"));
            }
            let lane = server::lane(method, path);
            let path = path.to_owned();
            let app = Arc::clone(&ctx.app);
            // Serialising (and dropping the Value) also happens on the pool.
            gated(
                ctx.gate
                    .run(lane, move || server::encode(server::route(&method, &path, &app, &body)))
                    .await,
            )
        }
    }
}

/// API stream keep-alive comment period; also bounds dead-peer detection.
pub const EVENTS_KEEPALIVE: Duration = Duration::from_secs(15);
/// Suggested wait before retrying a refused stream.
const EVENTS_RETRY_AFTER: u64 = 10;
/// An API stream ends with `bye lifetime` after this; the client reconnects.
pub const EVENTS_MAX_LIFE: Duration = Duration::from_secs(30 * 60);

/// `GET /api/events?topics=a,b[&signal_ms=N]`: JSON SSE (`docs/events.md`).
fn events(ctx: &Ctx, owner: SessionId, query: Option<&str>) -> Response {
    let (topics, signal_hint) = match events_query(query) {
        Ok(q) => q,
        Err(e) => return json_reply(400, &server::error_body(&e)),
    };
    let hub = &ctx.app.hub;
    // A reconnect replaces its predecessor at once (`SubscribeReq::replace_same`).
    let sub = match hub.subscribe(SubscribeReq { owner, topics, signal_hint, replace_same: Some(0) }) {
        Ok(sub) => sub,
        Err(Refused::Full) => {
            let mut resp = json_reply(503, &server::error_body("too many event streams"));
            resp.headers_mut().insert(RETRY_AFTER, HeaderValue::from(EVENTS_RETRY_AFTER));
            return resp;
        }
    };
    let cadence = hub.cadences(topics);
    let instance = crate::sim_identity::agent_instance();
    let em = JsonEmitter::new(hello(instance, topics, &cadence, EVENTS_KEEPALIVE));
    sse::response(drive(events_env(Arc::clone(&ctx.app.auth) as _, owner), sub, em))
}

/// Keep-alive comments and the lifetime of an API stream.
pub(crate) fn events_env(auth: Arc<dyn Sessions>, owner: SessionId) -> StreamEnv {
    StreamEnv {
        auth,
        owner,
        tick: EVENTS_KEEPALIVE,
        first_tick: EVENTS_KEEPALIVE,
        min_gap: Duration::ZERO,
        // A client that vanished behind a peer that keeps acknowledging
        // (`tailscaled`, a suspended app) samples for at most this long.
        max_life: Some(EVENTS_MAX_LIFE),
    }
}

/// `topics` (required; comma-separated, percent-decoded, trimmed,
/// de-duplicated; an unknown name is shown up to 32 characters) and
/// `signal_ms` (1000-10000); other parameters are ignored.
fn events_query(query: Option<&str>) -> Result<(TopicSet, Option<Duration>), String> {
    let mut topics = None;
    let mut signal_ms = None;
    for pair in query.unwrap_or("").split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "topics" if topics.is_none() => topics = Some(percent_decode(v)),
            "signal_ms" if signal_ms.is_none() => signal_ms = Some(v),
            _ => {}
        }
    }
    let mut set = TopicSet::default();
    for name in topics.as_deref().unwrap_or("").split(',').map(|n| n.trim_matches(' ')) {
        match Topic::from_name(name) {
            Some(t) => set.insert(t),
            None if name.is_empty() => {}
            None => {
                let end = name.char_indices().nth(32).map_or(name.len(), |(i, _)| i);
                return Err(format!("unknown topic: {}", &name[..end]));
            }
        }
    }
    if set.is_empty() {
        return Err("topics required".into());
    }
    let hint = match signal_ms.map(str::parse::<u64>) {
        None => None,
        Some(Ok(ms @ 1000..=10_000)) => Some(Duration::from_millis(ms)),
        Some(_) => return Err("signal_ms must be 1000-10000".into()),
    };
    Ok((set, hint))
}

/// `%XX` and `+` (space) in a query or form value; invalid escapes stay as they are.
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |i: usize| b.get(i).and_then(|&c| (c as char).to_digit(16));
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 2;
            }
            (b'+', ..) => out.push(b' '),
            (c, ..) => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Encoded {
    pub fn into_response(self) -> Response {
        let mut resp = Response::new(Body::from(self.body));
        *resp.status_mut() =
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let h = resp.headers_mut();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
        if let Some(secs) = self.retry_after {
            h.insert(RETRY_AFTER, HeaderValue::from(secs));
        }
        resp
    }
}

/// A small fixed reply, serialised on the current thread.
pub fn json_reply(status: u16, body: &Value) -> Response {
    Encoded { status, body: serde_json::to_vec(body).unwrap_or_default(), retry_after: None }
        .into_response()
}

fn busy() -> Response {
    let mut resp = json_reply(503, &server::error_body("agent busy, retry"));
    resp.headers_mut().insert(RETRY_AFTER, HeaderValue::from(1));
    resp
}

fn gated(r: Result<Encoded, GateError>) -> Response {
    match r {
        Ok(enc) => enc.into_response(),
        Err(GateError::Busy) => busy(),
        Err(GateError::Panicked) => json_reply(500, &server::error_body("internal error")),
    }
}

/// First value of a header, if it is visible ASCII.
pub(crate) fn header<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The headers the Host and Origin checks look at.
pub(crate) fn req_headers(headers: &HeaderMap) -> ReqHeaders<'_> {
    ReqHeaders {
        host: header(headers, "host"),
        origin: header(headers, "origin"),
        referer: header(headers, "referer"),
        sec_fetch_site: header(headers, "sec-fetch-site"),
    }
}

/// The declared body length; `None` if absent. hyper has already answered
/// 400 to anything but plain digits (`5, 5` included) and kept only the first
/// of repeated equal headers, so this is the length hyper reads.
fn content_length(headers: &HeaderMap) -> Option<u64> {
    header(headers, "content-length").and_then(|v| v.parse().ok())
}

pub(crate) enum ReadErr {
    /// Over the limit; `read` bytes were consumed from `rest`.
    TooLarge { read: u64, rest: BodyDataStream },
    /// The body ended early, the connection failed or the body did not
    /// arrive within the wait.
    Failed,
}

/// The whole body if it is at most `limit` bytes and arrives within `wait`.
/// A declared length over the limit is refused before reading anything;
/// otherwise bytes are counted as they arrive, so nothing is ever allocated
/// past the limit.
pub(crate) async fn read_capped(
    headers: &HeaderMap,
    body: Body,
    limit: usize,
    wait: Duration,
) -> Result<Vec<u8>, ReadErr> {
    let mut rest = body.into_data_stream();
    let declared = content_length(headers);
    if declared.is_some_and(|n| n > limit as u64) {
        return Err(ReadErr::TooLarge { read: 0, rest });
    }
    let mut buf = Vec::with_capacity(declared.unwrap_or(0) as usize);
    // Err(Some(n)): over the limit after n bytes; Err(None): failed.
    let read = async {
        while let Some(chunk) = rest.next().await {
            let chunk = chunk.map_err(|_| None)?;
            if buf.len() + chunk.len() > limit {
                return Err(Some((buf.len() + chunk.len()) as u64));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(())
    };
    match tokio::time::timeout(wait, read).await {
        Ok(Ok(())) => Ok(buf),
        Ok(Err(Some(read))) => Err(ReadErr::TooLarge { read, rest }),
        Ok(Err(None)) | Err(_) => Err(ReadErr::Failed),
    }
}

/// 413 for an over-long body (connection kept when the rest can be drained);
/// 400 when the body could not be read whole: a partial body is never
/// dispatched, and the rest of it is not waited for.
async fn read_failed(headers: &HeaderMap, e: ReadErr) -> Response {
    match e {
        ReadErr::TooLarge { read, rest } => {
            let resp = json_reply(413, &server::error_body("body too large"));
            finish_early(headers, rest, read, resp).await
        }
        ReadErr::Failed => close(json_reply(400, &server::error_body("incomplete body"))),
    }
}

/// For a reply that leaves (part of) the request body unread. hyper closes a
/// connection whose body was not read to the end, and the client may then
/// see a reset instead of the reply. So read and discard what is left if it
/// is small (`DRAIN_MAX` within `DRAIN_WAIT`) and keep the connection;
/// otherwise say `Connection: close`. Never allocates the declared length.
pub(crate) async fn finish_early(
    headers: &HeaderMap,
    mut rest: BodyDataStream,
    already_read: u64,
    resp: Response,
) -> Response {
    let declared = content_length(headers);
    if !headers.contains_key(TRANSFER_ENCODING) && declared.unwrap_or(0) == 0 {
        return resp;
    }
    // Do not invite a body we are going to refuse (hyper sends the
    // 100 Continue on the first read); one already read from was invited.
    if (headers.contains_key(EXPECT) && already_read == 0)
        || declared.is_some_and(|n| n.saturating_sub(already_read) > DRAIN_MAX)
    {
        return close(resp);
    }
    let drain = async {
        let mut drained = 0u64;
        while let Some(chunk) = rest.next().await {
            let Ok(chunk) = chunk else {
                return false;
            };
            drained += chunk.len() as u64;
            if drained > DRAIN_MAX {
                return false;
            }
        }
        true
    };
    match tokio::time::timeout(DRAIN_WAIT, drain).await {
        Ok(true) => resp,
        _ => close(resp),
    }
}

pub(crate) fn close(mut resp: Response) -> Response {
    resp.headers_mut().insert(CONNECTION, HeaderValue::from_static("close"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthConfig, AuthState, Kind, SeqRng};
    use crate::boottime::FakeClock;
    use crate::handlers::AppState;
    use crate::httpd::gate::Gate;
    use crate::httpd::{app, Ctx, Peer, BODY_WAIT, LAN_STREAMS};
    use axum::body::BodyDataStream;
    use axum::http::request::Builder;
    use serde_json::json;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tower::ServiceExt;

    const PW: &str = "test-password";
    const IP_HOST: &str = "192.168.0.1:9090";

    /// A distinct peer per test (third octet) and request (fourth).
    fn peer(test: u8, n: u8) -> Peer {
        Peer(SocketAddr::from(([10, 0, test, n], 5000)))
    }

    fn ctx(password: Option<&str>) -> Ctx {
        let state = AppState::new();
        if let Some(pw) = password {
            state.auth.set_password(pw);
        }
        Ctx::new(Arc::new(state), 2)
    }

    fn token(ctx: &Ctx, test: u8) -> String {
        let issued = ctx.app.auth.login(PW, Kind::Bearer, PeerKey::from(peer(test, 250).0), &[]);
        issued.unwrap().token.expose().to_owned()
    }

    fn req(method: &str, uri: &str, peer: Peer) -> Builder {
        axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", IP_HOST)
            .extension(peer)
    }

    struct Reply {
        status: u16,
        headers: HeaderMap,
        body: Vec<u8>,
    }

    impl Reply {
        fn json(&self) -> Value {
            serde_json::from_slice(&self.body).unwrap()
        }

        fn header(&self, name: &str) -> Option<&str> {
            self.headers.get(name).map(|v| v.to_str().unwrap())
        }

        /// Status, exact JSON and the exact old Content-Type.
        fn assert_json(&self, status: u16, expect: Value) {
            assert_eq!((self.status, self.json()), (status, expect));
            assert_eq!(self.header("content-type"), Some("application/json"));
            assert_eq!(self.header("content-length"), Some(self.body.len().to_string().as_str()));
            assert_eq!(self.header("x-content-type-options"), Some("nosniff"));
        }
    }

    async fn send(ctx: &Ctx, req: axum::http::Request<Body>) -> Reply {
        let resp = app(ctx.clone()).oneshot(req).await.unwrap();
        reply(resp).await
    }

    async fn reply(resp: Response) -> Reply {
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec();
        Reply { status, headers, body }
    }

    async fn get(ctx: &Ctx, uri: &str, peer: Peer) -> Reply {
        send(ctx, req("GET", uri, peer).body(Body::empty()).unwrap()).await
    }

    fn err(e: &str) -> Value {
        json!({"ok": false, "error": e})
    }

    #[tokio::test]
    async fn lan_ping_and_routing_without_password() {
        let c = ctx(None);
        for uri in ["/api/lan/ping", "/api/lan/ping?x=1"] {
            get(&c, uri, peer(1, 1)).await.assert_json(200, json!({"ok": true}));
        }
        // No 405 and no trailing-slash or HEAD-to-GET routing: always 404.
        for (method, uri) in [("DELETE", "/api/device"), ("GET", "/api/device/"), ("GET", "/nope"), ("PATCH", "/api/device")] {
            let r = send(&c, req(method, uri, peer(1, 2)).body(Body::empty()).unwrap()).await;
            r.assert_json(404, err("not found"));
        }
        get(&c, "/api/live-activity", peer(1, 3)).await.assert_json(403, err("agent password required"));
        let r = get(&c, "/api/esim/switch", peer(1, 4)).await;
        assert_eq!(r.status, 200);
        assert_eq!(r.json()["data"]["state"], "idle");
        assert_eq!(r.header("content-type"), Some("application/json"));
        let r = get(&c, "/api/doh/status", peer(1, 5)).await;
        assert_eq!((r.status, r.json()["ok"].as_bool()), (200, Some(true)));
    }

    #[tokio::test]
    async fn head_without_password_answered_without_a_gate_permit() {
        let mut c = ctx(None);
        // No permits at all: anything gated would wait 30 s.
        c.gate = Arc::new(Gate::new(0, 0, Duration::from_secs(30)));
        // `/` itself redirects to the web UI (no permit either).
        let r = send(&c, req("HEAD", "/", peer(2, 3)).body(Body::empty()).unwrap()).await;
        assert_eq!((r.status, r.header("location")), (303, Some("/ui/")));
        for (method, uri) in [("HEAD", "/api/device"), ("OPTIONS", "/api/device")] {
            let r = req(method, uri, peer(2, 1)).body(Body::empty()).unwrap();
            let r = tokio::time::timeout(Duration::from_secs(5), send(&c, r)).await.unwrap();
            assert_eq!(r.status, 404);
            assert_eq!(r.header("content-type"), Some("application/json"));
            let not_found = serde_json::to_vec(&err("not found")).unwrap();
            assert_eq!(not_found.len(), 32);
            if method == "HEAD" {
                // HEAD: the length of the body that is not sent.
                assert_eq!(r.header("content-length"), Some("32"));
                assert!(r.body.is_empty());
            } else {
                assert_eq!(r.body, not_found);
            }
        }
        // A gated call with no free permit: busy after the wait.
        c.gate = Arc::new(Gate::new(0, 0, Duration::from_millis(20)));
        let r = get(&c, "/api/doh/status", peer(2, 2)).await;
        r.assert_json(503, err("agent busy, retry"));
        assert_eq!(r.header("retry-after"), Some("1"));
    }

    #[tokio::test]
    async fn auth_with_password() {
        let c = ctx(Some(PW));
        let tok = token(&c, 3);
        get(&c, "/api/device", peer(3, 1)).await.assert_json(401, err("unauthorized"));
        let lower = req("GET", "/api/device", peer(3, 2)).header("authorization", format!("bearer {tok}"));
        send(&c, lower.body(Body::empty()).unwrap()).await.assert_json(401, err("unauthorized"));
        let r = send(&c, req("HEAD", "/api/device", peer(3, 3)).body(Body::empty()).unwrap()).await;
        assert_eq!(r.status, 401);
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.header("content-length"), Some(serde_json::to_vec(&err("unauthorized")).unwrap().len().to_string().as_str()));
        assert!(r.body.is_empty());
        let r = send(&c, req("OPTIONS", "/api/device", peer(3, 4)).body(Body::empty()).unwrap()).await;
        r.assert_json(401, err("unauthorized"));
        // The login path is exempt from the token check but has no GET.
        get(&c, "/api/auth/login", peer(3, 5)).await.assert_json(404, err("not found"));
        // With a token: the old routes answer.
        let ok = req("GET", "/api/doh/status", peer(3, 6)).header("authorization", format!("Bearer {tok}"));
        let r = send(&c, ok.body(Body::empty()).unwrap()).await;
        assert_eq!((r.status, r.json()["ok"].as_bool()), (200, Some(true)));
        let r = send(&c, req("HEAD", "/api/device", peer(3, 7)).header("authorization", format!("Bearer {tok}")).body(Body::empty()).unwrap()).await;
        assert_eq!((r.status, r.header("content-length")), (404, Some("32")));
        // `/` is the web UI's: a redirect, with or without a token.
        let r = send(&c, req("HEAD", "/", peer(3, 8)).body(Body::empty()).unwrap()).await;
        assert_eq!((r.status, r.header("location")), (303, Some("/ui/")));
    }

    fn login(test: u8, n: u8, body: &str) -> axum::http::Request<Body> {
        req("POST", "/api/auth/login", peer(test, n))
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    #[tokio::test]
    async fn login_replies() {
        let c = ctx(Some(PW));
        send(&c, login(4, 1, "{}")).await.assert_json(400, err("missing 'password' field"));
        send(&c, login(4, 2, "not json")).await.assert_json(400, err("invalid JSON"));
        send(&c, login(4, 3, r#"{"password":"wrong"}"#)).await.assert_json(401, err("invalid password"));
        let r = send(&c, login(4, 4, &format!(r#"{{"password":"{PW}"}}"#))).await;
        assert_eq!((r.status, r.header("content-type")), (200, Some("application/json")));
        let tok = r.json()["data"]["token"].as_str().unwrap().to_owned();
        assert!(tok.len() == 64 && tok.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert!(c.app.auth.validate(&tok));
        // Over the login cap.
        let big = format!(r#"{{"password":"{PW}"}}"#) + &" ".repeat(LOGIN_BODY_MAX);
        send(&c, login(4, 5, &big)).await.assert_json(413, err("body too large"));
    }

    #[tokio::test]
    async fn sixth_wrong_login_from_one_peer_is_429() {
        // A fake clock: the 6th try is inside the 1 s block however slow the host.
        let mut st = AppState::new();
        st.auth = Arc::new(AuthState::with_parts(FakeClock::at_secs(1000), Box::new(SeqRng::new()), AuthConfig::default()));
        st.auth.set_password(PW);
        let c = Ctx::new(Arc::new(st), 2);
        for _ in 0..5 {
            send(&c, login(5, 1, r#"{"password":"wrong"}"#)).await.assert_json(401, err("invalid password"));
        }
        let r = send(&c, login(5, 1, r#"{"password":"wrong"}"#)).await;
        r.assert_json(429, json!({"ok": false, "error": "too many login attempts", "retry_after": 1}));
        assert_eq!(r.header("retry-after"), Some("1"));
        // Another peer is not affected.
        let r = send(&c, login(5, 2, &format!(r#"{{"password":"{PW}"}}"#))).await;
        assert_eq!((r.status, r.header("retry-after")), (200, None));
    }

    #[tokio::test]
    async fn body_caps() {
        let c = ctx(Some(PW));
        let auth = format!("Bearer {}", token(&c, 6));
        let with_body = |method: &str, uri: &str, n: usize| {
            req(method, uri, peer(6, 1)).header("authorization", &auth).body(Body::from(vec![b'x'; n])).unwrap()
        };
        send(&c, with_body("PUT", "/api/live-activity", 4097)).await.assert_json(413, err("body too large"));
        send(&c, with_body("PUT", "/api/live-activity/nowhere", 4096)).await.assert_json(404, err("not found"));
        send(&c, with_body("POST", "/api/scheduler/jobs", 65_537)).await.assert_json(413, err("body too large"));
        send(&c, with_body("POST", "/api/nowhere", 65_536)).await.assert_json(404, err("not found"));
        // A declared length over the cap is refused without reading.
        let declared = req("POST", "/api/scheduler/jobs", peer(6, 2))
            .header("authorization", &auth)
            .header("content-length", "65537")
            .body(Body::from(vec![b'x'; 65_537]))
            .unwrap();
        let r = send(&c, declared).await;
        r.assert_json(413, err("body too large"));
        // Drained (small), so the connection may stay open.
        assert_eq!(r.header("connection"), None);
    }

    /// A body that never arrives, as hyper would hand it over.
    fn stalled_body() -> Body {
        let pending = futures_util::stream::pending::<Result<axum::body::Bytes, std::io::Error>>();
        Body::from_stream(pending)
    }

    // Paused clock: the waits below pass at once, and exactly.
    #[tokio::test(start_paused = true)]
    async fn unread_body_that_stalls_closes_after_drain_wait() {
        let c = ctx(Some(PW));
        let r = req("POST", "/api/device/reboot", peer(9, 1)).header("transfer-encoding", "chunked");
        let started = tokio::time::Instant::now();
        let r = send(&c, r.body(stalled_body()).unwrap()).await;
        r.assert_json(401, err("unauthorized"));
        assert_eq!(r.header("connection"), Some("close"));
        assert_eq!(started.elapsed(), DRAIN_WAIT);
    }

    #[tokio::test(start_paused = true)]
    async fn body_that_stalls_is_not_waited_for_past_body_wait() {
        let c = ctx(None);
        let started = tokio::time::Instant::now();
        let r = req("POST", "/api/nowhere", peer(9, 2)).header("content-length", "100");
        let r = send(&c, r.body(stalled_body()).unwrap()).await;
        r.assert_json(400, err("incomplete body"));
        assert_eq!(r.header("connection"), Some("close"));
        assert_eq!(started.elapsed(), BODY_WAIT);
        // A LAN upload counts what came before the pause.
        let first = futures_util::stream::iter([Ok::<_, std::io::Error>(axum::body::Bytes::from_static(&[0; 10]))]);
        let body = Body::from_stream(first.chain(futures_util::stream::pending()));
        let r = send(&c, req("POST", "/api/lan/upload", peer(9, 3)).body(body).unwrap()).await;
        assert_eq!((r.status, r.json()["data"]["bytes"].as_u64()), (200, Some(10)));
        assert_eq!(c.lan.available_permits(), LAN_STREAMS);
    }

    #[tokio::test]
    async fn lan_streams() {
        let c = ctx(None);
        let r = get(&c, "/api/lan/download?size=100000", peer(7, 1)).await;
        assert_eq!(r.status, 200);
        assert_eq!(r.header("content-type"), Some("application/octet-stream"));
        assert_eq!(r.header("content-length"), Some("100000"));
        assert_eq!(r.body.len(), 100_000);
        assert!(r.body.iter().all(|&b| b == 0));
        let r = get(&c, "/api/lan/download?size=0", peer(7, 1)).await;
        assert_eq!((r.status, r.header("content-length"), r.body.len()), (200, Some("0"), 0));

        let upload = |n: usize| req("POST", "/api/lan/upload", peer(7, 2)).body(Body::from(vec![7u8; n])).unwrap();
        let r = send(&c, upload(1 << 20)).await;
        assert_eq!((r.status, r.header("content-type")), (200, Some("application/json")));
        let v = r.json();
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["bytes"], 1_048_576);
        let mut keys: Vec<_> = v["data"].as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["bytes", "duration_secs", "mbps"]);

        // Two streams at once; a third is refused at once, then works again.
        let open = || async {
            let r = req("GET", "/api/lan/download?size=10000000", peer(7, 3)).body(Body::empty()).unwrap();
            app(c.clone()).oneshot(r).await.unwrap()
        };
        let first = open().await;
        let second = open().await;
        assert_eq!((first.status(), second.status()), (StatusCode::OK, StatusCode::OK));
        reply(open().await).await.assert_json(503, err("agent busy, retry"));
        send(&c, upload(10)).await.assert_json(503, err("agent busy, retry"));
        drop(first);
        assert_eq!(open().await.status(), StatusCode::OK);
        drop(second);
    }

    #[tokio::test]
    async fn host_and_origin_checks_without_password() {
        let c = ctx(None);
        let reboot = req("POST", "/api/device/reboot", peer(8, 1)).header("origin", "http://evil");
        send(&c, reboot.body(Body::empty()).unwrap()).await.assert_json(403, err("cross-origin request refused"));
        let evil = axum::http::Request::builder()
            .uri("/api/device")
            .header("host", "evil.example")
            .extension(peer(8, 2));
        send(&c, evil.body(Body::empty()).unwrap()).await.assert_json(403, err("host not allowed"));
        let no_host = axum::http::Request::builder().uri("/api/device").extension(peer(8, 3));
        send(&c, no_host.body(Body::empty()).unwrap()).await.assert_json(400, err("missing host"));
        // Names the device answers to pass, `localhost` only from itself.
        let local = |p: Peer| {
            axum::http::Request::builder().uri("/api/lan/ping").header("host", "localhost:9090").extension(p)
        };
        let lo = Peer(SocketAddr::from(([127, 0, 0, 1], 5000)));
        send(&c, local(lo).body(Body::empty()).unwrap()).await.assert_json(200, json!({"ok": true}));
        send(&c, local(peer(8, 4)).body(Body::empty()).unwrap()).await.assert_json(403, err("host not allowed"));
    }

    #[tokio::test]
    async fn rebound_browser_login_refused_with_password() {
        let c = ctx(Some(PW));
        let rebound = |n| {
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("host", "evil.example:9090")
                .header("origin", "http://evil.example:9090")
                .header("content-type", "application/json")
                .extension(peer(9, n))
                .body(Body::from(r#"{"password":"wrong"}"#))
                .unwrap()
        };
        for n in 0..40 {
            send(&c, rebound(n)).await.assert_json(403, err("host not allowed"));
        }
        // Nothing reached the limiter: an app (no Origin, any name) logs in.
        let app = axum::http::Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("host", "u60.example:9090")
            .extension(peer(9, 1));
        let r = send(&c, app.body(Body::from(format!(r#"{{"password":"{PW}"}}"#))).unwrap()).await;
        assert_eq!(r.status, 200);
    }

    #[test]
    fn lane_table() {
        use Method::{Delete, Get, Post, Put};
        let slow = [
            (Post, "/api/esim/enable"),
            (Get, "/api/esim/profiles"),
            (Get, "/api/esim/chip"),
            (Post, "/api/esim/nickname"),
            (Get, "/api/esim/notifications"),
            (Post, "/api/esim/notifications/process"),
            (Post, "/api/esim/delete"),
            (Post, "/api/esim/download"),
            (Post, "/api/tailscale/setup"),
            (Post, "/api/tailscale/login-url"),
            (Post, "/api/tailscale/enable"),
            (Post, "/api/tailscale/disable"),
            (Put, "/api/tailscale/config"),
            (Post, "/api/tailscale/logout"),
            (Post, "/api/tailscale/update"),
            (Post, "/api/ussd/send"),
            (Post, "/api/stk/select"),
            (Post, "/api/stk/respond"),
            (Post, "/api/at/send"),
            (Post, "/api/modem/online"),
            (Post, "/api/modem/airplane"),
            (Post, "/api/modem/scan"),
            (Post, "/api/modem/register"),
            (Post, "/api/sim/pin/verify"),
            (Post, "/api/sim/unlock"),
            (Post, "/api/cell/lock/nr"),
            (Post, "/api/cell/neighbors/scan"),
            (Post, "/api/sms/forward/test"),
            (Post, "/api/sms/forward/retry"),
            (Get, "/api/speedtest/servers"),
            (Post, "/api/speedtest/start"),
            (Post, "/api/doh/enable"),
            (Post, "/api/doh/disable"),
        ];
        for (m, p) in slow {
            assert_eq!(server::lane(m, p), Lane::Slow, "{m:?} {p}");
        }
        let general = [
            (Get, "/api/esim/switch"),
            (Get, "/api/esim/download"),
            (Delete, "/api/esim/download"),
            (Post, "/api/esim/download/confirm"),
            (Post, "/api/esim/download/cancel"),
            (Get, "/api/tailscale/status"),
            (Get, "/api/stk/menu"),
            (Get, "/api/stk/session"),
            (Get, "/api/call/status"),
            (Post, "/api/call/dial"),
            (Post, "/api/call/hangup"),
            (Post, "/api/call/answer"),
            (Post, "/api/call/dtmf"),
            (Post, "/api/call/mute"),
            (Get, "/api/sim/info"),
            (Get, "/api/cell/neighbors/nr"),
            (Get, "/api/at/port"),
            (Get, "/api/modem/scan/status"),
            (Put, "/api/modem/data"),
            (Post, "/api/sms/send"),
            (Get, "/api/speedtest/progress"),
            (Post, "/api/speedtest/stop"),
            (Get, "/api/doh/status"),
            (Get, "/api/device"),
            (Post, "/api/auth/login"),
        ];
        for (m, p) in general {
            assert_eq!(server::lane(m, p), Lane::General, "{m:?} {p}");
        }
    }

    // --- GET /api/events ---

    /// The next SSE frame, or None if nothing arrives within 5 s.
    async fn frame(b: &mut BodyDataStream) -> Option<String> {
        match tokio::time::timeout(Duration::from_secs(5), b.next()).await {
            Ok(Some(Ok(bytes))) => Some(String::from_utf8(bytes.to_vec()).unwrap()),
            _ => None,
        }
    }

    /// The `data:` JSON of a frame.
    fn data(frame: &str) -> Value {
        let line = frame.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
        serde_json::from_str(line).unwrap()
    }

    async fn open_events(c: &Ctx, uri: &str, tok: &str, peer: Peer) -> Response {
        let r = req("GET", uri, peer).header("authorization", format!("Bearer {tok}"));
        app(c.clone()).oneshot(r.body(Body::empty()).unwrap()).await.unwrap()
    }

    #[tokio::test]
    async fn events_refusals() {
        let c = ctx(Some(PW));
        let tok = token(&c, 20);
        get(&c, "/api/events?topics=sms", peer(20, 1)).await.assert_json(401, err("unauthorized"));
        let long = "x".repeat(40);
        let cases = [
            ("/api/events", "topics required"),
            ("/api/events?topics=", "topics required"),
            ("/api/events?topics=%20,%20&signal_ms=2000", "topics required"),
            ("/api/events?topic=sms", "topics required"),
            ("/api/events?topics=signal,wifi", "unknown topic: wifi"),
            ("/api/events?topics=signal&signal_ms=999", "signal_ms must be 1000-10000"),
            ("/api/events?topics=signal&signal_ms=10001", "signal_ms must be 1000-10000"),
            ("/api/events?topics=signal&signal_ms=2s", "signal_ms must be 1000-10000"),
        ];
        for (uri, e) in cases {
            reply(open_events(&c, uri, &tok, peer(20, 2)).await).await.assert_json(400, err(e));
        }
        let uri = format!("/api/events?topics={long}");
        let r = reply(open_events(&c, &uri, &tok, peer(20, 3)).await).await;
        r.assert_json(400, err(&format!("unknown topic: {}", &long[..32])));
        // Streams need a token: without a password there is none.
        let open = ctx(None);
        get(&open, "/api/events?topics=sms", peer(20, 4)).await.assert_json(403, err("agent password required"));
        assert_eq!(c.app.hub.demand(Topic::Sms), 0);
    }

    #[tokio::test]
    async fn events_hello_then_snapshot_then_changes() {
        let c = ctx(Some(PW));
        let tok = token(&c, 21);
        c.app.hub.set(Topic::PowerState, json!({"action": "mode_power_on", "parameter": "quicken"}));
        let uri = "/api/events?topics=sms%2Cpower_state,+sms,signal&signal_ms=5000&x=1";
        let resp = open_events(&c, uri, &tok, peer(21, 1)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        assert_eq!(resp.headers()["cache-control"], "no-cache");
        assert_eq!(c.app.hub.demand(Topic::Sms), 1);
        let mut b = resp.into_body().into_data_stream();
        let hello = frame(&mut b).await.unwrap();
        assert!(hello.starts_with("retry: 3000\nevent: hello\ndata: "), "{hello}");
        let h = data(&hello);
        assert_eq!(h["v"], 1);
        assert_eq!(h["agent_instance"], crate::sim_identity::agent_instance());
        assert_eq!(h["topics"], json!(["signal", "power_state", "sms"]));
        assert_eq!(h["cadence_ms"], json!({"signal": 5000}));
        assert_eq!(h["keepalive_ms"], 15000);
        // Snapshot: every subscribed topic that has a value (no signal read yet).
        let f = frame(&mut b).await.unwrap();
        assert!(f.starts_with("event: power_state\n"), "{f}");
        assert_eq!(data(&f)["data"], json!({"action": "mode_power_on", "parameter": "quicken"}));
        let f = frame(&mut b).await.unwrap();
        assert!(f.starts_with("event: sms\n"), "{f}");
        let d = data(&f);
        assert_eq!((d["data"].clone(), d["error"].clone()), (json!({"received": 0}), Value::Null));
        // Then only changes.
        c.app.hub.on_bus_event("zwrt_wms_status_event", &json!({"wms_status": "new_sms_received"}));
        let f = frame(&mut b).await.unwrap();
        assert_eq!(data(&f)["data"], json!({"received": 1}));
        assert!(data(&f)["seq"].as_u64() > d["seq"].as_u64());
        // The client goes away: its demand goes with it.
        drop(b);
        assert_eq!((c.app.hub.demand(Topic::Sms), c.app.hub.demand(Topic::Signal)), (0, 0));
    }

    #[tokio::test]
    async fn events_registry_full_is_503_with_retry_after() {
        let c = ctx(Some(PW));
        let mut open = Vec::new();
        for n in 0..crate::events::hub::MAX_STREAMS as u8 {
            let r = open_events(&c, "/api/events?topics=sms", &token(&c, 100 + n), peer(22, n)).await;
            assert_eq!(r.status(), StatusCode::OK);
            open.push(r);
        }
        let r = reply(open_events(&c, "/api/events?topics=sms", &token(&c, 22), peer(22, 99)).await).await;
        r.assert_json(503, err("too many event streams"));
        assert_eq!(r.header("retry-after"), Some("10"));
        drop(open.pop());
        let r = open_events(&c, "/api/events?topics=sms", &token(&c, 22), peer(22, 98)).await;
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn events_fourth_stream_of_a_token_replaces_the_oldest() {
        let c = ctx(Some(PW));
        let tok = token(&c, 23);
        let mut streams = Vec::new();
        for (n, topics) in ["sms", "sms,signal", "sms,battery", "sms,wan"].into_iter().enumerate() {
            let r = open_events(&c, &format!("/api/events?topics={topics}"), &tok, peer(23, n as u8)).await;
            assert_eq!(r.status(), StatusCode::OK, "newest wins, never 503");
            streams.push(r.into_body().into_data_stream());
        }
        let oldest = &mut streams[0];
        assert!(frame(oldest).await.unwrap().contains("event: hello"));
        assert!(frame(oldest).await.unwrap().starts_with("event: sms\n"));
        assert_eq!(frame(oldest).await.unwrap(), "event: bye\ndata: {\"reason\":\"evicted\"}\n\n");
        assert!(frame(oldest).await.is_none());
        let newest = &mut streams[3];
        assert!(frame(newest).await.unwrap().contains("event: hello"));
        assert!(frame(newest).await.unwrap().starts_with("event: sms\n"));
        assert_eq!(c.app.hub.demand(Topic::Sms), 3);
    }

    #[tokio::test]
    async fn events_reconnect_with_the_same_topics_replaces_the_old_stream_at_once() {
        let c = ctx(Some(PW));
        let tok = token(&c, 24);
        let uri = "/api/events?topics=signal,sms";
        let mut old = open_events(&c, uri, &tok, peer(24, 1)).await.into_body().into_data_stream();
        let other = open_events(&c, "/api/events?topics=sms", &tok, peer(24, 2)).await;
        // The old connection may be dead but unnoticed (a tailnet peer): the
        // reconnect does not wait for a fourth stream to end it.
        let new = open_events(&c, uri, &tok, peer(24, 3)).await;
        assert_eq!(new.status(), StatusCode::OK);
        assert!(frame(&mut old).await.unwrap().contains("event: hello"));
        assert!(frame(&mut old).await.unwrap().starts_with("event: sms\n"));
        assert_eq!(frame(&mut old).await.unwrap(), "event: bye\ndata: {\"reason\":\"evicted\"}\n\n");
        assert_eq!((c.app.hub.demand(Topic::Signal), c.app.hub.demand(Topic::Sms)), (1, 2));
        drop((other, new));
    }

    #[test]
    fn events_query_parsing() {
        let q = |s| events_query(Some(s));
        let set = |t: &[Topic]| t.iter().copied().collect::<TopicSet>();
        assert_eq!(q("topics=sms,signal,sms"), Ok((set(&[Topic::Sms, Topic::Signal]), None)));
        assert_eq!(q("topics=%73ms%2c%20wan&topics=battery"), Ok((set(&[Topic::Sms, Topic::Wan]), None)));
        assert_eq!(q("signal_ms=1000&topics=signal"), Ok((set(&[Topic::Signal]), Some(Duration::from_secs(1)))));
        assert_eq!(q("topics=signal&signal_ms=10000"), Ok((set(&[Topic::Signal]), Some(Duration::from_secs(10)))));
        assert_eq!(q("topics=sms,%"), Err("unknown topic: %".into()));
        assert_eq!(events_query(None), Err("topics required".into()));
        assert_eq!(percent_decode("a%2Cb+c%zz%4"), "a,b c%zz%4");
        assert_eq!(percent_decode("%e2%82%ac%ff"), "\u{20ac}\u{fffd}");
        // Not UTF-8: unknown, shown lossily.
        assert_eq!(q("topics=sms,%ff"), Err("unknown topic: \u{fffd}".into()));
    }

    #[test]
    fn gate_errors_map_to_json() {
        let busy = gated(Err(GateError::Busy));
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        let panicked = gated(Err(GateError::Panicked));
        assert_eq!(panicked.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = futures_util::FutureExt::now_or_never(axum::body::to_bytes(panicked.into_body(), 64));
        // serde_json sorts keys (as the old listener's replies did).
        assert_eq!(&body.unwrap().unwrap()[..], br#"{"error":"internal error","ok":false}"#);
    }

    #[test]
    fn encode_adds_retry_after_only_on_429() {
        let e = server::encode((429, json!({"ok": false, "retry_after": 7})));
        assert_eq!(e.retry_after, Some(7));
        let r = e.into_response();
        assert_eq!(r.headers()[RETRY_AFTER], "7");
        assert_eq!(r.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(server::encode((503, json!({"retry_after": 7}))).retry_after, None);
        assert_eq!(server::encode((200, json!({"ok": true}))).body, br#"{"ok":true}"#);
    }
}
