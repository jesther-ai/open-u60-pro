//! 固定锚点、单候选挑战；只比较相邻窗口的共享接收空口效率代理。
//! 到网关的洪泛 ping 只判断候选宽度坏没坏，不参与比较快慢。
//! 此模块没有设备 I/O。watcher 带关联代次执行动作，再把读回结果送回来。

pub(super) mod cache;
mod hotspot;
pub(super) use hotspot::Hotspot;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

const REVIEW: Duration = Duration::from_secs(15 * 60);
const TRIAL_LIMIT: Duration = Duration::from_secs(45);
const APPLY_LIMIT: Duration = Duration::from_secs(18);
const APPLY_RETRY: Duration = Duration::from_secs(6);
const GAP: Duration = Duration::from_secs(6);
const WINDOW_LIMIT: Duration = Duration::from_secs(30);
/// B31 静默丢弃保持中的 `iwpriv chwidth` 窄档（2.4 GHz 约 100 s 后下行塌到约 3 Mbit/s，
/// 5 GHz 约 190–260 s 后回到宽档），读回不变；同值重发无效，只有改变宽度才重新生效。
const REAPPLY: Duration = Duration::from_secs(60);
/// 洪泛 ping 的在途包数（watcher 的 `ping -l`）；截止时仍在途的不算丢。
pub(super) const PROBE_PRELOAD: u32 = 8;
/// 改宽度后网关不回 ARP 时，最多再“切走再切回”两次。
const VERIFY_TOGGLES: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode { Auto, Observe, Off }
impl Mode {
    pub(super) fn parse(value: &str) -> Self {
        match value { "auto" => Self::Auto, "observe" => Self::Observe, _ => Self::Off }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Sample {
    pub seconds: f64,
    pub bytes: u64,
    pub airtime_ms: u64,
    pub hotspot_bytes: u64,
    pub tx_bytes: u64,
    pub signal: i32,
    pub rx_mcs: Option<u32>,
    pub scanned: bool,
    pub radio: u32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Score {
    pub e: f64,
    pub r: f64,
    hotspot_r: f64,
    signal: f64,
    scanned: bool,
    seconds: f64,
    airtime_ms: u64,
    tx_r: f64,
    /// 窗口内逐 tick R 的变异系数（标准差/均值）。
    spread: f64,
}
impl Score {
    /// 逐 tick 需求平稳。测速时波动约 0.1–0.2；时断时续的浏览远超 1，这时 E 随突发起落，不可比。
    fn steady(self) -> bool { self.spread <= 0.5 }
    fn suspicious(self, other: Self) -> bool {
        (other.r < 0.8 * self.r && other.e > self.e)
            || (self.hotspot_r.max(other.hotspot_r) > 2.0 * self.hotspot_r.min(other.hotspot_r)
                && (self.hotspot_r - other.hotspot_r).abs() > 5.0)
            || (self.signal - other.signal).abs() > 6.0
            || self.scanned || other.scanned
            || self.load_changed(other)
    }
    /// R/E 即本 STA 的接收空口占比。两窗都远没用满时 R 反映需求而非宽度，E 随需求升降；
    /// 真机 W_Game：锚点 3 Mbit/s、候选 22 Mbit/s，40 MHz 凭虚高的 E 判了 Won。
    /// 用满时 R 的差距是真实容量（W 满载 20 对 40：R 3.7 倍），照常比较。
    fn load_changed(self, other: Self) -> bool {
        self.r < 0.25 * self.e && other.r < 0.25 * other.e && self.r.max(other.r) > 2.0 * self.r.min(other.r)
    }
    fn compare(self, other: Self) -> ResultKind {
        if self.suspicious(other) { ResultKind::Unknown }
        // 只拦胜：坏宽度本身就会让交付忽高忽低，不能因此放过真实的负。
        else if other.e >= 1.15 * self.e { if self.steady() && other.steady() { ResultKind::Won } else { ResultKind::Unknown } }
        else { ResultKind::Lost }
    }
    fn evidence(self) -> String {
        format!("E={:.2} R={:.2} seconds={:.1} airtime_ms={} hotspot_R={:.2} tx_R={:.2} signal={:.1} scan={} spread={:.2}",
            self.e, self.r, self.seconds, self.airtime_ms, self.hotspot_r, self.tx_r, self.signal, self.scanned, self.spread)
    }
}

/// 到网关的 2 s 洪泛 ping。只说明这一跳通不通，不是测速：回包率受往返时延限制，
/// 两个都正常的宽度比不出快慢。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Probe { pub sent: u32, pub received: u32, pub ms: u32 }
impl Probe {
    fn loss(self) -> f64 {
        f64::from(self.sent.saturating_sub(self.received).saturating_sub(PROBE_PRELOAD)) / f64::from(self.sent.max(1))
    }
    fn rate(self) -> f64 { f64::from(self.received) / f64::from(self.ms.max(1)) }
    /// 网关不回 ICMP、限速或锚点本身就丢包时，本次挑战不靠探测下结论。
    fn usable(self) -> bool { self.sent >= 50 && self.loss() < 0.05 }
    /// 真机 W（2.4 GHz）：20 MHz 丢 0.2%；坏的 40 MHz 丢 25–67%，回包只有 20 MHz 的 5–29%。
    fn fails(self, reference: Self) -> bool { self.loss() >= 0.10 || self.rate() < 0.3 * reference.rate() }
    fn evidence(self) -> String { format!("sent={} received={} ms={}", self.sent, self.received, self.ms) }
}

#[derive(Default, Debug)]
struct Window {
    start: Option<Instant>,
    last: Option<Instant>,
    qualified: f64,
    bytes: u64,
    airtime_ms: u64,
    hotspot_bytes: u64,
    tx_bytes: u64,
    signal_sum: f64,
    signal_time: f64,
    scanned: bool,
    ticks: u32,
    rate_sum: f64,
    rate_squares: f64,
}
impl Window {
    fn expired(&self, now: Instant) -> bool {
        self.start.is_some_and(|at| now.duration_since(at) >= WINDOW_LIMIT)
            || self.last.is_some_and(|at| now.duration_since(at) >= GAP)
    }
    fn push(&mut self, now: Instant, s: Sample) -> Option<Score> {
        if !s.seconds.is_finite() || s.seconds <= 0.0 || s.seconds > GAP.as_secs_f64() { return None; }
        self.scanned |= s.scanned;
        if s.airtime_ms == 0 { return None; }
        self.start.get_or_insert(now - Duration::from_secs_f64(s.seconds));
        self.bytes += s.bytes;
        self.airtime_ms += s.airtime_ms;
        self.hotspot_bytes += s.hotspot_bytes;
        self.tx_bytes += s.tx_bytes;
        self.signal_sum += f64::from(s.signal) * s.seconds;
        self.signal_time += s.seconds;
        self.scanned |= s.scanned;
        if s.airtime_ms > 0 {
            self.qualified += s.seconds;
            self.last = Some(now);
            let rate = s.bytes as f64 * 8.0 / s.seconds / 1e6;
            self.ticks += 1;
            self.rate_sum += rate;
            self.rate_squares += rate * rate;
        }
        let score = self.score(now)?;
        (self.qualified >= 12.0 && self.airtime_ms >= 100 && score.r >= 1.0).then_some(score)
    }
    fn score(&self, now: Instant) -> Option<Score> {
        let seconds = now.duration_since(self.start?).as_secs_f64();
        if self.airtime_ms == 0 || seconds <= 0.0 || self.signal_time <= 0.0 { return None; }
        let mean = self.rate_sum / f64::from(self.ticks.max(1));
        let spread = if mean > 0.0 { (self.rate_squares / f64::from(self.ticks.max(1)) - mean * mean).max(0.0).sqrt() / mean } else { 0.0 };
        Some(Score {
            spread,
            e: self.bytes as f64 * 0.008 / self.airtime_ms as f64,
            r: self.bytes as f64 * 8.0 / seconds / 1e6,
            hotspot_r: self.hotspot_bytes as f64 * 8.0 / seconds / 1e6,
            tx_r: self.tx_bytes as f64 * 8.0 / seconds / 1e6,
            signal: self.signal_sum / self.signal_time,
            scanned: self.scanned, seconds, airtime_ms: self.airtime_ms,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResultKind { Untested, Won, Lost, Unknown }
#[derive(Debug)]
struct Candidate {
    width: u32,
    result: ResultKind,
    at: Option<Instant>,
    next_ok: Option<Instant>,
    loss_level: u8,
    unknown_level: u8,
    broken_level: u8,
}
impl Candidate {
    fn new(width: u32) -> Self {
        Self { width, result: ResultKind::Untested, at: None, next_ok: None, loss_level: 0, unknown_level: 0, broken_level: 0 }
    }
    /// 健康探测判坏：每次试探都让用户停几秒，按 1/2/4/8/24 h 退避。返回新级别与小时数。
    fn record_broken(&mut self, now: Instant) -> (u8, u64) {
        let hours = [1, 2, 4, 8, 24][usize::from(self.broken_level.min(4))];
        self.broken_level = (self.broken_level + 1).min(5);
        self.result = ResultKind::Lost;
        self.at = Some(now);
        self.next_ok = Some(now + Duration::from_secs(hours * 3600));
        (self.broken_level, hours)
    }
    fn record(&mut self, now: Instant, result: ResultKind, operation: bool) {
        self.result = result;
        self.at = Some(now);
        let minutes = match result {
            ResultKind::Lost => {
                let minutes = [15, 30, 60, 120, 240][usize::from(self.loss_level.min(4))];
                self.loss_level = (self.loss_level + 1).min(4);
                self.unknown_level = 0;
                minutes
            }
            ResultKind::Unknown => {
                let minutes = [5, 10, 20, 40, 60][usize::from(self.unknown_level.min(4))];
                self.unknown_level = (self.unknown_level + 1).min(4);
                minutes
            }
            _ => { self.unknown_level = 0; 15 }
        };
        self.next_ok = Some(now + Duration::from_secs(if operation { minutes.max(15) } else { minutes } * 60));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Check { Before, Confirmed, End, RolledBack }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pause { Unconfirmed, Hotspot }
#[derive(Debug)]
struct Applying { began: Instant, attempted: Instant, attempts: u8 }
impl Applying {
    fn new(now: Instant) -> Self { Self { began: now, attempted: now, attempts: 1 } }
    fn retry(&mut self, now: Instant) -> bool {
        if self.attempts >= 3 || now.duration_since(self.attempted) < APPLY_RETRY { return false; }
        self.attempts += 1;
        self.attempted = now;
        true
    }
}
#[derive(Debug)]
enum Phase {
    Stable,
    Checking(Check),
    /// 挑战前在锚点探测一次，作为候选的参照。
    Referencing,
    Applying(Applying),
    Settling(u8),
    Measuring,
    RollingBack(Applying),
    /// 收窄、刷新或回滚已读回；下一 tick 确认数据路径，参数为已做的“切走再切回”次数。
    /// 真机两次见过读回正确但网关不通，再“切走再切回”一次即恢复。
    Verifying(u8),
    Paused(Pause),
}
#[derive(Debug)]
struct Trial {
    target: u32,
    baseline: Option<Score>,
    deadline: Instant,
    last_traffic: Instant,
    hotspot: Hotspot,
    result: ResultKind,
    operation: bool,
    score: Option<Score>,
    low_ticks: u8,
    low_ms: [u64; 2],
    reference: Option<Probe>,
    probes: u8,
    suspect: u8,
    /// 因候选探测失败而结束。
    broken: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Purpose { Reference, Candidate }
#[derive(Debug)]
pub(super) enum Event {
    Tick { channel: Option<(u32, u32)>, sample: Option<Sample>, reset: bool },
    Hotspot { check: Check, snapshot: Hotspot },
    Applied { width: u32, ok: bool },
    /// `None`：没有网关可探测。
    Probed { purpose: Purpose, result: Option<Probe> },
    /// `None`：无法检查（没有网关或 `ping`）。
    Alive { toggles: u8, alive: Option<bool> },
}
#[derive(Debug)]
pub(super) enum Action {
    Apply(u32),
    /// 先设 `via` 再设回 `width`，重新施加保持中的窄档。
    Reapply { width: u32, via: u32 },
    Hotspot(Check),
    /// 到网关的洪泛 ping。
    Probe(Purpose),
    /// 到网关 ping 一个回包（最多 2 s）。不能用 ARP：真机坏状态下网关照回 ARP，普通数据帧却收不到。
    Alive(u8),
    /// 探测判坏的宽度：watcher 在本进程内跨重连记住，下次关联直接避开。
    Avoid { width: u32, until: Instant, level: u8 },
    Save { width: u32, signal: i32 },
    Log(String),
}

pub(super) struct Controller {
    gen: u64,
    mode: Mode,
    primary: u32,
    negotiated: u32,
    actual: u32,
    anchor: u32,
    anchor_score: Option<Score>,
    phase: Phase,
    trial: Option<Trial>,
    proposal: Option<(u32, Option<Score>)>,
    initial: Option<u32>,
    external: Option<u32>,
    history_hint: Option<u32>,
    candidates: Vec<Candidate>,
    budget: VecDeque<Instant>,
    last_challenge: Option<Instant>,
    radio: Option<u32>,
    window: Window,
    stable_settle: u8,
    saved_anchor: bool,
    hotspot_paused: bool,
    held: Option<Instant>,
    blocked: bool,
    /// 本次关联网关是否回过 ICMP；`Some(false)`：绕过一次仍不回，不再检查数据路径。
    icmp: Option<bool>,
    state: &'static str,
}
impl Controller {
    pub(super) fn new(gen: u64, mode: Mode, primary: u32, negotiated: u32, start: u32, history: Option<u32>) -> Self {
        let legal = legal(primary, negotiated);
        let initial = (mode == Mode::Auto && start != negotiated && legal.contains(&start)).then_some(start);
        Self {
            gen, mode, primary, negotiated, actual: negotiated, anchor: negotiated, anchor_score: None,
            phase: Phase::Stable, trial: None, proposal: None, initial, external: None, history_hint: history,
            candidates: legal.into_iter().map(Candidate::new).collect(), budget: VecDeque::new(), last_challenge: None,
            radio: None, window: Window::default(), stable_settle: 0, saved_anchor: false, hotspot_paused: false, held: None,
            blocked: false, icmp: None,
            state: if history == Some(start) && mode == Mode::Auto { "From history" } else { "Waiting for traffic" },
        }
    }
    pub(super) fn generation(&self) -> u64 { self.gen }
    pub(super) fn width(&self) -> u32 { self.actual }
    pub(super) fn primary(&self) -> u32 { self.primary }
    pub(super) fn status(&self) -> Option<&'static str> {
        match self.mode { Mode::Off => None, Mode::Observe => Some("Observing"), Mode::Auto => Some(self.state) }
    }
    pub(super) fn active(&self) -> bool {
        self.initial.is_some() || matches!(self.phase, Phase::Applying(_) | Phase::Settling(_) | Phase::Measuring | Phase::RollingBack(_)
            | Phase::Checking(_) | Phase::Referencing | Phase::Verifying(_))
    }
    /// 保持窄档时按时重设，空闲也做：收不到数据的链路看起来和空闲一样，不能因此停止重设。
    pub(super) fn refresh_due(&self, now: Instant) -> bool {
        self.mode == Mode::Auto && matches!(self.phase, Phase::Stable) && self.trial.is_none()
            && self.anchor < self.negotiated && self.actual == self.anchor
            && self.held.is_none_or(|at| now.duration_since(at) >= REAPPLY)
    }
    pub(super) fn collecting(&self) -> bool { self.window.start.is_some() }
    /// 上次关联探测判坏的宽度，到 `until` 前不挑战；级别延续，再坏就退避更久。
    pub(super) fn avoid(&mut self, width: u32, until: Instant, level: u8) {
        if let Some(c) = self.candidates.iter_mut().find(|c| c.width == width) {
            c.result = ResultKind::Lost;
            c.broken_level = level;
            c.next_ok = Some(until);
        }
    }
    pub(super) fn scanned(&mut self) {
        self.window.scanned = true;
        if let Some(baseline) = self.trial.as_mut().and_then(|t| t.baseline.as_mut()) { baseline.scanned = true; }
    }
    pub(super) fn idle(&mut self, now: Instant, reset: bool) {
        if reset { self.reset_epoch(false); }
        if matches!(self.phase, Phase::Stable) && self.window.expired(now) {
            self.window = Window::default();
            self.state = "Not enough evidence";
        }
    }
    pub(super) fn initialize(&mut self) -> Vec<Action> {
        if let Some(target) = self.initial.take() { self.propose(target, None) } else { Vec::new() }
    }
    pub(super) fn handle(&mut self, gen: u64, now: Instant, event: Event) -> Vec<Action> {
        if gen != self.gen || self.mode == Mode::Off { return Vec::new(); }
        match event {
            Event::Hotspot { check, snapshot } => self.hotspot(now, check, snapshot),
            Event::Applied { width, ok } => {
                if !ok && matches!(self.phase, Phase::Applying(_)) && self.trial.as_ref().is_some_and(|t| t.target == width) {
                    self.finish(now, ResultKind::Unknown, true, "command rejected")
                } else { Vec::new() }
            }
            Event::Tick { channel, sample, reset } => self.tick(now, channel, sample, reset),
            Event::Probed { purpose, result } => self.probed(now, purpose, result),
            Event::Alive { toggles, alive } => self.alive(now, toggles, alive),
        }
    }
    fn reset_epoch(&mut self, history: bool) {
        self.window = Window::default();
        self.anchor_score = None;
        if history { self.candidates = legal(self.primary, self.negotiated).into_iter().map(Candidate::new).collect(); }
    }
    fn candidate(&self, now: Instant) -> Option<u32> {
        let index = |w| [160u32, 80, 40, 20].iter().position(|&v| v == w).unwrap_or(0);
        self.candidates.iter().filter(|c| c.width != self.anchor && c.next_ok.is_none_or(|at| now >= at))
            .min_by_key(|c| (c.result != ResultKind::Untested, index(c.width).abs_diff(index(self.anchor)),
                std::cmp::Reverse(c.width), c.at, self.history_hint != Some(c.width)))
            .map(|c| c.width)
    }
    fn propose(&mut self, target: u32, baseline: Option<Score>) -> Vec<Action> {
        self.proposal = Some((target, baseline));
        self.phase = Phase::Checking(Check::Before);
        vec![Action::Hotspot(Check::Before)]
    }
    fn tick(&mut self, now: Instant, channel: Option<(u32, u32)>, sample: Option<Sample>, reset: bool) -> Vec<Action> {
        let channel = channel.filter(|&(primary, actual)| chwidth_code(actual).is_some() && (primary >= 3000 || actual <= 40));
        let mut changed_epoch = reset;
        if let Some((primary, actual)) = channel {
            let topology = primary != self.primary;
            let ceiling = actual > self.negotiated;
            self.primary = primary;
            self.negotiated = self.negotiated.max(actual);
            if topology || ceiling { self.reset_epoch(true); changed_epoch = true; }
            if legal(primary, self.negotiated).contains(&actual) {
                self.actual = actual;
                if matches!(self.phase, Phase::Paused(Pause::Unconfirmed)) {
                    self.saved_anchor &= self.anchor == actual;
                    self.anchor = actual;
                    self.held = Some(now);
                    self.reset_epoch(false);
                    self.trial = None;
                    self.phase = if self.hotspot_paused { Phase::Paused(Pause::Hotspot) } else { Phase::Stable };
                    self.state = if self.hotspot_paused { "Paused: hotspot changed" } else { "Waiting for traffic" };
                    return vec![Action::Log(format!("readback recovered width={actual}"))];
                }
                let expected = self.trial.as_ref().map_or(self.anchor, |t| t.target);
                if !matches!(self.phase, Phase::Applying(_) | Phase::RollingBack(_) | Phase::Paused(_) | Phase::Checking(_) | Phase::Referencing) && actual != expected {
                    if self.trial.is_some() {
                        // 先检查原挑战的热点，再决定接受外部宽度还是回滚旧锚点。
                        self.external = Some(actual);
                        return self.finish(now, ResultKind::Unknown, false, "external width during trial");
                    }
                    self.saved_anchor &= self.anchor == actual;
                    self.anchor = actual;
                    self.held = Some(now);
                    self.trial = None;
                    self.reset_epoch(ceiling || topology);
                    self.state = if self.hotspot_paused { "Paused: hotspot changed" } else { "Waiting for traffic" };
                    let mut actions = vec![Action::Log(format!("external width readback={actual}"))];
                    // A failed recovery toggle may leave us at via. Keep its retry
                    // count and gateway check; completing it must preserve the pause.
                    if let Phase::Verifying(toggles) = self.phase {
                        actions.push(Action::Alive(toggles));
                    } else { self.settle(); }
                    return actions;
                }
                if !legal(primary, self.negotiated).contains(&self.anchor) {
                    self.saved_anchor &= self.anchor == actual;
                    self.anchor = actual;
                    self.trial = None;
                    self.phase = Phase::Stable;
                    self.reset_epoch(true);
                    return Vec::new();
                }
            }
        }
        if let Some(s) = sample {
            if self.radio.is_some_and(|radio| radio != s.radio) { self.reset_epoch(true); changed_epoch = true; }
            self.radio = Some(s.radio);
        }
        if changed_epoch {
            self.reset_epoch(false);
            if self.trial.is_some() && !matches!(self.phase, Phase::RollingBack(_) | Phase::Paused(_)) {
                return self.finish(now, ResultKind::Unknown, false, "epoch changed");
            }
        }
        if let Phase::RollingBack(apply) = &mut self.phase {
            if channel.is_some_and(|(_, w)| w == self.anchor) {
                self.phase = Phase::Checking(Check::RolledBack);
                return vec![Action::Hotspot(Check::RolledBack)];
            }
            if now.duration_since(apply.began) >= APPLY_LIMIT {
                self.phase = Phase::Paused(Pause::Unconfirmed);
                self.state = "Cannot confirm";
                return vec![Action::Log(format!("rollback unconfirmed anchor={} readback={channel:?}", self.anchor))];
            }
            return if apply.retry(now) { vec![Action::Apply(self.anchor)] } else { Vec::new() };
        }
        if matches!(self.phase, Phase::Paused(_)) { return Vec::new(); }
        if self.trial.as_ref().is_some_and(|t| now >= t.deadline) {
            return self.finish(now, ResultKind::Unknown, false, "trial deadline");
        }
        // 参照探测没有执行（动作被丢弃）时不卡住挑战。
        if matches!(self.phase, Phase::Referencing) { return self.referenced(now, None); }
        if let Phase::Verifying(toggles) = self.phase {
            if self.icmp == Some(false) { self.settle(); return Vec::new(); }
            // Ping does not need an iw readback. A missing readback is not recovery.
            return vec![Action::Alive(toggles)];
        }
        if let Phase::Applying(apply) = &mut self.phase {
            let target = self.trial.as_ref().unwrap().target;
            if channel.is_some_and(|(_, w)| w == target) {
                self.phase = Phase::Checking(Check::Confirmed);
                return vec![Action::Hotspot(Check::Confirmed)];
            }
            if now.duration_since(apply.began) >= APPLY_LIMIT {
                return self.finish(now, ResultKind::Unknown, true, "apply unconfirmed");
            }
            return if apply.retry(now) { vec![Action::Apply(target)] } else { Vec::new() };
        }
        if channel.is_none() {
            self.window = Window::default();
            return if self.trial.is_some() { self.finish(now, ResultKind::Unknown, false, "readback unavailable") } else { Vec::new() };
        }
        let sample = sample.filter(|s| !changed_epoch && s.seconds.is_finite() && s.seconds > 0.0 && s.seconds <= GAP.as_secs_f64());
        if self.trial.is_some() && sample.is_some_and(|s| s.bytes == 0) {
            return self.finish(now, ResultKind::Unknown, false, "no delivered bytes");
        }
        if let Some(t) = self.trial.as_mut() {
            if sample.is_some_and(|s| s.bytes > 0) { t.last_traffic = now; }
            else if now.duration_since(t.last_traffic) >= GAP {
                return self.finish(now, ResultKind::Unknown, false, "traffic stopped");
            }
        }
        let trying = matches!(self.phase, Phase::Settling(_) | Phase::Measuring);
        if let (true, Some(s), Some(t)) = (trying, sample, self.trial.as_mut()) {
            // 坏宽度上的交付常低于 1 Mbit/s，凑不满 settling，原来只能等到 45 s 期限。
            let slow = !busy(s.seconds, s.bytes) && t.baseline.is_some_and(|b| b.r >= 2.0);
            let degraded = busy(s.seconds, s.bytes) && s.signal >= -55 && s.rx_mcs.is_some_and(|m| m <= 1);
            t.suspect = if slow || degraded { t.suspect + 1 } else { 0 };
            if t.suspect >= 2 {
                t.suspect = 0;
                if t.reference.is_some() && t.probes < 2 {
                    t.probes += 1;
                    return vec![Action::Probe(Purpose::Candidate)];
                }
                // 只有 MCS 偏低时交付仍够，交给 E 判断。
                if slow { return self.finish(now, ResultKind::Unknown, false, "delivery collapsed"); }
            }
        }
        if let Phase::Settling(n) = &mut self.phase {
            if sample.is_some_and(|s| busy(s.seconds, s.bytes)) {
                *n -= 1;
                if *n == 0 { self.phase = Phase::Measuring; self.window = Window::default(); }
            }
            return Vec::new();
        }
        if matches!(self.phase, Phase::Checking(_)) { return Vec::new(); }
        if self.refresh_due(now) {
            // 真机只验证过经协商上限绕回能重新生效（5 GHz 160→80、2.4 GHz 40→20）。
            let via = self.negotiated;
            self.held = Some(now);
            self.window = Window::default();
            self.stable_settle = 1;
            let mut actions = vec![Action::Log(format!("reapply width={} via={via}", self.anchor)), Action::Reapply { width: self.anchor, via }];
            actions.extend(self.verify());
            return actions;
        }
        if self.window.expired(now) {
            self.window = Window::default();
            if matches!(self.phase, Phase::Measuring) { return self.finish(now, ResultKind::Unknown, false, "window expired"); }
            self.state = "Not enough evidence";
        }
        let Some(s) = sample else {
            if let Some(t) = self.trial.as_mut() { t.low_ticks = 0; t.low_ms = [0, 0]; }
            return Vec::new();
        };
        if self.stable_settle > 0 {
            if busy(s.seconds, s.bytes) { self.stable_settle -= 1; }
            return Vec::new();
        }
        let complete = self.window.push(now, s);
        if matches!(self.phase, Phase::Measuring) {
            let t = self.trial.as_mut().unwrap();
            let baseline = t.baseline.unwrap();
            if s.airtime_ms > 0 && busy(s.seconds, s.bytes) && s.bytes as f64 * 0.008 / (s.airtime_ms as f64) < baseline.e * 0.5 {
                t.low_ticks = (t.low_ticks + 1).min(2);
                t.low_ms = [t.low_ms[1], s.airtime_ms];
            } else { t.low_ticks = 0; t.low_ms = [0, 0]; }
            if let Some(score) = complete {
                t.score = Some(score);
                return self.finish(now, baseline.compare(score), false, "window complete");
            }
            if t.low_ticks >= 2 && t.low_ms.iter().sum::<u64>() >= 100 {
                let score = self.window.score(now).unwrap();
                t.score = Some(score);
                let result = if baseline.suspicious(score) { ResultKind::Unknown } else { ResultKind::Lost };
                return self.finish(now, result, false, "early stop");
            }
            return Vec::new();
        }
        let Some(score) = complete else { return Vec::new() };
        self.window = Window::default();
        // 只是需求降了导致 E 掉一半，不算塌陷，不提前挑战。
        let collapsed = self.anchor_score.is_some_and(|a| score.e < a.e * 0.5 && !a.load_changed(score));
        self.anchor_score = Some(score);
        self.state = "Settled";
        let mut actions = Vec::new();
        if self.mode == Mode::Observe {
            actions.push(Action::Log(format!("observed window width={} {}", self.anchor, score.evidence())));
        }
        if self.mode == Mode::Auto && !self.saved_anchor {
            self.saved_anchor = true;
            actions.push(Action::Save { width: self.anchor, signal: score.signal.round() as i32 });
            actions.push(Action::Log(format!("anchor={} {}", self.anchor, score.evidence())));
        }
        while self.budget.front().is_some_and(|at| now.duration_since(*at) >= REVIEW) { self.budget.pop_front(); }
        if self.blocked || self.budget.len() >= 2 || (!collapsed && self.last_challenge.is_some_and(|at| now.duration_since(at) < REVIEW)) { return actions; }
        // 需求忽高忽低时不挑战，也不占预算；等下一个平稳的锚点窗口。
        if !score.steady() {
            actions.push(Action::Log("challenge deferred: traffic not steady".into()));
            return actions;
        }
        if let Some(target) = self.candidate(now) {
            if self.mode == Mode::Observe {
                self.last_challenge = Some(now);
                self.budget.push_back(now);
                actions.push(Action::Log(format!("would challenge {} -> {target} {}", self.anchor, score.evidence())));
            } else { actions.extend(self.propose(target, Some(score))); }
        }
        actions
    }
    fn finish(&mut self, _now: Instant, result: ResultKind, operation: bool, reason: &str) -> Vec<Action> {
        let Some(t) = self.trial.as_mut() else { return Vec::new() };
        t.result = result;
        t.operation = operation;
        self.window = Window::default();
        self.phase = Phase::Checking(Check::End);
        vec![Action::Log(format!("{} -> {} {result:?} reason={reason} A=[{}] B=[{}]", self.anchor, t.target,
            t.baseline.map_or_else(|| "initial".into(), Score::evidence), t.score.map_or_else(|| "missing".into(), Score::evidence))), Action::Hotspot(Check::End)]
    }
    fn rollback(&mut self, now: Instant) -> Vec<Action> {
        self.external = None;
        self.window = Window::default();
        self.phase = Phase::RollingBack(Applying::new(now));
        self.state = if self.hotspot_paused { "Paused: hotspot changed" } else { "Comparing" };
        vec![Action::Apply(self.anchor)]
    }
    fn settle(&mut self) {
        self.phase = if self.hotspot_paused { Phase::Paused(Pause::Hotspot) } else { Phase::Stable };
    }
    /// 改宽度读回后立即 ping 网关；失败时下一 tick 起再查。
    fn verify(&mut self) -> Vec<Action> {
        if self.icmp == Some(false) { self.settle(); return Vec::new(); }
        self.phase = Phase::Verifying(0);
        vec![Action::Alive(0)]
    }
    fn referenced(&mut self, now: Instant, result: Option<Probe>) -> Vec<Action> {
        let Some(t) = self.trial.as_mut() else { return Vec::new() };
        if result.is_some_and(|p| p.received > 0) { self.icmp = Some(true); }
        let reference = result.filter(|p| p.usable());
        t.reference = reference;
        // 探测占用墙钟并清空计数端点；不能把这段时间算成用户流量停了。
        t.last_traffic = now;
        let target = t.target;
        self.phase = Phase::Applying(Applying::new(now));
        let mut actions = Vec::new();
        if let Some(p) = result {
            actions.push(Action::Log(format!("probe reference width={} {} usable={}", self.anchor, p.evidence(), reference.is_some())));
        }
        actions.push(Action::Log(format!("apply {} -> {target} mode={:?}", self.anchor, self.mode)));
        actions.push(Action::Apply(target));
        actions
    }
    fn probed(&mut self, now: Instant, purpose: Purpose, result: Option<Probe>) -> Vec<Action> {
        match purpose {
            Purpose::Reference if matches!(self.phase, Phase::Referencing) => self.referenced(now, result),
            Purpose::Candidate if matches!(self.phase, Phase::Settling(_) | Phase::Measuring) => {
                let Some(t) = self.trial.as_ref() else { return Vec::new() };
                let (Some(reference), Some(p)) = (t.reference, result) else { return Vec::new() };
                let target = t.target;
                if p.received > 0 { self.icmp = Some(true); }
                // 洪泛的回包不能进任何窗口。
                self.window = Window::default();
                let evidence = format!("width={target} {} reference=[{}]", p.evidence(), reference.evidence());
                let t = self.trial.as_mut().unwrap();
                if p.fails(reference) {
                    t.broken = true;
                    return self.finish(now, ResultKind::Lost, false, &format!("probe failed {evidence}"));
                }
                t.last_traffic = now;
                vec![Action::Log(format!("probe passed {evidence}"))]
            }
            _ => Vec::new(),
        }
    }
    fn alive(&mut self, now: Instant, toggles: u8, alive: Option<bool>) -> Vec<Action> {
        if !matches!(self.phase, Phase::Verifying(n) if n == toggles) { return Vec::new(); }
        // 2.4 GHz 实测的恢复办法是经协商上限绕一次（40→等 1 s→20）。
        let via = if self.anchor < self.negotiated { self.negotiated } else { narrower(self.anchor).unwrap_or(self.anchor) };
        match alive {
            None => { self.settle(); Vec::new() }
            Some(true) => {
                self.icmp = Some(true);
                self.settle();
                if toggles == 0 { Vec::new() } else { vec![Action::Log(format!("data path recovered width={} after {toggles} reapply", self.anchor))] }
            }
            // 从没回过 ping 的网关可能只是拦 ICMP：先按故障绕一次，仍不回就不再检查，也不阻止挑战。
            Some(false) if self.icmp != Some(true) && (toggles >= 1 || via == self.anchor) => {
                self.icmp = Some(false);
                self.settle();
                vec![Action::Log(format!("gateway does not answer ping at width={}; data path checks off for this association", self.anchor))]
            }
            Some(false) if toggles >= VERIFY_TOGGLES || via == self.anchor => {
                self.settle();
                self.blocked = true;
                if !self.hotspot_paused { self.state = "Data path unconfirmed"; }
                vec![Action::Log(format!("gateway silent at width={} after {toggles} reapply; no further challenges", self.anchor))]
            }
            Some(false) => {
                self.held = Some(now);
                self.phase = Phase::Verifying(toggles + 1);
                vec![Action::Log(format!("gateway silent at width={}; reapply via {via}", self.anchor)), Action::Reapply { width: self.anchor, via }]
            }
        }
    }
    fn hotspot(&mut self, now: Instant, check: Check, snapshot: Hotspot) -> Vec<Action> {
        if !matches!(self.phase, Phase::Checking(expected) if check == expected) { return Vec::new(); }
        if check == Check::Before {
            let Some((target, baseline)) = self.proposal.take() else { return Vec::new() };
            if !snapshot.ready() {
                self.phase = Phase::Stable;
                return vec![Action::Log("hotspot not ready; challenge skipped".into())];
            }
            if baseline.is_some() { self.budget.push_back(now); self.last_challenge = Some(now); }
            self.trial = Some(Trial { target, baseline, deadline: now + TRIAL_LIMIT, last_traffic: now,
                hotspot: snapshot, result: ResultKind::Unknown, operation: false, score: None, low_ticks: 0, low_ms: [0, 0],
                reference: None, probes: 0, suspect: 0, broken: false });
            self.window = Window::default();
            self.state = "Comparing";
            // 历史/扫描起点只会变窄，不需要参照；挑战才在锚点先探测一次。
            if baseline.is_some() {
                self.phase = Phase::Referencing;
                return vec![Action::Probe(Purpose::Reference)];
            }
            self.phase = Phase::Applying(Applying::new(now));
            return vec![Action::Log(format!("apply {} -> {target} mode={:?}", self.anchor, self.mode)), Action::Apply(target)];
        }
        let Some(t) = self.trial.as_ref() else { return Vec::new() };
        let fields = t.hotspot.changes(&snapshot);
        let mut actions = vec![Action::Log(format!("hotspot={} checkpoint={check:?} clients={:?}->{:?} changed={fields:?}",
            hotspot_iface(self.primary), t.hotspot.clients(), snapshot.clients()))];
        if !fields.is_empty() { self.hotspot_paused = true; }
        if check == Check::End {
            let t = self.trial.as_mut().unwrap();
            if self.hotspot_paused || now >= t.deadline { t.result = ResultKind::Unknown; }
            if let Some(c) = self.candidates.iter_mut().find(|c| c.width == t.target) {
                if t.broken && t.result == ResultKind::Lost {
                    let (level, hours) = c.record_broken(now);
                    actions.push(Action::Log(format!("avoid width={} for {hours} h", t.target)));
                    actions.push(Action::Avoid { width: t.target, until: now + Duration::from_secs(hours * 3600), level });
                } else { c.record(now, t.result, t.operation); }
            }
        }
        if check == Check::End && !self.hotspot_paused {
            if let Some(actual) = self.external.take() {
                self.saved_anchor &= self.anchor == actual;
                self.anchor = actual;
                self.held = Some(now);
                self.trial = None;
                self.reset_epoch(false);
                self.phase = Phase::Stable;
                self.state = "Waiting for traffic";
                actions.push(Action::Log(format!("external width readback={actual}; hotspot unchanged")));
                return actions;
            }
        }
        let t = self.trial.as_ref().unwrap();
        if check == Check::RolledBack {
            self.held = Some(now);
            self.trial = None;
            self.window = Window::default();
            self.state = if self.hotspot_paused { "Paused: hotspot changed" } else { "Not enough evidence" };
            actions.push(Action::Log(format!("rollback confirmed width={} hotspot_restored={}", self.actual, fields.is_empty())));
            // 读回回到锚点不等于数据路径已恢复（真机：回滚后网关不通，直到下一次刷新）。
            actions.extend(self.verify());
        } else if self.hotspot_paused {
            // 不发布缓存，也不修热点；即使 STA 回滚后热点恢复，本关联仍暂停。
            if let Some(c) = self.candidates.iter_mut().find(|c| c.width == t.target) {
                if check == Check::Confirmed { c.record(now, ResultKind::Unknown, false); }
            }
            actions.extend(self.rollback(now));
        } else if check == Check::Confirmed && now >= t.deadline {
            actions.extend(self.finish(now, ResultKind::Unknown, false, "trial deadline during hotspot check"));
        } else if check == Check::Confirmed {
            self.held = Some(now);
            actions.push(Action::Log(format!("readback confirmed width={}", self.actual)));
            if t.baseline.is_none() {
                self.anchor = t.target;
                self.trial = None;
                self.reset_epoch(false);
                self.stable_settle = 2;
                self.state = if self.history_hint == Some(self.anchor) { "From history" } else { "Waiting for traffic" };
                // 连接后按历史收窄，真机见过读回 20 而网关不通约 60 s，直到下一次刷新。
                actions.extend(self.verify());
            } else {
                self.phase = Phase::Settling(2);
                // 读回确认时已离申请至少一个 tick，AP 速率控制有时间跟上；坏宽度在这里就退出。
                if t.reference.is_some() {
                    self.trial.as_mut().unwrap().probes = 1;
                    actions.push(Action::Probe(Purpose::Candidate));
                }
            }
        } else if t.result == ResultKind::Won {
            let t = self.trial.take().unwrap();
            if let Some(c) = self.candidates.iter_mut().find(|c| c.width == self.anchor) {
                c.result = ResultKind::Won;
                c.next_ok = Some(now + REVIEW);
                c.at = Some(now);
                c.unknown_level = 0;
            }
            self.anchor = t.target;
            self.anchor_score = t.score;
            self.saved_anchor = true;
            self.phase = Phase::Stable;
            self.state = "Settled";
            actions.push(Action::Save { width: self.anchor, signal: t.score.unwrap().signal.round() as i32 });
        } else { actions.extend(self.rollback(now)); }
        actions
    }
}

fn legal(primary: u32, negotiated: u32) -> Vec<u32> {
    [160, 80, 40, 20].into_iter().filter(|&w| w <= negotiated && (primary >= 3000 || w <= 40)).collect()
}
pub(super) fn hotspot_iface(primary: u32) -> &'static str { if primary < 3000 { "wlan0" } else { "wlan2" } }
pub(super) fn busy(seconds: f64, bytes: u64) -> bool { seconds > 0.0 && bytes as f64 * 8.0 >= 1_000_000.0 * seconds }

/// 扫描先验只决定起点，不剔除任何合法候选。
pub(super) fn predict(primary: u32, width: u32, center: u32, neighbours: &[(u32, i32)]) -> u32 {
    let (mut width, mut center) = (width, center);
    while width > 20 {
        let (low, high) = (center.saturating_sub(width / 2), center + width / 2);
        if !neighbours.iter().any(|&(f, dbm)| dbm >= -82 && f != primary && f + 10 > low && f < high + 10) { return width; }
        width /= 2;
        center = if primary < center { center - width / 2 } else { center + width / 2 };
    }
    20
}

pub(super) fn parse_channel(info: &str) -> Option<(u32, u32, u32)> {
    let line = info.lines().map(str::trim).find(|l| l.starts_with("channel "))?;
    let number = |key: &str| line.split_once(key)?.1.trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok();
    Some((number("(")?, number("width:")?, number("center1:")?))
}
pub(super) fn parse_signal(dump: &str) -> Option<i32> {
    dump.lines().map(str::trim).find_map(|l| l.strip_prefix("signal:"))?.split_whitespace().next()?.parse().ok()
}
pub(super) fn parse_rx_rate(dump: &str) -> Option<u32> {
    let value = dump.lines().map(str::trim).find_map(|l| l.strip_prefix("rx bitrate:"))?;
    value.split_whitespace().next()?.parse::<f64>().ok().map(|r| r.round() as u32)
}
/// `rx bitrate:` 行里的 MCS 序号（HE-MCS、VHT-MCS 或 HT 的 MCS）。
pub(super) fn parse_rx_mcs(dump: &str) -> Option<u32> {
    let value = dump.lines().map(str::trim).find_map(|l| l.strip_prefix("rx bitrate:"))?;
    let words: Vec<_> = value.split_whitespace().collect();
    words.windows(2).find(|w| w[0].ends_with("MCS")).and_then(|w| w[1].parse().ok())
}
/// iputils 摘要：`913 packets transmitted, 680 received, [+N errors, ]25.5% packet loss, time 2998ms`。
pub(super) fn parse_probe(text: &str) -> Option<Probe> {
    let line = text.lines().find(|l| l.contains(" packets transmitted, "))?;
    let words: Vec<_> = line.split([',', ' ']).filter(|w| !w.is_empty()).collect();
    let before = |key: &str| -> Option<u32> { words.get(words.iter().position(|w| *w == key)?.checked_sub(1)?)?.parse().ok() };
    let ms = words.iter().position(|w| *w == "time").and_then(|i| words.get(i + 1))?.strip_suffix("ms")?.parse().ok()?;
    Some(Probe { sent: before("packets")?, received: before("received")?, ms })
}
/// 连接复位、窄档刷新用：同值重发无效，要先切到别的宽度。
pub(super) fn narrower(width: u32) -> Option<u32> {
    match width { 160 => Some(80), 80 => Some(40), 40 => Some(20), _ => None }
}
pub(super) fn chwidth_code(width: u32) -> Option<&'static str> {
    match width { 20 => Some("0"), 40 => Some("1"), 80 => Some("2"), 160 => Some("3"), _ => None }
}

#[cfg(test)]
mod tests;
