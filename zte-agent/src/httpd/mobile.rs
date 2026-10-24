//! The agent refuses connections from the mobile-data side itself (owner
//! decision 53), whatever the router's firewall says: the web UI can turn
//! the firewall off or set it to Low, and a port forward or DMZ can hand an
//! internet client to the router's LAN address.
//!
//! A connection is served when its peer is
//! - loopback (every Tailscale client: tailscaled runs with userspace
//!   networking, so they all arrive as 127.0.0.1),
//! - one of the router's own addresses (a process on the router),
//! - an IPv6 link-local address whose scope (the interface it came in on)
//!   is not on the mobile side,
//! - or in a subnet of an interface that is not on the mobile side (`br-lan`
//!   with the Wi-Fi networks, the USB network, any other local interface),
//!   when no mobile-side subnet holds it as specifically or more: the
//!   kernel answers through the interface with the longest matching prefix,
//!   so a 10.0.0.0/8 LAN does not let in the carrier's 10.47.3.0/30, and a
//!   tie is refused.
//!
//! The mobile side is every interface named `rmnet*`: Qualcomm's cellular
//! data interfaces (`rmnet_data0` carries the data call, `rmnet_ipa0` the
//! hardware path; probe evidence of 2026-09-11 and 2026-09-14). The PEER is
//! checked, not the address it reached: a port forward or DMZ to the router
//! reaches its LAN address from the internet. TCP needs the replies to reach
//! the peer, and they leave through the interface whose subnet holds it
//! most specifically, so a client on the mobile side cannot complete a
//! connection as a LAN one (policy routes the table does not show aside).
//!
//! The interface table comes from `getifaddrs`: two netlink dumps, which on
//! the router's kernel wait for its rtnl lock (held for a while by, say, a
//! Wi-Fi restart), so they run on the blocking pool: open connections keep
//! being served meanwhile, new ones wait for the answer. It is read at an
//! accept that needs it (never for loopback): when 5 s old, and when a
//! connection would be refused, unless such a read ran within the second (a
//! new address is picked up at once; a flood from the mobile side costs a
//! read a second). No timer, no thread of its own. A refused connection is
//! closed without a reply. The first refusal is logged, then at most one
//! line a minute (with the count), with the peer, the address it reached
//! and the interface; never anything it sent.
//!
//! A failed read keeps the last table for 10 minutes (a peer it refuses
//! cannot turn the check off by making a read fail) and is tried again at
//! most once a second. With no table that recent every connection is served
//! (fail open: the firewall stays the first line, and the owner is never
//! locked out over Wi-Fi, USB or Tailscale by a lookup error); that is
//! logged once and `mobile_served` says so (the firewall questions).
//! `ZTE_AGENT_ALLOW_MOBILE=1` turns the check off.

use std::ffi::OsStr;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::boottime::{BootClock, BootInstant, Clock};

/// The opt-out: `1` serves the mobile side too.
pub const ENV: &str = "ZTE_AGENT_ALLOW_MOBILE";

/// Interface names on the mobile-data side.
const MOBILE: [&str; 1] = ["rmnet"];

/// The table is read again at the first connection that needs it after this
/// long (so a subnet that went away is served at most this long).
const MAX_AGE: Duration = Duration::from_secs(5);
/// A connection that would be refused reads the table again first, unless
/// one did within this long; a failed read is tried again after this long.
const RETRY: Duration = Duration::from_secs(1);
/// A failed read keeps the last table this long after it was read.
const KEEP: Duration = Duration::from_secs(600);
/// At most one refusal line this often (the first at once).
const LOG_EVERY: Duration = Duration::from_secs(60);
/// The accept loop waits this long for a read (the kernel's lock can be held
/// for seconds while Wi-Fi is reconfigured); a slower one finishes in the
/// background and the last table decides meanwhile.
const READ_WAIT: Duration = Duration::from_millis(500);

/// Set once at startup by `Guard::from_env`.
static ALLOW_MOBILE: AtomicBool = AtomicBool::new(false);
/// Per router listener (:9090, then HTTPS): its guard serves everything
/// because its lookup fails. Each listener writes only its own flag.
static UNGUARDED: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

/// Whether the agent serves the mobile side now: `ZTE_AGENT_ALLOW_MOBILE=1`,
/// or a listener's interface lookup fails with no recent table (the
/// firewall questions then say this web UI may become reachable).
pub fn mobile_served() -> bool {
    ALLOW_MOBILE.load(Ordering::Relaxed) || UNGUARDED.iter().any(|f| f.load(Ordering::Relaxed))
}

/// The opt-out from the variable: exactly `1` (spaces around it ignored)
/// turns the check off; any other value is ignored. With the startup line.
fn opt_out(value: Option<&OsStr>) -> (bool, Option<String>) {
    match value.map(|v| v.to_str().map(str::trim)) {
        None => (false, None),
        Some(Some("1")) => (true, Some(format!("[http] {ENV}=1: connections from the mobile side (rmnet*) are served; the router's firewall is the only guard"))),
        Some(_) => (false, Some(format!("[http] {ENV} is set but not to 1: ignored, connections from the mobile side are refused"))),
    }
}

/// One address of a network interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfAddr {
    pub name: String,
    pub addr: IpAddr,
    /// The netmask's length; 0 (no netmask) counts as the address alone.
    pub prefix: u8,
    /// For an IPv6 link-local address, its scope id (the interface's
    /// index); else 0.
    pub scope: u32,
}

/// The table's source: `getifaddrs` on the router, a fixed table in tests.
/// It runs on the blocking pool.
pub type Lookup = Arc<dyn Fn() -> std::io::Result<Vec<IfAddr>> + Send + Sync>;

/// Every address of every interface that is up (`getifaddrs`; blocking).
pub fn interfaces() -> std::io::Result<Vec<IfAddr>> {
    sys::interfaces()
}

/// An interface on the mobile-data side.
pub fn mobile_side(name: &str) -> bool {
    MOBILE.iter().any(|p| name.starts_with(p))
}

fn link_local(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V6(a) if a.is_unicast_link_local())
}

/// The length of `i`'s subnet (no netmask: the address alone).
fn bits(i: &IfAddr) -> u8 {
    let max = if i.addr.is_ipv4() { 32 } else { 128 };
    if i.prefix == 0 {
        max
    } else {
        i.prefix.min(max)
    }
}

/// `ip` is in the subnet of `i`.
fn contains(i: &IfAddr, ip: IpAddr) -> bool {
    let len = u32::from(bits(i));
    match (i.addr, ip) {
        (IpAddr::V4(n), IpAddr::V4(a)) => {
            let keep = u32::MAX.checked_shl(32 - len).unwrap_or(0);
            u32::from(n) & keep == u32::from(a) & keep
        }
        (IpAddr::V6(n), IpAddr::V6(a)) => {
            let keep = u128::MAX.checked_shl(128 - len).unwrap_or(0);
            u128::from(n) & keep == u128::from(a) & keep
        }
        _ => false,
    }
}

/// The interface whose subnet holds `ip` most specifically (the one the
/// kernel answers it through); on a tie, a mobile-side one.
fn subnet_of(table: &[IfAddr], ip: IpAddr) -> Option<&IfAddr> {
    table.iter().filter(|i| contains(i, ip)).max_by_key(|i| (bits(i), mobile_side(&i.name)))
}

/// Whether a connection from `peer` is served under `table` (see the
/// module's list). IPv4-mapped IPv6 peers (a `[::]` listener) count as
/// their IPv4 address.
pub fn admits(table: &[IfAddr], peer: SocketAddr) -> bool {
    let ip = peer.ip().to_canonical();
    if link_local(ip) {
        return arrived_on(table, peer).is_some_and(|i| !mobile_side(&i.name));
    }
    ip.is_loopback() || table.iter().any(|i| i.addr == ip) || subnet_of(table, ip).is_some_and(|i| !mobile_side(&i.name))
}

/// The interface an IPv6 link-local peer came in on: its scope id (every
/// interface has fe80::/64, so the subnet says nothing).
fn arrived_on(table: &[IfAddr], peer: SocketAddr) -> Option<&IfAddr> {
    match peer {
        SocketAddr::V6(s) if s.scope_id() != 0 => table.iter().find(|i| i.scope == s.scope_id()),
        _ => None,
    }
}

/// The accept loop's check (one per listener, owned by its loop: no lock).
pub struct Guard {
    /// `ZTE_AGENT_ALLOW_MOBILE=1`.
    off: bool,
    lookup: Lookup,
    clock: Arc<dyn Clock>,
    /// A router listener's guard: its flag in `mobile_served` (the set and
    /// this listener's place in it), true while it serves everything
    /// (cleared when the guard goes, at a stop).
    flag: Option<(&'static [AtomicBool; 2], usize)>,
    /// The last table read; kept through failed reads for `KEEP`. `None`
    /// before the first and after that (every connection is then served).
    table: Option<Vec<IfAddr>>,
    /// When `table` was read.
    good_at: Option<BootInstant>,
    /// The last read, failed or not.
    read_at: Option<BootInstant>,
    /// The last read a would-be refusal asked for.
    miss_at: Option<BootInstant>,
    /// The last read failed (said once per streak).
    failing: bool,
    /// Refusals since the last line.
    unlogged: u64,
    logged_at: Option<BootInstant>,
    /// A read that outlasted `READ_WAIT`; taken once it has finished.
    pending: Option<tokio::task::JoinHandle<std::io::Result<Vec<IfAddr>>>>,
}

/// A listener that stops (HTTPS reset) no longer serves anything.
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some((f, i)) = self.flag {
            f[i].store(false, Ordering::Relaxed);
        }
    }
}

impl Guard {
    /// The router's: `getifaddrs`, the boot clock, and the opt-out from
    /// `ZTE_AGENT_ALLOW_MOBILE` (logged when set, also when ignored).
    pub fn from_env() -> Guard {
        let (off, line) = opt_out(std::env::var_os(ENV).as_deref());
        ALLOW_MOBILE.store(off, Ordering::Relaxed);
        if let Some(line) = line {
            eprintln!("{line}");
        }
        let mut g = Guard::new(off, Arc::new(sys::interfaces), Arc::new(BootClock));
        g.flag = Some((&UNGUARDED, 0));
        g
    }

    /// Another listener's check with the same settings (its own table and,
    /// for the router's, its own flag in `mobile_served`).
    pub fn twin(&self) -> Guard {
        let mut g = Guard::new(self.off, Arc::clone(&self.lookup), Arc::clone(&self.clock));
        g.flag = self.flag.map(|(f, _)| (f, 1));
        g
    }

    pub fn new(off: bool, lookup: Lookup, clock: Arc<dyn Clock>) -> Guard {
        Guard {
            off,
            lookup,
            clock,
            flag: None,
            table: None,
            good_at: None,
            read_at: None,
            miss_at: None,
            failing: false,
            unlogged: 0,
            logged_at: None,
            pending: None,
        }
    }

    /// Whether to serve a connection from `peer`. `local` (the address it
    /// reached) is asked only for a refusal's log line. A read of the table
    /// runs on the blocking pool (the runtime keeps serving meanwhile) and is
    /// awaited for at most `READ_WAIT`; while a slower one is still running,
    /// no other starts and the last table decides.
    pub async fn admit(&mut self, peer: SocketAddr, local: impl FnOnce() -> Option<SocketAddr>) -> bool {
        if self.off {
            return true;
        }
        let now = self.clock.now();
        let mut got = None;
        if self.pending.as_ref().is_some_and(|h| h.is_finished()) {
            if let Some(h) = self.pending.take() {
                got = Some(joined(h.await));
            }
        } else if self.pending.is_none() && self.wants_read(peer, now) {
            let lookup = Arc::clone(&self.lookup);
            let mut read = tokio::task::spawn_blocking(move || lookup());
            match tokio::time::timeout(READ_WAIT, &mut read).await {
                Ok(done) => got = Some(joined(done)),
                Err(_) => self.pending = Some(read),
            }
        }
        let (ok, lines) = self.decide(now, peer, local, got);
        for line in lines {
            eprintln!("{line}");
        }
        ok
    }

    /// `admit` on this thread, with its log lines returned.
    #[cfg(test)]
    pub(crate) fn check(&mut self, peer: SocketAddr, local: impl FnOnce() -> Option<SocketAddr>) -> (bool, Vec<String>) {
        if self.off {
            return (true, Vec::new());
        }
        let now = self.clock.now();
        let got = self.wants_read(peer, now).then(|| (self.lookup)());
        self.decide(now, peer, local, got)
    }

    /// Whether `peer` needs the table read first: never read, 5 s old, a
    /// failed read a second ago, or it would be refused (unless such a read
    /// ran within the second). Loopback never does.
    fn wants_read(&mut self, peer: SocketAddr, now: BootInstant) -> bool {
        if peer.ip().to_canonical().is_loopback() {
            return false;
        }
        let Some(read) = self.read_at else { return true };
        let age = now.saturating_duration_since(read);
        if age >= MAX_AGE || (self.failing && age >= RETRY) {
            return true;
        }
        let miss = !self.serves(peer) && self.miss_at.is_none_or(|t| now.saturating_duration_since(t) >= RETRY);
        if miss {
            self.miss_at = Some(now);
        }
        miss
    }

    fn serves(&self, peer: SocketAddr) -> bool {
        self.table.as_deref().is_none_or(|t| admits(t, peer))
    }

    /// Take a read's result (if one ran), then decide.
    fn decide(
        &mut self,
        now: BootInstant,
        peer: SocketAddr,
        local: impl FnOnce() -> Option<SocketAddr>,
        got: Option<std::io::Result<Vec<IfAddr>>>,
    ) -> (bool, Vec<String>) {
        let mut lines = Vec::new();
        if let Some(got) = got {
            self.store(now, got, &mut lines);
        }
        let ok = self.serves(peer);
        if !ok {
            self.refused(now, peer, local, &mut lines);
        }
        (ok, lines)
    }

    fn store(&mut self, now: BootInstant, got: std::io::Result<Vec<IfAddr>>, lines: &mut Vec<String>) {
        self.read_at = Some(now);
        match got {
            Ok(table) => {
                if self.failing {
                    lines.push("[http] interface lookup works again: connections from the mobile side are refused".to_owned());
                    self.failing = false;
                }
                self.table = Some(table);
                self.good_at = Some(now);
            }
            Err(e) => {
                let kept = self.table.is_some() && self.good_at.is_some_and(|t| now.saturating_duration_since(t) < KEEP);
                if kept {
                    if !self.failing {
                        lines.push(format!("[http] interface lookup failed ({e}): the last table is used for up to 10 minutes"));
                    }
                } else if self.table.take().is_some() || !self.failing {
                    lines.push(format!("[http] interface lookup failed ({e}): every connection is served until it works; the router's firewall is the only guard"));
                }
                self.failing = true;
            }
        }
        if let Some((f, i)) = self.flag {
            f[i].store(self.open(), Ordering::Relaxed);
        }
    }

    /// Every connection is served because the lookup fails.
    fn open(&self) -> bool {
        self.failing && self.table.is_none()
    }

    /// Count a refusal; a line for the first, then at most one a minute.
    fn refused(&mut self, now: BootInstant, peer: SocketAddr, local: impl FnOnce() -> Option<SocketAddr>, lines: &mut Vec<String>) {
        self.unlogged += 1;
        if self.logged_at.is_some_and(|t| now.saturating_duration_since(t) < LOG_EVERY) {
            return;
        }
        let ip = peer.ip().to_canonical();
        let table = self.table.as_deref().unwrap_or_default();
        let local = local().map(|l| SocketAddr::new(l.ip().to_canonical(), l.port()));
        // The interface it came in on (link-local) or whose subnet holds
        // it; a client in no local subnet (a port forward's or DMZ's) is
        // said so, with the interface of the address it reached.
        let came = if link_local(ip) { arrived_on(table, peer) } else { subnet_of(table, ip) };
        let via = match came {
            Some(i) => format!("interface {}", i.name),
            None => match local.and_then(|l| table.iter().find(|i| i.addr == l.ip())) {
                Some(i) => format!("no local subnet, reached via {}", i.name),
                None => "no local subnet".to_owned(),
            },
        };
        let to = local.map_or_else(|| "?".to_owned(), |l| l.to_string());
        let count = if self.unlogged > 1 { format!("; {} refused since the last line", self.unlogged) } else { String::new() };
        lines.push(format!("[http] refused a connection from the mobile side: {} to {to} ({via}){count}; {ENV}=1 serves them", SocketAddr::new(ip, peer.port())));
        self.unlogged = 0;
        self.logged_at = Some(now);
    }
}

/// A finished read's result (a read that panicked counts as failed).
fn joined(done: Result<std::io::Result<Vec<IfAddr>>, tokio::task::JoinError>) -> std::io::Result<Vec<IfAddr>> {
    done.unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
}

/// The router's interface table.
#[cfg(target_os = "linux")]
mod sys {
    use std::ffi::CStr;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::IfAddr;

    /// Every address of every interface that is up, with its prefix.
    pub fn interfaces() -> std::io::Result<Vec<IfAddr>> {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: `head` is a valid out-pointer; the list is freed below.
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut out = Vec::new();
        let mut cur = head;
        while !cur.is_null() {
            // SAFETY: `cur` is a node of the list `getifaddrs` returned, not yet freed.
            let ifa = unsafe { &*cur };
            cur = ifa.ifa_next;
            if ifa.ifa_name.is_null() || ifa.ifa_flags & libc::IFF_UP as libc::c_uint == 0 {
                continue;
            }
            // SAFETY: null, or a sockaddr of its family's size (Linux fills them so).
            let Some((addr, scope)) = (unsafe { ip_of(ifa.ifa_addr) }) else { continue };
            // SAFETY: as above; a netmask of another family is ignored.
            let prefix = match unsafe { ip_of(ifa.ifa_netmask) } {
                Some((IpAddr::V4(m), _)) if addr.is_ipv4() => u32::from(m).leading_ones() as u8,
                Some((IpAddr::V6(m), _)) if addr.is_ipv6() => u128::from(m).leading_ones() as u8,
                _ => 0,
            };
            // SAFETY: a NUL-terminated name owned by the list.
            let name = unsafe { CStr::from_ptr(ifa.ifa_name) }.to_string_lossy().into_owned();
            let scope = if super::link_local(addr) { scope } else { 0 };
            out.push(IfAddr { name, addr, prefix, scope });
        }
        // SAFETY: `head` came from `getifaddrs` and is freed once.
        unsafe { libc::freeifaddrs(head) };
        Ok(out)
    }

    /// The address (and IPv6 scope id) of an `AF_INET` / `AF_INET6` sockaddr.
    ///
    /// # Safety
    /// `sa` is null or points to a sockaddr of its family's full size.
    unsafe fn ip_of(sa: *const libc::sockaddr) -> Option<(IpAddr, u32)> {
        if sa.is_null() {
            return None;
        }
        match i32::from((*sa).sa_family) {
            libc::AF_INET => {
                let s = &*sa.cast::<libc::sockaddr_in>();
                Some((IpAddr::V4(Ipv4Addr::from(s.sin_addr.s_addr.to_ne_bytes())), 0))
            }
            libc::AF_INET6 => {
                let s = &*sa.cast::<libc::sockaddr_in6>();
                Some((IpAddr::V6(Ipv6Addr::from(s.sin6_addr.s6_addr)), s.sin6_scope_id))
            }
            _ => None,
        }
    }
}

/// Elsewhere (host builds): no lookup, so every connection is served.
#[cfg(not(target_os = "linux"))]
mod sys {
    pub fn interfaces() -> std::io::Result<Vec<super::IfAddr>> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "only on Linux"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boottime::FakeClock;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV6};
    use std::sync::Mutex;

    fn ifa(name: &str, addr: &str, prefix: u8, scope: u32) -> IfAddr {
        IfAddr { name: name.into(), addr: addr.parse().unwrap(), prefix, scope }
    }

    /// A made-up router: the LAN (Wi-Fi) bridge, a USB network, the data
    /// call (TEST-NET-2 and the IPv6 documentation prefix), loopback.
    fn router() -> Vec<IfAddr> {
        vec![
            ifa("lo", "127.0.0.1", 8, 0),
            ifa("lo", "::1", 128, 0),
            ifa("br-lan", "192.168.0.1", 24, 0),
            ifa("br-lan", "fd00:db8:1::1", 64, 0),
            ifa("br-lan", "fe80::1", 64, 3),
            ifa("usb0", "192.168.42.1", 24, 0),
            ifa("rmnet_data0", "198.51.100.10", 24, 0),
            ifa("rmnet_data0", "2001:db8:5::2", 64, 0),
            ifa("rmnet_data0", "fe80::2", 64, 7),
        ]
    }

    fn v4(s: &str) -> SocketAddr {
        SocketAddr::new(s.parse::<Ipv4Addr>().unwrap().into(), 40000)
    }

    fn v6(s: &str, scope: u32) -> SocketAddr {
        SocketAddr::V6(SocketAddrV6::new(s.parse::<Ipv6Addr>().unwrap(), 40000, 0, scope))
    }

    #[test]
    fn local_networks_and_loopback_are_served_the_mobile_side_is_not() {
        let t = router();
        // LAN (Wi-Fi), USB, loopback (Tailscale), the router itself.
        for ok in ["192.168.0.23", "192.168.0.254", "192.168.42.129", "127.0.0.1", "127.8.9.10", "198.51.100.10"] {
            assert!(admits(&t, v4(ok)), "{ok}");
        }
        for ok in [v6("::1", 0), v6("fd00:db8:1::23", 0), v6("fe80::23", 3), v6("2001:db8:5::2", 0)] {
            assert!(admits(&t, ok), "{ok}");
        }
        // The data call's subnet, the internet (also when a port forward or
        // DMZ handed it to the LAN address: the peer decides), another LAN.
        for no in ["198.51.100.1", "203.0.113.7", "192.168.1.5", "10.0.0.5", "0.0.0.0"] {
            assert!(!admits(&t, v4(no)), "{no}");
        }
        // Global IPv6 from the mobile side, link-local from the mobile side
        // or without a scope.
        for no in [v6("2001:db8:5::99", 0), v6("2001:db8:77::1", 0), v6("fe80::23", 7), v6("fe80::23", 0), v6("fe80::1", 7)] {
            assert!(!admits(&t, no), "{no}");
        }
        // No interface but loopback (no LAN yet): only loopback.
        assert!(admits(&t[..2], v4("127.0.0.1")) && !admits(&t[..2], v4("192.168.0.23")));
    }

    #[test]
    fn ipv4_mapped_peers_count_as_ipv4() {
        let t = router();
        assert!(admits(&t, v6("::ffff:192.168.0.23", 0)));
        assert!(admits(&t, v6("::ffff:127.0.0.1", 0)));
        assert!(!admits(&t, v6("::ffff:203.0.113.7", 0)));
        assert!(!admits(&t, v6("::ffff:198.51.100.1", 0)));
    }

    /// A LAN subnet over the carrier's: the most specific subnet decides
    /// (the kernel answers through it); a tie refuses.
    #[test]
    fn the_most_specific_subnet_decides() {
        // A 10.0.0.0/8 LAN; the data call's carrier-grade NAT address in it.
        let t = vec![ifa("lo", "127.0.0.1", 8, 0), ifa("br-lan", "10.0.0.1", 8, 0), ifa("rmnet_data0", "10.47.3.2", 30, 0)];
        for no in ["10.47.3.1", "10.47.3.0", "10.47.3.3"] {
            assert!(!admits(&t, v4(no)), "{no}");
        }
        for ok in ["10.0.0.23", "10.47.3.4", "10.200.1.1", "10.47.3.2", "127.0.0.1"] {
            assert!(admits(&t, v4(ok)), "{ok}");
        }
        assert!(!admits(&t, v6("::ffff:10.47.3.1", 0)));
        // The same length on both sides: refused.
        let tie = vec![ifa("br-lan", "10.0.0.1", 8, 0), ifa("rmnet_data0", "10.47.3.2", 8, 0)];
        assert!(!admits(&tie, v4("10.99.1.1")));
        // A LAN inside the carrier's subnet: its own subnet is served.
        let inner = vec![ifa("rmnet_data0", "10.47.3.2", 8, 0), ifa("usb0", "10.20.0.1", 16, 0)];
        assert!(admits(&inner, v4("10.20.5.5")) && !admits(&inner, v4("10.21.0.1")));
        // IPv6: the carrier's /64 on the LAN too (a tie), or a /56 LAN over it.
        let v6t = vec![ifa("br-lan", "2001:db8:5::1", 64, 0), ifa("rmnet_data0", "2001:db8:5::2", 64, 0)];
        assert!(!admits(&v6t, v6("2001:db8:5::99", 0)));
        let wide = vec![ifa("br-lan", "2001:db8::1", 32, 0), ifa("rmnet_data0", "2001:db8:5::2", 64, 0)];
        assert!(!admits(&wide, v6("2001:db8:5::99", 0)) && admits(&wide, v6("2001:db8:6::99", 0)));
    }

    #[test]
    fn prefixes() {
        let host = ifa("tun9", "192.0.2.103", 32, 0);
        assert!(contains(&host, "192.0.2.103".parse().unwrap()) && !contains(&host, "192.0.2.104".parse().unwrap()));
        // No netmask: the address alone, never "everything".
        let none = ifa("x0", "192.168.9.1", 0, 0);
        assert!(!contains(&none, "8.8.8.8".parse().unwrap()) && contains(&none, "192.168.9.1".parse().unwrap()));
        assert_eq!((bits(&none), bits(&ifa("x0", "fd00::1", 0, 0)), bits(&ifa("x0", "10.0.0.1", 40, 0))), (32, 128, 32));
        assert!(contains(&ifa("x1", "10.0.0.1", 8, 0), "10.200.3.4".parse().unwrap()));
        assert!(!contains(&ifa("x2", "fd00::1", 64, 0), "192.168.0.1".parse().unwrap()));
        assert!(mobile_side("rmnet_data0") && mobile_side("rmnet_ipa0") && !mobile_side("br-lan") && !mobile_side("usb0") && !mobile_side("wlan0"));
    }

    /// Exactly `1` turns the check off; anything else is ignored, and said.
    #[test]
    fn the_opt_out_takes_only_1() {
        let on = |v: &str| opt_out(Some(OsStr::new(v)));
        assert!(on("1").0 && on(" 1\n").0);
        assert_eq!(on("1").1.unwrap(), "[http] ZTE_AGENT_ALLOW_MOBILE=1: connections from the mobile side (rmnet*) are served; the router's firewall is the only guard");
        for v in ["0", "yes", "true", ""] {
            assert_eq!(on(v), (false, Some("[http] ZTE_AGENT_ALLOW_MOBILE is set but not to 1: ignored, connections from the mobile side are refused".to_owned())), "{v}");
        }
        assert_eq!(opt_out(None), (false, None));
    }

    /// A guard over a table the test changes, counting reads.
    struct Fake {
        table: Arc<Mutex<std::io::Result<Vec<IfAddr>>>>,
        reads: Arc<Mutex<usize>>,
        clock: Arc<FakeClock>,
    }

    impl Fake {
        fn set(&self, table: std::io::Result<Vec<IfAddr>>) {
            *self.table.lock().unwrap() = table;
        }

        fn reads(&self) -> usize {
            *self.reads.lock().unwrap()
        }
    }

    fn fake(off: bool) -> (Guard, Fake) {
        let table: Arc<Mutex<std::io::Result<Vec<IfAddr>>>> = Arc::new(Mutex::new(Ok(router())));
        let reads = Arc::new(Mutex::new(0));
        let clock = FakeClock::at_secs(1000);
        let (t, r) = (Arc::clone(&table), Arc::clone(&reads));
        let lookup: Lookup = Arc::new(move || {
            *r.lock().unwrap() += 1;
            match &*t.lock().unwrap() {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
            }
        });
        (Guard::new(off, lookup, clock.clone()), Fake { table, reads, clock })
    }

    fn lan_ip() -> Option<SocketAddr> {
        Some("192.168.0.1:9090".parse().unwrap())
    }

    fn busy() -> std::io::Result<Vec<IfAddr>> {
        Err(std::io::Error::other("too many open files (fake)"))
    }

    #[test]
    fn refusals_are_logged_once_then_once_a_minute() {
        let (mut g, f) = fake(false);
        assert_eq!(g.check(v4("192.168.0.23"), lan_ip), (true, Vec::new()));
        // Over a port forward to the LAN address: refused, the first one logged.
        let (ok, lines) = g.check(v4("203.0.113.7"), lan_ip);
        assert!(!ok);
        assert_eq!(lines, ["[http] refused a connection from the mobile side: 203.0.113.7:40000 to 192.168.0.1:9090 (no local subnet, reached via br-lan); ZTE_AGENT_ALLOW_MOBILE=1 serves them"]);
        // Within the minute: refused, not logged.
        for _ in 0..3 {
            f.clock.advance(Duration::from_secs(5));
            assert_eq!(g.check(v4("198.51.100.1"), || Some("198.51.100.10:9090".parse().unwrap())), (false, Vec::new()));
        }
        f.clock.advance(Duration::from_secs(50));
        let (ok, lines) = g.check(v6("::ffff:198.51.100.1", 0), || Some("[::ffff:198.51.100.10]:9090".parse().unwrap()));
        assert!(!ok);
        assert_eq!(lines, ["[http] refused a connection from the mobile side: 198.51.100.1:40000 to 198.51.100.10:9090 (interface rmnet_data0); 4 refused since the last line; ZTE_AGENT_ALLOW_MOBILE=1 serves them"]);
        // Link-local: the interface of its scope.
        f.clock.advance(Duration::from_secs(61));
        let (ok, lines) = g.check(v6("fe80::23", 7), || Some("[fe80::2%7]:9090".parse().unwrap()));
        assert!(!ok);
        assert_eq!(lines, ["[http] refused a connection from the mobile side: [fe80::23]:40000 to [fe80::2]:9090 (interface rmnet_data0); ZTE_AGENT_ALLOW_MOBILE=1 serves them"]);
        // The most specific subnet names the interface.
        f.set(Ok(vec![ifa("br-lan", "10.0.0.1", 8, 0), ifa("rmnet_data0", "10.47.3.2", 30, 0)]));
        f.clock.advance(Duration::from_secs(61));
        let (ok, lines) = g.check(v4("10.47.3.1"), || Some("10.47.3.2:9090".parse().unwrap()));
        assert!(!ok);
        assert_eq!(lines, ["[http] refused a connection from the mobile side: 10.47.3.1:40000 to 10.47.3.2:9090 (interface rmnet_data0); ZTE_AGENT_ALLOW_MOBILE=1 serves them"]);
        // The address it reached is only asked for a line.
        assert!(g.check(v4("10.0.0.23"), || panic!("asked")).0);
        assert!(!g.check(v4("10.47.3.1"), || panic!("asked")).0);
    }

    #[test]
    fn the_table_is_read_again_when_old_or_on_a_miss_at_most_once_a_second() {
        let (mut g, f) = fake(false);
        let without_usb: Vec<IfAddr> = router().into_iter().filter(|i| i.name != "usb0").collect();
        f.set(Ok(without_usb.clone()));
        assert!(g.check(v4("192.168.0.23"), lan_ip).0);
        assert_eq!(f.reads(), 1);
        // A USB network that came up since: the miss reads again, served,
        // also right after a read.
        f.set(Ok(router()));
        f.clock.advance(Duration::from_millis(300));
        assert!(g.check(v4("192.168.42.129"), lan_ip).0);
        assert_eq!(f.reads(), 2);
        // Misses within a second of a miss's read: no read.
        f.clock.advance(Duration::from_millis(500));
        assert!(!g.check(v4("203.0.113.7"), lan_ip).0);
        assert_eq!(f.reads(), 2);
        f.clock.advance(Duration::from_millis(600));
        assert!(!g.check(v4("203.0.113.7"), lan_ip).0);
        assert_eq!(f.reads(), 3);
        // Served peers read only once the table is 5 s old; loopback never.
        f.clock.advance(Duration::from_secs(3));
        assert!(g.check(v4("192.168.0.23"), lan_ip).0);
        assert_eq!(f.reads(), 3);
        f.clock.advance(Duration::from_secs(2));
        assert!(g.check(v4("127.0.0.1"), lan_ip).0 && g.check(v6("::1", 0), lan_ip).0);
        assert_eq!(f.reads(), 3);
        // The USB network went away: its peers are refused once the table
        // is 5 s old.
        f.set(Ok(without_usb));
        assert!(!g.check(v4("192.168.42.129"), lan_ip).0);
        assert_eq!(f.reads(), 4);
    }

    #[test]
    fn a_failed_lookup_serves_everything_and_says_so_once() {
        let (mut g, f) = fake(false);
        f.set(Err(std::io::Error::other("netlink busy (fake)")));
        let (ok, lines) = g.check(v4("203.0.113.7"), lan_ip);
        assert!(ok && g.open());
        assert_eq!(lines, ["[http] interface lookup failed (netlink busy (fake)): every connection is served until it works; the router's firewall is the only guard"]);
        f.clock.advance(Duration::from_secs(2));
        assert_eq!(g.check(v4("198.51.100.1"), lan_ip), (true, Vec::new()));
        assert_eq!(f.reads(), 2);
        // Within a second of a failed read: not tried again.
        assert_eq!(g.check(v4("198.51.100.1"), lan_ip), (true, Vec::new()));
        assert_eq!(f.reads(), 2);
        // It works again: said, and the mobile side is refused again.
        f.set(Ok(router()));
        f.clock.advance(Duration::from_secs(2));
        let (ok, lines) = g.check(v4("203.0.113.7"), lan_ip);
        assert!(!ok && !g.open());
        assert_eq!(lines[0], "[http] interface lookup works again: connections from the mobile side are refused");
        assert_eq!(lines.len(), 2);
        assert!(!mobile_served(), "only the router's guard says so");
    }

    /// A refused peer's re-read failing (fd exhaustion, say) does not serve
    /// it: the last table holds for 10 minutes after it was read.
    #[test]
    fn a_failed_read_keeps_the_last_table_for_ten_minutes() {
        let (mut g, f) = fake(false);
        assert!(!g.check(v4("203.0.113.7"), lan_ip).0);
        f.set(busy());
        f.clock.advance(Duration::from_secs(2));
        let (ok, lines) = g.check(v4("203.0.113.7"), lan_ip);
        assert!(!ok && !g.open());
        assert_eq!(lines[0], "[http] interface lookup failed (too many open files (fake)): the last table is used for up to 10 minutes");
        assert_eq!(f.reads(), 2);
        // Tried again at most once a second; the LAN is still served.
        assert_eq!(g.check(v4("192.168.0.23"), lan_ip), (true, Vec::new()));
        assert_eq!(f.reads(), 2);
        f.clock.advance(Duration::from_secs(590));
        assert!(!g.check(v4("203.0.113.7"), lan_ip).0);
        assert!(g.check(v4("192.168.42.129"), lan_ip).0);
        assert_eq!(f.reads(), 3);
        // 10 minutes after the last good read: every connection is served, said.
        f.clock.advance(Duration::from_secs(8));
        let (ok, lines) = g.check(v4("203.0.113.7"), lan_ip);
        assert!(ok && g.open());
        assert_eq!(lines, ["[http] interface lookup failed (too many open files (fake)): every connection is served until it works; the router's firewall is the only guard"]);
        f.clock.advance(Duration::from_secs(2));
        assert_eq!(g.check(v4("203.0.113.7"), lan_ip), (true, Vec::new()));
        // Back: refused again.
        f.set(Ok(router()));
        f.clock.advance(Duration::from_secs(2));
        let (ok, lines) = g.check(v4("203.0.113.7"), lan_ip);
        assert!(!ok && !g.open());
        assert_eq!(lines, ["[http] interface lookup works again: connections from the mobile side are refused"]);
    }

    /// Each listener (:9090 and its HTTPS twin) has its own fail-open
    /// flag: one failing open is said while the other reads fine (no last
    /// writer wins), and a stopped listener's flag goes with it.
    #[test]
    fn each_listener_says_when_it_fails_open() {
        let flags: &'static [AtomicBool; 2] = Box::leak(Box::new([AtomicBool::new(false), AtomicBool::new(false)]));
        let any = || flags.iter().any(|f| f.load(Ordering::Relaxed));
        let (mut plain, f) = fake(false);
        plain.flag = Some((flags, 0));
        let mut https = plain.twin();
        assert_eq!(https.flag.map(|(_, i)| i), Some(1));
        f.set(Err(std::io::Error::other("netlink busy (fake)")));
        assert!(plain.check(v4("203.0.113.7"), lan_ip).0 && any());
        f.set(Ok(router()));
        assert!(!https.check(v4("203.0.113.7"), lan_ip).0);
        assert!(any(), "the HTTPS guard's good read hid :9090 serving everything");
        // Ten minutes on, HTTPS's lookup fails with no recent table.
        f.set(Err(std::io::Error::other("netlink busy (fake)")));
        f.clock.advance(Duration::from_secs(601));
        assert!(https.check(v4("203.0.113.7"), lan_ip).0);
        f.set(Ok(router()));
        assert!(!plain.check(v4("203.0.113.7"), lan_ip).0);
        assert!(any(), ":9090's good read hid HTTPS serving everything");
        drop(https);
        assert!(!any());
        assert!(!mobile_served(), "test flags are not the router's");
    }

    #[test]
    fn the_opt_out_serves_everything_without_a_lookup() {
        let (mut g, f) = fake(true);
        for peer in [v4("203.0.113.7"), v4("198.51.100.1"), v6("2001:db8:77::1", 0), v4("192.168.0.23")] {
            assert_eq!(g.check(peer, || panic!("asked")), (true, Vec::new()));
        }
        assert_eq!(f.reads(), 0);
        assert!(!mobile_served(), "only from_env sets it");
    }

    /// `admit` reads the table on the blocking pool, not on the runtime's
    /// thread (a wait for the kernel's lock must not stall open connections).
    #[tokio::test]
    async fn the_lookup_runs_off_the_runtime_thread() {
        let here = std::thread::current().id();
        let ran = Arc::new(Mutex::new(Vec::new()));
        let r = Arc::clone(&ran);
        let lookup: Lookup = Arc::new(move || {
            r.lock().unwrap().push(std::thread::current().id());
            Ok(router())
        });
        let mut g = Guard::new(false, lookup, FakeClock::at_secs(1000));
        assert!(g.admit(v4("127.0.0.1"), lan_ip).await);
        assert!(ran.lock().unwrap().is_empty(), "loopback reads nothing");
        assert!(g.admit(v4("192.168.0.23"), lan_ip).await);
        assert!(!g.admit(v4("203.0.113.7"), lan_ip).await);
        let ran = ran.lock().unwrap();
        assert_eq!(ran.len(), 2);
        assert!(ran.iter().all(|t| *t != here), "on the runtime thread");
    }

    /// A read held up in the kernel delays one accept by at most `READ_WAIT`;
    /// meanwhile the last table decides and no second read starts, and its
    /// result is taken once it has finished.
    #[tokio::test]
    async fn a_slow_read_is_awaited_at_most_half_a_second() {
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let table = Arc::new(Mutex::new(router().into_iter().filter(|i| i.name != "usb0").collect::<Vec<_>>()));
        let reads = Arc::new(Mutex::new(0usize));
        let (g2, t2, r2) = (Arc::clone(&gate), Arc::clone(&table), Arc::clone(&reads));
        let lookup: Lookup = Arc::new(move || {
            let n = {
                let mut r = r2.lock().unwrap();
                *r += 1;
                *r
            };
            if n > 1 {
                let (open, cv) = &*g2;
                // Bounded, so a failed assert below cannot hang the test.
                let _held = cv.wait_timeout_while(open.lock().unwrap(), Duration::from_secs(10), |o| !*o).unwrap();
            }
            Ok(t2.lock().unwrap().clone())
        });
        let clock = FakeClock::at_secs(1000);
        let mut g = Guard::new(false, lookup, Arc::clone(&clock) as Arc<dyn Clock>);
        assert!(g.admit(v4("192.168.0.23"), lan_ip).await);
        // A USB network came up; the miss's read hangs.
        *table.lock().unwrap() = router();
        clock.advance(Duration::from_secs(2));
        let started = std::time::Instant::now();
        assert!(!g.admit(v4("192.168.42.129"), lan_ip).await, "the last table decides");
        let waited = started.elapsed();
        assert!(waited >= READ_WAIT && waited < Duration::from_secs(3), "{waited:?}");
        // While it hangs: no wait, no second read, loopback and the LAN served.
        clock.advance(Duration::from_secs(10));
        let started = std::time::Instant::now();
        assert!(g.admit(v4("127.0.0.1"), lan_ip).await);
        assert!(g.admit(v4("192.168.0.23"), lan_ip).await);
        assert!(!g.admit(v4("203.0.113.7"), lan_ip).await);
        assert!(started.elapsed() < Duration::from_millis(200));
        assert_eq!(*reads.lock().unwrap(), 2);
        // It finishes: the next accept takes its table.
        {
            let (open, cv) = &*gate;
            *open.lock().unwrap() = true;
            cv.notify_all();
        }
        for _ in 0..200 {
            if g.pending.as_ref().is_some_and(|h| h.is_finished()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(g.admit(v4("192.168.42.129"), lan_ip).await, "the USB network is in the new table");
        assert!(g.pending.is_none());
        assert_eq!(*reads.lock().unwrap(), 2);
    }
}
