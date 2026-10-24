//! Tools › Automations (iOS `SchedulerView`, `SchedulerJobFormView`,
//! `SchedulerModels.swift`): the agent's scheduler jobs.
//!
//! - **The list** (a table screen, `tabs::tools`): each job's name, the
//!   app's schedule and action summaries, its switch (`…/toggle`, the app's
//!   `{"id","enabled"}`), a dot for its last run (green for 200, else red)
//!   and "Delete" (the app's swipe deletes at once; here it asks first,
//!   naming a restore that would then not run); "New Automation".
//! - **New / Edit Automation** (`…/new`, `…/<id>`): name, one of the app's
//!   five actions, "Recurring" (time and days) or "One-time" (date and
//!   time), "Reverse action at" (not for Reboot) and Save. Times are the
//!   router's local time, which the scheduler uses (the app's one-time
//!   date is the phone's; a page has no time zone without JavaScript), and
//!   the form says which. Without `:has()` every field shows and the server
//!   reads only the chosen schedule's.
//! - **Templates** are matched by method, path and body (and the restore's
//!   body): a job the app did not write this way, or one written through
//!   the API, is "Custom: METHOD path", kept as it is (the app turns it
//!   into "Airplane Mode ON" on save); its restore time can change only if
//!   it has one (its body is kept).
//! - Save carries a single-use token (a double click creates one job). A
//!   one-time job in the past runs at the next minute, so a new or changed
//!   one must be in the future (a web addition).
//! - The agent gives a new job the highest id plus one, so a deleted
//!   newest job's id comes back: Save, the switch and Delete carry the
//!   job's creation time (`c`) and act only on the job the page showed
//!   (else "Automation not found" / "job not found").
//! - A remote browser is told on the form that Airplane Mode ON and Mobile
//!   Data OFF cut its connection when they run (the immediate switches ask
//!   first; a schedule cannot).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use axum::extract::Request;
use axum::http::Method as HttpMethod;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::{json, Value};

use crate::auth::Principal;
use crate::handlers::AppState;
use crate::httpd::gate::{self, Lane};
use crate::httpd::{Ctx, Peer};
use crate::server::{self, Method};
use crate::web::action::Ask;
use crate::web::kit::{self, Icon, Tab};
use crate::web::render::{self, PageCtx};
use crate::web::screen::PageData;
use crate::web::session::{self, Form};
use crate::web::job::{self, OnceJob, Sent};
use crate::web::store::{Claim, Once, Token, SETTLE};
use crate::web::{action, fmt, sms, CspNonce};

pub const LIST: &str = "/ui/tools/automations";
pub const NEW: &str = "/ui/tools/automations/new";
pub const SAVE: &str = "/ui/tools/automations/save";
pub const TOGGLE: &str = "/ui/tools/automations/toggle";
pub const DELETE: &str = "/ui/tools/automations/delete";
pub const JOBS: &str = "/api/scheduler/jobs";

/// The list's notices (the app's words, green as the app's).
pub const SAVED: [(&str, &str); 3] =
    [("created", "Automation created"), ("updated", "Automation updated"), ("deleted", "Automation deleted")];

// --- The app's templates ---

/// One of the app's actions (`ActionTemplate`): its request and the
/// restore's body (none: no restore, Reboot).
pub struct Template {
    pub key: &'static str,
    pub label: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub body: Option<&'static str>,
    pub restore: Option<&'static str>,
}

pub const TEMPLATES: [Template; 5] = [
    Template {
        key: "airplane",
        label: "Airplane Mode ON",
        method: "POST",
        path: "/api/modem/airplane",
        body: Some(r#"{"operate_mode":"LPM"}"#),
        restore: Some(r#"{"operate_mode":"ONLINE"}"#),
    },
    Template {
        key: "data",
        label: "Mobile Data OFF",
        method: "PUT",
        path: "/api/modem/data",
        body: Some(r#"{"cid":1,"enable":0,"connect_status":"disconnected"}"#),
        restore: Some(r#"{"cid":1,"enable":1}"#),
    },
    Template {
        key: "guest",
        label: "Guest WiFi OFF",
        method: "PUT",
        path: "/api/wifi/guest",
        body: Some(r#"{"guest_disabled_2g":"1","guest_disabled_5g":"1"}"#),
        restore: Some(r#"{"guest_disabled_2g":"0","guest_disabled_5g":"0"}"#),
    },
    Template { key: "reboot", label: "Reboot", method: "POST", path: "/api/device/reboot", body: None, restore: None },
    Template {
        key: "powersave",
        label: "Power Save ON",
        method: "PUT",
        path: "/api/device/power-save",
        body: Some(r#"{"deviceInfoList":{"power_saver_mode":"1"}}"#),
        restore: Some(r#"{"deviceInfoList":{"power_saver_mode":"0"}}"#),
    },
];

fn json_of(s: Option<&str>) -> Value {
    s.and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null)
}

fn template(key: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.key == key)
}

/// The template a job is: its method, path and body as the app writes
/// them, and a restore only with the template's restore body.
pub fn template_of(job: &Value) -> Option<&'static Template> {
    let a = &job["action"];
    let restore = Some(&job["restore"]).filter(|r| !r.is_null());
    TEMPLATES.iter().find(|t| {
        a["method"] == t.method
            && a["path"] == t.path
            && a["body"] == json_of(t.body)
            && restore.is_none_or(|r| t.restore.is_some() && r["body"] == json_of(t.restore))
    })
}

// --- Summaries ---

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// "Oct 23, 2026 at 3:41 PM" (the app's medium date and short time), in
/// the router's time zone at that time.
fn medium(t: i64) -> String {
    let off = sms::local_offset(t);
    format!("{} at {}", sms::day(t, off), sms::clock(t, off))
}

/// The app's `scheduleSummary`: "Every day", "Mon–Fri", "Weekends" or the
/// days, " at HH:MM", " → HH:MM" with a restore; "Once: <date>".
pub fn schedule_summary(job: &Value) -> String {
    let s = &job["schedule"];
    match s["type"].as_str().unwrap_or("") {
        "recurring" => {
            let time = s["time"].as_str().unwrap_or("??:??");
            let days: Vec<u64> = s["days"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
            let days = match days.as_slice() {
                d if d.len() == 7 => "Every day".to_owned(),
                [0, 1, 2, 3, 4] => "Mon\u{2013}Fri".to_owned(),
                [5, 6] => "Weekends".to_owned(),
                d => d.iter().filter_map(|&d| DAYS.get(d as usize).copied()).collect::<Vec<_>>().join(", "),
            };
            match job["restore"]["time"].as_str() {
                Some(rt) => format!("{days} at {time} \u{2192} {rt}"),
                None => format!("{days} at {time}"),
            }
        }
        "once" => s["at"].as_i64().map_or_else(|| "One-time".to_owned(), |at| format!("Once: {}", medium(at))),
        other => other.to_owned(),
    }
}

/// The app's `actionSummary`: the template's label, else "METHOD path".
pub fn action_summary(job: &Value) -> String {
    match template_of(job) {
        Some(t) => t.label.to_owned(),
        None => format!("{} {}", job["action"]["method"].as_str().unwrap_or(""), job["action"]["path"].as_str().unwrap_or("")),
    }
}

/// A job as the app parses it (`SchedulerJob.parse`): one without these
/// fields is left out.
fn valid(job: &Value) -> bool {
    job["id"].is_u64()
        && job["name"].is_string()
        && job["enabled"].is_boolean()
        && job["schedule"]["type"].is_string()
        && job["action"]["method"].is_string()
        && job["action"]["path"].is_string()
}

fn job_id(job: &Value) -> u64 {
    job["id"].as_u64().unwrap_or(0)
}

/// When the agent created the job (unix seconds): with its id, which job
/// a form showed.
fn created(job: &Value) -> i64 {
    job["created_at"].as_i64().unwrap_or(0)
}

/// Job `id` of a jobs read.
fn find(jobs: &Value, id: u32) -> Option<&Value> {
    jobs.as_array()?.iter().find(|j| valid(j) && job_id(j) == u64::from(id))
}

// --- The list ---

/// A job: name, schedule and action (a link to edit it), its switch, the
/// last run's dot, and "Delete" (asked first).
fn row(csrf: &Markup, job: &Value) -> Markup {
    let id = job_id(job);
    let name = job["name"].as_str().unwrap_or("");
    let on = job["enabled"].as_bool().unwrap_or(false);
    html! {
        li.cr.fr id=(format!("j{id}")) {
            a.it.cv href=(format!("{LIST}/{id}")) {
                span.cvb {
                    span.nm { (name) }
                    span.rs { (schedule_summary(job)) }
                    span.rs.rd { (action_summary(job)) }
                }
            }
            // The switch with the last run's dot under it, as the app's.
            span.sd {
                form.sf method="post" action=(TOGGLE) {
                    (csrf)
                    input type="hidden" name="id" value=(id);
                    input type="hidden" name="c" value=(created(job));
                    input type="hidden" name="enabled" value=(if on { "0" } else { "1" });
                    button.sw type="submit" role="switch" aria-checked=(on) aria-label=(name) {}
                }
                @if let Some(status) = job["last_status"].as_u64() {
                    @if status == 200 {
                        span.dot role="img" aria-label="Last run succeeded" {}
                    } @else {
                        span.dot.bad role="img" aria-label="Last run failed" {}
                    }
                }
            }
            a.del href=(format!("{DELETE}?id={id}")) { "Delete" span.sr { " automation " (name) } }
        }
    }
}

/// Automations (`SchedulerView`), from the jobs read.
pub fn list(pd: &PageData) -> Markup {
    let jobs: Vec<&Value> = pd.read(0).and_then(Value::as_array).map(|a| a.iter().filter(|j| valid(j)).collect()).unwrap_or_default();
    let csrf = pd.csrf();
    html! {
        @if let Some(Err(e)) = pd.reads.first() {
            (kit::notice(false, &format!("Failed to load: {}", fmt::truncate(e, 120))))
        }
        @if jobs.is_empty() {
            (kit::empty_state(Icon::ClockArrow, "No Automations",
                "Schedule actions like airplane mode, reboot, or WiFi toggles to run automatically on the device."))
        } @else {
            (kit::section(Some("Automations"), None, html! { @for j in &jobs { (row(&csrf, j)) } }))
        }
        (kit::section(None, None, html! {
            li { a.it.bt href=(NEW) { (kit::icon(Icon::Plus)) span { "New Automation" } } }
        }))
    }
}

/// The delete question's job (`?id=`), if the read has it.
fn asked<'a>(pd: &'a PageData) -> Option<&'a Value> {
    let id = pd.param("id").and_then(|v| job_key(&v))?;
    find(pd.read(0)?, id)
}

/// "Delete “<name>”?" (the app deletes at once). A job that is gone (404)
/// or a failed read (503) offers only Cancel.
pub fn ask_delete(pd: &PageData) -> Markup {
    let job = asked(pd);
    let id = job.map(job_id).unwrap_or(0).to_string();
    let c = job.map(created).unwrap_or(0).to_string();
    let name = job.and_then(|j| j["name"].as_str()).unwrap_or("");
    let title = if job.is_some() { format!("Delete \u{201c}{name}\u{201d}?") } else { "Delete automation?".to_owned() };
    let restore = job.and_then(|j| j["restore"]["time"].as_str()).map(|t| {
        format!("Its restore at {t} no longer runs either, so nothing undoes the action if it has already run.")
    });
    let mut lines = vec!["It no longer runs."];
    if let Some(r) = &restore {
        lines.push(r);
    }
    let note = match pd.reads.first() {
        Some(Err(e)) => Some(format!("Failed to load: {}", fmt::truncate(e, 120))),
        _ if job.is_none() => Some("Automation not found. It may have been deleted.".to_owned()),
        _ => None,
    };
    let q = Ask {
        title: &title,
        lines: if job.is_some() { &lines[..] } else { &[] },
        action: DELETE,
        hidden: &[("id", id.as_str()), ("c", c.as_str())],
        label: job.map(|_| "Delete"),
        danger: true,
        cancel: LIST,
    };
    html! {
        @if let Some(n) = &note { (kit::notice(false, n)) }
        (action::ask(pd.csrf(), &q))
    }
}

/// The delete question's status: 404 for a job that is gone, 503 when the
/// jobs cannot be read.
pub fn ask_status(pd: &PageData) -> u16 {
    match pd.reads.first() {
        Some(Err(_)) => 503,
        _ if asked(pd).is_none() => 404,
        _ => 200,
    }
}

/// The id of the job the form showed (its id and creation time, `c`), in
/// the jobs read; for another job there now, 0: an id the agent never
/// gives, so it answers "job not found" and nothing changes.
fn shown_id(v: &action::Valid, reads: &[Value]) -> i64 {
    let id = u32::try_from(v.int("id")).unwrap_or(0);
    let same = reads.first().and_then(|jobs| find(jobs, id)).is_some_and(|j| created(j) == v.int("c"));
    if same {
        i64::from(id)
    } else {
        0
    }
}

/// The toggle's body (the app's).
pub fn toggle_body(v: &action::Valid, reads: &[Value]) -> Option<Value> {
    Some(json!({"id": shown_id(v, reads), "enabled": v.get("enabled") == "1"}))
}

/// The delete's body; none when the job the question showed is gone (a
/// second post of the same question, another tab): nothing is sent and
/// the post lands on "Automation deleted", as the first one.
pub fn delete_body(v: &action::Valid, reads: &[Value]) -> Option<Value> {
    Some(shown_id(v, reads)).filter(|id| *id != 0).map(|id| json!({"id": id}))
}

/// After a switch: back at its row.
pub fn at_job(v: &action::Valid) -> String {
    format!("#j{}", v.int("id"))
}

// --- Times (the router's local time) ---

/// "HH:MM" (zero-padded, as the scheduler compares it) from a time field
/// ("H:MM" and seconds accepted).
pub fn parse_time(s: &str) -> Option<String> {
    let mut parts = s.trim().split(':');
    let (h, m) = (parts.next()?, parts.next()?);
    let sec = parts.next();
    let num = |p: &str, max: u32| p.parse::<u32>().ok().filter(|n| (1..=2).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()) && *n <= max);
    let ok = parts.next().is_none() && sec.is_none_or(|s| s.split('.').next().and_then(|s| num(s, 59)).is_some());
    let (h, m) = (num(h, 23)?, num(m, 59)?);
    ok.then(|| format!("{h:02}:{m:02}"))
}

/// A `datetime-local` value ("YYYY-MM-DDTHH:MM", seconds accepted) read as
/// the router's local time: unix seconds.
pub fn parse_local(s: &str) -> Option<i64> {
    let (date, time) = s.trim().split_once('T')?;
    let d: Vec<&str> = date.split('-').collect();
    let [y, mo, day] = d.as_slice() else { return None };
    let n = |p: &str, len: usize| p.parse::<i64>().ok().filter(|_| p.len() == len && p.bytes().all(|b| b.is_ascii_digit()));
    let (y, mo, day) = (n(y, 4)?, n(mo, 2)?, n(day, 2)?);
    let hm = parse_time(time)?;
    let ok = (2000..=2099).contains(&y) && (1..=12).contains(&mo) && (1..=31).contains(&day);
    // The day must exist (no 31 April).
    let days = sms::days_from_civil(y, mo, day);
    let ok = ok && sms::local(days * 86_400, 0).0 == (y, mo, day);
    let naive = days * 86_400 + hm[..2].parse::<i64>().ok()? * 3600 + hm[3..].parse::<i64>().ok()? * 60;
    let first = naive - sms::local_offset(naive);
    ok.then(|| naive - sms::local_offset(first))
}

/// Unix seconds as a `datetime-local` value in the router's local time.
pub fn local_input(t: i64) -> String {
    let ((y, m, d), s) = sms::local(t, sms::local_offset(t));
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}", s / 3600, s / 60 % 60)
}

/// "UTC+2", "UTC+5:45", "UTC-3", "UTC".
pub fn utc_label(off: i64) -> String {
    let (sign, a) = if off < 0 { ("-", -off) } else { ("+", off) };
    match (a / 3600, a / 60 % 60) {
        (0, 0) => "UTC".to_owned(),
        (h, 0) => format!("UTC{sign}{h}"),
        (h, m) => format!("UTC{sign}{h}:{m:02}"),
    }
}

// --- New / Edit Automation ---

/// A job id in a path or field: digits, 1 and up.
fn job_key(s: &str) -> Option<u32> {
    s.parse::<u32>().ok().filter(|n| *n > 0 && s.bytes().all(|b| b.is_ascii_digit()))
}

pub enum Page {
    New,
    Edit(u32),
}

impl Page {
    pub fn of(path: &str) -> Option<Page> {
        match path.strip_prefix(LIST)?.strip_prefix('/')? {
            "new" => Some(Page::New),
            id => job_key(id).map(Page::Edit),
        }
    }
}

/// The form as typed (or as the job is).
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub id: Option<u32>,
    /// The edited job's creation time (`c`): with the id, which job it is.
    pub created: Option<i64>,
    pub name: String,
    /// A template key; "" keeps the job's own action (a custom one).
    pub action: String,
    pub once: bool,
    pub time: String,
    pub days: [bool; 7],
    pub at: String,
    pub restore: bool,
    pub rtime: String,
}

impl Input {
    /// The app's defaults (`SchedulerJobFormView`): Airplane Mode ON,
    /// recurring at 23:00 Mon–Fri, one-time an hour from now, restore off
    /// at 06:30.
    pub fn blank(now: i64) -> Input {
        Input {
            id: None,
            created: None,
            name: String::new(),
            action: "airplane".into(),
            once: false,
            time: "23:00".into(),
            days: [true, true, true, true, true, false, false],
            at: local_input(now + 3600),
            restore: false,
            rtime: "06:30".into(),
        }
    }

    /// A job's values; the other schedule's fields keep the defaults.
    pub fn of_job(job: &Value, now: i64) -> Input {
        let s = &job["schedule"];
        let mut days = [false; 7];
        for d in s["days"].as_array().into_iter().flatten().filter_map(Value::as_u64).filter(|d| *d < 7) {
            days[d as usize] = true;
        }
        let blank = Input::blank(now);
        let once = s["type"] == "once";
        Input {
            id: job["id"].as_u64().and_then(|id| u32::try_from(id).ok()),
            created: Some(created(job)),
            name: job["name"].as_str().unwrap_or("").to_owned(),
            action: template_of(job).map_or("", |t| t.key).to_owned(),
            once,
            time: s["time"].as_str().and_then(parse_time).unwrap_or(blank.time),
            days: if once { blank.days } else { days },
            at: s["at"].as_i64().map_or(blank.at, local_input),
            restore: !job["restore"].is_null(),
            rtime: job["restore"]["time"].as_str().and_then(parse_time).unwrap_or(blank.rtime),
        }
    }

    /// A posted form; `None` for a value no form of this page sends (400).
    pub fn parse(form: &Form) -> Option<Input> {
        let id = match form.get("id") {
            Some(v) => Some(job_key(v)?),
            None => None,
        };
        let action = form.get("action").unwrap_or("");
        if !(action.is_empty() && id.is_some() || template(action).is_some()) {
            return None;
        }
        let once = match form.get("st")? {
            "once" => true,
            "recurring" => false,
            _ => return None,
        };
        let mut days = [false; 7];
        for (_, v) in form.pairs().iter().filter(|(k, _)| k == "days") {
            days[["0", "1", "2", "3", "4", "5", "6"].iter().position(|d| d == v)?] = true;
        }
        let created = match form.get("c") {
            Some(c) => Some(c.parse::<i64>().ok().filter(|_| c.bytes().all(|b| b.is_ascii_digit()))?),
            None => None,
        };
        let text = |k: &str, n: usize| form.get(k).unwrap_or("").chars().take(n).collect::<String>();
        Some(Input {
            id,
            created,
            name: text("name", 200),
            action: action.to_owned(),
            once,
            time: text("time", 16),
            days,
            at: text("at", 32),
            restore: form.get("restore") == Some("1"),
            rtime: text("rtime", 16),
        })
    }

    /// Whether the form offers "Reverse action at": a template's (hidden
    /// by CSS for Reboot), or a custom job's own restore.
    fn restore_offered(&self, job: Option<&Value>) -> bool {
        !self.action.is_empty() || job.is_some_and(|j| !j["restore"].is_null())
    }
}

/// A field's message.
pub type Errors = Vec<(&'static str, &'static str)>;

/// The request body of a checked form (the app's `createJob` /
/// `updateJob`), or the messages per field. `job` is the stored job of an
/// edit (its switch is kept; a custom action and its restore body too).
pub fn body(input: &Input, job: Option<&Value>, now: i64) -> Result<Value, Errors> {
    let mut errors = Errors::new();
    let name = input.name.trim();
    if name.is_empty() {
        errors.push(("name", "Enter a name."));
    } else if name.chars().count() > 100 {
        errors.push(("name", "Enter at most 100 characters."));
    } else if name.chars().any(char::is_control) {
        errors.push(("name", "Enter a name without line breaks."));
    }
    // The template's request, or the job's own (kept as it is).
    let (mut act, restore_body) = match (template(&input.action), job) {
        (Some(t), _) => {
            let mut a = json!({"method": t.method, "path": t.path});
            if let Some(b) = t.body {
                a["body"] = json_of(Some(b));
            }
            (a, t.restore.map(|r| json_of(Some(r))))
        }
        (None, Some(j)) => (j["action"].clone(), j.get("restore").filter(|r| !r.is_null()).map(|r| r["body"].clone())),
        (None, None) => (Value::Null, None),
    };
    if act["body"].is_null() {
        if let Some(a) = act.as_object_mut() {
            a.remove("body");
        }
    }
    let schedule = if input.once {
        let stored = job.and_then(|j| j["schedule"]["at"].as_i64()).filter(|at| local_input(*at) == input.at.trim());
        match (stored, parse_local(&input.at)) {
            (Some(at), _) => json!({"type": "once", "at": at}),
            (None, Some(at)) if at > now => json!({"type": "once", "at": at}),
            (None, Some(_)) => {
                errors.push(("at", "Choose a time in the future."));
                Value::Null
            }
            (None, None) => {
                errors.push(("at", "Enter a date and time."));
                Value::Null
            }
        }
    } else {
        let days: Vec<usize> = (0..7).filter(|d| input.days[*d]).collect();
        if days.is_empty() {
            errors.push(("days", "Select at least one day."));
        }
        match parse_time(&input.time) {
            Some(t) => json!({"type": "recurring", "time": t, "days": days}),
            None => {
                errors.push(("time", "Enter a time."));
                Value::Null
            }
        }
    };
    let restore = match restore_body.filter(|_| input.restore) {
        Some(b) => match parse_time(&input.rtime) {
            Some(t) if b.is_null() => Some(json!({"time": t})),
            Some(t) => Some(json!({"time": t, "body": b})),
            None => {
                errors.push(("rtime", "Enter a time."));
                None
            }
        },
        None => None,
    };
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut b = json!({"name": name, "action": act, "schedule": schedule});
    if let Some(r) = restore {
        b["restore"] = r;
    }
    if let (Some(id), Some(j)) = (input.id, job) {
        b["id"] = id.into();
        b["enabled"] = j["enabled"].as_bool().unwrap_or(true).into();
    }
    Ok(b)
}

/// What a form is drawn with.
struct Draw<'a> {
    p: &'a Principal,
    nonce: &'a str,
    once: &'a Once,
    /// Connected over Tailscale: the form names the actions that cut it.
    remote: bool,
}

const REMOTE: &str = "You are connected remotely: Airplane Mode ON and Mobile Data OFF cut this connection when they run. Turn on \u{201c}Reverse action at\u{201d} to get it back.";

/// A notice above the form: (success, text).
type Note = Option<(bool, String)>;

fn err_of(errors: &Errors, name: &str) -> Option<&'static str> {
    errors.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

fn field_err(id: &str, err: Option<&str>) -> Markup {
    html! { @if let Some(e) = err { p.ferr id=(format!("{id}-err")) { (e) } } }
}

/// A time or date row: its label, the field (the app's compact picker).
fn picker(class: &str, kind: &str, name: &str, label: &str, value: &str, err: Option<&str>) -> Markup {
    html! {
        li class=(format!("it {class}")) {
            label for=(name) { (label) }
            input id=(name) type=(kind) name=(name) value=(value)
                aria-invalid=[err.map(|_| "true")] aria-describedby=[err.map(|_| format!("{name}-err"))];
        }
        @if let Some(e) = err { li class=(format!("it {class}")) { (field_err(name, Some(e))) } }
    }
}

/// The form (`SchedulerJobFormView`) with a new single-use token. `job`
/// is the stored job of an edit (a custom action is shown, not chosen).
fn form(app: &AppState, w: &Draw, input: &Input, job: Option<&Value>, note: Note, errors: &Errors, status: u16) -> Response {
    let token = w.once.fresh(&app.auth, w.p.id);
    let title = if input.id.is_some() { "Edit Automation" } else { "New Automation" };
    let custom = job.filter(|_| input.action.is_empty()).map(|j| {
        format!("Custom: {} {}", j["action"]["method"].as_str().unwrap_or(""), j["action"]["path"].as_str().unwrap_or(""))
    });
    let ferr = |name: &str| err_of(errors, name);
    let name_err = ferr("name");
    let days_err = ferr("days");
    let off = sms::local_offset(sms::now_unix());
    let body = html! {
        @match &note {
            Some((true, msg)) => p.msg.ok role="status" { (msg) },
            Some((false, msg)) => p.msg.err role="alert" { (msg) },
            None => {},
        }
        @if !errors.is_empty() && note.is_none() { (kit::notice(false, "Check the highlighted fields.")) }
        form.rf.af method="post" action=(SAVE) {
            (render::csrf_field(&app.auth, w.p))
            (render::once_field(&token))
            @if let Some(id) = input.id { input type="hidden" name="id" value=(id); }
            @if let Some(c) = input.created { input type="hidden" name="c" value=(c); }
            (kit::section(Some("Name"), None, html! {
                li.it.fld {
                    input type="text" name="name" value=(input.name) placeholder="Automation name" aria-label="Automation name"
                        maxlength="100" autocomplete="off" aria-invalid=[name_err.map(|_| "true")]
                        aria-describedby=[name_err.map(|_| "name-err")];
                    (field_err("name", name_err))
                }
            }))
            (kit::section(Some("Action"), (w.remote && custom.is_none()).then(|| html! { p.foot { (REMOTE) } }), html! {
                @match &custom {
                    Some(text) => li.it { span.muted { "Action" } span.dn { (text) } },
                    None => li.it {
                        label for="action" { "Action" }
                        select #action name="action" {
                            @for t in &TEMPLATES { option value=(t.key) selected[t.key == input.action] { (t.label) } }
                        }
                    },
                }
            }))
            (kit::section(Some("Schedule"), Some(html! {
                p.foot { "Times are the router's local time (" (utc_label(off)) ")." }
            }), html! {
                // The app's segmented "Type" (its label hidden, as the app's).
                li.it.fld {
                    fieldset.chips.c2 {
                        legend.sr { "Type" }
                        div {
                            @for (value, label, on) in [("recurring", "Recurring", !input.once), ("once", "One-time", input.once)] {
                                label {
                                    input.sr type="radio" id=(format!("st-{value}")) name="st" value=(value) checked[on];
                                    span { (label) }
                                }
                            }
                        }
                    }
                }
                (picker("k-r", "time", "time", "Time", &input.time, ferr("time")))
                li.it.fld.k-r {
                    fieldset.chips.c4 aria-invalid=[days_err.map(|_| "true")] aria-describedby=[days_err.map(|_| "days-err")] {
                        legend { "Days" }
                        div {
                            @for (i, d) in DAYS.iter().enumerate() {
                                label {
                                    input.sr type="checkbox" name="days" value=(i) checked[input.days[i]];
                                    span { (d) }
                                }
                            }
                        }
                    }
                    (field_err("days", days_err))
                }
                (picker("k-o", "datetime-local", "at", "Date & Time", &input.at, ferr("at")))
            }))
            // (`kit::section`'s markup, hidden by CSS for Reboot.)
            @if input.restore_offered(job) {
                section.group.k-x {
                    h2 { "Restore" }
                    ul.list {
                        li.it {
                            label.tl for="restore" {
                                span { "Reverse action at" }
                                input.sr #restore type="checkbox" role="switch" name="restore" value="1" checked[input.restore];
                                span.sw aria-hidden="true" {}
                            }
                        }
                        (picker("k-t", "time", "rtime", "Restore time", &input.rtime, ferr("rtime")))
                    }
                }
            }
            (kit::section(None, None, html! { li { button.it.cv.snd type="submit" { "Save" } } }))
        }
    };
    let pc = PageCtx { title, tab: Some(Tab::Tools), nonce: w.nonce, lead: html! { a.nl href=(LIST) { "Cancel" } }, ..PageCtx::default() };
    render::html(status, render::page(&pc, body))
}

/// The jobs read (in-process).
fn jobs(app: &AppState) -> Result<Value, (u16, String)> {
    crate::web::forward::data(server::route(&Method::Get, JOBS, app, b""))
}

/// "Automation not found" with the way back (a job that is gone).
fn gone(nonce: &str, title: &str, text: &str, status: u16) -> Response {
    let body = html! {
        (kit::empty_state(Icon::ClockArrow, title, text))
        p.ctr { a.btn href=(LIST) { "Back to Automations" } }
    };
    let pc = PageCtx { title: "Automations", tab: Some(Tab::Tools), nonce, lead: kit::back(LIST, "Automations"), ..PageCtx::default() };
    render::html(status, render::page(&pc, body))
}

/// `GET …/new` (reads nothing: it shows with the router busy) and
/// `GET …/<id>` (reads the job).
pub async fn get(ctx: &Ctx, p: Principal, nonce: String, remote: bool, page: Page) -> Response {
    let id = match page {
        Page::New => {
            let w = Draw { p: &p, nonce: &nonce, once: &ctx.once, remote };
            return form(&ctx.app, &w, &Input::blank(sms::now_unix()), None, None, &Vec::new(), 200);
        }
        Page::Edit(id) => id,
    };
    let (once, n) = (Arc::clone(&ctx.once), nonce.clone());
    let job = Box::new(move |app: &AppState| {
        gate::read_only(|| {
            let data = match jobs(app) {
                Ok(d) => d,
                Err((_, e)) => return gone(&n, "Failed to load", &fmt::truncate(&e, 120), 503),
            };
            let w = Draw { p: &p, nonce: &n, once: &once, remote };
            match find(&data, id) {
                Some(j) => form(app, &w, &Input::of_job(j, sms::now_unix()), Some(j), None, &Vec::new(), 200),
                None => gone(&n, "Automation not found", "It may have been deleted.", 404),
            }
        })
    });
    job::run(ctx, Lane::General, &nonce, &format!("{LIST}/{id}"), job).await
}

const EXPIRED: &str = "This form expired, so the automation was not saved. Save it again.";
const UNKNOWN: &str = "The automation may not have been saved. Check the list before saving again.";
const BUSY: &str = "Failed: the router is busy. Try again in a few seconds.";

/// What a save needs besides the form.
struct Req {
    ctx: Ctx,
    p: Principal,
    nonce: String,
    remote: bool,
    input: Input,
}

impl Req {
    fn retry(&self) -> String {
        self.input.id.map_or_else(|| NEW.to_owned(), |id| format!("{LIST}/{id}"))
    }
}

fn digest(form: &Form) -> u64 {
    let mut h = DefaultHasher::new();
    for (k, v) in form.pairs().iter().filter(|(k, _)| !matches!(k.as_str(), "csrf" | "once")) {
        (k, v).hash(&mut h);
    }
    h.finish()
}

/// The form again with `note`, as typed (an edit reads its job: whether
/// its action is custom).
async fn show(r: Req, note: Note, status: u16) -> Response {
    let (retry, new) = (r.retry(), r.input.id.is_none());
    let Req { ctx, p, nonce, remote, input } = r;
    let (once, n) = (Arc::clone(&ctx.once), nonce.clone());
    let draw = move |app: &AppState| {
        let data = input.id.and_then(|_| jobs(app).ok());
        let j = data.as_ref().zip(input.id).and_then(|(d, id)| find(d, id));
        form(app, &Draw { p: &p, nonce: &n, once: &once, remote }, &input, j, note, &Vec::new(), status)
    };
    if new {
        return draw(&ctx.app);
    }
    job::run(&ctx, Lane::General, &nonce, &retry, Box::new(move |app: &AppState| gate::read_only(|| draw(app)))).await
}

/// `POST …/save`: a session (else the login, nothing changed), the form,
/// origin and CSRF token, the fields (a value no form sends is 400), then
/// the single-use token (`job::once`): checked first, claimed right before
/// the call, given back if nothing is sent.
pub async fn post(ctx: Ctx, req: Request) -> Response {
    let nonce = CspNonce::of(&req);
    let remote = session::is_remote(req.extensions().get::<Peer>());
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, LIST, None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    let input = Input::parse(&form);
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        let back = input.and_then(|i| i.id).map_or_else(|| NEW.to_owned(), |id| format!("{LIST}/{id}"));
        return render::form_refused(&nonce, &back);
    }
    let Some(input) = input else {
        return render::bad_request(&nonce);
    };
    let token = form.get("once").unwrap_or("").to_owned();
    let (once, wp, wn, wi) = (Arc::clone(&ctx.once), p.clone(), nonce.clone(), input.clone());
    let work: OnceJob = Box::new(move |app: &AppState, claim| save(app, &Draw { p: &wp, nonce: &wn, once: &once, remote }, &wi, claim));
    let sent = job::once(&ctx, p.id, &nonce, &token, digest(&form), Lane::General, SETTLE, work).await;
    let r = Req { ctx, p, nonce, remote, input };
    match sent {
        Sent::Ran(resp) => resp,
        Sent::Done(Ok(())) => session::see_other(&saved_url(&r.input)),
        Sent::Done(Err((status, msg))) => show(r, Some((false, msg)), status).await,
        Sent::Expired => show(r, Some((false, EXPIRED.to_owned())), 400).await,
        Sent::Unknown => show(r, Some((false, UNKNOWN.to_owned())), 409).await,
        Sent::Busy => show(r, Some((false, BUSY.to_owned())), 503).await,
    }
}

fn saved_url(input: &Input) -> String {
    format!("{LIST}?saved={}", if input.id.is_some() { "updated" } else { "created" })
}

/// Save with a claimed token, in one gate task: read the jobs (an edit's
/// job, the one the form showed: its switch, a custom action), check, then
/// the call. Saved: the list with "Automation created" / "Automation
/// updated"; else the form with why.
fn save(app: &AppState, w: &Draw, input: &Input, claim: &mut Claim) -> Response {
    let data = match jobs(app) {
        Ok(d) => d,
        Err((status, e)) => return form(app, w, input, None, Some((false, format!("Failed: {}", fmt::truncate(&e, 120)))), &Vec::new(), status),
    };
    let stored = input.id.and_then(|id| find(&data, id));
    if input.id.is_some() && stored.is_none_or(|j| Some(created(j)) != input.created) {
        return gone(w.nonce, "Automation not found", "It may have been deleted.", 404);
    }
    let b = match body(input, stored, sms::now_unix()) {
        Ok(b) => b,
        Err(errors) => return form(app, w, input, stored, None, &errors, 400),
    };
    claim.disarm();
    let method = if input.id.is_some() { Method::Put } else { Method::Post };
    match crate::web::forward::data(server::route(&method, JOBS, app, b.to_string().as_bytes())) {
        Ok(_) => {
            claim.set(Token::Sent(claim.digest()));
            session::see_other(&saved_url(input))
        }
        Err((status, e)) => {
            let msg = format!("Failed: {}", fmt::truncate(&e, 120));
            claim.set(Token::Failed(claim.digest(), status, msg.clone()));
            form(app, w, input, stored, Some((false, msg)), &Vec::new(), status)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(v: Value) -> Value {
        let mut j = json!({"id": 3, "name": "Night", "enabled": true, "created_at": 0, "last_run": null, "last_status": null});
        for (k, x) in v.as_object().unwrap() {
            j[k] = x.clone();
        }
        j
    }

    /// The app's templates, matched by method, path and body: the app's
    /// bodies give the label; another body is a custom action.
    #[test]
    fn templates_match_the_app_bodies() {
        for t in &TEMPLATES {
            let mut a = json!({"method": t.method, "path": t.path});
            if let Some(b) = t.body {
                a["body"] = serde_json::from_str(b).unwrap();
            }
            let mut j = job(json!({"action": a, "schedule": {"type": "recurring", "time": "23:00", "days": [0]}}));
            assert_eq!(template_of(&j).map(|t| t.key), Some(t.key));
            assert_eq!(action_summary(&j), t.label);
            if let Some(r) = t.restore {
                j["restore"] = json!({"time": "06:30", "body": serde_json::from_str::<Value>(r).unwrap()});
                assert_eq!(template_of(&j).map(|t| t.key), Some(t.key));
                j["restore"]["body"] = json!({"x": 1});
                assert!(template_of(&j).is_none());
            } else {
                j["restore"] = json!({"time": "06:30"});
                assert!(template_of(&j).is_none(), "a reboot with a restore is custom");
            }
        }
        // Mobile data ON through the API: the app would call it "Mobile Data OFF".
        let j = job(json!({"action": {"method": "PUT", "path": "/api/modem/data", "body": {"cid": 1, "enable": 1}}}));
        assert!(template_of(&j).is_none());
        assert_eq!(action_summary(&j), "PUT /api/modem/data");
        assert_eq!(TEMPLATES.iter().filter(|t| t.restore.is_none()).map(|t| t.key).collect::<Vec<_>>(), ["reboot"]);
    }

    #[test]
    fn schedule_summaries_follow_the_app() {
        let s = |schedule: Value, restore: Value| schedule_summary(&job(json!({"schedule": schedule, "restore": restore})));
        let rec = |days: Value| json!({"type": "recurring", "time": "23:00", "days": days});
        assert_eq!(s(rec(json!([0, 1, 2, 3, 4, 5, 6])), Value::Null), "Every day at 23:00");
        assert_eq!(s(rec(json!([0, 1, 2, 3, 4])), json!({"time": "06:30"})), "Mon\u{2013}Fri at 23:00 \u{2192} 06:30");
        assert_eq!(s(rec(json!([5, 6])), Value::Null), "Weekends at 23:00");
        assert_eq!(s(rec(json!([6, 0, 9])), Value::Null), "Sun, Mon at 23:00");
        assert_eq!(s(json!({"type": "recurring", "days": [1]}), Value::Null), "Tue at ??:??");
        assert_eq!(s(json!({"type": "once"}), Value::Null), "One-time");
        assert!(s(json!({"type": "once", "at": 1_790_000_000}), Value::Null).starts_with("Once: "));
        assert_eq!(s(json!({"type": "cron"}), Value::Null), "cron");
    }

    #[test]
    fn times_are_checked_and_padded() {
        for (raw, want) in [("23:00", Some("23:00")), ("7:05", Some("07:05")), ("00:00:30", Some("00:00")), ("12:30:00.000", Some("12:30"))] {
            assert_eq!(parse_time(raw).as_deref(), want, "{raw}");
        }
        for bad in ["", "24:00", "12:60", "1200", "12:3a", "+1:00", "12:00:61", "1:2:3:4", "012:00"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
        assert_eq!((utc_label(0), utc_label(7200), utc_label(20_700), utc_label(-10_800)), ("UTC".into(), "UTC+2".into(), "UTC+5:45".into(), "UTC-3".into()));
        // A datetime-local value and back, in the router's zone.
        let at = parse_local("2026-10-23T15:41").unwrap();
        assert_eq!(local_input(at), "2026-10-23T15:41");
        assert_eq!(parse_local("2026-10-23T15:41:09"), Some(at));
        for bad in ["", "2026-10-23", "2026-02-30T10:00", "1999-01-01T00:00", "2026-1-23T10:00", "2026-10-23T25:00", "2026-10-23 10:00"] {
            assert_eq!(parse_local(bad), None, "{bad}");
        }
    }

    fn input() -> Input {
        Input { name: " Night ".into(), ..Input::blank(1_790_000_000) }
    }

    /// The app's bodies: the template's request, the schedule, a restore
    /// only when on and the template has one; an edit keeps the switch.
    #[test]
    fn bodies_follow_the_app() {
        let now = 1_790_000_000;
        let b = body(&input(), None, now).unwrap();
        assert_eq!(b, json!({
            "name": "Night",
            "action": {"method": "POST", "path": "/api/modem/airplane", "body": {"operate_mode": "LPM"}},
            "schedule": {"type": "recurring", "time": "23:00", "days": [0, 1, 2, 3, 4]},
        }));
        let i = Input { restore: true, action: "data".into(), ..input() };
        let b = body(&i, None, now).unwrap();
        assert_eq!(b["restore"], json!({"time": "06:30", "body": {"cid": 1, "enable": 1}}));
        assert_eq!(b["action"]["body"], json!({"cid": 1, "enable": 0, "connect_status": "disconnected"}));
        // Reboot: no body, never a restore.
        let i = Input { restore: true, action: "reboot".into(), ..input() };
        assert_eq!(body(&i, None, now).unwrap()["action"], json!({"method": "POST", "path": "/api/device/reboot"}));
        assert!(body(&i, None, now).unwrap().get("restore").is_none());
        // One-time: the router's local time, in the future.
        let i = Input { once: true, at: local_input(now + 7200), ..input() };
        assert_eq!(body(&i, None, now).unwrap()["schedule"], json!({"type": "once", "at": now + 7200 - (now + 7200) % 60}));
        let i = Input { once: true, at: local_input(now - 60), ..input() };
        assert_eq!(body(&i, None, now), Err(vec![("at", "Choose a time in the future.")]));
        // An edit: id and the job's switch; a past time it already had is kept to the second.
        let stored = job(json!({"id": 3, "enabled": false, "schedule": {"type": "once", "at": now - 3601},
            "action": {"method": "POST", "path": "/api/modem/airplane", "body": {"operate_mode": "LPM"}}}));
        let i = Input { id: Some(3), ..Input::of_job(&stored, now) };
        assert_eq!(i.action, "airplane");
        let b = body(&i, Some(&stored), now).unwrap();
        assert_eq!((b["id"].clone(), b["enabled"].clone(), b["schedule"]["at"].clone()), (json!(3), json!(false), json!(now - 3601)));
        // The checks.
        let i = Input { name: "  ".into(), time: "25:00".into(), days: [false; 7], restore: true, rtime: "x".into(), ..input() };
        assert_eq!(
            body(&i, None, now),
            Err(vec![("name", "Enter a name."), ("days", "Select at least one day."), ("time", "Enter a time."), ("rtime", "Enter a time.")])
        );
        assert_eq!(body(&Input { name: "a\nb".into(), ..input() }, None, now), Err(vec![("name", "Enter a name without line breaks.")]));
        assert_eq!(body(&Input { name: "x".repeat(101), ..input() }, None, now), Err(vec![("name", "Enter at most 100 characters.")]));
    }

    /// A custom action is kept as it is (the app turns it into Airplane
    /// Mode ON), with its own restore body; without one, no restore.
    #[test]
    fn custom_actions_are_kept() {
        let now = 1_790_000_000;
        let stored = job(json!({
            "action": {"method": "PUT", "path": "/api/modem/data", "body": {"cid": 1, "enable": 1}},
            "schedule": {"type": "recurring", "time": "07:00", "days": [0]},
            "restore": {"time": "08:00", "body": {"cid": 1, "enable": 0}},
        }));
        let i = Input::of_job(&stored, now);
        assert_eq!((i.action.as_str(), i.time.as_str(), i.restore, i.rtime.as_str()), ("", "07:00", true, "08:00"));
        assert!(i.restore_offered(Some(&stored)));
        let b = body(&Input { rtime: "09:15".into(), ..i.clone() }, Some(&stored), now).unwrap();
        assert_eq!(b["action"], stored["action"]);
        assert_eq!(b["restore"], json!({"time": "09:15", "body": {"cid": 1, "enable": 0}}));
        // Without a restore of its own, none is offered or sent.
        let mut bare = stored.clone();
        bare["restore"] = Value::Null;
        let i = Input { restore: true, ..Input::of_job(&bare, now) };
        assert!(!i.restore_offered(Some(&bare)));
        assert!(body(&i, Some(&bare), now).unwrap().get("restore").is_none());
    }

    /// The last run's dot: green for 200, else red; none before a run.
    #[test]
    fn rows_show_the_last_run() {
        let csrf = html! {};
        let j = |st: Value| {
            let j = job(json!({"id": 4, "last_status": st, "schedule": {"type": "once"}, "action": {"method": "POST", "path": "/api/x"}}));
            row(&csrf, &j).into_string()
        };
        assert!(j(json!(200)).contains("<span class=\"dot\" role=\"img\" aria-label=\"Last run succeeded\"></span>"));
        assert!(j(json!(503)).contains("<span class=\"dot bad\" role=\"img\" aria-label=\"Last run failed\"></span>"));
        assert!(!j(Value::Null).contains("dot"));
        assert!(j(Value::Null).contains("<span class=\"rs\">One-time</span><span class=\"rs rd\">POST /api/x</span>"));
    }

    #[test]
    fn posted_forms_are_whitelisted() {
        let f = |s: &str| Form::parse(s.as_bytes());
        let i = Input::parse(&f("name=N&action=guest&st=recurring&time=22%3A00&days=0&days=6&restore=1&rtime=07%3A00")).unwrap();
        assert_eq!((i.action.as_str(), i.days, i.restore, i.id), ("guest", [true, false, false, false, false, false, true], true, None));
        for bad in [
            "name=N&action=nope&st=once", "name=N&st=once", "name=N&action=reboot&st=weekly", "name=N&action=reboot",
            "name=N&action=reboot&st=once&days=7", "id=0&name=N&action=reboot&st=once", "id=x&action=&st=once",
        ] {
            assert_eq!(Input::parse(&f(bad)), None, "{bad}");
        }
        // An edit may keep its own action.
        assert_eq!(Input::parse(&f("id=4&name=N&st=once")).unwrap().action, "");
        assert_eq!(Page::of("/ui/tools/automations/12").map(|p| matches!(p, Page::Edit(12))), Some(true));
        for no in ["/ui/tools/automations/0", "/ui/tools/automations/x", "/ui/tools/automations/", "/ui/tools/automations"] {
            assert!(Page::of(no).is_none(), "{no}");
        }
    }
}
