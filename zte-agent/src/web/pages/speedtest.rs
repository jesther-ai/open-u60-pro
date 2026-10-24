//! Tools › Speed Test (iOS `SpeedTestView`): the router's own test against
//! a speedtest.net server.
//!
//! - The page reads the server list (the agent fetches it from
//!   speedtest.net and keeps it 5 minutes, so it waits in the slow lane)
//!   and streams the `speedtest` topic (the agent's progress, in memory:
//!   it is re-read only when the test changes it, and only while a page
//!   watches). It shows the router's test, whoever started it, and the
//!   last results.
//! - One message line, as the app's: the start's or Stop's result line
//!   (`?r=`), replaced by how a test ended once it ends while the page is
//!   open (the app shows only a test it watched: a fresh page shows no old
//!   outcome). The page's watch point (`since`: the progress's version at
//!   load, or before the start) travels in the stream URL and, without
//!   JavaScript, in the reload URL. After Stop, the app's "Speed test
//!   stopped" stands.
//! - "Start Speed Test" is a GET of the confirmation page (mobile data
//!   use, owner decision) with the chosen server; its form carries a
//!   single-use token (`pages::confirm`). "Stop Test" is an action; both
//!   come back to the chosen server.
//! - The parts that change are fragments (message, progress, results);
//!   the controls are always in the page and CSS shows Start or Stop by
//!   the progress fragment's class. Without JavaScript the page reloads
//!   every 3 s while a test runs.

use maud::{html, Markup};
use serde_json::{json, Value};

use crate::events::Topic;
use crate::web::action::Valid;
use crate::web::fmt;
use crate::web::kit::{self, Icon};
use crate::web::render::{self, Fragment, Views};
use crate::web::roll::roll;
use crate::web::screen::PageData;
use crate::web::session::query_param;

pub const PATH: &str = "/ui/tools/speedtest";
pub const START: &str = "/ui/tools/speedtest/start";
pub const STOP: &str = "/ui/tools/speedtest/stop";
pub const SERVERS: &str = "/api/speedtest/servers";

const T: &[Topic] = &[Topic::Speedtest];

pub const FRAGMENTS: [Fragment; 3] = [
    Fragment { id: "st-msg", topics: T, render: message },
    Fragment { id: "st-run", topics: T, render: controls },
    Fragment { id: "st-res", topics: T, render: results },
];

fn progress(v: &Views) -> Option<&Value> {
    v.value(Topic::Speedtest)
}

/// A test in flight: one of its phases, or the moment between the start's
/// reset (idle with a server) and its first phase.
pub fn running(p: &Value) -> bool {
    match p["phase"].as_str() {
        Some("latency" | "download" | "upload") => true,
        Some("idle") => p["server"].as_str().is_some_and(|s| !s.is_empty()),
        _ => false,
    }
}

/// The page's watch point (`Screen::arg`): the progress's version when
/// the page loaded (or `since`: before the start, or the first load of a
/// no-JavaScript reload), doubled, plus 1 on the page Stop led to.
pub fn since(v: &Views, query: Option<&str>) -> u64 {
    let carried = query_param(query, "since").filter(|s| s.len() <= 16).and_then(|s| u64::from_str_radix(&s, 16).ok());
    carried.unwrap_or_else(|| v.slot(Topic::Speedtest).map_or(0, |s| s.version) << 1 | u64::from(query_param(query, "stop").is_some()))
}

/// While a test runs, without JavaScript: reload every 3 s, keeping the
/// server, the result line and the watch point.
pub fn refresh(pd: &PageData) -> Option<String> {
    progress(pd.views).is_some_and(running).then(|| {
        let ok = |v: &String, hex: bool| (1..=32).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_digit() || hex && matches!(b, b'a'..=b'f'));
        let mut url = format!("{PATH}?");
        if let Some(server) = pd.param("server").filter(|v| ok(v, false)) {
            url += &format!("server={server}&");
        }
        if let Some(r) = pd.param("r").filter(|v| ok(v, true)) {
            url += &format!("r={r}&");
        }
        format!("{url}since={:x}", pd.views.arg)
    })
}

/// The app's `phaseLabel`.
fn phase_label(phase: &str) -> String {
    match phase {
        "latency" => "Testing Latency...".into(),
        "download" => "Downloading...".into(),
        "upload" => "Uploading...".into(),
        p => {
            let mut c = p.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        }
    }
}

/// The app's `formatBytes`.
pub fn bytes(n: u64) -> String {
    let kb = n as f64 / 1024.0;
    match kb / 1024.0 {
        _ if n < 1024 => format!("{n} B"),
        _ if kb < 1024.0 => format!("{kb:.1} KB"),
        mb if mb < 1024.0 => format!("{mb:.1} MB"),
        mb => format!("{:.2} GB", mb / 1024.0),
    }
}

/// `#st-msg`: how a test ended, in the app's words (green, or red for an
/// error), once it ends after the watch point; nothing while one runs, on
/// a fresh page, or on the page Stop led to. CSS hides the result line
/// after it while it shows.
fn message(v: &Views) -> Markup {
    let watched = v.arg & 1 == 0 && v.slot(Topic::Speedtest).is_some_and(|s| s.version > v.arg >> 1);
    let p = progress(v).filter(|_| watched);
    let note = match p.and_then(|p| p["phase"].as_str()) {
        Some("complete") => Some((true, "Speed test complete".to_owned())),
        Some("cancelled") => Some((true, "Speed test cancelled".to_owned())),
        Some("error") => {
            let e = p.and_then(|p| p["error"].as_str()).filter(|e| !e.is_empty()).unwrap_or("Speed test failed");
            Some((false, fmt::truncate(e, 120)))
        }
        _ => None,
    };
    html! {
        div #st-msg {
            @match note {
                Some((true, text)) => p.msg.ok role="status" { (text) },
                Some((false, text)) => p.msg.err role="alert" { (text) },
                None => {},
            }
        }
    }
}

/// `#st-run`: the phase, the percentage, its bar and the live speed while
/// a test runs (class `run`, which shows "Stop Test" instead of "Start");
/// else an empty row, hidden.
fn controls(v: &Views) -> Markup {
    let Some(p) = progress(v).filter(|p| running(p)) else {
        return html! { li #st-run .it {} };
    };
    let phase = p["phase"].as_str().unwrap_or("");
    let label = phase_label(phase);
    let pct = p["progress"].as_u64().unwrap_or(0).min(100);
    let mbps = p["live_speed_mbps"].as_f64().unwrap_or(0.0);
    html! {
        li #st-run .it .fld .run {
            span.row { span { (label) } span.muted.num { (roll(v, "pct", &format!("{pct}%"))) } }
            progress max="100" value=(pct) aria-label=(label) {}
            p.stn {
                @if phase == "download" { (kit::icon(Icon::ArrowDown)) }
                @if phase == "upload" { span.c-orange { (kit::icon(Icon::ArrowUp)) } }
                b.num { (roll(v, "mbps", &format!("{mbps:.1}"))) } " " span.muted { "Mbps" }
            }
        }
    }
}

/// `#st-res`: "Results" and "Transfer" once a test is complete.
fn results(v: &Views) -> Markup {
    let Some(p) = progress(v).filter(|p| p["phase"] == "complete") else {
        return html! { div #st-res {} };
    };
    let row = |label: &str, key: &str, unit: &str, places: usize| {
        p[key].as_f64().map(|x| kit::kv_row(None, label, &format!("{x:.places$} {unit}")))
    };
    html! {
        div #st-res {
            (kit::section(Some("Results"), None, html! {
                (row("Ping", "ping_ms", "ms", 1).unwrap_or_default())
                (row("Jitter", "jitter_ms", "ms", 1).unwrap_or_default())
                (row("Download", "download_mbps", "Mbps", 2).unwrap_or_default())
                (row("Upload", "upload_mbps", "Mbps", 2).unwrap_or_default())
            }))
            (kit::section(Some("Transfer"), None, html! {
                (kit::kv_row(None, "Downloaded", &bytes(p["download_bytes"].as_u64().unwrap_or(0))))
                (kit::kv_row(None, "Uploaded", &bytes(p["upload_bytes"].as_u64().unwrap_or(0))))
                @if let Some(s) = p["server"].as_str().filter(|s| !s.is_empty()) { (kit::kv_row(None, "Server", s)) }
            }))
        }
    }
}

/// A server's id (a number, or digits as a string).
pub fn id_of(s: &Value) -> Option<u64> {
    s["id"].as_u64().or_else(|| s["id"].as_str()?.parse().ok())
}

/// "<sponsor> - <name>, <country>" (the app's picker row).
pub fn server_label(s: &Value) -> String {
    let f = |k: &str| s[k].as_str().unwrap_or("").to_owned();
    format!("{} - {}, {}", f("sponsor"), f("name"), f("country"))
}

/// The page: the message line, the server picker (the first server
/// chosen, as the app's, or `?server=` coming back from the question or
/// Stop), the controls and the results.
pub fn page(pd: &PageData) -> Markup {
    let v = pd.views;
    let servers: Vec<&Value> = pd.read(0).and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    let ids: Vec<u64> = servers.iter().filter_map(|s| id_of(s)).collect();
    let asked = pd.param("server").and_then(|s| s.parse::<u64>().ok()).filter(|id| ids.contains(id));
    let chosen = asked.or_else(|| ids.first().copied());
    html! {
        (render::live_snapshot(PATH))
        @if let Some(Err(e)) = pd.reads.first() {
            p.msg.err role="alert" { "Failed to load servers: " (fmt::truncate(e, 120)) }
        }
        (message(v))
        @if let Some(n) = &pd.notice { (n) }
        // The picker and Start belong to this GET form (`form=`), which
        // asks first; Stop has a form of its own.
        form #st-go method="get" action=(START) {}
        (kit::section(Some("Server"), None, html! {
            li.it {
                label for="srv" { "Server" }
                select #srv name="server" form="st-go" {
                    option value="" selected[chosen.is_none()] { "Select a server" }
                    @for s in &servers {
                        @if let Some(id) = id_of(s) { option value=(id) selected[Some(id) == chosen] { (server_label(s)) } }
                    }
                }
            }
        }))
        section.group {
            h2 { "Controls" }
            ul.list {
                (controls(v))
                li.go { button.it.cv.snd type="submit" form="st-go" { "Start Speed Test" } }
                li.halt {
                    form.sf method="post" action=(STOP) {
                        (pd.csrf())
                        input type="hidden" name="server" value=(chosen.unwrap_or(0));
                        button.it.cv.snd.danger type="submit" { "Stop Test" }
                    }
                }
            }
        }
        (results(v))
    }
}

/// "Stop Test": the agent's body is ignored.
pub fn stop_body(_: &Valid, _: &[Value]) -> Option<Value> {
    Some(json!({}))
}

/// After Stop: the page at the chosen server, marked as the one Stop led
/// to (`since`).
pub fn stop_at(v: &Valid) -> String {
    match v.int("server") {
        n if n > 0 => format!("?server={n}&stop=1&"),
        _ => "?stop=1&".to_owned(),
    }
}

/// The app's "Speed test stopped"; a test that had already ended: said so.
pub fn stopped(_: &Valid, data: &Value) -> (Option<bool>, String) {
    if data["status"] == "stopping" {
        (Some(true), "Speed test stopped".into())
    } else {
        (None, "No speed test is running.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boottime::BootInstant;
    use crate::events::hub::SlotView;
    use std::sync::Arc;
    use std::time::Duration;

    fn views(p: Value) -> Views {
        let now = BootInstant::from_duration(Duration::from_secs(100));
        let slot = SlotView { version: 1, at: now, value_at: now, value: Some(Arc::new(p)), error: None };
        Views::new(now, vec![(Topic::Speedtest, slot)])
    }

    #[test]
    fn bytes_and_labels_follow_the_app() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(10_000_000), "9.5 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
        assert_eq!(phase_label("latency"), "Testing Latency...");
        assert_eq!(phase_label("upload"), "Uploading...");
        assert_eq!(phase_label("idle"), "Idle");
        assert_eq!(phase_label(""), "");
    }

    /// Running: the phase, percent, bar and speed (class `run`); done: the
    /// app's message and the results; before any test: nothing.
    #[test]
    fn fragments_follow_the_phase() {
        let v = views(json!({"phase": "download", "progress": 42, "live_speed_mbps": 87.26, "server": "S (N)"}));
        let run = controls(&v).into_string();
        assert!(run.starts_with("<li class=\"it fld run\" id=\"st-run\"><span class=\"row\"><span>Downloading...</span><span class=\"muted num\">42%</span></span>"), "{run}");
        assert!(run.contains("<progress max=\"100\" value=\"42\" aria-label=\"Downloading...\"></progress>"));
        assert!(run.contains("<b class=\"num\">87.3</b> <span class=\"muted\">Mbps</span>"), "{run}");
        assert_eq!(message(&v).into_string(), "<div id=\"st-msg\"></div>");
        assert_eq!(results(&v).into_string(), "<div id=\"st-res\"></div>");
        let done = views(json!({
            "phase": "complete", "progress": 100, "ping_ms": 21.456, "jitter_ms": 1.2, "download_mbps": 123.456,
            "upload_mbps": 20.0, "download_bytes": 240_000_000u64, "upload_bytes": 10_000_000, "server": "Example <b>Net</b> (Town)",
        }));
        assert_eq!(controls(&done).into_string(), "<li class=\"it\" id=\"st-run\"></li>");
        assert!(message(&done).into_string().contains("<p class=\"msg ok\" role=\"status\">Speed test complete</p>"));
        let r = results(&done).into_string();
        for want in ["<span>Ping</span><span class=\"v\">21.5 ms</span>", "<span>Download</span><span class=\"v\">123.46 Mbps</span>",
            "<span>Downloaded</span><span class=\"v\">228.9 MB</span>", "<span>Uploaded</span><span class=\"v\">9.5 MB</span>",
            "<span class=\"v\">Example &lt;b&gt;Net&lt;/b&gt; (Town)</span>"] {
            assert!(r.contains(want), "{want} in {r}");
        }
        let err = views(json!({"phase": "error", "error": "all ping attempts failed"}));
        assert!(message(&err).into_string().contains("<p class=\"msg err\" role=\"alert\">all ping attempts failed</p>"));
        assert!(message(&views(json!({"phase": "error"}))).into_string().contains(">Speed test failed<"));
        assert!(message(&views(json!({"phase": "cancelled"}))).into_string().contains(">Speed test cancelled<"));
        // Only an end after the watch point (version 1 here) shows; never
        // on the page Stop led to.
        for arg in [2, 1, 3] {
            let mut seen = views(json!({"phase": "complete"}));
            seen.arg = arg;
            assert_eq!(message(&seen).into_string(), "<div id=\"st-msg\"></div>", "{arg}");
        }
        let q = Some("server=7&since=1f&r=ab");
        assert_eq!((since(&views(Value::Null), q), since(&views(Value::Null), Some("stop=1")), since(&views(Value::Null), None)), (0x1f, 3, 2));
        assert_eq!(stop_at(&Valid::of(&[("server", "7")])), "?server=7&stop=1&");
        assert_eq!(stop_at(&Valid::of(&[("server", "0")])), "?stop=1&");
        // The moment after the start's reset counts as running; idle before any test does not.
        assert!(running(&json!({"phase": "idle", "server": "S (N)"})) && !running(&json!({"phase": "idle", "server": ""})));
    }
}
