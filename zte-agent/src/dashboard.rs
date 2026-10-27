//! Shared dashboard sampling and an authenticated, bounded SSE fan-out.
//!
//! Browsers receive a complete snapshot on every connection, followed by changed readings.
//! Slow readers retain only their previous snapshot; no unbounded per-browser queue is used.

use std::collections::BTreeMap;
#[cfg(test)]
use std::io::{self, Write};

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::handlers::{self, AppState};
use crate::{device_ext, modem_ext, network_ext, sms, wifi};

const MAX_STREAMS: usize = 8;
const HEARTBEAT: Duration = Duration::from_secs(10);
/// Firmware notifications accelerate sampling; periodic reads cover values with no event.
pub const EVENTS: &[&str] = &[
    "router_event_wan_connect_status",
    "network.interface",
    "BSP_CHARGER_EVENT",
    "BSP_POWERBANK_EVENT",
    "zwrt_card_state_event",
    "zwrt_servicestatus",
    "zwrt_wms_status_event",
    crate::sim_identity::EVENT,
];
const READINGS: &[(&str, u64)] = &[
    ("device", 15),
    ("wan", 10),
    ("wan6", 10),
    ("signal", 2),
    ("modem", 5),
    ("mobile", 5),
    ("charging", 10),
    ("speed", 2),
    ("battery", 10),
    ("wifi", 10),
    ("guest", 10),
    ("clients", 5),
    ("sms", 30),
    ("sms_capacity", 30),
];
type Readings = BTreeMap<String, Value>;

const TOPICS: &[&str] = &[
    "operators",
    "forwarding",
    "health",
    "radio",
    "speedtest",
    "terminal",
    "routing",
    "clients",
    "esim",
];
const EXTRA_READINGS: &[(&str, u64, usize)] = &[
    ("operators", 2, 0),
    ("forwarding", 2, 1),
    ("resources", 5, 2),
    ("neighbors", 5, 3),
    ("sweep", 2, 3),
    ("speedtest", 1, 4),
    ("at_port", 5, 5),
    ("lan_config", 10, 6),
    ("firewall_config", 10, 6),
    ("upnp_config", 10, 6),
    ("port_rules", 10, 6),
    ("qos_config", 10, 6),
    ("client_details", 2, 7),
    ("esim_status", 2, 8),
];

#[derive(Clone, Copy, Default)]
struct Interests {
    sms: bool,
    extras: u16,
}

impl From<bool> for Interests {
    fn from(sms: bool) -> Self {
        Self { sms, extras: 0 }
    }
}

impl Interests {
    fn from_url(url: &str) -> Self {
        let mut interests = Self::default();
        for part in url
            .split_once('?')
            .map_or("", |(_, query)| query)
            .split('&')
        {
            if part == "sms=1" {
                interests.sms = true;
            }
            if let Some(topics) = part.strip_prefix("topics=") {
                for topic in topics.split(',') {
                    if let Some(index) = TOPICS.iter().position(|name| *name == topic) {
                        interests.extras |= 1 << index;
                    }
                }
            }
        }
        interests
    }
    fn includes(self, name: &str) -> bool {
        (!is_sms(name) || self.sms)
            && extra_topic(name).is_none_or(|index| self.extras & (1 << index) != 0)
    }
}

fn extra_topic(name: &str) -> Option<usize> {
    EXTRA_READINGS
        .iter()
        .find(|(key, _, _)| *key == name)
        .map(|(_, _, topic)| *topic)
}

#[derive(Default)]
struct FeedState {
    clients: usize,
    sms_clients: usize,
    extra_clients: [usize; TOPICS.len()],
    refresh_version: u64,
    generation: u64,
    refresh_requested: bool,
    readings: Readings,
}

#[derive(Default)]
pub struct Dashboard {
    state: Mutex<FeedState>,
    changed: Condvar,
    live: tokio::sync::Notify,
}

struct Subscription {
    interests: Interests,
    feed: Arc<Dashboard>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut state = self.feed.state.lock().unwrap_or_else(|p| p.into_inner());
        state.clients -= 1;
        if self.interests.sms {
            state.sms_clients -= 1;
            if state.sms_clients == 0 {
                state.readings.retain(|key, _| !is_sms(key));
            }
        }
        for index in 0..TOPICS.len() {
            if self.interests.extras & (1 << index) != 0 {
                state.extra_clients[index] -= 1;
                if state.extra_clients[index] == 0 {
                    state
                        .readings
                        .retain(|key, _| extra_topic(key) != Some(index));
                }
            }
        }
        if state.clients == 0 {
            // The next viewer must not receive values sampled before an idle period.
            state.readings.clear();
        }
        self.feed.changed.notify_all();
    }
}

impl Dashboard {
    /// Local writes accelerate the shared feed without adding browser polling.
    pub fn request_refresh(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.refresh_requested = true;
        state.refresh_version = state.refresh_version.wrapping_add(1);
        self.changed.notify_all();
        self.live.notify_waiters();
    }

    fn subscribe(self: &Arc<Self>, interests: impl Into<Interests>) -> Option<Subscription> {
        let interests = interests.into();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.clients >= MAX_STREAMS {
            return None;
        }
        state.clients += 1;
        if interests.sms {
            state.sms_clients += 1;
        }
        for index in 0..TOPICS.len() {
            if interests.extras & (1 << index) != 0 {
                state.extra_clients[index] += 1;
            }
        }
        self.changed.notify_all();
        self.live.notify_waiters();
        Some(Subscription {
            interests,
            feed: Arc::clone(self),
        })
    }

    fn publish(&self, readings: Readings) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.clients == 0 {
            return;
        }
        let mut updated = false;
        for (key, value) in readings {
            if (is_sms(&key) && state.sms_clients == 0)
                || extra_topic(&key).is_some_and(|index| state.extra_clients[index] == 0)
            {
                continue;
            }
            if state.readings.get(&key) != Some(&value) {
                state.readings.insert(key, value);
                updated = true;
            }
        }
        if updated {
            state.generation = state.generation.wrapping_add(1);
            self.changed.notify_all();
            self.live.notify_waiters();
        }
    }

    #[cfg(test)]
    fn next(&self, generation: u64, interests: impl Into<Interests>) -> (u64, Readings) {
        let interests = interests.into();
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let (state, _) = self
            .changed
            .wait_timeout_while(state, HEARTBEAT, |state| {
                state.generation == generation || state.readings.is_empty()
            })
            .unwrap_or_else(|p| p.into_inner());
        let readings = state
            .readings
            .iter()
            .filter(|(key, _)| interests.includes(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        (state.generation, readings)
    }

    fn start_extra_collectors(self: &Arc<Self>, app: Arc<AppState>) {
        for (index, topic_name) in TOPICS.iter().enumerate() {
            let feed = Arc::clone(self);
            let app = Arc::clone(&app);
            crate::system::spawn_named(&format!("dashboard-{topic_name}"), move || {
                let mut sampled = BTreeMap::<&str, Instant>::new();
                let mut version = 0;
                loop {
                    let (active, refresh, missing) = {
                        let guard = feed.state.lock().unwrap_or_else(|p| p.into_inner());
                        (
                            guard.extra_clients[index] > 0,
                            guard.refresh_version,
                            EXTRA_READINGS.iter().any(|(name, _, topic)| {
                                *topic == index && !guard.readings.contains_key(*name)
                            }),
                        )
                    };
                    if refresh != version || missing {
                        sampled.clear();
                        version = refresh;
                    }
                    if active {
                        let mut updates = Readings::new();
                        for &(name, seconds, topic) in EXTRA_READINGS {
                            if topic != index
                                || sampled
                                    .get(name)
                                    .is_some_and(|at| at.elapsed() < Duration::from_secs(seconds))
                            {
                                continue;
                            }
                            updates.insert(name.into(), crate::dashboard_extra::read(&app, name));
                            sampled.insert(name, Instant::now());
                        }
                        feed.publish(updates);
                    } else {
                        sampled.clear();
                    }
                    let guard = feed.state.lock().unwrap_or_else(|p| p.into_inner());
                    if guard.extra_clients[index] == 0 {
                        drop(
                            feed.changed
                                .wait_while(guard, |state| state.extra_clients[index] == 0),
                        );
                    } else {
                        drop(feed.changed.wait_timeout(guard, Duration::from_secs(1)));
                    }
                }
            });
        }
    }

    pub fn start(self: &Arc<Self>, state: Arc<AppState>, events: Arc<crate::event_bus::EventBus>) {
        let weak = Arc::downgrade(self);
        let cache = Arc::clone(&state.cache);
        events.subscribe_fn(
            EVENTS,
            Arc::new(move |name, payload| {
                cache.on_event(name, payload);
                if let Some(feed) = weak.upgrade() {
                    feed.request_refresh();
                }
            }),
        );
        self.start_extra_collectors(Arc::clone(&state));
        let feed = Arc::clone(self);
        crate::system::spawn_named("dashboard-feed", move || {
            let mut sampled = BTreeMap::<&str, Instant>::new();
            loop {
                let (active, sms_active, sms_missing, refresh_requested) = {
                    let mut guard = feed.state.lock().unwrap_or_else(|p| p.into_inner());
                    (
                        guard.clients > 0,
                        guard.sms_clients > 0,
                        !guard.readings.contains_key("sms"),
                        std::mem::take(&mut guard.refresh_requested),
                    )
                };
                if active {
                    let fresh_connection = feed
                        .state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .readings
                        .is_empty();
                    if fresh_connection || refresh_requested {
                        sampled.clear();
                    }
                    let mut updates = Readings::new();
                    for &(name, seconds) in READINGS {
                        if is_sms(name) {
                            if !sms_active {
                                sampled.remove(name);
                                continue;
                            }
                            if sms_missing {
                                sampled.remove(name);
                            }
                        }
                        if sampled
                            .get(name)
                            .is_some_and(|at| at.elapsed() < Duration::from_secs(seconds))
                        {
                            continue;
                        }
                        updates.insert(name.to_string(), read(&state, name));
                        sampled.insert(name, Instant::now());
                    }
                    feed.publish(updates);
                } else {
                    sampled.clear();
                }
                let guard = feed.state.lock().unwrap_or_else(|p| p.into_inner());
                if guard.clients == 0 {
                    drop(feed.changed.wait_while(guard, |state| state.clients == 0));
                } else {
                    drop(feed.changed.wait_timeout(guard, Duration::from_secs(1)));
                }
            }
        });
    }
}

fn is_sms(name: &str) -> bool {
    matches!(name, "sms" | "sms_capacity")
}

fn read(state: &AppState, name: &str) -> Value {
    let (_, mut result) = match name {
        "device" => handlers::device(state),
        "wan" => network_ext::network_wan(state),
        "wan6" => network_ext::network_wan6(state),
        "signal" => handlers::network_signal(state),
        "modem" => handlers::modem_status(state),
        "mobile" => modem_ext::modem_data_get(state),
        "charging" => device_ext::charge_control_get(state),
        "speed" => handlers::network_speed(state),
        "battery" => handlers::battery(state),
        "wifi" => wifi::wifi_status(state),
        "guest" => wifi::guest_status(state),
        "clients" => network_ext::network_clients(state),
        "sms" => sms::sms_list(state, br#"{"page":0,"data_per_page":500,"mem_store":2,"tags":10,"order_by":"order by id desc"}"#),
        "sms_capacity" => sms::sms_capacity(state),
        _ => return json!({"ok": false}),
    };
    // Wi-Fi's configuration API includes keys; the monitoring feed never needs passwords.
    sanitize_reading(name, &mut result);
    result
}

fn sanitize_reading(name: &str, result: &mut Value) {
    if name == "guest" {
        if let Some(data) = result["data"].as_object_mut() {
            data.retain(|key, _| {
                matches!(
                    key.as_str(),
                    "ssid"
                        | "encryption"
                        | "disabled_2g"
                        | "disabled_5g"
                        | "hidden"
                        | "isolate"
                        | "guest_active_time"
                        | "remaining_seconds"
                )
            });
        }
    }
    if name == "mobile" {
        if let Some(data) = result["data"].as_object_mut() {
            data.retain(|key, _| {
                matches!(
                    key.as_str(),
                    "enable" | "roam_enable" | "connect_mode" | "connect_status"
                )
            });
        }
    }
    if name == "wifi" {
        if let Some(data) = result["data"].as_object_mut() {
            data.retain(|key, _| {
                matches!(
                    key.as_str(),
                    "wifi_onoff"
                        | "wifi6_switch"
                        | "encryption_2g"
                        | "encryption_5g"
                        | "hidden_2g"
                        | "hidden_5g"
                        | "htmode_2g"
                        | "htmode_5g"
                        | "txpower_2g"
                        | "txpower_5g"
                        | "radio2_disabled"
                        | "radio5_disabled"
                        | "ssid_2g"
                        | "ssid_5g"
                        | "channel_2g"
                        | "channel_5g"
                )
            });
        }
    }
}

fn delta(previous: &Readings, current: &Readings) -> Readings {
    current
        .iter()
        .filter(|(key, value)| previous.get(*key) != Some(*value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[cfg(test)]
fn frame(
    writer: &mut impl Write,
    event: &str,
    generation: u64,
    readings: &Readings,
) -> io::Result<()> {
    writeln!(
        writer,
        "id: {generation}\nevent: {event}\ndata: {}\n",
        json!({"readings": readings})
    )?;
    writer.flush()
}

/// A console stream on the same axum listener and bearer precheck as the JSON API.
pub fn response(app: Arc<AppState>, url: String, token: String) -> axum::response::Response {
    let interests = Interests::from_url(&url);
    let Some(subscription) = app.dashboard.subscribe(interests) else {
        return crate::httpd::api::json_reply(
            503,
            &json!({"ok":false,"error":"too many live dashboards"}),
        );
    };
    struct Viewer {
        app: Arc<AppState>,
        token: String,
        interests: Interests,
        _subscription: Subscription,
        previous: Readings,
        generation: u64,
        initial: bool,
        started: Instant,
    }
    let viewer = Viewer {
        app,
        token,
        interests,
        _subscription: subscription,
        previous: Readings::new(),
        generation: 0,
        initial: true,
        started: Instant::now(),
    };
    let stream = futures_util::stream::unfold(Some(viewer), |viewer| async move {
        let mut viewer = viewer?;
        loop {
            if viewer.started.elapsed() >= Duration::from_secs(30 * 60) {
                return None;
            }
            if viewer
                .app
                .auth
                .authenticate(crate::auth::Kind::Bearer, &viewer.token)
                .is_none()
            {
                return Some((
                    Ok::<_, std::convert::Infallible>("event: auth\ndata: {}\n\n".to_string()),
                    None,
                ));
            }
            let feed = Arc::clone(&viewer.app.dashboard);
            let notified = feed.live.notified();
            let (generation, current) = {
                let state = feed.state.lock().unwrap_or_else(|p| p.into_inner());
                (
                    state.generation,
                    state
                        .readings
                        .iter()
                        .filter(|(name, _)| viewer.interests.includes(name))
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect::<Readings>(),
                )
            };
            let changes = delta(&viewer.previous, &current);
            if !changes.is_empty() {
                let name = if viewer.initial { "snapshot" } else { "patch" };
                let frame = format!(
                    "id: {generation}\nevent: {name}\ndata: {}\n\n",
                    json!({"readings":changes})
                );
                viewer.previous = current;
                viewer.generation = generation;
                viewer.initial = false;
                return Some((Ok(frame), Some(viewer)));
            }
            if tokio::time::timeout(HEARTBEAT, notified).await.is_err() {
                return Some((Ok(": heartbeat\n\n".to_string()), Some(viewer)));
            }
        }
    });
    crate::events::sse::response(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esim_topic_uses_ninth_bit_and_is_private_to_its_subscribers() {
        let interests = Interests::from_url("/api/dashboard/events?topics=esim");
        assert!(interests.includes("esim_status"));
        assert!(!interests.includes("client_details"));
        assert!(!Interests::default().includes("esim_status"));
        assert!(Interests::default().includes("wan6"));
        let feed = Arc::new(Dashboard::default());
        let viewer = feed.subscribe(interests).unwrap();
        feed.publish(BTreeMap::from([(
            "esim_status".into(),
            json!({"ok":true,"data":{"iccid":"private"}}),
        )]));
        assert!(!feed.next(0, false).1.contains_key("esim_status"));
        assert!(feed.next(0, interests).1.contains_key("esim_status"));
        drop(viewer);
        assert!(!feed
            .state
            .lock()
            .unwrap()
            .readings
            .contains_key("esim_status"));
    }

    #[test]
    fn optional_topics_are_whitelisted_and_never_leak_to_ordinary_streams() {
        let interests = Interests::from_url(
            "/api/dashboard/events?sms=1&topics=operators,forwarding,unknown,terminal",
        );
        assert!(
            interests.sms
                && interests.includes("operators")
                && interests.includes("forwarding")
                && interests.includes("at_port")
        );
        assert!(!interests.includes("resources"));
        let feed = Arc::new(Dashboard::default());
        let _ordinary = feed.subscribe(false).unwrap();
        let viewer = feed.subscribe(interests).unwrap();
        feed.publish(BTreeMap::from([
            ("signal".into(), json!({"ok":true})),
            (
                "forwarding".into(),
                json!({"ok":true,"data":{"log":["private preview"]}}),
            ),
        ]));
        assert!(!feed.next(0, false).1.contains_key("forwarding"));
        assert!(feed.next(0, interests).1.contains_key("forwarding"));
        drop(viewer);
        assert!(!feed
            .state
            .lock()
            .unwrap()
            .readings
            .contains_key("forwarding"));
    }

    #[test]
    fn routing_settings_only_reach_subscribed_viewers_and_expire_after_leaving() {
        let feed = Arc::new(Dashboard::default());
        let _ordinary = feed.subscribe(false).unwrap();
        let interests = Interests::from_url("/api/dashboard/events?topics=routing");
        let viewer = feed.subscribe(interests).unwrap();
        feed.publish(BTreeMap::from([
            ("signal".into(), json!({"ok":true})),
            (
                "lan_config".into(),
                json!({"ok":true,"data":{"lan_ipaddr":"192.168.0.1"}}),
            ),
            (
                "port_rules".into(),
                json!({"ok":true,"data":{"rule_list":[]}}),
            ),
        ]));
        assert!(!feed.next(0, false).1.contains_key("lan_config"));
        assert!(feed.next(0, interests).1.contains_key("port_rules"));
        drop(viewer);
        assert!(!feed
            .state
            .lock()
            .unwrap()
            .readings
            .contains_key("port_rules"));
    }

    #[test]
    fn client_details_use_the_eighth_topic_without_leaking_into_basic_streams() {
        let feed = Arc::new(Dashboard::default());
        let _ordinary = feed.subscribe(false).unwrap();
        let interests = Interests::from_url("/api/dashboard/events?topics=clients");
        let viewer = feed.subscribe(interests).unwrap();
        feed.publish(BTreeMap::from([
            ("clients".into(), json!({"ok":true,"data":{"hosts":{}}})),
            (
                "client_details".into(),
                json!({"ok":true,"data":{"devices":[]}}),
            ),
        ]));
        assert!(feed.next(0, interests).1.contains_key("client_details"));
        assert!(!feed.next(0, false).1.contains_key("client_details"));
        drop(viewer);
        assert!(!feed
            .state
            .lock()
            .unwrap()
            .readings
            .contains_key("client_details"));
    }

    #[test]
    fn changes_include_failed_readings_but_not_unchanged_dependencies() {
        let previous = BTreeMap::from([
            ("signal".into(), json!({"ok": true, "data": {"rsrp": -90}})),
            (
                "battery".into(),
                json!({"ok": true, "data": {"capacity": 80}}),
            ),
        ]);
        let mut current = previous.clone();
        current.insert(
            "signal".into(),
            json!({"ok": false, "error": "unavailable"}),
        );
        assert_eq!(
            delta(&previous, &current),
            BTreeMap::from([("signal".into(), current["signal"].clone())])
        );
    }

    #[test]
    fn configuration_readings_exclude_passwords_and_unknown_private_fields() {
        for (name, data, expected) in [
            (
                "wifi",
                json!({"ssid_2g":"Home","key_2g":"secret","key_5g":"secret","future_password":"secret","txpower_2g":"80"}),
                json!({"ssid_2g":"Home","txpower_2g":"80"}),
            ),
            (
                "guest",
                json!({"ssid":"Guest","key":"secret","future_password":"secret","remaining_seconds":30}),
                json!({"ssid":"Guest","remaining_seconds":30}),
            ),
            (
                "mobile",
                json!({"enable":1,"roam_enable":0,"username":"secret","password":"secret"}),
                json!({"enable":1,"roam_enable":0}),
            ),
        ] {
            let mut envelope = json!({"ok":true,"data":data});
            sanitize_reading(name, &mut envelope);
            assert_eq!(envelope["data"], expected, "{name}");
        }
    }

    #[test]
    fn admission_is_bounded_and_released_on_disconnect() {
        let feed = Arc::new(Dashboard::default());
        let mut subscriptions: Vec<_> = (0..MAX_STREAMS)
            .map(|_| feed.subscribe(false).unwrap())
            .collect();
        assert!(feed.subscribe(false).is_none());
        subscriptions.pop();
        assert!(feed.subscribe(false).is_some());
    }

    #[test]
    fn idle_feed_discards_old_readings_and_does_not_collect_a_backlog() {
        let feed = Arc::new(Dashboard::default());
        let subscription = feed.subscribe(false).unwrap();
        feed.publish(BTreeMap::from([("battery".into(), json!({"ok": true}))]));
        drop(subscription);
        feed.publish(BTreeMap::from([("signal".into(), json!({"ok": true}))]));
        assert!(feed.state.lock().unwrap().readings.is_empty());
    }

    #[test]
    fn wire_frame_is_one_complete_sse_message_even_with_newlines_in_values() {
        let mut bytes = Vec::new();
        frame(
            &mut bytes,
            "patch",
            7,
            &BTreeMap::from([("wifi".into(), json!({"ssid": "hello\nworld"}))]),
        )
        .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("id: 7\nevent: patch\ndata: "));
        assert_eq!(text.matches("\n\n").count(), 1);
        assert!(text.contains("hello\\nworld"));
    }
    #[tokio::test]
    async fn axum_stream_emits_snapshots_patches_and_auth_and_releases_subscription() {
        use futures_util::StreamExt;
        let app = Arc::new(AppState::new());
        app.auth.set_password("test-only");
        let issued = app
            .auth
            .login(
                "test-only",
                crate::auth::Kind::Bearer,
                crate::auth::PeerKey::UNKNOWN,
                &[],
            )
            .unwrap();
        let token = issued.token.expose().to_owned();
        let response = response(
            Arc::clone(&app),
            "/api/dashboard/events".into(),
            token.clone(),
        );
        assert_eq!(response.status(), 200);
        let mut body = response.into_body().into_data_stream();
        app.dashboard.publish(BTreeMap::from([(
            "battery".into(),
            json!({"ok":true,"data":{"capacity":80}}),
        )]));
        let snapshot = body.next().await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&snapshot).contains("event: snapshot"));
        app.dashboard.publish(BTreeMap::from([(
            "battery".into(),
            json!({"ok":true,"data":{"capacity":79}}),
        )]));
        assert!(
            String::from_utf8_lossy(&body.next().await.unwrap().unwrap()).contains("event: patch")
        );
        app.auth.logout(crate::auth::Kind::Bearer, &token);
        assert!(
            String::from_utf8_lossy(&body.next().await.unwrap().unwrap()).contains("event: auth")
        );
        drop(body);
        assert_eq!(app.dashboard.state.lock().unwrap().clients, 0);
    }
    #[test]
    fn messages_are_discarded_when_the_last_message_viewer_leaves() {
        let feed = Arc::new(Dashboard::default());
        let _ordinary = feed.subscribe(false).unwrap();
        feed.publish(BTreeMap::from([(
            "sms".into(),
            json!({"ok": true, "data": {"messages": []}}),
        )]));
        assert!(!feed.state.lock().unwrap().readings.contains_key("sms"));
        let inbox = feed.subscribe(true).unwrap();
        feed.publish(BTreeMap::from([(
            "sms".into(),
            json!({"ok": true, "data": {"messages": []}}),
        )]));
        assert!(feed.state.lock().unwrap().readings.contains_key("sms"));
        assert!(
            !feed.next(0, false).1.contains_key("sms"),
            "ordinary viewers never receive message bodies"
        );
        assert!(feed.next(0, true).1.contains_key("sms"));
        drop(inbox);
        assert!(!feed.state.lock().unwrap().readings.contains_key("sms"));
    }
}
