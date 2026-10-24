//! Authentication for the JSON API (bearer tokens, used by the apps) and the
//! web UI (browser sessions): 32 random bytes per token, expiry on the boot
//! clock, a bounded table with lazy pruning (no timer) and a login rate limit.
//!
//! Every method may be called from the HTTP thread. `inner` is a leaf lock
//! held for microseconds: the RNG (which can block until the kernel CRNG is
//! seeded) and the revoke hook always run outside it, and a poisoned lock is
//! recovered instead of turning one panic into a panic on every request.

mod limiter;
mod rng;
mod table;

use std::fmt;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::boottime::{BootClock, BootInstant, Clock};
use limiter::{Admit, LoginLimiter};
pub use limiter::{retry_after_secs, LimiterConfig, PeerKey};
use rng::{OsRng, TokenRng};
#[cfg(test)]
pub(crate) use rng::{FailRng, SeqRng};
use table::{Entry, SessionTable};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Bearer,
    Browser,
}

impl Kind {
    /// Mixed into the stored digest, so a bearer token never validates as a
    /// browser cookie or the reverse.
    fn tag(self) -> &'static [u8] {
        match self {
            Kind::Bearer => b"bearer\0",
            Kind::Browser => b"browser\0",
        }
    }
}

/// Internal handle for a session; never sent to a client.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

#[cfg(test)]
impl SessionId {
    /// An id that no table issued (hub and stream tests).
    pub fn for_test(n: u64) -> Self {
        SessionId(n)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct Policy {
    /// Sliding: every authenticated use restarts it.
    pub idle: Duration,
    /// From login, regardless of use.
    pub absolute: Duration,
    /// Live entries of this kind; a new login evicts the least recently used.
    pub cap: usize,
}

#[derive(Copy, Clone, Debug)]
pub struct AuthConfig {
    pub bearer: Policy,
    pub browser: Policy,
    pub login_form_ttl: Duration,
    pub limiter: LimiterConfig,
}

impl AuthConfig {
    fn policy(&self, kind: Kind) -> &Policy {
        match kind {
            Kind::Bearer => &self.bearer,
            Kind::Browser => &self.browser,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        const HOUR: u64 = 3600;
        AuthConfig {
            // Same 1 h as before for an idle token; active apps re-login at most daily.
            bearer: Policy {
                idle: Duration::from_secs(HOUR),
                absolute: Duration::from_secs(24 * HOUR),
                cap: 32,
            },
            // RAM only: an agent restart logs every browser out.
            browser: Policy {
                idle: Duration::from_secs(12 * HOUR),
                absolute: Duration::from_secs(7 * 24 * HOUR),
                cap: 16,
            },
            login_form_ttl: Duration::from_secs(15 * 60),
            limiter: LimiterConfig::default(),
        }
    }
}

/// A token or CSRF value: 64 lowercase hex. `Debug` never shows it.
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug)]
pub struct Issued {
    // Only tests read it: requests carry the id in their `Principal`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub id: SessionId,
    pub token: Secret,
    /// Browser sessions only.
    // Only tests read it: pages get the value through `csrf_token(principal)`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub csrf: Option<Secret>,
    /// Cookie `Max-Age`: the absolute lifetime of this kind.
    pub max_age: Duration,
}

/// Who an authenticated request belongs to.
#[derive(Clone)]
pub struct Principal {
    pub id: SessionId,
    pub kind: Kind,
    /// Per-session CSRF secret (zero and never accepted for bearer tokens).
    csrf: [u8; 32],
}

impl fmt::Debug for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Principal")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoginError {
    NoPassword,
    BadPassword,
    RateLimited { retry_after: Duration },
    Entropy,
}

/// What a long-lived stream learns when it re-checks its session.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionCheck {
    Live { remaining: Duration },
    Expired,
    Revoked,
}

struct Inner {
    table: SessionTable,
    limiter: LoginLimiter,
}

/// Gets the ids of the sessions just revoked.
type RevokeHook = Box<dyn Fn(&[SessionId]) + Send + Sync>;

pub struct AuthState {
    clock: Arc<dyn Clock>,
    rng: Box<dyn TokenRng>,
    cfg: AuthConfig,
    /// SHA-256 of the agent password. Set once at startup, so `has_password`
    /// never takes a lock.
    password: OnceLock<[u8; 32]>,
    inner: Mutex<Inner>,
    /// Bumped whenever sessions are revoked (logout, rotation, eviction,
    /// `revoke_all`), never by expiry; tests count revocations with it.
    #[cfg(test)]
    epoch: AtomicU64,
    revoke_hook: OnceLock<RevokeHook>,
    /// Per-process HMAC key for login form tokens, filled on first use.
    form_key: OnceLock<[u8; 32]>,
}

impl AuthState {
    /// Boot clock, kernel RNG, default policies.
    pub fn new() -> Self {
        Self::with_parts(Arc::new(BootClock), Box::new(OsRng), AuthConfig::default())
    }

    pub fn with_parts(clock: Arc<dyn Clock>, rng: Box<dyn TokenRng>, cfg: AuthConfig) -> Self {
        let now = clock.now();
        AuthState {
            clock,
            rng,
            cfg,
            password: OnceLock::new(),
            inner: Mutex::new(Inner {
                table: SessionTable::new(&cfg),
                limiter: LoginLimiter::new(cfg.limiter, now),
            }),
            #[cfg(test)]
            epoch: AtomicU64::new(0),
            revoke_hook: OnceLock::new(),
            form_key: OnceLock::new(),
        }
    }

    /// Callers read the clock only after this returns, so the times stored
    /// under the lock never go backwards when two requests race for it.
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Set the agent password (once, at startup). An empty one is no
    /// password: the web UI then explains how to set one instead of showing
    /// a login form anyone could pass. Changing it needs a restart, which
    /// also ends every session (they live in RAM only).
    pub fn set_password(&self, password: &str) {
        if password.is_empty() {
            eprintln!("[auth] ZTE_AGENT_PASSWORD is empty: running without an agent password");
            return;
        }
        let hash: [u8; 32] = Sha256::digest(password.as_bytes()).into();
        if self.password.set(hash).is_err() {
            eprintln!("[auth] password already set; ignoring the second call");
        }
    }

    pub fn has_password(&self) -> bool {
        self.password.get().is_some()
    }

    /// Log in with the agent password. Browser logins first remove every
    /// session named in `replacing` (all presented session cookies), which
    /// rotates the session; bearer logins never revoke other bearer tokens
    /// (Live Activity, LAN test and several phones each hold one).
    pub fn login(
        &self,
        password: &str,
        kind: Kind,
        peer: PeerKey,
        replacing: &[&str],
    ) -> Result<Issued, LoginError> {
        // Entropy and hashing outside the lock.
        let mut token = [0u8; 32];
        self.rng.fill(&mut token).map_err(|_| LoginError::Entropy)?;
        let mut csrf = [0u8; 32];
        if kind == Kind::Browser {
            self.rng.fill(&mut csrf).map_err(|_| LoginError::Entropy)?;
        }
        let presented: [u8; 32] = Sha256::digest(password.as_bytes()).into();
        let token_digest = digest(kind, &token);
        let replaced: Vec<[u8; 32]> = match kind {
            Kind::Browser => replacing
                .iter()
                .filter_map(|t| parse_hex(t))
                .map(|t| digest(Kind::Browser, &t))
                .collect(),
            Kind::Bearer => Vec::new(),
        };

        let (id, revoked) = {
            let mut inner = self.inner();
            let now = self.clock.now();
            inner.table.prune(now, &self.cfg);
            if let Admit::Deny { retry_after } = inner.limiter.check(peer, now) {
                return Err(LoginError::RateLimited { retry_after });
            }
            let Some(stored) = self.password.get() else {
                return Err(LoginError::NoPassword);
            };
            if !bool::from(presented.ct_eq(stored)) {
                let note = inner.limiter.record_failure(peer, now);
                drop(inner);
                if let Some(line) = note {
                    eprintln!("{line}");
                }
                return Err(LoginError::BadPassword);
            }
            inner.limiter.record_success(peer);
            let mut revoked = Vec::new();
            for d in &replaced {
                revoked.extend(inner.table.remove_digest(Kind::Browser, d));
            }
            let id = inner.table.alloc_id();
            let entry = Entry {
                id,
                kind,
                digest: token_digest,
                csrf,
                created: now,
                last_used: now,
            };
            revoked.extend(inner.table.insert(entry, &self.cfg));
            (id, revoked)
        };
        self.revoked(&revoked);
        Ok(Issued {
            id,
            token: Secret(hex(&token)),
            csrf: (kind == Kind::Browser).then(|| Secret(hex(&csrf))),
            max_age: self.cfg.policy(kind).absolute,
        })
    }

    /// Check the agent password again for a confirmation that needs it (the
    /// web UI's reboot): compared in constant time and counted by the same
    /// rate limit as a login (a wrong one is a failure of `peer`). Nothing
    /// is issued.
    pub fn check_password(&self, password: &str, peer: PeerKey) -> Result<(), LoginError> {
        let presented: [u8; 32] = Sha256::digest(password.as_bytes()).into();
        let mut inner = self.inner();
        let now = self.clock.now();
        if let Admit::Deny { retry_after } = inner.limiter.check(peer, now) {
            return Err(LoginError::RateLimited { retry_after });
        }
        let Some(stored) = self.password.get() else {
            return Err(LoginError::NoPassword);
        };
        if !bool::from(presented.ct_eq(stored)) {
            let note = inner.limiter.record_failure(peer, now);
            drop(inner);
            if let Some(line) = note {
                eprintln!("{line}");
            }
            return Err(LoginError::BadPassword);
        }
        inner.limiter.record_success(peer);
        Ok(())
    }

    /// The hook (and in tests the epoch) for `ids`, if any. Never called
    /// with `inner` held: the hook takes other locks (the event hub's) and
    /// may call back into auth.
    fn revoked(&self, ids: &[SessionId]) {
        if ids.is_empty() {
            return;
        }
        #[cfg(test)]
        self.epoch.fetch_add(1, Ordering::SeqCst);
        if let Some(hook) = self.revoke_hook.get() {
            hook(ids);
        }
    }

    /// The live session this token belongs to; slides its idle deadline.
    pub fn authenticate(&self, kind: Kind, token: &str) -> Option<Principal> {
        let token = parse_hex::<32>(token)?;
        let d = digest(kind, &token);
        let mut inner = self.inner();
        let now = self.clock.now();
        inner.table.prune(now, &self.cfg);
        let e = inner.table.find(kind, &d)?;
        e.last_used = now;
        Some(Principal {
            id: e.id,
            kind,
            csrf: e.csrf,
        })
    }

    /// Bearer check (the listener uses `authenticate` to keep the principal).
    #[cfg(test)]
    pub fn validate(&self, token: &str) -> bool {
        self.authenticate(Kind::Bearer, token).is_some()
    }

    /// The form's CSRF value against the session's secret (browser sessions only).
    pub fn check_csrf(&self, p: &Principal, presented: &str) -> bool {
        p.kind == Kind::Browser
            && parse_hex::<32>(presented).is_some_and(|b| bool::from(b.ct_eq(&p.csrf)))
    }

    /// The session's CSRF value for a hidden form field.
    pub fn csrf_token(&self, p: &Principal) -> String {
        hex(&p.csrf)
    }

    // Only tests call it today (web logout ends a session by id: `logout_id`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn logout(&self, kind: Kind, token: &str) -> bool {
        let Some(token) = parse_hex::<32>(token) else {
            return false;
        };
        let d = digest(kind, &token);
        let removed = {
            let mut inner = self.inner();
            inner.table.prune(self.clock.now(), &self.cfg);
            inner.table.remove_digest(kind, &d)
        };
        self.revoked(removed.as_slice());
        removed.is_some()
    }

    pub fn logout_id(&self, id: SessionId) -> bool {
        let removed = {
            let mut inner = self.inner();
            inner.table.prune(self.clock.now(), &self.cfg);
            inner.table.remove_id(id)
        };
        if removed {
            self.revoked(&[id]);
        }
        removed
    }

    // For a password change (no such route yet); only tests call it today.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn revoke_all(&self) -> usize {
        let ids = self.inner().table.clear();
        self.revoked(&ids);
        ids.len()
    }

    /// For open streams: slide the idle deadline and report the time left.
    /// The lookup happens before pruning, so a session past its deadline
    /// reports `Expired` (and is then removed); a missing id is `Expired` if
    /// it was recently pruned by any caller, otherwise `Revoked`.
    pub fn touch_session(&self, id: SessionId) -> SessionCheck {
        let mut guard = self.inner();
        let now = self.clock.now();
        let inner = &mut *guard;
        let check = if let Some(e) = inner.table.by_id(id) {
            if now >= e.deadline(&self.cfg) {
                SessionCheck::Expired
            } else {
                e.last_used = now;
                SessionCheck::Live {
                    remaining: e.deadline(&self.cfg).saturating_duration_since(now),
                }
            }
        } else if inner.table.recently_expired(id) {
            SessionCheck::Expired
        } else {
            SessionCheck::Revoked
        };
        inner.table.prune(now, &self.cfg);
        check
    }

    #[cfg(test)]
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Called after every revocation with the revoked ids, outside the auth
    /// lock (the event hub ends their streams).
    pub fn set_revoke_hook(&self, f: RevokeHook) {
        if self.revoke_hook.set(f).is_err() {
            eprintln!("[auth] revoke hook already set; ignoring the second call");
        }
    }

    /// Value for the pre-login cookie the login form token is bound to.
    pub fn new_pre_token(&self) -> Result<Secret, LoginError> {
        self.random_hex(32).map(Secret)
    }

    fn form_key(&self) -> Result<&[u8; 32], LoginError> {
        if let Some(key) = self.form_key.get() {
            return Ok(key);
        }
        let mut key = [0u8; 32];
        self.rng.fill(&mut key).map_err(|_| LoginError::Entropy)?;
        // A lost race keeps the winner's key; both callers then use it.
        let _ = self.form_key.set(key);
        self.form_key.get().ok_or(LoginError::Entropy)
    }

    /// `"<issued>.<32 hex>"`: an HMAC over the pre-login cookie and the issue
    /// time, so the login form needs no server-side state. `issued` is the
    /// boot time in seconds plus a per-process offset (`form_offset`), so the
    /// page does not tell anyone on the LAN when the router booted.
    pub fn login_form_token(&self, pre: &str) -> Result<String, LoginError> {
        // A malformed pre value would get a token that never verifies.
        debug_assert!(parse_hex::<32>(pre).is_some(), "login_form_token: malformed pre value");
        let key = self.form_key()?;
        let issued = self.clock.now().as_secs();
        let mac = form_mac(key, pre, issued).ok_or(LoginError::Entropy)?;
        Ok(format!("{}.{}", issued + form_offset(key), hex(&mac.finalize().into_bytes()[..16])))
    }

    /// False for a malformed pre value or token, a token older than
    /// `login_form_ttl`, or before any form token was issued by this process.
    pub fn verify_login_form_token(&self, pre: &str, token: &str) -> bool {
        let Some(key) = self.form_key.get() else {
            return false;
        };
        if parse_hex::<32>(pre).is_none() {
            return false;
        }
        let Some((issued, tag)) = self.form_token_parts(key, token) else {
            return false;
        };
        form_mac(key, pre, issued).is_some_and(|m| m.verify_truncated_left(&tag).is_ok())
    }

    /// A well-formed form token within `login_form_ttl`, its MAC unchecked
    /// (that needs the pre-login cookie). Only for telling an expired form
    /// from blocked cookies when the cookie is missing.
    pub fn login_form_token_fresh(&self, token: &str) -> bool {
        self.form_key.get().is_some_and(|key| self.form_token_parts(key, token).is_some())
    }

    /// The boot-time issue time and the MAC of a well-formed token that is
    /// not older than `login_form_ttl`.
    fn form_token_parts(&self, key: &[u8; 32], token: &str) -> Option<(u64, [u8; 16])> {
        let (secs, tag) = token.split_once('.')?;
        // Canonical decimal only: the MAC covers the value, not the text.
        let canonical = !secs.is_empty()
            && secs.len() <= 20
            && secs.bytes().all(|b| b.is_ascii_digit())
            && (secs == "0" || !secs.starts_with('0'));
        if !canonical {
            return None;
        }
        let issued = secs.parse::<u64>().ok()?.checked_sub(form_offset(key))?;
        let tag = parse_hex::<16>(tag)?;
        let now = self.clock.now().as_secs();
        (issued <= now && now - issued <= self.cfg.login_form_ttl.as_secs()).then_some((issued, tag))
    }

    /// An opaque key for `value` (16 hex: an HMAC with this process's random
    /// key, separate per `domain`), for web UI URLs that must not carry the
    /// value itself, such as an SMS conversation's number. Stable until the
    /// agent restarts; `None` without randomness.
    pub fn opaque_key(&self, domain: &str, value: &str) -> Option<String> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(self.form_key().ok()?).ok()?;
        mac.update(b"zte-ui-key\0");
        mac.update(domain.as_bytes());
        mac.update(b"\0");
        mac.update(value.as_bytes());
        Some(hex(&mac.finalize().into_bytes()[..8]))
    }

    /// `bytes` random bytes as lowercase hex (CSP nonces, operation ids).
    pub fn random_hex(&self, bytes: usize) -> Result<String, LoginError> {
        let mut buf = vec![0u8; bytes];
        self.rng.fill(&mut buf).map_err(|_| LoginError::Entropy)?;
        Ok(hex(&buf))
    }

    pub fn now(&self) -> BootInstant {
        self.clock.now()
    }

    /// Leaves `inner` poisoned, as a panic in another request would.
    #[cfg(test)]
    pub fn poison_for_test(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = self.inner.lock();
            panic!("poisoning the auth lock for a test");
        }));
        assert!(self.inner.is_poisoned());
    }
}

fn digest(kind: Kind, token: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(kind.tag());
    h.update(token);
    h.finalize().into()
}

fn form_mac(key: &[u8; 32], pre: &str, issued: u64) -> Option<Hmac<Sha256>> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).ok()?;
    mac.update(b"zte-ui-login\0");
    mac.update(pre.as_bytes());
    mac.update(&issued.to_be_bytes());
    Some(mac)
}

/// Added to the boot time shown in login form tokens: 32 bits derived from
/// the form key, so fixed for the process and unknown outside it.
fn form_offset(key: &[u8; 32]) -> u64 {
    let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(key) else {
        return 0;
    };
    mac.update(b"zte-ui-login-offset\0");
    let b = mac.finalize().into_bytes();
    u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn hex(bytes: &[u8]) -> String {
    euicc::hex::encode(bytes)
}

/// Exactly `2 * N` lowercase hex digits; anything else (including uppercase)
/// is rejected before any hashing.
fn parse_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    }
    let s = s.as_bytes();
    if s.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (o, pair) in out.iter_mut().zip(s.chunks_exact(2)) {
        *o = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::rng::{EntropyUnavailable, FailRng, SeqRng};
    use super::*;
    use crate::boottime::FakeClock;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::{mpsc, Weak};

    const PW: &str = "correct horse";
    const MIN: Duration = Duration::from_secs(60);
    const HOUR: Duration = Duration::from_secs(3600);

    fn peer(test: u8, n: u8) -> PeerKey {
        PeerKey::from(SocketAddr::from(([10, 0, test, n], 5000)))
    }

    fn auth_with(cfg: AuthConfig) -> (Arc<FakeClock>, AuthState) {
        let clock = FakeClock::at_secs(1000);
        let auth = AuthState::with_parts(clock.clone(), Box::new(SeqRng::new()), cfg);
        auth.set_password(PW);
        (clock, auth)
    }

    fn auth() -> (Arc<FakeClock>, AuthState) {
        auth_with(AuthConfig::default())
    }

    fn login(auth: &AuthState, kind: Kind, p: PeerKey) -> Issued {
        auth.login(PW, kind, p, &[]).expect("login")
    }

    fn browser(auth: &AuthState, token: &Issued) -> Principal {
        auth.authenticate(Kind::Browser, token.token.expose()).expect("browser session")
    }

    fn is_lower_hex(s: &str, len: usize) -> bool {
        s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }

    // --- tokens and sessions ---

    /// A password confirmation counts like a login: right is Ok (nothing
    /// issued), wrong is a failure, and enough of them are rate limited,
    /// even for the right password; a login from the same peer is too.
    #[test]
    fn check_password_shares_the_login_limit() {
        let (_, a) = auth();
        let p = peer(36, 1);
        assert_eq!(a.check_password(PW, p), Ok(()));
        let mut limited = false;
        for _ in 0..50 {
            match a.check_password("wrong", p) {
                Err(LoginError::BadPassword) => {}
                Err(LoginError::RateLimited { .. }) => {
                    limited = true;
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(limited);
        assert!(matches!(a.check_password(PW, p), Err(LoginError::RateLimited { .. })));
        assert!(matches!(a.login(PW, Kind::Browser, p, &[]), Err(LoginError::RateLimited { .. })));
        let (_, none) = auth_with(AuthConfig::default());
        assert_eq!(none.check_password("", peer(36, 2)), Err(LoginError::BadPassword));
    }


    #[test]
    fn token_is_64_lower_hex_from_rng() {
        let (_, a) = auth();
        let t = login(&a, Kind::Bearer, peer(1, 1));
        assert!(is_lower_hex(t.token.expose(), 64));
        assert!(t.csrf.is_none());
        // SeqRng leads with the call number: the token is the RNG's output.
        assert!(t.token.expose().starts_with("0000000000000001"));
        assert!(a.validate(t.token.expose()));
        let b = login(&a, Kind::Browser, peer(1, 1));
        assert!(is_lower_hex(b.csrf.as_ref().unwrap().expose(), 64));
        assert_eq!(b.max_age, Duration::from_secs(604_800));
    }

    #[test]
    fn os_rng_tokens_are_random() {
        let a = AuthState::new();
        a.set_password(PW);
        let t1 = login(&a, Kind::Bearer, peer(2, 1));
        let t2 = login(&a, Kind::Bearer, peer(2, 1));
        assert!(is_lower_hex(t1.token.expose(), 64));
        assert_ne!(t1.token.expose(), t2.token.expose());
        assert!(a.validate(t1.token.expose()) && a.validate(t2.token.expose()));
    }

    /// Opaque keys: 16 hex, stable in a process, apart per domain and per
    /// process, never containing the value; none without randomness.
    #[test]
    fn opaque_keys() {
        let (_, a) = auth();
        let k = a.opaque_key("sms", "55550100").unwrap();
        assert!(is_lower_hex(&k, 16) && !k.contains("5555"));
        assert_eq!(a.opaque_key("sms", "55550100").unwrap(), k);
        assert_ne!(a.opaque_key("sms", "55550101").unwrap(), k);
        assert_ne!(a.opaque_key("esim", "55550100").unwrap(), k);
        assert_ne!(AuthState::new().opaque_key("sms", "55550100").unwrap(), k);
        let failing = AuthState::with_parts(FakeClock::at_secs(5), Box::new(FailRng), AuthConfig::default());
        assert_eq!(failing.opaque_key("sms", "55550100"), None);
    }

    #[test]
    fn consecutive_logins_distinct() {
        let (_, a) = auth();
        let tokens: Vec<String> =
            (0..10).map(|_| login(&a, Kind::Bearer, peer(3, 1)).token.expose().to_owned()).collect();
        for (i, t) in tokens.iter().enumerate() {
            assert!(tokens[i + 1..].iter().all(|u| u != t));
            assert!(a.validate(t));
        }
    }

    #[test]
    fn entropy_failure_is_error_and_inserts_nothing() {
        let a = AuthState::with_parts(FakeClock::at_secs(5), Box::new(FailRng), AuthConfig::default());
        a.set_password(PW);
        assert_eq!(a.login(PW, Kind::Bearer, peer(4, 1), &[]).unwrap_err(), LoginError::Entropy);
        assert_eq!(a.login(PW, Kind::Browser, peer(4, 1), &[]).unwrap_err(), LoginError::Entropy);
        let inner = a.inner();
        assert_eq!(inner.table.len(Kind::Bearer) + inner.table.len(Kind::Browser), 0);
        drop(inner);
        assert!(a.random_hex(16).is_err());
    }

    /// RNG that records whether it was ever called while `inner` was held.
    struct ProbeRng {
        auth: Arc<OnceLock<Weak<AuthState>>>,
        calls: Arc<AtomicUsize>,
        under_lock: Arc<AtomicBool>,
        seq: SeqRng,
    }

    impl TokenRng for ProbeRng {
        fn fill(&self, buf: &mut [u8]) -> Result<(), EntropyUnavailable> {
            if let Some(a) = self.auth.get().and_then(Weak::upgrade) {
                if a.inner.try_lock().is_err() {
                    self.under_lock.store(true, Ordering::SeqCst);
                }
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seq.fill(buf)
        }
    }

    #[test]
    fn rng_never_called_under_inner_lock() {
        let slot = Arc::new(OnceLock::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let under_lock = Arc::new(AtomicBool::new(false));
        let rng = ProbeRng {
            auth: slot.clone(),
            calls: calls.clone(),
            under_lock: under_lock.clone(),
            seq: SeqRng::new(),
        };
        let a = Arc::new(AuthState::with_parts(FakeClock::at_secs(5), Box::new(rng), AuthConfig::default()));
        assert!(slot.set(Arc::downgrade(&a)).is_ok());
        a.set_password(PW);
        let s1 = login(&a, Kind::Browser, peer(5, 1));
        let _ = a.login(PW, Kind::Browser, peer(5, 1), &[s1.token.expose()]).unwrap();
        let _ = login(&a, Kind::Bearer, peer(5, 1));
        let _ = a.login("wrong", Kind::Bearer, peer(5, 1), &[]);
        let pre = a.new_pre_token().unwrap();
        let _ = a.login_form_token(pre.expose()).unwrap();
        let _ = a.random_hex(16).unwrap();
        assert!(calls.load(Ordering::SeqCst) >= 7);
        assert!(!under_lock.load(Ordering::SeqCst), "RNG drawn while holding the auth lock");
    }

    #[test]
    fn poisoned_inner_lock_still_serves() {
        let (_, a) = auth();
        let t = login(&a, Kind::Bearer, peer(6, 1));
        a.poison_for_test();
        assert!(a.has_password());
        assert!(a.validate(t.token.expose()));
        let t2 = login(&a, Kind::Bearer, peer(6, 1));
        assert!(a.validate(t2.token.expose()));
        assert_eq!(a.login("wrong", Kind::Bearer, peer(6, 1), &[]).unwrap_err(), LoginError::BadPassword);
    }

    #[test]
    fn has_password_lock_free() {
        let (_, a) = auth();
        let a = Arc::new(a);
        let held = a.inner();
        let (tx, rx) = mpsc::channel();
        let a2 = Arc::clone(&a);
        std::thread::spawn(move || {
            let _ = tx.send(a2.has_password());
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true), "has_password waited on the auth lock");
        drop(held);
    }

    #[test]
    fn set_password_second_call_ignored() {
        let (_, a) = auth();
        a.set_password("second");
        assert!(a.login(PW, Kind::Bearer, peer(7, 1), &[]).is_ok());
        assert_eq!(a.login("second", Kind::Bearer, peer(7, 1), &[]).unwrap_err(), LoginError::BadPassword);
    }

    #[test]
    fn empty_password_is_no_password() {
        let a = AuthState::with_parts(FakeClock::at_secs(5), Box::new(SeqRng::new()), AuthConfig::default());
        a.set_password("");
        assert!(!a.has_password());
        assert_eq!(a.login("", Kind::Bearer, peer(8, 1), &[]).unwrap_err(), LoginError::NoPassword);
        assert_eq!(a.login("", Kind::Browser, peer(8, 1), &[]).unwrap_err(), LoginError::NoPassword);
        // A real password can still be set afterwards (nothing was stored).
        a.set_password("x");
        assert!(a.login("x", Kind::Bearer, peer(8, 1), &[]).is_ok());
    }

    #[test]
    fn bearer_idle_expiry_on_boottime() {
        let (clock, a) = auth();
        let t1 = login(&a, Kind::Bearer, peer(9, 1));
        let t2 = login(&a, Kind::Bearer, peer(9, 1));
        clock.advance(HOUR - Duration::from_secs(1));
        assert!(a.validate(t1.token.expose()));
        clock.advance(Duration::from_secs(1));
        // t2 was idle for exactly 1 h; t1 was used a second ago.
        assert!(!a.validate(t2.token.expose()));
        assert!(a.validate(t1.token.expose()));
    }

    #[test]
    fn touch_slides_idle_not_absolute() {
        let (clock, a) = auth();
        let t = login(&a, Kind::Bearer, peer(10, 1));
        // Used every 59 min: alive until the 24 h absolute limit.
        let step = 59 * MIN;
        let mut elapsed = Duration::ZERO;
        while elapsed + step < 24 * HOUR {
            clock.advance(step);
            elapsed += step;
            assert!(a.validate(t.token.expose()), "expired early at {elapsed:?}");
        }
        clock.advance(24 * HOUR - elapsed);
        assert!(!a.validate(t.token.expose()));

        // Browser: touched every 11 h, gone at 7 days.
        let b = login(&a, Kind::Browser, peer(10, 2));
        let id = b.id;
        let mut elapsed = Duration::ZERO;
        while elapsed + 11 * HOUR < 7 * 24 * HOUR {
            clock.advance(11 * HOUR);
            elapsed += 11 * HOUR;
            assert!(matches!(a.touch_session(id), SessionCheck::Live { .. }));
        }
        clock.advance(7 * 24 * HOUR - elapsed);
        assert_eq!(a.touch_session(id), SessionCheck::Expired);
        assert!(a.authenticate(Kind::Browser, b.token.expose()).is_none());
    }

    #[test]
    fn expiry_counts_suspended_time() {
        // The boot clock keeps counting through suspend: one jump of 2 h
        // (no wakeups in between) still expires an idle bearer token.
        let (clock, a) = auth();
        let t = login(&a, Kind::Bearer, peer(11, 1));
        clock.advance(2 * HOUR);
        assert!(!a.validate(t.token.expose()));
    }

    #[test]
    fn bearer_token_rejected_as_cookie_and_vice_versa() {
        let (_, a) = auth();
        let bearer = login(&a, Kind::Bearer, peer(12, 1));
        let cookie = login(&a, Kind::Browser, peer(12, 1));
        assert!(a.authenticate(Kind::Browser, bearer.token.expose()).is_none());
        assert!(!a.validate(cookie.token.expose()));
        assert!(a.validate(bearer.token.expose()));
        assert!(a.authenticate(Kind::Browser, cookie.token.expose()).is_some());
    }

    #[test]
    fn malformed_tokens_rejected() {
        let (_, a) = auth();
        let t = login(&a, Kind::Bearer, peer(13, 1));
        let good = t.token.expose();
        let bad = [
            String::new(),
            good[..32].to_owned(),
            good[..63].to_owned(),
            format!("{good}0"),
            format!("{}g", &good[..63]),
            format!("{} ", &good[..63]),
            good.to_ascii_uppercase(),
        ];
        for b in &bad {
            assert!(!a.validate(b), "accepted {b:?}");
            assert!(a.authenticate(Kind::Browser, b).is_none());
        }
        assert!(a.validate(good));
    }

    #[test]
    fn cap_per_kind_lru_evicted_epoch_bumped() {
        let (clock, a) = auth();
        let cookie = login(&a, Kind::Browser, peer(14, 1));
        let tokens: Vec<Issued> = (0..32)
            .map(|_| {
                clock.advance(Duration::from_secs(1));
                login(&a, Kind::Bearer, peer(14, 1))
            })
            .collect();
        assert_eq!(a.epoch(), 0);
        // Use the oldest, so the second-oldest becomes least recently used.
        clock.advance(Duration::from_secs(1));
        assert!(a.validate(tokens[0].token.expose()));
        let newest = login(&a, Kind::Bearer, peer(14, 1));
        assert_eq!(a.epoch(), 1);
        assert!(!a.validate(tokens[1].token.expose()));
        assert!(a.validate(tokens[0].token.expose()));
        assert!(tokens[2..].iter().all(|t| a.validate(t.token.expose())));
        assert!(a.validate(newest.token.expose()));
        assert_eq!(a.inner().table.len(Kind::Bearer), 32);
        // The other kind is not touched.
        assert!(a.authenticate(Kind::Browser, cookie.token.expose()).is_some());

        // Browser sessions have their own cap (16): `cookie`, just used above,
        // is now their least recently used one.
        let cookies: Vec<Issued> = (0..15)
            .map(|_| {
                clock.advance(Duration::from_secs(1));
                login(&a, Kind::Browser, peer(14, 2))
            })
            .collect();
        assert_eq!(a.epoch(), 1);
        clock.advance(Duration::from_secs(1));
        let newest_cookie = login(&a, Kind::Browser, peer(14, 2));
        assert_eq!(a.epoch(), 2);
        assert!(a.authenticate(Kind::Browser, cookie.token.expose()).is_none());
        assert!(cookies.iter().chain([&newest_cookie]).all(|c| a.authenticate(Kind::Browser, c.token.expose()).is_some()));
        assert_eq!(a.inner().table.len(Kind::Browser), 16);
        // Bearer tokens are not touched.
        assert_eq!(a.inner().table.len(Kind::Bearer), 32);
        assert!(a.validate(tokens[0].token.expose()) && a.validate(newest.token.expose()));
        assert!(tokens[2..].iter().all(|t| a.validate(t.token.expose())));
    }

    #[test]
    fn expired_pruned_before_eviction() {
        let cfg = AuthConfig {
            bearer: Policy { idle: HOUR, absolute: 24 * HOUR, cap: 3 },
            ..AuthConfig::default()
        };
        let (clock, a) = auth_with(cfg);
        let first = login(&a, Kind::Bearer, peer(15, 1));
        clock.advance(30 * MIN);
        let second = login(&a, Kind::Bearer, peer(15, 1));
        let third = login(&a, Kind::Bearer, peer(15, 1));
        clock.advance(31 * MIN);
        // `first` is past its idle deadline: pruned, so nothing is evicted.
        let fourth = login(&a, Kind::Bearer, peer(15, 1));
        assert_eq!(a.epoch(), 0);
        assert!(!a.validate(first.token.expose()));
        for t in [&second, &third, &fourth] {
            assert!(a.validate(t.token.expose()));
        }
    }

    #[test]
    fn bearer_login_keeps_older_tokens() {
        let (_, a) = auth();
        let t1 = login(&a, Kind::Bearer, peer(16, 1));
        // `replacing` is ignored for bearer logins.
        let t2 = a.login(PW, Kind::Bearer, peer(16, 1), &[t1.token.expose()]).unwrap();
        let t3 = login(&a, Kind::Bearer, peer(16, 2));
        for t in [&t1, &t2, &t3] {
            assert!(a.validate(t.token.expose()));
        }
        assert_eq!(a.epoch(), 0);
    }

    #[test]
    fn browser_login_rotates_every_presented_cookie() {
        let (_, a) = auth();
        let s1 = login(&a, Kind::Browser, peer(17, 1));
        let s2 = login(&a, Kind::Browser, peer(17, 1));
        let s3 = login(&a, Kind::Browser, peer(17, 1));
        let bearer = login(&a, Kind::Bearer, peer(17, 1));
        let new = a
            .login(PW, Kind::Browser, peer(17, 1), &[s1.token.expose(), "not-a-token", s2.token.expose(), bearer.token.expose()])
            .unwrap();
        assert!(a.authenticate(Kind::Browser, s1.token.expose()).is_none());
        assert!(a.authenticate(Kind::Browser, s2.token.expose()).is_none());
        assert!(a.authenticate(Kind::Browser, s3.token.expose()).is_some());
        assert!(a.authenticate(Kind::Browser, new.token.expose()).is_some());
        assert!(a.validate(bearer.token.expose()));
        assert_eq!(a.epoch(), 1);
        // A failed login rotates nothing.
        assert!(a.login("wrong", Kind::Browser, peer(17, 2), &[s3.token.expose()]).is_err());
        assert!(a.authenticate(Kind::Browser, s3.token.expose()).is_some());
    }

    #[test]
    fn logout_revokes_only_that_session() {
        let (_, a) = auth();
        let b1 = login(&a, Kind::Browser, peer(18, 1));
        let b2 = login(&a, Kind::Browser, peer(18, 1));
        let t = login(&a, Kind::Bearer, peer(18, 1));
        // The kind must match.
        assert!(!a.logout(Kind::Bearer, b1.token.expose()));
        assert!(a.logout(Kind::Browser, b1.token.expose()));
        assert_eq!(a.epoch(), 1);
        assert!(!a.logout(Kind::Browser, b1.token.expose()));
        assert!(!a.logout(Kind::Browser, "zz"));
        assert_eq!(a.epoch(), 1);
        assert!(a.authenticate(Kind::Browser, b1.token.expose()).is_none());
        assert!(a.authenticate(Kind::Browser, b2.token.expose()).is_some());
        assert!(a.validate(t.token.expose()));
    }

    #[test]
    fn logout_id() {
        let (_, a) = auth();
        let b1 = login(&a, Kind::Browser, peer(19, 1));
        let b2 = login(&a, Kind::Browser, peer(19, 1));
        assert!(a.logout_id(b1.id));
        assert!(!a.logout_id(b1.id));
        assert_eq!(a.epoch(), 1);
        assert!(a.authenticate(Kind::Browser, b1.token.expose()).is_none());
        assert!(a.authenticate(Kind::Browser, b2.token.expose()).is_some());
        assert_eq!(a.revoke_all(), 1);
        assert_eq!(a.epoch(), 2);
        assert!(a.authenticate(Kind::Browser, b2.token.expose()).is_none());
    }

    #[test]
    fn revoke_hook_runs_after_unlock() {
        let (_, a) = auth();
        let a = Arc::new(a);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let weak = Arc::downgrade(&a);
        let seen = calls.clone();
        a.set_revoke_hook(Box::new(move |ids| {
            // Re-entering auth would deadlock if the hook ran under `inner`.
            if let Some(a) = weak.upgrade() {
                let _ = a.validate(&"0".repeat(64));
                let _ = a.epoch();
            }
            seen.lock().unwrap().push(ids.to_vec());
        }));
        let (tx, rx) = mpsc::channel();
        let a2 = Arc::clone(&a);
        std::thread::spawn(move || {
            let s1 = login(&a2, Kind::Browser, peer(20, 1));
            let s2 = a2.login(PW, Kind::Browser, peer(20, 1), &[s1.token.expose()]).unwrap();
            assert!(a2.logout_id(s2.id));
            let _ = tx.send((s1.id, s2.id));
        });
        let (s1, s2) = rx.recv_timeout(Duration::from_secs(5)).expect("revoke hook deadlocked");
        // The hook names exactly the revoked sessions.
        assert_eq!(*calls.lock().unwrap(), [vec![s1], vec![s2]]);
        assert_eq!(a.epoch(), 2);
        let s3 = login(&a, Kind::Bearer, peer(20, 2));
        let s4 = login(&a, Kind::Bearer, peer(20, 3));
        assert_eq!(a.revoke_all(), 2);
        assert_eq!(calls.lock().unwrap().last().unwrap().len(), 2);
        assert!([s3.id, s4.id].iter().all(|id| calls.lock().unwrap().last().unwrap().contains(id)));
    }

    #[test]
    fn no_password_login_is_no_password() {
        let a = AuthState::with_parts(FakeClock::at_secs(5), Box::new(SeqRng::new()), AuthConfig::default());
        assert!(!a.has_password());
        assert_eq!(a.login("anything", Kind::Bearer, peer(21, 1), &[]).unwrap_err(), LoginError::NoPassword);
        // Not recorded as failures.
        for _ in 0..10 {
            assert_eq!(a.login("x", Kind::Bearer, peer(21, 1), &[]).unwrap_err(), LoginError::NoPassword);
        }
    }

    #[test]
    fn secret_debug_redacted() {
        let (_, a) = auth();
        let t = login(&a, Kind::Browser, peer(22, 1));
        let p = browser(&a, &t);
        let shown = format!("{t:?} {:?} {p:?}", t.token);
        assert!(!shown.contains(t.token.expose()));
        assert!(!shown.contains(t.csrf.as_ref().unwrap().expose()));
        assert!(!shown.contains(&a.csrf_token(&p)));
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn touch_session_reports_expired_vs_revoked() {
        let (clock, a) = auth();
        let live = login(&a, Kind::Browser, peer(23, 1));
        assert_eq!(a.touch_session(live.id), SessionCheck::Live { remaining: 12 * HOUR });
        clock.advance(HOUR);
        assert_eq!(a.touch_session(live.id), SessionCheck::Live { remaining: 12 * HOUR });
        let gone = login(&a, Kind::Browser, peer(23, 1));
        assert!(a.logout_id(gone.id));
        assert_eq!(a.touch_session(gone.id), SessionCheck::Revoked);
        clock.advance(12 * HOUR);
        assert_eq!(a.touch_session(live.id), SessionCheck::Expired);
        // Removed now; still reported as expired.
        assert_eq!(a.touch_session(live.id), SessionCheck::Expired);
        // Bearer sessions too, with their own deadline.
        let t = login(&a, Kind::Bearer, peer(23, 1));
        let p = a.authenticate(Kind::Bearer, t.token.expose()).unwrap();
        assert_eq!(a.touch_session(p.id), SessionCheck::Live { remaining: HOUR });
        // Near the absolute limit, remaining is capped by it.
        let b = login(&a, Kind::Browser, peer(23, 1));
        for _ in 0..16 {
            clock.advance(10 * HOUR);
            assert!(matches!(a.touch_session(b.id), SessionCheck::Live { .. }));
        }
        assert_eq!(a.touch_session(b.id), SessionCheck::Live { remaining: 8 * HOUR });
    }

    #[test]
    fn touch_session_expired_even_if_pruned_by_another_call() {
        let (clock, a) = auth();
        let s = login(&a, Kind::Browser, peer(24, 1));
        clock.advance(13 * HOUR);
        // Another login prunes the expired entry first.
        let _ = login(&a, Kind::Bearer, peer(24, 1));
        assert_eq!(a.touch_session(s.id), SessionCheck::Expired);
    }

    #[test]
    fn touch_session_expired_after_mass_prune() {
        // After a long suspend one prune removes every session at once; each
        // must still report `Expired`, not `Revoked`.
        let (clock, a) = auth();
        let ids: Vec<SessionId> = (0..32)
            .map(|_| login(&a, Kind::Bearer, peer(28, 1)).id)
            .chain((0..16).map(|_| login(&a, Kind::Browser, peer(28, 1)).id))
            .collect();
        clock.advance(8 * 24 * HOUR);
        assert!(!a.validate(&"0".repeat(64)));
        for id in ids {
            assert_eq!(a.touch_session(id), SessionCheck::Expired);
        }
    }

    // --- CSRF ---

    #[test]
    fn csrf_own_session_only() {
        let (_, a) = auth();
        let s1 = login(&a, Kind::Browser, peer(25, 1));
        let s2 = login(&a, Kind::Browser, peer(25, 1));
        let p1 = browser(&a, &s1);
        let p2 = browser(&a, &s2);
        let c1 = a.csrf_token(&p1);
        assert_eq!(c1, s1.csrf.as_ref().unwrap().expose());
        assert!(a.check_csrf(&p1, &c1));
        assert!(!a.check_csrf(&p2, &c1));
        assert!(a.check_csrf(&p2, &a.csrf_token(&p2)));
    }

    #[test]
    fn csrf_rejects_empty_odd_uppercase() {
        let (_, a) = auth();
        let s = login(&a, Kind::Browser, peer(26, 1));
        let p = browser(&a, &s);
        let good = a.csrf_token(&p);
        for bad in [String::new(), good[..63].to_owned(), format!("{good}a"), good.to_ascii_uppercase(), good[..32].to_owned()] {
            assert!(!a.check_csrf(&p, &bad), "accepted {bad:?}");
        }
        assert!(a.check_csrf(&p, &good));
    }

    #[test]
    fn check_csrf_bearer_principal_false() {
        let (_, a) = auth();
        let t = login(&a, Kind::Bearer, peer(27, 1));
        let p = a.authenticate(Kind::Bearer, t.token.expose()).unwrap();
        assert!(!a.check_csrf(&p, &a.csrf_token(&p)));
        assert!(!a.check_csrf(&p, &"0".repeat(64)));
    }

    fn pre(a: &AuthState) -> String {
        a.new_pre_token().unwrap().expose().to_owned()
    }

    #[test]
    fn login_form_token_valid_within_15min() {
        let (clock, a) = auth();
        let pre = pre(&a);
        let token = a.login_form_token(&pre).unwrap();
        let (secs, mac) = token.split_once('.').unwrap();
        // Not the boot time (1000 s): shifted by the per-process offset.
        let offset = form_offset(a.form_key.get().unwrap());
        assert!(offset > 0);
        assert_eq!(secs, (1000 + offset).to_string());
        assert!(is_lower_hex(mac, 32));
        assert!(a.verify_login_form_token(&pre, &token));
        clock.advance(15 * MIN);
        assert!(a.verify_login_form_token(&pre, &token));
    }

    #[test]
    fn login_form_token_expired() {
        let (clock, a) = auth();
        let pre = pre(&a);
        let token = a.login_form_token(&pre).unwrap();
        assert!(a.login_form_token_fresh(&token));
        clock.advance(15 * MIN + Duration::from_secs(1));
        assert!(!a.verify_login_form_token(&pre, &token));
        assert!(!a.login_form_token_fresh(&token));
    }

    #[test]
    fn login_form_token_bound_to_pre() {
        let (_, a) = auth();
        let (pre1, pre2) = (pre(&a), pre(&a));
        let token = a.login_form_token(&pre1).unwrap();
        assert!(!a.verify_login_form_token(&pre2, &token));
        assert!(!a.verify_login_form_token(&pre1.to_ascii_uppercase(), &token));
        assert!(!a.verify_login_form_token("", &token));
    }

    #[test]
    fn login_form_token_tampered_secs() {
        let (clock, a) = auth();
        let pre = pre(&a);
        let token = a.login_form_token(&pre).unwrap();
        let (secs, mac) = token.split_once('.').unwrap();
        let shown: u64 = secs.parse().unwrap();
        clock.advance(10 * MIN);
        // Claiming a later issue time to stretch the lifetime fails the MAC.
        let s = shown;
        for secs in [(s + 1).to_string(), (s + 300).to_string(), format!("+{s}"), format!("0{s}"), String::new(), format!("{s} ")] {
            assert!(!a.verify_login_form_token(&pre, &format!("{secs}.{mac}")), "accepted {secs:?}");
        }
        // Future-dated tokens are refused too, and so is the bare boot time
        // (below the offset).
        for secs in [shown + 3600, 1000] {
            assert!(!a.verify_login_form_token(&pre, &format!("{secs}.{mac}")));
            assert!(!a.login_form_token_fresh(&format!("{secs}.{mac}")));
        }
        assert!(!a.verify_login_form_token(&pre, &token.replace('.', "")));
        assert!(!a.verify_login_form_token(&pre, &format!("{token}0")));
        assert!(!a.verify_login_form_token(&pre, &token.to_ascii_uppercase()));
        assert!(a.verify_login_form_token(&pre, &token));
    }

    #[test]
    fn login_form_token_new_form_key_invalid() {
        let (_, a) = auth();
        let (_, restarted) = auth();
        let pre = pre(&a);
        let token = a.login_form_token(&pre).unwrap();
        // Before the restarted process issued any form token: no key yet.
        assert!(!restarted.verify_login_form_token(&pre, &token));
        // After it has its own key: still a different key.
        let _ = restarted.login_form_token(&pre).unwrap();
        assert!(!restarted.verify_login_form_token(&pre, &token));
    }

    #[test]
    fn form_key_entropy_failure_is_error() {
        let a = AuthState::with_parts(FakeClock::at_secs(5), Box::new(FailRng), AuthConfig::default());
        assert_eq!(a.login_form_token(&"a".repeat(64)).unwrap_err(), LoginError::Entropy);
        assert_eq!(a.new_pre_token().unwrap_err(), LoginError::Entropy);
        assert!(!a.verify_login_form_token(&"a".repeat(64), "5.00000000000000000000000000000000"));
    }

    // --- login rate limit through the facade ---

    fn attempt(a: &AuthState, pw: &str, p: PeerKey) -> Result<(), LoginError> {
        a.login(pw, Kind::Bearer, p, &[]).map(|_| ())
    }

    fn limited(d: Duration) -> Result<(), LoginError> {
        Err(LoginError::RateLimited { retry_after: d })
    }

    #[test]
    fn schedule_table_exact() {
        let (clock, a) = auth();
        let p = peer(30, 1);
        for _ in 1..=5 {
            assert_eq!(attempt(&a, "wrong", p), Err(LoginError::BadPassword));
        }
        // The sixth attempt within 1 s is refused.
        assert_eq!(attempt(&a, "wrong", p), limited(Duration::from_secs(1)));
        clock.advance(Duration::from_millis(999));
        assert_eq!(attempt(&a, "wrong", p), limited(Duration::from_millis(1)));
        clock.advance(Duration::from_millis(1));
        // Failure n blocks for block(n): 6 → 2 s, 7 → 4 s, ... 13 → 256 s, then 300 s.
        let expected = [2u64, 4, 8, 16, 32, 64, 128, 256, 300, 300];
        for secs in expected {
            assert_eq!(attempt(&a, "wrong", p), Err(LoginError::BadPassword));
            assert_eq!(attempt(&a, "wrong", p), limited(Duration::from_secs(secs)));
            clock.advance(Duration::from_secs(secs) - Duration::from_millis(1));
            assert_eq!(attempt(&a, PW, p), limited(Duration::from_millis(1)));
            clock.advance(Duration::from_millis(1));
        }
        // Once the block ends, the right password works.
        assert!(attempt(&a, PW, p).is_ok());
    }

    #[test]
    fn blocked_correct_password_is_rate_limited_and_not_recorded() {
        let (clock, a) = auth();
        let p = peer(31, 1);
        for _ in 0..5 {
            assert_eq!(attempt(&a, "wrong", p), Err(LoginError::BadPassword));
        }
        assert_eq!(attempt(&a, PW, p), limited(Duration::from_secs(1)));
        for _ in 0..20 {
            assert!(matches!(attempt(&a, "wrong", p), Err(LoginError::RateLimited { .. })));
        }
        clock.advance(Duration::from_secs(1));
        // Still only five failures recorded: the next one blocks for 2 s.
        assert_eq!(attempt(&a, "wrong", p), Err(LoginError::BadPassword));
        assert_eq!(attempt(&a, "wrong", p), limited(Duration::from_secs(2)));
    }

    #[test]
    fn block_ends_without_extension() {
        let (clock, a) = auth();
        let p = peer(32, 1);
        for _ in 0..5 {
            let _ = attempt(&a, "wrong", p);
        }
        for left in (1..10u64).rev() {
            assert_eq!(attempt(&a, "wrong", p), limited(Duration::from_millis(left * 100 + 100)));
            clock.advance(Duration::from_millis(100));
        }
        clock.advance(Duration::from_millis(100));
        assert!(attempt(&a, PW, p).is_ok());
    }

    #[test]
    fn success_clears_slot() {
        let (_, a) = auth();
        let p = peer(33, 1);
        for _ in 0..4 {
            assert_eq!(attempt(&a, "wrong", p), Err(LoginError::BadPassword));
        }
        assert!(attempt(&a, PW, p).is_ok());
        // Counting starts over: five more failures before any block.
        for _ in 0..5 {
            assert_eq!(attempt(&a, "wrong", p), Err(LoginError::BadPassword));
        }
        assert_eq!(attempt(&a, PW, p), limited(Duration::from_secs(1)));
    }

    #[test]
    fn success_not_counted_in_global_bucket() {
        let (_, a) = auth();
        // 29 failures spread over peers that never reach their own block.
        for n in 0..29u8 {
            assert_eq!(attempt(&a, "wrong", peer(34, n)), Err(LoginError::BadPassword));
        }
        for n in 0..10u8 {
            assert!(attempt(&a, PW, peer(34, 100 + n)).is_ok());
        }
        // The bucket still has room for exactly one more failure.
        assert_eq!(attempt(&a, "wrong", peer(34, 200)), Err(LoginError::BadPassword));
        assert_eq!(attempt(&a, PW, peer(34, 201)), limited(Duration::from_secs(10)));
    }

    #[test]
    fn global_bucket_keeps_existing_sessions() {
        let (clock, a) = auth();
        let t = login(&a, Kind::Bearer, peer(35, 250));
        for n in 0..30u8 {
            let _ = attempt(&a, "wrong", peer(35, n));
        }
        assert!(matches!(attempt(&a, PW, peer(35, 251)), Err(LoginError::RateLimited { .. })));
        assert!(a.validate(t.token.expose()));
        clock.advance(Duration::from_secs(10));
        assert!(attempt(&a, PW, peer(35, 251)).is_ok());
    }
}
