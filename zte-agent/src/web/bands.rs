//! LTE and NR band facts for the signal page (3GPP TS 36.101 / 38.101), the
//! same table as the iOS app's `BandSpecDatabase.swift`, and the band and
//! bandwidth strings of its `SignalFormat`.
//!
//! Each entry is 8 bytes of plain numbers (no string pointers to relocate);
//! the common name is rebuilt from the frequency and a tag.

/// One band: number, frequency (MHz; GHz above 10000; AWS: the AWS number),
/// widest channel (MHz) and `kind` = duplex | name tag.
#[derive(Copy, Clone)]
struct Spec {
    band: u16,
    mhz: u16,
    max_bw: u16,
    kind: u8,
}

const FDD: u8 = 0;
const TDD: u8 = 1;
const SDL: u8 = 2;
const DUPLEX: u8 = 3;
/// Name tags (`kind` bits 2-4).
const PS: u8 = 1 << 2;
const LAA: u8 = 2 << 2;
const CBRS: u8 = 3 << 2;
const AWS: u8 = 4 << 2;
const TAG: u8 = 7 << 2;

const fn b(band: u16, mhz: u16, max_bw: u16, kind: u8) -> Spec {
    Spec { band, mhz, max_bw, kind }
}

const LTE: [Spec; 44] = [
    b(1, 2100, 20, FDD),
    b(2, 1900, 20, FDD),
    b(3, 1800, 20, FDD),
    b(4, 1, 20, FDD | AWS),
    b(5, 850, 10, FDD),
    b(7, 2600, 20, FDD),
    b(8, 900, 10, FDD),
    b(11, 1500, 10, FDD),
    b(12, 700, 10, FDD),
    b(13, 700, 10, FDD),
    b(14, 700, 10, FDD | PS),
    b(17, 700, 10, FDD),
    b(18, 850, 15, FDD),
    b(19, 850, 15, FDD),
    b(20, 800, 20, FDD),
    b(21, 1500, 15, FDD),
    b(25, 1900, 20, FDD),
    b(26, 850, 15, FDD),
    b(28, 700, 20, FDD),
    b(29, 700, 10, SDL),
    b(30, 2300, 10, FDD),
    b(31, 450, 5, FDD),
    b(32, 1500, 20, SDL),
    b(34, 2000, 15, TDD),
    b(38, 2600, 20, TDD),
    b(39, 1900, 20, TDD),
    b(40, 2300, 20, TDD),
    b(41, 2500, 20, TDD),
    b(42, 3500, 20, TDD),
    b(43, 3700, 20, TDD),
    b(46, 5200, 20, TDD | LAA),
    b(48, 3500, 20, TDD | CBRS),
    b(50, 1500, 20, TDD),
    b(51, 1500, 5, TDD),
    b(53, 2400, 10, TDD),
    b(65, 2100, 20, FDD),
    b(66, 3, 20, FDD | AWS),
    b(70, 4, 15, FDD | AWS),
    b(71, 600, 20, FDD),
    b(72, 450, 5, FDD),
    b(73, 450, 5, FDD),
    b(74, 1500, 20, FDD),
    b(75, 1500, 20, SDL),
    b(85, 700, 10, FDD),
];

const NR: [Spec; 38] = [
    b(1, 2100, 50, FDD),
    b(2, 1900, 20, FDD),
    b(3, 1800, 50, FDD),
    b(5, 850, 20, FDD),
    b(7, 2600, 50, FDD),
    b(8, 900, 20, FDD),
    b(12, 700, 15, FDD),
    b(13, 700, 10, FDD),
    b(14, 700, 10, FDD | PS),
    b(18, 850, 15, FDD),
    b(20, 800, 20, FDD),
    b(25, 1900, 40, FDD),
    b(26, 850, 20, FDD),
    b(28, 700, 30, FDD),
    b(29, 700, 10, SDL),
    b(30, 2300, 10, FDD),
    b(34, 2000, 15, TDD),
    b(38, 2600, 40, TDD),
    b(39, 1900, 40, TDD),
    b(40, 2300, 50, TDD),
    b(41, 2500, 100, TDD),
    b(46, 5200, 100, TDD | LAA),
    b(48, 3500, 100, TDD | CBRS),
    b(50, 1500, 80, TDD),
    b(51, 1500, 5, TDD),
    b(53, 2400, 10, TDD),
    b(65, 2100, 50, FDD),
    b(66, 3, 40, FDD | AWS),
    b(70, 4, 25, FDD | AWS),
    b(71, 600, 35, FDD),
    b(74, 1500, 20, FDD),
    b(77, 3700, 100, TDD),
    b(78, 3500, 100, TDD),
    b(79, 4700, 100, TDD),
    b(257, 28000, 400, TDD),
    b(258, 26000, 400, TDD),
    b(260, 39000, 400, TDD),
    b(261, 28000, 400, TDD),
];

fn spec(nr: bool, band: u32) -> Option<Spec> {
    let table: &[Spec] = if nr { &NR } else { &LTE };
    table.iter().copied().find(|s| u32::from(s.band) == band)
}

impl Spec {
    /// "3500 MHz (CBRS)", "AWS-3", "28 GHz".
    fn name(self) -> String {
        if self.kind & TAG == AWS {
            return format!("AWS-{}", self.mhz);
        }
        let mut s = if self.mhz >= 10_000 {
            format!("{} GHz", self.mhz / 1000)
        } else {
            format!("{} MHz", self.mhz)
        };
        s.push_str(match self.kind & TAG {
            PS => " (PS)",
            LAA => " (LAA)",
            CBRS => " (CBRS)",
            _ => "",
        });
        s
    }

    fn duplex(self) -> &'static str {
        match self.kind & DUPLEX {
            TDD => "TDD",
            SDL => "SDL",
            _ => "FDD",
        }
    }
}

/// The band number in a firmware value: a number, or one with a prefix
/// such as "n78", "B3", "LTE BAND 3", "NR5G_BAND78".
pub fn number(raw: &str) -> Option<u32> {
    const PREFIXES: [&str; 8] = ["NR5G", "LTE", "BAND", "NR", "5G", "4G", "B", "N"];
    let sep = |c: char| c.is_whitespace() || matches!(c, ':' | '_' | '-');
    let upper = raw.to_ascii_uppercase();
    let mut v = upper.trim_matches(sep);
    while let Some(p) = PREFIXES.iter().find(|p| v.starts_with(**p)) {
        v = v[p.len()..].trim_matches(sep);
    }
    if v.is_empty() || !v.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    v.parse().ok().filter(|n| *n > 0)
}

/// "n78" or "B3".
fn label(nr: bool, n: u32) -> String {
    format!("{}{n}", if nr { 'n' } else { 'B' })
}

/// The primary band row: "n78 (3500 MHz, TDD)", plus " · PCC" with carrier
/// aggregation; "—" if there is no band.
pub fn band(raw: &str, nr: bool, pcc: bool) -> String {
    let Some(n) = number(raw) else { return "--".into() };
    let mut s = label(nr, n);
    if let Some(sp) = spec(nr, n) {
        s = format!("{s} ({}, {})", sp.name(), sp.duplex());
    }
    if pcc {
        s.push_str(" \u{b7} PCC");
    }
    s
}

/// A secondary carrier's band: "n78 (3500 MHz)".
pub fn carrier_band(raw: &str, nr: bool) -> String {
    match number(raw) {
        Some(n) => match spec(nr, n) {
            Some(sp) => format!("{} ({})", label(nr, n), sp.name()),
            None => label(nr, n),
        },
        None => "--".into(),
    }
}

/// MHz in a firmware bandwidth ("20", "20MHz", "BW 100").
pub fn mhz(raw: &str) -> Option<u32> {
    raw.trim_matches(|c: char| c.is_alphabetic() || c.is_whitespace()).parse().ok()
}

/// "50 / 100 MHz max" when the band is known, else the raw value or "—".
pub fn bandwidth(raw: &str, band_raw: &str, nr: bool) -> String {
    match (mhz(raw), number(band_raw).and_then(|n| spec(nr, n))) {
        (Some(v), Some(sp)) => format!("{v} / {} MHz max", sp.max_bw),
        _ if raw.trim().is_empty() => "--".into(),
        _ => raw.trim().to_owned(),
    }
}

/// The app's Band Lock lists (`BandConfig.commonNRBands`, `commonLTEBands`).
pub const LOCK_NR: [u16; 19] = [1, 2, 3, 5, 7, 8, 12, 20, 25, 28, 38, 40, 41, 48, 66, 71, 77, 78, 79];
pub const LOCK_LTE: [u16; 28] = [1, 2, 3, 4, 5, 7, 8, 12, 13, 14, 17, 20, 25, 26, 28, 29, 30, 32, 38, 39, 40, 41, 42, 43, 46, 48, 66, 71];

/// Downlink and uplink of the Band Lock bands (MHz, low and high; no
/// uplink for SDL), the app's `dlRange` and `ulRange`: the same for LTE
/// and NR where a band is both.
const RANGES: [(u16, [u16; 4]); 31] = [
    (1, [2110, 2170, 1920, 1980]),
    (2, [1930, 1990, 1850, 1910]),
    (3, [1805, 1880, 1710, 1785]),
    (4, [2110, 2155, 1710, 1755]),
    (5, [869, 894, 824, 849]),
    (7, [2620, 2690, 2500, 2570]),
    (8, [925, 960, 880, 915]),
    (12, [729, 746, 699, 716]),
    (13, [746, 756, 777, 787]),
    (14, [758, 768, 788, 798]),
    (17, [734, 746, 704, 716]),
    (20, [791, 821, 832, 862]),
    (25, [1930, 1995, 1850, 1915]),
    (26, [859, 894, 814, 849]),
    (28, [758, 803, 703, 748]),
    (29, [717, 728, 0, 0]),
    (30, [2350, 2360, 2305, 2315]),
    (32, [1452, 1496, 0, 0]),
    (38, [2570, 2620, 2570, 2620]),
    (39, [1880, 1920, 1880, 1920]),
    (40, [2300, 2400, 2300, 2400]),
    (41, [2496, 2690, 2496, 2690]),
    (42, [3400, 3600, 3400, 3600]),
    (43, [3600, 3800, 3600, 3800]),
    (46, [5150, 5925, 5150, 5925]),
    (48, [3550, 3700, 3550, 3700]),
    (66, [2110, 2200, 1710, 1780]),
    (71, [617, 652, 663, 698]),
    (77, [3300, 4200, 3300, 4200]),
    (78, [3300, 3800, 3300, 3800]),
    (79, [4400, 5000, 4400, 5000]),
];

/// A Band Lock band's common name ("3500 MHz") and the facts of the app's
/// long-press popover: "TDD · Max BW 100 MHz · DL 3300–3800 MHz · UL
/// 3300–3800 MHz" (no UL for SDL; every band in the lists is FR1).
pub fn lock_facts(nr: bool, band: u16) -> Option<(String, String)> {
    let sp = spec(nr, u32::from(band))?;
    let [dl0, dl1, ul0, ul1] = RANGES.iter().find(|(b, _)| *b == band)?.1;
    let mut facts = format!("{} \u{b7} Max BW {} MHz \u{b7} DL {dl0}\u{2013}{dl1} MHz", sp.duplex(), sp.max_bw);
    if ul0 > 0 {
        facts.push_str(&format!(" \u{b7} UL {ul0}\u{2013}{ul1} MHz"));
    }
    Some((sp.name(), facts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted_unique_and_complete() {
        for table in [&LTE[..], &NR[..]] {
            assert!(table.windows(2).all(|w| w[0].band < w[1].band));
        }
        assert_eq!(LTE.len() + NR.len(), 82);
        assert_eq!(std::mem::size_of::<Spec>(), 8);
    }

    /// Every Band Lock band has the app's facts; sorted lists, as the app's.
    #[test]
    fn lock_bands_have_their_facts() {
        for (nr, list) in [(true, &LOCK_NR[..]), (false, &LOCK_LTE[..])] {
            assert!(list.windows(2).all(|w| w[0] < w[1]));
            for b in list {
                assert!(lock_facts(nr, *b).is_some(), "{nr} {b}");
            }
        }
        assert!(RANGES.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(
            lock_facts(true, 78),
            Some(("3500 MHz".into(), "TDD \u{b7} Max BW 100 MHz \u{b7} DL 3300\u{2013}3800 MHz \u{b7} UL 3300\u{2013}3800 MHz".into()))
        );
        assert_eq!(lock_facts(false, 29).unwrap().1, "SDL \u{b7} Max BW 10 MHz \u{b7} DL 717\u{2013}728 MHz");
        assert_eq!(lock_facts(false, 66).unwrap().0, "AWS-3");
        assert_eq!(lock_facts(true, 41).unwrap().1, "TDD \u{b7} Max BW 100 MHz \u{b7} DL 2496\u{2013}2690 MHz \u{b7} UL 2496\u{2013}2690 MHz");
        assert_eq!(lock_facts(false, 41).unwrap().1, "TDD \u{b7} Max BW 20 MHz \u{b7} DL 2496\u{2013}2690 MHz \u{b7} UL 2496\u{2013}2690 MHz");
        assert_eq!(lock_facts(true, 99), None);
    }

    #[test]
    fn band_numbers_from_firmware_values() {
        for (raw, n) in [("78", 78), ("n78", 78), ("B3", 3), ("LTE BAND 3", 3), ("NR5G_BAND78", 78), (" band:41 ", 41), ("5G-n41", 41)] {
            assert_eq!(number(raw), Some(n), "{raw}");
        }
        for raw in ["", "0", "n", "x3", "3a", "-", "BAND"] {
            assert_eq!(number(raw), None, "{raw}");
        }
    }

    #[test]
    fn strings_match_the_app() {
        assert_eq!(band("41", true, false), "n41 (2500 MHz, TDD)");
        assert_eq!(band("1", false, false), "B1 (2100 MHz, FDD)");
        assert_eq!(band("78", true, true), "n78 (3500 MHz, TDD) \u{b7} PCC");
        assert_eq!(band("48", false, false), "B48 (3500 MHz (CBRS), TDD)");
        assert_eq!(band("66", false, false), "B66 (AWS-3, FDD)");
        assert_eq!(band("29", false, false), "B29 (700 MHz, SDL)");
        assert_eq!(band("14", true, false), "n14 (700 MHz (PS), FDD)");
        assert_eq!(band("46", false, false), "B46 (5200 MHz (LAA), TDD)");
        assert_eq!(band("258", true, false), "n258 (26 GHz, TDD)");
        assert_eq!(band("99", false, true), "B99 \u{b7} PCC");
        assert_eq!(band("", true, false), "--");
        assert_eq!(carrier_band("3", false), "B3 (1800 MHz)");
        assert_eq!(carrier_band("200", true), "n200");
        assert_eq!(bandwidth("50", "41", true), "50 / 100 MHz max");
        assert_eq!(bandwidth("10MHz", "1", false), "10 / 20 MHz max");
        assert_eq!(bandwidth("15", "99", false), "15");
        assert_eq!(bandwidth("", "1", false), "--");
        assert_eq!(mhz("BW 100"), Some(100));
        assert_eq!(mhz("wide"), None);
    }
}
