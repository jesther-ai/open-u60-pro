//! Signed, crash-safe updates for the on-device agent.
//!
//! Release manifests are signed offline. Downloads always land in the
//! inactive slot, are bounded and hashed, and are never selected by the boot
//! supervisor until a short local health trial succeeds.

pub mod manifest;
pub mod storage;

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use manifest::{Channel, ManifestVerifier, ReleaseManifest, MAX_BINARY_BYTES, MAX_ENVELOPE_BYTES};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use storage::{Paths, Slot};

const STABLE_CHANNEL_URL: &str =
    "https://raw.githubusercontent.com/jesther-ai/open-u60-pro/ota-channels/stable.json";
const BETA_CHANNEL_URL: &str =
    "https://raw.githubusercontent.com/jesther-ai/open-u60-pro/ota-channels/beta.json";
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(45);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const REQUIRED_HEADROOM: u64 = 8 * 1024 * 1024;
const TRIAL_SECONDS: u64 = 60;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub channel: Channel,
    pub automatic_checks: bool,
    pub automatic_downloads: bool,
    /// Reserved for a future hardware-validated release. It cannot be enabled
    /// by the current API, even if an old settings file contains `true`.
    pub automatic_install: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            channel: Channel::Stable,
            automatic_checks: true,
            automatic_downloads: false,
            automatic_install: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    #[default]
    Idle,
    Checking,
    Downloading,
    Staged,
    Installing,
    RollingBack,
    Failed,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct PersistentState {
    phase: Phase,
    available: Option<ReleaseManifest>,
    staged: Option<ReleaseManifest>,
    staged_slot: Option<String>,
    last_check: Option<u64>,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct RuntimeState {
    persistent: PersistentState,
    bytes_received: u64,
    bytes_total: u64,
}

pub struct OtaManager {
    paths: Paths,
    verifier: ManifestVerifier,
    settings: Mutex<Settings>,
    state: Mutex<RuntimeState>,
    operation: AtomicBool,
    dashboard: Weak<crate::dashboard::Dashboard>,
}

impl OtaManager {
    pub fn system(dashboard: Weak<crate::dashboard::Dashboard>) -> Arc<Self> {
        let paths = if cfg!(test) {
            let root =
                std::env::temp_dir().join(format!("u60-ota-appstate-{}", std::process::id()));
            Paths::new(root.clone(), root.join("supervisor"), root.join("legacy"))
        } else {
            Paths::system()
        };
        if let Err(error) = paths.prepare() {
            eprintln!("[ota] cannot prepare update storage: {error}");
        }
        let mut settings = storage::read_json::<Settings>(&paths.settings).unwrap_or_default();
        settings.automatic_install = false;
        let persistent = storage::read_json(&paths.state).unwrap_or_default();
        let key = include_str!(concat!(env!("OUT_DIR"), "/ota-public-key.pem"));
        Arc::new(Self {
            paths,
            verifier: ManifestVerifier::from_pem((!key.trim().is_empty()).then_some(key)),
            settings: Mutex::new(settings),
            state: Mutex::new(RuntimeState {
                persistent,
                ..RuntimeState::default()
            }),
            operation: AtomicBool::new(false),
            dashboard,
        })
    }

    #[cfg(test)]
    fn test(paths: Paths, verifier: ManifestVerifier) -> Self {
        paths.prepare().unwrap();
        Self {
            paths,
            verifier,
            settings: Mutex::new(Settings::default()),
            state: Mutex::new(RuntimeState::default()),
            operation: AtomicBool::new(false),
            dashboard: Weak::new(),
        }
    }

    pub fn start(self: &Arc<Self>) {
        if std::env::var("ZTE_OTA_TRIAL").as_deref() == Ok("1") {
            let manager = Arc::clone(self);
            crate::system::spawn_named("ota-trial-state", move || {
                std::thread::sleep(Duration::from_secs(TRIAL_SECONDS));
                for _ in 0..12 {
                    if !manager.paths.pending.exists() {
                        manager.change(|state| {
                            state.persistent.phase = Phase::Idle;
                            state.persistent.available = None;
                            state.persistent.staged = None;
                            state.persistent.staged_slot = None;
                            state.persistent.last_error = None;
                            state.bytes_received = 0;
                            state.bytes_total = 0;
                        });
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(5));
                }
            });
        }
        let manager = Arc::clone(self);
        crate::system::spawn_named("ota-check", move || {
            // Give WAN and DNS time to settle after boot.
            std::thread::sleep(Duration::from_secs(90));
            loop {
                if manager.settings().automatic_checks {
                    if let Err(error) = manager.check() {
                        eprintln!("[ota] automatic check failed: {error}");
                    }
                }
                std::thread::sleep(CHECK_INTERVAL);
            }
        });
    }

    pub fn status(&self) -> Value {
        let settings = self.settings();
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let active = self.paths.active_slot();
        let trial_in_progress = self.paths.pending.exists();
        let staged_slot = state
            .persistent
            .staged_slot
            .as_deref()
            .and_then(Slot::parse);
        let rollback_available =
            self.paths.rollback_available() && active.map(Slot::other) != staged_slot;
        json!({"ok":true,"data":{
            "current":crate::build_info::current(),
            "channel":settings.channel,
            "settings":settings,
            "phase":state.persistent.phase,
            "available":state.persistent.available,
            "staged":state.persistent.staged,
            "staged_slot":state.persistent.staged_slot,
            "last_check":state.persistent.last_check,
            "last_error":state.persistent.last_error,
            "progress":{"received":state.bytes_received,"total":state.bytes_total},
            "signing_configured":self.verifier.configured(),
            "bootstrap_ready":self.paths.bootstrap_ready(),
            "rollback_available":rollback_available,
            "active_slot":active.map(Slot::as_str),
            "trial_in_progress":trial_in_progress,
            "automatic_install_supported":false,
            "restart_scope":"agent_only"
        }})
    }

    pub fn settings(&self) -> Settings {
        self.settings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn update_settings(&self, body: &[u8]) -> Result<(), String> {
        if self.operation.swap(true, Ordering::AcqRel) {
            return Err("another update operation is already running".into());
        }
        let _guard = Operation(&self.operation);
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            channel: Channel,
            automatic_checks: bool,
            automatic_downloads: bool,
        }
        let input: Input =
            serde_json::from_slice(body).map_err(|_| "invalid settings".to_string())?;
        let next = Settings {
            channel: input.channel,
            automatic_checks: input.automatic_checks,
            automatic_downloads: input.automatic_downloads,
            automatic_install: false,
        };
        storage::write_json(&self.paths.settings, &next).map_err(|e| e.to_string())?;
        let channel_changed = {
            let mut settings = self.settings.lock().unwrap_or_else(|p| p.into_inner());
            let changed = settings.channel != next.channel;
            *settings = next;
            changed
        };
        if channel_changed {
            self.change(|state| {
                state.persistent.phase = Phase::Idle;
                state.persistent.available = None;
                state.persistent.staged = None;
                state.persistent.staged_slot = None;
                state.persistent.last_check = None;
                state.persistent.last_error = None;
                state.bytes_received = 0;
                state.bytes_total = 0;
            });
        }
        self.notify();
        Ok(())
    }

    pub fn check(&self) -> Result<(), String> {
        let _guard = self.begin(Phase::Checking)?;
        let result = self.check_inner();
        match result {
            Ok(available) => {
                let auto_download = self.settings().automatic_downloads && available.is_some();
                self.change(|state| {
                    state.persistent.available = available;
                    state.persistent.last_check = Some(unix_now());
                    state.persistent.last_error = None;
                    state.persistent.phase = if state.persistent.staged.is_some() {
                        Phase::Staged
                    } else {
                        Phase::Idle
                    };
                });
                drop(_guard);
                if auto_download {
                    self.download()?;
                }
                Ok(())
            }
            Err(error) => {
                self.fail(&error);
                Err(error)
            }
        }
    }

    fn check_inner(&self) -> Result<Option<ReleaseManifest>, String> {
        if !self.verifier.configured() {
            return Err("this build has no OTA release signing key".into());
        }
        let channel = self.settings().channel;
        let stable = self.fetch_manifest(stable_url())?;
        let mut releases = vec![stable.clone()];
        if channel == Channel::Beta {
            releases.push(self.fetch_manifest(beta_url())?);
        }
        let current = Version::parse(crate::build_info::VERSION)
            .map_err(|_| "current build has an invalid version".to_string())?;
        let blocked: Vec<String> = storage::read_json(&self.paths.blocked).unwrap_or_default();
        releases.sort_by_key(|release| release.semver().ok());
        let candidate = if channel == Channel::Stable {
            // Leaving Beta explicitly returns to the newest Stable release,
            // even when it sorts below a later prerelease already installed.
            let version = stable.semver().map_err(|e| e.to_string())?;
            (stable.version != crate::build_info::VERSION
                && !blocked.contains(&stable.version)
                && (version > current || !current.pre.is_empty()))
            .then_some(stable)
        } else {
            releases
                .into_iter()
                .filter(|release| !blocked.contains(&release.version))
                .filter(|release| release.semver().is_ok_and(|version| version > current))
                .next_back()
        };
        if let Some(release) = &candidate {
            if release
                .minimum_updater_semver()
                .map_err(|e| e.to_string())?
                > current
            {
                return Err(format!(
                    "{} requires updater {} or newer",
                    release.version, release.minimum_updater_version
                ));
            }
        }
        Ok(candidate)
    }

    fn fetch_manifest(&self, url: &str) -> Result<ReleaseManifest, String> {
        validate_channel_url(url)?;
        let response = client(HTTP_TIMEOUT)?
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|e| format!("release channel request failed: {e}"))?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_ENVELOPE_BYTES as u64)
        {
            return Err("signed release envelope is too large".into());
        }
        let mut bytes = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or(4096)
                .min(MAX_ENVELOPE_BYTES as u64) as usize,
        );
        response
            .take(MAX_ENVELOPE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("release channel read failed: {e}"))?;
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err("signed release envelope is too large".into());
        }
        self.verifier.verify(&bytes).map_err(|e| e.to_string())
    }

    pub fn download(&self) -> Result<(), String> {
        let _guard = self.begin(Phase::Downloading)?;
        let result = self.download_inner();
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    fn download_inner(&self) -> Result<(), String> {
        let manifest = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .persistent
            .available
            .clone()
            .ok_or_else(|| "check for an update first".to_string())?;
        manifest.validate().map_err(|e| e.to_string())?;
        let slot = self.paths.active_slot().unwrap_or(Slot::A).other();
        let available = storage::available_bytes(&self.paths.root).map_err(|e| e.to_string())?;
        if available < manifest.size.saturating_add(REQUIRED_HEADROOM) {
            return Err("not enough free storage for a safe update".into());
        }
        let destination = self.paths.slot_download(slot);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o700)
            .open(&destination)
            .map_err(|e| e.to_string())?;
        let mut response = client(DOWNLOAD_TIMEOUT)?
            .get(&manifest.binary_url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|e| format!("binary download failed: {e}"))?;
        if response
            .content_length()
            .is_some_and(|size| size != manifest.size)
        {
            return Err("download size does not match the signed manifest".into());
        }
        self.change(|state| {
            state.bytes_received = 0;
            state.bytes_total = manifest.size;
        });
        let mut digest = Sha256::new();
        let mut received = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = response.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            received = received.saturating_add(count as u64);
            if received > manifest.size || received > MAX_BINARY_BYTES {
                return Err("download exceeded the signed size".into());
            }
            file.write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
            digest.update(&buffer[..count]);
            self.change(|state| state.bytes_received = received);
        }
        if received != manifest.size || manifest.sha256 != manifest::hex(&digest.finalize()) {
            return Err("download failed signed size or SHA-256 verification".into());
        }
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        probe(&destination, &manifest.version)?;
        fs::rename(&destination, self.paths.slot_binary(slot)).map_err(|e| e.to_string())?;
        storage::sync_parent(&self.paths.slot_binary(slot)).map_err(|e| e.to_string())?;
        storage::write_json(&self.paths.slot_meta(slot), &manifest).map_err(|e| e.to_string())?;
        self.change(|state| {
            state.persistent.staged = Some(manifest);
            state.persistent.staged_slot = Some(slot.as_str().into());
            state.persistent.phase = Phase::Staged;
            state.persistent.last_error = None;
        });
        Ok(())
    }

    pub fn install(&self) -> Result<(), String> {
        let _guard = self.begin(Phase::Installing)?;
        let result = self.install_inner();
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    fn install_inner(&self) -> Result<(), String> {
        if self.paths.pending.exists() {
            return Err("an agent health trial is already in progress".into());
        }
        if !self.paths.bootstrap_ready() {
            return Err("OTA bootstrap supervisor is not installed yet".into());
        }
        let (manifest, slot) = {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let manifest = state
                .persistent
                .staged
                .clone()
                .ok_or_else(|| "download an update first".to_string())?;
            let slot = state
                .persistent
                .staged_slot
                .as_deref()
                .and_then(Slot::parse)
                .ok_or_else(|| "staged update slot is invalid".to_string())?;
            (manifest, slot)
        };
        let previous = self
            .paths
            .active_slot()
            .ok_or_else(|| "active OTA slot is missing".to_string())?;
        self.paths
            .write_pending(slot, previous, &manifest.version)
            .map_err(|e| e.to_string())?;
        let current = std::env::current_exe().map_err(|e| e.to_string())?;
        Command::new(current)
            .args([
                "ota-helper",
                slot.as_str(),
                previous.as_str(),
                &manifest.version,
                &std::process::id().to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start update helper: {e}"))?;
        self.paths.append_log(&format!(
            "installing {} in slot {}",
            manifest.version,
            slot.as_str()
        ));
        crate::system::spawn_named("ota-exit", || {
            std::thread::sleep(Duration::from_millis(1200));
            std::process::exit(0);
        });
        Ok(())
    }

    pub fn rollback(&self) -> Result<(), String> {
        let _guard = self.begin(Phase::RollingBack)?;
        let result = self.rollback_inner();
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    fn rollback_inner(&self) -> Result<(), String> {
        if self.paths.pending.exists() {
            return Err("an agent health trial is already in progress".into());
        }
        let active = self
            .paths
            .active_slot()
            .ok_or_else(|| "active OTA slot is missing".to_string())?;
        let previous = active.other();
        if self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .persistent
            .staged_slot
            .as_deref()
            .and_then(Slot::parse)
            == Some(previous)
        {
            return Err("the inactive slot contains a staged update, not a rollback build".into());
        }
        if !storage::executable(&self.paths.slot_binary(previous)) {
            return Err("no previous agent slot is available".into());
        }
        let version = storage::read_json::<ReleaseManifest>(&self.paths.slot_meta(previous))
            .map(|meta| meta.version)
            .unwrap_or_else(|| "bootstrap-slot".into());
        self.paths
            .write_pending(previous, active, &version)
            .map_err(|e| e.to_string())?;
        let current = std::env::current_exe().map_err(|e| e.to_string())?;
        Command::new(current)
            .args([
                "ota-helper",
                previous.as_str(),
                active.as_str(),
                &version,
                &std::process::id().to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start rollback helper: {e}"))?;
        crate::system::spawn_named("ota-exit", || {
            std::thread::sleep(Duration::from_millis(1200));
            std::process::exit(0);
        });
        Ok(())
    }

    fn begin(&self, phase: Phase) -> Result<Operation<'_>, String> {
        if self.operation.swap(true, Ordering::AcqRel) {
            return Err("another update operation is already running".into());
        }
        self.change(|state| {
            state.persistent.phase = phase;
            state.persistent.last_error = None;
        });
        Ok(Operation(&self.operation))
    }

    fn fail(&self, error: &str) {
        self.change(|state| {
            state.persistent.phase = Phase::Failed;
            state.persistent.last_error = Some(error.into());
        });
    }

    fn change(&self, change: impl FnOnce(&mut RuntimeState)) {
        let persistent = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            change(&mut state);
            state.persistent.clone()
        };
        if let Err(error) = storage::write_json(&self.paths.state, &persistent) {
            eprintln!("[ota] cannot save state: {error}");
        }
        self.notify();
    }

    fn notify(&self) {
        if let Some(dashboard) = self.dashboard.upgrade() {
            dashboard.request_refresh();
        }
    }
}

#[derive(Debug)]
struct Operation<'a>(&'a AtomicBool);

impl Drop for Operation<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(concat!("open-u60-ota/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

fn stable_url() -> &'static str {
    option_env!("ZTE_OTA_STABLE_URL").unwrap_or(STABLE_CHANNEL_URL)
}

fn beta_url() -> &'static str {
    option_env!("ZTE_OTA_BETA_URL").unwrap_or(BETA_CHANNEL_URL)
}

fn validate_channel_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|_| "invalid release channel URL".to_string())?;
    let production = url.scheme() == "https"
        && url.host_str() == Some("raw.githubusercontent.com")
        && url
            .path()
            .starts_with("/jesther-ai/open-u60-pro/ota-channels/");
    let test = cfg!(test) && matches!(url.host_str(), Some("127.0.0.1" | "localhost"));
    if production || test {
        Ok(())
    } else {
        Err("release channel URL is not trusted".into())
    }
}

fn probe(binary: &Path, expected: &str) -> Result<(), String> {
    let status = Command::new(binary)
        .args(["ota-probe", expected])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("downloaded agent cannot run: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("downloaded agent failed its compatibility probe".into())
    }
}

pub fn probe_cli(expected: Option<&str>) -> i32 {
    if expected == Some(crate::build_info::VERSION)
        && crate::build_info::TARGET == manifest::RELEASE_TARGET
    {
        0
    } else {
        2
    }
}

/// Run by the old binary after the API response is sent. The pending marker
/// makes a power loss fail safe: the shell supervisor boots the previous slot.
pub fn helper_cli(args: &[String]) -> i32 {
    if args.len() != 4 {
        return 2;
    }
    let Some(candidate) = Slot::parse(&args[0]) else {
        return 2;
    };
    let Some(previous) = Slot::parse(&args[1]) else {
        return 2;
    };
    let expected = &args[2];
    let Ok(parent) = args[3].parse::<u32>() else {
        return 2;
    };
    let paths = Paths::system();
    if !wait_for_exit(parent, Duration::from_secs(15)) {
        paths.append_log("update helper timed out waiting for the previous agent");
        return 3;
    }
    let binary = paths.slot_binary(candidate);
    let mut child = match Command::new(&binary)
        .env("ZTE_OTA_TRIAL", "1")
        .stdin(Stdio::null())
        .stdout(log_file(&paths.update_log))
        .stderr(log_file(&paths.update_log))
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            paths.append_log(&format!("candidate {expected} could not start: {error}"));
            block_version(&paths, expected);
            return recover(&paths, previous, None);
        }
    };
    if health_trial(&mut child, bind_address()) {
        if let Err(error) = paths
            .write_active(candidate)
            .and_then(|()| paths.point_legacy_link(candidate))
            .and_then(|()| paths.clear_pending())
        {
            paths.append_log(&format!("candidate {expected} commit failed: {error}"));
            return recover(&paths, previous, Some(&mut child));
        }
        if storage::read_json::<ReleaseManifest>(&paths.slot_meta(candidate))
            .is_some_and(|manifest| manifest.channel == Channel::Stable)
        {
            let _ =
                storage::atomic_copy(&paths.slot_binary(candidate), &paths.stable_binary(), 0o755);
            let _ = storage::atomic_copy(&paths.slot_meta(candidate), &paths.stable_meta(), 0o600);
        }
        paths.append_log(&format!(
            "candidate {expected} passed the {TRIAL_SECONDS}s trial"
        ));
        0
    } else {
        paths.append_log(&format!("candidate {expected} failed its health trial"));
        block_version(&paths, expected);
        recover(&paths, previous, Some(&mut child))
    }
}

fn health_trial(child: &mut Child, address: SocketAddr) -> bool {
    let started = Instant::now();
    let mut consecutive = 0u8;
    while started.elapsed() < Duration::from_secs(TRIAL_SECONDS) {
        if child.try_wait().ok().flatten().is_some() {
            return false;
        }
        if TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_ok() {
            consecutive = consecutive.saturating_add(1);
        } else {
            consecutive = 0;
        }
        // Require the listener to remain reachable, then keep observing the
        // process for the full trial to catch delayed startup crashes.
        std::thread::sleep(Duration::from_secs(if consecutive >= 3 { 5 } else { 1 }));
    }
    consecutive >= 3 && child.try_wait().ok().flatten().is_none()
}

fn recover(paths: &Paths, previous: Slot, child: Option<&mut Child>) -> i32 {
    if let Some(child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = paths.write_active(previous);
    let _ = paths.point_legacy_link(previous);
    let _ = paths.clear_pending();
    match Command::new(paths.slot_binary(previous))
        .stdin(Stdio::null())
        .stdout(log_file(&paths.update_log))
        .stderr(log_file(&paths.update_log))
        .spawn()
    {
        Ok(_) => 4,
        Err(error) => {
            paths.append_log(&format!("rollback agent could not start: {error}"));
            5
        }
    }
}

fn block_version(paths: &Paths, version: &str) {
    let mut blocked: Vec<String> = storage::read_json(&paths.blocked).unwrap_or_default();
    if !blocked.iter().any(|item| item == version) {
        blocked.push(version.to_owned());
        let _ = storage::write_json(&paths.blocked, &blocked);
    }
}

fn log_file(path: &Path) -> Stdio {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null())
}

fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn bind_address() -> SocketAddr {
    let mut address: SocketAddr = std::env::var("ZTE_AGENT_BIND")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:9090".parse().expect("static socket address"));
    if address.ip().is_unspecified() {
        address.set_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    }
    address
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn status_api(state: &crate::handlers::AppState) -> (u16, Value) {
    (200, state.ota.status())
}

pub fn settings_api(state: &crate::handlers::AppState, body: &[u8]) -> (u16, Value) {
    mutate(state, || state.ota.update_settings(body))
}

pub fn check_api(state: &crate::handlers::AppState) -> (u16, Value) {
    mutate(state, || state.ota.check())
}

pub fn download_api(state: &crate::handlers::AppState) -> (u16, Value) {
    mutate(state, || state.ota.download())
}

pub fn install_api(state: &crate::handlers::AppState, body: &[u8]) -> (u16, Value) {
    if !confirmed(body) {
        return (
            400,
            json!({"ok":false,"error":"explicit confirmation required"}),
        );
    }
    if let Some(reason) = install_busy_reason(state) {
        return (
            409,
            json!({"ok":false,"error":format!("update deferred during {reason}")}),
        );
    }
    mutate(state, || state.ota.install())
}

pub fn rollback_api(state: &crate::handlers::AppState, body: &[u8]) -> (u16, Value) {
    if !confirmed(body) {
        return (
            400,
            json!({"ok":false,"error":"explicit confirmation required"}),
        );
    }
    if let Some(reason) = install_busy_reason(state) {
        return (
            409,
            json!({"ok":false,"error":format!("rollback deferred during {reason}")}),
        );
    }
    mutate(state, || state.ota.rollback())
}

fn mutate(
    state: &crate::handlers::AppState,
    action: impl FnOnce() -> Result<(), String>,
) -> (u16, Value) {
    if !state.auth.has_password() {
        return (403, json!({"ok":false,"error":"agent password required"}));
    }
    match action() {
        Ok(()) => (200, state.ota.status()),
        Err(error) => (
            503,
            json!({"ok":false,"error":error,"data":state.ota.status()["data"].clone()}),
        ),
    }
}

fn confirmed(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .is_some_and(|value| value["confirm"] == true)
}

fn install_busy_reason(state: &crate::handlers::AppState) -> Option<&'static str> {
    use std::sync::atomic::Ordering;
    if state.speedtest.is_running() {
        return Some("a speed test");
    }
    if state.esim.recently_switched() {
        return Some("an eSIM profile switch");
    }
    if state.call_active.load(Ordering::Relaxed) {
        return Some("a voice call");
    }
    if crate::system::read_battery().is_some_and(|battery| {
        battery.capacity <= 15
            && !matches!(
                battery.status.to_ascii_lowercase().as_str(),
                "charging" | "full"
            )
    }) {
        return Some("a critically low battery");
    }
    let ussd_at = state.ussd_prompt_at.load(Ordering::Relaxed);
    if ussd_at != 0 && unix_now().saturating_sub(ussd_at) < 180 {
        return Some("a USSD session");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::SigningKey;

    fn manager(name: &str) -> OtaManager {
        let root =
            std::env::temp_dir().join(format!("u60-ota-manager-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let key = SigningKey::from_bytes((&[9u8; 32]).into()).unwrap();
        OtaManager::test(
            Paths::new(root.clone(), root.join("supervisor"), root.join("legacy")),
            ManifestVerifier::from_key(*key.verifying_key()),
        )
    }

    fn release(channel: Channel, version: &str) -> ReleaseManifest {
        let tag = format!("v{version}");
        ReleaseManifest {
            schema: 1,
            channel,
            version: version.into(),
            tag: tag.clone(),
            commit: "1234567890ab".into(),
            target: manifest::RELEASE_TARGET.into(),
            size: 7_000_000,
            sha256: "a".repeat(64),
            binary_url: format!(
                "https://github.com/jesther-ai/open-u60-pro/releases/download/{tag}/zte-agent"
            ),
            notes_url: format!("https://github.com/jesther-ai/open-u60-pro/releases/tag/{tag}"),
            published_at: "2026-11-02T00:00:00Z".into(),
            minimum_updater_version: "2.4.0-beta.1".into(),
        }
    }

    #[test]
    fn automatic_install_cannot_be_enabled() {
        let manager = manager("settings");
        manager
            .update_settings(
                br#"{"channel":"beta","automatic_checks":false,"automatic_downloads":true}"#,
            )
            .unwrap();
        let settings = manager.settings();
        assert_eq!(settings.channel, Channel::Beta);
        assert!(settings.automatic_downloads);
        assert!(!settings.automatic_install);
    }

    #[test]
    fn switching_channels_discards_the_old_candidate() {
        let manager = manager("channel-switch");
        manager.change(|state| {
            state.persistent.phase = Phase::Staged;
            state.persistent.available = Some(release(Channel::Stable, "2.4.1"));
            state.persistent.staged = Some(release(Channel::Stable, "2.4.1"));
            state.persistent.staged_slot = Some("b".into());
            state.persistent.last_check = Some(1);
            state.bytes_received = 7_000_000;
            state.bytes_total = 7_000_000;
        });

        manager
            .update_settings(
                br#"{"channel":"beta","automatic_checks":true,"automatic_downloads":false}"#,
            )
            .unwrap();

        let state = manager.state.lock().unwrap();
        assert_eq!(state.persistent.phase, Phase::Idle);
        assert!(state.persistent.available.is_none());
        assert!(state.persistent.staged.is_none());
        assert!(state.persistent.staged_slot.is_none());
        assert!(state.persistent.last_check.is_none());
        assert_eq!(state.bytes_received, 0);
        assert_eq!(state.bytes_total, 0);
    }

    #[test]
    fn preference_changes_wait_for_an_update_operation() {
        let manager = manager("settings-operation");
        let _operation = manager.begin(Phase::Checking).unwrap();
        assert_eq!(
            manager
                .update_settings(
                    br#"{"channel":"beta","automatic_checks":true,"automatic_downloads":false}"#,
                )
                .unwrap_err(),
            "another update operation is already running"
        );
    }

    #[test]
    fn a_staged_candidate_is_not_presented_as_a_rollback() {
        let manager = manager("rollback-status");
        manager.paths.write_active(Slot::A).unwrap();
        fs::write(manager.paths.slot_binary(Slot::A), b"active").unwrap();
        fs::write(manager.paths.slot_binary(Slot::B), b"inactive").unwrap();
        for slot in [Slot::A, Slot::B] {
            fs::set_permissions(
                manager.paths.slot_binary(slot),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        manager.change(|state| {
            state.persistent.staged = Some(release(Channel::Stable, "2.4.1"));
            state.persistent.staged_slot = Some("b".into());
        });
        assert_eq!(manager.status()["data"]["rollback_available"], false);

        manager.change(|state| {
            state.persistent.staged = None;
            state.persistent.staged_slot = None;
        });
        assert_eq!(manager.status()["data"]["rollback_available"], true);
    }

    #[test]
    fn overlapping_operations_are_rejected() {
        let manager = manager("operation");
        let _first = manager.begin(Phase::Checking).unwrap();
        assert_eq!(
            manager.begin(Phase::Downloading).unwrap_err(),
            "another update operation is already running"
        );
    }

    #[test]
    fn update_mutations_require_an_agent_password() {
        let state = crate::handlers::AppState::new();
        assert_eq!(
            settings_api(
                &state,
                br#"{"channel":"stable","automatic_checks":true,"automatic_downloads":false}"#
            )
            .0,
            403
        );
        state.auth.set_password("fixture-only");
        assert_eq!(
            settings_api(
                &state,
                br#"{"channel":"stable","automatic_checks":true,"automatic_downloads":false}"#
            )
            .0,
            200
        );
        assert_eq!(install_api(&state, br#"{"confirm":false}"#).0, 400);
    }
}
