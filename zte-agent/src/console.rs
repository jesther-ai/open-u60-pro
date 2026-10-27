//! Responsive client console. The existing server-rendered UI remains at /ui/.
use crate::httpd::{api, Ctx, Peer};
use axum::http::{
    header::{HeaderValue, CACHE_CONTROL, CONTENT_TYPE},
    StatusCode,
};
use axum::{
    body::Body,
    extract::{Request, State},
    response::Response,
    routing::get,
    Router,
};

pub fn router() -> Router<Ctx> {
    Router::new()
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
    let Some((mime, body)) = asset(req.uri().path()) else {
        let mut reply = Response::new(Body::from("Not found"));
        *reply.status_mut() = StatusCode::NOT_FOUND;
        return reply;
    };
    if req.uri().path().trim_end_matches('/') == "/console" && !ctx.app.auth.has_password() {
        return crate::web::session::see_other("/ui/");
    }
    let mut reply = Response::new(Body::from(body));
    let headers = reply.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(mime));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    for (name,value) in [("x-content-type-options","nosniff"),("referrer-policy","no-referrer"),("content-security-policy","default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'")] {headers.insert(name,HeaderValue::from_static(value));}
    reply
}

fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    match path {
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
            ("/ui/login", 200),
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
                assert_eq!(response.headers()["location"], "/ui/");
            }
        }
    }
}
