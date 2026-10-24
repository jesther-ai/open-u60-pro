//! LAN HTTPS with the router's own certificate authority (owner decision
//! 59): the web UI at `https://<LAN address>:9443/ui/` and at a local name
//! (`u60.home.arpa`), so browsers can install it as an app. Each device
//! trusts the CA once (Settings offers the iPhone profile, the `.crt` and
//! the fingerprint). Offline, no accounts; plain http on :9090 stays.
//!
//! - Nothing exists until the owner turns HTTPS on (`turn_on`); "Reset
//!   HTTPS" (`reset`) deletes everything. Files in `/data/zte-agent-tls/`
//!   (directory 0700, keys 0600); keys are never logged, rendered or served.
//! - The CA (`x509::authority`): a root, name-constrained to this router
//!   only (its LAN address at Turn On as a /32, if private, and the local
//!   name), that signs one issuing CA with the same constraints; the root's
//!   key is never stored. The agent keeps the issuing CA's key, which can
//!   vouch for nothing outside the constraints on any device (Android
//!   ignores a user-installed root's constraints, not an intermediate's).
//!   The server certificate names the LAN address (`br-lan`'s IPv4) when
//!   the CA covers it and the local name, for at most 397 days, and goes
//!   out with the issuing CA. After a LAN address change only the name
//!   works until Reset (a new CA for the new address).
//! - No timers. The server certificate is issued again at agent start, at a
//!   TLS connection (`wants_check` / `check`, at most once a minute) and
//!   after a LAN change (`lan_changed`): when the address it should name
//!   changed (same dates: no clock needed), or when fewer than 30 days are
//!   left by the **verified** clock (UTC checked over TLS by the
//!   live-activity probe, which runs only with a Live Activity provider
//!   configured; without one Settings offers Renew in the last 30 days).
//!   The router's own clock is stale after a fast-boot power-on until the
//!   network sets it, so it alone never renews anything; dates are
//!   backdated two days, and an owner-confirmed action (Turn on, Renew)
//!   may date from it when it is past `CLOCK_FLOOR`.
//! - The listener (`httpd::tls`) follows `subscribe()` and takes the
//!   current key from `keys()` at every handshake (hot swap).
//! - The local name: `/tmp/hosts/zte-agent` for dnsmasq (`hosts`).

pub mod hosts;
pub mod x509;

use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::sign::CertifiedKey;
use serde_json::{json, Value};

use crate::boottime::{BootClock, Clock};
use x509::{Info, KeyCert, LeafSpec};

pub const BIND_ENV: &str = "ZTE_AGENT_TLS_BIND";
pub const DEFAULT_BIND: &str = "0.0.0.0:9443";
pub const NAME_ENV: &str = "ZTE_AGENT_TLS_NAME";
pub const DEFAULT_NAME: &str = "u60.home.arpa";
/// The CA's DNS zone: RFC 8375's name for home networks, never delegated
/// on the internet.
pub const ZONE: &str = "home.arpa";
pub const DIR: &str = "/data/zte-agent-tls";
/// The interface whose IPv4 address the server certificate names.
pub const LAN_IF: &str = "br-lan";
/// The private IPv4 ranges (RFC 1918) the CA may vouch for.
pub const PRIVATE_V4: [(Ipv4Addr, u8); 3] =
    [(Ipv4Addr::new(10, 0, 0, 0), 8), (Ipv4Addr::new(172, 16, 0, 0), 12), (Ipv4Addr::new(192, 168, 0, 0), 16)];

const DAY: i64 = 86_400;
/// notBefore lies this far back: a client clock a little behind, the
/// firmware's +8 h NITZ jumps.
const BACKDATE: i64 = 2 * DAY;
/// Apple's and Chrome's limit for public TLS certificates is 398 days;
/// Apple's for every TLS certificate (user-trusted roots too) 825.
pub const SERVER_DAYS: i64 = 397;
const CA_DAYS: i64 = 3650;
pub const RENEW_BEFORE: i64 = 30 * DAY;
/// 2026-10-01 00:00 UTC: a router clock before this is not set.
pub const CLOCK_FLOOR: i64 = 1_790_812_800;
/// A TLS connection asks for a check at most this often (boot clock, s).
const CHECK_EVERY: u64 = 60;

/// The root's certificate (its key is never stored).
const CA_CRT: &str = "ca.crt";
const ISSUING_KEY: &str = "issuing.key";
const ISSUING_CRT: &str = "issuing.crt";
const SERVER_KEY: &str = "server.key";
const SERVER_CRT: &str = "server.crt";

pub const CLOCK_UNSET: &str = "The router\u{2019}s clock isn\u{2019}t set yet, so no certificate can be dated. Try again once the router shows today\u{2019}s date (it sets its clock from the mobile network).";
const NO_LAN_IP: &str = "The router\u{2019}s LAN address could not be read. Try again in a moment.";

/// The router's UTC (unix seconds) and how far it can be trusted.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Utc {
    /// Checked over TLS against a time server (live activity's probe).
    Verified(i64),
    /// The router's clock, past `CLOCK_FLOOR` (and the CA's own date).
    Unverified(i64),
    /// The router's clock, before that: not set.
    Unset(i64),
}

impl Utc {
    /// A time an owner-confirmed action may date certificates from.
    pub fn usable(self) -> Option<i64> {
        match self {
            Utc::Verified(t) | Utc::Unverified(t) => Some(t),
            Utc::Unset(_) => None,
        }
    }

    pub fn secs(self) -> i64 {
        match self {
            Utc::Verified(t) | Utc::Unverified(t) | Utc::Unset(t) => t,
        }
    }
}

/// An address the CA may vouch for.
pub fn private(ip: Ipv4Addr) -> bool {
    PRIVATE_V4.iter().any(|(net, len)| {
        let keep = u32::MAX << (32 - u32::from(*len));
        u32::from(ip) & keep == u32::from(*net)
    })
}

/// A local name the CA's constraints cover: one or more labels under
/// `home.arpa`, as a Host header would carry it.
pub fn valid_name(n: &str) -> bool {
    use crate::httpd::guard::{parse_authority, Authority};
    n.len() <= 253
        && n.strip_suffix(ZONE).is_some_and(|l| l.len() > 1 && l.ends_with('.'))
        && parse_authority(n).is_some_and(|(a, port)| port.is_none() && a == Authority::Name(n.to_owned()))
}

/// The certificate authority on disk: the root's certificate (what devices
/// trust) and the issuing CA, chained by name, the key matching.
struct Ca {
    root: Vec<u8>,
    root_info: Info,
    issuing: KeyCert,
    issuing_info: Info,
    /// What it may vouch for (its name constraints).
    scope: x509::Scope,
    sha256: String,
    sha1: String,
}

impl Ca {
    fn new(root: Vec<u8>, issuing: KeyCert) -> Result<Ca, String> {
        let root_info = x509::parse(&root).ok_or("the CA certificate is unreadable")?;
        let issuing_info = x509::parse(&issuing.cert).ok_or("the issuing CA certificate is unreadable")?;
        if issuing_info.issuer != root_info.subject {
            return Err("the certificates do not belong together".into());
        }
        if x509::key_point(&issuing.key).as_ref() != Some(&issuing_info.point) {
            return Err("the issuing CA key does not match its certificate".into());
        }
        let scope = x509::scope(&issuing_info).ok_or("the issuing CA has no name constraints")?;
        Ok(Ca { sha256: x509::sha256_fingerprint(&root), sha1: x509::sha1_fingerprint(&root), root, root_info, issuing, issuing_info, scope })
    }

    /// `["192.168.0.1", "u60.home.arpa"]` (an address alone, a network as
    /// `net/len`).
    fn covers(&self) -> Vec<String> {
        self.scope.ips.iter().map(|(n, l)| net(*n, *l)).chain(self.scope.dns.iter().cloned()).collect()
    }
}

fn net(n: Ipv4Addr, len: u8) -> String {
    if len == 32 {
        n.to_string()
    } else {
        format!("{n}/{len}")
    }
}

/// What the agent holds while HTTPS is on.
struct Loaded {
    ca: Arc<Ca>,
    server_info: Info,
}

impl Loaded {
    /// The server pair parsed, issued by `ca`, the key matching; and the
    /// rustls key for the listener (the server certificate, then the
    /// issuing CA's).
    fn new(ca: Arc<Ca>, server: &KeyCert) -> Result<(Loaded, Arc<CertifiedKey>), String> {
        let server_info = x509::parse(&server.cert).ok_or("the server certificate is unreadable")?;
        if server_info.issuer != ca.issuing_info.subject {
            return Err("the server certificate is not the issuing CA's".into());
        }
        if x509::key_point(&server.key).as_ref() != Some(&server_info.point) {
            return Err("the server key does not match its certificate".into());
        }
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server.key.clone()));
        let signer = rustls::crypto::ring::sign::any_ecdsa_type(&key).map_err(|_| "the server key is unusable")?;
        let chain = vec![CertificateDer::from(server.cert.clone()), CertificateDer::from(ca.issuing.cert.clone())];
        let certified = Arc::new(CertifiedKey::new(chain, signer));
        Ok((Loaded { ca, server_info }, certified))
    }

    /// The LAN address the server certificate names.
    fn server_ip(&self) -> Option<Ipv4Addr> {
        self.server_info.ips.iter().find_map(v4)
    }
}

fn v4(ip: &IpAddr) -> Option<Ipv4Addr> {
    match ip {
        IpAddr::V4(a) => Some(*a),
        IpAddr::V6(_) => None,
    }
}

#[derive(Default)]
struct State {
    loaded: Option<Arc<Loaded>>,
    /// The CA on disk while HTTPS is off because its server pair could not
    /// be loaded or issued again at start: Turn On issues one from it
    /// (devices keep trusting it).
    kept: Option<Arc<Ca>>,
    /// The LAN address last read.
    lan: Option<Ipv4Addr>,
    /// Where the listener listens (set by `httpd::tls`).
    listening: Option<SocketAddr>,
    /// Why it does not.
    listen_error: Option<String>,
    /// The last problem with the certificates (files, an address the CA
    /// does not cover); worked out again at every refresh.
    error: Option<String>,
}

/// The current server key, read by the listener at every handshake.
pub type Keys = Arc<RwLock<Option<Arc<CertifiedKey>>>>;

/// How a `Tls` reaches the router (tests swap every part).
pub struct Parts {
    pub dir: PathBuf,
    pub name: String,
    pub bind: String,
    pub hosts: hosts::Hosts,
    /// The LAN address (`br-lan`'s IPv4; blocking).
    pub lan_ip: Box<dyn Fn() -> Option<Ipv4Addr> + Send + Sync>,
    /// TLS-verified UTC, if any.
    pub verified: Box<dyn Fn() -> Option<i64> + Send + Sync>,
    /// The router's clock (unix seconds).
    pub wall: Box<dyn Fn() -> i64 + Send + Sync>,
    pub boot: Arc<dyn Clock>,
}

pub struct Tls {
    p: Parts,
    port: u16,
    /// Serialises changes (issue, renew, reset) and their files.
    work: Mutex<()>,
    state: RwLock<State>,
    keys: Keys,
    on: tokio::sync::watch::Sender<bool>,
    /// The last `wants_check` that said yes (boot seconds + 1; 0: never).
    checked: AtomicU64,
}

/// `ZTE_AGENT_TLS_BIND` and `ZTE_AGENT_TLS_NAME`, or their defaults (a
/// value that does not parse is reported and ignored).
fn from_env() -> (String, String) {
    let bind = match std::env::var(BIND_ENV) {
        Ok(b) if b.trim().parse::<SocketAddr>().is_ok() => b.trim().to_owned(),
        Ok(b) => {
            eprintln!("[tls] {BIND_ENV}={b:?} is not an address:port, using {DEFAULT_BIND}");
            DEFAULT_BIND.to_owned()
        }
        Err(_) => DEFAULT_BIND.to_owned(),
    };
    let name = match std::env::var(NAME_ENV) {
        Ok(n) if valid_name(&n.trim().to_ascii_lowercase()) => n.trim().to_ascii_lowercase(),
        Ok(n) => {
            eprintln!("[tls] {NAME_ENV}={n:?} is not a name under .{ZONE}, using {DEFAULT_NAME}");
            DEFAULT_NAME.to_owned()
        }
        Err(_) => DEFAULT_NAME.to_owned(),
    };
    (bind, name)
}

/// `br-lan`'s IPv4 address (`getifaddrs`; blocking).
fn lan_ip() -> Option<Ipv4Addr> {
    crate::httpd::mobile::interfaces().ok()?.into_iter().find_map(|i| match i.addr {
        IpAddr::V4(a) if i.name == LAN_IF => Some(a),
        _ => None,
    })
}

fn lock(m: &Mutex<()>) -> MutexGuard<'_, ()> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Settings' line when the files on disk cannot be used at all.
const NEW_CA: &str = "Turn On HTTPS makes a new certificate authority, which each device must trust again; Reset HTTPS deletes these files.";
/// Settings' line when the CA is fine but its server pair could not be
/// issued again at start (no verified clock to date it).
pub const NO_PAIR: &str = "The server certificate is missing or damaged, and the router\u{2019}s clock has not been checked over the internet yet. Turn On HTTPS issues a new one from the same certificate authority: devices that trust it need nothing new.";

impl Tls {
    /// The router's: `/data/zte-agent-tls`, dnsmasq, `br-lan`, the live
    /// activity's verified UTC. Touches nothing until `start` or `turn_on`.
    pub fn router(live: Arc<crate::live_activity::LiveActivity>) -> Tls {
        let (bind, name) = from_env();
        Tls::new(Parts {
            dir: PathBuf::from(DIR),
            name,
            bind,
            hosts: hosts::Hosts::router(),
            lan_ip: Box::new(lan_ip),
            verified: Box::new(move || live.verified_utc().and_then(|t| i64::try_from(t).ok())),
            wall: Box::new(crate::web::sms::now_unix),
            boot: Arc::new(BootClock),
        })
    }

    pub fn new(p: Parts) -> Tls {
        let port = p.bind.parse::<SocketAddr>().map_or(9443, |a| a.port());
        Tls {
            p,
            port,
            work: Mutex::new(()),
            state: RwLock::default(),
            keys: Arc::default(),
            on: tokio::sync::watch::channel(false).0,
            checked: AtomicU64::new(0),
        }
    }

    pub fn name(&self) -> &str {
        &self.p.name
    }

    pub fn bind(&self) -> &str {
        &self.p.bind
    }

    /// The listener's server key (hot-swapped).
    pub fn keys(&self) -> Keys {
        Arc::clone(&self.keys)
    }

    /// Whether HTTPS is on, as it changes (the listener starts and stops by
    /// it; `true` sent again while on: listen again after a failed bind).
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<bool> {
        self.on.subscribe()
    }

    pub fn is_on(&self) -> bool {
        self.read().loaded.is_some()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn loaded(&self) -> Option<Arc<Loaded>> {
        self.read().loaded.clone()
    }

    fn set_error(&self, e: Option<String>) {
        self.write().error = e;
    }

    /// The listener's outcome: where it listens, or why it does not.
    pub fn set_listening(&self, r: Result<SocketAddr, String>) {
        let mut s = self.write();
        match r {
            Ok(a) => (s.listening, s.listen_error) = (Some(a), None),
            Err(e) => (s.listening, s.listen_error) = (None, Some(e)),
        }
    }

    pub fn stopped(&self) {
        self.write().listening = None;
    }

    fn install(&self, loaded: Loaded, key: Arc<CertifiedKey>) {
        *self.keys.write().unwrap_or_else(PoisonError::into_inner) = Some(key);
        let mut s = self.write();
        s.loaded = Some(Arc::new(loaded));
        s.kept = None;
    }

    pub fn utc(&self) -> Utc {
        if let Some(t) = (self.p.verified)() {
            return Utc::Verified(t);
        }
        let wall = (self.p.wall)();
        let ca_from = {
            let s = self.read();
            s.loaded.as_ref().map(|l| l.ca.root_info.not_before).or(s.kept.as_ref().map(|c| c.root_info.not_before))
        };
        let floor = ca_from.map_or(CLOCK_FLOOR, |t| t + BACKDATE).max(CLOCK_FLOOR);
        if wall >= floor {
            Utc::Unverified(wall)
        } else {
            Utc::Unset(wall)
        }
    }

    fn sync_hosts(&self, ip: Ipv4Addr) {
        if let Err(e) = self.p.hosts.set(ip, &self.p.name) {
            eprintln!("[tls] could not write {}: {e}", self.p.hosts.file.display());
        }
    }

    /// A new server pair from `ca` for `lan` (if the CA covers it) and the
    /// local name (if it covers that), ending no later than the CA; saved
    /// (key, then certificate) and swapped in.
    fn issue(&self, ca: &Arc<Ca>, lan: Option<Ipv4Addr>, not_before: i64, not_after: i64) -> Result<(), String> {
        let ips: Vec<IpAddr> = lan.filter(|a| ca.scope.covers_ip(*a)).into_iter().map(IpAddr::V4).collect();
        let dns: Vec<&str> = Some(self.p.name.as_str()).filter(|n| ca.scope.covers_name(n)).into_iter().collect();
        if ips.is_empty() && dns.is_empty() {
            return Err(format!("The certificate authority covers neither the router\u{2019}s LAN address nor {}. {NEW_CA}", self.p.name));
        }
        let not_after = not_after.min(ca.issuing_info.not_after);
        let spec = LeafSpec { dns: &dns, ips: &ips, not_before, not_after };
        let server = x509::leaf(&ca.issuing, &spec)?;
        save(&self.p.dir, SERVER_KEY, &server.key, 0o600).and_then(|_| save(&self.p.dir, SERVER_CRT, &server.cert, 0o644)).map_err(saving)?;
        let (loaded, key) = Loaded::new(Arc::clone(ca), &server)?;
        self.install(loaded, key);
        Ok(())
    }

    /// What the CA cannot cover at LAN address `lan` (Settings' line).
    fn scope_error(&self, ca: &Ca, lan: Ipv4Addr) -> Option<String> {
        let name_url = format!("https://{}:{}/ui/", self.p.name, self.port);
        if !ca.scope.covers_name(&self.p.name) {
            Some(format!(
                "The certificate authority was made for {}, not {} ({NAME_ENV}). To use the new name, Reset HTTPS and turn it on again (each device must then trust the new authority).",
                ca.scope.dns.join(", "),
                self.p.name
            ))
        } else if !private(lan) {
            Some(format!("The router\u{2019}s LAN address {lan} is not a private address (10.x, 172.16\u{2013}31.x, 192.168.x): only {name_url} works."))
        } else if !ca.scope.covers_ip(lan) {
            let was = match ca.scope.ips.as_slice() {
                [] => "no address".to_owned(),
                ips => ips.iter().map(|(n, l)| net(*n, *l)).collect::<Vec<_>>().join(", "),
            };
            Some(format!(
                "The router\u{2019}s LAN address is now {lan}, but its certificate authority was made for {was}: only {name_url} works. To use the address, Reset HTTPS and turn it on again (each device must then trust the new authority)."
            ))
        } else {
            None
        }
    }

    /// The server certificate against the LAN address (`lan`, if it could
    /// be read) and the verified clock: issued again when the address it
    /// should name changed (same dates: no clock needed) or fewer than 30
    /// days are left by the verified clock (and a renewal would end later);
    /// the hosts line follows the address; Settings' certificate line is
    /// worked out again (cleared once nothing is wrong).
    fn refresh(&self, cur: &Loaded, lan: Option<Ipv4Addr>) {
        if lan.is_some() {
            self.write().lan = lan;
        }
        let ca = &cur.ca;
        let want = lan.or(cur.server_ip()).filter(|a| ca.scope.covers_ip(*a));
        let renewal = |t: i64| (t - BACKDATE, (t - BACKDATE + SERVER_DAYS * DAY).min(ca.issuing_info.not_after));
        let expiring =
            (self.p.verified)().filter(|t| cur.server_info.not_after - t < RENEW_BEFORE && renewal(*t).1 > cur.server_info.not_after);
        let moved = want != cur.server_ip();
        let mut error = lan.and_then(|a| self.scope_error(ca, a));
        if expiring.is_some() || moved {
            let ((nb, na), why) = match expiring {
                Some(t) => (renewal(t), "renewed"),
                None => ((cur.server_info.not_before, cur.server_info.not_after), "the LAN address changed"),
            };
            match self.issue(ca, want, nb, na) {
                Ok(()) => eprintln!("[tls] server certificate issued again ({why})"),
                Err(e) => {
                    eprintln!("[tls] could not issue the server certificate again: {e}");
                    error = Some(e);
                }
            }
        }
        if let Some(ip) = lan.or(cur.server_ip()) {
            self.sync_hosts(ip);
        }
        if lan.is_some() || error.is_some() {
            self.set_error(error);
        }
    }

    /// At agent start (blocking; once): the files, then the address,
    /// renewal and the hosts file; the listener starts when it is on. A
    /// server pair that is missing or damaged (a save cut short) is issued
    /// again from the same CA, dated as the old certificate when it still
    /// reads, else by the verified clock; with neither, the CA is kept for
    /// Turn On. The router's own clock is never used here.
    pub fn start(&self) {
        let _w = lock(&self.work);
        let (root, issuing, server) = match read_files(&self.p.dir) {
            Ok(None) => {
                // Off: no name left behind (an agent stopped mid-reset).
                let _ = self.p.hosts.remove();
                return;
            }
            Ok(Some(files)) => files,
            Err(e) => {
                eprintln!("[tls] {}: {e}", self.p.dir.display());
                self.set_error(Some(format!("Could not read the certificates: {e}. {NEW_CA}")));
                return;
            }
        };
        let ca = match Ca::new(root, issuing) {
            Ok(ca) => Arc::new(ca),
            Err(e) => {
                eprintln!("[tls] {e}");
                self.set_error(Some(format!("The saved certificate authority is unusable ({e}). {NEW_CA}")));
                return;
            }
        };
        let lan = (self.p.lan_ip)();
        let pair = match (server.key, &server.cert) {
            (Some(key), Some(cert)) => Some(KeyCert { key, cert: cert.clone() }),
            _ => None,
        };
        match pair.map(|s| Loaded::new(Arc::clone(&ca), &s)) {
            Some(Ok((loaded, key))) => self.install(loaded, key),
            saved => {
                match saved {
                    Some(Err(e)) => eprintln!("[tls] {e}: issuing a new server certificate"),
                    _ => eprintln!("[tls] the server key or certificate is missing: issuing a new server certificate"),
                }
                let old = server.cert.as_deref().and_then(x509::parse).filter(|i| i.issuer == ca.issuing_info.subject);
                let dates = old
                    .as_ref()
                    .map(|i| (i.not_before, i.not_after))
                    .or_else(|| (self.p.verified)().map(|t| (t - BACKDATE, t - BACKDATE + SERVER_DAYS * DAY)));
                let ip = lan.or(old.as_ref().and_then(|i| i.ips.iter().find_map(v4)));
                let issued = match dates {
                    Some((nb, na)) => self.issue(&ca, ip, nb, na),
                    None => Err(NO_PAIR.to_owned()),
                };
                if let Err(e) = issued {
                    eprintln!("[tls] {e}");
                    self.write().kept = Some(ca);
                    self.set_error(Some(e));
                    return;
                }
                eprintln!("[tls] server certificate issued again (the saved one was missing or damaged)");
            }
        }
        let Some(cur) = self.loaded() else { return };
        self.refresh(&cur, lan);
        self.on.send_replace(true);
    }

    /// Settings › Turn On HTTPS: a new CA (for the LAN address and the
    /// local name only) and server certificate, dated from the router's
    /// clock, which the owner saw on the question. With a CA kept at start
    /// (its server pair lost), only a new server pair from it. Already on
    /// but not listening (a failed bind): the listener tries again.
    pub fn turn_on(&self) -> Result<(), String> {
        let _w = lock(&self.work);
        if self.is_on() {
            if self.read().listening.is_none() {
                self.on.send_replace(true);
            }
            return Ok(());
        }
        let now = self.utc().usable().ok_or(CLOCK_UNSET)?;
        let ip = (self.p.lan_ip)().ok_or(NO_LAN_IP)?;
        let kept = self.read().kept.clone();
        if let Some(ca) = kept {
            self.issue(&ca, Some(ip), now - BACKDATE, now - BACKDATE + SERVER_DAYS * DAY)?;
            eprintln!("[tls] HTTPS on: {}, a new server certificate", ca.root_info.cn);
        } else {
            let mut id = [0u8; 4];
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut id).map_err(|_| "No randomness available.")?;
            let id = format!("{:02X}{:02X}{:02X}{:02X}", id[0], id[1], id[2], id[3]);
            let (cn, issuing_cn) = (format!("U60 Pro local CA {id}"), format!("U60 Pro issuing CA {id}"));
            // This router only: its LAN address (if private) and the name.
            let ips: Vec<(Ipv4Addr, u8)> = Some(ip).filter(|a| private(*a)).map(|a| (a, 32)).into_iter().collect();
            let name = [self.p.name.as_str()];
            let spec = |cn| x509::CaSpec { cn, not_before: now - BACKDATE, not_after: now + CA_DAYS * DAY, ips: &ips, dns: &name };
            let (root, issuing) = x509::authority(&spec(&cn), &spec(&issuing_cn))?;
            let leaf_ips: Vec<IpAddr> = ips.iter().map(|(a, _)| IpAddr::V4(*a)).collect();
            let spec = LeafSpec { dns: &name, ips: &leaf_ips, not_before: now - BACKDATE, not_after: now - BACKDATE + SERVER_DAYS * DAY };
            let server = x509::leaf(&issuing, &spec)?;
            let files = [
                (CA_CRT, &root, 0o644),
                (ISSUING_KEY, &issuing.key, 0o600),
                (ISSUING_CRT, &issuing.cert, 0o644),
                (SERVER_KEY, &server.key, 0o600),
                (SERVER_CRT, &server.cert, 0o644),
            ];
            if let Err(e) = files.into_iter().try_for_each(|(name, bytes, mode)| save(&self.p.dir, name, bytes, mode)) {
                let _ = remove_files(&self.p.dir);
                return Err(saving(e));
            }
            let ca = Arc::new(Ca::new(root, issuing)?);
            let (loaded, key) = Loaded::new(ca, &server)?;
            eprintln!("[tls] HTTPS on: {cn}, SHA-256 {}", loaded.ca.sha256);
            self.install(loaded, key);
        }
        let cur = self.loaded().ok_or("HTTPS is off.")?;
        self.refresh(&cur, Some(ip));
        self.on.send_replace(true);
        Ok(())
    }

    /// Settings › Renew Certificate (owner-confirmed date): a new server
    /// certificate from the same CA (devices keep trusting it).
    pub fn renew(&self) -> Result<(), String> {
        let _w = lock(&self.work);
        let cur = self.loaded().ok_or("HTTPS is off.")?;
        let now = self.utc().usable().ok_or(CLOCK_UNSET)?;
        let lan = (self.p.lan_ip)();
        self.issue(&cur.ca, lan.or(cur.server_ip()), now - BACKDATE, now - BACKDATE + SERVER_DAYS * DAY)?;
        eprintln!("[tls] server certificate renewed");
        if let Some(cur) = self.loaded() {
            self.refresh(&cur, lan);
        }
        Ok(())
    }

    /// Settings › Reset HTTPS: the listener stops (and ends its open
    /// connections); the CA, the certificate and the local name are
    /// deleted.
    pub fn reset(&self) -> Result<(), String> {
        let _w = lock(&self.work);
        *self.keys.write().unwrap_or_else(PoisonError::into_inner) = None;
        {
            let mut s = self.write();
            (s.loaded, s.kept, s.lan) = (None, None, None);
            (s.error, s.listen_error) = (None, None);
        }
        self.on.send_replace(false);
        let files = remove_files(&self.p.dir);
        let hosts = self.p.hosts.remove();
        eprintln!("[tls] HTTPS reset: certificates deleted");
        files.map_err(|e| format!("Could not delete {}: {e}", self.p.dir.display()))?;
        hosts.map_err(|e| format!("Could not delete {}: {e}", self.p.hosts.file.display()))?;
        Ok(())
    }

    /// The LAN settings were applied with this address (`router_lan_set`).
    pub fn lan_changed(&self, ip: Ipv4Addr) {
        let _w = lock(&self.work);
        if let Some(cur) = self.loaded() {
            self.refresh(&cur, Some(ip));
        }
    }

    /// At a TLS connection to `local` (on the runtime thread: memory only):
    /// whether to `check` first. Yes when it reached a private address
    /// that is neither the certificate's nor the LAN address last read
    /// (the LAN may have moved), or fewer than 30 days are left by the
    /// router's clock; at most once a minute.
    pub fn wants_check(&self, local: IpAddr) -> bool {
        let (cur, lan) = {
            let s = self.read();
            (s.loaded.clone(), s.lan)
        };
        let Some(cur) = cur else { return false };
        let moved = matches!(local.to_canonical(), IpAddr::V4(a) if private(a) && Some(a) != cur.server_ip() && Some(a) != lan);
        let expiring = cur.server_info.not_after - (self.p.wall)() < RENEW_BEFORE;
        if !moved && !expiring {
            return false;
        }
        let now = self.p.boot.now().as_secs() + 1;
        let last = self.checked.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < CHECK_EVERY {
            return false;
        }
        self.checked.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok()
    }

    /// The address read again and the verified clock asked (blocking).
    pub fn check(&self) {
        let _w = lock(&self.work);
        if let Some(cur) = self.loaded() {
            self.refresh(&cur, (self.p.lan_ip)());
        }
    }

    /// The root's certificate (DER), while HTTPS is on.
    pub fn ca_der(&self) -> Option<Vec<u8>> {
        self.loaded().map(|l| l.ca.root.clone())
    }

    /// The iPhone/iPad profile with the root, while HTTPS is on.
    pub fn mobileconfig(&self) -> Option<String> {
        self.loaded().map(|l| mobileconfig(&l.ca.root, &l.ca.root_info.cn, &l.ca.covers().join(" and "), self.port))
    }

    pub fn status(&self) -> Status {
        let clock = self.utc();
        let s = self.read();
        let l = s.loaded.as_deref();
        Status {
            on: l.is_some(),
            listening: s.listening,
            error: match (&s.error, &s.listen_error) {
                (Some(a), Some(b)) => Some(format!("{a} {b}")),
                (a, b) => a.clone().or_else(|| b.clone()),
            },
            ip: l.and_then(Loaded::server_ip),
            name: self.p.name.clone(),
            port: self.port,
            not_after: l.map(|l| l.server_info.not_after),
            ca_name: l.map(|l| l.ca.root_info.cn.clone()).unwrap_or_default(),
            covers: l.map(|l| l.ca.covers()).unwrap_or_default(),
            kept: s.kept.as_ref().map(|c| c.root_info.cn.clone()),
            sha256: l.map(|l| l.ca.sha256.clone()).unwrap_or_default(),
            sha1: l.map(|l| l.ca.sha1.clone()).unwrap_or_default(),
            renew_due: l.is_some_and(|l| l.server_info.not_after - clock.secs() < RENEW_BEFORE),
            clock,
        }
    }

    /// `GET /api/https` (docs/web-ui.md).
    pub fn status_json(&self) -> (u16, Value) {
        let s = self.status();
        let data = if s.on {
            json!({
                "enabled": true,
                "listening": s.listening.is_some(),
                "port": s.port,
                "ip": s.ip.map(|ip| ip.to_string()),
                "name": s.name,
                "urls": s.urls(),
                "not_after": s.not_after,
                "renew_due": s.renew_due,
                "ca_name": s.ca_name,
                "ca_sha256": s.sha256,
                "ca_covers": s.covers,
                "ca_crt": DOWNLOAD_CRT,
                "ca_mobileconfig": DOWNLOAD_PROFILE,
                "error": s.error,
            })
        } else {
            json!({"enabled": false, "port": s.port, "name": s.name, "error": s.error})
        };
        (200, json!({"ok": true, "data": data}))
    }
}

/// The device-trust downloads (public, `web::pages::https`).
pub const DOWNLOAD_CRT: &str = "/ui/https/u60-pro-ca.crt";
pub const DOWNLOAD_PROFILE: &str = "/ui/https/u60-pro-ca.mobileconfig";

/// What Settings and the API show.
#[derive(Clone, Debug)]
pub struct Status {
    pub on: bool,
    pub listening: Option<SocketAddr>,
    pub error: Option<String>,
    pub ip: Option<Ipv4Addr>,
    pub name: String,
    pub port: u16,
    pub not_after: Option<i64>,
    pub ca_name: String,
    /// What the CA may vouch for ("192.168.0.1", "u60.home.arpa").
    pub covers: Vec<String>,
    /// The CA kept while off (its server pair lost): Turn On reuses it.
    pub kept: Option<String>,
    pub sha256: String,
    pub sha1: String,
    pub clock: Utc,
    /// Fewer than 30 days left by the router's clock.
    pub renew_due: bool,
}

impl Status {
    /// `https://<ip>:<port>/ui/`, then the name's.
    pub fn urls(&self) -> Vec<String> {
        let port = if self.port == 443 { String::new() } else { format!(":{}", self.port) };
        self.ip.iter().map(|ip| ip.to_string()).chain([self.name.clone()]).map(|h| format!("https://{h}{port}/ui/")).collect()
    }
}

fn saving(e: std::io::Error) -> String {
    format!("Could not save the certificates: {e}")
}

/// `bytes` as `dir/name` with exactly `mode`: written beside it, synced,
/// renamed over it (the directory is created 0700).
fn save(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let tmp = dir.join(format!(".{name}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
    f.set_permissions(std::fs::Permissions::from_mode(mode))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, dir.join(name))?;
    std::fs::File::open(dir)?.sync_all()
}

/// The root's certificate, the issuing CA pair and the server pair's
/// files; `None` without a root certificate (HTTPS is off). The directory
/// and keys readable by others are made 0700 / 0600 again.
fn read_files(dir: &Path) -> std::io::Result<Option<(Vec<u8>, KeyCert, Server)>> {
    use std::os::unix::fs::PermissionsExt;
    let read = |name: &str| match std::fs::read(dir.join(name)) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    };
    let Some(root) = read(CA_CRT)? else { return Ok(None) };
    if std::fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let missing = |f: &str| std::io::Error::new(ErrorKind::NotFound, format!("{f} is missing"));
    let key = read(ISSUING_KEY)?.ok_or_else(|| missing(ISSUING_KEY))?;
    let cert = read(ISSUING_CRT)?.ok_or_else(|| missing(ISSUING_CRT))?;
    for k in [ISSUING_KEY, SERVER_KEY] {
        if let Ok(m) = std::fs::metadata(dir.join(k)) {
            if m.permissions().mode() & 0o077 != 0 {
                std::fs::set_permissions(dir.join(k), std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    let server = Server { key: read(SERVER_KEY)?, cert: read(SERVER_CRT)? };
    Ok(Some((root, KeyCert { key, cert }, server)))
}

/// The server pair's files as found (either may be missing).
struct Server {
    key: Option<Vec<u8>>,
    cert: Option<Vec<u8>>,
}

fn remove_files(dir: &Path) -> std::io::Result<()> {
    for name in [SERVER_CRT, SERVER_KEY, ISSUING_CRT, ISSUING_KEY, CA_CRT] {
        for f in [dir.join(name), dir.join(format!(".{name}.tmp"))] {
            match std::fs::remove_file(&f) {
                Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
    }
    match std::fs::remove_dir(dir) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A UUID-shaped id from the CA's digest: the same CA gives the same
/// profile (installing it again replaces it); a new CA a new one.
fn uuid(ca: &[u8], tag: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, &[ca, tag.as_bytes()].concat());
    let mut b = [0u8; 16];
    b.copy_from_slice(&d.as_ref()[..16]);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// An unsigned configuration profile with one `com.apple.security.root`
/// payload (the CA). iOS shows it as "Not Verified"; trust still has to
/// be turned on in Certificate Trust Settings.
pub fn mobileconfig(ca: &[u8], ca_name: &str, covers: &str, port: u16) -> String {
    use base64::Engine;
    let id = &x509::sha256_fingerprint(ca).replace(':', "")[..16];
    let data = base64::engine::general_purpose::STANDARD.encode(ca);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>PayloadContent</key>
	<array>
		<dict>
			<key>PayloadCertificateFileName</key>
			<string>u60-pro-ca.cer</string>
			<key>PayloadContent</key>
			<data>{data}</data>
			<key>PayloadDescription</key>
			<string>The router's own certificate authority.</string>
			<key>PayloadDisplayName</key>
			<string>{ca_name}</string>
			<key>PayloadIdentifier</key>
			<string>com.openu60.zte-agent.ca.{id}.root</string>
			<key>PayloadType</key>
			<string>com.apple.security.root</string>
			<key>PayloadUUID</key>
			<string>{root_uuid}</string>
			<key>PayloadVersion</key>
			<integer>1</integer>
		</dict>
	</array>
	<key>PayloadDescription</key>
	<string>Lets this device open the U60 Pro web UI over HTTPS (port {port}). The certificate authority can only vouch for {covers} (a name includes the names under it).</string>
	<key>PayloadDisplayName</key>
	<string>U60 Pro web UI (HTTPS)</string>
	<key>PayloadIdentifier</key>
	<string>com.openu60.zte-agent.ca.{id}</string>
	<key>PayloadOrganization</key>
	<string>OpenU60</string>
	<key>PayloadRemovalDisallowed</key>
	<false/>
	<key>PayloadType</key>
	<string>Configuration</string>
	<key>PayloadUUID</key>
	<string>{uuid}</string>
	<key>PayloadVersion</key>
	<integer>1</integer>
</dict>
</plist>
"#,
        ca_name = xml(ca_name),
        covers = xml(covers),
        root_uuid = uuid(ca, "root"),
        uuid = uuid(ca, "profile"),
    )
}

#[cfg(test)]
pub mod tests;
