//! Owns relay changes to the existing hotspots. One preparation per band:
//! 2.4 GHz uses disable/set/enable (CSA cannot restore HT40 on B31);
//! 5 GHz always stages at non-DFS 20 MHz, then lets the driver follow STA.
//! Every changed runtime setting has a journaled undo. Owner OFF wins.
use super::{ap, dfs_switch, net, scan::Channels, width, Cmd, Step, Transaction, HOSTAPD};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn prepare(
    tx: &mut Transaction,
    upstream: u32,
    channels: &Channels,
) -> Result<(), String> {
    if upstream < 3000 {
        return prepare_2ghz(tx, upstream);
    }
    dfs_switch::allow();
    let before = Cmd::new("iw", &["dev", "wlan2", "info"]).run()?;
    let (frequency, bandwidth, center) =
        width::parse_channel(&before).ok_or("Cannot snapshot hotspot channel")?;
    let staging = channels
        .staging_5ghz()
        .ok_or("No enabled non-DFS hotspot staging channel")?;
    tx.apply(Step::new(
        command(staging, 20, staging),
        command(frequency, bandwidth, center),
    ))
}

/// File reads only: the three-second relay watcher must not spawn extra UCI
/// processes. Missing configuration is unknown, not proof of an owner OFF.
pub(super) fn owner_off(iface: &str) -> bool {
    fs::read_to_string("/etc/config/wireless").is_ok_and(|text| configured_off(&text, iface))
}

fn configured_off(text: &str, iface: &str) -> bool {
    let option = |section, key| crate::wifi_watchdog::config_option(text, section, key);
    let (radio, ssid) = if iface == "wlan0" {
        ("wifi0", "main_2g")
    } else {
        ("wifi1", "main_5g")
    };
    option("zte_mbb", "wifi_onoff").as_deref() == Some("0")
        || option(radio, "disabled").as_deref() == Some("1")
        || option(ssid, "disabled").as_deref() == Some("1")
}

/// All hotspot undos (including journals from older releases) share this
/// policy. A removed/disabled hotspot no longer owns a runtime snapshot.
pub(crate) fn skip_undo(cmd: &Cmd) -> bool {
    let iface = if cmd.program == "@u60-relay-channel" {
        Some("wlan2")
    } else if cmd.program == "hostapd_cli" {
        cmd.args
            .windows(2)
            .find(|w| w[0] == "-i")
            .map(|w| w[1].as_str())
    } else {
        None
    };
    iface
        .filter(|i| matches!(*i, "wlan0" | "wlan2"))
        .is_some_and(|i| owner_off(i) || !Path::new(&format!("/sys/class/net/{i}")).exists())
}

fn prepare_2ghz(tx: &mut Transaction, frequency: u32) -> Result<(), String> {
    let iface = "wlan0";
    let channel =
        super::channel(frequency).ok_or("Only 2.4 GHz and 5 GHz upstreams are supported")?;
    let before = ap(iface, "status", &[])?;
    if net::key(&before, "state") != Some("ENABLED") {
        return Err("The matching main hotspot must be enabled".into());
    }
    let original = fs::read_to_string(format!("/data/vendor/wifi/hostapd-{iface}.conf"))
        .map_err(|_| "Cannot snapshot hotspot configuration")?;
    let ht = net::key(&original, "ht_capab").ok_or("Cannot snapshot hotspot HT capabilities")?;
    let mut pairs = vec![
        (
            "channel",
            channel.to_string(),
            net::key(&before, "channel")
                .ok_or("Cannot snapshot hotspot channel")?
                .to_owned(),
        ),
        ("ht_capab", "[SHORT-GI-20]".into(), ht.to_owned()),
    ];
    for key in [
        "vht_oper_chwidth",
        "vht_oper_centr_freq_seg0_idx",
        "he_oper_chwidth",
        "he_oper_centr_freq_seg0_idx",
        "eht_oper_chwidth",
        "eht_oper_centr_freq_seg0_idx",
    ] {
        let fallback = if key.ends_with("idx") {
            "vht_oper_centr_freq_seg0_idx"
        } else {
            "vht_oper_chwidth"
        };
        let found = net::key(&before, key)
            .or_else(|| net::key(&original, key))
            .or_else(|| net::key(&before, fallback));
        // A 2.4 GHz hotspot runs without VHT, so it has no `vht_oper_*` to snapshot or restore.
        let Some(old) = found else {
            if key.starts_with("vht_") {
                continue;
            }
            return Err("Cannot snapshot hotspot channel width".into());
        };
        pairs.push((
            key,
            if key.ends_with("idx") {
                channel.to_string()
            } else {
                "0".into()
            },
            old.into(),
        ));
    }
    let control = |action: &str, args: &[&str]| {
        let mut a = vec!["-p", HOSTAPD, "-i", iface, action];
        a.extend_from_slice(args);
        Cmd::new("hostapd_cli", &a)
    };
    tx.apply(Step::new(control("disable", &[]), control("enable", &[])))?;
    for (key, new, old) in &pairs {
        tx.apply(Step::new(
            control("set", &[key, new]),
            control("set", &[key, old]),
        ))?;
    }
    if owner_off(iface) {
        return Err("Hotspot was turned off while preparing relay".into());
    }
    tx.apply(Step::new(control("enable", &[]), control("disable", &[])))?;
    std::thread::sleep(Duration::from_secs(2));
    let after = ap(iface, "status", &[])?;
    let driver = Cmd::new("iw", &["dev", iface, "info"]).run()?;
    if net::key(&after, "state") != Some("ENABLED")
        || net::key(&after, "freq") != Some(&frequency.to_string())
        || !driver.contains(&format!("({frequency} MHz)"))
    {
        return Err("Hotspot channel change was not confirmed by the driver".into());
    }
    Ok(())
}

fn command(frequency: u32, bandwidth: u32, center: u32) -> Cmd {
    Cmd::new(
        "@u60-relay-channel",
        &[
            &frequency.to_string(),
            &bandwidth.to_string(),
            &center.to_string(),
        ],
    )
}

pub(crate) fn switch(args: &[String]) -> Result<(), String> {
    if owner_off("wlan2") {
        return Err("Hotspot was turned off during relay channel change".into());
    }
    let started = Instant::now();
    switch_with(
        args,
        |cmd| {
            if owner_off("wlan2") {
                return Err("Hotspot was turned off during relay channel change".into());
            }
            net::run(cmd.command(), Duration::from_secs(2))
        },
        || started.elapsed(),
        || std::thread::sleep(Duration::from_secs(1)),
    )
}

fn switch_with(
    args: &[String],
    mut run: impl FnMut(&Cmd) -> Result<String, String>,
    mut elapsed: impl FnMut() -> Duration,
    mut wait: impl FnMut(),
) -> Result<(), String> {
    let values: Vec<u32> = args.iter().filter_map(|s| s.parse().ok()).collect();
    let [frequency, bandwidth, center] = values.as_slice() else {
        return Err("Invalid hotspot recovery channel".into());
    };
    if args.len() != 3
        || !(5000..5900).contains(frequency)
        || ![20, 40, 80, 160].contains(bandwidth)
        || center.abs_diff(*frequency) >= bandwidth / 2
    {
        return Err("Invalid hotspot recovery channel".into());
    }
    let low = center + 10 - bandwidth / 2;
    if *frequency < low || !(frequency - low).is_multiple_of(20) {
        return Err("Invalid hotspot recovery channel".into());
    }
    let info = Cmd::new("iw", &["dev", "wlan2", "info"]);
    let status = Cmd::new("hostapd_cli", &["-p", HOSTAPD, "-i", "wlan2", "status"]);
    let matches =
        |text: &str| width::parse_channel(text) == Some((*frequency, *bandwidth, *center));
    let current = run(&info)?;
    if !matches(&current) {
        if crate::net_tools::key(&run(&status)?, "state") != Some("ENABLED") {
            return Err("Wait for the hotspot radar check before connecting relay".into());
        }
        let mut args = vec![
            "-p".into(),
            HOSTAPD.into(),
            "-i".into(),
            "wlan2".into(),
            "chan_switch".into(),
            "5".into(),
            frequency.to_string(),
        ];
        if *bandwidth > 20 {
            let offset = if ((frequency - low) / 20).is_multiple_of(2) {
                1
            } else {
                -1
            };
            args.push(format!("sec_channel_offset={offset}"));
            args.push(format!("center_freq1={center}"));
        }
        args.push(format!("bandwidth={bandwidth}"));
        args.push("ht".into());
        if *bandwidth >= 80 {
            args.push("vht".into());
        }
        let cmd = Cmd {
            program: "hostapd_cli".into(),
            args,
        };
        if run(&cmd)?.lines().last() != Some("OK") {
            return Err("Hotspot refused the channel switch".into());
        }
    }
    // A return to a 160 MHz hotspot may need the full 60 s CAC. Confirm
    // completion before removing the journal; never skip radar checks.
    while elapsed() < Duration::from_secs(80) {
        if crate::net_tools::key(&run(&status)?, "state") == Some("ENABLED")
            && matches(&run(&info)?)
        {
            return Ok(());
        }
        wait();
    }
    Err("Hotspot channel switch or radar check did not finish; recovery retained".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn external_off_wins_over_runtime_restore() {
        for text in [
            "config zte_mbb 'zte_mbb'\n option wifi_onoff '0'\n",
            "config wifi-device 'wifi1'\n option disabled '1'\n",
            "config wifi-iface 'main_5g'\n option disabled '1'\n",
        ] {
            assert!(configured_off(text, "wlan2"));
        }
        assert!(
            !configured_off("", "wlan2"),
            "unknown config is not owner OFF"
        );
        assert!(!configured_off(
            "config wifi-device 'wifi0'\n option disabled '1'\n",
            "wlan2"
        ));
        assert!(configured_off(
            "config wifi-device 'wifi0'\n option disabled '1'\n",
            "wlan0"
        ));
    }

    #[test]
    fn recovery_deadline_includes_slow_commands() {
        let elapsed = Cell::new(Duration::ZERO);
        let calls = Cell::new(0);
        let result = switch_with(
            &["5180".into(), "160".into(), "5250".into()],
            |cmd| {
                calls.set(calls.get() + 1);
                elapsed.set(elapsed.get() + Duration::from_secs(2));
                Ok(if cmd.program == "iw" {
                    "channel 36 (5180 MHz), width: 160 MHz, center1: 5250 MHz"
                } else {
                    "state=DFS"
                }
                .into())
            },
            || elapsed.get(),
            || elapsed.set(elapsed.get() + Duration::from_secs(1)),
        );
        assert!(result.unwrap_err().contains("recovery retained"));
        assert!(elapsed.get() <= Duration::from_secs(83));
        assert!(
            calls.get() < 30,
            "CAC must not launch hundreds of processes"
        );
    }
    #[test]
    fn dfs_join_staging_and_recovery_wait_for_the_real_channel() {
        let channels = Channels::parse(
            "* 5180 MHz [36]\n* 5260 MHz [52]\n  Radar detection\n* 5500 MHz [100] (disabled)\n",
        );
        assert_eq!(channels.staging_5ghz(), Some(5180));
        assert!(channels.contains(5260));
        assert!(!channels.contains(5500));
        let mut switched = false;
        let mut polls = 0;
        let waits = Cell::new(0);
        switch_with(
            &["5180".into(), "160".into(), "5250".into()],
            |cmd| {
                if cmd.program == "iw" {
                    return Ok(if switched {
                        "channel 36 (5180 MHz), width: 160 MHz, center1: 5250 MHz"
                    } else {
                        "channel 52 (5260 MHz), width: 160 MHz, center1: 5250 MHz"
                    }
                    .into());
                }
                if cmd.args.iter().any(|s| s == "chan_switch") {
                    assert!(cmd.args.contains(&"sec_channel_offset=1".into()));
                    assert!(cmd.args.contains(&"center_freq1=5250".into()));
                    switched = true;
                    return Ok("OK".into());
                }
                polls += 1;
                Ok(if switched && polls < 4 {
                    "state=DFS"
                } else {
                    "state=ENABLED"
                }
                .into())
            },
            || Duration::from_secs(waits.get()),
            || waits.set(waits.get() + 1),
        )
        .unwrap();
        assert_eq!(waits.get(), 2);
        assert!(switch_with(
            &["5180".into(), "20".into(), "5180".into()],
            |cmd| {
                Ok(if cmd.program == "iw" {
                    "channel 52 (5260 MHz), width: 160 MHz, center1: 5250 MHz"
                } else if cmd.args.iter().any(|s| s == "chan_switch") {
                    "FAIL"
                } else {
                    "state=ENABLED"
                }
                .into())
            },
            || Duration::ZERO,
            || panic!("refused CSA must fail immediately")
        )
        .is_err());
    }
}
