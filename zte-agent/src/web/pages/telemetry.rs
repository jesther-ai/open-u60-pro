//! Router › Telemetry Blocker (iOS `TelemetryBlockerView`): the domain
//! filter's switch, Quick Actions ("Block All ZTE Telemetry" and the app's
//! five known domains, each with a check once blocked), Add Custom Domain
//! and the blocked domains, read at page load (no stream) and again after
//! every change (the app adds rows with ids of its own making, so a delete
//! before a refresh sends a wrong id).
//!
//! - The switch, Add and Block All post from the page (single-use token),
//!   as the app's buttons; removing a domain is asked first (the plan's
//!   confirmation; the app's trash button asks nothing) and names the list
//!   it was chosen from (`g`): a list that changed since sends nothing.
//! - Add checks a domain name (the app's "Enter a domain name"; then
//!   letters, digits and hyphens in dot-separated labels: a web addition),
//!   and one already blocked is not added again.
//! - Reads under the app's names, else the firmware's (`section_id`,
//!   `fqdn`, `enable`); the bodies are the app's plus the firmware's names
//!   from `ubus -v list` (`router_set_domain_filter {"action","section_id",
//!   "enable","fqdn"}`; the switch's `enable` as an integer). Not verified on
//!   the device.

use maud::{html, Markup};
use serde_json::{json, Value};

use super::confirm::{asking, nothing, Bad, Question};
use super::netcfg::{failed, field, first, generation, rule_id, text, truthy, val, Done, Kind, Vals};
use crate::handlers::AppState;
use crate::server::Method;
use crate::web::fmt;
use crate::web::kit::{self, Icon};
use crate::web::render::once_field;
use crate::web::screen::{FormState, PageData};
use crate::web::session::percent_encode;
use crate::web::store::Claim;

pub const PATH: &str = "/ui/router/telemetry";
pub const SWITCH: &str = "/ui/router/telemetry/switch";
pub const ADD: &str = "/ui/router/telemetry/add";
pub const BLOCK_ALL: &str = "/ui/router/telemetry/block-all";
pub const DELETE: &str = "/ui/router/telemetry/delete";
pub const API: &str = "/api/router/domain-filter";

/// The app's `knownTelemetryDomains`.
const KNOWN: [&str; 5] = ["dclient.ztems.com", "dconfig.ztems.com", "iot.ztems.com", "mcs-cloud.ztems.com", "update.ztems.com"];

const STALE: &str = "The domain list was out of date and has been reloaded. Try again.";

/// A blocked domain (the app's `DomainFilterRule`).
struct Rule {
    id: String,
    domain: String,
    enabled: bool,
}

fn rules(d: &Value) -> Vec<Rule> {
    let rows = d["rule_list"].as_array().map(Vec::as_slice).unwrap_or_default();
    rows.iter()
        .enumerate()
        .map(|(i, r)| Rule { id: rule_id(r, i), domain: text(first(r, &["domain", "fqdn"])), enabled: truthy(first(r, &["enabled", "enable"])) })
        .collect()
}

fn blocked(list: &[Rule], domain: &str) -> bool {
    list.iter().any(|r| r.domain.eq_ignore_ascii_case(domain))
}

/// A one-button form posting `fields` with the page's token.
fn post_button(pd: &PageData, action: &str, fields: &[(&str, &str)], label: &str) -> Markup {
    html! {
        li {
            form.sf method="post" action=(action) {
                (pd.csrf())
                (once_field(&pd.once()))
                @for (k, v) in fields { input type="hidden" name=(k) value=(v); }
                button.it.bt type="submit" { span { (label) } }
            }
        }
    }
}

pub fn page(pd: &PageData) -> Markup {
    let d = match pd.reads.first() {
        Some(Ok(d)) => d,
        Some(Err(e)) => return html! { p.msg.err role="alert" { "Failed to load filters: " (fmt::truncate(e, 120)) } },
        None => return html! {},
    };
    let on = truthy(&d["enable"]);
    let list = rules(d);
    let g = generation(d);
    let f = pd.form;
    let typed = f.and_then(|f| f.value("domain")).unwrap_or("");
    let err = f.and_then(|f| f.error("domain"));
    html! {
        (kit::section(Some("Domain Filter"), None, html! {
            li.it { span { "Status" } @if on { span.c-green { "Enabled" } } @else { span.v { "Disabled" } } }
            (post_button(pd, SWITCH, &[("on", if on { "0" } else { "1" })], if on { "Disable Filter" } else { "Enable Filter" }))
        }))
        (kit::section(Some("Quick Actions"), None, html! {
            (post_button(pd, BLOCK_ALL, &[], "Block All ZTE Telemetry"))
            @for domain in KNOWN {
                li.it {
                    span.dom { (domain) }
                    @if blocked(&list, domain) {
                        span.c-green { (kit::icon(Icon::Check)) span.sr { "Blocked" } }
                    } @else {
                        span.v { (kit::icon(Icon::Circle)) span.sr { "Not blocked" } }
                    }
                }
            }
        }))
        (kit::section(Some("Add Custom Domain"), None, html! {
            li.it.tdom {
                form.rf method="post" action=(ADD) autocomplete="off" {
                    (pd.csrf())
                    (once_field(&pd.once()))
                    label.sr for="domain" { "Domain to block" }
                    input #domain type="text" name="domain" value=(typed) placeholder="Domain to block" maxlength="253" inputmode="url"
                        autocomplete="off" autocapitalize="off" spellcheck="false" aria-invalid=[err.map(|_| "true")]
                        aria-describedby=[err.map(|_| "domain-err")];
                    button.btn.sm type="submit" { "Add" }
                }
                @if let Some(e) = err { p.ferr #domain-err { (e) } }
            }
        }))
        @if !list.is_empty() {
            (kit::section(Some("Blocked Domains"), None, html! {
                @for r in &list {
                    li.it.pf {
                        span.cvb {
                            span.dom { (r.domain) }
                            @if r.enabled { small.c-green { "Active" } } @else { small { "Inactive" } }
                        }
                        a.del href=(format!("{DELETE}?id={}&g={g}", percent_encode(&r.id))) aria-label=(format!("Remove {}", r.domain)) {
                            (kit::icon(Icon::Trash))
                        }
                    }
                }
            }))
        }
    }
}

/// A host name: dot-separated labels (at least two) of letters, digits
/// and hyphens (not at either end), up to 253 characters.
fn domain_ok(d: &str) -> bool {
    let label = |l: &str| (1..=63).contains(&l.len()) && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    d.len() <= 253 && d.split('.').count() >= 2 && d.split('.').all(label)
}

/// Add Custom Domain's field (lower case): the app's "Enter a domain name"
/// when empty, then a host name (the page again as typed, `400`).
pub fn add_fields(pairs: &[(String, String)]) -> Result<Vals, Bad> {
    let typed: String = field(pairs, "domain").unwrap_or("").trim().chars().take(254).collect();
    let domain = typed.to_ascii_lowercase();
    let error = if domain.is_empty() {
        "Enter a domain name"
    } else if !domain_ok(&domain) {
        "Enter a domain name such as ads.example.com: letters, digits, hyphens and dots."
    } else {
        return Ok(vec![("domain", domain)]);
    };
    Err(Bad::Fields(FormState { values: vec![("domain", typed)], errors: vec![("domain", error.into())] }))
}

/// "Remove …?", for the list the page showed.
pub fn question(app: &AppState, vals: &[(&'static str, String)]) -> Question {
    let d = match super::api_get(app, API) {
        Ok(d) => d,
        Err(e) => return nothing("Failed to load filters", vec![fmt::truncate(&e, 120)]),
    };
    if generation(&d) != val(vals, "g") {
        return nothing("The domain list changed", vec!["It changed since this page was loaded. Go back and choose the domain again.".into()]);
    }
    match rules(&d).into_iter().find(|r| r.id == val(vals, "id")) {
        None => nothing("Domain not found", vec!["It may have been removed.".into()]),
        Some(r) => asking(format!("Remove {}?", r.domain), vec![format!("Devices on the router can reach {} again.", r.domain)], "Remove", false, false),
    }
}

fn add_body(domain: &str) -> Value {
    json!({"action": "add", "domain": domain, "enabled": "1", "fqdn": domain})
}

pub fn send(kind: Kind, app: &AppState, vals: &[(&'static str, String)], claim: &mut Claim) -> Result<Done, (u16, String)> {
    let put = |body: Value| super::call(app, Method::Put, API, &body).map_err(|(status, e)| (status, failed(&e)));
    let read = || super::api_get(app, API).map_err(|e| (503, failed(&e)));
    match kind {
        Kind::TmSwitch => {
            let on = val(vals, "on") == "1";
            claim.disarm();
            put(json!({"enable": i64::from(on)}))?;
            Ok(Done::Line(Some(true), format!("Domain filter {}", if on { "enabled" } else { "disabled" })))
        }
        Kind::TmAdd => {
            let domain = val(vals, "domain");
            if blocked(&rules(&read()?), domain) {
                return Ok(Done::Line(None, format!("{domain} is already blocked")));
            }
            claim.disarm();
            put(add_body(domain))?;
            Ok(Done::Line(Some(true), format!("Added {domain}")))
        }
        Kind::TmBlockAll => {
            let list = rules(&read()?);
            let missing: Vec<&str> = KNOWN.into_iter().filter(|d| !blocked(&list, d)).collect();
            if missing.is_empty() {
                return Ok(Done::Line(Some(true), "All telemetry domains already blocked".into()));
            }
            claim.disarm();
            // The app's: one add per domain, on past a failure.
            let mut added = 0;
            let mut failure = None;
            for d in &missing {
                match put(add_body(d)) {
                    Ok(_) => added += 1,
                    Err(e) => {
                        failure.get_or_insert(e);
                    }
                }
            }
            let blocked_n = |n: usize| format!("Blocked {n} telemetry domain{}", if n == 1 { "" } else { "s" });
            match failure {
                None => Ok(Done::Line(Some(true), blocked_n(added))),
                Some(e) if added == 0 => Err(e),
                Some((_, e)) => Ok(Done::Line(None, format!("{}; {} could not be added. {e}", blocked_n(added), missing.len() - added))),
            }
        }
        _ => {
            let d = read()?;
            if generation(&d) != val(vals, "g") {
                return Err((409, STALE.into()));
            }
            let id = val(vals, "id");
            let Some(r) = rules(&d).into_iter().find(|r| r.id == id) else { return Err((404, "Domain not found".into())) };
            claim.disarm();
            put(json!({"action": "delete", "id": id, "section_id": [id]}))?;
            Ok(Done::Line(Some(true), format!("Removed {}", r.domain)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains() {
        for ok in ["ads.example.com", "a-b.example", "x1.y2"] {
            assert!(domain_ok(ok), "{ok}");
        }
        for bad in ["example", "-a.example", "a-.example", "a..example", "a.example.", "a b.example", "a_b.example", "https://a.example", &format!("{}.example", "a".repeat(64))] {
            assert!(!domain_ok(bad), "{bad}");
        }
        let pairs = |v: &str| vec![("domain".to_owned(), v.to_owned())];
        assert_eq!(add_fields(&pairs("  Ads.Example.COM ")).ok().unwrap(), vec![("domain", "ads.example.com".to_owned())]);
        match add_fields(&pairs("   ")) {
            Err(Bad::Fields(fs)) => assert_eq!(fs.error("domain"), Some("Enter a domain name")),
            _ => panic!("empty"),
        }
        let r = rules(&json!({"rule_list": [{"domain": "a.example", "enabled": "1"}, {"section_id": "cfg01", "fqdn": "b.example"}]}));
        assert_eq!((r[0].id.as_str(), r[0].enabled, r[1].id.as_str(), r[1].domain.as_str(), r[1].enabled), ("0", true, "cfg01", "b.example", false));
        assert!(blocked(&r, "A.Example") && !blocked(&r, "c.example"));
    }
}
