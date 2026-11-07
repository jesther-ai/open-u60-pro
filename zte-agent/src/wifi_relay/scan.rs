//! Upstream scans over wpa_supplicant's control socket.
//!
//! The B31 WLAN driver (qcacld 5.2.1.110D) removes from every multi-channel
//! STA scan the 5 GHz channels that share a MAC with an AP it counts as DFS
//! beaconing, and any 5 GHz AP at 160 MHz counts, such as the main hotspot
//! on channel 36 (`policy_mgr_scan_trim_5g_chnls_for_dfs_ap`, applied in
//! `scm_scan_start_req`). A list left empty is refused ("Reject 0 channel
//! Scan"). One-channel requests skip that filtering, so each 5 GHz channel is
//! requested on its own; 2.4 GHz channels are kept and stay one request.
//! wpa_supplicant reports even a refused scan as finished and refills its
//! table from the kernel's cache, so the caller keeps only entries whose age
//! fits inside this scan.

use std::collections::VecDeque;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `BSS` fields: id, bssid, freq, level, age, flags, ssid and the entry delimiter.
const BSS_MASK: u32 = 0x1 | 0x2 | 0x4 | 0x80 | 0x200 | 0x800 | 0x1000 | 0x20000;

/// One read of the PHY's enabled frequencies drives scanning, selection and
/// hotspot preparation. DFS is a property of the radio, not a relay blacklist.
pub(super) struct Channels(Vec<(u32, bool)>);

impl Channels {
    pub(super) fn parse(text: &str) -> Self {
        let mut channels = Vec::new();
        let mut current = None;
        for line in text.lines() {
            if let Some((mhz, rest)) = line.trim_start().strip_prefix("* ").and_then(|l| l.split_once(" MHz [")) {
                current = None;
                if let Ok(frequency) = mhz.parse::<u32>() {
                    if super::channel(frequency).is_some() && !rest.contains("(disabled)") {
                        current = Some(channels.len());
                        channels.push((frequency, false));
                    }
                }
            } else if line.trim() == "Radar detection" {
                if let Some(index) = current { channels[index].1 = true; }
            }
        }
        Self(channels)
    }

    pub(super) fn contains(&self, frequency: u32) -> bool {
        self.0.iter().any(|&(f, _)| f == frequency)
    }

    pub(super) fn staging_5ghz(&self) -> Option<u32> {
        self.0.iter().find(|&&(f, dfs)| f >= 5000 && !dfs).map(|&(f, _)| f)
    }

    /// Group 2.4 GHz, then scan each 5 GHz frequency separately for B31.
    pub(super) fn plan(&self) -> Vec<Vec<u32>> {
        let low: Vec<_> = self.0.iter().filter(|&&(f, _)| f < 3000).map(|&(f, _)| f).collect();
        let mut plan = Vec::new();
        if !low.is_empty() { plan.push(low); }
        plan.extend(self.0.iter().filter(|&&(f, _)| f >= 5000).map(|&(f, _)| vec![f]));
        plan
    }
}

/// The supplicant as the scan sequence sees it, so tests can stand in for it.
pub(super) trait Supplicant {
    /// Sends one command and returns its reply.
    fn request(&mut self, command: &str) -> Result<String, String>;
    /// Waits for the next scan to end: `true` with results, `false` failed.
    fn scan_done(&mut self, deadline: Instant) -> Result<bool, String>;
}

/// Runs `plan` and returns the supplicant's whole BSS table with the time the
/// scan took. Entries cached before it are flushed first.
pub(super) fn run(s: &mut impl Supplicant, plan: &[Vec<u32>]) -> Result<(String, Duration), String> {
    let started = Instant::now();
    if s.request("BSS_FLUSH 0")? != "OK" {
        return Err("Wi-Fi scan was refused".into());
    }
    let mut finished = 0;
    for freqs in plan {
        let list = freqs.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        // 13 channels at 2.4 GHz took 1.9 s with the relay connected.
        let wait = Duration::from_millis(2000 + 400 * freqs.len() as u64);
        for _ in 0..2 {
            match s.request(&format!("SCAN freq={list}"))?.as_str() {
                "OK" => {
                    finished += usize::from(s.scan_done(Instant::now() + wait)?);
                    break;
                }
                // Another scan is running: let it end, then ask once more.
                "FAIL-BUSY" => {
                    let _ = s.scan_done(Instant::now() + wait);
                }
                _ => break,
            }
        }
    }
    if finished == 0 {
        return Err("Wi-Fi scan was refused".into());
    }
    Ok((table(s)?, started.elapsed()))
}

/// Replies hold at most 4 KB, so the table is read in pages by entry id
/// until the supplicant marks its last entry with `####`.
fn table(s: &mut impl Supplicant) -> Result<String, String> {
    let mut out = String::new();
    let mut next = 0u32;
    for _ in 0..64 {
        let page = s.request(&format!("BSS RANGE={next}- MASK=0x{BSS_MASK:x}"))?;
        if page.is_empty() || page == "FAIL" {
            break;
        }
        out.push_str(&page);
        out.push('\n');
        if page.ends_with("####") {
            break;
        }
        match page.lines().filter_map(|l| l.strip_prefix("id=")?.parse::<u32>().ok()).next_back() {
            Some(id) if id >= next => next = id + 1,
            _ => break,
        }
    }
    Ok(out)
}

/// An attached control-socket client. Unlike `wpa_cli` it receives the
/// supplicant's events, so a scan waits for its completion, not a sleep.
pub(super) struct Ctrl {
    socket: UnixDatagram,
    local: PathBuf,
    events: VecDeque<String>,
    buf: Vec<u8>,
}

impl Ctrl {
    /// `server` is the supplicant's socket; the client socket lives in `dir`.
    pub(super) fn attach(server: &Path, dir: &Path) -> Result<Self, String> {
        crate::httpd::gate::debug_assert_may_block();
        let mut nonce = [0u8; 8];
        getrandom::getrandom(&mut nonce).map_err(|_| "Cannot open Wi-Fi control")?;
        let local = dir.join(format!("scan-{}", nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()));
        let socket = UnixDatagram::bind(&local).map_err(|_| "Cannot open Wi-Fi control")?;
        let mut ctrl = Self { socket, local, events: VecDeque::new(), buf: vec![0; 16 * 1024] };
        ctrl.socket.connect(server).map_err(|_| "Wi-Fi control is unavailable")?;
        if ctrl.request("ATTACH")? != "OK" {
            return Err("Wi-Fi control refused event delivery".into());
        }
        Ok(ctrl)
    }

    fn recv(&mut self, deadline: Instant) -> Result<Option<String>, String> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        self.socket.set_read_timeout(Some(left)).map_err(|_| "Wi-Fi control failed")?;
        match self.socket.recv(&mut self.buf) {
            Ok(n) => Ok(Some(String::from_utf8_lossy(&self.buf[..n]).into_owned())),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Ok(None),
            Err(_) => Err("Wi-Fi control failed".into()),
        }
    }
}

impl Supplicant for Ctrl {
    fn request(&mut self, command: &str) -> Result<String, String> {
        // Events from before this command cannot be its scan's completion.
        self.events.clear();
        self.socket.send(command.as_bytes()).map_err(|_| "Wi-Fi control failed")?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while let Some(message) = self.recv(deadline)? {
            if message.starts_with('<') {
                self.events.push_back(message);
            } else {
                return Ok(message.trim_end().to_owned());
            }
        }
        Err("Wi-Fi control did not answer".into())
    }

    fn scan_done(&mut self, deadline: Instant) -> Result<bool, String> {
        loop {
            let message = match self.events.pop_front() {
                Some(message) => message,
                None => self.recv(deadline)?.ok_or("Wi-Fi scan did not finish")?,
            };
            let event = message.split_once('>').map_or("", |(_, event)| event);
            if event.starts_with("CTRL-EVENT-SCAN-RESULTS") {
                return Ok(true);
            }
            if event.starts_with("CTRL-EVENT-SCAN-FAILED") {
                return Ok(false);
            }
        }
    }
}

impl Drop for Ctrl {
    fn drop(&mut self) {
        let _ = self.socket.send(b"DETACH");
        let _ = std::fs::remove_file(&self.local);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `iw phy phy0 channels` from the B31 (CN), shortened per channel.
    const CHANNELS: &str = "Band 1:\n\t* 2412 MHz [1] \n\t  Maximum TX power: 20.0 dBm\n\t  Channel widths: 20MHz HT40+\n\t* 2437 MHz [6] \n\t* 2472 MHz [13] \n\t* 2484 MHz [14] (disabled)\nBand 2:\n\t* 5180 MHz [36] \n\t  Channel widths: 20MHz HT40+ VHT80 VHT160\n\t* 5220 MHz [44] \n\t* 5260 MHz [52] \n\t  Radar detection\n\t* 5500 MHz [100] (disabled)\n\t* 5745 MHz [149] \n\t* 5825 MHz [165] \n\t* 5845 MHz [169] (disabled)\nBand 4:\n\t* 5955 MHz [1] \n";

    #[test]
    fn plan_scans_each_5ghz_channel_alone_and_2ghz_together() {
        assert_eq!(
            Channels::parse(CHANNELS).plan(),
            vec![vec![2412, 2437, 2472], vec![5180], vec![5220], vec![5260], vec![5745], vec![5825]]
        );
        assert!(Channels::parse("Band 1:\n\t* 2484 MHz [14] (disabled)\n").plan().is_empty());
    }

    /// Behaves like the B31 driver while the 5 GHz hotspot runs at 160 MHz:
    /// multi-channel requests lose their 5 GHz channels, and an emptied list
    /// still "finishes" with whatever the kernel had cached.
    struct TrimmingDriver {
        air: Vec<(u32, &'static str)>,
        cache: Vec<(u32, &'static str, u32)>,
        table: Vec<(u32, &'static str, u32)>,
        clock: u32,
        pending: VecDeque<bool>,
        busy_once: bool,
        requests: Vec<String>,
    }

    impl TrimmingDriver {
        fn new() -> Self {
            Self {
                air: vec![(2412, "Upstream"), (5220, "Upstream_5G"), (5745, "Far")],
                // Seen 20 s ago, before this scan.
                cache: vec![(5180, "Stale", 0)],
                table: Vec::new(),
                clock: 20,
                pending: VecDeque::new(),
                busy_once: false,
                requests: Vec::new(),
            }
        }
    }

    impl Supplicant for TrimmingDriver {
        fn request(&mut self, command: &str) -> Result<String, String> {
            self.requests.push(command.to_owned());
            if command == "BSS_FLUSH 0" {
                self.table.clear();
                return Ok("OK".into());
            }
            if let Some(list) = command.strip_prefix("SCAN freq=") {
                if std::mem::take(&mut self.busy_once) {
                    self.pending.push_back(true);
                    return Ok("FAIL-BUSY".into());
                }
                let mut freqs: Vec<u32> = list.split(',').map(|f| f.parse().unwrap()).collect();
                if freqs.len() > 1 {
                    freqs.retain(|f| *f < 3000);
                }
                for (freq, ssid) in self.air.iter().filter(|(f, _)| freqs.contains(f)) {
                    self.cache.retain(|(f, s, _)| (f, s) != (freq, ssid));
                    self.cache.push((*freq, ssid, self.clock));
                }
                self.table = self.cache.clone();
                self.pending.push_back(true);
                return Ok("OK".into());
            }
            let first: u32 = command.strip_prefix("BSS RANGE=").unwrap().split('-').next().unwrap().parse().unwrap();
            // Two entries per reply, like a full 4 KB buffer.
            let page: Vec<String> = self.table.iter().enumerate().skip(first as usize).take(2).map(|(id, (freq, ssid, seen))| {
                format!("id={id}\nbssid=02:00:00:00:00:{id:02x}\nfreq={freq}\nlevel=-50\nage={}\nflags=[ESS]\nssid={ssid}\n====", self.clock - seen)
            }).collect();
            let mut page = page.join("\n");
            if first as usize + 2 >= self.table.len() && !page.is_empty() {
                page.truncate(page.len() - 4);
                page.push_str("####");
            }
            Ok(page)
        }

        fn scan_done(&mut self, _deadline: Instant) -> Result<bool, String> {
            self.pending.pop_front().ok_or_else(|| "Wi-Fi scan did not finish".into())
        }
    }

    #[test]
    fn trimmed_5ghz_upstreams_are_found_fresh_and_cached_ones_dropped() {
        let mut driver = TrimmingDriver::new();
        driver.busy_once = true;
        let (table, took) = run(&mut driver, &Channels::parse(CHANNELS).plan()).unwrap();
        let networks = crate::wifi_relay::parse_bss(&table, took.as_secs() + 1);
        let mut found: Vec<_> = networks.iter().map(|n| (n.frequency, n.ssid.as_str())).collect();
        found.sort();
        assert_eq!(found, [(2412, "Upstream"), (5220, "Upstream_5G"), (5745, "Far")]);
        assert!(driver.requests.iter().filter(|r| r.starts_with("SCAN ")).all(|r| r.contains("=24") || !r.contains(',')), "no 5 GHz channel may share a request");
        assert_eq!(driver.requests.iter().filter(|r| r.starts_with("SCAN freq=2412,")).count(), 2, "a busy supplicant is asked again");

        // The old single full-band request: the 5 GHz upstream never appears.
        let mut driver = TrimmingDriver::new();
        let all = Channels::parse(CHANNELS).plan().concat();
        let (table, took) = run(&mut driver, &[all]).unwrap();
        let networks = crate::wifi_relay::parse_bss(&table, took.as_secs() + 1);
        assert!(networks.iter().all(|n| n.frequency < 3000), "{:?}", networks.iter().map(|n| n.frequency).collect::<Vec<_>>());
    }

    #[test]
    fn unanswered_or_refused_scans_are_errors() {
        struct Refusing;
        impl Supplicant for Refusing {
            fn request(&mut self, command: &str) -> Result<String, String> {
                Ok(if command.starts_with("SCAN") { "FAIL" } else { "OK" }.into())
            }
            fn scan_done(&mut self, _: Instant) -> Result<bool, String> {
                Err("Wi-Fi scan did not finish".into())
            }
        }
        assert_eq!(run(&mut Refusing, &Channels::parse(CHANNELS).plan()).unwrap_err(), "Wi-Fi scan was refused");
    }
}
