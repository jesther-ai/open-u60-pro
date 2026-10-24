//! The radio model of the signal page, read from one `network/signal`
//! reading: the 5G NR, LTE and WCDMA panels, their carrier aggregation
//! lists and the data session. A port of the iOS app's `SignalParser` and
//! panel rules (`SignalModels.swift`, `SignalMonitorView.swift`).

use serde_json::Value;

use super::{bands, fmt};

/// A number from a JSON number or string; "", "--", "N/A" and the
/// firmware's ±9000+ placeholders are none.
pub fn reading(v: &Value) -> Option<f64> {
    fmt::float(v).filter(|f| f.abs() <= 9000.0)
}

/// A JSON string or number as text (the firmware sends both).
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// RSRP, RSRQ, SINR, RSSI.
pub type Metrics = [Option<f64>; 4];

/// One entry of `lteca` / `nrca` ("PCI,Band,Index,Channel,BW") with the
/// matching `ltecasig` / `nrcasig` entry ("RSRP,RSRQ,SINR,RSSI,…").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Carrier {
    pub pci: String,
    pub band: String,
    pub channel: String,
    pub bw: String,
    pub sig: Metrics,
}

/// The 5G NR or LTE panel.
#[derive(Debug, Default, PartialEq)]
pub struct Radio {
    pub nr: bool,
    pub sig: Metrics,
    pub band: String,
    pub pci: String,
    pub cell: String,
    pub channel: String,
    pub bw: String,
    /// `lteca_state` (LTE only).
    pub ca_state: String,
    /// Secondary carriers, in the firmware's order (signal entry `i` belongs
    /// to secondary carrier `i`).
    pub sccs: Vec<Carrier>,
    /// All carriers from the list, the primary included, for the count.
    all: Vec<Carrier>,
}

/// The carrier list, without placeholder rows (no band, or PCI and channel
/// both 0, such as "0,0,0,0,0").
fn carriers(list: &str) -> Vec<Carrier> {
    list.split(';')
        .filter_map(|entry| {
            let p: Vec<&str> = entry.split(',').map(str::trim).collect();
            let zero = |s: &str| s.parse::<u32>().ok() == Some(0);
            let real = p.len() >= 5 && bands::number(p[1]).is_some() && !(zero(p[0]) && zero(p[3]));
            real.then(|| Carrier {
                pci: p[0].into(),
                band: p[1].into(),
                channel: p[3].into(),
                bw: p[4].into(),
                sig: [None; 4],
            })
        })
        .collect()
}

fn signals(sigs: &str) -> Vec<Metrics> {
    sigs.split(';')
        .filter_map(|entry| {
            let p: Vec<&str> = entry.split(',').collect();
            (p.len() >= 4).then(|| [0, 1, 2, 3].map(|i| p[i].trim().parse().ok().filter(|f: &f64| f.is_finite())))
        })
        .collect()
}

impl Radio {
    pub fn parse(d: &Value, nr: bool) -> Radio {
        let k = |nr_key: &str, lte_key: &str| &d[if nr { nr_key } else { lte_key }];
        let mut r = Radio {
            nr,
            sig: [
                fmt::rsrp(d, if nr { "nr5g_rsrp" } else { "lte_rsrp" }),
                reading(k("nr5g_rsrq", "lte_rsrq")),
                reading(k("nr5g_snr", "lte_snr")),
                reading(k("nr5g_rssi", "lte_rssi")),
            ],
            band: text(k("nr5g_action_band", "wan_active_band")),
            pci: text(k("nr5g_pci", "lte_pci")),
            cell: text(k("nr5g_cell_id", "cell_id")),
            channel: text(k("nr5g_action_channel", "wan_active_channel")),
            bw: if nr { text(&d["nr5g_bandwidth"]) } else { String::new() },
            ca_state: if nr { String::new() } else { text(&d["lteca_state"]) },
            ..Radio::default()
        };
        let list = carriers(&text(k("nrca", "lteca")));
        let sigs = signals(&text(k("nrcasig", "ltecasig")));
        let mut primary = false;
        for c in &list {
            if !primary && !r.pci.is_empty() && c.pci == r.pci && c.channel == r.channel {
                primary = true;
                if !c.bw.is_empty() {
                    r.bw = c.bw.clone();
                }
            } else {
                let mut c = c.clone();
                c.sig = sigs.get(r.sccs.len()).copied().unwrap_or([None; 4]);
                r.sccs.push(c);
            }
        }
        r.all = list;
        r
    }

    pub fn connected(&self) -> bool {
        self.sig[0].is_some()
    }

    /// Something to show: the primary or any secondary carrier has an RSRP.
    pub fn has_signal(&self) -> bool {
        self.connected() || self.sccs.iter().any(|c| c.sig[0].is_some())
    }

    /// Secondary carriers, more than one distinct carrier (a repeated
    /// primary row is not CA), and for LTE a CA state other than "0".
    pub fn ca_active(&self) -> bool {
        !self.sccs.is_empty() && (self.nr || self.ca_state != "0") && self.cc_count() > 1
    }

    /// Component carriers: distinct (PCI, band, channel) over the list and
    /// the primary (the firmware can repeat rows), at least 1.
    pub fn cc_count(&self) -> usize {
        let max_pci = if self.nr { 1007 } else { 503 };
        let key = |pci: &str, band: &str, ch: &str| {
            let pci = pci.parse::<u32>().ok().filter(|p| *p <= max_pci)?;
            Some((pci, bands::number(band)?, ch.parse::<u32>().ok()?))
        };
        let mut seen: Vec<(u32, u32, u32)> = Vec::new();
        let primary = key(&self.pci, &self.band, &self.channel);
        for k in self.all.iter().map(|c| key(&c.pci, &c.band, &c.channel)).chain([primary]).flatten() {
            if !seen.contains(&k) {
                seen.push(k);
            }
        }
        seen.len().max(1)
    }

    /// NR: the primary's bandwidth plus every secondary's, in MHz.
    pub fn total_bw(&self) -> Option<u32> {
        let sccs: Vec<u32> = self.sccs.iter().filter_map(|c| bands::mhz(&c.bw)).collect();
        match bands::mhz(&self.bw) {
            Some(p) => Some(sccs.iter().fold(p, |a, b| a.saturating_add(*b))),
            None if sccs.is_empty() => None,
            None => Some(sccs.iter().fold(0, |a: u32, b| a.saturating_add(*b))),
        }
    }
}

/// Which panels the page shows (the app's rules): NR with any NR reading;
/// LTE unless the mode is SA, with LTE readings and a network type that
/// fits; WCDMA only when neither is shown.
pub struct Panels {
    pub nr: bool,
    pub lte: bool,
    pub wcdma: bool,
}

pub fn panels(d: &Value, nr: &Radio, lte: &Radio) -> Panels {
    let raw = d["network_type"].as_str().unwrap_or("").trim().to_ascii_uppercase();
    let hints = raw.is_empty()
        || raw == "4G"
        || raw == "4G+"
        || ["LTE", "E-UTRAN", "ENDC", "EN-DC", "SA", "NR", "5G"].iter().any(|h| raw.contains(h));
    let show_nr = nr.has_signal();
    let show_lte = !fmt::is_sa(d) && lte.has_signal() && hints;
    let (rscp, ecio) = wcdma(d);
    Panels { nr: show_nr, lte: show_lte, wcdma: !show_nr && !show_lte && (rscp.is_some() || ecio.is_some()) }
}

/// The network type the app shows on its cards (iOS `displayNetworkType`):
/// "5G NSA" when NR is connected (a plausible NR RSRP, the reading the
/// card's "NR RSRP" shows) while the firmware says LTE or something it has
/// no name for; "4G" / "4G+" (LTE secondaries) when it still says SA or
/// NSA but NR has dropped; LTE as "4G", or "4G+" for CA / LTE-A / LTE+;
/// anything else as the firmware says it ("NO_SERVICE"). Unlike the app,
/// the firmware's "NR5G-SA" spelling is SA (the app's `SignalActivityMode`
/// knows it; its card does not).
pub fn display_type(d: &Value) -> String {
    #[derive(PartialEq)]
    enum M {
        Sa,
        Nsa,
        Lte,
        Legacy,
        Unknown,
    }
    let raw = d["network_type"].as_str().unwrap_or("").trim();
    let up = raw.to_ascii_uppercase();
    let has = |words: &[&str]| words.iter().any(|w| up.contains(w));
    let mode = if up == "SA" || has(&["5G SA", "NR SA", "NR-SA", "5G-SA", "5G_SA"]) {
        M::Sa
    } else if up.contains("NSA") || up == "ENDC" || up == "EN-DC" {
        M::Nsa
    } else if up.contains("LTE") || up == "4G" || up == "4G+" {
        M::Lte
    } else if has(&["WCDMA", "UMTS", "GSM", "2G", "3G"]) {
        M::Legacy
    } else {
        M::Unknown
    };
    let nr = Radio::parse(d, true).connected();
    let label = match mode {
        M::Lte | M::Unknown if nr => "5G NSA",
        M::Sa | M::Nsa if !nr => {
            let lte = Radio::parse(d, false);
            if lte.connected() && !lte.sccs.is_empty() {
                "4G+"
            } else {
                "4G"
            }
        }
        M::Sa => "5G SA",
        M::Nsa => "5G NSA",
        M::Lte if has(&["CA", "LTE-A", "LTE+"]) || up == "4G+" => "4G+",
        M::Lte => "4G",
        M::Legacy | M::Unknown => raw,
    };
    label.to_owned()
}

/// RSCP (0 is none) and Ec/Io.
pub fn wcdma(d: &Value) -> (Option<f64>, Option<f64>) {
    (reading(&d["rscp"]).filter(|v| *v != 0.0), reading(&d["ecio"]))
}

/// The data session: QCI/5QI and the AMBR (down, up), each only when > 0.
/// `None` while there is no data session.
pub fn bearer(d: &Value) -> Option<(Option<i64>, Option<f64>, Option<f64>)> {
    let b = &d["bearer"];
    let pos = |v: &Value| fmt::float(v).filter(|f| *f > 0.0);
    let out = (pos(&b["qci"]).map(|q| q as i64), pos(&b["ambr_dl_mbps"]), pos(&b["ambr_ul_mbps"]));
    (b.is_object() && out != (None, None, None)).then_some(out)
}

/// Mbps without a decimal point when whole, else up to 3 decimals.
pub fn mbps(v: f64) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r == r.trunc() && r.abs() < 1e15 {
        format!("{}", r as i64)
    } else {
        format!("{r:.3}").trim_end_matches('0').to_owned()
    }
}

/// Colour classes of the metric grid (the app's thresholds).
pub fn rsrq_class(v: f64) -> &'static str {
    tri(v, -15.0, -20.0)
}

pub fn sinr_class(v: f64) -> &'static str {
    tri(v, 10.0, 0.0)
}

pub fn rscp_class(v: f64) -> &'static str {
    tri(v, -85.0, -95.0)
}

pub fn ecio_class(v: f64) -> &'static str {
    tri(v, -10.0, -15.0)
}

/// RSCP quality label (-75 / -85 / -95).
pub fn rscp_quality(v: f64) -> &'static str {
    if v >= -75.0 {
        "Excellent"
    } else if v >= -85.0 {
        "Good"
    } else if v >= -95.0 {
        "Fair"
    } else {
        "Poor"
    }
}

fn tri(v: f64, good: f64, fair: f64) -> &'static str {
    if v >= good {
        "q-good"
    } else if v >= fair {
        "q-fair"
    } else {
        "q-poor"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nsa() -> Value {
        json!({
            "network_type": "ENDC", "network_provider": "Example Mobile",
            "nr5g_rsrp": "-77", "nr5g_rsrq": "-11", "nr5g_snr": "11", "nr5g_rssi": "-68",
            "nr5g_action_band": "n41", "nr5g_pci": "774", "nr5g_cell_id": "1234567890",
            "nr5g_action_channel": "513000", "nr5g_bandwidth": "40",
            "nrca": "774,41,0,513000,50;", "nrcasig": "",
            "lte_rsrp": -85, "lte_rsrq": "-12", "lte_snr": "5", "lte_rssi": "-56",
            "wan_active_band": "LTE BAND 1", "lte_pci": "178", "cell_id": "12345678",
            "wan_active_channel": "301", "lteca_state": "1",
            "lteca": "178,1,0,301,10;;12,3,1,1300,20;12,3,1,1300,20;9,7,2,3000,15,extra;1,2,3,4",
            "ltecasig": "-90,-10,8,-60;-95.5,-13,2,-62,7;bad;-101,-14,1",
            "rscp": "0", "ecio": "", "bearer": {"qci": 9, "ambr_dl_mbps": 10000, "ambr_ul_mbps": "8.6400"},
        })
    }

    #[test]
    fn ca_lists_split_the_primary_and_align_signals() {
        let d = nsa();
        let lte = Radio::parse(&d, false);
        // The 4-field entry is skipped, empty entries too; the primary
        // (PCI 178 on 301) is taken out and its BW used.
        assert_eq!(lte.bw, "10");
        assert_eq!(lte.sccs.len(), 3);
        assert_eq!((lte.sccs[0].pci.as_str(), lte.sccs[0].band.as_str(), lte.sccs[0].bw.as_str()), ("12", "3", "20"));
        assert_eq!(lte.sccs[0].sig, [Some(-90.0), Some(-10.0), Some(8.0), Some(-60.0)]);
        assert_eq!(lte.sccs[1].sig[0], Some(-95.5));
        // "bad" and the 3-field entry are not signals: the third SCC has none.
        assert_eq!(lte.sccs[2].sig, [None; 4]);
        // The repeated row counts once: 178/1, 12/3, 9/7.
        assert_eq!(lte.cc_count(), 3);
        assert!(lte.ca_active());
        assert_eq!(lte.band, "LTE BAND 1");
        let nr = Radio::parse(&d, true);
        assert_eq!((nr.bw.as_str(), nr.sccs.len(), nr.cc_count()), ("50", 0, 1));
        assert!(!nr.ca_active());
        assert_eq!(nr.sig, [Some(-77.0), Some(-11.0), Some(11.0), Some(-68.0)]);
        // LTE CA state 0: inactive even with secondaries.
        let mut off = nsa();
        off["lteca_state"] = json!("0");
        assert!(!Radio::parse(&off, false).ca_active());
        // Placeholder rows are no carriers; a repeated primary row is no CA.
        let mut d = nsa();
        d["nrca"] = json!("774,41,0,513000,50;0,0,0,0,0;0,41,0,0,20;774,41,0,513000,50");
        let nr = Radio::parse(&d, true);
        assert_eq!((nr.sccs.len(), nr.cc_count()), (1, 1));
        assert!(!nr.ca_active());
    }

    #[test]
    fn nr_total_bandwidth() {
        let d = json!({"nr5g_pci": "1", "nr5g_action_channel": "10", "nr5g_bandwidth": "100",
                       "nrca": "1,78,0,10,100;2,78,1,20,40MHz;3,41,2,30,"});
        let nr = Radio::parse(&d, true);
        assert_eq!(nr.total_bw(), Some(140));
        assert!(nr.ca_active());
        assert_eq!(nr.cc_count(), 3);
        // Nonsense from the firmware does not overflow.
        let d = json!({"nr5g_bandwidth": "4294967295", "nrca": "1,78,0,10,4294967295;2,78,1,20,100"});
        assert_eq!(Radio::parse(&d, true).total_bw(), Some(u32::MAX));
    }

    #[test]
    fn panel_visibility() {
        let d = nsa();
        let (nr, lte) = (Radio::parse(&d, true), Radio::parse(&d, false));
        let p = panels(&d, &nr, &lte);
        assert!(p.nr && p.lte && !p.wcdma);
        // SA hides LTE, also spelled NR5G-SA.
        for t in ["SA", "NR5G-SA", "NR5G_SA"] {
            let mut sa = nsa();
            sa["network_type"] = json!(t);
            let p = panels(&sa, &Radio::parse(&sa, true), &Radio::parse(&sa, false));
            assert!(p.nr && !p.lte, "{t}");
        }
        // WCDMA only when neither is shown.
        let w = json!({"network_type": "WCDMA", "rscp": "-80", "ecio": "-7", "lte_rsrp": "-90"});
        let p = panels(&w, &Radio::parse(&w, true), &Radio::parse(&w, false));
        assert!(!p.nr && !p.lte && p.wcdma);
        assert_eq!(wcdma(&w), (Some(-80.0), Some(-7.0)));
        // No service: nothing.
        let n = json!({"network_type": "NO_SERVICE", "rscp": 0});
        let p = panels(&n, &Radio::parse(&n, true), &Radio::parse(&n, false));
        assert!(!p.nr && !p.lte && !p.wcdma);
    }

    #[test]
    fn display_type_follows_the_app() {
        let t = |v: Value| display_type(&v);
        assert_eq!(t(json!({"network_type": "NO_SERVICE"})), "NO_SERVICE");
        assert_eq!(t(json!({"network_type": "LIMITED_SERVICE_LTE", "lte_rsrp": "-100"})), "4G");
        assert_eq!(t(json!({"network_type": "LIMITED_SERVICE_SA"})), "LIMITED_SERVICE_SA");
        assert_eq!(t(json!({"network_type": "WCDMA", "rscp": "-80"})), "WCDMA");
        assert_eq!(t(json!({"network_type": ""})), "");
        // ENDC without an NR reading: 4G, or 4G+ with LTE secondaries.
        assert_eq!(t(json!({"network_type": "ENDC", "lte_rsrp": "-90", "nr5g_rsrp": "0"})), "4G");
        let mut endc = nsa();
        endc["nr5g_rsrp"] = json!("--");
        assert_eq!(t(endc), "4G+");
        assert_eq!(t(nsa()), "5G NSA");
        // LTE (or a type without a name) with an NR reading: 5G NSA.
        assert_eq!(t(json!({"network_type": "LTE", "nr5g_rsrp": "-95"})), "5G NSA");
        assert_eq!(t(json!({"network_type": "NO_SERVICE", "nr5g_rsrp": "-95"})), "5G NSA");
        assert_eq!(t(json!({"network_type": "LTE", "lte_rsrp": "-90"})), "4G");
        assert_eq!(t(json!({"network_type": "LTE-CA"})), "4G+");
        assert_eq!(t(json!({"network_type": "LTE_CA"})), "4G+");
        assert_eq!(t(json!({"network_type": "lte+"})), "4G+");
        assert_eq!(t(json!({"network_type": "4G+"})), "4G+");
        for sa in ["SA", "NR5G-SA", "NR5G_SA"] {
            assert_eq!(t(json!({"network_type": sa, "nr5g_rsrp": "-80"})), "5G SA", "{sa}");
            assert_eq!(t(json!({"network_type": sa})), "4G", "{sa} without NR");
        }
    }

    #[test]
    fn readings_and_bearer() {
        assert_eq!(reading(&json!("--")), None);
        assert_eq!(reading(&json!("N/A")), None);
        assert_eq!(reading(&json!(-9999)), None);
        assert_eq!(reading(&json!(" -12.5 ")), Some(-12.5));
        assert_eq!(bearer(&nsa()), Some((Some(9), Some(10000.0), Some(8.64))));
        assert_eq!(bearer(&json!({"bearer": null})), None);
        assert_eq!(bearer(&json!({"bearer": {"qci": 0}})), None);
        assert_eq!(mbps(10000.0), "10000");
        assert_eq!(mbps(8.64), "8.64");
        assert_eq!(mbps(0.5), "0.5");
        assert_eq!(mbps(1.23456), "1.235");
        assert_eq!((rsrq_class(-15.0), rsrq_class(-20.0), rsrq_class(-20.5)), ("q-good", "q-fair", "q-poor"));
        assert_eq!((sinr_class(10.0), sinr_class(0.0), sinr_class(-1.0)), ("q-good", "q-fair", "q-poor"));
        assert_eq!((rscp_quality(-75.0), rscp_quality(-85.0), rscp_quality(-95.0), rscp_quality(-96.0)), ("Excellent", "Good", "Fair", "Poor"));
    }
}
