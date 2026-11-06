//! SIM identity bridge: tells the rest of the agent, and other processes through `ubus send`,
//! when the active SIM identity changes, whether by an eSIM profile switch from the agent, a
//! switch from the card's own menu or a physical swap.
//!
//! Phases `leaving` → `changed` → `online` (or `stalled`) are decided by comparing ICCIDs, so the
//! same SIM reloading (an LPM cycle, a REFRESH that keeps the profile) is not a change. The
//! firmware already treats a new identity as a SIM change (data teardown, APN choice, NAT flush);
//! nothing here sends firmware events or touches its daemons.
//!
//! `Machine` holds the decisions and is pure. The runtime thread feeds it the firmware's card
//! and WAN events, the agent's own switch calls, SIM reads through the shared cache and network
//! checks (file reads only), and emits each phase: cache invalidation, the `stamp` state, one
//! masked log line, `EventBus::publish` and `ubus send`. Idle, it blocks on its channel: no
//! timers and no forks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::cache::{self, UbusCache};
use crate::event_bus::EventBus;

/// Published in-process and with `ubus send`.
pub const EVENT: &str = "zte_agent.sim_identity";
/// Firmware events the tracker needs; subscribe to each before `EventBus::start`.
pub const FIRMWARE_EVENTS: [&str; 3] = [
    "zwrt_card_state_event",
    "router_event_wan_connect_status",
    "network.interface",
];

/// A firmware `switch_slot_begin` this soon after the agent armed is the agent's own switch.
const ARM_WINDOW: Duration = Duration::from_secs(60);
/// SIM reads while a transition waits for the new identity; network checks while it waits for
/// the router to come online.
const POLL: Duration = Duration::from_secs(3);
/// A read showing the same ICCID ends a transition only this long after `leaving` (the SIM is
/// still the old one for the first seconds of a switch).
const SAME_SETTLE: Duration = Duration::from_secs(10);
/// `online` waits until WAN events have been quiet this long (the firmware brings the WAN up,
/// reloads the firewall several times and rewrites DNS within a few seconds).
const QUIET: Duration = Duration::from_secs(8);
/// ...or until a default route has been there this long.
const GATE_CAP: Duration = Duration::from_secs(30);
/// No `online` this long after `leaving` publishes `stalled` once.
const STALL_AFTER: Duration = Duration::from_secs(150);
/// SIM reads at start until the first identity is known.
const SEED_RETRY: Duration = Duration::from_secs(30);
const SEED_TRIES: u32 = 10;

const RMNET_LOCKS: [&str; 2] = ["/tmp/rmnet_script_zte_wan.lock", "/tmp/rmnet_script_zte_wan6.lock"];

/// Identifies this agent run (start time, ms). Compared for equality only; the same value is
/// reported by every endpoint that carries it.
pub fn agent_instance() -> u64 {
    static INSTANCE: OnceLock<u64> = OnceLock::new();
    *INSTANCE.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default()
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    iccid: String,
    /// `<mcc>-<mnc>`.
    plmn: Option<String>,
    /// Service provider name from the card.
    operator: Option<String>,
}

impl Identity {
    fn tail(&self) -> String {
        let digits = self.iccid.trim().trim_end_matches(['F', 'f']);
        digits.chars().skip(digits.chars().count().saturating_sub(4)).collect()
    }
}

/// What the router's network looks like, from files only.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gate {
    route: bool,
    dns: bool,
    rmnet_busy: bool,
}

impl Gate {
    pub(crate) fn ready_for_probe(self) -> bool { self.route && !self.rmnet_busy }
}

#[derive(Debug)]
pub enum Input {
    /// A `zwrt_card_state_event` payload.
    Card(Value),
    /// WAN activity: `Some(up)` when the event says which way it went.
    Wan(Option<bool>),
    /// A SIM read: the loaded identity, or `None` when the SIM isn't ready or the read failed.
    Sim(Option<Identity>),
    Gate(Gate),
    AgentArm,
    AgentDisarm,
    AgentStarted { generation: u64, previous: Option<String> },
    Timer,
}

#[derive(Debug, PartialEq)]
pub enum Output {
    Seeded(Identity),
    Event(Value),
}

#[derive(Debug)]
struct Transition {
    started: Instant,
    source: &'static str,
    generation: Option<u64>,
    /// Set when the new identity is known: `Some(true)` changed, `Some(false)` same SIM again.
    resolved: Option<bool>,
    stalled: bool,
    wan_up_seen: bool,
    last_wan: Option<Instant>,
    route_since: Option<Instant>,
    next_read: Instant,
    next_gate: Instant,
}

/// The decisions, without I/O or clocks of its own.
#[derive(Debug, Default)]
pub struct Machine {
    known: Option<Identity>,
    epoch: u64,
    armed_at: Option<Instant>,
    /// The firmware already started the armed switch: the agent's own `AgentStarted`, which can
    /// arrive late (the card call may run up to its time limit), only attributes it.
    claimed_at: Option<Instant>,
    transition: Option<Transition>,
    seed_tries: u32,
    next_seed: Option<Instant>,
    /// One read asked for by an event (a ready event without an ICCID, activity after stalled).
    read_requested: bool,
}

impl Machine {
    pub fn new(now: Instant) -> Self {
        Machine { next_seed: Some(now), ..Machine::default() }
    }

    pub fn step(&mut self, input: Input, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        match input {
            Input::AgentArm => {
                self.armed_at = Some(now);
                self.claimed_at = None;
            }
            Input::AgentDisarm => self.armed_at = None,
            Input::AgentStarted { generation, previous } => {
                self.armed_at = None;
                let claimed = self
                    .claimed_at
                    .take()
                    .is_some_and(|t| now.saturating_duration_since(t) < ARM_WINDOW);
                if self.known.is_none() {
                    // The card's previous profile: an agent switch before the first read counts.
                    if let Some(iccid) = previous.filter(|s| !s.trim().is_empty()) {
                        self.known = Some(Identity { iccid, plmn: None, operator: None });
                    }
                }
                match &mut self.transition {
                    // The firmware's switch_slot_begin came first (while armed): attribute it.
                    Some(t) if t.source == "agent" && t.generation.is_none() => t.generation = Some(generation),
                    Some(t) if t.resolved.is_none() => {
                        t.source = "agent";
                        t.generation = Some(generation);
                    }
                    // The firmware's signals already opened (and maybe closed) this switch.
                    _ if claimed => {}
                    _ => self.leave("agent", Some(generation), now, &mut out),
                }
            }
            Input::Card(payload) => self.card(&payload, now, &mut out),
            Input::Sim(Some(id)) => {
                self.after_read(now);
                self.identity(id, now, &mut out);
            }
            Input::Sim(None) => self.after_read(now),
            Input::Wan(up) => {
                if let Some(t) = &mut self.transition {
                    t.last_wan = Some(now);
                    if up == Some(true) {
                        t.wan_up_seen = true;
                    }
                    if t.stalled && t.resolved.is_none() {
                        self.read_requested = true;
                    }
                }
            }
            Input::Gate(gate) => self.gate(gate, now, &mut out),
            Input::Timer => {}
        }
        self.check_stall(now, &mut out);
        out
    }

    /// A SIM read is due: one asked for by an event, a seeding retry, or a transition's poll.
    pub fn wants_read(&self, now: Instant) -> bool {
        self.read_requested
            || self.seed_due(now)
            || self
                .transition
                .as_ref()
                .is_some_and(|t| t.resolved.is_none() && !t.stalled && now >= t.next_read)
    }

    fn seed_due(&self, now: Instant) -> bool {
        self.known.is_none() && self.next_seed.is_some_and(|t| now >= t)
    }

    /// A network check is due.
    pub fn wants_gate(&self, now: Instant) -> bool {
        self.transition.as_ref().is_some_and(|t| Self::gating(t, now) && now >= t.next_gate)
    }

    /// When the runtime should wake without an input; `None` blocks until one arrives.
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        let mut wake: Option<Instant> = None;
        let mut at = |t: Instant| wake = Some(wake.map_or(t, |w| w.min(t)));
        if self.known.is_none() {
            if let Some(t) = self.next_seed {
                at(t);
            }
        }
        if let Some(t) = &self.transition {
            if !t.stalled {
                at(t.started + STALL_AFTER);
                if t.resolved.is_none() {
                    at(t.next_read);
                }
            }
            if Self::gating(t, now) {
                at(t.next_gate);
            }
        }
        wake
    }

    #[cfg(test)]
    fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Network checks run while the new identity waits for `online`; after `stalled`, only for a
    /// while after WAN activity.
    fn gating(t: &Transition, now: Instant) -> bool {
        t.resolved.is_some()
            && (!t.stalled || t.last_wan.is_some_and(|w| now.saturating_duration_since(w) < QUIET + GATE_CAP))
    }

    fn armed(&self, now: Instant) -> bool {
        self.armed_at.is_some_and(|t| now.saturating_duration_since(t) < ARM_WINDOW)
    }

    fn card(&mut self, payload: &Value, now: Instant, out: &mut Vec<Output>) {
        if payload["slot_state"] == "switch_slot_begin" {
            let source = if self.armed(now) { "agent" } else { "card" };
            self.leave(source, None, now, out);
        } else if payload["sim_hotswap"] == "sim hot remove" {
            self.leave("hotswap", None, now, out);
        } else if payload["sim_states"] == "sim ready" {
            match identity_from(payload) {
                Some(id) => self.identity(id, now, out),
                None => self.read_requested = true,
            }
        } else if self.known.is_none() || self.transition.as_ref().is_some_and(|t| t.stalled) {
            // Seeding gave up, or a stalled transition: any card event is worth one read.
            self.read_requested = true;
        }
    }

    /// The outgoing identity is leaving. A second signal for the same, still unresolved
    /// transition only refines who started it.
    fn leave(&mut self, source: &'static str, generation: Option<u64>, now: Instant, out: &mut Vec<Output>) {
        if let Some(t) = &mut self.transition {
            if t.resolved.is_none() {
                if source == "agent" {
                    t.source = "agent";
                    t.generation = generation.or(t.generation);
                }
                return;
            }
        }
        self.transition = Some(Transition {
            started: now,
            source,
            generation,
            resolved: None,
            stalled: false,
            wan_up_seen: false,
            last_wan: None,
            route_since: None,
            next_read: now + POLL,
            next_gate: now,
        });
        if source == "agent" && generation.is_none() {
            self.claimed_at = Some(now);
        }
        self.armed_at = None;
        let known = self.known.clone();
        out.push(Output::Event(self.payload("leaving", known.as_ref(), now, Map::new())));
    }

    fn identity(&mut self, id: Identity, now: Instant, out: &mut Vec<Output>) {
        let unresolved = self.transition.as_ref().is_some_and(|t| t.resolved.is_none());
        let Some(known) = &mut self.known else {
            if !unresolved {
                self.known = Some(id.clone());
                out.push(Output::Seeded(id));
                return;
            }
            // Something left before the first read: the identity that arrives counts as new.
            self.change(id, now, out);
            return;
        };
        if crate::esim::same_iccid(&known.iccid, &id.iccid) {
            if id.plmn.is_some() {
                known.plmn = id.plmn;
            }
            if id.operator.is_some() {
                known.operator = id.operator;
            }
            if let Some(t) = &mut self.transition {
                if t.resolved.is_none() && now.saturating_duration_since(t.started) >= SAME_SETTLE {
                    t.resolved = Some(false);
                    t.next_gate = now;
                }
            }
            return;
        }
        self.change(id, now, out);
    }

    fn change(&mut self, id: Identity, now: Instant, out: &mut Vec<Output>) {
        self.epoch += 1;
        self.known = Some(id);
        let source = if self.armed(now) { "agent" } else { "unknown" };
        if source == "agent" {
            self.claimed_at = Some(now);
            self.armed_at = None;
        }
        match &mut self.transition {
            Some(t) if t.resolved.is_none() => {}
            _ => {
                // The leaving was missed (or a second change came before online).
                self.transition = Some(Transition {
                    started: now,
                    source,
                    generation: None,
                    resolved: None,
                    stalled: false,
                    wan_up_seen: false,
                    last_wan: None,
                    route_since: None,
                    next_read: now,
                    next_gate: now,
                });
            }
        }
        if let Some(t) = &mut self.transition {
            t.resolved = Some(true);
            t.next_gate = now;
        }
        let known = self.known.clone();
        out.push(Output::Event(self.payload("changed", known.as_ref(), now, Map::new())));
    }

    fn gate(&mut self, gate: Gate, now: Instant, out: &mut Vec<Output>) {
        let Some(t) = &mut self.transition else { return };
        let Some(changed) = t.resolved else { return };
        t.next_gate = now + POLL;
        if !gate.route {
            t.route_since = None;
            return;
        }
        let route_since = *t.route_since.get_or_insert(now);
        let quiet = t.last_wan.is_none_or(|w| now.saturating_duration_since(w) >= QUIET);
        let settled = (t.wan_up_seen || !changed) && gate.dns && !gate.rmnet_busy && quiet;
        if !settled && now.saturating_duration_since(route_since) < GATE_CAP {
            return;
        }
        let mut extra = Map::new();
        extra.insert("identity_changed".into(), json!(changed));
        extra.insert("settled".into(), json!(settled));
        let known = self.known.clone();
        out.push(Output::Event(self.payload("online", known.as_ref(), now, extra)));
        self.transition = None;
    }

    fn check_stall(&mut self, now: Instant, out: &mut Vec<Output>) {
        let Some(t) = &mut self.transition else { return };
        if t.stalled || now.saturating_duration_since(t.started) < STALL_AFTER {
            return;
        }
        t.stalled = true;
        let reason = if t.resolved.is_none() { "sim_not_ready" } else { "no_wan" };
        let mut extra = Map::new();
        extra.insert("reason".into(), json!(reason));
        let known = self.known.clone();
        out.push(Output::Event(self.payload("stalled", known.as_ref(), now, extra)));
    }

    fn after_read(&mut self, now: Instant) {
        self.read_requested = false;
        // Only a seeding retry counts against the seeding budget, not a transition's poll.
        if self.seed_due(now) {
            self.seed_tries += 1;
            self.next_seed = (self.seed_tries < SEED_TRIES).then(|| now + SEED_RETRY);
        }
        if let Some(t) = &mut self.transition {
            t.next_read = now + POLL;
        }
    }

    /// The event payload: masked identity only (PLMN, operator name, last 4 ICCID digits).
    fn payload(&self, phase: &str, id: Option<&Identity>, now: Instant, extra: Map<String, Value>) -> Value {
        let t = self.transition.as_ref();
        let mut p = Map::new();
        p.insert("phase".into(), json!(phase));
        p.insert("epoch".into(), json!(self.epoch));
        p.insert("source".into(), json!(t.map_or("unknown", |t| t.source)));
        if let Some(g) = t.filter(|t| t.source == "agent").and_then(|t| t.generation) {
            p.insert("generation".into(), json!(g));
        }
        p.insert("plmn".into(), json!(id.and_then(|i| i.plmn.clone())));
        p.insert("operator".into(), json!(id.and_then(|i| i.operator.clone())));
        p.insert("iccid_tail".into(), json!(id.map(Identity::tail).filter(|s| !s.is_empty())));
        let since = t.map_or(0, |t| now.saturating_duration_since(t.started).as_millis() as u64);
        p.insert("since_leaving_ms".into(), json!(since));
        p.extend(extra);
        Value::Object(p)
    }
}

/// The identity in a "sim ready" event or a `get_sim_info` reply; `None` without an ICCID.
fn identity_from(info: &Value) -> Option<Identity> {
    let iccid = info["sim_iccid"].as_str().map(str::trim).filter(|s| !s.is_empty())?;
    let field = |k: &str| match &info[k] {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let plmn = field("mdm_mcc").zip(field("mdm_mnc")).map(|(mcc, mnc)| format!("{mcc}-{mnc}"));
    let operator = info["spn_name_data"].as_str().map(decode_spn).filter(|s| !s.is_empty());
    Some(Identity { iccid: iccid.to_string(), plmn, operator })
}

/// `spn_name_data` is UCS-2 hex; padding characters are dropped.
fn decode_spn(hex: &str) -> String {
    crate::sms_forward::decode_ucs2_hex(hex)
        .chars()
        .filter(|c| !matches!(c, '\u{0}' | '\u{FFFF}'))
        .collect::<String>()
        .trim()
        .to_string()
}

/// `/proc/net/ipv6_route` has a default route (::/0) on an interface other than `lo`.
fn ipv6_default_route(table: &str) -> bool {
    table.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() >= 10 && f[0] == "0".repeat(32) && f[1] == "00" && f[9] != "lo"
    })
}

/// `/etc/resolv.conf` names a nameserver other than loopback.
fn resolv_has_wan_dns(text: &str) -> bool {
    text.lines().any(|l| {
        let mut words = l.split_whitespace();
        words.next() == Some("nameserver")
            && words.next().is_some_and(|ip| !ip.starts_with("127.") && ip != "::1")
    })
}

pub(crate) fn observe_gate() -> Gate {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    Gate {
        route: crate::esim::has_default_route(&read("/proc/net/route")) || ipv6_default_route(&read("/proc/net/ipv6_route")),
        dns: resolv_has_wan_dns(&read("/etc/resolv.conf")),
        rmnet_busy: RMNET_LOCKS.iter().any(|p| std::path::Path::new(p).exists()),
    }
}

/// The loaded identity from `get_sim_info`, through the shared cache (1 s, so it shares forks
/// with app polls).
fn read_sim(cache: &UbusCache) -> Option<Identity> {
    let info = cache
        .get_or_fetch(cache::SIM_INFO, Duration::from_secs(1), || {
            crate::ubus::call("zwrt_zte_mdm.api", "get_sim_info", Some("{}"))
        })
        .ok()?;
    if crate::esim::sim_states_ready(&info) != Some(true) {
        return None;
    }
    identity_from(&info)
}

#[derive(Clone, Copy)]
struct Status {
    epoch: u64,
    phase: &'static str,
}

pub struct SimIdentity {
    tx: Mutex<Sender<Input>>,
    rx: Mutex<Option<Receiver<Input>>>,
    status: Mutex<Status>,
}

impl SimIdentity {
    /// The channel exists from the start, so switch calls made before `start` are kept.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        SimIdentity {
            tx: Mutex::new(tx),
            rx: Mutex::new(Some(rx)),
            status: Mutex::new(Status { epoch: 0, phase: "idle" }),
        }
    }

    fn send(&self, input: Input) {
        let _ = self.tx.lock().unwrap_or_else(|p| p.into_inner()).send(input);
    }

    /// The agent is about to ask the card to enable a profile.
    pub fn agent_switch_arm(&self) {
        self.send(Input::AgentArm);
    }

    /// The card refused (busy) or was never asked: the switch didn't start.
    pub fn agent_switch_disarm(&self) {
        self.send(Input::AgentDisarm);
    }

    /// The card took the enable (or may have: a failed call is verified like a success).
    pub fn agent_switch_started(&self, generation: u64, previous: Option<&str>) {
        self.send(Input::AgentStarted { generation, previous: previous.map(str::to_string) });
    }

    /// Adds `sim_epoch`, `sim_phase` and `agent_instance` to an endpoint's data, so the apps can
    /// tell when to refresh.
    pub fn stamp(&self, obj: &mut Map<String, Value>) {
        let status = *self.status.lock().unwrap_or_else(|p| p.into_inner());
        obj.insert("sim_epoch".into(), json!(status.epoch));
        obj.insert("sim_phase".into(), json!(status.phase));
        obj.insert("agent_instance".into(), json!(agent_instance()));
    }

    /// Starts the tracker; `events` are the `FIRMWARE_EVENTS` subscriptions. Call once.
    pub fn start(self: &Arc<Self>, bus: Arc<EventBus>, cache: Arc<UbusCache>, events: Vec<(&'static str, Receiver<Value>)>) {
        let Some(rx) = self.rx.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return;
        };
        for (name, events) in events {
            let tx = self.tx.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let thread = match name {
                "zwrt_card_state_event" => "simid-card",
                "router_event_wan_connect_status" => "simid-wan",
                _ => "simid-netif",
            };
            let _ = crate::system::try_spawn_named(thread, move || {
                for payload in events.iter() {
                    if let Some(input) = firmware_input(name, payload) {
                        if tx.send(input).is_err() {
                            return;
                        }
                    }
                }
            });
        }
        let me = Arc::clone(self);
        let _ = crate::system::try_spawn_named("sim-identity", move || me.run(rx, &bus, &cache));
    }

    fn run(&self, rx: Receiver<Input>, bus: &EventBus, cache: &UbusCache) {
        let mut machine = Machine::new(Instant::now());
        loop {
            // Reads and network checks the machine is waiting for, until none is due.
            loop {
                let now = Instant::now();
                let input = if machine.wants_read(now) {
                    Input::Sim(read_sim(cache))
                } else if machine.wants_gate(now) {
                    Input::Gate(observe_gate())
                } else {
                    break;
                };
                for out in machine.step(input, Instant::now()) {
                    self.emit(out, bus, cache);
                }
            }
            let now = Instant::now();
            let input = match machine.next_wake(now) {
                Some(at) => match rx.recv_timeout(at.saturating_duration_since(now)) {
                    Ok(input) => input,
                    Err(RecvTimeoutError::Timeout) => Input::Timer,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(input) => input,
                    Err(_) => return,
                },
            };
            for out in machine.step(input, Instant::now()) {
                self.emit(out, bus, cache);
            }
        }
    }

    fn emit(&self, out: Output, bus: &EventBus, cache: &UbusCache) {
        let mut payload = match out {
            Output::Seeded(id) => {
                eprintln!("[sim_identity] seeded {} …{}", id.plmn.as_deref().unwrap_or("?"), id.tail());
                return;
            }
            Output::Event(payload) => payload,
        };
        if payload["reason"] == "no_wan" {
            if let Some(reason) = crate::esim::data_blocker() {
                payload["reason"] = json!(reason);
            }
        }
        let phase = match payload["phase"].as_str() {
            Some("leaving") => "leaving",
            Some("changed") => "changed",
            Some("online") => "online",
            _ => "stalled",
        };
        // Stale SIM and WAN values go before the new phase is visible to the apps.
        cache.on_sim_identity(phase);
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = Status {
            epoch: payload["epoch"].as_u64().unwrap_or_default(),
            phase,
        };
        eprintln!("[sim_identity] {}", log_line(&payload));
        bus.publish(EVENT, &payload);
        broadcast(payload);
    }
}

/// The firmware event as a tracker input; `None` for interfaces other than the WAN.
fn firmware_input(name: &str, payload: Value) -> Option<Input> {
    match name {
        "zwrt_card_state_event" => Some(Input::Card(payload)),
        "router_event_wan_connect_status" => Some(Input::Wan(crate::conn_watchdog::wan_from_event(&payload))),
        "network.interface" if matches!(payload["interface"].as_str(), Some("zte_wan" | "zte_wan6")) => {
            match payload["action"].as_str() {
                Some("ifup") => Some(Input::Wan(Some(true))),
                Some("ifdown") => Some(Input::Wan(Some(false))),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `ubus send` on a helper thread. A send still running when the next phase comes is not
/// queued behind: that phase is skipped (and logged), never delayed.
fn broadcast(payload: Value) {
    static SENDING: AtomicBool = AtomicBool::new(false);
    if SENDING.swap(true, Ordering::AcqRel) {
        eprintln!("[sim_identity] ubus send skipped: the previous one is still running");
        return;
    }
    let spawned = crate::system::try_spawn_named("simid-send", move || {
        if let Err(e) = crate::ubus::send(EVENT, &payload) {
            eprintln!("[sim_identity] {e}");
        }
        SENDING.store(false, Ordering::Release);
    });
    if spawned.is_err() {
        SENDING.store(false, Ordering::Release);
    }
}

/// `changed epoch=1 source=agent 515-02 GOMO …0001 (+4.1 s)`.
fn log_line(p: &Value) -> String {
    let mut line = format!(
        "{} epoch={} source={}",
        p["phase"].as_str().unwrap_or("?"),
        p["epoch"],
        p["source"].as_str().unwrap_or("?")
    );
    for key in ["plmn", "operator"] {
        if let Some(v) = p[key].as_str() {
            line.push(' ');
            line.push_str(v);
        }
    }
    if let Some(tail) = p["iccid_tail"].as_str() {
        line.push_str(&format!(" …{tail}"));
    }
    if let Some(reason) = p["reason"].as_str() {
        line.push_str(&format!(" reason={reason}"));
    }
    if p["phase"] == "online" {
        line.push_str(&format!(" settled={}", p["settled"]));
    }
    let secs = p["since_leaving_ms"].as_u64().unwrap_or_default() as f64 / 1000.0;
    line.push_str(&format!(" (+{secs:.1} s)"));
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "8963000000000000001";
    const B: &str = "8963000000000000002";

    fn ready(iccid: &str) -> Input {
        // "GOMO" as UCS-2 hex.
        Input::Card(json!({"sim_states": "sim ready", "sim_changed": "1", "sim_iccid": iccid,
            "mdm_mcc": "515", "mdm_mnc": "02", "spn_name_data": "0047004F004D004F", "sim_imsi": "515020000000001"}))
    }

    fn id(iccid: &str) -> Identity {
        Identity { iccid: iccid.into(), plmn: Some("515-03".into()), operator: None }
    }

    fn begin() -> Input {
        Input::Card(json!({"sim_states": "sim not ready", "slot_state": "switch_slot_begin"}))
    }

    fn seeded(now: Instant) -> Machine {
        let mut m = Machine::new(now);
        assert_eq!(m.step(Input::Sim(Some(id(A))), now), vec![Output::Seeded(id(A))]);
        m
    }

    fn events(out: Vec<Output>) -> Vec<Value> {
        out.into_iter()
            .filter_map(|o| match o {
                Output::Event(v) => Some(v),
                Output::Seeded(_) => None,
            })
            .collect()
    }

    fn phases(out: &[Value]) -> Vec<&str> {
        out.iter().map(|v| v["phase"].as_str().unwrap()).collect()
    }

    const OPEN: Gate = Gate { route: true, dns: true, rmnet_busy: false };

    #[test]
    fn the_same_sim_reloading_is_not_a_change() {
        let t0 = Instant::now();
        let mut m = seeded(t0);
        let mut all = Vec::new();
        all.extend(events(m.step(Input::Card(json!({"sim_states": "sim not ready"})), t0)));
        all.extend(events(m.step(ready(A), t0 + Duration::from_secs(3))));
        all.extend(events(m.step(ready(&format!("{A}F")), t0 + Duration::from_secs(5))));
        assert!(all.is_empty(), "{all:?}");
        assert_eq!(m.epoch(), 0);
        assert!(m.transition.is_none());
        assert_eq!(m.next_wake(t0), None, "idle: no timers");
    }

    #[test]
    fn an_agent_switch_publishes_leaving_changed_online_once() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let mut m = seeded(t0);
        let mut all = Vec::new();
        all.extend(events(m.step(Input::AgentArm, t0)));
        all.extend(events(m.step(begin(), s(1))));
        all.extend(events(m.step(Input::AgentStarted { generation: 7, previous: Some(A.into()) }, s(1))));
        all.extend(events(m.step(Input::Wan(Some(false)), s(2))));
        all.extend(events(m.step(ready(B), s(5))));
        all.extend(events(m.step(ready(B), s(7))));
        all.extend(events(m.step(Input::Gate(Gate::default()), s(10))));
        all.extend(events(m.step(Input::Wan(Some(true)), s(30))));
        all.extend(events(m.step(Input::Gate(Gate { rmnet_busy: true, ..OPEN }), s(31))));
        all.extend(events(m.step(Input::Wan(None), s(33))));
        all.extend(events(m.step(Input::Gate(OPEN), s(36))));
        assert_eq!(phases(&all), ["leaving", "changed"], "still inside QUIET");
        all.extend(events(m.step(Input::Gate(OPEN), s(41))));
        assert_eq!(phases(&all), ["leaving", "changed", "online"]);
        assert_eq!(all[0]["source"], "agent");
        assert_eq!(all[0]["iccid_tail"], "0001", "leaving carries the outgoing identity");
        assert_eq!(all[1]["epoch"], 1);
        assert_eq!(all[1]["generation"], 7);
        assert_eq!(all[1]["iccid_tail"], "0002");
        assert_eq!(all[1]["operator"], "GOMO");
        assert_eq!(all[1]["plmn"], "515-02");
        assert_eq!(all[2]["identity_changed"], true);
        assert_eq!(all[2]["settled"], true);
        assert_eq!(all[2]["since_leaving_ms"], 40_000);
        assert!(m.transition.is_none());
    }

    #[test]
    fn the_agent_call_returning_first_still_gives_one_leaving() {
        let t0 = Instant::now();
        let mut m = seeded(t0);
        m.step(Input::AgentArm, t0);
        let first = events(m.step(Input::AgentStarted { generation: 3, previous: None }, t0));
        let second = events(m.step(begin(), t0 + Duration::from_millis(500)));
        assert_eq!(phases(&first), ["leaving"]);
        assert_eq!(first[0]["generation"], 3);
        assert!(second.is_empty());
    }

    #[test]
    fn card_menu_switches_and_hot_swaps_are_attributed() {
        let t0 = Instant::now();
        let mut m = seeded(t0);
        assert_eq!(events(m.step(begin(), t0))[0]["source"], "card");
        let mut m = seeded(t0);
        let out = events(m.step(Input::Card(json!({"sim_hotswap": "sim hot remove"})), t0));
        assert_eq!(out[0]["source"], "hotswap");
    }

    #[test]
    fn a_refused_or_late_arm_does_not_claim_the_switch() {
        let t0 = Instant::now();
        let mut m = seeded(t0);
        m.step(Input::AgentArm, t0);
        m.step(Input::AgentDisarm, t0);
        assert_eq!(events(m.step(begin(), t0))[0]["source"], "card");
        let mut m = seeded(t0);
        m.step(Input::AgentArm, t0);
        assert_eq!(events(m.step(begin(), t0 + ARM_WINDOW))[0]["source"], "card");
    }

    #[test]
    fn a_missed_leaving_still_counts_the_change() {
        let t0 = Instant::now();
        let mut m = seeded(t0);
        let out = events(m.step(ready(B), t0));
        assert_eq!(phases(&out), ["changed"]);
        assert_eq!(out[0]["source"], "unknown");
        assert!(out[0].get("generation").is_none());
        // No WAN-up seen: online only once the route has been there for GATE_CAP.
        assert!(events(m.step(Input::Gate(OPEN), t0 + Duration::from_secs(1))).is_empty());
        let out = events(m.step(Input::Gate(OPEN), t0 + Duration::from_secs(31)));
        assert_eq!(phases(&out), ["online"]);
        assert_eq!(out[0]["settled"], false);
    }

    #[test]
    fn a_ready_without_iccid_asks_for_a_read() {
        let t0 = Instant::now();
        let mut m = seeded(t0);
        m.step(Input::Card(json!({"sim_states": "sim ready"})), t0);
        assert!(m.wants_read(t0));
        m.step(Input::Sim(None), t0);
        assert!(!m.wants_read(t0));
    }

    #[test]
    fn a_refresh_that_keeps_the_identity_ends_online_without_an_epoch() {
        let t0 = Instant::now();
        let mut m = seeded(t0);
        assert_eq!(phases(&events(m.step(begin(), t0))), ["leaving"]);
        m.step(Input::Sim(Some(id(A))), t0 + Duration::from_secs(1));
        assert!(m.transition.as_ref().unwrap().resolved.is_none(), "too early to call it the same SIM");
        m.step(Input::Sim(Some(id(A))), t0 + Duration::from_secs(12));
        let out = events(m.step(Input::Gate(OPEN), t0 + Duration::from_secs(13)));
        assert_eq!(phases(&out), ["online"]);
        assert_eq!(out[0]["identity_changed"], false);
        assert_eq!(out[0]["epoch"], 0);
    }

    #[test]
    fn no_online_within_the_limit_is_stalled_once_and_recovers() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let mut m = seeded(t0);
        m.step(begin(), t0);
        assert!(events(m.step(Input::Timer, s(149))).is_empty());
        let out = events(m.step(Input::Timer, s(150)));
        assert_eq!(phases(&out), ["stalled"]);
        assert_eq!(out[0]["reason"], "sim_not_ready");
        assert!(events(m.step(Input::Timer, s(200))).is_empty(), "once");
        assert!(!m.wants_read(s(400)), "no timed reads after stalled");
        assert_eq!(m.next_wake(s(400)), None);
        m.step(Input::Wan(None), s(401));
        assert!(m.wants_read(s(401)), "activity asks for one read");
        assert!(events(m.step(Input::Sim(Some(id(B))), s(402))).len() == 1);
        m.step(Input::Wan(Some(true)), s(403));
        let out = events(m.step(Input::Gate(OPEN), s(412)));
        assert_eq!(phases(&out), ["online"]);
    }

    #[test]
    fn seeding_retries_are_bounded() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        let mut now = t0;
        for _ in 0..SEED_TRIES {
            assert!(m.wants_read(now));
            m.step(Input::Sim(None), now);
            now += SEED_RETRY;
        }
        assert!(!m.wants_read(now + Duration::from_secs(3600)));
        assert_eq!(m.next_wake(now), None);
        m.step(Input::Card(json!({"sim_states": "sim not ready"})), now);
        assert!(m.wants_read(now), "a card event asks again");
    }

    #[test]
    fn an_unseeded_transition_polls_without_spinning_and_counts_the_new_identity() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let mut m = Machine::new(t0);
        m.step(Input::Sim(None), t0);
        m.step(begin(), s(1));
        for now in [s(4), s(5), s(10), s(29)] {
            assert!(m.next_wake(now).is_some_and(|w| w > now) || m.wants_read(now) || m.wants_gate(now), "{:?}", now - t0);
            if m.wants_read(now) {
                m.step(Input::Sim(None), now);
            }
        }
        assert_eq!(m.seed_tries, 1, "transition polls don't use up the seeding budget");
        let out = events(m.step(ready(B), s(30)));
        assert_eq!(phases(&out), ["changed"]);
        assert_eq!(out[0]["epoch"], 1);
    }

    #[test]
    fn a_late_agent_call_after_online_does_not_open_a_second_switch() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let mut m = seeded(t0);
        let mut all = Vec::new();
        all.extend(events(m.step(Input::AgentArm, t0)));
        all.extend(events(m.step(begin(), s(1))));
        all.extend(events(m.step(ready(B), s(5))));
        all.extend(events(m.step(Input::Wan(Some(true)), s(15))));
        all.extend(events(m.step(Input::Gate(OPEN), s(24))));
        all.extend(events(m.step(Input::AgentStarted { generation: 9, previous: Some(A.into()) }, s(35))));
        assert_eq!(phases(&all), ["leaving", "changed", "online"]);
        assert!(m.transition.is_none());
        // The next switch, with no firmware signal before the call returns, still opens one.
        m.step(Input::AgentArm, s(100));
        let out = events(m.step(Input::AgentStarted { generation: 10, previous: None }, s(101)));
        assert_eq!(phases(&out), ["leaving"]);
    }

    #[test]
    fn a_new_switch_after_the_change_supersedes_the_old_transition() {
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        let mut m = seeded(t0);
        m.step(begin(), t0);
        m.step(ready(B), s(4));
        let out = events(m.step(begin(), s(10)));
        assert_eq!(phases(&out), ["leaving"]);
        assert_eq!(out[0]["iccid_tail"], "0002");
        let out = events(m.step(ready(A), s(14)));
        assert_eq!(out[0]["epoch"], 2);
    }

    #[test]
    fn an_unseeded_agent_switch_uses_the_cards_previous_profile() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        m.step(Input::AgentArm, t0);
        let out = events(m.step(Input::AgentStarted { generation: 1, previous: Some(A.into()) }, t0));
        assert_eq!(out[0]["iccid_tail"], "0001");
        let out = events(m.step(ready(B), t0 + Duration::from_secs(4)));
        assert_eq!(phases(&out), ["changed"]);
        assert_eq!(out[0]["epoch"], 1);
    }

    #[test]
    fn payloads_carry_only_masked_identity() {
        let t0 = Instant::now();
        let mut m = seeded(t0);
        m.step(Input::AgentArm, t0);
        m.step(Input::AgentStarted { generation: 2, previous: None }, t0);
        let out = events(m.step(ready(B), t0 + Duration::from_secs(4)));
        let keys: Vec<&String> = out[0].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["epoch", "generation", "iccid_tail", "operator", "phase", "plmn", "since_leaving_ms", "source"]);
        let text = out[0].to_string();
        assert!(!text.contains(B) && !text.contains("515020000000001"), "{text}");
    }

    #[test]
    fn card_events_that_are_not_identity_signals_are_ignored() {
        let t0 = Instant::now();
        let mut m = seeded(t0);
        for p in [
            json!({"sim_states": "sim not ready"}),
            json!({"slot_state": "switch_slot_end"}),
            json!({"sim_status": "1"}),
            json!({"sim_states": "sim pin"}),
            json!({"sim_changed": "1"}),
        ] {
            assert!(events(m.step(Input::Card(p), t0)).is_empty());
        }
        assert!(m.transition.is_none());
        assert!(!m.wants_read(t0));
    }

    #[test]
    fn network_checks_read_routes_and_dns_correctly() {
        let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200       lo\n\
                  00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003 rmnet_data0\n";
        assert!(ipv6_default_route(v6));
        assert!(!ipv6_default_route(v6.lines().next().unwrap()), "the unreachable route on lo doesn't count");
        assert!(resolv_has_wan_dns("search lan\nnameserver 10.1.2.3\n"));
        assert!(!resolv_has_wan_dns("nameserver 127.0.0.1\nnameserver ::1\n"));
        assert!(!resolv_has_wan_dns(""));
    }

    #[test]
    fn identity_parsing_masks_and_decodes() {
        let i = identity_from(&json!({"sim_iccid": "896300000000000000F", "mdm_mcc": "515", "mdm_mnc": 3, "spn_name_data": "0053004D00410052005400FFFF"})).unwrap();
        assert_eq!(i.tail(), "0000");
        assert_eq!(i.plmn.as_deref(), Some("515-3"));
        assert_eq!(identity_from(&json!({"sim_iccid": ""})), None);
        assert_eq!(decode_spn("0053004D0041005200540000FFFF"), "SMART");
    }

    #[test]
    fn inputs_sent_before_start_are_kept() {
        let s = SimIdentity::new();
        s.agent_switch_arm();
        s.agent_switch_started(4, Some(A));
        let rx = s.rx.lock().unwrap().take().unwrap();
        assert!(matches!(rx.try_recv(), Ok(Input::AgentArm)));
        assert!(matches!(rx.try_recv(), Ok(Input::AgentStarted { generation: 4, .. })));
    }

    #[test]
    fn stamp_inserts_epoch_phase_instance_together() {
        let s = SimIdentity::new();
        let mut obj = Map::new();
        s.stamp(&mut obj);
        assert_eq!(obj["sim_epoch"], 0);
        assert_eq!(obj["sim_phase"], "idle");
        assert_eq!(obj["agent_instance"], agent_instance());
    }

    #[test]
    fn firmware_events_map_to_inputs() {
        assert!(matches!(firmware_input("router_event_wan_connect_status", json!({"wan_status": "ipv6_connected"})), Some(Input::Wan(Some(true)))));
        assert!(matches!(firmware_input("network.interface", json!({"interface": "zte_wan6", "action": "ifdown"})), Some(Input::Wan(Some(false)))));
        assert!(firmware_input("network.interface", json!({"interface": "lan", "action": "ifup"})).is_none());
        assert!(matches!(firmware_input("zwrt_card_state_event", json!({})), Some(Input::Card(_))));
    }
}
