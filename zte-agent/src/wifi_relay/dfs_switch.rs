//! B31 firmware turns the driver's DFS channel switch off for the first
//! 10 minutes of every 160 MHz 5 GHz start: `/sbin/zte_wifi_dfs_hdlr.sh`
//! runs `iwpriv wlan2 disableDfsCS 1`, sleeps 599 s, then sets it back to 0
//! (the ini also loads it as 1). While it is off, a STA connect with the AP
//! spanning DFS (44/160 covers 52–64) asks the AP to CSA to non-DFS 44/80,
//! but the driver ends that CSA without the channel change: the connect
//! waits 10 s and fails, and `Channel switch in progress` rejects every
//! later connect until the hotspot restarts (the reboot failure).
//!
//! Owner decision 62: before each join the agent turns the switch back on.
//! It first waits for the script to pass its own `disableDfsCS 1`, or the
//! script would turn it off again right after us.

use std::time::{Duration, Instant};

use super::Cmd;

const SCRIPT: &str = "/sbin/zte_wifi_dfs_hdlr.sh";
const WAIT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct Process {
    pid: i32,
    parent: i32,
    args: Vec<String>,
}

fn is_script(process: &Process) -> bool {
    process.args.iter().any(|a| a == SCRIPT)
}

/// The vendor script runs and has not reached its long sleep yet, so it may
/// still run `disableDfsCS 1`. Its only other sleep is the `sleep 1` before.
fn before_long_sleep(processes: &[Process]) -> bool {
    processes.iter().filter(|p| is_script(p)).any(|script| {
        !processes.iter().any(|p| {
            p.parent == script.pid
                && p.args.first().and_then(|a| a.rsplit('/').next()) == Some("sleep")
                && p.args.get(1).is_some_and(|n| n != "1")
        })
    })
}

#[cfg(target_os = "linux")]
fn processes() -> Vec<Process> {
    let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            let args = cmdline.split(|&b| b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
            let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
            let parent = status.lines().find_map(|l| l.strip_prefix("PPid:"))?.trim().parse().ok()?;
            Some(Process { pid, parent, args })
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn processes() -> Vec<Process> {
    Vec::new()
}

/// Best effort: a firmware without the command keeps its old behaviour, and
/// the join goes on either way.
pub(super) fn allow() {
    let started = Instant::now();
    let mut vendor = false;
    loop {
        let processes = processes();
        vendor |= processes.iter().any(is_script);
        if !before_long_sleep(&processes) {
            break;
        }
        if started.elapsed() >= WAIT {
            eprintln!("[relay] {SCRIPT} has not reached its sleep after {} s; turning the DFS channel switch on anyway", WAIT.as_secs());
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    match Cmd::new("iwpriv", &["wlan2", "disableDfsCS", "0"]).run() {
        // Only the firmware's window is worth a line; other joins stay quiet.
        Ok(_) if vendor => eprintln!("[relay] DFS channel switch turned on for wlan2 inside {SCRIPT}'s window (waited {:.1} s)", started.elapsed().as_secs_f32()),
        Ok(_) => {}
        Err(e) => eprintln!("[relay] cannot turn the DFS channel switch on for wlan2: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: i32, parent: i32, args: &[&str]) -> Process {
        Process { pid, parent, args: args.iter().map(|a| (*a).to_owned()).collect() }
    }

    #[test]
    fn no_vendor_script_means_no_wait() {
        assert!(!before_long_sleep(&[process(1, 0, &["init"]), process(9, 1, &["sleep", "599"])]));
    }

    #[test]
    fn script_before_or_inside_its_disable_step_is_waited_for() {
        let script = process(40, 1, &["sh", SCRIPT]);
        assert!(before_long_sleep(&[process(40, 1, &["sh", SCRIPT])]));
        assert!(before_long_sleep(&[process(40, 1, &["sh", SCRIPT]), process(41, 40, &["sleep", "1"])]));
        assert!(before_long_sleep(&[script, process(42, 40, &["/usr/sbin/iwpriv", "wlan2", "disableDfsCS", "1"])]));
    }

    #[test]
    fn script_in_its_long_sleep_is_past_the_disable_step() {
        assert!(!before_long_sleep(&[process(40, 1, &["sh", SCRIPT, "wlan2"]), process(43, 40, &["sleep", "599"])]));
        assert!(!before_long_sleep(&[process(40, 1, &["sh", SCRIPT]), process(43, 40, &["/bin/sleep", "600"])]));
    }

    #[test]
    fn only_the_scripts_own_sleep_counts_and_grep_is_not_the_script() {
        // The vendor's `ps | grep "zte_wifi_dfs_hdlr.sh"` names the file without its path.
        assert!(!before_long_sleep(&[process(7, 1, &["grep", "zte_wifi_dfs_hdlr.sh"])]));
        assert!(before_long_sleep(&[process(40, 1, &["sh", SCRIPT]), process(43, 99, &["sleep", "599"])]));
    }
}
