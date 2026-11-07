//! Private device effects shared by the optional network modules. No shell,
//! bounded commands, private atomic files and an on-disk rollback journal.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub mod firewall;

#[derive(Debug, PartialEq, Eq)]
pub enum PolicyRule {
    Missing,
    Owned,
    Foreign,
    /// Several rules use the priority; none can be attributed to us.
    Shared,
}

/// Errors are inspection failures only (the rule table could not be read).
pub fn plain_policy_rule(family: &str, priority: &str, table: &str) -> Result<PolicyRule, String> {
    let text = Cmd::new("ip", &["-j", family, "rule", "show"]).run()?;
    policy_rule_state(&text, priority, table)
}

fn policy_rule_state(text: &str, priority: &str, table: &str) -> Result<PolicyRule, String> {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(text).map_err(|_| "Cannot inspect policy rules")?;
    let priority: u64 = priority.parse().map_err(|_| "Invalid policy priority")?;
    let matches: Vec<_> = rows
        .iter()
        .filter(|r| r["priority"].as_u64() == Some(priority))
        .collect();
    if matches.is_empty() {
        return Ok(PolicyRule::Missing);
    }
    if matches.len() != 1 {
        return Ok(PolicyRule::Shared);
    }
    let rule = matches[0];
    let known = [
        "priority", "src", "dst", "from", "to", "table", "protocol", "flags", "action",
    ];
    let plain = rule
        .as_object()
        .is_some_and(|m| m.keys().all(|k| known.contains(&k.as_str())))
        && ["src", "dst", "from", "to"].iter().all(|k| {
            rule[*k].is_null() || matches!(rule[*k].as_str(), Some("all" | "0.0.0.0/0" | "::/0"))
        })
        && (rule["flags"].is_null() || rule["flags"].as_array().is_some_and(Vec::is_empty))
        && (rule["action"].is_null() || rule["action"] == "to_tbl")
        && (rule["table"] == table || rule["table"].as_u64() == table.parse().ok());
    Ok(if plain {
        PolicyRule::Owned
    } else {
        PolicyRule::Foreign
    })
}

/// Whether any `family` route uses `table`. An emptied table may not exist at
/// all: `ip route show table N` then fails (ENOENT) instead of listing nothing.
pub fn route_table_listed(family: &str, table: &str) -> Result<bool, String> {
    let all = Cmd::new("ip", &[family, "route", "show", "table", "all"]).run()?;
    Ok(lists_table(&all, table))
}

fn lists_table(routes: &str, table: &str) -> bool {
    routes.lines().any(|line| {
        line.split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w == ["table", table])
    })
}

/// Route type and destination of an `ip <family> route <action> ...` command.
pub fn route_target(args: &[String]) -> (&str, &str) {
    let kind = args[3].as_str();
    if matches!(kind, "throw" | "local") {
        (kind, args[4].as_str())
    } else {
        ("unicast", kind)
    }
}

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

/// Exact-address exemptions are re-read from their chain at most this often.
pub const EXEMPTION_AUDIT: Duration = Duration::from_secs(15);

/// Bring one chain's exact-address exemptions in line with `current`, given a
/// single snapshot of that chain (`present`): missing ones (new, or deleted by
/// another writer) are inserted ahead of the rules they exempt, then the
/// `previous` ones that are no longer current are removed. Each rule keeps its
/// own journal entry, so a failure part-way is retried against a fresh
/// snapshot without duplicates. The caller filters both lists by family.
pub fn sync_exemptions(
    network: &mut Transaction,
    present: &HashSet<IpAddr>,
    current: &[IpAddr],
    previous: &[IpAddr],
    rule: impl Fn(&IpAddr) -> Step,
) -> Result<(), String> {
    for address in current.iter().filter(|a| !present.contains(a)) {
        network.ensure(rule(address))?;
    }
    for address in previous.iter().filter(|a| !current.contains(a)) {
        network.remove(&rule(address).undo)?;
    }
    Ok(())
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
        // Don't delete an externally replaced rule using a broad priority match.
        if self.program == "ip"
            && self.args.len() == 7
            && self.args[1..4] == ["rule", "del", "pref"]
            && self.args[5] == "lookup"
        {
            match plain_policy_rule(&self.args[0], &self.args[4], &self.args[6])? {
                PolicyRule::Owned => {}
                PolicyRule::Missing | PolicyRule::Foreign => return Ok(()),
                PolicyRule::Shared => {
                    return Err("Policy priority is shared; cleanup paused".into())
                }
            }
        }
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
                        let (kind, destination) = route_target(&self.args);
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
                        if !route_table_listed(family, table)? {
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

pub fn run(command: Command, timeout: Duration) -> Result<String, String> {
    run_with_input(command, timeout, None)
}

pub fn run_input(command: Command, timeout: Duration, input: Vec<u8>) -> Result<String, String> {
    run_with_input(command, timeout, Some(input))
}

fn run_with_input(
    mut command: Command,
    timeout: Duration,
    input: Option<Vec<u8>>,
) -> Result<String, String> {
    crate::httpd::gate::debug_assert_may_block();
    use std::os::unix::process::CommandExt;
    // Kill the command's whole process group, including descendants that
    // would otherwise keep its output pipes open beyond the deadline.
    command.process_group(0);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot run device tool: {e}"))?;
    let writer = input.map(|bytes| {
        let mut stdin = child.stdin.take().unwrap();
        std::thread::spawn(move || stdin.write_all(&bytes))
    });
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
    // These tools usually finish within a few milliseconds; a fixed 20 ms
    // poll made every one of the hundreds of firewall/route commands wait
    // for it. Back off from 1 ms so long-running tools still poll cheaply.
    let mut pause = Duration::from_millis(1);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(20));
            }
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
    let written = writer.map(|writer| {
        writer
            .join()
            .map_err(|_| "Device tool input failed")
            .and_then(|r| r.map_err(|_| "Device tool input failed"))
    });
    let status = result?;
    if out.len() > 256 * 1024 || err.len() > 256 * 1024 {
        return Err("Device tool output exceeded its limit".into());
    }
    if !status.success() {
        // Tool output may contain credentials or nearby SSIDs. Do not expose it.
        return Err(format!("Device tool failed ({status})"));
    }
    if let Some(written) = written {
        written?;
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

    /// A network plan with supported firewall runs batched into restore. Every
    /// undo is journaled with one durable write before the first effect (so
    /// before any table can COMMIT), and the applies then run in order. An
    /// interrupted plan therefore also rolls back steps that never ran: their
    /// undo must succeed for absent objects (see `Cmd::undo`) and must never
    /// match another writer's object, so a rule in a chain we do not own jumps
    /// to an owned `U60_*` chain or matches only our interfaces or comment
    /// (`foreign_identity`). Never retry a failed restore as individual
    /// commands: it may be partly applied. Radio and daemon changes use `apply`.
    pub fn apply_network(&mut self, steps: Vec<Step>) -> Result<(), String> {
        if steps.is_empty() {
            return Ok(());
        }
        self.undo.extend(steps.iter().map(|s| s.undo.clone()));
        self.save()?;
        let mut at = 0;
        while at < steps.len() {
            if let Some(batch) = firewall::Batch::collect(&steps[at..]) {
                let count = batch.count;
                batch.run()?;
                at += count;
            } else {
                steps[at].apply.run()?;
                at += 1;
            }
        }
        Ok(())
    }

    /// Reapply a missing independent effect without duplicating its undo.
    /// Save even on retries: a prior failed save may have changed only RAM.
    pub fn ensure(&mut self, step: Step) -> Result<(), String> {
        if !self.undo.contains(&step.undo) {
            self.undo.push(step.undo);
        }
        self.save()?;
        step.apply.run().map(|_| ())
    }

    /// Undo one journaled change now and forget it. Only for independent
    /// objects (an exact-address firewall exemption), never a dependency
    /// chain, because the remaining undos keep their order.
    pub fn remove(&mut self, undo: &Cmd) -> Result<(), String> {
        let Some(index) = self.undo.iter().position(|c| c == undo) else {
            return Ok(());
        };
        self.undo[index].undo()?;
        self.undo.remove(index);
        self.save()
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

/// Steps of a prejournaled plan whose undo could delete an identical rule of
/// another writer: a rule in a chain we do not own that neither jumps to an
/// owned chain nor matches one of our interfaces or our comment.
#[cfg(test)]
pub fn foreign_identity(plan: &[Step]) -> Vec<&Step> {
    plan.iter()
        .filter(|step| {
            let args = &step.undo.args;
            if !matches!(step.undo.program.as_str(), "iptables" | "ip6tables") {
                return false;
            }
            let Some(at) = args.iter().position(|a| a == "-D") else {
                return false;
            };
            if args
                .get(at + 1)
                .is_some_and(|chain| chain.starts_with("U60_"))
            {
                return false;
            }
            let values = |flag: &str| {
                args.windows(2)
                    .filter(|w| w[0] == flag)
                    .map(|w| w[1].clone())
                    .collect::<Vec<_>>()
            };
            let owned_target = values("-j").iter().any(|t| t.starts_with("U60_"));
            let owned_interface = values("-i")
                .iter()
                .chain(&values("-o"))
                .any(|name| name.starts_with("u60"));
            let owned_comment = values("--comment")
                .iter()
                .any(|c| c.starts_with("zte-agent"));
            !(owned_target || owned_interface || owned_comment)
        })
        .collect()
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
        Err(_) => route_table_listed(family, route_table)?,
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

    fn touch_step(path: &Path) -> Step {
        let name = path.to_str().unwrap();
        Step::new(Cmd::new("touch", &[name]), Cmd::new("rm", &["-f", name]))
    }

    #[test]
    fn batched_plan_is_fully_journaled_before_its_first_effect_and_recovers_after_failure() {
        let dir = std::env::temp_dir().join(format!("u60-batch-{}", std::process::id()));
        private_dir(&dir).unwrap();
        let journal = dir.join("journal.json");
        let files: Vec<_> = (1..=3).map(|n| dir.join(format!("effect-{n}"))).collect();
        let mut first = touch_step(&files[0]);
        // Runs as the first effect: the journal must already name the last undo.
        first.apply = Cmd::new(
            "sh",
            &[
                "-c",
                &format!(
                    "grep -q effect-3 '{}' && touch '{}'",
                    journal.display(),
                    files[0].display()
                ),
            ],
        );
        let broken = Step::new(
            Cmd::new("false", &[]),
            Cmd::new("rm", &["-f", files[1].to_str().unwrap()]),
        );
        let mut tx = Transaction::new(journal.clone());
        assert!(tx
            .apply_network(vec![first, broken, touch_step(&files[2])])
            .is_err());
        assert!(files[0].exists() && !files[1].exists() && !files[2].exists());
        assert_eq!(
            serde_json::from_slice::<Journal>(&fs::read(&journal).unwrap())
                .unwrap()
                .undo
                .len(),
            3,
            "never-run steps stay journaled; their undo is idempotent"
        );
        drop(tx);
        Transaction::recover(&journal).unwrap();
        assert!(!files[0].exists() && !journal.exists());
        let mut tx = Transaction::new(journal.clone());
        tx.apply_network(Vec::new()).unwrap();
        assert!(!journal.exists(), "an empty batch writes nothing");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exemptions_follow_addresses_in_place_from_one_chain_snapshot() {
        let dir = std::env::temp_dir().join(format!("u60-exemptions-{}", std::process::id()));
        private_dir(&dir).unwrap();
        let journal = dir.join("journal.json");
        let step = |address: &IpAddr| touch_step(&dir.join(address.to_string().replace(':', "_")));
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        let (a, b, c) = (ip("10.0.0.1"), ip("2001:db8::7"), ip("100.64.0.9"));
        let mut tx = Transaction::new(journal.clone());
        for address in [a, b] {
            tx.apply(step(&address)).unwrap();
        }
        let undos = |tx: &Transaction| tx.undo.len();
        sync_exemptions(&mut tx, &[a, b].into(), &[b, c], &[a, b], step).unwrap();
        assert!(!dir.join("10.0.0.1").exists());
        assert!(dir.join("2001_db8__7").exists() && dir.join("100.64.0.9").exists());
        assert_eq!(
            undos(&tx),
            2,
            "the removed address leaves the journal, the new one joins it"
        );
        // An externally deleted rule is repaired with its existing undo; one
        // still in the snapshot is never inserted twice.
        fs::remove_file(dir.join("100.64.0.9")).unwrap();
        sync_exemptions(&mut tx, &[b].into(), &[b, c], &[b, c], step).unwrap();
        assert!(dir.join("100.64.0.9").exists());
        assert_eq!(undos(&tx), 2);
        assert_eq!(
            serde_json::from_slice::<Journal>(&fs::read(&journal).unwrap())
                .unwrap()
                .undo,
            tx.undo
        );
        tx.rollback().unwrap();
        assert!(!dir.join("2001_db8__7").exists() && !dir.join("100.64.0.9").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_emptied_route_table_is_recognised_from_the_full_listing() {
        let all = "default via 10.0.0.1 dev u60sta0 table 60631\nlocal ::1 dev lo table local proto kernel metric 0";
        assert!(lists_table(all, "60631"));
        assert!(!lists_table(all, "6063"));
        assert!(!lists_table(
            "fe80::/64 dev br-lan proto kernel metric 256",
            "60631"
        ));
    }

    #[test]
    fn only_rules_reaching_into_shared_chains_without_our_identity_are_foreign() {
        let foreign = rule(
            "ip6tables",
            "filter",
            "OUTPUT",
            &["-o", "rmnet+", "-j", "REJECT"],
            true,
        );
        let owned = [
            rule("ip6tables", "filter", "OUTPUT", &["-j", "U60_X"], true),
            rule(
                "iptables",
                "nat",
                "POSTROUTING",
                &["-o", "u60sta0", "-j", "MASQUERADE"],
                true,
            ),
            rule(
                "iptables",
                "nat",
                "redirect_chain",
                &[
                    "-i",
                    "br-lan",
                    "-m",
                    "comment",
                    "--comment",
                    "zte-agent-wifi-relay",
                    "-j",
                    "RETURN",
                ],
                true,
            ),
            rule(
                "ip6tables",
                "filter",
                "U60_X",
                &["-o", "rmnet+", "-j", "REJECT"],
                false,
            ),
        ];
        let mut plan = owned.to_vec();
        plan.push(foreign.clone());
        assert_eq!(foreign_identity(&plan).len(), 1);
        assert_eq!(foreign_identity(&plan)[0].undo, foreign.undo);
    }

    #[test]
    fn policy_rule_checks_selectors_before_repair_or_undo() {
        assert_eq!(
            policy_rule_state("[]", "60631", "60631").unwrap(),
            PolicyRule::Missing
        );
        assert_eq!(
            policy_rule_state(
                r#"[{"priority":60631,"src":"all","table":60631}]"#,
                "60631",
                "60631"
            )
            .unwrap(),
            PolicyRule::Owned
        );
        for text in [
            r#"[{"priority":60631,"src":"2001:db8::/64","table":60631}]"#,
            r#"[{"priority":60631,"src":"all","fwmark":1,"table":60631}]"#,
            r#"[{"priority":60631,"src":"all","table":100}]"#,
        ] {
            assert_eq!(
                policy_rule_state(text, "60631", "60631").unwrap(),
                PolicyRule::Foreign
            );
        }
        assert_eq!(
            policy_rule_state(
                r#"[{"priority":60631},{"priority":60631}]"#,
                "60631",
                "60631"
            )
            .unwrap(),
            PolicyRule::Shared
        );
        assert!(
            policy_rule_state("not json", "60631", "60631").is_err(),
            "only an unreadable table is an error"
        );
    }

    #[test]
    fn piped_tool_input_is_bounded_by_the_same_process_deadline() {
        assert_eq!(
            run_input(
                Command::new("cat"),
                Duration::from_secs(1),
                b"test input".to_vec()
            )
            .unwrap(),
            "test input"
        );
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 10"]);
        let start = Instant::now();
        assert_eq!(
            run_input(command, Duration::from_millis(50), vec![0; 1024 * 1024]).unwrap_err(),
            "Device tool timed out"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "Mutates firewall; only run in an empty disposable network namespace"]
    fn isolated_restore_partial_failure_keeps_vendor_rules_and_rolls_back() {
        assert_eq!(
            std::env::var("U60_FIREWALL_ISOLATED_QA").as_deref(),
            Ok("1")
        );
        let links: serde_json::Value =
            serde_json::from_str(&Cmd::new("ip", &["-j", "link"]).run().unwrap()).unwrap();
        let private_ns =
            fs::read_link("/proc/self/ns/net").unwrap() != fs::read_link("/proc/1/ns/net").unwrap();
        assert!(
            Path::new("/.dockerenv").exists() || private_ns,
            "never mutate the host namespace"
        );
        // B31 creates dormant fallback tunnel devices in every fresh namespace.
        let fallback = [
            "tunl0", "gre0", "gretap0", "erspan0", "ip_vti0", "ip6_vti0", "sit0", "ip6tnl0",
            "ip6gre0",
        ];
        assert!(links.as_array().unwrap().iter().all(|l| l["ifname"] == "lo"
            || (private_ns
                && l["operstate"] == "DOWN"
                && l["ifname"].as_str().is_some_and(|n| fallback.contains(&n)))));
        let dir = std::env::temp_dir().join(format!("u60-restore-{}", std::process::id()));
        private_dir(&dir).unwrap();
        for tool in ["iptables", "ip6tables"] {
            Cmd::new(tool, &["-t", "filter", "-N", "VENDOR_QA"])
                .run()
                .unwrap();
            Cmd::new(tool, &["-t", "filter", "-A", "VENDOR_QA", "-j", "RETURN"])
                .run()
                .unwrap();
            let before = Cmd::new(tool, &["-t", "filter", "-S", "VENDOR_QA"])
                .run()
                .unwrap();
            // An identical vendor rule in a shared chain must survive the
            // prejournaled undo of an owned-chain step that never ran.
            let vendor = [
                "-t", "filter", "-A", "OUTPUT", "-o", "rmnet+", "-j", "REJECT",
            ];
            Cmd::new(tool, &vendor).run().unwrap();
            let mut tx = Transaction::new(dir.join("network.json"));
            let mut plan = chain(tool, "filter", "U60_BATCH_F");
            plan.push(rule(
                tool,
                "filter",
                "U60_BATCH_F",
                &["-j", "RETURN"],
                false,
            ));
            plan.extend(chain(tool, "nat", "U60_BATCH_N"));
            plan.push(rule(tool, "nat", "U60_MISSING", &["-j", "ACCEPT"], false));
            plan.extend(chain(tool, "filter", "U60_BATCH_O"));
            plan.push(rule(
                tool,
                "filter",
                "U60_BATCH_O",
                &["-o", "rmnet+", "-j", "REJECT"],
                false,
            ));
            plan.push(rule(tool, "filter", "OUTPUT", &["-j", "U60_BATCH_O"], true));
            assert!(foreign_identity(&plan).is_empty());
            assert!(tx.apply_network(plan).is_err());
            assert!(
                Cmd::new(tool, &["-t", "filter", "-S", "U60_BATCH_F"])
                    .run()
                    .is_ok(),
                "earlier table committed before the later error"
            );
            let saved: Journal =
                serde_json::from_slice(&fs::read(dir.join("network.json")).unwrap()).unwrap();
            assert_eq!(
                saved.undo.len(),
                7,
                "even unexecuted undos were durable before the first commit"
            );
            tx.rollback().unwrap();
            assert!(Cmd::new(tool, &["-t", "filter", "-S", "U60_BATCH_F"])
                .run()
                .is_err());
            assert!(Cmd::new(tool, &["-t", "nat", "-S", "U60_BATCH_N"])
                .run()
                .is_err());
            assert_eq!(
                Cmd::new(tool, &["-t", "filter", "-S", "VENDOR_QA"])
                    .run()
                    .unwrap(),
                before
            );
            let mut check = vendor;
            check[2] = "-C";
            assert!(
                Cmd::new(tool, &check).run().is_ok(),
                "the vendor's identical reject survived"
            );
            check[2] = "-D";
            Cmd::new(tool, &check).run().unwrap();
            Cmd::new(tool, &["-t", "filter", "-F", "VENDOR_QA"])
                .run()
                .unwrap();
            Cmd::new(tool, &["-t", "filter", "-X", "VENDOR_QA"])
                .run()
                .unwrap();
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn repair_retry_keeps_one_undo_and_requires_a_saved_journal() {
        let dir = std::env::temp_dir().join(format!("u60-ensure-{}", std::process::id()));
        private_dir(&dir).unwrap();
        let mut tx = Transaction::new(dir.join("journal.json"));
        let file = dir.join("effect");
        let mut step = touch_step(&file);
        step.apply = Cmd::new("false", &[]);
        assert!(tx.ensure(step.clone()).is_err());
        assert!(tx.ensure(step).is_err());
        assert_eq!(tx.undo.len(), 1);
        tx.ensure(touch_step(&file)).unwrap();
        assert_eq!(tx.undo.len(), 1);
        tx.rollback().unwrap();
        assert!(!file.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remove_undoes_and_forgets_exactly_one_journaled_change() {
        let dir = std::env::temp_dir().join(format!("u60-remove-{}", std::process::id()));
        private_dir(&dir).unwrap();
        let journal = dir.join("journal.json");
        let (first, second) = (dir.join("effect-1"), dir.join("effect-2"));
        let mut tx = Transaction::new(journal.clone());
        tx.apply(touch_step(&first)).unwrap();
        tx.apply(touch_step(&second)).unwrap();
        let first_undo = touch_step(&first).undo;
        tx.remove(&first_undo).unwrap();
        assert!(!first.exists() && second.exists());
        assert_eq!(
            serde_json::from_slice::<Journal>(&fs::read(&journal).unwrap())
                .unwrap()
                .undo,
            vec![touch_step(&second).undo]
        );
        tx.remove(&first_undo).unwrap();
        assert!(
            second.exists(),
            "removing an unknown change touches nothing"
        );
        tx.rollback().unwrap();
        assert!(!second.exists() && !journal.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_removal_stays_journaled_for_recovery() {
        let dir = std::env::temp_dir().join(format!("u60-remove-fail-{}", std::process::id()));
        private_dir(&dir).unwrap();
        let journal = dir.join("journal.json");
        let blocker = dir.join("blocker");
        let mut tx = Transaction::new(journal.clone());
        tx.apply(Step::new(
            Cmd::new("mkdir", &[blocker.to_str().unwrap()]),
            Cmd::new("rmdir", &[blocker.to_str().unwrap()]),
        ))
        .unwrap();
        fs::write(blocker.join("busy"), b"x").unwrap();
        let undo = Cmd::new("rmdir", &[blocker.to_str().unwrap()]);
        assert!(tx.remove(&undo).is_err());
        assert!(journal.exists());
        fs::remove_file(blocker.join("busy")).unwrap();
        tx.remove(&undo).unwrap();
        assert!(!blocker.exists());
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
