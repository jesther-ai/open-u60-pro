//! Bounded restore batches for generated, agent-owned firewall plans.
//! Preserve command order (including table COMMIT boundaries); a failed batch
//! is partially applied and must be unwound by its caller's durable journal.
use std::collections::HashSet;
use std::net::IpAddr;
use std::process::Command;
use std::time::Duration;

use super::{Cmd, Step};

fn token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:/,+=![]".contains(&b))
}

fn entry(cmd: &Cmd) -> Option<(&str, &str, String)> {
    let program = match cmd.program.as_str() {
        "iptables" => "iptables-restore",
        "ip6tables" => "ip6tables-restore",
        _ => return None,
    };
    let a = &cmd.args;
    if a.len() < 6 || a[..3] != ["-w", "2", "-t"] || !a.iter().all(|s| token(s)) {
        return None;
    }
    let table = a[3].as_str();
    if !["filter", "nat", "mangle"].contains(&table) {
        return None;
    }
    let owned = a[5].starts_with("U60_");
    match a[4].as_str() {
        "-N" if owned && a.len() == 6 => Some((program, table, format!("-N {}", a[5]))),
        "-A" | "-I"
            if owned
                || [
                    "INPUT",
                    "OUTPUT",
                    "FORWARD",
                    "PREROUTING",
                    "POSTROUTING",
                    "redirect_chain",
                ]
                .contains(&a[5].as_str()) =>
        {
            Some((program, table, a[4..].join(" ")))
        }
        _ => None,
    }
}

#[derive(Debug)]
pub(super) struct Batch {
    program: String,
    input: String,
    pub(super) count: usize,
}

impl Batch {
    pub(super) fn collect(steps: &[Step]) -> Option<Self> {
        let (program, _, _) = entry(&steps.first()?.apply)?;
        let mut input = String::new();
        let mut current = "";
        let mut count = 0;
        for step in steps {
            let Some((next, table, line)) = entry(&step.apply) else {
                break;
            };
            if next != program || input.len() + line.len() > 128 * 1024 {
                break;
            }
            if table != current {
                if !current.is_empty() {
                    input.push_str("COMMIT\n");
                }
                input.push('*');
                input.push_str(table);
                input.push('\n');
                current = table;
            }
            input.push_str(&line);
            input.push('\n');
            count += 1;
        }
        if count == 0 {
            return None;
        }
        input.push_str("COMMIT\n");
        Some(Self {
            program: program.into(),
            input,
            count,
        })
    }

    pub(super) fn run(self) -> Result<(), String> {
        let mut command = Command::new(self.program);
        command.args(["--noflush", "--wait", "2"]);
        super::run_input(command, Duration::from_secs(8), self.input.into_bytes()).map(|_| ())
    }
}

/// Read a chain once instead of spawning one `-C` per local address. Only
/// exact-address exclusions of the requested target count as present.
pub fn exclusions(
    tool: &str,
    table: &str,
    chain: &str,
    target: &str,
) -> Result<HashSet<IpAddr>, String> {
    let text = Cmd::new(tool, &["-w", "2", "-t", table, "-S", chain]).run()?;
    Ok(parse_exclusions(&text, chain, target))
}

fn parse_exclusions(text: &str, chain: &str, target: &str) -> HashSet<IpAddr> {
    text.lines()
        .filter_map(|line| {
            let a: Vec<_> = line.split_whitespace().collect();
            if a.len() != 6 || a[..3] != ["-A", chain, "-d"] || a[4..] != ["-j", target] {
                return None;
            }
            let (ip, prefix) = a[3]
                .split_once('/')
                .map_or((a[3], None), |(ip, p)| (ip, Some(p)));
            let ip: IpAddr = ip.parse().ok()?;
            if prefix.is_some_and(|p| p != if ip.is_ipv4() { "32" } else { "128" }) {
                return None;
            }
            Some(ip)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_preserves_tables_insert_positions_and_effect_barriers() {
        let mut plan = super::super::chain("iptables", "filter", "U60_TEST");
        plan.push(super::super::rule(
            "iptables",
            "filter",
            "INPUT",
            &["-j", "U60_TEST"],
            true,
        ));
        plan.extend(super::super::chain("iptables", "nat", "U60_DNS"));
        plan.push(Step::new(
            Cmd::new("ip", &["-4", "rule", "add"]),
            Cmd::new("true", &[]),
        ));
        plan.extend(super::super::chain("iptables", "mangle", "U60_LATER"));
        let batch = Batch::collect(&plan).unwrap();
        assert_eq!(batch.count, 3);
        assert_eq!(
            batch.input,
            "*filter\n-N U60_TEST\n-I INPUT 1 -j U60_TEST\nCOMMIT\n*nat\n-N U60_DNS\nCOMMIT\n"
        );
        for cmd in [
            Cmd::new("iptables", &["-F"]),
            Cmd::new("iptables", &["-w", "2", "-t", "filter", "-N", "vendor"]),
            Cmd::new(
                "iptables",
                &["-w", "2", "-t", "filter", "-A", "U60_TEST", "\nCOMMIT"],
            ),
        ] {
            assert!(entry(&cmd).is_none());
        }
    }
    #[test]
    fn snapshots_match_only_exact_unconditional_exclusions() {
        let text = "-A U60_X -d 192.0.2.1/32 -j RETURN\n-A U60_X -d 192.0.2.0/24 -j RETURN\n-A U60_X -d 2001:db8::1/128 -j RETURN\n-A U60_X -p tcp -d 192.0.2.2 -j RETURN\n-A OTHER -d 192.0.2.3 -j RETURN";
        assert_eq!(
            parse_exclusions(text, "U60_X", "RETURN"),
            ["192.0.2.1".parse().unwrap(), "2001:db8::1".parse().unwrap()].into()
        );
    }
}
