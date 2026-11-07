//! Console access to the loopback core controller: routing mode, proxy-group
//! choice and delay tests. Only these fixed operations exist. The controller
//! secret never leaves the router, URLs are built from validated segments, the
//! delay target is a constant, and nothing here can reload or restart the core.

use std::time::Duration;

use serde_json::{json, Map, Value};

use super::{Mihomo, CONTROLLER};

const MAX_BODY: u64 = 4 * 1024 * 1024;
const MAX_GROUPS: usize = 64;
const MAX_MEMBERS: usize = 2000;
const MAX_NODES: usize = 4000;
const MAX_NAME: usize = 256;
const DELAY_URL: &str = "https://www.gstatic.com/generate_204";
const DELAY_MS: u64 = 5000;
const MODES: [&str; 3] = ["rule", "global", "direct"];

type Failure = (u16, String);

fn off() -> Failure {
    (409, "Clash is off".into())
}

fn finish(result: Result<Value, Failure>) -> (u16, Value) {
    match result {
        Ok(data) => (200, json!({"ok": true, "data": data})),
        Err((status, error)) => (status, json!({"ok": false, "error": error})),
    }
}

/// One controller request. `Ok(None)` is a success without a body (204).
fn call(
    base: &str,
    secret: &str,
    method: &str,
    segments: &[&str],
    query: &[(&str, &str)],
    body: Option<&Value>,
    wait: Duration,
) -> Result<Option<Value>, String> {
    let mut url = reqwest::Url::parse(&format!("http://{base}/")).map_err(|_| "Invalid controller address")?;
    url.path_segments_mut()
        .map_err(|_| "Invalid controller address")?
        .clear()
        .extend(segments);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    let agent = ureq::Agent::config_builder()
        .proxy(None)
        .http_status_as_error(false)
        .timeout_global(Some(wait))
        .build()
        .new_agent();
    let auth = format!("Bearer {secret}");
    let sent = match (method, body) {
        ("GET", None) => agent.get(url.as_str()).header("Authorization", &auth).call(),
        ("PUT", Some(body)) => agent.put(url.as_str()).header("Authorization", &auth).send_json(body),
        ("PATCH", Some(body)) => agent.patch(url.as_str()).header("Authorization", &auth).send_json(body),
        _ => return Err("Unsupported controller request".into()),
    };
    let mut response = sent.map_err(|_| "Mihomo did not answer".to_owned())?;
    let status = response.status().as_u16();
    let value = response
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_json::<Value>()
        .ok();
    if (200..300).contains(&status) {
        return Ok(value);
    }
    let detail = value
        .as_ref()
        .and_then(|v| v["message"].as_str())
        .map(|m| m.chars().filter(|c| !c.is_control()).take(160).collect::<String>())
        .filter(|m| !m.is_empty());
    Err(detail.unwrap_or_else(|| format!("Mihomo refused the request (HTTP {status})")))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME && !name.chars().any(char::is_control)
}

fn parse(body: &[u8]) -> Result<Value, Failure> {
    serde_json::from_slice(body).map_err(|_| (400, "Invalid request".to_owned()))
}

fn name_field(value: &Value, key: &str) -> Result<String, Failure> {
    value[key]
        .as_str()
        .filter(|name| valid_name(name))
        .map(str::to_owned)
        .ok_or_else(|| (400, format!("Invalid {key}")))
}

/// Groups in the order the configuration declares them (the core lists them in
/// `GLOBAL`), each with its members; every other proxy is a node with its last
/// measured delay. Bounded so a huge subscription cannot produce a huge reply.
fn summarize(mode: &str, listing: &Value) -> Value {
    let empty = Map::new();
    let proxies = listing["proxies"].as_object().unwrap_or(&empty);
    let group = |name: &str, proxy: &Value| -> Option<Value> {
        let members = proxy["all"].as_array()?;
        Some(json!({
            "name": name,
            "type": proxy["type"].as_str().unwrap_or("Unknown"),
            "now": proxy["now"].as_str(),
            "selectable": proxy["type"].as_str() == Some("Selector"),
            "all": members.iter().filter_map(Value::as_str).take(MAX_MEMBERS).collect::<Vec<_>>(),
        }))
    };
    let mut order: Vec<&str> = proxies
        .get("GLOBAL")
        .and_then(|g| g["all"].as_array())
        .map(|all| all.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    order.retain(|name| proxies.get(*name).is_some_and(|p| p["all"].is_array()));
    for name in proxies.keys() {
        if name != "GLOBAL" && !order.contains(&name.as_str()) && proxies[name]["all"].is_array() {
            order.push(name);
        }
    }
    if proxies.get("GLOBAL").is_some_and(|p| p["all"].is_array()) {
        order.push("GLOBAL");
    }
    let groups: Vec<Value> = order
        .into_iter()
        .filter_map(|name| group(name, &proxies[name]))
        .take(MAX_GROUPS)
        .collect();
    let mut nodes = Map::new();
    for (name, proxy) in proxies.iter().filter(|(_, p)| !p["all"].is_array()).take(MAX_NODES) {
        let delay = proxy["history"]
            .as_array()
            .and_then(|history| history.last())
            .and_then(|last| last["delay"].as_u64());
        nodes.insert(name.clone(), json!({"type": proxy["type"].as_str().unwrap_or("Unknown"), "delay": delay}));
    }
    json!({"mode": mode, "groups": groups, "nodes": nodes})
}

impl Mihomo {
    fn controller(
        &self,
        method: &str,
        segments: &[&str],
        query: &[(&str, &str)],
        body: Option<&Value>,
        wait: Duration,
    ) -> Result<Option<Value>, Failure> {
        let secret = self.secret().ok_or_else(off)?;
        call(CONTROLLER, &secret, method, segments, query, body, wait).map_err(|error| (503, error))
    }

    fn proxy_listing(&self) -> Result<Value, Failure> {
        let wait = Duration::from_secs(3);
        let configs = self.controller("GET", &["configs"], &[], None, wait)?;
        let listing = self.controller("GET", &["proxies"], &[], None, wait)?;
        let mode = configs
            .as_ref()
            .and_then(|c| c["mode"].as_str())
            .filter(|mode| MODES.contains(mode))
            .unwrap_or("rule");
        Ok(summarize(mode, &listing.ok_or((503, "Mihomo returned no proxies".to_owned()))?))
    }
}

pub fn proxies(state: &crate::handlers::AppState) -> (u16, Value) {
    finish(state.mihomo.proxy_listing())
}

pub fn select(state: &crate::handlers::AppState, body: &[u8]) -> (u16, Value) {
    finish((|| {
        let request = parse(body)?;
        let (group, name) = (name_field(&request, "group")?, name_field(&request, "name")?);
        state.mihomo.controller("PUT", &["proxies", &group], &[], Some(&json!({"name": name})), Duration::from_secs(3))?;
        Ok(json!({"group": group, "now": name}))
    })())
}

/// A failed or timed-out test is an answer (`delay: 0`, as in the core's own
/// history), not an API error. `null` only ever means "never tested".
pub fn delay(state: &crate::handlers::AppState, body: &[u8]) -> (u16, Value) {
    finish((|| {
        let name = name_field(&parse(body)?, "name")?;
        let timeout = DELAY_MS.to_string();
        match state.mihomo.controller(
            "GET",
            &["proxies", &name, "delay"],
            &[("url", DELAY_URL), ("timeout", &timeout)],
            None,
            Duration::from_millis(DELAY_MS + 3000),
        ) {
            Ok(value) => Ok(json!({"name": name, "delay": value.and_then(|v| v["delay"].as_u64()).unwrap_or(0)})),
            Err((409, error)) => Err((409, error)),
            Err((_, error)) => Ok(json!({"name": name, "delay": 0, "error": error})),
        }
    })())
}

pub fn mode(state: &crate::handlers::AppState, body: &[u8]) -> (u16, Value) {
    finish((|| {
        let request = parse(body)?;
        let mode = request["mode"]
            .as_str()
            .filter(|mode| MODES.contains(mode))
            .ok_or((400, "Choose rule, global or direct".to_owned()))?;
        state.mihomo.controller("PATCH", &["configs"], &[], Some(&json!({"mode": mode})), Duration::from_secs(3))?;
        Ok(json!({"mode": mode}))
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn listing() -> Value {
        json!({"proxies": {
            "GLOBAL": {"type": "Selector", "now": "Auto", "all": ["Auto", "Manual", "DIRECT"]},
            "Manual": {"type": "Selector", "now": "香港 01", "all": ["香港 01", "US 02", "DIRECT"]},
            "Auto": {"type": "URLTest", "now": "US 02", "all": ["香港 01", "US 02"]},
            "香港 01": {"type": "Trojan", "history": [{"delay": 0}, {"delay": 88}]},
            "JP 03": {"type": "Vless", "history": [{"delay": 120}, {"delay": 0}]},
            "US 02": {"type": "Vmess", "history": []},
            "DIRECT": {"type": "Direct"},
        }})
    }

    #[test]
    fn groups_follow_the_declared_order_and_nodes_carry_their_last_delay() {
        let value = summarize("rule", &listing());
        let names: Vec<_> = value["groups"].as_array().unwrap().iter().map(|g| g["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Auto", "Manual", "GLOBAL"], "GLOBAL's own list orders the rest; GLOBAL is last");
        let manual = &value["groups"][1];
        assert_eq!((manual["selectable"].as_bool(), manual["now"].as_str()), (Some(true), Some("香港 01")));
        assert_eq!(value["groups"][0]["selectable"], false, "URLTest chooses by itself");
        assert_eq!(value["nodes"]["香港 01"]["delay"], 88);
        assert!(value["nodes"]["US 02"]["delay"].is_null() && value["nodes"]["DIRECT"]["delay"].is_null(), "never tested is null");
        assert_eq!(value["nodes"]["JP 03"]["delay"], 0, "a failed last test is 0, not 'never tested'");
        assert!(value["nodes"].get("GLOBAL").is_none() && value["nodes"].get("Auto").is_none());
        assert_eq!(value["mode"], "rule");
    }

    #[test]
    fn a_huge_subscription_gives_a_bounded_reply() {
        let mut proxies = Map::new();
        let names: Vec<String> = (0..MAX_NODES + 500).map(|n| format!("node-{n}")).collect();
        for name in &names {
            proxies.insert(name.clone(), json!({"type": "Vmess"}));
        }
        proxies.insert("Big".into(), json!({"type": "Selector", "now": "node-0", "all": names}));
        for n in 0..MAX_GROUPS + 10 {
            proxies.insert(format!("g{n}"), json!({"type": "Selector", "now": "node-0", "all": ["node-0"]}));
        }
        let value = summarize("global", &json!({"proxies": proxies}));
        assert_eq!(value["groups"].as_array().unwrap().len(), MAX_GROUPS);
        assert!(value["nodes"].as_object().unwrap().len() <= MAX_NODES);
        assert!(summarize("rule", &json!({"proxies": proxies}))["groups"].as_array().unwrap().iter().all(|g| g["all"].as_array().unwrap().len() <= MAX_MEMBERS));
        assert_eq!(summarize("rule", &json!({}))["groups"], json!([]));
    }

    #[test]
    fn names_are_validated_before_they_reach_the_controller() {
        for bad in ["", "a\nb", "tab\there", &"x".repeat(MAX_NAME + 1)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        for good in ["香港 01 🚀", "a/b?c#d%20", "DIRECT"] {
            assert!(valid_name(good), "{good:?}");
        }
        assert_eq!(name_field(&json!({"group": 7}), "group").unwrap_err().0, 400);
    }

    /// A loopback stand-in for the core controller: one canned reply per path.
    fn controller(replies: Vec<(&'static str, u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut raw = Vec::new();
                let mut chunk = [0u8; 4096];
                let head = loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break String::new();
                    }
                    raw.extend_from_slice(&chunk[..n]);
                    if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&raw[..end]).into_owned();
                        let length = head
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                            .unwrap_or(0);
                        while raw.len() < end + 4 + length {
                            let n = stream.read(&mut chunk).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            raw.extend_from_slice(&chunk[..n]);
                        }
                        break format!("{head}\r\n\r\n{}", String::from_utf8_lossy(&raw[end + 4..]));
                    }
                };
                let target = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(head);
                let (status, body) = replies
                    .iter()
                    .find(|(path, _, _)| target.starts_with(path))
                    .map_or((404, "{}"), |(_, s, b)| (*s, *b));
                let reply = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        (address, seen)
    }

    #[test]
    fn requests_are_authenticated_encoded_and_errors_are_bounded() {
        let (base, seen) = controller(vec![
            ("/proxies/%E9%A6%99%E6%B8%AF%20%2F01", 204, ""),
            ("/proxies/node/delay?url=https%3A%2F%2Fwww.gstatic.com%2Fgenerate_204&timeout=5000", 200, r#"{"delay":42}"#),
            ("/configs", 400, r#"{"message":"bad\u0007 mode"}"#),
        ]);
        let wait = Duration::from_secs(3);
        let none = call(&base, "core-secret", "PUT", &["proxies", "香港 /01"], &[], Some(&json!({"name": "US 02"})), wait).unwrap();
        assert!(none.is_none());
        let delay = call(&base, "core-secret", "GET", &["proxies", "node", "delay"], &[("url", DELAY_URL), ("timeout", "5000")], None, wait).unwrap();
        assert_eq!(delay.unwrap()["delay"], 42);
        let error = call(&base, "core-secret", "PATCH", &["configs"], &[], Some(&json!({"mode": "nope"})), wait).unwrap_err();
        assert_eq!(error, "bad mode", "control characters are dropped from the core's message");
        let requests = seen.lock().unwrap().clone();
        assert!(requests.iter().all(|r| r.to_ascii_lowercase().contains("authorization: bearer core-secret")));
        assert!(requests[0].starts_with("PUT /proxies/%E9%A6%99%E6%B8%AF%20%2F01 HTTP/1.1"), "{}", requests[0]);
        let sent = requests[0].split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or_default();
        assert_eq!(serde_json::from_str::<Value>(sent).unwrap(), json!({"name": "US 02"}));
        assert!(call(&base, "k", "DELETE", &["connections"], &[], None, wait).is_err(), "only the fixed verbs exist");
        assert!(call("127.0.0.1:9", "k", "GET", &["configs"], &[], None, Duration::from_millis(500)).is_err());
    }

    #[test]
    fn every_operation_refuses_while_clash_is_off() {
        let state = crate::handlers::AppState::new();
        for (status, body) in [
            proxies(&state),
            select(&state, br#"{"group":"Manual","name":"DIRECT"}"#),
            delay(&state, br#"{"name":"DIRECT"}"#),
            mode(&state, br#"{"mode":"global"}"#),
        ] {
            assert_eq!((status, body["error"].as_str()), (409, Some("Clash is off")));
        }
        assert_eq!(select(&state, b"nope").0, 400);
        assert_eq!(select(&state, br#"{"group":"","name":"x"}"#).0, 400);
        assert_eq!(mode(&state, br#"{"mode":"turbo"}"#).0, 400);
    }
}
