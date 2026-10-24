//! The last 120 signal readings for the signal page's chart: a fixed ring,
//! so its memory never grows (about 2 KB), stamped on the boot clock. The
//! chart spans 2 minutes, and a page may ask for a reading every second
//! (Settings › Polling).

use serde_json::Value;

use crate::boottime::BootInstant;

pub const LEN: usize = 120;
/// No reading (absent, zero or out of range).
pub const NONE: i16 = i16::MIN;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HistPoint {
    /// Boot clock, ms.
    pub at_ms: u64,
    pub nr_rsrp: i16,
    pub lte_rsrp: i16,
    pub rscp: i16,
}

const EMPTY: HistPoint = HistPoint { at_ms: 0, nr_rsrp: NONE, lte_rsrp: NONE, rscp: NONE };

pub struct SignalHistory {
    points: [HistPoint; LEN],
    /// Next slot to write.
    next: usize,
    len: usize,
}

impl SignalHistory {
    pub const fn new() -> Self {
        SignalHistory { points: [EMPTY; LEN], next: 0, len: 0 }
    }

    /// One `GET /api/network/signal` reading; values outside -160..-20 dBm
    /// count as none (the Live Activity's filter).
    pub fn push(&mut self, at: BootInstant, data: &Value) {
        let dbm = |key| {
            crate::live_activity::number(data, key, -160.0, -20.0).map_or(NONE, |v| v.round() as i16)
        };
        self.points[self.next] = HistPoint {
            at_ms: at.as_millis(),
            nr_rsrp: dbm("nr5g_rsrp"),
            lte_rsrp: dbm("lte_rsrp"),
            rscp: dbm("rscp"),
        };
        self.next = (self.next + 1) % LEN;
        self.len = (self.len + 1).min(LEN);
    }

    /// Oldest first.
    pub fn to_vec(&self) -> Vec<HistPoint> {
        let start = (self.next + LEN - self.len) % LEN;
        (0..self.len).map(|i| self.points[(start + i) % LEN]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn ring_keeps_the_last_120_oldest_first() {
        let mut h = SignalHistory::new();
        assert!(h.to_vec().is_empty());
        for i in 0..135u64 {
            let at = BootInstant::from_duration(Duration::from_secs(100 + i));
            h.push(at, &json!({"lte_rsrp": format!("-{}", 90 + i % 10), "nr5g_rsrp": 0, "rscp": "-80.4"}));
        }
        let v = h.to_vec();
        assert_eq!(v.len(), LEN);
        assert_eq!(v[0].at_ms, 115_000);
        assert_eq!(v[LEN - 1].at_ms, 234_000);
        assert_eq!((v[0].lte_rsrp, v[0].nr_rsrp, v[0].rscp), (-95, NONE, -80));
    }
}
