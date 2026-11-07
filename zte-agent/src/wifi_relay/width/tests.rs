use super::*;

struct Rig {
    c: Controller,
    now: Instant,
    actual: u32,
    writes: Vec<u32>,
    saves: Vec<u32>,
    reapplies: Vec<(u32, u32)>,
    commands: Vec<Instant>,
    hotspot: Hotspot,
    auto_apply: bool,
    readback: bool,
    /// 按当前宽度给出的探测结果；`None` 表示没有网关。
    probe: Option<Box<dyn FnMut(u32) -> Probe>>,
    probed: Vec<(Purpose, u32)>,
    /// 网关是否回 ARP；默认 `None`（无法检查）。
    alive: Box<dyn FnMut(u32) -> Option<bool>>,
    checks: Vec<(u8, u32)>,
    avoids: Vec<(u32, u8)>,
}
impl Rig {
    fn new(primary: u32, max: u32, start: u32) -> Self {
        let now = Instant::now();
        let mut rig = Self { c: Controller::new(1, Mode::Auto, primary, max, start, None), now, actual: max,
            writes: Vec::new(), saves: Vec::new(), reapplies: Vec::new(), commands: Vec::new(), hotspot: Hotspot::Off, auto_apply: true, readback: true,
            probe: None, probed: Vec::new(), alive: Box::new(|_| None), checks: Vec::new(), avoids: Vec::new() };
        let actions = rig.c.initialize();
        rig.run(actions);
        if start != max { rig.tick(None); }
        rig
    }
    fn run(&mut self, actions: Vec<Action>) {
        let mut actions: VecDeque<_> = actions.into();
        while let Some(action) = actions.pop_front() {
            match action {
                Action::Apply(w) => { self.writes.push(w); self.commands.push(self.now); if self.auto_apply { self.actual = w; } }
                Action::Reapply { width, via } => { self.reapplies.push((width, via)); self.commands.push(self.now); if self.auto_apply { self.actual = width; } }
                Action::Save { width, .. } => self.saves.push(width),
                Action::Hotspot(check) => actions.extend(self.c.handle(1, self.now, Event::Hotspot { check, snapshot: self.hotspot.clone() })),
                Action::Probe(purpose) => {
                    let actual = self.actual;
                    self.probed.push((purpose, actual));
                    let result = self.probe.as_mut().map(|f| f(actual));
                    actions.extend(self.c.handle(1, self.now, Event::Probed { purpose, result }));
                }
                Action::Alive(toggles) => {
                    let actual = self.actual;
                    self.checks.push((toggles, actual));
                    let alive = (self.alive)(actual);
                    actions.extend(self.c.handle(1, self.now, Event::Alive { toggles, alive }));
                }
                Action::Avoid { width, level, .. } => self.avoids.push((width, level)),
                Action::Log(_) => (),
            }
        }
    }
    fn tick(&mut self, sample: Option<Sample>) {
        self.now += Duration::from_secs(3);
        let channel = self.readback.then_some((self.c.primary, self.actual));
        let actions = self.c.handle(1, self.now, Event::Tick { channel, sample, reset: false });
        self.run(actions);
    }
    fn traffic(&mut self, ticks: usize, rate: impl Fn(u32) -> u64) {
        for _ in 0..ticks { self.tick(Some(sample(rate(self.actual)))); }
    }
    fn later(&mut self) { self.now += REVIEW + Duration::from_secs(1); self.tick(None); }
}
fn sample(e: u64) -> Sample {
    Sample { seconds: 3.0, bytes: e * 125_000, airtime_ms: 1000, hotspot_bytes: 0, tx_bytes: 1000, signal: -40, rx_mcs: None, scanned: false, radio: 0 }
}
fn score(e: f64) -> Score {
    Score { e, r: 10.0, hotspot_r: 0.0, signal: -40.0, scanned: false, seconds: 12.0, airtime_ms: 1000, tx_r: 0.1, spread: 0.0 }
}

#[test]
fn a_good_40_anchor_is_never_replaced_by_two_equally_bad_wide_candidates() {
    let mut r = Rig::new(5220, 160, 40);
    for _ in 0..20 {
        r.traffic(30, |w| if w == 40 { 500 } else { 200 });
        assert_eq!(r.c.anchor, 40);
        r.later();
    }
    assert!(r.writes.contains(&80) && r.writes.contains(&160));
    assert!(r.saves.iter().all(|w| *w == 40));
}

#[test]
fn nonmonotonic_scores_eventually_find_40_without_a_sweep() {
    let mut r = Rig::new(5220, 160, 160);
    for _ in 0..10 {
        r.traffic(30, |w| match w { 160 => 300, 80 => 280, 40 => 500, _ => 250 });
        r.later();
    }
    assert_eq!(r.c.anchor, 40);
    assert!(r.writes.contains(&80) && r.writes.contains(&40));
}

#[test]
fn stopped_traffic_is_unknown_and_rolls_back_to_20_not_the_challenger() {
    let mut r = Rig::new(2412, 40, 20);
    r.traffic(6, |_| 200);
    assert_eq!(r.c.anchor, 20);
    assert!(matches!(r.c.phase, Phase::Applying(_)));
    r.tick(None);
    for _ in 0..5 { r.tick(None); }
    assert_eq!(r.c.anchor, 20);
    assert_eq!(r.actual, 20);
    let candidate = r.c.candidates.iter().find(|c| c.width == 40).unwrap();
    assert_eq!(candidate.result, ResultKind::Unknown);
    assert_eq!(candidate.loss_level, 0);
}

#[test]
fn windows_sum_counts_not_ratios_and_allow_small_airtime_ticks() {
    let now = Instant::now();
    let mut window = Window::default();
    let mut completed = None;
    for i in 1..=4 {
        let mut s = sample(200);
        s.airtime_ms = if i < 4 { 10 } else { 1000 };
        completed = window.push(now + Duration::from_secs(i * 3), s);
    }
    let result = completed.unwrap();
    assert!((result.e - 800_000_000.0 / 1.03 / 1e6).abs() < 0.001);
    assert_eq!(result.seconds, 12.0);
    assert_ne!(result.e, (20_000.0 * 3.0 + 200.0) / 4.0);
}

#[test]
fn settling_is_not_scored_and_early_stop_requires_two_ticks_and_airtime() {
    let mut r = Rig::new(2412, 40, 40);
    r.traffic(4, |_| 200);
    assert_eq!(r.c.anchor, 40);
    r.traffic(1, |_| 1); // 读回区间不计分。
    r.traffic(2, |_| 20); // 两个有业务的 settling tick。
    assert!(matches!(r.c.phase, Phase::Measuring));
    assert_eq!(r.c.window.bytes, 0);
    let mut low = sample(10);
    low.airtime_ms = 40;
    low.bytes = 400_000;
    r.tick(Some(low));
    r.tick(Some(low));
    assert!(matches!(r.c.phase, Phase::Measuring));
    r.tick(Some(low));
    assert!(matches!(r.c.phase, Phase::Measuring), "只看相邻两个 tick 的空口总量，不能累计三个凑门槛");
    low.airtime_ms = 60;
    r.tick(Some(low));
    assert!(matches!(r.c.phase, Phase::RollingBack(_)));
    assert_eq!(r.c.candidates.iter().find(|c| c.width == 20).unwrap().result, ResultKind::Lost);
}

#[test]
fn no_bytes_never_invent_a_zero_score_or_performance_loss() {
    let mut r = Rig::new(2412, 40, 40);
    r.traffic(4, |_| 200);
    r.traffic(1, |_| 200);
    for _ in 0..15 { r.tick(Some(sample(0))); }
    assert_eq!(r.c.anchor, 40);
    assert_eq!(r.c.candidates.iter().find(|c| c.width == 20).unwrap().result, ResultKind::Unknown);
}

#[test]
fn readback_not_command_success_commits_and_deadline_never_moves() {
    let mut r = Rig::new(2412, 40, 40);
    r.auto_apply = false;
    r.traffic(4, |_| 200);
    let deadline = r.c.trial.as_ref().unwrap().deadline;
    assert_eq!(r.c.width(), 40);
    for _ in 0..3 { r.traffic(1, |_| 200); assert_eq!(r.c.trial.as_ref().unwrap().deadline, deadline); }
    assert_eq!(r.writes, [20, 20]);
    r.actual = 20;
    r.traffic(3, |_| 200);
    assert_eq!(r.c.anchor, 40);
    assert_eq!(r.c.width(), 20);
    assert_eq!(r.c.trial.as_ref().unwrap().deadline, deadline);
    r.now = deadline;
    r.tick(Some(sample(200)));
    assert!(matches!(r.c.phase, Phase::RollingBack(_)));
    assert_eq!(r.writes.last(), Some(&40));
}

#[test]
fn failed_apply_and_rollback_pause_without_saving_and_valid_readback_recovers() {
    let mut r = Rig::new(2412, 40, 40);
    r.auto_apply = false;
    r.traffic(4, |_| 200);
    let saved = r.saves.len();
    r.readback = false;
    for _ in 0..14 { r.tick(None); }
    assert!(matches!(r.c.phase, Phase::Paused(Pause::Unconfirmed)));
    assert_eq!(r.c.status(), Some("Cannot confirm"));
    assert_eq!(r.saves.len(), saved);
    assert_eq!(r.writes, [20, 20, 20, 40, 40, 40]);
    r.readback = true;
    r.actual = 20;
    r.tick(Some(sample(200)));
    assert_eq!(r.c.anchor, 20);
    assert_eq!(r.c.negotiated, 40);
    assert!(matches!(r.c.phase, Phase::Stable));
}

#[test]
fn an_external_width_change_preserves_ceiling_but_invalidates_window() {
    let mut r = Rig::new(5220, 160, 160);
    r.traffic(2, |_| 300);
    r.actual = 40;
    r.tick(Some(sample(300)));
    assert_eq!(r.c.anchor, 40);
    assert_eq!(r.c.negotiated, 160);
    assert_eq!(r.c.window.bytes, 0);
    assert!(r.c.anchor_score.is_none());
}

#[test]
fn reset_radio_and_generation_changes_never_join_old_windows() {
    for radio_change in [false, true] {
        let mut r = Rig::new(2412, 40, 40);
        r.traffic(8, |_| 200);
        let mut s = sample(400);
        if radio_change { s.radio = 1; }
        let actions = r.c.handle(1, r.now, Event::Tick { channel: Some((2412, r.actual)), sample: Some(s), reset: !radio_change });
        r.run(actions);
        assert_eq!(r.c.anchor, 40);
        assert!(matches!(r.c.phase, Phase::RollingBack(_)));
        assert_eq!(r.c.window.bytes, 0);
        if radio_change { assert!(r.c.candidates.iter().all(|c| c.loss_level == 0)); }
    }
    let mut r = Rig::new(2412, 40, 40);
    r.traffic(4, |_| 200);
    let deadline = r.c.trial.as_ref().unwrap().deadline;
    assert!(r.c.handle(0, r.now, Event::Tick { channel: Some((5220, 160)), sample: Some(sample(900)), reset: true }).is_empty());
    assert!(r.c.handle(0, r.now, Event::Applied { width: 20, ok: false }).is_empty());
    assert!(r.c.handle(0, r.now, Event::Hotspot { check: Check::Confirmed, snapshot: Hotspot::Unavailable }).is_empty());
    assert_eq!(r.c.trial.as_ref().unwrap().deadline, deadline);
    assert_eq!(r.c.anchor, 40);
}

#[test]
fn losses_and_unknowns_back_off_independently_and_operation_has_a_floor() {
    let now = Instant::now();
    let mut c = Candidate::new(80);
    for minutes in [15, 30, 60, 120, 240, 240] {
        c.record(now, ResultKind::Lost, false);
        assert_eq!(c.next_ok, Some(now + Duration::from_secs(minutes * 60)));
    }
    for minutes in [5, 10, 20, 40, 60, 60] {
        c.record(now, ResultKind::Unknown, false);
        assert_eq!(c.next_ok, Some(now + Duration::from_secs(minutes * 60)));
        assert_eq!(c.loss_level, 4);
    }
    c.record(now, ResultKind::Won, false);
    assert_eq!(c.unknown_level, 0);
    c.record(now, ResultKind::Unknown, true);
    assert_eq!(c.next_ok, Some(now + REVIEW));
}

#[test]
fn candidate_order_budget_and_collapse_do_not_jump_to_the_widest() {
    let mut r = Rig::new(5220, 160, 40);
    assert_eq!(r.c.candidate(r.now), Some(80));
    r.c.candidates.iter_mut().find(|c| c.width == 80).unwrap().record(r.now, ResultKind::Lost, false);
    assert_eq!(r.c.candidate(r.now), Some(20));
    r.c.budget = VecDeque::from([r.now, r.now]);
    r.c.anchor_score = Some(score(1000.0));
    let writes = r.writes.len();
    r.traffic(8, |_| 200);
    assert_eq!(r.writes.len(), writes);
    r.c.budget.clear();
    r.c.anchor_score = Some(score(1000.0));
    r.traffic(4, |_| 200);
    assert_eq!(r.writes.last(), Some(&20));
    r.c.budget = VecDeque::from([r.now, r.now]);
    r.now += TRIAL_LIMIT;
    r.tick(None);
    assert_eq!(r.writes.last(), Some(&40));
}

#[test]
fn suspicious_rules_each_produce_unknown_and_equal_results_lose() {
    let a = score(200.0);
    let mut b = score(300.0);
    b.r = 7.9;
    assert_eq!(a.compare(b), ResultKind::Unknown);
    b = score(300.0); b.hotspot_r = 5.1;
    assert_eq!(a.compare(b), ResultKind::Unknown);
    b = score(300.0); b.signal = -46.1;
    assert_eq!(a.compare(b), ResultKind::Unknown);
    b = score(300.0); b.scanned = true;
    assert_eq!(a.compare(b), ResultKind::Unknown);
    assert_eq!(a.compare(score(200.0)), ResultKind::Lost);
    assert_eq!(a.compare(score(229.9)), ResultKind::Lost);
    assert_eq!(a.compare(score(230.0)), ResultKind::Won);
}

#[test]
fn observe_and_off_never_apply_or_save() {
    for mode in [Mode::Observe, Mode::Off] {
        let mut r = Rig::new(5220, 160, 160);
        r.c = Controller::new(1, mode, 5220, 160, 40, Some(40));
        assert!(r.c.initialize().is_empty());
        for _ in 0..5 { r.traffic(20, |_| 300); r.later(); }
        assert!(r.writes.is_empty());
        assert!(r.saves.is_empty());
        assert_eq!(r.c.status(), if mode == Mode::Off { None } else { Some("Observing") });
    }
}

fn hot(width: u32, clients: usize) -> Hotspot {
    Hotspot::parse("state=ENABLED\nfreq=5220\nchannel=44\nvht_oper_chwidth=1\nhe_oper_centr_freq_seg0_idx=42", &format!("channel 44 (5220 MHz), width: {width} MHz, center1: 5210 MHz"), Some(clients))
}

#[test]
fn hotspot_not_ready_does_not_spend_budget_or_backoff() {
    let mut r = Rig::new(5220, 160, 160);
    r.hotspot = Hotspot::Unavailable;
    r.traffic(8, |_| 200);
    assert!(r.writes.is_empty());
    assert!(r.c.budget.is_empty());
    assert!(r.c.candidates.iter().all(|c| c.result == ResultKind::Untested));
}

#[test]
fn hotspot_change_at_confirmation_or_end_rolls_back_and_pauses_for_session() {
    for at_end in [false, true] {
        let mut r = Rig::new(5220, 160, 160);
        r.hotspot = hot(80, 1);
        r.traffic(4, |_| 200);
        if at_end { r.traffic(3, |_| 300); }
        r.hotspot = hot(40, 1);
        r.traffic(8, |_| 300);
        assert_eq!(r.c.anchor, 160);
        assert_eq!(r.actual, 160);
        assert_eq!(r.c.status(), Some("Paused: hotspot changed"));
        let writes = r.writes.len();
        r.hotspot = hot(80, 1);
        for _ in 0..3 { r.later(); r.traffic(20, |_| 300); }
        assert_eq!(r.writes.len(), writes);
        assert_eq!(r.saves, [160]);
    }
}

#[test]
fn client_count_changes_do_not_block_a_win_and_baseline_is_saved_once() {
    let mut r = Rig::new(5220, 160, 160);
    r.hotspot = hot(80, 1);
    r.traffic(4, |_| 200);
    r.hotspot = hot(80, 3);
    r.traffic(10, |_| 300);
    assert_eq!(r.c.anchor, 80);
    assert_eq!(r.saves, [160, 80]);
    r.traffic(12, |_| 300);
    assert_eq!(r.saves, [160, 80]);
}

#[test]
fn expired_windows_do_not_join_sparse_bursts_or_turn_into_zero_scores() {
    let now = Instant::now();
    let mut window = Window::default();
    assert!(window.push(now, sample(200)).is_none());
    assert!(!window.expired(now + Duration::from_secs(5)));
    assert!(window.expired(now + Duration::from_secs(6)));
    let mut window = Window::default();
    for i in 0..10 {
        let mut low = sample(1);
        low.bytes = 100;
        assert!(window.push(now + Duration::from_secs(i * 3), low).is_none());
    }
    assert!(window.expired(now + Duration::from_secs(27)));
    let mut r = Rig::new(2412, 40, 40);
    r.traffic(2, |_| 200);
    let saved = r.saves.len();
    r.c.idle(r.now + GAP, false);
    assert_eq!(r.c.window.bytes, 0);
    assert_eq!(r.saves.len(), saved);
    assert_eq!(r.c.status(), Some("Not enough evidence"));
}

#[test]
fn early_stop_does_not_span_missing_ticks() {
    let mut r = Rig::new(2412, 40, 40);
    r.traffic(7, |_| 200);
    r.traffic(1, |_| 20);
    r.tick(None);
    // 此时距前一个合格 tick 已达 6 秒，窗口作废，不能拼成两次低分。
    r.traffic(1, |_| 20);
    assert!(matches!(r.c.phase, Phase::RollingBack(_)));
    assert_eq!(r.c.candidates.iter().find(|c| c.width == 20).unwrap().result, ResultKind::Unknown);
}

#[test]
fn delayed_hotspot_check_cannot_commit_a_win_past_the_trial_deadline() {
    let mut r = Rig::new(2412, 40, 40);
    r.traffic(7, |_| 200);
    r.traffic(3, |_| 300);
    r.now += Duration::from_secs(3);
    let actions = r.c.handle(1, r.now, Event::Tick { channel: Some((2412, 20)), sample: Some(sample(300)), reset: false });
    assert!(actions.iter().any(|a| matches!(a, Action::Hotspot(Check::End))));
    r.now = r.c.trial.as_ref().unwrap().deadline;
    r.run(actions);
    assert_eq!(r.c.anchor, 40);
    assert!(matches!(r.c.phase, Phase::RollingBack(_)));
    assert_eq!(r.saves, [40]);
}

#[test]
fn external_width_during_trial_still_checks_hotspot_before_adopting_it() {
    for changed in [false, true] {
        let mut r = Rig::new(5220, 160, 160);
        r.hotspot = hot(80, 1);
        r.traffic(7, |_| 200);
        if changed { r.hotspot = hot(40, 1); }
        r.actual = 40;
        r.traffic(1, |_| 200);
        if changed {
            assert_eq!(r.c.anchor, 160);
            assert_eq!(r.writes.last(), Some(&160));
            r.tick(None);
            assert_eq!(r.c.status(), Some("Paused: hotspot changed"));
            assert_eq!(r.saves, [160]);
        } else {
            assert_eq!(r.c.anchor, 40);
            assert!(matches!(r.c.phase, Phase::Stable));
            assert_eq!(r.writes, [80]);
        }
    }
}

#[test]
fn a_zero_byte_final_tick_cannot_complete_a_losing_window() {
    let mut r = Rig::new(2412, 40, 40);
    r.traffic(10, |_| 200);
    assert_eq!(r.c.window.qualified, 9.0);
    r.tick(Some(sample(0)));
    assert!(matches!(r.c.phase, Phase::RollingBack(_)));
    let c = r.c.candidates.iter().find(|c| c.width == 20).unwrap();
    assert_eq!(c.result, ResultKind::Unknown);
    assert_eq!(c.loss_level, 0);
    assert_eq!(r.saves, [40]);
}

#[test]
fn parsing_and_scan_prior_remain_stable() {
    assert_eq!(predict(2412, 40, 2422, &[(2437, -60)]), 20);
    assert_eq!(predict(5220, 160, 5250, &[(5260, -70)]), 80);
    assert_eq!(predict(5220, 160, 5250, &[(5180, -70)]), 40);
    assert_eq!(predict(5220, 160, 5250, &[]), 160);
    assert_eq!(parse_channel("channel 44 (5220 MHz), width: 160 MHz, center1: 5250 MHz"), Some((5220, 160, 5250)));
    assert_eq!(parse_signal("signal: -44 dBm\nrx bitrate: 286.7 MBit/s"), Some(-44));
    assert_eq!(parse_rx_rate("signal: -44 dBm\nrx bitrate: 286.7 MBit/s"), Some(287));
    assert_eq!(chwidth_code(320), None);
}

// B31 drops a held iwpriv width silently after about 100-260 s; only a change re-applies it.
#[test]
fn a_narrow_anchor_is_reapplied_through_the_ceiling_at_least_every_minute_of_traffic() {
    let mut r = Rig::new(2412, 40, 20);
    assert_eq!(r.c.anchor, 20);
    r.c.last_challenge = Some(r.now + Duration::from_secs(3600));
    let start = r.now;
    r.traffic(100, |w| if w == 20 { 500 } else { 200 });
    assert_eq!(r.c.anchor, 20);
    assert!(r.reapplies.len() >= 4, "{:?}", r.reapplies);
    assert!(r.reapplies.iter().all(|&(w, via)| w == 20 && via == 40));
    let mut marks = vec![start];
    marks.extend(r.commands.iter().copied().filter(|&at| at >= start));
    marks.push(r.now);
    assert!(marks.windows(2).all(|m| m[1].duration_since(m[0]) <= REAPPLY + Duration::from_secs(3)), "{marks:?}");
}

#[test]
fn an_anchor_at_the_negotiated_width_is_never_reapplied() {
    let mut r = Rig::new(5220, 160, 160);
    r.c.last_challenge = Some(r.now + Duration::from_secs(3600));
    r.traffic(100, |_| 500);
    assert!(r.reapplies.is_empty());
}

#[test]
fn a_reapply_restarts_the_window_so_the_toggle_is_never_scored() {
    let mut r = Rig::new(5220, 160, 80);
    r.c.last_challenge = Some(r.now + Duration::from_secs(3600));
    for _ in 0..40 {
        let before = r.reapplies.len();
        r.tick(Some(sample(500)));
        if r.reapplies.len() > before {
            assert!(r.c.window.start.is_none());
            assert_eq!(r.c.stable_settle, 1);
            return;
        }
    }
    panic!("no reapply within 120 s of traffic");
}

#[test]
fn the_first_busy_tick_after_a_long_idle_reapplies_at_once() {
    let mut r = Rig::new(2412, 40, 20);
    r.c.last_challenge = Some(r.now + Duration::from_secs(3600));
    r.traffic(3, |_| 500);
    assert!(r.reapplies.is_empty());
    r.now += Duration::from_secs(120);
    r.c.idle(r.now, false);
    assert!(r.reapplies.is_empty());
    r.traffic(1, |_| 500);
    assert_eq!(r.reapplies, [(20, 40)]);
}

// W（2.4 GHz）上洪泛的量级：20 MHz 几乎不丢；坏的 40 MHz 丢一半。
const GOOD: Probe = Probe { sent: 1500, received: 1497, ms: 2000 };
const BAD: Probe = Probe { sent: 600, received: 300, ms: 2000 };

#[test]
fn a_broken_candidate_fails_its_probe_at_confirmation_and_the_rollback_is_verified() {
    let mut r = Rig::new(2412, 40, 20);
    r.probe = Some(Box::new(|w| if w == 40 { BAD } else { GOOD }));
    r.traffic(6, |_| 200);
    assert_eq!(r.probed, [(Purpose::Reference, 20)]);
    assert_eq!(r.writes.last(), Some(&40));
    let applied = r.now;
    r.traffic(1, |_| 200);
    assert_eq!(r.probed.last(), Some(&(Purpose::Candidate, 40)));
    assert_eq!(r.writes.last(), Some(&20));
    assert_eq!(r.now.duration_since(applied), Duration::from_secs(3), "只停留到读回确认的那个 tick");
    let c = r.c.candidates.iter().find(|c| c.width == 40).unwrap();
    assert_eq!((c.result, c.broken_level, c.next_ok), (ResultKind::Lost, 1, Some(r.now + Duration::from_secs(3600))));
    r.alive = Box::new(|_| Some(true));
    r.traffic(2, |_| 200);
    assert_eq!(r.checks.last(), Some(&(0, 20)));
    assert!(matches!(r.c.phase, Phase::Stable));
    assert_eq!(r.c.anchor, 20);
    assert!(r.reapplies.is_empty());
}

#[test]
fn a_lossy_reference_turns_probes_off_for_that_trial() {
    let mut r = Rig::new(2412, 40, 20);
    // 网关限速 ICMP：锚点自己就丢四分之一。
    r.probe = Some(Box::new(|_| Probe { sent: 400, received: 300, ms: 2000 }));
    r.traffic(6, |_| 200);
    r.traffic(12, |w| if w == 20 { 200 } else { 80 });
    assert_eq!(r.probed, [(Purpose::Reference, 20)]);
    assert_eq!(r.c.candidates.iter().find(|c| c.width == 40).unwrap().result, ResultKind::Lost);
    assert!(matches!(r.c.phase, Phase::Stable));
}

#[test]
fn slow_delivery_after_a_passing_probe_is_probed_again_and_fails_in_seconds() {
    let mut r = Rig::new(2412, 40, 20);
    let mut calls = 0;
    // 先过关：故障要等真实负载才出现。
    r.probe = Some(Box::new(move |w| {
        calls += usize::from(w == 40);
        if w == 40 && calls > 1 { BAD } else { GOOD }
    }));
    r.traffic(6, |_| 200);
    let applied = r.now;
    r.traffic(3, |w| if w == 20 { 200 } else { 2 });
    assert_eq!(r.probed[1..], [(Purpose::Candidate, 40), (Purpose::Candidate, 40)]);
    assert_eq!(r.writes.last(), Some(&20));
    assert_eq!(r.now.duration_since(applied), Duration::from_secs(9));
    assert_eq!(r.c.candidates.iter().find(|c| c.width == 40).unwrap().result, ResultKind::Lost);
}

#[test]
fn without_a_gateway_slow_delivery_still_rolls_back_well_before_the_deadline() {
    let mut r = Rig::new(2412, 40, 20);
    r.traffic(6, |_| 200);
    let applied = r.now;
    r.traffic(3, |w| if w == 20 { 200 } else { 2 });
    assert_eq!(r.writes.last(), Some(&20));
    assert_eq!(r.now.duration_since(applied), Duration::from_secs(9));
    let c = r.c.candidates.iter().find(|c| c.width == 40).unwrap();
    assert_eq!((c.result, c.loss_level), (ResultKind::Unknown, 0));
    r.traffic(2, |_| 200);
    assert_eq!(r.probed, [(Purpose::Reference, 20)]);
    assert!(matches!(r.c.phase, Phase::Stable));
}

#[test]
fn a_low_mcs_at_strong_signal_triggers_one_more_probe_but_e_still_decides() {
    let mut r = Rig::new(2412, 40, 20);
    r.probe = Some(Box::new(|_| GOOD));
    r.traffic(7, |_| 200);
    let mut low = sample(150);
    low.rx_mcs = Some(0);
    r.tick(Some(low));
    r.tick(Some(low));
    assert_eq!(r.probed.iter().filter(|p| p.0 == Purpose::Candidate).count(), 2);
    assert!(r.c.trial.is_some());
}

#[test]
fn a_silent_gateway_after_rollback_is_reapplied_twice_then_challenges_stop() {
    let mut r = Rig::new(2412, 40, 20);
    r.probe = Some(Box::new(|w| if w == 40 { BAD } else { GOOD }));
    // 回滚读回正常，网关却不回 ARP。
    r.alive = Box::new(|_| Some(false));
    r.traffic(6, |_| 200);
    r.traffic(5, |_| 200);
    // 第一项是 Rig::new 里窄起点的检查。
    assert_eq!(r.checks[1..], [(0, 20), (1, 20), (2, 20)]);
    assert_eq!(r.reapplies, [(20, 40), (20, 40)]);
    assert_eq!(r.c.status(), Some("Data path unconfirmed"));
    assert!(matches!(r.c.phase, Phase::Stable));
    let writes = r.writes.len();
    for _ in 0..3 { r.later(); r.traffic(20, |_| 200); }
    assert_eq!(r.writes.len(), writes);
}

#[test]
fn hotspot_pause_survives_a_partial_gateway_recovery_toggle() {
    let mut r = Rig::new(2412, 40, 20);
    r.probe = Some(Box::new(|_| GOOD));
    r.alive = Box::new(|_| Some(false));
    r.traffic(6, |_| 200);
    r.hotspot = Hotspot::parse("state=ENABLED", "channel 1 (2412 MHz), width: 40 MHz, center1: 2422 MHz", None);
    r.traffic(1, |_| 200);
    assert!(r.c.hotspot_paused);
    assert!(matches!(r.c.phase, Phase::RollingBack(_)));
    r.traffic(1, |_| 200);
    assert!(matches!(r.c.phase, Phase::Verifying(1)));
    // The first write succeeds, but the second leaves the device at via=40.
    r.actual = 40;
    r.tick(None);
    r.tick(None);
    assert!(matches!(r.c.phase, Phase::Paused(Pause::Hotspot)));
    assert_eq!(r.c.status(), Some("Paused: hotspot changed"));
    assert_eq!(r.checks[1..], [(0, 20), (1, 40), (2, 40)]);
    assert_eq!(r.reapplies, [(20, 40), (40, 20)]);
    let (commands, saves) = (r.commands.len(), r.saves.len());
    r.later();
    r.traffic(30, |_| 200);
    assert_eq!((r.commands.len(), r.saves.len()), (commands, saves));
}

#[test]
fn gateway_recovery_continues_without_width_readback_and_stays_bounded() {
    for alive in [Some(true), Some(false), None] {
        let mut r = Rig::new(2412, 40, 40);
        r.probe = Some(Box::new(|w| if w == 40 { GOOD } else { BAD }));
        r.alive = Box::new(|_| Some(false));
        r.traffic(6, |_| 200);
        assert_eq!(r.c.icmp, Some(true));
        assert_eq!(r.c.anchor, r.c.negotiated);
        assert!(matches!(r.c.phase, Phase::Verifying(1)));
        r.readback = false;
        r.alive = Box::new(move |_| alive);
        r.tick(None);
        assert_eq!(r.checks, [(0, 40), (1, 40)]);
        if alive == Some(false) {
            assert!(matches!(r.c.phase, Phase::Verifying(2)));
            r.tick(None);
            assert_eq!(r.checks, [(0, 40), (1, 40), (2, 40)]);
            assert_eq!(r.c.status(), Some("Data path unconfirmed"));
        }
        assert!(matches!(r.c.phase, Phase::Stable));
        assert_eq!(r.c.blocked, alive == Some(false));
        assert_eq!(r.reapplies.len(), if alive == Some(false) { 2 } else { 1 });
        let checks = r.checks.len();
        r.tick(None);
        assert_eq!(r.checks.len(), checks);
    }
}

#[test]
fn a_silent_gateway_after_the_history_start_recovers_with_one_reapply() {
    let mut calls = 0;
    let mut r = Rig::new(2412, 40, 40);
    // 连接后按历史收窄到 20：第一次检查网关不回，绕一次 40 后恢复。
    r.alive = Box::new(move |_| { calls += 1; Some(calls > 1) });
    r.c = Controller::new(1, Mode::Auto, 2412, 40, 20, Some(20));
    let actions = r.c.initialize();
    r.run(actions);
    r.tick(None);
    assert_eq!(r.c.anchor, 20);
    assert_eq!(r.reapplies, [(20, 40)]);
    assert!(matches!(r.c.phase, Phase::Verifying(1)));
    r.tick(None);
    assert_eq!(r.checks, [(0, 20), (1, 20)]);
    assert!(matches!(r.c.phase, Phase::Stable));
    assert_eq!((r.c.icmp, r.c.status()), (Some(true), Some("From history")));
}

#[test]
fn a_gateway_that_never_answers_ping_gets_one_reapply_then_no_more_checks() {
    let mut r = Rig::new(2412, 40, 40);
    r.alive = Box::new(|_| Some(false));
    r.c = Controller::new(1, Mode::Auto, 2412, 40, 20, Some(20));
    let actions = r.c.initialize();
    r.run(actions);
    r.tick(None);
    r.tick(None);
    assert_eq!(r.checks, [(0, 20), (1, 20)]);
    assert_eq!(r.reapplies, [(20, 40)]);
    assert_eq!(r.c.icmp, Some(false));
    r.c.last_challenge = Some(r.now + Duration::from_secs(3600));
    r.traffic(45, |_| 300);
    assert!(r.reapplies.len() >= 3, "{:?}", r.reapplies);
    assert_eq!(r.checks.len(), 2, "不再检查");
    assert!(!r.c.blocked);
}

#[test]
fn each_refresh_is_followed_by_one_gateway_check() {
    let mut r = Rig::new(2412, 40, 20);
    r.alive = Box::new(|_| Some(true));
    r.c.last_challenge = Some(r.now + Duration::from_secs(3600));
    r.traffic(45, |_| 300);
    assert!(r.reapplies.len() >= 2);
    assert_eq!(r.checks.iter().filter(|c| c.0 == 0).count(), r.reapplies.len() + 1, "{:?}", r.checks);
}

#[test]
fn a_5ghz_narrow_anchor_is_reapplied_through_the_ceiling_the_only_verified_path() {
    let mut r = Rig::new(5220, 160, 80);
    r.c.last_challenge = Some(r.now + Duration::from_secs(3600));
    r.traffic(45, |_| 500);
    assert!(r.reapplies.len() >= 2, "{:?}", r.reapplies);
    assert!(r.reapplies.iter().all(|&(w, via)| w == 80 && via == 160));
}

#[test]
fn a_probe_failure_backs_off_for_hours_and_is_reported_for_later_associations() {
    let mut r = Rig::new(2412, 40, 20);
    r.probe = Some(Box::new(|w| if w == 40 { BAD } else { GOOD }));
    r.traffic(7, |_| 200);
    assert_eq!(r.avoids, [(40, 1)]);
    let c = r.c.candidates.iter().find(|c| c.width == 40).unwrap();
    assert_eq!(c.next_ok, Some(r.now + Duration::from_secs(3600)));
    assert_eq!(c.loss_level, 0, "不占用普通的 15 分钟退避");
    r.now += Duration::from_secs(3600);
    r.traffic(10, |_| 200);
    assert_eq!(r.avoids, [(40, 1), (40, 2)]);
    let left = r.c.candidates.iter().find(|c| c.width == 40).unwrap().next_ok.unwrap().duration_since(r.now);
    assert!(left <= Duration::from_secs(2 * 3600) && left > Duration::from_secs(2 * 3600 - 60), "{left:?}");
}

#[test]
fn an_avoided_width_is_not_challenged_before_its_time_and_keeps_its_level() {
    let mut r = Rig::new(2412, 40, 20);
    r.probe = Some(Box::new(|w| if w == 40 { BAD } else { GOOD }));
    r.c.avoid(40, r.now + Duration::from_secs(1800), 3);
    r.traffic(30, |_| 200);
    assert_eq!(r.writes, [20], "30 分钟内不挑战 40");
    r.now += Duration::from_secs(1800);
    r.traffic(10, |_| 200);
    assert_eq!(r.writes, [20, 40, 20]);
    assert_eq!(r.avoids, [(40, 4)], "级别延续：第四次判坏退避 8 h");
}

#[test]
fn probe_loss_ignores_packets_still_in_flight_and_iputils_output_parses() {
    let reference = Probe { sent: 2323, received: 2319, ms: 2997 };
    assert_eq!(reference.loss(), 0.0);
    assert!(reference.usable() && !reference.fails(reference));
    // W 上实测的四次坏 40 MHz（3 s 洪泛）。
    for (sent, received) in [(913, 680), (819, 561), (362, 120), (458, 218)] {
        assert!(Probe { sent, received, ms: 2995 }.fails(reference));
    }
    assert!(!Probe { sent: 400, received: 300, ms: 2000 }.usable());
    assert!(!Probe::default().usable());
    assert!(Probe::default().fails(reference));
    let text = "--- 192.168.2.1 ping statistics ---\n913 packets transmitted, 680 received, 25.5482% packet loss, time 2998ms\nrtt min/avg/max/mdev = 1.416/9.128/57.993/6.105 ms, pipe 8, ipg/ewma 3.287/7.910 ms\n";
    assert_eq!(parse_probe(text), Some(Probe { sent: 913, received: 680, ms: 2998 }));
    assert_eq!(parse_probe("60 packets transmitted, 1 received, +12 errors, 98.3333% packet loss, time 3060ms"), Some(Probe { sent: 60, received: 1, ms: 3060 }));
    assert_eq!(parse_probe("ping: sendmsg: Network is unreachable"), None);
    assert_eq!(parse_rx_mcs("signal: -35 dBm\n\trx bitrate:\t34.4 MBit/s 40MHz HE-MCS 0 HE-NSS 2 HE-GI 0 HE-DCM 0"), Some(0));
    assert_eq!(parse_rx_mcs("rx bitrate: 866.7 MBit/s VHT-MCS 9 80MHz short GI VHT-NSS 2"), Some(9));
    assert_eq!(parse_rx_mcs("rx bitrate: 144.4 MBit/s MCS 15 short GI"), Some(15));
    assert_eq!(parse_rx_mcs("rx bitrate: 54.0 MBit/s"), None);
}

#[test]
fn time_spent_probing_is_not_mistaken_for_stopped_traffic() {
    let mut r = Rig::new(5220, 160, 160);
    r.probe = Some(Box::new(|_| GOOD));
    r.traffic(4, |_| 500);
    assert!(matches!(r.c.phase, Phase::Applying(_)));
    r.traffic(1, |_| 500);
    assert!(matches!(r.c.phase, Phase::Settling(2)));
    // 真机：探测清空计数端点，下一个 tick 没有样本。
    r.tick(None);
    assert!(matches!(r.c.phase, Phase::Settling(2)), "{:?}", r.c.phase);
    r.traffic(8, |w| if w == 160 { 500 } else { 300 });
    assert_eq!(r.c.candidates.iter().find(|c| c.width == 80).unwrap().result, ResultKind::Lost);
}

#[test]
fn a_load_jump_between_idle_windows_is_unknown_but_saturated_gains_still_count() {
    // 真机 W_Game（5 GHz）：锚点窗只有 3 Mbit/s，候选窗赶上测速，40 MHz 凭虚高的 E 判了 Won。
    let a = Score { e: 157.59, r: 3.00, airtime_ms: 251, seconds: 13.2, ..score(0.0) };
    let b = Score { e: 209.98, r: 22.35, airtime_ms: 1402, seconds: 13.2, ..score(0.0) };
    assert_eq!(a.compare(b), ResultKind::Unknown);
    assert_eq!(b.compare(a), ResultKind::Unknown);
    // 同一次关联里正常判负的那一挑战：两窗都没用满，R 51 对 26，差距不到 2 倍。
    let a = Score { e: 613.02, r: 50.93, ..score(0.0) };
    let b = Score { e: 339.93, r: 26.13, ..score(0.0) };
    assert_eq!(a.compare(b), ResultKind::Lost);
    // 阶段 B 的 W（2.4 GHz）满载：40→20 的 R 涨 3.7 倍是真实容量差。
    let a = Score { e: 41.0, r: 28.4, ..score(0.0) };
    let b = Score { e: 166.0, r: 104.0, ..score(0.0) };
    assert_eq!(a.compare(b), ResultKind::Won);
}

#[test]
fn an_e_drop_from_lower_demand_does_not_count_as_a_collapse() {
    let mut r = Rig::new(5220, 160, 160);
    r.c.last_challenge = Some(r.now);
    // 锚点窗口 R/E 都远低于 0.25：先 51 Mbit/s，后需求降到 3 Mbit/s，E 也跟着掉一半以上。
    r.c.anchor_score = Some(Score { e: 613.0, r: 51.0, ..score(0.0) });
    let mut idle = sample(150);
    idle.bytes = 1_125_000;
    idle.airtime_ms = 60;
    for _ in 0..6 { r.tick(Some(idle)); }
    assert!(r.writes.is_empty(), "{:?}", r.writes);
    // 同样掉一半但需求没变（R 相近）才是塌陷，提前挑战。
    r.c.anchor_score = Some(Score { e: 613.0, r: 3.2, ..score(0.0) });
    for _ in 0..6 { r.tick(Some(idle)); }
    assert_eq!(r.writes, [80]);
}

#[test]
fn spread_is_the_coefficient_of_variation_of_per_tick_rates() {
    let now = Instant::now();
    let mut window = Window::default();
    let mut done = None;
    for (i, e) in [30u64, 90, 30, 90].into_iter().enumerate() {
        done = window.push(now + Duration::from_secs(3 * (i as u64 + 1)), sample(e));
    }
    // 10、30、10、30 Mbit/s：均值 20、标准差 10。
    let score = done.unwrap();
    assert!((score.spread - 0.5).abs() < 1e-9, "{}", score.spread);
    assert!(score.steady());
    let mut window = Window::default();
    for (i, e) in [300u64, 30, 300, 30].into_iter().enumerate() {
        done = window.push(now + Duration::from_secs(3 * (i as u64 + 1)), sample(e));
    }
    assert!(!done.unwrap().steady());
}

#[test]
fn bursty_anchor_windows_defer_challenges_without_spending_budget() {
    let mut r = Rig::new(5220, 160, 160);
    for i in 0..12 { r.tick(Some(sample(if i % 2 == 0 { 300 } else { 30 }))); }
    assert!(r.writes.is_empty());
    assert!(r.c.budget.is_empty() && r.c.last_challenge.is_none());
    r.traffic(5, |_| 300);
    assert_eq!(r.writes, [80]);
}

#[test]
fn an_unsteady_candidate_window_cannot_win() {
    let mut r = Rig::new(5220, 160, 160);
    r.traffic(4, |_| 200);
    assert_eq!(r.writes, [80]);
    r.traffic(3, |_| 200);
    assert!(matches!(r.c.phase, Phase::Measuring));
    // 均值 E 300 本可胜过 200，但逐 tick 需求在 167 与 33 Mbit/s 之间跳。
    for i in 0..4 { r.tick(Some(sample(if i % 2 == 0 { 500 } else { 100 }))); }
    assert_eq!(r.c.candidates.iter().find(|c| c.width == 80).unwrap().result, ResultKind::Unknown);
    assert_eq!(r.c.anchor, 160);
    assert_eq!(r.writes.last(), Some(&160));
}

#[test]
fn adaptive_width_requires_explicit_opt_in() {
    for value in ["", "off", "unexpected"] { assert_eq!(Mode::parse(value), Mode::Off); }
    assert_eq!(Mode::parse("auto"), Mode::Auto);
    assert_eq!(Mode::parse("observe"), Mode::Observe);
}
