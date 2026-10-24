//! A mutex that publishes: mutable access to the value marks its topic
//! stale (`events::touch`), so the hub re-reads it through the same handler
//! the GET route uses, only if someone is subscribed. Call sites keep their
//! `.lock().unwrap()`.

use std::ops::{Deref, DerefMut};
use std::sync::{LockResult, Mutex, MutexGuard, PoisonError};

use super::Topic;

pub struct Watched<T> {
    inner: Mutex<T>,
    topic: Topic,
}

impl<T> Watched<T> {
    pub const fn new(v: T, topic: Topic) -> Self {
        Watched { inner: Mutex::new(v), topic }
    }

    pub fn lock(&self) -> LockResult<WatchedGuard<'_, T>> {
        let wrap = |guard| WatchedGuard { guard, topic: self.topic, dirty: false };
        match self.inner.lock() {
            Ok(guard) => Ok(wrap(guard)),
            Err(poisoned) => Err(PoisonError::new(wrap(poisoned.into_inner()))),
        }
    }
}

pub struct WatchedGuard<'a, T> {
    guard: MutexGuard<'a, T>,
    topic: Topic,
    dirty: bool,
}

impl<T> Deref for WatchedGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for WatchedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.dirty = true;
        &mut self.guard
    }
}

impl<T> Drop for WatchedGuard<'_, T> {
    /// Runs before `guard` is released, i.e. still under the lock, so
    /// touches follow mutation order.
    fn drop(&mut self) {
        if self.dirty {
            super::touch(self.topic);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_sink;
    use super::*;

    #[test]
    fn mutable_access_touches_read_only_does_not() {
        let w = Watched::new(5u32, Topic::EsimDownload);
        test_sink::take();
        assert_eq!(*w.lock().unwrap(), 5);
        let g = w.lock().unwrap();
        let _ = *g + 1;
        drop(g);
        assert!(test_sink::take().is_empty());
        *w.lock().unwrap() = 6;
        w.lock().unwrap().clone_from(&7);
        assert_eq!(test_sink::take(), [Topic::EsimDownload, Topic::EsimDownload]);
        assert_eq!(*w.lock().unwrap(), 7);
    }

    #[test]
    fn poisoned_lock_still_maps_and_touches() {
        let w = std::sync::Arc::new(Watched::new(String::from("a"), Topic::EsimSwitch));
        let w2 = std::sync::Arc::clone(&w);
        let _ = std::thread::spawn(move || {
            let _g = w2.lock().unwrap();
            panic!("poison");
        })
        .join();
        test_sink::take();
        let Err(poisoned) = w.lock() else { panic!("not poisoned") };
        let mut g = poisoned.into_inner();
        assert_eq!(g.as_str(), "a");
        g.push('b');
        drop(g);
        assert_eq!(test_sink::take(), [Topic::EsimSwitch]);
        assert_eq!(w.lock().unwrap_or_else(PoisonError::into_inner).as_str(), "ab");
    }
}
