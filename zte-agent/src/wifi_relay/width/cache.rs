//! 2026-10-28：带宽历史只提供起点或候选排序参考，不替代本次关联的测量。
//! 写入时机由 Controller 决定；这里不读取设备、不校时，也不启动后台任务。

use std::path::Path;

use serde::{Deserialize, Serialize};

const LIMIT: usize = 32;
const MAX_AGE: u64 = 30 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct History {
    pub width: u32,
    /// false 时只供候选排序参考，不能作为起始带宽。
    pub usable: bool,
}

#[derive(Deserialize, Serialize)]
struct Entry {
    id: String,
    width: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal_dbm: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved_utc: Option<u64>,
}

fn legal_width(width: u32) -> bool {
    matches!(width, 20 | 40 | 80 | 160)
}

impl Entry {
    fn valid(&self) -> bool {
        legal_width(self.width) && self.source.as_deref() == Some("airtime") && self.signal_dbm.is_some()
    }
}

fn read(path: &Path) -> Vec<Entry> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// utc 必须来自本次启动的已验证网络时钟；缺失时不推断跨重启的记录年龄。
pub(crate) fn remembered(
    path: &Path,
    id: &str,
    signal_dbm: i32,
    utc: Option<u64>,
) -> Option<History> {
    let entry = read(path)
        .into_iter()
        .find(|entry| entry.id == id && entry.valid())?;
    let signal_changed = entry
        .signal_dbm
        .is_some_and(|saved| saved.abs_diff(signal_dbm) > 10);
    let expired = utc
        .zip(entry.saved_utc)
        .is_some_and(|(now, saved)| now.saturating_sub(saved) > MAX_AGE);
    Some(History {
        width: entry.width,
        usable: !signal_changed && !expired,
    })
}

/// 只写 airtime 记录；最新优先、按网络替换，最多 32 条。不是 airtime 记录的条目随重写丢弃。
pub(crate) fn remember(
    path: &Path,
    id: &str,
    width: u32,
    signal_dbm: i32,
    utc: Option<u64>,
) -> Result<(), String> {
    if !legal_width(width) {
        return Err("Invalid upstream width".into());
    }
    let mut entries = read(path);
    entries.retain(|entry| entry.id != id && entry.valid());
    entries.insert(
        0,
        Entry {
            id: id.to_owned(),
            width,
            source: Some("airtime".into()),
            signal_dbm: Some(signal_dbm),
            saved_utc: utc,
        },
    );
    entries.truncate(LIMIT);
    crate::net_tools::write_private(
        path,
        &serde_json::to_vec(&entries).map_err(|error| error.to_string())?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct File(PathBuf);

    impl File {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "u60-width-cache-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir.join("widths.json"))
        }

        fn entries(&self) -> Value {
            serde_json::from_slice(&fs::read(&self.0).unwrap()).unwrap()
        }
    }

    impl Drop for File {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn missing_or_damaged_history_is_empty_and_can_be_replaced() {
        let file = File::new();
        assert_eq!(remembered(&file.0, "one", -50, None), None);
        remember(&file.0, "one", 20, -50, None).unwrap();
        for damaged in [
            "not json",
            "[",
            "null",
            "{}",
            r#"[{"id":"one","width":"20"}]"#,
        ] {
            fs::write(&file.0, damaged).unwrap();
            assert_eq!(remembered(&file.0, "one", -50, None), None);
            remember(&file.0, "one", 40, -50, None).unwrap();
            assert_eq!(
                remembered(&file.0, "one", -50, None),
                Some(History {
                    width: 40,
                    usable: true
                })
            );
        }
        assert_eq!(remembered(&file.0, "missing", -50, None), None);
    }

    #[test]
    fn airtime_entries_with_and_without_a_verified_utc_can_be_read_together() {
        let file = File::new();
        fs::write(
            &file.0,
            br#"[
            {"id":"airtime80","width":80,"source":"airtime","signal_dbm":-50},
            {"id":"airtime160","width":160,"source":"airtime","signal_dbm":-50,"saved_utc":100}
        ]"#,
        )
        .unwrap();
        for (id, width) in [("airtime80", 80), ("airtime160", 160)] {
            assert_eq!(
                remembered(&file.0, id, -50, Some(100)),
                Some(History {
                    width,
                    usable: true
                })
            );
        }
    }

    #[test]
    fn new_records_have_airtime_signal_and_only_optional_verified_utc() {
        let file = File::new();
        remember(&file.0, "one", 20, -50, None).unwrap();
        assert_eq!(
            file.entries(),
            json!([
                {"id":"one","width":20,"source":"airtime","signal_dbm":-50}
            ])
        );
        remember(&file.0, "one", 40, -55, Some(123)).unwrap();
        assert_eq!(
            file.entries(),
            json!([
                {"id":"one","width":40,"source":"airtime","signal_dbm":-55,"saved_utc":123}
            ])
        );
    }

    #[test]
    fn rewrites_keep_airtime_records_and_drop_everything_else() {
        let file = File::new();
        fs::write(
            &file.0,
            br#"[
            {"id":"old-format","width":40},
            {"id":"phy","width":80,"source":"phy","signal_dbm":-50},
            {"id":"bad-width","width":320},
            {"id":"kept","width":160,"source":"airtime","signal_dbm":-45}
        ]"#,
        )
        .unwrap();
        remember(&file.0, "new", 20, -50, None).unwrap();
        assert_eq!(
            file.entries(),
            json!([
                {"id":"new","width":20,"source":"airtime","signal_dbm":-50},
                {"id":"kept","width":160,"source":"airtime","signal_dbm":-45}
            ])
        );
    }

    #[test]
    fn illegal_widths_and_non_airtime_sources_are_not_history() {
        let file = File::new();
        for width in [0, 10, 30, 320, u32::MAX] {
            fs::write(&file.0, json!([{"id":"one","width":width}]).to_string()).unwrap();
            assert_eq!(remembered(&file.0, "one", -50, None), None);
        }
        for source in ["phy", "", "AIRTIME", "unknown"] {
            fs::write(
                &file.0,
                json!([{"id":"one","width":20,"source":source,"signal_dbm":-50}]).to_string(),
            )
            .unwrap();
            assert_eq!(remembered(&file.0, "one", -50, None), None);
        }
        fs::write(&file.0, br#"[{"id":"one","width":20,"source":"airtime"}]"#).unwrap();
        assert_eq!(remembered(&file.0, "one", -50, None), None);
        fs::write(&file.0, br#"[{"id":"one","width":20}]"#).unwrap();
        assert_eq!(remembered(&file.0, "one", -50, None), None, "a record without a source is not a measurement");
    }

    #[test]
    fn invalid_writes_leave_existing_history_unchanged() {
        let file = File::new();
        remember(&file.0, "one", 20, -50, None).unwrap();
        let before = fs::read(&file.0).unwrap();
        for width in [0, 10, 30, 320, u32::MAX] {
            assert!(remember(&file.0, "one", width, -60, Some(100)).is_err());
            assert_eq!(fs::read(&file.0).unwrap(), before);
        }
    }

    #[test]
    fn signal_changes_over_ten_db_keep_only_a_sorting_hint() {
        let file = File::new();
        remember(&file.0, "one", 40, -50, None).unwrap();
        for (signal, usable) in [(-60, true), (-40, true), (-61, false), (-39, false)] {
            assert_eq!(
                remembered(&file.0, "one", signal, None),
                Some(History { width: 40, usable })
            );
        }
        remember(&file.0, "one", 40, i32::MIN, None).unwrap();
        assert_eq!(
            remembered(&file.0, "one", i32::MAX, None),
            Some(History {
                width: 40,
                usable: false
            })
        );
    }

    #[test]
    fn age_needs_verified_utc_and_expires_only_after_thirty_days() {
        let file = File::new();
        let saved = 1_800_000_000;
        remember(&file.0, "one", 80, -50, Some(saved)).unwrap();
        for (utc, usable) in [
            (None, true),
            (Some(saved - 1), true),
            (Some(saved), true),
            (Some(saved + MAX_AGE), true),
            (Some(saved + MAX_AGE + 1), false),
            (Some(u64::MAX), false),
        ] {
            assert_eq!(
                remembered(&file.0, "one", -50, utc),
                Some(History { width: 80, usable })
            );
        }
        remember(&file.0, "untimed", 20, -50, None).unwrap();
        assert_eq!(
            remembered(&file.0, "untimed", -50, Some(u64::MAX)),
            Some(History {
                width: 20,
                usable: true
            })
        );
        assert_eq!(
            remembered(&file.0, "one", -61, Some(saved + MAX_AGE + 1)),
            Some(History {
                width: 80,
                usable: false
            })
        );
    }

    #[test]
    fn writes_create_private_files_and_repair_existing_permissions() {
        let file = File::new();
        for existing in [false, true] {
            if existing {
                fs::set_permissions(&file.0, fs::Permissions::from_mode(0o644)).unwrap();
                fs::write(file.0.with_extension("new"), b"unfinished").unwrap();
                fs::set_permissions(
                    file.0.with_extension("new"),
                    fs::Permissions::from_mode(0o666),
                )
                .unwrap();
            }
            remember(&file.0, "one", 20, -50, None).unwrap();
            assert_eq!(
                fs::metadata(&file.0).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(file.0.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert!(!file.0.with_extension("new").exists());
        }
    }

    #[test]
    fn newest_networks_come_first_and_only_thirty_two_are_kept() {
        let file = File::new();
        for index in 0..40 {
            remember(&file.0, &format!("n{index}"), 80, -50, None).unwrap();
        }
        let entries = file.entries();
        let entries = entries.as_array().unwrap();
        assert_eq!(entries.len(), LIMIT);
        for (entry, index) in entries.iter().zip((8..40).rev()) {
            assert_eq!(entry["id"], format!("n{index}"));
        }
        assert_eq!(remembered(&file.0, "n7", -50, None), None);
        remember(&file.0, "n8", 20, -60, Some(123)).unwrap();
        let entries = file.entries();
        let entries = entries.as_array().unwrap();
        assert_eq!(entries.len(), LIMIT);
        assert_eq!(
            entries[0],
            json!({"id":"n8","width":20,"source":"airtime","signal_dbm":-60,"saved_utc":123})
        );
        assert_eq!(entries[1]["id"], "n39");
        assert_eq!(
            entries.iter().filter(|entry| entry["id"] == "n8").count(),
            1
        );
    }

    #[test]
    fn newest_matching_entry_wins_and_replacement_removes_duplicates() {
        let file = File::new();
        fs::write(
            &file.0,
            br#"[{"id":"one","width":20,"source":"airtime","signal_dbm":-50},{"id":"one","width":40,"source":"airtime","signal_dbm":-50}]"#,
        )
        .unwrap();
        assert_eq!(
            remembered(&file.0, "one", -50, None),
            Some(History {
                width: 20,
                usable: true
            })
        );
        remember(&file.0, "one", 80, -50, None).unwrap();
        assert_eq!(file.entries().as_array().unwrap().len(), 1);
        assert_eq!(
            remembered(&file.0, "one", -50, None),
            Some(History {
                width: 80,
                usable: true
            })
        );
    }

    #[test]
    fn write_errors_are_returned_to_the_controller() {
        let file = File::new();
        fs::write(&file.0, b"not a directory").unwrap();
        assert!(remember(&file.0.join("widths.json"), "one", 20, -50, None).is_err());
        assert_eq!(fs::read(&file.0).unwrap(), b"not a directory");
    }
}
