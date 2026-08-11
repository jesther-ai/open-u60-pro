use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
}

// ── SMS message from ubus ───────────────────────────────────────────

struct DecodedSms {
    id: u64,
    sender: String,
    content: String,
    date: String,
}

// ── UCS-2 hex decoding ──────────────────────────────────────────────

fn decode_ucs2_hex(hex: &str) -> String {
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
            // Goes through sms::send_via_ubus rather than calling ubus directly:
            // firmware from the 2025-12 build requires `number` and
            // `message_body` to be encrypted (see web_crypto).
            let resp = crate::sms::send_via_ubus(&params)
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

// ── Time helper ─────────────────────────────────────────────────────

fn now_ts() -> i64 {
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}

// ── Core forwarder ──────────────────────────────────────────────────

pub struct SmsForwarder {
    config: Mutex<SmsForwardConfig>,
    state: Mutex<ForwardState>,
    has_service: AtomicBool,
    wan_connected: AtomicBool,
}

impl SmsForwarder {
    pub fn new() -> Self {
        let config = fs::read_to_string(CONFIG_PATH)
            .ok()
            .and_then(|s| serde_json::from_str::<SmsForwardConfig>(&s).ok())
            .unwrap_or_default();

        let state = fs::read_to_string(STATE_PATH)
            .ok()
            .and_then(|s| serde_json::from_str::<ForwardState>(&s).ok())
            .unwrap_or_default();

        SmsForwarder {
            config: Mutex::new(config),
            state: Mutex::new(state),
            has_service: AtomicBool::new(false),
            wan_connected: AtomicBool::new(false),
        }
    }

    /// Start the SMS forwarder with event-driven reception.
    /// Falls back to polling if the event channel disconnects.
    pub fn start(self: &Arc<Self>, sms_events: mpsc::Receiver<Value>, service_rx: mpsc::Receiver<Value>, wan_rx: mpsc::Receiver<Value>) {
        // Seed initial connectivity state (retry if services aren't registered yet)
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
                if let Ok(data) = ubus::call("zwrt_data", "get_wwaniface", Some(r#"{"source_module":"zte_topsw_data","cid":1}"#)) {
                    let connected = data["connect_status"].as_str().map(|s| s.starts_with("ipv4")).unwrap_or(false);
                    self.wan_connected.store(connected, Ordering::Relaxed);
                    eprintln!("[sms_forward] initial WAN state: connected={connected}");
                    connected
                } else {
                    false
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

        let conn_self = Arc::clone(self);
        std::thread::spawn(move || {
            conn_self.connectivity_monitor(service_rx, wan_rx);
        });

        let forwarder = Arc::clone(self);
        std::thread::spawn(move || {
            forwarder.event_loop(sms_events);
        });
    }

    fn connectivity_monitor(&self, service_rx: mpsc::Receiver<Value>, wan_rx: mpsc::Receiver<Value>) {
        loop {
            let mut got_event = false;

            // Check service events
            match service_rx.try_recv() {
                Ok(event) => {
                    let network_type = event["network_type"].as_str().unwrap_or("");
                    let has = !network_type.is_empty() && network_type != "NO_SERVICE" && !network_type.starts_with("LIMITED_SERVICE");
                    let prev = self.has_service.swap(has, Ordering::Relaxed);
                    if prev != has {
                        eprintln!("[sms_forward] service state changed: {network_type} (has_service={has})");
                    }
                    got_event = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    eprintln!("[sms_forward] service_rx disconnected");
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            // Check WAN events
            match wan_rx.try_recv() {
                Ok(event) => {
                    let wan_status = event["wan_status"].as_str().unwrap_or("");
                    let connected = wan_status.starts_with("ipv4");
                    let prev = self.wan_connected.swap(connected, Ordering::Relaxed);
                    if prev != connected {
                        eprintln!("[sms_forward] WAN state changed: {wan_status} (connected={connected})");
                    }
                    got_event = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    eprintln!("[sms_forward] wan_rx disconnected");
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            if !got_event {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
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

        loop {
            // Wait for event OR 5-min fallback timeout
            match rx.recv_timeout(Duration::from_secs(300)) {
                Ok(event) => {
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
                    // Fallback poll for any missed events
                    self.poll_once();
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

    /// Pure polling fallback if event bus dies entirely.
    fn poll_loop(&self) {
        loop {
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

                // Pre-forward connectivity check
                let connectivity_err = match &rule.destination {
                    ForwardDestination::Sms { .. } => {
                        if !self.has_service.load(Ordering::Relaxed) {
                            Some("no cellular service (permanent error)".to_string())
                        } else {
                            None
                        }
                    }
                    _ => {
                        if !self.wan_connected.load(Ordering::Relaxed) {
                            Some("no WAN connectivity (permanent error)".to_string())
                        } else {
                            None
                        }
                    }
                };

                if let Some(err) = connectivity_err {
                    eprintln!("[sms_forward] skipping forward for SMS {} -> {}: {err}", sms.id, destination_type_name(&rule.destination));
                    let entry = ForwardLogEntry {
                        timestamp: now_ts(),
                        sms_id: sms.id,
                        sender: sms.sender.clone(),
                        content_preview: preview(&sms.content, 80),
                        rule_name: rule.name.clone(),
                        destination_type: destination_type_name(&rule.destination).to_string(),
                        success: false,
                        error: Some(err),
                        rule_id: rule.id,
                        content: sms.content.clone(),
                        date: sms.date.clone(),
                    };
                    let mut state = self.state.lock().unwrap();
                    state.log.push(entry);
                    if state.log.len() > MAX_LOG_ENTRIES {
                        let excess = state.log.len() - MAX_LOG_ENTRIES;
                        state.log.drain(..excess);
                    }
                    drop(state);
                    all_succeeded = false;
                    if i + 1 < enabled_rules.len() {
                        std::thread::sleep(Duration::from_millis(INTER_RULE_DELAY_MS));
                    }
                    continue;
                }

                let result = forward_with_retry(&rule.destination, sms, &agent);

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
                    error: result.err(),
                    rule_id: rule.id,
                    content: sms.content.clone(),
                    date: sms.date.clone(),
                };

                let mut state = self.state.lock().unwrap();
                state.log.push(entry);
                if state.log.len() > MAX_LOG_ENTRIES {
                    let excess = state.log.len() - MAX_LOG_ENTRIES;
                    state.log.drain(..excess);
                }
                drop(state);

                // Delay between rules for the same SMS
                if i + 1 < enabled_rules.len() {
                    std::thread::sleep(Duration::from_millis(INTER_RULE_DELAY_MS));
                }
            }

            if sms.id > max_processed_id {
                max_processed_id = sms.id;
            }

            // Post-forward actions — only when ALL rules succeeded
            if all_succeeded && config.mark_read_after_forward {
                if let Err(e) = ubus::call(
                    "zwrt_wms",
                    "zwrt_wms_modify_tag",
                    Some(&json!({"id": sms.id.to_string(), "tag": 0}).to_string()),
                ) {
                    eprintln!("[sms_forward] mark-read failed for SMS {}: {e}", sms.id);
                }
            }
            if all_succeeded && config.delete_after_forward {
                if let Err(e) = ubus::call(
                    "zwrt_wms",
                    "zwrt_wms_delete_sms",
                    Some(&json!({"id": sms.id.to_string()}).to_string()),
                ) {
                    eprintln!("[sms_forward] delete failed for SMS {}: {e}", sms.id);
                }
            }

            // Delay between different SMS messages
            std::thread::sleep(Duration::from_millis(INTER_SMS_DELAY_MS));
        }

        // Update watermark
        let mut state = self.state.lock().unwrap();
        state.last_forwarded_id = max_processed_id;
        save_state(&state);
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
        if let Ok(mut data) = ubus::call("zwrt_wms", "zte_libwms_get_sms_data", Some(&params.to_string())) {
            // Firmware that has a key established returns number/content
            // encrypted; decrypt before any of the parsing below runs.
            crate::sms::resolve_message_fields(&mut data);
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
    let next_id = config.rules.iter().map(|r| r.id).max().unwrap_or(0) + 1;

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
    (201, result)
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
    fwd_state.log.clear();
    save_state(&fwd_state);
    (200, json!({"ok": true}))
}

/// POST /api/sms/forward/retry — Retry a failed forward log entry
pub fn retry_forward(state: &AppState, body: &[u8]) -> (u16, Value) {
    #[derive(Deserialize)]
    struct Req {
        index: usize,
    }

    let req: Req = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    // Convert reversed index (newest-first) to internal chronological index
    let (entry_clone, rule_id, internal_index) = {
        let fwd_state = state.sms_forward.state.lock().unwrap();
        let internal_index = match fwd_state.log.len().checked_sub(1 + req.index) {
            Some(i) => i,
            None => return (404, json!({"ok": false, "error": "log entry not found"})),
        };
        let entry = match fwd_state.log.get(internal_index) {
            Some(e) => e,
            None => return (404, json!({"ok": false, "error": "log entry not found"})),
        };
        if entry.success {
            return (400, json!({"ok": false, "error": "entry already succeeded"}));
        }
        (entry.clone(), entry.rule_id, internal_index)
    };

    // Look up rule by id from current config
    let dest = {
        let config = state.sms_forward.config.lock().unwrap();
        match config.rules.iter().find(|r| r.id == rule_id) {
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
    if let Some(entry) = fwd_state.log.get_mut(internal_index) {
        match &result {
            Ok(()) => {
                entry.success = true;
                entry.error = None;
                entry.timestamp = now_ts();
            }
            Err(e) => {
                entry.error = Some(e.clone());
                entry.timestamp = now_ts();
            }
        }
    }
    save_state(&fwd_state);

    match result {
        Ok(()) => (200, json!({"ok": true, "data": {"status": "sent"}})),
        Err(e) => (502, json!({"ok": false, "error": e})),
    }
}
