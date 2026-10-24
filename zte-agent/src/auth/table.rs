//! Bounded session table. Only kind-tagged SHA-256 digests of the tokens are
//! stored; expiry is lazy (the facade prunes at the top of each call) and a
//! full kind evicts its least recently used entry.

use subtle::ConstantTimeEq;

use super::{AuthConfig, Kind, SessionId};
use crate::boottime::BootInstant;

pub(crate) struct Entry {
    pub(crate) id: SessionId,
    pub(crate) kind: Kind,
    pub(crate) digest: [u8; 32],
    pub(crate) csrf: [u8; 32],
    pub(crate) created: BootInstant,
    pub(crate) last_used: BootInstant,
}

impl Entry {
    /// `min(last_used + idle, created + absolute)` on the boot clock.
    pub(crate) fn deadline(&self, cfg: &AuthConfig) -> BootInstant {
        let p = cfg.policy(self.kind);
        self.last_used
            .saturating_add(p.idle)
            .min(self.created.saturating_add(p.absolute))
    }
}

pub(crate) struct SessionTable {
    entries: Vec<Entry>,
    next_id: u64,
    /// Ids pruned most recently, so `touch_session` can still report them as
    /// expired (rather than revoked) after another call pruned them first.
    /// One slot per possible live entry: a single prune cannot overwrite an
    /// id it just recorded.
    recent_expired: Box<[Option<SessionId>]>,
    ring_pos: usize,
}

impl SessionTable {
    pub(crate) fn new(cfg: &AuthConfig) -> Self {
        let ring = (cfg.bearer.cap + cfg.browser.cap).max(1);
        SessionTable {
            entries: Vec::new(),
            next_id: 1,
            recent_expired: vec![None; ring].into_boxed_slice(),
            ring_pos: 0,
        }
    }

    pub(crate) fn alloc_id(&mut self) -> SessionId {
        let id = SessionId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Drop every entry past its deadline, remembering their ids.
    pub(crate) fn prune(&mut self, now: BootInstant, cfg: &AuthConfig) -> usize {
        let mut pruned = 0;
        let mut i = 0;
        while i < self.entries.len() {
            if now >= self.entries[i].deadline(cfg) {
                let id = self.entries.swap_remove(i).id;
                self.recent_expired[self.ring_pos] = Some(id);
                self.ring_pos = (self.ring_pos + 1) % self.recent_expired.len();
                pruned += 1;
            } else {
                i += 1;
            }
        }
        pruned
    }

    /// Insert, first evicting the least recently used entry of the same kind
    /// if that kind is at its cap. Returns the evicted id.
    pub(crate) fn insert(&mut self, e: Entry, cfg: &AuthConfig) -> Option<SessionId> {
        let cap = cfg.policy(e.kind).cap;
        let mut evicted = None;
        if self.entries.iter().filter(|x| x.kind == e.kind).count() >= cap {
            let lru = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, x)| x.kind == e.kind)
                .min_by_key(|(_, x)| x.last_used)
                .map(|(i, _)| i);
            if let Some(i) = lru {
                evicted = Some(self.entries.swap_remove(i).id);
            }
        }
        self.entries.push(e);
        evicted
    }

    /// Index of the entry with this digest; compares every entry's digest in
    /// constant time and never exits early.
    fn position(&self, kind: Kind, digest: &[u8; 32]) -> Option<usize> {
        let mut found = None;
        for (i, e) in self.entries.iter().enumerate() {
            let same: bool = e.digest.ct_eq(digest).into();
            if same && e.kind == kind && found.is_none() {
                found = Some(i);
            }
        }
        found
    }

    pub(crate) fn find(&mut self, kind: Kind, digest: &[u8; 32]) -> Option<&mut Entry> {
        let i = self.position(kind, digest)?;
        self.entries.get_mut(i)
    }

    pub(crate) fn by_id(&mut self, id: SessionId) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    pub(crate) fn remove_digest(&mut self, kind: Kind, digest: &[u8; 32]) -> Option<SessionId> {
        let i = self.position(kind, digest)?;
        Some(self.entries.swap_remove(i).id)
    }

    pub(crate) fn remove_id(&mut self, id: SessionId) -> bool {
        match self.entries.iter().position(|e| e.id == id) {
            Some(i) => {
                self.entries.swap_remove(i);
                true
            }
            None => false,
        }
    }

    pub(crate) fn recently_expired(&self, id: SessionId) -> bool {
        self.recent_expired.contains(&Some(id))
    }

    /// Removes every entry; returns their ids.
    pub(crate) fn clear(&mut self) -> Vec<SessionId> {
        self.entries.drain(..).map(|e| e.id).collect()
    }

    #[cfg(test)]
    pub(crate) fn len(&self, kind: Kind) -> usize {
        self.entries.iter().filter(|e| e.kind == kind).count()
    }
}
