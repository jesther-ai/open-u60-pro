use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::system::spawn_named;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::conn_watchdog::wan_connected_from_status;
use crate::handlers::AppState;
use crate::ubus;

const CONFIG_PATH: &str = "/data/local/tmp/sms_forward.json";
const STATE_PATH: &str = "/data/local/tmp/sms_forward_state.json";
const MAX_LOG_ENTRIES: usize = 200;
const HTTP_TIMEOUT_SECS: u64 = 15;
const MAX_RETRIES: u32 = 3;
const RETRY_DELAYS: [u64; 3] = [5, 15, 60];
const INTER_RULE_DELAY_MS: u64 = 500;
const INTER_SMS_DELAY_MS: u64 = 1000;
/// Safety-net poll cadence when an SMS event could have been missed.
const FALLBACK_POLL_SECS: u64 = 900;
/// ... and while forwarding is disabled (the poll is a no-op then).
const FALLBACK_POLL_DISABLED_SECS: u64 = 3600;
/// While forwards are held, the event loop wakes this often to retry them.
const PENDING_RECHECK_SECS: u64 = 15;
/// A WAN that just came up must stay up this long before forwards go out
/// over it, so a forward does not race the attach.
const WAN_SETTLE: Duration = Duration::from_secs(10);
/// While every held forward is blocked, re-read `get_wwaniface` in case a
/// connect event was missed: first after this long, then twice as long each
/// time, up to `WAN_REFRESH_MAX`; it starts over when the WAN comes up.
const WAN_REFRESH_MIN: Duration = Duration::from_secs(60);
const WAN_REFRESH_MAX: Duration = Duration::from_secs(30 * 60);
/// A held forward still blocked after this long (monotonic) is settled as failed.
const PENDING_MAX_AGE: Duration = Duration::from_secs(86_400);

// ── Data types ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct SmsForwardConfig {
    pub enabled: bool,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub mark_read_after_forward: bool,
    #[serde(default)]
    pub delete_after_forward: bool,
    #[serde(default)]
    pub rules: Vec<ForwardRule>,
}

fn default_poll_interval() -> u64 {
    30
}

impl Default for SmsForwardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_secs: 30,
            mark_read_after_forward: false,
            delete_after_forward: false,
            rules: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ForwardRule {
    pub id: u32,
    pub name: String,
    pub enabled: bool,
    pub filter: SmsFilter,
    pub destination: ForwardDestination,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum SmsFilter {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "sender")]
    Sender { patterns: Vec<String> },
    #[serde(rename = "content")]
    Content { keywords: Vec<String> },
    #[serde(rename = "sender_and_content")]
    SenderAndContent {
        patterns: Vec<String>,
        keywords: Vec<String>,
    },
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum ForwardDestination {
    #[serde(rename = "telegram")]
    Telegram {
        bot_token: String,
        chat_id: String,
        #[serde(default)]
        silent: bool,
    },
    #[serde(rename = "webhook")]
    Webhook {
        url: String,
        #[serde(default = "default_method")]
        method: String,
        #[serde(default)]
        headers: Vec<HttpHeader>,
    },
    #[serde(rename = "sms")]
    Sms { forward_number: String },
    #[serde(rename = "ntfy")]
    Ntfy {
        url: String,
        topic: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    #[serde(rename = "discord")]
    Discord { webhook_url: String },
    #[serde(rename = "slack")]
    Slack { webhook_url: String },
}

fn default_method() -> String {
    "POST".into()
}

#[derive(Serialize, Deserialize, Clone)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ForwardState {
    pub last_forwarded_id: u64,
    #[serde(default)]
    pub log: Vec<ForwardLogEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ForwardLogEntry {
    pub timestamp: i64,
    pub sms_id: u64,
    pub sender: String,
    pub content_preview: String,
    pub rule_name: String,
    pub destination_type: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub rule_id: u32,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub date: String,
    /// Held while the link was down; `flush_pending` retries it.
    #[serde(default)]
    pub pending: bool,
    /// When a held forward was held, on the monotonic clock (seeded from
    /// `timestamp` at load), so a wall-clock jump cannot expire the hold.
    #[serde(skip)]
    held_at: Option<Instant>,
}

// ── SMS message from ubus ───────────────────────────────────────────

struct DecodedSms {
    id: u64,
    sender: String,
    content: String,
    date: String,
}

// ── UCS-2 hex decoding ──────────────────────────────────────────────

pub(crate) fn decode_ucs2_hex(hex: &str) -> String {
    let hex = hex.trim();
    if hex.is_empty() {
        return String::new();
    }
    let mut chars = Vec::new();
    let mut i = 0;
    let bytes = hex.as_bytes();
    while i + 4 <= bytes.len() {
        if let Ok(code) = u16::from_str_radix(&hex[i..i + 4], 16) {
            if let Some(ch) = char::from_u32(code as u32) {
                chars.push(ch);
            }
        }
        i += 4;
    }
    chars.into_iter().collect()
}

/// Check if a string looks like UCS-2 hex (all hex chars, length multiple of 4).
fn is_ucs2_hex(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && s.len() % 4 == 0 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse ZTE timestamp `YY,MM,DD,HH,MM,SS,+TZ` (or `;` separators) into human-readable string.
fn humanize_zte_date(raw: &str) -> String {
    let parts: Vec<&str> = raw.split(|c| c == ',' || c == ';').collect();
    if parts.len() < 6 {
        return raw.to_string();
    }
    let ok = || -> Option<String> {
        let yy: u32 = parts[0].trim().parse().ok()?;
        let mm: u32 = parts[1].trim().parse().ok()?;
        let dd: u32 = parts[2].trim().parse().ok()?;
        let hh: u32 = parts[3].trim().parse().ok()?;
        let min: u32 = parts[4].trim().parse().ok()?;
        let month = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ]
        .get((mm as usize).wrapping_sub(1))?;
        let (h12, ampm) = match hh {
            0 => (12, "AM"),
            1..=11 => (hh, "AM"),
            12 => (12, "PM"),
            _ => (hh - 12, "PM"),
        };
        let tz = parts.get(6).map(|s| s.trim()).unwrap_or("");
        let tz_str = if tz.is_empty() {
            String::new()
        } else {
            format!(" UTC{tz}")
        };
        Some(format!(
            "{month} {dd}, 20{yy:02} {h12}:{min:02} {ampm}{tz_str}"
        ))
    }();
    ok.unwrap_or_else(|| raw.to_string())
}

/// Encode text as UTF-16BE hex (UCS-2), matching the apps' `encodeUCS2Hex`.
fn encode_ucs2_hex(text: &str) -> String {
    text.encode_utf16()
        .map(|c| format!("{c:04X}"))
        .collect()
}

/// Generate ZTE-format SMS timestamp "YY;MM;DD;HH;MM;SS;+TZ".
fn format_sms_time() -> String {
    let mut t: i64 = 0;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::time(&mut t);
        libc::localtime_r(&t, &mut tm);
    }
    let tz_offset = tm.tm_gmtoff / 3600;
    let tz_sign = if tz_offset >= 0 { "+" } else { "" };
    format!(
        "{:02};{:02};{:02};{:02};{:02};{:02};{}{}",
        tm.tm_year % 100,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        tz_sign,
        tz_offset,
    )
}

// ── Filter matching ─────────────────────────────────────────────────

fn normalize_phone(num: &str) -> String {
    let num = num.trim();
    if let Some(rest) = num.strip_prefix("+63") {
        return format!("0{rest}");
    }
    num.to_string()
}

fn is_forward_loop(dest: &ForwardDestination, sender: &str) -> bool {
    match dest {
        ForwardDestination::Sms { forward_number } => {
            normalize_phone(sender) == normalize_phone(forward_number)
        }
        _ => false,
    }
}

/// Detect garbled echo content (firmware artifact: mostly `@` / NUL chars).
fn is_garbled_echo(content: &str) -> bool {
    if content.is_empty() {
        return true;
    }
    let junk = content.chars().filter(|&c| c == '@' || c == '\0').count();
    junk * 2 > content.len() // >50% junk chars
}

fn matches_filter(filter: &SmsFilter, sender: &str, content: &str) -> bool {
    match filter {
        SmsFilter::All => true,
        SmsFilter::Sender { patterns } => sender_matches(sender, patterns),
        SmsFilter::Content { keywords } => content_matches(content, keywords),
        SmsFilter::SenderAndContent { patterns, keywords } => {
            sender_matches(sender, patterns) && content_matches(content, keywords)
        }
    }
}

fn sender_matches(sender: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let sender_lower = sender.to_lowercase();
    patterns.iter().any(|p| {
        let p_lower = p.to_lowercase();
        if p_lower.ends_with('*') {
            sender_lower.starts_with(&p_lower[..p_lower.len() - 1])
        } else if p_lower.starts_with('*') {
            sender_lower.ends_with(&p_lower[1..])
        } else {
            sender_lower == p_lower
        }
    })
}

fn content_matches(content: &str, keywords: &[String]) -> bool {
    if keywords.is_empty() {
        return true;
    }
    let content_lower = content.to_lowercase();
    keywords
        .iter()
        .any(|kw| content_lower.contains(&kw.to_lowercase()))
}

// ── Destination formatting + dispatch ───────────────────────────────

fn destination_type_name(dest: &ForwardDestination) -> &'static str {
    match dest {
        ForwardDestination::Telegram { .. } => "telegram",
        ForwardDestination::Webhook { .. } => "webhook",
        ForwardDestination::Sms { .. } => "sms",
        ForwardDestination::Ntfy { .. } => "ntfy",
        ForwardDestination::Discord { .. } => "discord",
        ForwardDestination::Slack { .. } => "slack",
    }
}

fn format_message(sms: &DecodedSms) -> String {
    format!(
        "SMS from {}\n{}\n\n{}",
        sms.sender,
        humanize_zte_date(&sms.date),
        sms.content
    )
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(HTTP_TIMEOUT_SECS)))
        .build()
        .into()
}

fn forward_to(
    dest: &ForwardDestination,
    sms: &DecodedSms,
    agent: &ureq::Agent,
) -> Result<(), String> {
    match dest {
        ForwardDestination::Telegram {
            bot_token,
            chat_id,
            silent,
        } => {
            let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
            let body = json!({
                "chat_id": chat_id,
                "text": format_message(sms),
                "disable_notification": silent,
            });
            let resp = agent
                .post(&url)
                .header("Content-Type", "application/json")
                .send(body.to_string().as_bytes())
                .map_err(|e| format!("telegram: {e}"))?;
            check_http_status(resp.status().into(), "telegram")
        }
        ForwardDestination::Webhook {
            url,
            method,
            headers,
        } => {
            let body = json!({
                "event": "sms_received",
                "sms": {
                    "id": sms.id,
                    "sender": sms.sender,
                    "content": sms.content,
                    "date": sms.date,
                },
                "timestamp": now_ts(),
            });
            let body_bytes = body.to_string();
            let mut req = match method.to_uppercase().as_str() {
                "PUT" => agent.put(url),
                _ => agent.post(url),
            };
            req = req.header("Content-Type", "application/json");
            for h in headers {
                req = req.header(&h.name, &h.value);
            }
            let resp = req
                .send(body_bytes.as_bytes())
                .map_err(|e| format!("webhook: {e}"))?;
            check_http_status(resp.status().into(), "webhook")
        }
        ForwardDestination::Sms { forward_number } => {
            let text = format_message(sms);
            let encode_type = "UNICODE";
            let message_body = encode_ucs2_hex(&text);
            let params = json!({
                "number": forward_number,
                "message_body": message_body,
                "encode_type": encode_type,
                "sms_time": format_sms_time(),
                "id": "-1",
            });
            let resp = ubus::call(
                "zwrt_wms",
                "zte_libwms_send_sms",
                Some(&params.to_string()),
            )
            .map_err(|e| format!("sms forward: {e}"))?;
            check_sms_send_result(&resp)?;

            // Auto-delete the forwarded outgoing SMS to prevent conversation clutter
            cleanup_forwarded_sms(forward_number);

            Ok(())
        }
        ForwardDestination::Ntfy { url, topic, token } => {
            let full_url = format!("{}/{}", url.trim_end_matches('/'), topic);
            let mut req = agent.post(&full_url);
            req = req.header("Title", &format!("SMS from {}", sms.sender));
            if let Some(t) = token {
                req = req.header("Authorization", &format!("Bearer {t}"));
            }
            let resp = req
                .send(sms.content.as_bytes())
                .map_err(|e| format!("ntfy: {e}"))?;
            check_http_status(resp.status().into(), "ntfy")
        }
        ForwardDestination::Discord { webhook_url } => {
            let text = format_message(sms);
            // Discord has 2000 char limit
            let text = if text.len() > 2000 {
                format!("{}...", &text[..text.floor_char_boundary(1997)])
            } else {
                text
            };
            let body = json!({ "content": text });
            let resp = agent
                .post(webhook_url)
                .header("Content-Type", "application/json")
                .send(body.to_string().as_bytes())
                .map_err(|e| format!("discord: {e}"))?;
            check_http_status(resp.status().into(), "discord")
        }
        ForwardDestination::Slack { webhook_url } => {
            let body = json!({ "text": format_message(sms) });
            let resp = agent
                .post(webhook_url)
                .header("Content-Type", "application/json")
                .send(body.to_string().as_bytes())
                .map_err(|e| format!("slack: {e}"))?;
            check_http_status(resp.status().into(), "slack")
        }
    }
}

fn check_http_status(status: u16, dest: &str) -> Result<(), String> {
    if (200..300).contains(&status) {
        Ok(())
    } else if status == 429 {
        Err(format!("{dest}: HTTP 429 rate limited"))
    } else if (400..500).contains(&status) {
        Err(format!("{dest}: HTTP {status} (permanent error)"))
    } else {
        Err(format!("{dest}: HTTP {status}"))
    }
}

/// Check ZTE SMS send response. result=3 means success, anything else is failure.
fn check_sms_send_result(resp: &Value) -> Result<(), String> {
    let result = resp
        .get("result")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
    match result {
        Some(3) | None => Ok(()),
        Some(code) => Err(format!("sms forward: device rejected (result={code})")),
    }
}

/// Best-effort cleanup of forwarded outgoing SMS.
/// The firmware stores sent SMS on the device; we delete it so it doesn't appear
/// in the conversation list. Only appears in the forward log.
fn cleanup_forwarded_sms(forward_number: &str) {
    // Small delay to let firmware store the outgoing SMS
    std::thread::sleep(Duration::from_millis(200));

    // Fetch latest sent messages (tag=2)
    let messages = fetch_sms_both_stores(2, 0, 5, "order by id desc");

    // Find the most recent sent SMS to the forward number
    let normalized_dest = normalize_phone(forward_number);
    for item in &messages {
        let number = item["number"].as_str().unwrap_or("");
        let decoded_number = if is_ucs2_hex(number) {
            decode_ucs2_hex(number)
        } else {
            number.to_string()
        };

        if normalize_phone(&decoded_number) == normalized_dest {
            let id = item["id"].as_u64()
                .or_else(|| item["id"].as_str().and_then(|s| s.parse().ok()))
                .unwrap_or(0);
            if id > 0 {
                match ubus::call(
                    "zwrt_wms",
                    "zwrt_wms_delete_sms",
                    Some(&json!({"id": id.to_string()}).to_string()),
                ) {
                    Ok(_) => eprintln!("[sms_forward] cleanup: deleted forwarded outgoing SMS id={id}"),
                    Err(e) => eprintln!("[sms_forward] cleanup: failed to delete SMS id={id}: {e}"),
                }
            }
            break; // Only delete the most recent match
        }
    }
}

fn is_transient_error(err: &str) -> bool {
    // HTTP 4xx are permanent — everything else is transient
    !err.contains("(permanent error)")
}

// ── Held forwards ───────────────────────────────────────────────────

/// Why a forward to `dest` cannot go out right now, or `None` when the link
/// it needs is up. The text never says "(permanent error)": the forward is
/// held and retried, not dropped.
fn blocker(wan_ready: bool, has_service: bool, dest: &ForwardDestination) -> Option<&'static str> {
    match dest {
        ForwardDestination::Sms { .. } => {
            (!has_service).then_some("no cellular service; held for retry")
        }
        _ => (!wan_ready).then_some("no WAN connectivity; held for retry"),
    }
}

/// A forward that failed transiently while its link is down (it was running
/// when the link dropped) is held rather than settled as failed.
fn hold_after_failure(result: &Result<(), String>, blocked: bool) -> bool {
    blocked && matches!(result, Err(e) if is_transient_error(e))
}

/// Log text for a forward held after a transient failure.
fn held_error(err: &str) -> String {
    format!("{err}; held for retry")
}

/// The destination of a held entry's rule, or `Err(reason)` to settle the
/// entry as failed because its rule can no longer forward it.
fn pending_action(
    entry: &ForwardLogEntry,
    config: &SmsForwardConfig,
) -> Result<ForwardDestination, &'static str> {
    if !config.enabled {
        return Err("forwarding disabled while held");
    }
    let rule = config
        .rules
        .iter()
        .find(|r| r.id == entry.rule_id)
        .ok_or("rule no longer exists")?;
    if !rule.enabled {
        return Err("rule disabled while held");
    }
    Ok(rule.destination.clone())
}

/// The monotonic instant a forward held since wall-clock `timestamp` was
/// held, for entries loaded from disk.
fn hold_start(timestamp: i64, now: i64) -> Instant {
    let max = PENDING_MAX_AGE.as_secs() as i64 + 1;
    let age = Duration::from_secs(now.saturating_sub(timestamp).clamp(0, max) as u64);
    let mono = Instant::now();
    mono.checked_sub(age).unwrap_or(mono)
}

/// A held forward has been held longer than `PENDING_MAX_AGE`, timed on the
/// monotonic clock: the firmware moves the wall clock by hours on network
/// attach, just before the link a hold waits for comes back.
fn hold_expired(entry: &ForwardLogEntry) -> bool {
    let held_at = entry
        .held_at
        .unwrap_or_else(|| hold_start(entry.timestamp, now_ts()));
    held_at.elapsed() > PENDING_MAX_AGE
}

/// What `flush_pending` does with a held entry right now.
enum Flush {
    /// Its link is up: try it again.
    Send(ForwardDestination),
    /// Its link is still down: keep holding it.
    Wait(ForwardDestination),
    /// Settle it as failed, for this reason.
    Settle(&'static str),
}

/// The link is checked before the age, so a forward whose link is back goes
/// out rather than expiring. `blocked` says whether a destination's link is
/// down (`SmsForwarder::link_blocker`).
fn flush_action(
    entry: &ForwardLogEntry,
    config: &SmsForwardConfig,
    blocked: impl Fn(&ForwardDestination) -> bool,
) -> Flush {
    match pending_action(entry, config) {
        Err(reason) => Flush::Settle(reason),
        Ok(dest) if !blocked(&dest) => Flush::Send(dest),
        Ok(_) if hold_expired(entry) => Flush::Settle("held for over 24 h without a link"),
        Ok(dest) => Flush::Wait(dest),
    }
}

/// What the held forwards are waiting for; sets how long the event loop sleeps.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Held {
    Nothing,
    /// One can go out or be settled now, or the WAN is up and settling.
    Retry,
    /// All are blocked, with nothing for a WAN that is up; `needs_wan` when
    /// one waits on the WAN (and not only on cellular service).
    Blocked {
        needs_wan: bool,
    },
}

fn held_state(
    log: &[ForwardLogEntry],
    config: &SmsForwardConfig,
    wan_up: bool,
    blocked: impl Fn(&ForwardDestination) -> bool,
) -> Held {
    let mut any = false;
    let mut needs_wan = false;
    for entry in log.iter().filter(|e| e.pending) {
        any = true;
        match flush_action(entry, config, &blocked) {
            Flush::Wait(dest) => needs_wan |= !matches!(dest, ForwardDestination::Sms { .. }),
            Flush::Send(_) | Flush::Settle(_) => return Held::Retry,
        }
    }
    match (any, needs_wan && wan_up) {
        (false, _) => Held::Nothing,
        (true, true) => Held::Retry,
        (true, false) => Held::Blocked { needs_wan },
    }
}

/// Backoff for the `get_wwaniface` re-read while every held forward is
/// blocked (the event loop sleeps until the next one is due).
struct WanRefresh {
    last: Instant,
    interval: Duration,
}

impl WanRefresh {
    fn new() -> Self {
        WanRefresh {
            last: Instant::now(),
            interval: WAN_REFRESH_MIN,
        }
    }

    /// Time until the next re-read is due.
    fn remaining(&self) -> Duration {
        self.interval.saturating_sub(self.last.elapsed())
    }

    /// Whether a re-read is due; if so it counts as done, and the next one
    /// waits twice as long (up to `WAN_REFRESH_MAX`).
    fn due(&mut self) -> bool {
        if !self.remaining().is_zero() {
            return false;
        }
        self.last = Instant::now();
        self.interval = (self.interval * 2).min(WAN_REFRESH_MAX);
        true
    }
}

/// Write a held forward's outcome back to its log entry, found by
/// (sms_id, rule_id, pending) rather than by index: `process_new_messages`
/// may have pushed and drained the log while the forward ran. An entry that
/// stays held keeps its timestamp and `held_at`.
fn update_pending(
    log: &mut [ForwardLogEntry],
    (sms_id, rule_id): (u64, u32),
    error: Option<String>,
    pending: bool,
    now: i64,
) -> bool {
    let Some(entry) = log
        .iter_mut()
        .find(|e| e.pending && e.sms_id == sms_id && e.rule_id == rule_id)
    else {
        return false;
    };
    entry.success = error.is_none();
    entry.error = error;
    entry.pending = pending;
    if !pending {
        entry.timestamp = now;
    }
    true
}

/// `update_pending`, then whether every forward of that SMS has now gone
/// out, so its post-forward actions (mark read, delete) are due.
fn settle_pending(
    log: &mut [ForwardLogEntry],
    (sms_id, rule_id): (u64, u32),
    error: Option<String>,
    pending: bool,
    now: i64,
) -> bool {
    let sent = error.is_none() && !pending;
    update_pending(log, (sms_id, rule_id), error, pending, now)
        && sent
        && log.iter().filter(|e| e.sms_id == sms_id).all(|e| e.success)
}

/// SMS ids with a held forward.
fn held_ids(log: &[ForwardLogEntry]) -> std::collections::HashSet<u64> {
    log.iter().filter(|e| e.pending).map(|e| e.sms_id).collect()
}

/// Entries that trimming and clearing keep: a held entry is the only copy of
/// its forward, and a failed sibling of a held entry is what stops the
/// post-forward actions (delete, mark read) once the held one goes out.
fn keep_entry(e: &ForwardLogEntry, held: &std::collections::HashSet<u64>) -> bool {
    e.pending || (!e.success && held.contains(&e.sms_id))
}

/// Push a log entry and trim the log to `MAX_LOG_ENTRIES`, oldest settled
/// entries first (see `keep_entry`).
fn push_log(log: &mut Vec<ForwardLogEntry>, entry: ForwardLogEntry) {
    log.push(entry);
    let mut excess = log.len().saturating_sub(MAX_LOG_ENTRIES);
    if excess == 0 {
        return;
    }
    let held = held_ids(log);
    log.retain(|e| {
        let drop = excess > 0 && !keep_entry(e, &held);
        if drop {
            excess -= 1;
        }
        !drop
    });
    // Only held entries left over the cap
    for e in log.drain(..excess) {
        eprintln!(
            "[sms_forward] held SMS {} -> {}: dropped (log full)",
            e.sms_id, e.destination_type
        );
    }
}

/// The log entry a manual retry targets (`index` counts newest-first), or
/// the error response. A held entry is refused: `flush_pending` retries it,
/// and a manual send alongside would deliver the SMS twice.
fn retry_target(log: &[ForwardLogEntry], index: usize) -> Result<ForwardLogEntry, (u16, Value)> {
    let entry = log
        .len()
        .checked_sub(1 + index)
        .and_then(|i| log.get(i))
        .ok_or((404, json!({"ok": false, "error": "log entry not found"})))?;
    if entry.success {
        return Err((
            400,
            json!({"ok": false, "error": "entry already succeeded"}),
        ));
    }
    if entry.pending {
        return Err((
            409,
            json!({"ok": false, "error": "held; will retry automatically"}),
        ));
    }
    Ok(entry.clone())
}

/// Write a manual retry's outcome back to its entry, found by (sms_id,
/// rule_id, timestamp) rather than by index: `process_new_messages` may have
/// pushed and drained the log while the forward ran. Nothing is written if
/// the entry is gone or was settled meanwhile.
fn update_retried(
    log: &mut [ForwardLogEntry],
    target: &ForwardLogEntry,
    result: &Result<(), String>,
    now: i64,
) -> bool {
    let Some(entry) = log.iter_mut().find(|e| {
        !e.pending
            && !e.success
            && e.sms_id == target.sms_id
            && e.rule_id == target.rule_id
            && e.timestamp == target.timestamp
    }) else {
        return false;
    };
    match result {
        Ok(()) => {
            entry.success = true;
            entry.error = None;
        }
        Err(e) => entry.error = Some(e.clone()),
    }
    entry.timestamp = now;
    true
}

/// Mark read / delete an SMS once every rule forwarded it.
fn post_forward(config: &SmsForwardConfig, sms_id: u64) {
    if config.mark_read_after_forward {
        if let Err(e) = ubus::call(
            "zwrt_wms",
            "zwrt_wms_modify_tag",
            Some(&json!({"id": sms_id.to_string(), "tag": 0}).to_string()),
        ) {
            eprintln!("[sms_forward] mark-read failed for SMS {sms_id}: {e}");
        }
    }
    if config.delete_after_forward {
        if let Err(e) = ubus::call(
            "zwrt_wms",
            "zwrt_wms_delete_sms",
            Some(&json!({"id": sms_id.to_string()}).to_string()),
        ) {
            eprintln!("[sms_forward] delete failed for SMS {sms_id}: {e}");
        }
    }
}

/// Safety-net poll interval: 15 min while forwarding is active, 1 h while it
/// is disabled (the poll is a no-op then).
fn fallback_interval(active: bool) -> Duration {
    Duration::from_secs(if active {
        FALLBACK_POLL_SECS
    } else {
        FALLBACK_POLL_DISABLED_SECS
    })
}

/// How long the event loop may sleep: the pending recheck while a held
/// forward can go out (or the WAN is settling); while all are blocked, until
/// the next backed-off WAN re-read (a returning link wakes the loop sooner),
/// at most the poll interval; otherwise the safety-net poll interval.
/// Nothing held, no extra wakeups.
fn loop_timeout(held: Held, blocked_wait: Duration, fallback: Duration) -> Duration {
    match held {
        Held::Nothing => fallback,
        Held::Retry => Duration::from_secs(PENDING_RECHECK_SECS),
        Held::Blocked { .. } => blocked_wait.min(fallback),
    }
}

// ── Time helper ─────────────────────────────────────────────────────

fn now_ts() -> i64 {
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}

// ── Core forwarder ──────────────────────────────────────────────────

pub struct SmsForwarder {
    revision: AtomicU64,
    config: Mutex<SmsForwardConfig>,
    state: Mutex<ForwardState>,
    has_service: AtomicBool,
    wan_connected: AtomicBool,
    /// When the WAN last came up (monotonic); `None` while it is down.
    wan_up_at: Mutex<Option<Instant>>,
    /// Backoff for the WAN re-read while every held forward is blocked;
    /// starts over when the WAN comes up.
    wan_refresh: Mutex<WanRefresh>,
    /// Sends on `event_loop`'s channel, so a returning link wakes the loop
    /// out of a backed-off sleep. `None` once the SMS event channel closed.
    wake: Mutex<Option<mpsc::Sender<Value>>>,
}

impl SmsForwarder {
    pub fn new() -> Self {
        let config = fs::read_to_string(CONFIG_PATH)
            .ok()
            .and_then(|s| serde_json::from_str::<SmsForwardConfig>(&s).ok())
            .unwrap_or_default();

        let mut state = fs::read_to_string(STATE_PATH)
            .ok()
            .and_then(|s| serde_json::from_str::<ForwardState>(&s).ok())
            .unwrap_or_default();
        let now = now_ts();
        for entry in state.log.iter_mut().filter(|e| e.pending) {
            entry.held_at = Some(hold_start(entry.timestamp, now));
        }

        SmsForwarder {
            revision: AtomicU64::new(0),
            config: Mutex::new(config),
            state: Mutex::new(state),
            has_service: AtomicBool::new(false),
            wan_connected: AtomicBool::new(false),
            wan_up_at: Mutex::new(None),
            wan_refresh: Mutex::new(WanRefresh::new()),
            wake: Mutex::new(None),
        }
    }

    /// Start the SMS forwarder with event-driven reception.
    /// Falls back to polling if the event channel disconnects.
    ///
    /// Everything runs on its own threads so `main()` reaches the HTTP server
    /// immediately; the connectivity seed below can block for up to 10 s at boot.
    pub fn start(self: &Arc<Self>, sms_events: mpsc::Receiver<Value>, service_rx: mpsc::Receiver<Value>, wan_rx: mpsc::Receiver<Value>) {
        let this = Arc::clone(self);
        spawn_named("sms-start", move || {
            this.seed_connectivity();

            // The event loop reads SMS events through its own channel, which
            // the monitors also send on to wake it (`wake_loop`)
            let (tx, events) = mpsc::channel();
            *this.wake.lock().unwrap() = Some(tx.clone());
            let relay = Arc::clone(&this);
            spawn_named("sms-relay", move || relay.relay_events(sms_events, tx));

            let conn_self = Arc::clone(&this);
            spawn_named("sms-svc", move || conn_self.service_monitor(service_rx));

            let conn_self = Arc::clone(&this);
            spawn_named("sms-wan", move || conn_self.wan_monitor(wan_rx));

            let forwarder = Arc::clone(&this);
            spawn_named("sms-events", move || forwarder.event_loop(events));
        });
    }

    /// Feeds firmware SMS events to `event_loop`. When their channel closes,
    /// the loop's closes too (no sender left), so it falls back to polling.
    fn relay_events(&self, sms_events: mpsc::Receiver<Value>, tx: mpsc::Sender<Value>) {
        for event in sms_events.iter() {
            if tx.send(event).is_err() {
                break;
            }
        }
        self.wake.lock().unwrap().take();
    }

    /// Wake `event_loop` so it recomputes its timeout: a link came back
    /// while held forwards may be sleeping out a backed-off interval.
    fn wake_loop(&self) {
        if !self.has_pending() {
            return;
        }
        if let Some(tx) = &*self.wake.lock().unwrap() {
            let _ = tx.send(json!({ "wms_status": "link_up" }));
        }
    }

    /// Seed initial connectivity state (retry if services aren't registered yet).
    fn seed_connectivity(&self) {
        for attempt in 1..=5 {
            let got_service = if !self.has_service.load(Ordering::Relaxed) {
                if let Ok(data) = ubus::call("zte_nwinfo_api", "nwinfo_get_netinfo", Some("{}")) {
                    let network_type = data["network_type"].as_str().unwrap_or("");
                    let has = !network_type.is_empty() && network_type != "NO_SERVICE" && !network_type.starts_with("LIMITED_SERVICE");
                    self.has_service.store(has, Ordering::Relaxed);
                    eprintln!("[sms_forward] initial service state: {network_type} (has_service={has})");
                    has
                } else {
                    false
                }
            } else {
                true
            };

            let got_wan = if !self.wan_connected.load(Ordering::Relaxed) {
                match self.refresh_wan() {
                    Some(connected) => {
                        eprintln!("[sms_forward] initial WAN state: connected={connected}");
                        connected
                    }
                    None => false,
                }
            } else {
                true
            };

            if got_service && got_wan {
                break;
            }
            if attempt < 5 {
                eprintln!("[sms_forward] connectivity seed attempt {attempt}/5 incomplete (service={got_service}, wan={got_wan}), retrying in 2s");
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }

    /// Blocks on the service-status event channel; no timer, zero wakeups at idle.
    fn service_monitor(&self, service_rx: mpsc::Receiver<Value>) {
        for event in service_rx.iter() {
            let network_type = event["network_type"].as_str().unwrap_or("");
            let has = !network_type.is_empty() && network_type != "NO_SERVICE" && !network_type.starts_with("LIMITED_SERVICE");
            let prev = self.has_service.swap(has, Ordering::Relaxed);
            if prev != has {
                eprintln!("[sms_forward] service state changed: {network_type} (has_service={has})");
                if has {
                    self.wake_loop();
                }
            }
        }
        eprintln!("[sms_forward] service_rx disconnected");
    }

    /// Blocks on the WAN connect-status event channel; no timer, zero wakeups at idle.
    fn wan_monitor(&self, wan_rx: mpsc::Receiver<Value>) {
        for event in wan_rx.iter() {
            let wan_status = event["wan_status"].as_str().unwrap_or("");
            let connected = wan_connected_from_status(wan_status);
            if self.set_wan(connected) {
                eprintln!("[sms_forward] WAN state changed: {wan_status} (connected={connected})");
                if connected {
                    self.wake_loop();
                }
            }
        }
        eprintln!("[sms_forward] wan_rx disconnected");
    }

    /// Record the WAN state; `true` when it changed. A change stamps
    /// `wan_up_at` (set on up, cleared on down) for the settle window; coming
    /// up also restarts the WAN re-read backoff.
    fn set_wan(&self, connected: bool) -> bool {
        let mut up_at = self.wan_up_at.lock().unwrap();
        let changed = self.wan_connected.swap(connected, Ordering::Relaxed) != connected;
        if changed {
            *up_at = connected.then(Instant::now);
        }
        drop(up_at);
        if changed && connected {
            *self.wan_refresh.lock().unwrap() = WanRefresh::new();
        }
        changed
    }

    /// One `get_wwaniface` read applied through `set_wan`: the boot seed, and
    /// the backstop for a missed connect event while forwards are held.
    /// `None` when the read fails.
    fn refresh_wan(&self) -> Option<bool> {
        let data = ubus::call(
            "zwrt_data",
            "get_wwaniface",
            Some(r#"{"source_module":"zte_topsw_data","cid":1}"#),
        )
        .ok()?;
        let connected = data["connect_status"]
            .as_str()
            .is_some_and(wan_connected_from_status);
        self.set_wan(connected);
        Some(connected)
    }

    /// The WAN is up and has stayed up for `WAN_SETTLE`.
    fn wan_ready(&self) -> bool {
        let up_at = self.wan_up_at.lock().unwrap();
        self.wan_connected.load(Ordering::Relaxed)
            && up_at.is_some_and(|t| t.elapsed() >= WAN_SETTLE)
    }

    /// Why a forward to `dest` must be held right now, or `None`.
    fn link_blocker(&self, dest: &ForwardDestination) -> Option<&'static str> {
        let has_service = self.has_service.load(Ordering::Relaxed);
        blocker(self.wan_ready(), has_service, dest)
    }

    fn has_pending(&self) -> bool {
        self.state.lock().unwrap().log.iter().any(|e| e.pending)
    }

    /// What the held forwards are waiting for.
    fn held(&self) -> Held {
        let config = self.config.lock().unwrap();
        let state = self.state.lock().unwrap();
        let wan_up = self.wan_connected.load(Ordering::Relaxed);
        held_state(&state.log, &config, wan_up, |dest| {
            self.link_blocker(dest).is_some()
        })
    }

    /// How long the event loop may sleep before a safety-net poll: 15 min while
    /// forwarding is active, 1 h while it is disabled (the poll is a no-op then).
    fn fallback_poll_interval(&self) -> Duration {
        let config = self.config.lock().unwrap();
        fallback_interval(config.enabled && config.rules.iter().any(|r| r.enabled))
    }

    fn init_watermark(&self) {
        let last_id = self.state.lock().unwrap().last_forwarded_id;
        if last_id == 0 {
            match fetch_max_sms_id() {
                Ok(max_id) if max_id > 0 => {
                    let mut state = self.state.lock().unwrap();
                    state.last_forwarded_id = max_id;
                    save_state(&state);
                    eprintln!("[sms_forward] watermark initialized to {max_id}");
                }
                Ok(_) => eprintln!("[sms_forward] no SMS on device, watermark stays at 0"),
                Err(e) => eprintln!("[sms_forward] watermark init failed: {e}"),
            }
        }
    }

    fn event_loop(&self, rx: mpsc::Receiver<Value>) {
        self.init_watermark();

        let mut last_poll = Instant::now();
        loop {
            // Wait for an event; while forwards are held, wake every
            // PENDING_RECHECK_SECS, or with all of them blocked, for the next
            // backed-off WAN re-read; otherwise only for the safety-net poll
            let fallback = self.fallback_poll_interval();
            let blocked_wait = self.wan_refresh.lock().unwrap().remaining();
            match rx.recv_timeout(loop_timeout(self.held(), blocked_wait, fallback)) {
                Ok(event) => {
                    // Anything else, such as a `wake_loop` nudge, only
                    // recomputes the timeout
                    if event["wms_status"].as_str() != Some("new_sms_received") {
                        continue;
                    }
                    let config = self.config.lock().unwrap().clone();
                    if !config.enabled || config.rules.iter().all(|r| !r.enabled) {
                        continue;
                    }
                    // Small delay to let modem store the SMS
                    std::thread::sleep(Duration::from_millis(500));
                    let last_id = self.state.lock().unwrap().last_forwarded_id;
                    eprintln!("[sms_forward] SMS event received, processing after id {last_id}");
                    self.process_new_messages(last_id);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.recheck_pending();
                    // Fallback poll for any missed events
                    if last_poll.elapsed() >= fallback {
                        last_poll = Instant::now();
                        self.poll_once();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    eprintln!("[sms_forward] event channel disconnected, falling back to polling");
                    self.poll_loop();
                    return;
                }
            }
        }
    }

    /// Single poll cycle — used as fallback on timeout.
    fn poll_once(&self) {
        let config = self.config.lock().unwrap().clone();
        if !config.enabled || config.rules.iter().all(|r| !r.enabled) {
            return;
        }

        self.init_watermark();

        let last_id = self.state.lock().unwrap().last_forwarded_id;
        match fetch_sms_capacity() {
            Ok(cap) if cap > 0 => {
                self.process_new_messages(last_id);
            }
            Ok(_) => {}
            Err(e) => eprintln!("[sms_forward] poll: failed to fetch SMS capacity: {e}"),
        }
    }

    /// While forwards are held: if all are blocked and one waits on the WAN,
    /// re-read the WAN state when the backed-off re-read is due; then retry
    /// or settle them.
    fn recheck_pending(&self) {
        match self.held() {
            Held::Nothing => return,
            Held::Blocked { needs_wan } => {
                let due = self.wan_refresh.lock().unwrap().due();
                if due && needs_wan && self.refresh_wan() == Some(true) {
                    eprintln!("[sms_forward] WAN found connected on re-read (event missed)");
                }
            }
            Held::Retry => {}
        }
        self.flush_pending();
    }

    /// Pure polling fallback if event bus dies entirely.
    fn poll_loop(&self) {
        loop {
            self.recheck_pending();
            self.poll_once();
            let interval = self.config.lock().unwrap().poll_interval_secs.max(10);
            std::thread::sleep(Duration::from_secs(interval));
        }
    }

    fn process_new_messages(&self, after_id: u64) {
        let messages = match fetch_new_messages(after_id) {
            Ok(msgs) => msgs,
            Err(e) => {
                eprintln!("[sms_forward] failed to fetch messages after id {after_id}: {e}");
                return;
            }
        };

        if messages.is_empty() {
            return;
        }

        eprintln!(
            "[sms_forward] found {} new message(s) after id {after_id}",
            messages.len()
        );

        let config = self.config.lock().unwrap().clone();
        let enabled_rules: Vec<&ForwardRule> =
            config.rules.iter().filter(|r| r.enabled).collect();

        if enabled_rules.is_empty() {
            return;
        }

        let agent = http_agent();
        let mut max_processed_id = after_id;

        for sms in &messages {
            if sms.id <= after_id {
                continue;
            }

            let mut all_succeeded = true;

            for (i, rule) in enabled_rules.iter().enumerate() {
                // Skip firmware echo: sender matches destination AND content is garbled
                if is_forward_loop(&rule.destination, &sms.sender) && is_garbled_echo(&sms.content) {
                    continue;
                }

                if !matches_filter(&rule.filter, &sms.sender, &sms.content) {
                    continue;
                }

                // Pre-forward connectivity check: hold the forward while its
                // link is down; flush_pending retries it once the link is back
                if let Some(reason) = self.link_blocker(&rule.destination) {
                    eprintln!(
                        "[sms_forward] skipping forward for SMS {} -> {}: {reason}",
                        sms.id,
                        destination_type_name(&rule.destination)
                    );
                    let entry = ForwardLogEntry {
                        timestamp: now_ts(),
                        sms_id: sms.id,
                        sender: sms.sender.clone(),
                        content_preview: preview(&sms.content, 80),
                        rule_name: rule.name.clone(),
                        destination_type: destination_type_name(&rule.destination).to_string(),
                        success: false,
                        error: Some(reason.to_string()),
                        rule_id: rule.id,
                        content: sms.content.clone(),
                        date: sms.date.clone(),
                        pending: true,
                        held_at: Some(Instant::now()),
                    };
                    push_log(&mut self.state.lock().unwrap().log, entry);
                    all_succeeded = false;
                    if i + 1 < enabled_rules.len() {
                        std::thread::sleep(Duration::from_millis(INTER_RULE_DELAY_MS));
                    }
                    continue;
                }

                let result = forward_with_retry(&rule.destination, sms, &agent);
                // A forward that was running when the link dropped is held too
                let blocked = self.link_blocker(&rule.destination).is_some();
                let pending = hold_after_failure(&result, blocked);

                if result.is_err() {
                    all_succeeded = false;
                }

                let entry = ForwardLogEntry {
                    timestamp: now_ts(),
                    sms_id: sms.id,
                    sender: sms.sender.clone(),
                    content_preview: preview(&sms.content, 80),
                    rule_name: rule.name.clone(),
                    destination_type: destination_type_name(&rule.destination).to_string(),
                    success: result.is_ok(),
                    error: result
                        .err()
                        .map(|e| if pending { held_error(&e) } else { e }),
                    rule_id: rule.id,
                    content: sms.content.clone(),
                    date: sms.date.clone(),
                    pending,
                    held_at: pending.then(Instant::now),
                };

                push_log(&mut self.state.lock().unwrap().log, entry);

                // Delay between rules for the same SMS
                if i + 1 < enabled_rules.len() {
                    std::thread::sleep(Duration::from_millis(INTER_RULE_DELAY_MS));
                }
            }

            if sms.id > max_processed_id {
                max_processed_id = sms.id;
            }

            // Post-forward actions — only when ALL rules succeeded (for a
            // held forward, flush_pending runs them once it goes out)
            if all_succeeded {
                post_forward(&config, sms.id);
            }

            // Delay between different SMS messages
            std::thread::sleep(Duration::from_millis(INTER_SMS_DELAY_MS));
        }

        // Update watermark
        let mut state = self.state.lock().unwrap();
        state.last_forwarded_id = max_processed_id;
        save_state(&state);
    }

    /// Retry the held forwards whose link is back and settle the ones that can
    /// no longer go out. Runs only on the sms-events thread (or the polling
    /// fallback that replaces it), so two flushes never overlap.
    fn flush_pending(&self) {
        let held: Vec<ForwardLogEntry> = self
            .state
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|e| e.pending)
            .cloned()
            .collect();
        if held.is_empty() {
            return;
        }

        let config = self.config.lock().unwrap().clone();
        let agent = http_agent();
        let blocked = |dest: &ForwardDestination| self.link_blocker(dest).is_some();

        for entry in held {
            let id = entry.sms_id;
            let (error, pending) = match flush_action(&entry, &config, blocked) {
                Flush::Wait(_) => continue,
                Flush::Settle(reason) => {
                    let kind = &entry.destination_type;
                    eprintln!("[sms_forward] held SMS {id} -> {kind}: dropped ({reason})");
                    (Some(reason.to_string()), false)
                }
                Flush::Send(dest) => {
                    // Rebuild the SMS from the held copy in the log entry
                    let sms = DecodedSms {
                        id,
                        sender: entry.sender.clone(),
                        content: entry.content.clone(),
                        date: entry.date.clone(),
                    };
                    let result = forward_with_retry(&dest, &sms, &agent);
                    let pending = hold_after_failure(&result, self.link_blocker(&dest).is_some());
                    let kind = destination_type_name(&dest);
                    std::thread::sleep(Duration::from_millis(INTER_RULE_DELAY_MS));
                    match result {
                        Ok(()) => {
                            eprintln!("[sms_forward] held SMS {id} -> {kind}: sent");
                            (None, false)
                        }
                        Err(e) if pending => {
                            eprintln!("[sms_forward] held SMS {id} -> {kind}: still held: {e}");
                            (Some(held_error(&e)), true)
                        }
                        Err(e) => {
                            eprintln!("[sms_forward] held SMS {id} -> {kind}: failed: {e}");
                            (Some(e), false)
                        }
                    }
                }
            };

            let key = (id, entry.rule_id);
            let mut state = self.state.lock().unwrap();
            let done = settle_pending(&mut state.log, key, error, pending, now_ts());
            save_state(&state);
            drop(state);
            if done {
                post_forward(&config, id);
            }
        }
    }
}

fn preview(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..s.floor_char_boundary(max)])
    }
}

fn forward_with_retry(
    dest: &ForwardDestination,
    sms: &DecodedSms,
    agent: &ureq::Agent,
) -> Result<(), String> {
    let mut last_err = String::new();

    for attempt in 0..MAX_RETRIES {
        match forward_to(dest, sms, agent) {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!(
                    "[sms_forward] attempt {}/{MAX_RETRIES} failed for SMS {} -> {}: {e}",
                    attempt + 1,
                    sms.id,
                    destination_type_name(dest),
                );
                if !is_transient_error(&e) {
                    return Err(e);
                }
                last_err = e;
                if attempt + 1 < MAX_RETRIES {
                    std::thread::sleep(Duration::from_secs(RETRY_DELAYS[attempt as usize]));
                }
            }
        }
    }

    Err(format!("{last_err} (failed after {MAX_RETRIES} retries)"))
}

// ── ubus helpers ────────────────────────────────────────────────────

/// Query SMS from both NV and SIM storage, merged and deduplicated by ID.
/// ZTE firmware bug: mem_store=2 ("all") silently omits SIM messages.
fn fetch_sms_both_stores(tags: u64, page: u64, count: u64, order: &str) -> Vec<Value> {
    let mut all: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();

    for store in [1u64, 0] {
        // NV (1) first — more common, then SIM (0)
        let params = json!({
            "tags": tags,
            "page": page,
            "data_per_page": count,
            "mem_store": store,
            "order_by": order,
        });
        if let Ok(data) = ubus::call("zwrt_wms", "zte_libwms_get_sms_data", Some(&params.to_string())) {
            if let Some(arr) = data["messages"].as_array() {
                for item in arr {
                    let id = item["id"]
                        .as_u64()
                        .or_else(|| item["id"].as_str().and_then(|s| s.parse().ok()))
                        .unwrap_or(0);
                    if seen.insert(id) {
                        all.push(item.clone());
                    }
                }
            }
        }
    }
    all
}

/// Returns the unread SMS count.
fn fetch_sms_capacity() -> Result<u64, String> {
    let data = ubus::call("zwrt_wms", "zwrt_wms_get_wms_capacity", Some("{}"))?;
    Ok(data["sms_dev_unread_num"]
        .as_u64()
        .or_else(|| {
            data["sms_dev_unread_num"]
                .as_str()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(0))
}

/// Fetch SMS list and return messages with id > after_id, sorted ascending.
fn fetch_new_messages(after_id: u64) -> Result<Vec<DecodedSms>, String> {
    let sms_list = fetch_sms_both_stores(1, 0, 50, "order by id desc");

    let mut messages: Vec<DecodedSms> = Vec::new();

    for item in &sms_list {
        let id = item["id"]
            .as_u64()
            .or_else(|| item["id"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);

        if id <= after_id {
            continue;
        }

        // tag: 0=unread, 1=read, 2=sent, 3=draft — only forward received (0/1)
        let tag_num = item["tag"]
            .as_u64()
            .or_else(|| item["tag"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);
        if tag_num >= 2 {
            continue;
        }

        let sender_raw = item["number"].as_str().unwrap_or("");
        let content_raw = item["content"].as_str().unwrap_or("");
        let date = item["date"]
            .as_str()
            .or_else(|| item["received_time"].as_str())
            .or_else(|| item["sms_time"].as_str())
            .unwrap_or("")
            .to_string();

        let sender = if is_ucs2_hex(sender_raw) {
            decode_ucs2_hex(sender_raw)
        } else {
            sender_raw.to_string()
        };

        let content = if is_ucs2_hex(content_raw) {
            decode_ucs2_hex(content_raw)
        } else {
            content_raw.to_string()
        };

        messages.push(DecodedSms {
            id,
            sender,
            content,
            date,
        });
    }

    messages.sort_by_key(|m| m.id);
    Ok(messages)
}

/// Get the maximum SMS id currently on the device.
fn fetch_max_sms_id() -> Result<u64, String> {
    let all = fetch_sms_both_stores(10, 0, 5, "order by id desc");

    let max_id = all
        .iter()
        .filter_map(|item| {
            item["id"]
                .as_u64()
                .or_else(|| item["id"].as_str().and_then(|s| s.parse().ok()))
        })
        .max()
        .unwrap_or(0);

    Ok(max_id)
}

// ── Persistence ─────────────────────────────────────────────────────

fn save_config(config: &SmsForwardConfig) {
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = fs::write(CONFIG_PATH, json);
    }
}

fn save_state(state: &ForwardState) {
    if let Ok(json) = serde_json::to_string(state) {
        let _ = fs::write(STATE_PATH, json);
    }
}

// ── HTTP handlers ───────────────────────────────────────────────────

/// GET /api/sms/forward/config
pub fn config_get(state: &AppState) -> (u16, Value) {
    let config = state.sms_forward.config.lock().unwrap();
    let fwd_state = state.sms_forward.state.lock().unwrap();
    (
        200,
        json!({
            "ok": true,
            "data": {
                "config": *config,
                "revision": state.sms_forward.revision.load(Ordering::Relaxed),
                "last_forwarded_id": fwd_state.last_forwarded_id,
            }
        }),
    )
}

/// PUT /api/sms/forward/config
pub fn config_set(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(default)]
        poll_interval_secs: Option<u64>,
        #[serde(default)]
        mark_read_after_forward: Option<bool>,
        #[serde(default)]
        delete_after_forward: Option<bool>,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    let mut config = state.sms_forward.config.lock().unwrap();

    if let Some(enabled) = req.enabled {
        config.enabled = enabled;
    }
    if let Some(interval) = req.poll_interval_secs {
        if interval < 10 {
            return (400, json!({"ok": false, "error": "poll_interval_secs must be >= 10"}));
        }
        config.poll_interval_secs = interval;
    }
    if let Some(v) = req.mark_read_after_forward {
        config.mark_read_after_forward = v;
    }
    if let Some(v) = req.delete_after_forward {
        config.delete_after_forward = v;
    }

    save_config(&config);
    state.sms_forward.revision.fetch_add(1, Ordering::Relaxed);
    (200, json!({"ok": true, "data": *config}))
}

/// POST /api/sms/forward/rules — Create a new rule
pub fn rules_create(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        name: String,
        #[serde(default = "default_true")]
        enabled: bool,
        filter: SmsFilter,
        destination: ForwardDestination,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    let mut config = state.sms_forward.config.lock().unwrap();
    let Some(next_id) = next_rule_id(&config.rules, &state.sms_forward.state.lock().unwrap().log) else {
        return (409, json!({"ok":false,"error":"forwarding rule IDs exhausted"}));
    };

    let rule = ForwardRule {
        id: next_id,
        name: req.name,
        enabled: req.enabled,
        filter: req.filter,
        destination: req.destination,
    };

    let result = json!({"ok": true, "data": rule});
    config.rules.push(rule);
    save_config(&config);
    state.sms_forward.revision.fetch_add(1, Ordering::Relaxed);
    (201, result)
}

fn next_rule_id(rules: &[ForwardRule], log: &[ForwardLogEntry]) -> Option<u32> {
    rules
        .iter()
        .map(|rule| rule.id)
        .chain(log.iter().map(|entry| entry.rule_id))
        .max()
        .unwrap_or(0)
        .checked_add(1)
}


fn default_true() -> bool {
    true
}

/// PUT /api/sms/forward/rules — Update a rule
pub fn rules_update(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        id: u32,
        name: String,
        enabled: bool,
        filter: SmsFilter,
        destination: ForwardDestination,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    let mut config = state.sms_forward.config.lock().unwrap();
    let rule = match config.rules.iter_mut().find(|r| r.id == req.id) {
        Some(r) => r,
        None => return (404, json!({"ok": false, "error": "rule not found"})),
    };

    rule.name = req.name;
    rule.enabled = req.enabled;
    rule.filter = req.filter;
    rule.destination = req.destination;

    let result = json!({"ok": true, "data": rule.clone()});
    save_config(&config);
    state.sms_forward.revision.fetch_add(1, Ordering::Relaxed);
    (200, result)
}

/// DELETE /api/sms/forward/rules — Delete a rule
pub fn rules_delete(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = match parsed["id"].as_u64() {
        Some(id) => id as u32,
        None => return (400, json!({"ok": false, "error": "missing 'id' field"})),
    };

    let mut config = state.sms_forward.config.lock().unwrap();
    let len_before = config.rules.len();
    config.rules.retain(|r| r.id != id);

    if config.rules.len() == len_before {
        return (404, json!({"ok": false, "error": "rule not found"}));
    }

    save_config(&config);
    state.sms_forward.revision.fetch_add(1, Ordering::Relaxed);
    (200, json!({"ok": true}))
}

/// PUT /api/sms/forward/rules/toggle — Enable/disable a rule
pub fn rules_toggle(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = match parsed["id"].as_u64() {
        Some(id) => id as u32,
        None => return (400, json!({"ok": false, "error": "missing 'id' field"})),
    };
    let enabled = match parsed["enabled"].as_bool() {
        Some(e) => e,
        None => return (400, json!({"ok": false, "error": "missing 'enabled' field"})),
    };

    let mut config = state.sms_forward.config.lock().unwrap();
    let rule = match config.rules.iter_mut().find(|r| r.id == id) {
        Some(r) => r,
        None => return (404, json!({"ok": false, "error": "rule not found"})),
    };

    rule.enabled = enabled;
    let result = json!({"ok": true, "data": rule.clone()});
    save_config(&config);
    state.sms_forward.revision.fetch_add(1, Ordering::Relaxed);
    (200, result)
}

/// POST /api/sms/forward/test — Send a test message
pub fn test_forward(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        destination: ForwardDestination,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    let _ = state; // keep signature consistent

    let test_sms = DecodedSms {
        id: 0,
        sender: "+1234567890".to_string(),
        content: "This is a test message from zte-agent SMS forwarder.".to_string(),
        date: "2025-01-01 12:00:00".to_string(),
    };

    let agent = http_agent();
    match forward_to(&req.destination, &test_sms, &agent) {
        Ok(()) => (200, json!({"ok": true, "data": {"status": "sent"}})),
        Err(e) => (502, json!({"ok": false, "error": e})),
    }
}

/// GET /api/sms/forward/log — returns newest-first
pub fn log_get(state: &AppState) -> (u16, Value) {
    let fwd_state = state.sms_forward.state.lock().unwrap();
    let mut log = fwd_state.log.clone();
    log.reverse();
    (200, json!({"ok": true, "data": log}))
}

/// POST /api/sms/forward/log/clear
pub fn log_clear(state: &AppState) -> (u16, Value) {
    let mut fwd_state = state.sms_forward.state.lock().unwrap();
    clear_log(&mut fwd_state.log);
    save_state(&fwd_state);
    (200, json!({"ok": true}))
}

/// Clear the log but keep held forwards: each entry is the only copy of its
/// forward, which `flush_pending` still sends or settles.
fn clear_log(log: &mut Vec<ForwardLogEntry>) {
    let held = held_ids(log);
    log.retain(|e| keep_entry(e, &held));
}


#[derive(Deserialize)]
struct RetryIdentity {
    sms_id: u64,
    rule_id: u32,
    timestamp: i64,
}

fn checked_retry_target(
    log: &[ForwardLogEntry],
    index: usize,
    expected: Option<&RetryIdentity>,
) -> Result<ForwardLogEntry, (u16, Value)> {
    let entry = retry_target(log, index)?;
    if expected.is_some_and(|id| {
        id.sms_id != entry.sms_id || id.rule_id != entry.rule_id || id.timestamp != entry.timestamp
    }) {
        return Err((
            409,
            json!({"ok":false,"error":"The forwarding log changed. Review the current entry before retrying."}),
        ));
    }
    Ok(entry)
}

/// Only non-secret configuration metadata and log previews enter the optional web feed.
pub(crate) fn dashboard_status(state: &AppState) -> Value {
    let config = state
        .sms_forward
        .config
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let forward_state = state
        .sms_forward
        .state
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    dashboard_snapshot(
        &config,
        &forward_state.log,
        state.sms_forward.revision.load(Ordering::Relaxed),
    )
}

fn dashboard_snapshot(config: &SmsForwardConfig, log: &[ForwardLogEntry], revision: u64) -> Value {
    let rules: Vec<_> = config.rules.iter().map(|rule| json!({"id":rule.id,"name":rule.name,"enabled":rule.enabled,"destination_type":destination_type_name(&rule.destination)})).collect();
    let entries: Vec<_> = log.iter().rev().enumerate().map(|(index, entry)| json!({
        "index":index,"timestamp":entry.timestamp,"sms_id":entry.sms_id,"rule_id":entry.rule_id,
        "sender":entry.sender,"content_preview":entry.content_preview,"rule_name":entry.rule_name,
        "destination_type":entry.destination_type,"success":entry.success,"pending":entry.pending,
        "error":entry.error.as_ref().map(|_| "Destination did not accept the forward. Check its settings or send a test."),
    })).collect();
    json!({"ok":true,"data":{"revision":revision,"config":{
        "enabled":config.enabled,"poll_interval_secs":config.poll_interval_secs,
        "mark_read_after_forward":config.mark_read_after_forward,"delete_after_forward":config.delete_after_forward,
    },"rules":rules,"log":entries}})
}

/// POST /api/sms/forward/retry — Retry a failed forward log entry
pub fn retry_forward(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        index: usize,
        #[serde(default)]
        expected: Option<RetryIdentity>,
        #[serde(default)]
        expected_rule_revision: Option<u64>,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    let entry_clone = match checked_retry_target(&state.sms_forward.state.lock().unwrap().log, req.index, req.expected.as_ref()) {
        Ok(entry) => entry,
        Err(response) => return response,
    };

    // Look up rule by id from current config
    let dest = {
        let config = state.sms_forward.config.lock().unwrap();
        if req.expected_rule_revision.is_some_and(|revision| revision != state.sms_forward.revision.load(Ordering::Relaxed)) {
            return (409, json!({"ok":false,"error":"Forwarding rules changed. Review the destination before retrying."}));
        }
        match config.rules.iter().find(|r| r.id == entry_clone.rule_id) {
            Some(rule) => rule.destination.clone(),
            None => return (404, json!({"ok": false, "error": "rule no longer exists"})),
        }
    };

    // Reconstruct the SMS from stored log data
    let sms = DecodedSms {
        id: entry_clone.sms_id,
        sender: entry_clone.sender.clone(),
        content: entry_clone.content.clone(),
        date: entry_clone.date.clone(),
    };

    // Single attempt — no auto-retry for manual retries
    let agent = http_agent();
    let result = forward_to(&dest, &sms, &agent);

    // Update the log entry
    let mut fwd_state = state.sms_forward.state.lock().unwrap();
    update_retried(&mut fwd_state.log, &entry_clone, &result, now_ts());
    save_state(&fwd_state);

    match result {
        Ok(()) => (200, json!({"ok": true, "data": {"status": "sent"}})),
        Err(e) => (502, json!({"ok": false, "error": e})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forwarder() -> SmsForwarder {
        SmsForwarder {
            revision: AtomicU64::new(0),
            config: Mutex::new(SmsForwardConfig::default()),
            state: Mutex::new(ForwardState::default()),
            has_service: AtomicBool::new(false),
            wan_connected: AtomicBool::new(false),
            wan_up_at: Mutex::new(None),
            wan_refresh: Mutex::new(WanRefresh::new()),
            wake: Mutex::new(None),
        }
    }

    fn http_destinations() -> Vec<ForwardDestination> {
        vec![
            ForwardDestination::Telegram {
                bot_token: "token".into(),
                chat_id: "chat".into(),
                silent: false,
            },
            ForwardDestination::Webhook {
                url: "https://example.invalid/hook".into(),
                method: default_method(),
                headers: Vec::new(),
            },
            ForwardDestination::Ntfy {
                url: "https://example.invalid".into(),
                topic: "topic".into(),
                token: None,
            },
            ForwardDestination::Discord {
                webhook_url: "https://example.invalid/discord".into(),
            },
            ForwardDestination::Slack {
                webhook_url: "https://example.invalid/slack".into(),
            },
        ]
    }

    fn sms_destination() -> ForwardDestination {
        ForwardDestination::Sms {
            forward_number: "12345".into(),
        }
    }

    fn rule(id: u32, enabled: bool) -> ForwardRule {
        ForwardRule {
            id,
            name: format!("rule {id}"),
            enabled,
            filter: SmsFilter::All,
            destination: ForwardDestination::Slack {
                webhook_url: "https://example.invalid/slack".into(),
            },
        }
    }

    fn entry(sms_id: u64, rule_id: u32, pending: bool, timestamp: i64) -> ForwardLogEntry {
        ForwardLogEntry {
            timestamp,
            sms_id,
            sender: "sender".into(),
            content_preview: "body".into(),
            rule_name: format!("rule {rule_id}"),
            destination_type: "slack".into(),
            success: false,
            error: Some("no WAN connectivity; held for retry".into()),
            rule_id,
            content: "body".into(),
            date: String::new(),
            pending,
            held_at: pending.then(Instant::now),
        }
    }

    fn sms_rule(id: u32) -> ForwardRule {
        ForwardRule {
            destination: sms_destination(),
            ..rule(id, true)
        }
    }

    fn ago(secs: u64) -> Option<Instant> {
        Instant::now().checked_sub(Duration::from_secs(secs))
    }

    /// WAN flag after feeding one `router_event_wan_connect_status` payload
    /// through the real monitor.
    fn wan_after(status: &str) -> bool {
        let f = forwarder();
        let (tx, rx) = mpsc::channel();
        tx.send(json!({ "wan_status": status })).unwrap();
        drop(tx);
        f.wan_monitor(rx);
        f.wan_connected.load(Ordering::Relaxed)
    }

    #[test]
    fn dashboard_snapshot_omits_destination_credentials_raw_errors_and_full_messages() {
        let mut rule = rule(1, true);
        rule.destination = ForwardDestination::Telegram {
            bot_token: "private-bot-token".into(),
            chat_id: "private-chat".into(),
            silent: false,
        };
        let config = SmsForwardConfig {
            rules: vec![rule],
            ..Default::default()
        };
        let mut log = entry(1, 1, false, 100);
        log.content = "private-full-message".into();
        log.error = Some("https://api.telegram.org/bot/private-bot-token".into());
        let text = dashboard_snapshot(&config, &[log], 9).to_string();
        for private in [
            "private-bot-token",
            "private-chat",
            "private-full-message",
            "https://",
        ] {
            assert!(!text.contains(private), "leaked {private}");
        }
        assert!(text.contains("content_preview") && text.contains("revision"));
    }

    #[test]
    fn retry_identity_rejects_an_index_shift_before_any_forward() {
        let log = vec![entry(1, 1, false, 100), entry(2, 1, false, 200)];
        let expected = RetryIdentity {
            sms_id: 1,
            rule_id: 1,
            timestamp: 100,
        };
        assert_eq!(
            checked_retry_target(&log, 0, Some(&expected))
                .err()
                .unwrap()
                .0,
            409
        );
        assert_eq!(
            checked_retry_target(&log, 1, Some(&expected))
                .unwrap()
                .sms_id,
            1
        );
    }

    #[test]
    fn new_rule_ids_do_not_reuse_destinations_referenced_by_activity() {
        assert_eq!(
            next_rule_id(&[rule(1, true)], &[entry(1, 10, true, 100)]),
            Some(11)
        );
    }

    #[test]
    fn wan_status_parse() {
        let up = [
            "ipv6_connected",
            "ipv4_connected",
            "ipv4_ipv6_connected",
            "connected",
        ];
        for up in up {
            assert!(wan_after(up), "{up} should count as WAN up");
        }
        for down in ["disconnected", "ipv4_disconnected", "connecting"] {
            assert!(!wan_after(down), "{down} should count as WAN down");
        }
    }

    #[test]
    fn blocker_matrix() {
        // Args: (wan_ready, has_service, dest)
        for dest in http_destinations() {
            assert_eq!(blocker(true, true, &dest), None);
            assert_eq!(blocker(true, false, &dest), None, "service is not needed");
            assert!(blocker(false, true, &dest).is_some());
        }
        let sms = sms_destination();
        assert_eq!(blocker(true, true, &sms), None);
        assert_eq!(blocker(false, true, &sms), None, "WAN is not needed");
        assert!(blocker(true, false, &sms).is_some());

        let http = &http_destinations()[0];
        let texts = [blocker(false, false, &sms), blocker(false, false, http)];
        for text in texts.into_iter().map(Option::unwrap) {
            assert!(!text.contains("(permanent error)"), "{text}");
            assert!(is_transient_error(text));
        }
    }

    #[test]
    fn wan_settle() {
        let f = forwarder();
        let dest = &http_destinations()[0];
        assert!(f.set_wan(true));
        assert!(!f.set_wan(true), "no change, no re-stamp");
        assert!(!f.wan_ready(), "just came up");

        *f.wan_up_at.lock().unwrap() = Instant::now().checked_sub(Duration::from_secs(5));
        assert!(!f.wan_ready());
        assert!(f.link_blocker(dest).is_some());

        *f.wan_up_at.lock().unwrap() = Instant::now().checked_sub(Duration::from_secs(11));
        assert!(f.wan_ready());
        assert_eq!(f.link_blocker(dest), None);

        assert!(f.set_wan(false));
        assert!(f.wan_up_at.lock().unwrap().is_none());
        assert!(!f.wan_ready());
    }

    #[test]
    fn pending_after_failure() {
        let transient: Result<(), String> =
            Err("telegram: io: timed out (failed after 3 retries)".into());
        let permanent: Result<(), String> = Err("telegram: HTTP 400 (permanent error)".into());
        assert!(hold_after_failure(&transient, true));
        assert!(!hold_after_failure(&permanent, true));
        assert!(!hold_after_failure(&transient, false));
        assert!(!hold_after_failure(&Ok(()), true));
    }

    #[test]
    fn pending_action() {
        let now = 1_800_000_000;
        let config = SmsForwardConfig {
            enabled: true,
            rules: vec![rule(1, true), rule(2, false)],
            ..SmsForwardConfig::default()
        };

        let action = super::pending_action;

        let held = entry(7, 1, true, now - 60);
        let dest = action(&held, &config).unwrap();
        assert_eq!(destination_type_name(&dest), "slack");

        let missing = entry(7, 3, true, now);
        assert!(action(&missing, &config).is_err(), "missing rule");
        let disabled = entry(7, 2, true, now);
        assert!(action(&disabled, &config).is_err(), "disabled rule");
        let off = SmsForwardConfig {
            enabled: false,
            ..config.clone()
        };
        assert!(action(&held, &off).is_err(), "forwarding off");
    }

    /// The 24 h limit runs on the monotonic clock and only for a forward
    /// whose link is still down.
    #[test]
    fn hold_expiry() {
        let now = now_ts();
        let config = SmsForwardConfig {
            enabled: true,
            rules: vec![rule(1, true)],
            ..SmsForwardConfig::default()
        };
        let day = PENDING_MAX_AGE.as_secs();
        let action = |entry: &ForwardLogEntry, blocked: bool| match flush_action(
            entry,
            &config,
            |_| blocked,
        ) {
            Flush::Send(_) => "send",
            Flush::Wait(_) => "wait",
            Flush::Settle(_) => "settle",
        };

        // Held 17 h, then attach moves the wall clock +8 h while still blocked
        let mut jumped = entry(7, 1, true, now - 25 * 3600);
        jumped.held_at = ago(17 * 3600);
        assert_eq!(action(&jumped, true), "wait");
        assert_eq!(action(&jumped, false), "send");

        let mut expired = entry(7, 1, true, now);
        expired.held_at = ago(day + 1);
        assert_eq!(action(&expired, true), "settle");
        assert_eq!(action(&expired, false), "send", "its link is back");

        // Loaded from disk: seeded from the wall clock once, at load
        let mut loaded = entry(7, 1, true, now - day as i64 - 60);
        loaded.held_at = Some(hold_start(loaded.timestamp, now));
        assert_eq!(action(&loaded, true), "settle");
        loaded.timestamp = now + 8 * 3600;
        assert_eq!(
            action(&loaded, true),
            "settle",
            "a later clock jump is ignored"
        );
        let mut fresh = entry(7, 1, true, now - 60);
        fresh.held_at = Some(hold_start(fresh.timestamp, now));
        assert_eq!(action(&fresh, true), "wait");
        fresh.held_at = Some(hold_start(now + 3600, now));
        assert_eq!(action(&fresh, true), "wait", "future timestamp");
    }

    #[test]
    fn flush_update_by_key() {
        let mut log = vec![
            entry(1, 1, false, 10),
            entry(2, 2, false, 20),
            entry(2, 1, true, 30),
            entry(2, 2, true, 40),
            entry(3, 2, true, 50),
        ];
        // A new message drains the oldest entry while the flush forwards
        log.drain(..1);
        let json = |e: &ForwardLogEntry| serde_json::to_value(e).unwrap();
        let before: Vec<Value> = log.iter().map(json).collect();

        assert!(update_pending(&mut log, (2, 2), None, false, 99));
        assert!(log[2].success && !log[2].pending && log[2].error.is_none());
        assert_eq!(log[2].timestamp, 99);
        for i in [0, 1, 3] {
            assert_eq!(json(&log[i]), before[i], "entry {i} changed");
        }

        // Still held: the error is updated, the hold timestamp kept
        let error = Some("x; held for retry".to_string());
        assert!(update_pending(&mut log, (3, 2), error, true, 99));
        assert!(log[3].pending && !log[3].success);
        assert_eq!(log[3].timestamp, 50);

        // Nothing pending under this key any more
        assert!(!update_pending(&mut log, (2, 2), None, false, 99));
    }

    #[test]
    fn state_backcompat() {
        let old = r#"{"last_forwarded_id":7,"log":[{"timestamp":1,"sms_id":7,
            "sender":"s","content_preview":"p","rule_name":"r",
            "destination_type":"telegram","success":false,
            "error":"no WAN connectivity (permanent error)","rule_id":1}]}"#;
        let state: ForwardState = serde_json::from_str(old).unwrap();
        assert!(!state.log[0].pending);
        let saved = serde_json::to_value(&state.log[0]).unwrap();
        assert_eq!(saved["pending"], false);
    }

    #[test]
    fn loop_timeout() {
        let secs = Duration::from_secs;
        let timeout =
            |held, active| super::loop_timeout(held, secs(240), fallback_interval(active));
        let blocked = Held::Blocked { needs_wan: true };
        assert_eq!(timeout(Held::Retry, true), secs(15));
        assert_eq!(timeout(Held::Retry, false), secs(15));
        assert_eq!(timeout(blocked, true), secs(240));
        assert_eq!(timeout(Held::Nothing, true), secs(900));
        assert_eq!(timeout(Held::Nothing, false), secs(3600));
        let long = super::loop_timeout(blocked, WAN_REFRESH_MAX, fallback_interval(true));
        assert_eq!(long, secs(900), "no later than the safety-net poll");
    }

    /// Held entries keep the loop on the 15 s recheck only while one of them
    /// can make progress; with every one blocked, it backs off.
    #[test]
    fn held_states() {
        let now = now_ts();
        let config = SmsForwardConfig {
            enabled: true,
            rules: vec![rule(1, true), sms_rule(2), rule(3, false)],
            ..SmsForwardConfig::default()
        };
        let http = entry(1, 1, true, now);
        let sms = entry(1, 2, true, now);
        let disabled = entry(1, 3, true, now);
        let mut expired = entry(2, 1, true, now);
        expired.held_at = ago(PENDING_MAX_AGE.as_secs() + 1);
        let state = |log: &[ForwardLogEntry], wan_up, blocked: bool| {
            held_state(log, &config, wan_up, |_| blocked)
        };

        assert_eq!(state(&[], false, true), Held::Nothing);
        assert_eq!(
            state(&[entry(1, 1, false, now)], false, true),
            Held::Nothing
        );

        // Data off: WAN flag down, everything blocked
        let both = [http.clone(), sms];
        assert_eq!(state(&both, false, true), Held::Blocked { needs_wan: true });
        // Only cellular service is missing: no WAN re-read
        let sms_only = &both[1..];
        assert_eq!(
            state(sms_only, false, true),
            Held::Blocked { needs_wan: false }
        );
        assert_eq!(
            state(sms_only, true, true),
            Held::Blocked { needs_wan: false }
        );
        // The WAN is up and settling
        assert_eq!(state(&both, true, true), Held::Retry);
        // Something can go out or be settled now
        assert_eq!(state(&both, false, false), Held::Retry);
        assert_eq!(state(&[http.clone(), disabled], false, true), Held::Retry);
        assert_eq!(state(&[http, expired], false, true), Held::Retry);
    }

    #[test]
    fn wan_refresh_backoff() {
        let f = forwarder();
        let mut refresh = WanRefresh::new();
        assert!(!refresh.due(), "first re-read after a minute");
        let mut waits = Vec::new();
        for _ in 0..7 {
            refresh.last = refresh.last.checked_sub(refresh.interval).unwrap();
            assert!(refresh.remaining().is_zero());
            assert!(refresh.due());
            assert!(!refresh.due(), "counted as done");
            waits.push(refresh.remaining().as_secs().div_ceil(60));
        }
        assert_eq!(
            waits,
            [2, 4, 8, 16, 30, 30, 30],
            "minutes until the next re-read"
        );

        // The WAN coming up starts it over
        f.wan_refresh.lock().unwrap().interval = WAN_REFRESH_MAX;
        f.set_wan(false);
        assert_eq!(f.wan_refresh.lock().unwrap().interval, WAN_REFRESH_MAX);
        assert!(f.set_wan(true));
        assert_eq!(f.wan_refresh.lock().unwrap().interval, WAN_REFRESH_MIN);
    }

    /// A WAN (or service) that comes back wakes the event loop at once, but
    /// only while something is held.
    #[test]
    fn link_up_wakes_loop() {
        let f = forwarder();
        let (tx, rx) = mpsc::channel();
        *f.wake.lock().unwrap() = Some(tx);
        let feed = |status: &str| {
            let (wan_tx, wan_rx) = mpsc::channel();
            wan_tx.send(json!({ "wan_status": status })).unwrap();
            drop(wan_tx);
            f.wan_monitor(wan_rx);
        };

        feed("connected");
        assert!(rx.try_recv().is_err(), "nothing held, no wakeup");
        feed("disconnected");

        f.state
            .lock()
            .unwrap()
            .log
            .push(entry(1, 1, true, now_ts()));
        feed("disconnected");
        assert!(rx.try_recv().is_err(), "no change");
        feed("connected");
        let wake = rx.try_recv().unwrap();
        assert_ne!(
            wake["wms_status"], "new_sms_received",
            "the loop only recomputes"
        );
        feed("disconnected");
        assert!(rx.try_recv().is_err(), "down does not wake");

        let (svc_tx, svc_rx) = mpsc::channel();
        svc_tx.send(json!({ "network_type": "LTE" })).unwrap();
        drop(svc_tx);
        f.service_monitor(svc_rx);
        assert!(rx.try_recv().is_ok(), "service back");
    }

    /// The loop's channel still closes when the SMS event channel does, so
    /// the loop falls back to polling.
    #[test]
    fn relay_closes_loop_channel() {
        let f = forwarder();
        let (tx, rx) = mpsc::channel();
        *f.wake.lock().unwrap() = Some(tx.clone());
        let (sms_tx, sms_rx) = mpsc::channel();
        sms_tx
            .send(json!({ "wms_status": "new_sms_received" }))
            .unwrap();
        drop(sms_tx);
        f.relay_events(sms_rx, tx);
        assert_eq!(rx.recv().unwrap()["wms_status"], "new_sms_received");
        assert!(rx.recv().is_err(), "disconnected");
        assert!(f.wake.lock().unwrap().is_none());
    }

    /// Manual retry leaves held entries to flush_pending, and writes its
    /// result to the entry it sent even after the log shifted.
    #[test]
    fn retry_by_key() {
        let mut log: Vec<ForwardLogEntry> = (0..MAX_LOG_ENTRIES as u64)
            .map(|id| entry(id, 1, false, 100 + id as i64))
            .collect();
        let held = log.len() - 1;
        log[held].pending = true;

        let refused = retry_target(&log, 0).err().unwrap();
        assert_eq!(refused.0, 409);
        assert_eq!(refused.1["error"], "held; will retry automatically");
        log[0].success = true;
        assert_eq!(retry_target(&log, held).err().unwrap().0, 400);
        assert_eq!(retry_target(&log, held + 1).err().unwrap().0, 404);

        // Retry the failed entry just before the held one; a new SMS shifts
        // the log while the forward runs
        let target = retry_target(&log, 1).unwrap();
        assert_eq!(target.sms_id, held as u64 - 1);
        push_log(&mut log, entry(999, 1, false, 999));

        assert!(update_retried(&mut log, &target, &Ok(()), 1000));
        let sent = log.iter().find(|e| e.sms_id == target.sms_id).unwrap();
        assert!(sent.success && sent.error.is_none());
        let held = log.iter().find(|e| e.sms_id == held as u64).unwrap();
        assert!(held.pending && !held.success, "the held entry is untouched");

        // Settled (or gone) meanwhile: nothing written
        assert!(!update_retried(&mut log, &target, &Err("x".into()), 1001));
        assert!(log.iter().any(|e| e.sms_id == target.sms_id && e.success));
    }

    /// Neither the cap nor "Clear log" silently drops a held forward.
    #[test]
    fn log_keeps_held() {
        let mut log: Vec<ForwardLogEntry> = (0..MAX_LOG_ENTRIES as u64)
            .map(|id| entry(id, 1, id % 2 == 0, id as i64))
            .collect();
        push_log(&mut log, entry(500, 1, true, 500));
        assert_eq!(log.len(), MAX_LOG_ENTRIES);
        assert!(
            log.iter().any(|e| e.sms_id == 0),
            "the oldest held entry stays"
        );
        assert!(
            !log.iter().any(|e| e.sms_id == 1),
            "the oldest settled entry goes"
        );
        assert_eq!(log.last().unwrap().sms_id, 500);

        // All held: only then is the oldest dropped
        let mut log: Vec<ForwardLogEntry> = (0..MAX_LOG_ENTRIES as u64)
            .map(|id| entry(id, 1, true, id as i64))
            .collect();
        push_log(&mut log, entry(500, 1, true, 500));
        assert_eq!(log.len(), MAX_LOG_ENTRIES);
        assert_eq!(log[0].sms_id, 1);

        clear_log(&mut log);
        assert_eq!(log.len(), MAX_LOG_ENTRIES, "held entries survive a clear");
        let mut log = vec![entry(1, 1, false, 1), entry(2, 1, true, 2)];
        clear_log(&mut log);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].sms_id, 2);
    }

    /// A failed rule of an SMS whose other rule is held stays in the log, so
    /// the held one going out later doesn't run delete/mark-read for it.
    #[test]
    fn failed_sibling_of_a_held_forward_survives_clear_and_trim() {
        let mut log = vec![entry(9, 1, false, 1), entry(9, 2, true, 1)];
        clear_log(&mut log);
        assert_eq!(log.len(), 2, "the failure is kept while rule 2 is held");
        assert!(!settle_pending(&mut log, (9, 2), None, false, 9), "rule 1 failed");

        let mut log = vec![entry(9, 1, false, 1), entry(9, 2, true, 1)];
        for id in 10..10 + MAX_LOG_ENTRIES as u64 {
            let mut done = entry(id, 1, false, id as i64);
            done.success = true;
            push_log(&mut log, done);
        }
        assert!(log.iter().any(|e| e.sms_id == 9 && e.rule_id == 1), "trimmed after newer settled entries");
        assert!(!settle_pending(&mut log, (9, 2), None, false, 9));
    }

    /// The post-forward actions become due when a held forward goes out and
    /// every other forward of that SMS already succeeded.
    #[test]
    fn post_forward_after_flush() {
        let mut sent = entry(5, 1, false, 1);
        sent.success = true;
        sent.error = None;
        let mut log = vec![
            sent,
            entry(5, 2, true, 1),
            entry(6, 1, false, 1),
            entry(6, 2, true, 1),
            entry(7, 1, true, 1),
            entry(7, 2, true, 1),
        ];

        assert!(
            settle_pending(&mut log, (5, 2), None, false, 9),
            "all rules done"
        );
        assert!(
            !settle_pending(&mut log, (6, 2), None, false, 9),
            "rule 1 failed"
        );
        assert!(
            !settle_pending(&mut log, (7, 1), None, false, 9),
            "rule 2 still held"
        );
        let held = Some("x; held for retry".to_string());
        assert!(
            !settle_pending(&mut log, (7, 2), held, true, 9),
            "still held"
        );
        let failed = Some("x (permanent error)".to_string());
        assert!(
            !settle_pending(&mut log.clone(), (7, 2), failed, false, 9),
            "failed"
        );
        assert!(settle_pending(&mut log, (7, 2), None, false, 9));
        assert!(
            !settle_pending(&mut log, (7, 2), None, false, 9),
            "already settled"
        );
    }
}
