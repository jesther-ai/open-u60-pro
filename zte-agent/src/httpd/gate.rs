//! Bounded hand-off of blocking work (ubus/uci forks, AT, card APDUs,
//! blocking HTTP) from the async runtime thread to tokio's blocking pool.
//!
//! Each lane is a FIFO semaphore. A permit is taken before the work is
//! spawned and moves into the blocking closure, so it is released when the
//! work ends, not when the HTTP future is dropped (blocking work cannot be
//! aborted). Waiting for a permit is bounded; running work is not.

use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Lane {
    /// Everything that normally answers within a few seconds.
    General,
    /// Calls that can legitimately take tens of seconds or minutes (eSIM
    /// card commands, Tailscale setup, AT/STK waits, outbound HTTP).
    Slow,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GateError {
    /// No permit within the wait: 503 `agent busy, retry`.
    Busy,
    /// The work panicked: 500 `internal error`.
    Panicked,
}

pub struct Gate {
    general: Arc<Semaphore>,
    slow: Arc<Semaphore>,
    wait: Duration,
}

impl Gate {
    pub fn new(general: usize, slow: usize, wait: Duration) -> Self {
        Gate {
            general: Arc::new(Semaphore::new(general)),
            slow: Arc::new(Semaphore::new(slow)),
            wait,
        }
    }

    /// Run `f` on the blocking pool once a permit of `lane` is free, waiting
    /// at most `wait` for it.
    pub async fn run<T, F>(&self, lane: Lane, f: F) -> Result<T, GateError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let sem = match lane {
            Lane::General => &self.general,
            Lane::Slow => &self.slow,
        };
        let permit = tokio::time::timeout(self.wait, Arc::clone(sem).acquire_owned())
            .await
            .map_err(|_| GateError::Busy)?
            .map_err(|_| GateError::Busy)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _scope = BlockingScope::enter();
            f()
        })
        .await
        .map_err(|_| GateError::Panicked)
    }

    #[cfg(test)]
    pub fn available(&self, lane: Lane) -> usize {
        match lane {
            Lane::General => self.general.available_permits(),
            Lane::Slow => self.slow.available_permits(),
        }
    }
}

thread_local! {
    static IN_GATE: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as running gated work.
pub(crate) struct BlockingScope;

impl BlockingScope {
    pub(crate) fn enter() -> Self {
        IN_GATE.with(|g| g.set(true));
        BlockingScope
    }
}

impl Drop for BlockingScope {
    fn drop(&mut self) {
        IN_GATE.with(|g| g.set(false));
    }
}

/// Debug builds: panic if a blocking device call runs on a thread inside the
/// tokio runtime without going through `Gate::run` (it would stall every
/// connection). Std threads (scheduler, policies) have no runtime context
/// and pass. Compiled out of release builds.
#[inline]
pub fn debug_assert_may_block() {
    #[cfg(debug_assertions)]
    if tokio::runtime::Handle::try_current().is_ok() && !IN_GATE.with(Cell::get) {
        panic!("blocking device call on the async runtime thread");
    }
}

#[cfg(debug_assertions)]
thread_local! {
    static READ_ONLY: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` as a page load (a web screen's reads and rendering): debug
/// builds panic if it calls a mutating API route (`debug_assert_may_change`).
pub fn read_only<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(debug_assertions)]
    struct Restore(bool);
    #[cfg(debug_assertions)]
    impl Drop for Restore {
        fn drop(&mut self) {
            READ_ONLY.with(|r| r.set(self.0));
        }
    }
    #[cfg(debug_assertions)]
    let _restore = Restore(READ_ONLY.with(|r| r.replace(true)));
    f()
}

/// Debug builds: panic on a call that changes the device inside `read_only`
/// (a GET page must never change anything). Compiled out of release builds.
#[inline]
pub fn debug_assert_may_change(path: &str) {
    #[cfg(debug_assertions)]
    if READ_ONLY.with(Cell::get) {
        panic!("a page load changed the device: {path}");
    }
    #[cfg(not(debug_assertions))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[tokio::test]
    async fn busy_after_wait() {
        let gate = Arc::new(Gate::new(1, 1, Duration::from_millis(50)));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let g = Arc::clone(&gate);
        let first = tokio::spawn(async move {
            g.run(Lane::General, move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                1
            })
            .await
        });
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap()).await.unwrap();
        assert_eq!(gate.run(Lane::General, || 2).await, Err(GateError::Busy));
        // The other lane is independent.
        assert_eq!(gate.run(Lane::Slow, || 3).await, Ok(3));
        release_tx.send(()).unwrap();
        assert_eq!(first.await.unwrap(), Ok(1));
        assert_eq!(gate.run(Lane::General, || 4).await, Ok(4));
    }

    #[tokio::test]
    async fn permit_held_until_closure_ends_even_if_future_dropped() {
        let gate = Gate::new(1, 1, Duration::from_secs(5));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel();
        let run = gate.run(Lane::Slow, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            done_tx.send(()).unwrap();
        });
        // The caller gives up (client gone) while the work runs.
        let waiting = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || started_rx.recv().unwrap()).await.unwrap();
        });
        tokio::select! {
            _ = run => panic!("the closure was released early"),
            _ = waiting => {}
        }
        assert_eq!(gate.available(Lane::Slow), 0);
        release_tx.send(()).unwrap();
        tokio::task::spawn_blocking(move || done_rx.recv().unwrap()).await.unwrap();
        // The permit comes back as the closure ends (FIFO wait, at most 5 s).
        assert_eq!(gate.run(Lane::Slow, || ()).await, Ok(()));
        assert_eq!(gate.available(Lane::Slow), 1);
    }

    #[tokio::test]
    async fn panic_maps_to_panicked() {
        let gate = Gate::new(1, 1, Duration::from_secs(1));
        let r: Result<(), _> = gate.run(Lane::General, || panic!("test panic")).await;
        assert_eq!(r, Err(GateError::Panicked));
        // The permit came back and the flag was cleared on the pool thread.
        assert_eq!(gate.available(Lane::General), 1);
        assert_eq!(gate.run(Lane::General, || IN_GATE.with(Cell::get)).await, Ok(true));
    }

    #[cfg(debug_assertions)]
    #[tokio::test]
    #[should_panic(expected = "blocking device call on the async runtime thread")]
    async fn ubus_call_on_runtime_thread_panics() {
        let _ = crate::ubus::call("system", "board", None);
    }

    #[cfg(debug_assertions)]
    #[tokio::test]
    #[should_panic(expected = "blocking device call on the async runtime thread")]
    async fn route_on_runtime_thread_panics() {
        let state = crate::handlers::AppState::new();
        let _ = crate::server::route(&crate::server::Method::Get, "/api/nope", &state, b"");
    }

    #[tokio::test]
    async fn blocking_calls_inside_gate_pass() {
        let gate = Gate::new(1, 1, Duration::from_secs(5));
        // No ubus on the test host: an error, but no guard panic.
        let r = gate.run(Lane::General, || crate::ubus::call("system", "board", None).is_err()).await;
        assert_eq!(r, Ok(true));
        let state = Arc::new(crate::handlers::AppState::new());
        let r = gate
            .run(Lane::General, move || {
                crate::server::route(&crate::server::Method::Get, "/api/nope", &state, b"").0
            })
            .await;
        assert_eq!(r, Ok(404));
        // Plain std threads have no runtime context.
        std::thread::spawn(debug_assert_may_block).join().unwrap();
    }
}
