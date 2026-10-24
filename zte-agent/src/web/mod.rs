//! The browser UI under `/ui/`: server-rendered HTML that works without
//! JavaScript, on the same listener as the JSON API.
//!
//! Every `/`, `/ui` and `/ui/**` request first passes one middleware
//! (`guard`), which in this order:
//! 1. checks the Host allowlist (DNS-rebinding defence);
//! 2. gives HTML pages a CSP nonce (request extension `CspNonce`);
//! 3. without an agent password, answers every page with how to set one
//!    (assets and the manifest still load);
//! 4. puts the security headers on every response it returns (and
//!    `Cache-Control: no-store` + the CSP with the same nonce on HTML),
//!    and draws HTML pages in the browser's theme (`prefs::themed`).
//!
//! Then sessions (`session::Session`), CSRF (`session::verify_post`) and the
//! gate for anything that reads the device. Every screen, action,
//! operation page and live stream is in the route table (`routes`), behind
//! `/ui/` and `/ui/{*rest}`; unknown `/ui/*` paths get the HTML 404 page
//! there instead of falling through to the JSON API.
//!
//! Screens and actions are data (`screen`, `action`, the tables in `tabs`),
//! drawn with the shared primitives in `kit`; `pages` holds the renderers
//! that are not tables (dashboard cards, signal panels, login, progress).

pub mod action;
pub mod assets;
pub mod bands;
pub mod fmt;
pub mod forward;
pub mod job;
pub mod kit;
pub mod live;
pub mod ops;
pub mod pages;
pub mod prefs;
pub mod qr;
pub mod qr_decode;
pub mod render;
pub mod roll;
pub mod routes;
pub mod screen;
pub mod session;
pub mod signal;
pub mod sms;
pub mod store;
pub mod tabs;
pub mod upload;
#[cfg(test)]
mod tests;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{
    HeaderValue, CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::StatusCode;
use axum::middleware::{from_fn_with_state, Next};
use axum::response::Response;
use axum::routing::{any, get};
use axum::Router;

use crate::httpd::{api, Ctx, Peer};

/// This response's CSP nonce (request extension set by `guard`).
#[derive(Clone)]
pub struct CspNonce(pub String);

impl CspNonce {
    /// The nonce of `req`; empty only for paths the middleware gives none
    /// (assets), which render no HTML.
    pub fn of<B>(req: &axum::http::Request<B>) -> String {
        req.extensions().get::<CspNonce>().map(|n| n.0.clone()).unwrap_or_default()
    }
}

pub fn router(ctx: &Ctx) -> Router<Ctx> {
    Router::new()
        // Anything but GET/HEAD on `/` is the API's (401/404 JSON as before).
        .route("/", get(to_ui).fallback(api::api_entry))
        .route("/ui", get(to_ui))
        .route("/ui/", get(routes::dispatch))
        .route("/ui/login", get(pages::login::form).post(pages::login::submit))
        .route("/ui/logout", get(pages::login::logout_form).post(pages::login::logout))
        .route("/ui/a/{file}", get(assets::asset))
        .route("/ui/manifest.webmanifest", get(assets::manifest))
        // Every other page, action, live stream and the 404 page.
        .route("/ui/{*rest}", any(routes::dispatch))
        .layer(from_fn_with_state(ctx.clone(), guard))
}

async fn to_ui() -> Response {
    session::see_other("/ui/")
}

/// Static files (the paths of the asset and manifest routes): no nonce, no
/// password needed. `/ui/a/` itself and deeper `/ui/a/…` paths are the 404
/// page's.
fn is_static(path: &str) -> bool {
    path.strip_prefix("/ui/a/").is_some_and(|file| !file.is_empty() && !file.contains('/'))
        || path == "/ui/manifest.webmanifest"
        || path == crate::tls::DOWNLOAD_CRT
        || path == crate::tls::DOWNLOAD_PROFILE
}

/// Paths that may render HTML: everything under `/ui` but static files.
fn is_page(path: &str) -> bool {
    path.starts_with("/ui") && !is_static(path)
}

fn text(status: StatusCode, msg: &'static str) -> Response {
    let mut resp = Response::new(Body::from(msg));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    resp
}

/// Host check, CSP nonce, no-password page and security headers (one layer
/// instead of three: the same order, less generic code).
async fn guard(State(ctx): State<Ctx>, mut req: Request, next: Next) -> Response {
    let page = is_page(req.uri().path());
    // This browser's theme (Settings › Appearance) for its pages.
    let theme = if page { prefs::Prefs::of(req.headers()).theme } else { prefs::Theme::System };
    let loopback = req.extensions().get::<Peer>().is_some_and(|p| p.0.ip().to_canonical().is_loopback());
    let over_tls = req.extensions().get::<crate::httpd::tls::Secure>().is_some();
    match api::header(req.headers(), "host") {
        None => return early(req, text(StatusCode::BAD_REQUEST, "Missing Host"), None).await,
        Some(h) if !ctx.hosts.allows_via(h, loopback, over_tls, &|| ctx.ts.get()) => {
            return early(req, text(StatusCode::FORBIDDEN, "Host not allowed"), None).await
        }
        Some(_) => {}
    }
    let nonce = if page {
        match ctx.app.auth.random_hex(16) {
            Ok(n) => Some(n),
            Err(_) => return early(req, text(StatusCode::SERVICE_UNAVAILABLE, "Entropy unavailable"), None).await,
        }
    } else {
        None
    };
    let resp = match &nonce {
        Some(n) if !ctx.app.auth.has_password() => {
            let resp = prefs::themed(render::no_password(n), theme, n).await;
            return early(req, resp, Some(n)).await;
        }
        Some(n) => {
            req.extensions_mut().insert(CspNonce(n.clone()));
            next.run(req).await
        }
        None => next.run(req).await,
    };
    secure(prefs::themed(resp, theme, nonce.as_deref().unwrap_or("")).await, nonce.as_deref())
}

/// A reply that leaves the request body unread (drained if small, else the
/// connection closes; see `api::finish_early`).
async fn early(req: Request, resp: Response, nonce: Option<&str>) -> Response {
    let (parts, body) = req.into_parts();
    secure(api::finish_early(&parts.headers, body.into_data_stream(), 0, resp).await, nonce)
}

/// Security headers on every response; HTML and live streams (device
/// data) are never stored, and HTML gets the CSP with this response's nonce.
/// Nothing under `/ui` may be embedded by another origin (ZTE's own UI on
/// port 80 is the same site, so `SameSite` cookies would go along).
fn secure(mut resp: Response, nonce: Option<&str>) -> Response {
    let ct = session::content_type(&resp).unwrap_or("");
    let (html, stream) = (ct.starts_with("text/html"), ct.starts_with("text/event-stream"));
    let h = resp.headers_mut();
    h.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    h.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    if html || stream {
        h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if html {
        h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
        if let Ok(csp) = HeaderValue::from_str(&csp(nonce.unwrap_or(""))) {
            h.insert(CONTENT_SECURITY_POLICY, csp);
        }
    }
    resp
}

/// Scripts by nonce only: the Datastar tag, the System theme's script tag
/// (`assets/theme.js`) and the scripts Datastar itself adds for `data-*`
/// expressions carry it; no other script, same-origin or not, runs.
fn csp(nonce: &str) -> String {
    format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'self'; img-src 'self'; \
         connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'; manifest-src 'self'"
    )
}
