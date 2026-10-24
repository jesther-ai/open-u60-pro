//! The local name in the router's DNS: one line `<LAN address> <name>` in
//! a file of dnsmasq's `addn-hosts` directory (`/tmp/hosts`, read again on
//! SIGHUP; `/tmp` is tmpfs, so the agent writes it again at every start).
//! Only a change is written, and only a change signals dnsmasq.

use std::io::{ErrorKind, Write};
use std::net::Ipv4Addr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// Sends SIGHUP to a process; whether it was delivered.
pub type Signal = Box<dyn Fn(i32) -> bool + Send + Sync>;

pub struct Hosts {
    /// `/tmp/hosts/zte-agent`.
    pub file: PathBuf,
    /// The LAN instance's pid file (`/var/run/dnsmasq/dnsmasq.lan_dns.pid`).
    pub pid_file: PathBuf,
    /// `/proc`: a pid is signalled only if its `comm` is `dnsmasq`.
    pub proc: PathBuf,
    pub signal: Signal,
}

impl Hosts {
    pub fn router() -> Hosts {
        Hosts {
            file: PathBuf::from("/tmp/hosts/zte-agent"),
            pid_file: PathBuf::from("/var/run/dnsmasq/dnsmasq.lan_dns.pid"),
            proc: PathBuf::from("/proc"),
            signal: Box::new(|pid| {
                // SAFETY: kill(2) with a pid read from /proc and checked to be dnsmasq.
                unsafe { libc::kill(pid, libc::SIGHUP) == 0 }
            }),
        }
    }

    /// `<ip> <name>` in the file; dnsmasq reloads if that changed it.
    /// Whether it changed.
    pub fn set(&self, ip: Ipv4Addr, name: &str) -> std::io::Result<bool> {
        let line = format!("{ip} {name}\n");
        if std::fs::read_to_string(&self.file).is_ok_and(|cur| cur == line) {
            return Ok(false);
        }
        let dir = self.file.parent().unwrap_or(std::path::Path::new("/"));
        std::fs::create_dir_all(dir)?;
        let tmp = self.tmp();
        // A new file (`O_EXCL`: a link planted at that name in the
        // world-writable /tmp is removed, never followed), 0644.
        let _ = std::fs::remove_file(&tmp);
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp)
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .and_then(|()| std::fs::rename(&tmp, &self.file));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written?;
        self.reload();
        Ok(true)
    }

    /// The temporary file: next to the directory (same tmpfs, so the
    /// rename is atomic), never inside it: dnsmasq reads every file there.
    fn tmp(&self) -> PathBuf {
        let dir = self.file.parent().unwrap_or(std::path::Path::new("/"));
        dir.with_file_name(".zte-agent-hosts.tmp")
    }

    /// The file removed (dnsmasq reloads if there was one). Whether it was there.
    pub fn remove(&self) -> std::io::Result<bool> {
        match std::fs::remove_file(&self.file) {
            Ok(()) => {
                self.reload();
                Ok(true)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn is_dnsmasq(&self, pid: i32) -> bool {
        std::fs::read_to_string(self.proc.join(pid.to_string()).join("comm")).is_ok_and(|c| c.trim_end() == "dnsmasq")
    }

    /// SIGHUP to the LAN dnsmasq (its pid file), else to every dnsmasq;
    /// the pids signalled.
    pub fn reload(&self) -> Vec<i32> {
        let from_file = std::fs::read_to_string(&self.pid_file).ok().and_then(|s| s.trim().parse::<i32>().ok());
        let pids: Vec<i32> = match from_file.filter(|&p| p > 1 && self.is_dnsmasq(p)) {
            Some(p) => vec![p],
            None => std::fs::read_dir(&self.proc)
                .map(|d| {
                    d.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
                        .filter(|&p| p > 1 && self.is_dnsmasq(p))
                        .collect()
                })
                .unwrap_or_default(),
        };
        let sent: Vec<i32> = pids.into_iter().filter(|&p| (self.signal)(p)).collect();
        if sent.is_empty() {
            eprintln!("[tls] dnsmasq not found: the local name works after its next restart");
        }
        sent
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub fn temp(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("zte-tls-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake /proc with a dnsmasq (pid 123) and a shell (pid 456), and a
    /// hosts file in `root/hosts`; the signals sent are recorded.
    pub fn fake(root: &std::path::Path) -> (Hosts, Arc<Mutex<Vec<i32>>>) {
        for (pid, comm) in [(123, "dnsmasq"), (456, "sh")] {
            let d = root.join("proc").join(pid.to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
        }
        let sent = Arc::new(Mutex::new(Vec::new()));
        let s = Arc::clone(&sent);
        let hosts = Hosts {
            file: root.join("hosts").join("zte-agent"),
            pid_file: root.join("dnsmasq.lan_dns.pid"),
            proc: root.join("proc"),
            signal: Box::new(move |pid| {
                s.lock().unwrap().push(pid);
                true
            }),
        };
        (hosts, sent)
    }

    #[test]
    fn writes_only_changes_and_signals_dnsmasq() {
        let root = temp("hosts");
        let (h, sent) = fake(&root);
        std::fs::write(&h.pid_file, "123\n").unwrap();
        let ip = Ipv4Addr::new(192, 168, 0, 1);
        assert!(h.set(ip, "u60.home.arpa").unwrap());
        assert_eq!(std::fs::read_to_string(&h.file).unwrap(), "192.168.0.1 u60.home.arpa\n");
        assert_eq!(*sent.lock().unwrap(), [123]);
        // Same content: no write, no signal.
        assert!(!h.set(ip, "u60.home.arpa").unwrap());
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert!(h.set(Ipv4Addr::new(192, 168, 8, 1), "u60.home.arpa").unwrap());
        assert_eq!(std::fs::read_to_string(&h.file).unwrap(), "192.168.8.1 u60.home.arpa\n");
        assert!(h.remove().unwrap());
        assert!(!h.file.exists());
        assert!(!h.remove().unwrap());
        assert_eq!(*sent.lock().unwrap(), [123, 123, 123]);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The temporary file is outside the hosts directory (dnsmasq reads
    /// every file in it), and a link planted at its name is replaced, never
    /// followed (the agent runs as root; /tmp is world-writable).
    #[test]
    fn the_temporary_file_is_new_and_outside_the_directory() {
        let root = temp("tmp");
        let (h, _) = fake(&root);
        assert_ne!(h.tmp().parent(), h.file.parent());
        assert_eq!(h.tmp().parent(), h.file.parent().and_then(|d| d.parent()));
        let victim = root.join("victim.txt");
        std::fs::write(&victim, "keep me\n").unwrap();
        std::fs::create_dir_all(h.file.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, h.tmp()).unwrap();
        assert!(h.set(Ipv4Addr::new(192, 168, 0, 1), "u60.home.arpa").unwrap());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me\n");
        let meta = std::fs::symlink_metadata(&h.file).unwrap();
        assert!(meta.file_type().is_file(), "the hosts file is a link");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(meta.permissions().mode() & 0o777, 0o644);
        assert_eq!(std::fs::read_to_string(&h.file).unwrap(), "192.168.0.1 u60.home.arpa\n");
        assert!(!h.tmp().exists() && std::fs::symlink_metadata(h.tmp()).is_err());
        // Only the agent's file is in the directory.
        let names: Vec<_> = std::fs::read_dir(h.file.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["zte-agent"]);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A stale pid file (another process) or none: every dnsmasq in /proc,
    /// never anything else.
    #[test]
    fn signals_only_dnsmasq() {
        let root = temp("pid");
        let (h, sent) = fake(&root);
        std::fs::write(&h.pid_file, "456").unwrap();
        assert_eq!(h.reload(), [123]);
        std::fs::remove_file(&h.pid_file).unwrap();
        assert_eq!(h.reload(), [123]);
        std::fs::remove_dir_all(root.join("proc").join("123")).unwrap();
        assert!(h.reload().is_empty());
        assert_eq!(*sent.lock().unwrap(), [123, 123]);
        let _ = std::fs::remove_dir_all(root);
    }
}
