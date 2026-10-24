use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::handlers::AppState;

const STORAGE_PATH: &str = "/data/local/tmp/scheduler.json";
/// Delay before the first tick after start, so past-due Once jobs do not fire
/// while the system is still coming up.
const BOOT_GRACE_SECS: u64 = 30;

#[derive(Serialize, Deserialize, Clone)]
pub struct Job {
    pub id: u32,
    pub name: String,
    pub enabled: bool,
    pub schedule: Schedule,
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore: Option<Restore>,
    pub last_run: Option<i64>,
    pub last_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub last_restore: Option<i64>,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum Schedule {
    #[serde(rename = "once")]
    Once { at: i64 },
    #[serde(rename = "recurring")]
    Recurring { time: String, days: Vec<u8> },
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Action {
    pub method: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Restore {
    pub time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Serialize, Deserialize)]
struct SchedulerData {
    jobs: Vec<Job>,
}

struct PendingAction {
    job_id: u32,
    method: String,
    path: String,
    body: Vec<u8>,
    is_restore: bool,
}

struct ExecResult {
    job_id: u32,
    is_restore: bool,
    status: u16,
    error: Option<String>,
}

pub struct Scheduler {
    data: Mutex<SchedulerData>,
    /// Signalled after every save() from the HTTP handlers so the scheduler
    /// thread, parked while no job is enabled, re-checks the job list.
    changed: Condvar,
}

fn now_local() -> (i64, u8, u8, u8) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let dow = ((tm.tm_wday + 6) % 7) as u8;
        (t as i64, tm.tm_hour as u8, tm.tm_min as u8, dow)
    }
}

/// Seconds to sleep so the next tick lands about one second after the next
/// minute boundary. Recomputed from the wall clock before every sleep, so
/// tick duration and sleep overshoot never accumulate into drift, and a
/// clock step costs at most one extra or one late tick (never a skipped
/// day, unlike sleeping until the next due HH:MM).
fn next_tick_delay(now_ts: i64) -> Duration {
    let sec = now_ts.rem_euclid(60);
    Duration::from_secs((61 - sec).clamp(1, 61) as u64)
}

fn has_enabled_job(data: &SchedulerData) -> bool {
    data.jobs.iter().any(|j| j.enabled)
}

fn parse_method(s: &str) -> Option<crate::server::Method> {
    use crate::server::Method;
    match s {
        "GET" => Some(Method::Get),
        "POST" => Some(Method::Post),
        "PUT" => Some(Method::Put),
        "DELETE" => Some(Method::Delete),
        _ => None,
    }
}

fn validate_time(s: &str) -> Result<(), String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err("time must be HH:MM".into());
    }
    let h: u8 = parts[0]
        .parse()
        .map_err(|_| "invalid hour".to_string())?;
    let m: u8 = parts[1]
        .parse()
        .map_err(|_| "invalid minute".to_string())?;
    if h > 23 {
        return Err("hour must be 0-23".into());
    }
    if m > 59 {
        return Err("minute must be 0-59".into());
    }
    Ok(())
}

fn validate_job(
    action: &Action,
    schedule: &Schedule,
    restore: &Option<Restore>,
) -> Result<(), String> {
    if !action.path.starts_with("/api/") {
        return Err("action.path must start with /api/".into());
    }
    if action.path.starts_with("/api/scheduler/") {
        return Err("cannot schedule scheduler endpoints".into());
    }
    if action.path.starts_with("/api/auth/") {
        return Err("cannot schedule auth endpoints".into());
    }
    if matches!(
        action.path.as_str(),
        "/api/tailscale/setup" | "/api/tailscale/logout" | "/api/tailscale/update"
    ) {
        // A scheduled logout/update is a remote-lockout device
        return Err("cannot schedule tailscale enrollment or maintenance endpoints".into());
    }
    if parse_method(&action.method).is_none() {
        return Err("action.method must be GET, POST, PUT, or DELETE".into());
    }
    match schedule {
        Schedule::Recurring { time, days } => {
            validate_time(time)?;
            if days.is_empty() {
                return Err("days must not be empty".into());
            }
            for &d in days {
                if d > 6 {
                    return Err("days must be 0-6 (Mon-Sun)".into());
                }
            }
        }
        Schedule::Once { at } => {
            if *at <= 1_000_000_000 {
                return Err("once.at must be a valid unix timestamp".into());
            }
        }
    }
    if let Some(r) = restore {
        validate_time(&r.time)?;
    }
    Ok(())
}

fn save(data: &SchedulerData) {
    if let Ok(json) = serde_json::to_string_pretty(data) {
        let _ = std::fs::write(STORAGE_PATH, json);
    }
}

impl Scheduler {
    pub fn new() -> Self {
        let data = std::fs::read_to_string(STORAGE_PATH)
            .ok()
            .and_then(|s| serde_json::from_str::<SchedulerData>(&s).ok())
            .unwrap_or(SchedulerData { jobs: Vec::new() });
        Scheduler {
            data: Mutex::new(data),
            changed: Condvar::new(),
        }
    }

    pub fn start(&self, state: Arc<AppState>) {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(BOOT_GRACE_SECS));
            loop {
                let scheduler = &state.scheduler;
                scheduler.wait_for_enabled_job();
                let (now_ts, _, _, _) = now_local();
                std::thread::sleep(next_tick_delay(now_ts));
                scheduler.tick(&state);
            }
        });
    }

    /// Parks the scheduler thread while no job is enabled. The HTTP handlers
    /// call `changed.notify_all()` after every save, so creating or
    /// re-enabling a job resumes the once-a-minute cadence.
    fn wait_for_enabled_job(&self) {
        let mut data = self.data.lock().unwrap();
        if has_enabled_job(&data) {
            return;
        }
        eprintln!("[scheduler] no enabled jobs, parking");
        while !has_enabled_job(&data) {
            data = self.changed.wait(data).unwrap();
        }
        eprintln!("[scheduler] enabled job found, resuming minute ticks");
    }

    fn tick(&self, state: &AppState) {
        let (now_ts, hour, minute, dow) = now_local();
        let now_hm = format!("{:02}:{:02}", hour, minute);

        // Phase 1: collect pending actions
        let pending: Vec<PendingAction> = {
            let data = self.data.lock().unwrap();
            let mut pending = Vec::new();

            for job in &data.jobs {
                if !job.enabled {
                    continue;
                }

                match &job.schedule {
                    Schedule::Recurring { time, days } => {
                        if *time == now_hm && days.contains(&dow) {
                            let guard = job
                                .last_run
                                .map(|lr| now_ts - lr > 60)
                                .unwrap_or(true);
                            if guard {
                                let body = action_body(&job.action.body);
                                pending.push(PendingAction {
                                    job_id: job.id,
                                    method: job.action.method.clone(),
                                    path: job.action.path.clone(),
                                    body,
                                    is_restore: false,
                                });
                            }
                        }
                    }
                    Schedule::Once { at } => {
                        if now_ts >= *at && job.last_run.is_none() {
                            let body = action_body(&job.action.body);
                            pending.push(PendingAction {
                                job_id: job.id,
                                method: job.action.method.clone(),
                                path: job.action.path.clone(),
                                body,
                                is_restore: false,
                            });
                        }
                    }
                }

                // Unified restore — fires at restore.time if action ran since last restore
                if let Some(restore) = &job.restore {
                    if restore.time == now_hm {
                        let should_restore = match (job.last_run, job.last_restore) {
                            (Some(lr), Some(lrest)) => lr > lrest,
                            (Some(_), None) => true,
                            _ => false,
                        };
                        let guard = job
                            .last_restore
                            .map(|lr| now_ts - lr > 60)
                            .unwrap_or(true);
                        if should_restore && guard {
                            let body = action_body(&restore.body);
                            pending.push(PendingAction {
                                job_id: job.id,
                                method: job.action.method.clone(),
                                path: job.action.path.clone(),
                                body,
                                is_restore: true,
                            });
                        }
                    }
                }
            }

            pending
        };

        if pending.is_empty() {
            return;
        }

        // Phase 2: execute actions (no lock held)
        let results: Vec<ExecResult> = pending
            .into_iter()
            .filter_map(|pa| {
                let method = parse_method(&pa.method)?;
                let (status, resp) = crate::server::route(&method, &pa.path, state, &pa.body);
                let error = if status >= 400 {
                    resp.get("error")
                        .and_then(|e| e.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                };
                Some(ExecResult {
                    job_id: pa.job_id,
                    is_restore: pa.is_restore,
                    status,
                    error,
                })
            })
            .collect();

        // Phase 3: update job state
        let mut data = self.data.lock().unwrap();
        for result in results {
            if let Some(job) = data.jobs.iter_mut().find(|j| j.id == result.job_id) {
                if result.is_restore {
                    job.last_restore = Some(now_ts);
                    // Disable once jobs after restore completes
                    if matches!(job.schedule, Schedule::Once { .. }) {
                        job.enabled = false;
                    }
                } else {
                    job.last_run = Some(now_ts);
                    job.last_status = Some(result.status);
                    job.last_error = result.error;

                    // Auto-disable once jobs only if no restore pending
                    if matches!(job.schedule, Schedule::Once { .. }) && job.restore.is_none() {
                        job.enabled = false;
                    }
                }
            }
        }
        save(&data);
    }
}

fn action_body(body: &Option<Value>) -> Vec<u8> {
    match body {
        Some(v) => serde_json::to_vec(v).unwrap_or_default(),
        None => Vec::new(),
    }
}

// --- HTTP handlers ---

pub fn jobs_list(state: &AppState) -> (u16, Value) {
    let data = state.scheduler.data.lock().unwrap();
    (200, json!({"ok": true, "data": data.jobs}))
}

#[derive(Deserialize)]
struct CreateJobRequest {
    name: String,
    schedule: Schedule,
    action: Action,
    restore: Option<Restore>,
}

pub fn jobs_create(state: &AppState, body: &[u8]) -> (u16, Value) {
    let req: CreateJobRequest = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    if let Err(e) = validate_job(&req.action, &req.schedule, &req.restore) {
        return (400, json!({"ok": false, "error": e}));
    }

    let (now_ts, _, _, _) = now_local();
    let mut data = state.scheduler.data.lock().unwrap();

    let next_id = data.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;

    let job = Job {
        id: next_id,
        name: req.name,
        enabled: true,
        schedule: req.schedule,
        action: req.action,
        restore: req.restore,
        last_run: None,
        last_status: None,
        last_error: None,
        last_restore: None,
        created_at: now_ts,
    };

    let result = json!({"ok": true, "data": job});
    data.jobs.push(job);
    save(&data);
    state.scheduler.changed.notify_all();
    (201, result)
}

#[derive(Deserialize)]
struct UpdateJobRequest {
    id: u32,
    name: String,
    enabled: bool,
    schedule: Schedule,
    action: Action,
    restore: Option<Restore>,
}

pub fn jobs_update(state: &AppState, body: &[u8]) -> (u16, Value) {
    let req: UpdateJobRequest = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return (400, json!({"ok": false, "error": format!("invalid JSON: {e}")})),
    };

    if let Err(e) = validate_job(&req.action, &req.schedule, &req.restore) {
        return (400, json!({"ok": false, "error": e}));
    }

    let mut data = state.scheduler.data.lock().unwrap();
    let job = match data.jobs.iter_mut().find(|j| j.id == req.id) {
        Some(j) => j,
        None => return (404, json!({"ok": false, "error": "job not found"})),
    };

    job.name = req.name;
    job.enabled = req.enabled;
    job.schedule = req.schedule;
    job.action = req.action;
    job.restore = req.restore;

    let result = json!({"ok": true, "data": job.clone()});
    save(&data);
    state.scheduler.changed.notify_all();
    (200, result)
}

pub fn jobs_delete(state: &AppState, body: &[u8]) -> (u16, Value) {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, json!({"ok": false, "error": "invalid JSON"})),
    };
    let id = match parsed["id"].as_u64() {
        Some(id) => id as u32,
        None => return (400, json!({"ok": false, "error": "missing 'id' field"})),
    };

    let mut data = state.scheduler.data.lock().unwrap();
    let len_before = data.jobs.len();
    data.jobs.retain(|j| j.id != id);

    if data.jobs.len() == len_before {
        return (404, json!({"ok": false, "error": "job not found"}));
    }

    save(&data);
    state.scheduler.changed.notify_all();
    (200, json!({"ok": true}))
}

pub fn jobs_toggle(state: &AppState, body: &[u8]) -> (u16, Value) {
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

    let mut data = state.scheduler.data.lock().unwrap();
    let job = match data.jobs.iter_mut().find(|j| j.id == id) {
        Some(j) => j,
        None => return (404, json!({"ok": false, "error": "job not found"})),
    };

    job.enabled = enabled;
    let result = json!({"ok": true, "data": job.clone()});
    save(&data);
    state.scheduler.changed.notify_all();
    (200, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn job(id: u32, enabled: bool) -> Job {
        Job {
            id,
            name: format!("job-{id}"),
            enabled,
            schedule: Schedule::Once { at: 1_700_000_000 },
            action: Action {
                method: "GET".into(),
                path: "/api/device".into(),
                body: None,
            },
            restore: None,
            last_run: None,
            last_status: None,
            last_error: None,
            last_restore: None,
            created_at: 1_700_000_000,
        }
    }

    fn scheduler_with(jobs: Vec<Job>) -> Scheduler {
        Scheduler {
            data: Mutex::new(SchedulerData { jobs }),
            changed: Condvar::new(),
        }
    }

    #[test]
    fn parse_method_maps_to_route_methods() {
        use crate::server::Method;
        assert_eq!(parse_method("GET"), Some(Method::Get));
        assert_eq!(parse_method("POST"), Some(Method::Post));
        assert_eq!(parse_method("PUT"), Some(Method::Put));
        assert_eq!(parse_method("DELETE"), Some(Method::Delete));
        assert_eq!(parse_method("get"), None);
        assert_eq!(parse_method("HEAD"), None);
    }

    #[test]
    fn next_tick_delay_lands_one_second_after_the_minute_boundary() {
        // Two full minutes starting on a boundary (1_700_000_040 % 60 == 0).
        let base: i64 = 1_700_000_040;
        for now in base..base + 120 {
            let delay = next_tick_delay(now).as_secs() as i64;
            assert!((1..=61).contains(&delay), "now={now} delay={delay}");
            assert_eq!((now + delay).rem_euclid(60), 1, "now={now} delay={delay}");
        }
    }

    #[test]
    fn next_tick_delay_spot_values() {
        let base: i64 = 1_700_000_040;
        assert_eq!(next_tick_delay(base).as_secs(), 61); // :00 -> next :01
        assert_eq!(next_tick_delay(base + 1).as_secs(), 60); // :01 -> next :01
        assert_eq!(next_tick_delay(base + 30).as_secs(), 31); // :30 -> :01
        assert_eq!(next_tick_delay(base + 59).as_secs(), 2); // :59 -> :01
    }

    #[test]
    fn next_tick_delay_handles_negative_timestamps() {
        for now in -130i64..=0 {
            let delay = next_tick_delay(now).as_secs() as i64;
            assert!((1..=61).contains(&delay), "now={now} delay={delay}");
            assert_eq!((now + delay).rem_euclid(60), 1, "now={now} delay={delay}");
        }
    }

    #[test]
    fn has_enabled_job_ignores_disabled_jobs() {
        assert!(!has_enabled_job(&SchedulerData { jobs: vec![] }));
        assert!(!has_enabled_job(&SchedulerData {
            jobs: vec![job(1, false), job(2, false)]
        }));
        assert!(has_enabled_job(&SchedulerData {
            jobs: vec![job(1, false), job(2, true)]
        }));
    }

    #[test]
    fn wait_for_enabled_job_returns_immediately_when_a_job_is_enabled() {
        let scheduler = scheduler_with(vec![job(1, true)]);
        scheduler.wait_for_enabled_job();
    }

    #[test]
    fn wait_for_enabled_job_parks_until_notified() {
        let scheduler = Arc::new(scheduler_with(vec![job(1, false)]));
        let (tx, rx) = mpsc::channel();
        let worker = {
            let scheduler = Arc::clone(&scheduler);
            std::thread::spawn(move || {
                scheduler.wait_for_enabled_job();
                tx.send(()).unwrap();
            })
        };

        // Still parked: nothing is enabled, so the worker cannot have returned.
        std::thread::sleep(Duration::from_millis(50));
        assert!(rx.try_recv().is_err());

        // Enable a job and notify, exactly what the handlers do after save().
        scheduler.data.lock().unwrap().jobs[0].enabled = true;
        scheduler.changed.notify_all();
        rx.recv_timeout(Duration::from_secs(5))
            .expect("scheduler thread should resume after notify_all");
        worker.join().unwrap();
    }
}
