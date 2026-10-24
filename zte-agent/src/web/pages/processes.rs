//! Processes (iOS `ProcessListSheet`), over CPU & Memory: the bloat
//! summary and the top 50 processes from the hub's `processes` topic (its
//! own tracker, sampled every 3 s only while the page is open), each bloat
//! row with a "Kill" link, and "Kill All Bloat" in the nav bar.
//!
//! Every kill is confirmed first on a page of its own (the app's single
//! kill has none), which reads the process name from `/proc`; the form
//! posts that name with the PID, and nothing is sent unless the PID still
//! runs it (a PID can be reused while the question is open). Kill All
//! Bloat on the web leaves the SMS daemon and the boot-barrier daemons of
//! daemon.conf running (owner decision): it sends their PIDs only for the
//! other bloat daemons, which the agent checks against its list again. A
//! single kill stays possible for every bloat daemon, with a warning for
//! the risky ones. The result ("Killed PID 812, freed 1.2 MB", or that
//! nothing was killed) is built from the agent's answer and shown on this
//! page by `?r=<id>`, to the session that asked.

use maud::{html, Markup};
use serde_json::{json, Value};

use crate::events::Topic;
use crate::handlers::AppState;
use crate::web::action::{self, Ask, Valid};
use crate::web::kit::{self, Icon};
use crate::web::render::{self, Fragment, Views};
use crate::web::screen::PageData;

pub const PATH: &str = "/ui/health/processes";
pub const KILL: &str = "/ui/health/processes/kill";
pub const KILL_ALL: &str = "/ui/health/processes/kill-all";

/// The largest PID Linux hands out (`pid_max` at most 2^22).
pub const PID_MAX: u32 = 4_194_304;

/// The agent's bloat daemons (`system::BLOAT_DAEMONS`, private there; a
/// test keeps the two lists equal): the names a kill form may post.
const BLOAT: [&str; 17] = [
    "zte_topsw_tr069",
    "zte_topsw_tr069_sub",
    "zte_topsw_tr098db",
    "zte_topsw_fota_result",
    "zte_mqtt_sdk_st",
    "zte_topsw_diag",
    "zte_topsw_samba",
    "zte_topsw_nfc",
    "zte_smart_manage",
    "zte_topsw_get_brand",
    "zte_topsw_mc",
    "zte_dm",
    "zte_topsw_wms",
    "zte_topsw_sleep_faw",
    "zte_topsw_jwxk_query",
    "zte-topsw-tunnel",
    "zte_dua",
];

/// Checked values of the kill form's `name` field.
pub const NAMES: &[&str] = &BLOAT;

/// The SMS daemon: the agent's SMS features (`ubus zwrt_wms`) stop with it.
const SMS: &str = "zte_topsw_wms";

/// The bloat daemons Kill All leaves running: the SMS daemon and the ones
/// `zte_topsw_daemon` waits for at boot (daemon.conf).
const KEEP: [&str; 7] = [
    SMS,
    "zte_topsw_mc",
    "zte_topsw_tr098db",
    "zte_dm",
    "zte_topsw_fota_result",
    "zte_smart_manage",
    "zte_topsw_sleep_faw",
];

fn is_bloat(name: &str) -> bool {
    BLOAT.contains(&name)
}

/// A bloat daemon Kill All Bloat kills on the web.
fn safe(name: &str) -> bool {
    is_bloat(name) && !KEEP.contains(&name)
}

// --- The list ---

pub const FRAGMENTS: [Fragment; 3] = [
    // The nav bar's trailing item (`screen::Bar::DoneTrail`).
    Fragment { id: "pr-all", topics: &[Topic::Processes], render: kill_all_link },
    Fragment { id: "pr-sum", topics: &[Topic::Processes], render: summary },
    Fragment { id: "pr-list", topics: &[Topic::Processes], render: list },
];

/// `1.2 MB` from 1024 KB up, else `812 KB` (the app's `formatKB`).
pub fn kb(kb: u64) -> String {
    if kb >= 1024 {
        format!("{:.1} MB", kb as f64 / 1024.0)
    } else {
        format!("{kb} KB")
    }
}

fn bloat_count(d: &Value) -> u64 {
    d["bloat_count"].as_u64().unwrap_or(0)
}

/// "Kill All Bloat" (red) while there is bloat it would kill, as the app's
/// toolbar: hidden while every bloat daemon is one it leaves (all of them
/// in the list, which holds the top 50 only).
pub fn kill_all_link(v: &Views) -> Markup {
    let kept = |d: &Value| d["processes"].as_array().map_or(0, |l| l.iter().filter(|p| KEEP.contains(&p["name"].as_str().unwrap_or(""))).count());
    let any = v.value(Topic::Processes).is_some_and(|d| bloat_count(d) as usize > kept(d));
    html! {
        span #pr-all { @if any { a.nl.c-red href=(KILL_ALL) { "Kill All Bloat" } } }
    }
}

/// The bloat summary, then a read error (the app's order), or "Loading
/// processes..." before the first reading.
fn summary(v: &Views) -> Markup {
    let slot = v.slot(Topic::Processes);
    let d = v.value(Topic::Processes);
    let error = slot.and_then(|s| s.error.as_deref());
    html! {
        div #pr-sum {
            @match d {
                None => {
                    @if error.is_none() { p.cap.ctr { "Loading processes..." } }
                }
                Some(d) => {
                    @if bloat_count(d) > 0 {
                    section.group {
                        ul.list {
                            li.it {
                                span.c-orange { (kit::icon(Icon::Warn)) " Bloat Daemons" }
                                span.v {
                                    small.fg { (bloat_count(d)) " processes" }
                                    small {
                                        (format!("{:.1}% CPU, ", d["bloat_cpu_pct"].as_f64().unwrap_or(0.0)))
                                        (kb(d["bloat_rss_kb"].as_u64().unwrap_or(0))) " RSS"
                                    }
                                }
                            }
                        }
                    }
                    }
                }
            }
            @if let Some(e) = error { (kit::notice(false, &crate::web::fmt::truncate(e, 120))) }
        }
    }
}

/// "Top Processes": name (orange for bloat), "PID n · state", CPU (red
/// above 10 %, orange above 2 %) and RSS; bloat rows link to their kill
/// confirmation (a link: a live fragment holds no form).
fn list(v: &Views) -> Markup {
    let Some(d) = v.value(Topic::Processes) else {
        return html! { section #pr-list hidden {} };
    };
    let rows = d["processes"].as_array().map(Vec::as_slice).unwrap_or_default();
    html! {
        section #pr-list .group {
            h2 { "Top Processes" }
            ul.list.pr {
                @for p in rows {
                    @let name = p["name"].as_str().unwrap_or("");
                    @let pid = p["pid"].as_u64().unwrap_or(0);
                    @let bloat = p["is_bloat"].as_bool().unwrap_or(false);
                    @let cpu = p["cpu_pct"].as_f64().unwrap_or(0.0);
                    @let tone = if cpu > 10.0 { Some("c-red") } else if cpu > 2.0 { Some("c-orange") } else { None };
                    li.it {
                        span {
                            span class=[bloat.then_some("c-orange")] { (name) }
                            small { "PID " (pid) " \u{b7} " (p["state"].as_str().unwrap_or("")) }
                        }
                        span.v {
                            span class=[tone] { (format!("{cpu:.1}%")) }
                            small { (kb(p["rss_kb"].as_u64().unwrap_or(0))) }
                        }
                        @if bloat && pid > 0 {
                            a.kl href=(format!("{KILL}?pid={pid}")) aria-label=(format!("Kill {name}")) { "Kill" }
                        }
                    }
                }
            }
        }
    }
}

pub fn page(pd: &PageData) -> Markup {
    html! {
        (render::live_snapshot(PATH))
        (summary(pd.views))
        (list(pd.views))
    }
}

// --- Kills ---

/// A process's name as the agent gives it (`system::read_proc_name_rss`):
/// the basename of the command, else `comm`.
fn proc_name(pid: u32) -> Option<String> {
    let cmd = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).ok().and_then(|s| {
        let s = s.replace('\0', " ");
        let first = s.split_whitespace().next()?;
        let base = first.rsplit('/').next().unwrap_or(first);
        (!base.is_empty()).then(|| base.to_owned())
    });
    cmd.or_else(|| {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        Some(stat.get(stat.find('(')? + 1..stat.rfind(')')?)?.to_owned())
    })
}

/// Every running bloat daemon: (pid, name), by PID.
fn running_bloat() -> Vec<(u32, String)> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    let mut out: Vec<(u32, String)> = dir
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.parse().ok()?;
            let name = proc_name(pid)?;
            is_bloat(&name).then_some((pid, name))
        })
        .collect();
    out.sort_unstable();
    out
}

/// The confirmation's read: `{"pid", "name"}` of `?pid=`, or why not.
pub fn process_read(_: &AppState, query: Option<&str>) -> Result<Value, String> {
    let pid = crate::web::session::query_param(query, "pid")
        .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|p| p.parse::<u32>().ok())
        .filter(|p| (1..=PID_MAX).contains(p))
        .ok_or("no process")?;
    let name = proc_name(pid).ok_or("not running")?;
    Ok(json!({"pid": pid, "name": name}))
}

/// The confirmation's read: the running bloat daemons, split into those
/// Kill All kills and those it leaves.
pub fn bloat_read(_: &AppState, _: Option<&str>) -> Result<Value, String> {
    Ok(split(&running_bloat()))
}

/// The kill-all action's read: the PIDs to kill now.
pub fn targets_read(_: &AppState, _: Option<&str>) -> Result<Value, String> {
    Ok(split(&running_bloat())["kill"].as_array().map(|k| k.iter().map(|p| p[0].clone()).collect()).unwrap_or_default())
}

/// `{"kill": [[pid, name], …], "keep": [name, …]}` (each kept name once).
fn split(running: &[(u32, String)]) -> Value {
    let mut keep: Vec<&str> = Vec::new();
    let mut kill = Vec::new();
    for (pid, name) in running {
        if safe(name) {
            kill.push(json!([pid, name]));
        } else if !keep.contains(&name.as_str()) {
            keep.push(name);
        }
    }
    json!({"kill": kill, "keep": keep})
}

/// Why a daemon is risky to kill (inferred copy: the app has no warning).
fn warnings(name: &str, remote: bool) -> Vec<String> {
    let mut out = Vec::new();
    if name == SMS {
        out.push(
            "zte_topsw_wms handles SMS: until it restarts or the router reboots, messages stop arriving and the SMS \
             tab, sending and forwarding stop working."
                .to_owned(),
        );
    } else if KEEP.contains(&name) {
        out.push(format!(
            "{name} is a boot-critical firmware daemon (daemon.conf). Killing it can disturb the router until it restarts."
        ));
        if remote {
            out.push("You are connected remotely: if the router stops answering, this page can\u{2019}t be reached until it restarts.".into());
        }
    }
    out
}

/// `GET …/kill?pid=N`: "Kill <name>?" (Android's copy; the app kills
/// without asking), with a warning for the SMS and daemon.conf daemons.
pub fn kill_page(pd: &PageData) -> Markup {
    let csrf = pd.csrf();
    let Some((pid, name)) = pd.read(0).and_then(|p| Some((p["pid"].as_u64()?, p["name"].as_str()?))) else {
        let q = Ask { title: "This process is no longer running.", lines: &[], label: None, ..ask(PATH) };
        return action::ask(csrf, &q);
    };
    if !is_bloat(name) {
        let title = format!("{name} is not a bloat daemon.");
        let q = Ask { title: &title, lines: &["Only bloat daemons can be killed here."], label: None, ..ask(PATH) };
        return action::ask(csrf, &q);
    }
    let title = format!("Kill {name}?");
    let mut lines = vec![format!("SIGKILL PID {pid}. It will return on reboot.")];
    lines.extend(warnings(name, pd.remote));
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let pid = pid.to_string();
    let q = Ask { title: &title, lines: &lines, hidden: &[("pid", &pid), ("name", name)], label: Some("Kill"), ..ask(KILL) };
    action::ask(csrf, &q)
}

/// `GET …/kill-all`: the app's question for the daemons it will kill,
/// named, and which it leaves running.
pub fn kill_all_page(pd: &PageData) -> Markup {
    let d = pd.read(0);
    let names = |key: &str, at: Option<usize>| -> Vec<String> {
        let list = d.and_then(|d| d[key].as_array()).map(Vec::as_slice).unwrap_or_default();
        let mut out: Vec<String> = Vec::new();
        for v in list {
            let name = at.map_or(v, |i| &v[i]).as_str().unwrap_or("").to_owned();
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    };
    let n = d.and_then(|d| d["kill"].as_array()).map_or(0, Vec::len);
    let (kill, keep) = (names("kill", Some(1)), names("keep", None));
    let mut lines = Vec::new();
    if n > 0 {
        lines.push(format!("This will SIGKILL {n} bloat daemons. They will return on reboot."));
        lines.push(kill.join(", "));
    } else {
        lines.push("No bloat daemons that are safe to kill are running.".to_owned());
    }
    if !keep.is_empty() {
        lines.push(format!(
            "Left running: {}. The SMS daemon and the boot-critical daemons of daemon.conf are never killed all at \
             once; kill one from the list if you must.",
            keep.join(", ")
        ));
    }
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let label = (n > 0).then_some("Kill All Bloat");
    let q = Ask { title: "Kill all bloat daemons?", lines: &lines, label, ..ask(KILL_ALL) };
    action::ask(pd.csrf(), &q)
}

/// The defaults of a kill confirmation posting to `action`.
fn ask(action: &str) -> Ask<'_> {
    Ask { title: "", lines: &[], action, hidden: &[], label: None, danger: true, cancel: PATH }
}

/// The kill of one PID, only while it still runs the daemon the question
/// named (else nothing is sent); the agent checks the name against its
/// list again.
pub fn kill_body(v: &Valid, _: &[Value]) -> Option<Value> {
    let pid = u32::try_from(v.int("pid")).ok()?;
    (proc_name(pid).as_deref() == Some(v.get("name"))).then(|| json!({"pids": [pid]}))
}

/// Kill All without the SMS and daemon.conf daemons: their PIDs only, or
/// nothing to send.
pub fn kill_all_body(_: &Valid, cur: &[Value]) -> Option<Value> {
    let pids = cur.first()?.as_array().filter(|p| !p.is_empty())?;
    Some(json!({"pids": pids}))
}

/// The app's result line, when the agent's answer lists the PID as killed;
/// else what happened instead (nothing sent: the PID runs another process
/// now or is gone; sent: the agent did not kill it).
pub fn killed_one(v: &Valid, d: &Value) -> (Option<bool>, String) {
    let pid = v.int("pid");
    let killed = d["killed"].as_array().is_some_and(|k| k.iter().any(|p| p["pid"].as_i64() == Some(pid)));
    if killed {
        (Some(true), format!("Killed PID {pid}, freed {}", kb(d["freed_rss_kb"].as_u64().unwrap_or(0))))
    } else if d.is_null() {
        (Some(false), format!("PID {pid} is no longer {}. Nothing was killed.", v.get("name")))
    } else {
        (Some(false), format!("PID {pid} was not killed."))
    }
}

/// The app's result line; nothing sent (no daemon to kill by the time of
/// the post): a neutral line, not a success.
pub fn killed_all(_: &Valid, d: &Value) -> (Option<bool>, String) {
    if d.is_null() {
        return (None, "No bloat daemons that are safe to kill are running. Nothing was killed.".to_owned());
    }
    let n = d["killed"].as_array().map_or(0, Vec::len);
    (Some(true), format!("Killed {n} daemons, freed {}", kb(d["freed_rss_kb"].as_u64().unwrap_or(0))))
}

/// CPU & Memory's "Processes" row: the app's orange "N bloat P%" when
/// there is bloat (`GET /api/system/top` at page load), then the chevron.
pub fn summary_row(top: Option<&Value>) -> Markup {
    let bloat = top.map(bloat_count).filter(|n| *n > 0);
    html! {
        li {
            a.it href=(PATH) {
                (kit::icon(Icon::ListNumber)) span { "Processes" }
                @if let (Some(n), Some(d)) = (bloat, top) {
                    span.v.c-orange { (format!("{n} bloat {:.0}%", d["bloat_cpu_pct"].as_f64().unwrap_or(0.0))) }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boottime::BootInstant;
    use crate::events::hub::SlotView;
    use std::sync::Arc;
    use std::time::Duration;

    const NOW: BootInstant = BootInstant::from_duration(Duration::from_secs(10_000));

    fn views(value: Option<Value>, error: Option<&str>) -> Views {
        let slot = SlotView { version: 1, at: NOW, value_at: NOW, value: value.map(Arc::new), error: error.map(Arc::from) };
        Views::new(NOW, vec![(Topic::Processes, slot)])
    }

    pub(crate) fn fixture() -> Value {
        json!({"total_count": 120, "bloat_count": 3, "bloat_cpu_pct": 4.25, "bloat_rss_kb": 23_142, "processes": [
            {"pid": 812, "name": "zte_topsw_diag", "cpu_pct": 12.0, "rss_kb": 2048, "state": "running", "is_bloat": true},
            {"pid": 90, "name": "<b>odd</b>", "cpu_pct": 2.5, "rss_kb": 1000, "state": "sleeping", "is_bloat": false},
            {"pid": 400, "name": "zte_topsw_wms", "cpu_pct": 0.0, "rss_kb": 1023, "state": "sleeping", "is_bloat": true},
            {"pid": 7, "name": "kthreadd", "cpu_pct": 2.0, "rss_kb": 0, "state": "zombie", "is_bloat": false},
        ]})
    }

    /// The agent's list, read from its source: the web's split must cover
    /// exactly the daemons the agent will kill.
    #[test]
    fn the_split_covers_the_agents_bloat_list() {
        let src = include_str!("../../system.rs");
        let start = src.find("const BLOAT_DAEMONS: &[&str] = &[").unwrap();
        let end = start + src[start..].find("];").unwrap();
        let mut agent: Vec<&str> = src[start..end].split('"').skip(1).step_by(2).collect();
        let mut web = BLOAT.to_vec();
        agent.sort_unstable();
        web.sort_unstable();
        assert_eq!(agent, web);
        assert!(KEEP.iter().all(|k| is_bloat(k)));
        // Kept: every bloat daemon that `zte_topsw_daemon` waits for at boot
        // (/etc/config/zte_topsw_daemon.conf, as listed in CLAUDE.md).
        const DAEMON_CONF: [&str; 16] = [
            "zte_topsw_mc", "zte_router", "zte_topsw_data", "zte_topsw_nwinfo", "zte_topsw_mdm", "zte_topsw_sleep_faw",
            "zte_topsw_apn", "zte_topsw_wms", "zte_topsw_key", "zte_topsw_led", "zte_topsw_tr098db", "zte_dm",
            "zte_topsw_fota_result", "zte_topsw_devui", "zte_topsw_wlan", "zte_smart_manage",
        ];
        for name in web {
            assert_eq!(KEEP.contains(&name), DAEMON_CONF.contains(&name), "{name}");
            assert_eq!(safe(name), !DAEMON_CONF.contains(&name), "{name}");
        }
    }

    #[test]
    fn rows_follow_the_app() {
        let v = views(Some(fixture()), None);
        let s = list(&v).into_string();
        assert!(s.starts_with("<section class=\"group\" id=\"pr-list\"><h2>Top Processes</h2><ul class=\"list pr\">"), "{s}");
        for want in [
            "<span class=\"c-orange\">zte_topsw_diag</span><small>PID 812 · running</small>",
            "<span class=\"c-red\">12.0%</span><small>2.0 MB</small>",
            "<a class=\"kl\" href=\"/ui/health/processes/kill?pid=812\" aria-label=\"Kill zte_topsw_diag\">Kill</a>",
            "<span>&lt;b&gt;odd&lt;/b&gt;</span><small>PID 90 · sleeping</small>",
            "<span class=\"c-orange\">2.5%</span><small>1000 KB</small>",
            "<small>1023 KB</small>",
            // 2.0 % is not above 2: the secondary colour.
            "<span>2.0%</span><small>0 KB</small>",
            "PID 7 · zombie",
        ] {
            assert!(s.contains(want), "{want} in {s}");
        }
        // Kill links on bloat rows only; no form in a live fragment.
        assert_eq!(s.matches("class=\"kl\"").count(), 2);
        assert!(!s.contains("<form") && !s.contains("<input"));
        let sum = summary(&v).into_string();
        assert!(sum.contains("Bloat Daemons") && sum.contains("<small class=\"fg\">3 processes</small><small>4.2% CPU, 22.6 MB RSS</small>"), "{sum}");
        assert!(kill_all_link(&v).into_string().contains("<a class=\"nl c-red\" href=\"/ui/health/processes/kill-all\">Kill All Bloat</a>"));
        // Only bloat Kill All leaves (all of it in the list): no Kill All
        // Bloat; one more bloat daemon outside the top 50: it is back.
        let rows = json!([{"pid": 6, "name": SMS}, {"pid": 7, "name": "zte_topsw_mc"}, {"pid": 8, "name": "init"}]);
        let kept = views(Some(json!({"bloat_count": 2, "processes": rows.clone()})), None);
        assert_eq!(kill_all_link(&kept).into_string(), "<span id=\"pr-all\"></span>");
        assert!(summary(&kept).into_string().contains("Bloat Daemons"));
        let more = views(Some(json!({"bloat_count": 3, "processes": rows})), None);
        assert!(kill_all_link(&more).into_string().contains(">Kill All Bloat</a>"));
        // No bloat: no summary, no Kill All Bloat.
        let none = views(Some(json!({"bloat_count": 0, "processes": []})), None);
        assert_eq!(summary(&none).into_string(), "<div id=\"pr-sum\"></div>");
        assert_eq!(kill_all_link(&none).into_string(), "<span id=\"pr-all\"></span>");
        // Before the first reading, and a failed one.
        let empty = views(None, None);
        assert!(summary(&empty).into_string().contains("Loading processes..."));
        assert_eq!(list(&empty).into_string(), "<section id=\"pr-list\" hidden></section>");
        let failed = summary(&views(None, Some("no /proc"))).into_string();
        assert!(failed.contains("role=\"alert\">no /proc<") && !failed.contains("Loading"), "{failed}");
        // The app's order: the bloat summary, then the error.
        let both = summary(&views(Some(fixture()), Some("no /proc"))).into_string();
        assert!(both.find("Bloat Daemons").unwrap() < both.find("no /proc").unwrap(), "{both}");
    }

    #[test]
    fn kb_and_summary_row() {
        assert_eq!((kb(0), kb(1023), kb(1024), kb(23_142)), ("0 KB".into(), "1023 KB".into(), "1.0 MB".into(), "22.6 MB".into()));
        let row = summary_row(Some(&fixture())).into_string();
        assert!(row.contains("<a class=\"it\" href=\"/ui/health/processes\">") && row.contains("<span class=\"v c-orange\">3 bloat 4%</span>"), "{row}");
        assert!(!summary_row(Some(&json!({"bloat_count": 0}))).into_string().contains("bloat"));
        assert!(!summary_row(None).into_string().contains("bloat"));
    }

    /// Kill All leaves the SMS and daemon.conf daemons; each kept name once.
    #[test]
    fn kill_all_targets_and_bodies() {
        let running: Vec<(u32, String)> = [(5, "zte_topsw_diag"), (6, SMS), (7, "zte_dm"), (8, "zte_topsw_diag"), (9, "zte_dm"), (10, "zte_dua")]
            .into_iter()
            .map(|(p, n)| (p, n.to_owned()))
            .collect();
        let s = split(&running);
        assert_eq!(s, json!({"kill": [[5, "zte_topsw_diag"], [8, "zte_topsw_diag"], [10, "zte_dua"]], "keep": [SMS, "zte_dm"]}));
        let v = Valid::of(&[]);
        assert_eq!(kill_all_body(&v, &[json!([5, 8, 10])]), Some(json!({"pids": [5, 8, 10]})));
        // Nothing safe to kill: nothing is sent (the agent's `{"all": true}` is never used).
        assert_eq!(kill_all_body(&v, &[json!([])]), None);
        // One PID: sent only while it runs the daemon the question named.
        assert_eq!(kill_body(&Valid::of(&[("pid", "4194304"), ("name", "zte_topsw_diag")]), &[]), None);
        let me = std::process::id();
        if let Some(name) = proc_name(me) {
            assert_eq!(kill_body(&Valid::of(&[("pid", &me.to_string()), ("name", &name)]), &[]), Some(json!({"pids": [me]})));
            assert_eq!(kill_body(&Valid::of(&[("pid", &me.to_string()), ("name", "zte_topsw_diag")]), &[]), None);
        }
        let freed = json!({"killed": [{"pid": 5}, {"pid": 8}], "skipped": [], "freed_rss_kb": 2048});
        assert_eq!(killed_all(&v, &freed), (Some(true), "Killed 2 daemons, freed 2.0 MB".into()));
        // Nothing to kill by the time of the post: nothing sent, a neutral line.
        let none = "No bloat daemons that are safe to kill are running. Nothing was killed.";
        assert_eq!(killed_all(&v, &Value::Null), (None, none.into()));
        // The app's line only for a PID the agent reports killed.
        let one = Valid::of(&[("pid", "812"), ("name", "zte_topsw_diag")]);
        let d = json!({"killed": [{"pid": 812, "name": "zte_topsw_diag"}], "skipped": [], "freed_rss_kb": 900});
        assert_eq!(killed_one(&one, &d), (Some(true), "Killed PID 812, freed 900 KB".into()));
        let gone = json!({"killed": [], "skipped": [{"pid": 812, "name": "zte_topsw_diag"}], "freed_rss_kb": 0});
        assert_eq!(killed_one(&one, &gone), (Some(false), "PID 812 was not killed.".into()));
        assert_eq!(killed_one(&one, &json!({"killed": [{"pid": 9}]})), (Some(false), "PID 812 was not killed.".into()));
        assert_eq!(killed_one(&one, &Value::Null), (Some(false), "PID 812 is no longer zte_topsw_diag. Nothing was killed.".into()));
    }

    #[test]
    fn risky_daemons_get_warnings() {
        assert!(warnings("zte_topsw_diag", true).is_empty());
        let w = warnings(SMS, true);
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("zte_topsw_wms handles SMS") && w[0].contains("SMS tab, sending and forwarding"));
        assert_eq!(warnings("zte_dm", false), ["zte_dm is a boot-critical firmware daemon (daemon.conf). Killing it can disturb the router until it restarts."]);
        assert!(warnings("zte_dm", true)[1].starts_with("You are connected remotely: "));
    }

    /// Render a confirmation block from a read, as a page would.
    fn confirm(f: fn(&PageData) -> Markup, read: Result<Value, String>, remote: bool) -> String {
        use crate::auth::{Kind, PeerKey};
        let app = AppState::new();
        app.auth.set_password("pw");
        let issued = app.auth.login("pw", Kind::Browser, PeerKey::UNKNOWN, &[]).unwrap();
        let p = app.auth.authenticate(Kind::Browser, issued.token.expose()).unwrap();
        let v = Views::new(NOW, vec![]);
        let reads = [read];
        f(&PageData { app: &app, p: &p, views: &v, reads: &reads, remote, form: None, query: None, once: None, at: None, ussd: None, notice: None, token: Default::default(), prefs: Default::default(), secure: false }).into_string()
    }

    #[test]
    fn kill_confirmations_name_the_daemons() {
        let s = confirm(kill_page, Ok(json!({"pid": 812, "name": "zte_topsw_diag"})), false);
        for want in [
            "<h2 class=\"q\">Kill zte_topsw_diag?</h2>", "<li>SIGKILL PID 812. It will return on reboot.</li>",
            "<form class=\"panel\" method=\"post\" action=\"/ui/health/processes/kill\">",
            "<input type=\"hidden\" name=\"pid\" value=\"812\"><input type=\"hidden\" name=\"name\" value=\"zte_topsw_diag\">\
             <input type=\"hidden\" name=\"confirm\" value=\"1\">",
            "<button class=\"btn btn-danger\" type=\"submit\">Kill</button>",
            "<a class=\"btn btn-quiet\" href=\"/ui/health/processes\">Cancel</a>",
        ] {
            assert!(s.contains(want), "{want} in {s}");
        }
        assert_eq!(s.matches("<li>").count(), 1);
        // The SMS daemon and a daemon.conf one: warned, still possible.
        let s = confirm(kill_page, Ok(json!({"pid": 400, "name": SMS})), false);
        assert!(s.contains("<li>zte_topsw_wms handles SMS:") && s.contains(">Kill</button>"), "{s}");
        let s = confirm(kill_page, Ok(json!({"pid": 401, "name": "zte_topsw_mc"})), true);
        assert!(s.contains("boot-critical firmware daemon (daemon.conf)") && s.contains("connected remotely"), "{s}");
        // Not bloat, or gone: no form.
        let s = confirm(kill_page, Ok(json!({"pid": 1, "name": "<b>init</b>"})), false);
        assert!(s.contains("&lt;b&gt;init&lt;/b&gt; is not a bloat daemon.") && !s.contains("<form"), "{s}");
        assert!(!confirm(kill_page, Err("not running".into()), false).contains("<form"));
        // Kill All: the app's question for the daemons it kills, named once,
        // and the SMS and daemon.conf daemons left running.
        let read = split(&[(5, "zte_topsw_diag".into()), (6, SMS.into()), (8, "zte_topsw_diag".into()), (9, "zte_dua".into()), (11, "zte_dm".into())]);
        let s = confirm(kill_all_page, Ok(read), false);
        for want in [
            "<h2 class=\"q\">Kill all bloat daemons?</h2>", "<li>This will SIGKILL 3 bloat daemons. They will return on reboot.</li>",
            "<li>zte_topsw_diag, zte_dua</li>", "<li>Left running: zte_topsw_wms, zte_dm. The SMS daemon",
            "action=\"/ui/health/processes/kill-all\"", ">Kill All Bloat</button>",
        ] {
            assert!(s.contains(want), "{want} in {s}");
        }
        assert!(!s.contains("name=\"pid\"") && !s.contains("name=\"name\""));
        let s = confirm(kill_all_page, Ok(split(&[(6, SMS.into())])), false);
        assert!(s.contains("No bloat daemons that are safe to kill are running.") && s.contains("Left running: zte_topsw_wms.") && !s.contains("<form"), "{s}");
    }

    #[test]
    fn the_confirmation_read_takes_one_pid() {
        let app = AppState::new();
        for bad in [None, Some("pid="), Some("pid=0"), Some("pid=-1"), Some("pid=%2B5"), Some("pid=1,2"), Some("pid=4194305"), Some("pid=x")] {
            assert_eq!(process_read(&app, bad), Err("no process".to_owned()), "{bad:?}");
        }
    }
}
