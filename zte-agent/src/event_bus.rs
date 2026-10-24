use std::io::BufRead;
use std::panic::AssertUnwindSafe;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde_json::Value;

const RESTART_DELAY_SECS: u64 = 5;
const IGNORED_EVENTS: &[&str] = &["zwrt_deviceui_event.touchstatus"];
/// Events the agent publishes itself (`publish`, and `ubus send` for other processes). They are
/// delivered in-process only: never asked of `ubus listen`, and skipped if it shows them anyway.
pub const AGENT_EVENT_PREFIX: &str = "zte_agent.";

/// Handles events itself (`subscribe_fn`): called with the event name and payload.
pub type Handler = Arc<dyn Fn(&str, &Value) + Send + Sync>;

/// Where an event goes: a channel for the payload, or a handler that runs
/// on the dispatching thread (no channel, no thread of its own).
enum Sink {
    Channel(mpsc::Sender<Value>),
    Call(Handler),
}

struct Subscriber {
    event: String,
    tx: Sink,
}

pub struct EventBus {
    subscribers: Mutex<Vec<Subscriber>>,
}

impl EventBus {
    pub fn new() -> Self {
        EventBus {
            subscribers: Mutex::new(Vec::new()),
        }
    }

    /// Register interest in a specific ubus event. Returns a receiver
    /// that will get the event payload each time it fires.
    pub fn subscribe(&self, event_name: &str) -> mpsc::Receiver<Value> {
        let (tx, rx) = mpsc::channel();
        let mut subs = self.subscribers.lock().unwrap();
        subs.push(Subscriber {
            event: event_name.to_string(),
            tx: Sink::Channel(tx),
        });
        rx
    }

    /// `handler` gets each of these events with its name, on the thread that
    /// dispatches it (the `ubus listen` reader, or an agent event's publisher)
    /// while the subscriber list is locked: it must return quickly and must
    /// not block or publish. Same rules as `subscribe`: firmware names before `start`.
    pub fn subscribe_fn(&self, names: &[&str], handler: Handler) {
        let mut subs = self.subscribers.lock().unwrap();
        for &name in names {
            subs.push(Subscriber {
                event: name.to_string(),
                tx: Sink::Call(Arc::clone(&handler)),
            });
        }
    }

    /// Spawn the listener thread. Call this after all firmware subscriptions
    /// are registered: the set of event names passed to `ubus listen` is fixed
    /// here and the kernel filters everything else out before it reaches the
    /// agent. Agent events (`AGENT_EVENT_PREFIX`) can be subscribed at any time.
    pub fn start(self: &Arc<Self>) {
        let bus = Arc::clone(self);
        // Named: it also runs the event hub's handler, and power traces count per thread.
        crate::system::spawn_named("event-bus", move || bus.run_loop());
    }

    /// Delivers an agent event to its in-process subscribers, as if `ubus listen` had read it.
    pub fn publish(&self, event_name: &str, payload: &Value) {
        self.dispatch(event_name, payload);
    }

    /// The `ubus listen` argument list derived from the current subscribers.
    pub(crate) fn listen_patterns(&self) -> Vec<String> {
        let subs = self.subscribers.lock().unwrap();
        listen_patterns(subs.iter().map(|s| s.event.as_str()))
    }

    fn run_loop(&self) {
        let patterns = self.listen_patterns();
        if patterns.is_empty() {
            eprintln!("[event_bus] no subscribers, ubus listen is unfiltered");
        } else {
            eprintln!("[event_bus] ubus listen filter: {}", patterns.join(" "));
        }
        loop {
            match spawn_ubus_listen(&patterns) {
                Ok(mut child) => {
                    eprintln!("[event_bus] ubus listen started (pid {})", child.id());
                    if let Some(stdout) = child.stdout.take() {
                        self.read_events(std::io::BufReader::new(stdout));
                    }
                    // Process exited — reap it
                    let _ = child.wait();
                    eprintln!(
                        "[event_bus] ubus listen exited, restarting in {RESTART_DELAY_SECS}s"
                    );
                }
                Err(e) => {
                    eprintln!("[event_bus] failed to spawn ubus listen: {e}");
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(RESTART_DELAY_SECS));
        }
    }

    fn read_events<R: BufRead>(&self, reader: R) {
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break, // pipe closed
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // ubus listen outputs: { "event_name": { ...payload... } }
            let parsed: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let obj = match parsed.as_object() {
                Some(o) => o,
                None => continue,
            };

            // Extract single top-level key as event name
            for (event_name, payload) in obj {
                if IGNORED_EVENTS.contains(&event_name.as_str())
                    || event_name.starts_with(AGENT_EVENT_PREFIX)
                {
                    continue;
                }

                self.dispatch(event_name, payload);
            }
        }
    }

    fn dispatch(&self, event_name: &str, payload: &Value) {
        let subs = self.subscribers.lock().unwrap();
        for sub in subs.iter() {
            if sub.event == event_name {
                // The channel is unbounded, so send() never blocks; it only
                // fails once the receiver has been dropped, in which case the
                // event is simply not delivered to that subscriber.
                match &sub.tx {
                    Sink::Channel(tx) => drop(tx.send(payload.clone())),
                    // A panicking handler must not poison the subscriber
                    // list: every later dispatch (and publish) would panic.
                    Sink::Call(handler) => {
                        let call = AssertUnwindSafe(|| handler(event_name, payload));
                        if std::panic::catch_unwind(call).is_err() {
                            eprintln!("[event_bus] handler for {event_name} panicked");
                        }
                    }
                }
            }
        }
    }
}

/// Build the `ubus listen` pattern list from subscriber event names:
/// sorted, de-duplicated, with empty names dropped. `ubus listen` treats
/// each argument as an exact event name and accepts several at once; an
/// empty argument would match nothing, so it is never emitted. An empty
/// result means "listen to everything", which is the right fallback when
/// nobody has subscribed.
fn listen_patterns<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut patterns: Vec<String> = names
        .into_iter()
        .filter(|name| !name.is_empty() && !name.starts_with(AGENT_EVENT_PREFIX))
        .map(str::to_string)
        .collect();
    patterns.sort();
    patterns.dedup();
    patterns
}

fn ubus_listen_command(patterns: &[String]) -> Command {
    let mut cmd = Command::new("ubus");
    cmd.arg("listen")
        .args(patterns)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    die_with_parent(&mut cmd);
    cmd
}

fn spawn_ubus_listen(patterns: &[String]) -> Result<Child, String> {
    ubus_listen_command(patterns)
        .spawn()
        .map_err(|e| format!("spawn ubus listen: {e}"))
}

/// Ask the kernel to SIGTERM the child when the thread that spawned it
/// dies. The event-bus thread lives for the whole process, so this makes
/// `ubus listen` die together with the agent instead of lingering as an
/// orphan after a `killall zte-agent`.
#[cfg(target_os = "linux")]
fn die_with_parent(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;

    let parent = std::process::id();
    // SAFETY: the closure runs in the forked child before exec and only
    // uses async-signal-safe calls (prctl, getppid, _exit); it touches no
    // locks or heap allocations.
    unsafe {
        cmd.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong);
            // Close the fork -> prctl race: if the parent already went away
            // the death signal will never arrive, so bail out here.
            if libc::getppid() as u32 != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn die_with_parent(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::Duration;

    #[test]
    fn listen_patterns_sorts_dedups_and_drops_empty_names() {
        let names = [
            "zwrt_wms_status_event",
            "router_event_wan_connect_status",
            "BSP_CHARGER_EVENT",
            "",
            "router_event_wan_connect_status",
            "network.interface",
            "",
        ];
        assert_eq!(
            listen_patterns(names),
            [
                "BSP_CHARGER_EVENT",
                "network.interface",
                "router_event_wan_connect_status",
                "zwrt_wms_status_event",
            ]
        );
    }

    #[test]
    fn listen_patterns_is_empty_without_usable_subscribers() {
        assert!(listen_patterns([]).is_empty());
        assert!(listen_patterns(["", ""]).is_empty());
    }

    #[test]
    fn bus_derives_patterns_from_subscribers() {
        let bus = EventBus::new();
        let _a = bus.subscribe("zwrt_wms_status_event");
        let _b = bus.subscribe("BSP_CHARGER_EVENT");
        let _c = bus.subscribe("router_event_wan_connect_status");
        let _d = bus.subscribe("router_event_wan_connect_status");
        let _e = bus.subscribe("");
        assert_eq!(
            bus.listen_patterns(),
            [
                "BSP_CHARGER_EVENT",
                "router_event_wan_connect_status",
                "zwrt_wms_status_event",
            ]
        );
    }

    #[test]
    fn ubus_listen_command_passes_patterns_as_arguments() {
        let patterns = listen_patterns(["b.event", "a.event", "b.event"]);
        let cmd = ubus_listen_command(&patterns);
        assert_eq!(cmd.get_program(), "ubus");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["listen", "a.event", "b.event"]);
    }

    #[test]
    fn ubus_listen_command_is_unfiltered_without_patterns() {
        let cmd = ubus_listen_command(&[]);
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["listen"]);
    }

    #[test]
    fn read_events_dispatches_only_to_matching_subscribers() {
        let bus = EventBus::new();
        let charger_rx = bus.subscribe("BSP_CHARGER_EVENT");
        let charger_rx2 = bus.subscribe("BSP_CHARGER_EVENT");
        let sms_rx = bus.subscribe("zwrt_wms_status_event");
        let touch_rx = bus.subscribe("zwrt_deviceui_event.touchstatus");
        // A subscriber whose receiver is gone must not break dispatch.
        drop(bus.subscribe("BSP_CHARGER_EVENT"));

        let input = concat!(
            "\n",
            "not json\n",
            "[1, 2, 3]\n",
            "{ \"zwrt_deviceui_event.touchstatus\": {\"status\": 1} }\n",
            "{ \"BSP_CHARGER_EVENT\": {\"charger\": \"connected\"} }\n",
            "{ \"unsubscribed_event\": {} }\n",
        );
        bus.read_events(Cursor::new(input));

        let expected = serde_json::json!({"charger": "connected"});
        assert_eq!(charger_rx.try_recv().unwrap(), expected);
        assert_eq!(charger_rx2.try_recv().unwrap(), expected);
        assert!(charger_rx.try_recv().is_err());
        assert!(sms_rx.try_recv().is_err());
        assert!(touch_rx.recv_timeout(Duration::from_millis(10)).is_err());
    }

    #[test]
    fn subscribe_fn_gets_name_and_payload_on_the_dispatching_thread() {
        let bus = EventBus::new();
        let plain = bus.subscribe("zwrt_servicestatus");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let me = std::thread::current().id();
        let names = ["zwrt_servicestatus", "zwrt_wms_status_event", "zte_agent.sim_identity"];
        bus.subscribe_fn(
            &names,
            Arc::new(move |name: &str, payload: &Value| {
                // Synchronous: the reader thread itself, no channel.
                assert_eq!(std::thread::current().id(), me);
                log.lock().unwrap().push((name.to_owned(), payload.clone()));
            }),
        );
        // The listen filter is the same as for plain subscriptions.
        assert_eq!(bus.listen_patterns(), ["zwrt_servicestatus", "zwrt_wms_status_event"]);
        bus.read_events(Cursor::new(concat!(
            "{ \"zwrt_wms_status_event\": {\"wms_status\": \"new_sms_received\"} }\n",
            "{ \"zwrt_servicestatus\": {\"network_type\": \"ENDC\"} }\n",
        )));
        bus.publish("zte_agent.sim_identity", &serde_json::json!({"phase": "online"}));
        let seen = seen.lock().unwrap();
        let got: Vec<_> = seen.iter().map(|(n, p)| (n.as_str(), p.clone())).collect();
        assert_eq!(
            got,
            [
                ("zwrt_wms_status_event", serde_json::json!({"wms_status": "new_sms_received"})),
                ("zwrt_servicestatus", serde_json::json!({"network_type": "ENDC"})),
                ("zte_agent.sim_identity", serde_json::json!({"phase": "online"})),
            ]
        );
        assert_eq!(plain.try_recv().unwrap(), serde_json::json!({"network_type": "ENDC"}));
    }

    #[test]
    fn a_panicking_handler_does_not_break_the_bus() {
        let bus = EventBus::new();
        bus.subscribe_fn(&["zwrt_servicestatus"], Arc::new(|_: &str, _: &Value| panic!("handler panic (expected by this test)")));
        let rx = bus.subscribe("zwrt_servicestatus");
        for _ in 0..2 {
            bus.read_events(Cursor::new("{ \"zwrt_servicestatus\": {\"network_type\": \"LTE\"} }\n"));
            assert_eq!(rx.try_recv().unwrap(), serde_json::json!({"network_type": "LTE"}));
        }
        assert!(!bus.subscribers.is_poisoned());
    }

    #[test]
    fn agent_events_are_published_in_process_and_never_read_from_ubus() {
        let bus = EventBus::new();
        let _wan = bus.subscribe("router_event_wan_connect_status");
        let identity = bus.subscribe("zte_agent.sim_identity");
        assert_eq!(bus.listen_patterns(), ["router_event_wan_connect_status"]);

        bus.read_events(Cursor::new("{ \"zte_agent.sim_identity\": {\"phase\": \"online\"} }\n"));
        assert!(identity.try_recv().is_err(), "our own ubus send is not delivered twice");

        let payload = serde_json::json!({"phase": "changed"});
        bus.publish("zte_agent.sim_identity", &payload);
        assert_eq!(identity.try_recv().unwrap(), payload);
    }
}
