//! Link-layer statistics of the relay STA, for width measurement.
//!
//! qcacld exposes per-radio airtime (`on_time`, `cca_busy_time`, `tx_time`,
//! `rx_time`, ms) in debugfs `ll_stats` once LL stats are enabled on the
//! interface, which needs the QCA vendor command `LL_STATS_SET` over nl80211.
//! `iw vendor send` cannot do it here: the kernel wants the vendor data
//! flagged nested. Delivered downlink bits per second of receive airtime then
//! separates widths by what the link actually carries (on a busy 2.4 GHz
//! channel 40 MHz gave 0-42 Mbit/s per airtime-second, 20 MHz 101-129) and
//! needs no saturated link.
// Host builds (macOS) have no netlink; the constants serve the Linux path.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const GENL_ID_CTRL: u16 = 0x10;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const NL80211_CMD_VENDOR: u8 = 103;
const NL80211_ATTR_IFINDEX: u16 = 3;
const NL80211_ATTR_VENDOR_ID: u16 = 195;
const NL80211_ATTR_VENDOR_SUBCMD: u16 = 196;
const NL80211_ATTR_VENDOR_DATA: u16 = 197;
const NLA_F_NESTED: u16 = 0x8000;
const QCA_OUI: u32 = 0x001374;
const QCA_LL_STATS_SET: u32 = 14;
/// `LL_STATS_SET` attributes: MPDU size threshold, aggressive gathering (off).
const LL_MPDU_SIZE_THRESHOLD: u16 = 1;
const LL_AGGRESSIVE: u16 = 2;

fn nla(buf: &mut Vec<u8>, kind: u16, value: &[u8]) {
    buf.extend_from_slice(&((4 + value.len()) as u16).to_ne_bytes());
    buf.extend_from_slice(&kind.to_ne_bytes());
    buf.extend_from_slice(value);
    buf.resize(buf.len().next_multiple_of(4), 0);
}

fn attributes(mut payload: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    std::iter::from_fn(move || {
        if payload.len() < 4 {
            return None;
        }
        let len = u16::from_ne_bytes([payload[0], payload[1]]) as usize;
        let kind = u16::from_ne_bytes([payload[2], payload[3]]) & !NLA_F_NESTED;
        if len < 4 || len > payload.len() {
            return None;
        }
        let value = &payload[4..len];
        payload = &payload[len.next_multiple_of(4).min(payload.len())..];
        Some((kind, value))
    })
}

#[cfg(target_os = "linux")]
struct Genl(OwnedFd);

#[cfg(target_os = "linux")]
impl Genl {
    fn open() -> Result<Self, String> {
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, libc::NETLINK_GENERIC) };
        if fd < 0 {
            return Err("Cannot open nl80211".into());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let timeout = libc::timeval { tv_sec: 1, tv_usec: 0 };
        let size = std::mem::size_of::<libc::timeval>() as libc::socklen_t;
        unsafe { libc::setsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVTIMEO, (&timeout as *const libc::timeval).cast(), size) };
        Ok(Self(fd))
    }

    /// One acknowledged request; returns each reply's attributes payload.
    fn call(&self, family: u16, cmd: u8, attrs: &[u8], seq: u32) -> Result<Vec<Vec<u8>>, String> {
        let mut msg = Vec::with_capacity(20 + attrs.len());
        msg.extend_from_slice(&((20 + attrs.len()) as u32).to_ne_bytes());
        msg.extend_from_slice(&family.to_ne_bytes());
        msg.extend_from_slice(&((libc::NLM_F_REQUEST | libc::NLM_F_ACK) as u16).to_ne_bytes());
        msg.extend_from_slice(&seq.to_ne_bytes());
        msg.extend_from_slice(&0u32.to_ne_bytes());
        msg.extend_from_slice(&[cmd, 1, 0, 0]);
        msg.extend_from_slice(attrs);
        let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        let size = std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
        let sent = unsafe { libc::sendto(self.0.as_raw_fd(), msg.as_ptr().cast(), msg.len(), 0, (&kernel as *const libc::sockaddr_nl).cast(), size) };
        if sent < 0 {
            return Err("nl80211 request failed".into());
        }
        let mut replies = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = unsafe { libc::recv(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n <= 0 {
                return Err("nl80211 did not answer".into());
            }
            let mut rest = &buf[..n as usize];
            while rest.len() >= 16 {
                let len = u32::from_ne_bytes(rest[0..4].try_into().unwrap()) as usize;
                let kind = u16::from_ne_bytes([rest[4], rest[5]]);
                let reply_seq = u32::from_ne_bytes(rest[8..12].try_into().unwrap());
                if len < 16 || len > rest.len() {
                    break;
                }
                if reply_seq == seq {
                    match kind {
                        NLMSG_ERROR => {
                            let code = rest.get(16..20).map_or(-1, |b| i32::from_ne_bytes(b.try_into().unwrap()));
                            return if code == 0 { Ok(replies) } else { Err(format!("nl80211 refused the request ({code})")) };
                        }
                        NLMSG_DONE => return Ok(replies),
                        _ => replies.push(rest.get(20..len).unwrap_or_default().to_vec()),
                    }
                }
                rest = &rest[len.next_multiple_of(4).min(rest.len())..];
            }
        }
    }
}

/// Enables LL stats on `iface` (per vdev; again after each association).
#[cfg(not(target_os = "linux"))]
pub(super) fn enable(_iface: &str) -> Result<(), String> {
    Err("nl80211 is Linux-only".into())
}

/// Enables LL stats on `iface` (per vdev; again after each association).
#[cfg(target_os = "linux")]
pub(super) fn enable(iface: &str) -> Result<(), String> {
    crate::httpd::gate::debug_assert_may_block();
    let genl = Genl::open()?;
    let mut name = Vec::new();
    nla(&mut name, CTRL_ATTR_FAMILY_NAME, b"nl80211\0");
    let family = genl
        .call(GENL_ID_CTRL, CTRL_CMD_GETFAMILY, &name, 1)?
        .iter()
        .find_map(|p| attributes(p).find(|(k, _)| *k == CTRL_ATTR_FAMILY_ID).and_then(|(_, v)| Some(u16::from_ne_bytes(v.get(..2)?.try_into().ok()?))))
        .ok_or("nl80211 is unavailable")?;
    let index: u32 = std::fs::read_to_string(format!("/sys/class/net/{iface}/ifindex"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or("Unknown relay interface")?;
    let mut data = Vec::new();
    nla(&mut data, LL_MPDU_SIZE_THRESHOLD, &128u32.to_ne_bytes());
    nla(&mut data, LL_AGGRESSIVE, &0u32.to_ne_bytes());
    let mut attrs = Vec::new();
    nla(&mut attrs, NL80211_ATTR_IFINDEX, &index.to_ne_bytes());
    nla(&mut attrs, NL80211_ATTR_VENDOR_ID, &QCA_OUI.to_ne_bytes());
    nla(&mut attrs, NL80211_ATTR_VENDOR_SUBCMD, &QCA_LL_STATS_SET.to_ne_bytes());
    nla(&mut attrs, NL80211_ATTR_VENDOR_DATA | NLA_F_NESTED, &data);
    genl.call(family, NL80211_CMD_VENDOR, &attrs, 2).map(|_| ())
}

/// Current LL stats text; each read asks the firmware afresh.
pub(super) fn read(iface: &str) -> Result<String, String> {
    crate::httpd::gate::debug_assert_may_block();
    // Bounded: the driver waits a fixed time for the firmware's answer.
    std::fs::read_to_string(format!("/sys/kernel/debug/{iface}/ll_stats")).map_err(|_| "Link-layer stats are unavailable".into())
}

fn field(line: &str, key: &str) -> Option<u64> {
    let mut words = line.split_whitespace();
    words.by_ref().find(|w| *w == key)?;
    let value = words.next()?.trim_end_matches(',').parse().ok()?;
    (!words.any(|w| w == key)).then_some(value)
}

/// 原始累计计数；只解析文本，不读设备，也不在单次快照里猜测承载链路的 radio。
#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    primary: u32,
    radios: BTreeMap<u32, Radio>,
}

#[derive(Clone, Debug)]
struct Radio {
    rx_ms: u64,
    channels: BTreeMap<Channel, u64>,
}

/// 顺序不是身份：同一主信道可同时留下不同宽度或中心频率的历史记录。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Channel {
    width: u32,
    primary: u32,
    center0: u32,
    center1: u32,
}

/// 缺少字段、计数冲突或条目数不全时不可比，调用方应丢弃旧快照并重开 epoch。
/// 相同 channel identity 的重复记录只计一次；不同 identity 留到差分时逐项检查。
pub(super) fn snapshot(stats: &str, primary: u32) -> Option<Snapshot> {
    let mut radios = BTreeMap::new();
    let mut current = None;
    let mut expected_radios = None;
    let mut expected_channels = None;
    let mut seen_channels = 0;
    for line in stats.lines().map(str::trim) {
        if line.starts_with("===LL_STATS_RADIO:") {
            if expected_radios.replace(field(line.trim_end_matches('='), "radios:")?).is_some() {
                return None;
            }
        } else if line.starts_with("Radio:") {
            if current.is_some() && expected_channels.is_some_and(|count| count != seen_channels) {
                return None;
            }
            let id = u32::try_from(field(line, "Radio:")?).ok()?;
            let radio = Radio { rx_ms: field(line, "rx_time:")?, channels: BTreeMap::new() };
            if radios.insert(id, radio).is_some() {
                return None;
            }
            current = Some(id);
            expected_channels = None;
            seen_channels = 0;
        } else if line.starts_with("Num channels:") {
            current?;
            if expected_channels.replace(field(line, "channels:")?).is_some() {
                return None;
            }
        } else if line.starts_with("Chan width:") {
            let channel = Channel {
                width: u32::try_from(field(line, "width:")?).ok()?,
                primary: u32::try_from(field(line, "center_freq:")?).ok()?,
                center0: u32::try_from(field(line, "center_freq0:")?).ok()?,
                center1: u32::try_from(field(line, "center_freq1:")?).ok()?,
            };
            let on_ms = field(line, "on_time:")?;
            let radio = radios.get_mut(&current?)?;
            if radio.channels.insert(channel, on_ms).is_some_and(|old| old != on_ms) {
                return None;
            }
            seen_channels += 1;
        }
    }
    // 支持裁剪过的文本；若有条目数声明，则按原始行数核对，不能默许截断。
    if radios.is_empty() || expected_radios.is_some_and(|count| count != radios.len() as u64) || expected_channels.is_some_and(|count| count != seen_channels) {
        return None;
    }
    for radio in radios.values_mut() {
        // 其它信道只校验输入完整性，不参与当前主信道的差分与选取。
        radio.channels.retain(|channel, _| channel.primary == primary);
    }
    radios.values().any(|radio| !radio.channels.is_empty()).then_some(Snapshot { primary, radios })
}

#[derive(Debug, PartialEq)]
pub(super) enum Delta {
    Sample { radio: u32, rx_ms: u64 },
    Ambiguous,
    Reset,
}

/// 按本 tick 当前主信道 on_time 的增量选 radio，返回该 radio 的 rx_time 增量。
/// 任何参与计数下降（含回绕）、radio/主信道/channel identity 集合改变都作废。
/// radio 迁移仍返回新 ID，由调用方对照前次 ID 重开 epoch，不跨 radio 累计窗口。
pub(super) fn rx_airtime(before: &Snapshot, after: &Snapshot) -> Delta {
    if before.primary != after.primary || before.radios.len() != after.radios.len() {
        return Delta::Reset;
    }
    let mut best = (0, 0, 0);
    let mut runner_up = 0;
    for (&id, radio) in &after.radios {
        let Some(old) = before.radios.get(&id) else { return Delta::Reset };
        if old.channels.len() != radio.channels.len() {
            return Delta::Reset;
        }
        let Some(rx_ms) = radio.rx_ms.checked_sub(old.rx_ms) else { return Delta::Reset };
        let mut on_ms = 0u64;
        for (channel, &on) in &radio.channels {
            let Some(&old_on) = old.channels.get(channel) else { return Delta::Reset };
            let Some(delta) = on.checked_sub(old_on) else { return Delta::Reset };
            // 先逐项检查连续性再求和，不能让另一宽度的增长掩盖某项重置。
            let Some(total) = on_ms.checked_add(delta) else { return Delta::Reset };
            on_ms = total;
        }
        if on_ms > best.0 {
            runner_up = best.0;
            best = (on_ms, id, rx_ms);
        } else {
            runner_up = runner_up.max(on_ms);
        }
    }
    // 保守初值：领先不超过 max(10 ms, 最大增量的 5% 向上取整) 都视为并列。
    // 包含边界；全零和只有极小增量也无结论。不用浮点，避免大计数丢精度。
    let tolerance = best.0.div_ceil(20).max(10);
    if best.0 == 0 || best.0 - runner_up <= tolerance {
        Delta::Ambiguous
    } else {
        Delta::Sample { radio: best.1, rx_ms: best.2 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B31 真机 `ll_stats` 的裁剪记录，STA 位于 2412 MHz（40 MHz）。
    const STATS: &str = "===LL_STATS_IFACE: num_peers: 1===\nbeacon_rx: 2867, mgmt_rx: 2887\n\n===LL_STATS_RADIO: number of radios: 2===\nRadio: 0 on_time: 60307669, tx_time: 2061865, rx_time: 800656, on_time_scan: 126631, on_time_nbd: 0\ntotal_num_tx_pwr_levels: 180\n0 0 0 275 0\nNum channels: 4\nChan width: 3, center_freq: 5180, center_freq0: 5210, center_freq1: 5250, on_time: 474, cca_busy_time: 9, tx_time: 0, rx_time: 0\nChan width: 0, center_freq: 2462, center_freq0: 2462, center_freq1: 0, on_time: 42210125, cca_busy_time: 13525720, tx_time: 1305819, rx_time: 17071\nChan width: 1, center_freq: 2412, center_freq0: 2422, center_freq1: 0, on_time: 18041873, cca_busy_time: 10136242, tx_time: 749567, rx_time: 4936859\nChan width: 0, center_freq: 2412, center_freq0: 2412, center_freq1: 0, on_time: 5000, cca_busy_time: 10, tx_time: 1, rx_time: 2\nRadio: 1 on_time: 55215472, tx_time: 646712, rx_time: 326952, on_time_scan: 74786\nNum channels: 2\nChan width: 0, center_freq: 5180, center_freq0: 5180, center_freq1: 0, on_time: 40385199, cca_busy_time: 175221, tx_time: 291493, rx_time: 30045\nChan width: 0, center_freq: 2412, center_freq0: 2412, center_freq1: 0, on_time: 9000, cca_busy_time: 1, tx_time: 0, rx_time: 0\n";

    // 在真机累计值上增加一个 tick；on 依次为 radio 0 的 40/20 MHz、radio 1 的 20 MHz。
    fn later(on: [u64; 3], rx: [u64; 2]) -> String {
        STATS
            .replace("on_time: 18041873,", &format!("on_time: {},", 18_041_873 + on[0]))
            .replace("on_time: 5000,", &format!("on_time: {},", 5_000 + on[1]))
            .replace("on_time: 9000,", &format!("on_time: {},", 9_000 + on[2]))
            .replace("rx_time: 800656,", &format!("rx_time: {},", 800_656 + rx[0]))
            .replace("rx_time: 326952,", &format!("rx_time: {},", 326_952 + rx[1]))
    }

    fn delta(after: &str) -> Delta {
        rx_airtime(&snapshot(STATS, 2412).unwrap(), &snapshot(after, 2412).unwrap())
    }

    fn duplicate_channel(stats: &str) -> String {
        let line = stats.lines().find(|line| line.starts_with("Chan width: 0, center_freq: 2412,")).unwrap();
        stats.replacen("Num channels: 4", "Num channels: 5", 1).replacen(line, &format!("{line}\n{line}"), 1)
    }

    #[test]
    fn snapshot_keeps_radio_counters_and_stable_primary_channel_identities() {
        let stats = snapshot(STATS, 2412).unwrap();
        assert_eq!(stats.primary, 2412);
        assert_eq!(stats.radios.len(), 2);
        assert_eq!(stats.radios[&0].rx_ms, 800_656);
        assert_eq!(stats.radios[&1].rx_ms, 326_952);
        assert_eq!(stats.radios[&0].channels.len(), 2);
        assert_eq!(stats.radios[&1].channels.len(), 1);
        let forty = Channel { width: 1, primary: 2412, center0: 2422, center1: 0 };
        let twenty = Channel { width: 0, primary: 2412, center0: 2412, center1: 0 };
        assert_eq!(stats.radios[&0].channels[&forty], 18_041_873);
        assert_eq!(stats.radios[&0].channels[&twenty], 5_000);
        assert_eq!(stats.radios[&1].channels[&twenty], 9_000);
        assert!(snapshot(STATS, 5745).is_none());
        assert!(snapshot("Link-layer stats are not enabled", 2412).is_none());
        assert!(snapshot("", 2412).is_none());
        assert!(snapshot("===LL_STATS_RADIO: number of radios: 0===", 2412).is_none());
    }

    #[test]
    fn migration_follows_tick_on_time_not_historical_on_time_or_rx_time() {
        let before = snapshot(STATS, 2412).unwrap();
        let first = snapshot(&later([3_000, 0, 10], [120, 0]), 2412).unwrap();
        assert_eq!(rx_airtime(&before, &first), Delta::Sample { radio: 0, rx_ms: 120 });
        // radio 0 的历史累计仍大得多，本 tick 的 RX 增量也更大，但主信道已迁到 radio 1。
        let second = snapshot(&later([3_010, 0, 3_010], [1_120, 250]), 2412).unwrap();
        assert_eq!(rx_airtime(&first, &second), Delta::Sample { radio: 1, rx_ms: 250 });
    }

    #[test]
    fn five_ghz_also_uses_the_current_tick() {
        let after = later([3_000, 0, 10], [75, 1_000])
            .replace("on_time: 474,", "on_time: 3474,")
            .replace("on_time: 40385199,", "on_time: 40385209,");
        assert_eq!(rx_airtime(&snapshot(STATS, 5180).unwrap(), &snapshot(&after, 5180).unwrap()), Delta::Sample { radio: 0, rx_ms: 75 });
    }

    #[test]
    fn different_widths_on_the_same_primary_are_summed() {
        assert_eq!(delta(&later([1_600, 1_400, 2_500], [50, 200])), Delta::Sample { radio: 0, rx_ms: 50 });
    }

    #[test]
    fn duplicate_channel_identity_is_counted_once_even_if_only_one_snapshot_repeats_it() {
        let after = later([0, 100, 150], [12, 35]);
        assert_eq!(delta(&duplicate_channel(&after)), Delta::Sample { radio: 1, rx_ms: 35 });
        assert_eq!(rx_airtime(&snapshot(&duplicate_channel(STATS), 2412).unwrap(), &snapshot(&after, 2412).unwrap()), Delta::Sample { radio: 1, rx_ms: 35 });
        let conflict = duplicate_channel(STATS).replacen("on_time: 5000,", "on_time: 4999,", 1);
        assert!(snapshot(&conflict, 2412).is_none(), "相同 identity 的计数冲突不能取最大值掩盖");
    }

    #[test]
    fn zero_ties_and_conservative_tolerance_boundaries_are_ambiguous() {
        // 最大/次大之差必须严格大于 max(10 ms, ceil(最大值 / 20))。
        for (zero, one, winner) in [
            (0, 0, None),
            (3_000, 3_000, None),
            (3_000, 2_850, None),
            (3_000, 2_849, Some(0)),
            (2_850, 3_000, None),
            (2_849, 3_000, Some(1)),
            (3_001, 2_850, None),
            (3_001, 2_849, Some(0)),
            (100, 90, None),
            (100, 89, Some(0)),
            (90, 100, None),
            (89, 100, Some(1)),
            (10, 0, None),
            (11, 0, Some(0)),
            (0, 10, None),
            (0, 11, Some(1)),
        ] {
            let expected = match winner {
                Some(0) => Delta::Sample { radio: 0, rx_ms: 37 },
                Some(1) => Delta::Sample { radio: 1, rx_ms: 91 },
                _ => Delta::Ambiguous,
            };
            assert_eq!(delta(&later([zero, 0, one], [37, 91])), expected, "on_time 增量 {zero}/{one}");
        }
    }

    #[test]
    fn a_third_radio_is_included_in_the_runner_up() {
        let third = "Radio: 7 rx_time: 100\nNum channels: 1\nChan width: 0, center_freq: 2412, center_freq0: 2412, center_freq1: 0, on_time: 10000\n";
        let before = format!("{}{third}", STATS.replace("number of radios: 2", "number of radios: 3"));
        let after = format!("{}{}", later([3_000, 0, 10], [50, 0]).replace("number of radios: 2", "number of radios: 3"), third.replace("on_time: 10000", "on_time: 12900"));
        assert_eq!(rx_airtime(&snapshot(&before, 2412).unwrap(), &snapshot(&after, 2412).unwrap()), Delta::Ambiguous);
    }

    #[test]
    fn positive_on_time_can_return_zero_rx_without_inventing_airtime() {
        assert_eq!(delta(&later([3_000, 0, 10], [0, 500])), Delta::Sample { radio: 0, rx_ms: 0 });
    }

    #[test]
    fn rx_reset_on_any_radio_invalidates_the_tick() {
        for (old, new) in [("rx_time: 800856,", "rx_time: 800655,"), ("rx_time: 326952,", "rx_time: 0,")] {
            assert_eq!(delta(&later([3_000, 0, 10], [200, 0]).replace(old, new)), Delta::Reset);
        }
        // 即使没有任何 on_time 增量，也先报告重置而不是含糊。
        assert_eq!(delta(&STATS.replace("rx_time: 326952,", "rx_time: 326951,")), Delta::Reset);
    }

    #[test]
    fn every_participating_channel_is_checked_before_summing_or_selecting() {
        for (old, new) in [("on_time: 18044873,", "on_time: 0,"), ("on_time: 5000,", "on_time: 4999,"), ("on_time: 9010,", "on_time: 8999,")] {
            let after = later([3_000, 0, 10], [100, 0]).replace(old, new);
            assert_eq!(delta(&after), Delta::Reset, "不能用其它宽度或 radio 的增长掩盖 {old} 下降");
        }
    }

    #[test]
    fn counter_wrap_is_reset_not_a_saturating_or_wrapping_delta() {
        for limit in [u64::from(u32::MAX), u64::MAX] {
            for key in ["rx_time: 800656,", "on_time: 18041873,"] {
                let name = key.split_whitespace().next().unwrap();
                let before = STATS.replace(key, &format!("{name} {limit},"));
                let after = STATS.replace(key, &format!("{name} 7,"));
                assert_eq!(rx_airtime(&snapshot(&before, 2412).unwrap(), &snapshot(&after, 2412).unwrap()), Delta::Reset);
            }
        }
    }

    #[test]
    fn sums_and_tolerance_do_not_overflow_large_counters() {
        let before = STATS.replace("on_time: 18041873,", "on_time: 0,").replace("on_time: 5000,", "on_time: 0,").replace("rx_time: 800656,", "rx_time: 0,");
        let after = before.replace("on_time: 0, cca_busy_time: 10136242,", &format!("on_time: {}, cca_busy_time: 10136242,", u64::MAX)).replace("rx_time: 0, on_time_scan: 126631,", &format!("rx_time: {}, on_time_scan: 126631,", u64::MAX));
        let a = snapshot(&before, 2412).unwrap();
        assert_eq!(rx_airtime(&a, &snapshot(&after, 2412).unwrap()), Delta::Sample { radio: 0, rx_ms: u64::MAX });
        let overflow = after.replace("on_time: 0, cca_busy_time: 10,", "on_time: 1, cca_busy_time: 10,");
        assert_eq!(rx_airtime(&a, &snapshot(&overflow, 2412).unwrap()), Delta::Reset);
    }

    #[test]
    fn missing_malformed_or_duplicate_participating_fields_are_unusable() {
        for (old, new) in [
            ("Radio: 0 ", "Radio: "),
            ("Radio: 0 ", "Radio: 4294967296 "),
            ("rx_time: 800656,", ""),
            ("rx_time: 326952,", ""),
            ("rx_time: 800656,", "rx_time: -1,"),
            ("rx_time: 800656,", "rx_time: 18446744073709551616,"),
            ("rx_time: 800656,", "rx_time: 800656, rx_time: 800656,"),
            ("width: 1,", "width: missing,"),
            ("center_freq: 2412,", ""),
            ("center_freq0: 2422,", ""),
            ("center_freq0: 2422,", "center_freq0: 4294967296,"),
            ("center_freq1: 0, on_time: 18041873,", "on_time: 18041873,"),
            ("on_time: 18041873,", ""),
            ("on_time: 18041873,", "on_time: -1,"),
            ("on_time: 18041873,", "on_time: 18041873, on_time: 1,"),
        ] {
            assert!(snapshot(&STATS.replacen(old, new, 1), 2412).is_none(), "无效字段 {old} → {new}");
        }
    }

    #[test]
    fn truncated_or_contradictory_structure_cannot_be_used() {
        for (old, new) in [
            ("number of radios: 2", "number of radios: 3"),
            ("number of radios: 2", "number of radios: 1"),
            ("Num channels: 4", "Num channels: 5"),
            ("Num channels: 4", "Num channels: 3"),
            ("Num channels: 2", "Num channels: 3"),
            ("Radio: 1 ", "Radio: 0 "),
            ("Num channels: 4", "Num channels: 4\nNum channels: 4"),
        ] {
            assert!(snapshot(&STATS.replace(old, new), 2412).is_none(), "结构不完整或冲突 {old} → {new}");
        }
        let truncated = STATS.trim_end().rsplit_once('\n').unwrap().0;
        assert!(snapshot(truncated, 2412).is_none());
        let orphan = STATS.lines().find(|line| line.starts_with("Chan width:")).unwrap();
        assert!(snapshot(&format!("{orphan}\n{STATS}"), 2412).is_none());
        assert!(snapshot(&format!("===LL_STATS_RADIO: number of radios: 2===\n{STATS}"), 2412).is_none());
    }

    #[test]
    fn changed_radio_primary_or_channel_identity_resets_even_with_monotonic_counters() {
        for (old, new) in [
            ("Radio: 0 ", "Radio: 7 "),
            ("Chan width: 1,", "Chan width: 2,"),
            ("center_freq: 2412, center_freq0: 2422,", "center_freq: 2437, center_freq0: 2422,"),
            ("center_freq0: 2422,", "center_freq0: 2427,"),
            ("center_freq0: 2422, center_freq1: 0,", "center_freq0: 2422, center_freq1: 2432,"),
        ] {
            assert_eq!(delta(&later([3_000, 0, 10], [100, 0]).replacen(old, new, 1)), Delta::Reset, "identity 改变 {old} → {new}");
        }
        assert_eq!(rx_airtime(&snapshot(STATS, 2412).unwrap(), &snapshot(STATS, 5180).unwrap()), Delta::Reset);
    }

    #[test]
    fn added_or_removed_radios_and_primary_channels_reset() {
        let next = later([3_000, 0, 10], [100, 0]);
        let removed_radio = next.split_once("Radio: 1 ").unwrap().0.replace("number of radios: 2", "number of radios: 1");
        let added_channel = next.replace("Num channels: 4", "Num channels: 5").replace("Radio: 1 ", "Chan width: 2, center_freq: 2412, center_freq0: 2442, center_freq1: 0, on_time: 100\nRadio: 1 ");
        let line = next.lines().find(|line| line.starts_with("Chan width: 0, center_freq: 2412,")).unwrap();
        let removed_channel = next.replacen(&format!("{line}\n"), "", 1).replace("Num channels: 4", "Num channels: 3");
        for stats in [removed_radio, added_channel, removed_channel] {
            let a = snapshot(STATS, 2412).unwrap();
            let b = snapshot(&stats, 2412).unwrap();
            assert_eq!(rx_airtime(&a, &b), Delta::Reset);
            assert_eq!(rx_airtime(&b, &a), Delta::Reset);
        }
    }

    #[test]
    fn radio_and_channel_order_do_not_change_identity() {
        let next = later([1_600, 1_400, 2_500], [100, 0]);
        let forty = next.lines().find(|line| line.contains("center_freq0: 2422,")).unwrap();
        let twenty = next.lines().find(|line| line.starts_with("Chan width: 0, center_freq: 2412,")).unwrap();
        let next = next.replace(&format!("{forty}\n{twenty}"), &format!("{twenty}\n{forty}"));
        let (header, radios) = next.split_once("Radio: 0 ").unwrap();
        let (zero, one) = radios.split_once("Radio: 1 ").unwrap();
        let reordered = format!("{header}Radio: 1 {one}Radio: 0 {zero}");
        assert_eq!(delta(&reordered), Delta::Sample { radio: 0, rx_ms: 100 });
        let before = STATS.replace("Radio: 0 ", "Radio: 42 ").replace("Radio: 1 ", "Radio: 7 ");
        let after = reordered.replace("Radio: 0 ", "Radio: 42 ").replace("Radio: 1 ", "Radio: 7 ");
        assert_eq!(rx_airtime(&snapshot(&before, 2412).unwrap(), &snapshot(&after, 2412).unwrap()), Delta::Sample { radio: 42, rx_ms: 100 });
    }

    #[test]
    fn other_channels_do_not_participate_in_the_current_primary_delta() {
        let after = later([0, 0, 3_000], [0, 100]).replace("on_time: 40385199,", "on_time: 0,").replace("center_freq0: 5210,", "center_freq0: 5190,");
        assert_eq!(delta(&after), Delta::Sample { radio: 1, rx_ms: 100 });
    }

    #[test]
    fn trimmed_input_without_optional_entry_counts_still_requires_all_counter_fields() {
        let trim = |stats: &str| stats.lines().filter(|line| line.starts_with("Radio:") || line.starts_with("Chan width:")).collect::<Vec<_>>().join("\n");
        let a = snapshot(&trim(STATS), 2412).unwrap();
        let b = snapshot(&trim(&later([3_000, 0, 10], [100, 0])), 2412).unwrap();
        assert_eq!(rx_airtime(&a, &b), Delta::Sample { radio: 0, rx_ms: 100 });
        assert!(snapshot(&trim(STATS).replace("rx_time: 326952,", ""), 2412).is_none());
    }

    #[test]
    fn netlink_attributes_round_trip_with_padding_and_nesting() {
        let mut buf = Vec::new();
        nla(&mut buf, 2, b"nl80211\0");
        nla(&mut buf, 197 | NLA_F_NESTED, &[1, 2, 3]);
        nla(&mut buf, 3, &7u32.to_ne_bytes());
        assert_eq!(buf.len() % 4, 0);
        let parsed: Vec<_> = attributes(&buf).collect();
        assert_eq!(parsed, [(2, &b"nl80211\0"[..]), (197, &[1, 2, 3][..]), (3, &7u32.to_ne_bytes()[..])]);
        assert_eq!(attributes(&[8, 0, 1]).count(), 0, "truncated input yields nothing");
    }
}
