//! The SMS model of the SMS pages (iOS `SMSModels.swift`, `SMSViewModel.swift`):
//! the list read, decoding, dates, conversations and their opaque keys,
//! the send body and its result.
//!
//! - **Stores** (DD3): "All" reads the internal store (1) then the SIM (0)
//!   and merges them by id, as the forwarder does (the firmware's
//!   `mem_store=2` leaves SIM messages out); "Internal" is 1, "SIM" 0. Each
//!   read is a `Result`: both failing is the page's error, one failing a
//!   note beside the other's messages, never an empty list.
//! - **Text**: UCS-2 hex decoded as UTF-16 code units, so surrogate pairs
//!   (emoji) survive. Only a body that is all hex in groups of 4 is
//!   decoded (the forwarder's rule; iOS also decodes the hex-looking parts
//!   of plain text, which turns "code 000000. …" into a NUL); anything else
//!   is shown as sent. Control characters other than tab and line breaks
//!   are dropped.
//! - **Conversations**: alphanumeric senders by their trimmed upper-case
//!   name, numbers by their last 8 digits; the longest variant is shown.
//!   A conversation's URL key is `AuthState::opaque_key("sms", key)`: 16
//!   hex, per process, never the number.
//! - **Times**: one offset for every SMS page, the router's local time
//!   zone (the zone its schedules use). Message dates carry their own
//!   offset and are converted; list labels count from the router clock,
//!   in the app's abbreviated words ("5 min. ago", "in 2 min."), or the
//!   date while the router clock is unset.

use serde_json::{json, Value};

use crate::handlers::AppState;

pub const UNREAD: u8 = 1;
pub const SENT: u8 = 2;
pub const FAILED: u8 = 3;
pub const DRAFT: u8 = 4;

/// The router clock counts as unset before 2024-01-01 (relative labels
/// would be wrong).
const CLOCK_SET: i64 = 1_704_067_200;

#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub id: u64,
    pub number: String,
    pub content: String,
    /// Unix seconds; `None` when the date does not parse.
    pub date: Option<i64>,
    /// 0 read, 1 unread, 2 sent, 3 failed, 4 draft.
    pub tag: u8,
}

impl Msg {
    pub fn incoming(&self) -> bool {
        self.tag < SENT
    }
}

pub struct Conv {
    /// The grouping key (`normalize`): never shown, never in a URL.
    pub key: String,
    /// The longest number variant (shown).
    pub number: String,
    /// Oldest first; never empty.
    pub msgs: Vec<Msg>,
    pub unread: usize,
}

impl Conv {
    pub fn latest(&self) -> &Msg {
        &self.msgs[self.msgs.len() - 1]
    }

    pub fn unread_ids(&self) -> Vec<u64> {
        self.msgs.iter().filter(|m| m.tag == UNREAD).map(|m| m.id).collect()
    }

    /// What a delete question was asked about: the count and the newest id
    /// ("3-12"), so a message that arrives meanwhile changes it.
    pub fn fingerprint(&self) -> String {
        format!("{}-{}", self.msgs.len(), self.msgs.iter().map(|m| m.id).max().unwrap_or(0))
    }

    /// The app's search: the number or any message, case-insensitive
    /// (`q` already lower-case).
    pub fn matches(&self, q: &str) -> bool {
        self.number.to_lowercase().contains(q) || self.msgs.iter().any(|m| m.content.to_lowercase().contains(q))
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Store {
    All,
    Internal,
    Sim,
}

impl Store {
    /// The segmented control: value, label (iOS `SMSStorageFilter`).
    pub const OPTIONS: [(Store, &'static str, &'static str); 3] =
        [(Store::All, "all", "All"), (Store::Internal, "internal", "Internal"), (Store::Sim, "sim", "SIM")];

    /// `store=`: none or empty is All; an unknown value is `None`.
    pub fn parse(v: Option<&str>) -> Option<Store> {
        match v.unwrap_or("") {
            "" => Some(Store::All),
            v => Store::OPTIONS.iter().find(|(_, value, _)| *value == v).map(|(s, _, _)| *s),
        }
    }

    pub fn value(self) -> &'static str {
        Store::OPTIONS.iter().find(|(s, _, _)| *s == self).map_or("all", |(_, v, _)| v)
    }

    /// `store=<value>&` for a link's query, empty for All (the default).
    pub fn param(self) -> &'static str {
        match self {
            Store::All => "",
            Store::Internal => "store=internal&",
            Store::Sim => "store=sim&",
        }
    }

    fn mem_stores(self) -> &'static [u8] {
        match self {
            Store::All => &[1, 0],
            Store::Internal => &[1],
            Store::Sim => &[0],
        }
    }
}

// --- Text ---

/// UCS-2 hex (UTF-16BE, 4 hex digits per code unit) as text (iOS
/// `decodeUCS2Hex`): code units are collected first, so surrogate pairs
/// survive; a group of 4 bytes that is not all hex is skipped (raw text,
/// even non-ASCII, never panics); a lone surrogate becomes U+FFFD.
pub fn decode_ucs2(hex: &str) -> String {
    let units: Vec<u16> = hex
        .as_bytes()
        .chunks_exact(4)
        .filter_map(|quad| quad.iter().try_fold(0u16, |acc, &b| Some(acc << 4 | (b as char).to_digit(16)? as u16)))
        .collect();
    String::from_utf16_lossy(&units)
}

/// A message body as shown: decoded if it is UCS-2 hex (every byte hex,
/// groups of 4) and that leaves text, else the raw text; either without
/// control characters other than tab and line breaks.
pub fn content(raw: &str) -> String {
    let keep = |c: char| !c.is_control() || matches!(c, '\t' | '\n' | '\r');
    let hex = raw.len().is_multiple_of(4) && raw.bytes().all(|b| b.is_ascii_hexdigit());
    let mut text = if hex { decode_ucs2(raw) } else { String::new() };
    text.retain(keep);
    if text.is_empty() {
        text = raw.to_owned();
        text.retain(keep);
    }
    text
}

/// Text as UCS-2 hex (iOS `encodeUCS2Hex`): every UTF-16 unit, upper case.
pub fn encode_ucs2(text: &str) -> String {
    text.encode_utf16().map(|u| format!("{u:04X}")).collect()
}

/// The GSM 7-bit default alphabet as the app has it (anything else needs
/// UNICODE).
const GSM7: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞ ÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?\
                    ¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜabcdefghijklmnopqrstuvwxyzäöñüà§";

pub fn is_gsm7(text: &str) -> bool {
    text.chars().all(|c| GSM7.contains(c))
}

/// `encode_type` of a send (iOS `getEncodeType`).
pub fn encode_type(text: &str) -> &'static str {
    if is_gsm7(text) {
        "GSM7_default"
    } else {
        "UNICODE"
    }
}

/// (length, most, per SMS) as the app's compose counter (`charInfo`,
/// "<n>/<max> (<n div_ceil per> SMS)"): characters for GSM-7 (765, 153
/// each), UTF-16 units for UNICODE (335, 67 each; an emoji counts 2, as on
/// the network; the app counts it 1).
pub fn length(text: &str) -> (usize, usize, usize) {
    if is_gsm7(text) {
        (text.chars().count(), 765, 153)
    } else {
        (text.encode_utf16().count(), 335, 67)
    }
}

/// A message the web sends: not blank, and within the limit (a web
/// addition; the app does not check).
pub fn check_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Enter a message.".into());
    }
    match length(text) {
        (n, max, _) if n > max && max == 765 => Err("Enter at most 765 characters.".into()),
        (n, max, _) if n > max => Err(format!("Enter at most {max} characters (this text needs Unicode).")),
        _ => Ok(()),
    }
}

/// The compose counter (iOS `charInfo`): "<length>/<max> (<n> SMS)", "0 SMS"
/// while empty.
pub fn counter(text: &str) -> String {
    let (n, max, per) = length(text);
    format!("{n}/{max} ({} SMS)", n.div_ceil(per))
}

/// A typed number as sent (New Message): without the spaces (no-break
/// ones too), `-`, `(`, `)` and `.` that phone keyboards and autofill add.
pub fn clean_number(n: &str) -> String {
    let mut n = n.to_owned();
    n.retain(|c| !matches!(c, ' ' | '\t' | '\u{a0}' | '\u{202f}' | '-' | '(' | ')' | '.'));
    n
}

/// A cleaned number (a web addition, the app sends what was typed): not
/// blank, at most 32 digits, letters, `+ * #`.
pub fn check_number(n: &str) -> Result<(), String> {
    if n.is_empty() {
        Err("Enter a phone number.".into())
    } else if n.chars().count() > 32 || !n.chars().all(|c| c.is_ascii_alphanumeric() || "+*#".contains(c)) {
        Err("Enter at most 32 digits, letters, spaces, +, * or #.".into())
    } else {
        Ok(())
    }
}

// --- Dates ---

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// The civil date of a day count.
pub fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `"YY,MM,DD,HH,MM,SS,TZ"` (`;` also), TZ in quarter-hours ("+32" is
/// +8 h; none is UTC): unix seconds, `None` if it does not parse.
pub fn parse_date(s: &str) -> Option<i64> {
    // Empty fields are dropped, as Swift's `split` does.
    let f: Vec<&str> = s.split([',', ';']).map(str::trim).filter(|f| !f.is_empty()).collect();
    let n: Vec<i64> = f.get(..6)?.iter().map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let &[y, mo, d, h, mi, sec] = n.as_slice() else { return None };
    let ok = (0..=99).contains(&y)
        && (1..=12).contains(&mo)
        && (1..=31).contains(&d)
        && (0..=23).contains(&h)
        && (0..=59).contains(&mi)
        && (0..=60).contains(&sec);
    // An offset that does not parse or is beyond ±16 h: UTC (the app's
    // `TimeZone(secondsFromGMT:)` gives none either).
    let tz = f.get(6).and_then(|t| t.parse::<i64>().ok()).filter(|q| q.abs() <= 64).unwrap_or(0);
    ok.then(|| days_from_civil(2000 + y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - tz * 900)
}

/// Now, in unix seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The router's UTC offset in seconds at `now` (its local time zone).
pub fn local_offset(now: i64) -> i64 {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // `time_t` is i64 on the router (and deprecated in libc's musl types).
    let t: i64 = now;
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        0
    } else {
        tm.tm_gmtoff
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// (civil date, seconds into the day) at `t` in the offset `off`.
pub fn local(t: i64, off: i64) -> ((i64, i64, i64), i64) {
    let l = t + off;
    (civil(l.div_euclid(86_400)), l.rem_euclid(86_400))
}

/// "3:41 PM", or with `secs` "3:41:07 PM", from seconds into the day.
fn twelve(s: i64, secs: bool) -> String {
    let (h, m) = (s / 3600, s / 60 % 60);
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    let ss = if secs { format!(":{:02}", s % 60) } else { String::new() };
    format!("{h12}:{m:02}{ss} {}", if h < 12 { "AM" } else { "PM" })
}

/// "3:41 PM" (a bubble's time; iOS `.time`).
pub fn clock(t: i64, off: i64) -> String {
    twelve(local(t, off).1, false)
}

/// "10/22/26, 3:41:07 PM" (the app's Forward Log: short date, medium
/// time).
pub fn stamp(t: i64, off: i64) -> String {
    let ((y, m, d), s) = local(t, off);
    format!("{m}/{d}/{:02}, {}", y.rem_euclid(100), twelve(s, true))
}

/// "Oct 21, 2026".
pub fn day(t: i64, off: i64) -> String {
    let ((y, m, d), _) = local(t, off);
    format!("{} {d}, {y}", MONTHS[(m - 1) as usize])
}

/// Whole calendar months from `a` to `b` (`a <= b`) in the offset `off`.
fn months_between(a: i64, b: i64, off: i64) -> i64 {
    let (((ya, ma, da), sa), ((yb, mb, db), sb)) = (local(a, off), local(b, off));
    let months = (yb * 12 + mb) - (ya * 12 + ma);
    months - i64::from((db, sb) < (da, sa))
}

/// The app's list time (`RelativeDateTimeFormatter`, abbreviated): the
/// largest whole unit, "5 sec. ago" … "2 yr. ago", "in 3 min." for a time
/// ahead of the router clock (as the app, "in 0 sec." for now). While the
/// router clock is unset, the date.
pub fn relative(t: i64, now: i64, off: i64) -> String {
    if now < CLOCK_SET {
        return day(t, off);
    }
    let (a, b) = if t < now { (t, now) } else { (now, t) };
    let secs = b - a;
    let (n, unit) = if secs < 60 {
        (secs, "sec.")
    } else if secs < 3600 {
        (secs / 60, "min.")
    } else if secs < 86_400 {
        (secs / 3600, "hr.")
    } else {
        match months_between(a, b, off) {
            m if m >= 12 => (m / 12, "yr."),
            m if m >= 1 => (m, "mo."),
            _ if secs >= 7 * 86_400 => (secs / (7 * 86_400), "wk."),
            _ => (secs / 86_400, if secs < 2 * 86_400 { "day" } else { "days" }),
        }
    };
    if t < now {
        format!("{n} {unit} ago")
    } else {
        format!("in {n} {unit}")
    }
}

/// `sms_time` of a send (iOS `formatSMSTime`): "YY;MM;DD;HH;MM;SS;+H", the
/// router's local time and its offset in whole hours.
pub fn sms_time(now: i64, off: i64) -> String {
    let ((y, mo, d), s) = local(now, off);
    let hours = off / 3600;
    let sign = if hours >= 0 { "+" } else { "" };
    format!("{:02};{mo:02};{d:02};{:02};{:02};{:02};{sign}{hours}", y % 100, s / 3600, s / 60 % 60, s % 60)
}

// --- Messages and conversations ---

fn num(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.trim().parse().ok())
}

/// One item of `messages[]`; `None` if it lacks an id, a number or a known
/// tag (the app drops those too).
fn message(item: &Value) -> Option<Msg> {
    let tag = num(&item["tag"]).filter(|t| *t <= u64::from(DRAFT))? as u8;
    Some(Msg {
        id: num(&item["id"])?,
        number: item["number"].as_str()?.to_owned(),
        content: content(item["content"].as_str().unwrap_or("")),
        date: item["date"].as_str().and_then(parse_date),
        tag,
    })
}

/// A list reply's messages.
pub fn messages(data: &Value) -> Vec<Msg> {
    data["messages"].as_array().map(|a| a.iter().filter_map(message).collect()).unwrap_or_default()
}

/// The conversation key of a sender (iOS `normalizeNumber`).
pub fn normalize(number: &str) -> String {
    let t = number.trim();
    if t.chars().any(char::is_alphabetic) {
        return t.to_uppercase();
    }
    let digits: Vec<char> = t.chars().filter(|c| c.is_numeric()).collect();
    if digits.is_empty() {
        return t.to_owned();
    }
    digits[digits.len().saturating_sub(8)..].iter().collect()
}

/// Messages grouped by sender (iOS `groupIntoConversations`): each oldest
/// first, the conversations newest first, both by (date, id); a message
/// without a date counts as `now` (the app's fallback). The display number
/// is the longest variant, the newest of equals.
pub fn group(msgs: Vec<Msg>, now: i64) -> Vec<Conv> {
    // Newest first, by ordered insertion of indices: the list arrives
    // nearly in order, and a generic sort costs about 5 KB of code per type.
    let at = |m: &Msg| (m.date.unwrap_or(now), m.id);
    let mut order: Vec<usize> = Vec::with_capacity(msgs.len());
    for (i, m) in msgs.iter().enumerate() {
        let k = at(m);
        order.insert(order.partition_point(|&j| at(&msgs[j]) > k), i);
    }
    let mut slots: Vec<Option<Msg>> = msgs.into_iter().map(Some).collect();
    // Conversations in the order of their newest message.
    let mut convs: Vec<Conv> = Vec::new();
    for m in order.into_iter().filter_map(|i| slots[i].take()) {
        let key = normalize(&m.number);
        match convs.iter_mut().find(|c| c.key == key) {
            Some(c) => c.msgs.push(m),
            None => convs.push(Conv { key, number: String::new(), msgs: vec![m], unread: 0 }),
        }
    }
    for c in &mut convs {
        let longest = c.msgs.iter().map(|m| m.number.chars().count()).max().unwrap_or(0);
        c.number = c.msgs.iter().find(|m| m.number.chars().count() == longest).map(|m| m.number.clone()).unwrap_or_default();
        c.msgs.reverse();
        c.unread = c.msgs.iter().filter(|m| m.tag == UNREAD).count();
    }
    convs
}

/// The URL key of a conversation (per process; `None` without randomness).
pub fn url_key(app: &AppState, c: &Conv) -> Option<String> {
    app.auth.opaque_key("sms", &c.key)
}

/// The conversation whose URL key is `key`.
pub fn find(app: &AppState, convs: Vec<Conv>, key: &str) -> Option<Conv> {
    convs.into_iter().find(|c| url_key(app, c).as_deref() == Some(key))
}

/// A URL key: 16 lowercase hex.
pub fn is_key(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

// --- Device calls ---

/// A handler's `(status, envelope)`: its `data`, or its error.
fn data((status, mut v): (u16, Value)) -> Result<Value, String> {
    if status < 400 {
        Ok(v["data"].take())
    } else {
        Err(v["error"].as_str().unwrap_or("error").to_owned())
    }
}

fn list_store(app: &AppState, mem: u8) -> Result<Value, String> {
    // Tests never reach the device: without a fake one, a failure.
    #[cfg(test)]
    return fake::with(app, |d| if mem == 1 { d.internal.clone() } else { d.sim.clone() }).unwrap_or(Err(fake::NONE.into()));
    // The app's request (at most 500 per store).
    #[cfg(not(test))]
    data(crate::sms::sms_list(app, json!({"page": 0, "data_per_page": 500, "mem_store": mem, "tags": 10, "order_by": "order by id desc"}).to_string().as_bytes()))
}

/// A list read: the messages, and a note when one of two stores failed.
pub struct Listing {
    pub msgs: Vec<Msg>,
    pub partial: Option<String>,
}

/// Read `store` (blocking; the read changes nothing). Every store failing
/// is an error; for All, one failing is a note beside the other's messages.
pub fn read(app: &AppState, store: Store) -> Result<Listing, String> {
    let mut msgs: Vec<Msg> = Vec::new();
    let mut failed = Vec::new();
    for &mem in store.mem_stores() {
        match list_store(app, mem) {
            // A message both stores list (by id) once, as the internal one.
            Ok(d) => {
                let known = msgs.len();
                for m in messages(&d) {
                    if !msgs[..known].iter().any(|k| k.id == m.id) {
                        msgs.push(m);
                    }
                }
            }
            Err(e) => failed.push((mem, e)),
        }
    }
    match failed.first() {
        Some((_, e)) if failed.len() == store.mem_stores().len() => Err(e.clone()),
        Some((mem, e)) => {
            let which = if *mem == 0 { "SIM" } else { "internal" };
            Ok(Listing { msgs, partial: Some(format!("Couldn't read {which} messages: {e}")) })
        }
        None => Ok(Listing { msgs, partial: None }),
    }
}

/// `(used, total)` of the internal store when its total is known and above
/// 0 (numbers or strings; the app reads numbers only).
pub fn parse_capacity(d: &Value) -> Option<(u64, u64)> {
    let total = num(&d["sms_nv_total"]).filter(|t| *t > 0)?;
    Some((num(&d["sms_nvused_total"]).unwrap_or(0), total))
}

/// The capacity read; a failure shows nothing (as the app).
pub fn capacity(app: &AppState) -> Option<(u64, u64)> {
    #[cfg(test)]
    return parse_capacity(&fake::with(app, |d| d.capacity.clone())?.ok()?);
    #[cfg(not(test))]
    parse_capacity(&data(crate::sms::sms_capacity(app)).ok()?)
}

/// A change through the API route (in-process).
fn post(app: &AppState, path: &'static str, body: Value) -> (u16, Value) {
    #[cfg(test)]
    return match fake::with(app, |d| {
        d.calls.push((path, body.clone()));
        (d.wait, d.reply.clone())
    }) {
        Some((wait, reply)) => {
            std::thread::sleep(wait.unwrap_or_default());
            reply
        }
        None => (503, json!({"ok": false, "error": fake::NONE})),
    };
    #[cfg(not(test))]
    crate::server::route(&crate::server::Method::Post, path, app, body.to_string().as_bytes())
}

/// `"3;5;"`: the legacy id list.
pub fn ids_param(ids: &[u64]) -> String {
    ids.iter().map(|id| format!("{id};")).collect()
}

/// Mark `ids` read (as the app, failures are silent).
pub fn mark_read(app: &AppState, ids: &[u64]) {
    if !ids.is_empty() {
        post(app, "/api/sms/read", json!({"id": ids_param(ids), "tag": 0}));
    }
}

/// Delete messages (the app's request, every id of a conversation; the
/// agent falls back to the database for SIM rows).
pub fn delete(app: &AppState, ids: &[u64]) -> Result<(), String> {
    data(post(app, "/api/sms/delete", json!({"id": ids_param(ids)}))).map(drop)
}

/// The send request (iOS `sendSMS`): always UCS-2 hex, `encode_type` by
/// alphabet, `id` "-1".
pub fn send_body(number: &str, text: &str, time: &str) -> Value {
    json!({
        "number": number,
        "sms_time": time,
        "message_body": encode_ucs2(text),
        "id": "-1",
        "encode_type": encode_type(text),
    })
}

/// The device's answer to a send: `result` 3 (or "3") or none is sent,
/// anything else a rejection (the forwarder's check; the API route does
/// not look).
pub fn send_result(data: &Value) -> Result<(), String> {
    match &data["result"] {
        Value::Null => Ok(()),
        r if num(r) == Some(3) => Ok(()),
        r => Err(format!("device rejected (result={})", super::fmt::truncate(&r.as_str().map_or_else(|| r.to_string(), str::to_owned), 20))),
    }
}

/// Send `text` to `number`: the status and error of a failure.
pub fn send(app: &AppState, number: &str, text: &str) -> Result<(), (u16, String)> {
    let now = now_unix();
    match data(post(app, "/api/sms/send", send_body(number, text, &sms_time(now, local_offset(now))))) {
        Ok(d) => send_result(&d).map_err(|e| (502, e)),
        Err(e) => Err((503, e)),
    }
}

/// Test only: the SMS device of one `AppState` (its two stores, the
/// capacity, the answer to every change, and the changes made).
#[cfg(test)]
pub mod fake {
    use std::sync::{Mutex, PoisonError};
    use std::time::Duration;

    use serde_json::{json, Value};

    use crate::handlers::AppState;

    /// What every read and change of an `AppState` without a fake device
    /// fails with (a test never reaches `ubus`).
    pub const NONE: &str = "no test device";

    #[derive(Clone)]
    pub struct Device {
        pub internal: Result<Value, String>,
        pub sim: Result<Value, String>,
        pub capacity: Result<Value, String>,
        pub reply: (u16, Value),
        pub calls: Vec<(&'static str, Value)>,
        /// How long each change takes (a send in progress).
        pub wait: Option<Duration>,
    }

    impl Device {
        /// Both stores with these `messages[]`, nothing in the SIM.
        pub fn with(internal: Value) -> Device {
            Device {
                internal: Ok(json!({"messages": internal})),
                sim: Ok(json!({"messages": []})),
                capacity: Ok(json!({"sms_nv_total": 100, "sms_nvused_total": 12})),
                reply: (200, json!({"ok": true, "data": {"result": 3}})),
                calls: Vec::new(),
                wait: None,
            }
        }
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

    #[must_use]
    pub fn set(app: &AppState, d: Device) -> Guard {
        let mut all = DEVICES.lock().unwrap_or_else(PoisonError::into_inner);
        all.retain(|(k, _)| *k != key(app));
        all.push((key(app), d));
        Guard(key(app))
    }

    pub fn with<R>(app: &AppState, f: impl FnOnce(&mut Device) -> R) -> Option<R> {
        let mut all = DEVICES.lock().unwrap_or_else(PoisonError::into_inner);
        all.iter_mut().find(|(k, _)| *k == key(app)).map(|(_, d)| f(d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-01-01T00:00:00Z.
    const NEW_YEAR: i64 = 1_704_067_200;
    const H: i64 = 3600;
    const DAY: i64 = 86_400;

    #[test]
    fn ucs2_keeps_emoji_and_never_panics_on_raw_text() {
        assert_eq!(decode_ucs2("00480069"), "Hi");
        // A surrogate pair is one emoji; lower-case hex works too.
        assert_eq!(decode_ucs2("D83DDE00"), "\u{1F600}");
        assert_eq!(decode_ucs2("d83dde000021"), "\u{1F600}!");
        // A lone surrogate is the replacement character, not a dropped unit.
        assert_eq!(decode_ucs2("D83D0041"), "\u{FFFD}A");
        // Groups that are not hex are skipped; a trailing partial group too.
        assert_eq!(decode_ucs2("0041zz4200420"), "AB");
        // Raw text, even non-ASCII (bytes, not chars): nothing, so shown raw.
        for raw in ["aéé", "Hello", "Код 1234", "\u{1F600}\u{1F600}", "é"] {
            assert_eq!(decode_ucs2(raw), "", "{raw}");
            assert_eq!(content(raw), raw);
        }
        assert_eq!(content(""), "");
        assert_eq!(content("004F004B"), "OK");
        // Only a body that is all hex in groups of 4 is decoded: plain text
        // with a hex-looking group stays itself (iOS shows a NUL for this).
        for raw in ["Your fake code is 000000. Do not share it.", "0041zz4200420", "004F004", "CAFE ", "0041 0042"] {
            assert_eq!(content(raw), raw);
        }
        // Control characters other than tab and line breaks are dropped;
        // nothing left: the raw text (without them).
        assert_eq!(content("0041000D000A00090007"), "A\r\n\t");
        assert_eq!(content("Fake\u{0}code\u{7}\n1"), "Fakecode\n1");
        assert_eq!((content("0000"), content("\u{0}")), ("0000".to_owned(), String::new()));
        // Round trip, emoji included.
        let text = "Hi \u{1F600} Ж\n€";
        assert_eq!(encode_ucs2(text), "004800690020D83DDE0000200416000A20AC");
        assert_eq!(content(&encode_ucs2(text)), text);
    }

    #[test]
    fn dates_carry_quarter_hour_offsets() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(days_from_civil(2024, 1, 1) * DAY, NEW_YEAR);
        for days in [0, 10_957, 19_723, 19_782, 20_747, -1, 2_932_896] {
            let (y, m, d) = civil(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil(19_782), (2024, 2, 29));
        // "+32" is +8 h, "-20" is -5 h, none is UTC; ";" works as ",".
        assert_eq!(parse_date("24,01,01,08,00,00,+32"), Some(NEW_YEAR));
        assert_eq!(parse_date("23,12,31,19,00,00,-20"), Some(NEW_YEAR));
        assert_eq!(parse_date("24;01;01;00;00;00"), Some(NEW_YEAR));
        assert_eq!(parse_date(" 24, 01, 01, 05, 45, 30, +23 "), Some(NEW_YEAR + 30));
        assert_eq!(parse_date("24,01,01,00,00,00,"), Some(NEW_YEAR));
        assert_eq!(parse_date("24,01,01,00,00,00,+99"), Some(NEW_YEAR));
        assert_eq!(parse_date("24,01,01,00,00,00,x"), Some(NEW_YEAR));
        for bad in ["", "24,01,01", "24,13,01,00,00,00", "24,01,01,24,00,00", "x,01,01,00,00,00", "2024,01,01,00,00,00"] {
            assert_eq!(parse_date(bad), None, "{bad}");
        }
    }

    #[test]
    fn times_in_one_offset() {
        let t = NEW_YEAR + 15 * H + 41 * 60;
        assert_eq!(clock(t, 0), "3:41 PM");
        assert_eq!(clock(t, 8 * H), "11:41 PM");
        assert_eq!(clock(t, -16 * H), "11:41 PM");
        assert_eq!(clock(NEW_YEAR, 0), "12:00 AM");
        assert_eq!(clock(NEW_YEAR + 12 * H + 5 * 60, 0), "12:05 PM");
        assert_eq!(day(t, 0), "Jan 1, 2024");
        assert_eq!(stamp(t + 7, 0), "1/1/24, 3:41:07 PM");
        assert_eq!(stamp(t, 9 * H), "1/2/24, 12:41:00 AM");
        assert_eq!(stamp(NEW_YEAR + 300 * DAY + 9 * H + 5, -H), "10/27/24, 8:00:05 AM");
        assert_eq!(day(t, 9 * H), "Jan 2, 2024");
        assert_eq!(sms_time(t, 8 * H), "24;01;01;23;41;00;+8");
        assert_eq!(sms_time(t, -5 * H - 1800), "24;01;01;10;11;00;-5");
        assert_eq!(sms_time(t, 0), "24;01;01;15;41;00;+0");
    }

    #[test]
    fn relative_labels_follow_the_app() {
        let now = NEW_YEAR + 200 * DAY + 12 * H;
        let cases: [(i64, &str); 16] = [
            (0, "in 0 sec."),
            (-5, "5 sec. ago"),
            (-59, "59 sec. ago"),
            (-60, "1 min. ago"),
            (-300, "5 min. ago"),
            (-2 * H - 59, "2 hr. ago"),
            (-23 * H, "23 hr. ago"),
            (-DAY, "1 day ago"),
            (-3 * DAY, "3 days ago"),
            (-8 * DAY, "1 wk. ago"),
            (-27 * DAY, "3 wk. ago"),
            (-40 * DAY, "1 mo. ago"),
            (-200 * DAY, "6 mo. ago"),
            (-400 * DAY, "1 yr. ago"),
            (90, "in 1 min."),
            (3 * H, "in 3 hr."),
        ];
        for (delta, want) in cases {
            assert_eq!(relative(now + delta, now, 0), want, "{delta}");
        }
        // An unset router clock: the date.
        assert_eq!(relative(NEW_YEAR, 1_000_000, 0), "Jan 1, 2024");
    }

    fn item(id: u64, number: &str, text: &str, date: &str, tag: impl Into<Value>) -> Value {
        let tag: Value = tag.into();
        json!({"id": id, "number": number, "content": encode_ucs2(text), "date": date, "tag": tag})
    }

    #[test]
    fn grouping_keys_and_order() {
        assert_eq!(normalize("+1 555 555 0100"), "55550100");
        assert_eq!(normalize("5555550100"), "55550100");
        assert_eq!(normalize("005555550100"), "55550100");
        assert_eq!(normalize(" 12345 "), "12345");
        assert_eq!(normalize(" Example-Bank "), "EXAMPLE-BANK");
        assert_eq!(normalize("+"), "+");
        let data = json!({"messages": [
            item(5, "+15555550100", "Second", "24,01,02,10,00,00,+0", "1"),
            item(4, "5555550100", "First", "24,01,01,10,00,00,+0", "0"),
            item(3, "EXAMPLE-BANK", "Code 000000", "24,01,03,10,00,00,+0", 1),
            item(2, "5555550100", "Mine", "24,01,01,11,00,00,+0", "3"),
            {"id": "7", "number": "Example-Bank", "content": "Plain text", "date": "bad", "tag": "0"},
            {"id": 8, "number": "+15555550199", "content": "", "date": "24,01,01,00,00,00", "tag": "9"},
            {"number": "+15555550199", "content": "", "tag": "0"},
            {"id": 9, "content": "", "tag": "0"},
        ]});
        let msgs = messages(&data);
        assert_eq!(msgs.iter().map(|m| m.id).collect::<Vec<_>>(), [5, 4, 3, 2, 7]);
        assert_eq!(msgs[4].content, "Plain text");
        assert_eq!(msgs[4].date, None);
        let now = NEW_YEAR + 10 * DAY;
        let convs = group(msgs, now);
        // The dateless message counts as now: the bank is newest. The
        // longest variant, the newest of equals: "Example-Bank" (id 7, now).
        assert_eq!(convs.iter().map(|c| c.number.as_str()).collect::<Vec<_>>(), ["Example-Bank", "+15555550100"]);
        let bank = &convs[0];
        assert_eq!((bank.key.as_str(), bank.unread, bank.latest().id), ("EXAMPLE-BANK", 1, 7));
        let person = &convs[1];
        assert_eq!(person.msgs.iter().map(|m| m.id).collect::<Vec<_>>(), [4, 2, 5]);
        assert_eq!((person.unread, person.unread_ids(), person.latest().content.as_str()), (1, vec![5], "Second"));
        assert!(person.matches("555") && person.matches("mine") && !person.matches("bank"));
        assert!(bank.matches("code") && bank.matches("example"));
        // Equal lengths: the newest message's, not the oldest's.
        let two = json!({"messages": [
            item(10, "015555550101", "Newer", "24,01,02,00,00,00", "0"),
            item(9, "+15555550101", "Older", "24,01,01,00,00,00", "0"),
        ]});
        let c = &group(messages(&two), now)[0];
        assert_eq!((c.number.as_str(), c.latest().id), ("015555550101", 10));
        assert_eq!(ids_param(&[3, 5]), "3;5;");
        assert_eq!((person.fingerprint(), bank.fingerprint()), ("3-5".to_owned(), "2-7".to_owned()));
        assert!(is_key("0123456789abcdef") && !is_key("0123456789ABCDEF") && !is_key("0123") && !is_key("+15555550100xxxx"));
    }

    #[test]
    fn stores_are_read_as_results() {
        let app = AppState::new();
        let mut d = fake::Device::with(json!([item(2, "+15555550100", "Internal", "24,01,01,00,00,00", "0")]));
        d.sim = Ok(json!({"messages": [
            item(2, "+15555550100", "Same id", "24,01,01,00,00,00", "0"),
            item(1, "+15555550101", "On the SIM", "24,01,01,00,00,00", "1"),
        ]}));
        let _g = fake::set(&app, d.clone());
        let all = read(&app, Store::All).unwrap();
        assert_eq!(all.msgs.iter().map(|m| (m.id, m.content.as_str())).collect::<Vec<_>>(), [(2, "Internal"), (1, "On the SIM")]);
        assert!(all.partial.is_none());
        assert_eq!(read(&app, Store::Sim).unwrap().msgs.len(), 2);
        assert_eq!(read(&app, Store::Internal).unwrap().msgs.len(), 1);
        // One store failing: the other's messages and a note.
        let _g = fake::set(&app, fake::Device { sim: Err("ubus timeout".into()), ..d.clone() });
        let all = read(&app, Store::All).unwrap();
        assert_eq!((all.msgs.len(), all.partial.as_deref()), (1, Some("Couldn't read SIM messages: ubus timeout")));
        assert_eq!(read(&app, Store::Sim).err().as_deref(), Some("ubus timeout"));
        // Both failing: the first error, never an empty list.
        let _g = fake::set(&app, fake::Device { internal: Err("first".into()), sim: Err("second".into()), ..d });
        assert_eq!(read(&app, Store::All).err().as_deref(), Some("first"));
        assert_eq!(Store::parse(None), Some(Store::All));
        assert_eq!(Store::parse(Some("sim")), Some(Store::Sim));
        assert_eq!(Store::parse(Some("2")), None);
        assert_eq!((Store::Internal.value(), Store::Sim.param(), Store::All.param()), ("internal", "store=sim&", ""));
    }

    #[test]
    fn encoding_limits_and_counter() {
        assert_eq!(encode_type("Hello @£$ ÄÖ\n"), "GSM7_default");
        for text in ["Привет", "€", "{", "\u{1F600}", "ç"] {
            assert_eq!(encode_type(text), "UNICODE", "{text}");
        }
        assert!(check_text(&"a".repeat(765)).is_ok());
        assert_eq!(check_text(&"a".repeat(766)).unwrap_err(), "Enter at most 765 characters.");
        assert!(check_text(&"Ж".repeat(335)).is_ok());
        assert!(check_text(&"Ж".repeat(336)).unwrap_err().contains("at most 335 characters"));
        // An emoji is two UTF-16 units.
        assert!(check_text(&"\u{1F600}".repeat(168)).is_err() && check_text(&"\u{1F600}".repeat(167)).is_ok());
        for blank in ["", "  ", "\n\t"] {
            assert_eq!(check_text(blank).unwrap_err(), "Enter a message.");
        }
        // The app's counter: 0 SMS while empty, then by 153 / 67.
        for (text, want) in [
            ("", "0/765 (0 SMS)"),
            ("Hi", "2/765 (1 SMS)"),
            (&*"a".repeat(153), "153/765 (1 SMS)"),
            (&*"a".repeat(154), "154/765 (2 SMS)"),
            (&*"Ж".repeat(68), "68/335 (2 SMS)"),
            (&*"Ж".repeat(336), "336/335 (6 SMS)"),
        ] {
            assert_eq!(counter(text), want, "{text}");
        }
        // The formatting autofill adds is dropped before the check.
        for (typed, sent) in [
            ("+15555550100", "+15555550100"),
            ("+1 (555) 555-0100", "+15555550100"),
            (" 555.555.0100\u{a0}", "5555550100"),
            ("*100#", "*100#"),
            ("EXAMPLE", "EXAMPLE"),
        ] {
            assert_eq!((clean_number(typed).as_str(), check_number(&clean_number(typed))), (sent, Ok(())), "{typed}");
        }
        assert_eq!(check_number(&clean_number(" - ")).unwrap_err(), "Enter a phone number.");
        for bad in ["555<0100>", "555/0100", "\u{0660}1", &*"5".repeat(33)] {
            assert_eq!(check_number(&clean_number(bad)).unwrap_err(), "Enter at most 32 digits, letters, spaces, +, * or #.", "{bad}");
        }
        assert!(check_number(&"5".repeat(32)).is_ok());
        assert_eq!(length(""), (0, 765, 153));
        assert_eq!(length(&"a".repeat(154)), (154, 765, 153));
        assert_eq!(length("Ж\u{1F600}"), (3, 335, 67));
    }

    #[test]
    fn send_body_result_and_capacity() {
        let body = send_body("+15555550100", "Hi\n\u{1F600}", "24;01;01;10;00;00;+8");
        assert_eq!(
            body,
            json!({"number": "+15555550100", "sms_time": "24;01;01;10;00;00;+8", "message_body": "00480069000AD83DDE00",
                   "id": "-1", "encode_type": "UNICODE"})
        );
        assert_eq!(send_body("+15555550100", "Hi", "t")["encode_type"], "GSM7_default");
        for ok in [json!({"result": 3}), json!({"result": "3"}), json!({}), json!({"result": null}), Value::Null] {
            assert_eq!(send_result(&ok), Ok(()), "{ok}");
        }
        // Anything else that is there is a rejection, whatever its type.
        for (bad, shown) in [(json!(1), "1"), (json!("4"), "4"), (json!(-1), "-1"), (json!("fail"), "fail"), (json!(3.0), "3.0"), (json!([3]), "[3]")] {
            assert_eq!(send_result(&json!({"result": bad})).unwrap_err(), format!("device rejected (result={shown})"));
        }
        assert_eq!(parse_capacity(&json!({"sms_nv_total": "100", "sms_nvused_total": "12"})), Some((12, 100)));
        assert_eq!(parse_capacity(&json!({"sms_nv_total": 50, "sms_nvused_total": 50})), Some((50, 50)));
        assert_eq!(parse_capacity(&json!({"sms_nv_total": 0, "sms_nvused_total": 0})), None);
        assert_eq!(parse_capacity(&json!({"sms_nv_total": "x"})), None);
        assert_eq!(parse_capacity(&json!({"sms_nv_total": 100})), Some((0, 100)));
    }

    /// Random input never panics (xorshift, so a failure repeats): text,
    /// numbers and dates through every parser, random list items through
    /// the grouping, times across a wide range in every offset.
    #[test]
    fn random_input_never_panics() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        const PIECES: [&str; 18] = ["D83D", "DE00", "00", "0", "F", "zz", "é", "\u{1F600}", ",", ";", "+", "-", "\0", " ", "24", "1", "Ж", "#"];
        for _ in 0..3000 {
            let n = (next() % 24) as usize;
            let s: String = (0..n).map(|_| PIECES[(next() % 18) as usize]).collect();
            let date = format!(
                "{},{},{},{},{},{},{}",
                next() % 120, next() % 14, next() % 33, next() % 25, next() % 61, next() % 62, (next() % 200) as i64 - 100
            );
            let _ = (content(&s), decode_ucs2(&s), parse_date(&s), parse_date(&date), normalize(&s), length(&s), counter(&s));
            let _ = (check_text(&s), check_number(&clean_number(&s)), encode_ucs2(&s), send_body(&s, &s, &s));
            let t = (next() % (1 << 41)) as i64 - (1 << 40);
            let off = (next() % (32 * 3600)) as i64 - 16 * 3600;
            let _ = (relative(t, NEW_YEAR + (next() % (1 << 32)) as i64, off), clock(t, off), day(t, off), sms_time(t, off));
            let items = json!({"messages": [
                {"id": next() % 4, "number": s, "content": s, "date": date, "tag": next() % 6},
                {"id": (next() % 4).to_string(), "number": date, "content": s, "date": s, "tag": "1"},
                {"id": "x"}, 5, null,
            ]});
            for c in group(messages(&items), t) {
                let _ = (c.latest(), c.unread_ids(), c.fingerprint(), c.matches(&s.to_lowercase()));
            }
        }
    }
}
