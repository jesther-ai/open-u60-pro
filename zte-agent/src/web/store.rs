//! Small per-session tables in RAM (operations, flash results, the USB
//! card's dismissal, single-use form tokens): capped, each entry kept
//! for a fixed time on the boot clock and only for the session that made
//! it. Pruned when touched; nothing runs in the background.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::auth::{AuthState, SessionId};
use crate::boottime::BootInstant;

/// The result line of an action whose message depends on the device's
/// answer ("Killed PID 812, freed 1.2 MB") and its tone (`Some(true)`
/// success, `Some(false)` failure, `None` neutral), shown by `?r=<id>` on
/// the form's screen (on every load of that URL): at most 8 per session
/// (other sessions' lines never push one out) and 64 in all, each for 10
/// minutes, only to its session.
pub type Flash = SessionStore<(Option<bool>, String)>;

impl Default for Flash {
    fn default() -> Self {
        SessionStore::shared(64, 8, Duration::from_secs(10 * 60))
    }
}

/// Single-use form tokens (an SMS send, a forwarding rule's save or
/// test), each with its send's state and,
/// once claimed, a hash of what it sends (destination and text; never the
/// text itself). A used token stays until it expires, so a replay of the
/// same send (a double click, a reload of the answer) is recognised, sends
/// nothing and shows what the first post did; another send with it is an
/// expired form, as is an unknown token. At most 64 across all sessions,
/// each for an hour (`Once::issue` says which goes when full).
pub type Once = SessionStore<Token>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Fresh,
    /// Claimed by a post that is sending (or about to); back to `Fresh` if
    /// it ends up sending nothing.
    Sending(u64),
    Sent(u64),
    /// The send failed: its status and the message shown.
    Failed(u64, u16, String),
}

impl Default for Once {
    fn default() -> Self {
        SessionStore::new(64, Duration::from_secs(3600))
    }
}

impl Once {
    /// A new token of `owner`. Full: one that is sending never goes; else
    /// the oldest used one (the session's first: it only recognises a
    /// replay, which then sends nothing either), else the oldest unused one
    /// of the session holding the most (this one on a tie): a page that
    /// reloads itself pushes out its own old tokens, and a session holding
    /// few keeps them.
    pub fn issue(&self, id: String, owner: SessionId, now: BootInstant) {
        let mut items = self.lock(now);
        if items.len() >= self.cap {
            let used = |mine: bool| items.iter().position(|e| matches!(e.value, Token::Sent(_) | Token::Failed(..)) && (e.owner == owner) == mine);
            let fresh = || {
                let held = |o: SessionId| items.iter().filter(|e| e.owner == o && e.value == Token::Fresh).count();
                let top = items.iter().filter(|e| e.value == Token::Fresh).map(|e| e.owner).max_by_key(|&o| (held(o), o == owner))?;
                items.iter().position(|e| e.owner == top && e.value == Token::Fresh)
            };
            if let Some(i) = used(true).or_else(|| used(false)).or_else(fresh) {
                items.remove(i);
            }
        }
        items.push(Entry { id, owner, at: now, value: Token::Fresh });
    }

    /// A new random token of `owner` for a form ("" without randomness:
    /// that form's post is then an expired one).
    pub fn fresh(&self, auth: &AuthState, owner: SessionId) -> String {
        let token = auth.random_hex(8).unwrap_or_default();
        if !token.is_empty() {
            self.issue(token.clone(), owner, auth.now());
        }
        token
    }
}

/// A claimed single-use token: given back (`Fresh`) if dropped before the
/// device is asked, also when the request is dropped while it waits for
/// the router or the work panics.
pub struct Claim {
    once: Arc<Once>,
    auth: Arc<AuthState>,
    token: String,
    owner: SessionId,
    digest: u64,
    armed: bool,
}

impl Claim {
    /// Claim a fresh token of `owner` for a post with `digest`.
    pub fn take(once: &Arc<Once>, auth: &Arc<AuthState>, token: &str, owner: SessionId, digest: u64) -> Option<Claim> {
        let fresh = once.update(token, owner, auth.now(), |t| {
            let fresh = *t == Token::Fresh;
            if fresh {
                *t = Token::Sending(digest);
            }
            fresh
        });
        (fresh == Some(true)).then(|| Claim {
            once: Arc::clone(once),
            auth: Arc::clone(auth),
            token: token.to_owned(),
            owner,
            digest,
            armed: true,
        })
    }

    pub fn digest(&self) -> u64 {
        self.digest
    }

    /// From here the outcome is the device's: never given back.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// The device was asked (`disarm` was called).
    pub fn sent(&self) -> bool {
        !self.armed
    }

    pub fn set(&self, state: Token) {
        self.once.update(&self.token, self.owner, self.auth.now(), |t| *t = state);
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.armed {
            self.set(Token::Fresh);
        }
    }
}

/// What a post of a token another post claimed finds.
#[derive(Debug, PartialEq, Eq)]
pub enum Settled {
    /// Claimed for another destination or text (an old copy of the form).
    Other,
    /// That post sent nothing (the router was busy, a read failed).
    Fresh,
    Sent,
    /// Its status and the message it showed.
    Failed(u16, String),
    /// Still sending after the wait, or the token is gone.
    Unknown,
}

/// How long a replay waits for its first post: the general lane's work.
pub const SETTLE: Duration = Duration::from_secs(20);
/// The slow lane's (a forwarding test or retry): up to 7 s for a permit,
/// then up to 15 s for the destination.
pub const SETTLE_SLOW: Duration = Duration::from_secs(30);

/// Wait (at most `limit`) until the post that claimed `token` is done.
pub async fn settle(once: &Once, token: &str, owner: SessionId, digest: u64, auth: &AuthState, limit: Duration) -> Settled {
    let step = Duration::from_millis(100);
    for _ in 0..limit.as_millis() / step.as_millis() {
        match once.get(token, owner, auth.now()) {
            Some(Token::Sending(d) | Token::Sent(d) | Token::Failed(d, ..)) if d != digest => return Settled::Other,
            Some(Token::Sending(_)) => tokio::time::sleep(step).await,
            Some(Token::Sent(_)) => return Settled::Sent,
            Some(Token::Failed(_, status, msg)) => return Settled::Failed(status, msg),
            Some(Token::Fresh) => return Settled::Fresh,
            None => break,
        }
    }
    Settled::Unknown
}

/// Drafts kept by a form's single-use token, so the page never holds what
/// they keep: at most 16, each for an hour (as its token), only for the
/// session that typed them; dropped on logout.
/// - Secrets typed into a forwarding rule form that was shown again (a
///   test's answer, a refused save), so the next post of that form keeps
///   them (`web::forward`). A form whose draft is gone (pushed out) says so
///   instead of falling back to the rule's secrets.
/// - A Router settings form that was posted (`pages::netcfg`): what its
///   question shows and its confirmed post sends, passwords included until
///   it is sent.
pub type Drafts = SessionStore<Draft>;

#[derive(Clone)]
pub enum Draft {
    Rule(crate::web::forward::Secrets),
    Router(crate::web::pages::netcfg::Draft),
}

impl Default for Drafts {
    fn default() -> Self {
        SessionStore::new(16, Duration::from_secs(3600))
    }
}

/// USSD answers (SIM Services, `pages::services`): the code sent, the
/// network's answer and whether its session waits for a reply, or the
/// outcome's line; shown by `?u=<id>` to the session that sent it: at most
/// 8, each for 10 minutes, in RAM only, gone when the session logs out.
pub type UssdLog = SessionStore<Ussd>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ussd {
    pub code: String,
    /// The answer and whether the network waits for a reply.
    pub answer: Option<(String, bool)>,
    /// A result line (`true`: the app's green).
    pub line: Option<(bool, String)>,
}

impl Default for UssdLog {
    fn default() -> Self {
        SessionStore::new(8, Duration::from_secs(10 * 60))
    }
}

/// Dismissals per session (the dashboard's USB-C card, `pages::usb`): at
/// most 16, each for a day.
pub type Dismissed = SessionStore<()>;

impl Default for Dismissed {
    fn default() -> Self {
        SessionStore::new(16, Duration::from_secs(24 * 3600))
    }
}

/// AT Terminal history (`pages::at`): each session's last 20 commands and
/// answers, newest first, in RAM only. At most 64 entries across all
/// sessions (the oldest goes first), each kept 12 hours (a session's idle
/// limit); logging out, or in again in the same browser, drops the
/// session's. Commands and answers are stored masked (`pages::at`) and
/// cut to 4 KiB.
#[derive(Default)]
pub struct AtLog {
    items: Mutex<Vec<(SessionId, BootInstant, AtEntry)>>,
}

#[derive(Clone)]
pub struct AtEntry {
    pub cmd: String,
    pub resp: String,
    pub port: String,
    pub ms: u64,
    /// "HH:MM:SS", the router's local time.
    pub clock: String,
    pub err: bool,
    /// The timeout it was sent with (the form keeps the last one).
    pub timeout: u8,
}

const AT_PER_SESSION: usize = 20;
const AT_CAP: usize = 64;
const AT_TTL: Duration = Duration::from_secs(12 * 3600);

impl AtLog {
    fn lock(&self, now: BootInstant) -> MutexGuard<'_, Vec<(SessionId, BootInstant, AtEntry)>> {
        let mut items = self.items.lock().unwrap_or_else(PoisonError::into_inner);
        items.retain(|(_, at, _)| now.saturating_duration_since(*at) < AT_TTL);
        items
    }

    pub fn push(&self, owner: SessionId, entry: AtEntry, now: BootInstant) {
        let mut items = self.lock(now);
        if items.iter().filter(|(o, ..)| *o == owner).count() >= AT_PER_SESSION {
            if let Some(i) = items.iter().position(|(o, ..)| *o == owner) {
                items.remove(i);
            }
        }
        if items.len() >= AT_CAP {
            items.remove(0);
        }
        items.push((owner, now, entry));
    }

    /// `owner`'s entries, newest first.
    pub fn list(&self, owner: SessionId, now: BootInstant) -> Vec<AtEntry> {
        self.lock(now).iter().rev().filter(|(o, ..)| *o == owner).map(|(.., e)| e.clone()).collect()
    }

    pub fn clear(&self, owner: SessionId) {
        self.items.lock().unwrap_or_else(PoisonError::into_inner).retain(|(o, ..)| *o != owner);
    }
}

struct Entry<T> {
    /// 16 lowercase hex characters (random).
    id: String,
    owner: SessionId,
    at: BootInstant,
    value: T,
}

pub struct SessionStore<T> {
    cap: usize,
    /// Entries per session (`shared`), if limited.
    share: Option<usize>,
    ttl: Duration,
    items: Mutex<Vec<Entry<T>>>,
}

impl<T: Clone> SessionStore<T> {
    pub const fn new(cap: usize, ttl: Duration) -> Self {
        SessionStore { cap, share: None, ttl, items: Mutex::new(Vec::new()) }
    }

    /// At most `share` entries per session and `cap` in all: a session at
    /// its share loses its own oldest; a full store, the oldest of the
    /// session holding the most.
    pub const fn shared(cap: usize, share: usize, ttl: Duration) -> Self {
        SessionStore { cap, share: Some(share), ttl, items: Mutex::new(Vec::new()) }
    }

    fn lock(&self, now: BootInstant) -> MutexGuard<'_, Vec<Entry<T>>> {
        let mut items = self.items.lock().unwrap_or_else(PoisonError::into_inner);
        items.retain(|e| now.saturating_duration_since(e.at) < self.ttl);
        items
    }

    /// Full: the oldest entry of the same session goes (another session's
    /// only if this one has none); see `shared` for a store with shares.
    pub fn insert(&self, id: String, owner: SessionId, value: T, now: BootInstant) {
        let mut items = self.lock(now);
        let held = |o: SessionId| items.iter().filter(|e| e.owner == o).count();
        let gone = match self.share {
            Some(share) if held(owner) >= share => items.iter().position(|e| e.owner == owner),
            Some(_) if items.len() >= self.cap => {
                let most = items.iter().map(|e| e.owner).max_by_key(|o| held(*o));
                items.iter().position(|e| Some(e.owner) == most)
            }
            None if items.len() >= self.cap => Some(items.iter().position(|e| e.owner == owner).unwrap_or(0)),
            _ => None,
        };
        if let Some(i) = gone {
            items.remove(i);
        }
        items.push(Entry { id, owner, at: now, value });
    }

    /// Entry `id` if `owner` made it (anyone else: none).
    pub fn get(&self, id: &str, owner: SessionId, now: BootInstant) -> Option<T> {
        self.lock(now).iter().find(|e| e.id == id && e.owner == owner).map(|e| e.value.clone())
    }

    /// Change entry `id` of `owner` under the lock: `f`'s answer, or none
    /// if there is no such entry.
    pub fn update<R>(&self, id: &str, owner: SessionId, now: BootInstant, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.lock(now).iter_mut().find(|e| e.id == id && e.owner == owner).map(|e| f(&mut e.value))
    }

    /// Forget entry `id` of `owner` (`None`: of every session).
    pub fn remove(&self, id: &str, owner: Option<SessionId>, now: BootInstant) {
        self.lock(now).retain(|e| e.id != id || owner.is_some_and(|o| o != e.owner));
    }

    /// Forget every entry of `owner` (its session ended).
    pub fn clear(&self, owner: SessionId) {
        self.items.lock().unwrap_or_else(PoisonError::into_inner).retain(|e| e.owner != owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Result lines: other sessions' lines never push out this session's
    /// (two posts at once keep both); a session past its share loses its
    /// own oldest; a full store, the biggest holder's oldest.
    #[test]
    fn result_lines_are_shared_per_session() {
        let at = |s: u64| BootInstant::from_duration(Duration::from_secs(s));
        let line = |s: &str| (Some(true), s.to_owned());
        let flash = Flash::default();
        for i in 0..40u64 {
            flash.insert(format!("other{i}"), SessionId::for_test(10 + i % 5), line("theirs"), at(1));
        }
        let me = SessionId::for_test(1);
        flash.insert("first".into(), me, line("first"), at(2));
        flash.insert("second".into(), me, line("second"), at(2));
        assert_eq!(flash.get("first", me, at(3)), Some(line("first")));
        assert_eq!(flash.get("second", me, at(3)), Some(line("second")));
        for i in 0..7 {
            flash.insert(format!("mine{i}"), me, line("more"), at(4));
        }
        assert_eq!(flash.get("first", me, at(5)), None, "past its share: its own oldest");
        assert!(flash.get("second", me, at(5)).is_some());
        // Full: the session holding the most (8 lines each) loses its oldest.
        for i in 0..30u64 {
            flash.insert(format!("late{i}"), SessionId::for_test(20 + i), line("x"), at(6));
        }
        let items = flash.items.lock().unwrap();
        assert_eq!(items.len(), 64);
        assert!(items.iter().all(|e| items.iter().filter(|o| o.owner == e.owner).count() <= 8));
        assert!(items.iter().any(|e| e.id == "mine6") && items.iter().any(|e| e.id == "late29"));
    }

    /// Single-use tokens when the table is full: one that is sending never
    /// goes; used ones first (the session's, then another's), then the
    /// session's oldest unused one, then another session's. Each lasts an
    /// hour.
    #[test]
    fn a_full_once_table_keeps_sends_in_flight() {
        let (me, other) = (SessionId::for_test(1), SessionId::for_test(2));
        let at = |s: u64| BootInstant::from_duration(Duration::from_secs(s));
        let once = Once::default();
        once.issue("sending".into(), me, at(1));
        once.update("sending", me, at(1), |t| *t = Token::Sending(7));
        once.issue("theirs-sent".into(), other, at(2));
        once.update("theirs-sent", other, at(2), |t| *t = Token::Sent(7));
        once.issue("sent".into(), me, at(2));
        once.update("sent", me, at(2), |t| *t = Token::Sent(7));
        once.issue("theirs".into(), other, at(3));
        for i in 4..64 {
            once.issue(format!("fresh{i}"), me, at(4));
        }
        let has = |id: &str| once.get(id, me, at(5)).is_some() || once.get(id, other, at(5)).is_some();
        // Full (64): my used token goes, then another session's used one.
        once.issue("new1".into(), me, at(5));
        assert!(!has("sent") && has("theirs-sent") && has("fresh4"));
        once.issue("new2".into(), me, at(5));
        assert!(!has("theirs-sent") && has("fresh4") && has("theirs"));
        // Then my oldest unused one; the one sending and another session's stay.
        once.issue("new3".into(), me, at(5));
        assert!(!has("fresh4") && has("fresh5") && has("sending") && has("theirs") && has("new1"));
        for i in 5..64 {
            once.issue(format!("again{i}"), me, at(5));
        }
        assert!(has("sending") && has("theirs") && !has("fresh63"));
        // Another session's turn: I hold the most, so my oldest goes; its own stays.
        once.issue("theirs2".into(), other, at(6));
        assert!(has("theirs") && has("theirs2") && has("sending") && !has("new1") && has("new2"));
        // A new session in a full table: its tokens in a row all stay (the
        // first is never the one that goes).
        let third = SessionId::for_test(3);
        for t in ["a", "b", "c"] {
            once.issue(t.into(), third, at(7));
        }
        assert!(["a", "b", "c"].iter().all(|t| once.get(t, third, at(7)).is_some()));
        // An hour later, all gone.
        assert_eq!(once.get("sending", me, at(1 + 3600)), None);
        assert_eq!(once.get("again63", me, at(5 + 3599)), Some(Token::Fresh));
    }

    /// AT history: 20 per session, 64 in all, 12 hours, each session its own.
    #[test]
    fn at_history_is_per_session_and_capped() {
        let (me, other) = (SessionId::for_test(1), SessionId::for_test(2));
        let at = |s: u64| BootInstant::from_duration(Duration::from_secs(s));
        let e = |n: usize| AtEntry { cmd: format!("AT{n}"), resp: String::new(), port: String::new(), ms: 0, clock: String::new(), err: false, timeout: 3 };
        let log = AtLog::default();
        for n in 0..25 {
            log.push(me, e(n), at(1));
        }
        let mine = log.list(me, at(2));
        assert_eq!(mine.len(), 20);
        assert_eq!((mine[0].cmd.as_str(), mine[19].cmd.as_str()), ("AT24", "AT5"));
        assert!(log.list(other, at(2)).is_empty());
        for n in 0..50 {
            log.push(other, e(100 + n), at(3));
        }
        assert_eq!(log.list(other, at(4)).len(), 20);
        // 64 in all: the oldest entries (mine) go first.
        for s in 3..5 {
            for n in 0..20 {
                log.push(SessionId::for_test(s), e(n), at(4));
            }
        }
        let mine = log.list(me, at(5));
        assert_eq!((mine.len(), mine.last().map(|e| e.cmd.as_str())), (4, Some("AT21")));
        log.clear(other);
        assert!(log.list(other, at(5)).is_empty() && log.list(me, at(5)).len() == 4);
        assert!(log.list(me, at(1 + 12 * 3600)).is_empty());
    }

    /// A replay waits for its first post at most `limit` (the slow lane's
    /// work takes longer than the general's), then calls it unknown.
    #[tokio::test]
    async fn settle_waits_up_to_its_limit() {
        let auth = AuthState::new();
        let me = SessionId::for_test(1);
        let once = Once::default();
        once.issue("t".into(), me, auth.now());
        once.update("t", me, auth.now(), |t| *t = Token::Sending(7));
        let started = std::time::Instant::now();
        assert_eq!(settle(&once, "t", me, 7, &auth, Duration::from_millis(300)).await, Settled::Unknown);
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert_eq!(settle(&once, "t", me, 8, &auth, SETTLE).await, Settled::Other);
        once.update("t", me, auth.now(), |t| *t = Token::Sent(7));
        assert_eq!(settle(&once, "t", me, 7, &auth, SETTLE_SLOW).await, Settled::Sent);
        assert!(SETTLE_SLOW >= crate::httpd::GATE_WAIT + Duration::from_secs(15) + Duration::from_secs(5));
    }
}
