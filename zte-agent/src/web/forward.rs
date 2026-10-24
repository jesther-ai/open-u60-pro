//! SMS Forwarding (iOS `SMSForwardModels`, `SMSForwardViewModel` and the
//! rule form's `RuleDraft`): the config as pages may show it, the rule
//! summaries, the rule form (one table of destination types drives its
//! fields, their parsing and checks and the wire format), the merge of
//! saved secrets, and the device calls.
//!
//! **Secrets** (a Telegram bot token, a webhook, Discord or Slack URL, the
//! webhook's header values, an ntfy token) never reach a renderer: `public`
//! drops them from the config read (a URL becomes its host), the forms'
//! password fields are never filled, and an answer keeps what was typed in
//! a server-side `Secrets` draft (`store::Drafts`). An empty secret field
//! keeps the secret the rule (or the draft) has, but only while the
//! destination type and its server stay the same (Telegram: the type), so
//! nobody can point a rule at another server and receive its secrets; a
//! changed server needs them typed again. Header values stay only while the
//! webhook URL is the same: shared webhook hosts show a request's headers
//! to whoever owns its path; headers that choose where a request goes
//! (`Host`, …) cannot be set. A draft holds only secrets typed into the
//! form, never the rule's own, so a secret changed in the app meanwhile is
//! not written back.

use std::hash::{DefaultHasher, Hash, Hasher};

use serde_json::{json, Map, Value};

use super::session::Form;
use crate::handlers::AppState;
use crate::server::Method;

pub const CONFIG: &str = "/api/sms/forward/config";
pub const RULES: &str = "/api/sms/forward/rules";
pub const TOGGLE: &str = "/api/sms/forward/rules/toggle";
pub const TEST: &str = "/api/sms/forward/test";
pub const LOG: &str = "/api/sms/forward/log";
pub const LOG_CLEAR: &str = "/api/sms/forward/log/clear";
pub const RETRY: &str = "/api/sms/forward/retry";

/// Where Telegram's secrets go.
const TELEGRAM: &str = "https://api.telegram.org";
/// Header rows a form posts at most.
pub const MAX_HEADERS: usize = 20;
const NAME_MAX: usize = 100;
const LIST_MAX: usize = 500;
const URL_MAX: usize = 2048;
const TEXT_MAX: usize = 512;

pub const REQUIRED: &str = "Required.";
pub const AGAIN: &str = "Enter it again: the server changed.";
pub const AGAIN_URL: &str = "Enter the value again: the URL changed.";
const BAD_URL: &str = "Enter a URL starting with https:// or http://.";
const SAVED_URL: &str = "The saved URL does not start with https:// or http://. Enter it again.";

/// A field of a destination type.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    /// A server address, shown (ntfy's).
    Url,
    Tel,
    /// Never shown; empty keeps the saved one (same type and server).
    Secret,
    /// A URL that is itself the credential (webhook, Discord, Slack): never
    /// shown, only its host; empty keeps the saved one (same type).
    SecretUrl,
    Check,
    /// POST or PUT.
    Method,
    /// Header rows: the name shown, the value a secret.
    Headers,
}

pub struct Field {
    /// The form's name (unique over all types).
    pub name: &'static str,
    /// The agent's key.
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub required: bool,
    /// New Rule's value.
    pub default: &'static str,
}

const fn f(name: &'static str, key: &'static str, label: &'static str, kind: Kind, required: bool) -> Field {
    Field { name, key, label, kind, required, default: "" }
}

/// A destination type (iOS `ForwardDestination`): its key, label and
/// fields in the app's order.
pub struct Dest {
    pub key: &'static str,
    pub label: &'static str,
    pub fields: &'static [Field],
}

pub static DESTS: [Dest; 6] = [
    Dest {
        key: "telegram",
        label: "Telegram",
        fields: &[
            f("tg_token", "bot_token", "Bot Token", Kind::Secret, true),
            f("tg_chat", "chat_id", "Chat ID", Kind::Text, true),
            f("tg_silent", "silent", "Silent", Kind::Check, false),
        ],
    },
    Dest {
        key: "webhook",
        label: "Webhook",
        fields: &[
            f("wh_url", "url", "URL", Kind::SecretUrl, true),
            Field { default: "POST", ..f("wh_method", "method", "Method", Kind::Method, false) },
            f("wh_h", "headers", "Headers", Kind::Headers, false),
        ],
    },
    Dest { key: "sms", label: "SMS", fields: &[f("sms_number", "forward_number", "Forward to number", Kind::Tel, true)] },
    Dest {
        key: "ntfy",
        label: "ntfy",
        fields: &[
            Field { default: "https://ntfy.sh", ..f("nt_url", "url", "Server URL", Kind::Url, true) },
            f("nt_topic", "topic", "Topic", Kind::Text, true),
            f("nt_token", "token", "Token (optional)", Kind::Secret, false),
        ],
    },
    Dest { key: "discord", label: "Discord", fields: &[f("dc_url", "webhook_url", "Webhook URL", Kind::SecretUrl, true)] },
    Dest { key: "slack", label: "Slack", fields: &[f("sl_url", "webhook_url", "Webhook URL", Kind::SecretUrl, true)] },
];

/// The filter types (value, the app's label).
pub const FILTERS: [(&str, &str); 4] =
    [("all", "All Messages"), ("sender", "By Sender"), ("content", "By Content"), ("sender_and_content", "Sender + Content")];

pub fn dest(key: &str) -> Option<&'static Dest> {
    DESTS.iter().find(|d| d.key == key)
}

/// The destination type a field belongs to.
fn owner(field: &str) -> &'static Dest {
    DESTS.iter().find(|d| d.fields.iter().any(|f| f.name == field)).unwrap_or(&DESTS[0])
}

/// A field by its form name.
pub fn field(name: &str) -> Option<&'static Field> {
    DESTS.iter().flat_map(|d| d.fields).find(|f| f.name == name)
}

// --- URLs ---

/// `scheme://host[:port]` of an http(s) URL, lowercase, without the user
/// name and password; "" for anything else.
pub fn origin(url: &str) -> String {
    let lower = url.trim().to_ascii_lowercase();
    let Some((scheme, rest)) = lower.split_once("://").filter(|(s, _)| matches!(*s, "http" | "https")) else {
        return String::new();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host.is_empty() {
        return String::new();
    }
    format!("{scheme}://{host}")
}

/// The host (and port) of a URL, for the rules list and the form. One the
/// app saved without an http(s) scheme (it takes any text) shows the part
/// before its path if that looks like a host name, else nothing.
pub fn host(url: &str) -> String {
    let o = origin(url);
    if let Some((_, h)) = o.split_once("://") {
        return h.to_owned();
    }
    let rest = url.trim().split_once("://").map_or(url.trim(), |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let a = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (name, port) = a.rsplit_once(':').unwrap_or((a, "0"));
    let hostlike = name.contains('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit());
    if hostlike { a.to_ascii_lowercase() } else { String::new() }
}

/// The URL without a user name or password (they are a secret).
fn without_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_owned() };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_owned(),
    }
}

/// A server URL as pages may show it: without a user name and password,
/// a query or a fragment (ntfy takes `?auth=` as a credential).
fn shown_url(url: &str) -> String {
    let u = without_userinfo(url);
    let start = u.find("://").map_or(0, |i| i + 3);
    match u[start..].find(['?', '#']) {
        Some(end) => u[..start + end].to_owned(),
        None => u,
    }
}

fn check_url(url: &str) -> Result<(), &'static str> {
    if origin(url).is_empty() || url.chars().any(char::is_whitespace) {
        return Err(BAD_URL);
    }
    Ok(())
}

// --- What a page may show ---

/// The forwarding config as pages may show it (`GET` of the config, the
/// secrets dropped): `enabled`, `poll_interval_secs`, the two after-forward
/// flags, `last_forwarded_id` and `rules` (`public_rule`).
pub fn public(data: &Value) -> Value {
    let c = &data["config"];
    let rules: Vec<Value> = c["rules"].as_array().map(|rs| rs.iter().map(public_rule).collect()).unwrap_or_default();
    json!({
        "enabled": c["enabled"].as_bool().unwrap_or(false),
        "poll_interval_secs": c["poll_interval_secs"],
        "mark_read_after_forward": c["mark_read_after_forward"].as_bool().unwrap_or(false),
        "delete_after_forward": c["delete_after_forward"].as_bool().unwrap_or(false),
        "last_forwarded_id": data["last_forwarded_id"].as_u64().unwrap_or(0),
        "rules": rules,
    })
}

/// A rule without its secrets: `id`, `name`, `enabled`, `filter` (as
/// stored) and `destination`: its type, its shown fields (by the agent's
/// keys), the header names, `host` for a secret URL, and `kept`: the form
/// fields whose secret the rule has.
pub fn public_rule(r: &Value) -> Value {
    let d = &r["destination"];
    let mut m = Map::new();
    let mut kept = Vec::new();
    if let Some(spec) = d["type"].as_str().and_then(dest) {
        m.insert("type".into(), spec.key.into());
        for f in spec.fields {
            let v = &d[f.key];
            let set = v.as_str().is_some_and(|s| !s.is_empty());
            match f.kind {
                Kind::Secret if set => kept.push(f.name),
                Kind::SecretUrl if set => {
                    kept.push(f.name);
                    m.insert("host".into(), host(v.as_str().unwrap_or("")).into());
                }
                Kind::Secret | Kind::SecretUrl => {}
                Kind::Headers => {
                    let names: Vec<&str> = v.as_array().into_iter().flatten().filter_map(|h| h["name"].as_str()).collect();
                    m.insert("headers".into(), json!(names));
                }
                Kind::Url => {
                    m.insert(f.key.into(), shown_url(v.as_str().unwrap_or("")).into());
                }
                Kind::Check => {
                    m.insert(f.key.into(), v.as_bool().unwrap_or(false).into());
                }
                Kind::Text | Kind::Tel | Kind::Method => {
                    m.insert(f.key.into(), v.as_str().unwrap_or("").into());
                }
            }
        }
    }
    m.insert("kept".into(), json!(kept));
    json!({"id": r["id"], "name": r["name"], "enabled": r["enabled"], "filter": r["filter"], "destination": m})
}

fn list(v: &Value) -> String {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
}

/// The rules list's filter line (iOS `filterSummary`).
pub fn filter_summary(filter: &Value) -> String {
    let (p, k) = (list(&filter["patterns"]), list(&filter["keywords"]));
    match filter["type"].as_str().unwrap_or("all") {
        "sender" => format!("Sender: {p}"),
        "content" => format!("Keywords: {k}"),
        "sender_and_content" => format!("Sender: {p} + Keywords: {k}"),
        _ => "All messages".to_owned(),
    }
}

/// The rules list's destination line (iOS `destinationSummary`; a webhook
/// by its host only, never its URL). From a `public` destination.
pub fn dest_summary(d: &Value) -> String {
    let s = |k: &str| d[k].as_str().unwrap_or("").to_owned();
    match d["type"].as_str().unwrap_or("") {
        "telegram" => format!("Telegram (chat: {})", s("chat_id")),
        "webhook" if s("host").is_empty() => "Webhook".to_owned(),
        "webhook" => format!("Webhook ({})", s("host")),
        "sms" => format!("SMS ({})", s("forward_number")),
        "ntfy" => format!("ntfy ({})", s("topic")),
        "discord" => "Discord".to_owned(),
        "slack" => "Slack".to_owned(),
        _ => String::new(),
    }
}

// --- Secrets ---

/// Secrets of one destination: what a rule has, or what an answer's form
/// keeps (`store::Drafts`: only secrets typed into the form). Merged into a
/// post only for the same type and server. No `Debug`: it would print them.
#[derive(Clone, Default)]
pub struct Secrets {
    /// The destination type's key ("" none).
    pub dest: &'static str,
    /// The rule's own (`of`), not typed into the form.
    pub saved: bool,
    /// Where they go (`origin`; Telegram's API for a bot token).
    pub server: String,
    /// The webhook URL (its header values stay only with it).
    pub url: String,
    /// By form field name.
    pub fields: Vec<(&'static str, String)>,
    pub headers: Vec<(String, String)>,
}

impl Secrets {
    /// The secrets of a stored destination (the agent's format).
    pub fn of(d: &Value) -> Secrets {
        let Some(spec) = d["type"].as_str().and_then(dest) else { return Secrets::default() };
        let mut s = Secrets { dest: spec.key, saved: true, ..Secrets::default() };
        for f in spec.fields {
            let v = d[f.key].as_str().unwrap_or("");
            match f.kind {
                Kind::Secret | Kind::SecretUrl if !v.is_empty() => s.fields.push((f.name, v.to_owned())),
                Kind::Headers => {
                    s.headers = d[f.key]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|h| Some((h["name"].as_str()?.to_owned(), h["value"].as_str()?.to_owned())))
                        .collect();
                }
                _ => {}
            }
            if matches!(f.kind, Kind::Url | Kind::SecretUrl) {
                s.server = origin(v);
                s.url = v.to_owned();
            }
        }
        if spec.key == "telegram" {
            s.server = TELEGRAM.to_owned();
        }
        s
    }

    pub fn get(&self, field: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| *k == field).map(|(_, v)| v.as_str())
    }

    /// Whether a form of type `dest` has a secret here for `field`.
    pub fn has(&self, field: &str) -> bool {
        self.dest == owner(field).key && self.get(field).is_some()
    }

    /// Whether a header named `name` (any case) has a value here.
    pub fn has_header(&self, name: &str) -> bool {
        self.dest == "webhook" && self.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    }

    /// Whether anything was typed (a draft worth keeping).
    pub fn any(&self) -> bool {
        !self.fields.is_empty() || !self.headers.is_empty()
    }
}

// --- The rule form ---

/// A rule form as posted (or as a rule fills it): every type's fields as
/// typed, secrets included; only the chosen type's are used. No `Debug`:
/// it would print the secrets.
#[derive(Clone)]
pub struct Input {
    pub id: Option<u32>,
    pub name: String,
    pub filter: &'static str,
    pub patterns: String,
    pub keywords: String,
    pub dest: &'static Dest,
    pub values: Vec<(&'static str, String)>,
    /// Header rows (name, value) as typed, blank rows dropped.
    pub headers: Vec<(String, String)>,
    /// How many header rows the form had (it draws at least as many again).
    pub rows: usize,
    /// "Remove the saved token" (ntfy).
    pub clear_token: bool,
}

/// Every field's default, by name.
fn defaults() -> Vec<(&'static str, String)> {
    DESTS.iter().flat_map(|d| d.fields).map(|f| (f.name, f.default.to_owned())).collect()
}

impl Input {
    /// New Rule: the app's blank form (Telegram, all messages).
    pub fn blank() -> Input {
        Input {
            id: None,
            name: String::new(),
            filter: "all",
            patterns: String::new(),
            keywords: String::new(),
            dest: &DESTS[0],
            values: defaults(),
            headers: Vec::new(),
            rows: 0,
            clear_token: false,
        }
    }

    /// Edit Rule: a `public` rule's shown values (no secrets; the other
    /// types keep their defaults, as the app's draft).
    pub fn of_rule(r: &Value) -> Input {
        let mut input = Input::blank();
        input.id = r["id"].as_u64().and_then(|id| u32::try_from(id).ok());
        input.name = r["name"].as_str().unwrap_or("").to_owned();
        let filter = &r["filter"];
        input.filter = FILTERS.iter().find(|(k, _)| Some(*k) == filter["type"].as_str()).map_or("all", |(k, _)| k);
        input.patterns = list(&filter["patterns"]);
        input.keywords = list(&filter["keywords"]);
        let d = &r["destination"];
        input.dest = d["type"].as_str().and_then(dest).unwrap_or(&DESTS[0]);
        for f in input.dest.fields {
            let v = match f.kind {
                Kind::Check => if d[f.key].as_bool() == Some(true) { "1" } else { "" }.to_owned(),
                Kind::Text | Kind::Tel | Kind::Url | Kind::Method => d[f.key].as_str().unwrap_or(f.default).to_owned(),
                _ => continue,
            };
            input.set(f.name, v);
        }
        input.headers = d["headers"].as_array().into_iter().flatten().filter_map(|n| Some((n.as_str()?.to_owned(), String::new()))).collect();
        input
    }

    /// A posted form; `None` is a bad request (an unknown filter or type,
    /// a bad id).
    pub fn parse(form: &Form) -> Option<Input> {
        let id = match form.get("id") {
            None | Some("") => None,
            Some(raw) => Some(raw.parse::<u32>().ok().filter(|n| *n > 0 && raw.bytes().all(|b| b.is_ascii_digit()))?),
        };
        let filter = FILTERS.iter().find(|(k, _)| Some(*k) == form.get("ft"))?.0;
        let dest = dest(form.get("dt")?)?;
        let mut values = Vec::new();
        for f in DESTS.iter().flat_map(|d| d.fields) {
            // A field the post lacks (the SMS test question carries only
            // its type's) takes its default; a checkbox is sent only on.
            let raw = form.get(f.name).unwrap_or(if f.kind == Kind::Check { "" } else { f.default });
            let v = match f.kind {
                Kind::Check => if raw.is_empty() { "" } else { "1" }.to_owned(),
                Kind::Method if !matches!(raw, "POST" | "PUT") => return None,
                Kind::Headers => continue,
                _ => raw.trim().to_owned(),
            };
            values.push((f.name, v));
        }
        let headers = (0..MAX_HEADERS)
            .map(|i| {
                let get = |k: &str| form.get(&format!("{k}{i}")).unwrap_or("").trim().to_owned();
                let (name, value) = (get("wh_hn"), get("wh_hv"));
                // "Name: value" in the name box (the app's line format).
                match name.split_once(':') {
                    Some((n, v)) if value.is_empty() => (n.trim().to_owned(), v.trim().to_owned()),
                    _ => (name, value),
                }
            })
            .filter(|(n, v)| !n.is_empty() || !v.is_empty())
            .collect();
        let rows = (0..MAX_HEADERS).rev().find(|i| form.get(&format!("wh_hn{i}")).is_some()).map_or(0, |i| i + 1);
        Some(Input {
            id,
            name: form.get("name").unwrap_or("").trim().to_owned(),
            filter,
            patterns: form.get("patterns").unwrap_or("").trim().to_owned(),
            keywords: form.get("keywords").unwrap_or("").trim().to_owned(),
            dest,
            values,
            headers,
            rows,
            clear_token: form.get("nt_token_clear").is_some(),
        })
    }

    pub fn value(&self, name: &str) -> &str {
        self.values.iter().find(|(k, _)| *k == name).map_or("", |(_, v)| v.as_str())
    }

    fn set(&mut self, name: &'static str, v: String) {
        match self.values.iter_mut().find(|(k, _)| *k == name) {
            Some(slot) => slot.1 = v,
            None => self.values.push((name, v)),
        }
    }

    /// The same values without any secret (what a page may show again):
    /// no secret field, no header value, an ntfy URL without a user name,
    /// password, query or fragment, and no header name that is not one (a
    /// "Name: value" line pasted beside a value).
    pub fn shown(&self) -> Input {
        let mut shown = self.clone();
        for f in DESTS.iter().flat_map(|d| d.fields) {
            match f.kind {
                Kind::Secret | Kind::SecretUrl => shown.set(f.name, String::new()),
                Kind::Url => shown.set(f.name, shown_url(self.value(f.name))),
                _ => {}
            }
        }
        for h in &mut shown.headers {
            h.1.clear();
            if !header_name(&h.0) {
                h.0.clear();
            }
        }
        shown
    }
}

/// Field errors: (field name, message).
pub type Errors = Vec<(String, String)>;

/// Trimmed text: at most `max` characters and no control characters.
fn text(v: &str, max: usize) -> Result<(), String> {
    if v.chars().count() > max {
        Err(format!("Enter at most {max} characters."))
    } else if v.chars().any(char::is_control) {
        Err("Remove line breaks and control characters.".to_owned())
    } else {
        Ok(())
    }
}

/// An HTTP header name (RFC 9110 token).
fn header_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Headers a rule may not set: they say where or how a request goes, not
/// what it carries (a `Host` row would send the kept headers, over the
/// same connection, to another site on that address).
fn reserved(n: &str) -> bool {
    let n = n.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "host" | "content-length" | "transfer-encoding" | "connection" | "keep-alive" | "te" | "trailer" | "upgrade" | "expect"
            | "forwarded" | "x-host" | "x-original-host"
    ) || n.starts_with("proxy-")
        || n.starts_with("x-forwarded-")
}

/// The comma list of the app (`splitCSV`): trimmed, empties dropped.
pub fn split(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect()
}

/// The posted destination with the secrets it keeps: its wire body (the
/// agent's `ForwardDestination`) or the field errors, and the secrets an
/// answer's form keeps (`store::Drafts`).
pub struct Resolved {
    pub dest: Result<Value, Errors>,
    pub secrets: Secrets,
}

/// Resolve the chosen destination of `input`. An empty secret field takes
/// the first of `sources` (the form's draft, then the rule) of the same
/// type that has it, a bot or ntfy token only for the same server, header
/// values only for the same webhook URL; else a required secret is an
/// error. What an answer keeps is only what was typed (in this post or an
/// earlier answer's draft) and passed its checks, never the rule's own:
/// the rule is read again at every post.
pub fn resolve(input: &Input, sources: &[&Secrets]) -> Resolved {
    let d = input.dest;
    let same = |s: &&&Secrets| s.dest == d.key;
    let mut errors: Errors = Vec::new();
    let mut keep = Secrets { dest: d.key, ..Secrets::default() };
    let mut wire = Map::new();
    wire.insert("type".into(), d.key.into());
    // The URL first: it names the server the other secrets go to.
    for f in d.fields.iter().filter(|f| matches!(f.kind, Kind::Url | Kind::SecretUrl)) {
        let typed = input.value(f.name);
        // The URL and whether it is the rule's as saved.
        let (url, saved) = match f.kind {
            Kind::SecretUrl if typed.is_empty() => sources
                .iter()
                .filter(same)
                .find_map(|s| Some((s.get(f.name)?.to_owned(), s.saved)))
                .unwrap_or_default(),
            // The shown ntfy URL lacks a saved user name, password and
            // query: the same URL keeps them.
            Kind::Url => sources
                .iter()
                .filter(same)
                .find(|s| s.saved && !s.url.is_empty() && shown_url(&s.url) == typed)
                .map_or_else(|| (typed.to_owned(), false), |s| (s.url.clone(), true)),
            _ => (typed.to_owned(), false),
        };
        let err = if url.is_empty() {
            Some(REQUIRED.to_owned())
        } else if let Err(e) = text(&url, URL_MAX) {
            Some(e)
        } else if check_url(&url).is_err() {
            Some(if saved { SAVED_URL } else { BAD_URL }.to_owned())
        } else if url != typed {
            None
        } else if f.kind == Kind::Url && without_userinfo(&url) != url {
            Some("Leave the user name and password out of the URL.".to_owned())
        } else if f.kind == Kind::Url && url.contains(['?', '#']) {
            Some("Enter the server address only, without ? or #.".to_owned())
        } else {
            None
        };
        match err {
            Some(e) => errors.push((f.name.to_owned(), e)),
            None if f.kind == Kind::SecretUrl && !saved => keep.fields.push((f.name, url.clone())),
            None => {}
        }
        keep.server = origin(&url);
        keep.url = url.clone();
        wire.insert(f.key.into(), url.into());
    }
    if d.key == "telegram" {
        keep.server = TELEGRAM.to_owned();
    }
    let server = keep.server.clone();
    let webhook_url = keep.url.clone();
    for f in d.fields {
        let typed = input.value(f.name);
        match f.kind {
            Kind::Url | Kind::SecretUrl => {}
            Kind::Secret => {
                let mut err = None;
                let mut saved = false;
                let v = if !typed.is_empty() {
                    typed.to_owned()
                } else if f.name == "nt_token" && input.clear_token {
                    String::new()
                } else {
                    let found = sources.iter().filter(same).find(|s| s.server == server && s.get(f.name).is_some());
                    match found.and_then(|s| Some((s.get(f.name)?, s.saved))) {
                        Some((v, from_rule)) => {
                            saved = from_rule;
                            v.to_owned()
                        }
                        // Kept for another server: never sent there (an
                        // optional one too: typed again, or removed).
                        None if sources.iter().filter(same).any(|s| s.get(f.name).is_some()) => {
                            err = Some(AGAIN.to_owned());
                            String::new()
                        }
                        None if !f.required => String::new(),
                        None => {
                            err = Some(REQUIRED.to_owned());
                            String::new()
                        }
                    }
                };
                if err.is_none() {
                    err = text(&v, TEXT_MAX).err();
                }
                match err {
                    Some(e) => errors.push((f.name.to_owned(), e)),
                    None if !v.is_empty() && !saved => keep.fields.push((f.name, v.clone())),
                    None => {}
                }
                if !v.is_empty() {
                    wire.insert(f.key.into(), v.into());
                } else if f.required {
                    wire.insert(f.key.into(), "".into());
                }
            }
            Kind::Check => {
                wire.insert(f.key.into(), (typed == "1").into());
            }
            Kind::Method => {
                wire.insert(f.key.into(), if typed == "PUT" { "PUT" } else { "POST" }.into());
            }
            Kind::Headers => {
                let mut taken: Vec<Vec<bool>> = sources.iter().map(|s| vec![false; s.headers.len()]).collect();
                let mut rows = Vec::new();
                for (i, (name, value)) in input.headers.iter().enumerate() {
                    let field = |k: &str| format!("{k}{i}");
                    if name.is_empty() {
                        errors.push((field("wh_hn"), "Enter the header name.".to_owned()));
                        continue;
                    }
                    if !header_name(name) {
                        errors.push((field("wh_hn"), "Not a valid header name.".to_owned()));
                        continue;
                    }
                    if reserved(name) {
                        errors.push((field("wh_hn"), "This header can't be set.".to_owned()));
                        continue;
                    }
                    // The value and whether it is the rule's as saved.
                    let v = if !value.is_empty() {
                        Some((value.clone(), false))
                    } else {
                        // The next saved value of that name (any case), same URL only.
                        let mut found = None;
                        for (si, s) in sources.iter().enumerate().filter(|(_, s)| s.dest == "webhook" && s.url == webhook_url) {
                            if let Some(hi) = s.headers.iter().enumerate().position(|(hi, (n, _))| !taken[si][hi] && n.eq_ignore_ascii_case(name)) {
                                taken[si][hi] = true;
                                found = Some((s.headers[hi].1.clone(), s.saved));
                                break;
                            }
                        }
                        found
                    };
                    match v {
                        Some((v, saved)) => match text(&v, URL_MAX) {
                            Ok(()) => {
                                if !saved {
                                    keep.headers.push((name.clone(), v.clone()));
                                }
                                rows.push((name.clone(), v));
                            }
                            Err(e) => errors.push((field("wh_hv"), e)),
                        },
                        None if sources.iter().any(|s| s.has_header(name)) => errors.push((field("wh_hv"), AGAIN_URL.to_owned())),
                        None => errors.push((field("wh_hv"), "Enter the value.".to_owned())),
                    }
                }
                wire.insert(f.key.into(), json!(rows.iter().map(|(n, v)| json!({"name": n, "value": v})).collect::<Vec<_>>()));
            }
            Kind::Text | Kind::Tel => {
                let err = if typed.is_empty() && f.required {
                    Some(REQUIRED.to_owned())
                } else if f.kind == Kind::Tel {
                    super::sms::check_number(&super::sms::clean_number(typed)).err()
                } else if f.name == "nt_topic" && !typed.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                    Some("Use letters, digits, - and _ only.".to_owned())
                } else {
                    text(typed, if f.name == "nt_topic" { 64 } else { TEXT_MAX }).err()
                };
                if let Some(e) = err {
                    errors.push((f.name.to_owned(), e));
                }
                let v = if f.kind == Kind::Tel { super::sms::clean_number(typed) } else { typed.to_owned() };
                wire.insert(f.key.into(), v.into());
            }
        }
    }
    let dest = if errors.is_empty() { Ok(Value::Object(wire)) } else { Err(errors) };
    Resolved { dest, secrets: keep }
}

/// The rule's name and filter (iOS `filterToDict`), or their errors.
pub fn rule_parts(input: &Input) -> Result<(String, Value), Errors> {
    let mut errors = Vec::new();
    if input.name.is_empty() {
        errors.push(("name".to_owned(), REQUIRED.to_owned()));
    } else if let Err(e) = text(&input.name, NAME_MAX) {
        errors.push(("name".to_owned(), e));
    }
    for (field, v) in [("patterns", &input.patterns), ("keywords", &input.keywords)] {
        if let Err(e) = text(v, LIST_MAX) {
            errors.push((field.to_owned(), e));
        }
    }
    let filter = match input.filter {
        "sender" => json!({"type": "sender", "patterns": split(&input.patterns)}),
        "content" => json!({"type": "content", "keywords": split(&input.keywords)}),
        "sender_and_content" => {
            json!({"type": "sender_and_content", "patterns": split(&input.patterns), "keywords": split(&input.keywords)})
        }
        _ => json!({"type": "all"}),
    };
    if errors.is_empty() {
        Ok((input.name.clone(), filter))
    } else {
        Err(errors)
    }
}

/// What a single-use token is claimed for: the post as sent (every field
/// but the tokens and the confirmation's buttons), so a replay of the
/// same post is recognised and another one is an expired form.
pub fn digest(test: bool, form: &Form) -> u64 {
    let mut h = DefaultHasher::new();
    test.hash(&mut h);
    for (k, v) in form.pairs().iter().filter(|(k, _)| !matches!(k.as_str(), "csrf" | "once" | "confirm" | "back")) {
        (k, v).hash(&mut h);
    }
    h.finish()
}

// --- Device calls ---

/// A handler's `(status, envelope)`: its `data`, or its status and error.
pub fn data((status, mut v): (u16, Value)) -> Result<Value, (u16, String)> {
    if status < 400 {
        Ok(v["data"].take())
    } else {
        Err((status, v["error"].as_str().unwrap_or("error").to_owned()))
    }
}

/// An API route of the forwarder, in-process. Tests answer a test send
/// and a retry from the fake (they would reach the network), and the log
/// and the config's read from it when one is set; the config and rules are
/// otherwise each test `AppState`'s own, in memory.
pub fn api(app: &AppState, method: Method, path: &'static str, body: &Value) -> (u16, Value) {
    #[cfg(test)]
    if let Some(answer) = fake::answer(app, &method, path, body) {
        return answer;
    }
    crate::server::route(&method, path, app, body.to_string().as_bytes())
}

/// The config and the last forwarded id (`GET`, with the secrets: for a
/// merge, never for a page).
pub fn read(app: &AppState) -> Result<Value, String> {
    data(api(app, Method::Get, CONFIG, &Value::Null)).map_err(|(_, e)| e)
}

/// The config as pages may show it (`public`).
pub fn public_read(app: &AppState, _: Option<&str>) -> Result<Value, String> {
    read(app).map(|d| public(&d))
}

/// The stored rule `id` of a config read.
pub fn rule(data: &Value, id: u32) -> Option<&Value> {
    data["config"]["rules"].as_array()?.iter().find(|r| r["id"].as_u64() == Some(u64::from(id)))
}

// --- The Forward Log ---

/// A forwarder error as pages may show it. The agent's errors are its own
/// words plus the HTTP client's, and two of those quote the request's URI
/// ("bad uri: <uri> is missing scheme", "configured for https only:
/// <uri>"), which can hold a secret (a webhook's path, a bot token): that
/// part is dropped ("; held for retry" stays).
pub fn redact(e: &str) -> String {
    for marker in ["bad uri: ", "configured for https only: "] {
        if let Some(at) = e.find(marker) {
            let tail = e.rfind("; held for retry").filter(|t| *t > at).map_or("", |t| &e[t..]);
            return format!("{}{tail}", &e[..at + marker.len() - 2]);
        }
    }
    e.to_owned()
}

/// The log as pages may show it (`GET`, newest first, as the agent's
/// retry counts): each entry's `index` in the agent's list, `timestamp`,
/// `sms_id`, `rule_id`, `sender`, `content_preview`, `rule_name`,
/// `destination_type`, `success`, `pending` and `error` (redacted). Never
/// the message's `content` or `date` (the app does not show them either).
/// An entry without a time or SMS id is left out, as the app's parser does.
pub fn public_log(app: &AppState, _: Option<&str>) -> Result<Value, String> {
    let log = data(api(app, Method::Get, LOG, &Value::Null)).map_err(|(_, e)| e)?;
    let entries = log.as_array().into_iter().flatten().enumerate().filter_map(|(i, e)| {
        let s = |k: &str| e[k].as_str().unwrap_or("");
        Some(json!({
            "index": i,
            "timestamp": e["timestamp"].as_i64()?,
            "sms_id": e["sms_id"].as_u64()?,
            "rule_id": e["rule_id"].as_u64().unwrap_or(0),
            "sender": s("sender"),
            "content_preview": s("content_preview"),
            "rule_name": s("rule_name"),
            "destination_type": s("destination_type"),
            "success": e["success"].as_bool().unwrap_or(false),
            "pending": e["pending"].as_bool().unwrap_or(false),
            "error": e["error"].as_str().map(redact),
        }))
    });
    Ok(Value::Array(entries.collect()))
}

/// Which entry a Retry button is for: its index when the page was drawn
/// and what identifies it (the agent's own key for a retry's outcome).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fingerprint {
    pub index: usize,
    pub sms_id: u64,
    pub rule_id: u64,
    pub timestamp: i64,
}

impl Fingerprint {
    pub fn of(e: &Value) -> Fingerprint {
        Fingerprint {
            index: e["index"].as_u64().unwrap_or(0) as usize,
            sms_id: e["sms_id"].as_u64().unwrap_or(0),
            rule_id: e["rule_id"].as_u64().unwrap_or(0),
            timestamp: e["timestamp"].as_i64().unwrap_or(0),
        }
    }

    /// From a Retry form (`i`, `sms`, `rule`, `ts`); `None` if any is not
    /// a number.
    pub fn parse(form: &Form) -> Option<Fingerprint> {
        let n = |k: &str| form.get(k).filter(|v| v.len() <= 20);
        Some(Fingerprint {
            index: n("i")?.parse().ok().filter(|i| *i < 100_000)?,
            sms_id: n("sms")?.parse().ok()?,
            rule_id: n("rule")?.parse().ok()?,
            timestamp: n("ts")?.parse().ok()?,
        })
    }

    pub fn digest(&self) -> u64 {
        let mut h = DefaultHasher::new();
        ("retry", self).hash(&mut h);
        h.finish()
    }

    /// The entry's index in `log` (an agent read, newest first) now: the
    /// one drawn if it is still there, else where it moved (new entries
    /// come first); `None` once it is gone or was retried (a retry
    /// changes its time).
    pub fn find(&self, log: &Value) -> Option<usize> {
        let entries = log.as_array()?;
        let same = |e: &Value| {
            e["sms_id"].as_u64() == Some(self.sms_id)
                && e["rule_id"].as_u64().unwrap_or(0) == self.rule_id
                && e["timestamp"].as_i64() == Some(self.timestamp)
        };
        entries.get(self.index).filter(|e| same(e)).map(|_| self.index).or_else(|| entries.iter().position(same))
    }
}

/// Test only: the destinations sent to `POST /api/sms/forward/test`, the
/// retries sent to `POST /api/sms/forward/retry`, their answer, a log for
/// `GET /api/sms/forward/log` (newest first) and an answer for `GET
/// /api/sms/forward/config`, per `AppState` (a test send or a retry would
/// reach the network; the agent's log and last forwarded id cannot be set
/// from outside, and its config read never fails).
#[cfg(test)]
pub mod fake {
    use std::sync::{Mutex, PoisonError};

    use serde_json::{json, Value};

    use crate::handlers::AppState;
    use crate::server::Method;

    #[derive(Default)]
    pub struct Device {
        pub tests: Vec<Value>,
        pub retries: Vec<Value>,
        pub reply: (u16, Value),
        /// Without one the agent's own (empty) log answers.
        pub log: Option<Vec<Value>>,
        /// The log's read fails with this.
        pub log_error: Option<String>,
        /// Without one the agent's own config answers its read.
        pub config: Option<(u16, Value)>,
    }

    static DEVICES: Mutex<Vec<(usize, Device)>> = Mutex::new(Vec::new());

    fn key(app: &AppState) -> usize {
        app as *const AppState as usize
    }

    pub struct Guard(usize);

    impl Drop for Guard {
        fn drop(&mut self) {
            DEVICES.lock().unwrap_or_else(PoisonError::into_inner).retain(|(k, _)| *k != self.0);
        }
    }

    /// Test sends and retries of `app` answer `reply` and are recorded.
    #[must_use]
    pub fn set(app: &AppState, reply: (u16, Value)) -> Guard {
        set_log(app, reply, None)
    }

    /// The same, and `log` answers the log's reads.
    #[must_use]
    pub fn set_log(app: &AppState, reply: (u16, Value), log: Option<Vec<Value>>) -> Guard {
        let mut all = DEVICES.lock().unwrap_or_else(PoisonError::into_inner);
        all.retain(|(k, _)| *k != key(app));
        all.push((key(app), Device { reply, log, ..Device::default() }));
        Guard(key(app))
    }

    /// Change `app`'s fake device.
    pub fn with<R>(app: &AppState, f: impl FnOnce(&mut Device) -> R) -> Option<R> {
        let mut all = DEVICES.lock().unwrap_or_else(PoisonError::into_inner);
        all.iter_mut().find(|(k, _)| *k == key(app)).map(|(_, d)| f(d))
    }

    pub fn tests(app: &AppState) -> Vec<Value> {
        with(app, |d| d.tests.clone()).unwrap_or_default()
    }

    pub fn retries(app: &AppState) -> Vec<Value> {
        with(app, |d| d.retries.clone()).unwrap_or_default()
    }

    /// The fake's answer to a forwarder route, if it has one.
    pub fn answer(app: &AppState, method: &Method, path: &str, body: &Value) -> Option<(u16, Value)> {
        let none = || (503, json!({"ok": false, "error": "no test device"}));
        match path {
            super::TEST => Some(
                with(app, |d| {
                    d.tests.push(body.clone());
                    d.reply.clone()
                })
                .unwrap_or_else(none),
            ),
            super::RETRY => Some(
                with(app, |d| {
                    d.retries.push(body.clone());
                    d.reply.clone()
                })
                .unwrap_or_else(none),
            ),
            super::CONFIG if matches!(method, Method::Get) => with(app, |d| d.config.clone()).flatten(),
            super::LOG => with(app, |d| match (&d.log_error, &d.log) {
                (Some(e), _) => Some((500, json!({"ok": false, "error": e}))),
                (None, Some(l)) => Some((200, json!({"ok": true, "data": l}))),
                (None, None) => None,
            })
            .flatten(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sms_forward::{ForwardDestination, SmsFilter};

    /// Fixture secrets: never on a page.
    pub const TOKEN: &str = "123456:FAKE-TOKEN";
    const AUTH: &str = "Bearer FAKE-SECRET";

    fn form(pairs: &[(&str, &str)]) -> Form {
        let body: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={}", super::super::session::percent_encode(v))).collect();
        Form::parse(body.join("&").as_bytes())
    }

    fn input(pairs: &[(&str, &str)]) -> Input {
        let mut all = vec![("name", "Fake rule"), ("ft", "all")];
        all.extend_from_slice(pairs);
        Input::parse(&form(&all)).expect("a valid form")
    }

    fn resolved(pairs: &[(&str, &str)], sources: &[&Secrets]) -> Result<Value, Errors> {
        resolve(&input(pairs), sources).dest
    }

    fn errors(pairs: &[(&str, &str)], sources: &[&Secrets]) -> Errors {
        resolved(pairs, sources).expect_err("errors")
    }

    fn err(name: &str, msg: &str) -> (String, String) {
        (name.to_owned(), msg.to_owned())
    }

    /// Every type's form gives the app's wire format (iOS
    /// `destinationToDict`), which the agent's own types read back
    /// unchanged; the comma lists and filters as `filterToDict`.
    #[test]
    fn every_type_round_trips_to_the_wire_format() {
        let cases = [
            (
                vec![("dt", "telegram"), ("tg_token", TOKEN), ("tg_chat", " -100123 "), ("tg_silent", "1")],
                json!({"type": "telegram", "bot_token": TOKEN, "chat_id": "-100123", "silent": true}),
            ),
            (
                vec![("dt", "telegram"), ("tg_token", TOKEN), ("tg_chat", "42")],
                json!({"type": "telegram", "bot_token": TOKEN, "chat_id": "42", "silent": false}),
            ),
            (
                vec![
                    ("dt", "webhook"), ("wh_url", "https://example.com/hook"), ("wh_method", "PUT"),
                    ("wh_hn0", "X-Request-Source"), ("wh_hv0", "u60-test"), ("wh_hn2", "X-Fake"), ("wh_hv2", "1"),
                ],
                json!({"type": "webhook", "url": "https://example.com/hook", "method": "PUT",
                       "headers": [{"name": "X-Request-Source", "value": "u60-test"}, {"name": "X-Fake", "value": "1"}]}),
            ),
            (
                vec![("dt", "webhook"), ("wh_url", "http://example.com/hook"), ("wh_method", "POST")],
                json!({"type": "webhook", "url": "http://example.com/hook", "method": "POST", "headers": []}),
            ),
            (vec![("dt", "sms"), ("sms_number", "+1 (555) 555-0123")], json!({"type": "sms", "forward_number": "+15555550123"})),
            (
                vec![("dt", "ntfy"), ("nt_url", "https://ntfy.sh"), ("nt_topic", "fake_topic-1")],
                json!({"type": "ntfy", "url": "https://ntfy.sh", "topic": "fake_topic-1"}),
            ),
            (
                vec![("dt", "ntfy"), ("nt_url", "https://ntfy.example.com"), ("nt_topic", "t"), ("nt_token", "tk_FAKE")],
                json!({"type": "ntfy", "url": "https://ntfy.example.com", "topic": "t", "token": "tk_FAKE"}),
            ),
            (
                vec![("dt", "discord"), ("dc_url", "https://discord.com/api/webhooks/1/FAKE-DISCORD")],
                json!({"type": "discord", "webhook_url": "https://discord.com/api/webhooks/1/FAKE-DISCORD"}),
            ),
            (
                vec![("dt", "slack"), ("sl_url", "https://hooks.slack.com/services/FAKE-SLACK")],
                json!({"type": "slack", "webhook_url": "https://hooks.slack.com/services/FAKE-SLACK"}),
            ),
        ];
        for (pairs, want) in cases {
            let got = resolved(&pairs, &[]).unwrap_or_else(|e| panic!("{pairs:?}: {e:?}"));
            assert_eq!(got, want);
            let typed: ForwardDestination = serde_json::from_value(got.clone()).unwrap();
            assert_eq!(serde_json::to_value(typed).unwrap(), got);
            // What a new rule is created with: the secrets kept for the next post.
            let keep = resolve(&input(&pairs), &[]).secrets;
            assert_eq!(keep.dest, want["type"]);
        }
        for (ft, p, k, want) in [
            ("all", "a", "b", json!({"type": "all"})),
            ("sender", " +1555*, ,*0123 ", "", json!({"type": "sender", "patterns": ["+1555*", "*0123"]})),
            ("content", "", "code, OTP,", json!({"type": "content", "keywords": ["code", "OTP"]})),
            ("sender_and_content", "BANK", "", json!({"type": "sender_and_content", "patterns": ["BANK"], "keywords": []})),
        ] {
            let i = Input::parse(&form(&[("name", " R "), ("ft", ft), ("patterns", p), ("keywords", k), ("dt", "sms")])).unwrap();
            let (name, filter) = rule_parts(&i).unwrap();
            assert_eq!((name.as_str(), &filter), ("R", &want));
            let typed: SmsFilter = serde_json::from_value(filter.clone()).unwrap();
            assert_eq!(serde_json::to_value(typed).unwrap(), filter);
        }
    }

    /// A type, filter or method outside the tables, or a bad id, is a bad
    /// request; checks with a message per field otherwise.
    #[test]
    fn form_checks() {
        for bad in [
            vec![("ft", "all"), ("dt", "email")],
            vec![("ft", "regex"), ("dt", "sms")],
            vec![("ft", "all"), ("dt", "webhook"), ("wh_method", "GET")],
            vec![("ft", "all"), ("dt", "sms"), ("id", "0")],
            vec![("ft", "all"), ("dt", "sms"), ("id", "+1")],
            vec![("ft", "all")],
        ] {
            assert!(Input::parse(&form(&bad)).is_none(), "{bad:?}");
        }
        assert_eq!(Input::parse(&form(&[("ft", "all"), ("dt", "sms"), ("id", "7")])).unwrap().id, Some(7));
        assert_eq!(
            errors(&[("dt", "telegram")], &[]),
            vec![err("tg_token", REQUIRED), err("tg_chat", REQUIRED)]
        );
        assert_eq!(errors(&[("dt", "webhook"), ("wh_url", "ftp://example.com")], &[])[0].1, "Enter a URL starting with https:// or http://.");
        assert_eq!(errors(&[("dt", "discord"), ("dc_url", "https://")], &[])[0].0, "dc_url");
        assert_eq!(
            errors(&[("dt", "ntfy"), ("nt_url", "https://user:pw@ntfy.example.com"), ("nt_topic", "a/b")], &[]),
            vec![err("nt_url", "Leave the user name and password out of the URL."), err("nt_topic", "Use letters, digits, - and _ only.")]
        );
        assert_eq!(errors(&[("dt", "sms"), ("sms_number", "+1555<0123>")], &[])[0].0, "sms_number");
        assert_eq!(errors(&[("dt", "telegram"), ("tg_token", "a\u{7}b"), ("tg_chat", "1")], &[])[0].1, "Remove line breaks and control characters.");
        // Header rows: a value without a name, a bad name, a name without a value.
        let e = errors(
            &[("dt", "webhook"), ("wh_url", "https://example.com/h"), ("wh_hv0", "v"), ("wh_hn1", "Bad Name"), ("wh_hv1", "v"), ("wh_hn2", "X-New")],
            &[],
        );
        assert_eq!(e, vec![err("wh_hn0", "Enter the header name."), err("wh_hn1", "Not a valid header name."), err("wh_hv2", "Enter the value.")]);
        // Name and lists.
        let blank = Input::parse(&form(&[("name", "  "), ("ft", "all"), ("dt", "sms")])).unwrap();
        assert_eq!(rule_parts(&blank).unwrap_err(), vec![err("name", REQUIRED)]);
        let long = Input::parse(&form(&[("name", &"n".repeat(101)), ("ft", "all"), ("dt", "sms")])).unwrap();
        assert_eq!(rule_parts(&long).unwrap_err(), vec![err("name", "Enter at most 100 characters.")]);
    }

    fn stored(d: Value) -> Secrets {
        Secrets::of(&d)
    }

    /// An empty secret keeps the saved one only for the same type and
    /// server (a bot token: the type); header values only for the same
    /// webhook URL; a draft (secrets typed into an earlier answer of the
    /// form) comes before the rule.
    #[test]
    fn secrets_merge_only_for_the_same_type_and_server() {
        let tg = stored(json!({"type": "telegram", "bot_token": TOKEN, "chat_id": "1", "silent": false}));
        let ok = resolved(&[("dt", "telegram"), ("tg_chat", "2")], &[&tg]).unwrap();
        assert_eq!(ok["bot_token"], TOKEN);
        // Another type's rule keeps nothing.
        let wh = stored(json!({"type": "webhook", "url": "https://example.com/hook/FAKE-PATH", "method": "POST",
                               "headers": [{"name": "Authorization", "value": AUTH}, {"name": "X-Two", "value": "2"}]}));
        assert_eq!(errors(&[("dt", "telegram"), ("tg_chat", "2")], &[&wh]), vec![err("tg_token", REQUIRED)]);
        // ntfy: the same server keeps the token; another server needs it again.
        let nt = stored(json!({"type": "ntfy", "url": "https://ntfy.example.com", "topic": "t", "token": "tk_FAKE"}));
        let same = resolved(&[("dt", "ntfy"), ("nt_url", "HTTPS://NTFY.example.com/"), ("nt_topic", "other")], &[&nt]).unwrap();
        assert_eq!(same["token"], "tk_FAKE");
        let moved = &[("dt", "ntfy"), ("nt_url", "https://evil.example"), ("nt_topic", "t")];
        assert_eq!(errors(moved, &[&nt]), vec![err("nt_token", "Enter it again: the server changed.")]);
        let typed = resolved(&[moved.as_slice(), &[("nt_token", "tk_NEW")]].concat(), &[&nt]).unwrap();
        assert_eq!(typed["token"], "tk_NEW");
        let removed = resolved(&[moved.as_slice(), &[("nt_token_clear", "1")]].concat(), &[&nt]).unwrap();
        assert_eq!(removed.get("token"), None);
        let cleared = resolved(&[("dt", "ntfy"), ("nt_url", "https://ntfy.example.com"), ("nt_topic", "t"), ("nt_token_clear", "1")], &[&nt]);
        assert_eq!(cleared.unwrap().get("token"), None);
        // Webhook: an empty URL keeps the saved one, and its header values by name (any case).
        let kept = resolved(&[("dt", "webhook"), ("wh_hn0", "authorization"), ("wh_hn1", "X-Two"), ("wh_hv1", "new")], &[&wh]).unwrap();
        assert_eq!(kept["url"], "https://example.com/hook/FAKE-PATH");
        assert_eq!(kept["headers"], json!([{"name": "authorization", "value": AUTH}, {"name": "X-Two", "value": "new"}]));
        let typed_same = resolved(&[("dt", "webhook"), ("wh_url", "https://example.com/hook/FAKE-PATH"), ("wh_hn0", "Authorization")], &[&wh]);
        assert_eq!(typed_same.unwrap()["headers"][0]["value"], AUTH);
        // Another URL, even on the same host: the header values are typed again.
        for url in ["https://example.com/other", "https://evil.example/hook/FAKE-PATH"] {
            assert_eq!(
                errors(&[("dt", "webhook"), ("wh_url", url), ("wh_hn0", "Authorization")], &[&wh]),
                vec![err("wh_hv0", "Enter the value again: the URL changed.")]
            );
        }
        // Discord: an empty URL keeps the saved one; a Slack rule's never.
        let dc = stored(json!({"type": "discord", "webhook_url": "https://discord.com/api/webhooks/1/FAKE-DISCORD"}));
        assert_eq!(resolved(&[("dt", "discord")], &[&dc]).unwrap()["webhook_url"], "https://discord.com/api/webhooks/1/FAKE-DISCORD");
        assert_eq!(errors(&[("dt", "slack")], &[&dc]), vec![err("sl_url", REQUIRED)]);
        // A draft first: a token typed into the last answer, for the same type.
        let draft = Secrets { dest: "telegram", server: TELEGRAM.into(), fields: vec![("tg_token", "999:NEW-FAKE".into())], ..Secrets::default() };
        assert_eq!(resolved(&[("dt", "telegram"), ("tg_chat", "2")], &[&draft, &tg]).unwrap()["bot_token"], "999:NEW-FAKE");
        // A draft for another ntfy server is not used there.
        let nt_draft = Secrets { dest: "ntfy", server: "https://a.example".into(), url: "https://a.example".into(),
                                 fields: vec![("nt_token", "tk_A".into())], ..Secrets::default() };
        let e = errors(&[("dt", "ntfy"), ("nt_url", "https://b.example"), ("nt_topic", "t")], &[&nt_draft]);
        assert_eq!(e, vec![err("nt_token", "Enter it again: the server changed.")]);
        // What an answer keeps: the chosen type's secrets typed into the
        // form (valid ones), never the rule's own.
        let pairs = [("dt", "webhook"), ("wh_hn0", "Authorization"), ("wh_hn1", "X-Two"), ("wh_hv1", "new"), ("tg_token", "x")];
        let keep = resolve(&input(&pairs), &[&wh]).secrets;
        assert_eq!((keep.dest, keep.saved, keep.url.as_str()), ("webhook", false, "https://example.com/hook/FAKE-PATH"));
        assert_eq!(keep.get("wh_url"), None);
        assert_eq!(keep.headers, vec![("X-Two".to_owned(), "new".to_owned())]);
        assert_eq!(keep.get("tg_token"), None);
        let typed = resolve(&input(&[("dt", "discord"), ("dc_url", "https://discord.com/api/webhooks/2/FAKE-NEW")]), &[&dc]).secrets;
        assert_eq!(typed.get("dc_url"), Some("https://discord.com/api/webhooks/2/FAKE-NEW"));
        let refused = resolve(&input(&[("dt", "discord"), ("dc_url", "discord.com/x")]), &[]).secrets;
        assert!(!refused.any(), "a refused URL is not kept");
        // A stale draft never brings back a rotated secret: the token kept
        // from the rule is not in the draft, so the rule's new one wins.
        let draft = resolve(&input(&[("dt", "telegram"), ("tg_chat", "2")]), &[&tg]).secrets;
        assert!(!draft.any());
        let rotated = stored(json!({"type": "telegram", "bot_token": "777:ROTATED-FAKE", "chat_id": "1"}));
        assert_eq!(resolved(&[("dt", "telegram"), ("tg_chat", "2")], &[&draft, &rotated]).unwrap()["bot_token"], "777:ROTATED-FAKE");
        // A stored ntfy URL with a user name, password and query shows
        // without them, and the same shown URL keeps them.
        let ui = stored(json!({"type": "ntfy", "url": "https://u:pw@ntfy.example.com/base?auth=FAKE-Q#f", "topic": "t"}));
        let r = resolved(&[("dt", "ntfy"), ("nt_url", "https://ntfy.example.com/base"), ("nt_topic", "t")], &[&ui]).unwrap();
        assert_eq!(r["url"], "https://u:pw@ntfy.example.com/base?auth=FAKE-Q#f");
        // Only the rule's: a draft's URL is what was typed.
        let not_saved = Secrets { saved: false, ..stored(json!({"type": "ntfy", "url": "https://u:pw@ntfy.example.com", "topic": "t"})) };
        let r = resolved(&[("dt", "ntfy"), ("nt_url", "https://ntfy.example.com"), ("nt_topic", "t")], &[&not_saved]).unwrap();
        assert_eq!(r["url"], "https://ntfy.example.com");
        // The same server is the same scheme, host and port; a path or the
        // case may change.
        for url in ["http://ntfy.example.com", "https://ntfy.example.com:8443", "https://ntfy.example.com:443", "https://ntfy.example.com.evil.example"] {
            assert_eq!(errors(&[("dt", "ntfy"), ("nt_url", url), ("nt_topic", "t")], &[&nt]), vec![err("nt_token", AGAIN)], "{url}");
        }
        for url in ["https://ntfy.example.com/sub", "https://NTFY.EXAMPLE.COM"] {
            assert_eq!(resolved(&[("dt", "ntfy"), ("nt_url", url), ("nt_topic", "t")], &[&nt]).unwrap()["token"], "tk_FAKE", "{url}");
        }
        // Headers that say where a request goes can't be set (a Host row
        // would send the kept Authorization to another site on that address).
        for name in ["Host", "host", "X-Forwarded-Host", "Forwarded", "Proxy-Authorization", "Transfer-Encoding", "Content-Length", "Connection"] {
            let e = errors(&[("dt", "webhook"), ("wh_hn0", "Authorization"), ("wh_hn1", name), ("wh_hv1", "attacker.example")], &[&wh]);
            assert_eq!(e, vec![err("wh_hn1", "This header can't be set.")], "{name}");
        }
        // A saved URL the app took without a scheme: said so, not "Enter a URL".
        let bare = stored(json!({"type": "webhook", "url": "example.com/hook/FAKE-PATH", "method": "POST", "headers": []}));
        assert_eq!(errors(&[("dt", "webhook")], &[&bare]), vec![err("wh_url", SAVED_URL)]);
        assert_eq!(errors(&[("dt", "webhook"), ("wh_url", "example.com/hook")], &[&bare]), vec![err("wh_url", BAD_URL)]);
        // An ntfy server URL is the server only: the topic follows it.
        assert_eq!(
            errors(&[("dt", "ntfy"), ("nt_url", "https://ntfy.example.com/?auth=FAKE-Q"), ("nt_topic", "t")], &[]),
            vec![err("nt_url", "Enter the server address only, without ? or #.")]
        );
    }

    /// A page shows a refused post again without its secrets: none of the
    /// secret fields, header values, an ntfy URL's user name, password and
    /// query, or a header name that is not one; "Name: value" in a name
    /// box is a header.
    #[test]
    fn a_form_shown_again_has_no_secrets() {
        let i = input(&[
            ("dt", "webhook"), ("wh_url", "https://example.com/FAKE-PATH"), ("tg_token", TOKEN), ("nt_url", "https://u:FAKE-PW@n.example/b?auth=FAKE-Q"),
            ("wh_hn0", "Authorization: Bearer FAKE-SECRET"), ("wh_hn1", "X-Bad Bearer FAKE-SECRET"), ("wh_hv1", "v"), ("wh_hn3", "X-Ok"), ("wh_hv3", "FAKE-SECRET"),
        ]);
        assert_eq!(i.headers[0], ("Authorization".to_owned(), AUTH.to_owned()));
        assert_eq!(i.rows, 4);
        let shown = i.shown();
        assert_eq!((shown.value("wh_url"), shown.value("tg_token"), shown.value("nt_url")), ("", "", "https://n.example/b"));
        assert_eq!(
            shown.headers,
            [("Authorization", ""), ("", ""), ("X-Ok", "")].map(|(n, v)| (n.to_owned(), v.to_owned()))
        );
    }

    /// The page's view has no secret: a webhook by its host, the header
    /// names, which secrets a rule has; summaries as the app's.
    #[test]
    fn the_public_config_has_no_secrets() {
        let data = json!({"config": {"enabled": true, "poll_interval_secs": 45, "mark_read_after_forward": true, "delete_after_forward": false,
            "rules": [
                {"id": 1, "name": "Bot", "enabled": true, "filter": {"type": "all"},
                 "destination": {"type": "telegram", "bot_token": TOKEN, "chat_id": "-100123", "silent": true}},
                {"id": 2, "name": "Hook", "enabled": false, "filter": {"type": "sender", "patterns": ["BANK", "+1555*"]},
                 "destination": {"type": "webhook", "url": "https://user:pw@Example.com:8443/hook/FAKE-PATH?k=FAKE-Q", "method": "PUT",
                                 "headers": [{"name": "Authorization", "value": AUTH}]}},
                {"id": 3, "name": "N", "enabled": true, "filter": {"type": "content", "keywords": ["code"]},
                 "destination": {"type": "ntfy", "url": "https://ntfy.sh", "topic": "fake", "token": "tk_FAKE"}},
                {"id": 4, "name": "D", "enabled": true, "filter": {"type": "sender_and_content", "patterns": ["A"], "keywords": ["b", "c"]},
                 "destination": {"type": "discord", "webhook_url": "https://discord.com/api/webhooks/1/FAKE-DISCORD"}},
                {"id": 5, "name": "S", "enabled": true, "filter": {"type": "all"}, "destination": {"type": "sms", "forward_number": "+15555550123"}},
                {"id": 6, "name": "L", "enabled": true, "filter": {"type": "all"}, "destination": {"type": "slack", "webhook_url": "https://hooks.slack.com/services/FAKE-SLACK"}},
            ]}, "last_forwarded_id": 9});
        let p = public(&data);
        let text = p.to_string();
        for secret in [TOKEN, "FAKE-SECRET", "FAKE-PATH", "FAKE-Q", "pw@", "tk_FAKE", "FAKE-DISCORD", "FAKE-SLACK"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        assert_eq!((p["enabled"].as_bool(), p["poll_interval_secs"].as_u64(), p["last_forwarded_id"].as_u64()), (Some(true), Some(45), Some(9)));
        let rules = p["rules"].as_array().unwrap();
        assert_eq!(rules[0]["destination"]["kept"], json!(["tg_token"]));
        assert_eq!(rules[1]["destination"], json!({"type": "webhook", "host": "example.com:8443", "method": "PUT", "headers": ["Authorization"], "kept": ["wh_url"]}));
        let summaries: Vec<(String, String)> =
            rules.iter().map(|r| (filter_summary(&r["filter"]), dest_summary(&r["destination"]))).collect();
        assert_eq!(
            summaries,
            [
                ("All messages", "Telegram (chat: -100123)"),
                ("Sender: BANK, +1555*", "Webhook (example.com:8443)"),
                ("Keywords: code", "ntfy (fake)"),
                ("Sender: A + Keywords: b, c", "Discord"),
                ("All messages", "SMS (+15555550123)"),
                ("All messages", "Slack"),
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
        // Edit Rule's values: the shown ones only.
        let i = Input::of_rule(&rules[1]);
        assert_eq!((i.id, i.filter, i.patterns.as_str(), i.dest.key, i.value("wh_method")), (Some(2), "sender", "BANK, +1555*", "webhook", "PUT"));
        assert_eq!((i.value("wh_url"), i.headers.clone()), ("", vec![("Authorization".to_owned(), String::new())]));
        // The others keep the blank form's defaults (the app's draft).
        assert_eq!(i.value("nt_url"), "https://ntfy.sh");
        assert_eq!(Input::of_rule(&rules[0]).value("tg_silent"), "1");
        // Secrets of the stored rules.
        let s = Secrets::of(&data["config"]["rules"][1]["destination"]);
        assert_eq!((s.server.as_str(), s.has("wh_url"), s.has_header("AUTHORIZATION"), s.has("tg_token")), ("https://example.com:8443", true, true, false));
    }

    #[test]
    fn urls() {
        assert_eq!(origin(" HTTPS://User:Pw@Example.COM:8443/x?y#z "), "https://example.com:8443");
        assert_eq!(origin("http://a.example"), "http://a.example");
        for bad in ["ftp://a", "https://", "example.com", "javascript:alert(1)", ""] {
            assert_eq!(origin(bad), "", "{bad}");
        }
        assert_eq!(host("https://discord.com/api/webhooks/1/x"), "discord.com");
        // Saved by the app without a scheme: the host if it looks like one.
        assert_eq!(host("u:FAKE-PW@Example.com:8080/hook?k=FAKE-Q"), "example.com:8080");
        assert_eq!(host("ftp://files.example.com/x"), "files.example.com");
        for bad in ["bot123:FAKE-TOKEN", "FAKE-SECRET", "example.com:x", "", "javascript:alert(1)"] {
            assert_eq!(host(bad), "", "{bad}");
        }
        assert_eq!(dest_summary(&json!({"type": "webhook", "host": ""})), "Webhook");
        assert_eq!(shown_url("https://u:p@h.example/a?b=c#d"), "https://h.example/a");
        assert_eq!(shown_url("h.example?x"), "h.example");
        assert_eq!(without_userinfo("https://u:p@h.example/a@b"), "https://h.example/a@b");
        assert_eq!(without_userinfo("https://h.example/a@b"), "https://h.example/a@b");
    }

    /// The token is claimed for the post as sent, without the tokens and
    /// the question's buttons.
    #[test]
    fn digests() {
        let a = form(&[("csrf", "1"), ("once", "a"), ("name", "x"), ("dt", "sms")]);
        let b = form(&[("csrf", "2"), ("once", "b"), ("name", "x"), ("dt", "sms"), ("confirm", "1")]);
        assert_eq!(digest(true, &a), digest(true, &b));
        assert_ne!(digest(true, &a), digest(false, &a));
        assert_ne!(digest(true, &a), digest(true, &form(&[("name", "y"), ("dt", "sms")])));
    }

    /// The HTTP client's errors that quote the request's URI lose it (it
    /// can hold a secret); the rest stays as the agent wrote it.
    #[test]
    fn errors_lose_quoted_uris() {
        for (e, want) in [
            ("webhook: bad uri: example.com/hook/FAKE-PATH is missing scheme", "webhook: bad uri"),
            ("slack: bad uri: unknown scheme: ftp", "slack: bad uri"),
            ("telegram: configured for https only: http://x/bot123:FAKE/sendMessage", "telegram: configured for https only"),
            ("discord: bad uri: hooks.example.com/FAKE is missing host; held for retry", "discord: bad uri; held for retry"),
            ("webhook: HTTP 404 (permanent error)", "webhook: HTTP 404 (permanent error)"),
            ("no WAN connectivity; held for retry", "no WAN connectivity; held for retry"),
            ("", ""),
        ] {
            assert_eq!(redact(e), want, "{e}");
        }
    }

    /// A Retry finds its entry where it is now: the index drawn if it still
    /// holds it, else where it moved; gone (or retried: a new time), none.
    #[test]
    fn fingerprints_find_their_entry() {
        let e = |ts: i64, sms: u64, rule: u64| json!({"timestamp": ts, "sms_id": sms, "rule_id": rule});
        let log = json!([e(30, 3, 1), e(20, 2, 1), e(20, 2, 2), e(10, 1, 1)]);
        let fp = |index: usize, sms_id: u64, rule_id: u64, timestamp: i64| Fingerprint { index, sms_id, rule_id, timestamp };
        assert_eq!(fp(2, 2, 2, 20).find(&log), Some(2));
        assert_eq!(fp(0, 1, 1, 10).find(&log), Some(3), "moved down: new entries came first");
        assert_eq!(fp(1, 2, 1, 21).find(&log), None, "retried meanwhile");
        assert_eq!(fp(1, 2, 3, 20).find(&log), None, "another rule");
        assert_eq!(fp(9, 4, 1, 40).find(&json!([])), None);
        // A rule id the agent left out is 0 (as its serde default).
        assert_eq!(fp(0, 5, 0, 50).find(&json!([{"timestamp": 50, "sms_id": 5}])), Some(0));
        assert_ne!(fp(1, 2, 1, 20).digest(), fp(2, 2, 1, 20).digest());
        let f = Fingerprint::parse(&form(&[("i", "1"), ("sms", "2"), ("rule", "3"), ("ts", "-4")])).unwrap();
        assert_eq!(f, fp(1, 2, 3, -4));
        for bad in [[("i", "x"), ("sms", "2"), ("rule", "3"), ("ts", "4")], [("i", "1"), ("sms", "-2"), ("rule", "3"), ("ts", "4")], [("i", "1"), ("sms", "2"), ("rule", "3"), ("tz", "4")]] {
            assert_eq!(Fingerprint::parse(&form(&bad)), None, "{bad:?}");
        }
    }

    /// The log as pages see it: each entry's index in the agent's list
    /// (an entry the app's parser drops keeps the others' places), never
    /// the message's text or date, errors without quoted URIs.
    #[test]
    fn the_public_log_has_no_message_text() {
        let app = AppState::new();
        let _fake = fake::set_log(
            &app,
            (200, Value::Null),
            Some(vec![
                json!({"sms_id": 7, "content": "FAKE-CONTENT"}),
                json!({"timestamp": 5, "sms_id": 6, "sender": "+15555550123", "content_preview": "p", "rule_name": "r",
                       "destination_type": "ntfy", "success": false, "pending": true, "rule_id": 2,
                       "error": "ntfy: bad uri: x is missing scheme", "content": "FAKE-CONTENT", "date": "26,01,01,00,00,00,+0"}),
            ]),
        );
        let log = public_log(&app, None).unwrap();
        assert_eq!(
            log,
            json!([{"index": 1, "timestamp": 5, "sms_id": 6, "rule_id": 2, "sender": "+15555550123", "content_preview": "p",
                    "rule_name": "r", "destination_type": "ntfy", "success": false, "pending": true, "error": "ntfy: bad uri"}])
        );
        fake::with(&app, |d| d.log_error = Some("x".into()));
        assert_eq!(public_log(&app, None), Err("x".into()));
    }
}
