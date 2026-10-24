//! Settings › "HTTPS & Install as App" (owner decision 59; a web
//! addition, the app has no counterpart): LAN HTTPS with the router's own
//! certificate authority (`crate::tls`), so Chrome, Edge and Samsung
//! Internet can install the web UI as an app (Safari adds it to the Home
//! Screen or the Dock over http too).
//!
//! - Settings shows the state: off with "Turn On HTTPS" (and "Reset HTTPS"
//!   when files are left that could not be used); on with the
//!   certificate's end date, the https addresses, "Install on Your
//!   Devices" (`PAGE`: the CA's fingerprint, what it can vouch for, the
//!   downloads and the steps per platform), "Try Again" when not
//!   listening, "Renew Certificate" when fewer than 30 days are left by
//!   the router's clock, and "Reset HTTPS".
//! - Turn On, Renew and Reset are questions (`GET`) whose form carries a
//!   single-use token; the post does it in a gate task and comes back to
//!   Settings with its line. Turn On and Renew show the router's clock
//!   (the certificates are dated from it) and say when it could not be
//!   checked over the internet.
//! - The downloads (`tls::DOWNLOAD_CRT`, `tls::DOWNLOAD_PROFILE`) are
//!   public while HTTPS is on (the CA certificate is public; a phone
//!   installing it may not be logged in) and never stored by caches.
//! - Over plain http, Settings and the login page point to the https
//!   address (once the device trusts the CA: Install on Your Devices).

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{HeaderValue, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::Method as HttpMethod;
use axum::response::Response;
use maud::{html, Markup};

use crate::handlers::AppState;
use crate::httpd::gate::Lane;
use crate::httpd::tls::Secure;
use crate::httpd::{Ctx, Peer};
use crate::tls::{Status, Utc, DOWNLOAD_CRT, DOWNLOAD_PROFILE};
use crate::web::action::{self, Ask};
use crate::web::job::{self, OnceJob, Sent};
use crate::web::kit::{self, Icon, Tab};
use crate::web::render::{self, PageCtx};
use crate::web::screen::PageData;
use crate::web::session;
use crate::web::store::{Token, SETTLE};
use crate::web::{sms, CspNonce};

/// "Install on Your Devices".
pub const PAGE: &str = "/ui/settings/https";
pub const ON: &str = "/ui/settings/https/on";
pub const RENEW: &str = "/ui/settings/https/renew";
pub const RESET: &str = "/ui/settings/https/reset";
const SETTINGS: &str = "/ui/settings";
const TITLE: &str = "HTTPS & Install as App";

/// "Oct 24, 2026" in the router's time zone.
fn date(t: i64) -> String {
    sms::day(t, sms::local_offset(t))
}

/// "Oct 24, 2026, 14:03 UTC".
fn utc_stamp(t: i64) -> String {
    let s = t.rem_euclid(86_400);
    format!("{}, {:02}:{:02} UTC", sms::day(t, 0), s / 3600, s / 60 % 60)
}

/// Whether this request came over HTTPS.
pub fn is_secure<B>(req: &axum::http::Request<B>) -> bool {
    req.extensions().get::<Secure>().is_some()
}

/// The https addresses as links (a row each).
fn url_rows(s: &Status) -> Markup {
    html! {
        @for url in s.urls() {
            li { a.it href=(url) rel="noreferrer" { (kit::icon(Icon::Lock)) span { (url) } } }
        }
    }
}

/// Over plain http with HTTPS on (and not over Tailscale, whose names the
/// certificate does not hold): where the installable copy is (the address,
/// else the name; plain http answers only by address, `HostPolicy`).
pub fn hint(s: &Status, secure: bool, remote: bool) -> Option<String> {
    if !s.on || secure || remote {
        return None;
    }
    s.urls().first().cloned()
}

/// The Settings section.
pub fn section(pd: &PageData) -> Markup {
    let s = pd.app.tls.status();
    let error = html! { @if let Some(e) = &s.error { p.foot role="status" { (e) } } };
    if !s.on {
        let foot = html! {
            p.foot {
                "Serves this web UI over HTTPS on port " (s.port) " with a certificate from the router\u{2019}s own \
                 certificate authority, so Chrome, Edge and Samsung Internet can install it as an app. Each phone or \
                 computer trusts that authority once."
            }
            @if let Some(ca) = &s.kept {
                p.foot { "The certificate authority " (ca) " is still on the router: Turn On HTTPS issues a new certificate from it, so devices that trust it need nothing new." }
            }
            (error)
        };
        return kit::section(Some(TITLE), Some(foot), html! {
            (kit::kv_row(Some(Icon::Lock), "HTTPS", "Off"))
            li { a.it href=(ON) { span { "Turn On HTTPS" } } }
            @if s.kept.is_some() || s.error.is_some() { li { a.it.danger href=(RESET) { span { "Reset HTTPS" } } } }
        });
    }
    let state = if s.listening.is_some() { "On" } else { "On \u{b7} not listening" };
    let foot = html! {
        @if pd.secure {
            p.foot { "You are using HTTPS. Install the web UI from your browser\u{2019}s menu." }
        } @else if let Some(url) = hint(&s, pd.secure, pd.remote) {
            p.foot { "This page is plain http. Once this device trusts the router\u{2019}s certificate authority (Install on Your Devices), open " a href=(url) rel="noreferrer" { (url) } " to install the web UI as an app." }
        }
        (error)
    };
    kit::section(Some(TITLE), Some(foot), html! {
        (kit::kv_row(Some(Icon::Lock), "HTTPS", state))
        @if s.listening.is_none() { li { a.it href=(ON) { span { "Try Again" } } } }
        @if let Some(t) = s.not_after {
            (kit::kv_row(None, if s.renew_due { "Certificate expires" } else { "Certificate valid until" }, &date(t)))
        }
        (url_rows(&s))
        (kit::link_row(PAGE, Some(Icon::Shield), "Install on Your Devices"))
        @if s.renew_due { li { a.it href=(RENEW) { span { "Renew Certificate" } } } }
        li { a.it.danger href=(RESET) { span { "Reset HTTPS" } } }
    })
}

/// A numbered list of steps in one row.
fn steps(items: &[Markup]) -> Markup {
    html! { li.it { ol.steps { @for i in items { li { (i) } } } } }
}

/// "Install on Your Devices": the CA, the downloads and how each platform
/// trusts it, then how to install the app.
pub fn install(pd: &PageData) -> Markup {
    let s = pd.app.tls.status();
    if !s.on {
        return html! {
            div.vc {
                (kit::empty_state(Icon::Lock, "HTTPS is off", "Turn it on in Settings first."))
                p.actions.ctr { a.btn href=(SETTINGS) { "Settings" } }
            }
        };
    }
    let ca = s.ca_name.as_str();
    let fp_foot = html! {
        @if pd.secure {
            p.foot { "This page came over HTTPS from the router, so these are its fingerprints. Check that the device you set up shows the same SHA-256 fingerprint; letter case, spaces and colons do not matter." }
        } @else {
            p.foot {
                "Before you trust it, check that your device shows the same SHA-256 fingerprint (letter case, spaces and colons do not matter). \
                 This page came over plain http, so someone on your network could change it together with the download: for a check they \
                 cannot fake, compare with this page opened over HTTPS on a device that already trusts the authority, or with "
                span.mono { "sha256sum " (DIR_CA) }
                " over SSH (the same digits)."
            }
        }
        p.foot { "Where devices show it: iPhone and iPad: Settings \u{203a} Profile Downloaded \u{203a} More Details \u{203a} the certificate, before you tap Install; Mac: Keychain Access \u{203a} the certificate; Android: after installing, Settings \u{203a} Trusted credentials \u{203a} User \u{203a} the certificate (remove it if they differ)." }
        p.foot { "Windows shows the SHA-1 \u{201c}Thumbprint\u{201d} (Details tab): " span.mono { (s.sha1.replace(':', "").to_ascii_lowercase()) } }
    };
    html! {
        (kit::section(Some("Certificate Authority"), Some(fp_foot), html! {
            (kit::kv_row(None, "Name", ca))
            (kit::kv_row(None, "Can vouch for", &s.covers.join(" and ")))
            // Line breaks only between the byte pairs; screen readers get
            // the whole value once.
            li.it { span.cvb { span { "SHA-256 fingerprint" } span.sr { (s.sha256) } small.mono.fp translate="no" aria-hidden="true" { @for (i, pair) in s.sha256.split(':').enumerate() { @if i > 0 { ":" wbr; } (pair) } } } }
        }))
        (kit::section(Some("Download"), Some(html! { p.foot { "The same certificate in two formats. It holds no secret." } }), html! {
            li { a.it href=(DOWNLOAD_PROFILE) download="u60-pro-ca.mobileconfig" { (kit::icon(Icon::Doc)) span { "Profile for iPhone and iPad" } } }
            li { a.it href=(DOWNLOAD_CRT) download="u60-pro-ca.crt" { (kit::icon(Icon::Doc)) span { "Certificate (.crt) for Android, Mac and Windows" } } }
        }))
        (kit::section(Some("iPhone and iPad"), None, steps(&[
            html! { "In Safari, tap " b { "Profile for iPhone and iPad" } ", then " b { "Allow" } "." },
            html! { "Open " b { "Settings" } " \u{203a} " b { "Profile Downloaded" } " \u{203a} " b { "Install" } "." },
            html! { b { "Settings" } " \u{203a} " b { "General" } " \u{203a} " b { "About" } " \u{203a} " b { "Certificate Trust Settings" } ": turn on full trust for " b { (ca) } "." },
        ])))
        (kit::section(Some("Android"), Some(html! { p.foot { "The menu names differ by maker: search Settings for \u{201c}CA certificate\u{201d}." } }), steps(&[
            html! { "Tap " b { "Certificate (.crt)" } " to download it." },
            html! { b { "Settings" } " \u{203a} " b { "Security & privacy" } " \u{203a} " b { "More security & privacy" } " \u{203a} " b { "Encryption & credentials" } " \u{203a} " b { "Install a certificate" } " \u{203a} " b { "CA certificate" } " \u{203a} " b { "Install anyway" } ", confirm with your screen lock, then pick the downloaded file." },
        ])))
        (kit::section(Some("Mac"), None, steps(&[
            html! { "Download the certificate (.crt) and open it: Keychain Access adds it to your login keychain." },
            html! { "Double-click " b { (ca) } " \u{203a} " b { "Trust" } " \u{203a} " b { "When using this certificate: Always Trust" } ", then close the window and confirm." },
        ])))
        (kit::section(Some("Windows"), Some(html! { p.foot { "Firefox 120 and later uses the certificates you add to Windows, macOS or Android. On Linux, or with that setting off: Settings \u{203a} Privacy & Security \u{203a} Certificates \u{203a} View Certificates\u{2026} \u{203a} Authorities \u{203a} Import\u{2026}, and trust it for websites." } }), steps(&[
            html! { "Download the certificate (.crt) and open it \u{203a} " b { "Install Certificate" } " \u{203a} " b { "Current User" } "." },
            html! { b { "Place all certificates in the following store" } " \u{203a} " b { "Browse" } " \u{203a} " b { "Trusted Root Certification Authorities" } " \u{203a} " b { "Finish" } " \u{203a} " b { "Yes" } "." },
        ])))
        (kit::section(Some("Then Install the App"), Some(html! {
            p.foot { "Open one of these addresses and log in. Chrome: menu \u{203a} " b { "Install" } " (Android: " b { "Add to Home screen" } "). Safari on iPhone: \u{2022}\u{2022}\u{2022} \u{203a} Share \u{203a} View More \u{203a} " b { "Add to Home Screen" } ". Safari on Mac: File \u{203a} " b { "Add to Dock" } "." }
            p.foot { "A device that uses its own DNS (Android\u{2019}s Private DNS set to a provider, DNS over HTTPS in the browser, a VPN) may not find " (s.name) "; the address always works. Each address keeps its own login." }
        }), url_rows(&s)))
    }
}

/// The root's certificate on the router (DER: its SHA-256 is the
/// fingerprint), for the SSH check.
const DIR_CA: &str = "/data/zte-agent-tls/ca.crt";

/// Turn On, Renew, Reset.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Post {
    On,
    Renew,
    Reset,
}

impl Post {
    pub fn of(path: &str) -> Option<Post> {
        match path {
            ON => Some(Post::On),
            RENEW => Some(Post::Renew),
            RESET => Some(Post::Reset),
            _ => None,
        }
    }

    fn path(self) -> &'static str {
        match self {
            Post::On => ON,
            Post::Renew => RENEW,
            Post::Reset => RESET,
        }
    }
}

/// The clock line of Turn On and Renew.
fn clock_line(c: Utc) -> String {
    match c {
        Utc::Verified(t) => format!("The certificates are dated from the router\u{2019}s clock, checked over the internet: {}.", utc_stamp(t)),
        _ => format!(
            "The certificates are dated from the router\u{2019}s clock: {}. It could not be checked over the internet; if this date is wrong, cancel and try again later.",
            utc_stamp(c.secs())
        ),
    }
}

/// That plain http keeps working, on the port it is bound to.
fn plain_http() -> String {
    format!("Plain http on port {} keeps working, for the apps too.", crate::httpd::http_port())
}

/// The question: its title, lines and button (none: nothing to do).
fn question(app: &AppState, k: Post, secure: bool) -> (String, Vec<String>, Option<&'static str>) {
    let tls = &app.tls;
    let s = tls.status();
    let port = s.port;
    match k {
        Post::On if s.on && s.listening.is_some() => ("HTTPS is already on".into(), Vec::new(), None),
        Post::On if s.on => (
            "Listen again?".into(),
            vec![
                format!("HTTPS is on, but the router is not listening on port {port}{}", s.error.as_deref().map_or(".".into(), |e| format!(": {e}"))),
                "Try again once whatever held the port has stopped. Nothing else changes.".into(),
            ],
            Some("Try Again"),
        ),
        Post::Renew if !s.on => ("HTTPS is off".into(), Vec::new(), None),
        Post::Reset if !s.on && s.kept.is_none() && s.error.is_none() => ("HTTPS is off".into(), Vec::new(), None),
        Post::On | Post::Renew if s.clock.usable().is_none() => {
            ("The router\u{2019}s clock isn\u{2019}t set yet".into(), vec![crate::tls::CLOCK_UNSET.into()], None)
        }
        Post::On if s.kept.is_some() => (
            "Turn on HTTPS?".into(),
            vec![
                format!(
                    "The router issues a new certificate for its LAN address and {} from its certificate authority ({}), and serves this web UI over HTTPS on port {port}. Devices that trust it need nothing new.",
                    tls.name(),
                    s.kept.as_deref().unwrap_or_default()
                ),
                clock_line(s.clock),
                plain_http(),
            ],
            Some("Turn On HTTPS"),
        ),
        Post::On => (
            "Turn on HTTPS?".into(),
            vec![
                format!(
                    "The router creates its own certificate authority and a certificate for its LAN address and {}, and serves this web UI over HTTPS on port {port}.",
                    tls.name()
                ),
                format!("Each phone or computer then trusts that authority once (Install on Your Devices shows how). It can only vouch for this router\u{2019}s LAN address and {} (and names under it), never for other websites or devices. If you later change the LAN address, only {} keeps working until you reset HTTPS.", tls.name(), tls.name()),
                clock_line(s.clock),
                plain_http(),
            ],
            Some("Turn On HTTPS"),
        ),
        Post::Renew => (
            "Renew the certificate?".into(),
            vec![
                format!("The router issues a new certificate for the next {} days from the same certificate authority; devices that trust it need nothing new.", crate::tls::SERVER_DAYS - 2),
                clock_line(s.clock),
            ],
            Some("Renew Certificate"),
        ),
        Post::Reset if !s.on => (
            "Reset HTTPS?".into(),
            vec![
                format!("This deletes the certificate files left on the router{}.", s.kept.as_deref().map_or(String::new(), |c| format!(" ({c})"))),
                "Remove that certificate authority from every device that trusts it. If you turn HTTPS on again, each device must trust the new one.".into(),
            ],
            Some("Reset HTTPS"),
        ),
        Post::Reset => {
            let mut lines = vec![
                format!("This deletes the router\u{2019}s certificate authority ({}) and certificate and stops HTTPS on port {port}, also for pages open over it.", s.ca_name),
                "Remove that certificate authority from every device that trusts it: it can no longer be used. If you turn HTTPS on again, each device must trust the new one.".into(),
            ];
            if secure {
                lines.push(format!("This page is open over HTTPS and stops working: continue on the http address (port {}).", crate::httpd::http_port()));
            }
            ("Reset HTTPS?".into(), lines, Some("Reset HTTPS"))
        }
    }
}

/// `GET` of a question: its form carries a new single-use token.
pub fn get(ctx: &Ctx, p: &crate::auth::Principal, req: &Request, k: Post) -> Response {
    let nonce = CspNonce::of(req);
    let (title, lines, label) = question(&ctx.app, k, is_secure(req));
    let token = label.map(|_| ctx.once.fresh(&ctx.app.auth, p.id)).unwrap_or_default();
    let hidden = [("once", token.as_str())];
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let a = Ask { title: &title, lines: &lines, action: k.path(), hidden: &hidden, label, danger: k == Post::Reset, cancel: SETTINGS };
    let pc = PageCtx {
        title: &title,
        tab: Some(Tab::Settings),
        nonce: &nonce,
        lead: kit::back(SETTINGS, "Settings"),
        heading: Some(html! { h1 { "Confirm" } }),
        ..PageCtx::default()
    };
    render::html(200, render::page(&pc, action::ask(render::csrf_field(&ctx.app.auth, p), &a)))
}

/// 303 to Settings with `text` for this session (`?r=<id>`).
fn line(ctx_flash: &crate::web::store::Flash, app: &AppState, owner: crate::auth::SessionId, ok: Option<bool>, text: String) -> Response {
    let Ok(id) = app.auth.random_hex(8) else { return session::see_other(SETTINGS) };
    ctx_flash.insert(id.clone(), owner, (ok, text), app.auth.now());
    session::see_other(&format!("{SETTINGS}?r={id}"))
}

fn done(k: Post) -> &'static str {
    match k {
        Post::On => "HTTPS is on. Next: Install on Your Devices (a device that already trusts the router\u{2019}s certificate authority needs nothing new).",
        Post::Renew => "Certificate renewed.",
        Post::Reset => "HTTPS was reset. Remove the old certificate authority from your devices.",
    }
}

/// `POST` of a question: the session (else the login), the form, origin and
/// CSRF token, `confirm=1`, then once per token: the change, and Settings
/// with its line.
pub async fn post(ctx: Ctx, req: Request, k: Post) -> Response {
    let nonce = CspNonce::of(&req);
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, k.path(), None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&nonce, SETTINGS);
    }
    if form.get("confirm") != Some("1") {
        return session::see_other(k.path());
    }
    let token = form.get("once").unwrap_or("").to_owned();
    let flash = Arc::clone(&ctx.flash);
    let owner = p.id;
    let work: OnceJob = Box::new(move |app: &AppState, claim| {
        claim.disarm();
        let r = match k {
            Post::On => app.tls.turn_on(),
            Post::Renew => app.tls.renew(),
            Post::Reset => app.tls.reset(),
        };
        claim.set(match &r {
            Ok(()) => Token::Sent(claim.digest()),
            Err(e) => Token::Failed(claim.digest(), 500, e.clone()),
        });
        match r {
            Ok(()) => line(&flash, app, owner, Some(true), done(k).into()),
            Err(e) => line(&flash, app, owner, Some(false), e),
        }
    });
    let note = |ok: Option<bool>, text: &str| line(&ctx.flash, &ctx.app, owner, ok, text.to_owned());
    match job::once(&ctx, owner, &nonce, &token, job::digest(&[k.path()]), Lane::General, SETTLE, work).await {
        Sent::Ran(resp) => resp,
        Sent::Done(Ok(())) => note(Some(true), done(k)),
        Sent::Done(Err((_, e))) => note(Some(false), &e),
        Sent::Expired => note(Some(false), "This page expired, so nothing was changed. Try again."),
        Sent::Unknown => note(None, "The router may still be working on it. Check before trying again."),
        Sent::Busy => note(Some(false), "Failed: the router is busy. Try again in a few seconds."),
    }
}

fn plain(status: u16, text: &'static str) -> Response {
    let mut resp = Response::new(Body::from(text));
    *resp.status_mut() = axum::http::StatusCode::from_u16(status).unwrap_or_default();
    resp.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    resp
}

/// The public downloads (static files: no session, no nonce): the file
/// while HTTPS is on, else plain-text 404; 405 for anything but GET and
/// HEAD. `None` for any other path.
pub fn download(ctx: &Ctx, method: &HttpMethod, path: &str) -> Option<Response> {
    let crt = match path {
        DOWNLOAD_CRT => true,
        DOWNLOAD_PROFILE => false,
        _ => return None,
    };
    if method != HttpMethod::GET && method != HttpMethod::HEAD {
        return Some(plain(405, "Method not allowed"));
    }
    let file = if crt {
        ctx.app.tls.ca_der().map(|der| (der, "application/x-x509-ca-cert", "u60-pro-ca.crt"))
    } else {
        ctx.app.tls.mobileconfig().map(|p| (p.into_bytes(), "application/x-apple-aspen-config", "u60-pro-ca.mobileconfig"))
    };
    let Some((body, ct, file)) = file else { return Some(plain(404, "Not found")) };
    let mut resp = Response::new(Body::from(body));
    let h = resp.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static(ct));
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{file}\"")) {
        h.insert(CONTENT_DISPOSITION, v);
    }
    Some(resp)
}

/// The login page's line over plain http while HTTPS is on.
pub fn login_hint(ctx: &Ctx, parts: &axum::http::request::Parts) -> Option<String> {
    let ext = &parts.extensions;
    hint(&ctx.app.tls.status(), ext.get::<Secure>().is_some(), session::is_remote(ext.get::<Peer>()))
}
