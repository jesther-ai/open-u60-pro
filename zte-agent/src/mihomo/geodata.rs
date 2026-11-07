//! Prepare official rule databases outside the shared network lock. The core's
//! short validation command must not double as a hidden first-run downloader.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Mutex;

use serde_yaml_ng::Value;
use sha2::{Digest, Sha256};

use crate::net_tools as net;

const BASE: &str = "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest";
const RAW: &str = "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/release";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Asset {
    remote: &'static str,
    local: &'static str,
    limit: u64,
}

fn required(config: &Value) -> Vec<Asset> {
    fn contains(value: &Value, needle: &str) -> bool {
        match value {
            Value::String(s) => {
                let s = s.to_ascii_uppercase();
                if needle == "GEOIP," {
                    s.split(needle).skip(1).any(|tail| {
                        tail.split(',')
                            .next()
                            .is_some_and(|country| country.trim() != "LAN")
                    })
                } else {
                    s.contains(needle)
                }
            }
            Value::Sequence(v) => v.iter().any(|v| contains(v, needle)),
            Value::Mapping(m) => m
                .iter()
                .any(|(k, v)| contains(k, needle) || contains(v, needle)),
            _ => false,
        }
    }
    // mihomo v1.19.33 also loads databases for the sniffer's `geosite:` and
    // `geoip:` entries (config/config.go parseSniffer) during `-t`.
    let references = Value::Sequence(vec![
        config["rules"].clone(),
        config["sub-rules"].clone(),
        config["rule-providers"].clone(),
        config["dns"].clone(),
        config["sniffer"].clone(),
    ]);
    let mut assets = Vec::new();
    if contains(&references, "GEOSITE,")
        || contains(&references, "GEOSITE:")
        || config["dns"]["fallback-filter"]["geosite"]
            .as_sequence()
            .is_some_and(|v| !v.is_empty())
    {
        assets.push(Asset {
            remote: "geosite.dat",
            local: "GeoSite.dat",
            limit: 16 * 1024 * 1024,
        });
    }
    if contains(&references, "GEOIP,")
        || contains(&references, "GEOIP:")
        || config["dns"]["fallback-filter"]["geoip"].as_bool() == Some(true)
        || config["dns"]["fallback"].as_sequence().is_some_and(|v| {
            !v.is_empty() && config["dns"]["fallback-filter"]["geoip"].as_bool() != Some(false)
        })
    {
        assets.push(if config["geodata-mode"].as_bool() == Some(true) {
            Asset {
                remote: "geoip.dat",
                local: "GeoIP.dat",
                limit: 32 * 1024 * 1024,
            }
        } else {
            Asset {
                remote: "geoip.metadb",
                local: "geoip.metadb",
                limit: 32 * 1024 * 1024,
            }
        });
    }
    if contains(&references, "IP-ASN,") {
        assets.push(Asset {
            remote: "GeoLite2-ASN.mmdb",
            local: "ASN.mmdb",
            limit: 32 * 1024 * 1024,
        });
    }
    assets
}

fn checksum(text: &[u8], filename: &str) -> Result<String, String> {
    let text = std::str::from_utf8(text).map_err(|_| "Invalid geodata checksum")?;
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields.is_empty()
        || fields.len() > 2
        || fields[0].len() != 64
        || !fields[0].bytes().all(|b| b.is_ascii_hexdigit())
        || (fields.len() == 2 && fields[1].trim_start_matches('*') != filename)
    {
        return Err("Invalid geodata checksum".into());
    }
    Ok(fields[0].to_ascii_lowercase())
}

pub(super) fn prepare(
    root: &Path,
    config: &str,
    download: &super::download::Download,
) -> Result<(), String> {
    let config: Value =
        serde_yaml_ng::from_str(config).map_err(|_| "Invalid managed configuration")?;
    prepare_with(root, &required(&config), |url, max| {
        download.bytes(url, max, std::time::Duration::from_secs(60))
    })
}

/// The identity of a database file as last hashed. A replacement (new inode),
/// a size change or any rewrite (which moves the change time) re-hashes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: metadata.len(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

/// Per-database digests, so repeated fingerprints of unchanged files do not
/// re-read tens of MiB under the network lock.
#[derive(Default)]
pub(super) struct Digests(Mutex<HashMap<&'static str, (Stamp, [u8; 32])>>);

impl Digests {
    fn of(&self, path: &Path, asset: &Asset) -> Option<[u8; 32]> {
        let file = fs::File::open(path).ok()?;
        let stamp = Stamp::of(&file.metadata().ok()?);
        let mut known = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, digest)) = known.get(asset.local).filter(|(at, _)| *at == stamp) {
            return Some(*digest);
        }
        let mut digest = Sha256::new();
        let mut file = file.take(asset.limit + 1);
        let mut length = 0u64;
        let mut buf = [0u8; 16384];
        loop {
            let n = file.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            length += n as u64;
            if length > asset.limit {
                return None;
            }
            digest.update(&buf[..n]);
        }
        // Empty, or changed while it was read: never a reusable identity.
        if length == 0 || length != stamp.len {
            known.remove(asset.local);
            return None;
        }
        let digest: [u8; 32] = digest.finalize().into();
        known.insert(asset.local, (stamp, digest));
        Some(digest)
    }
}

/// Hash the actual databases consumed by validation, not just the YAML that
/// names them. Missing/unreadable/oversized dependencies disable cache reuse.
pub(super) fn validation_inputs(root: &Path, config: &str, digests: &Digests) -> Option<String> {
    let config: Value = serde_yaml_ng::from_str(config).ok()?;
    let mut digest = Sha256::new();
    for asset in required(&config) {
        digest.update(asset.local.as_bytes());
        digest.update(digests.of(&root.join(asset.local), &asset)?);
    }
    Some(format!("{:x}", digest.finalize()))
}

fn file_matches(path: &Path, sha: &str, limit: u64) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).is_ok()
        && !bytes.is_empty()
        && bytes.len() as u64 <= limit
        && format!("{:x}", Sha256::digest(&bytes)) == sha
}

fn prepare_with(
    root: &Path,
    assets: &[Asset],
    mut fetch: impl FnMut(&str, u64) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    for asset in assets {
        let path = root.join(asset.local);
        let provenance = root.join(format!("{}.source.txt", asset.local));
        let url = format!("{BASE}/{}", asset.remote);
        let cached = fs::read_to_string(&provenance).ok().and_then(|text| {
            if text.lines().next() != Some(url.as_str()) {
                return None;
            }
            text.lines().find_map(|l| {
                l.strip_prefix("SHA256 ")
                    .and_then(|sha| checksum(sha.as_bytes(), asset.remote).ok())
            })
        });
        if cached.is_some_and(|sha| file_matches(&path, &sha, asset.limit)) {
            continue;
        }
        let mut from = |base: &str| {
            let remote = format!("{base}/{}", asset.remote);
            let sha = checksum(&fetch(&format!("{remote}.sha256sum"), 512)?, asset.remote)?;
            if !file_matches(&path, &sha, asset.limit) {
                let bytes = fetch(&remote, asset.limit)?;
                if bytes.is_empty()
                    || bytes.len() as u64 > asset.limit
                    || format!("{:x}", Sha256::digest(&bytes)) != sha
                {
                    return Err("Geodata size or SHA256 verification failed".into());
                }
                net::write_private(&path, &bytes)?;
            }
            net::write_private(&provenance,
                format!("{url}\nSHA256 {sha}\nVerified source {remote}\nSource and licenses: https://github.com/MetaCubeX/meta-rules-dat\n").as_bytes())
        };
        // Same official repository, independent delivery endpoints; each needs
        // its own checksum. No third-party mirrors or unchecked cached files.
        if let Err(error) = from(BASE).or_else(|_| from(RAW)) {
            return Err(format!("Cannot prepare {} from official MetaCubeX geodata: {error}. Previous configuration is unchanged", asset.local));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_cache_tracks_database_bytes_even_with_preserved_size_and_mtime() {
        let dir = std::env::temp_dir().join(format!("u60-geodata-key-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let config = "geodata-mode: true\nrules: ['GEOIP,CN,DIRECT']";
        let digests = Digests::default();
        assert!(validation_inputs(&dir, config, &digests).is_none());
        let file = dir.join("GeoIP.dat");
        fs::write(&file, b"first").unwrap();
        let time = fs::metadata(&file).unwrap().modified().unwrap();
        let first = validation_inputs(&dir, config, &digests).unwrap();
        assert_eq!(validation_inputs(&dir, config, &digests).unwrap(), first);
        // Content, not timestamps, is the key: an in-place rewrite that keeps
        // size and mtime still differs (fresh digests isolate the content).
        fs::write(&file, b"other").unwrap();
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(time)
            .unwrap();
        assert_ne!(
            validation_inputs(&dir, config, &Digests::default()).unwrap(),
            first
        );
        assert!(validation_inputs(&dir, "rules: ['MATCH,DIRECT']", &digests).is_some());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unchanged_databases_reuse_their_digest_but_a_replacement_is_rehashed() {
        let dir = std::env::temp_dir().join(format!("u60-geodata-memo-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let config = "rules: ['GEOSITE,cn,DIRECT']";
        let file = dir.join("GeoSite.dat");
        fs::write(&file, b"first").unwrap();
        let digests = Digests::default();
        let first = validation_inputs(&dir, config, &digests).unwrap();
        // A reused digest is not recomputed: altering the remembered one shows.
        digests.0.lock().unwrap().get_mut("GeoSite.dat").unwrap().1 = [7; 32];
        let reused = validation_inputs(&dir, config, &digests).unwrap();
        assert_ne!(reused, first);
        let time = fs::metadata(&file).unwrap().modified().unwrap();
        let next = dir.join("GeoSite.dat.new");
        fs::write(&next, b"other").unwrap();
        fs::File::options()
            .write(true)
            .open(&next)
            .unwrap()
            .set_modified(time)
            .unwrap();
        fs::rename(&next, &file).unwrap();
        let replaced = validation_inputs(&dir, config, &digests).unwrap();
        assert_ne!(replaced, reused);
        assert_eq!(
            replaced,
            validation_inputs(&dir, config, &Digests::default()).unwrap()
        );
        fs::remove_file(&file).unwrap();
        assert!(
            validation_inputs(&dir, config, &digests).is_none(),
            "a missing database disables reuse"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_sniffers_geosite_and_geoip_entries_are_required_databases() {
        let config: Value = serde_yaml_ng::from_str(
            "sniffer: {enable: true, skip-domain: ['geosite:apple'], skip-dst-address: ['geoip:cn']}\nrules: ['MATCH,DIRECT']",
        )
        .unwrap();
        let local: Vec<_> = required(&config).iter().map(|a| a.local).collect();
        assert_eq!(local, ["GeoSite.dat", "geoip.metadb"]);
        let plain: Value =
            serde_yaml_ng::from_str("sniffer: {enable: true, skip-domain: ['+.push.apple.com']}")
                .unwrap();
        assert!(required(&plain).is_empty());
    }

    #[test]
    fn databases_follow_rules_dns_policies_and_ip_mode() {
        let config: Value = serde_yaml_ng::from_str("geodata-mode: true\nrules: ['AND,((GEOIP,CN),(GEOSITE,cn)),DIRECT', 'IP-ASN,123,DIRECT']\n").unwrap();
        assert_eq!(
            required(&config)
                .iter()
                .map(|a| a.local)
                .collect::<Vec<_>>(),
            ["GeoSite.dat", "GeoIP.dat", "ASN.mmdb"]
        );
        let config: Value = serde_yaml_ng::from_str(
            "dns: {nameserver-policy: {'geosite:cn': [1.1.1.1]}, fallback: [8.8.8.8]}\n",
        )
        .unwrap();
        assert_eq!(
            required(&config)
                .iter()
                .map(|a| a.local)
                .collect::<Vec<_>>(),
            ["GeoSite.dat", "geoip.metadb"]
        );
        assert!(required(&serde_yaml_ng::from_str("rules: ['MATCH,DIRECT']").unwrap()).is_empty());
        assert!(required(
            &serde_yaml_ng::from_str(
                "rules: ['GEOIP,LAN,DIRECT', 'MATCH,DIRECT']\nproxies: [{name: 'GEOSITE,example'}]"
            )
            .unwrap()
        )
        .is_empty());
    }

    #[test]
    fn first_save_prepares_verified_data_and_reuses_it_offline() {
        let root = std::env::temp_dir().join(format!("u60-geodata-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bytes = b"test database";
        let asset = Asset {
            remote: "geosite.dat",
            local: "GeoSite.dat",
            limit: 1024,
        };
        let sha = format!("{:x}  geosite.dat\n", Sha256::digest(bytes));
        let mut calls = 0;
        prepare_with(&root, &[asset], |url, _| {
            calls += 1;
            Ok(if url.ends_with(".sha256sum") {
                sha.as_bytes().to_vec()
            } else {
                bytes.to_vec()
            })
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(fs::read(root.join(asset.local)).unwrap(), bytes);
        prepare_with(&root, &[asset], |_, _| {
            panic!("cached data must not need the Internet")
        })
        .unwrap();
        fs::write(root.join(asset.local), b"truncated").unwrap();
        prepare_with(&root, &[asset], |url, _| {
            Ok(if url.ends_with(".sha256sum") {
                sha.as_bytes().to_vec()
            } else {
                bytes.to_vec()
            })
        })
        .unwrap();
        assert_eq!(fs::read(root.join(asset.local)).unwrap(), bytes);
        fs::remove_file(root.join(asset.local)).unwrap();
        assert!(prepare_with(&root, &[asset], |url, _| Ok(
            if url.ends_with(".sha256sum") {
                sha.as_bytes().to_vec()
            } else {
                b"corrupt".to_vec()
            }
        ))
        .is_err());
        assert!(!root.join(asset.local).exists());
        // A failed attempt to update unproven legacy data cannot destroy it.
        fs::write(root.join(asset.local), b"previous database").unwrap();
        fs::remove_file(root.join("GeoSite.dat.source.txt")).unwrap();
        assert!(prepare_with(&root, &[asset], |url, _| Ok(
            if url.ends_with(".sha256sum") {
                sha.as_bytes().to_vec()
            } else {
                b"corrupt".to_vec()
            }
        ))
        .is_err());
        assert_eq!(
            fs::read(root.join(asset.local)).unwrap(),
            b"previous database"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn checksum_rejects_mismatched_assets_and_extra_content() {
        let hash = "a".repeat(64);
        assert!(checksum(format!("{hash} *geosite.dat").as_bytes(), "geosite.dat").is_ok());
        assert!(checksum(format!("{hash} geoip.dat").as_bytes(), "geosite.dat").is_err());
        assert!(checksum(format!("{hash} geosite.dat more").as_bytes(), "geosite.dat").is_err());
    }

    #[test]
    fn fallback_uses_official_raw_and_verifies_before_accepting_existing_bytes() {
        let root =
            std::env::temp_dir().join(format!("u60-geodata-fallback-{}", std::process::id()));
        let bytes = b"database from official repository";
        net::write_private(&root.join("GeoSite.dat"), bytes).unwrap();
        let mut calls = Vec::new();
        prepare_with(
            &root,
            &[Asset {
                remote: "geosite.dat",
                local: "GeoSite.dat",
                limit: 1024,
            }],
            |url, _| {
                calls.push(url.to_owned());
                if url.starts_with(BASE) {
                    return Err("Direct HTTPS download failed".into());
                }
                assert_eq!(url, format!("{RAW}/geosite.dat.sha256sum"));
                Ok(format!("{:x}  geosite.dat", Sha256::digest(bytes)).into_bytes())
            },
        )
        .unwrap();
        assert_eq!(calls.len(), 2);
        assert!(root.join("GeoSite.dat.source.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
