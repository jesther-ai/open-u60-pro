//! The LAN HTTPS listener (owner decision 59): `0.0.0.0:9443`
//! (`ZTE_AGENT_TLS_BIND`) while HTTPS is on (`crate::tls`), on the same
//! runtime as :9090, with the same router, the same connection caps (shared
//! with :9090) and its own mobile-side check. rustls with ring, TLS 1.2 and
//! 1.3, ALPN `http/1.1`; the server key is read at every handshake, so a
//! new certificate takes effect without a restart.
//!
//! Cookies are per host, not per port: on the same host the http and https
//! UIs would share cookie names. So the TLS side presents every request to
//! the app as the plain listener would see it, and translates back:
//! - in: only this origin's `__Host-zte_ui_*` cookies, without the prefix
//!   (the http origin's cookies, sent along by browsers, are dropped); an
//!   `https://` `Origin` / `Referer` as `http://` with the same host and
//!   port (an `http://` one becomes `null`: foreign), so the same-origin
//!   checks compare host and port exactly as on :9090;
//! - out: every `zte_ui_*` cookie becomes `__Host-zte_ui_*; Path=/; Secure`.
//!   `__Host-` cookies can only be set over HTTPS, for this host, with
//!   `Path=/`: the http origin can neither read nor overwrite them, and the
//!   https side never reads the http origin's.
//!
//! No HSTS: it would also force https onto :9090 of the same host.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use axum::body::Body;
use axum::http::header::{HeaderMap, HeaderValue, COOKIE, ORIGIN, REFERER, SET_COOKIE};
use axum::Router;
use futures_util::future::{select, Either};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use tokio::sync::watch;

use super::{accept_loop_with, listen, Admit, Caps, Ctx, Peer};
use crate::tls::{Keys, Tls};

/// A TLS handshake must be done within this (a stalled one holds a slot).
const HANDSHAKE_WAIT: Duration = Duration::from_secs(10);
/// A connection waits at most this long for `Tls::check` (a new address or
/// renewal) before its handshake; a slower one finishes in the background.
const CHECK_WAIT: Duration = Duration::from_secs(2);
/// Resumable TLS sessions kept.
const SESSIONS: usize = 64;

/// Request extension: this request came over the HTTPS listener.
#[derive(Copy, Clone, Debug)]
pub struct Secure;

/// The current server key at every handshake (`None`: HTTPS was reset;
/// the handshake fails).
#[derive(Debug)]
struct Resolver(Keys);

impl ResolvesServerCert for Resolver {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.0.read().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

fn server_config(keys: Keys) -> Option<Arc<rustls::ServerConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = match rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[https] TLS setup: {e}");
            return None;
        }
    };
    let mut config = builder.with_no_client_auth().with_cert_resolver(Arc::new(Resolver(keys)));
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config.session_storage = rustls::server::ServerSessionMemoryCache::new(SESSIONS);
    Some(Arc::new(config))
}

/// What the accept loop runs on a TLS connection.
pub struct Wire {
    acceptor: tokio_rustls::TlsAcceptor,
    tls: Arc<Tls>,
    /// Its sender is dropped when the listener stops (Reset): every
    /// connection of that listen ends (a request in progress may finish).
    stop: watch::Receiver<()>,
}

impl Wire {
    /// A connection: a quick check first when it reached an address the
    /// certificate does not name or renewal may be due (memory only; the
    /// check itself on the blocking pool, at most once a minute), then the
    /// handshake, then HTTP/1.1 with the translations above; all of it
    /// until the listener stops.
    pub(super) async fn serve(&self, tcp: tokio::net::TcpStream, addr: SocketAddr, router: Router, idle: Duration) {
        let mut stop = self.stop.clone();
        let setup = async {
            if let Ok(local) = tcp.local_addr() {
                if self.tls.wants_check(local.ip()) {
                    let tls = Arc::clone(&self.tls);
                    let _ = tokio::time::timeout(CHECK_WAIT, tokio::task::spawn_blocking(move || tls.check())).await;
                }
            }
            tokio::time::timeout(HANDSHAKE_WAIT, self.acceptor.accept(tcp)).await.ok()?.ok()
        };
        let stream = match select(pin!(setup), pin!(stop.changed())).await {
            Either::Left((Some(s), _)) => s,
            _ => return,
        };
        let svc = hyper::service::service_fn(move |mut req: axum::http::Request<hyper::body::Incoming>| {
            req.extensions_mut().insert(Peer(addr));
            req.extensions_mut().insert(Secure);
            inbound(req.headers_mut());
            let reply = tower::ServiceExt::oneshot(router.clone(), req.map(Body::new));
            async move {
                let Ok(mut resp) = reply.await;
                outbound(resp.headers_mut());
                Ok::<_, Infallible>(resp)
            }
        });
        let stopped = async move {
            let _ = stop.changed().await;
        };
        super::serve_http1(hyper_util::rt::TokioIo::new(stream), svc, idle, stopped).await;
    }
}

/// Runs the HTTPS listener while HTTPS is on (`Tls::subscribe`): binds
/// when it turns on, stops (closing the port and ending its connections)
/// when it is reset. `side` makes the mobile-side check for each time it
/// listens. A bind that fails, or a listener that stops by itself, is
/// reported in Settings and tried again at the next change (Turn On again
/// sends one); it never retries on its own.
pub(super) async fn supervise<A: Admit>(tls: Arc<Tls>, ctx: Ctx, caps: Caps, side: impl Fn() -> A) {
    supervise_with(tls, |listener, wire| accept_loop_with(listener, ctx.clone(), caps.clone(), side(), super::Wire::Tls(wire))).await
}

/// `supervise` with the accept loop as `serve` (tests swap it).
async fn supervise_with<F, Fut>(tls: Arc<Tls>, serve: F)
where
    F: Fn(std::net::TcpListener, Arc<Wire>) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut on = tls.subscribe();
    loop {
        while !*on.borrow_and_update() {
            if on.changed().await.is_err() {
                return;
            }
        }
        // A new config each time it listens: a reset also drops every
        // resumable session (a client could otherwise resume one made
        // under the old CA without seeing the new certificate).
        let bound = listen(tls.bind()).and_then(|l| Ok((l.local_addr()?, l))).map_err(|e| format!("Could not listen on {}: {e}", tls.bind()));
        let ready = bound.and_then(|b| Ok((b, server_config(tls.keys()).ok_or("Could not set up TLS.")?)));
        let ((addr, listener), config) = match ready {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[https] {e}");
                tls.set_listening(Err(e));
                if on.changed().await.is_err() {
                    return;
                }
                continue;
            }
        };
        eprintln!("[https] listening on {addr}");
        tls.set_listening(Ok(addr));
        let (stop, stop_rx) = watch::channel(());
        let wire = Arc::new(Wire { acceptor: tokio_rustls::TlsAcceptor::from(config), tls: Arc::clone(&tls), stop: stop_rx });
        let off = async {
            loop {
                if on.changed().await.is_err() || !*on.borrow_and_update() {
                    return;
                }
            }
        };
        let ended = matches!(select(pin!(serve(listener, wire)), pin!(off)).await, Either::Left(_));
        // Every connection of this listen ends.
        drop(stop);
        tls.stopped();
        if ended {
            eprintln!("[https] the listener on {addr} stopped");
            tls.set_listening(Err(format!("The listener on {addr} stopped.")));
            if on.changed().await.is_err() {
                return;
            }
            continue;
        }
        eprintln!("[https] stopped");
    }
}

const PREFIX: &str = "__Host-";
const APP: &str = "zte_ui_";

/// In: this origin's cookies without their prefix (nothing else), and the
/// origin headers as the plain listener would compare them.
pub fn inbound(h: &mut HeaderMap) {
    let mine: Vec<&str> = h
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().strip_prefix(PREFIX).filter(|p| p.starts_with(APP)))
        .collect();
    let cookie = (!mine.is_empty()).then(|| HeaderValue::from_str(&mine.join("; ")).ok()).flatten();
    h.remove(COOKIE);
    if let Some(v) = cookie {
        h.insert(COOKIE, v);
    }
    for name in [ORIGIN, REFERER] {
        let Some(v) = h.get(&name) else { continue };
        let mapped = match v.to_str().ok().and_then(|v| v.strip_prefix("https://")) {
            Some(rest) => HeaderValue::from_str(&format!("http://{rest}")).unwrap_or(HeaderValue::from_static("null")),
            None => HeaderValue::from_static("null"),
        };
        h.remove(&name);
        h.insert(name, mapped);
    }
}

/// Out: the app's cookies as `__Host-` cookies of this origin.
pub fn outbound(h: &mut HeaderMap) {
    if !h.contains_key(SET_COOKIE) {
        return;
    }
    let set: Vec<HeaderValue> = h.get_all(SET_COOKIE).iter().cloned().collect();
    h.remove(SET_COOKIE);
    for v in set {
        let mapped = match v.to_str() {
            Ok(s) if s.starts_with(APP) => HeaderValue::from_str(&secure_cookie(s)).ok(),
            _ => Some(v),
        };
        if let Some(v) = mapped {
            h.append(SET_COOKIE, v);
        }
    }
}

/// `zte_ui_x=v; Path=/ui/; …` as `__Host-zte_ui_x=v; Path=/; …; Secure`.
fn secure_cookie(s: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (i, p) in s.split(';').map(str::trim).enumerate() {
        let lower = p.to_ascii_lowercase();
        if i == 0 {
            parts.push(format!("{PREFIX}{p}"));
        } else if lower.starts_with("path=") {
            parts.push("Path=/".into());
        } else if !lower.starts_with("domain=") && lower != "secure" && !p.is_empty() {
            parts.push(p.to_owned());
        }
    }
    if !parts.iter().any(|p| p == "Path=/") {
        parts.push("Path=/".into());
    }
    parts.push("Secure".into());
    parts.join("; ")
}

#[cfg(test)]
mod tests;
