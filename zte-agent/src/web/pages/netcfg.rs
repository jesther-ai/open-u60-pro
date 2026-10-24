//! Router › WiFi, Guest WiFi, APN, LAN / DHCP, DNS, Firewall, Telemetry
//! Blocker, VPN Passthrough and QoS: their changes. Most are asked first
//! (the app asks none of them; each restarts Wi-Fi, can cut mobile data,
//! moves the router's address, changes how every device looks up names,
//! or opens devices to the internet or cuts access to them); the Telemetry
//! Blocker's switch, Add and Block All, VPN Passthrough's Apply and the QoS
//! switch post from their page, as the app's.
//!
//! Three shapes:
//! - **Drafted** (a form with many fields or a password: WiFi, Guest WiFi,
//!   New / Edit APN, LAN / DHCP, DNS, DMZ, New Port Forward). The form
//!   posts to `<path>` with its page's single-use token. A refused form
//!   comes back as typed (`400`);
//!   a password is never drawn: one typed and valid is kept in the draft
//!   under the token and its field says so. A valid form becomes a draft
//!   (`store::Drafts`, in RAM, by the token, with a random revision), then
//!   `303` to its question, `<path>?t=<token>`, drawn from the draft. The
//!   question's form posts only the token, the revision (`d`) and
//!   `confirm=1`: no field, so no password, is ever in a page. Cancel leads
//!   back to the form as typed (`<form>?t=<token>`); a post of an older
//!   copy of the form gives the draft a new revision, so a question shown
//!   for an earlier post sends nothing.
//! - **Asked by link** (APN mode, Activate, Delete; Clear Cache; every
//!   Firewall switch, the level, a port forward's delete; a blocked
//!   domain's removal): `GET <path>?<fields>` is the question; its form
//!   posts the fields, a new single-use token and `confirm=1`. A delete
//!   names the list it was chosen from (`g`, its generation): rule ids are
//!   list positions on some firmware, so a list that changed since sends
//!   nothing.
//! - **Posted from the page** (`Kind::asks` false): the page's form posts
//!   the fields with the page's single-use token and is sent at once; a
//!   refused field shows the page again as typed (`400`).
//!
//! The confirmed post claims the token right before the first device call
//! (given back if nothing is sent: a read failed, the router was busy), so
//! a double click sends once and a replay lands where the first post did:
//! a line on the screen (`?r=<token>`; a failure of a drafted form shows
//! the form again as typed), or the result page (WiFi's and Guest WiFi's
//! "Reconnect to …", LAN's new address), drawn from the draft once its
//! passwords are dropped, without a device read. A send that failed keeps
//! the passwords: the form shown again gets a new token (the old one is
//! spent) and they go with it (`carry`), so a retry sends them.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::Method as HttpMethod;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::Value;

use super::confirm::{Bad, Question};
use super::{apn, dns, firewall, lan, telemetry, vpnqos, wlan};
use crate::auth::{Principal, SessionId};
use crate::handlers::AppState;
use crate::httpd::gate::{self, Lane};
use crate::httpd::{api, Ctx, Peer};
use crate::web::action::{self, Ask};
use crate::web::job::{self, OnceJob, Sent};
use crate::web::kit;
use crate::web::ops::is_op_id;
use crate::web::render::{self, PageCtx};
use crate::web::screen::{self, FormState, Req, Screen};
use crate::web::session::{self, percent_encode, query_param, Form};
use crate::web::store::{Draft as Held, Drafts, Flash, Token, SETTLE, SETTLE_SLOW};
use crate::web::tabs::router;
use crate::web::{fmt, CspNonce};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    Wifi,
    Guest,
    ApnMode,
    ApnSave,
    ApnUse,
    ApnDelete,
    Lan,
    Dns,
    DnsClear,
    /// Firewall: its switch, the level, NAT, UPnP, DMZ, port forwarding's
    /// switch, a new port forward, a port forward's delete.
    FwSwitch,
    FwLevel,
    FwNat,
    FwUpnp,
    FwDmz,
    FwPfSwitch,
    FwPfAdd,
    FwPfDelete,
    /// Telemetry Blocker: the filter's switch, Add, Block All, a domain's
    /// removal.
    TmSwitch,
    TmAdd,
    TmBlockAll,
    TmDelete,
    Vpn,
    Qos,
}

const KINDS: [Kind; 23] = [
    Kind::Wifi,
    Kind::Guest,
    Kind::ApnMode,
    Kind::ApnSave,
    Kind::ApnUse,
    Kind::ApnDelete,
    Kind::Lan,
    Kind::Dns,
    Kind::DnsClear,
    Kind::FwSwitch,
    Kind::FwLevel,
    Kind::FwNat,
    Kind::FwUpnp,
    Kind::FwDmz,
    Kind::FwPfSwitch,
    Kind::FwPfAdd,
    Kind::FwPfDelete,
    Kind::TmSwitch,
    Kind::TmAdd,
    Kind::TmBlockAll,
    Kind::TmDelete,
    Kind::Vpn,
    Kind::Qos,
];

/// Checked values by field name (a password among them until it is sent).
pub type Vals = Vec<(&'static str, String)>;

/// The value of `name` ("" if absent).
pub fn val<'a>(vals: &'a [(&'static str, String)], name: &str) -> &'a str {
    vals.iter().find(|(k, _)| *k == name).map_or("", |(_, v)| v.as_str())
}

/// The password fields: never drawn into a page, a URL or a digest.
pub const SECRETS: [&str; 4] = ["key_2g", "key_5g", "gkey", "pass"];

/// What a form's values hold in place of a password the draft keeps.
pub const KEPT: &str = "\u{2022}";

/// After a send, whether a password was sent, by these names (the result
/// page says "with the new password").
fn sent_name(secret: &str) -> &'static str {
    match secret {
        "key_2g" => "set_2g",
        "key_5g" => "set_5g",
        "gkey" => "set_g",
        _ => "set_p",
    }
}

/// A posted form, kept by its single-use token. No `Debug`: it may hold a
/// password.
#[derive(Clone)]
pub struct Draft {
    kind: Kind,
    /// Random, per post of the form; the question's form names it.
    rev: String,
    /// Checked and complete; else only the passwords of a refused form.
    ready: bool,
    pub vals: Vals,
    /// After a failed send: the form's new token, which holds the passwords
    /// (here only `KEPT` stays in their place).
    next: String,
}

impl Draft {
    /// The passwords dropped once sent (whether one was, kept by name), and
    /// what the result page needs added.
    fn scrub(&mut self, extra: Vals) {
        let mut set = Vec::new();
        for (k, v) in &mut self.vals {
            if SECRETS.contains(k) {
                if !v.is_empty() {
                    set.push((sent_name(k), "1".to_owned()));
                }
                *v = String::new();
            }
        }
        self.vals.extend(set);
        self.vals.extend(extra);
    }
}

/// Where a sent change leads.
pub enum Done {
    /// A line on its screen (`Some(true)` green as the app's; `None`
    /// neutral: done, with a part that was not).
    Line(Option<bool>, String),
    /// Its result page, with these values added to the draft.
    Page(Vals),
}

/// "Failed: <error>", cut to 120 characters.
pub fn failed(e: &str) -> String {
    format!("Failed: {}", fmt::truncate(e, 120))
}

/// A firmware value as text (strings and numbers).
pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// The app's `asBool`: "1", "true", "on", a number other than 0, `true`.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "on"),
        _ => false,
    }
}

/// A checkbox: "1" when sent, else "".
pub fn check(pairs: &[(String, String)], name: &str) -> String {
    if pairs.iter().any(|(k, _)| k == name) { "1" } else { "" }.to_owned()
}

/// The first value of `name` as posted.
pub fn field<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// The first of `keys` that `d` has (the app's name, then the firmware's).
pub fn first<'a>(d: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter().map(|k| &d[*k]).find(|v| !v.is_null()).unwrap_or(&Value::Null)
}

/// A rule list's generation (16 hex): a delete names the list it was
/// chosen from, so one made after the list changed sends nothing.
pub fn generation(list: &Value) -> String {
    format!("{:016x}", job::digest(&[&list["rule_list"].to_string()]))
}

/// A rule's id: the firmware's (a section name), else its place in the
/// list (the app's fallback).
pub fn rule_id(rule: &Value, i: usize) -> String {
    let id = text(first(rule, &["id", "section_id"]));
    if id.is_empty() {
        i.to_string()
    } else {
        id
    }
}

/// `on`: "1" or "0", else a value no form sends.
pub fn on_fields(pairs: &[(String, String)]) -> Result<Vals, Bad> {
    match field(pairs, "on") {
        Some(v @ ("0" | "1")) => Ok(vec![("on", v.to_owned())]),
        _ => Err(Bad::Request),
    }
}

/// A rule's delete: its id (1-64 visible ASCII characters: a section name
/// such as `cfg0a1b2c` or `@redirect[0]`, or a place in the list; only
/// ever compared with the list read again) and the generation of the list
/// it was chosen from.
pub fn rule_fields(pairs: &[(String, String)]) -> Result<Vals, Bad> {
    let id_ok = |id: &str| (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_graphic());
    let g_ok = |g: &str| g.len() == 16 && g.bytes().all(|b| b.is_ascii_hexdigit());
    match (field(pairs, "id"), field(pairs, "g")) {
        (Some(id), Some(g)) if id_ok(id) && g_ok(g) => Ok(vec![("id", id.to_owned()), ("g", g.to_owned())]),
        _ => Err(Bad::Request),
    }
}

impl Kind {
    pub fn path(self) -> &'static str {
        match self {
            Kind::Wifi => wlan::APPLY,
            Kind::Guest => wlan::GUEST_APPLY,
            Kind::ApnMode => apn::MODE,
            Kind::ApnSave => apn::SAVE,
            Kind::ApnUse => apn::ACTIVATE,
            Kind::ApnDelete => apn::DELETE,
            Kind::Lan => lan::APPLY,
            Kind::Dns => dns::APPLY,
            Kind::DnsClear => dns::CLEAR,
            Kind::FwSwitch => firewall::SWITCH,
            Kind::FwLevel => firewall::LEVEL,
            Kind::FwNat => firewall::NAT,
            Kind::FwUpnp => firewall::UPNP,
            Kind::FwDmz => firewall::DMZ,
            Kind::FwPfSwitch => firewall::PF_SWITCH,
            Kind::FwPfAdd => firewall::PF_ADD,
            Kind::FwPfDelete => firewall::PF_DELETE,
            Kind::TmSwitch => telemetry::SWITCH,
            Kind::TmAdd => telemetry::ADD,
            Kind::TmBlockAll => telemetry::BLOCK_ALL,
            Kind::TmDelete => telemetry::DELETE,
            Kind::Vpn => vpnqos::VPN_APPLY,
            Kind::Qos => vpnqos::QOS_SWITCH,
        }
    }

    pub fn by_path(path: &str) -> Option<Kind> {
        KINDS.into_iter().find(|k| k.path() == path)
    }

    /// Its form comes as a draft (else its fields are in the question's URL).
    fn drafted(self) -> bool {
        matches!(self, Kind::Wifi | Kind::Guest | Kind::ApnSave | Kind::Lan | Kind::Dns | Kind::FwDmz | Kind::FwPfAdd)
    }

    /// It is asked first; else its page's form posts it at once (as the
    /// app's buttons and switches), with the page's single-use token.
    fn asks(self) -> bool {
        !matches!(self, Kind::TmSwitch | Kind::TmAdd | Kind::TmBlockAll | Kind::Vpn | Kind::Qos)
    }

    /// The screen of its form (an APN's: New, or Edit with its `id`).
    fn screen(self, id: &str) -> &'static Screen {
        match self {
            Kind::Wifi => &router::WIFI,
            Kind::Guest => &router::GUEST,
            Kind::ApnSave if !id.is_empty() => &router::APN_EDIT,
            Kind::ApnSave => &router::APN_NEW,
            Kind::ApnMode | Kind::ApnUse | Kind::ApnDelete => &router::APN,
            Kind::Lan => &router::LAN,
            Kind::Dns | Kind::DnsClear => &router::DNS,
            Kind::FwPfAdd => &router::PF_NEW,
            Kind::FwSwitch | Kind::FwLevel | Kind::FwNat | Kind::FwUpnp | Kind::FwDmz | Kind::FwPfSwitch | Kind::FwPfDelete => &router::FIREWALL,
            Kind::TmSwitch | Kind::TmAdd | Kind::TmBlockAll | Kind::TmDelete => &router::TELEMETRY,
            Kind::Vpn => &router::VPN,
            Kind::Qos => &router::QOS,
        }
    }

    /// Where its lines show (the app's message row).
    pub fn home(self) -> &'static str {
        match self {
            Kind::Wifi => wlan::WIFI,
            Kind::Guest => wlan::GUEST,
            Kind::ApnMode | Kind::ApnSave | Kind::ApnUse | Kind::ApnDelete => apn::PATH,
            Kind::Lan => lan::PATH,
            Kind::Dns | Kind::DnsClear => dns::PATH,
            Kind::FwSwitch | Kind::FwLevel | Kind::FwNat | Kind::FwUpnp | Kind::FwDmz | Kind::FwPfSwitch | Kind::FwPfAdd | Kind::FwPfDelete => {
                firewall::PATH
            }
            Kind::TmSwitch | Kind::TmAdd | Kind::TmBlockAll | Kind::TmDelete => telemetry::PATH,
            Kind::Vpn => vpnqos::VPN,
            Kind::Qos => vpnqos::QOS,
        }
    }

    /// Its result page.
    fn result(self) -> Option<&'static str> {
        match self {
            Kind::Wifi => Some(wlan::APPLIED),
            Kind::Guest => Some(wlan::GUEST_APPLIED),
            Kind::Lan => Some(lan::APPLIED),
            _ => None,
        }
    }

    /// The lane of its calls: DNS turns DNS over HTTPS on or off (dnsmasq
    /// restarts: the API's slow lane).
    fn lane(self) -> Lane {
        if self == Kind::Dns {
            Lane::Slow
        } else {
            Lane::General
        }
    }
}

/// The form screen whose `?t=<token>` shows a draft of this kind.
fn form_kind(path: &str) -> Option<Kind> {
    match path {
        wlan::WIFI => Some(Kind::Wifi),
        wlan::GUEST => Some(Kind::Guest),
        apn::NEW | apn::EDIT => Some(Kind::ApnSave),
        lan::PATH => Some(Kind::Lan),
        dns::PATH => Some(Kind::Dns),
        firewall::PATH => Some(Kind::FwDmz),
        firewall::NEW => Some(Kind::FwPfAdd),
        _ => None,
    }
}

/// The result page at `path`.
pub fn result_kind(path: &str) -> Option<Kind> {
    KINDS.into_iter().find(|k| k.result() == Some(path))
}

/// The form again with what was typed (`t`: its token; Edit APN its `id`).
fn form_url(kind: Kind, vals: &[(&'static str, String)], token: &str) -> String {
    let id = val(vals, "id");
    let s = kind.screen(id);
    if id.is_empty() {
        format!("{}?t={token}", s.path)
    } else {
        format!("{}?id={}&t={token}", s.path, percent_encode(id))
    }
}

/// The draft of `token`, of this kind and session.
fn draft_of(drafts: &Drafts, token: &str, owner: SessionId, now: crate::boottime::BootInstant, kind: Kind) -> Option<Draft> {
    match drafts.get(token, owner, now) {
        Some(Held::Router(d)) if d.kind == kind => Some(d),
        _ => None,
    }
}

/// Keep `d` as the draft of `token` (replacing an earlier one).
fn put(drafts: &Drafts, token: &str, owner: SessionId, d: Draft, now: crate::boottime::BootInstant) {
    drafts.remove(token, Some(owner), now);
    drafts.insert(token.to_owned(), owner, Held::Router(d), now);
}

/// A form's values without passwords: a kept one as `KEPT`.
fn shown(vals: &[(&'static str, String)]) -> Vec<(&'static str, String)> {
    vals.iter()
        .map(|(k, v)| match (SECRETS.contains(k), v.is_empty()) {
            (true, false) => (*k, KEPT.to_owned()),
            (true, true) => (*k, String::new()),
            _ => (*k, v.clone()),
        })
        .collect()
}

/// A form screen's GET with `?t=<token>` (the question's Cancel, a
/// failure): the draft's values as typed, if the session has that draft
/// and it was not sent; its page then keeps the token while it is unused.
/// After a failed send the page uses the token its passwords went to.
pub fn typed(ctx: &Ctx, p: &Principal, path: &str, r: &mut Req) {
    let Some(kind) = form_kind(path) else { return };
    let Some(t) = query_param(r.query.as_deref(), "t").filter(|t| is_op_id(t)) else { return };
    let now = ctx.app.auth.now();
    let failed = match ctx.once.get(&t, p.id, now) {
        Some(Token::Sent(_)) | None => return,
        Some(Token::Failed(..)) => true,
        Some(_) => false,
    };
    let Some(mut d) = draft_of(&ctx.drafts, &t, p.id, now, kind).filter(|d| d.ready) else { return };
    if failed {
        match carry(ctx, p.id, kind, &t, &d) {
            Some(next) => r.query = r.query.as_deref().map(|q| with_token(q, &next)),
            // Gone (used, expired): no password to keep.
            None => d.vals.iter_mut().filter(|(k, _)| SECRETS.contains(k)).for_each(|(_, v)| v.clear()),
        }
    }
    r.typed = Some(FormState { values: shown(&d.vals), errors: Vec::new() });
}

/// A failed send's draft `d` (of token `t`, spent): the token of a new
/// draft holding its passwords (as a refused form's), which the form shown
/// again posts with, so the retry sends them unless others are typed. One
/// per draft, reused while unused (a reload issues no more); `None` when
/// there are none (or they went with a token since used or gone; a gone
/// token's draft, which nothing can reach, is dropped with them).
fn carry(ctx: &Ctx, owner: SessionId, kind: Kind, t: &str, d: &Draft) -> Option<String> {
    let now = ctx.app.auth.now();
    if !d.next.is_empty() {
        let state = ctx.once.get(&d.next, owner, now);
        if state.is_none() {
            ctx.drafts.remove(&d.next, Some(owner), now);
        }
        let held = state == Some(Token::Fresh) && draft_of(&ctx.drafts, &d.next, owner, now, kind).is_some();
        return held.then(|| d.next.clone());
    }
    let keep: Vals = d.vals.iter().filter(|(k, v)| SECRETS.contains(k) && !v.is_empty() && v != KEPT).cloned().collect();
    if keep.is_empty() {
        return None;
    }
    let next = ctx.once.fresh(&ctx.app.auth, owner);
    if next.is_empty() {
        return None;
    }
    put(&ctx.drafts, &next, owner, Draft { kind, rev: String::new(), ready: false, vals: keep, next: String::new() }, now);
    // One copy of each password: the spent draft keeps only `KEPT`.
    ctx.drafts.update(t, owner, now, |h| {
        if let Held::Router(x) = h {
            x.next.clone_from(&next);
            x.vals.iter_mut().filter(|(k, v)| SECRETS.contains(k) && !v.is_empty()).for_each(|(_, v)| *v = KEPT.to_owned());
        }
    });
    Some(next)
}

/// `q` with its `t=` replaced by `token`.
fn with_token(q: &str, token: &str) -> String {
    let pairs: Vec<String> = q.split('&').map(|kv| if kv.starts_with("t=") { format!("t={token}") } else { kv.to_owned() }).collect();
    pairs.join("&")
}

/// A drafted kind's form, checked (in a gate task: some read the router's
/// state). `kept`: the passwords its draft keeps from a refused post.
fn check_form(kind: Kind, app: &AppState, pairs: &[(String, String)], kept: &Vals) -> Result<Vals, Bad> {
    match kind {
        Kind::Wifi => wlan::check(app, pairs, kept),
        Kind::Guest => wlan::check_guest(app, pairs, kept),
        Kind::ApnSave => apn::check(app, pairs, kept),
        Kind::Lan => lan::check(pairs),
        Kind::Dns => dns::check(app, pairs),
        Kind::FwDmz => firewall::check_dmz(app, pairs),
        Kind::FwPfAdd => firewall::check_rule(app, pairs),
        _ => Err(Bad::Request),
    }
}

/// The fields of a kind asked by link or posted from its page
/// (`Bad::Fields`: a typed value refused, the page again as typed).
fn fields(kind: Kind, pairs: &[(String, String)]) -> Result<Vals, Bad> {
    match kind {
        Kind::ApnMode => apn::mode_fields(pairs),
        Kind::ApnUse | Kind::ApnDelete => apn::id_fields(pairs),
        Kind::DnsClear | Kind::TmBlockAll => Ok(Vec::new()),
        Kind::FwSwitch | Kind::FwNat | Kind::FwUpnp | Kind::FwPfSwitch | Kind::TmSwitch | Kind::Qos => on_fields(pairs),
        Kind::FwLevel => firewall::level_fields(pairs),
        Kind::FwPfDelete | Kind::TmDelete => rule_fields(pairs),
        Kind::TmAdd => telemetry::add_fields(pairs),
        Kind::Vpn => Ok(vpnqos::vpn_fields(pairs)),
        _ => Err(Bad::Request),
    }
}

fn query_of(vals: &[(&'static str, String)]) -> String {
    let pairs: Vec<String> = vals.iter().map(|(k, v)| format!("{k}={}", percent_encode(v))).collect();
    pairs.join("&")
}

/// The question (read-only; it may read the router's state).
fn question(kind: Kind, app: &AppState, vals: &[(&'static str, String)], remote: bool, port: &str) -> Question {
    match kind {
        Kind::Wifi => wlan::question(app, vals),
        Kind::Guest => wlan::guest_question(app, vals),
        Kind::ApnMode | Kind::ApnSave | Kind::ApnUse | Kind::ApnDelete => apn::question(kind, app, vals, remote),
        Kind::Lan => lan::question(app, vals, port, remote),
        Kind::Dns | Kind::DnsClear => dns::question(kind, app, vals, remote),
        Kind::FwSwitch | Kind::FwLevel | Kind::FwNat | Kind::FwUpnp | Kind::FwDmz | Kind::FwPfSwitch | Kind::FwPfAdd | Kind::FwPfDelete => {
            firewall::question(kind, app, vals, remote)
        }
        Kind::TmDelete => telemetry::question(app, vals),
        // Posted from their page: never asked.
        Kind::TmSwitch | Kind::TmAdd | Kind::TmBlockAll | Kind::Vpn | Kind::Qos => super::confirm::nothing("Nothing to ask", Vec::new()),
    }
}

/// The device calls (the claim disarmed right before the first); the line
/// of a failure, in the app's words.
fn send(kind: Kind, app: &AppState, vals: &[(&'static str, String)], claim: &mut crate::web::store::Claim) -> Result<Done, (u16, String)> {
    match kind {
        Kind::Wifi => wlan::send(app, vals, claim),
        Kind::Guest => wlan::send_guest(app, vals, claim),
        Kind::ApnMode | Kind::ApnSave | Kind::ApnUse | Kind::ApnDelete => apn::send(kind, app, vals, claim),
        Kind::Lan => lan::send(app, vals, claim),
        Kind::Dns | Kind::DnsClear => dns::send(kind, app, vals, claim),
        Kind::FwSwitch | Kind::FwLevel | Kind::FwNat | Kind::FwUpnp | Kind::FwDmz | Kind::FwPfSwitch | Kind::FwPfAdd | Kind::FwPfDelete => {
            firewall::send(kind, app, vals, claim)
        }
        Kind::TmSwitch | Kind::TmAdd | Kind::TmBlockAll | Kind::TmDelete => telemetry::send(kind, app, vals, claim),
        Kind::Vpn | Kind::Qos => vpnqos::send(kind, app, vals, claim),
    }
}

/// The port the browser used (`Host`), for links to the router's new
/// address: ":9090", or "" for the default port.
pub fn port_of(host: Option<&str>) -> String {
    let port = host.and_then(|h| match h.strip_prefix('[') {
        Some(rest) => rest.split_once("]:").map(|(_, p)| p),
        None if h.matches(':').count() == 1 => h.split_once(':').map(|(_, p)| p),
        None => None,
    });
    match port {
        Some(p) if !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()) && p != "80" => format!(":{p}"),
        _ => String::new(),
    }
}

/// The question page; its form carries the token (`once`) and, for a
/// draft, its revision, else the fields.
#[allow(clippy::too_many_arguments)]
fn ask_page(app: &AppState, p: &Principal, nonce: &str, kind: Kind, q: &Question, hidden: &[(&str, &str)], cancel: &str, id: &str) -> Response {
    let lines: Vec<&str> = q.lines.iter().map(String::as_str).collect();
    let a = Ask { title: &q.title, lines: &lines, action: kind.path(), hidden, label: q.label, danger: q.danger, cancel };
    let s = kind.screen(id);
    let pc = PageCtx {
        title: &q.title,
        tab: Some(s.tab),
        nonce,
        lead: kit::back(cancel, s.title),
        heading: Some(html! { h1 { "Confirm" } }),
        ..PageCtx::default()
    };
    render::html(200, render::page(&pc, action::ask(render::csrf_field(&app.auth, p), &a)))
}

/// `GET <path>`: the question. Drafted: `?t=<token>`, its draft (else
/// back to the form); asked by link: the fields of the query string.
pub async fn get(ctx: &Ctx, p: Principal, req: Request, kind: Kind) -> Response {
    let nonce = CspNonce::of(&req);
    let remote = session::is_remote(req.extensions().get::<Peer>());
    let query = req.uri().query().map(str::to_owned);
    let port = port_of(api::header(req.headers(), "host"));
    drop(req);
    // Posted from its page: there is no question.
    if !kind.asks() {
        return session::see_other(kind.home());
    }
    let now = ctx.app.auth.now();
    let (vals, token, rev) = if kind.drafted() {
        let t = query_param(query.as_deref(), "t").unwrap_or_default();
        match draft_of(&ctx.drafts, &t, p.id, now, kind) {
            Some(d) if d.ready && ctx.once.get(&t, p.id, now) == Some(Token::Fresh) => (d.vals, t, d.rev),
            // Sent already, or gone: the form.
            _ => return session::see_other(kind.home()),
        }
    } else {
        match fields(kind, Form::parse(query.as_deref().unwrap_or("").as_bytes()).pairs()) {
            Ok(v) => (v, String::new(), String::new()),
            Err(_) => return render::bad_request(&nonce),
        }
    };
    let (once, n) = (Arc::clone(&ctx.once), nonce.clone());
    let retry = match &query {
        Some(q) => format!("{}?{q}", kind.path()),
        None => kind.path().to_owned(),
    };
    let job = Box::new(move |app: &AppState| {
        gate::read_only(|| {
            let q = question(kind, app, &vals, remote, &port);
            let id = val(&vals, "id").to_owned();
            if kind.drafted() {
                let hidden = [("d", rev.as_str()), ("once", token.as_str())];
                return ask_page(app, &p, &n, kind, &q, &hidden, &form_url(kind, &vals, &token), &id);
            }
            let token = q.label.map(|_| once.fresh(&app.auth, p.id)).unwrap_or_default();
            let mut hidden: Vec<(&str, &str)> = vals.iter().map(|(k, v)| (*k, v.as_str())).collect();
            hidden.push(("once", &token));
            ask_page(app, &p, &n, kind, &q, &hidden, kind.home(), &id)
        })
    });
    job::run(ctx, Lane::General, &nonce, &retry, job).await
}

/// The stores an answer needs.
struct Stores {
    flash: Arc<Flash>,
    drafts: Arc<Drafts>,
    owner: SessionId,
}

/// Where a line of `kind` shows: a drafted form's failure at the form as
/// typed, every other line at the screen.
fn line_url(kind: Kind, vals: &[(&'static str, String)], token: &str, failure: bool, id: &str) -> String {
    if failure && kind.drafted() {
        format!("{}&r={id}", form_url(kind, vals, token))
    } else {
        format!("{}?r={id}", kind.home())
    }
}

/// The answer to a confirmed post: its line (kept under the token when
/// something was sent, so a replay finds it) or its result page. The
/// draft's passwords go once sent; a failed send keeps them (`carry`).
fn answer(app: &AppState, st: &Stores, kind: Kind, vals: &[(&'static str, String)], token: &str, out: Result<Done, (u16, String)>, sent: bool) -> Response {
    let now = app.auth.now();
    if sent && kind.drafted() && out.is_ok() {
        let extra = match &out {
            Ok(Done::Page(v)) => v.clone(),
            _ => Vec::new(),
        };
        st.drafts.update(token, st.owner, now, |d| {
            if let Held::Router(d) = d {
                d.scrub(extra);
            }
        });
    }
    let (ok, line) = match out {
        Ok(Done::Page(_)) => return session::see_other(&format!("{}?t={token}", kind.result().unwrap_or(kind.home()))),
        Ok(Done::Line(ok, line)) => (ok, line),
        Err((_, line)) => (Some(false), line),
    };
    // Nothing sent: the token is fresh again, the line gets an id of its own.
    let id = if sent { token.to_owned() } else { app.auth.random_hex(8).unwrap_or_default() };
    if id.is_empty() {
        return session::see_other(kind.home());
    }
    st.flash.insert(id.clone(), st.owner, (ok, line), now);
    session::see_other(&line_url(kind, vals, token, ok == Some(false), &id))
}

/// A line for this session (a random id) at the screen.
fn note(ctx: &Ctx, owner: SessionId, kind: Kind, ok: Option<bool>, line: &str) -> Response {
    let Ok(id) = ctx.app.auth.random_hex(8) else { return session::see_other(kind.home()) };
    ctx.flash.insert(id.clone(), owner, (ok, line.to_owned()), ctx.app.auth.now());
    session::see_other(&format!("{}?r={id}", kind.home()))
}

/// A post whose token was used: where the first post went (its line or
/// result page).
fn replayed(ctx: &Ctx, owner: SessionId, kind: Kind, vals: &[(&'static str, String)], token: &str) -> Option<String> {
    let now = ctx.app.auth.now();
    if let Some((ok, _)) = ctx.flash.get(token, owner, now) {
        return Some(line_url(kind, vals, token, ok == Some(false), token));
    }
    match (ctx.once.get(token, owner, now), kind.result()) {
        (Some(Token::Sent(_)), Some(page)) => Some(format!("{page}?t={token}")),
        _ => None,
    }
}

const EXPIRED: &str = "This page expired, so nothing was sent. Try again.";

/// `POST <path>`.
pub async fn post(ctx: Ctx, req: Request, kind: Kind) -> Response {
    let nonce = CspNonce::of(&req);
    let r = Req { query: None, ..Req::of(&ctx, &req) };
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, kind.path(), None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&nonce, kind.home());
    }
    let token = form.get("once").unwrap_or("").to_owned();
    let confirmed = form.get("confirm") == Some("1");
    if kind.drafted() && !confirmed {
        return drafting(ctx, p, r, kind, form, token).await;
    }
    let (vals, digest) = if kind.drafted() {
        match draft_of(&ctx.drafts, &token, p.id, ctx.app.auth.now(), kind) {
            Some(d) if d.ready && !d.rev.is_empty() && form.get("d") == Some(d.rev.as_str()) => {
                let digest = job::digest(&[kind.path(), &d.rev]);
                (d.vals, digest)
            }
            // A question for an earlier post of the form, or the draft is gone.
            d => {
                let vals = d.map(|d| d.vals).unwrap_or_default();
                return match replayed(&ctx, p.id, kind, &vals, &token) {
                    Some(to) => session::see_other(&to),
                    None => note(&ctx, p.id, kind, Some(false), EXPIRED),
                };
            }
        }
    } else {
        match fields(kind, form.pairs()) {
            Ok(v) => {
                if !confirmed && kind.asks() {
                    return session::see_other(&format!("{}?{}", kind.path(), query_of(&v)));
                }
                let digest = job::digest(&[kind.path(), &query_of(&v)]);
                (v, digest)
            }
            // The page again as typed (its token kept while unused).
            Err(Bad::Fields(fs)) => {
                let mut r = r;
                r.query = is_op_id(&token).then(|| format!("t={token}"));
                let job = Box::new(move |app: &AppState| screen::render(app, kind.screen(""), &p, &r, None, Some(&fs), 400));
                return job::run(&ctx, Lane::General, &nonce, kind.home(), job).await;
            }
            Err(Bad::Request) => return render::bad_request(&nonce),
        }
    };
    let lane = kind.lane();
    let st = Stores { flash: Arc::clone(&ctx.flash), drafts: Arc::clone(&ctx.drafts), owner: p.id };
    let (wv, wt) = (vals.clone(), token.clone());
    let work: OnceJob = Box::new(move |app: &AppState, claim| {
        let out = send(kind, app, &wv, claim);
        let sent = claim.sent();
        if sent {
            claim.set(match &out {
                Ok(_) => Token::Sent(claim.digest()),
                Err((status, line)) => Token::Failed(claim.digest(), *status, line.clone()),
            });
        }
        answer(app, &st, kind, &wv, &wt, out, sent)
    });
    let limit = if lane == Lane::Slow { SETTLE_SLOW } else { SETTLE };
    match job::once(&ctx, p.id, &nonce, &token, digest, lane, limit, work).await {
        Sent::Ran(resp) => resp,
        Sent::Done(_) | Sent::Unknown => match replayed(&ctx, p.id, kind, &vals, &token) {
            Some(to) => session::see_other(&to),
            None => note(&ctx, p.id, kind, None, "The router may still be working on it. Check before trying again."),
        },
        Sent::Expired => note(&ctx, p.id, kind, Some(false), EXPIRED),
        Sent::Busy => note(&ctx, p.id, kind, Some(false), "Failed: the router is busy. Try again in a few seconds."),
    }
}

/// A drafted form's post (not yet confirmed): checked in a gate task, then
/// its draft and question, or the form again as typed (`400`) with the
/// valid passwords kept in the draft, never in the page.
async fn drafting(ctx: Ctx, p: Principal, mut r: Req, kind: Kind, form: Form, token: String) -> Response {
    let nonce = r.nonce.clone();
    let now = ctx.app.auth.now();
    // An old copy of the form (its token used or gone): nothing to draft.
    if !is_op_id(&token) || ctx.once.get(&token, p.id, now) != Some(Token::Fresh) {
        return note(&ctx, p.id, kind, Some(false), "This form expired, so nothing was sent. Open it again.");
    }
    let kept: Vals = draft_of(&ctx.drafts, &token, p.id, now, kind)
        .map(|d| d.vals.into_iter().filter(|(k, v)| SECRETS.contains(k) && !v.is_empty()).collect())
        .unwrap_or_default();
    let drafts = Arc::clone(&ctx.drafts);
    let pairs = form.pairs().to_vec();
    let owner = p.id;
    let n = nonce.clone();
    let job = Box::new(move |app: &AppState| {
        let now = app.auth.now();
        match gate::read_only(|| check_form(kind, app, &pairs, &kept)) {
            Ok(vals) => {
                let Ok(rev) = app.auth.random_hex(8) else {
                    return render::error(503, "Try again", "The router could not keep this form right now. Nothing was changed.", &n);
                };
                put(&drafts, &token, owner, Draft { kind, rev, ready: true, vals, next: String::new() }, now);
                session::see_other(&format!("{}?t={token}", kind.path()))
            }
            Err(Bad::Request) => render::bad_request(&n),
            Err(Bad::Fields(mut fs)) => {
                // Valid passwords stay in the draft; the page only says so.
                let keep: Vals = fs.values.iter().filter(|(k, v)| SECRETS.contains(k) && !v.is_empty() && v != KEPT).cloned().collect();
                fs.values = shown(&fs.values);
                if keep.is_empty() {
                    drafts.remove(&token, Some(owner), now);
                } else {
                    put(&drafts, &token, owner, Draft { kind, rev: String::new(), ready: false, vals: keep, next: String::new() }, now);
                }
                let id = fs.value("id").unwrap_or("").to_owned();
                r.query = Some(if id.is_empty() { format!("t={token}") } else { format!("id={}&t={token}", percent_encode(&id)) });
                screen::render(app, kind.screen(&id), &p, &r, None, Some(&fs), 400)
            }
        }
    });
    job::run(&ctx, Lane::General, &nonce, kind.home(), job).await
}

/// `GET <result>?t=<token>`: after a change that can cut this browser off
/// (WiFi or Guest WiFi restarting, the LAN address moving), drawn from the sent draft
/// (its passwords dropped) without a device read.
pub fn result(ctx: &Ctx, p: &Principal, req: &Request, kind: Kind) -> Response {
    let nonce = CspNonce::of(req);
    let now = ctx.app.auth.now();
    let t = query_param(req.uri().query(), "t").unwrap_or_default();
    let sent = matches!(ctx.once.get(&t, p.id, now), Some(Token::Sent(_)));
    let vals = draft_of(&ctx.drafts, &t, p.id, now, kind).filter(|d| sent && d.ready).map(|d| d.vals).unwrap_or_default();
    let remote = session::is_remote(req.extensions().get::<Peer>());
    let (title, body): (String, Markup) = match kind {
        Kind::Lan => lan::applied(&vals, &port_of(api::header(req.headers(), "host")), remote),
        Kind::Guest => wlan::guest_applied(&vals),
        _ => wlan::applied(&vals),
    };
    let s = kind.screen("");
    let pc = PageCtx { title: &title, tab: Some(s.tab), nonce: &nonce, lead: kit::back(s.path, s.title), ..PageCtx::default() };
    render::html(200, render::page(&pc, body))
}

// --- Form rows (the app's Form rows; labels above filled fields) ---

/// A footer under a section.
pub fn foot(text: &str) -> Markup {
    html! { p.foot { (text) } }
}

fn err_attrs(name: &str, err: Option<&str>) -> (Option<&'static str>, Option<String>) {
    (err.map(|_| "true"), err.map(|_| format!("{name}-err")))
}

fn err_line(name: &str, err: Option<&str>) -> Markup {
    html! { @if let Some(e) = err { p.ferr id=(format!("{name}-err")) { (e) } } }
}

/// A switch in a form (a checkbox named `name`, posting "1" when on);
/// `input`: the checkbox's classes ("sr", plus one CSS keys on).
pub fn switch(name: &str, label: &str, on: bool, row: &str, input: &str) -> Markup {
    html! {
        li class=(row) {
            label.tl for=(name) {
                span { (label) }
                input class=(input) id=(name) type="checkbox" role="switch" name=(name) value="1" checked[on];
                span.sw aria-hidden="true" {}
            }
        }
    }
}

/// A text field with its label above it (and as its placeholder, as the
/// app's).
pub fn text_row(name: &str, label: &str, value: &str, err: Option<&str>, row: &str, max: usize, mode: Option<&str>) -> Markup {
    let (invalid, described) = err_attrs(name, err);
    html! {
        li class=(row) {
            label for=(name) { (label) }
            input id=(name) type="text" name=(name) value=(value) placeholder=(label) maxlength=(max) inputmode=[mode]
                autocomplete="off" autocapitalize="off" spellcheck="false" aria-invalid=[invalid] aria-describedby=[described];
            (err_line(name, err))
        }
    }
}

/// A password field that is never filled (owner decision): "Leave empty to
/// keep" the router's (`has`) or the one typed before (`kept`); password
/// managers are asked to leave it alone.
pub fn password_row(name: &str, label: &str, has: bool, kept: bool, err: Option<&str>, row: &str) -> Markup {
    let (invalid, described) = err_attrs(name, err);
    let hint = if kept {
        "Leave empty to keep the password you entered"
    } else if has {
        "Leave empty to keep"
    } else {
        label
    };
    html! {
        li class=(row) {
            label for=(name) { (label) }
            input id=(name) type="password" name=(name) placeholder=(hint) maxlength="64" autocomplete="new-password"
                "data-1p-ignore" data-lpignore="true" data-bwignore aria-invalid=[invalid] aria-describedby=[described];
            (err_line(name, err))
        }
    }
}

/// A picker row: `opts` (value, label), `cur` chosen.
pub fn select_row(name: &str, label: &str, opts: &[(String, String)], cur: &str, err: Option<&str>, row: &str) -> Markup {
    let (invalid, described) = err_attrs(name, err);
    html! {
        li class=(if err.is_some() { format!("{row} hr") } else { row.to_owned() }) {
            label for=(name) { (label) }
            select id=(name) name=(name) aria-invalid=[invalid] aria-describedby=[described] {
                @for (v, l) in opts { option value=(v) selected[v == cur] { (l) } }
            }
            (err_line(name, err))
        }
    }
}

/// The app's choices, with the router's value first when it is not one of
/// them (kept unless another is chosen).
pub fn options(list: &[&str], router: &str, label: fn(&str) -> String) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(list.len() + 1);
    if !list.contains(&router) {
        out.push((router.to_owned(), if router.is_empty() { "Default".to_owned() } else { label(router) }));
    }
    out.extend(list.iter().map(|v| ((*v).to_owned(), label(v))));
    out
}

/// A choice posted: one of `list`, or the router's value; anything else is
/// a value no form sends.
pub fn choice(pairs: &[(String, String)], name: &str, list: &[&str], router: &str) -> Result<String, Bad> {
    match field(pairs, name) {
        Some(v) if list.contains(&v) || v == router => Ok(v.to_owned()),
        _ => Err(Bad::Request),
    }
}

/// The form's message (a read that failed at the post).
pub fn form_error(pd: &crate::web::screen::PageData) -> Markup {
    html! { @if let Some(e) = pd.form.and_then(|f| f.error("form")) { p.msg.err role="alert" { (e) } } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_kinds_and_ports() {
        for k in KINDS {
            assert_eq!(Kind::by_path(k.path()), Some(k));
            assert!(k.home().starts_with("/ui/router/"), "{k:?}");
        }
        assert_eq!(result_kind(wlan::APPLIED), Some(Kind::Wifi));
        assert_eq!(result_kind(wlan::GUEST_APPLIED), Some(Kind::Guest));
        assert_eq!(result_kind(lan::APPLIED), Some(Kind::Lan));
        assert_eq!(port_of(Some("192.168.0.1:9090")), ":9090");
        assert_eq!(port_of(Some("192.168.0.1")), "");
        assert_eq!(port_of(Some("192.168.0.1:80")), "");
        assert_eq!(port_of(Some("[fe80::1]:9090")), ":9090");
        assert_eq!(port_of(Some("fe80::1")), "");
        assert_eq!(port_of(None), "");
        let v: Vals = vec![("id", "manu2".into()), ("name", "Fake".into())];
        assert_eq!(form_url(Kind::ApnSave, &v, "00ff00ff00ff00ff"), "/ui/router/apn/edit?id=manu2&t=00ff00ff00ff00ff");
        assert_eq!(form_url(Kind::Wifi, &[], "00ff00ff00ff00ff"), "/ui/router/wifi?t=00ff00ff00ff00ff");
        // After a failed send: the page's token is the one the passwords went to.
        assert_eq!(with_token("id=manu2&t=00ff00ff00ff00ff&r=00ff00ff00ff00ff", "11aa11aa11aa11aa"), "id=manu2&t=11aa11aa11aa11aa&r=00ff00ff00ff00ff");
        assert_eq!(with_token("t=00ff00ff00ff00ff", "11aa11aa11aa11aa"), "t=11aa11aa11aa11aa");
    }

    /// Passwords never leave a draft for a page: shown as kept, dropped once
    /// sent (whether one was sent stays, for the result page).
    #[test]
    fn drafts_keep_passwords_out_of_pages() {
        let vals: Vals = vec![("ssid_2g", "Fake-Net".into()), ("key_2g", "fake-pass-123".into()), ("key_5g", String::new())];
        let s = shown(&vals);
        assert_eq!(s, vec![("ssid_2g", "Fake-Net".to_owned()), ("key_2g", KEPT.to_owned()), ("key_5g", String::new())]);
        let mut d = Draft { kind: Kind::Wifi, rev: "r".into(), ready: true, vals, next: String::new() };
        d.scrub(vec![("moved", "1".into())]);
        assert_eq!(val(&d.vals, "key_2g"), "");
        assert_eq!(val(&d.vals, "set_2g"), "1");
        assert_eq!(val(&d.vals, "set_5g"), "");
        assert_eq!(val(&d.vals, "moved"), "1");
        assert!(!d.vals.iter().any(|(_, v)| v.contains("fake-pass")));
    }
}
