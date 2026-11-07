//! 带宽控制的设备适配层：仅 relay-watch 调用。空闲时只读 RX 计数；保持窄档时约每 60 s
//! 读回并重设一次。
use super::{llstats, width, Cmd, HOSTAPD, IFACE, JOURNAL};
use std::collections::VecDeque;
use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use width::{Action, Controller, Event, Hotspot, Mode, Probe, Sample};

static GENERATION: AtomicU64 = AtomicU64::new(1);
/// 探测判坏的宽度，本进程内跨重连保留。
struct Avoided { network: String, width: u32, signal: i32, until: Instant, level: u8 }
static AVOIDED: Mutex<Vec<Avoided>> = Mutex::new(Vec::new());
const WIDTHS: &str = "widths.json";
/// 两次 `iwpriv chwidth` 之间的间隔；真机只验证过 1 s。
const TOGGLE_PAUSE: Duration = Duration::from_secs(1);
/// 洪泛 ping 自身 2 s 截止；持有 network/session 锁，不能更长。
const PROBE_LIMIT: Duration = Duration::from_secs(4);

struct Counters {
    at: Instant,
    rx: u64,
    tx: u64,
    hotspot_rx: u64,
    scan: u64,
    primary: u32,
    width: u32,
    stats: llstats::Snapshot,
}

pub(super) struct LinkWidth {
    tuner: Option<Controller>,
    network: String,
    neighbours: Vec<(u32, i32)>,
    signal: i32,
    generation: u64,
    scan: u64,
    mode: Mode,
    enabled: Option<bool>,
    last_rx: Option<(Instant, u64)>,
    last: Option<Counters>,
    last_log: Option<String>,
    phy: Option<u32>,
    gateway: Option<Ipv4Addr>,
}
impl LinkWidth {
    pub(super) fn new(network: &super::Network, neighbours: &[(u32, i32)], tuning: super::saved::Tuning) -> Self {
        Self {
            tuner: None, network: network.id.clone(), neighbours: neighbours.to_vec(), signal: network.signal,
            generation: GENERATION.fetch_add(1, Ordering::Relaxed), scan: 0,
            mode: Mode::parse(tuning.as_str()),
            enabled: None, last_rx: None, last: None, last_log: None, phy: None, gateway: None,
        }
    }
    pub(super) fn width(&self) -> Option<u32> { self.tuner.as_ref().map(Controller::width) }
    pub(super) fn state(&self) -> Option<&'static str> { self.tuner.as_ref().and_then(Controller::status) }
    pub(super) fn primary(&self) -> Option<u32> { self.tuner.as_ref().map(Controller::primary) }
    pub(super) fn scanned(&mut self) {
        self.scan = self.scan.wrapping_add(1);
        if let Some(tuner) = self.tuner.as_mut() { tuner.scanned(); }
    }
    fn log(&mut self, message: String) {
        if self.last_log.as_ref() != Some(&message) {
            eprintln!("[relay-width] id={} gen={} mode={:?} phy={:?} {message}", self.network, self.generation, self.mode, self.phy);
            self.last_log = Some(message);
        }
    }
    fn initialize(&mut self, io: &mut impl Driver) -> Result<(), String> {
        let (primary, negotiated, center) = io.channel()?;
        if width::chwidth_code(negotiated).is_none() { return Err("Unsupported upstream channel width".into()); }
        let history = io.history(&self.network, self.signal);
        let mut start = history.as_ref().filter(|h| h.usable && h.width <= negotiated).map(|h| h.width)
            .unwrap_or_else(|| width::predict(primary, negotiated, center, &self.neighbours));
        let (now, avoided) = (io.now(), io.avoided(&self.network, self.signal));
        // 起点正在退避（上次关联探测判坏）时直接从更窄一档开始，不让用户再踩一次。
        while avoided.iter().any(|&(w, until, _)| w == start && until > now) {
            let Some(narrower) = width::narrower(start) else { break };
            start = narrower;
        }
        let mut tuner = Controller::new(self.generation, self.mode, primary, negotiated, start, history.map(|h| h.width));
        for &(w, until, level) in &avoided { tuner.avoid(w, until, level); }
        let actions = tuner.initialize();
        self.tuner = Some(tuner);
        self.neighbours.clear();
        self.dispatch(self.generation, actions, io);
        Ok(())
    }
    fn dispatch(&mut self, gen: u64, actions: Vec<Action>, io: &mut impl Driver) {
        let mut queue: VecDeque<_> = actions.into();
        while let Some(action) = queue.pop_front() {
            if gen != self.generation || self.tuner.as_ref().is_none_or(|t| gen != t.generation()) { break; }
            match action {
                Action::Apply(target) => {
                    if self.mode != Mode::Auto { continue; }
                    // 任何动作都打断计数端点；命令成功仍需下一 tick 的 iw 读回。
                    self.last = None;
                    let ok = io.apply(target).is_ok();
                    let next = self.tuner.as_mut().unwrap().handle(gen, io.now(), Event::Applied { width: target, ok });
                    queue.extend(next);
                }
                Action::Reapply { width, via } => {
                    if self.mode != Mode::Auto { continue; }
                    self.last = None;
                    // 失败时宽度可能停在 via；下一 tick 的读回会把它当作外部宽度接受。
                    if io.apply(via).is_err() || { io.pause(); io.apply(width).is_err() } {
                        self.log(format!("reapply {width} via {via} failed"));
                    }
                }
                Action::Hotspot(check) => {
                    let iface = width::hotspot_iface(self.tuner.as_ref().unwrap().primary());
                    let snapshot = io.hotspot(iface);
                    queue.extend(self.tuner.as_mut().unwrap().handle(gen, io.now(), Event::Hotspot { check, snapshot }));
                }
                Action::Probe(purpose) => {
                    if self.mode != Mode::Auto { continue; }
                    // 洪泛的回包落在 STA RX 计数里，不能跨进下一样本。
                    self.last = None;
                    let result = self.gateway.map(|gateway| io.probe(gateway));
                    queue.extend(self.tuner.as_mut().unwrap().handle(gen, io.now(), Event::Probed { purpose, result }));
                }
                Action::Alive(toggles) => {
                    if self.mode != Mode::Auto { continue; }
                    let alive = self.gateway.and_then(|gateway| io.alive(gateway));
                    queue.extend(self.tuner.as_mut().unwrap().handle(gen, io.now(), Event::Alive { toggles, alive }));
                }
                Action::Avoid { width, until, level } => {
                    if self.mode == Mode::Auto { io.avoid(&self.network, self.signal, width, until, level); }
                }
                Action::Save { width, signal } => {
                    if self.mode == Mode::Auto && io.save(&self.network, width, signal).is_err() {
                        self.log("cannot save width history".into());
                    }
                }
                Action::Log(message) => self.log(message),
            }
        }
    }
    fn tick(&mut self, readback: Option<(u32, u32)>, sample: Option<Sample>, reset: bool, io: &mut impl Driver) {
        let actions = self.tuner.as_mut().unwrap().handle(self.generation, io.now(), Event::Tick { channel: readback, sample, reset });
        self.dispatch(self.generation, actions, io);
    }
}

trait Driver {
    fn now(&self) -> Instant;
    fn counter(&mut self, iface: &str, name: &str) -> Option<u64>;
    fn channel(&mut self) -> Result<(u32, u32, u32), String>;
    fn enable(&mut self) -> bool;
    fn stats(&mut self, primary: u32) -> Option<llstats::Snapshot>;
    fn station(&mut self) -> Option<String>;
    fn hotspot_rx(&mut self, iface: &str) -> Option<u64>;
    fn hotspot(&mut self, iface: &str) -> Hotspot;
    fn apply(&mut self, width: u32) -> Result<(), String>;
    fn pause(&mut self);
    fn probe(&mut self, gateway: Ipv4Addr) -> Probe;
    fn alive(&mut self, gateway: Ipv4Addr) -> Option<bool>;
    fn avoided(&mut self, network: &str, signal: i32) -> Vec<(u32, Instant, u8)>;
    fn avoid(&mut self, network: &str, signal: i32, width: u32, until: Instant, level: u8);
    fn history(&mut self, network: &str, signal: i32) -> Option<width::cache::History>;
    fn save(&mut self, network: &str, width: u32, signal: i32) -> Result<(), String>;
}
struct Device;
impl Driver for Device {
    fn now(&self) -> Instant { Instant::now() }
    fn counter(&mut self, iface: &str, name: &str) -> Option<u64> { counter(iface, name) }
    fn channel(&mut self) -> Result<(u32, u32, u32), String> { channel() }
    fn enable(&mut self) -> bool { llstats::enable(IFACE).is_ok() }
    fn stats(&mut self, primary: u32) -> Option<llstats::Snapshot> { llstats::read(IFACE).ok().and_then(|s| llstats::snapshot(&s, primary)) }
    fn station(&mut self) -> Option<String> { query(Cmd::new("iw", &["dev", IFACE, "station", "dump"])).ok() }
    fn hotspot_rx(&mut self, iface: &str) -> Option<u64> {
        if Path::new(&format!("/sys/class/net/{iface}")).exists() { counter(iface, "rx_bytes") } else { Some(0) }
    }
    fn hotspot(&mut self, iface: &str) -> Hotspot { hotspot(iface) }
    fn apply(&mut self, width: u32) -> Result<(), String> { set_width(width) }
    fn pause(&mut self) { std::thread::sleep(TOGGLE_PAUSE); }
    fn probe(&mut self, gateway: Ipv4Addr) -> Probe {
        let (preload, gateway) = (width::PROBE_PRELOAD.to_string(), gateway.to_string());
        let cmd = Cmd::new("ping", &["-q", "-f", "-I", IFACE, "-l", &preload, "-s", "1400", "-w", "2", &gateway]);
        // 一个回包都没有时 ping 以 1 退出、读不到摘要，按零回包处理；参照因此判为不可用。
        crate::net_tools::run(cmd.command(), PROBE_LIMIT).ok().and_then(|s| width::parse_probe(&s)).unwrap_or_default()
    }
    fn alive(&mut self, gateway: Ipv4Addr) -> Option<bool> {
        let gateway = gateway.to_string();
        // 每 0.2 s 发一个，收到第一个回包即以 0 退出；2 s 内一个都没有则以 1 退出。
        match crate::net_tools::run(Cmd::new("ping", &["-q", "-c", "1", "-i", "0.2", "-w", "2", "-I", IFACE, &gateway]).command(), PROBE_LIMIT) {
            Ok(_) => Some(true),
            Err(e) if e.starts_with("Cannot run device tool") => None,
            Err(_) => Some(false),
        }
    }
    fn avoided(&mut self, network: &str, signal: i32) -> Vec<(u32, Instant, u8)> {
        // 与宽度历史一致：信号差超过 10 dB 视为换了环境。
        AVOIDED.lock().unwrap_or_else(PoisonError::into_inner).iter()
            .filter(|a| a.network == network && a.signal.abs_diff(signal) <= 10)
            .map(|a| (a.width, a.until, a.level)).collect()
    }
    fn avoid(&mut self, network: &str, signal: i32, width: u32, until: Instant, level: u8) {
        let mut avoided = AVOIDED.lock().unwrap_or_else(PoisonError::into_inner);
        avoided.retain(|a| !(a.network == network && a.width == width));
        avoided.push(Avoided { network: network.to_owned(), width, signal, until, level });
        if avoided.len() > 32 { avoided.remove(0); }
    }
    fn history(&mut self, network: &str, signal: i32) -> Option<width::cache::History> {
        width::cache::remembered(&Path::new(JOURNAL).join(WIDTHS), network, signal, crate::network_clock::WIFI.verified_utc())
    }
    fn save(&mut self, network: &str, width: u32, signal: i32) -> Result<(), String> {
        width::cache::remember(&Path::new(JOURNAL).join(WIDTHS), network, width, signal, crate::network_clock::WIFI.verified_utc())
    }
}

fn counter(iface: &str, name: &str) -> Option<u64> {
    fs::read_to_string(format!("/sys/class/net/{iface}/statistics/{name}")).ok()?.trim().parse().ok()
}
fn query(cmd: Cmd) -> Result<String, String> {
    crate::net_tools::run(cmd.command(), std::time::Duration::from_secs(1))
}
fn channel() -> Result<(u32, u32, u32), String> {
    width::parse_channel(&query(Cmd::new("iw", &["dev", IFACE, "info"]))?).ok_or_else(|| "Cannot read the upstream channel width".into())
}
fn set_width(width: u32) -> Result<(), String> {
    let code = width::chwidth_code(width).ok_or("Unsupported upstream channel width")?;
    query(Cmd::new("iwpriv", &[IFACE, "chwidth", code])).map(|_| ()).map_err(|_| "Cannot change the upstream channel width".into())
}
fn hotspot(iface: &str) -> Hotspot {
    if !Path::new(&format!("/sys/class/net/{iface}")).exists() { return Hotspot::Off; }
    // 接口存在但 hostapd 读失败，不冒充热点关闭。
    let Ok(status) = query(Cmd::new("hostapd_cli", &["-p", HOSTAPD, "-i", iface, "status"])) else { return Hotspot::Unavailable };
    let Ok(info) = query(Cmd::new("iw", &["dev", iface, "info"])) else { return Hotspot::Unavailable };
    let clients = query(Cmd::new("iw", &["dev", iface, "station", "dump"])).ok()
        .map(|s| s.lines().filter(|l| l.starts_with("Station ")).count());
    Hotspot::parse(&status, &info, clients)
}

/// 新关联之后、带宽跟踪开始之前调用；三种模式都做，因为它只恢复协商宽度。
/// B31 的 `iwpriv chwidth` 跨断开、接口重建和换频段保留：读回是协商宽度，上游 AP
/// 却仍按旧的窄档发送。同值重发无效，所以先切窄一档再回到协商宽度。
pub(super) fn reset_width() {
    crate::httpd::gate::debug_assert_may_block();
    match reset(&mut Device) {
        Ok(Some(width)) => eprintln!("[relay-width] upstream width reset to the negotiated {width} MHz"),
        Ok(None) => {}
        Err(e) => eprintln!("[relay-width] cannot reset the upstream width: {e}"),
    }
}

fn reset(io: &mut impl Driver) -> Result<Option<u32>, String> {
    let (_, width, _) = io.channel()?;
    let Some(lower) = width::narrower(width) else { return Ok(None) };
    io.apply(lower)?;
    io.pause();
    io.apply(width)?;
    let (_, readback, _) = io.channel()?;
    if readback != width { return Err(format!("readback {readback} MHz, expected {width} MHz")); }
    Ok(Some(width))
}

/// 调用方持有 network lock 与当前 Session；扫描、断开和重关联不能在 I/O 间插入。
pub(super) fn tune_width(link: &mut LinkWidth, gateway: Ipv4Addr) -> Result<(), String> {
    crate::httpd::gate::debug_assert_may_block();
    link.gateway = Some(gateway);
    step(link, &mut Device)
}

fn step(link: &mut LinkWidth, io: &mut impl Driver) -> Result<(), String> {
    if link.tuner.is_none() { link.initialize(io)?; }
    if link.mode == Mode::Off { return Ok(()); }
    let now = io.now();
    let rx = io.counter(IFACE, "rx_bytes");
    let previous = link.last_rx.take();
    link.last_rx = rx.map(|rx| (now, rx));
    let traffic = previous.zip(rx).and_then(|((at, before), rx)| rx.checked_sub(before).map(|bytes| (now.duration_since(at).as_secs_f64(), bytes)));
    let tuner = link.tuner.as_ref().unwrap();
    let demand = traffic.is_some_and(|(seconds, bytes)| width::busy(seconds, bytes) || (bytes > 0 && (tuner.collecting() || link.last.is_some() || tuner.active())));
    if !demand && !tuner.active() && !tuner.refresh_due(now) {
        link.last = None;
        // 不查驱动；仍推进窗口空档计时，防止把前后两次业务拼起来。
        link.tuner.as_mut().unwrap().idle(now, previous.zip(rx).is_some_and(|((_, before), rx)| rx < before));
        return Ok(());
    }
    let Ok((primary, actual, _)) = io.channel() else {
        link.last = None;
        link.tick(None, None, false, io);
        return Ok(());
    };
    if !demand {
        link.last = None;
        link.tick(Some((primary, actual)), None, false, io);
        return Ok(());
    }
    if link.enabled.is_none() {
        link.enabled = Some(io.enable());
        if link.enabled == Some(false) { link.log("LL stats unavailable; keeping starting width, no PHY fallback".into()); }
    }
    if link.enabled == Some(false) {
        link.tick(Some((primary, actual)), None, false, io);
        return Ok(());
    }
    let raw = io.stats(primary);
    let dump = io.station();
    link.phy = dump.as_deref().and_then(width::parse_rx_rate);
    let signal = dump.as_deref().and_then(width::parse_signal);
    let mcs = dump.as_deref().and_then(width::parse_rx_mcs);
    let hotspot = width::hotspot_iface(primary);
    let hotspot_rx = io.hotspot_rx(hotspot);
    // LL/netdev 端点在同一读取批次内；不追溯分配尚未读取 LL 的区间。
    let counters = raw.zip(io.counter(IFACE, "rx_bytes")).zip(io.counter(IFACE, "tx_bytes")).zip(hotspot_rx)
        .map(|(((stats, rx), tx), hotspot_rx)| Counters { at: io.now(), rx, tx, hotspot_rx, scan: link.scan, primary, width: actual, stats });
    let before = link.last.take();
    let (sample, reset) = measurement(before.as_ref(), counters.as_ref(), signal, mcs);
    let reset = reset || (before.is_some() && counters.is_none());
    link.last = counters;
    if link.last.is_none() { link.log("LL stats or context unavailable; no PHY fallback".into()); }
    link.tick(Some((primary, actual)), sample, reset, io);
    Ok(())
}

fn measurement(before: Option<&Counters>, after: Option<&Counters>, signal: Option<i32>, rx_mcs: Option<u32>) -> (Option<Sample>, bool) {
    let (Some(a), Some(b), Some(signal)) = (before, after, signal) else { return (None, false) };
    if a.primary != b.primary || a.width != b.width { return (None, a.primary != b.primary); }
    let Some((bytes, tx_bytes, hotspot_bytes)) = b.rx.checked_sub(a.rx).zip(b.tx.checked_sub(a.tx)).zip(b.hotspot_rx.checked_sub(a.hotspot_rx)).map(|((rx, tx), ap)| (rx, tx, ap)) else {
        return (None, true);
    };
    match llstats::rx_airtime(&a.stats, &b.stats) {
        llstats::Delta::Sample { radio, rx_ms } => (Some(Sample {
            seconds: b.at.duration_since(a.at).as_secs_f64(), bytes, airtime_ms: rx_ms, hotspot_bytes, tx_bytes,
            signal, rx_mcs, scanned: a.scan != b.scan, radio,
        }), false),
        llstats::Delta::Ambiguous => (None, false),
        llstats::Delta::Reset => (None, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn counts(now: Instant, rx: u64, tx: u64, hotspot_rx: u64, time: u64) -> Counters {
        let text = format!("Radio: 0 on_time: {time}, tx_time: 0, rx_time: {time}, on_time_scan: 0\nChan width: 1, center_freq: 2412, center_freq0: 2422, center_freq1: 0, on_time: {time}, cca_busy_time: 0, tx_time: 0, rx_time: 0\n");
        Counters { at: now, rx, tx, hotspot_rx, scan: 0, primary: 2412, width: 40, stats: llstats::snapshot(&text, 2412).unwrap() }
    }
    struct Mock {
        now: Instant,
        rx: u64,
        airtime: u64,
        actual: u32,
        queries: usize,
        stats_reads: usize,
        enable_calls: usize,
        enable_ok: bool,
        stats_ok: bool,
        readback: bool,
        writes: Vec<u32>,
        saved: Vec<u32>,
        pauses: usize,
        history: Option<width::cache::History>,
        probes: Vec<u32>,
        alive: Vec<u32>,
        avoided: Vec<(String, u32, i32, Instant, u8)>,
        hotspot_state: Hotspot,
        failed_apply: Option<u32>,
        alive_value: Option<bool>,
    }
    impl Mock {
        fn new() -> Self {
            Self { now: Instant::now(), rx: 0, airtime: 100, actual: 40, queries: 0, stats_reads: 0, enable_calls: 0,
                enable_ok: true, stats_ok: true, readback: true, writes: Vec::new(), saved: Vec::new(), pauses: 0, history: None,
                probes: Vec::new(), alive: Vec::new(), avoided: Vec::new(), hotspot_state: Hotspot::Off,
                failed_apply: None, alive_value: Some(true) }
        }
        fn tick(&mut self, link: &mut LinkWidth, bytes: u64) {
            self.now += std::time::Duration::from_secs(3);
            self.rx += bytes;
            self.airtime += 1000;
            step(link, self).unwrap();
        }
    }
    impl Driver for Mock {
        fn now(&self) -> Instant { self.now }
        fn counter(&mut self, _iface: &str, name: &str) -> Option<u64> { Some(if name == "rx_bytes" { self.rx } else { 0 }) }
        fn channel(&mut self) -> Result<(u32, u32, u32), String> {
            self.queries += 1;
            if self.readback { Ok((2412, self.actual, 2422)) } else { Err("missing".into()) }
        }
        fn enable(&mut self) -> bool { self.enable_calls += 1; self.enable_ok }
        fn stats(&mut self, _primary: u32) -> Option<llstats::Snapshot> {
            self.stats_reads += 1;
            self.stats_ok.then(|| counts(self.now, self.rx, 0, 0, self.airtime).stats)
        }
        fn station(&mut self) -> Option<String> { self.queries += 1; Some("signal: -40 dBm\nrx bitrate: 9999 MBit/s".into()) }
        fn hotspot_rx(&mut self, _iface: &str) -> Option<u64> { Some(0) }
        fn hotspot(&mut self, _iface: &str) -> Hotspot { self.queries += 1; self.hotspot_state.clone() }
        fn apply(&mut self, width: u32) -> Result<(), String> {
            self.writes.push(width);
            if self.failed_apply == Some(width) { return Err("injected width failure".into()); }
            self.actual = width;
            Ok(())
        }
        fn pause(&mut self) { self.pauses += 1; }
        fn probe(&mut self, _gateway: Ipv4Addr) -> Probe {
            self.probes.push(self.actual);
            // 20 MHz 健康、40 MHz 坏，取自 W 上 3 s 洪泛的量级。
            if self.actual == 20 { Probe { sent: 1500, received: 1497, ms: 2000 } } else { Probe { sent: 600, received: 300, ms: 2000 } }
        }
        fn alive(&mut self, _gateway: Ipv4Addr) -> Option<bool> { self.alive.push(self.actual); self.alive_value }
        fn avoided(&mut self, network: &str, signal: i32) -> Vec<(u32, Instant, u8)> {
            self.avoided.iter().filter(|e| e.0 == network && e.2.abs_diff(signal) <= 10).map(|e| (e.1, e.3, e.4)).collect()
        }
        fn avoid(&mut self, network: &str, signal: i32, width: u32, until: Instant, level: u8) {
            self.avoided.retain(|e| !(e.0 == network && e.1 == width));
            self.avoided.push((network.into(), width, signal, until, level));
        }
        fn history(&mut self, _network: &str, _signal: i32) -> Option<width::cache::History> { self.history }
        fn save(&mut self, _network: &str, width: u32, _signal: i32) -> Result<(), String> { self.saved.push(width); Ok(()) }
    }
    fn link(mode: Mode) -> LinkWidth {
        let network = super::super::Network { id: "test-only".into(), ssid: "not-logged".into(), bssid: "not-logged".into(),
            frequency: 2412, signal: -40, security: "open", supported: true };
        let mut link = LinkWidth::new(&network, &[], super::super::saved::Tuning::Off);
        link.mode = mode;
        link
    }
    #[test]
    fn idle_does_not_query_driver_or_recover_from_cached_readback() {
        let mut link = link(Mode::Auto);
        let mut io = Mock::new();
        io.tick(&mut link, 0);
        let queries = io.queries;
        for _ in 0..20 { io.tick(&mut link, 100); }
        assert_eq!(io.queries, queries);
        assert_eq!(io.enable_calls, 0);
        assert_eq!(io.stats_reads, 0);
        assert!(io.writes.is_empty() && io.saved.is_empty());
    }
    #[test]
    fn observe_and_off_do_not_write_even_with_history_and_traffic() {
        for mode in [Mode::Observe, Mode::Off] {
            let mut link = link(mode);
            let mut io = Mock::new();
            io.history = Some(width::cache::History { width: 20, usable: true });
            for _ in 0..30 { io.tick(&mut link, 25_000_000); }
            assert!(io.writes.is_empty() && io.saved.is_empty());
            if mode == Mode::Off { assert_eq!(io.stats_reads, 0); assert_eq!(io.enable_calls, 0); assert_eq!(io.queries, 1); }
            else { assert!(io.stats_reads > 20); assert_eq!(io.enable_calls, 1); }
        }
    }
    #[test]
    fn ll_failure_never_uses_the_high_phy_rate_as_evidence() {
        for enable_ok in [false, true] {
            let mut link = link(Mode::Auto);
            let mut io = Mock::new();
            io.enable_ok = enable_ok;
            io.stats_ok = false;
            for _ in 0..30 { io.tick(&mut link, 25_000_000); }
            assert_eq!(io.actual, 40);
            assert!(io.writes.is_empty() && io.saved.is_empty());
            assert_eq!(io.enable_calls, 1);
        }
    }
    #[test]
    fn loss_of_statistics_during_trial_rolls_back_without_saving_candidate() {
        let mut link = link(Mode::Auto);
        let mut io = Mock::new();
        for _ in 0..6 { io.tick(&mut link, 25_000_000); }
        assert_eq!(io.writes, [20]);
        io.tick(&mut link, 25_000_000);
        io.stats_ok = false;
        for _ in 0..20 { io.tick(&mut link, 25_000_000); }
        assert_eq!(io.actual, 40);
        assert_eq!(io.writes, [20, 40]);
        assert_eq!(io.saved, [40]);
    }
    #[test]
    fn stale_actions_do_not_reach_device_or_cache() {
        let mut link = link(Mode::Auto);
        let mut io = Mock::new();
        io.tick(&mut link, 0);
        link.dispatch(link.generation.wrapping_sub(1), vec![Action::Apply(20), Action::Save { width: 20, signal: -40 }, Action::Hotspot(width::Check::Before)], &mut io);
        assert!(io.writes.is_empty() && io.saved.is_empty());
        assert_eq!(io.queries, 1);
    }
    #[test]
    fn scan_between_confirmation_and_measurement_prevents_a_win() {
        let mut link = link(Mode::Auto);
        let mut io = Mock::new();
        for _ in 0..7 { io.tick(&mut link, 25_000_000); }
        link.scanned();
        for _ in 0..15 { io.tick(&mut link, 50_000_000); }
        assert_eq!(io.writes, [20, 40]);
        assert_eq!(io.saved, [40]);
    }
    #[test]
    fn stale_history_uses_scan_prediction_not_the_old_start() {
        let mut link = link(Mode::Auto);
        let mut io = Mock::new();
        io.history = Some(width::cache::History { width: 20, usable: false });
        io.tick(&mut link, 0);
        assert_eq!(link.width(), Some(40));
        assert!(io.writes.is_empty());
    }
    #[test]
    fn join_reset_goes_through_a_narrower_width_and_confirms_the_negotiated_one() {
        let mut io = Mock::new();
        assert_eq!(reset(&mut io), Ok(Some(40)));
        assert_eq!(io.writes, [20, 40]);
        assert_eq!(io.pauses, 1);
        let mut io = Mock::new();
        io.actual = 20;
        assert_eq!(reset(&mut io), Ok(None));
        assert!(io.writes.is_empty());
        let mut io = Mock::new();
        io.readback = false;
        assert!(reset(&mut io).is_err());
        assert!(io.writes.is_empty());
    }
    #[test]
    fn reapply_writes_the_ceiling_then_the_held_width_and_only_in_auto() {
        for (mode, expected) in [(Mode::Auto, vec![40, 20]), (Mode::Observe, vec![])] {
            let mut link = link(mode);
            let mut io = Mock::new();
            io.tick(&mut link, 0);
            link.dispatch(link.generation, vec![Action::Reapply { width: 20, via: 40 }], &mut io);
            assert_eq!(io.writes, expected);
            assert_eq!(io.pauses, usize::from(mode == Mode::Auto));
            assert!(link.last.is_none());
        }
    }
    #[test]
    fn aligned_endpoints_reject_resets_and_width_crossings() {
        let now = Instant::now();
        let a = counts(now, 100, 100, 100, 100);
        let mut b = counts(now + std::time::Duration::from_secs(3), 1000, 1000, 1000, 300);
        let (sample, reset) = measurement(Some(&a), Some(&b), Some(-40), None);
        assert!(!reset);
        assert_eq!(sample.unwrap().bytes, 900);
        for (rx, tx, ap) in [(1, 1000, 1000), (1000, 1, 1000), (1000, 1000, 1)] {
            let c = counts(b.at, rx, tx, ap, 300);
            assert!(measurement(Some(&a), Some(&c), Some(-40), None).1);
        }
        b.width = 20;
        assert!(measurement(Some(&a), Some(&b), Some(-40), None).0.is_none());
        b.width = 40;
        b.scan = 1;
        assert!(measurement(Some(&a), Some(&b), Some(-40), None).0.unwrap().scanned);
        assert!(measurement(None, Some(&b), Some(-40), None).0.is_none());
    }
    #[test]
    fn a_narrow_anchor_is_refreshed_while_idle_without_other_driver_queries() {
        let mut link = link(Mode::Auto);
        let mut io = Mock::new();
        io.history = Some(width::cache::History { width: 20, usable: true });
        link.gateway = Some(Ipv4Addr::new(192, 168, 2, 1));
        io.tick(&mut link, 0);
        io.tick(&mut link, 0);
        assert_eq!((link.width(), io.writes.clone(), io.alive.clone()), (Some(20), vec![20], vec![20]));
        let queries = io.queries;
        for _ in 0..25 { io.tick(&mut link, 0); }
        assert_eq!(io.writes, [20, 40, 20]);
        assert_eq!(io.pauses, 1);
        assert_eq!(io.alive, [20, 20]);
        assert_eq!(io.queries, queries + 1, "只多一次读回");
        assert_eq!(io.stats_reads, 0);
        assert!(io.probes.is_empty());
    }
    #[test]
    fn probes_need_a_gateway_break_the_counter_endpoint_and_only_run_in_auto() {
        for (mode, gateway, expected) in [(Mode::Auto, None, 0), (Mode::Auto, Some([192, 168, 2, 1]), 1), (Mode::Observe, Some([192, 168, 2, 1]), 0)] {
            let mut link = link(mode);
            let mut io = Mock::new();
            io.tick(&mut link, 0);
            link.gateway = gateway.map(Ipv4Addr::from);
            link.last = Some(counts(io.now, 0, 0, 0, 100));
            link.dispatch(link.generation, vec![Action::Probe(width::Purpose::Candidate)], &mut io);
            assert_eq!(io.probes.len(), expected);
            assert_eq!(link.last.is_none(), mode == Mode::Auto);
        }
    }
    #[test]
    fn a_challenge_on_the_device_path_probes_reference_and_candidate_and_checks_the_gateway_after_rollback() {
        let mut link = link(Mode::Auto);
        let mut io = Mock::new();
        io.history = Some(width::cache::History { width: 20, usable: true });
        link.gateway = Some(Ipv4Addr::new(192, 168, 2, 1));
        io.tick(&mut link, 0);
        for _ in 0..12 { io.tick(&mut link, 25_000_000); }
        assert_eq!(io.writes, [20, 40, 20]);
        assert_eq!(io.probes, [20, 40]);
        // 连接收窄后、回滚后各检查一次网关。
        assert_eq!(io.alive, [20, 20]);
        assert_eq!(io.saved, [20]);
    }
    #[test]
    fn a_broken_width_is_remembered_across_associations_and_lowers_the_start() {
        let mut io = Mock::new();
        let mut first = link(Mode::Auto);
        first.gateway = Some(Ipv4Addr::new(192, 168, 2, 1));
        io.history = Some(width::cache::History { width: 20, usable: true });
        io.tick(&mut first, 0);
        for _ in 0..12 { io.tick(&mut first, 25_000_000); }
        assert_eq!(io.writes, [20, 40, 20]);
        assert_eq!(io.avoided.iter().map(|e| (e.1, e.4)).collect::<Vec<_>>(), [(40, 1)]);
        // 重新关联、没有可用历史：扫描预测本是 40，因正在退避直接从 20 开始，之后也不再挑战 40。
        io.history = None;
        io.writes.clear();
        io.actual = 40;
        let mut second = link(Mode::Auto);
        second.gateway = first.gateway;
        io.tick(&mut second, 0);
        // 15 个 tick 足够完成锚点窗口并决定挑战，又早于 60 s 刷新。
        for _ in 0..15 { io.tick(&mut second, 25_000_000); }
        assert_eq!(io.writes, [20]);
        assert_eq!(io.probes, [20, 40], "第二次关联没有再探测");
    }

    #[test]
    fn hotspot_pause_survives_a_partial_gateway_recovery_toggle_on_device_path() {
        for alive in [Some(true), Some(false), None] {
            let mut link = link(Mode::Auto);
            let mut io = Mock::new();
            io.history = Some(width::cache::History { width: 20, usable: true });
            link.gateway = Some(Ipv4Addr::new(192, 168, 2, 1));
            io.tick(&mut link, 0);
            for _ in 0..20 {
                io.tick(&mut link, 25_000_000);
                if io.probes == [20] { break; }
            }
            assert_eq!(io.probes, [20]);
            assert_eq!(io.actual, 40);
            io.hotspot_state = Hotspot::parse("state=ENABLED", "channel 1 (2412 MHz), width: 40 MHz, center1: 2422 MHz", None);
            io.tick(&mut link, 25_000_000);
            assert_eq!(io.actual, 20);
            io.alive_value = Some(false);
            io.failed_apply = Some(20);
            io.tick(&mut link, 25_000_000);
            assert_eq!(io.actual, 40, "the recovery toggle stopped at via");
            assert_eq!(link.state(), Some("Paused: hotspot changed"));
            io.failed_apply = None;
            io.alive_value = alive;
            io.tick(&mut link, 25_000_000);
            io.tick(&mut link, 25_000_000);
            assert_eq!(link.width(), Some(40));
            assert_eq!(link.state(), Some("Paused: hotspot changed"));
            assert_eq!(io.alive, if alive == Some(false) { vec![20, 20, 40, 40] } else { vec![20, 20, 40] });
            let (writes, saves) = (io.writes.len(), io.saved.len());
            io.now += Duration::from_secs(901);
            for _ in 0..30 { io.tick(&mut link, 25_000_000); }
            assert_eq!((io.writes.len(), io.saved.len()), (writes, saves));
            assert_eq!(link.state(), Some("Paused: hotspot changed"));
        }
    }

    #[test]
    fn gateway_recovery_continues_without_width_readback_on_device_path() {
        for alive in [Some(true), Some(false), None] {
            let mut link = link(Mode::Auto);
            let mut io = Mock::new();
            io.history = Some(width::cache::History { width: 20, usable: true });
            link.gateway = Some(Ipv4Addr::new(192, 168, 2, 1));
            io.tick(&mut link, 0);
            for _ in 0..20 {
                io.tick(&mut link, 25_000_000);
                if io.probes == [20, 40] { break; }
            }
            assert_eq!(io.probes, [20, 40]);
            assert_eq!(io.actual, 20);
            io.alive_value = Some(false);
            io.tick(&mut link, 25_000_000);
            assert_eq!(io.alive, [20, 20]);
            io.readback = false;
            io.alive_value = alive;
            io.tick(&mut link, 25_000_000);
            assert_eq!(io.alive, [20, 20, 20]);
            io.tick(&mut link, 25_000_000);
            assert_eq!(io.alive.len(), if alive == Some(false) { 4 } else { 3 });
            if alive == Some(false) { assert_eq!(link.state(), Some("Data path unconfirmed")); }
            let checks = io.alive.len();
            io.readback = true;
            io.tick(&mut link, 25_000_000);
            assert_eq!(io.alive.len(), checks);
        }
    }
}
