//! Crash-safe files used by both the running agent and the update helper.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub const fn as_str(self) -> &'static str {
        match self {
            Slot::A => "a",
            Slot::B => "b",
        }
    }

    pub const fn other(self) -> Self {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "a" => Some(Slot::A),
            "b" => Some(Slot::B),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
    pub settings: PathBuf,
    pub state: PathBuf,
    pub active: PathBuf,
    pub pending: PathBuf,
    pub blocked: PathBuf,
    pub update_log: PathBuf,
    pub supervisor: PathBuf,
    pub legacy_link: PathBuf,
}

impl Paths {
    pub fn system() -> Self {
        Self::new(
            std::env::var_os("ZTE_OTA_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/data/open-u60/update")),
            std::env::var_os("ZTE_OTA_SUPERVISOR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/data/local/tmp/zte-agent-supervisor.sh")),
            std::env::var_os("ZTE_OTA_LEGACY_LINK")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/data/zte-agent")),
        )
    }

    pub fn new(root: PathBuf, supervisor: PathBuf, legacy_link: PathBuf) -> Self {
        Self {
            settings: root.join("settings.json"),
            state: root.join("state.json"),
            active: root.join("active"),
            pending: root.join("pending"),
            blocked: root.join("blocked.json"),
            update_log: root.join("update.log"),
            root,
            supervisor,
            legacy_link,
        }
    }

    pub fn slot_dir(&self, slot: Slot) -> PathBuf {
        self.root.join(format!("slot-{}", slot.as_str()))
    }

    pub fn slot_binary(&self, slot: Slot) -> PathBuf {
        self.slot_dir(slot).join("zte-agent")
    }

    pub fn slot_download(&self, slot: Slot) -> PathBuf {
        self.slot_dir(slot).join("zte-agent.download")
    }

    pub fn slot_meta(&self, slot: Slot) -> PathBuf {
        self.slot_dir(slot).join("release.json")
    }

    pub fn stable_dir(&self) -> PathBuf {
        self.root.join("last-stable")
    }

    pub fn stable_binary(&self) -> PathBuf {
        self.stable_dir().join("zte-agent")
    }

    pub fn stable_meta(&self) -> PathBuf {
        self.stable_dir().join("release.json")
    }

    pub fn prepare(&self) -> io::Result<()> {
        create_private_dir(&self.root)?;
        create_private_dir(&self.slot_dir(Slot::A))?;
        create_private_dir(&self.slot_dir(Slot::B))?;
        create_private_dir(&self.stable_dir())
    }

    pub fn active_slot(&self) -> Option<Slot> {
        fs::read_to_string(&self.active)
            .ok()
            .and_then(|value| Slot::parse(&value))
    }

    pub fn bootstrap_ready(&self) -> bool {
        self.active_slot()
            .is_some_and(|slot| executable(&self.slot_binary(slot)))
            && executable(&self.supervisor)
    }

    pub fn rollback_available(&self) -> bool {
        self.active_slot()
            .is_some_and(|slot| executable(&self.slot_binary(slot.other())))
    }

    pub fn write_active(&self, slot: Slot) -> io::Result<()> {
        atomic_write(
            &self.active,
            format!("{}\n", slot.as_str()).as_bytes(),
            0o600,
        )
    }

    pub fn write_pending(&self, candidate: Slot, previous: Slot, version: &str) -> io::Result<()> {
        if version.contains(['\n', '\r']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid version",
            ));
        }
        atomic_write(
            &self.pending,
            format!("{}\n{}\n{version}\n", candidate.as_str(), previous.as_str()).as_bytes(),
            0o600,
        )
    }

    pub fn clear_pending(&self) -> io::Result<()> {
        match fs::remove_file(&self.pending) {
            Ok(()) => sync_parent(&self.pending),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn point_legacy_link(&self, slot: Slot) -> io::Result<()> {
        let parent = self.legacy_link.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "legacy path has no parent")
        })?;
        let tmp = parent.join(".zte-agent.ota-link");
        let _ = fs::remove_file(&tmp);
        symlink(self.slot_binary(slot), &tmp)?;
        fs::rename(&tmp, &self.legacy_link)?;
        File::open(parent)?.sync_all()
    }

    pub fn append_log(&self, message: &str) {
        let clean = message.replace(['\r', '\n'], " ");
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.update_log)
        {
            let _ = writeln!(file, "{} {clean}", super::unix_now());
        }
    }
}

pub fn executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() > 256 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    atomic_write(path, &bytes, 0o600)
}

pub fn atomic_copy(source: &Path, destination: &Path, mode: u32) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    create_private_dir(parent)?;
    let tmp = temporary_path(destination);
    let _ = fs::remove_file(&tmp);
    fs::copy(source, &tmp)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, destination)?;
    File::open(parent)?.sync_all()
}

pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    create_private_dir(parent)?;
    let tmp = temporary_path(path);
    let _ = fs::remove_file(&tmp);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)?;
    File::open(parent)?.sync_all()
}

pub fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    File::open(parent)?.sync_all()
}

pub fn available_bytes(path: &Path) -> io::Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is NUL terminated and `stat` points to writable storage.
    if unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs returned success and initialized `stat`.
    let stat = unsafe { stat.assume_init() };
    Ok(u64::from(stat.f_bavail).saturating_mul(stat.f_frsize))
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(name: &str) -> Paths {
        let root =
            std::env::temp_dir().join(format!("u60-ota-storage-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Paths::new(
            root.clone(),
            root.join("supervisor.sh"),
            root.join("zte-agent"),
        )
    }

    #[test]
    fn active_slot_rejects_torn_or_unknown_markers() {
        let paths = paths("marker");
        paths.prepare().unwrap();
        fs::write(&paths.active, "outside\n").unwrap();
        assert_eq!(paths.active_slot(), None);
    }

    #[test]
    fn atomic_json_replaces_the_previous_value() {
        let paths = paths("json");
        paths.prepare().unwrap();
        write_json(&paths.state, &serde_json::json!({"version": 1})).unwrap();
        write_json(&paths.state, &serde_json::json!({"version": 2})).unwrap();
        assert_eq!(
            read_json::<serde_json::Value>(&paths.state).unwrap()["version"],
            2
        );
    }

    #[test]
    fn pending_marker_keeps_candidate_and_previous_on_separate_lines() {
        let paths = paths("pending");
        paths.prepare().unwrap();
        paths
            .write_pending(Slot::B, Slot::A, "2.5.0-beta.1")
            .unwrap();
        assert_eq!(
            fs::read_to_string(&paths.pending).unwrap(),
            "b\na\n2.5.0-beta.1\n"
        );
    }

    #[test]
    fn rollback_requires_an_executable_inactive_slot() {
        let paths = paths("rollback");
        paths.prepare().unwrap();
        paths.write_active(Slot::A).unwrap();
        assert!(!paths.rollback_available());
        fs::write(paths.slot_binary(Slot::B), b"binary").unwrap();
        fs::set_permissions(
            paths.slot_binary(Slot::B),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(paths.rollback_available());
    }
}
