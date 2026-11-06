//! Bounded, on-demand data-path verification during an explicit eSIM handover.
use serde_json::{json, Value};
use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

const STABLE: Duration = Duration::from_secs(12);
const RECOVER_AFTER: Duration = Duration::from_secs(12);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Policy {
    enabled: bool,
    automatic: bool,
    pub revision: u64,
}

pub(crate) fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}

impl Policy {
    pub fn capture(data: &Value, revision: u64) -> Self {
        Self {
            enabled: integer(&data["enable"]) == Some(1),
            automatic: integer(&data["connect_mode"]) == Some(1),
            revision,
        }
    }
    pub fn allows(self, revision: u64, current: &Value) -> bool {
        self.enabled
            && self.automatic
            && self.revision == revision
            && integer(&current["enable"]) == Some(1)
            && integer(&current["connect_mode"]) == Some(1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Reachability {
    Ready,
    Down,
    Unsupported,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Decision {
    Wait,
    Ready,
    Recover,
}

#[derive(Default)]
pub(crate) struct Handover {
    healthy_since: Option<Instant>,
    down_since: Option<Instant>,
    recovered: bool,
}

impl Handover {
    pub fn reset(&mut self) {
        self.healthy_since = None;
        self.down_since = None;
    }
    pub fn recovery_started(&mut self) {
        self.recovered = true;
        self.reset();
    }
    pub fn observe(&mut self, health: Reachability, allowed: bool, now: Instant) -> Decision {
        match health {
            Reachability::Ready | Reachability::Unsupported => {
                self.down_since = None;
                let since = *self.healthy_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= STABLE {
                    Decision::Ready
                } else {
                    Decision::Wait
                }
            }
            Reachability::Down => {
                self.healthy_since = None;
                let since = *self.down_since.get_or_insert(now);
                if allowed
                    && !self.recovered
                    && now.saturating_duration_since(since) >= RECOVER_AFTER
                {
                    Decision::Recover
                } else {
                    Decision::Wait
                }
            }
        }
    }
}

/// The modem's bearer status says packets have an active IP path. Some U60
/// firmware reports `enable: 0` even while this field, the assigned address
/// and the kernel route are live, so callers that only need usable transport
/// must not treat that preference flag as stronger evidence than this status.
pub(crate) fn bearer_reported_connected(data: &Value) -> bool {
    matches!(
        data["connect_status"].as_str(),
        Some("connected" | "ipv4_connected" | "ipv6_connected" | "ipv4_ipv6_connected")
    )
}

/// Strict state for profile handover and automatic recovery. Here the user's
/// enable preference matters in addition to the modem's bearer status.
pub(crate) fn bearer_connected(data: &Value) -> bool {
    !matches!(integer(&data["enable"]), Some(0))
        && data["enable"] != false
        && bearer_reported_connected(data)
}

fn sources(data: &Value) -> Vec<IpAddr> {
    ["ipv4_address", "ipv6_address"]
        .into_iter()
        .filter_map(|key| data[key].as_str()?.parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast())
        .collect()
}

/// A fresh TLS connection bound to the selected CID's source address. No pool, proxy or
/// saved socket can make an old interface look healthy. Runs only during switch verification.
pub(crate) fn probe(data: &Value) -> Reachability {
    let sources = sources(data);
    if sources.is_empty() {
        return Reachability::Unsupported;
    }
    for source in sources {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .local_address(source)
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(4))
            .pool_max_idle_per_host(0)
            .redirect(reqwest::redirect::Policy::none())
            .build();
        let Ok(client) = client else {
            return Reachability::Unsupported;
        };
        // A valid TLS/HTTP response proves this path reached the host, even for HTTP errors.
        if client
            .head("https://example.com/")
            .header("Connection", "close")
            .send()
            .is_ok()
        {
            return Reachability::Ready;
        }
    }
    Reachability::Down
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Recovery {
    Restored,
    Cancelled,
    Failed,
}

/// The caller holds the eSIM operation guard. New agent modem commands cancel this operation.
/// Even an uncertain disconnect is followed by the matching restore while intent is unchanged.
pub(crate) fn reconnect(
    allowed: impl Fn() -> bool,
    restore_allowed: impl Fn() -> bool,
    mut read: impl FnMut() -> Option<Value>,
    mut write: impl FnMut(Value) -> bool,
    mut pause: impl FnMut(),
) -> Recovery {
    if !allowed() {
        return Recovery::Cancelled;
    }
    let Some(before) = read() else {
        return Recovery::Failed;
    };
    if integer(&before["enable"]) != Some(1) || integer(&before["connect_mode"]) != Some(1) {
        return Recovery::Cancelled;
    }
    let Some(roaming @ (0 | 1)) = integer(&before["roam_enable"]) else {
        return Recovery::Cancelled;
    };
    if !allowed() {
        return Recovery::Cancelled;
    }
    let _ = write(
        json!({"cid":1,"enable":0,"connect_mode":1,"roam_enable":roaming,"connect_status":"disconnected"}),
    );
    for _ in 0..5 {
        if !restore_allowed() {
            return Recovery::Cancelled;
        }
        // A queued manual command shortens the wait. Finish this atomic restore first;
        // it will take the same command lock and apply the user's choice afterward.
        if !allowed() {
            break;
        }
        if read().is_some_and(|v| v["connect_status"] == "disconnected") {
            break;
        }
        pause();
    }
    if !restore_allowed() {
        return Recovery::Cancelled;
    }
    // Re-read policy fields so a factory-UI roaming/mode change is not overwritten.
    let Some(latest) = read() else {
        if !restore_allowed() {
            return Recovery::Cancelled;
        }
        return if write(json!({"cid":1,"enable":1,"connect_mode":1,"roam_enable":roaming})) {
            Recovery::Restored
        } else {
            Recovery::Failed
        };
    };
    if integer(&latest["connect_mode"]) != Some(1)
        || integer(&latest["roam_enable"]) != Some(roaming)
    {
        return Recovery::Cancelled;
    }
    if !restore_allowed() {
        return Recovery::Cancelled;
    }
    if write(json!({"cid":1,"enable":1,"connect_mode":1,"roam_enable":roaming})) {
        Recovery::Restored
    } else {
        Recovery::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn a_stale_connected_string_does_not_override_disabled_data_or_unknown_states() {
        for state in [
            "connected",
            "ipv4_connected",
            "ipv6_connected",
            "ipv4_ipv6_connected",
        ] {
            assert!(bearer_connected(
                &json!({"enable":1,"connect_status":state})
            ));
            assert!(!bearer_connected(
                &json!({"enable":"0","connect_status":state})
            ));
            assert!(bearer_reported_connected(
                &json!({"enable":0,"connect_status":state})
            ));
        }
        for state in ["disconnected", "not_connected", "connecting", "unknown"] {
            assert!(!bearer_connected(
                &json!({"enable":1,"connect_status":state})
            ));
            assert!(!bearer_reported_connected(
                &json!({"enable":1,"connect_status":state})
            ));
        }
    }

    #[test]
    fn readiness_resets_on_a_stall_and_recovery_is_bounded_to_one_attempt() {
        let mut check = Handover::default();
        let start = Instant::now();
        assert_eq!(
            check.observe(Reachability::Ready, true, start),
            Decision::Wait
        );
        assert_eq!(
            check.observe(Reachability::Down, true, start + Duration::from_secs(5)),
            Decision::Wait
        );
        assert_eq!(
            check.observe(Reachability::Down, true, start + Duration::from_secs(18)),
            Decision::Recover
        );
        check.recovery_started();
        assert_eq!(
            check.observe(Reachability::Down, true, start + Duration::from_secs(40)),
            Decision::Wait
        );
        assert_eq!(
            check.observe(Reachability::Down, true, start + Duration::from_secs(60)),
            Decision::Wait
        );
        assert_eq!(
            check.observe(Reachability::Ready, true, start + Duration::from_secs(61)),
            Decision::Wait
        );
        assert_eq!(
            check.observe(Reachability::Ready, true, start + Duration::from_secs(74)),
            Decision::Ready
        );
    }

    #[test]
    fn disabled_manual_or_superseded_intent_cannot_reconnect() {
        let data = json!({"enable":1,"connect_mode":1});
        let policy = Policy::capture(&data, 7);
        assert!(policy.allows(7, &data));
        assert!(!policy.allows(8, &data));
        assert!(!policy.allows(7, &json!({"enable":0,"connect_mode":1})));
        assert!(!Policy::capture(&json!({"enable":1,"connect_mode":0}), 7).allows(7, &data));
        assert!(!Policy::capture(&json!({"enable":0,"connect_mode":1}), 7).allows(7, &data));
    }

    #[test]
    fn uncertain_disconnect_is_restored_without_touching_apn_or_roaming() {
        let writes = RefCell::new(Vec::new());
        let reads = Cell::new(0);
        let result = reconnect(
            || true,
            || true,
            || {
                let n = reads.get();
                reads.set(n + 1);
                Some(
                    json!({"enable":if n==0{1}else{0},"connect_mode":1,"roam_enable":0,"connect_status":"disconnected"}),
                )
            },
            |v| {
                writes.borrow_mut().push(v);
                writes.borrow().len() > 1
            },
            || {},
        );
        assert_eq!(result, Recovery::Restored);
        let writes = writes.borrow();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0]["enable"], 0);
        assert_eq!(writes[1]["enable"], 1);
        assert!(writes
            .iter()
            .all(|v| v["roam_enable"] == 0 && v.get("apn").is_none()));
    }

    #[test]
    fn a_new_manual_command_cancels_the_automatic_reenable() {
        let cancelled = Cell::new(false);
        let writes = RefCell::new(Vec::new());
        let result = reconnect(
            || !cancelled.get(),
            || !cancelled.get(),
            || Some(json!({"enable":1,"connect_mode":1,"roam_enable":1})),
            |v| {
                writes.borrow_mut().push(v);
                cancelled.set(true);
                true
            },
            || {},
        );
        assert_eq!(result, Recovery::Cancelled);
        assert_eq!(writes.borrow().len(), 1);
    }

    #[test]
    fn source_addresses_exclude_local_management_and_unset_addresses() {
        assert!(sources(&json!({"ipv4_address":"0.0.0.0","ipv6_address":"::1"})).is_empty());
        assert_eq!(
            sources(&json!({"ipv4_address":"100.64.0.2","ipv6_address":"2001:db8::2"})).len(),
            2
        );
    }
    #[test]
    fn a_queued_manual_command_shortens_the_wait_and_keeps_the_restore_atomic() {
        let pending = Cell::new(false);
        let writes = RefCell::new(Vec::new());
        let reads = Cell::new(0);
        let result = reconnect(
            || !pending.get(),
            || true,
            || {
                let n = reads.get();
                reads.set(n + 1);
                Some(
                    json!({"enable":if n==0{1}else{0},"connect_mode":1,"roam_enable":1,"connect_status":"disconnected"}),
                )
            },
            |v| {
                writes.borrow_mut().push(v);
                pending.set(true);
                true
            },
            || panic!("a pending command must not wait for the disconnect poll"),
        );
        assert_eq!(result, Recovery::Restored);
        assert_eq!(writes.borrow().len(), 2);
        assert_eq!(writes.borrow()[1]["enable"], 1);
    }

    #[test]
    fn a_factory_mode_change_is_not_overwritten_by_the_restore() {
        let reads = Cell::new(0);
        let writes = RefCell::new(Vec::new());
        let result = reconnect(
            || true,
            || true,
            || {
                let n = reads.get();
                reads.set(n + 1);
                Some(
                    json!({"enable":if n==0{1}else{0},"connect_mode":if n==0{1}else{0},"roam_enable":0,"connect_status":"disconnected"}),
                )
            },
            |v| {
                writes.borrow_mut().push(v);
                true
            },
            || {},
        );
        assert_eq!(result, Recovery::Cancelled);
        assert_eq!(writes.borrow().len(), 1);
    }
}
