//! B31's offline DNS portal follows cellular state, not our Wi-Fi uplink.
//! Suppress it with one volatile UCI delta; never commit or restore a snapshot.

use crate::net_tools::{Cmd, Step};

const KEY: &str = "zwrt_redirect.redirect.switch";
const PACKAGE: &str = "zwrt_redirect";

pub(super) fn plan() -> Result<Option<Step>, String> {
    plan_with(&mut |cmd| cmd.run())
}

pub(super) fn web_bypass(tool: &str) -> Step {
    crate::net_tools::rule(
        tool,
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
    )
}

pub(super) fn reconcile_web() -> Result<(), String> {
    for tool in ["iptables", "ip6tables"] {
        let rule = web_bypass(tool);
        let mut check = rule.undo;
        check.args[4] = "-C".into();
        if check.run().is_err() {
            // The matching undo is already journaled before relay activation.
            // Vendor WAN events can replace redirect_chain while STA stays up.
            rule.apply.run()?;
        }
    }
    Ok(())
}

fn plan_with(run: &mut impl FnMut(&Cmd) -> Result<String, String>) -> Result<Option<Step>, String> {
    match current(run)?.as_str() {
        "0" => Ok(None),
        "1" if switch_delta(&run(&Cmd::new("uci", &["-q", "changes", PACKAGE]))?).is_none() => {
            Ok(Some(Step::new(
                Cmd::new("@u60-relay-portal", &["disable"]),
                Cmd::new("@u60-relay-portal", &["release"]),
            )))
        }
        _ => Err("Offline redirect setting has pending or unsupported changes; finish those before connecting relay".into()),
    }
}

fn current(run: &mut impl FnMut(&Cmd) -> Result<String, String>) -> Result<String, String> {
    run(&Cmd::new("uci", &["-q", "get", KEY]))
        .map(|value| value.trim().to_owned())
        .map_err(|_| "Cannot read firmware offline redirect setting".into())
}

fn switch_delta(changes: &str) -> Option<&str> {
    changes.lines().find(|line| {
        let line = line.strip_prefix('-').unwrap_or(line);
        line == KEY
            || line
                .strip_prefix(KEY)
                .is_some_and(|rest| rest.starts_with('='))
    })
}

fn owned_delta(changes: &str) -> bool {
    switch_delta(changes) == Some("zwrt_redirect.redirect.switch='0'")
}

pub(crate) fn command(args: &[String]) -> Result<(), String> {
    command_with(args, &mut |cmd| cmd.run())
}

fn command_with(
    args: &[String],
    run: &mut impl FnMut(&Cmd) -> Result<String, String>,
) -> Result<(), String> {
    match args {
        [action] if action == "disable" => {
            let value = current(run)?;
            let changes = run(&Cmd::new("uci", &["-q", "changes", PACKAGE]))?;
            if value == "1" && switch_delta(&changes).is_none() {
                run(&Cmd::new("uci", &["set", &format!("{KEY}=0")]))?;
            } else if value != "0" || !owned_delta(&changes) {
                return Err("Offline redirect changed while connecting relay; retry".into());
            }
        }
        [action] if action == "release" => {
            let changes = run(&Cmd::new("uci", &["-q", "changes", PACKAGE]))?;
            if owned_delta(&changes) && current(run)? == "0" {
                // A vendor/user commit consumes the delta. In that case the
                // current persisted choice wins, including after a reboot.
                run(&Cmd::new("uci", &["revert", KEY]))?;
            }
        }
        _ => return Err("Invalid relay offline redirect command".into()),
    }
    // B31 reads this switch at process start; SIGHUP leaves it unchanged.
    // Repeat on recovery even if revert already succeeded before a crash.
    run(&Cmd::new("/etc/init.d/dnsmasq", &["restart"]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Firmware {
        persisted: String,
        delta: Option<String>,
        unrelated: String,
        serving_redirect: bool,
        fail_restart: bool,
        calls: Vec<Cmd>,
    }

    impl Firmware {
        fn new() -> Self {
            Self {
                persisted: "1".into(),
                delta: None,
                unrelated: "zwrt_redirect.redirect.each_user_redirect='0'".into(),
                serving_redirect: true,
                fail_restart: false,
                calls: Vec::new(),
            }
        }

        fn run(&mut self, cmd: &Cmd) -> Result<String, String> {
            self.calls.push(cmd.clone());
            let args: Vec<_> = cmd.args.iter().map(String::as_str).collect();
            match (cmd.program.as_str(), args.as_slice()) {
                ("uci", ["-q", "get", KEY]) => {
                    Ok(self.delta.as_ref().unwrap_or(&self.persisted).clone())
                }
                ("uci", ["-q", "changes", PACKAGE]) => Ok(format!(
                    "{}\n{}",
                    self.unrelated,
                    self.delta
                        .as_ref()
                        .map(|d| format!("{KEY}='{d}'"))
                        .unwrap_or_default()
                )),
                ("uci", ["set", "zwrt_redirect.redirect.switch=0"]) => {
                    self.delta = Some("0".into());
                    Ok(String::new())
                }
                ("uci", ["revert", KEY]) => {
                    self.delta = None;
                    Ok(String::new())
                }
                ("/etc/init.d/dnsmasq", ["restart"]) => {
                    if self.fail_restart {
                        return Err("restart failed".into());
                    }
                    self.serving_redirect = self.delta.as_ref().unwrap_or(&self.persisted) == "1";
                    Ok(String::new())
                }
                _ => panic!("unexpected command: {cmd:?}"),
            }
        }

        fn action(&mut self, action: &str) -> Result<(), String> {
            command_with(&[action.into()], &mut |cmd| self.run(cmd))
        }
    }

    #[test]
    fn relay_disables_vendor_dns_portal_and_releases_only_its_delta() {
        let mut fw = Firmware::new();
        let step = plan_with(&mut |cmd| fw.run(cmd)).unwrap().unwrap();
        assert_eq!(step.undo, Cmd::new("@u60-relay-portal", &["release"]));
        fw.action("disable").unwrap();
        assert!(!fw.serving_redirect);
        assert_eq!(fw.persisted, "1");
        fw.action("release").unwrap();
        assert!(fw.serving_redirect);
        assert!(fw.delta.is_none());
        assert!(!fw.unrelated.is_empty());
        assert!(!fw
            .calls
            .iter()
            .any(|c| c.args.iter().any(|a| a == "commit")));
    }

    #[test]
    fn disabled_or_pending_user_settings_are_not_taken_over() {
        let mut fw = Firmware::new();
        fw.persisted = "0".into();
        assert!(plan_with(&mut |cmd| fw.run(cmd)).unwrap().is_none());
        fw.persisted = "1".into();
        fw.delta = Some("1".into());
        assert!(plan_with(&mut |cmd| fw.run(cmd)).is_err());
        assert!(fw.action("disable").is_err());
        assert_eq!(fw.delta.as_deref(), Some("1"));
    }

    #[test]
    fn vendor_commit_or_new_user_value_is_preserved_at_release() {
        for committed in [false, true] {
            let mut fw = Firmware::new();
            fw.action("disable").unwrap();
            if committed {
                fw.persisted = "0".into();
                fw.delta = None;
            } else {
                fw.delta = Some("1".into());
            }
            let before = (fw.persisted.clone(), fw.delta.clone());
            fw.action("release").unwrap();
            assert_eq!((fw.persisted, fw.delta), before);
            assert!(!fw
                .calls
                .iter()
                .any(|c| c.args.first().is_some_and(|a| a == "revert")));
        }
    }

    #[test]
    fn partial_apply_and_release_restart_failures_are_retryable() {
        let mut fw = Firmware::new();
        fw.fail_restart = true;
        assert!(fw.action("disable").is_err());
        assert_eq!(fw.delta.as_deref(), Some("0"));
        fw.fail_restart = false;
        fw.action("disable").unwrap();
        assert!(!fw.serving_redirect);
        fw.fail_restart = true;
        assert!(fw.action("release").is_err());
        assert!(fw.delta.is_none());
        assert!(!fw.serving_redirect);
        fw.fail_restart = false;
        fw.action("release").unwrap();
        assert!(fw.serving_redirect);
        fw.action("release").unwrap();
    }

    #[test]
    fn delta_matching_is_exact_and_commands_are_allowlisted() {
        assert!(!owned_delta("zwrt_redirect.redirect.switch_extra='0'"));
        assert!(!owned_delta("-zwrt_redirect.redirect.switch"));
        assert!(!owned_delta("zwrt_redirect.redirect.switch='1'"));
        assert!(owned_delta(
            "zwrt_redirect.other.switch='1'\nzwrt_redirect.redirect.switch='0'"
        ));
        assert!(command_with(&["commit".into()], &mut |_| panic!("unexpected effect")).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "Requires a disposable OpenWrt chroot with real UCI"]
    fn isolated_openwrt_portal_lifecycle_preserves_configuration() {
        assert_eq!(std::env::var("U60_IPV6_ISOLATED_QA").as_deref(), Ok("1"));
        assert!(std::path::Path::new("/u60-ipv6-qa-disposable").is_file());
        let config = std::path::Path::new("/etc/config/zwrt_redirect");
        assert!(!config.exists());
        let original =
            "config zredirect 'redirect'\n option switch '1'\n option each_user_redirect '0'\n";
        std::fs::write(config, original).unwrap();
        Cmd::new(
            "uci",
            &["set", "zwrt_redirect.redirect.each_user_redirect=1"],
        )
        .run()
        .unwrap();
        let mut restarts = 0;
        let mut run = |cmd: &Cmd| {
            if cmd.program == "/etc/init.d/dnsmasq" {
                restarts += 1;
                Ok(String::new())
            } else {
                cmd.run()
            }
        };
        let step = plan_with(&mut run).unwrap().unwrap();
        command_with(&step.apply.args, &mut run).unwrap();
        assert_eq!(current(&mut run).unwrap(), "0");
        assert_eq!(std::fs::read_to_string(config).unwrap(), original);
        command_with(&step.undo.args, &mut run).unwrap();
        command_with(&step.undo.args, &mut run).unwrap();
        assert_eq!(current(&mut run).unwrap(), "1");
        assert_eq!(
            Cmd::new("uci", &["-q", "changes", PACKAGE]).run().unwrap(),
            "zwrt_redirect.redirect.each_user_redirect='1'"
        );
        assert_eq!(std::fs::read_to_string(config).unwrap(), original);
        command_with(&step.apply.args, &mut run).unwrap();
        // Simulate a vendor commit: release must not replay an older value.
        Cmd::new("uci", &["commit", PACKAGE]).run().unwrap();
        command_with(&step.undo.args, &mut run).unwrap();
        assert_eq!(current(&mut run).unwrap(), "0");
        assert!(Cmd::new("uci", &["-q", "changes", PACKAGE])
            .run()
            .unwrap()
            .is_empty());
        assert!(restarts >= 4);
        std::fs::remove_file(config).unwrap();
    }
}
