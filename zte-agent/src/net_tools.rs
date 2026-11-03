//! Private device effects shared by the optional network modules. No shell,
//! bounded commands, private atomic files and an on-disk rollback journal.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub type NetworkLock = Arc<Mutex<()>>;

pub fn local_addresses() -> Result<Vec<IpAddr>, String> {
    let mut addresses: Vec<_> = crate::httpd::mobile::interfaces()
        .map_err(|_| "Cannot read local addresses for network exclusions")?
        .into_iter()
        .map(|i| i.addr)
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    Ok(addresses)
}

// B31 has no xt_addrtype. Exact address exemptions use basic iptables matches,
// and callers include newly owned addresses before installing interception.
pub fn local_exclusions(
    tool: &str,
    table: &str,
    chain: &str,
    target: &str,
    addresses: &[IpAddr],
) -> Vec<Step> {
    addresses
        .iter()
        .filter(|a| a.is_ipv4() == (tool == "iptables"))
        .map(|a| {
            rule(
                tool,
                table,
                chain,
                &["-d", &a.to_string(), "-j", target],
                false,
            )
        })
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
}

impl Cmd {
    pub fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|s| (*s).into()).collect(),
        }
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args).stdin(Stdio::null());
        c
    }

    pub fn run(&self) -> Result<String, String> {
        if self.program == "@u60-relay-channel" {
            return crate::wifi_relay::hotspot::switch(&self.args).map(|_| String::new());
        }
        if self.program == "@u60-relay-ipv6-dad" {
            return crate::wifi_relay::ipv6::wait_addresses(&self.args).map(|_| String::new());
        }
        if self.program == "@u60-relay-ra" {
            return crate::wifi_relay::ipv6::ra_command(&self.args).map(|_| String::new());
        }
        if self.program == "@u60-relay-dns" {
            return crate::wifi_relay::dns::command(&self.args).map(|_| String::new());
        }
        if self.program == "@u60-relay-portal" {
            return crate::wifi_relay::portal::command(&self.args).map(|_| String::new());
        }
        if self.program == "@u60-relay-roam" {
            return match self.args.as_slice() {
                [mode] if matches!(mode.as_str(), "0" | "1") => {
                    crate::wifi_relay::restore_roam(mode).map(|_| String::new())
                }
                _ => Err("Invalid relay recovery mode".into()),
            };
        }
        if self.program == "@u60-relay-detach" {
            return if self.args.is_empty() {
                crate::wifi_relay::detach_station().map(|_| String::new())
            } else {
                Err("Invalid relay disconnect recovery".into())
            };
        }
        let out = run(self.command(), Duration::from_secs(8))?;
        if self.program == "hostapd_cli"
            && self
                .args
                .iter()
                .any(|a| matches!(a.as_str(), "set" | "disable" | "enable"))
            && out.lines().last() != Some("OK")
        {
            return Err("Hostapd refused the runtime change".into());
        }
        if self.program == "wpa_cli"
            && self
                .args
                .iter()
                .any(|a| matches!(a.as_str(), "SETROAMMODE" | "reconfigure"))
            && out.lines().last() != Some("OK")
        {
            return Err("Wi-Fi control refused the runtime change".into());
        }
        Ok(out)
    }

    fn undo(&self) -> Result<(), String> {
        if crate::wifi_relay::hotspot::skip_undo(self) {
            return Ok(());
        }
        let error = match self.run() {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };
        // OFF during a channel switch/CAC makes that runtime snapshot obsolete.
        if crate::wifi_relay::hotspot::skip_undo(self) {
            return Ok(());
        }
        // A crash may have occurred after journaling but before applying a
        // step. Already-absent objects are successful, idempotent recovery.
        if matches!(self.program.as_str(), "iptables" | "ip6tables") {
            let table = self
                .args
                .windows(2)
                .find(|a| a[0] == "-t")
                .map(|a| a[1].as_str())
                .unwrap_or("filter");
            // If the tool/table itself is unavailable, that is NOT evidence
            // that our rule is absent. Preserve the journal for recovery.
            Cmd::new(&self.program, &["-w", "2", "-t", table, "-S"]).run()?;
            if let Some(i) = self.args.iter().position(|a| a == "-D") {
                let mut check = self.clone();
                check.args[i] = "-C".into();
                if check.run().is_err() {
                    return Ok(());
                }
            }
            if let Some(i) = self.args.iter().position(|a| a == "-X") {
                let mut check = self.clone();
                check.args[i] = "-S".into();
                if check.run().is_err() {
                    return Ok(());
                }
            }
        }
        if self.program == "iw" && !Path::new("/sys/class/net/u60sta0").exists() {
            return Ok(());
        }
        if self.program == "ip" && self.args.get(2).map(String::as_str) == Some("del") {
            let family = self.args[0].as_str();
            let value = |name| {
                self.args
                    .windows(2)
                    .find(|a| a[0] == name)
                    .map(|a| a[1].as_str())
            };
            match self.args[1].as_str() {
                "rule" => {
                    let rules = Cmd::new("ip", &[family, "rule", "show"]).run()?;
                    if value("pref").is_some_and(|p| {
                        !rules
                            .lines()
                            .any(|l| l.trim_start().starts_with(&format!("{p}:")))
                    }) {
                        return Ok(());
                    }
                    if let (Some(oif), Some(table)) = (value("oif"), value("lookup")) {
                        if !has_oif_rule(&rules, oif, table) {
                            return Ok(());
                        }
                    }
                }
                "route" => {
                    if let Some(table) = value("table") {
                        let kind = self.args[3].as_str();
                        let (kind, destination) = if matches!(kind, "throw" | "local") {
                            (kind, self.args[4].as_str())
                        } else {
                            ("unicast", kind)
                        };
                        if Cmd::new(
                            "ip",
                            &[
                                family,
                                "route",
                                "show",
                                "table",
                                table,
                                "type",
                                kind,
                                "exact",
                                destination,
                            ],
                        )
                        .run()
                        .is_ok_and(|s| s.is_empty())
                        {
                            return Ok(());
                        }
                        let all =
                            Cmd::new("ip", &[family, "route", "show", "table", "all"]).run()?;
                        if !all.lines().any(|l| {
                            l.split_whitespace()
                                .collect::<Vec<_>>()
                                .windows(2)
                                .any(|w| w == ["table", table])
                        }) {
                            return Ok(());
                        }
                    }
                }
                "addr" => {
                    if let Some(iface) = value("dev") {
                        if !Path::new(&format!("/sys/class/net/{iface}")).exists() {
                            return Ok(());
                        }
                        let address = &self.args[3];
                        let all =
                            Cmd::new("ip", &["-o", family, "addr", "show", "dev", iface]).run()?;
                        if !all.split_whitespace().any(|s| s == address) {
                            return Ok(());
                        }
                    }
                }
                _ => {}
            }
        }
        Err(if self.program.starts_with("@u60-relay-") {
            error
        } else {
            "Cannot restore an owned network change".into()
        })
    }
}

fn has_oif_rule(rules: &str, oif: &str, table: &str) -> bool {
    rules.lines().any(|line| {
        let words = line.split_whitespace().collect::<Vec<_>>();
        words.windows(2).any(|w| w == ["oif", oif])
            && words.windows(2).any(|w| w == ["lookup", table])
    })
}

pub fn run(mut command: Command, timeout: Duration) -> Result<String, String> {
    crate::httpd::gate::debug_assert_may_block();
    use std::os::unix::process::CommandExt;
    // Kill the command's whole process group, including descendants that
    // would otherwise keep its output pipes open beyond the deadline.
    command.process_group(0);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot run device tool: {e}"))?;
    // Drain both pipes while waiting; a verbose tool must not deadlock on a full pipe.
    let read = |pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let r = pipe.take(256 * 1024 + 1).read_to_end(&mut bytes);
            (r, bytes)
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()));
    let stderr = read(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + timeout;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                break Err("Device tool timed out".to_owned());
            }
        }
    };
    // A tool can exit successfully while leaving a background child holding
    // stdout/stderr. These tools are synchronous; no descendants are owned.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let (_, out) = stdout.join().map_err(|_| "Device tool reader failed")?;
    let (_, err) = stderr.join().map_err(|_| "Device tool reader failed")?;
    let status = result?;
    if out.len() > 256 * 1024 || err.len() > 256 * 1024 {
        return Err("Device tool output exceeded its limit".into());
    }
    if !status.success() {
        // Tool output may contain credentials or nearby SSIDs. Do not expose it.
        return Err(format!("Device tool failed ({status})"));
    }
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

pub fn private_dir(path: &Path) -> Result<(), String> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| e.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

pub fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid private path")?;
    private_dir(parent)?;
    let tmp = path.with_extension("new");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    file.write_all(data)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(tmp, path).map_err(|e| e.to_string())
}

/// The child cannot outlive the agent on Linux, unless its exec changes the
/// SELinux domain: the kernel then clears the parent-death signal (B31:
/// wpa_supplicant, odhcp6c), and its owner must stop such leftovers when it
/// recovers (`wifi_relay::stop_strays`). The supervisor additionally reaps
/// exits and rolls back its network rules while the agent is running.
pub fn owned_child(mut cmd: Command) -> Result<Child, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let parent = unsafe { libc::getpid() };
        unsafe {
            cmd.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != parent
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    // Linux's parent-death signal follows the spawning THREAD. A tokio
    // blocking worker may retire while the agent is still alive, so every
    // long-lived child must be spawned by this one persistent thread.
    type Spawn = (Command, mpsc::SyncSender<Result<Child, String>>);
    static SPAWNER: OnceLock<mpsc::Sender<Spawn>> = OnceLock::new();
    let sender = SPAWNER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Spawn>();
        std::thread::Builder::new()
            .name("net-children".into())
            .spawn(move || {
                while let Ok((mut cmd, reply)) = rx.recv() {
                    let result = cmd
                        .spawn()
                        .map_err(|e| format!("Cannot start network process: {e}"));
                    if let Err(mpsc::SendError(Ok(mut child))) = reply.send(result) {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
            })
            .expect("network child spawner");
        tx
    });
    let (tx, rx) = mpsc::sync_channel(1);
    sender
        .send((cmd, tx))
        .map_err(|_| "Network child spawner stopped")?;
    rx.recv().map_err(|_| "Network child spawner stopped")?
}

pub fn stop_child(child: &mut Option<Child>) {
    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

pub fn key<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Step {
    pub apply: Cmd,
    pub undo: Cmd,
}

impl Step {
    pub fn new(apply: Cmd, undo: Cmd) -> Self {
        Self { apply, undo }
    }
}

/// The caller first reserves empty chain names, route tables and rule priorities.
/// Save each undo BEFORE applying it, so interruption is recoverable at startup.
pub struct Transaction {
    path: PathBuf,
    undo: Vec<Cmd>,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    boot: String,
    undo: Vec<Cmd>,
}

fn boot_id() -> String {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .unwrap_or_default()
        .trim()
        .to_owned()
}

impl Transaction {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            undo: Vec::new(),
        }
    }

    pub fn recover(path: &Path) -> Result<(), String> {
        if !path.exists() {
            return Ok(());
        }
        let journal: Journal = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|_| "Network recovery journal is invalid")?;
        // All owned changes are runtime-only. Never replay an old boot's
        // hostapd snapshot over a newly configured/started vendor hotspot.
        if !journal.boot.is_empty() && journal.boot != boot_id() {
            return fs::remove_file(path).map_err(|e| e.to_string());
        }
        Self {
            path: path.into(),
            undo: journal.undo,
        }
        .rollback()
    }

    fn save(&self) -> Result<(), String> {
        write_private(
            &self.path,
            &serde_json::to_vec(&Journal {
                boot: boot_id(),
                undo: self.undo.clone(),
            })
            .map_err(|e| e.to_string())?,
        )
    }

    pub fn apply(&mut self, step: Step) -> Result<(), String> {
        self.undo.push(step.undo);
        self.save()?;
        step.apply.run().map(|_| ())
    }

    pub fn rollback(&mut self) -> Result<(), String> {
        while let Some(c) = self.undo.last() {
            if let Err(error) = c.undo() {
                self.save()?;
                return Err(format!("Network cleanup paused: {error}. Retry cleanup in the panel; if it persists, use network-recover over USB ADB"));
            }
            self.undo.pop();
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

pub fn chain(tool: &str, table: &str, name: &str) -> Vec<Step> {
    vec![Step::new(
        Cmd::new(tool, &["-w", "2", "-t", table, "-N", name]),
        Cmd::new(tool, &["-w", "2", "-t", table, "-X", name]),
    )]
}

pub fn rule(tool: &str, table: &str, name: &str, args: &[&str], first: bool) -> Step {
    let mut a = vec![
        "-w",
        "2",
        "-t",
        table,
        if first { "-I" } else { "-A" },
        name,
    ];
    if first {
        a.push("1");
    }
    a.extend_from_slice(args);
    let mut d = vec!["-w", "2", "-t", table, "-D", name];
    d.extend_from_slice(args);
    Step::new(Cmd::new(tool, &a), Cmd::new(tool, &d))
}

pub fn ip_step(family: &str, what: &str, args: &[&str]) -> Step {
    let mut a = vec![family, what, "add"];
    a.extend_from_slice(args);
    let mut d = vec![family, what, "del"];
    d.extend_from_slice(args);
    Step::new(Cmd::new("ip", &a), Cmd::new("ip", &d))
}

pub fn reserve(
    tool: &str,
    table: &str,
    names: &[&str],
    family: &str,
    route_table: &str,
    pref: &str,
) -> Result<(), String> {
    for name in names {
        if Cmd::new(tool, &["-w", "2", "-t", table, "-S", name])
            .run()
            .is_ok()
        {
            return Err(
                "A reserved network chain already exists; recover it before starting".into(),
            );
        }
    }
    // An empty table may return ENOENT. Verify against all tables rather
    // than treating every command failure as permission to take it over.
    let occupied = match Cmd::new("ip", &[family, "route", "show", "table", route_table]).run() {
        Ok(routes) => !routes.trim().is_empty(),
        Err(_) => Cmd::new("ip", &[family, "route", "show", "table", "all"])
            .run()?
            .lines()
            .any(|line| {
                line.split_whitespace()
                    .collect::<Vec<_>>()
                    .windows(2)
                    .any(|w| w == ["table", route_table])
            }),
    };
    if occupied {
        return Err("A reserved network route table is already in use".into());
    }
    let rules = Cmd::new("ip", &[family, "rule", "show"]).run()?;
    if rules
        .lines()
        .any(|l| l.trim_start().starts_with(&format!("{pref}:")))
    {
        return Err("A reserved network rule priority is already in use".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_tun_recovery_matches_only_its_exact_rule_including_detached_interfaces() {
        let rules = "32765: from all oif u60mh0 [detached] lookup 60630\n32764: from all oif other lookup 60630\n";
        assert!(has_oif_rule(rules, "u60mh0", "60630"));
        assert!(!has_oif_rule(rules, "u60mh0", "6063"));
        assert!(!has_oif_rule(
            "32765: from all oif other lookup 60630",
            "u60mh0",
            "60630"
        ));
        assert!(!has_oif_rule(
            "32765: from all oif u60mh0 lookup 2022",
            "u60mh0",
            "60630"
        ));
    }

    #[test]
    fn generated_undo_matches_only_the_owned_rule() {
        let s = rule(
            "iptables",
            "mangle",
            "PREROUTING",
            &["-i", "br-lan", "-j", "U60_MH"],
            true,
        );
        assert_eq!(
            s.apply.args,
            [
                "-w",
                "2",
                "-t",
                "mangle",
                "-I",
                "PREROUTING",
                "1",
                "-i",
                "br-lan",
                "-j",
                "U60_MH"
            ]
        );
        assert_eq!(
            s.undo.args,
            [
                "-w",
                "2",
                "-t",
                "mangle",
                "-D",
                "PREROUTING",
                "-i",
                "br-lan",
                "-j",
                "U60_MH"
            ]
        );
    }

    #[test]
    fn runtime_file_is_private_and_atomic() {
        let dir = std::env::temp_dir().join(format!("u60-private-{}", std::process::id()));
        let path = dir.join("config.json");
        write_private(&path, b"secret").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::read(&path).unwrap(), b"secret");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commands_are_not_shell_interpreted() {
        let mut c = Command::new("printf");
        c.args(["%s", "$(touch /no-such-file); ' &"]);
        assert_eq!(
            run(c, Duration::from_secs(1)).unwrap(),
            "$(touch /no-such-file); ' &"
        );
    }

    #[test]
    fn failed_effect_is_journaled_and_recovers_after_restart() {
        let dir = std::env::temp_dir().join(format!("u60-failed-effect-{}", std::process::id()));
        private_dir(&dir).unwrap();
        let journal = dir.join("journal.json");
        let file = dir.join("effect");
        let name = file.to_str().unwrap();
        let mut tx = Transaction::new(journal.clone());
        tx.apply(Step::new(
            Cmd::new("touch", &[name]),
            Cmd::new("rm", &["-f", name]),
        ))
        .unwrap();
        assert!(tx
            .apply(Step::new(
                Cmd::new("false", &[]),
                Cmd::new("rm", &["-f", name])
            ))
            .is_err());
        assert_eq!(
            serde_json::from_slice::<Journal>(&fs::read(&journal).unwrap())
                .unwrap()
                .undo
                .len(),
            2
        );
        drop(tx);
        Transaction::recover(&journal).unwrap();
        Transaction::recover(&journal).unwrap();
        assert!(!file.exists() && !journal.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rollback_stops_on_failure_and_keeps_remaining_recovery() {
        let dir = std::env::temp_dir().join(format!("u60-blocked-undo-{}", std::process::id()));
        private_dir(&dir).unwrap();
        let journal = dir.join("journal.json");
        let file = dir.join("effect");
        let blocker = dir.join("blocker");
        let mut tx = Transaction::new(journal.clone());
        tx.apply(Step::new(
            Cmd::new("touch", &[file.to_str().unwrap()]),
            Cmd::new("rm", &["-f", file.to_str().unwrap()]),
        ))
        .unwrap();
        tx.apply(Step::new(
            Cmd::new("mkdir", &[blocker.to_str().unwrap()]),
            Cmd::new("rmdir", &[blocker.to_str().unwrap()]),
        ))
        .unwrap();
        fs::write(blocker.join("busy"), b"not empty").unwrap();
        assert!(tx.rollback().is_err());
        assert!(file.exists() && journal.exists());
        drop(tx);
        fs::remove_file(blocker.join("busy")).unwrap();
        Transaction::recover(&journal).unwrap();
        assert!(!file.exists() && !blocker.exists() && !journal.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn previous_boot_runtime_journal_does_not_replay() {
        let dir = std::env::temp_dir().join(format!("u60-old-boot-{}", std::process::id()));
        let journal = dir.join("journal.json");
        let j = Journal {
            boot: "previous-boot".into(),
            undo: vec![Cmd::new("false", &[])],
        };
        write_private(&journal, &serde_json::to_vec(&j).unwrap()).unwrap();
        Transaction::recover(&journal).unwrap();
        assert!(!journal.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn short_lived_command_timeout_is_bounded() {
        let mut command = Command::new("sleep");
        command.arg("10");
        let before = Instant::now();
        assert!(run(command, Duration::from_millis(80)).is_err());
        assert!(before.elapsed() < Duration::from_secs(2));
    }
}
