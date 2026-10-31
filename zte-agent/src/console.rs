//! The sole browser UI, with safe redirects for retired /ui/ bookmarks.
use crate::httpd::{api, Ctx, Peer};
use axum::http::{
    header::{HeaderValue, CACHE_CONTROL, CONTENT_TYPE},
    StatusCode,
};
use axum::{
    body::Body,
    extract::{Request, State},
    response::Response,
    routing::{any, get},
    Router,
};

pub fn router() -> Router<Ctx> {
    Router::new()
        .route("/", get(serve).fallback(api::api_entry))
        .route("/ui", any(serve))
        .route("/ui/", any(serve))
        .route("/ui/{*rest}", any(serve))
        .route("/console/setup", get(serve))
        .route("/console/manifest.webmanifest", get(serve))
        .route("/console", get(serve))
        .route("/console/", get(serve))
        .route("/console/a/{file}", get(serve))
}

async fn serve(State(ctx): State<Ctx>, req: Request) -> Response {
    let loopback = req
        .extensions()
        .get::<Peer>()
        .is_some_and(|peer| peer.0.ip().to_canonical().is_loopback());
    let secure = req
        .extensions()
        .get::<crate::httpd::tls::Secure>()
        .is_some();
    if !api::header(req.headers(), "host").is_some_and(|host| {
        ctx.hosts
            .allows_via(host, loopback, secure, &|| ctx.ts.get())
    }) {
        let mut reply = Response::new(Body::from("Host not allowed"));
        *reply.status_mut() = StatusCode::FORBIDDEN;
        return reply;
    }
    let path = req.uri().path();
    if path.starts_with("/ui")
        && req.method() != axum::http::Method::GET
        && req.method() != axum::http::Method::HEAD
    {
        let mut reply = Response::new(Body::from(
            "This interface has been retired. Open /console/. Nothing was changed.",
        ));
        *reply.status_mut() = StatusCode::GONE;
        reply
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        let (parts, body) = req.into_parts();
        return api::finish_early(&parts.headers, body.into_data_stream(), 0, reply).await;
    }
    if path == "/console/manifest.webmanifest" || path == "/ui/manifest.webmanifest" {
        return manifest();
    }
    if let Some(reply) = certificate_download(&ctx, req.method(), path) {
        return reply;
    }
    let icon: Option<&'static [u8]> = match path {
        "/console/a/icon-180.png" => Some(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/console-icon-180.png"
        ))),
        "/console/a/icon-192.png" => Some(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/console-icon-192.png"
        ))),
        "/console/a/icon-512.png" => Some(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/console-icon-512.png"
        ))),
        _ => None,
    };
    if let Some(icon) = icon {
        let mut reply = Response::new(Body::from(icon));
        reply
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("image/png"));
        reply
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return reply;
    }
    if path == "/" || path == "/ui" || path.starts_with("/ui/") {
        if path.starts_with("/ui/a/") || path.starts_with("/ui/live/") {
            let mut reply = Response::new(Body::from(
                "This interface has been retired. Open /console/.",
            ));
            *reply.status_mut() = StatusCode::GONE;
            reply
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            return reply;
        }
        return redirect(legacy_destination(path));
    }
    if path == "/console/setup" && ctx.app.auth.has_password() {
        return redirect("/console/");
    }
    let Some((mime, body)) = asset(req.uri().path()) else {
        let mut reply = Response::new(Body::from("Not found"));
        *reply.status_mut() = StatusCode::NOT_FOUND;
        return reply;
    };
    if req.uri().path().trim_end_matches('/') == "/console" && !ctx.app.auth.has_password() {
        return redirect("/console/setup");
    }
    let mut reply = Response::new(Body::from(body));
    let headers = reply.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(mime));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    for (name,value) in [("x-content-type-options","nosniff"),("referrer-policy","no-referrer"),("content-security-policy","default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'; manifest-src 'self'")] {headers.insert(name,HeaderValue::from_static(value));}
    reply
}

fn redirect(to: &'static str) -> Response {
    let mut reply = Response::new(Body::empty());
    *reply.status_mut() = StatusCode::SEE_OTHER;
    reply
        .headers_mut()
        .insert(axum::http::header::LOCATION, HeaderValue::from_static(to));
    reply
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    reply
}

fn legacy_destination(path: &str) -> &'static str {
    let route = path.strip_prefix("/ui/").unwrap_or("");
    if route == "battery" {
        "/console/#tools/battery"
    } else if route.starts_with("settings/https") || route == "settings/install" {
        "/console/#settings/https"
    } else if route.starts_with("settings/theme") {
        "/console/#settings/appearance"
    } else if route.starts_with("settings") || route == "logout" {
        "/console/#settings/connection"
    } else if route.starts_with("sms/forward") || route.starts_with("tools/sms-forward") {
        "/console/#sms/forwarding"
    } else if route.starts_with("sms/new") {
        "/console/#sms/compose"
    } else if route.starts_with("sms") {
        "/console/#sms/inbox"
    } else if route.starts_with("router/esim") {
        "/console/#network/esim"
    } else if route.starts_with("router/apn") {
        "/console/#network/apn"
    } else if route.starts_with("router/lan") {
        "/console/#network/lan"
    } else if route.starts_with("router/firewall") {
        "/console/#network/firewall"
    } else if route.starts_with("router/qos") {
        "/console/#network/qos"
    } else if route.starts_with("router/wifi") || route.starts_with("wifi") {
        "/console/#network/wifi"
    } else if route.starts_with("router/device") {
        "/console/#settings/device"
    } else if route.starts_with("router/cell") || route.starts_with("tools/band") {
        "/console/#network/bands"
    } else if route.starts_with("router") || route.starts_with("network-mode") {
        "/console/#network/mobile"
    } else if route.starts_with("tools/at") {
        "/console/#tools/terminal"
    } else if route.starts_with("tools/speedtest") {
        "/console/#tools/speedtest"
    } else if route.starts_with("tools/devices") {
        "/console/#network/clients"
    } else if route.starts_with("tools") {
        "/console/#tools/device"
    } else if route.starts_with("signal") {
        "/console/#network/signal"
    } else if route.starts_with("traffic") {
        "/console/#tools/traffic"
    } else if ["health", "cpu", "processes"]
        .iter()
        .any(|prefix| route.starts_with(prefix))
    {
        "/console/#tools/diagnostics"
    } else {
        "/console/"
    }
}

fn manifest() -> Response {
    let body = serde_json::json!({"name":"OpenU60","short_name":"OpenU60","id":"/ui/","start_url":"/console/","scope":"/","display":"standalone","background_color":"#f3f5f8","theme_color":"#f3f5f8","icons":[{"src":"/console/a/icon-192.png","sizes":"192x192","type":"image/png"},{"src":"/console/a/icon-512.png","sizes":"512x512","type":"image/png","purpose":"any maskable"}]}).to_string();
    let mut reply = Response::new(Body::from(body));
    reply.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/manifest+json"),
    );
    reply
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    reply
}

use crate::tls::{DOWNLOAD_CRT, DOWNLOAD_PROFILE};
fn plain(status: u16, text: &'static str) -> Response {
    let mut resp = Response::new(Body::from(text));
    *resp.status_mut() = axum::http::StatusCode::from_u16(status).unwrap_or_default();
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    resp
}

/// The public downloads (static files: no session, no nonce): the file
/// while HTTPS is on, else plain-text 404; 405 for anything but GET and
/// HEAD. `None` for any other path.
fn certificate_download(ctx: &Ctx, method: &axum::http::Method, path: &str) -> Option<Response> {
    let crt = match path {
        DOWNLOAD_CRT => true,
        DOWNLOAD_PROFILE => false,
        _ => return None,
    };
    if method != axum::http::Method::GET && method != axum::http::Method::HEAD {
        return Some(plain(405, "Method not allowed"));
    }
    let file = if crt {
        ctx.app
            .tls
            .ca_der()
            .map(|der| (der, "application/x-x509-ca-cert", "u60-pro-ca.crt"))
    } else {
        ctx.app.tls.mobileconfig().map(|p| {
            (
                p.into_bytes(),
                "application/x-apple-aspen-config",
                "u60-pro-ca.mobileconfig",
            )
        })
    };
    let Some((body, ct, file)) = file else {
        return Some(plain(404, "Not found"));
    };
    let mut resp = Response::new(Body::from(body));
    let h = resp.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static(ct));
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{file}\"")) {
        h.insert(axum::http::header::CONTENT_DISPOSITION, v);
    }
    Some(resp)
}

fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    match path {
        "/console/setup" => Some(("text/html; charset=utf-8", include_str!("../ui/setup.html"))),
        "/console/a/https.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/https.js"),
        )),
        "/console" | "/console/" => {
            Some(("text/html; charset=utf-8", include_str!("../ui/index.html")))
        }
        "/console/a/dashboard.css" => Some((
            "text/css; charset=utf-8",
            include_str!("../ui/dashboard.css"),
        )),
        "/console/a/numbers.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/numbers.js"),
        )),
        "/console/a/motion.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/motion.js"),
        )),
        "/console/a/addresses.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/addresses.js"),
        )),
        "/console/a/esim-model.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/esim-model.js"),
        )),
        "/console/a/esim.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/esim.js"),
        )),
        "/console/a/clients-model.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/clients-model.js"),
        )),
        "/console/a/data-usage-model.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/data-usage-model.js"),
        )),
        "/console/a/data-usage.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/data-usage.js"),
        )),
        "/console/a/clients.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/clients.js"),
        )),
        "/console/a/routing-model.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/routing-model.js"),
        )),
        "/console/a/routing.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/routing.js"),
        )),
        "/console/a/advanced.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/advanced.js"),
        )),
        "/console/a/operators.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/operators.js"),
        )),
        "/console/a/forwarding.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/forwarding.js"),
        )),
        "/console/a/battery-model.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/battery-model.js"),
        )),
        "/console/a/battery.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/battery.js"),
        )),
        "/console/a/diagnostics.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/diagnostics.js"),
        )),
        "/console/a/actions.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/actions.js"),
        )),
        "/console/a/controls.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/controls.js"),
        )),
        "/console/a/appearance.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/appearance.js"),
        )),
        "/console/a/navigation.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/navigation.js"),
        )),
        "/console/a/cellular.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/cellular.js"),
        )),
        "/console/a/dashboard.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../ui/dashboard.js"),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::AppState;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn console_keeps_existing_ui_and_requires_bearer_for_live_data() {
        let app = Arc::new(AppState::new());
        app.auth.set_password("fixture-only");
        let router = crate::httpd::app(Ctx::new(app, 2));
        for (path, status) in [
            ("/console/", 200),
            ("/console/a/motion.js", 200),
            ("/console/a/data-usage.js", 200),
            ("/ui/login", 303),
            ("/api/dashboard/events", 401),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("host", "192.168.0.1:9090")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status, "{path}");
            if path == "/console/" {
                assert!(response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("frame-ancestors 'none'"));
            }
        }
    }

    #[tokio::test]
    async fn console_refuses_untrusted_hosts_and_preserves_no_password_setup() {
        let router = crate::httpd::app(Ctx::new(Arc::new(AppState::new()), 2));
        for (host, status) in [("rebound.example", 403), ("192.168.0.1:9090", 303)] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/console/")
                        .header("host", host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
            if status == 303 {
                assert_eq!(response.headers()["location"], "/console/setup");
            }
        }
    }
    #[tokio::test]
    async fn production_retires_old_pages_actions_assets_and_streams() {
        let state = Arc::new(AppState::new());
        state.auth.set_password("fixture-only");
        let app = crate::httpd::app(Ctx::new(state, 2));
        for (path, to) in [
            ("/", "/console/"),
            ("/ui/", "/console/"),
            (
                "/ui/login?next=https://evil.example&token=private",
                "/console/",
            ),
            ("/ui/router/esim?iccid=private", "/console/#network/esim"),
            ("/ui/battery", "/console/#tools/battery"),
            ("/ui/settings/https", "/console/#settings/https"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("host", "192.168.0.1:9090")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SEE_OTHER, "{path}");
            assert_eq!(response.headers()["location"], to);
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
        for (method, path) in [
            ("POST", "/ui/router/esim/switch"),
            ("POST", "/ui/login"),
            ("GET", "/ui/live/dashboard"),
            ("GET", "/ui/a/datastar-1.0.4.js"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("host", "192.168.0.1:9090")
                        .body(Body::from("old-form=private"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::GONE, "{method} {path}");
        }
    }

    #[tokio::test]
    async fn install_shortcuts_launch_the_console_and_setup_does_not_loop() {
        let app = crate::httpd::app(Ctx::new(Arc::new(AppState::new()), 2));
        for path in ["/ui/manifest.webmanifest", "/console/manifest.webmanifest"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("host", "192.168.0.1:9090")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(manifest["id"], "/ui/");
            assert_eq!(manifest["start_url"], "/console/");
            assert_eq!(manifest["scope"], "/");
        }
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/console/setup")
                    .header("host", "192.168.0.1:9090")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("Set an agent password"));
    }

    #[tokio::test]
    async fn replacement_https_controls_require_bearer_origin_and_explicit_confirmation() {
        use crate::auth::{Kind, PeerKey};
        let world = crate::tls::tests::World::new("console-https-api");
        let mut state = AppState::new();
        state.auth.set_password("fixture-only");
        state.tls = Arc::new(world.tls());
        let token = state
            .auth
            .login("fixture-only", Kind::Bearer, PeerKey::UNKNOWN, &[])
            .unwrap()
            .token
            .expose()
            .to_owned();
        let state = Arc::new(state);
        let app = crate::httpd::app(Ctx::new(Arc::clone(&state), 2));
        for (auth, origin, body, status) in [
            (false, "http://192.168.0.1:9090", "{\"confirm\":true}", 401),
            (true, "http://evil.example", "{\"confirm\":true}", 403),
            (true, "http://192.168.0.1:9090", "{\"confirm\":false}", 400),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/https/on")
                .header("host", "192.168.0.1:9090")
                .header("origin", origin)
                .header("content-type", "application/json");
            if auth {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::from(body)).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
            assert!(!state.tls.is_on());
        }
        for action in ["on", "renew", "reset"] {
            let path = format!("/api/https/{action}");
            assert_eq!(
                crate::server::lane(crate::server::Method::Post, &path),
                crate::httpd::gate::Lane::Slow
            );
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(&path)
                        .header("host", "192.168.0.1:9090")
                        .header("origin", "http://192.168.0.1:9090")
                        .header("authorization", format!("Bearer {token}"))
                        .header("content-type", "application/json")
                        .body(Body::from("{\"confirm\":true}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{action}");
            assert_eq!(state.tls.is_on(), action != "reset");
            if action == "on" {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(crate::tls::DOWNLOAD_CRT)
                            .header("host", "192.168.0.1:9090")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.headers()["cache-control"], "no-store");
                let bytes = axum::body::to_bytes(response.into_body(), 32768)
                    .await
                    .unwrap();
                assert_eq!(bytes.as_ref(), state.tls.ca_der().unwrap());
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri("/console/a/issuing.key")
                            .header("host", "192.168.0.1:9090")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::NOT_FOUND);
            }
        }
    }
}
