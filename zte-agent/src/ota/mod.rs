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
use std::path::{Path, PathBuf};
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
const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const REQUIRED_HEADROOM: u64 = 8 * 1024 * 1024;
const TRIAL_SECONDS: u64 = 60;
const SUPERVISOR_SCRIPT: &[u8] = include_bytes!("../../../scripts/router/zte-agent-supervisor.sh");

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
        if std::env::var("ZTE_OTA_TRIAL").as_deref() != Ok("1") {
            if let Err(error) = ensure_supervisor(&paths) {
                eprintln!("[ota] cannot repair update supervisor: {error}");
            }
        }
        let mut settings = storage::read_json::<Settings>(&paths.settings).unwrap_or_default();
        settings.automatic_install = false;
        let mut persistent = storage::read_json(&paths.state).unwrap_or_default();
        if let Some(message) = reconcile_startup_state(&paths, &mut persistent) {
            if let Err(error) = storage::write_json(&paths.state, &persistent) {
                eprintln!("[ota] cannot save recovered update state: {error}");
            }
            paths.append_log(message);
        }
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
                        if let Err(error) = ensure_supervisor(&manager.paths) {
                            manager.paths.append_log(&format!(
                                "could not repair update supervisor: {error}"
                            ));
                        }
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
        let beta = if channel == Channel::Beta {
            self.fetch_manifest(beta_url())?
        } else {
            None
        };
        let current = Version::parse(crate::build_info::VERSION)
            .map_err(|_| "current build has an invalid version".to_string())?;
        let blocked: Vec<String> = storage::read_json(&self.paths.blocked).unwrap_or_default();
        select_candidate(channel, &current, stable, beta, &blocked)
    }

    fn fetch_manifest(&self, url: &str) -> Result<Option<ReleaseManifest>, String> {
        validate_channel_url(url)?;
        let response = client(HTTP_TIMEOUT)?
            .get(url)
            .send()
            .map_err(|e| format!("release channel request failed: {e}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = response
            .error_for_status()
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
        self.verifier
            .verify(&bytes)
            .map(Some)
            .map_err(|e| e.to_string())
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
        let metadata_destination = self.paths.slot_meta_download(slot);
        let _cleanup = RemoveFilesOnDrop([destination.clone(), metadata_destination.clone()]);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o700)
            .open(&destination)
            .map_err(|e| e.to_string())?;
        let mut response = client(DOWNLOAD_IDLE_TIMEOUT)?
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
        stream_verified_binary(
            &mut response,
            &mut file,
            manifest.size,
            &manifest.sha256,
            DOWNLOAD_TIMEOUT,
            |received| self.change(|state| state.bytes_received = received),
        )?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        probe(&destination, &manifest.version)?;
        storage::write_json(&metadata_destination, &manifest).map_err(|e| e.to_string())?;
        fs::rename(&destination, self.paths.slot_binary(slot)).map_err(|e| e.to_string())?;
        storage::sync_parent(&self.paths.slot_binary(slot)).map_err(|e| e.to_string())?;
        fs::rename(&metadata_destination, self.paths.slot_meta(slot)).map_err(|e| e.to_string())?;
        storage::sync_parent(&self.paths.slot_meta(slot)).map_err(|e| e.to_string())?;
        self.paths
            .clear_slot_bootstrap_digest(slot)
            .map_err(|e| format!("cannot retire bootstrap digest: {e}"))?;
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
        ensure_supervisor(&self.paths)
            .map_err(|error| format!("update supervisor is not repairable: {error}"))?;
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
        if slot == previous {
            return Err("staged update must be in the inactive slot".into());
        }
        verify_staged_slot(&self.paths, slot, &manifest)?;
        verify_active_slot(&self.paths, previous)?;
        let current = std::env::current_exe().map_err(|e| e.to_string())?;
        launch_helper(
            &self.paths,
            &current,
            slot,
            previous,
            &manifest.version,
            std::process::id(),
        )?;
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
        ensure_supervisor(&self.paths)
            .map_err(|error| format!("update supervisor is not repairable: {error}"))?;
        let active = self
            .paths
            .active_slot()
            .ok_or_else(|| "active OTA slot is missing".to_string())?;
        verify_active_slot(&self.paths, active)?;
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
        let version = verify_rollback_slot(&self.paths, previous)?;
        let current = std::env::current_exe().map_err(|e| e.to_string())?;
        launch_helper(
            &self.paths,
            &current,
            previous,
            active,
            &version,
            std::process::id(),
        )?;
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

struct RemoveFilesOnDrop([PathBuf; 2]);

impl Drop for RemoveFilesOnDrop {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn ensure_supervisor(paths: &Paths) -> std::io::Result<()> {
    let current = fs::read(&paths.supervisor).ok();
    let executable = fs::metadata(&paths.supervisor)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if current.as_deref() == Some(SUPERVISOR_SCRIPT) && executable {
        return Ok(());
    }
    storage::atomic_write_preserving_parent(&paths.supervisor, SUPERVISOR_SCRIPT, 0o755)
}

fn stream_verified_binary(
    reader: &mut impl Read,
    writer: &mut impl Write,
    expected_size: u64,
    expected_sha256: &str,
    total_timeout: Duration,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    let started = Instant::now();
    let mut digest = Sha256::new();
    let mut received = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if started.elapsed() >= total_timeout {
            return Err("binary download exceeded the total time limit".into());
        }
        let count = reader.read(&mut buffer).map_err(|error| {
            if error.kind() == std::io::ErrorKind::TimedOut {
                "binary download stalled".to_string()
            } else {
                format!("binary download read failed: {error}")
            }
        })?;
        if started.elapsed() >= total_timeout {
            return Err("binary download exceeded the total time limit".into());
        }
        if count == 0 {
            break;
        }
        received = received.saturating_add(count as u64);
        if received > expected_size || received > MAX_BINARY_BYTES {
            return Err("download exceeded the signed size".into());
        }
        writer
            .write_all(&buffer[..count])
            .map_err(|error| format!("cannot write update download: {error}"))?;
        digest.update(&buffer[..count]);
        progress(received);
    }
    if received != expected_size || expected_sha256 != manifest::hex(&digest.finalize()) {
        return Err("download failed signed size or SHA-256 verification".into());
    }
    Ok(())
}

fn verify_file_integrity(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("update binary is missing: {error}"))?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err("update binary size no longer matches the signed manifest".into());
    }
    let mut file =
        fs::File::open(path).map_err(|error| format!("cannot read update binary: {error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read update binary: {error}"))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if expected_sha256 != manifest::hex(&digest.finalize()) {
        return Err("update binary SHA-256 no longer matches the signed manifest".into());
    }
    Ok(())
}

fn verify_release_binary(path: &Path, manifest: &ReleaseManifest) -> Result<(), String> {
    manifest.validate().map_err(|error| error.to_string())?;
    if !storage::executable(path) {
        return Err("update binary is not executable".into());
    }
    verify_file_integrity(path, manifest.size, &manifest.sha256)?;
    probe(path, &manifest.version)
}

fn verify_staged_slot(paths: &Paths, slot: Slot, manifest: &ReleaseManifest) -> Result<(), String> {
    let stored = storage::read_json::<ReleaseManifest>(&paths.slot_meta(slot))
        .ok_or_else(|| "staged update metadata is missing or corrupted".to_string())?;
    if stored != *manifest {
        return Err("staged update metadata changed after download".into());
    }
    verify_release_binary(&paths.slot_binary(slot), manifest)
        .map_err(|error| format!("staged update failed preflight verification: {error}"))
}

fn verify_active_slot(paths: &Paths, slot: Slot) -> Result<(), String> {
    let binary = paths.slot_binary(slot);
    if let Some(manifest) = storage::read_json::<ReleaseManifest>(&paths.slot_meta(slot)) {
        if manifest.version != crate::build_info::VERSION {
            return Err("active rollback slot metadata does not match the running agent".into());
        }
        return verify_release_binary(&binary, &manifest).map_err(|error| {
            format!("active rollback slot failed preflight verification: {error}")
        });
    }
    if paths.slot_bootstrap_digest(slot).exists() {
        verify_bootstrap_slot(paths, slot, Some(crate::build_info::VERSION)).map_err(|error| {
            format!("active bootstrap slot failed preflight verification: {error}")
        })?;
        return Ok(());
    }
    if !storage::executable(&binary) {
        return Err("active rollback slot is missing".into());
    }
    probe(&binary, crate::build_info::VERSION)
        .map_err(|error| format!("active rollback slot failed preflight verification: {error}"))
}

fn verify_rollback_slot(paths: &Paths, slot: Slot) -> Result<String, String> {
    let binary = paths.slot_binary(slot);
    if let Some(manifest) = storage::read_json::<ReleaseManifest>(&paths.slot_meta(slot)) {
        verify_release_binary(&binary, &manifest)
            .map_err(|error| format!("rollback slot failed preflight verification: {error}"))?;
        return Ok(manifest.version);
    }
    if paths.slot_bootstrap_digest(slot).exists() {
        return verify_bootstrap_slot(paths, slot, None)
            .map_err(|error| format!("rollback bootstrap slot failed preflight: {error}"));
    }
    if storage::executable(&binary) {
        Ok("bootstrap-slot".into())
    } else {
        Err("no previous agent slot is available".into())
    }
}

fn verify_bootstrap_slot(
    paths: &Paths,
    slot: Slot,
    expected_version: Option<&str>,
) -> Result<String, String> {
    let identity = fs::read_to_string(paths.slot_bootstrap_digest(slot))
        .map_err(|error| format!("bootstrap digest is unreadable: {error}"))?;
    let mut lines = identity.lines();
    let sha256 = lines.next().unwrap_or_default();
    let version = lines.next().unwrap_or_default();
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || Version::parse(version).is_err()
    {
        return Err("bootstrap identity is malformed".into());
    }
    if expected_version.is_some_and(|expected| expected != version) {
        return Err("bootstrap identity does not match the running agent".into());
    }
    let binary = paths.slot_binary(slot);
    if !storage::executable(&binary) {
        return Err("bootstrap binary is not executable".into());
    }
    let size = fs::metadata(&binary)
        .map_err(|error| format!("bootstrap binary is missing: {error}"))?
        .len();
    verify_file_integrity(&binary, size, sha256)?;
    probe(&binary, version)?;
    Ok(version.into())
}

fn launch_helper(
    paths: &Paths,
    helper: &Path,
    candidate: Slot,
    previous: Slot,
    version: &str,
    parent: u32,
) -> Result<(), String> {
    paths
        .clear_helper_ready()
        .map_err(|error| format!("cannot clear stale helper handshake: {error}"))?;
    paths
        .write_pending(candidate, previous, version)
        .map_err(|error| error.to_string())?;
    let parent = parent.to_string();
    let result = Command::new(helper)
        .args([
            "ota-helper",
            candidate.as_str(),
            previous.as_str(),
            version,
            &parent,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match result {
        Ok(child) => child,
        Err(error) => {
            return clear_failed_launch(paths, format!("cannot start update helper: {error}"))
        }
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if paths.helper_ready.exists() {
            if let Err(error) = paths.clear_helper_ready() {
                let _ = child.kill();
                let _ = child.wait();
                return clear_failed_launch(
                    paths,
                    format!("cannot clear helper handshake: {error}"),
                );
            }
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return clear_failed_launch(
                paths,
                format!("update helper exited before handoff with {status}"),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    clear_failed_launch(paths, "update helper did not confirm startup".into())
}

fn clear_failed_launch(paths: &Paths, error: String) -> Result<(), String> {
    let ready_error = paths.clear_helper_ready().err();
    match paths.clear_pending() {
        Ok(()) if ready_error.is_none() => Err(error),
        Ok(()) => Err(format!(
            "{error}; cannot clear helper handshake: {}",
            ready_error.expect("checked")
        )),
        Err(cleanup) => Err(format!("{error}; cannot clear pending marker: {cleanup}")),
    }
}

fn reconcile_startup_state(
    paths: &Paths,
    persistent: &mut PersistentState,
) -> Option<&'static str> {
    // A live helper owns state reconciliation while its pending marker exists.
    // Without that marker, a transient phase survived a process or router
    // restart and must not leave the console permanently stuck in progress.
    if paths.pending.exists() {
        return None;
    }

    match persistent.phase {
        Phase::Installing => {
            let candidate = persistent.staged_slot.as_deref().and_then(Slot::parse);
            let completed = candidate.is_some() && candidate == paths.active_slot();
            persistent.phase = Phase::Idle;
            persistent.staged = None;
            persistent.staged_slot = None;
            if completed {
                persistent.available = None;
                persistent.last_error = None;
                Some("reconciled a completed update after restart")
            } else {
                persistent.last_error = Some(
                    "the previous update trial was interrupted; the previous agent was restored"
                        .into(),
                );
                Some("recovered an interrupted update trial at startup")
            }
        }
        Phase::RollingBack => {
            persistent.phase = Phase::Idle;
            persistent.staged = None;
            persistent.staged_slot = None;
            persistent.last_error = Some(
                "the previous rollback trial was interrupted; an active agent was restored".into(),
            );
            Some("recovered an interrupted rollback trial at startup")
        }
        Phase::Checking | Phase::Downloading => {
            persistent.phase = if persistent.staged.is_some() {
                Phase::Staged
            } else {
                Phase::Idle
            };
            persistent.last_error =
                Some("the previous update operation was interrupted; retry the operation".into());
            Some("recovered an interrupted update operation at startup")
        }
        Phase::Idle | Phase::Staged | Phase::Failed => None,
    }
}

fn select_candidate(
    channel: Channel,
    current: &Version,
    stable: Option<ReleaseManifest>,
    beta: Option<ReleaseManifest>,
    blocked: &[String],
) -> Result<Option<ReleaseManifest>, String> {
    let candidate = if channel == Channel::Stable {
        let Some(stable) = stable else {
            return Ok(None);
        };
        // Leaving Beta explicitly returns to the newest Stable release, even
        // when it sorts below a later prerelease already installed.
        let version = stable.semver().map_err(|e| e.to_string())?;
        (stable.version != crate::build_info::VERSION
            && !blocked.contains(&stable.version)
            && (version.cmp(current).is_gt() || !current.pre.is_empty()))
        .then_some(stable)
    } else {
        let mut releases: Vec<_> = stable.into_iter().chain(beta).collect();
        releases.sort_by_key(|release| release.semver().ok());
        releases
            .into_iter()
            .filter(|release| !blocked.contains(&release.version))
            .filter(|release| {
                release
                    .semver()
                    .is_ok_and(|version| version.cmp(current).is_gt())
            })
            .next_back()
    };
    if let Some(release) = &candidate {
        if release
            .minimum_updater_semver()
            .map_err(|e| e.to_string())?
            .cmp(current)
            .is_gt()
        {
            return Err(format!(
                "{} requires updater {} or newer",
                release.version, release.minimum_updater_version
            ));
        }
    }
    Ok(candidate)
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
    if let Err(error) = paths.write_helper_ready() {
        paths.append_log(&format!("update helper could not confirm startup: {error}"));
        return 7;
    }
    if !wait_for_exit(parent, Duration::from_secs(15)) {
        paths.append_log("update helper timed out waiting for the previous agent");
        if let Err(error) = paths.clear_pending() {
            paths.append_log(&format!("could not clear pending update marker: {error}"));
        }
        let _ = paths.clear_helper_ready();
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
    if let Err(error) = paths.write_active(previous) {
        paths.append_log(&format!("could not restore active slot marker: {error}"));
    }
    if let Err(error) = paths.point_legacy_link(previous) {
        paths.append_log(&format!("could not restore legacy agent link: {error}"));
    }
    if let Err(error) = paths.clear_pending() {
        paths.append_log(&format!("could not clear pending update marker: {error}"));
    }
    let previous_error = match verify_active_slot(paths, previous) {
        Ok(()) => match Command::new(paths.slot_binary(previous))
            .stdin(Stdio::null())
            .stdout(log_file(&paths.update_log))
            .stderr(log_file(&paths.update_log))
            .spawn()
        {
            Ok(_) => return 4,
            Err(error) => format!("rollback agent could not start: {error}"),
        },
        Err(error) => format!("rollback agent is no longer safe: {error}"),
    };
    paths.append_log(&previous_error);

    let stable = paths.stable_binary();
    let Some(manifest) = storage::read_json::<ReleaseManifest>(&paths.stable_meta()) else {
        paths.append_log("Stable rescue metadata is missing or corrupted");
        return 5;
    };
    if let Err(error) = verify_release_binary(&stable, &manifest) {
        paths.append_log(&format!("Stable rescue agent is not safe: {error}"));
        return 5;
    }
    match Command::new(&stable)
        .stdin(Stdio::null())
        .stdout(log_file(&paths.update_log))
        .stderr(log_file(&paths.update_log))
        .spawn()
    {
        Ok(_) => {
            if let Err(error) = paths.write_rescue() {
                paths.append_log(&format!("could not mark Stable rescue mode: {error}"));
            }
            if let Err(error) = paths.point_legacy_path(&stable) {
                paths.append_log(&format!("could not link the Stable rescue agent: {error}"));
            }
            paths.append_log("started the last Stable rescue agent");
            6
        }
        Err(error) => {
            paths.append_log(&format!("Stable rescue agent could not start: {error}"));
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

    struct TimedOutReader;

    impl Read for TimedOutReader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "fixture stalled",
            ))
        }
    }

    struct SlowReader {
        inner: std::io::Cursor<Vec<u8>>,
        delay: Duration,
    }

    impl Read for SlowReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(self.delay);
            self.inner.read(buffer)
        }
    }

    struct FailedWriter;

    impl Write for FailedWriter {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "fixture storage failure",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

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

    fn sha256(bytes: &[u8]) -> String {
        manifest::hex(&Sha256::digest(bytes))
    }

    fn release_for_binary(channel: Channel, version: &str, bytes: &[u8]) -> ReleaseManifest {
        let mut manifest = release(channel, version);
        manifest.size = bytes.len() as u64;
        manifest.sha256 = sha256(bytes);
        manifest
    }

    fn test_agent(version: &str, probe_ok: bool, marker: &str) -> Vec<u8> {
        let mut script = format!(
            "#!/bin/sh\nif [ \"${{1:-}}\" = ota-probe ]; then\n  [ {probe_ok} = true ] && {{ [ -z \"${{2:-}}\" ] || [ \"$2\" = \"{version}\" ]; }} && exit 0\n  exit 2\nfi\n[ -z \"${{ZTE_TEST_MARKER:-}}\" ] || printf '%s' '{marker}' > \"$ZTE_TEST_MARKER\"\n#"
        )
        .into_bytes();
        script.resize(1024, b'x');
        script
    }

    fn write_release_agent(paths: &Paths, slot: Slot, manifest: &ReleaseManifest, bytes: &[u8]) {
        fs::write(paths.slot_binary(slot), bytes).unwrap();
        fs::set_permissions(paths.slot_binary(slot), fs::Permissions::from_mode(0o755)).unwrap();
        storage::write_json(&paths.slot_meta(slot), manifest).unwrap();
    }

    fn channel_response(status: &str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_owned();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
        });
        format!("http://{address}/channel.json")
    }

    #[test]
    fn beta_channel_selects_beta_before_the_first_stable_pointer() {
        let current = Version::parse("2.4.0-beta.1").unwrap();
        let candidate = select_candidate(
            Channel::Beta,
            &current,
            None,
            Some(release(Channel::Beta, "2.4.0-beta.2")),
            &[],
        )
        .unwrap()
        .unwrap();

        assert_eq!(candidate.version, "2.4.0-beta.2");
    }

    #[test]
    fn stable_channel_has_no_candidate_before_its_first_pointer() {
        let current = Version::parse("2.4.0-beta.1").unwrap();
        let candidate = select_candidate(Channel::Stable, &current, None, None, &[]).unwrap();

        assert!(candidate.is_none());
    }

    #[test]
    fn missing_channel_pointer_is_not_an_update_failure() {
        let manager = manager("missing-channel");
        let result = manager
            .fetch_manifest(&channel_response("404 Not Found"))
            .unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn channel_server_failure_is_not_treated_as_unpublished() {
        let manager = manager("failed-channel");
        let error = manager
            .fetch_manifest(&channel_response("503 Service Unavailable"))
            .unwrap_err();

        assert!(error.contains("503 Service Unavailable"), "{error}");
    }

    #[test]
    fn interrupted_trial_clears_staging_after_the_supervisor_restores_previous() {
        let manager = manager("interrupted-trial");
        manager.paths.write_active(Slot::B).unwrap();
        let candidate = release(Channel::Beta, "2.4.0-beta.5");
        let mut persistent = PersistentState {
            phase: Phase::Installing,
            available: Some(candidate.clone()),
            staged: Some(candidate),
            staged_slot: Some("a".into()),
            ..PersistentState::default()
        };

        let message = reconcile_startup_state(&manager.paths, &mut persistent);

        assert_eq!(
            message,
            Some("recovered an interrupted update trial at startup")
        );
        assert_eq!(persistent.phase, Phase::Idle);
        assert!(persistent.available.is_some());
        assert!(persistent.staged.is_none());
        assert!(persistent.staged_slot.is_none());
        assert!(persistent
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("previous agent was restored")));
    }

    #[test]
    fn completed_trial_clears_the_installed_candidate_without_an_error() {
        let manager = manager("completed-trial");
        manager.paths.write_active(Slot::A).unwrap();
        let candidate = release(Channel::Beta, "2.4.0-beta.5");
        let mut persistent = PersistentState {
            phase: Phase::Installing,
            available: Some(candidate.clone()),
            staged: Some(candidate),
            staged_slot: Some("a".into()),
            last_error: Some("old error".into()),
            ..PersistentState::default()
        };

        let message = reconcile_startup_state(&manager.paths, &mut persistent);

        assert_eq!(message, Some("reconciled a completed update after restart"));
        assert_eq!(persistent.phase, Phase::Idle);
        assert!(persistent.available.is_none());
        assert!(persistent.staged.is_none());
        assert!(persistent.staged_slot.is_none());
        assert!(persistent.last_error.is_none());
    }

    #[test]
    fn live_trial_is_left_for_the_update_helper_to_reconcile() {
        let manager = manager("live-trial");
        manager.paths.write_active(Slot::B).unwrap();
        manager
            .paths
            .write_pending(Slot::A, Slot::B, "2.4.0-beta.5")
            .unwrap();
        let candidate = release(Channel::Beta, "2.4.0-beta.5");
        let mut persistent = PersistentState {
            phase: Phase::Installing,
            available: Some(candidate.clone()),
            staged: Some(candidate),
            staged_slot: Some("a".into()),
            ..PersistentState::default()
        };

        assert!(reconcile_startup_state(&manager.paths, &mut persistent).is_none());
        assert_eq!(persistent.phase, Phase::Installing);
        assert!(persistent.staged.is_some());
        assert_eq!(persistent.staged_slot.as_deref(), Some("a"));
    }

    #[test]
    fn interrupted_download_returns_to_an_actionable_idle_state() {
        let manager = manager("interrupted-download");
        let mut persistent = PersistentState {
            phase: Phase::Downloading,
            available: Some(release(Channel::Beta, "2.4.0-beta.5")),
            ..PersistentState::default()
        };

        let message = reconcile_startup_state(&manager.paths, &mut persistent);

        assert_eq!(
            message,
            Some("recovered an interrupted update operation at startup")
        );
        assert_eq!(persistent.phase, Phase::Idle);
        assert!(persistent.available.is_some());
        assert!(persistent
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("retry")));
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

    #[test]
    fn verified_stream_accepts_only_the_exact_signed_payload() {
        let bytes = vec![0x5a; 1024];
        let digest = sha256(&bytes);
        let mut output = Vec::new();
        let mut progress = Vec::new();

        stream_verified_binary(
            &mut bytes.as_slice(),
            &mut output,
            bytes.len() as u64,
            &digest,
            Duration::from_secs(1),
            |received| progress.push(received),
        )
        .unwrap();

        assert_eq!(output, bytes);
        assert_eq!(progress.last(), Some(&(bytes.len() as u64)));
    }

    #[test]
    fn verified_stream_rejects_truncated_oversized_and_corrupted_payloads() {
        let signed = vec![0x41; 1024];
        let digest = sha256(&signed);
        for (name, payload) in [
            ("truncated", signed[..1023].to_vec()),
            ("oversized", [signed.clone(), vec![0x41]].concat()),
            ("corrupted", vec![0x42; 1024]),
        ] {
            let error = stream_verified_binary(
                &mut payload.as_slice(),
                &mut Vec::new(),
                signed.len() as u64,
                &digest,
                Duration::from_secs(1),
                |_| {},
            )
            .unwrap_err();
            assert!(
                error.contains("signed size") || error.contains("SHA-256"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn verified_stream_stops_on_idle_total_and_storage_failures() {
        let bytes = vec![0x51; 1024];
        let digest = sha256(&bytes);
        let idle_error = stream_verified_binary(
            &mut TimedOutReader,
            &mut Vec::new(),
            bytes.len() as u64,
            &digest,
            Duration::from_secs(1),
            |_| {},
        )
        .unwrap_err();
        assert!(idle_error.contains("stalled"), "{idle_error}");

        let total_error = stream_verified_binary(
            &mut SlowReader {
                inner: std::io::Cursor::new(bytes.clone()),
                delay: Duration::from_millis(10),
            },
            &mut Vec::new(),
            bytes.len() as u64,
            &digest,
            Duration::from_millis(1),
            |_| {},
        )
        .unwrap_err();
        assert!(total_error.contains("total time limit"), "{total_error}");

        let write_error = stream_verified_binary(
            &mut bytes.as_slice(),
            &mut FailedWriter,
            bytes.len() as u64,
            &digest,
            Duration::from_secs(1),
            |_| {},
        )
        .unwrap_err();
        assert!(write_error.contains("cannot write"), "{write_error}");
    }

    #[test]
    fn install_preflight_rejects_a_staged_binary_changed_after_download() {
        let manager = manager("staged-corruption");
        let bytes = test_agent("2.4.0-beta.8", true, "candidate");
        let manifest = release_for_binary(Channel::Beta, "2.4.0-beta.8", &bytes);
        write_release_agent(&manager.paths, Slot::B, &manifest, &bytes);
        verify_staged_slot(&manager.paths, Slot::B, &manifest).unwrap();

        let mut corrupted = bytes;
        *corrupted.last_mut().unwrap() = b'y';
        fs::write(manager.paths.slot_binary(Slot::B), corrupted).unwrap();
        fs::set_permissions(
            manager.paths.slot_binary(Slot::B),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let error = verify_staged_slot(&manager.paths, Slot::B, &manifest).unwrap_err();
        assert!(error.contains("SHA-256"), "{error}");
        assert!(!manager.paths.pending.exists());
    }

    #[test]
    fn install_preflight_rejects_changed_staged_metadata() {
        let manager = manager("staged-metadata");
        let bytes = test_agent("2.4.0-beta.8", true, "candidate");
        let expected = release_for_binary(Channel::Beta, "2.4.0-beta.8", &bytes);
        let changed = release_for_binary(Channel::Beta, "2.4.0-beta.9", &bytes);
        write_release_agent(&manager.paths, Slot::B, &changed, &bytes);

        let error = verify_staged_slot(&manager.paths, Slot::B, &expected).unwrap_err();
        assert!(error.contains("metadata changed"), "{error}");
    }

    #[test]
    fn install_preflight_rejects_a_corrupted_bootstrap_fallback() {
        let manager = manager("bootstrap-preflight");
        let original = test_agent(crate::build_info::VERSION, true, "active");
        let mut corrupted = original.clone();
        *corrupted.last_mut().unwrap() = b'y';
        fs::write(manager.paths.slot_binary(Slot::A), corrupted).unwrap();
        fs::set_permissions(
            manager.paths.slot_binary(Slot::A),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(
            manager.paths.slot_bootstrap_digest(Slot::A),
            format!("{}\n{}\n", sha256(&original), crate::build_info::VERSION),
        )
        .unwrap();

        let error = verify_active_slot(&manager.paths, Slot::A).unwrap_err();
        assert!(error.contains("SHA-256"), "{error}");
    }

    #[test]
    fn helper_launch_failure_clears_the_pending_marker() {
        let manager = manager("helper-launch");
        let error = launch_helper(
            &manager.paths,
            &manager.paths.root.join("missing-helper"),
            Slot::B,
            Slot::A,
            "2.4.0-beta.8",
            std::process::id(),
        )
        .unwrap_err();

        assert!(error.contains("cannot start update helper"), "{error}");
        assert!(!manager.paths.pending.exists());
    }

    #[test]
    fn helper_immediate_exit_clears_the_pending_marker() {
        let manager = manager("helper-exit");
        let error = launch_helper(
            &manager.paths,
            Path::new("/usr/bin/false"),
            Slot::B,
            Slot::A,
            "2.4.0-beta.8",
            std::process::id(),
        )
        .unwrap_err();

        assert!(error.contains("exited before handoff"), "{error}");
        assert!(!manager.paths.pending.exists());
    }

    #[test]
    fn helper_must_confirm_startup_before_the_agent_can_exit() {
        let manager = manager("helper-handshake");
        let helper = manager.paths.root.join("fixture-helper.sh");
        fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf 'ready\\n' > '{}'\nsleep 1\n",
                manager.paths.helper_ready.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();

        launch_helper(
            &manager.paths,
            &helper,
            Slot::B,
            Slot::A,
            "2.4.0-beta.8",
            std::process::id(),
        )
        .unwrap();

        assert!(manager.paths.pending.exists());
        assert!(!manager.paths.helper_ready.exists());
        manager.paths.clear_pending().unwrap();
    }

    #[test]
    fn missing_or_damaged_supervisor_is_repaired_atomically() {
        let manager = manager("supervisor-repair");
        fs::write(&manager.paths.supervisor, b"damaged").unwrap();
        fs::set_permissions(&manager.paths.supervisor, fs::Permissions::from_mode(0o600)).unwrap();

        ensure_supervisor(&manager.paths).unwrap();

        assert_eq!(
            fs::read(&manager.paths.supervisor).unwrap(),
            SUPERVISOR_SCRIPT
        );
        assert!(storage::executable(&manager.paths.supervisor));
    }

    #[test]
    fn supervisor_skips_a_corrupted_active_slot_and_runs_the_verified_fallback() {
        let manager = manager("supervisor-fallback");
        ensure_supervisor(&manager.paths).unwrap();
        let marker = manager.paths.root.join("started");
        let active_bytes = test_agent("2.4.0-beta.7", true, "a");
        let active = release_for_binary(Channel::Beta, "2.4.0-beta.7", &active_bytes);
        let mut corrupted_active = active_bytes;
        *corrupted_active.last_mut().unwrap() = b'y';
        write_release_agent(&manager.paths, Slot::A, &active, &corrupted_active);
        let fallback_bytes = test_agent("2.4.0-beta.8", true, "b");
        let fallback = release_for_binary(Channel::Beta, "2.4.0-beta.8", &fallback_bytes);
        write_release_agent(&manager.paths, Slot::B, &fallback, &fallback_bytes);
        manager.paths.write_active(Slot::A).unwrap();

        let status = Command::new("sh")
            .arg(&manager.paths.supervisor)
            .env("ZTE_OTA_ROOT", &manager.paths.root)
            .env("ZTE_OTA_LEGACY_LINK", &manager.paths.legacy_link)
            .env("ZTE_TEST_MARKER", &marker)
            .status()
            .unwrap();

        assert!(status.success());
        assert_eq!(fs::read_to_string(marker).unwrap(), "b");
        assert_eq!(manager.paths.active_slot(), Some(Slot::B));
    }

    #[test]
    fn supervisor_hash_checks_fresh_bootstrap_copies() {
        let manager = manager("supervisor-bootstrap");
        ensure_supervisor(&manager.paths).unwrap();
        let marker = manager.paths.root.join("started");
        let original = test_agent("bootstrap", true, "a");
        let mut corrupted = original.clone();
        *corrupted.last_mut().unwrap() = b'y';
        fs::write(manager.paths.slot_binary(Slot::A), &corrupted).unwrap();
        fs::set_permissions(
            manager.paths.slot_binary(Slot::A),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(
            manager.paths.slot_bootstrap_digest(Slot::A),
            format!("{}\nbootstrap\n", sha256(&original)),
        )
        .unwrap();
        let fallback = test_agent("bootstrap", true, "b");
        fs::write(manager.paths.slot_binary(Slot::B), &fallback).unwrap();
        fs::set_permissions(
            manager.paths.slot_binary(Slot::B),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(
            manager.paths.slot_bootstrap_digest(Slot::B),
            format!("{}\nbootstrap\n", sha256(&fallback)),
        )
        .unwrap();
        manager.paths.write_active(Slot::A).unwrap();

        let status = Command::new("sh")
            .arg(&manager.paths.supervisor)
            .env("ZTE_OTA_ROOT", &manager.paths.root)
            .env("ZTE_OTA_LEGACY_LINK", &manager.paths.legacy_link)
            .env("ZTE_TEST_MARKER", &marker)
            .status()
            .unwrap();

        assert!(status.success());
        assert_eq!(fs::read_to_string(marker).unwrap(), "b");
        assert_eq!(manager.paths.active_slot(), Some(Slot::B));
    }

    #[test]
    fn pending_candidate_is_never_selected_during_boot_recovery() {
        let manager = manager("supervisor-pending");
        ensure_supervisor(&manager.paths).unwrap();
        let marker = manager.paths.root.join("started");
        let candidate_bytes = test_agent("2.4.0-beta.9", true, "candidate");
        let candidate = release_for_binary(Channel::Beta, "2.4.0-beta.9", &candidate_bytes);
        write_release_agent(&manager.paths, Slot::A, &candidate, &candidate_bytes);
        let previous_bytes = test_agent("2.4.0-beta.8", false, "previous");
        let previous = release_for_binary(Channel::Beta, "2.4.0-beta.8", &previous_bytes);
        write_release_agent(&manager.paths, Slot::B, &previous, &previous_bytes);
        let stable_bytes = test_agent("2.3.0", true, "stable");
        let stable = release_for_binary(Channel::Stable, "2.3.0", &stable_bytes);
        fs::write(manager.paths.stable_binary(), &stable_bytes).unwrap();
        fs::set_permissions(
            manager.paths.stable_binary(),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        storage::write_json(&manager.paths.stable_meta(), &stable).unwrap();
        manager.paths.write_active(Slot::B).unwrap();
        manager
            .paths
            .write_pending(Slot::A, Slot::B, "2.4.0-beta.9")
            .unwrap();

        let status = Command::new("sh")
            .arg(&manager.paths.supervisor)
            .env("ZTE_OTA_ROOT", &manager.paths.root)
            .env("ZTE_OTA_LEGACY_LINK", &manager.paths.legacy_link)
            .env("ZTE_TEST_MARKER", &marker)
            .status()
            .unwrap();

        assert!(status.success());
        assert_eq!(fs::read_to_string(marker).unwrap(), "stable");
        assert!(!manager.paths.pending.exists());
        assert_eq!(manager.paths.active_slot(), None);
    }

    #[test]
    fn supervisor_never_boots_a_quarantined_candidate() {
        let manager = manager("supervisor-quarantine");
        ensure_supervisor(&manager.paths).unwrap();
        let marker = manager.paths.root.join("started");
        let broken_bytes = test_agent("2.4.0-beta.7", false, "broken");
        let broken = release_for_binary(Channel::Beta, "2.4.0-beta.7", &broken_bytes);
        write_release_agent(&manager.paths, Slot::A, &broken, &broken_bytes);
        let blocked_bytes = test_agent("2.4.0-beta.8", true, "blocked");
        let blocked = release_for_binary(Channel::Beta, "2.4.0-beta.8", &blocked_bytes);
        write_release_agent(&manager.paths, Slot::B, &blocked, &blocked_bytes);
        storage::write_json(&manager.paths.blocked, &vec![blocked.version.clone()]).unwrap();
        let stable_bytes = test_agent("2.3.0", true, "stable");
        let stable = release_for_binary(Channel::Stable, "2.3.0", &stable_bytes);
        fs::write(manager.paths.stable_binary(), &stable_bytes).unwrap();
        fs::set_permissions(
            manager.paths.stable_binary(),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        storage::write_json(&manager.paths.stable_meta(), &stable).unwrap();
        manager.paths.write_active(Slot::A).unwrap();

        let status = Command::new("sh")
            .arg(&manager.paths.supervisor)
            .env("ZTE_OTA_ROOT", &manager.paths.root)
            .env("ZTE_OTA_LEGACY_LINK", &manager.paths.legacy_link)
            .env("ZTE_TEST_MARKER", &marker)
            .status()
            .unwrap();

        assert!(status.success());
        assert_eq!(fs::read_to_string(marker).unwrap(), "stable");
        assert_eq!(manager.paths.active_slot(), None);
    }

    #[test]
    fn failed_candidate_recovery_uses_only_a_verified_stable_rescue() {
        let manager = manager("helper-stable-rescue");
        let previous_bytes = test_agent(crate::build_info::VERSION, false, "previous");
        let previous =
            release_for_binary(Channel::Stable, crate::build_info::VERSION, &previous_bytes);
        write_release_agent(&manager.paths, Slot::B, &previous, &previous_bytes);
        let stable_bytes = test_agent("2.3.0", true, "stable");
        let stable = release_for_binary(Channel::Stable, "2.3.0", &stable_bytes);
        fs::write(manager.paths.stable_binary(), &stable_bytes).unwrap();
        fs::set_permissions(
            manager.paths.stable_binary(),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        storage::write_json(&manager.paths.stable_meta(), &stable).unwrap();

        assert_eq!(recover(&manager.paths, Slot::B, None), 6);
        assert_eq!(manager.paths.active_slot(), None);
        assert_eq!(
            fs::read_link(&manager.paths.legacy_link).unwrap(),
            manager.paths.stable_binary()
        );
    }
}
