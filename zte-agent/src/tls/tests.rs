//! The manager on temporary directories with fake clocks, a fake LAN
//! address and a fake dnsmasq.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::hosts::tests::{fake, temp};
use super::x509::tests::verify;
use super::*;
use crate::boottime::FakeClock;

pub const NOW: i64 = 1_793_000_000; // 2026-10-26

/// The fakes a test turns: LAN address, verified UTC, wall clock, boot clock.
#[derive(Clone)]
pub struct World {
    pub root: PathBuf,
    pub lan: Arc<Mutex<Option<Ipv4Addr>>>,
    pub verified: Arc<Mutex<Option<i64>>>,
    pub wall: Arc<AtomicI64>,
    pub boot: Arc<FakeClock>,
    pub signals: Arc<Mutex<Vec<i32>>>,
    /// The listener's address (`127.0.0.1:0` for the wire tests).
    pub bind: String,
}

impl World {
    pub fn new(name: &str) -> World {
        World {
            root: temp(name),
            lan: Arc::new(Mutex::new(Some(Ipv4Addr::new(192, 168, 0, 1)))),
            verified: Arc::new(Mutex::new(None)),
            wall: Arc::new(AtomicI64::new(NOW)),
            boot: FakeClock::at_secs(1000),
            signals: Arc::default(),
            bind: "127.0.0.1:0".into(),
        }
    }

    /// A manager over this world (several can share its files, as agent
    /// restarts do).
    pub fn tls(&self) -> Tls {
        let (mut hosts, signals) = fake(&self.root);
        let all = Arc::clone(&self.signals);
        hosts.signal = Box::new(move |pid| {
            signals.lock().unwrap().push(pid);
            all.lock().unwrap().push(pid);
            true
        });
        std::fs::write(&hosts.pid_file, "123").unwrap();
        let (lan, verified, wall) = (Arc::clone(&self.lan), Arc::clone(&self.verified), Arc::clone(&self.wall));
        Tls::new(Parts {
            dir: self.dir(),
            name: DEFAULT_NAME.into(),
            bind: self.bind.clone(),
            hosts,
            lan_ip: Box::new(move || *lan.lock().unwrap()),
            verified: Box::new(move || *verified.lock().unwrap()),
            wall: Box::new(move || wall.load(Ordering::SeqCst)),
            boot: self.boot.clone(),
        })
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join("data").join("zte-agent-tls")
    }

    pub fn hosts_file(&self) -> Option<String> {
        std::fs::read_to_string(self.root.join("hosts").join("zte-agent")).ok()
    }

    fn mode(&self, name: &str) -> u32 {
        std::fs::metadata(self.dir().join(name)).unwrap().permissions().mode() & 0o777
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if Arc::strong_count(&self.lan) == 1 {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn server_cert(w: &World) -> x509::Info {
    x509::parse(&std::fs::read(w.dir().join(SERVER_CRT)).unwrap()).unwrap()
}

fn key_ptr(t: &Tls) -> usize {
    Arc::as_ptr(t.keys().read().unwrap().as_ref().unwrap()) as *const () as usize
}

#[test]
fn constants_follow_the_design() {
    assert_eq!(CLOCK_FLOOR, crate::web::sms::days_from_civil(2026, 10, 1) * DAY);
    assert_eq!((SERVER_DAYS, RENEW_BEFORE, BACKDATE), (397, 30 * DAY, 2 * DAY));
    for ok in ["10.0.0.1", "10.255.255.255", "172.16.0.1", "172.31.255.255", "192.168.0.1"] {
        assert!(private(ok.parse().unwrap()), "{ok}");
    }
    for bad in ["9.255.255.255", "11.0.0.0", "172.15.255.255", "172.32.0.0", "192.169.0.1", "127.0.0.1", "100.64.0.1", "8.8.8.8"] {
        assert!(!private(bad.parse().unwrap()), "{bad}");
    }
    for ok in ["u60.home.arpa", "router.lan.home.arpa", "a-b.home.arpa"] {
        assert!(valid_name(ok), "{ok}");
    }
    for bad in ["home.arpa", ".home.arpa", "u60.home.arpa.", "u60.example.com", "U60.home.arpa", "u60.home.arpa:9443", "a..home.arpa", "evilhome.arpa", "1.2.3.4"] {
        assert!(!valid_name(bad), "{bad}");
    }
}

#[test]
fn clock_is_verified_unverified_or_unset() {
    let w = World::new("clock");
    let t = w.tls();
    assert_eq!(t.utc(), Utc::Unverified(NOW));
    w.wall.store(CLOCK_FLOOR - 1, Ordering::SeqCst);
    assert_eq!(t.utc(), Utc::Unset(CLOCK_FLOOR - 1));
    assert_eq!(t.utc().usable(), None);
    *w.verified.lock().unwrap() = Some(NOW + 5);
    assert_eq!(t.utc(), Utc::Verified(NOW + 5));
}

/// Turn on: the CA and server pairs on disk (directory 0700, keys 0600),
/// the server certificate for the LAN address and the name, valid 397 days
/// from two days back, the hosts line with a SIGHUP, the listener told.
#[test]
fn turn_on_creates_everything_once() {
    let w = World::new("on");
    let t = w.tls();
    let mut on = t.subscribe();
    assert!(!*on.borrow_and_update());
    assert!(!t.status().on && t.ca_der().is_none() && t.mobileconfig().is_none());
    t.turn_on().unwrap();
    assert!(on.has_changed().unwrap() && *on.borrow_and_update());
    assert_eq!(std::fs::metadata(w.dir()).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(
        [ISSUING_KEY, SERVER_KEY, CA_CRT, ISSUING_CRT, SERVER_CRT].map(|f| w.mode(f)),
        [0o600, 0o600, 0o644, 0o644, 0o644]
    );
    // Only these five: the root's key is never stored.
    let mut names: Vec<String> = std::fs::read_dir(w.dir()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["ca.crt", "issuing.crt", "issuing.key", "server.crt", "server.key"]);
    let s = server_cert(&w);
    assert_eq!((s.dns.clone(), s.ips.clone()), (vec![DEFAULT_NAME.to_owned()], vec![IpAddr::from([192, 168, 0, 1])]));
    assert_eq!((s.not_before, s.not_after), (NOW - 2 * DAY, NOW - 2 * DAY + 397 * DAY));
    let ca = t.ca_der().unwrap();
    assert_eq!(ca, std::fs::read(w.dir().join(CA_CRT)).unwrap());
    let server = std::fs::read(w.dir().join(SERVER_CRT)).unwrap();
    let issuing = std::fs::read(w.dir().join(ISSUING_CRT)).unwrap();
    verify(&ca, &[&server, &issuing], "192.168.0.1", NOW).unwrap();
    verify(&ca, &[&server, &issuing], "u60.home.arpa", NOW).unwrap();
    // The CA vouches for this router only (owner decision 59): the stored
    // issuing key cannot make another private address or home.arpa name
    // verify (what a stolen key could otherwise impersonate).
    assert_eq!(t.status().covers, ["192.168.0.1", "u60.home.arpa"]);
    let stored = x509::KeyCert { key: std::fs::read(w.dir().join(ISSUING_KEY)).unwrap(), cert: issuing.clone() };
    for (dns, ip, name) in [(&["nas.home.arpa"][..], &["192.168.5.5"][..], "nas.home.arpa"), (&[], &["192.168.0.2"], "192.168.0.2"), (&[], &["10.20.30.40"], "10.20.30.40")] {
        let forged = x509::tests::test_leaf(&stored, dns, ip);
        assert!(verify(&ca, &[&forged.cert, &issuing], name, NOW).is_err(), "{name}");
    }
    // No stored key is the root's.
    let root_point = x509::parse(&ca).unwrap().point;
    for k in [ISSUING_KEY, SERVER_KEY] {
        assert_ne!(x509::key_point(&std::fs::read(w.dir().join(k)).unwrap()), Some(root_point.clone()));
    }
    assert_eq!(w.hosts_file().as_deref(), Some("192.168.0.1 u60.home.arpa\n"));
    assert_eq!(*w.signals.lock().unwrap(), [123]);
    let st = t.status();
    assert!(st.on && st.ca_name.starts_with("U60 Pro local CA ") && st.ca_name.len() == 25);
    assert_eq!(st.sha256, x509::sha256_fingerprint(&ca));
    assert_eq!(st.urls(), ["https://192.168.0.1:0/ui/", "https://u60.home.arpa:0/ui/"]);
    assert!(!st.renew_due);
    // Again: nothing changes.
    let before = key_ptr(&t);
    t.turn_on().unwrap();
    assert_eq!(key_ptr(&t), before);
    assert_eq!(t.ca_der().unwrap(), ca);
    // The key files never hold anything but the keys' PKCS#8.
    let api = t.status_json().1.to_string();
    for k in [ISSUING_KEY, SERVER_KEY] {
        let key = std::fs::read(w.dir().join(k)).unwrap();
        assert!(x509::key_point(&key).is_some());
        assert!(!api.contains(&base64_of(&key)));
    }
}

fn base64_of(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

#[test]
fn turn_on_needs_a_set_clock_and_the_lan_address() {
    let w = World::new("refused");
    let t = w.tls();
    w.wall.store(CLOCK_FLOOR - DAY, Ordering::SeqCst);
    assert_eq!(t.turn_on().unwrap_err(), CLOCK_UNSET);
    assert!(!w.dir().exists());
    // The verified clock is enough on its own.
    *w.verified.lock().unwrap() = Some(NOW);
    *w.lan.lock().unwrap() = None;
    assert_eq!(t.turn_on().unwrap_err(), NO_LAN_IP);
    assert!(!w.dir().exists() && w.hosts_file().is_none());
    *w.lan.lock().unwrap() = Some(Ipv4Addr::new(10, 1, 1, 1));
    t.turn_on().unwrap();
    assert_eq!(server_cert(&w).not_before, NOW - 2 * DAY);
}

/// A LAN address outside the private ranges: the certificate names only
/// the local name (an address outside the constraints would make every
/// client refuse it), and Settings says why.
/// The CA then covers the name only and excludes every IPv4 address; the
/// name still maps to the address.
#[test]
fn a_public_lan_address_is_left_out() {
    let w = World::new("public");
    *w.lan.lock().unwrap() = Some(Ipv4Addr::new(100, 64, 0, 1));
    let t = w.tls();
    t.turn_on().unwrap();
    assert!(server_cert(&w).ips.is_empty());
    assert!(t.status().error.unwrap().contains("100.64.0.1 is not a private address"));
    assert_eq!(w.hosts_file().as_deref(), Some("100.64.0.1 u60.home.arpa\n"));
    assert_eq!(t.status().urls(), ["https://u60.home.arpa:0/ui/"]);
    assert_eq!(t.status().covers, ["u60.home.arpa"]);
    let (ca, issuing) = (t.ca_der().unwrap(), std::fs::read(w.dir().join(ISSUING_CRT)).unwrap());
    let stored = x509::KeyCert { key: std::fs::read(w.dir().join(ISSUING_KEY)).unwrap(), cert: issuing.clone() };
    let forged = x509::tests::test_leaf(&stored, &[], &["100.64.0.1"]);
    assert!(verify(&ca, &[&forged.cert, &issuing], "100.64.0.1", NOW).is_err());
}

/// After a restart (a new manager on the same files; /tmp emptied by the
/// reboot): loaded, the hosts line written again, the listener told.
#[test]
fn start_loads_and_writes_the_name_again() {
    let w = World::new("start");
    w.tls().turn_on().unwrap();
    let ca = std::fs::read(w.dir().join(CA_CRT)).unwrap();
    std::fs::remove_file(w.root.join("hosts").join("zte-agent")).unwrap();
    let t = w.tls();
    let on = t.subscribe();
    t.start();
    assert!(*on.borrow());
    assert_eq!(t.ca_der().unwrap(), ca);
    assert_eq!(w.hosts_file().as_deref(), Some("192.168.0.1 u60.home.arpa\n"));
    // Unchanged: no second signal.
    let n = w.signals.lock().unwrap().len();
    w.tls().start();
    assert_eq!(w.signals.lock().unwrap().len(), n);
    // Off: nothing loaded, a stale hosts line removed.
    let off = World::new("start-off");
    std::fs::create_dir_all(off.root.join("hosts")).unwrap();
    std::fs::write(off.root.join("hosts").join("zte-agent"), "192.168.0.1 u60.home.arpa\n").unwrap();
    let t = off.tls();
    t.start();
    assert!(!t.is_on() && !*t.subscribe().borrow() && off.hosts_file().is_none());
}

/// The directory and keys left readable by others are made 0700 / 0600
/// again at start; a CA that does not read leaves HTTPS off with an error
/// (Turn On makes a new one; Reset deletes the files).
#[test]
fn start_repairs_or_reports_its_files() {
    let w = World::new("repair");
    w.tls().turn_on().unwrap();
    std::fs::set_permissions(w.dir().join(ISSUING_KEY), std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(w.dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let t = w.tls();
    t.start();
    assert!(t.is_on());
    assert_eq!(w.mode(ISSUING_KEY), 0o600);
    assert_eq!(std::fs::metadata(w.dir()).unwrap().permissions().mode() & 0o777, 0o700);
    std::fs::write(w.dir().join(CA_CRT), b"not a certificate").unwrap();
    let t = w.tls();
    t.start();
    assert!(!t.is_on() && t.status().kept.is_none());
    let e = t.status().error.unwrap();
    assert!(e.contains("The saved certificate authority is unusable") && e.contains("Turn On HTTPS makes a new certificate authority"), "{e}");
    t.reset().unwrap();
    assert!(!w.dir().exists());
}

/// A server pair that a power cut or a full disk left behind (the new key
/// with the old certificate, a key or certificate missing or damaged): a
/// new pair from the same CA at start, dated as the old certificate (no
/// clock needed), so devices keep trusting it; without a readable old
/// certificate, by the verified clock; with neither, the CA is kept and
/// Turn On (owner-confirmed date) issues the pair from it. The router's
/// own clock is never used at start.
#[test]
fn start_issues_a_lost_server_pair_from_the_same_ca() {
    let w = World::new("lost-pair");
    let t = w.tls();
    t.turn_on().unwrap();
    let ca = t.ca_der().unwrap();
    let first = server_cert(&w);
    w.wall.store(0, Ordering::SeqCst);
    let restart = |w: &World| {
        let t = w.tls();
        let on = t.subscribe();
        t.start();
        (t, on)
    };
    // The new key saved, then the power cut before its certificate.
    let other = x509::leaf(&x509::KeyCert { key: std::fs::read(w.dir().join(ISSUING_KEY)).unwrap(), cert: std::fs::read(w.dir().join(ISSUING_CRT)).unwrap() }, &x509::LeafSpec { dns: &["u60.home.arpa"], ips: &[], not_before: NOW, not_after: NOW + DAY }).unwrap();
    std::fs::write(w.dir().join(SERVER_KEY), &other.key).unwrap();
    let (t, on) = restart(&w);
    assert!(t.is_on() && *on.borrow() && t.status().error.is_none(), "{:?}", t.status().error);
    assert_eq!(t.ca_der().unwrap(), ca);
    let s = server_cert(&w);
    assert_eq!((s.not_before, s.not_after, s.ips.clone()), (first.not_before, first.not_after, first.ips.clone()));
    assert_eq!(x509::key_point(&std::fs::read(w.dir().join(SERVER_KEY)).unwrap()), Some(s.point.clone()));
    // The key missing.
    std::fs::remove_file(w.dir().join(SERVER_KEY)).unwrap();
    let (t, _) = restart(&w);
    assert!(t.is_on());
    assert_eq!((server_cert(&w).not_before, t.ca_der().unwrap()), (first.not_before, ca.clone()));
    // The certificate damaged and no verified clock: off, the CA kept.
    std::fs::write(w.dir().join(SERVER_CRT), b"half a certificate").unwrap();
    w.wall.store(NOW + 5 * DAY, Ordering::SeqCst);
    let (t, on) = restart(&w);
    assert!(!t.is_on() && !*on.borrow());
    let st = t.status();
    assert_eq!((st.error.as_deref(), st.kept.as_deref().map(|k| k.starts_with("U60 Pro local CA "))), (Some(NO_PAIR), Some(true)));
    assert_eq!(std::fs::read(w.dir().join(SERVER_CRT)).unwrap(), b"half a certificate");
    // Turn On: the same CA, a pair dated from the owner-confirmed clock.
    t.turn_on().unwrap();
    assert!(t.is_on() && t.status().error.is_none() && t.status().kept.is_none());
    assert_eq!(t.ca_der().unwrap(), ca);
    assert_eq!(server_cert(&w).not_before, NOW + 3 * DAY);
    // Damaged again, with a verified clock: issued at start from it.
    std::fs::write(w.dir().join(SERVER_CRT), b"half a certificate").unwrap();
    *w.verified.lock().unwrap() = Some(NOW + 9 * DAY);
    let (t, on) = restart(&w);
    assert!(t.is_on() && *on.borrow());
    assert_eq!((server_cert(&w).not_before, t.ca_der().unwrap()), (NOW + 7 * DAY, ca));
}

/// Turn On while on but not listening (a bind that failed): the listener
/// is told again (it binds again); while listening, nothing.
#[test]
fn turn_on_again_retries_a_failed_bind() {
    let w = World::new("rebind");
    let t = w.tls();
    t.turn_on().unwrap();
    let mut on = t.subscribe();
    on.borrow_and_update();
    t.set_listening(Err("Could not listen on 0.0.0.0:9443: in use".into()));
    t.turn_on().unwrap();
    assert!(on.has_changed().unwrap() && *on.borrow_and_update());
    t.set_listening(Ok("0.0.0.0:9443".parse().unwrap()));
    t.turn_on().unwrap();
    assert!(!on.has_changed().unwrap());
}

/// Lazy renewal with fake clocks: the router's clock alone never renews
/// (Settings offers Renew instead); the verified clock does, at a TLS
/// connection's check, and the listener's key is swapped in place.
#[test]
fn renewal_needs_the_verified_clock() {
    let w = World::new("renew");
    let t = w.tls();
    t.turn_on().unwrap();
    let first = server_cert(&w);
    let local = IpAddr::from([192, 168, 0, 1]);
    // 20 days left by the router's own clock.
    let late = first.not_after - 20 * DAY;
    w.wall.store(late, Ordering::SeqCst);
    assert!(t.status().renew_due);
    assert!(t.wants_check(local));
    // At most once a minute (boot clock).
    assert!(!t.wants_check(local));
    t.check();
    assert_eq!(server_cert(&w).not_after, first.not_after, "renewed on an unverified clock");
    w.boot.advance(Duration::from_secs(61));
    *w.verified.lock().unwrap() = Some(late);
    assert!(t.wants_check(local));
    let before = key_ptr(&t);
    t.check();
    let renewed = server_cert(&w);
    assert_eq!((renewed.not_before, renewed.not_after), (late - 2 * DAY, late - 2 * DAY + 397 * DAY));
    assert_ne!(key_ptr(&t), before);
    assert!(!t.status().renew_due);
    let ca = t.ca_der().unwrap();
    let chain = [std::fs::read(w.dir().join(SERVER_CRT)).unwrap(), std::fs::read(w.dir().join(ISSUING_CRT)).unwrap()];
    verify(&ca, &[&chain[0], &chain[1]], "u60.home.arpa", late + 300 * DAY).unwrap();
    // Nothing to do: no check wanted.
    w.boot.advance(Duration::from_secs(61));
    assert!(!t.wants_check(local));
    // Owner-confirmed Renew dates from the router's clock.
    *w.verified.lock().unwrap() = None;
    w.wall.store(late + 10 * DAY, Ordering::SeqCst);
    t.renew().unwrap();
    assert_eq!(server_cert(&w).not_before, late + 8 * DAY);
    w.wall.store(CLOCK_FLOOR - 1, Ordering::SeqCst);
    assert_eq!(t.renew().unwrap_err(), CLOCK_UNSET);
}

/// A new LAN address (the LAN apply, a TLS connection to it, or a
/// restart): the CA covers only the address it was made for, so the
/// certificate is issued again for the name alone, with the same dates (no
/// clock needed); the hosts line follows the address and Settings says
/// why. Back at the old address: the address again and no line. Loopback,
/// the certificate's and the known LAN address never ask for a check.
#[test]
fn a_new_lan_address_keeps_the_name_working() {
    let w = World::new("moved");
    let t = w.tls();
    t.turn_on().unwrap();
    let first = server_cert(&w);
    w.wall.store(0, Ordering::SeqCst);
    t.lan_changed(Ipv4Addr::new(192, 168, 8, 1));
    let s = server_cert(&w);
    assert_eq!((s.dns.clone(), s.ips.clone()), (vec![DEFAULT_NAME.to_owned()], vec![]));
    assert_eq!((s.not_before, s.not_after), (first.not_before, first.not_after));
    assert_eq!(w.hosts_file().as_deref(), Some("192.168.8.1 u60.home.arpa\n"));
    let st = t.status();
    assert_eq!(st.urls(), ["https://u60.home.arpa:0/ui/"]);
    let e = st.error.unwrap();
    assert!(e.contains("now 192.168.8.1, but its certificate authority was made for 192.168.0.1") && e.contains("Reset HTTPS"), "{e}");
    let ca = t.ca_der().unwrap();
    let chain = [std::fs::read(w.dir().join(SERVER_CRT)).unwrap(), std::fs::read(w.dir().join(ISSUING_CRT)).unwrap()];
    verify(&ca, &[&chain[0], &chain[1]], "u60.home.arpa", NOW).unwrap();
    // A connection to an address neither the certificate's nor the known
    // LAN address: the address is read again.
    w.wall.store(NOW, Ordering::SeqCst);
    for local in ["127.0.0.1", "::1", "192.168.8.1", "100.64.0.1"] {
        assert!(!t.wants_check(local.parse().unwrap()), "{local}");
    }
    *w.lan.lock().unwrap() = Some(Ipv4Addr::new(10, 0, 0, 1));
    assert!(t.wants_check("::ffff:10.0.0.1".parse().unwrap()));
    t.check();
    assert_eq!(w.hosts_file().as_deref(), Some("10.0.0.1 u60.home.arpa\n"));
    assert!(server_cert(&w).ips.is_empty() && t.status().error.unwrap().contains("now 10.0.0.1"));
    // Back: the address again, same dates, the line cleared.
    t.lan_changed(Ipv4Addr::new(192, 168, 0, 1));
    let s = server_cert(&w);
    assert_eq!((s.ips.clone(), s.not_after), (vec![IpAddr::from([192, 168, 0, 1])], first.not_after));
    assert_eq!((t.status().error, w.hosts_file().as_deref()), (None, Some("192.168.0.1 u60.home.arpa\n")));
    // A public address and back: the line comes and goes.
    t.lan_changed(Ipv4Addr::new(100, 64, 0, 1));
    assert!(t.status().error.unwrap().contains("100.64.0.1 is not a private address"));
    t.lan_changed(Ipv4Addr::new(192, 168, 0, 1));
    assert_eq!(t.status().error, None);
    // At start too.
    *w.lan.lock().unwrap() = Some(Ipv4Addr::new(172, 16, 5, 1));
    let t = w.tls();
    t.start();
    assert!(t.is_on() && server_cert(&w).ips.is_empty());
    assert_eq!(w.hosts_file().as_deref(), Some("172.16.5.1 u60.home.arpa\n"));
    assert!(t.status().error.unwrap().contains("now 172.16.5.1"));
}

/// A renewal never ends after the issuing CA; once it cannot end later
/// than the current certificate, it is not done again.
#[test]
fn renewal_never_outlasts_the_ca() {
    let w = World::new("ca-end");
    let t = w.tls();
    t.turn_on().unwrap();
    let ca_end = x509::parse(&std::fs::read(w.dir().join(ISSUING_CRT)).unwrap()).unwrap().not_after;
    *w.verified.lock().unwrap() = Some(ca_end - 100 * DAY);
    t.check();
    assert_eq!(server_cert(&w).not_after, ca_end);
    *w.verified.lock().unwrap() = Some(ca_end - 20 * DAY);
    let before = key_ptr(&t);
    t.check();
    assert_eq!(key_ptr(&t), before);
}

/// Reset: the listener told, every file and the directory gone, the hosts
/// line removed (with a SIGHUP), nothing to download; on again: a new CA.
#[test]
fn reset_deletes_everything() {
    let w = World::new("reset");
    let t = w.tls();
    t.turn_on().unwrap();
    let old = t.ca_der().unwrap();
    let mut on = t.subscribe();
    on.borrow_and_update();
    t.reset().unwrap();
    assert!(on.has_changed().unwrap() && !*on.borrow_and_update());
    assert!(!w.dir().exists() && w.hosts_file().is_none());
    assert!(t.keys().read().unwrap().is_none());
    assert!(!t.status().on && t.ca_der().is_none() && t.mobileconfig().is_none());
    assert_eq!(*w.signals.lock().unwrap(), [123, 123]);
    assert_eq!(t.status_json().1["data"], serde_json::json!({"enabled": false, "port": 0, "name": "u60.home.arpa", "error": null}));
    t.reset().unwrap();
    t.turn_on().unwrap();
    assert_ne!(t.ca_der().unwrap(), old);
}

#[test]
fn status_json_for_the_apps() {
    let w = World::new("json");
    let t = w.tls();
    t.turn_on().unwrap();
    t.set_listening(Ok("0.0.0.0:9443".parse().unwrap()));
    let (status, v) = t.status_json();
    let d = &v["data"];
    assert_eq!(status, 200);
    assert_eq!((d["enabled"].as_bool(), d["listening"].as_bool(), d["ip"].as_str()), (Some(true), Some(true), Some("192.168.0.1")));
    assert_eq!(d["urls"], serde_json::json!(["https://192.168.0.1:0/ui/", "https://u60.home.arpa:0/ui/"]));
    assert_eq!(d["not_after"].as_i64(), Some(NOW - 2 * DAY + 397 * DAY));
    assert_eq!(d["ca_sha256"].as_str().map(str::len), Some(95));
    assert_eq!(d["ca_covers"], serde_json::json!(["192.168.0.1", "u60.home.arpa"]));
    assert_eq!((d["ca_crt"].as_str(), d["ca_mobileconfig"].as_str()), (Some(DOWNLOAD_CRT), Some(DOWNLOAD_PROFILE)));
    t.set_listening(Err("could not listen on 0.0.0.0:9443: in use".into()));
    let d = t.status_json().1["data"].clone();
    assert_eq!((d["listening"].as_bool(), d["error"].as_str()), (Some(false), Some("could not listen on 0.0.0.0:9443: in use")));
    // A certificate problem stays when the listener starts.
    t.lan_changed(Ipv4Addr::new(100, 64, 0, 1));
    t.set_listening(Ok("0.0.0.0:9443".parse().unwrap()));
    assert!(t.status().error.unwrap().contains("100.64.0.1 is not a private address"));
}

/// The profile: one root payload holding the CA, ids from the CA (the same
/// CA gives the same profile).
#[test]
fn mobileconfig_holds_the_ca() {
    let w = World::new("profile");
    let t = w.tls();
    t.turn_on().unwrap();
    let ca = t.ca_der().unwrap();
    let p = t.mobileconfig().unwrap();
    assert!(p.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
    assert!(p.contains(&format!("<data>{}</data>", base64_of(&ca))));
    assert_eq!(p.matches("<string>com.apple.security.root</string>").count(), 1);
    assert!(p.contains("can only vouch for 192.168.0.1 and u60.home.arpa (a name includes the names under it).</string>"));
    assert!(p.contains("<string>Configuration</string>") && p.contains(&format!("<string>{}</string>", t.status().ca_name)));
    assert_eq!(p, t.mobileconfig().unwrap());
    let (a, b) = (uuid(&ca, "root"), uuid(&ca, "profile"));
    assert!(a != b && a.len() == 36 && &a[14..15] == "4");
    assert!(!p.contains("PRIVATE") && !p.contains(&base64_of(&std::fs::read(w.dir().join(ISSUING_KEY)).unwrap())));
}
