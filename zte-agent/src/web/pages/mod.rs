//! The renderers that are not tables: dashboard cards, the dashboard's
//! detail sheets, signal panels, SMS and its forwarding, the Tools
//! screens, login and logout, operation progress. A
//! page that shows device data reads it in one gate task, through the same
//! topic readers as the live hub, and offers each read to the hub so a live
//! stream opened right after needs no second fork.

pub mod apn;
pub mod at;
pub mod automations;
pub mod bandlock;
pub mod cell;
pub mod confirm;
pub mod dashboard;
pub mod devctl;
pub mod dns;
pub mod esim;
pub mod esim_add;
pub mod firewall;
pub mod forward;
pub mod https;
pub mod lan;
pub mod login;
pub mod mobile;
pub mod netcfg;
pub mod op;
pub mod processes;
pub mod services;
pub mod sheets;
pub mod signal;
pub mod sim;
pub mod sms;
pub mod speedtest;
pub mod telemetry;
pub mod tools;
pub mod usb;
pub mod vpnqos;
pub mod wifi;
pub mod wlan;

use std::time::Instant;

use serde_json::Value;

use crate::boottime::{BootClock, Clock};
use crate::events::{readers, Topic};
use crate::handlers::AppState;
use crate::server::{self, Method};
use crate::web::render::Views;

/// A change's device call (an API route, in-process): its `data`, or its
/// status and error. A test may stand in for the device (`device`).
pub fn call(app: &AppState, method: Method, path: &str, body: &Value) -> Result<Value, (u16, String)> {
    #[cfg(test)]
    if let Some(r) = device::call(app, path, body) {
        return r;
    }
    crate::web::forward::data(server::route(&method, path, app, body.to_string().as_bytes()))
}

/// `GET` of an API route, in-process (blocking): its `data`, or its
/// error. A test may answer it (`device::Device::gets`).
pub fn api_get(app: &AppState, path: &str) -> Result<Value, String> {
    #[cfg(test)]
    if let Some(r) = device::get(app, path) {
        return r;
    }
    let (status, mut v) = server::route(&Method::Get, path, app, b"");
    if status < 400 {
        Ok(v["data"].take())
    } else {
        Err(v["error"].as_str().unwrap_or("error").to_owned())
    }
}

/// Read every topic of `topics` (blocking: gate tasks only) and return the
/// hub's resulting view of them.
pub fn read_topics(app: &AppState, topics: &[Topic]) -> Views {
    let hub = &app.hub;
    #[cfg(test)]
    if let Some(v) = fake::get(app) {
        for &t in topics {
            let out = crate::events::hub::ReadOut { value: Some(v.clone()), error: None };
            hub.offer(t, out, Instant::now());
        }
        return Views::new(BootClock.now(), topics.iter().map(|&t| (t, hub.peek(t))).collect());
    }
    for &t in topics {
        let started = Instant::now();
        if let Some(out) = readers::read(t, app, hub) {
            hub.offer(t, out, started);
        }
    }
    Views::new(BootClock.now(), topics.iter().map(|&t| (t, hub.peek(t))).collect())
}

/// Test only: a device for one `AppState`'s changes made through `call`
/// (each call recorded; the answer by path, or by "<path>#<n>" for the
/// n-th call to it, else `{"ok":true,"data":{}}`; optionally slow). Without one, calls reach the agent's handlers.
#[cfg(test)]
pub mod device {
    use std::sync::{Mutex, PoisonError};
    use std::time::Duration;

    use serde_json::{json, Value};

    use crate::handlers::AppState;

    #[derive(Clone, Default)]
    pub struct Device {
        pub replies: Vec<(&'static str, (u16, Value))>,
        pub calls: Vec<(String, Value)>,
        pub wait: Option<Duration>,
        /// Answers to page-load reads (`api_get`) by path: `data` or an
        /// error. Not recorded as calls.
        pub gets: Vec<(&'static str, Result<Value, String>)>,
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

    /// The calls made so far.
    pub fn calls(app: &AppState) -> Vec<(String, Value)> {
        let all = DEVICES.lock().unwrap_or_else(PoisonError::into_inner);
        all.iter().find(|(k, _)| *k == key(app)).map(|(_, d)| d.calls.clone()).unwrap_or_default()
    }

    /// A page-load read of `path`, if the test answers it.
    pub fn get(app: &AppState, path: &str) -> Option<Result<Value, String>> {
        let all = DEVICES.lock().unwrap_or_else(PoisonError::into_inner);
        let d = &all.iter().find(|(k, _)| *k == key(app))?.1;
        d.gets.iter().find(|(p, _)| *p == path).map(|(_, r)| r.clone())
    }

    pub fn call(app: &AppState, path: &str, body: &Value) -> Option<Result<Value, (u16, String)>> {
        let (reply, wait) = {
            let mut all = DEVICES.lock().unwrap_or_else(PoisonError::into_inner);
            let d = &mut all.iter_mut().find(|(k, _)| *k == key(app))?.1;
            d.calls.push((path.to_owned(), body.clone()));
            // "<path>#<n>": the n-th call to that path only.
            let nth = format!("{path}#{}", d.calls.iter().filter(|(c, _)| c == path).count());
            let reply = d.replies.iter().find(|(p, _)| *p == nth).or_else(|| d.replies.iter().find(|(p, _)| *p == path)).map(|(_, r)| r.clone());
            (reply.unwrap_or((200, json!({"ok": true, "data": {}}))), d.wait)
        };
        if let Some(w) = wait {
            std::thread::sleep(w);
        }
        Some(crate::web::forward::data(reply))
    }
}

/// Test only: a value that page loads of one `AppState` read instead of
/// the device (every topic, the Wi-Fi and guest status), so a test can
/// send hostile data through the real routes. Kept per `AppState`, so tests
/// running in parallel never see each other's; dropping the guard ends it.
#[cfg(test)]
pub mod fake {
    use std::sync::{Mutex, PoisonError};

    use serde_json::Value;

    use crate::handlers::AppState;

    static READS: Mutex<Vec<(usize, Value)>> = Mutex::new(Vec::new());

    fn key(app: &AppState) -> usize {
        app as *const AppState as usize
    }

    pub struct Guard(usize);

    impl Drop for Guard {
        fn drop(&mut self) {
            READS.lock().unwrap_or_else(PoisonError::into_inner).retain(|(k, _)| *k != self.0);
        }
    }

    #[must_use]
    pub fn set(app: &AppState, v: Value) -> Guard {
        let mut reads = READS.lock().unwrap_or_else(PoisonError::into_inner);
        reads.retain(|(k, _)| *k != key(app));
        reads.push((key(app), v));
        Guard(key(app))
    }

    pub fn get(app: &AppState) -> Option<Value> {
        READS.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(k, _)| *k == key(app)).map(|(_, v)| v.clone())
    }
}
