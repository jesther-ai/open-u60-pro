//! `GET /ui/signal`: "Connection & signal", the radio details (iOS
//! `SignalMonitorView` with the dashboard's WAN addresses and "Network
//! mode" link). Each panel is a fragment with a stable id, rendered from
//! topic data alone (a panel that does not apply is an empty hidden
//! element, so live updates still find it); the nav bar's age badge is one
//! too. Without JavaScript every load adds one point to the chart. Web
//! additions: in airplane mode the panels are blank (the firmware keeps
//! its last readings) and say why; a failed read says so once, under the
//! operator card.

use maud::{html, Markup};
use serde_json::Value;

use super::dashboard::{self, airplane};
use crate::events::history::{HistPoint, NONE};
use crate::events::hub::{SIGNAL_CADENCE, SIGNAL_HINT_MAX};
use crate::events::Topic;
use crate::web::kit::{self, Icon};
use crate::web::render::{self, Fragment, Views};
use crate::web::roll::roll;
use crate::web::screen::PageData;
use crate::web::signal::{self as model, Metrics, Radio};
use crate::web::{bands, fmt};

pub const TOPICS: [Topic; 3] = [Topic::Signal, Topic::Wan, Topic::Modem];

const S: &[Topic] = &[Topic::Signal, Topic::Modem];

/// The cards, in the app's order.
const CARDS: [Fragment; 7] = [
    Fragment { id: "sig-op", topics: S, render: sig_op },
    Fragment { id: "sig-nr", topics: S, render: sig_nr },
    Fragment { id: "sig-lte", topics: S, render: sig_lte },
    Fragment { id: "sig-wcdma", topics: S, render: sig_wcdma },
    Fragment { id: "sig-bearer", topics: S, render: sig_bearer },
    Fragment { id: "sig-chart", topics: S, render: sig_chart },
    Fragment { id: "sig-wan", topics: &[Topic::Wan], render: sig_wan },
];

pub const FRAGMENTS: [Fragment; 8] = [
    CARDS[0],
    CARDS[1],
    CARDS[2],
    CARDS[3],
    CARDS[4],
    CARDS[5],
    CARDS[6],
    // The nav bar's leading badge (`screen::Bar::AgeDone`).
    Fragment { id: "age", topics: &[Topic::Signal], render: dashboard::read_badge },
];

/// The page's content (`tabs::dashboard`).
pub fn block(pd: &PageData) -> Markup {
    body(pd.views)
}

pub fn body(v: &Views) -> Markup {
    html! {
        (render::live_snapshot("/ui/signal"))
        div.cards.wide {
            @for f in &CARDS { ((f.render)(v)) }
        }
        (kit::section(None, None, kit::link_row("/ui/signal/network-mode", None, "Network mode")))
    }
}

/// The reading the panels show: none in airplane mode (the firmware keeps
/// the last values while the radio is off).
fn signal(v: &Views) -> Option<&Value> {
    v.value(Topic::Signal).filter(|_| !airplane(v))
}

/// What the page shows of one reading.
struct Model {
    nr: Radio,
    lte: Radio,
    show: model::Panels,
}

fn model_of(v: &Views) -> Option<Model> {
    let d = signal(v)?;
    let (nr, lte) = (Radio::parse(d, true), Radio::parse(d, false));
    let show = model::panels(d, &nr, &lte);
    Some(Model { nr, lte, show })
}

/// The app's operator card (`OperatorCardView`): the provider or "No
/// Operator", the network type or "--", "Roaming", the network-type pill
/// and a signal glyph (full while the modem reports any bar). Blank in
/// airplane mode.
fn sig_op(v: &Views) -> Markup {
    let s = signal(v);
    let provider = s.and_then(|s| s["network_provider"].as_str()).map(str::trim).filter(|o| !o.is_empty());
    let kind = s.filter(|s| s["network_type"].as_str().is_some_and(|t| !t.trim().is_empty())).map(model::display_type);
    let kind = kind.map_or_else(|| "--".to_owned(), |k| fmt::truncate(&k, 24));
    let roaming = s.is_some_and(|s| fmt::int(&s["simcard_roam"]) == Some(1));
    let bars = s.and_then(|s| fmt::int(&s["signalbar"])).is_some_and(|b| b > 0);
    html! {
        section #sig-op .card {
            div.op {
                div { b { (provider.unwrap_or("No Operator")) } span.muted { (kind) } }
                @if roaming { span.roam { (kit::icon(Icon::AntennaSlash)) " Roaming" } }
                (type_pill(&kind))
                span.sb.muted[!bars] role="img" aria-label="Signal" { (kit::icon(Icon::Bars)) }
            }
            @if airplane(v) { p.muted { "Airplane mode is on: the radio is off." } }
            (render::error_line(v, &[Topic::Signal]))
        }
    }
}

/// iOS `NetworkTypeIcon`: "5G" over "SA" or "NSA"; a warning for limited or
/// no service (also spelled with "_", as the firmware does); else the type
/// itself, grey without one. The line above says the same in words.
fn type_pill(kind: &str) -> Markup {
    let words = kind.to_ascii_lowercase().replace(['_', '-'], " ");
    let sub = kind.strip_prefix("5G ").filter(|s| *s == "SA" || *s == "NSA");
    html! {
        @if words.contains("limited") || words.contains("no service") {
            span.pill.warn aria-hidden="true" { (kit::icon(Icon::Warn)) }
        } @else if let Some(sub) = sub {
            span.pill aria-hidden="true" { "5G" small { (sub) } }
        } @else {
            span.pill.none[kind == "--"] aria-hidden="true" { (kind) }
        }
    }
}

/// The app's `Int(value)`: the decimals cut off, not rounded.
fn int(v: f64) -> i64 {
    v.trunc() as i64
}

/// The four-value grid (RSRP, RSRQ, SINR, RSSI); `key`: the radio's
/// letters for its rolling numbers (`roll`).
fn metrics(v: &Views, key: &str, m: &Metrics) -> Markup {
    const NAMES: [(&str, &str, &str); 4] = [("RSRP", "dBm", "p"), ("RSRQ", "dB", "q"), ("SINR", "dB", "s"), ("RSSI", "dBm", "i")];
    html! {
        div.metrics {
            @for (i, (name, unit, k)) in NAMES.iter().enumerate() {
                @let class = m[i].map(|x| match i {
                    0 => fmt::quality_class(x),
                    1 => model::rsrq_class(x),
                    2 => model::sinr_class(x),
                    _ => "",
                });
                div {
                    span.lbl { (name) }
                    @match m[i] {
                        Some(x) => b class=[class.filter(|c| !c.is_empty())] { (roll(v, &format!("{key}{k}"), &int(x).to_string())) },
                        None => b.muted { "--" },
                    }
                    small { (unit) }
                }
            }
        }
    }
}

/// The 5G NR or LTE panel (empty and hidden when it does not apply).
fn radio(id: &str, v: &Views, nr: bool) -> Markup {
    let Some(m) = model_of(v).filter(|m| if nr { m.show.nr } else { m.show.lte }) else {
        return html! { section.card id=(id) hidden {} };
    };
    let r = if nr { &m.nr } else { &m.lte };
    let ca = r.ca_active();
    // Rolling-number keys: "n…" for 5G NR, "l…" for LTE, a second letter
    // for each secondary carrier.
    let k = if nr { "n" } else { "l" };
    html! {
        section.card id=(id) {
            div.ph {
                h2 { (kit::icon(if nr { Icon::Antenna } else { Icon::Bars })) " " (if nr { "5G NR" } else { "LTE" }) }
                @if !nr && m.show.nr { span.badge { "NSA Anchor" } }
                @if ca { span.badge.bl[nr] { (r.cc_count()) " CC" } }
                @match r.sig[0] {
                    Some(x) => b class=(fmt::quality_class(x)) { (fmt::quality(x)) },
                    None => span.muted { "Disconnected" },
                }
            }
            @if r.connected() {
                (metrics(v, k, &r.sig))
                dl.kv.meta {
                    div { dt { "Band" } dd { (bands::band(&r.band, nr, ca)) } }
                    div { dt { "PCI" } dd { (int_roll(v, &format!("{k}pci"), &r.pci)) } }
                    div { dt { "Cell ID" } dd { (int_roll(v, &format!("{k}cid"), &r.cell)) } }
                    div { dt { (if nr { "Channel" } else { "EARFCN" }) } dd { (int_roll(v, &format!("{k}ch"), &r.channel)) } }
                    div { dt { "Bandwidth" } dd { (bands::bandwidth(&r.bw, &r.band, nr)) } }
                    div { dt { "CA" } dd { @if ca { "Active (" (r.cc_count()) " CC)" } @else { "Inactive" } } }
                    @if let Some(total) = r.total_bw().filter(|_| nr && ca) {
                        div { dt { "Total BW" } dd { (total) " MHz" } }
                    }
                }
                @for (i, c) in r.sccs.iter().enumerate() {
                    div.scc {
                        p.lbl { (if nr { "5G SCC" } else { "SCC" }) (i) }
                        p.cap { (bands::carrier_band(&c.band, nr)) "\u{2003}PCI " (dash(&c.pci)) "\u{2003}BW " (dash(&c.bw)) }
                        (metrics(v, &format!("{k}{}", letters(i)), &c.sig))
                    }
                }
            }
        }
    }
}

/// A row value that rolls when it is a whole number (the app's
/// `SignalRow`: `Int(value)` round-trips), else as it is.
fn int_roll(v: &Views, key: &str, s: &str) -> Markup {
    match s.parse::<i64>() {
        Ok(n) if n.to_string() == s => roll(v, key, s),
        _ => html! { (dash(s)) },
    }
}

/// "a", "b", … "z", "ba", …: lowercase letters for an index (roll keys).
pub fn letters(mut i: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'a' + (i % 26) as u8);
        i /= 26;
        if i == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// The app's placeholder for an empty value on this page.
fn dash(s: &str) -> &str {
    if s.is_empty() {
        "--"
    } else {
        s
    }
}

fn sig_nr(v: &Views) -> Markup {
    radio("sig-nr", v, true)
}

fn sig_lte(v: &Views) -> Markup {
    radio("sig-lte", v, false)
}

fn sig_wcdma(v: &Views) -> Markup {
    let shown = model_of(v).is_some_and(|m| m.show.wcdma);
    let Some((rscp, ecio)) = signal(v).map(model::wcdma).filter(|_| shown) else {
        return html! { section #sig-wcdma .card hidden {} };
    };
    html! {
        section #sig-wcdma .card {
            div.ph {
                h2 { (kit::icon(Icon::Antenna)) " WCDMA" }
                @if let Some(x) = rscp { b class=(model::rscp_class(x)) { (model::rscp_quality(x)) } }
            }
            div.metrics.two {
                div {
                    span.lbl { "RSCP" }
                    @match rscp { Some(x) => b class=(model::rscp_class(x)) { (roll(v, "wr", &int(x).to_string())) }, None => b.muted { "--" } }
                    small { "dBm" }
                }
                div {
                    span.lbl { "Ec/Io" }
                    @match ecio { Some(x) => b class=(model::ecio_class(x)) { (roll(v, "we", &int(x).to_string())) }, None => b.muted { "--" } }
                    small { "dB" }
                }
            }
        }
    }
}

fn sig_bearer(v: &Views) -> Markup {
    let Some((qci, down, up)) = signal(v).and_then(model::bearer) else {
        return html! { section #sig-bearer .card hidden {} };
    };
    let sa = v.value(Topic::Signal).is_some_and(fmt::is_sa);
    let rate = |r: Option<f64>| r.map(model::mbps).unwrap_or_else(|| "--".into());
    html! {
        section #sig-bearer .card {
            h2 { "Data Session" }
            dl.kv.meta {
                @if let Some(q) = qci { div { dt { (if sa { "5QI" } else { "QCI" }) } dd { (q) } } }
                @if down.is_some() || up.is_some() {
                    div { dt { "AMBR" } dd { (rate(down)) " / " (rate(up)) " Mbps" } }
                }
            }
            @if down.is_some() || up.is_some() {
                p.note { "Down / up. Operator rate cap, set when the data session started. Not a speed test." }
            }
        }
    }
}

fn wan_ipv4(wan: &Value) -> Option<&str> {
    wan["ipv4-address"][0]["address"].as_str().filter(|a| !a.is_empty())
}

/// The first global IPv6 address, else the first delegated prefix.
fn wan_ipv6(wan6: &Value) -> Option<&str> {
    ["ipv6-address", "ipv6-prefix-assignment"].iter().find_map(|key| {
        wan6[key].as_array()?.iter().find_map(|e| {
            e["address"].as_str().filter(|a| !a.is_empty() && !a.to_ascii_lowercase().starts_with("fe80"))
        })
    })
}

/// The WAN addresses card (the dashboard's, which the app has; here only).
fn sig_wan(v: &Views) -> Markup {
    let w = v.value(Topic::Wan);
    let v4 = w.and_then(|w| wan_ipv4(&w["wan"]));
    let v6 = w.and_then(|w| wan_ipv6(&w["wan6"]));
    html! {
        section #sig-wan .card {
            h2 { "WAN addresses" }
            dl.kv {
                div { dt { "IPv4" } dd { (v4.unwrap_or("\u{2014}")) } }
                div { dt { "IPv6" } dd { (v6.unwrap_or("\u{2014}")) } }
            }
            (render::error_line(v, &[Topic::Wan]))
        }
    }
}

/// Chart geometry (SVG user units): the plot is `PLOT_W` wide with the dBm
/// labels to its right; -40 dBm at `TOP`, -140 dBm at `TOP + 100 * Y_SCALE`.
const PLOT_W: f64 = 268.0;
const TOP: f64 = 6.0;
const Y_SCALE: f64 = 1.08;
/// The chart spans this many ms back from the newest reading.
const SPAN_MS: u64 = 120_000;

fn y_of(dbm: i16) -> f64 {
    TOP + (-40.0 - f64::from(dbm).clamp(-140.0, -40.0)) * Y_SCALE
}

/// Whether reading `i` continues the line from reading `i - 1` (times `t`,
/// ms): at most 3 sampling periods later. The period is the longer spacing
/// next to this one (the cadence follows the fastest open page, Settings ›
/// Polling: 1–10 s), kept between the default and the slowest cadence, so
/// a pause of the sampler (no page open) still breaks the line.
fn joined(t: &[u64], i: usize) -> bool {
    let gap = |j: usize| (1..t.len()).contains(&j).then(|| t[j] - t[j - 1]);
    let period = [gap(i - 1), gap(i + 1)].into_iter().flatten().max().unwrap_or(0);
    let (lo, hi) = (SIGNAL_CADENCE.as_millis() as u64, SIGNAL_HINT_MAX.as_millis() as u64);
    t[i] - t[i - 1] <= 3 * period.clamp(lo, hi)
}

/// Polyline point lists of one series, split where a reading is missing or
/// the gap is over 3 sampling periods (`joined`). A lone point is drawn as a
/// dot.
fn segments(h: &[HistPoint], get: fn(&HistPoint) -> i16) -> Vec<String> {
    let Some(newest) = h.last().map(|p| p.at_ms) else { return Vec::new() };
    let pts: Vec<&HistPoint> = h.iter().filter(|p| newest - p.at_ms <= SPAN_MS).collect();
    let t: Vec<u64> = pts.iter().map(|p| p.at_ms).collect();
    let mut out: Vec<String> = Vec::new();
    let mut prev = false;
    for (i, p) in pts.iter().enumerate() {
        let dbm = get(p);
        if dbm == NONE {
            prev = false;
            continue;
        }
        let x = PLOT_W - (newest - p.at_ms) as f64 * PLOT_W / SPAN_MS as f64;
        let pt = format!("{x:.1},{:.1}", y_of(dbm));
        match out.last_mut() {
            Some(seg) if prev && joined(&t, i) => {
                seg.push(' ');
                seg.push_str(&pt);
            }
            _ => out.push(pt),
        }
        prev = true;
    }
    for seg in &mut out {
        if !seg.contains(' ') {
            *seg = format!("{seg} {seg}");
        }
    }
    out
}

/// The signal history chart: RSRP (RSCP for 3G) over the last 2 minutes,
/// relative to the newest reading, so it only changes with a new reading.
fn sig_chart(v: &Views) -> Markup {
    let m = model_of(v);
    let show = |i: usize| match &m {
        Some(m) => [m.show.nr || !m.show.lte && !m.show.wcdma, m.show.lte, m.show.wcdma][i],
        None => i == 0,
    };
    type Get = fn(&HistPoint) -> i16;
    const SERIES: [(&str, &str, Get); 3] =
        [("nr", "NR", |p| p.nr_rsrp), ("lte", "LTE", |p| p.lte_rsrp), ("wcdma", "3G", |p| p.rscp)];
    let lines: Vec<(&str, &str, Vec<String>)> = SERIES
        .iter()
        .enumerate()
        .filter(|(i, _)| show(*i))
        .map(|(_, (class, name, get))| (*class, *name, segments(&v.history, *get)))
        .filter(|(_, _, segs)| !segs.is_empty())
        .collect();
    let latest = |get: Get| v.history.last().map(get).filter(|d| *d != NONE);
    let mut label = String::from("Signal strength over the last 2 minutes");
    for (i, (_, name, get)) in SERIES.iter().enumerate() {
        if let Some(d) = latest(*get).filter(|_| show(i)) {
            label.push_str(&format!(", {name} now {d} dBm"));
        }
    }
    html! {
        figure #sig-chart .card {
            h2 { "Signal History" }
            // The firmware keeps reporting its last readings while the radio is off.
            @if airplane(v) {
                p.muted { "No readings in airplane mode." }
            } @else if lines.is_empty() {
                p.muted { "Collecting data..." }
            } @else {
                svg.chart viewBox="0 0 300 130" role="img" aria-label=(label) {
                    path.grid d=(grid()) {}
                    @for i in 0..6u8 {
                        text x="272" y=(format!("{:.1}", y_of(-40 - 20 * i16::from(i)) + 3.0)) { (-40 - 20 * i16::from(i)) }
                    }
                    text x="0" y="127" { "2 min ago" }
                    text x=(PLOT_W) y="127" text-anchor="end" { "now" }
                    @for (class, _, segs) in &lines {
                        @for seg in segs { polyline class=(class) points=(seg) {} }
                    }
                }
                figcaption.legend {
                    @for (class, name, _) in &lines { span class=(class) { (name) } " " }
                }
            }
        }
    }
}

/// Horizontal lines every 20 dBm and vertical ones every 30 s.
fn grid() -> String {
    let mut d = String::new();
    for i in 0..6 {
        d.push_str(&format!("M0 {:.1}H{PLOT_W}", y_of(-40 - 20 * i)));
    }
    for i in 0..4 {
        d.push_str(&format!("M{:.1} {TOP}V{:.1}", f64::from(i) * PLOT_W / 4.0, y_of(-140)));
    }
    d
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::boottime::BootInstant;
    use crate::events::hub::SlotView;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    const NOW: BootInstant = BootInstant::from_duration(Duration::from_secs(10_000));

    fn views(pairs: Vec<(Topic, Value)>) -> Views {
        let slots = pairs
            .into_iter()
            .map(|(t, v)| (t, SlotView { version: 1, at: NOW, value_at: NOW, value: Some(Arc::new(v)), error: None }))
            .collect();
        Views::new(NOW, slots)
    }

    fn signal() -> Value {
        json!({
            "network_type": "ENDC", "network_provider": "Example <b>Mobile</b>", "simcard_roam": "0",
            "nr5g_rsrp": "-77", "nr5g_rsrq": "-11", "nr5g_snr": "11", "nr5g_rssi": "-68",
            "nr5g_action_band": "41", "nr5g_pci": "774", "nr5g_cell_id": "4242",
            "nr5g_action_channel": "513000", "nr5g_bandwidth": "50",
            "lte_rsrp": "-85", "lte_rsrq": "-12", "lte_snr": "5", "lte_rssi": "-56",
            "wan_active_band": "1", "lte_pci": "178", "cell_id": "77", "wan_active_channel": "301",
            "lteca_state": "1", "lteca": "178,1,0,301,10;12,3,1,1300,20", "ltecasig": "-90,-10,8,-60",
            "bearer": {"qci": 9, "ambr_dl_mbps": 10000, "ambr_ul_mbps": 10000},
            "sim_iccid": "8900100000009998877", "imsi": "001019999911111", "imei": "000000000000042",
            "client": {"mac": "aa:bb:cc:dd:ee:42", "hostname": "alice-laptop-42"},
        })
    }

    pub(crate) fn fixture() -> Views {
        let mut v = views(vec![
            (Topic::Signal, signal()),
            (Topic::Modem, json!({
                "status": {"operate_mode": "ONLINE", "imei": "000000000000042"},
                "data": {"enable": 1, "connect_status": "ipv4_connected", "key_2g": "WIFIKEY-7f3a"},
            })),
            (Topic::Wan, json!({"wan": {"ipv4-address": [{"address": "10.20.30.40"}]}, "wan6": {}})),
            (Topic::Sim, json!({
                "sim_states": "Ready", "sim_iccid": "8900100000009998877", "msisdn": "+10000000042",
                "sim_imsi": "001019999911111", "imei": "000000000000042",
            })),
        ]);
        let pt = |s: u64, nr: i16, lte: i16| HistPoint { at_ms: s * 1000, nr_rsrp: nr, lte_rsrp: lte, rscp: NONE };
        v.history = vec![pt(9_880, -70, -80), pt(9_990, -77, -85), pt(9_992, -78, NONE), pt(9_994, -76, -84), pt(10_000, -77, -85)];
        v
    }

    #[test]
    fn renders_the_app_panels() {
        let s = body(&fixture()).into_string();
        for want in [
            "Example &lt;b&gt;Mobile&lt;/b&gt;", "5G NSA", "5G NR", "n41 (2500 MHz, TDD)", "50 / 100 MHz max", ">774<", ">4242<",
            "513000", "NSA Anchor", "2 CC", "B1 (2100 MHz, FDD) \u{b7} PCC", "Active (2 CC)", "EARFCN", "SCC0",
            "B3 (1800 MHz)\u{2003}PCI 12\u{2003}BW 20", "Data Session", "QCI", "10000 / 10000 Mbps", "Not a speed test.",
            "Signal History", "10.20.30.40", "Excellent", "Good",
        ] {
            assert!(s.contains(want), "missing {want:?} in {s}");
        }
        // The WCDMA panel does not apply: hidden, but present for patches.
        assert!(s.contains("<section class=\"card\" id=\"sig-wcdma\" hidden></section>"));
        for secret in ["9998877", "001019999911111", "10000000042", "000000000000042", "WIFIKEY-7f3a", "aa:bb:cc:dd:ee:42", "alice-laptop-42"] {
            assert!(!s.contains(secret), "{secret}");
        }
    }

    /// The page as the screen renders it: the age badge leads the nav bar.
    pub(crate) fn page(v: &Views) -> String {
        body(v).into_string() + &dashboard::read_badge(v).into_string()
    }

    #[test]
    fn every_fragment_is_in_the_page_byte_for_byte() {
        for v in [fixture(), views(vec![])] {
            let page = page(&v);
            for f in &FRAGMENTS {
                let html = (f.render)(&v).into_string();
                assert!(html.contains(&format!("id=\"{}\"", f.id)), "{}", f.id);
                assert!(page.contains(&html), "{} differs from the page", f.id);
            }
            assert!(page.contains("id=\"live-status\""));
        }
    }

    #[test]
    fn chart_splits_at_gaps_and_missing_readings() {
        let v = fixture();
        let s = sig_chart(&v).into_string();
        // NR: the 120 s old point stands alone (gap) and is drawn as a dot;
        // the rest is one line up to the newest point at the right edge.
        assert!(s.contains("<polyline class=\"nr\" points=\"0.0,38.4 0.0,38.4\"></polyline>"), "{s}");
        assert!(s.contains("<polyline class=\"nr\" points=\"245.7,46.0 250.1,47.0 254.6,44.9 268.0,46.0\">"), "{s}");
        // LTE: the missing reading splits the line.
        assert!(s.contains("<polyline class=\"lte\" points=\"245.7,54.6 245.7,54.6\">"), "{s}");
        assert!(s.contains("<polyline class=\"lte\" points=\"254.6,53.5 268.0,54.6\">"), "{s}");
        assert_eq!(s.matches("<polyline").count(), 5);
        assert!(s.contains("role=\"img\"") && s.contains("NR now -77 dBm, LTE now -85 dBm"));
        // The app's legend names, nothing else.
        assert!(s.contains("<figcaption class=\"legend\"><span class=\"nr\">NR</span> <span class=\"lte\">LTE</span> </figcaption>"), "{s}");
        // No history yet (the app's text, with three dots).
        assert!(sig_chart(&views(vec![])).into_string().contains("<p class=\"muted\">Collecting data...</p>"));
    }

    /// The line follows the cadence a page asked for (Settings › Polling,
    /// 1–10 s): readings 10 s apart are one line, a mix of cadences too;
    /// a pause of the sampler still breaks it.
    #[test]
    fn chart_lines_follow_the_cadence() {
        let at = |times: &[u64]| -> Vec<HistPoint> {
            times.iter().map(|ms| HistPoint { at_ms: *ms, nr_rsrp: -80, lte_rsrp: -90, rscp: NONE }).collect()
        };
        let lines = |h: &[HistPoint]| segments(h, |p| p.nr_rsrp).len();
        let every = |step: u64, n: u64, from: u64| (0..n).map(move |i| from + i * step);
        // 10.02 s and 6.05 s apart (the hub schedules after each read): one line.
        assert_eq!(lines(&at(&every(10_020, 12, 0).collect::<Vec<_>>())), 1);
        assert_eq!(lines(&at(&every(6_050, 20, 0).collect::<Vec<_>>())), 1);
        // 2 s, then 10 s (a faster page closed): one line.
        let mixed: Vec<u64> = every(2_000, 20, 0).chain(every(10_000, 8, 48_000)).collect();
        assert_eq!(lines(&at(&mixed)), 1);
        // 2 s with a 30 s pause: two lines.
        let paused: Vec<u64> = every(2_000, 20, 0).chain(every(2_000, 20, 68_000)).collect();
        assert_eq!(lines(&at(&paused)), 2);
        // One reading between two 50 s pauses stands alone.
        let lone: Vec<u64> = every(2_000, 5, 0).chain([58_000]).chain(every(2_000, 5, 108_000)).collect();
        assert_eq!(lines(&at(&lone)), 3);
    }

    #[test]
    fn airplane_mode_blanks_the_readings() {
        let mut v = fixture();
        v.set(Topic::Modem, SlotView { version: 2, at: NOW, value_at: NOW, value: Some(Arc::new(json!({"status": {"operate_mode": "LPM"}}))), error: None });
        let s = body(&v).into_string();
        assert!(s.contains("Airplane mode is on: the radio is off."));
        for id in ["sig-nr", "sig-lte", "sig-bearer"] {
            assert!(s.contains(&format!("id=\"{id}\" hidden>")), "{id}");
        }
        // The chart neither draws nor reads out the frozen values.
        let chart = sig_chart(&v).into_string();
        assert!(chart.contains("No readings in airplane mode.") && !chart.contains("now") && !chart.contains("<svg"), "{chart}");
    }

    #[test]
    fn wcdma_shows_its_own_quality_and_mode() {
        let v = views(vec![(Topic::Signal, json!({"network_type": "WCDMA", "network_provider": "Example", "rscp": "-80", "ecio": "-7"}))]);
        let s = sig_op(&v).into_string();
        assert!(s.contains("<span class=\"muted\">WCDMA</span>") && s.contains("<span class=\"pill\" aria-hidden=\"true\">WCDMA</span>"), "{s}");
        assert!(!s.contains("No signal data") && !s.contains("Mode unknown"));
        let w = sig_wcdma(&v).into_string();
        assert!(w.contains("RSCP") && w.contains("class=\"q-good\">Good<"), "{w}");
    }

    /// iOS `OperatorCardView` and `NetworkTypeIcon`.
    #[test]
    fn operator_card_follows_the_app() {
        let op = |d: Value| sig_op(&views(vec![(Topic::Signal, d)])).into_string();
        let s = sig_op(&fixture()).into_string();
        assert!(s.contains("<div><b>Example &lt;b&gt;Mobile&lt;/b&gt;</b><span class=\"muted\">5G NSA</span></div>"), "{s}");
        assert!(s.contains("<span class=\"pill\" aria-hidden=\"true\">5G<small>NSA</small></span>"), "{s}");
        // No bars reported: the glyph is grey; no roaming label, no quality word.
        assert!(s.contains("<span class=\"sb muted\" role=\"img\" aria-label=\"Signal\">"), "{s}");
        assert!(!s.contains("Roaming") && !s.contains("Excellent") && !s.contains("No Operator"), "{s}");
        let s = op(json!({"network_type": "SA", "nr5g_rsrp": "-80", "signalbar": "3", "simcard_roam": "1", "network_provider": " "}));
        assert!(s.contains("<b>No Operator</b><span class=\"muted\">5G SA</span>") && s.contains("5G<small>SA</small>"), "{s}");
        assert!(s.contains("<span class=\"sb\" role=\"img\"") && s.contains(" Roaming</span>"), "{s}");
        // Limited or no service: the warning pill; the type in words above it.
        for t in ["LIMITED_SERVICE_SA", "NO_SERVICE"] {
            let s = op(json!({"network_type": t}));
            assert!(s.contains("<span class=\"pill warn\" aria-hidden=\"true\"><svg") && s.contains(&format!(">{t}</span>")), "{s}");
        }
        let s = op(json!({"network_type": "LTE", "lte_rsrp": "-90"}));
        assert!(s.contains("<span class=\"pill\" aria-hidden=\"true\">4G</span>"), "{s}");
        // Nothing read: "--" in grey.
        let s = sig_op(&views(vec![])).into_string();
        assert!(s.contains("<b>No Operator</b><span class=\"muted\">--</span>") && s.contains("<span class=\"pill none\" aria-hidden=\"true\">--</span>"), "{s}");
    }

    #[test]
    fn placeholders_are_the_apps() {
        // Connected but with gaps: "--" in the grid and the rows (WAN keeps "—").
        let v = views(vec![(Topic::Signal, json!({"network_type": "LTE", "lte_rsrp": "-90", "bearer": {"ambr_dl_mbps": 50}}))]);
        let lte = sig_lte(&v).into_string();
        assert!(lte.contains("<b class=\"muted\">--</b>") && lte.contains("<dt>PCI</dt><dd>--</dd>") && lte.contains("<dt>Band</dt><dd>--</dd>"), "{lte}");
        assert!(lte.contains("<h2><svg") && lte.contains("</svg> LTE</h2>") && !lte.contains("NSA Anchor"), "{lte}");
        assert!(sig_bearer(&v).into_string().contains("50 / -- Mbps"));
        assert!(sig_wan(&v).into_string().contains("<dt>IPv4</dt><dd>—</dd>"));
        // The page ends with the way to Network Mode.
        let page = body(&fixture()).into_string();
        assert!(page.ends_with("<a class=\"it\" href=\"/ui/signal/network-mode\"><span>Network mode</span></a></li></ul></section>"), "{page}");
        // NR carrier aggregation: a blue badge.
        let mut d = signal();
        d["nrca"] = json!("774,41,0,513000,50;775,78,1,630000,100");
        let nr = sig_nr(&views(vec![(Topic::Signal, d)])).into_string();
        assert!(nr.contains("<span class=\"badge bl\">2 CC</span>") && nr.contains("</svg> 5G NR</h2>"), "{nr}");
    }
}
