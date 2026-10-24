//! Router › DNS (iOS `DNSSettingsView`, `DoHCacheInspectorView`): the mode
//! (Auto / Custom / DoH), the servers, the DoH upstream, Quick Setup and
//! the DoH cache, read at page load (no stream). Apply and Clear Cache are
//! asked first (`pages::netcfg`; the app asks neither): every device on the
//! router looks names up through what is chosen.
//!
//! - The mode shown is the app's: DoH while DNS over HTTPS runs, Custom for
//!   manual servers, else Auto. Quick Setup's presets are links
//!   (`?preset=`) that draw the form filled (Auto becomes Custom, as the
//!   app's); nothing is saved until Apply.
//! - Apply makes the app's calls: Auto and Custom set the router's DNS
//!   and turn DNS over HTTPS off (the app always does); DoH sets the
//!   servers, the upstream URL, and turns it on if it is off. Checks: the
//!   app's messages, then (web additions) a server that is a unicast
//!   address other than the router's own (its DNS service would ask
//!   itself), and an `https://` URL with an IPv4, IPv6 or host name, an
//!   optional port and a plain path, whose host passes the servers' checks
//!   (and is not `localhost`). When the router's LAN address can't be read,
//!   a private server or DoH host is refused ("Couldn't read the router's
//!   network."), as DMZ and port forward refuse; a public one, every preset
//!   included, is taken (the router's LAN address is always private, so it
//!   can't be one). The question says "Nothing to apply" when the router
//!   already has these settings, and for a remote browser that the router's
//!   own lookups use the new servers too.
//! - A failed DNS read shows only the error (no form: it would show Auto).
//! - The cache inspector is its own page ("Done" back; the app's title
//!   "DNS Cache (N)", N the entries shown): the entries by TTL,
//!   50 more per "Load More" (`?n=`), filtered by a posted search (the
//!   cache is the network's browsing history: the text stays out of URLs,
//!   as Messages' search).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use maud::{html, Markup};
use serde_json::{json, Value};

use super::confirm::{asking, nothing, Bad, Question};
use super::netcfg::{failed, field, foot, form_error, text, text_row, val, Done, Kind, Vals};
use crate::handlers::AppState;
use crate::server::Method;
use crate::web::fmt;
use crate::web::kit::{self, Icon};
use crate::web::render::once_field;
use crate::web::screen::{FormState, PageData};
use crate::web::store::Claim;

pub const PATH: &str = "/ui/router/dns";
pub const APPLY: &str = "/ui/router/dns/apply";
pub const CACHE: &str = "/ui/router/dns/cache";
pub const CLEAR: &str = "/ui/router/dns/cache-clear";
const DNS_API: &str = "/api/router/dns";
const DOH_API: &str = "/api/doh/status";

/// The app's Quick Setup: (key, label, primary, secondary, DoH upstream).
const PRESETS: [(&str, &str, &str, &str, &str); 3] = [
    ("cloudflare", "Cloudflare (1.1.1.1)", "1.1.1.1", "1.0.0.1", "https://1.1.1.1/dns-query"),
    ("google", "Google (8.8.8.8)", "8.8.8.8", "8.8.4.4", "https://8.8.8.8/dns-query"),
    ("quad9", "Quad9 (9.9.9.9)", "9.9.9.9", "149.112.112.112", "https://9.9.9.9:5053/dns-query"),
];

const MODES: [(&str, &str, &str); 3] = [
    ("auto", "Auto", "Use DNS servers assigned by your ISP."),
    ("custom", "Custom", "Use custom DNS servers."),
    ("doh", "DoH", "Encrypt DNS queries over HTTPS."),
];

fn running(doh: Option<&Value>) -> bool {
    doh.is_some_and(|d| d["running"] == true)
}

fn ipv4(s: &str) -> bool {
    s.parse::<Ipv4Addr>().is_ok()
}

const NOT_A_SERVER: &str = "Use the address of a DNS server.";
const ROUTER_ITSELF: &str = "This is the router\u{2019}s own address; use a DNS server\u{2019}s address.";
/// As DMZ and port forward say when the router's network can't be read.
const NO_LAN: &str = "Couldn\u{2019}t read the router\u{2019}s network. Try again.";
const URL_FORMAT: &str = "Enter an https:// URL with a server address or name, such as https://1.1.1.1/dns-query.";

/// Why an address can't be a DNS server (or the DoH server): not a single
/// host (0.0.0.0, `::`, loopback, broadcast, multicast), or the router
/// itself. `router`: its LAN address, `None` when it couldn't be read; a
/// private IPv4 address could then be the router (its LAN address is
/// always private), so it is refused until the read works.
fn addr_error(a: IpAddr, router: Option<Ipv4Addr>) -> Option<String> {
    let msg = match a.to_canonical() {
        IpAddr::V4(a) if a.is_unspecified() || a.is_loopback() || a.is_broadcast() || a.is_multicast() => NOT_A_SERVER,
        IpAddr::V6(a) if a.is_unspecified() || a.is_loopback() || a.is_multicast() => NOT_A_SERVER,
        IpAddr::V4(a) if Some(a) == router => ROUTER_ITSELF,
        IpAddr::V4(a) if router.is_none() && a.is_private() => NO_LAN,
        _ => return None,
    };
    Some(msg.into())
}

/// Why a server address (an IPv4 address; other text is the app's
/// "Invalid …") can't be a DNS server.
fn server_error(s: &str, router: Option<Ipv4Addr>) -> Option<String> {
    addr_error(IpAddr::V4(s.parse().ok()?), router)
}

/// A DoH URL's host.
#[derive(Debug, PartialEq, Eq)]
enum Host<'a> {
    Ip(IpAddr),
    Name(&'a str),
}

/// Why a DoH URL's host can't be the DoH server: an address as a DNS
/// server's, and `localhost` (loopback by name).
fn host_error(h: &Host, router: Option<Ipv4Addr>) -> Option<String> {
    match h {
        Host::Ip(a) => addr_error(*a, router),
        Host::Name(n) => {
            let n = n.to_ascii_lowercase();
            (n == "localhost" || n.ends_with(".localhost")).then(|| NOT_A_SERVER.into())
        }
    }
}

/// A host name: dot-separated labels of letters, digits and hyphens (not
/// at either end), the last not all digits.
fn hostname(h: &str) -> bool {
    let label = |l: &str| (1..=63).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !l.starts_with('-') && !l.ends_with('-');
    (1..=253).contains(&h.len()) && h.split('.').all(label) && !h.rsplit('.').next().is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
}

/// The host of an `https://` URL the DoH client can use: an IPv4 address,
/// an IPv6 address in brackets or a host name; an optional port 1–65535; a
/// path of visible characters without quotes, angle brackets or
/// backslashes. `None`: not such a URL.
fn url_host(u: &str) -> Option<Host<'_>> {
    let rest = u.strip_prefix("https://").filter(|_| u.len() <= 256)?;
    let (auth, path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let (host, port) = match auth.strip_prefix('[') {
        Some(v6) => {
            let (a, tail) = v6.split_once(']')?;
            (Host::Ip(IpAddr::V6(a.parse::<Ipv6Addr>().ok()?)), tail)
        }
        None => {
            let (h, tail) = auth.find(':').map_or((auth, ""), |i| auth.split_at(i));
            match h.parse::<Ipv4Addr>() {
                Ok(a) => (Host::Ip(IpAddr::V4(a)), tail),
                Err(_) if hostname(h) => (Host::Name(h), tail),
                Err(_) => return None,
            }
        }
    };
    let port_ok = match port.strip_prefix(':') {
        None => port.is_empty(),
        Some(p) => (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|n| n >= 1),
    };
    let path_ok = path.bytes().all(|b| b.is_ascii_graphic() && !b"\"'<>\\`{}|^".contains(&b));
    (port_ok && path_ok).then_some(host)
}

pub fn page(pd: &PageData) -> Markup {
    if let Some(Err(e)) = pd.reads.first() {
        return html! { p.msg.err role="alert" { "Failed to load DNS: " (fmt::truncate(e, 120)) } };
    }
    let doh = pd.read(1);
    let d = pd.read(0);
    let on = running(doh);
    let manual = d.is_some_and(|d| d["dns_mode"].as_str() == Some("manual"));
    let mut mode = if on { "doh" } else if manual { "custom" } else { "auto" };
    let (mut dns1, mut dns2) = d.map_or((String::new(), String::new()), |d| (text(&d["prefer_dns_manual"]), text(&d["standby_dns_manual"])));
    let mut url = doh.map(|d| text(&d["config"]["upstream_url"])).unwrap_or_default();
    if let Some(p) = pd.param("preset").and_then(|k| PRESETS.iter().find(|p| p.0 == k)) {
        if mode == "auto" {
            mode = "custom";
        }
        (dns1, dns2, url) = (p.2.into(), p.3.into(), p.4.into());
    }
    let f = pd.form;
    let typed = |name: &str, shown: String| f.and_then(|f| f.value(name)).map_or(shown, str::to_owned);
    let err = |name: &str| f.and_then(|f| f.error(name));
    let mode = f.and_then(|f| f.value("mode")).unwrap_or(mode).to_owned();
    let stats = &doh.map_or(Value::Null, |d| d["stats"].clone());
    let n = |k: &str| fmt::int(&stats[k]).unwrap_or(0);
    let (entries, hits, misses) = (n("cache_entries"), n("cache_hits"), n("cache_misses"));
    let ratio = if hits + misses > 0 { hits as f64 / (hits + misses) as f64 * 100.0 } else { 0.0 };
    let footers = html! { @for (m, _, text) in MODES { p class=(format!("foot dmf dm-{m}")) { (text) } } };
    html! {
        // The app's line when only the DoH status could not be read.
        @if pd.reads.get(1).is_some_and(Result::is_err) { p.msg.info role="status" { "DoH status unavailable" } }
        (form_error(pd))
        form.rf.dnf method="post" action=(APPLY) autocomplete="off" {
            (pd.csrf())
            (once_field(&pd.once()))
            (kit::section(Some("DNS Mode"), Some(footers), html! {
                li.it.fld {
                    fieldset.chips {
                        legend.sr { "Mode" }
                        div {
                            @for (m, label, _) in MODES {
                                label { input.sr type="radio" id=(format!("dm-{m}")) name="mode" value=(m) checked[m == mode]; span { (label) } }
                            }
                        }
                    }
                }
            }))
            div.dsv {
                (kit::section(Some("DNS Servers"), None, html! {
                    (text_row("dns1", "Primary", &typed("dns1", dns1), err("dns1"), "it fld", 15, Some("decimal")))
                    (text_row("dns2", "Secondary", &typed("dns2", dns2), err("dns2"), "it fld", 15, Some("decimal")))
                }))
            }
            div.doh {
                (kit::section(Some("DoH Upstream"), None, text_row("url", "URL", &typed("url", url), err("url"), "it fld", 256, Some("url"))))
            }
            (kit::section(None, None, html! { li { button.it.cv.snd type="submit" { "Apply" } } }))
        }
        (kit::section(Some("Quick Setup"), Some(foot("Fills DNS fields. Tap Apply to save.")), html! {
            @for (key, label, ..) in PRESETS { li { a.it.bt href=(format!("{PATH}?preset={key}")) { span { (label) } } } }
        }))
        @if on {
            (kit::section(Some("DoH Cache"), None, html! {
                (kit::kv_row(None, "Entries", &entries.to_string()))
                (kit::kv_row(None, "Hits", &hits.to_string()))
                (kit::kv_row(None, "Misses", &misses.to_string()))
                (kit::kv_row(None, "Hit Ratio", &format!("{ratio:.1}%")))
                @if entries > 0 {
                    li { a.it.bt href=(CACHE) { "Inspect Cache" } }
                    li { a.it.bt href=(CLEAR) { "Clear Cache" } }
                } @else {
                    li { span.it.bt aria-disabled="true" { "Inspect Cache" } }
                    li { span.it.bt aria-disabled="true" { "Clear Cache" } }
                }
            }))
        }
    }
}

/// The DNS form, checked against the router's own address (read now; when
/// it can't be read, a private address is refused, see `addr_error`).
pub fn check(app: &AppState, pairs: &[(String, String)]) -> Result<Vals, Bad> {
    let router = super::api_get(app, "/api/router/lan").ok().and_then(|d| super::lan::ip(&text(&d["lan_ipaddr"])));
    check_with(pairs, router)
}

/// The DNS form, checked: the app's messages first.
fn check_with(pairs: &[(String, String)], router: Option<Ipv4Addr>) -> Result<Vals, Bad> {
    let mode = match field(pairs, "mode") {
        Some(m @ ("auto" | "custom" | "doh")) => m.to_owned(),
        _ => return Err(Bad::Request),
    };
    let get = |n: &str, max: usize| field(pairs, n).unwrap_or("").trim().chars().take(max).collect::<String>();
    let (dns1, dns2, url) = (get("dns1", 64), get("dns2", 64), get("url", 300));
    let mut errors: Vec<(&'static str, String)> = Vec::new();
    if mode != "auto" {
        if dns1.is_empty() {
            errors.push(("dns1", "Primary DNS cannot be empty".into()));
        } else if !ipv4(&dns1) {
            errors.push(("dns1", "Invalid primary DNS address".into()));
        }
        if !dns2.is_empty() && !ipv4(&dns2) {
            errors.push(("dns2", "Invalid secondary DNS address".into()));
        }
        for (name, s) in [("dns1", &dns1), ("dns2", &dns2)] {
            if let Some(e) = server_error(s, router) {
                errors.push((name, e));
            }
        }
    }
    if mode == "doh" {
        if url.is_empty() {
            errors.push(("url", "DoH upstream URL cannot be empty".into()));
        } else {
            match url_host(&url) {
                None => errors.push(("url", URL_FORMAT.into())),
                Some(h) => errors.extend(host_error(&h, router).map(|e| ("url", e))),
            }
        }
    }
    let vals: Vals = vec![("mode", mode), ("dns1", dns1), ("dns2", dns2), ("url", url)];
    if errors.is_empty() {
        Ok(vals)
    } else {
        Err(Bad::Fields(FormState { values: vals, errors }))
    }
}

/// For a remote browser: the router's own lookups use the new servers too
/// (Custom), or the DoH server (DoH: dnsmasq forwards only to the DoH
/// client then).
const REMOTE: &str = "You are connected remotely: the router\u{2019}s own lookups (Tailscale reconnects, SMS forwarding webhooks) use these servers too, so servers that can\u{2019}t be reached can cut remote access.";
const REMOTE_DOH: &str = "You are connected remotely: the router\u{2019}s own lookups (Tailscale reconnects, SMS forwarding webhooks) go through this DoH server too, so a server that can\u{2019}t be reached can cut remote access.";

pub fn question(kind: Kind, app: &AppState, vals: &[(&'static str, String)], remote: bool) -> Question {
    let doh = super::api_get(app, DOH_API).ok();
    let on = running(doh.as_ref());
    if kind == Kind::DnsClear {
        let n = doh.as_ref().and_then(|d| fmt::int(&d["stats"]["cache_entries"])).unwrap_or(0);
        return match (on, n) {
            (false, _) => nothing("DNS over HTTPS is off", vec!["There is no cache to clear.".into()]),
            (true, 0) => nothing("The DNS cache is empty", Vec::new()),
            _ => asking(
                "Clear the DNS cache?",
                vec![format!("The {n} cached answers are removed; the next lookups ask the DoH server again.")],
                "Clear Cache",
                false,
                false,
            ),
        };
    }
    let (dns1, dns2, url) = (val(vals, "dns1"), val(vals, "dns2"), val(vals, "url"));
    let servers = if dns2.is_empty() { dns1.to_owned() } else { format!("{dns1} and {dns2}") };
    let mode = val(vals, "mode");
    // The router's settings now (`None`: unknown, so taken as different).
    let dns = super::api_get(app, DNS_API).ok();
    let manual = dns.as_ref().map(|d| d["dns_mode"].as_str() == Some("manual"));
    let same_servers = dns.as_ref().is_some_and(|d| text(&d["prefer_dns_manual"]) == dns1 && text(&d["standby_dns_manual"]) == dns2);
    let same_url = doh.as_ref().is_some_and(|d| text(&d["config"]["upstream_url"]) == url);
    let off = doh.is_some() && !on;
    let unchanged = match mode {
        "auto" => manual == Some(false) && off,
        "custom" => manual == Some(true) && same_servers && off,
        _ => on && same_url && manual == Some(true) && same_servers,
    };
    if unchanged {
        return nothing("Nothing to apply", vec!["These are the router\u{2019}s settings already.".into()]);
    }
    let mut lines: Vec<String> = Vec::new();
    let title = match mode {
        "auto" => {
            lines.push("Devices on the router use the DNS servers your operator assigns.".into());
            "Switch DNS to Auto?"
        }
        "custom" => {
            lines.push(format!("Devices on the router look up names through {servers}."));
            lines.push("If these servers don\u{2019}t answer, websites stop loading for devices on the router until DNS is changed back.".into());
            "Use custom DNS servers?"
        }
        _ => {
            if !(on && same_url) {
                lines.push(format!("Devices on the router look up names through {url}, encrypted."));
                lines.push("If that server can\u{2019}t be reached, name lookups fail for devices on the router until DNS is changed back.".into());
            }
            if !(manual == Some(true) && same_servers) {
                lines.push(format!("The router\u{2019}s DNS servers become {servers}."));
            }
            match (on, same_url) {
                (false, _) => "Turn on DNS over HTTPS?",
                (true, false) => "Change the DoH server?",
                (true, true) => "Change the DNS servers?",
            }
        }
    };
    if mode != "doh" {
        if on {
            lines.push("DNS over HTTPS turns off.".into());
        } else if doh.is_none() {
            lines.push("DNS over HTTPS turns off if it is on.".into());
        }
    }
    if !(on && mode == "doh") {
        lines.push("Name lookups pause for a moment while the router\u{2019}s DNS service restarts.".into());
    }
    if remote && mode != "auto" {
        lines.push(if mode == "custom" { REMOTE } else { REMOTE_DOH }.into());
    }
    asking(title, lines, "Apply", false, false)
}

pub fn send(kind: Kind, app: &AppState, vals: &[(&'static str, String)], claim: &mut Claim) -> Result<Done, (u16, String)> {
    let call = |method, path, body: Value| super::call(app, method, path, &body).map_err(|(status, e)| (status, failed(&e)));
    if kind == Kind::DnsClear {
        claim.disarm();
        call(Method::Post, "/api/doh/cache/clear", json!({}))?;
        return Ok(Done::Line(Some(true), "DNS cache cleared".into()));
    }
    let (dns1, dns2, url) = (val(vals, "dns1"), val(vals, "dns2"), val(vals, "url"));
    match val(vals, "mode") {
        "auto" => {
            claim.disarm();
            call(Method::Put, DNS_API, json!({"dns_mode": "auto", "prefer_dns_manual": "", "standby_dns_manual": ""}))?;
            // Always (the app's: no dnsmasq forwarding left behind).
            call(Method::Post, "/api/doh/disable", json!({}))?;
            Ok(Done::Line(Some(true), "DNS set to Auto".into()))
        }
        "custom" => {
            claim.disarm();
            call(Method::Put, DNS_API, json!({"dns_mode": "manual", "prefer_dns_manual": dns1, "standby_dns_manual": dns2}))?;
            call(Method::Post, "/api/doh/disable", json!({}))?;
            Ok(Done::Line(Some(true), format!("DNS set to Custom ({dns1})")))
        }
        _ => {
            let on = super::api_get(app, DOH_API).map(|d| d["running"] == true).map_err(|e| (503, failed(&e)))?;
            claim.disarm();
            call(Method::Put, DNS_API, json!({"dns_mode": "manual", "prefer_dns_manual": dns1, "standby_dns_manual": dns2}))?;
            call(Method::Put, "/api/doh/config", json!({"upstream_url": url}))?;
            if !on {
                call(Method::Post, "/api/doh/enable", json!({}))?;
            }
            Ok(Done::Line(Some(true), format!("DoH enabled with {}", fmt::truncate(url, 120))))
        }
    }
}

// --- DNS Cache ---

/// The app's `formatTTL`.
fn ttl(secs: i64) -> String {
    match secs {
        s if s >= 3600 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s if s >= 60 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

/// The app's type badge colours.
fn badge(t: &str) -> &'static str {
    match t {
        "A" => "badge bl",
        "AAAA" => "badge pu",
        "CNAME" => "badge",
        "HTTPS" => "badge gr",
        "MX" => "badge rd",
        "TXT" => "badge br",
        _ => "badge gy",
    }
}

const STEP: usize = 50;

/// The posted search.
fn search<'a>(pd: &'a PageData) -> &'a str {
    pd.form.and_then(|f| f.value("q")).unwrap_or("")
}

/// The cached entries (domain, type, TTL) matching the search, by TTL.
fn entries<'a>(pd: &'a PageData) -> Vec<(&'a str, &'a str, i64)> {
    let needle = search(pd).to_lowercase();
    let mut entries: Vec<(&str, &str, i64)> = pd
        .read(0)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|e| (e["domain"].as_str().unwrap_or(""), e["type"].as_str().unwrap_or("?"), fmt::int(&e["ttl"]).unwrap_or(0)))
        .filter(|(d, ..)| needle.is_empty() || d.to_lowercase().contains(&needle))
        .collect();
    entries.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(b.0)));
    entries
}

/// The app's title: "DNS Cache (N)", N the entries shown (searched).
pub fn cache_heading(pd: &PageData) -> String {
    format!("DNS Cache ({})", entries(pd).len())
}

/// The inspector: the entries by TTL, `?n=` at a time (50 more per "Load
/// More", as the app's), filtered by the posted search.
pub fn cache(pd: &PageData) -> Markup {
    let on = running(pd.read(1));
    let q = search(pd);
    let n = pd.param("n").and_then(|n| n.parse::<usize>().ok()).map_or(STEP, |n| n.clamp(STEP, 100_000));
    let entries = entries(pd);
    let more = format!("{CACHE}?n={}", n + STEP);
    html! {
        @if let Some(Err(e)) = pd.reads.first() { p.msg.err role="alert" { "Couldn\u{2019}t read the cache: " (fmt::truncate(e, 120)) } }
        form.srch method="post" action=(CACHE) role="search" {
            (pd.csrf())
            input type="search" name="q" value=(q) placeholder="Filter by domain" aria-label="Filter by domain" autocomplete="off" maxlength="64";
            button.btn.sm type="submit" { "Search" }
            @if !q.is_empty() { a.nl href=(CACHE) { "Cancel" } }
        }
        @if entries.is_empty() {
            @let (title, sub) = match (on, q.is_empty()) {
                (false, _) => ("DNS over HTTPS is off", "The cache fills while it runs."),
                (true, false) => ("No matches", "No cached domain contains this text."),
                (true, true) => ("No cached entries", "Answers appear here as devices look up names."),
            };
            (kit::empty_state(Icon::Globe, title, sub))
        } @else {
            (kit::section(None, None, html! {
                @for (domain, kind, secs) in entries.iter().take(n) {
                    li.it { span.cvb { span.mono.dom { (domain) } small { (ttl(*secs)) } } span class=(badge(kind)) { (kind) } }
                }
            }))
            @if entries.len() > n {
                @if q.is_empty() {
                    p.ctr { a.btn.btn-quiet href=(more) { "Load More" } }
                } @else {
                    form.ctr method="post" action=(more) {
                        (pd.csrf())
                        input type="hidden" name="q" value=(q);
                        button.btn.btn-quiet type="submit" { "Load More" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(q: &str) -> Vec<(String, String)> {
        crate::web::session::Form::parse(q.as_bytes()).pairs().to_vec()
    }

    fn errors(q: &str) -> Vec<(&'static str, String)> {
        match check_with(&pairs(q), Some(Ipv4Addr::new(192, 168, 0, 1))) {
            Err(Bad::Fields(fs)) => fs.errors,
            Ok(_) => Vec::new(),
            Err(Bad::Request) => panic!("bad request"),
        }
    }

    #[test]
    fn the_apps_messages_then_the_url() {
        assert!(errors("mode=auto&dns1=&dns2=x&url=").is_empty());
        assert_eq!(errors("mode=custom&dns1=&dns2=&url="), [("dns1", "Primary DNS cannot be empty".to_owned())]);
        assert_eq!(errors("mode=custom&dns1=1.1.1&dns2=&url="), [("dns1", "Invalid primary DNS address".to_owned())]);
        assert_eq!(errors("mode=custom&dns1=1.1.1.1&dns2=x&url="), [("dns2", "Invalid secondary DNS address".to_owned())]);
        assert_eq!(errors("mode=doh&dns1=1.1.1.1&dns2=&url="), [("url", "DoH upstream URL cannot be empty".to_owned())]);
        for bad in ["http://1.1.1.1/dns-query", "https://", "https:///x", "https://a b"] {
            assert_eq!(errors(&format!("mode=doh&dns1=1.1.1.1&url={}", crate::web::session::percent_encode(bad))).len(), 1, "{bad}");
        }
        assert!(errors("mode=doh&dns1=9.9.9.9&url=https%3A%2F%2F9.9.9.9%3A5053%2Fdns-query").is_empty());
        // Only a DNS server's address: not a broadcast, loopback, multicast or the router itself.
        for bad in ["0.0.0.0", "127.0.0.1", "255.255.255.255", "224.0.0.1", "192.168.0.1"] {
            assert_eq!(errors(&format!("mode=custom&dns1={bad}&dns2=")).len(), 1, "{bad}");
            assert_eq!(errors(&format!("mode=custom&dns1=1.1.1.1&dns2={bad}")).len(), 1, "{bad}");
        }
        assert!(errors("mode=custom&dns1=192.168.0.5&dns2=").is_empty(), "a server on the network");
        for ok in ["https://dns.fake.example/dns-query", "https://[2606:4700:4700::1111]/dns-query", "https://9.9.9.9:5053/dns-query?x=1", "https://a-b.example"] {
            assert!(url_host(ok).is_some(), "{ok}");
        }
        for bad in [
            "https://a\"b<c>/x", "https://a/b\\c", "https://-a.example/", "https://a..example/", "https://1.2.3/", "https://user@host/",
            "https://host:0/", "https://host:65536/", "https://host:/", "https://[::1/", "https://[zz]/",
        ] {
            assert!(url_host(bad).is_none(), "{bad}");
        }
        assert!(matches!(check_with(&pairs("mode=x"), None), Err(Bad::Request)));
        assert_eq!((ttl(7260), ttl(61), ttl(5)), ("2h 1m".to_owned(), "1m 1s".to_owned(), "5s".to_owned()));
    }

    fn doh(url: &str, router: Option<Ipv4Addr>) -> Vec<(&'static str, String)> {
        let q = format!("mode=doh&dns1=1.1.1.1&dns2=&url={}", crate::web::session::percent_encode(url));
        match check_with(&pairs(&q), router) {
            Err(Bad::Fields(fs)) => fs.errors,
            _ => Vec::new(),
        }
    }

    /// The DoH URL's host is checked as the servers are: one host, not the
    /// router itself (nor `localhost`), in the same words.
    #[test]
    fn the_doh_host_is_checked_like_a_server() {
        let router = Some(Ipv4Addr::new(192, 168, 0, 1));
        for bad in [
            "https://0.0.0.0/dns-query", "https://127.0.0.1/dns-query", "https://127.8.9.10:853/x", "https://255.255.255.255/", "https://224.0.0.1/",
            "https://[::1]/dns-query", "https://[::]/dns-query", "https://[ff02::1]/", "https://[::ffff:127.0.0.1]/", "https://localhost/dns-query",
            "https://LocalHost:8443/", "https://dns.localhost/",
        ] {
            assert_eq!(doh(bad, router), [("url", NOT_A_SERVER.to_owned())], "{bad}");
        }
        assert_eq!(doh("https://192.168.0.1/dns-query", router), [("url", ROUTER_ITSELF.to_owned())]);
        assert_eq!(doh("https://[::ffff:192.168.0.1]/dns-query", router), [("url", ROUTER_ITSELF.to_owned())]);
        for ok in ["https://192.168.0.5/dns-query", "https://1.1.1.1/dns-query", "https://[2001:db8::53]/dns-query", "https://dns.fake.example/q"] {
            assert!(doh(ok, router).is_empty(), "{ok}");
        }
        // The servers' words, as before.
        assert_eq!(errors("mode=custom&dns1=127.0.0.1&dns2="), [("dns1", NOT_A_SERVER.to_owned())]);
        assert_eq!(errors("mode=custom&dns1=192.168.0.1&dns2="), [("dns1", ROUTER_ITSELF.to_owned())]);
    }

    /// The router's LAN address unread: a private server or DoH host could
    /// be the router, so it waits for a read that works; a public one (the
    /// presets) does not need it.
    #[test]
    fn an_unread_lan_refuses_only_private_addresses() {
        let check = |q: &str| match check_with(&pairs(q), None) {
            Err(Bad::Fields(fs)) => fs.errors,
            _ => Vec::new(),
        };
        assert_eq!(check("mode=custom&dns1=192.168.0.1&dns2="), [("dns1", NO_LAN.to_owned())]);
        assert_eq!(check("mode=custom&dns1=1.1.1.1&dns2=10.0.0.53"), [("dns2", NO_LAN.to_owned())]);
        assert!(check("mode=auto&dns1=&dns2=&url=").is_empty());
        for (_, _, d1, d2, url) in PRESETS {
            assert!(check(&format!("mode=doh&dns1={d1}&dns2={d2}&url={}", crate::web::session::percent_encode(url))).is_empty(), "{url}");
        }
        assert_eq!(doh("https://172.16.0.1/dns-query", None), [("url", NO_LAN.to_owned())]);
        assert_eq!(doh("https://127.0.0.1/dns-query", None), [("url", NOT_A_SERVER.to_owned())]);
        assert!(doh("https://dns.fake.example/dns-query", None).is_empty());
    }
}
