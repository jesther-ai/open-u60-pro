//! The demand-driven sampler: two threads ("hub-fast" for in-memory, /proc
//! and sysfs reads, "hub-ubus" for anything that forks), each taking the
//! next due topic of its lane from the hub. With no subscribers they park
//! without a timeout; one read serves every subscriber. Deadlines use
//! `Instant`, which pauses in suspend: one read after a resume, no burst.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

use super::hub::{Hub, ReadOut};
use super::{readers, Lane, Topic};
use crate::handlers::AppState;

pub trait Readers: Send + Sync + 'static {
    fn read(&self, t: Topic, hub: &Hub) -> Option<ReadOut>;
}

/// Production readers: the existing handlers (`readers::read`).
pub struct AppReaders(pub Arc<AppState>);

impl Readers for AppReaders {
    fn read(&self, t: Topic, hub: &Hub) -> Option<ReadOut> {
        readers::read(t, &self.0, hub)
    }
}

pub fn spawn(hub: Arc<Hub>, readers: Arc<dyn Readers>) {
    for (name, lane) in [("hub-fast", Lane::Fast), ("hub-ubus", Lane::Ubus)] {
        let (hub, readers) = (Arc::clone(&hub), Arc::clone(&readers));
        crate::system::spawn_named(name, move || lane_loop(&hub, lane, &*readers));
    }
}

fn lane_loop(hub: &Hub, lane: Lane, readers: &dyn Readers) -> ! {
    loop {
        let (t, started) = hub.next_job(lane);
        // No hub lock is held while reading. A reader that panics (a handler
        // meeting a poisoned lock, say) is a failed read: the lane lives on.
        let out = panic::catch_unwind(AssertUnwindSafe(|| readers.read(t, hub)))
            .unwrap_or_else(|_| Some(ReadOut { value: None, error: Some("internal error".into()) }));
        match out {
            Some(out) => hub.store(t, out, started),
            None => hub.read_skipped(t, started),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::hub::SubscribeReq;
    use super::*;
    use crate::auth::SessionId;
    use crate::boottime::BootClock;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Counting {
        calls: AtomicUsize,
    }

    impl Readers for Counting {
        fn read(&self, t: Topic, _: &Hub) -> Option<ReadOut> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            (t == Topic::Traffic).then(|| ReadOut { value: Some(json!(n)), error: None })
        }
    }

    #[test]
    fn samples_only_while_subscribed() {
        let hub = Arc::new(Hub::new(Arc::new(BootClock)));
        hub.set_test_cadence(Duration::from_millis(10));
        let readers = Arc::new(Counting::default());
        spawn(Arc::clone(&hub), readers.clone());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(readers.calls.load(Ordering::SeqCst), 0, "no subscriber, no read");

        let topics = [Topic::Traffic].into_iter().collect();
        let req = SubscribeReq { owner: SessionId::for_test(1), topics, signal_hint: None, replace_same: None };
        let sub = hub.subscribe(req).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while readers.calls.load(Ordering::SeqCst) < 3 {
            assert!(Instant::now() < deadline, "not sampled at the cadence while subscribed");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(hub.demand(Topic::Traffic), 1);

        drop(sub);
        assert_eq!(hub.due_job(Lane::Ubus, Instant::now()), Err(None));
        // A read already running may still finish: wait until the count
        // holds still, then it must stay put (10 cadences).
        let mut after = readers.calls.load(Ordering::SeqCst);
        loop {
            assert!(Instant::now() < deadline, "still sampling after the last subscriber left");
            std::thread::sleep(Duration::from_millis(50));
            let now = readers.calls.load(Ordering::SeqCst);
            if now == after {
                break;
            }
            after = now;
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(readers.calls.load(Ordering::SeqCst), after, "stopped with the last subscriber");
    }

    /// Panics on its first read.
    #[derive(Default)]
    struct PanicsOnce {
        calls: AtomicUsize,
    }

    impl Readers for PanicsOnce {
        fn read(&self, _: Topic, _: &Hub) -> Option<ReadOut> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("reader panic (expected by this test)");
            }
            Some(ReadOut { value: Some(json!("ok")), error: None })
        }
    }

    #[test]
    fn a_panicking_read_is_an_error_and_the_lane_survives() {
        let hub = Arc::new(Hub::new(Arc::new(BootClock)));
        hub.set_test_cadence(Duration::from_millis(10));
        let readers = Arc::new(PanicsOnce::default());
        spawn(Arc::clone(&hub), readers.clone());
        let topics = [Topic::Battery].into_iter().collect();
        let req = SubscribeReq { owner: SessionId::for_test(1), topics, signal_hint: None, replace_same: None };
        let _sub = hub.subscribe(req).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while hub.peek(Topic::Battery).value.is_none() {
            assert!(Instant::now() < deadline, "the lane died with its reader");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(readers.calls.load(Ordering::SeqCst) >= 2);
        assert_eq!(hub.peek(Topic::Battery).error, None);
    }
}
