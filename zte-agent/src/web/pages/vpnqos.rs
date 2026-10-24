//! Router › VPN Passthrough (iOS `VPNPassthroughView`: L2TP, PPTP and
//! IPSec, then Apply) and QoS (`QoSView`: "Enable QoS", applied at once),
//! read at page load (no stream). Both post from their page with its
//! single-use token, as the app's (nothing is asked: neither opens a device
//! to the internet or cuts access to the router), with the app's bodies and
//! lines.
//!
//! - A failed read shows only the error on VPN Passthrough (the app would
//!   show every switch off, and Apply would turn them all off), and the
//!   error with the switch "Unavailable" (both directions offered) on QoS.
//! - The firmware's `ubus -v list` names no passthrough switches (its
//!   `router_set_alg_switch` takes only `alg_sip_enable`) and no
//!   `router_get_qos_switch` / `router_set_qos_switch` (it has
//!   `router_get_qos` / `router_set_qos`), which the agent's routes call:
//!   on this firmware these pages may only show the router's error (not
//!   verified on the device; the app's too).

use maud::{html, Markup};
use serde_json::{json, Map, Value};

use super::netcfg::{check, failed, switch, truthy, val, Done, Kind, Vals};
use crate::handlers::AppState;
use crate::server::Method;
use crate::web::fmt;
use crate::web::kit::{self, Act};
use crate::web::render::once_field;
use crate::web::screen::PageData;
use crate::web::store::Claim;

pub const VPN: &str = "/ui/router/vpn";
pub const VPN_APPLY: &str = "/ui/router/vpn/apply";
pub const QOS: &str = "/ui/router/qos";
pub const QOS_SWITCH: &str = "/ui/router/qos/switch";
const VPN_API: &str = "/api/router/vpn";
const QOS_API: &str = "/api/router/qos";

/// The app's switches: (field, label, the API's key).
const PASSTHROUGH: [(&str, &str, &str); 3] =
    [("l2tp", "L2TP", "l2tp_passthrough"), ("pptp", "PPTP", "pptp_passthrough"), ("ipsec", "IPSec", "ipsec_passthrough")];

pub fn vpn_page(pd: &PageData) -> Markup {
    let d = match pd.reads.first() {
        Some(Ok(d)) => d,
        Some(Err(e)) => return html! { p.msg.err role="alert" { "Failed to load VPN: " (fmt::truncate(e, 120)) } },
        None => return html! {},
    };
    html! {
        form.rf method="post" action=(VPN_APPLY) {
            (pd.csrf())
            (once_field(&pd.once()))
            (kit::section(Some("VPN Passthrough"), None, html! {
                @for (name, label, key) in PASSTHROUGH { (switch(name, label, truthy(&d[key]), "it", "sr")) }
            }))
            (kit::section(None, None, html! { li { button.it.cv.snd type="submit" { "Apply" } } }))
        }
    }
}

pub fn qos_page(pd: &PageData) -> Markup {
    let state = match pd.reads.first() {
        Some(Ok(d)) => Some(truthy(&d["qos_switch"])),
        _ => None,
    };
    let hidden = html! { (pd.csrf()) (once_field(&pd.once())) };
    let act = |on: &'static str| Act::Post { action: QOS_SWITCH, name: "on", value: on };
    html! {
        @if let Some(Err(e)) = pd.reads.first() { p.msg.err role="alert" { "Failed to load QoS: " (fmt::truncate(e, 120)) } }
        (kit::section(Some("Quality of Service"), None, kit::switch_row(&hidden, "Enable QoS", state, act("1"), act("0"))))
    }
}

/// The three switches (checkboxes: "1" when sent).
pub fn vpn_fields(pairs: &[(String, String)]) -> Vals {
    PASSTHROUGH.iter().map(|(name, ..)| (*name, check(pairs, name))).collect()
}

pub fn send(kind: Kind, app: &AppState, vals: &[(&'static str, String)], claim: &mut Claim) -> Result<Done, (u16, String)> {
    let put = |path: &str, body: Value| super::call(app, Method::Put, path, &body).map_err(|(status, e)| (status, failed(&e)));
    let bit = |on: bool| if on { "1" } else { "0" };
    if kind == Kind::Qos {
        let on = val(vals, "on") == "1";
        claim.disarm();
        put(QOS_API, json!({"qos_switch": bit(on)}))?;
        return Ok(Done::Line(Some(true), format!("QoS {}", if on { "enabled" } else { "disabled" })));
    }
    let body: Map<String, Value> = PASSTHROUGH.iter().map(|(name, _, key)| ((*key).to_owned(), bit(val(vals, name) == "1").into())).collect();
    claim.disarm();
    put(VPN_API, Value::Object(body))?;
    Ok(Done::Line(Some(true), "VPN passthrough updated".into()))
}
