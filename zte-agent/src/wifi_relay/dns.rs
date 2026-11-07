//! Add current Wi-Fi resolvers to the vendor's volatile resolv.conf. Teardown
//! removes tagged lines only; there is deliberately no baseline to restore.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

const TAG: &str = " # zte-agent-wifi-relay";
const MARKER: &str = "# zte-agent-wifi-relay nameserver ";
const PATH: &str = "/tmp/resolv.conf";

pub(super) fn reconcile(resolvers: &[Ipv4Addr]) -> Result<(), String> {
    update_current(
        Path::new(PATH),
        Path::new("/tmp/resolv.conf.d/resolv.conf.auto"),
        resolvers,
    )
}

fn update_current(entry: &Path, auto: &Path, resolvers: &[Ipv4Addr]) -> Result<(), String> {
    let target = match fs::read_link(entry) {
        Ok(link) if link == auto => auto,
        Ok(_) => {
            return Err("Unsupported runtime DNS symlink; configuration was not changed".into())
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => entry,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && resolvers.is_empty() => return Ok(()),
        Err(_) => return Err("Cannot inspect runtime DNS path".into()),
    };
    update(target, resolvers)
}

fn render(current: &str, resolvers: &[Ipv4Addr]) -> String {
    let mut next = String::new();
    for address in resolvers.iter().take(3) {
        // B31 BusyBox nslookup does not strip trailing resolver comments.
        next.push_str(&format!("{MARKER}{address}\nnameserver {address}\n"));
    }
    let mut lines = current.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        let text = line.trim_end_matches(['\n', '\r']);
        if let Some(address) = text
            .strip_prefix(MARKER)
            .and_then(|s| s.parse::<Ipv4Addr>().ok())
        {
            if lines.peek().is_some_and(|next| {
                next.trim_end_matches(['\n', '\r']) == format!("nameserver {address}")
            }) {
                lines.next();
            }
            continue;
        }
        let owned = text
            .strip_suffix(TAG)
            .and_then(|s| s.strip_prefix("nameserver "))
            .is_some_and(|s| s.parse::<Ipv4Addr>().is_ok());
        if !owned {
            next.push_str(line);
        }
    }
    next
}

fn update(path: &Path, resolvers: &[Ipv4Addr]) -> Result<(), String> {
    for attempt in 0..3 {
        let mut source = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && resolvers.is_empty() => {
                return Ok(())
            }
            Err(_) => return Err("Cannot read current runtime DNS configuration".into()),
        };
        let meta = source
            .metadata()
            .map_err(|_| "Cannot inspect runtime DNS configuration")?;
        let mut before = String::new();
        Read::by_ref(&mut source)
            .take(65537)
            .read_to_string(&mut before)
            .map_err(|_| "Invalid runtime DNS configuration")?;
        if before.len() > 65536 || !meta.is_file() {
            return Err("Invalid runtime DNS configuration".into());
        }
        let next = render(&before, resolvers);
        if next == before {
            return Ok(());
        }
        let temporary = path.with_file_name(format!(
            ".u60-relay-resolv-{}-{attempt}",
            std::process::id()
        ));
        let mut staged = false;
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)
                .map_err(|_| "Cannot stage runtime DNS configuration")?;
            staged = true;
            file.write_all(next.as_bytes())
                .map_err(|_| "Cannot write runtime DNS configuration")?;
            if unsafe { libc::getuid() } == 0
                && unsafe {
                    libc::fchown(
                        std::os::fd::AsRawFd::as_raw_fd(&file),
                        meta.uid(),
                        meta.gid(),
                    )
                } != 0
            {
                return Err("Cannot preserve runtime DNS ownership");
            }
            file.set_permissions(fs::Permissions::from_mode(meta.mode()))
                .map_err(|_| "Cannot preserve runtime DNS permissions")?;
            let current = fs::symlink_metadata(path)
                .map_err(|_| "Cannot inspect current runtime DNS configuration")?;
            if current.ino() != meta.ino()
                || current.dev() != meta.dev()
                || fs::read_to_string(path).ok().as_ref() != Some(&before)
            {
                return Ok(false);
            }
            fs::rename(&temporary, path).map_err(|_| "Cannot update runtime DNS configuration")?;
            Ok(true)
        })();
        if staged {
            let _ = fs::remove_file(temporary);
        }
        if result? {
            return Ok(());
        }
    }
    Err("Runtime DNS changed concurrently; retry relay DNS update".into())
}

pub(crate) fn command(args: &[String]) -> Result<(), String> {
    let resolvers = args
        .iter()
        .map(|a| {
            a.parse::<Ipv4Addr>()
                .map_err(|_| "Invalid relay DNS resolver")
        })
        .collect::<Result<Vec<_>, _>>()?;
    reconcile(&resolvers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_vendor_dns_gets_current_wifi_dns_without_restoring_stale_cellular() {
        let wifi = ["192.168.2.1".parse().unwrap()];
        let active = render("", &wifi);
        assert!(active.contains("\nnameserver 192.168.2.1\n"));
        assert!(active
            .lines()
            .filter(|l| l.starts_with("nameserver "))
            .all(|l| l.split_whitespace().count() == 2));
        assert_eq!(render(&active, &wifi), active);
        let external = "# vendor current\nnameserver 2001:db8:9::53\nsearch current.test\n";
        let renewed = render(&(active + external), &wifi);
        assert_eq!(render(&renewed, &[]), external);
        assert_eq!(
            render("# vendor cleared DNS\n", &[]),
            "# vendor cleared DNS\n"
        );
        assert_eq!(render("", &[]), "");
        assert_eq!(
            render("nameserver 192.0.2.1 # zte-agent-wifi-relay\n", &[]),
            ""
        );
        assert_eq!(
            render(
                "# zte-agent-wifi-relay nameserver 192.0.2.1\nnameserver 192.0.2.2\n",
                &[]
            ),
            "nameserver 192.0.2.2\n"
        );
    }

    #[test]
    fn runtime_file_tracks_fresh_vendor_writes_and_removes_only_owned_lines() {
        let root = std::env::temp_dir().join(format!("u60-relay-dns-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("resolv.conf");
        fs::write(&path, b"").unwrap();
        let wifi = ["192.168.2.1".parse().unwrap()];
        update(&path, &wifi).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), render("", &wifi));
        fs::write(&path, "# new vendor data\nnameserver 2001:db8:9::54\n").unwrap();
        update(&path, &wifi).unwrap();
        update(&path, &[]).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# new vendor data\nnameserver 2001:db8:9::54\n"
        );
        update(&path, &[]).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn b31_netifd_symlink_is_preserved_and_unknown_targets_are_refused() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("u60-relay-dns-link-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let auto = root.join("resolv.conf.auto");
        let link = root.join("resolv.conf");
        fs::write(&auto, "").unwrap();
        symlink(&auto, &link).unwrap();
        update_current(&link, &auto, &["192.0.2.53".parse().unwrap()]).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), auto);
        assert!(fs::read_to_string(&auto)
            .unwrap()
            .contains("nameserver 192.0.2.53"));
        update_current(&link, &auto, &[]).unwrap();
        assert_eq!(fs::read_to_string(&auto).unwrap(), "");
        assert!(update_current(&link, &root.join("other"), &[]).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
