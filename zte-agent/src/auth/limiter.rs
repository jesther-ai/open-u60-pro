//! Login rate limit: exponential backoff per peer address plus a global
//! failure bucket. A blocked attempt is refused before the password is looked
//! at and is not recorded, so hammering cannot extend a block, and every block
//! ends on its own (no permanent lockout). Successes are never counted.
//!
//! Loopback is every Tailscale client at once (userspace networking), so its
//! blocks stay short: one phone with an old password, retrying as soon as
//! each block ends, must not lock every remote user out for minutes. The
//! global bucket still bounds guessing from there (one failure per refill).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use crate::boottime::BootInstant;

/// The socket peer address, never `X-Forwarded-For`. Tailnet peers all arrive
/// as 127.0.0.1 (userspace networking) and share one key.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PeerKey {
    V4([u8; 4]),
    /// The full /128.
    V6([u8; 16]),
}

impl PeerKey {
    /// For callers without a socket address (the in-process `route()` arm).
    pub const UNKNOWN: PeerKey = PeerKey::V4([0; 4]);

    fn ip(self) -> IpAddr {
        match self {
            PeerKey::V4(a) => IpAddr::V4(Ipv4Addr::from(a)),
            PeerKey::V6(a) => IpAddr::V6(Ipv6Addr::from(a)),
        }
    }

    /// This device: shared by every Tailscale client.
    fn shared(self) -> bool {
        self.ip().is_loopback()
    }
}

impl From<SocketAddr> for PeerKey {
    fn from(addr: SocketAddr) -> Self {
        match addr.ip() {
            IpAddr::V4(v4) => PeerKey::V4(v4.octets()),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => PeerKey::V4(v4.octets()),
                None => PeerKey::V6(v6.octets()),
            },
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct LimiterConfig {
    /// Failures a peer gets before the first block.
    pub free_failures: u32,
    /// Block after the `free_failures`-th failure; doubles per further failure.
    pub base: Duration,
    pub max_backoff: Duration,
    /// Longest block of the shared loopback key (Tailscale clients); at most
    /// `global_refill`, so it cannot use up the global bucket on its own.
    pub shared_max_backoff: Duration,
    /// A peer with no failure for this long starts over.
    pub forget_after: Duration,
    /// Peers tracked at once; a new one evicts the oldest last failure.
    pub slots: usize,
    /// Failures from all peers the global bucket absorbs before it denies everyone.
    pub global_burst: u32,
    /// One failure is given back per period.
    pub global_refill: Duration,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        LimiterConfig {
            free_failures: 5,
            base: Duration::from_secs(1),
            max_backoff: Duration::from_secs(300),
            shared_max_backoff: Duration::from_secs(10),
            forget_after: Duration::from_secs(15 * 60),
            slots: 64,
            global_burst: 30,
            global_refill: Duration::from_secs(10),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Admit {
    Allow,
    Deny { retry_after: Duration },
}

struct Slot {
    key: PeerKey,
    failures: u32,
    last_failure: BootInstant,
}

pub struct LoginLimiter {
    cfg: LimiterConfig,
    slots: Vec<Slot>,
    /// Failures the global bucket can still absorb.
    tokens: u32,
    /// Start of the current refill period (tracks `now` while the bucket is full).
    refill_from: BootInstant,
    /// The empty bucket was logged; cleared once it is full again.
    empty_logged: bool,
}

impl LoginLimiter {
    pub fn new(cfg: LimiterConfig, now: BootInstant) -> Self {
        // Otherwise `forget_stale` could drop a peer whose block is still running.
        debug_assert!(cfg.forget_after >= cfg.max_backoff, "forget_after below max_backoff");
        LoginLimiter {
            cfg,
            slots: Vec::new(),
            tokens: cfg.global_burst,
            refill_from: now,
            empty_logged: false,
        }
    }

    /// How long `key` is blocked after its n-th failure:
    /// 0 below `free_failures`, then `base · 2^(n − free_failures)`, capped
    /// (lower for the shared loopback key).
    fn block(&self, key: PeerKey, n: u32) -> Duration {
        if n < self.cfg.free_failures {
            return Duration::ZERO;
        }
        let factor = 1u32
            .checked_shl(n - self.cfg.free_failures)
            .unwrap_or(u32::MAX);
        let max = if key.shared() { self.cfg.shared_max_backoff } else { self.cfg.max_backoff };
        self.cfg.base.saturating_mul(factor).min(max)
    }

    fn refill(&mut self, now: BootInstant) {
        let burst = self.cfg.global_burst;
        let period = self.cfg.global_refill;
        if self.tokens >= burst || period.is_zero() {
            self.tokens = burst;
            self.refill_from = now;
            self.empty_logged = false;
            return;
        }
        let periods = now.saturating_duration_since(self.refill_from).as_nanos() / period.as_nanos();
        if periods >= u128::from(burst - self.tokens) {
            self.tokens = burst;
            self.refill_from = now;
            self.empty_logged = false;
        } else if periods > 0 {
            // periods < burst here, so it fits a u32
            let periods = periods as u32;
            self.tokens += periods;
            self.refill_from = self.refill_from.saturating_add(period * periods);
        }
    }

    fn forget_stale(&mut self, now: BootInstant) {
        let after = self.cfg.forget_after;
        self.slots
            .retain(|s| now.saturating_duration_since(s.last_failure) < after);
    }

    /// Called before the password is looked at.
    pub fn check(&mut self, key: PeerKey, now: BootInstant) -> Admit {
        self.forget_stale(now);
        self.refill(now);
        let mut wait = Duration::ZERO;
        if self.tokens == 0 {
            let next = self.refill_from.saturating_add(self.cfg.global_refill);
            wait = next.saturating_duration_since(now);
        }
        if let Some(s) = self.slots.iter().find(|s| s.key == key) {
            let until = s.last_failure.saturating_add(self.block(key, s.failures));
            wait = wait.max(until.saturating_duration_since(now));
        }
        if wait.is_zero() {
            Admit::Allow
        } else {
            Admit::Deny { retry_after: wait }
        }
    }

    /// Counts a wrong password. Returns a line for the log (never with the
    /// password) when the global bucket first runs empty, or when this peer's
    /// block first reaches a minute; the caller logs it outside its lock.
    pub fn record_failure(&mut self, key: PeerKey, now: BootInstant) -> Option<String> {
        self.refill(now);
        self.tokens = self.tokens.saturating_sub(1);
        self.forget_stale(now);
        let failures = match self.slots.iter_mut().find(|s| s.key == key) {
            Some(s) => {
                s.failures = s.failures.saturating_add(1);
                s.last_failure = now;
                s.failures
            }
            None => {
                self.add_slot(key, now);
                1
            }
        };
        let ip = key.ip();
        if self.tokens == 0 && !self.empty_logged {
            self.empty_logged = true;
            return Some(format!("[auth] too many wrong passwords from all clients: new logins refused for now (last from {ip})"));
        }
        let minute = Duration::from_secs(60);
        let block = self.block(key, failures);
        (block >= minute && self.block(key, failures - 1) < minute).then(|| {
            format!("[auth] logins from {ip} blocked for {} s after {failures} wrong passwords", block.as_secs())
        })
    }

    fn add_slot(&mut self, key: PeerKey, now: BootInstant) {
        if self.slots.len() >= self.cfg.slots {
            let oldest = self
                .slots
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.last_failure)
                .map(|(i, _)| i);
            if let Some(i) = oldest {
                self.slots.swap_remove(i);
            }
        }
        self.slots.push(Slot {
            key,
            failures: 1,
            last_failure: now,
        });
    }

    /// Drops the peer's slot; the global bucket is not touched.
    pub fn record_success(&mut self, key: PeerKey) {
        self.slots.retain(|s| s.key != key);
    }
}

/// Whole seconds for `Retry-After`: rounded up, at least 1.
pub fn retry_after_secs(d: Duration) -> u64 {
    (d.as_secs() + u64::from(d.subsec_nanos() > 0)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> BootInstant {
        BootInstant::from_duration(Duration::from_millis(ms))
    }

    fn peer(n: u8) -> PeerKey {
        PeerKey::from(SocketAddr::from(([10, 0, 90, n], 5000)))
    }

    fn deny_for(admit: Admit) -> Duration {
        match admit {
            Admit::Deny { retry_after } => retry_after,
            Admit::Allow => Duration::ZERO,
        }
    }

    #[test]
    fn block_table_matches_the_schedule() {
        let l = LoginLimiter::new(LimiterConfig::default(), at(0));
        let secs: Vec<u64> = (1..=16).map(|n| l.block(peer(1), n).as_secs()).collect();
        assert_eq!(secs, [0, 0, 0, 0, 1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300, 300]);
        assert_eq!(l.block(peer(1), u32::MAX), Duration::from_secs(300));
        // The shared loopback key (every Tailscale client): at most 10 s.
        for lo in ["127.0.0.1:5000", "[::1]:5000", "[::ffff:127.0.0.1]:5000"] {
            let lo = PeerKey::from(lo.parse::<SocketAddr>().unwrap());
            let secs: Vec<u64> = (1..=10).map(|n| l.block(lo, n).as_secs()).collect();
            assert_eq!(secs, [0, 0, 0, 0, 1, 2, 4, 8, 10, 10]);
            assert_eq!(l.block(lo, u32::MAX), Duration::from_secs(10));
        }
    }

    /// One remote phone with an old password, retrying the moment each block
    /// ends, for an hour: the tailnet gets a login slot every 10 s, and the
    /// global bucket (LAN logins) never runs dry.
    #[test]
    fn stale_tailnet_phone_neither_locks_out_remote_users_nor_empties_the_bucket() {
        let mut l = LoginLimiter::new(LimiterConfig::default(), at(0));
        let lo = PeerKey::from(SocketAddr::from(([127, 0, 0, 1], 40000)));
        let mut t = 0;
        let mut longest = Duration::ZERO;
        while t < 3_600_000 {
            match l.check(lo, at(t)) {
                Admit::Allow => {
                    let _ = l.record_failure(lo, at(t));
                }
                Admit::Deny { retry_after } => longest = longest.max(retry_after),
            }
            assert!(l.tokens > 0, "global bucket empty at {t} ms");
            t += 500;
        }
        assert!(longest > Duration::from_secs(9) && longest <= Duration::from_secs(10), "{longest:?}");
        assert_eq!(l.check(peer(7), at(t)), Admit::Allow);
    }

    #[test]
    fn lockouts_are_logged_once_with_the_peer() {
        let mut l = LoginLimiter::new(LimiterConfig::default(), at(0));
        let p = peer(3);
        let lines: Vec<Option<String>> = (0..12).map(|_| l.record_failure(p, at(0))).collect();
        // The 11th failure is the first 64 s block.
        assert!(lines[..10].iter().all(Option::is_none), "{lines:?}");
        assert_eq!(lines[10].as_deref(), Some("[auth] logins from 10.0.90.3 blocked for 64 s after 11 wrong passwords"));
        assert_eq!(lines[11], None);
        // The global bucket: logged when it first runs dry, not again until full.
        let mut empty = Vec::new();
        for n in 0..40u8 {
            if let Some(line) = l.record_failure(peer(100 + n), at(0)) {
                empty.push(line);
            }
        }
        assert_eq!(empty, ["[auth] too many wrong passwords from all clients: new logins refused for now (last from 10.0.90.117)"]);
        assert!(l.record_failure(peer(200), at(10_000)).is_none());
        // Full again after 5 min; the next time it runs dry is logged again.
        let later = 10_000 + 300_000;
        assert_eq!(l.check(peer(201), at(later)), Admit::Allow);
        let lines: Vec<_> = (0..30u8).filter_map(|n| l.record_failure(peer(n), at(later))).collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
    }

    #[test]
    fn forget_after_15min() {
        let mut l = LoginLimiter::new(LimiterConfig::default(), at(0));
        let p = peer(1);
        for _ in 0..4 {
            l.record_failure(p, at(0));
        }
        // 15 min without a failure: the peer starts over.
        let later = 15 * 60 * 1000;
        assert_eq!(l.check(p, at(later)), Admit::Allow);
        for _ in 0..4 {
            l.record_failure(p, at(later));
            assert_eq!(l.check(p, at(later)), Admit::Allow);
        }
        l.record_failure(p, at(later));
        assert_eq!(l.check(p, at(later)), Admit::Deny { retry_after: Duration::from_secs(1) });

        // Just under 15 min the earlier failures still count: the fifth blocks.
        let mut l = LoginLimiter::new(LimiterConfig::default(), at(0));
        for _ in 0..4 {
            l.record_failure(p, at(0));
        }
        l.record_failure(p, at(later - 1));
        assert_eq!(l.check(p, at(later - 1)), Admit::Deny { retry_after: Duration::from_secs(1) });
    }

    #[test]
    fn other_peer_unaffected() {
        let mut l = LoginLimiter::new(LimiterConfig::default(), at(0));
        for _ in 0..8 {
            l.record_failure(peer(1), at(0));
        }
        assert!(matches!(l.check(peer(1), at(0)), Admit::Deny { .. }));
        assert_eq!(l.check(peer(2), at(0)), Admit::Allow);
    }

    #[test]
    fn slots_bounded_64_evict_oldest_last_failure() {
        let cfg = LimiterConfig { global_burst: 1000, ..LimiterConfig::default() };
        let mut l = LoginLimiter::new(cfg, at(0));
        for n in 0..64u8 {
            l.record_failure(peer(n), at(u64::from(n)));
        }
        // Make peer 0 the most recent failure; peer 1 is now the oldest.
        l.record_failure(peer(0), at(100));
        assert_eq!(l.slots.len(), 64);
        l.record_failure(peer(200), at(101));
        assert_eq!(l.slots.len(), 64);
        assert!(l.slots.iter().all(|s| s.key != peer(1)));
        assert!(l.slots.iter().any(|s| s.key == peer(0) && s.failures == 2));
        assert!(l.slots.iter().any(|s| s.key == peer(200)));
    }

    #[test]
    fn ipv6_full_address_key() {
        let a: SocketAddr = "[2001:db8::1]:5000".parse().unwrap();
        let b: SocketAddr = "[2001:db8::2]:5000".parse().unwrap();
        assert_ne!(PeerKey::from(a), PeerKey::from(b));
        let mut l = LoginLimiter::new(LimiterConfig::default(), at(0));
        for _ in 0..5 {
            l.record_failure(PeerKey::from(a), at(0));
        }
        assert!(matches!(l.check(PeerKey::from(a), at(0)), Admit::Deny { .. }));
        assert_eq!(l.check(PeerKey::from(b), at(0)), Admit::Allow);
    }

    #[test]
    fn v4_mapped_is_v4() {
        let mapped: SocketAddr = "[::ffff:10.0.0.1]:5000".parse().unwrap();
        let plain: SocketAddr = "10.0.0.1:1234".parse().unwrap();
        assert_eq!(PeerKey::from(mapped), PeerKey::V4([10, 0, 0, 1]));
        assert_eq!(PeerKey::from(mapped), PeerKey::from(plain));
        // Loopback v6 is not v4-mapped.
        let lo: SocketAddr = "[::1]:5000".parse().unwrap();
        assert!(matches!(PeerKey::from(lo), PeerKey::V6(_)));
    }

    #[test]
    fn global_bucket_denies_fresh_peer_then_refills() {
        let mut l = LoginLimiter::new(LimiterConfig::default(), at(0));
        // 30 failures spread over 30 peers: no peer is blocked on its own.
        for n in 0..30u8 {
            assert_eq!(l.check(peer(n), at(0)), Admit::Allow);
            l.record_failure(peer(n), at(0));
        }
        let fresh = peer(250);
        assert_eq!(l.check(fresh, at(0)), Admit::Deny { retry_after: Duration::from_secs(10) });
        assert_eq!(deny_for(l.check(fresh, at(4_000))), Duration::from_secs(6));
        // One failure is given back after 10 s ...
        assert_eq!(l.check(fresh, at(10_000)), Admit::Allow);
        l.record_failure(fresh, at(10_000));
        // ... and used up again.
        assert_eq!(deny_for(l.check(peer(251), at(10_000))), Duration::from_secs(10));
        // A long quiet period refills the whole bucket.
        assert_eq!(l.check(peer(252), at(10_000 + 300_000)), Admit::Allow);
        assert_eq!(l.tokens, 30);
    }

    #[test]
    fn success_does_not_touch_the_bucket() {
        let mut l = LoginLimiter::new(LimiterConfig::default(), at(0));
        for n in 0..29u8 {
            l.record_failure(peer(n), at(0));
        }
        for n in 0..29u8 {
            l.record_success(peer(n));
        }
        assert_eq!(l.tokens, 1);
        assert_eq!(l.check(peer(100), at(0)), Admit::Allow);
    }

    #[test]
    fn retry_after_ceil_min_1() {
        assert_eq!(retry_after_secs(Duration::ZERO), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_after_secs(Duration::from_secs(1)), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(1001)), 2);
        assert_eq!(retry_after_secs(Duration::from_millis(9_999)), 10);
        assert_eq!(retry_after_secs(Duration::from_secs(300)), 300);
    }
}
