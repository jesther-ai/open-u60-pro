//! 热点快照只保留保护需要的字段；客户端身份从不进入状态或日志。
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(in crate::wifi_relay) enum Hotspot {
    Off,
    Unavailable,
    On { fields: BTreeMap<String, String>, ready: bool, clients: Option<usize> },
}
impl Hotspot {
    pub(in crate::wifi_relay) fn parse(status: &str, info: &str, clients: Option<usize>) -> Self {
        let Some((primary, width, center)) = super::parse_channel(info) else { return Self::Unavailable };
        let mut fields = BTreeMap::new();
        let mut switching = false;
        for (key, value) in status.lines().filter_map(|l| l.trim().split_once('=')) {
            if matches!(key, "state" | "freq" | "channel" | "secondary_channel" | "ieee80211n" | "ieee80211ac" | "ieee80211ax" | "ieee80211be")
                || ["vht_oper_", "he_oper_", "eht_oper_"].iter().any(|p| key.starts_with(p) && (key.contains("chwidth") || key.contains("centr_freq"))) {
                fields.insert(key.to_owned(), value.to_owned());
            }
            if matches!(key, "cac_time_left_seconds" | "cac_time_left" | "cac_started" | "csa_in_progress" | "channel_switch_in_progress" | "chan_switch" | "csa_active") {
                // B31 在没有 CAC 倒计时时返回 N/A；只对剩余时间字段接受此值。
                let inactive_cac = matches!(key, "cac_time_left_seconds" | "cac_time_left") && value == "N/A";
                switching |= !inactive_cac && value.parse::<u64>() != Ok(0);
            }
        }
        let Some(state) = fields.get("state") else { return Self::Unavailable };
        let ready = state == "ENABLED" && !switching;
        fields.insert("iw_frequency".into(), primary.to_string());
        fields.insert("iw_width".into(), width.to_string());
        fields.insert("iw_center1".into(), center.to_string());
        if let Some(value) = info.split_once("center2:").and_then(|(_, s)| s.split_whitespace().next()) {
            fields.insert("iw_center2".into(), value.to_owned());
        }
        // CAC/CSA 只比较是否进行中，不比较会自然递减的剩余秒数。
        fields.insert("switching".into(), switching.to_string());
        Self::On { fields, ready, clients }
    }
    pub(super) fn ready(&self) -> bool { matches!(self, Self::Off | Self::On { ready: true, .. }) }
    pub(super) fn clients(&self) -> Option<usize> {
        match self { Self::On { clients, .. } => *clients, _ => None }
    }
    pub(super) fn changes(&self, after: &Self) -> Vec<String> {
        match (self, after) {
            (Self::Off, Self::Off) => Vec::new(),
            (Self::On { fields: a, .. }, Self::On { fields: b, .. }) => {
                let mut keys: Vec<_> = a.keys().chain(b.keys()).filter(|k| a.get(*k) != b.get(*k)).cloned().collect();
                keys.sort();
                keys.dedup();
                keys
            }
            _ => vec!["availability".into()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const INFO: &str = "channel 44 (5220 MHz), width: 80 MHz, center1: 5210 MHz";
    #[test]
    fn state_cac_csa_and_missing_fields_block_a_challenge() {
        for status in ["state=DISABLED", "state=DFS", "state=ENABLED\ncac_time_left_seconds=12", "state=ENABLED\ncsa_in_progress=1", "state=ENABLED\nchan_switch=unknown"] {
            assert!(!Hotspot::parse(status, INFO, None).ready());
        }
        assert!(!Hotspot::parse("freq=5220", INFO, None).ready());
        assert!(!Hotspot::parse("state=ENABLED", "missing", None).ready());
        assert!(Hotspot::parse("state=ENABLED\ncac_time_left_seconds=0", INFO, None).ready());
        assert!(Hotspot::Off.ready());
    }
    #[test]
    fn b31_inactive_cac_is_reported_as_not_applicable() {
        assert!(Hotspot::parse("state=ENABLED\ncac_time_left_seconds=N/A", INFO, Some(2)).ready());
        assert!(!Hotspot::parse("state=DFS\ncac_time_left_seconds=N/A", INFO, Some(2)).ready());
        assert!(!Hotspot::parse("state=ENABLED\ncac_time_left_seconds=N/A\ncsa_in_progress=1", INFO, Some(2)).ready());
    }
    #[test]
    fn only_protected_fields_not_clients_or_identifiers_are_compared() {
        let before = Hotspot::parse("state=ENABLED\nfreq=5220\nhe_oper_chwidth=1\nssid=secret", INFO, Some(1));
        let after = Hotspot::parse("state=ENABLED\nfreq=5220\nhe_oper_chwidth=1\nssid=another", INFO, Some(3));
        assert!(before.changes(&after).is_empty());
        assert_eq!(before.clients(), Some(1));
        assert!(!format!("{before:?}").contains("secret"));
        for (key, value) in [("state", "DISABLED"), ("freq", "5180"), ("channel", "36"), ("he_oper_chwidth", "2"), ("vht_oper_centr_freq_seg0_idx", "50")] {
            let changed = Hotspot::parse(&format!("state=ENABLED\nfreq=5220\nhe_oper_chwidth=1\n{key}={value}"), INFO, None);
            assert!(before.changes(&changed).contains(&key.to_owned()));
        }
        for changed in [INFO.replace("80 MHz", "40 MHz"), INFO.replace("5210", "5230"), INFO.replace("5220", "5180")] {
            assert!(!before.changes(&Hotspot::parse("state=ENABLED", &changed, None)).is_empty());
        }
    }
}
