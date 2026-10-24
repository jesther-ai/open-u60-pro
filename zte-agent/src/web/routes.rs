//! The route table: every screen (`SCREENS`), every action (`ACTIONS`, and
//! their `…/confirm` pages), the operation pages and the live streams, all
//! behind one axum handler (each axum route costs about 3 KB of generic
//! code). Anything else, including a wrong method, is the HTML 404 page.

use axum::extract::{Request, State};
use axum::http::Method;
use axum::response::Response;

use super::action::{self, Action};
use super::pages::{at, automations, confirm, devctl, esim, esim_add, forward as forward_page, https, netcfg, op, services, sim, sms as sms_page};
use super::screen::{self, Screen, Search};
use super::tabs::{dashboard, router, settings, sms, tools};
use super::{live, prefs, render, session, CspNonce};
use crate::httpd::{Ctx, Peer};

pub static SCREENS: [&Screen; 75] = [
    &dashboard::DASHBOARD,
    &dashboard::SIGNAL,
    &dashboard::TRAFFIC,
    &dashboard::BATTERY,
    &dashboard::HEALTH,
    &dashboard::CPU,
    &dashboard::PROCESSES,
    &dashboard::KILL_ONE,
    &dashboard::KILL_ALL,
    &dashboard::WIFI,
    &dashboard::SHARE,
    &dashboard::NET_MODE,
    &dashboard::ROUTER_NET_MODE,
    &sms::SMS,
    &sms::FORWARD,
    &sms::DELETE,
    &sms::LOG,
    &sms::CLEAR,
    &tools::TOOLS,
    &sms::TOOLS_FORWARD,
    &sms::TOOLS_DELETE,
    &sms::TOOLS_LOG,
    &sms::TOOLS_CLEAR,
    &tools::USB_MODE,
    &tools::DEVICE_INFO,
    &tools::DEVICES,
    &tools::LAN_SPEED,
    &tools::CONFIG,
    &tools::TTL,
    &tools::SSH,
    &tools::EXPLORER,
    &tools::AUTOMATIONS,
    &tools::AUTO_DELETE,
    &tools::SPEEDTEST,
    &tools::ADB,
    &tools::BAND_LOCK,
    &tools::AT,
    &router::ROUTER,
    &router::MOBILE_NETWORK,
    &router::REBOOT_REQUIRED,
    &router::CELL_LOCK,
    &router::STC,
    &router::SIGNAL_DETECT,
    &router::SIM,
    &router::SIM_PIN,
    &router::SIM_LOCK_ON,
    &router::SIM_LOCK_OFF,
    &router::SIM_CHANGE,
    &router::SIM_PUK,
    &router::SIM_NCK,
    &router::SIM_SERVICES,
    &router::ESIM,
    &router::ESIM_SWITCH,
    &router::ESIM_DELETE,
    &router::ESIM_RENAME,
    &router::ESIM_ADD,
    &router::WIFI,
    &router::GUEST,
    &router::APN,
    &router::APN_NEW,
    &router::APN_EDIT,
    &router::APN_AUTO,
    &router::LAN,
    &router::DNS,
    &router::DNS_CACHE,
    &router::FIREWALL,
    &router::PF_NEW,
    &router::TELEMETRY,
    &router::VPN,
    &router::QOS,
    &router::DEVICE,
    &router::DEVICE_REBOOT,
    &router::DEVICE_RESET,
    &settings::SETTINGS,
    &settings::INSTALL,
];

pub static ACTIONS: [&Action; 25] = [
    &dashboard::POWERBANK,
    &dashboard::USB_DISMISS,
    &router::MOBILE_DATA,
    &router::AIRPLANE,
    &router::CHARGE_LIMIT,
    &router::STC_PARAMS,
    &dashboard::SET_NET_MODE,
    &dashboard::SET_ROUTER_NET_MODE,
    &dashboard::KILL,
    &dashboard::KILL_ALL_BLOAT,
    &tools::USB_POWERBANK,
    &tools::AUTO_TOGGLE,
    &tools::AUTO_DELETE_JOB,
    &tools::SPEEDTEST_STOP,
    &tools::AT_CLEAR,
    &sms::FW_ENABLED,
    &sms::FW_SETTINGS,
    &sms::FW_TOGGLE,
    &sms::FW_DELETE,
    &sms::FW_CLEAR,
    &sms::TOOLS_FW_ENABLED,
    &sms::TOOLS_FW_SETTINGS,
    &sms::TOOLS_FW_TOGGLE,
    &sms::TOOLS_FW_DELETE,
    &sms::TOOLS_FW_CLEAR,
];

fn screen(path: &str) -> Option<&'static Screen> {
    SCREENS.into_iter().find(|s| s.path == path)
}

fn action(path: &str) -> Option<&'static Action> {
    ACTIONS.into_iter().find(|a| a.path == path)
}

/// The live screen with stream id `id`.
pub fn live_screen(id: &str) -> Option<&'static Screen> {
    SCREENS.into_iter().find(|s| s.id == id && !s.live.is_empty())
}

/// A screen searched by a POST to its own path.
fn searched(path: &str) -> Option<&'static Screen> {
    screen(path).filter(|s| s.search != Search::No)
}

/// The screen showing the form that posts to `path`, if it is an action.
pub fn form_page(path: &str) -> Option<&'static str> {
    action(path)
        .map(|a| a.screen.path)
        .or_else(|| searched(path).map(|s| s.path))
        .or_else(|| sms_page::form_page(path))
        .or_else(|| forward_page::form_page(path))
        .or_else(|| (path == automations::SAVE).then_some(automations::LIST))
        .or_else(|| confirm::Kind::by_path(path).map(|k| k.screen().path))
        .or_else(|| (path == at::SEND).then_some(at::PATH))
        .or_else(|| devctl::Power::of(path).map(devctl::Power::path))
        .or_else(|| prefs::Post::of(path).map(prefs::Post::form))
        .or_else(|| https::Post::of(path).map(|_| settings::SETTINGS.path))
        .or_else(|| sim::Sheet::of(path).map(sim::Sheet::path))
        .or_else(|| services::Post::of(path).map(|_| services::PATH))
        .or_else(|| esim::Post::of(path).map(|_| esim::PATH))
        .or_else(|| esim_add::Post::of(path).map(|_| esim_add::ADD))
        .or_else(|| netcfg::Kind::by_path(path).map(netcfg::Kind::home))
}

enum Get {
    Screen(&'static Screen),
    Confirm(&'static Action),
    Op,
    /// New Message, a conversation or its delete question (by its opaque key).
    Sms(sms_page::Page),
    /// New Rule or Edit Rule (SMS Forwarding, either mount).
    Rule(forward_page::Page),
    /// New Automation or Edit Automation.
    Job(automations::Page),
    /// A question that hands out a single-use token (Speed Test, Enable
    /// ADB, Band Lock).
    Ask(confirm::Kind),
    /// A Router settings question (WiFi, Guest WiFi, APN, LAN, DNS,
    /// Firewall, a blocked domain's removal), or the page after one
    /// (WiFi's "Reconnect to …", LAN's new address).
    Net(netcfg::Kind),
    NetDone(netcfg::Kind),
    /// Turn On, Renew or Reset HTTPS: the question.
    Https(https::Post),
}

/// `/ui/` and `/ui/{*rest}`.
pub async fn dispatch(State(ctx): State<Ctx>, req: Request) -> Response {
    let path = req.uri().path();
    if req.method() == Method::POST {
        if let Some(a) = action(path) {
            return action::post(ctx, req, a).await;
        }
        // SIM Card's sheets (PINs in the body only; single-use token).
        if let Some(sheet) = sim::Sheet::of(path) {
            return sim::post(ctx, req, sheet).await;
        }
        // SIM Services: USSD and the SIM Toolkit (single-use token).
        if let Some(k) = services::Post::of(path) {
            return services::post(ctx, req, k).await;
        }
        // eSIM: switch, delete, rename, send receipts (single-use token).
        if let Some(k) = esim::Post::of(path) {
            return esim::post(ctx, req, k).await;
        }
        // Add eSIM: the photo or code (multipart, bounded), Install profile,
        // Cancel download (single-use token).
        if let Some(k) = esim_add::Post::of(path) {
            return esim_add::post(ctx, req, k).await;
        }
        // WiFi, Guest WiFi, APN, LAN, DNS, Firewall: a form becomes a draft
        // and its question; the confirmed post; Telemetry Blocker, VPN
        // Passthrough and QoS post from their page (single-use token).
        if let Some(k) = netcfg::Kind::by_path(path) {
            return netcfg::post(ctx, req, k).await;
        }
        // A search (Messages, Connected Devices, DNS Cache): the text in no URL.
        if let Some(s) = searched(path) {
            return screen::search(ctx, req, s).await;
        }
        // An automation's save (single-use token).
        if path == automations::SAVE {
            return automations::post(ctx, req).await;
        }
        // Speed test start, Enable ADB, Band Lock, the Router changes; an AT
        // command (single-use tokens).
        if let Some(kind) = confirm::Kind::by_path(path) {
            return confirm::post(ctx, req, kind).await;
        }
        if path == at::SEND {
            return at::post(ctx, req).await;
        }
        // Reboot Router, Factory Reset, Mobile Network's "Reboot Now" (the
        // router password, single-use token).
        if let Some(w) = devctl::Power::of(path) {
            return devctl::post(ctx, req, w).await;
        }
        // Settings: the theme and the refresh interval (cookies).
        if let Some(k) = prefs::Post::of(path) {
            return prefs::post(ctx, req, k).await;
        }
        // Settings: Turn On, Renew, Reset HTTPS (single-use token).
        if let Some(k) = https::Post::of(path) {
            return https::post(ctx, req, k).await;
        }
        // SMS open, send (single-use token) and delete: keyed (`pages::sms`).
        if let Some(post) = sms_page::Post::of(path) {
            return sms_page::post(ctx, req, post).await;
        }
        // A forwarding rule's save and test (single-use token, secrets
        // kept), the Forward Log's Retry (single-use token).
        if let Some(post) = forward_page::Post::of(path) {
            return forward_page::post(ctx, req, post).await;
        }
    }
    // GET only: a HEAD would subscribe (and could replace another stream
    // of the session) for a body nobody reads.
    if req.method() == Method::GET && path.starts_with("/ui/live/") {
        if let Some(resp) = live::stream(&ctx, &req) {
            return resp;
        }
    }
    // The CA downloads: public (no session), while HTTPS is on.
    if let Some(resp) = https::download(&ctx, req.method(), path) {
        let (parts, body) = req.into_parts();
        return session::reply_unread(&parts.headers, body, resp).await;
    }
    let target = if let Some(s) = screen(path) {
        Some(Get::Screen(s))
    } else if let Some(page) = sms_page::Page::of(path) {
        Some(Get::Sms(page))
    } else if let Some(page) = forward_page::Page::of(path) {
        Some(Get::Rule(page))
    } else if let Some(page) = automations::Page::of(path) {
        Some(Get::Job(page))
    } else if let Some(kind) = confirm::Kind::by_path(path) {
        Some(Get::Ask(kind))
    } else if let Some(kind) = netcfg::Kind::by_path(path) {
        Some(Get::Net(kind))
    } else if let Some(kind) = netcfg::result_kind(path) {
        Some(Get::NetDone(kind))
    } else if let Some(k) = https::Post::of(path) {
        Some(Get::Https(k))
    } else if let Some(a) = path.strip_suffix("/confirm").and_then(action).filter(|a| !a.confirm.is_empty()) {
        Some(Get::Confirm(a))
    } else {
        path.starts_with("/ui/op/").then_some(Get::Op)
    };
    let get = matches!(*req.method(), Method::GET | Method::HEAD);
    let Some(target) = target.filter(|_| get) else {
        let nonce = CspNonce::of(&req);
        let (parts, body) = req.into_parts();
        return session::reply_unread(&parts.headers, body, render::not_found(&nonce)).await;
    };
    // GET pages: as the `Session` extractor (with the marker hop).
    let (path, query) = (req.uri().path(), req.uri().query());
    let Some(p) = session::find_session(&ctx.app.auth, req.headers()) else {
        let nonce = CspNonce::of(&req);
        return session::without_session(req.headers(), req.method(), path, query, &nonce, |q| {
            session::login_redirect(req.method(), path, q)
        });
    };
    if let Some(clean) = session::after_hop(path, query) {
        return clean;
    }
    match target {
        Get::Screen(s) => {
            // A Router settings form with `?t=`: as typed (its draft).
            let mut r = screen::Req::of(&ctx, &req);
            netcfg::typed(&ctx, &p, s.path, &mut r);
            screen::get(&ctx, p, r, s).await
        }
        Get::Confirm(a) => action::confirm_page(&ctx, &p, &req, a),
        Get::Op => op::page(ctx, p, req).await,
        Get::Sms(page) => {
            let query = req.uri().query().map(str::to_owned);
            sms_page::get(&ctx, p, CspNonce::of(&req), query, page).await
        }
        Get::Rule(page) => forward_page::get(&ctx, p, CspNonce::of(&req), page).await,
        Get::Job(page) => {
            let remote = session::is_remote(req.extensions().get::<Peer>());
            automations::get(&ctx, p, CspNonce::of(&req), remote, page).await
        }
        Get::Ask(kind) => confirm::get(&ctx, p, req, kind).await,
        Get::Net(kind) => netcfg::get(&ctx, p, req, kind).await,
        Get::NetDone(kind) => netcfg::result(&ctx, &p, &req, kind),
        Get::Https(k) => https::get(&ctx, &p, &req, k),
    }
}
