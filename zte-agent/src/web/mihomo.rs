//! The retired maud UI's Zashboard entry (compiled for tests only): a cookie
//! session in front of the shared dashboard and bridge in `crate::mihomo::dashboard`.

use axum::extract::Request;
use axum::http::Method;
use axum::response::Response;

use super::session;
use crate::httpd::Ctx;
use crate::mihomo::{API, DASHBOARD};

pub use crate::mihomo::dashboard::{dashboard_link, secure_dashboard};

pub fn is_path(path: &str) -> bool {
    path == DASHBOARD.trim_end_matches('/') || path.starts_with(DASHBOARD) || path == API || path.starts_with(&format!("{API}/"))
}

pub async fn dispatch(ctx: Ctx, req: Request) -> Response {
    let path = req.uri().path().to_owned();
    let Some(p) = session::find_session(&ctx.app.auth, req.headers()) else {
        let (parts, body) = req.into_parts();
        let response = if path.starts_with(DASHBOARD) && matches!(parts.method, Method::GET | Method::HEAD) {
            session::login_redirect(&Method::GET, "/ui/router/mihomo", None)
        } else {
            let mut reply = Response::new(axum::body::Body::from("Sign in to the router first"));
            *reply.status_mut() = axum::http::StatusCode::UNAUTHORIZED;
            reply
        };
        return session::reply_unread(&parts.headers, body, response).await;
    };
    crate::mihomo::dashboard::serve(ctx, req, p.id).await
}
