//! Time since boot for anything duration-based (session expiry, rate limits,
//! data ages). `CLOCK_BOOTTIME` keeps counting while the SoC is suspended and
//! ignores the +8 h NITZ wall-clock jumps. `std::time::Instant` (which pauses
//! in suspend) stays reserved for scheduling wakeups.

use std::time::Duration;

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::Arc;

/// A point on the boot clock. Only differences between two instants mean
/// anything; the value is reset by a reboot.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BootInstant(Duration);

impl BootInstant {
    pub const ZERO: Self = BootInstant(Duration::ZERO);

    #[cfg(test)]
    pub const fn from_duration(d: Duration) -> Self {
        BootInstant(d)
    }

    pub fn saturating_add(self, d: Duration) -> Self {
        BootInstant(self.0.saturating_add(d))
    }

    pub fn saturating_duration_since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }

    pub fn as_millis(self) -> u64 {
        u64::try_from(self.0.as_millis()).unwrap_or(u64::MAX)
    }

    pub fn as_secs(self) -> u64 {
        self.0.as_secs()
    }
}

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> BootInstant;
}

/// `CLOCK_BOOTTIME` on Linux; `CLOCK_MONOTONIC` elsewhere (macOS host tests).
pub struct BootClock;

// A constant per target rather than `if cfg!()`: Apple's libc has no
// CLOCK_BOOTTIME, and `cfg!()` would still type-check the Linux branch.
#[cfg(target_os = "linux")]
const CLOCK_ID: libc::clockid_t = libc::CLOCK_BOOTTIME;
#[cfg(not(target_os = "linux"))]
const CLOCK_ID: libc::clockid_t = libc::CLOCK_MONOTONIC;

impl Clock for BootClock {
    fn now(&self) -> BootInstant {
        // SAFETY: an all-zero timespec is a valid value of this plain C struct.
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        // SAFETY: ts points to writable timespec storage; CLOCK_ID exists on this target.
        if unsafe { libc::clock_gettime(CLOCK_ID, &mut ts) } != 0 {
            // Cannot fail for a supported clock id; never panic on a request path.
            return BootInstant::ZERO;
        }
        BootInstant(Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
    }
}

/// Test clock that only moves when told to.
#[cfg(test)]
pub struct FakeClock {
    nanos: AtomicU64,
}

#[cfg(test)]
impl FakeClock {
    pub fn at_secs(s: u64) -> Arc<Self> {
        Arc::new(FakeClock {
            nanos: AtomicU64::new(s * 1_000_000_000),
        })
    }

    pub fn advance(&self, d: Duration) {
        self.nanos.fetch_add(d.as_nanos() as u64, Ordering::SeqCst);
    }
}

#[cfg(test)]
impl Clock for FakeClock {
    fn now(&self) -> BootInstant {
        BootInstant(Duration::from_nanos(self.nanos.load(Ordering::SeqCst)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_clock_is_monotonic_and_nonzero() {
        let a = BootClock.now();
        let b = BootClock.now();
        assert!(a > BootInstant::ZERO);
        assert!(b >= a);
    }

    #[test]
    fn fake_clock_moves_only_when_advanced() {
        let clock = FakeClock::at_secs(100);
        assert_eq!(clock.now().as_secs(), 100);
        clock.advance(Duration::from_millis(1500));
        assert_eq!(clock.now().as_millis(), 101_500);
        let t = clock.now();
        assert_eq!(t.saturating_add(Duration::from_secs(1)).saturating_duration_since(t), Duration::from_secs(1));
        assert_eq!(BootInstant::ZERO.saturating_duration_since(t), Duration::ZERO);
        assert_eq!(BootInstant::from_duration(Duration::from_secs(7)).as_secs(), 7);
    }
}
