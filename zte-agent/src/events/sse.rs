//! SSE frames, encoded here: every frame the agent sends is a fixed shape
//! (an event name and `data:` lines, or a comment), so axum's general
//! `Sse`/`Event` builder is not linked. JSON from serde never contains a raw
//! CR or LF, so each JSON event is exactly one `data:` line.

use std::convert::Infallible;
use std::fmt::Write as _;
use std::time::Duration;

use axum::body::Body;
use axum::http::header::{HeaderValue, CACHE_CONTROL, CONTENT_TYPE};
use axum::response::Response;
use futures_util::Stream;

use super::hub::{SlotView, TopicAge};
use super::stream::{ByeReason, Emitter};
use super::{Topic, TopicSet};
use crate::boottime::BootInstant;

/// One encoded frame, including the blank line that ends it.
pub type Frame = String;

/// Wire format version in `hello`.
const VERSION: u32 = 1;

/// `200 text/event-stream` (`Cache-Control: no-cache`) streaming `frames`;
/// each frame goes out as soon as it is produced.
pub fn response(frames: impl Stream<Item = Result<Frame, Infallible>> + Send + 'static) -> Response {
    let mut resp = Response::new(Body::from_stream(frames));
    let h = resp.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}

/// `event: <name>` and one `data:` line; neither may contain CR or LF.
pub(crate) fn frame(name: &str, data: &str) -> Frame {
    let mut f = String::with_capacity(name.len() + data.len() + 16);
    f.push_str("event: ");
    f.push_str(name);
    f.push_str("\ndata: ");
    f.push_str(data);
    f.push_str("\n\n");
    f
}

/// The first frame: what this stream carries. `retry: 3000` is the
/// reconnect delay for a network error (never after a `bye`).
pub fn hello(agent_instance: u64, topics: TopicSet, cadence: &[(Topic, Duration)], keepalive: Duration) -> Frame {
    let mut d = format!(r#"{{"v":{VERSION},"agent_instance":{agent_instance},"topics":["#);
    for (i, t) in topics.iter().enumerate() {
        d.push_str(if i == 0 { "\"" } else { ",\"" });
        d.push_str(t.name());
        d.push('"');
    }
    d.push_str(r#"],"cadence_ms":{"#);
    for (i, (t, c)) in cadence.iter().enumerate() {
        d.push_str(if i == 0 { "\"" } else { ",\"" });
        d.push_str(t.name());
        let _ = write!(d, "\":{}", c.as_millis() as u64);
    }
    let _ = write!(d, r#"}},"keepalive_ms":{}}}"#, keepalive.as_millis() as u64);
    "retry: 3000\n".to_owned() + &frame("hello", &d)
}

/// `event: <topic>` with `{"seq","age_ms","error","data"}`; `data` is the
/// GET route's `data`, serialised straight from the shared value.
pub fn json_event(t: Topic, s: &SlotView, now: BootInstant) -> Frame {
    let age = now.saturating_duration_since(s.at).as_millis() as u64;
    let mut d = format!(r#"{{"seq":{},"age_ms":{age},"error":"#, s.version).into_bytes();
    let _ = serde_json::to_writer(&mut d, &s.error.as_deref());
    d.extend_from_slice(br#","data":"#);
    let _ = serde_json::to_writer(&mut d, &s.value.as_deref());
    d.push(b'}');
    frame(t.name(), &String::from_utf8(d).unwrap_or_default())
}

/// `: keep-alive` (a comment; clients ignore it).
pub fn keepalive() -> Frame {
    ": keep-alive\n\n".into()
}

pub fn bye(reason: ByeReason) -> Frame {
    frame("bye", &format!(r#"{{"reason":"{}"}}"#, reason.name()))
}

/// A Datastar `datastar-patch-elements` frame: every line of `html` becomes
/// an `elements` line (morphed by id). Empty lines are dropped, and CR and
/// LF only ever end a line, so no line can be split elsewhere.
pub fn ds_patch_elements(html: &str) -> Frame {
    let mut f = String::with_capacity(html.len() + 64);
    f.push_str("event: datastar-patch-elements\n");
    for line in html.split(['\n', '\r']).filter(|l| !l.is_empty()) {
        f.push_str("data: elements ");
        f.push_str(line);
        f.push('\n');
    }
    f.push('\n');
    f
}

/// `GET /api/events`: hello and the snapshot, then changes, a keep-alive
/// comment per tick, and `bye`.
pub struct JsonEmitter {
    hello: Option<Frame>,
}

impl JsonEmitter {
    pub fn new(hello: Frame) -> Self {
        JsonEmitter { hello: Some(hello) }
    }
}

impl Emitter for JsonEmitter {
    fn open(&mut self, snapshot: Vec<(Topic, SlotView)>, now: BootInstant) -> Vec<Frame> {
        let mut frames: Vec<Frame> = self.hello.take().into_iter().collect();
        frames.append(&mut self.changes(snapshot, now));
        frames
    }

    fn changes(&mut self, changed: Vec<(Topic, SlotView)>, now: BootInstant) -> Vec<Frame> {
        changed.iter().map(|(t, s)| json_event(*t, s, now)).collect()
    }

    fn tick(&mut self, _now: BootInstant, _ages: &[TopicAge]) -> Vec<Frame> {
        vec![keepalive()]
    }

    /// After `hello` even if the stream ends at open (the session expired
    /// between the request's check and the first poll).
    fn bye(&mut self, reason: ByeReason) -> Vec<Frame> {
        self.hello.take().into_iter().chain([bye(reason)]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::Arc;

    /// The exact bytes on the wire for `frames`, through `response`.
    async fn wire(frames: Vec<Frame>) -> String {
        let resp = response(futures_util::stream::iter(frames.into_iter().map(Ok)));
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        assert_eq!(resp.headers()["cache-control"], "no-cache");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn view(version: u64, at_ms: u64, value: Option<Value>, error: Option<&str>) -> SlotView {
        let at = BootInstant::from_duration(Duration::from_millis(at_ms));
        SlotView {
            version,
            at,
            value_at: at,
            value: value.map(Arc::new),
            error: error.map(Arc::from),
        }
    }

    #[tokio::test]
    async fn hello_frame() {
        let topics = [Topic::Traffic, Topic::Signal, Topic::Sms].into_iter().collect();
        let cadence = [(Topic::Signal, Duration::from_secs(2)), (Topic::Traffic, Duration::from_secs(1))];
        let s = wire(vec![hello(1729500000000, topics, &cadence, Duration::from_secs(15))]).await;
        assert_eq!(
            s,
            "retry: 3000\nevent: hello\ndata: {\"v\":1,\"agent_instance\":1729500000000,\
             \"topics\":[\"signal\",\"traffic\",\"sms\"],\"cadence_ms\":{\"signal\":2000,\"traffic\":1000},\
             \"keepalive_ms\":15000}\n\n"
        );
        let data: Value = serde_json::from_str(s.lines().nth(2).unwrap().strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(data["keepalive_ms"], 15000);
        assert_eq!(data["v"], VERSION);
        // No sampled topic: empty cadence object, still valid JSON.
        let h = hello(7, [Topic::Sms].into_iter().collect(), &[], Duration::from_secs(15));
        let data: Value = serde_json::from_str(h.lines().nth(2).unwrap().strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(data, json!({"v": 1, "agent_instance": 7, "topics": ["sms"], "cadence_ms": {}, "keepalive_ms": 15000}));
    }

    #[tokio::test]
    async fn json_event_is_one_data_line() {
        let now = BootInstant::from_duration(Duration::from_millis(10_120));
        let v = json!({"note": "line1\nline2\r", "n": 1});
        let s = wire(vec![json_event(Topic::Signal, &view(1834, 10_000, Some(v.clone()), None), now)]).await;
        assert_eq!(
            s,
            "event: signal\ndata: {\"seq\":1834,\"age_ms\":120,\"error\":null,\
             \"data\":{\"n\":1,\"note\":\"line1\\nline2\\r\"}}\n\n"
        );
        let s = wire(vec![json_event(Topic::Wan, &view(7, 0, None, Some("ubus \"timeout\"")), now)]).await;
        assert_eq!(s.matches("data: ").count(), 1);
        let data: Value = serde_json::from_str(s.lines().nth(1).unwrap().strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(data, json!({"seq": 7, "age_ms": 10_120, "error": "ubus \"timeout\"", "data": null}));
    }

    #[tokio::test]
    async fn keepalive_and_bye() {
        assert_eq!(wire(vec![keepalive()]).await, ": keep-alive\n\n");
        let all = [
            (ByeReason::TokenExpired, "token_expired"),
            (ByeReason::LoggedOut, "logged_out"),
            (ByeReason::Evicted, "evicted"),
            (ByeReason::Lifetime, "lifetime"),
            (ByeReason::Standby, "standby"),
        ];
        for (reason, name) in all {
            assert_eq!(wire(vec![bye(reason)]).await, format!("event: bye\ndata: {{\"reason\":\"{name}\"}}\n\n"));
        }
    }

    #[test]
    fn json_emitter_hello_always_first() {
        let topics = [Topic::Sms].into_iter().collect();
        let hello = || hello(7, topics, &[], Duration::from_secs(15));
        let mut em = JsonEmitter::new(hello());
        // Ended at open: hello, then bye.
        assert_eq!(em.bye(ByeReason::TokenExpired), [hello(), bye(ByeReason::TokenExpired)]);
        let mut em = JsonEmitter::new(hello());
        let now = BootInstant::from_duration(Duration::from_secs(1));
        assert_eq!(em.open(vec![], now), [hello()]);
        assert_eq!(em.bye(ByeReason::Standby), [bye(ByeReason::Standby)]);
    }

    #[tokio::test]
    async fn datastar_patch_lines() {
        let s = wire(vec![ds_patch_elements("<p id=\"a\">x\r\ny</p>")]).await;
        assert_eq!(s, "event: datastar-patch-elements\ndata: elements <p id=\"a\">x\ndata: elements y</p>\n\n");
        // Empty lines are dropped.
        let s = wire(vec![ds_patch_elements("\n<div id=\"b\">\n\n\r\n</div>\n")]).await;
        assert_eq!(s, "event: datastar-patch-elements\ndata: elements <div id=\"b\">\ndata: elements </div>\n\n");
    }

    /// The same bytes as axum's own `Sse`/`Event` encoder for these frames.
    #[tokio::test]
    async fn frames_match_axums_encoder() {
        use axum::response::sse::{Event, Sse};
        use axum::response::IntoResponse;
        let axum_bytes = |events: Vec<Event>| async move {
            let resp = Sse::new(futures_util::stream::iter(events.into_iter().map(Ok::<_, Infallible>))).into_response();
            let b = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            String::from_utf8(b.to_vec()).unwrap()
        };
        let d = r#"{"v":1}"#;
        let ours = "retry: 3000\n".to_owned() + &frame("hello", d);
        assert_eq!(ours, axum_bytes(vec![Event::default().retry(Duration::from_secs(3)).event("hello").data(d)]).await);
        assert_eq!(keepalive(), axum_bytes(vec![Event::default().comment("keep-alive")]).await);
        let html = "<p id=\"a\">x</p>\n<p id=\"b\">y</p>";
        let lines = "elements <p id=\"a\">x</p>\nelements <p id=\"b\">y</p>";
        let theirs = axum_bytes(vec![Event::default().event("datastar-patch-elements").data(lines)]).await;
        assert_eq!(ds_patch_elements(html), theirs);
    }
}
