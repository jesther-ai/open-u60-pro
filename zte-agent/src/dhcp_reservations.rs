//! DHCPv4 reservations via B31's already-enabled dnsmasq read-ethers.
//! A scoped /etc/ethers block survives reboot and avoids touching dhcp UCI
//! (the IPv6 relay owns temporary RA options there). SIGHUP reloads leases
//! without restarting dnsmasq, changing LAN addresses or interrupting relay.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::net_tools::Cmd;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const PATH: &str = "/etc/ethers";
const BEGIN: &str = "# BEGIN zte-agent DHCP reservations\n";
const END: &str = "# END zte-agent DHCP reservations\n";
const MAX_BYTES: u64 = 64 * 1024;

#[derive(Clone)]
struct Reservation {
    mac: String,
    ip: Ipv4Addr,
    name: String,
}

fn id(mac: &str) -> String {
    Sha256::digest(mac.as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn mac(input: &str) -> Result<String, String> {
    let parts: Vec<_> = input.split(':').collect();
    if parts.len() != 6 || parts.iter().any(|p| p.len() != 2) {
        return Err("Enter a MAC as aa:bb:cc:dd:ee:ff".into());
    }
    let bytes: Vec<u8> = parts
        .iter()
        .map(|p| u8::from_str_radix(p, 16))
        .collect::<Result<_, _>>()
        .map_err(|_| "Invalid MAC address")?;
    if bytes[0] & 1 != 0 || bytes.iter().all(|b| *b == 0) {
        return Err("Use an individual client MAC, not a multicast address".into());
    }
    Ok(bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":"))
}

fn read(path: &Path) -> Result<String, String> {
    let mut text = String::new();
    fs::File::open(path)
        .map_err(|_| "Cannot read DHCP reservations")?
        .take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Invalid DHCP reservation file")?;
    if text.len() as u64 > MAX_BYTES {
        return Err("DHCP reservation file is too large".into());
    }
    Ok(text)
}

fn split(text: &str) -> Result<(&str, &str, Vec<Reservation>), String> {
    let Some(start) = text.find(BEGIN) else {
        if text.contains(END) {
            return Err("Incomplete DHCP reservation block".into());
        }
        return Ok((text, "", Vec::new()));
    };
    if start > 0 && text.as_bytes()[start - 1] != b'\n' {
        return Err("Invalid DHCP reservation block".into());
    }
    let middle = &text[start + BEGIN.len()..];
    let end = middle
        .find(END)
        .ok_or("Incomplete DHCP reservation block")?;
    let after = &middle[end + END.len()..];
    if middle[..end].contains(BEGIN) || after.contains(BEGIN) || after.contains(END) {
        return Err("Duplicate DHCP reservation block".into());
    }
    let mut entries = Vec::new();
    let mut name = None;
    for line in middle[..end].lines().filter(|s| !s.trim().is_empty()) {
        if let Some(label) = line.strip_prefix("# client: ") {
            if name.replace(label).is_some() {
                return Err("Invalid saved reservation label".into());
            }
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 2 {
            return Err("Invalid saved reservation".into());
        }
        entries.push(Reservation {
            mac: mac(fields[0])?,
            ip: fields[1].parse().map_err(|_| "Invalid saved IP")?,
            name: name.take().ok_or("Missing saved reservation label")?.into(),
        });
    }
    if name.is_some() {
        return Err("Incomplete saved reservation".into());
    }
    Ok((&text[..start], after, entries))
}

fn render(before: &str, after: &str, entries: &[Reservation]) -> String {
    let mut text = before.to_owned();
    if !entries.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(BEGIN);
        for entry in entries {
            // read-ethers does NOT strip inline comments: it would interpret
            // "192.168.0.60 # Phone" as a hostname instead of a fixed address.
            text.push_str(&format!(
                "# client: {}\n{} {}\n",
                entry.name, entry.mac, entry.ip
            ));
        }
        text.push_str(END);
    }
    text.push_str(after);
    text
}

fn host(ip: Ipv4Addr, lan: Ipv4Addr, mask: Ipv4Addr) -> bool {
    let (ip, lan, mask) = (u32::from(ip), u32::from(lan), u32::from(mask));
    let bits = mask.leading_ones();
    mask.count_ones() == bits
        && (8..=30).contains(&bits)
        && ip & mask == lan & mask
        && ip != lan
        && ip & !mask != 0
        && ip | mask != u32::MAX
}

fn supported() -> Result<(), String> {
    let config = read(Path::new("/var/etc/dnsmasq.conf.lan_dns"))?;
    if !config.lines().any(|l| l.trim() == "read-ethers") {
        return Err(
            "This firmware does not enable dnsmasq read-ethers; reservations are unavailable"
                .into(),
        );
    }
    if !config
        .lines()
        .any(|l| l.trim_start().starts_with("dhcp-range="))
    {
        return Err("Enable the LAN DHCP server before adding reservations".into());
    }
    if Cmd::new("pidof", &["dnsmasq"]).run()?.is_empty() {
        return Err("dnsmasq is not running".into());
    }
    Ok(())
}

fn reload() -> Result<(), String> {
    let pids = Cmd::new("pidof", &["dnsmasq"]).run()?;
    if pids.trim().is_empty() {
        return Err("dnsmasq is not running".into());
    }
    let mut signaled = false;
    let process_ids: Vec<i32> = pids
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| "Invalid dnsmasq PID")?;
    for &pid in &process_ids {
        if pid <= 1
            || fs::read_link(format!("/proc/{pid}/exe")).ok().as_deref()
                != Some(Path::new("/usr/sbin/dnsmasq"))
        {
            return Err("dnsmasq process changed; retry".into());
        }
        // The DNS worker inherits its command line and UID. Signal only the
        // supervisor (its parent is not another dnsmasq), not the TCP worker.
        let status = fs::read_to_string(format!("/proc/{pid}/status"))
            .map_err(|_| "dnsmasq process changed; retry")?;
        let parent = status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))
            .and_then(|p| p.trim().parse::<i32>().ok())
            .ok_or("Cannot identify dnsmasq supervisor")?;
        if process_ids.contains(&parent) {
            continue;
        }
        if unsafe { libc::kill(pid, libc::SIGHUP) } != 0 {
            return Err("Cannot reload dnsmasq reservations".into());
        }
        signaled = true;
    }
    if signaled {
        Ok(())
    } else {
        Err("Cannot identify dnsmasq supervisor".into())
    }
}

fn write(path: &Path, previous: &str, next: &str) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Cannot inspect DHCP reservation file")?;
    if !metadata.is_file() {
        return Err("DHCP reservation file must be a regular file".into());
    }
    let stage = path.with_extension("u60-new");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(metadata.permissions().mode() & 0o777)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&stage)
        .map_err(|_| "DHCP reservation staging file is busy")?;
    let result = (|| {
        if unsafe { libc::fchown(file.as_raw_fd(), metadata.uid(), metadata.gid()) } != 0 {
            return Err("Cannot preserve DHCP reservation ownership".into());
        }
        file.set_permissions(metadata.permissions())
            .map_err(|_| "Cannot preserve DHCP reservation permissions")?;
        file.write_all(next.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|_| "Cannot write DHCP reservations")?;
        if read(path)? != previous {
            return Err("DHCP reservations changed outside this panel; reload and retry".into());
        }
        fs::rename(&stage, path).map_err(|_| "Cannot save DHCP reservations".into())
    })();
    if result.is_err() {
        let _ = fs::remove_file(stage);
    }
    result
}

fn status_data(text: &str) -> Result<Value, String> {
    let (_, _, entries) = split(text)?;
    Ok(json!({"entries":entries.iter().map(|r| json!({"id":id(&r.mac),"mac":r.mac,"ip":r.ip.to_string(),"name":r.name})).collect::<Vec<_>>()}))
}

pub fn status(_state: &crate::handlers::AppState) -> (u16, Value) {
    let result = read(Path::new(PATH)).and_then(|text| status_data(&text));
    match result {
        Ok(data) => (200, json!({"ok":true,"data":data})),
        Err(e) => (503, json!({"ok":false,"error":e})),
    }
}

pub fn change(state: &crate::handlers::AppState, body: &[u8]) -> (u16, Value) {
    let result = (|| {
        let _lock = state
            .network_lock
            .try_lock()
            .map_err(|_| "A network operation is in progress")?;
        let request: Value =
            serde_json::from_slice(body).map_err(|_| "Invalid reservation request")?;
        supported()?;
        let previous = read(Path::new(PATH))?;
        let (before, after, mut entries) = split(&previous)?;
        match request["action"].as_str() {
            Some("add") => {
                let mac = mac(request["mac"].as_str().unwrap_or("").trim())?;
                let ip: Ipv4Addr = request["ip"]
                    .as_str()
                    .unwrap_or("")
                    .parse()
                    .map_err(|_| "Enter an IPv4 address")?;
                let name = request["name"].as_str().unwrap_or("").trim();
                if name.is_empty()
                    || name.len() > 64
                    || name.chars().any(|c| c.is_control() || c == '#')
                {
                    return Err(
                        "Enter a name up to 64 bytes without control characters or #".into(),
                    );
                }
                let lan = Cmd::new("uci", &["-q", "get", "network.lan.ipaddr"])
                    .run()?
                    .parse()
                    .map_err(|_| "Invalid LAN IP")?;
                let mask = Cmd::new("uci", &["-q", "get", "network.lan.netmask"])
                    .run()?
                    .parse()
                    .map_err(|_| "Invalid LAN subnet")?;
                if !host(ip, lan, mask) {
                    return Err(
                        "IP must be a usable address in U60's LAN subnet, not the router address"
                            .into(),
                    );
                }
                if entries.len() >= 64 {
                    return Err("At most 64 reservations".into());
                }
                if entries.iter().any(|e| e.mac == mac || e.ip == ip) {
                    return Err(
                        "MAC or IP already reserved; remove the old reservation first".into(),
                    );
                }
                for line in before.lines().chain(after.lines()) {
                    let fields: Vec<_> = line
                        .split('#')
                        .next()
                        .unwrap_or("")
                        .split_whitespace()
                        .collect();
                    if fields.len() >= 2
                        && (fields[0].eq_ignore_ascii_case(&mac) || fields[1] == ip.to_string())
                    {
                        return Err(
                            "MAC or IP is reserved by the official panel; edit it there".into()
                        );
                    }
                }
                // Include vendor/UCI reservations and current leases in conflict checks.
                let dhcp =
                    crate::ubus::uci_show_map(&Cmd::new("uci", &["-q", "show", "dhcp"]).run()?);
                if dhcp.iter().any(|(key, value)| {
                    (key.ends_with(".mac")
                        && value
                            .split_whitespace()
                            .any(|m| m.eq_ignore_ascii_case(&mac)))
                        || (key.ends_with(".ip") && value == &ip.to_string())
                }) {
                    return Err("MAC or IP conflicts with an existing DHCP host".into());
                }
                if let Ok(leases) = fs::read_to_string("/tmp/dhcp.leases") {
                    for line in leases.lines() {
                        let fields: Vec<_> = line.split_whitespace().collect();
                        if fields.len() >= 3
                            && fields[2] == ip.to_string()
                            && !fields[1].eq_ignore_ascii_case(&mac)
                        {
                            return Err(
                                "Another client currently leases this IP; choose a free address"
                                    .into(),
                            );
                        }
                    }
                }
                entries.push(Reservation {
                    mac,
                    ip,
                    name: name.into(),
                });
            }
            Some("delete") => {
                let target = request["id"].as_str().ok_or("Missing reservation ID")?;
                let len = entries.len();
                entries.retain(|r| id(&r.mac) != target);
                if entries.len() == len {
                    return Err("Reservation not found".into());
                }
            }
            _ => return Err("Unknown reservation action".into()),
        }
        let next = render(before, after, &entries);
        write(Path::new(PATH), &previous, &next)?;
        if let Err(e) = reload() {
            write(Path::new(PATH), &next, &previous)?;
            let _ = reload();
            return Err(e);
        }
        Ok::<_, String>(())
    })();
    match result {
        Ok(()) => status(state),
        Err(e) => (503, json!({"ok":false,"error":e})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservation_status_shows_full_mac_but_keeps_opaque_action_id() {
        let text = render("", "", &[Reservation {
            mac: "02:aa:bb:00:01:02".into(),
            ip: "192.168.0.44".parse().unwrap(),
            name: "Mac".into(),
        }]);
        let data = status_data(&text).unwrap();
        assert_eq!(data["entries"][0]["mac"], "02:aa:bb:00:01:02");
        assert_eq!(data["entries"][0]["id"], id("02:aa:bb:00:01:02"));
        assert_eq!(data["entries"][0]["ip"], "192.168.0.44");
    }

    #[test]
    fn managed_block_preserves_other_entries_and_never_reuses_a_broadcast_mac() {
        let before = "# vendor\n02:11:22:33:44:55 192.168.0.3\n";
        let after = "# trailing vendor bytes\n";
        let entries = vec![Reservation {
            mac: mac("02:AA:bb:00:01:02").unwrap(),
            ip: "192.168.0.4".parse().unwrap(),
            name: "Phone".into(),
        }];
        let text = render(before, after, &entries);
        let (a, b, parsed) = split(&text).unwrap();
        assert_eq!(a, before);
        assert_eq!(b, after);
        assert_eq!(parsed[0].mac, "02:aa:bb:00:01:02");
        assert!(text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .all(|l| l.split_whitespace().count() == 2));
        assert_eq!(render(a, b, &[]), format!("{before}{after}"));
        assert!(mac("ff:ff:ff:ff:ff:ff").is_err());
        assert!(mac("00:00:00:00:00:00").is_err());
        assert!(split(&format!("{BEGIN}broken")).is_err());
    }
    #[test]
    fn reservation_must_be_a_client_address_inside_current_lan() {
        let lan = "192.168.0.1".parse().unwrap();
        let mask = "255.255.255.0".parse().unwrap();
        for ip in ["192.168.0.0", "192.168.0.1", "192.168.0.255", "10.0.0.2"] {
            assert!(!host(ip.parse().unwrap(), lan, mask));
        }
        assert!(host("192.168.0.60".parse().unwrap(), lan, mask));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "Mutates DHCP in a disposable OpenWrt container only"]
    fn isolated_dhcp_reservation_lifecycle() {
        assert!(Path::new("/.dockerenv").exists());
        assert!(Path::new("/u60-dhcp-disposable").exists());
        assert_eq!(std::env::var("U60_DHCP_QA").as_deref(), Ok("1"));
        let original = fs::read(PATH).unwrap();
        let app = crate::handlers::AppState::new();
        let result = change(&app,br#"{"action":"add","name":"QA client","mac":"02:00:00:00:60:02","ip":"192.168.50.60"}"#);
        assert_eq!(result.0, 200, "{}", result.1);
        assert_eq!(result.1["data"]["entries"][0]["mac"], "02:00:00:00:60:02");
        crate::net_tools::write_private(Path::new("/tmp/qa-ready"), b"1").unwrap();
        for _ in 0..200 {
            if Path::new("/tmp/qa-client-leased").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(
            Path::new("/tmp/qa-client-leased").exists(),
            "client must receive reserved address"
        );
        let conflict = change(
            &app,
            br#"{"action":"add","name":"Other","mac":"02:00:00:00:60:03","ip":"192.168.50.60"}"#,
        );
        assert_eq!(conflict.0, 503);
        let result = change(
            &app,
            json!({"action":"delete","id":id("02:00:00:00:60:02")})
                .to_string()
                .as_bytes(),
        );
        assert_eq!(result.0, 200);
        assert_eq!(
            fs::read(PATH).unwrap(),
            original,
            "remove must restore unrelated ethers bytes"
        );
    }
}
