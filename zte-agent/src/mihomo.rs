//! Optional Mihomo process and LAN-only, dual-stack TUN interception.
//! The agent owns lifecycle/routing; the separately distributed core owns
//! proxy protocols. No enhanced/GPL implementation is incorporated here.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serde_yaml_ng::{Mapping, Value as Yaml};
use sha2::{Digest, Sha256};

use crate::net_tools::{self as net, Cmd, NetworkLock, Step, Transaction};

pub mod control;
pub mod dashboard;
mod dns;
mod download;
mod geodata;
pub mod profiles;
pub mod transform;

pub const VERSION: &str = "1.19.33";
pub const DASHBOARD_VERSION: &str = "3.29.1";
pub const CONTROLLER: &str = "127.0.0.1:19090";
pub const ROOT: &str = "/data/zte-agent-mihomo";
/// Present while the owner wants Mihomo on (`restore`).
const WANTED: &str = "enabled";
pub const DASHBOARD: &str = "/ui/mihomo/dashboard/";
pub const API: &str = "/ui/mihomo/api";
const CORE_URL: &str =
    "https://github.com/MetaCubeX/mihomo/releases/download/v1.19.33/mihomo-linux-arm64-v1.19.33.gz";
const CORE_SHA: &str = "ba408b317080ee265c46cb923a50018c1b82cbd708551cdd46a1e3e307396187";
const UI_URL: &str =
    "https://github.com/Zephyruso/zashboard/releases/download/v3.29.1/dist-no-fonts.zip";
const UI_SHA: &str = "21371cd111b6b3d87774f3ea3ddeacdcb0f6aff8690a16d652daa3ea6e3b0145";
const MARK: &str = "0x600/0xff00";
const TABLE: &str = "60630";
const PREF: &str = "18630";
/// Sends the TUN's own subnet to `main`, ahead of Relay's catch-all table
/// (18640): the kernel TCP stack answers its clients from that subnet.
const LOCAL_PREF: &str = "18629";
const TUN: &str = "u60mh0";
const TUN4: &str = "198.18.0.1";
const TUN6: &str = "fdfe:dcba:9876::1";
pub const MAX_CONFIG: usize = 512 * 1024;

struct Running {
    child: Option<Child>,
    network: Transaction,
    secret: String,
    local_name: String,
    runtime: Value,
    local_addresses: Vec<std::net::IpAddr>,
    rules_checked: Option<Instant>,
    /// Egress this core's connections use: observed just before it started,
    /// then updated in place by non-stranding watchdog observations.
    uplink: Uplink,
    /// A stranding egress the watchdog saw, and since when it has held.
    stranding: Option<(Uplink, Instant)>,
}

impl Running {
    fn alive(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }
}

/// The relay's station interface (`wifi_relay::IFACE`). The relay announces
/// changes of its own path through `uplink_changed_locked`.
const RELAY_IFACE: &str = "u60sta0";
/// A stranding egress change must stay identical this long before the
/// watchdog restarts the core: a flapping uplink restarts nothing until it settles.
const EGRESS_SETTLE: Duration = Duration::from_secs(5);
/// At most one watchdog egress restart per window. Relay notifications and
/// owner operations are not limited.
const EGRESS_RESTART_GAP: Duration = Duration::from_secs(60);

/// Effective egress per family (IPv4, IPv6), excluding timers/cache counters
/// and unrelated local IPs. `None`: no readable route or source address set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Uplink {
    paths: [Option<Egress>; 2],
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Egress {
    route: RouteIdentity,
    source: SourceIdentity,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct RouteIdentity {
    device: String,
    gateway: Option<String>,
    source: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceIdentity {
    ifindex: u64,
    addresses: Vec<(std::net::IpAddr, u8)>,
}
impl Uplink {
    fn route(text: &str) -> Option<RouteIdentity> {
        let routes: Value = serde_json::from_str(text).ok()?;
        let route = routes.as_array()?.first()?;
        Some(RouteIdentity {
            device: route["dev"].as_str()?.into(),
            gateway: route["gateway"].as_str().map(str::to_owned),
            source: route["prefsrc"]
                .as_str()
                .or_else(|| route["src"].as_str())
                .map(str::to_owned),
        })
    }
    fn read(routes: [Option<RouteIdentity>; 2], text: &str) -> Self {
        let links: Option<Value> = serde_json::from_str(text).ok();
        let source = |route: &RouteIdentity, family: &str| -> Option<SourceIdentity> {
            let link = links
                .as_ref()?
                .as_array()?
                .iter()
                .find(|v| v["ifname"].as_str() == Some(&route.device))?;
            let mut addresses = Vec::new();
            for addr in link["addr_info"]
                .as_array()?
                .iter()
                .filter(|a| a["family"].as_str() == Some(family))
            {
                let ip: std::net::IpAddr = addr["local"].as_str()?.parse().ok()?;
                let prefix = addr["prefixlen"].as_u64()?;
                if ip.is_ipv4() != (family == "inet")
                    || prefix > if ip.is_ipv4() { 32 } else { 128 }
                {
                    return None;
                }
                addresses.push((ip, prefix as u8));
            }
            if addresses.is_empty() {
                return None;
            }
            addresses.sort_unstable();
            addresses.dedup();
            Some(SourceIdentity {
                ifindex: link["ifindex"].as_u64()?,
                addresses,
            })
        };
        let path = |route: Option<RouteIdentity>, family: &str| -> Option<Egress> {
            let route = route?;
            let source = source(&route, family)?;
            Some(Egress { route, source })
        };
        let [v4, v6] = routes;
        Self {
            paths: [path(v4, "inet"), path(v6, "inet6")],
        }
    }
    fn observe() -> Self {
        let routes = [("-4", "1.1.1.1"), ("-6", "2606:4700:4700::1111")].map(|(family, to)| {
            Cmd::new("ip", &["-j", family, "route", "get", to])
                .run()
                .ok()
                .and_then(|s| Self::route(&s))
        });
        let addresses = Cmd::new("ip", &["-j", "addr", "show"])
            .run()
            .unwrap_or_default();
        Self::read(routes, &addresses)
    }
    /// An unreadable/offline family is not evidence for skipping a restart:
    /// only a fully known dual-stack egress can make a notification redundant.
    fn complete(&self) -> bool {
        self.paths.iter().all(Option::is_some)
    }
    /// Connections opened on `earlier` cannot leave through this egress any
    /// more: a readable family moved to another interface (name or index) or
    /// lost a source address it had. Added addresses, gateway or preferred
    /// source changes and a newly available family keep them usable; an
    /// unreadable family is no evidence (a failed read, or a lost uplink).
    fn strands(&self, earlier: &Self) -> bool {
        self.paths
            .iter()
            .zip(&earlier.paths)
            .any(|(now, then)| match (now, then) {
                (Some(now), Some(then)) => {
                    now.route.device != then.route.device
                        || now.source.ifindex != then.source.ifindex
                        || then
                            .source
                            .addresses
                            .iter()
                            .any(|(ip, _)| !now.source.addresses.iter().any(|(kept, _)| kept == ip))
                }
                _ => false,
            })
    }
    /// Takes readable families as the new reference. An unreadable family
    /// keeps the last one, so its return on another address still strands.
    fn adopt(&mut self, observed: &Self) {
        for (kept, now) in self.paths.iter_mut().zip(&observed.paths) {
            if now.is_some() {
                kept.clone_from(now);
            }
        }
    }
    fn relayed(&self) -> bool {
        self.paths
            .iter()
            .flatten()
            .any(|path| path.route.device == RELAY_IFACE)
    }
}

/// One watchdog egress observation. `uplink` is the egress the running core's
/// connections use, `stranding` a stranding observation and when it was first
/// seen. True: restart the core now.
fn settle_egress(
    uplink: &mut Uplink,
    stranding: &mut Option<(Uplink, Instant)>,
    observed: Uplink,
    last_restart: Option<Instant>,
    now: Instant,
) -> bool {
    if uplink.relayed() || observed.relayed() {
        *stranding = None;
        return false;
    }
    if !observed.strands(uplink) {
        uplink.adopt(&observed);
        *stranding = None;
        return false;
    }
    match stranding {
        Some((seen, since)) if *seen == observed => restart_due(*since, last_restart) <= now,
        _ => {
            *stranding = Some((observed, now));
            false
        }
    }
}

fn restart_due(since: Instant, last_restart: Option<Instant>) -> Instant {
    let settled = since + EGRESS_SETTLE;
    last_restart.map_or(settled, |at| settled.max(at + EGRESS_RESTART_GAP))
}

struct RestartState {
    local: String,
    runtime: Value,
    stale: bool,
}

struct Inner {
    run: Option<Running>,
    error: Option<String>,
    /// Any owner switch supersedes this process's boot restoration.
    restore_cancelled: bool,
    /// Last watchdog egress restart; the rate limit spans core restarts.
    egress_restarted: Option<Instant>,
}

pub struct Mihomo {
    root: PathBuf,
    lock: NetworkLock,
    operation: Mutex<()>,
    profile_operation: Mutex<()>,
    profile_wake: Condvar,
    profile_revision: Mutex<u64>,
    library_revision: AtomicU64,
    status_cache: Mutex<Option<(profiles::LibraryStamp, Value)>>,
    /// Fingerprint (secret masked) of the last configuration that passed
    /// `mihomo -t` and then started; an identical restart skips the check.
    validated_config: Mutex<Option<String>>,
    digests: geodata::Digests,
    subscription_dns: Mutex<Vec<std::net::Ipv4Addr>>,
    passes: dashboard::Passes,
    inner: Mutex<Inner>,
    wake: Condvar,
}

impl Mihomo {
    pub fn new(lock: NetworkLock) -> Self {
        Self::at(PathBuf::from(ROOT), lock)
    }

    fn at(root: PathBuf, lock: NetworkLock) -> Self {
        Self {
            root,
            lock,
            operation: Mutex::new(()),
            profile_operation: Mutex::new(()),
            profile_wake: Condvar::new(),
            profile_revision: Mutex::new(0),
            library_revision: AtomicU64::new(0),
            status_cache: Mutex::new(None),
            validated_config: Mutex::new(None),
            digests: geodata::Digests::default(),
            subscription_dns: Mutex::new(Vec::new()),
            passes: dashboard::Passes::default(),
            inner: Mutex::new(Inner {
                run: None,
                error: None,
                restore_cancelled: false,
                egress_restarted: None,
            }),
            wake: Condvar::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_bridge(root: PathBuf, lock: NetworkLock, secret: String) -> Self {
        let this = Self::at(root.clone(), lock);
        this.inner.lock().unwrap().run = Some(Running {
            child: None,
            network: Transaction::new(root.join("network.json")),
            secret,
            local_name: "u60.home.arpa".into(),
            runtime: json!({"mode":"rule","log-level":"info"}),
            local_addresses: Vec::new(),
            rules_checked: None,
            uplink: Uplink::default(),
            stranding: None,
        });
        this
    }

    fn journal(&self) -> PathBuf {
        self.root.join("network.json")
    }

    pub fn status(&self) -> Value {
        self.status_with(|| self.profile_status())
    }

    fn status_with(&self, profiles: impl FnOnce() -> Result<Value, String>) -> Value {
        let installed = self.root.join("mihomo").is_file();
        let configured = profiles().ok().is_some_and(|p| p["selected"].is_string());
        let journal = self.journal().exists();
        let i = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        json!({"installed": installed, "configured": configured,
            "running": i.run.is_some(), "version": VERSION, "dashboard_version": DASHBOARD_VERSION,
            "dashboard": if i.run.is_some() { Some(DASHBOARD) } else { None },
            "scope": "lan_tcp_udp_dual_stack_tun", "experimental": true, "error": i.error,
            "autostart": self.wants_restore(), "recovery_required": i.run.is_none() && journal})
    }

    pub fn running(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .run
            .is_some()
    }

    pub fn owns_network(&self) -> bool {
        self.running() || self.journal().exists()
    }

    pub fn secret(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .run
            .as_ref()
            .map(|r| r.secret.clone())
    }

    pub fn dashboard_root(&self) -> PathBuf {
        self.root.join("ui")
    }

    pub fn recover(&self) -> Result<(), String> {
        Transaction::recover(&self.journal())
    }

    /// Disabled at boot. Recovery runs once; the watchdog parks while off.
    pub fn start(self: &Arc<Self>) {
        let this = Arc::clone(self);
        crate::system::spawn_named("mihomo-watch", move || {
            {
                let _lock = this
                    .lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                // Never roll back a core enabled before the watcher was scheduled.
                if !this.running() {
                    if let Err(e) = this.recover() {
                        this.inner.lock().unwrap().error = Some(e);
                    }
                }
            }
            let mut i = this
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if i.run.is_none() {
                    i = this
                        .wake
                        .wait(i)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    continue;
                }
                let (next, _) = this
                    .wake
                    .wait_timeout(i, Duration::from_secs(2))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                drop(next);
                let _network = this
                    .lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                i = this
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let dead = i.run.as_mut().is_some_and(|r| !r.alive());
                if dead {
                    drop(i);
                    let error = this
                        .disable_locked()
                        .err()
                        .unwrap_or_else(|| "Mihomo exited; proxy interception was removed".into());
                    i = this
                        .inner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    i.error = Some(error);
                } else if i.run.is_some() {
                    drop(i);
                    let refreshed = this.refresh_local_locked();
                    i = this
                        .inner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Err(error) = refreshed {
                        i.error = Some(error);
                    }
                }
            }
        });
    }

    // B31 cannot match the live FIB LOCAL type. Reuse the active watchdog to
    // refresh exact-address exemptions in place, including vendor IPv6
    // renumbering: an address change alone (Tailscale, LAN, a non-egress
    // interface) keeps the core running. Outside the relay (cellular) only an
    // egress that strands the core's connections restarts it: another
    // interface or a lost source address, settled for `EGRESS_SETTLE` and at
    // most once per `EGRESS_RESTART_GAP`. The relay announces changes of its
    // own path (`uplink_changed_locked`).
    fn refresh_local_locked(&self) -> Result<(), String> {
        let local = local_addresses()?;
        // The caller's network lock serializes every user of `run.network`, so
        // the journal leaves `inner` while device commands run: status, secret
        // and dashboard requests never wait for iptables or a journal fsync.
        let audit = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let last_restart = inner.egress_restarted;
            let Some(run) = inner.run.as_mut() else {
                return Ok(());
            };
            let audit = run.local_addresses != local
                || !run
                    .rules_checked
                    .is_some_and(|at| at.elapsed() < net::EXEMPTION_AUDIT);
            let settled = run
                .stranding
                .as_ref()
                .is_some_and(|(_, since)| restart_due(*since, last_restart) <= Instant::now());
            if !audit && !settled {
                return Ok(());
            }
            audit.then(|| {
                let network = std::mem::replace(&mut run.network, Transaction::new(PathBuf::new()));
                (network, run.local_addresses.clone())
            })
        };
        if let Some((mut network, previous)) = audit {
            let synced = sync_local(&mut network, &local, &previous);
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(run) = inner.run.as_mut() else {
                // Unreachable under the network lock; never lose the journal.
                drop(inner);
                return network.rollback().and(synced);
            };
            run.network = network;
            if synced.is_ok() {
                run.local_addresses = local;
                run.rules_checked = Some(Instant::now());
            }
            drop(inner);
            synced?;
        }
        let observed = Uplink::observe();
        let restart = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let last_restart = inner.egress_restarted;
            let Some(run) = inner.run.as_mut() else {
                return Ok(());
            };
            settle_egress(
                &mut run.uplink,
                &mut run.stranding,
                observed,
                last_restart,
                Instant::now(),
            )
        };
        if !restart {
            return Ok(());
        }
        // An owner operation restarts or stops the core itself; the next
        // watchdog tick retries while the same stranding egress holds.
        let Ok(_operation) = self.operation.try_lock() else {
            return Ok(());
        };
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .egress_restarted = Some(Instant::now());
        eprintln!(
            "[mihomo] egress left the core's interface or source address; restarting the core"
        );
        self.restart_locked()
    }

    pub fn install(&self) -> Result<(), String> {
        let _profiles = self
            .profile_operation
            .try_lock()
            .map_err(|_| "A profile operation is in progress")?;
        // Downloads must not park the relay watchdog behind the network lock.
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "A Mihomo operation is in progress")?;
        if self.running() {
            return Err("Turn off Mihomo before installing".into());
        }
        *self
            .validated_config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        net::private_dir(&self.root)?;
        let core = self.download(CORE_URL, CORE_SHA, 32 * 1024 * 1024)?;
        let mut binary = Vec::new();
        flate2::read::GzDecoder::new(core.as_slice())
            .take(100 * 1024 * 1024 + 1)
            .read_to_end(&mut binary)
            .map_err(|_| "Cannot decompress Mihomo")?;
        if binary.len() > 100 * 1024 * 1024 || !binary.starts_with(b"\x7fELF") {
            return Err("Invalid Mihomo binary".into());
        }
        let ui = self.download(UI_URL, UI_SHA, 8 * 1024 * 1024)?;
        let core_license = self.download(
            "https://raw.githubusercontent.com/MetaCubeX/mihomo/v1.19.33/LICENSE",
            "3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986",
            64 * 1024,
        )?;
        let ui_license = self.download(
            "https://raw.githubusercontent.com/Zephyruso/zashboard/v3.29.1/LICENSE",
            "ffc390eec51f16b58506b1a42c99fe0cd9ffea1e3081aab443100fd8c20182d9",
            8 * 1024,
        )?;
        net::write_private(&self.root.join("LICENSE-Mihomo-GPL-3.0.txt"), &core_license)?;
        net::write_private(&self.root.join("LICENSE-Zashboard-MIT.txt"), &ui_license)?;
        net::write_private(&self.root.join("SOURCES.txt"), b"Mihomo v1.19.33 (GPL-3.0): https://github.com/MetaCubeX/mihomo/tree/v1.19.33\nZashboard v3.29.1 (MIT): https://github.com/Zephyruso/zashboard/tree/v3.29.1\n")?;
        let stage = self.root.join("ui.new");
        if stage.exists() {
            fs::remove_dir_all(&stage).map_err(|e| e.to_string())?;
        }
        extract_ui(&ui, &stage)?;
        let ui_path = self.root.join("ui");
        let old = self.root.join("ui.previous");
        if old.exists() {
            fs::remove_dir_all(&old).map_err(|e| e.to_string())?;
        }
        if ui_path.exists() {
            fs::rename(&ui_path, &old).map_err(|e| e.to_string())?;
        }
        if let Err(e) = fs::rename(&stage, &ui_path) {
            let _ = fs::rename(&old, &ui_path);
            return Err(e.to_string());
        }
        net::write_private(&self.root.join("mihomo"), &binary)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(self.root.join("mihomo"), fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn configure(&self, source: &str) -> Result<(), String> {
        drop(
            self.operation
                .try_lock()
                .map_err(|_| "A Mihomo operation is in progress")?,
        );
        if self.running() {
            return Err("Turn off Mihomo before replacing its configuration".into());
        }
        self.profiles_change(profiles::Request {
            action: "replace-current".into(),
            id: String::new(),
            name: "Pasted configuration".into(),
            url: String::new(),
            yaml: source.into(),
            interval_hours: 0,
            extensions: transform::Extensions::default(),
            source_type: String::new(),
        })
    }

    fn validation_fingerprint(&self, config: &str, secret: &str) -> Option<String> {
        use std::os::unix::fs::MetadataExt;
        let core = fs::metadata(self.root.join("mihomo")).ok()?;
        let dependencies = geodata::validation_inputs(&self.root, config, &self.digests)?;
        Some(format!(
            "{}:{dependencies}:{}:{}:{}:{}:{}",
            config_fingerprint(config, secret),
            core.dev(),
            core.ino(),
            core.len(),
            core.mtime(),
            core.mtime_nsec()
        ))
    }

    fn check(&self, name: &str) -> Result<(), String> {
        self.check_with(name, false)
    }

    /// `background` keeps a validation that runs beside a live core (profile
    /// refresh) from competing with forwarding: one Go execution slot, a soft memory
    /// limit for the Go runtime, and a lower scheduling priority.
    fn check_with(&self, name: &str, background: bool) -> Result<(), String> {
        let mut c = Command::new(self.root.join("mihomo"));
        c.env("SKIP_SYSTEM_IPV6_CHECK", "true");
        if background {
            use std::os::unix::process::CommandExt;
            c.env("GOMAXPROCS", "1").env("GOMEMLIMIT", "128MiB");
            // SAFETY: only the async-signal-safe setpriority(2) runs before exec.
            unsafe {
                c.pre_exec(|| {
                    libc::setpriority(libc::PRIO_PROCESS as _, 0, 10);
                    Ok(())
                });
            }
        }
        c.args(["-t", "-d"])
            .arg(&self.root)
            .arg("-f")
            .arg(self.root.join(name));
        let started = Instant::now();
        let result = net::run(c, Duration::from_secs(30));
        let outcome = match &result {
            Ok(_) => "ok",
            Err(error) if error == "Device tool timed out" => "timeout",
            Err(_) => "rejected",
        };
        eprintln!(
            "[mihomo] validation background={background} elapsed_ms={} outcome={outcome}",
            started.elapsed().as_millis()
        );
        result.map(|_| ())
            .map_err(|error| {
                if error == "Device tool timed out" {
                    "Mihomo validation timed out; a provider or rule database may be unavailable. Check Internet access and retry; the saved configuration is unchanged".into()
                } else {
                    "Mihomo could not validate the configuration; check YAML, nodes, providers and rule databases".into()
                }
            })
    }

    pub fn enable(&self, local_name: &str) -> Result<(), String> {
        self.cancel_restore();
        let _lock = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "A Mihomo operation is in progress")?;
        let result = self.enable_locked(local_name);
        // The owner's choice outlives this process (`restore`).
        let wanted = match &result {
            Ok(()) => net::write_private(&self.root.join(WANTED), b"1"),
            Err(_) => self.forget_wanted(),
        };
        match (result, wanted) {
            (Ok(()), Err(e)) => {
                self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).error =
                    Some(format!("Mihomo is on, but it will stay off after a restart: {e}"));
                Ok(())
            }
            (Err(e), Err(clear)) => {
                let error = format!("{e}; {clear}. The saved restart choice could not be removed");
                self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).error = Some(error.clone());
                Err(error)
            }
            (result, _) => result,
        }
    }

    pub fn wants_restore(&self) -> bool {
        self.root.join(WANTED).is_file()
    }

    fn forget_wanted(&self) -> Result<(), String> {
        match std::fs::remove_file(self.root.join(WANTED)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err("Cannot clear Mihomo's restart setting".into()),
            _ => Ok(()),
        }
    }

    /// After the agent starts: turn Mihomo on again if the owner left it on
    /// (an update, a crash or a reboot). Call after the relay's recovery, or
    /// the core would bind to a stale relay route. Stops once the owner
    /// switches it either way.
    pub fn restore(&self, local_name: &str) {
        self.restore_with(&[0, 5, 15, 30], || self.enable_locked(local_name));
    }

    fn cancel_restore(&self) {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).restore_cancelled = true;
        self.wake.notify_all();
    }

    fn wait_restore(&self, wait: Duration) -> bool {
        if !self.wants_restore() { return false; }
        let i = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (i, _) = self.wake.wait_timeout_while(i, wait, |i| {
            !i.restore_cancelled && i.run.is_none()
        }).unwrap_or_else(std::sync::PoisonError::into_inner);
        !i.restore_cancelled && i.run.is_none()
    }

    fn restore_with(&self, waits: &[u64], mut enable: impl FnMut() -> Result<(), String>) {
        let mut last = String::new();
        for &wait in waits {
            if !self.wait_restore(Duration::from_secs(wait)) { return; }
            let _lock = self.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Ok(_operation) = self.operation.try_lock() else {
                last = "A Mihomo operation is in progress".into();
                continue;
            };
            if !self.wait_restore(Duration::ZERO) || !self.wants_restore() { return; }
            match enable() {
                Ok(()) => return eprintln!("[mihomo] restored"),
                Err(e) => {
                    eprintln!("[mihomo] restore failed: {e}");
                    last = e;
                }
            }
        }
        let mut i = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !i.restore_cancelled && i.run.is_none() {
            i.error = Some(format!("Mihomo did not start again after the agent restarted: {last}"));
        }
    }

    fn enable_locked(&self, local_name: &str) -> Result<(), String> {
        self.enable_with_runtime(local_name, None)
    }

    fn enable_with_runtime(&self, local_name: &str, runtime: Option<&Value>) -> Result<(), String> {
        if self.running() {
            return Ok(());
        }
        self.recover()?;
        let source = self.selected_source()?;
        let mut random = [0u8; 32];
        getrandom::getrandom(&mut random).map_err(|_| "Random secret unavailable")?;
        let secret: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let lan = crate::httpd::mobile::interfaces()
            .ok()
            .and_then(|v| {
                v.into_iter().find_map(|i| match i.addr {
                    std::net::IpAddr::V4(ip) if i.name == "br-lan" => Some(ip),
                    _ => None,
                })
            })
            .ok_or("Cannot read the LAN address for the dashboard management name")?;
        let mut config: Yaml = serde_yaml_ng::from_str(&config_with_lan_host(
            &source,
            &secret,
            Some((local_name, lan)),
        )?)
        .map_err(|_| "Cannot parse managed configuration")?;
        // A user may have disabled cellular data while Wi-Fi is offline. Keep
        // the core available without creating a route or enabling the modem.
        if Cmd::new("ip", &["-4", "route", "get", "1.1.1.1"])
            .run()
            .is_ok_and(|route| route.contains(&format!("dev {RELAY_IFACE}")))
        {
            if let Err(error) = crate::network_clock::sync_wifi() {
                // A captive portal must not tear down working IPv4 relay.
                // The clock floor still permits certificate-checked startup.
                eprintln!("[clock] {error}");
            }
            config
                .as_mapping_mut()
                .unwrap()
                .insert(Yaml::from("interface-name"), Yaml::from(RELAY_IFACE));
        }
        if let Some(runtime) = runtime {
            for key in ["mode", "log-level"] {
                if let Some(value) = runtime[key].as_str() {
                    config
                        .as_mapping_mut()
                        .unwrap()
                        .insert(Yaml::from(key), Yaml::from(value));
                }
            }
        }
        let runtime = json!({"mode":config["mode"].as_str().unwrap_or("rule"),"log-level":config["log-level"].as_str().unwrap_or("info")});
        let config =
            serde_yaml_ng::to_string(&config).map_err(|_| "Cannot encode managed configuration")?;
        net::write_private(&self.root.join("config.yaml"), config.as_bytes())?;
        let fingerprint = self.validation_fingerprint(&config, &secret);
        let reused = fingerprint.is_some()
            && *self
                .validated_config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                == fingerprint;
        let validated = if reused {
            eprintln!("[mihomo] validation reused");
            fingerprint
        } else {
            self.check("config.yaml")?;
            // Only dependencies that stayed the same across the check passed it.
            let after = self.validation_fingerprint(&config, &secret);
            fingerprint.filter(|key| after.as_ref() == Some(key))
        };
        dns::available(1053)?;
        drop(
            std::net::TcpListener::bind(download::PROXY)
                .map_err(|_| "Mihomo's private download proxy port is occupied")?,
        );
        if Cmd::new("ip", &["link", "show", "dev", TUN]).run().is_ok() {
            return Err("Mihomo's reserved TUN interface is already in use".into());
        }
        for (family, tool) in [("-4", "iptables"), ("-6", "ip6tables")] {
            net::reserve(tool, "mangle", &["U60_MH"], family, TABLE, PREF)?;
            if Cmd::new("ip", &[family, "rule", "show"]).run()?.lines().any(|l| l.trim_start().starts_with(&format!("{LOCAL_PREF}:"))) {
                return Err("A reserved network rule priority is already in use".into());
            }
            if Cmd::new("ip", &[family, "rule", "show"])
                .run()?
                .lines()
                .any(|line| {
                    line.split_whitespace()
                        .collect::<Vec<_>>()
                        .windows(2)
                        .any(|w| w == ["lookup", TABLE])
                })
            {
                return Err("Mihomo's reserved route table is referenced by another rule".into());
            }
            for (table, chain) in [
                ("filter", "U60_MH_IN"),
                ("filter", "U60_MH_FWD"),
                ("nat", "U60_MH_DNS"),
            ] {
                if Cmd::new(tool, &["-w", "2", "-t", table, "-S", chain])
                    .run()
                    .is_ok()
                {
                    return Err("Mihomo reserved firewall chains are occupied".into());
                }
            }
        }
        if fs::read_to_string("/proc/sys/net/ipv4/ip_forward")
            .unwrap_or_default()
            .trim()
            != "1"
        {
            return Err("IPv4 forwarding must already be enabled by the router".into());
        }
        let local_addresses = local_addresses()?;
        if local_addresses
            .iter()
            .any(|a| a.to_string() == TUN4 || a.to_string() == TUN6)
        {
            return Err("Mihomo's reserved TUN address is already in use".into());
        }
        let plan = proxy_plan(&local_addresses);
        let mut running = Running {
            child: None,
            network: Transaction::new(self.journal()),
            secret,
            local_name: local_name.into(),
            runtime,
            local_addresses,
            rules_checked: None,
            uplink: Uplink::observe(),
            stranding: None,
        };
        let (guards, interception): (Vec<_>, Vec<_>) = plan
            .into_iter()
            .partition(|s| s.apply.args.windows(2).any(|w| w == ["-t", "filter"]));
        // Guard DNS and the private TUN addresses before the core can bind.
        // The authenticated controller itself always binds loopback.
        if let Err(e) = running.network.apply_network(guards) {
            return Err(running.network.rollback().err().unwrap_or(e));
        }
        // sing-tun v0.4.28 still creates an IPv6 oif rule with auto-route off.
        // The TUN/routes vanish on core exit, but that rule survives SIGKILL.
        // Journal its exact selector before the core can create it.
        if let Err(e) = running.network.apply(Step::new(
            Cmd::new("true", &[]),
            Cmd::new("ip", &["-6", "rule", "del", "oif", TUN, "lookup", TABLE]),
        )) {
            return Err(running.network.rollback().err().unwrap_or(e));
        }
        let mut cmd = Command::new(self.root.join("mihomo"));
        // With cellular off and Relay disconnected B31 has only link-local
        // IPv6. The core must still create its private IPv6 TUN so it can stay
        // enabled offline and use IPv6 when the next uplink becomes available.
        cmd.env("SKIP_SYSTEM_IPV6_CHECK", "true");
        cmd.arg("-d")
            .arg(&self.root)
            .arg("-f")
            .arg(self.root.join("config.yaml"));
        running.child = match net::owned_child(cmd) {
            Ok(child) => Some(child),
            Err(e) => return Err(running.network.rollback().err().unwrap_or(e)),
        };
        let ready = (0..40).any(|_| {
            if matches!(running.child.as_mut().unwrap().try_wait(), Ok(None))
                && core_ready(&running.secret)
                && download::ready()
                && Cmd::new("ip", &["link", "show", "dev", TUN]).run().is_ok()
                && dns::ready(local_name, lan, 1053)
            {
                true
            } else {
                std::thread::sleep(Duration::from_millis(150));
                false
            }
        });
        let result = if !ready {
            Err("Mihomo controller, private download proxy, TUN or dual-stack TCP/UDP DNS did not become ready".into())
        } else {
            running.network.apply_network(interception).and_then(|_| {
                for (family, destination) in [("-4", "1.1.1.1"), ("-6", "2606:4700:4700::1111")] {
                    let route = Cmd::new(
                        "ip",
                        &[family, "route", "get", destination, "mark", "0x600"],
                    )
                    .run()?;
                    if !route.contains(&format!("dev {TUN}")) {
                        return Err(
                            "Firmware policy routing did not select Mihomo's TUN table".into()
                        );
                    }
                }
                Ok(())
            })
        };
        if let Err(e) = result {
            net::stop_child(&mut running.child);
            let rollback = running.network.rollback();
            return Err(rollback.err().unwrap_or(e));
        }
        let current = self.validation_fingerprint(&config, &running.secret);
        let mut i = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        i.run = Some(running);
        i.error = None;
        *self
            .validated_config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            validated.filter(|key| current.as_ref() == Some(key));
        self.wake.notify_all();
        Ok(())
    }

    /// Caller owns the shared network lock. A fresh core also drops persistent
    /// UDP/QUIC transports and old conntrack tuples, not merely dashboard flows.
    pub(crate) fn uplink_changed_locked(&self) -> Result<(), String> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "A Mihomo operation is in progress; restart it after the uplink change")?;
        let observed = Uplink::observe();
        let duplicate = observed.complete()
            && self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .run
                .as_mut()
                .is_some_and(|r| r.uplink == observed && r.alive());
        if duplicate {
            eprintln!("[mihomo] duplicate uplink notification; core kept running");
            return Ok(());
        }
        self.restart_locked()
    }

    /// Caller owns the network and operation locks.
    fn restart_locked(&self) -> Result<(), String> {
        if let Some(state) = self.capture_runtime() {
            self.disable_locked()?;
            self.resume_runtime(&state)?;
        }
        Ok(())
    }

    // Both callers hold the network and operation locks, never `inner` while
    // waiting on the controller. Profile rollback reuses this same snapshot.
    fn capture_runtime(&self) -> Option<RestartState> {
        let (local, secret, saved) = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .run
            .as_ref()
            .map(|r| (r.local_name.clone(), r.secret.clone(), r.runtime.clone()))?;
        let agent = ureq::Agent::config_builder()
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(1)))
            .build()
            .new_agent();
        let runtime = agent
            .get(&format!("http://{CONTROLLER}/configs"))
            .header("Authorization", &format!("Bearer {secret}"))
            .call()
            .ok()
            .and_then(|mut r| r.body_mut().read_json::<Value>().ok())
            .filter(valid_runtime);
        let stale = runtime.is_none();
        // A stalled controller must not strand the core on a removed STA.
        // Use the last successful start's validated settings if it cannot answer.
        let runtime = runtime.unwrap_or(saved);
        Some(RestartState {
            local,
            runtime,
            stale,
        })
    }

    fn resume_runtime(&self, state: &RestartState) -> Result<(), String> {
        self.enable_with_runtime(&state.local, Some(&state.runtime))?;
        if state.stale {
            self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).error = Some("Mihomo restarted with last known mode/log level; its previous controller did not answer with usable settings".into());
        }
        Ok(())
    }

    pub fn disable(&self) -> Result<(), String> {
        self.cancel_restore();
        let _lock = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "A Mihomo operation is in progress")?;
        // Off until the owner turns it on, restarts included.
        let forgotten = self.forget_wanted();
        self.disable_locked().and(forgotten)
    }

    fn disable_locked(&self) -> Result<(), String> {
        let run = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .run
            .take();
        let result = if let Some(mut r) = run {
            net::stop_child(&mut r.child);
            r.network.rollback()
        } else {
            self.recover()
        };
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .error = result.as_ref().err().cloned();
        self.wake.notify_all();
        result
    }
}

fn local_addresses() -> Result<Vec<std::net::IpAddr>, String> {
    let mut addresses: Vec<_> = crate::httpd::mobile::interfaces()
        .map_err(|_| "Cannot read local addresses for Mihomo exclusions")?
        .into_iter()
        .filter(|i| i.name != TUN)
        .map(|i| i.addr)
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    Ok(addresses)
}

/// The mangle exemption that keeps traffic to one of the router's own
/// addresses out of the proxy (`proxy_plan` appends the same rule at start).
fn exemption(address: &std::net::IpAddr) -> Step {
    let tool = if address.is_ipv4() {
        "iptables"
    } else {
        "ip6tables"
    };
    net::rule(
        tool,
        "mangle",
        "U60_MH",
        &["-d", &address.to_string(), "-j", "RETURN"],
        true,
    )
}

/// Each family's exemptions from one chain snapshot (`net::sync_exemptions`):
/// new or externally deleted addresses are inserted ahead of the MARK rules.
fn sync_local(
    network: &mut Transaction,
    local: &[std::net::IpAddr],
    previous: &[std::net::IpAddr],
) -> Result<(), String> {
    for tool in ["iptables", "ip6tables"] {
        let family = |a: &&std::net::IpAddr| a.is_ipv4() == (tool == "iptables");
        let current: Vec<_> = local.iter().filter(family).copied().collect();
        let previous: Vec<_> = previous.iter().filter(family).copied().collect();
        let present = net::firewall::exclusions(tool, "mangle", "U60_MH", "RETURN")?;
        net::sync_exemptions(network, &present, &current, &previous, exemption)?;
    }
    Ok(())
}

/// Identifies a configuration by content with the per-start secret masked.
fn config_fingerprint(config: &str, secret: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(config.replace(secret, "<secret>").as_bytes())
    )
}

fn valid_runtime(runtime: &Value) -> bool {
    matches!(runtime["mode"].as_str(), Some("rule" | "global" | "direct"))
        && matches!(
            runtime["log-level"].as_str(),
            Some("silent" | "error" | "warning" | "info" | "debug")
        )
}

fn core_ready(secret: &str) -> bool {
    ureq::Agent::config_builder()
        .proxy(None)
        .timeout_global(Some(Duration::from_millis(300)))
        .build()
        .new_agent()
        .get(&format!("http://{CONTROLLER}/version"))
        .header("Authorization", &format!("Bearer {secret}"))
        .call()
        .is_ok()
}

impl Mihomo {
    fn download(&self, url: &str, sha: &str, max: u64) -> Result<Vec<u8>, String> {
        let bytes = self
            .downloader()
            .bytes(url, max, Duration::from_secs(180))?;
        if format!("{:x}", Sha256::digest(&bytes)) != sha {
            return Err("Download SHA256 verification failed".into());
        }
        Ok(bytes)
    }
}

fn extract_ui(data: &[u8], destination: &Path) -> Result<(), String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|_| "Invalid dashboard ZIP")?;
    net::private_dir(destination)?;
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|_| "Invalid dashboard ZIP entry")?;
        let path = entry
            .enclosed_name()
            .ok_or("Unsafe dashboard archive path")?;
        let path = path.strip_prefix("dist").unwrap_or(&path);
        if path.as_os_str().is_empty() {
            continue;
        }
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err("Dashboard symlinks are not allowed".into());
        }
        total += entry.size();
        if total > 32 * 1024 * 1024 {
            return Err("Dashboard archive too large".into());
        }
        let out = destination.join(path);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        fs::create_dir_all(out.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut file = File::create(out).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
    }
    if !destination.join("index.html").is_file() {
        return Err("Dashboard index.html missing".into());
    }
    Ok(())
}

/// Parse rather than concatenate YAML: subscriptions cannot override the
/// controller, run scripts, open extra listeners or install their own routes.
pub fn managed_config(source: &str, secret: &str) -> Result<String, String> {
    config_with_lan_host(source, secret, None)
}

fn config_with_lan_host(
    source: &str,
    secret: &str,
    local: Option<(&str, std::net::Ipv4Addr)>,
) -> Result<String, String> {
    if source.len() > MAX_CONFIG {
        return Err("Configuration exceeds 512 KiB".into());
    }
    let mut root: Yaml = serde_yaml_ng::from_str(source).map_err(|_| "Invalid Mihomo YAML")?;
    let m = root
        .as_mapping_mut()
        .ok_or("Mihomo YAML must be a mapping")?;
    for section in ["proxies", "proxy-groups"] {
        if let Some(value) = m.get_mut(Yaml::from(section)) {
            clear_egress_overrides(value);
        }
    }
    for k in [
        "listeners",
        "tunnels",
        "external-controller-tls",
        "external-controller-unix",
        "external-controller-pipe",
        "external-ui",
        "external-ui-url",
        "external-ui-name",
        "external-doh-server",
        "interface-name",
        "routing-mark",
        "authentication",
        "script",
        "tun",
        "geox-url",
        "ss-config",
        "vmess-config",
        "tuic-server",
        "external-controller-cors",
    ] {
        m.remove(Yaml::from(k));
    }
    let set = |m: &mut Mapping, k: &str, v: Yaml| {
        m.insert(Yaml::from(k), v);
    };
    for k in ["port", "socks-port", "mixed-port", "redir-port"] {
        set(m, k, Yaml::from(0));
    }
    if let Some(providers) = m.get_mut(Yaml::from("proxy-providers")) {
        let providers = providers
            .as_mapping_mut()
            .ok_or("proxy-providers must be a mapping")?;
        for provider in providers.values_mut() {
            let provider = provider
                .as_mapping_mut()
                .ok_or("Each proxy provider must be a mapping")?;
            if let Some(payload) = provider.get_mut(Yaml::from("payload")) {
                clear_egress_overrides(payload);
            }
            let overrides = provider
                .entry(Yaml::from("override"))
                .or_insert_with(|| Yaml::Mapping(Mapping::new()));
            let overrides = overrides
                .as_mapping_mut()
                .ok_or("Provider override must be a mapping")?;
            // Apply after remote/file node data, so it cannot bind a SIM
            // interface or inject an independent policy mark on later refresh.
            set(overrides, "interface-name", Yaml::from(""));
            set(overrides, "routing-mark", Yaml::from(0));
            overrides.remove(Yaml::from("override-expr"));
        }
    }
    set(
        m,
        "listeners",
        serde_yaml_ng::to_value(json!([{
            "name":"u60-download", "type":"http", "listen":"127.0.0.1", "port":17893,
            "users":[{"username":download::USER,"password":secret}]
        }]))
        .unwrap(),
    );
    set(
        m,
        "skip-auth-prefixes",
        serde_yaml_ng::to_value(Vec::<String>::new()).unwrap(),
    );
    set(m, "tproxy-port", Yaml::from(0));
    set(m, "allow-lan", Yaml::from(true));
    set(m, "bind-address", Yaml::from("*"));
    set(m, "ipv6", Yaml::from(true));
    set(m, "external-controller", Yaml::from(CONTROLLER));
    set(m, "secret", Yaml::from(secret));
    set(
        m,
        "tun",
        serde_yaml_ng::to_value(json!({
            // Kernel TCP, gVisor UDP: gVisor TCP cut Relay forwarding from
            // ~230 to ~66 Mbit/s on B31; gVisor still handles fragmented UDP.
            "enable":true, "device":TUN, "stack":"mixed", "mtu":1500,
            "auto-route":false, "auto-redirect":false, "auto-detect-interface":false,
            "iproute2-table-index":60630, "inet6-address":["fdfe:dcba:9876::1/126"],
            "dns-hijack":[], "strict-route":false
        }))
        .unwrap(),
    );
    set(
        m,
        "iptables",
        serde_yaml_ng::to_value(json!({"enable":false})).unwrap(),
    );
    set(
        m,
        "ntp",
        serde_yaml_ng::to_value(json!({"enable":false})).unwrap(),
    );
    set(m, "geo-auto-update", Yaml::from(false));
    set(m, "find-process-mode", Yaml::from("off"));
    let profile = m
        .entry(Yaml::from("profile"))
        .or_insert_with(|| Yaml::Mapping(Mapping::new()));
    let profile = profile
        .as_mapping_mut()
        .ok_or("profile must be a mapping")?;
    set(profile, "store-selected", Yaml::from(true));
    if let Some((name, address)) = local {
        let hosts = m
            .entry(Yaml::from("hosts"))
            .or_insert_with(|| Yaml::Mapping(Mapping::new()));
        let hosts = hosts.as_mapping_mut().ok_or("hosts must be a mapping")?;
        set(hosts, name, Yaml::from(address.to_string()));
    }
    let dns = m
        .entry(Yaml::from("dns"))
        .or_insert_with(|| Yaml::Mapping(Mapping::new()));
    let dns = dns.as_mapping_mut().ok_or("dns must be a mapping")?;
    set(dns, "enable", Yaml::from(true));
    set(dns, "listen", Yaml::from("[::]:1053"));
    set(dns, "ipv6", Yaml::from(true));
    set(dns, "use-hosts", Yaml::from(true));
    // redir-host avoids fake-IP addresses escaping when a subscription is
    // switched off; normal nameserver and routing rules are retained.
    set(dns, "enhanced-mode", Yaml::from("redir-host"));
    // The pinned core derives the TUN's IPv4 /30 from this field,
    // even in redir-host mode. Keep its internal listeners predictable.
    set(dns, "fake-ip-range", Yaml::from("198.18.0.1/30"));
    if !dns.contains_key(Yaml::from("nameserver")) {
        set(
            dns,
            "nameserver",
            serde_yaml_ng::to_value(["1.1.1.1", "8.8.8.8"]).unwrap(),
        );
    }
    validate_dns_routes(&root)?;
    serde_yaml_ng::to_string(&root).map_err(|_| "Cannot encode Mihomo configuration".into())
}

// Mihomo interprets an unknown bare DNS URL fragment as SO_BINDTODEVICE,
// overriding the selected uplink even when global interface-name is managed.
fn validate_dns_routes(root: &Yaml) -> Result<(), String> {
    let mut names: Vec<&str> = vec![
        "",
        "DIRECT",
        "REJECT",
        "REJECT-DROP",
        "COMPATIBLE",
        "PASS",
        "PASS-RULE",
        "GLOBAL",
        "RULES",
    ];
    for section in ["proxies", "proxy-groups"] {
        if let Some(items) = root[section].as_sequence() {
            names.extend(items.iter().filter_map(|item| item["name"].as_str()));
        }
    }
    fn check(value: &Yaml, names: &[&str]) -> Result<(), String> {
        match value {
            Yaml::String(server) => {
                if let Some((_, fragment)) = server.split_once('#') {
                    let mut bytes = Vec::new();
                    let mut source = fragment.bytes();
                    while let Some(byte) = source.next() {
                        bytes.push(if byte == b'%' {
                            let hex = |b: Option<u8>| b.and_then(|b| (b as char).to_digit(16));
                            match (hex(source.next()), hex(source.next())) {
                                (Some(h), Some(l)) => (h * 16 + l) as u8,
                                _ => return Err("Invalid escape in a DNS resolver fragment".into()),
                            }
                        } else {
                            byte
                        });
                    }
                    let fragment =
                        std::str::from_utf8(&bytes).map_err(|_| "Invalid DNS resolver fragment")?;
                    let proxy = fragment
                        .split('&')
                        .rfind(|s| !s.contains('='))
                        .unwrap_or("");
                    if !names.contains(&proxy) {
                        return Err("DNS resolvers cannot bind a network interface. Use a declared proxy/group name, DIRECT or RULES after #, or remove that suffix".into());
                    }
                }
            }
            Yaml::Sequence(items) => {
                for item in items {
                    check(item, names)?;
                }
            }
            Yaml::Mapping(items) => {
                for item in items.values() {
                    check(item, names)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    for key in [
        "nameserver",
        "fallback",
        "default-nameserver",
        "proxy-server-nameserver",
        "direct-nameserver",
        "nameserver-policy",
        "proxy-server-nameserver-policy",
    ] {
        check(&root["dns"][key], &names)?;
    }
    Ok(())
}

fn clear_egress_overrides(value: &mut Yaml) {
    match value {
        Yaml::Mapping(map) => {
            map.remove(Yaml::from("interface-name"));
            map.remove(Yaml::from("routing-mark"));
            for child in map.values_mut() {
                clear_egress_overrides(child);
            }
        }
        Yaml::Sequence(values) => {
            for child in values {
                clear_egress_overrides(child);
            }
        }
        _ => {}
    }
}

pub fn proxy_plan(local: &[std::net::IpAddr]) -> Vec<Step> {
    let mut plan = Vec::new();
    for (family, tool, destination) in [("-4", "iptables", TUN4), ("-6", "ip6tables", TUN6)] {
        plan.extend(net::chain(tool, "filter", "U60_MH_IN"));
        for args in [
            // The kernel TCP stack gets each intercepted connection as TCP
            // the core injects on the TUN to this address. Only the core
            // writes the TUN; clients sending here are rejected below.
            vec!["-i", TUN, "-p", "tcp", "-d", destination, "-j", "ACCEPT"],
            vec!["-i", TUN, "-j", "REJECT"],
            vec!["-d", destination, "-j", "REJECT"],
        ] {
            plan.push(net::rule(tool, "filter", "U60_MH_IN", &args, false));
        }
        for proto in ["tcp", "udp"] {
            plan.push(net::rule(
                tool,
                "filter",
                "U60_MH_IN",
                &[
                    "-i", "br-lan", "-p", proto, "--dport", "1053", "-j", "ACCEPT",
                ],
                false,
            ));
        }
        for iface in ["lo", "br-lan"] {
            plan.push(net::rule(
                tool,
                "filter",
                "U60_MH_IN",
                &["-i", iface, "-j", "RETURN"],
                false,
            ));
        }
        for port in ["1053"] {
            for proto in ["tcp", "udp"] {
                plan.push(net::rule(
                    tool,
                    "filter",
                    "U60_MH_IN",
                    &["-p", proto, "--dport", port, "-j", "REJECT"],
                    false,
                ));
            }
        }
        plan.push(net::rule(
            tool,
            "filter",
            "INPUT",
            &["-j", "U60_MH_IN"],
            true,
        ));
        plan.extend(net::chain(tool, "filter", "U60_MH_FWD"));
        for args in [
            vec![
                "-i", "br-lan", "-o", TUN, "-m", "mark", "--mark", MARK, "-j", "ACCEPT",
            ],
            vec![
                "-i",
                TUN,
                "-o",
                "br-lan",
                "-m",
                "conntrack",
                "--ctstate",
                "ESTABLISHED,RELATED",
                "-j",
                "ACCEPT",
            ],
            vec!["-i", TUN, "-j", "REJECT"],
            vec!["-o", TUN, "-j", "REJECT"],
        ] {
            plan.push(net::rule(tool, "filter", "U60_MH_FWD", &args, false));
        }
        plan.push(net::rule(
            tool,
            "filter",
            "FORWARD",
            &["-j", "U60_MH_FWD"],
            true,
        ));
        plan.extend(net::chain(tool, "mangle", "U60_MH"));
        for proto in ["tcp", "udp"] {
            plan.push(net::rule(
                tool,
                "mangle",
                "U60_MH",
                &["-p", proto, "--dport", "53", "-j", "RETURN"],
                false,
            ));
        }
        // DNS to the router is handled by its dedicated NAT chain; all other
        // local/private/tailnet destinations remain reachable without proxy.
        plan.extend(net::local_exclusions(
            tool, "mangle", "U60_MH", "RETURN", local,
        ));
        let excluded: &[&str] = if family == "-4" {
            &[
                "0.0.0.0/8",
                "10.0.0.0/8",
                "100.64.0.0/10",
                "127.0.0.0/8",
                "169.254.0.0/16",
                "172.16.0.0/12",
                "192.168.0.0/16",
                "198.18.0.0/30",
                "224.0.0.0/3",
            ]
        } else {
            &["::/128", "::1/128", "fc00::/7", "fe80::/10", "ff00::/8"]
        };
        for dest in excluded {
            plan.push(net::rule(
                tool,
                "mangle",
                "U60_MH",
                &["-d", dest, "-j", "RETURN"],
                false,
            ));
        }
        for proto in ["tcp", "udp"] {
            plan.push(net::rule(
                tool,
                "mangle",
                "U60_MH",
                &["-p", proto, "-j", "MARK", "--set-xmark", MARK],
                false,
            ));
        }
        // With auto-route disabled the pinned core still installs its
        // IPv6 default in this reserved table. Rust owns the IPv4 default.
        if family == "-4" {
            plan.push(net::ip_step(
                family,
                "route",
                &["default", "dev", TUN, "table", TABLE],
            ));
        }
        let subnet = if family == "-4" { "198.18.0.0/30" } else { "fdfe:dcba:9876::/126" };
        plan.push(net::ip_step(
            family,
            "rule",
            &["pref", LOCAL_PREF, "to", subnet, "lookup", "main"],
        ));
        plan.push(net::ip_step(
            family,
            "rule",
            &["pref", PREF, "fwmark", MARK, "lookup", TABLE],
        ));
        plan.extend(net::chain(tool, "nat", "U60_MH_DNS"));
        for proto in ["tcp", "udp"] {
            plan.push(net::rule(
                tool,
                "nat",
                "U60_MH_DNS",
                &[
                    "-p",
                    proto,
                    "--dport",
                    "53",
                    "-j",
                    "REDIRECT",
                    "--to-ports",
                    "1053",
                ],
                false,
            ));
        }
        plan.push(net::rule(
            tool,
            "nat",
            "PREROUTING",
            &["-i", "br-lan", "-j", "U60_MH_DNS"],
            true,
        ));
        plan.push(net::rule(
            tool,
            "mangle",
            "PREROUTING",
            &["-i", "br-lan", "-j", "U60_MH"],
            true,
        ));
    }
    plan
}

pub fn status(state: &crate::handlers::AppState) -> (u16, Value) {
    (200, json!({"ok":true,"data":state.mihomo.status()}))
}

pub fn change(state: &crate::handlers::AppState, action: &str, body: &[u8]) -> (u16, Value) {
    let result = match action {
        "install" => state.mihomo.install(),
        "config" => serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v["yaml"].as_str().map(str::to_owned))
            .ok_or_else(|| "Missing yaml field".to_owned())
            .and_then(|yaml| state.mihomo.configure(&yaml)),
        "switch" => match serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v["enabled"].as_bool())
        {
            Some(true) => state.mihomo.enable(state.tls.name()),
            Some(false) => state.mihomo.disable(),
            _ => Err("enabled must be a boolean".into()),
        },
        _ => Err("Unknown Mihomo action".into()),
    };
    match result {
        Ok(()) => status(state),
        Err(e) => (503, json!({"ok":false,"error":e})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uplink_identity_ignores_route_counters_but_tracks_egress() {
        let a = Uplink::route(
            r#"[{"dev":"u60sta0","gateway":"192.0.2.1","prefsrc":"192.0.2.2","expires":30}]"#,
        );
        let b = Uplink::route(
            r#"[{"dev":"u60sta0","gateway":"192.0.2.1","prefsrc":"192.0.2.2","expires":10,"cache":[]}]"#,
        );
        assert_eq!(a, b);
        assert_ne!(
            a,
            Uplink::route(r#"[{"dev":"rmnet_data0","gateway":"192.0.2.1","prefsrc":"192.0.2.2"}]"#)
        );
        assert!(Uplink::route("[]").is_none());
        let mut links = json!([{ "ifindex": 7, "ifname": "u60sta0", "addr_info": [
            {"family":"inet","local":"192.0.2.2","prefixlen":24},
            {"family":"inet6","local":"2001:db8::2","prefixlen":64,"valid_life_time":30},
            {"family":"inet6","local":"2001:db8::3","prefixlen":64}
        ] }, {"ifindex":1,"ifname":"lo","addr_info":[]}]);
        let observe = |links: &Value| Uplink::read([a.clone(), a.clone()], &links.to_string());
        let before = observe(&links);
        assert!(before.complete());
        links[0]["addr_info"][1]["valid_life_time"] = json!(10);
        links[0]["addr_info"].as_array_mut().unwrap().reverse();
        links[1]["addr_info"] = json!([{"family":"inet6","local":"fd00::1","prefixlen":128}]);
        assert_eq!(
            observe(&links),
            before,
            "lifetimes, ordering and non-egress addresses are maintenance only"
        );
        assert!(!observe(&links).strands(&before));
        links[0]["addr_info"].as_array_mut().unwrap().remove(0);
        assert_ne!(
            observe(&links),
            before,
            "withdrawn secondary sources must invalidate deduplication"
        );
        assert!(observe(&links).strands(&before));
        assert!(
            !Uplink::read([a.clone(), None], &links.to_string()).complete(),
            "one unreadable family must disable deduplication"
        );
        assert!(!Uplink::read([None, a.clone()], &links.to_string()).complete());
        assert!(!Uplink::read([a.clone(), a], "[]").complete());
    }

    #[test]
    fn only_an_egress_that_strands_connections_outside_the_relay_counts() {
        let start = cellular("rmnet_data0", 3, "10.1.0.1", &["10.1.0.2", "2001:db8::2"]);
        assert!(!start.relayed() && !start.strands(&start));
        assert!(
            cellular("rmnet_data0", 3, "10.1.0.5", &["10.1.0.6", "2001:db8::2"]).strands(&start),
            "a re-dial that withdrew the old cellular address strands its connections"
        );
        assert!(
            cellular("rmnet_data0", 3, "10.1.0.1", &["10.1.0.2", "2001:db8:1::7"]).strands(&start),
            "IPv6 renumbering that removed the old source does too"
        );
        assert!(
            cellular("rmnet_data1", 4, "10.1.0.1", &["10.1.0.2", "2001:db8::2"]).strands(&start),
            "another interface strands even with the same addresses"
        );
        assert!(
            cellular("rmnet_data0", 9, "10.1.0.1", &["10.1.0.2", "2001:db8::2"]).strands(&start),
            "so does a recreated interface"
        );
        assert!(
            !cellular("rmnet_data0", 3, "10.1.0.9", &["10.1.0.2", "2001:db8::2"]).strands(&start),
            "a gateway change keeps connections usable"
        );
        let added = cellular(
            "rmnet_data0",
            3,
            "10.1.0.1",
            &["10.1.0.2", "2001:db8:1::7", "2001:db8::2"],
        );
        assert!(
            !added.strands(&start),
            "an added address that also became the preferred source keeps the old one usable"
        );
        let v4_only = cellular("rmnet_data0", 3, "10.1.0.1", &["10.1.0.2"]);
        assert!(
            !v4_only.strands(&start),
            "an unreadable or lost family is not a move"
        );
        assert!(
            !start.strands(&v4_only),
            "IPv6 becoming available is not either"
        );
        let mut reference = start.clone();
        reference.adopt(&v4_only);
        assert_eq!(reference, start, "a lost family keeps its last reference");
        assert!(
            cellular("rmnet_data0", 3, "10.1.0.1", &["10.1.0.2", "2001:db8:2::9"])
                .strands(&reference),
            "its return on another address strands"
        );
        reference.adopt(&added);
        assert_eq!(reference, added);
        assert!(
            start.strands(&reference),
            "an adopted address is protected like the original ones"
        );
        let route = |dev: &str, src: &str| {
            Uplink::route(&format!(r#"[{{"dev":"{dev}","prefsrc":"{src}"}}]"#))
        };
        let link = |index: u64, name: &str, v4: &str, v6: &str| {
            json!({"ifindex": index, "ifname": name, "addr_info": [
                {"family":"inet","local":v4,"prefixlen":30},
                {"family":"inet6","local":v6,"prefixlen":64}
            ]})
        };
        let links = json!([
            link(7, RELAY_IFACE, "192.0.2.2", "fe80::2"),
            link(3, "rmnet_data0", "10.1.0.2", "2001:db8::2")
        ])
        .to_string();
        let relayed = Uplink::read(
            [
                route(RELAY_IFACE, "192.0.2.2"),
                route("rmnet_data0", "2001:db8::2"),
            ],
            &links,
        );
        assert!(
            relayed.relayed(),
            "the relay announces its own path changes"
        );
    }

    /// One interface carrying `addrs`; each family routes through its first
    /// address, or is unreadable without one.
    fn cellular(dev: &str, index: u64, gateway: &str, addrs: &[&str]) -> Uplink {
        let info: Vec<Value> = addrs
            .iter()
            .map(|addr| {
                let v6 = addr.contains(':');
                json!({
                    "family": if v6 { "inet6" } else { "inet" },
                    "local": addr,
                    "prefixlen": if v6 { 64 } else { 30 },
                })
            })
            .collect();
        let links = json!([{"ifindex": index, "ifname": dev, "addr_info": info}]).to_string();
        let route = |v6: bool| {
            let src = addrs.iter().find(|addr| addr.contains(':') == v6)?;
            Uplink::route(&format!(
                r#"[{{"dev":"{dev}","gateway":"{gateway}","prefsrc":"{src}"}}]"#
            ))
        };
        Uplink::read([route(false), route(true)], &links)
    }

    #[test]
    fn a_stranding_egress_restarts_once_settled_and_at_most_once_per_gap() {
        let cell = |addr: &str| cellular("rmnet_data0", 3, "10.1.0.1", &[addr, "2001:db8::2"]);
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let mut uplink = cell("10.1.0.2");
        let mut stranding = None;
        assert!(!settle_egress(
            &mut uplink,
            &mut stranding,
            cell("10.1.0.6"),
            None,
            at(0)
        ));
        assert_eq!(
            stranding,
            Some((cell("10.1.0.6"), at(0))),
            "only starts settling"
        );
        assert!(!settle_egress(
            &mut uplink,
            &mut stranding,
            cell("10.1.0.6"),
            None,
            at(4)
        ));
        assert!(
            !settle_egress(&mut uplink, &mut stranding, cell("10.1.0.10"), None, at(4)),
            "a flapping uplink starts settling again"
        );
        assert!(!settle_egress(
            &mut uplink,
            &mut stranding,
            cell("10.1.0.10"),
            None,
            at(8)
        ));
        assert!(settle_egress(
            &mut uplink,
            &mut stranding,
            cell("10.1.0.10"),
            None,
            at(9)
        ));
        assert_eq!(
            uplink,
            cell("10.1.0.2"),
            "only the restarted core takes a new reference"
        );

        let mut stranding = Some((cell("10.1.0.6"), at(0)));
        assert!(
            !settle_egress(&mut uplink, &mut stranding, cell("10.1.0.2"), None, at(10)),
            "the old address came back before the change settled"
        );
        assert_eq!(stranding, None);

        let last = Some(at(0));
        assert_eq!(restart_due(at(10), last), at(60));
        assert_eq!(restart_due(at(70), last), at(75));
        let mut stranding = None;
        assert!(!settle_egress(
            &mut uplink,
            &mut stranding,
            cell("10.1.0.6"),
            last,
            at(10)
        ));
        assert!(
            !settle_egress(&mut uplink, &mut stranding, cell("10.1.0.6"), last, at(59)),
            "settled, but the last watchdog restart was under a minute ago"
        );
        assert!(settle_egress(
            &mut uplink,
            &mut stranding,
            cell("10.1.0.6"),
            last,
            at(60)
        ));

        let mut stranding = Some((cell("10.1.0.6"), at(0)));
        let dual = cellular(
            "rmnet_data0",
            3,
            "10.1.0.9",
            &["10.1.0.2", "10.1.0.3", "2001:db8::2"],
        );
        assert!(
            !settle_egress(&mut uplink, &mut stranding, dual.clone(), None, at(10)),
            "added addresses and a new gateway only update the reference"
        );
        assert_eq!((uplink.clone(), stranding), (dual, None));

        let relay = cellular(RELAY_IFACE, 7, "192.0.2.1", &["192.0.2.2", "2001:db8::2"]);
        let mut stranding = Some((cell("10.1.0.6"), at(0)));
        assert!(
            !settle_egress(&mut uplink, &mut stranding, relay, None, at(10)),
            "the relay announces its own path changes"
        );
        assert_eq!(stranding, None);
    }

    #[test]
    fn slow_profile_status_does_not_hold_dashboard_runtime_mutex() {
        let m = Arc::new(Mihomo::for_bridge(
            std::env::temp_dir(),
            Arc::new(Mutex::new(())),
            "fixture".into(),
        ));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = {
            let m = Arc::clone(&m);
            std::thread::spawn(move || {
                m.status_with(|| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                    Ok(json!({"selected":"fixture"}))
                })
            })
        };
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let available = m.inner.try_lock().is_ok();
        release_tx.send(()).unwrap();
        assert!(worker.join().unwrap()["running"].as_bool().unwrap());
        assert!(
            available,
            "async dashboard readers must not wait for profile I/O"
        );
    }

    #[test]
    fn dns_cannot_override_wifi_with_literal_or_escaped_interface_fragments() {
        for key in [
            "nameserver",
            "fallback",
            "default-nameserver",
            "proxy-server-nameserver",
            "direct-nameserver",
        ] {
            for suffix in [
                "rmnet_data0",
                "rmnet%5Fdata0",
                "h3=true&rmnet_data0",
                "DIRECT&rmnet_data0",
                "%GG",
            ] {
                assert!(
                    managed_config(
                        &format!("dns: {{{key}: ['https://dns.example/dns-query#{suffix}']}}"),
                        "private"
                    )
                    .is_err(),
                    "{key} {suffix}"
                );
            }
        }
        assert!(managed_config(
            "dns: {nameserver-policy: {'geosite:cn': ['tls://dns.example#rmnet_data0']}}",
            "private"
        )
        .is_err());
        for suffix in [
            "DIRECT",
            "RULES",
            "GLOBAL",
            "Choice",
            "A+B",
            "A%2BB",
            "h3=true&skip-cert-verify=true",
            "rmnet_data0&DIRECT",
        ] {
            let source = format!("proxy-groups: [{{name: Choice, type: select, proxies: [DIRECT]}}, {{name: A+B, type: select, proxies: [DIRECT]}}]\ndns: {{nameserver: ['https://dns.example/dns-query#{suffix}']}}");
            assert!(managed_config(&source, "private").is_ok(), "{suffix}");
        }
    }

    #[test]
    fn configuration_cannot_open_an_upstream_controller_or_override_routes() {
        let s = managed_config(
            "external-controller: 0.0.0.0:9090\nsecret: stolen\ntun: {enable: true}\nlisteners: [{port: 9999}]\nproxies: []\nrules: [MATCH,DIRECT]\n",
            "private",
        )
        .unwrap();
        let v: Yaml = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(v["external-controller"].as_str(), Some(CONTROLLER));
        assert_eq!(v["secret"].as_str(), Some("private"));
        assert_eq!(v["listeners"][0]["listen"], "127.0.0.1");
        assert_eq!(v["listeners"].as_sequence().unwrap().len(), 1);
        assert_eq!(v["listeners"][0]["users"][0]["password"], "private");
        assert_eq!(v["tun"]["enable"].as_bool(), Some(true));
        assert_eq!(v["tun"]["device"].as_str(), Some(TUN));
        assert_eq!(v["tun"]["auto-route"].as_bool(), Some(false));
        assert_eq!(v["tun"]["auto-redirect"].as_bool(), Some(false));
        assert_eq!(v["tun"]["iproute2-table-index"].as_u64(), Some(60630));
        // Kernel TCP (about 3x gVisor's relay throughput); gVisor keeps UDP.
        assert_eq!(v["tun"]["stack"].as_str(), Some("mixed"));
        assert_eq!(v["dns"]["fake-ip-range"].as_str(), Some("198.18.0.1/30"));
        assert_eq!(v["tproxy-port"].as_u64(), Some(0));
        assert_eq!(v["dns"]["ipv6"].as_bool(), Some(true));
        assert!(managed_config("- not-a-mapping", "x").is_err());
        assert!(managed_config("dns: []", "x").is_err());
    }

    #[test]
    fn the_subscriptions_sniffer_is_kept_but_cannot_bind_an_interface() {
        let s = managed_config(
            "sniffer: {enable: true, force-dns-mapping: true, parse-pure-ip: true, sniff: {TLS: {ports: [443]}}}\nrules: [MATCH,DIRECT]\n",
            "private",
        )
        .unwrap();
        let v: Yaml = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(v["sniffer"]["enable"].as_bool(), Some(true));
        assert_eq!(v["sniffer"]["force-dns-mapping"].as_bool(), Some(true));
        assert_eq!(v["sniffer"]["sniff"]["TLS"]["ports"][0].as_u64(), Some(443));
        let none = managed_config("rules: [MATCH,DIRECT]\n", "private").unwrap();
        assert!(serde_yaml_ng::from_str::<Yaml>(&none).unwrap()["sniffer"].is_null());
    }

    #[test]
    fn a_config_fingerprint_ignores_only_the_per_start_secret() {
        let config = |secret: &str, interface: &str| {
            format!("secret: {secret}\nlisteners: [{{password: {secret}}}]\ninterface-name: {interface}\n")
        };
        let a = config_fingerprint(&config("aaaa", "u60sta0"), "aaaa");
        assert_eq!(a, config_fingerprint(&config("bbbb", "u60sta0"), "bbbb"));
        assert_ne!(a, config_fingerprint(&config("aaaa", "rmnet0"), "aaaa"));
    }

    #[test]
    fn exemptions_are_the_same_rules_the_proxy_plan_installs() {
        let address: std::net::IpAddr = "192.168.0.1".parse().unwrap();
        let planned = proxy_plan(&[address]);
        assert!(
            net::foreign_identity(&planned).is_empty(),
            "prejournaled undos never match another writer's rule"
        );
        let installed = planned
            .iter()
            .find(|s| {
                s.apply.args.iter().any(|a| a == "192.168.0.1")
                    && s.undo.args.contains(&"RETURN".to_owned())
            })
            .expect("plan exempts the router's own address");
        assert_eq!(installed.undo, exemption(&address).undo);
        let v6: std::net::IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(exemption(&v6).apply.program, "ip6tables");
        assert_eq!(&exemption(&v6).apply.args[4..6], ["-I", "U60_MH"]);
    }

    #[test]
    fn transparent_plan_handles_both_protocols_and_families_without_changing_defaults() {
        let local = [
            "192.0.2.1".parse().unwrap(),
            "2001:db8:2::1".parse().unwrap(),
        ];
        let plan = proxy_plan(&local);
        assert!(!plan
            .iter()
            .any(|s| s.apply.args.iter().any(|a| a == "addrtype")));
        for address in local {
            assert!(plan.iter().any(|s| s
                .apply
                .args
                .windows(2)
                .any(|w| w == ["-d", &address.to_string()])));
        }
        for tool in ["iptables", "ip6tables"] {
            for proto in ["tcp", "udp"] {
                assert!(plan.iter().any(|s| s.apply.program == tool
                    && s.apply.args.windows(2).any(|a| a == ["-p", proto])
                    && s.apply.args.iter().any(|a| a == "--set-xmark")));
            }
        }
        // The kernel TCP stack receives each intercepted connection as TCP
        // injected on the TUN to its own address; everything else from the
        // TUN, and anything else to that address, stays rejected.
        for (tool, address) in [("iptables", "198.18.0.1"), ("ip6tables", "fdfe:dcba:9876::1")] {
            let guard: Vec<_> = plan
                .iter()
                .filter(|s| s.apply.program == tool && s.apply.args.windows(2).any(|w| w == ["-A", "U60_MH_IN"]))
                .map(|s| s.apply.args[s.apply.args.iter().position(|a| a == "U60_MH_IN").unwrap() + 1..].join(" "))
                .collect();
            assert_eq!(guard[0], format!("-i {TUN} -p tcp -d {address} -j ACCEPT"));
            assert_eq!(guard[1], format!("-i {TUN} -j REJECT"));
            assert_eq!(guard[2], format!("-d {address} -j REJECT"));
        }
        // The kernel TCP stack answers from the TUN subnet; that must reach
        // the TUN through main before Relay's catch-all table sends it out.
        for (family, subnet) in [("-4", "198.18.0.0/30"), ("-6", "fdfe:dcba:9876::/126")] {
            let rule = plan
                .iter()
                .find(|s| s.apply.args.first().is_some_and(|f| f == family) && s.apply.args.iter().any(|a| a == subnet))
                .expect("TUN subnet exemption");
            assert_eq!(rule.apply.args, ["rule", "add", "pref", LOCAL_PREF, "to", subnet, "lookup", "main"].iter().fold(vec![family.to_owned()], |mut v, a| { v.push((*a).to_owned()); v }));
            assert!(LOCAL_PREF.parse::<u32>().unwrap() < "18640".parse::<u32>().unwrap(), "before Relay's table");
        }
        assert!(plan
            .iter()
            .filter(|s| s.apply.program == "ip" && !s.apply.args.iter().any(|a| a == LOCAL_PREF))
            .all(|s| s.apply.args.iter().any(|a| a == TABLE)));
        assert!(plan
            .iter()
            .all(|s| !s.apply.args.iter().any(|a| a == "-F" || a == "flush")));
    }

    #[test]
    fn off_status_has_no_dashboard_and_creates_nothing() {
        let dir = std::env::temp_dir().join("u60-mihomo-no-side-effects");
        let m = Mihomo::at(dir.clone(), Arc::new(Mutex::new(())));
        assert_eq!(m.status()["running"], false);
        assert!(m.status()["dashboard"].is_null());
        assert!(!dir.exists());
    }

    #[test]
    fn managed_dns_preserves_the_current_custom_https_management_name() {
        let source = "hosts: {router.home.arpa: 8.8.8.8, example.test: 1.1.1.1}\ndns: {use-hosts: false, nameserver: [9.9.9.9]}\n";
        let config = config_with_lan_host(
            source,
            "private",
            Some(("router.home.arpa", "192.168.8.1".parse().unwrap())),
        )
        .unwrap();
        let v: Yaml = serde_yaml_ng::from_str(&config).unwrap();
        assert_eq!(v["hosts"]["router.home.arpa"].as_str(), Some("192.168.8.1"));
        assert_eq!(v["hosts"]["example.test"].as_str(), Some("1.1.1.1"));
        assert_eq!(v["dns"]["use-hosts"].as_bool(), Some(true));
        assert_eq!(v["dns"]["nameserver"][0].as_str(), Some("9.9.9.9"));
        assert!(config_with_lan_host(
            "hosts: []",
            "private",
            Some(("u60.home.arpa", "192.168.0.1".parse().unwrap()))
        )
        .is_err());
    }

    #[test]
    fn core_operations_refuse_while_installing_without_occupying_the_network_lock() {
        let dir = std::env::temp_dir().join(format!("u60-core-busy-{}", std::process::id()));
        let lock = Arc::new(Mutex::new(()));
        let m = Mihomo::at(dir.clone(), Arc::clone(&lock));
        let _operation = m.operation.lock().unwrap();
        assert!(
            lock.try_lock().is_ok(),
            "install serialization must not stall the relay watchdog"
        );
        for result in [
            m.install(),
            m.configure("{}"),
            m.enable("u60.home.arpa"),
            m.disable(),
        ] {
            assert_eq!(result.unwrap_err(), "A Mihomo operation is in progress");
        }
        assert!(!dir.exists());
    }

    #[test]
    fn restore_retries_after_uplink_recovers_but_never_after_an_owner_switch() {
        let dir = std::env::temp_dir().join(format!("u60-core-restore-{}", std::process::id()));
        let m = Mihomo::at(dir.clone(), Arc::new(Mutex::new(())));
        net::write_private(&dir.join(WANTED), b"1").unwrap();
        let mut attempts = 0;
        m.restore_with(&[0, 0, 0, 0], || { attempts += 1; Err("Uplink unavailable".into()) });
        assert_eq!(attempts, 4);
        assert!(m.wants_restore() && !m.running());
        assert!(m.inner.lock().unwrap().error.as_deref().unwrap().contains("Uplink unavailable"));
        // The startup coordinator calls restore again after a usable relay.
        m.restore_with(&[0], || { attempts += 1; Ok(()) });
        assert_eq!(attempts, 5);
        // Even a blocked owner Off request immediately cancels boot retries.
        let operation = m.operation.lock().unwrap();
        assert_eq!(m.disable().unwrap_err(), "A Mihomo operation is in progress");
        drop(operation);
        // A failed marker deletion must not let the same process retry.
        assert!(m.wants_restore());
        m.restore_with(&[0], || { attempts += 1; Ok(()) });
        assert_eq!(attempts, 5);
        assert!(!m.wait_restore(Duration::from_secs(300)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_stops_between_attempts_and_preserves_new_owner_error() {
        let dir = std::env::temp_dir().join(format!("u60-core-cancel-{}", std::process::id()));
        let m = Mihomo::at(dir.clone(), Arc::new(Mutex::new(())));
        net::write_private(&dir.join(WANTED), b"1").unwrap();
        let mut attempts = 0;
        m.restore_with(&[0, 0, 0], || {
            attempts += 1;
            m.cancel_restore();
            m.inner.lock().unwrap().error = Some("Owner operation failed".into());
            Err("Superseded startup failure".into())
        });
        assert_eq!(attempts, 1);
        assert_eq!(m.inner.lock().unwrap().error.as_deref(), Some("Owner operation failed"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_marker_write_and_delete_failures_are_reported() {
        let dir = std::env::temp_dir().join(format!("u60-core-marker-{}", std::process::id()));
        let m = Mihomo::at(dir.clone(), Arc::new(Mutex::new(())));
        std::fs::create_dir_all(dir.join(WANTED)).unwrap();
        assert!(net::write_private(&dir.join(WANTED), b"1").is_err());
        assert_eq!(m.forget_wanted().unwrap_err(), "Cannot clear Mihomo's restart setting");
        let error = m.enable("u60.home.arpa").unwrap_err();
        assert!(error.contains("Cannot clear Mihomo's restart setting"));
        assert!(m.inner.lock().unwrap().restore_cancelled);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn owner_choice_outlives_restarts_until_turned_off_or_failing() {
        let dir = std::env::temp_dir().join(format!("u60-core-wanted-{}", std::process::id()));
        let m = Mihomo::at(dir.clone(), Arc::new(Mutex::new(())));
        net::write_private(&dir.join(WANTED), b"1").unwrap();
        assert!(m.wants_restore());
        assert_eq!(m.status_with(|| Err("none".into()))["autostart"], true);
        // The owner saw it fail: a restart must not retry it.
        assert!(m.enable("u60.home.arpa").is_err());
        assert!(!m.wants_restore());
        net::write_private(&dir.join(WANTED), b"1").unwrap();
        m.disable().unwrap();
        assert!(!m.wants_restore());
        // Nothing wanted: returns at once and changes nothing.
        m.restore("u60.home.arpa");
        assert!(!m.running() && m.inner.lock().unwrap().error.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn managed_nodes_and_provider_refresh_cannot_override_selected_uplink() {
        let config = managed_config(
            r#"
proxies:
  - {name: DirectTest, type: direct, interface-name: rmnet_data0, routing-mark: 1234}
proxy-groups:
  - {name: Select, type: select, proxies: [DirectTest], interface-name: rmnet_data0}
proxy-providers:
  Hotel:
    type: inline
    payload: [{name: Supplied, type: direct, interface-name: rmnet_data0}]
    override:
      interface-name: rmnet_data0
      routing-mark: 1234
      override-expr: ['true']
hosts: {interface-name: 192.0.2.1}
"#,
            "private",
        )
        .unwrap();
        let v: Yaml = serde_yaml_ng::from_str(&config).unwrap();
        assert!(v["proxies"][0]["interface-name"].is_null());
        assert!(v["proxies"][0]["routing-mark"].is_null());
        assert!(v["proxy-groups"][0]["interface-name"].is_null());
        assert!(v["proxy-providers"]["Hotel"]["payload"][0]["interface-name"].is_null());
        assert_eq!(
            v["proxy-providers"]["Hotel"]["override"]["interface-name"].as_str(),
            Some("")
        );
        assert_eq!(
            v["proxy-providers"]["Hotel"]["override"]["routing-mark"].as_i64(),
            Some(0)
        );
        assert!(v["proxy-providers"]["Hotel"]["override"]["override-expr"].is_null());
        assert_eq!(v["hosts"]["interface-name"].as_str(), Some("192.0.2.1"));
        assert_eq!(v["profile"]["store-selected"].as_bool(), Some(true));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "Uses real netfilter/core; only the disposable Linux QA runner"]
    fn isolated_real_core_uplink_transitions() {
        assert_eq!(std::env::var("U60_CORE_ISOLATED_QA").as_deref(), Ok("1"));
        assert!(Path::new("/u60-ipv6-qa-disposable").exists());
        let root = PathBuf::from(std::env::var_os("U60_CORE_QA_ROOT").unwrap());
        let m = Arc::new(Mihomo::at(root, Arc::new(Mutex::new(()))));
        m.start();
        let source = "proxies: [{name: TestDirect, type: direct, udp: true, interface-name: rmnet_data0}]\nproxy-groups: [{name: SelectTest, type: select, proxies: [DIRECT, TestDirect]}]\nrules: ['MATCH,SelectTest']\ndns: {nameserver: [1.1.1.1]}\n";
        m.configure(source).unwrap();
        for tcp in [true, false] {
            let listener = tcp.then(|| std::net::TcpListener::bind("0.0.0.0:1053").unwrap());
            let datagram = (!tcp).then(|| std::net::UdpSocket::bind("0.0.0.0:1053").unwrap());
            assert!(m.enable("test.home.arpa").is_err());
            assert!(
                !m.owns_network(),
                "occupied DNS must never leave interception"
            );
            drop((listener, datagram));
        }
        m.enable("test.home.arpa").unwrap();
        assert!(
            download::ready(),
            "Unauthenticated proxy requests must be rejected"
        );
        {
            use base64::Engine;
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(download::PROXY).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let auth = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                download::USER,
                m.secret().unwrap()
            ));
            stream.write_all(format!("CONNECT 203.0.113.1:45679 HTTP/1.1\r\nHost: 203.0.113.1:45679\r\nProxy-Authorization: Basic {auth}\r\n\r\n").as_bytes()).unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut b = [0u8];
                stream.read_exact(&mut b).unwrap();
                headers.push(b[0]);
                assert!(headers.len() < 4096);
            }
            assert!(headers.starts_with(b"HTTP/1.1 200"));
            stream.write_all(b"test").unwrap();
            let mut echoed = [0u8; 8];
            stream.read_exact(&mut echoed).unwrap();
            assert_eq!(&echoed, b"10.9.0.2");
        }
        let profiles = m.profile_status().unwrap();
        let original = profiles["selected"].as_str().unwrap().to_owned();
        let request = |v| serde_json::from_value::<profiles::Request>(v).unwrap();
        assert!(m
            .profiles_change(request(json!({"action":"delete","id":original})))
            .is_err());
        assert!(m.running());
        m.profiles_change(request(
            json!({"action":"add","name":"Switch QA","yaml":source}),
        ))
        .unwrap();
        let alternate = m.profile_status().unwrap()["profiles"][1]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        m.profiles_change(request(json!({"action":"select","id":alternate})))
            .unwrap();
        assert!(m.running());
        m.profiles_change(request(json!({"action":"extensions","extensions":{
            "merge_enabled":true,"merge":"prepend-rules: ['DOMAIN,merge.test,DIRECT']",
            "script_enabled":true,"script":"function main(c,n){c.rules.unshift('DOMAIN,script.test,DIRECT');return c;}"
        }}))).unwrap();
        assert!(m.running());
        let compiled: Yaml =
            serde_yaml_ng::from_str(&fs::read_to_string(m.root.join("config.yaml")).unwrap())
                .unwrap();
        assert_eq!(compiled["rules"][0], "DOMAIN,script.test,DIRECT");
        assert_eq!(compiled["rules"][1], "DOMAIN,merge.test,DIRECT");
        m.profiles_change(request(json!({"action":"select","id":original})))
            .unwrap();
        let agent = ureq::Agent::config_builder()
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(2)))
            .build()
            .new_agent();
        for (group, selection) in [("SelectTest", "TestDirect"), ("GLOBAL", "SelectTest")] {
            agent
                .put(&format!("http://{CONTROLLER}/proxies/{group}"))
                .header("Authorization", &format!("Bearer {}", m.secret().unwrap()))
                .send_json(json!({"name":selection}))
                .unwrap();
        }
        agent
            .patch(&format!("http://{CONTROLLER}/configs"))
            .header("Authorization", &format!("Bearer {}", m.secret().unwrap()))
            .send_json(json!({"mode":"global","log-level":"debug"}))
            .unwrap();
        // The same restart is used by manual/scheduled profile refresh and
        // global extension edits. Neither may reset the dashboard's mode.
        m.profiles_change(request(json!({"action":"extensions","extensions":{
            "merge_enabled":true,"merge":"prepend-rules: ['DOMAIN,refresh.test,DIRECT']",
            "script_enabled":false
        }})))
        .unwrap();
        let refreshed: Yaml =
            serde_yaml_ng::from_str(&fs::read_to_string(m.root.join("config.yaml")).unwrap())
                .unwrap();
        assert_eq!(refreshed["mode"].as_str(), Some("global"));
        assert_eq!(refreshed["log-level"].as_str(), Some("debug"));
        std::thread::sleep(Duration::from_secs(1));
        let pid = m
            .inner
            .lock()
            .unwrap()
            .run
            .as_ref()
            .unwrap()
            .child
            .as_ref()
            .unwrap()
            .id();
        {
            let _network = m.lock.lock().unwrap();
            m.uplink_changed_locked().unwrap();
            m.uplink_changed_locked().unwrap();
        }
        assert_eq!(
            m.inner
                .lock()
                .unwrap()
                .run
                .as_ref()
                .unwrap()
                .child
                .as_ref()
                .unwrap()
                .id(),
            pid
        );
        net::write_private(Path::new("/qa-state/cell-ready"), b"1").unwrap();
        for (request, done) in [
            ("wifi", "wifi-ready"),
            ("v6", "v6-ready"),
            ("cell", "cell-again"),
            ("wifi2", "wifi2-ready"),
            ("offline", "offline-ready"),
            ("wifi3", "wifi3-ready"),
            ("stalled", "stalled-ready"),
        ] {
            for _ in 0..600 {
                if Path::new(&format!("/qa-state/{request}")).exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            assert!(
                Path::new(&format!("/qa-state/{request}")).exists(),
                "timed out at {request}"
            );
            let _network = m.lock.lock().unwrap();
            if request == "stalled" {
                let pid = m
                    .inner
                    .lock()
                    .unwrap()
                    .run
                    .as_ref()
                    .unwrap()
                    .child
                    .as_ref()
                    .unwrap()
                    .id();
                assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
            }
            m.uplink_changed_locked().unwrap();
            let config: Yaml =
                serde_yaml_ng::from_str(&fs::read_to_string(m.root.join("config.yaml")).unwrap())
                    .unwrap();
            assert_eq!(
                config["interface-name"].as_str(),
                if matches!(request, "cell" | "offline" | "stalled") {
                    None
                } else {
                    Some("u60sta0")
                }
            );
            assert_eq!(
                config["hosts"]["test.home.arpa"].as_str(),
                Some("192.168.0.1")
            );
            assert_eq!(config["mode"].as_str(), Some("global"));
            assert_eq!(config["log-level"].as_str(), Some("debug"));
            // The core exposes its controller/listeners before updateProfile
            // restores selectors. Observe bounded convergence, without writing
            // a selection again (which would conceal lost persisted state).
            let observed = Instant::now();
            let mut first = None;
            let proxies: Value = loop {
                let proxies: Value = agent
                    .get(&format!("http://{CONTROLLER}/proxies"))
                    .header("Authorization", &format!("Bearer {}", m.secret().unwrap()))
                    .call()
                    .unwrap()
                    .body_mut()
                    .read_json()
                    .unwrap();
                first.get_or_insert_with(|| {
                    (
                        proxies["proxies"]["SelectTest"]["now"].clone(),
                        proxies["proxies"]["GLOBAL"]["now"].clone(),
                    )
                });
                if (proxies["proxies"]["SelectTest"]["now"] == "TestDirect"
                    && proxies["proxies"]["GLOBAL"]["now"] == "SelectTest")
                    || observed.elapsed() >= Duration::from_secs(3)
                {
                    break proxies;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            eprintln!(
                "[qa] selectors stage={request} first={first:?} settled_ms={}",
                observed.elapsed().as_millis()
            );
            assert_eq!(proxies["proxies"]["SelectTest"]["now"], "TestDirect");
            assert_eq!(proxies["proxies"]["GLOBAL"]["now"], "SelectTest");
            net::write_private(Path::new(&format!("/qa-state/{done}")), b"1").unwrap();
        }
        for _ in 0..600 {
            if Path::new("/qa-state/finish").exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(Path::new("/qa-state/finish").exists());
        m.disable().unwrap();
        assert!(!m.owns_network());
    }
}
