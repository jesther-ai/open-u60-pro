//! B31 can lose UTC on reboot without cellular time. Bootstrap TLS from a
//! trusted local lower bound, then verify independent HTTPS Dates over Wi-Fi.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SAVED: &str = "/data/zte-agent-clock/utc";
pub(crate) static WIFI: WifiClock = WifiClock::new();

pub(crate) struct WifiClock {
    last: Mutex<Option<(u64, Instant)>>,
    error: Mutex<Option<String>>,
}

impl WifiClock {
    pub(crate) const fn new() -> Self {
        Self { last: Mutex::new(None), error: Mutex::new(None) }
    }

    pub(crate) fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// 2026-10-28：只推进本次启动已通过网络校验的 UTC，不读取系统时间或磁盘下界。
    /// 校时占用锁或锁已损坏时立即返回 None，不阻塞 watcher，也不发起网络请求。
    pub(crate) fn verified_utc(&self) -> Option<u64> {
        let last = self.last.try_lock().ok()?;
        let (utc, at) = (*last)?;
        utc.checked_add(at.elapsed().as_secs())
    }

    pub(crate) fn sync_with(&self, fresh: bool, check: impl FnOnce(&mut Option<(u64, Instant)>) -> Result<(), String>) -> Result<(), String> {
        let mut last = self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if fresh {
            *last = None;
        }
        let result = check(&mut last);
        if result.is_err() {
            *last = None;
        }
        // Status readers must not wait behind the HTTPS requests above.
        *self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = result.as_ref().err().cloned();
        result
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn floor(saved: Option<u64>) -> u64 {
    let build = env!("ZTE_AGENT_BUILD_EPOCH").parse::<u64>().unwrap();
    saved
        .filter(|n| *n >= build && *n < 4_102_444_800)
        .unwrap_or(build)
}

fn set(epoch: u64) -> Result<(), String> {
    let time = libc::timespec {
        tv_sec: epoch as _,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &time) } != 0 {
        return Err("Cannot set verified system UTC".into());
    }
    Ok(())
}

pub fn restore_floor() -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let saved = std::fs::read_to_string(SAVED)
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let earliest = floor(saved);
    if now() < earliest {
        set(earliest)?;
        eprintln!("[clock] restored local UTC lower bound; awaiting verified network time");
    }
    Ok(())
}

fn agreed(a: u64, b: u64) -> Result<u64, String> {
    if a < floor(None) || a >= 4_102_444_800 || a.abs_diff(b) > 15 {
        return Err("Independent HTTPS clocks disagree or precede this build".into());
    }
    Ok(a.max(b))
}

pub fn sync_wifi() -> Result<(), String> {
    sync_wifi_with(false)
}

pub fn recheck_wifi() -> Result<(), String> {
    sync_wifi_with(true)
}

fn sync_wifi_with(fresh: bool) -> Result<(), String> {
    // Ordinary unit tests never change host time or contact external services.
    if cfg!(test) {
        return Ok(());
    }
    WIFI.sync_with(fresh, sync)
}

fn sync(last: &mut Option<(u64, Instant)>) -> Result<(), String> {
    restore_floor()?;
    if let Some((utc, at)) = *last {
        if at.elapsed() < Duration::from_secs(300) {
            let live = utc + at.elapsed().as_secs();
            if now().abs_diff(live) > 3 {
                set(live)?;
            }
            return Ok(());
        }
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent("zte-agent-clock/1.0")
        .no_proxy()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .interface("u60sta0")
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(8))
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|_| "Cannot prepare verified clock client")?;
    let probe = |url: &str| -> Result<u64, String> {
        let start = Instant::now();
        let response = client
            .get(format!("{url}?u60-clock={}", now()))
            .header("Cache-Control", "no-cache, no-store")
            .send()
            .map_err(|_| "Cannot verify UTC over Wi-Fi HTTPS; check upstream/portal access")?;
        if !response.status().is_success()
            || start.elapsed() > Duration::from_secs(8)
            || response
                .headers()
                .get("age")
                .is_some_and(|v| v.to_str().ok().and_then(|s| s.parse::<u64>().ok()) != Some(0))
        {
            return Err("Clock response is unavailable or cached".into());
        }
        let date = response
            .headers()
            .get("date")
            .and_then(|v| v.to_str().ok())
            .ok_or("Clock response has no Date")?;
        httpdate::parse_http_date(date)
            .ok()
            .and_then(|d| d.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .ok_or_else(|| "Invalid HTTPS Date".into())
    };
    let a = probe("https://www.apple.com/library/test/success.html")?;
    let b = probe("https://www.microsoft.com/")?;
    let utc = agreed(a, b)?;
    if now().abs_diff(utc) > 3 {
        set(utc)?;
    }
    crate::net_tools::write_private(Path::new(SAVED), utc.to_string().as_bytes())?;
    *last = Some((utc, Instant::now()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_checks_bypass_cached_success_and_failures_cannot_reuse_it() {
        let clock = WifiClock::new();
        let verified = floor(None);
        let success = |last: &mut Option<(u64, Instant)>| {
            *last = Some((verified, Instant::now()));
            Ok(())
        };
        clock.sync_with(false, success).unwrap();
        clock.sync_with(false, |last| {
            assert_eq!(last.unwrap().0, verified, "Normal core startup may reuse verified UTC");
            Ok(())
        }).unwrap();
        assert!(clock.sync_with(true, |last| {
            assert!(last.is_none(), "An explicit recheck must probe again");
            Err("Upstream unavailable".into())
        }).is_err());
        assert_eq!(clock.error().as_deref(), Some("Upstream unavailable"));
        clock.sync_with(false, |last| {
            assert!(last.is_none(), "A failed recheck must not fall back to the old cache");
            assert_eq!(clock.error().as_deref(), Some("Upstream unavailable"), "Status stays readable while checking");
            success(last)
        }).unwrap();
        assert_eq!(clock.error(), None);
        assert!(clock.sync_with(false, |_| Err("Cannot set verified system UTC".into())).is_err());
        assert!(clock.last.lock().unwrap().is_none(), "Failures invalidate a previously verified cache too");
    }

    #[test]
    fn verified_utc_is_none_without_this_boots_verified_result() {
        let clock = WifiClock::new();
        assert_eq!(clock.verified_utc(), None);
        // 本地下界即使看似可信，也不能替代本次启动的网络校验。
        assert_eq!(floor(Some(floor(None) + 100)), floor(None) + 100);
        clock.sync_with(false, |_| Ok(())).unwrap();
        assert_eq!(clock.verified_utc(), None);
    }

    #[test]
    fn verified_utc_advances_only_from_the_verified_monotonic_anchor() {
        let clock = WifiClock::new();
        let utc = floor(None) + 100;
        let at = Instant::now() - Duration::from_secs(120);
        clock.sync_with(false, |last| {
            *last = Some((utc, at));
            Ok(())
        }).unwrap();
        let earliest = utc + at.elapsed().as_secs();
        let verified = clock.verified_utc().unwrap();
        let latest = utc + at.elapsed().as_secs();
        assert!((earliest..=latest).contains(&verified));
        assert!(verified >= utc + 120);
        assert!(clock.verified_utc().unwrap() >= verified);
    }

    #[test]
    fn verified_utc_does_not_wait_for_an_in_progress_check() {
        let clock = WifiClock::new();
        clock.sync_with(false, |last| {
            *last = Some((floor(None), Instant::now()));
            // 同线程持有校时锁；getter 若改用阻塞锁，本测试会卡住。
            assert_eq!(clock.verified_utc(), None);
            Ok(())
        }).unwrap();
        assert!(clock.verified_utc().is_some());
    }

    #[test]
    fn verified_utc_is_invalidated_by_any_failed_check() {
        for fresh in [false, true] {
            let clock = WifiClock::new();
            clock.sync_with(false, |last| {
                *last = Some((floor(None), Instant::now()));
                Ok(())
            }).unwrap();
            assert!(clock.verified_utc().is_some());
            assert!(clock.sync_with(fresh, |_| Err("Clock verification failed".into())).is_err());
            assert_eq!(clock.verified_utc(), None);
        }
    }

    #[test]
    fn verified_utc_does_not_wrap_on_overflow() {
        let clock = WifiClock::new();
        clock.sync_with(false, |last| {
            *last = Some((u64::MAX, Instant::now() - Duration::from_secs(1)));
            Ok(())
        }).unwrap();
        assert_eq!(clock.verified_utc(), None);
    }

    #[test]
    fn trusted_floor_and_independent_dates_reject_stale_or_disagreeing_time() {
        let build = floor(None);
        assert_eq!(floor(Some(1)), build);
        assert_eq!(floor(Some(u64::MAX)), build);
        assert_eq!(floor(Some(build + 100)), build + 100);
        assert!(agreed(build - 1, build - 1).is_err());
        assert!(agreed(build + 100, build + 120).is_err());
        assert_eq!(agreed(build + 100, build + 105).unwrap(), build + 105);
    }
}
