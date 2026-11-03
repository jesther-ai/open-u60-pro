//! Web-only optional networking controls. All device effects use the same
//! Rust handlers as the authenticated JSON routes; no mobile UI changes.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::Method as HttpMethod;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::json;

use crate::handlers::AppState;
use crate::httpd::{api, gate::Lane, Ctx};
use crate::server::Method;
use crate::web::job::{self, OnceJob, Sent};
use crate::web::kit;
use crate::web::render::{self, once_field};
use crate::web::screen::PageData;
use crate::web::session::{self, Form};
use crate::web::store::{Token, SETTLE_SLOW};
use crate::web::CspNonce;

pub const MIHOMO: &str = "/ui/router/mihomo";
pub const SWITCH: &str = "/ui/router/mihomo/switch";
pub const INSTALL: &str = "/ui/router/mihomo/install";
pub const CONFIG: &str = "/ui/router/mihomo/config";
pub const OPEN: &str = "/ui/router/mihomo/open";
pub const RELAY: &str = "/ui/relay";
pub const SCAN: &str = "/ui/relay/scan";
pub const CONNECT: &str = "/ui/relay/connect";
pub const DISCONNECT: &str = "/ui/relay/disconnect";
pub const CLOCK: &str = "/ui/relay/clock";
pub const FORGET: &str = "/ui/relay/forget";
pub const TUNING: &str = "/ui/relay/tuning";

pub fn mihomo_card(pd: &PageData) -> Markup {
    let Some(status) = pd.read(0) else {
        return html! { p.msg.err { "Mihomo status unavailable" } };
    };
    let on = status["running"].as_bool() == Some(true);
    let hidden = html! { (pd.csrf()) (once_field(&pd.once())) };
    let state = if on { "Running" } else { "Off" };
    kit::section(
        Some("Mihomo / Clash"),
        Some(html! {
            p.foot { "Experimental LAN TCP/UDP, IPv4/IPv6 interception. Node UDP support and IPv6 exit reachability still apply. Off restores direct routing; it is not a kill switch." }
            @if let Some(e) = status["error"].as_str() { p.foot.err role="alert" { (e) } }
        }),
        html! {
            li {
                form method="post" action=(SWITCH) data-net-switch {
                    (hidden)
                    input type="hidden" name="enable" value=(if on { "0" } else { "1" });
                    div.it {
                        span { "Clash proxy" small data-net-state data-idle-label=(state) role="status" { (state) } }
                        span data-net-spinner-host hidden {}
                        button.sw type="submit" role="switch" aria-checked=(if on { "true" } else { "false" }) aria-label="Clash proxy"
                            data-busy-label=(if on { "Stopping Clash..." } else { "Starting Clash..." }) {}
                    }
                    div data-net-feedback {}
                }
            }
            @if on { li { a.it href=(OPEN) target="_blank" rel="noopener noreferrer" { span { "Open Zashboard" } (kit::icon(kit::Icon::Arrows)) } } }
            (kit::link_row(MIHOMO, Some(kit::Icon::Gear), "Mihomo setup & status"))
        },
    )
}

pub fn mihomo_page(pd: &PageData) -> Markup {
    let Some(s) = pd.read(0) else {
        return html! { p.msg.err { "Mihomo status unavailable" } };
    };
    let on = s["running"].as_bool() == Some(true);
    let installed = s["installed"].as_bool() == Some(true);
    let recovery = s["recovery_required"].as_bool() == Some(true);
    let autostart = s["autostart"].as_bool() == Some(true);
    html! {
        (kit::section(Some("Mihomo"), Some(html! {
            p.foot { "Mihomo " (s["version"].as_str().unwrap_or("")) " and Zashboard " (s["dashboard_version"].as_str().unwrap_or("")) ". Attempts to start after an agent restart or reboot only when its enabled choice is saved. Switching off clears that choice. No subscription or paid node is included." }
            @if let Some(e) = s["error"].as_str() { p.foot.err role="alert" { (e) } }
        }), html! {
            (kit::kv_row(None, "Core", if installed { "Installed" } else { "Not installed" }))
            (kit::kv_row(None, "Configuration", if s["configured"].as_bool() == Some(true) { "Saved privately" } else { "Not configured" }))
            (kit::kv_row(None, "Clash proxy", if on { "Running" } else { "Off" }))
            (kit::kv_row(None, "After a restart", match s["autostart"].as_bool() {
                Some(true) => "Automatic start enabled",
                Some(false) => "Stays off",
                None => "Unknown",
            }))
            @if on { li { a.it href=(OPEN) target="_blank" rel="noopener noreferrer" { span { "Open Zashboard" } } } }
        }))
        @if recovery || (!on && autostart) {
            form method="post" action=(SWITCH) {
                (pd.csrf()) (once_field(&pd.once()))
                input type="hidden" name="enable" value="0";
                (kit::section(Some(if recovery { "Recovery required" } else { "Automatic start enabled" }), Some(html! {
                    p.foot { (if recovery { "Some proxy network changes remain. Retry cleanup before starting or reinstalling. This also clears automatic start." } else { "The core is off, but its restart choice is still saved. Cancel pending restoration and keep it off after future restarts." }) }
                }), html! {
                    li { button.it.cv.snd type="submit" { (if recovery { "Retry proxy cleanup" } else { "Stop automatic start & keep off" }) } }
                }))
            }
        }
        @if !on && !recovery {
            form method="post" action=(INSTALL) {
                (pd.csrf()) (once_field(&pd.once()))
                input type="hidden" name="install" value="1";
                (kit::section(Some("Install / repair"), Some(html! { p.foot { "Downloads about 22 MB from pinned official releases, verifies SHA-256, and saves to /data/zte-agent-mihomo. This may use cellular data. Does not start the proxy." } }), html! {
                    li { button.it.cv.snd type="submit" { (if installed { "Reinstall pinned core & dashboard" } else { "Install core & Zashboard" }) } }
                }))
            }
        }
        @if installed && !recovery { (super::mihomo_profiles::panel(pd)) }
        p.actions { a.btn href="/ui/router" { "Back to Router & proxy switch" } }
    }
}

pub fn relay_page(pd: &PageData) -> Markup {
    let Some(s) = pd.read(0) else {
        return html! { p.msg.err { "Wi-Fi relay status unavailable" } };
    };
    let connected = s["connected"].as_bool() == Some(true);
    let recovery = s["recovery_required"].as_bool() == Some(true);
    let autostart = s["autostart"].as_bool() == Some(true);
    // Resuming the saved owner-selected network after startup.
    let restoring = s["restoring"].as_str().filter(|_| !connected);
    let networks = s["networks"].as_array().filter(|n| !n.is_empty());
    html! {
        div.relay-page {
            section.relay-hero aria-labelledby="relay-heading" {
                div.relay-head {
                    span.relay-glyph { (kit::icon(kit::Icon::Wifi)) }
                    span class=(if connected { "relay-state on" } else { "relay-state" }) {
                        (if connected { "Wi-Fi linked" } else if restoring.is_some() { "Reconnecting" } else if recovery { "Recovery required" } else { "Relay off" })
                    }
                }
                h2 #relay-heading { (if connected { s["ssid"].as_str().unwrap_or("Wi-Fi relay") } else { restoring.unwrap_or("Share a Wi-Fi connection") }) }
                p { (if connected { "Connected (internet not verified). Network authentication may still be required." } else if restoring.is_some() { "Trying the last selected remembered or open network. After a reboot this can take a few minutes. Stop reconnecting cancels this attempt and clears the restart choice." } else if recovery { "Some relay settings still need cleanup. Retry without scanning or connecting to Wi-Fi." } else { "Use nearby Wi-Fi as your U60's internet connection." }) }
                div.relay-path aria-label="Upstream Wi-Fi to U60 Pro to your devices" {
                    span.relay-hop { (kit::icon(kit::Icon::Wifi)) "Wi-Fi" }
                    (kit::icon(kit::Icon::Arrows))
                    span.relay-hop { (kit::icon(kit::Icon::Router)) "U60 Pro" }
                    (kit::icon(kit::Icon::Arrows))
                    span.relay-hop { (kit::icon(kit::Icon::Laptop)) "Devices" }
                }
                @if connected {
                    div.relay-address { span { "UPSTREAM IPv4" } code { (s["address"].as_str().unwrap_or("")) } }
                    @if let (Some(frequency), Some(width)) = (s["frequency"].as_u64(), s["width"].as_u64()) {
                        div.relay-address {
                            span { "UPSTREAM LINK" }
                            span { (if frequency < 3000 { "2.4 GHz" } else { "5 GHz" }) " \u{b7} " (width) " MHz" @if let Some(state) = s["width_state"].as_str() { " \u{b7} " (state) } }
                        }
                    }
                    div.relay-address {
                        span { "BANDWIDTH POLICY" }
                        span { (width_label(s["width_mode"].as_str().unwrap_or("off"))) }
                    }
                    @if let Some(id) = s["saved_network_id"].as_str().filter(|id| !id.is_empty()) {
                        (width_form(pd, id, "relay-active-width", s["width_mode"].as_str().unwrap_or("off"), true))
                    }
                    div.relay-address {
                        span { "AFTER A RESTART" }
                        span { (match s["autostart"].as_bool() {
                            Some(true) => "Automatic reconnect enabled",
                            Some(false) => "Stays off; no restart choice saved",
                            None => "Unknown",
                        }) }
                    }
                    div.relay-address {
                        span { "UPSTREAM IPv6 / NAT66" }
                        span {
                            (match s["ipv6"]["state"].as_str().unwrap_or("off") {
                                "configured" => "Configured (internet not verified)",
                                "detecting" => "Discovering upstream IPv6",
                                "no_upstream" => "No usable upstream IPv6; IPv4 only",
                                "unsupported" => "Unavailable; IPv4 only",
                                "expired" => "Upstream IPv6 expired; IPv4 only",
                                "error" => "IPv6 setup failed; IPv4 only",
                                _ => "Off",
                            })
                        }
                    }
                    @if s["ipv6"]["upstream"].as_bool() == Some(true) {
                        div.relay-address { span { "UPSTREAM IPv6" } code { (s["ipv6"]["address"].as_str().unwrap_or("")) } }
                        div.relay-address { span { "LAN IPv6 PREFIX" } code { (s["ipv6"]["lan_prefix"].as_str().unwrap_or("")) } }
                    }
                    @if let Some(e) = s["ipv6"]["error"].as_str() { p.msg.err { (e) } }
                    @if let Some(e) = s["clock_error"].as_str() {
                        div.msg.info role="status" {
                            strong { "Wi-Fi connected; time not verified." }
                            p { (e) }
                            form method="post" action=(CLOCK) {
                                (pd.csrf()) (once_field(&pd.once()))
                                button.relay-btn.relay-secondary type="submit" data-busy-label="Verifying time over Wi-Fi..." {
                                    (kit::icon(kit::Icon::ClockArrow)) "Recheck time"
                                }
                            }
                            p.foot { "Retry after upstream internet or portal access is restored. This check does not reconnect Wi-Fi." }
                        }
                    }
                }
                @if autostart && !connected && restoring.is_none() {
                    p.relay-copy { "A restart connection choice is still saved. Disconnect clears it and cancels any pending restoration." }
                }
                @if connected || recovery || restoring.is_some() || autostart {
                    form method="post" action=(DISCONNECT) {
                        (pd.csrf()) (once_field(&pd.once()))
                        button.relay-btn.relay-stop type="submit" data-busy-label="Disconnecting relay and restoring network settings..." { (kit::icon(kit::Icon::Power)) (if connected { "Disconnect relay" } else if recovery { "Retry relay cleanup" } else if restoring.is_some() { "Stop reconnecting" } else { "Disconnect & clear restart choice" }) }
                    }
                }
            }
            p.relay-caution { (kit::icon(kit::Icon::Info)) span { "Hotspots may briefly reconnect. If Wi-Fi drops, SIM data may be used only if cellular data is already on. Relay never turns it on." } }
            @if let Some(e) = s["error"].as_str() { p.msg.err role="alert" { (e) } }
            section.relay-panel aria-labelledby="networks-heading" {
                form method="post" action=(SCAN) {
                    (pd.csrf()) (once_field(&pd.once()))
                    div.relay-head {
                        div { h2 #networks-heading { "Upstream networks" } p.relay-copy { (if networks.is_some() { "Select a Wi-Fi network." } else { "Find nearby Wi-Fi." }) } }
                        button.relay-btn.relay-secondary type="submit" aria-label="Scan upstream Wi-Fi" {
                            (kit::icon(kit::Icon::Antenna)) (if networks.is_some() { "Rescan" } else { "Scan" })
                        }
                    }
                }
                @if let Some(networks) = networks {
                    form.relay-connect method="post" action=(CONNECT) {
                        (pd.csrf()) (once_field(&pd.once()))
                        fieldset.relay-networks {
                            legend.sr { "Select upstream Wi-Fi" }
                            @for n in networks {
                                label.relay-network {
                                    input type="radio" name="network" value=(n["id"].as_str().unwrap_or("")) required disabled[n["supported"].as_bool() != Some(true)];
                                    (kit::icon(kit::Icon::Wifi))
                                    span.relay-network-name {
                                        strong { (n["ssid"].as_str().unwrap_or("")) }
                                        small {
                                            (if n["frequency"].as_u64().unwrap_or(0) < 3000 { "2.4 GHz" } else { "5 GHz" })
                                            " / " (n["signal"].as_i64().unwrap_or(0)) " dBm / "
                                            (if n["supported"].as_bool() != Some(true) { "Unsupported" } else if n["security"].as_str() == Some("open") { "Open" } else { "WPA2" })
                                        }
                                        small { "Saved bandwidth: " (width_label(n["width_mode"].as_str().unwrap_or("off"))) }
                                    }
                                    @if n["remembered"].as_bool() == Some(true) { span.badge.gr { "Saved" } }
                                }
                            }
                        }
                        label.relay-password for="relay-password" { span { "Wi-Fi password" } small { "Leave empty for saved or open networks" } }
                        input.relay-input #relay-password type="password" name="password" maxlength="64" autocomplete="new-password" placeholder="Network password";
                        label.relay-consent { input type="checkbox" name="remember" value="1" checked; span { "Remember this network’s password after a successful connection." } }
                        label.relay-consent {
                            input type="checkbox" name="ipv6" value="1" checked;
                            span { "Use upstream IPv6 for Clash and enable LAN NAT66 when available (experimental)." }
                        }
                        label.relay-consent {
                            input type="checkbox" name="confirmed" value="1" checked required;
                            span { "I accept hotspot reconnection and possible cellular data charges." }
                        }
                        div.relay-bandwidth {
                            label for="relay-connect-width" { "Bandwidth" }
                            select #relay-connect-width name="width_mode" {
                                option value="saved" selected { "Use saved choice" }
                                (width_options("saved"))
                            }
                            small { "New networks keep negotiated bandwidth. Each Wi-Fi remembers its choice." }
                        }
                        button.relay-btn.relay-primary type="submit" { "Connect to Wi-Fi" (kit::icon(kit::Icon::Arrows)) }
                        p.relay-private { (kit::icon(kit::Icon::Lock)) "Password stays private on U60. After a successful connection, the last selected remembered or open network can reconnect after restart. Disconnect clears that choice; a dropped link does not reconnect automatically while the agent runs." }
                    }
                } @else {
                    div.relay-empty { (kit::icon(kit::Icon::Wifi)) p { "Your next connection starts here." } small { "Tap Scan to discover available networks." } }
                }
            }
            @if let Some(error) = s["saved_error"].as_str() { p.msg.err { (error) } }
            @if let Some(saved) = s["saved_networks"].as_array().filter(|s| !s.is_empty()) {
                section.relay-panel {
                    h2 { "Saved Wi-Fi networks" }
                    p.relay-copy { "Bandwidth choices are saved separately from passwords. Scan and select a network to connect. Forget removes its choice and any remembered password without disconnecting an active relay." }
                    @for (index, network) in saved.iter().enumerate() {
                        @let id = network["id"].as_str().unwrap_or("");
                        @let active = connected && s["saved_network_id"].as_str() == Some(id);
                        @let mode = network["width_mode"].as_str().unwrap_or("off");
                        div.relay-connect {
                            div.relay-head {
                                span.relay-network-name {
                                    strong { (network["ssid"].as_str().unwrap_or("")) }
                                    small { (if network["security"].as_str() == Some("open") { "Open network; no password needed" } else if network["remembered"].as_bool() == Some(true) { "Password remembered; leave it empty when connecting" } else { "Password not saved; enter it when connecting" }) }
                                    small { "Saved bandwidth: " (width_label(mode)) }
                                }
                                @if active { span.badge.gr { "Connected" } }
                            }
                            (width_form(pd, id, &format!("relay-saved-width-{index}"), mode, active))
                            form method="post" action=(FORGET) {
                                (pd.csrf()) (once_field(&pd.once()))
                                input type="hidden" name="id" value=(id);
                                p.actions { button.btn type="submit" { "Forget" } }
                            }
                        }
                    }
                }
            }
            details.relay-notes {
                summary { "Compatibility & safety" }
                p { "Experimental B31 AP + STA, IPv4 NAT. Keep both main hotspots enabled and Guest WiFi off. Do not change WiFi, LAN or USB settings while connected." }
                p { "Open and WPA2-Personal networks on radio-enabled 2.4 GHz and 5 GHz channels, including DFS. DFS may delay hotspot availability during radar checks. WPA3-only, enterprise and hidden SSIDs are not supported." }
                p { "Optional upstream IPv6 with LAN SLAAC/NAT66. Without a usable upstream IPv6 address and default route, relay stays IPv4-only. Cellular IPv6 bypass stays blocked. Internet access and hotel authorization are not verified automatically." }
                p { "IPv6 needs the existing LAN odhcpd service and kernel NAT support. Its temporary settings are restored on disconnect; do not change the vendor LAN/DHCP settings while relaying. Restart resumes only the saved connection choice, not every remembered network. Upgrading an older agent cannot infer that choice from saved credentials." }
                p { "Refresh this page to read the current state. No background scanning or polling runs while relay is off and no startup restoration is pending." }
            }
        }
    }
}

fn width_label(mode: &str) -> &'static str {
    if mode == "auto" { "Auto tuning" } else { "Keep negotiated bandwidth" }
}

fn width_options(selected: &str) -> Markup {
    html! {
        @for mode in ["off", "auto"] {
            option value=(mode) selected[selected == mode] { (width_label(mode)) }
        }
    }
}

fn width_form(pd: &PageData, id: &str, select_id: &str, mode: &str, active: bool) -> Markup {
    html! {
        form method="post" action=(TUNING) {
            (pd.csrf()) (once_field(&pd.once()))
            input type="hidden" name="id" value=(id);
            div.relay-bandwidth {
                label for=(select_id) { "Bandwidth" }
                select id=(select_id) name="width_mode" { (width_options(mode)) }
            }
            p.relay-copy { "Auto tuning may briefly interrupt traffic while adjusting bandwidth." }
            @if active {
                label.relay-consent {
                    input type="checkbox" name="confirmed" value="1" checked required;
                    span { "Applying reconnects this Wi-Fi and briefly interrupts internet access. Cellular data may be used if already enabled." }
                }
            } @else {
                input type="hidden" name="confirmed" value="1";
                p.relay-copy { "Saved for the next connection. If this Wi-Fi is connected when you apply, it will reconnect." }
            }
            p.actions { button.relay-btn.relay-secondary type="submit" { "Apply" } }
        }
    }
}

pub fn custom_post(path: &str) -> bool {
    [CONFIG, CONNECT, FORGET, CLOCK, TUNING, super::reservations::APPLY].contains(&path)
}

pub async fn post(ctx: Ctx, req: Request) -> Response {
    let nonce = CspNonce::of(&req);
    let path = req.uri().path().to_owned();
    let home = if path == super::reservations::APPLY { super::reservations::PATH } else if [CONNECT, FORGET, CLOCK, TUNING].contains(&path.as_str()) { RELAY } else { MIHOMO };
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        return session::reply_unread(&parts.headers, body, session::login_redirect(&HttpMethod::POST, home, None)).await;
    };
    if !api::header(&parts.headers, "content-type").is_some_and(|s| s.split(';').next() == Some("application/x-www-form-urlencoded")) {
        return session::reply_unread(&parts.headers, body, render::bad_request(&nonce)).await;
    }
    let max = if path == CONFIG { crate::mihomo::MAX_CONFIG * 3 + 4096 } else { session::FORM_MAX };
    let form = match api::read_capped(&parts.headers, body, max, ctx.body_wait).await {
        Ok(b) => Form::parse(&b),
        Err(_) => return render::error(413, "Form too large", "The request was too large or incomplete.", &nonce),
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&nonce, home);
    }
    let width_mode = if path == CONNECT || path == TUNING {
        if form.get("confirmed") != Some("1") {
            return render::error(400, "Confirmation required", "Confirm the hotspot interruption and possible cellular data charges.", &nonce);
        }
        match form.get("width_mode") {
            Some(mode @ ("off" | "auto")) => Some(mode),
            None | Some("saved") if path == CONNECT => None,
            _ => return render::error(400, "Invalid bandwidth choice", "Choose negotiated bandwidth or experimental Auto. When connecting, you can also use the network’s saved choice.", &nonce),
        }
    } else { None };
    let api_path = if path == super::reservations::APPLY { "/api/router/reservations" } else if path == CONNECT { "/api/wifi-relay/connect" } else if path == FORGET { "/api/wifi-relay/forget" } else if path == CLOCK { "/api/wifi-relay/clock" } else if path == TUNING { "/api/wifi-relay/tuning" } else { "/api/mihomo/config" };
    let (method, data, success) = if path == super::reservations::APPLY {
        (Method::Post, json!({"action":form.get("action").unwrap_or(""),"id":form.get("id").unwrap_or(""),"name":form.get("name").unwrap_or(""),"mac":form.get("mac").unwrap_or(""),"ip":form.get("ip").unwrap_or("")}), "Reservation updated. Reconnect the client to renew its IPv4 lease.")
    } else if path == FORGET {
        (Method::Post, json!({"id":form.get("id").unwrap_or("")}), "Saved Wi-Fi choice and any remembered password removed. Active relay is unchanged.")
    } else if path == CLOCK {
        (Method::Post, json!({}), "Time verified over Wi-Fi. Relay connection was not restarted.")
    } else if path == TUNING {
        (Method::Post, json!({"id":form.get("id").unwrap_or(""),"width_mode":width_mode,"confirmed":true}), "Wi-Fi bandwidth choice saved.")
    } else if path == CONNECT {
        (
            Method::Post,
            json!({"network":form.get("network").unwrap_or(""),"password":form.get("password").unwrap_or(""),"confirmed":true,"ipv6":form.get("ipv6")==Some("1"),"remember":form.get("remember")==Some("1"),"width_mode":width_mode}),
            "Wi-Fi relay connected. Internet access still needs verification; a hotel may require web login.",
        )
    } else {
        (Method::Put, json!({"yaml":form.get("yaml").unwrap_or("")}), "Mihomo configuration validated and saved. Use the switch in Router to start Clash.")
    };
    let token = form.get("once").unwrap_or("").to_owned();
    let digest = job::digest(&[&path, &data.to_string()]);
    let owner = p.id;
    let flash = Arc::clone(&ctx.flash);
    let work: OnceJob = Box::new(move |app: &AppState, claim| {
        claim.disarm();
        let result = super::call(app, method, api_path, &data);
        claim.set(match &result {
            Ok(_) => Token::Sent(claim.digest()),
            Err((s, e)) => Token::Failed(claim.digest(), *s, e.clone()),
        });
        flash_line(&flash, app, owner, home, result.map(|_| success.to_owned()).map_err(|(_, e)| e))
    });
    let line = |r| flash_line(&ctx.flash, &ctx.app, owner, home, r);
    match job::once(&ctx, owner, &nonce, &token, digest, Lane::Slow, SETTLE_SLOW, work).await {
        Sent::Ran(r) => r,
        Sent::Done(Ok(())) => line(Ok(success.into())),
        Sent::Done(Err((_, e))) => line(Err(e)),
        Sent::Expired => line(Err("This form expired. Refresh and try again.".into())),
        Sent::Unknown => line(Err("The operation may still be running. Check before repeating it.".into())),
        Sent::Busy => line(Err("The router is busy. Nothing was changed.".into())),
    }
}

pub(super) fn flash_line(flash: &crate::web::store::Flash, app: &AppState, owner: crate::auth::SessionId, home: &str, result: Result<String, String>) -> Response {
    let Ok(id) = app.auth.random_hex(8) else { return session::see_other(home) };
    let (ok, text) = match result {
        Ok(s) => (true, s),
        Err(e) => (false, e),
    };
    flash.insert(id.clone(), owner, (Some(ok), text), app.auth.now());
    let separator = if home.contains('?') { '&' } else { '?' };
    session::see_other(&format!("{home}{separator}r={id}"))
}

pub fn open(ctx: &Ctx) -> Response {
    if !ctx.app.mihomo.running() {
        return session::see_other("/ui/router");
    }
    session::see_other(&crate::web::mihomo::dashboard_link())
}
