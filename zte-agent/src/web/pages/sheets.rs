//! The dashboard's detail sheets (iOS `DashboardView` sheets): Live traffic,
//! Battery Details (`BatteryDetailSheet`), CPU & temperatures (`CPUCardView`,
//! `TemperatureCardView`) and CPU & Memory (`CPUDetailSheet`). Each live
//! part is a fragment rendered from topic data alone; the page-load reads
//! (uptime, memory) and the "All sensors" disclosure sit outside every
//! fragment, so a live patch never closes it.

use maud::{html, Markup};

use super::dashboard::{self, cpu_usage, fresh, percent, DASH};
use super::signal::letters;
use crate::boottime::BootInstant;
use crate::events::Topic;
use crate::web::fmt;
use crate::web::kit::{self, Icon};
use crate::web::render::{self, age_line, Fragment, Views};
use crate::web::roll::roll;
use crate::web::screen::PageData;

// --- Live traffic ---

pub const TRAFFIC: [Fragment; 1] = [Fragment { id: "card-traffic", topics: &[Topic::Traffic], render: dashboard::traffic_sheet }];

pub fn traffic(pd: &PageData) -> Markup {
    traffic_body(pd.views)
}

pub fn traffic_body(v: &Views) -> Markup {
    html! {
        (render::live_snapshot("/ui/traffic"))
        div.dash {
            (dashboard::traffic_sheet(v))
            p.note {
                "These speeds show current cellular traffic across all connected devices. A speed test reports \
                 throughput over its test period; the live numbers change throughout that period."
            }
        }
    }
}

// --- Battery Details ---

const POWER: &[Topic] = &[Topic::Battery, Topic::Charger, Topic::BatteryInfo];

pub const BATTERY: [Fragment; 1] = [Fragment { id: "bat-detail", topics: POWER, render: battery_rows }];

pub fn battery(pd: &PageData) -> Markup {
    html! {
        (render::live_snapshot("/ui/battery"))
        (battery_rows(pd.views))
    }
}

fn dash_or(s: Option<String>) -> String {
    s.unwrap_or_else(|| DASH.into())
}

/// The app's rows: Voltage, Current and Power only when known; the times
/// only while charging (to full) or discharging (to empty).
pub fn battery_rows(v: &Views) -> Markup {
    let b = v.value(Topic::Battery);
    let state = fmt::charge(b, v.value(Topic::Charger));
    let capacity = b.and_then(fmt::capacity);
    let mv = b.and_then(|b| fmt::int(&b["voltage_uv"])).map(|uv| uv / 1000);
    let ma = b.and_then(|b| fmt::int(&b["current_ua"])).map(|ua| ua / 1000);
    let info = v.value(Topic::BatteryInfo);
    let eta = |key, when| dash_or(info.and_then(|i| fmt::int(&i[key])).filter(|m| *m > 0 && state == when).map(fmt::eta));
    html! {
        section #bat-detail .group {
            ul.list {
                (kit::kv_row(Some(kit::battery_icon(capacity)), "Capacity", &dash_or(capacity.map(|c| format!("{c}%")))))
                (kit::kv_row(Some(Icon::Bolt), "Status", fmt::charge_label(state, capacity).0))
                @if let Some(mv) = mv { (kit::kv_row_m(Some(Icon::BoltCircle), "Voltage", roll(v, "bv", &format!("{:.3} V", mv as f64 / 1000.0)))) }
                @if let Some(ma) = ma { (kit::kv_row_m(Some(Icon::Swap), "Current", roll(v, "bc", &format!("{ma} mA")))) }
                @if let Some(w) = b.and_then(fmt::watts) { (kit::kv_row_m(Some(Icon::Flame), "Power", roll(v, "bw", &format!("{w:.1} W")))) }
                // The app's temperature is in the primary colour (its other values are secondary).
                li.it {
                    (kit::icon(Icon::Thermometer)) span { "Temperature" }
                    @match b.and_then(fmt::battery_celsius) {
                        Some(t) => span.v.fg { (fmt::celsius(t)) },
                        None => span.v { (DASH) },
                    }
                }
                (kit::kv_row(Some(Icon::BatteryBolt), "Time to Full", &eta("battery_time_to_full", fmt::Charge::Charging)))
                (kit::kv_row(Some(Icon::Battery(25)), "Time to Empty", &eta("battery_time_to_empty", fmt::Charge::Discharging)))
            }
            (age_line(v, POWER))
        }
    }
}

// --- CPU & temperatures ---

pub const HEALTH: [Fragment; 3] = [
    Fragment { id: "hl-cpu", topics: &[Topic::Health], render: hl_cpu },
    Fragment { id: "hl-temps", topics: &[Topic::Health, Topic::Battery], render: hl_temps },
    Fragment { id: "hl-all", topics: &[Topic::Health], render: hl_all },
];

/// The app's name for a thermal zone (`TemperatureSensor.label`).
pub fn sensor_label(name: &str) -> String {
    if let Some(n) = name.strip_prefix("cpuss-") {
        return format!("CPU sensor {n}");
    }
    if let Some(n) = name.strip_prefix("sys-therm-") {
        return format!("Board sensor {n}");
    }
    match name {
        "battery" => "Battery",
        "usb" => "USB",
        "pmx75_tz" => "Power chip \u{b7} PMX75",
        "pmg1110_tz" => "Power chip \u{b7} PMG1110",
        "pm7550ba_tz" => "Power chip \u{b7} PM7550BA",
        "xo-therm" => "Crystal oscillator",
        other => other,
    }
    .to_owned()
}

/// The sensors of the health reading as the app lists them: named, not
/// the `zeroc` reference zone, each zone once; a reading outside -40..150 °C
/// is none.
fn sensors(v: &Views) -> Vec<(&str, Option<f64>)> {
    let mut seen: Vec<&str> = Vec::new();
    let list = v.value(Topic::Health).and_then(|h| h["thermal"]["sensors"].as_array());
    list.into_iter()
        .flatten()
        .filter_map(|s| {
            let (id, name) = (s["id"].as_str()?, s["name"].as_str()?);
            if name == "zeroc" || seen.contains(&id) {
                return None;
            }
            seen.push(id);
            Some((name, fmt::valid_celsius(fmt::float(&s["temperature_c"]))))
        })
        .collect()
}

/// When the last good thermal reading came in (the health topic's read
/// time when it carried the thermal part and no error).
fn thermal_at(v: &Views) -> Option<BootInstant> {
    let s = v.slot(Topic::Health)?;
    (s.error.is_none() && s.value.as_deref().is_some_and(|h| h["thermal"].is_object())).then_some(s.at)
}

fn cpu_temp(v: &Views) -> Option<f64> {
    v.value(Topic::Health).and_then(|h| fmt::cpu_temp(&h["thermal"]))
}

fn hl_cpu(v: &Views) -> Markup {
    html! {
        section #hl-cpu .card .ctr {
            (kit::icon(Icon::Cpu))
            p.big { (roll(v, "cpu", &percent(cpu_usage(v)))) }
            p.cap.num { (roll(v, "ct", &dash_or(cpu_temp(v).map(fmt::celsius)))) }
            p.cap { "CPU" }
            (age_line(v, &[Topic::Health]))
        }
    }
}

/// A temperature row: "—" once the reading is 10 s old, "Unavailable" when
/// it is fresh but the sensor reports nothing valid; `key` rolls it.
fn temp_row(v: &Views, key: &str, i: Icon, label: &str, value: Option<f64>, fresh: bool) -> Markup {
    html! {
        div {
            dt { (kit::icon(i)) " " (label) }
            @match (fresh, value) {
                (true, None) => dd.muted { "Unavailable" },
                (true, Some(t)) => dd { b { (roll(v, key, &fmt::celsius(t))) } },
                (false, _) => dd { b { (DASH) } },
            }
        }
    }
}

fn hl_temps(v: &Views) -> Markup {
    let at = thermal_at(v);
    let age = at.map(|at| v.now.saturating_duration_since(at).as_secs());
    let thermal_fresh = age.is_some_and(|s| s < 10);
    let battery = v.value(Topic::Battery).and_then(fmt::battery_celsius);
    let summary = |name: &str| name == "usb" || name == "pmx75_tz" || name.starts_with("sys-therm-");
    html! {
        div #hl-temps {
            dl.kv.tk {
                (temp_row(v, "tc", Icon::Cpu, "CPU \u{b7} hottest sensor", cpu_temp(v), thermal_fresh))
                (temp_row(v, "tb", Icon::Battery(100), "Battery", battery, fresh(v, Topic::Battery)))
                @for (i, (name, t)) in sensors(v).into_iter().filter(|(n, _)| summary(n)).enumerate() {
                    (temp_row(v, &format!("ts{}", letters(i)), Icon::Thermometer, &sensor_label(name), t, thermal_fresh))
                }
            }
            p.cap {
                @match age {
                    None => "Temperature readings unavailable \u{b7} retrying automatically",
                    Some(s) if s < 10 => { "Updated " (s) "s ago \u{b7} refreshes every 2s" },
                    Some(s) => { "Last sample " (s) "s ago \u{b7} waiting for device" },
                }
            }
        }
    }
}

/// Every sensor with its raw name under the label when they differ.
fn hl_all(v: &Views) -> Markup {
    html! {
        dl #hl-all .kv .tk {
            @for (i, (name, t)) in sensors(v).into_iter().enumerate() {
                @let label = sensor_label(name);
                div {
                    dt { (label) @if label != name { small.lbl { (name) } } }
                    @match t { Some(t) => dd { (roll(v, &format!("ta{}", letters(i)), &fmt::celsius(t))) }, None => dd.muted { "Unavailable" } }
                }
            }
        }
    }
}

pub fn health(pd: &PageData) -> Markup {
    health_body(pd.views)
}

/// "All sensors" is a `<details>` around its fragment, always in the page
/// (so live updates find it) and hidden by CSS while the list is empty
/// (the app shows it when there are sensors).
pub fn health_body(v: &Views) -> Markup {
    let temps = html! {
        (hl_temps(v))
        details { summary { "All sensors" } (hl_all(v)) }
        p.cap {
            "CPU shows the hottest CPU zone. Board numbers follow the device\u{2019}s sensor names. Unavailable \
             means the device is not reporting a valid temperature."
        }
    };
    html! {
        (render::live_snapshot("/ui/health"))
        div.dash {
            (hl_cpu(v))
            p.ctr { a.nl href="/ui/health/cpu" { "CPU, memory & processes" } }
            (kit::card(None, None, Icon::Thermometer, "Temperatures", temps))
        }
    }
}

// --- CPU & Memory ---

pub const CPU: [Fragment; 3] = [
    Fragment { id: "hc-use", topics: &[Topic::Health], render: hc_use },
    Fragment { id: "hc-cores", topics: &[Topic::Health], render: hc_cores },
    Fragment { id: "hc-temp", topics: &[Topic::Health], render: hc_temp },
];

fn live_row(id: &str, i: Icon, label: &str, value: Markup) -> Markup {
    html! { li.it id=(id) { (kit::icon(i)) span { (label) } span.v { (value) } } }
}

fn hc_use(v: &Views) -> Markup {
    live_row("hc-use", Icon::Cpu, "CPU Usage", roll(v, "cpu", &percent(cpu_usage(v))))
}

/// The cores the kernel reports (the app assumes 4).
fn hc_cores(v: &Views) -> Markup {
    let n = v.value(Topic::Health).and_then(|h| h["cpu"]["cores"].as_array()).map(Vec::len).filter(|n| *n > 0);
    live_row("hc-cores", Icon::Grid, "CPU Cores", html! { (dash_or(n.map(|n| n.to_string()))) })
}

fn hc_temp(v: &Views) -> Markup {
    live_row("hc-temp", Icon::Thermometer, "CPU \u{b7} hottest sensor", roll(v, "tc", &dash_or(cpu_temp(v).map(fmt::celsius))))
}

/// Uptime and memory from `/api/device/system` at page load (read 0), the
/// bloat summary from `/api/system/top` (read 1). "Memory Free" is the
/// memory available to programs (MemAvailable; the app shows MemFree,
/// which leaves out reclaimable caches).
pub fn cpu(pd: &PageData) -> Markup {
    let v = pd.views;
    let sys = pd.read(0);
    let mem = |key: &str| sys.and_then(|s| s["memory"][key].as_u64());
    let (total, avail) = (mem("total").filter(|t| *t > 0), mem("available"));
    let used = total.zip(avail).map(|(t, a)| format!("{:.0}%", t.saturating_sub(a) as f64 / t as f64 * 100.0));
    let uptime = sys.and_then(|s| s["uptime"].as_u64()).map(|s| fmt::eta((s / 60) as i64));
    html! {
        (render::live_snapshot("/ui/health/cpu"))
        section.group {
            ul.list {
                (hc_use(v))
                (hc_cores(v))
                (hc_temp(v))
                (kit::kv_row(Some(Icon::Clock), "Uptime", &dash_or(uptime)))
                (kit::kv_row(Some(Icon::Memory), "Memory Total", &dash_or(total.map(fmt::memory))))
                (kit::kv_row(Some(Icon::Memory), "Memory Free", &dash_or(avail.map(fmt::memory))))
                @if let Some(u) = used { (kit::kv_row(Some(Icon::Chart), "Memory Used", &u)) }
                (super::processes::summary_row(pd.read(1)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::hub::SlotView;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use std::time::Duration;

    const NOW: BootInstant = BootInstant::from_duration(Duration::from_secs(10_000));

    /// A read `secs` before now.
    fn slot(value: Value, secs: u64) -> SlotView {
        let at = BootInstant::from_duration(Duration::from_secs(10_000 - secs));
        SlotView { version: 1, at, value_at: at, value: Some(Arc::new(value)), error: None }
    }

    fn views(pairs: Vec<(Topic, Value)>) -> Views {
        Views::new(NOW, pairs.into_iter().map(|(t, v)| (t, slot(v, 0))).collect())
    }

    fn thermal() -> Value {
        json!({"cpu": {"overall": 12.34, "cores": [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]}, "thermal": {"cpuss_temp": 41.0, "sensors": [
            {"id": "thermal_zone0", "name": "cpuss-0", "temperature_c": 40.1},
            {"id": "thermal_zone1", "name": "cpuss-1", "temperature_c": 41.5},
            {"id": "thermal_zone2", "name": "usb", "temperature_c": 33.0},
            {"id": "thermal_zone3", "name": "sys-therm-2", "temperature_c": null},
            {"id": "thermal_zone4", "name": "pmx75_tz", "temperature_c": 38.25},
            {"id": "thermal_zone5", "name": "xo-therm", "temperature_c": 35.0},
            {"id": "thermal_zone6", "name": "zeroc", "temperature_c": 0.0},
            {"id": "thermal_zone1", "name": "cpuss-1", "temperature_c": 20.0},
            {"id": "thermal_zone7", "name": "<b>odd</b>", "temperature_c": 999},
        ]}})
    }

    #[test]
    fn battery_rows_follow_the_app() {
        let v = views(vec![
            (Topic::Battery, json!({"status": "Charging", "capacity": 69, "voltage_uv": 4_435_000, "current_ua": 5_680_000, "temperature": 260})),
            (Topic::Charger, json!({"charger_connect": 1, "charge_status": 1, "direct_power_supply_mode": "disable"})),
            (Topic::BatteryInfo, json!({"battery_time_to_full": 37, "battery_time_to_empty": 1240})),
        ]);
        let s = battery_rows(&v).into_string();
        let rows: Vec<(&str, &str)> = vec![
            ("Capacity", "69%"), ("Status", "Charging"), ("Voltage", "4.435 V"), ("Current", "5680 mA"), ("Power", "25.2 W"),
            ("Temperature", "26.0 °C"), ("Time to Full", "37m"), ("Time to Empty", "\u{2014}"),
        ];
        let mut at = 0;
        for (label, value) in rows {
            // The temperature in the primary colour, as the app's.
            let class = if label == "Temperature" { "v fg" } else { "v" };
            let row = format!("<span>{label}</span><span class=\"{class}\">{value}</span>");
            let i = s[at..].find(&row).unwrap_or_else(|| panic!("{row} in order in {s}"));
            at += i;
        }
        assert_eq!(s.matches("<li class=\"it\">").count(), 8);
        // Discharging: the time to empty, a signed current; no time to full.
        let v = views(vec![
            (Topic::Battery, json!({"status": "Discharging", "capacity": 40, "voltage_uv": 3_900_000, "current_ua": -480_000})),
            (Topic::BatteryInfo, json!({"battery_time_to_full": 37, "battery_time_to_empty": 1240})),
        ]);
        let s = battery_rows(&v).into_string();
        for want in [">Discharging<", ">-480 mA<", ">1.9 W<", ">3.900 V<", ">20h 40m<", "Time to Full</span><span class=\"v\">\u{2014}"] {
            assert!(s.contains(want), "{want} in {s}");
        }
        // Unknown voltage and current: no Voltage, Current or Power rows.
        let s = battery_rows(&views(vec![(Topic::Battery, json!({"status": "Full", "capacity": 100}))])).into_string();
        assert!(s.contains(">Full<") && !s.contains("Voltage") && !s.contains("Current") && !s.contains("Power"), "{s}");
        assert_eq!(s.matches("<li class=\"it\">").count(), 5);
        let s = battery_rows(&views(vec![(Topic::Battery, json!({"capacity": 50, "current_ua": 120_000}))])).into_string();
        assert!(s.contains(">Unavailable<") && s.contains(">120 mA<") && !s.contains("Power"), "{s}");
        // Nothing read yet.
        let s = battery_rows(&views(vec![])).into_string();
        assert!(s.contains("Capacity</span><span class=\"v\">\u{2014}") && s.contains(">Unavailable<"), "{s}");
    }

    #[test]
    fn sensors_are_labelled_like_the_app() {
        for (name, label) in [
            ("cpuss-3", "CPU sensor 3"), ("sys-therm-1", "Board sensor 1"), ("battery", "Battery"), ("usb", "USB"),
            ("pmx75_tz", "Power chip \u{b7} PMX75"), ("pmg1110_tz", "Power chip \u{b7} PMG1110"),
            ("pm7550ba_tz", "Power chip \u{b7} PM7550BA"), ("xo-therm", "Crystal oscillator"), ("modem-0", "modem-0"),
        ] {
            assert_eq!(sensor_label(name), label);
        }
        let v = views(vec![(Topic::Health, thermal())]);
        let names: Vec<&str> = sensors(&v).into_iter().map(|(n, _)| n).collect();
        // zeroc and a repeated zone are left out; 999 °C is no reading.
        assert_eq!(names, ["cpuss-0", "cpuss-1", "usb", "sys-therm-2", "pmx75_tz", "xo-therm", "<b>odd</b>"]);
        let all = hl_all(&v).into_string();
        assert!(all.contains("<dt>CPU sensor 0<small class=\"lbl\">cpuss-0</small></dt><dd>40.1 °C</dd>"), "{all}");
        assert!(all.contains("<dt>Board sensor 2<small class=\"lbl\">sys-therm-2</small></dt><dd class=\"muted\">Unavailable</dd>"));
        // The raw name only when it differs; device text escaped.
        assert!(all.contains("<dt>&lt;b&gt;odd&lt;/b&gt;</dt><dd class=\"muted\">Unavailable</dd>"), "{all}");
    }

    /// The temperature rows show "—" once the reading is 10 s old and
    /// "Unavailable" for a fresh reading without a value.
    #[test]
    fn temperatures_blank_after_10_s_and_say_unavailable() {
        let at = |health_secs, battery_secs| {
            Views::new(NOW, vec![
                (Topic::Health, slot(thermal(), health_secs)),
                (Topic::Battery, slot(json!({"temperature": 260}), battery_secs)),
            ])
        };
        let s = hl_temps(&at(3, 3)).into_string();
        for want in [
            "CPU · hottest sensor</dt><dd><b>41.5 °C</b>", "Battery</dt><dd><b>26.0 °C</b>", "USB</dt><dd><b>33.0 °C</b>",
            "Board sensor 2</dt><dd class=\"muted\">Unavailable</dd>", "Power chip · PMX75</dt><dd><b>38.2 °C</b>",
            "Updated 3s ago · refreshes every 2s",
        ] {
            assert!(s.contains(want), "{want} in {s}");
        }
        // Only the summary sensors: no CPU zones or crystal oscillator rows.
        assert!(!s.contains("CPU sensor") && !s.contains("Crystal"), "{s}");
        let s = hl_temps(&at(10, 3)).into_string();
        assert!(s.contains("CPU · hottest sensor</dt><dd><b>—</b>") && s.contains("Board sensor 2</dt><dd><b>—</b>"), "{s}");
        assert!(s.contains("Battery</dt><dd><b>26.0 °C</b>") && s.contains("Last sample 10s ago · waiting for device"), "{s}");
        let s = hl_temps(&at(3, 12)).into_string();
        assert!(s.contains("Battery</dt><dd><b>—</b>") && s.contains("Updated 3s ago"), "{s}");
        // A failed read is not fresh, whatever its time.
        let mut v = at(1, 1);
        v.set(Topic::Battery, SlotView { error: Some(Arc::from("no battery")), ..slot(json!({"temperature": 260}), 1) });
        assert!(hl_temps(&v).into_string().contains("Battery</dt><dd><b>—</b>"));
        let mut v = at(1, 1);
        v.set(Topic::Health, SlotView { error: Some(Arc::from("thermal")), ..slot(json!({"cpu": {}, "thermal": null}), 1) });
        let s = hl_temps(&v).into_string();
        assert!(s.contains("Temperature readings unavailable · retrying automatically") && s.contains("<dd><b>—</b>"), "{s}");
        // Nothing read yet.
        let s = hl_temps(&views(vec![])).into_string();
        assert!(s.contains("Temperature readings unavailable") && s.contains("CPU · hottest sensor</dt><dd><b>—</b>"), "{s}");
    }

    #[test]
    fn health_page_keeps_details_outside_its_fragments() {
        let v = views(vec![(Topic::Health, thermal()), (Topic::Battery, json!({"temperature": 260}))]);
        let page = health_body(&v).into_string();
        for f in &HEALTH {
            let html = (f.render)(&v).into_string();
            assert!(page.contains(&html), "{} differs from the page", f.id);
            assert!(!html.contains("<details") && !html.contains("<summary"), "{}", f.id);
        }
        // The fragment sits inside the disclosure, never around it.
        assert!(page.contains("<details><summary>All sensors</summary><dl class=\"kv tk\" id=\"hl-all\">"), "{page}");
        for want in [
            "<section class=\"card ctr\" id=\"hl-cpu\">", "<p class=\"big\">12.3%</p>", "<p class=\"cap num\">41.5 °C</p>",
            "<a class=\"nl\" href=\"/ui/health/cpu\">CPU, memory &amp; processes</a>", "Temperatures",
            "CPU shows the hottest CPU zone. Board numbers follow the device’s sensor names.",
        ] {
            assert!(page.contains(want), "{want} in {page}");
        }
        // No sensors: an empty list, which CSS hides with its "All sensors"
        // (the app shows it only with sensors); live updates still find it.
        let none = health_body(&views(vec![(Topic::Health, json!({"cpu": {"overall": 5}, "thermal": {"sensors": []}}))])).into_string();
        assert!(none.contains("<details><summary>All sensors</summary><dl class=\"kv tk\" id=\"hl-all\"></dl></details>"), "{none}");
        assert!(none.contains("<p class=\"cap num\">—</p>"), "{none}");
        assert!(include_str!("../assets/app.css").contains("details:has(>#hl-all:empty) { display: none; }"));
    }

    #[test]
    fn cpu_and_memory_rows() {
        let v = views(vec![(Topic::Health, thermal())]);
        let sys = json!({"uptime": 93_784, "memory": {"total": 3_221_225_472u64, "free": 104_857_600, "available": 1_610_612_736}});
        let reads = [Ok(sys), Ok(json!({"bloat_count": 4, "bloat_cpu_pct": 1.6, "processes": []}))];
        use crate::auth::{Kind, PeerKey};
        let app = crate::handlers::AppState::new();
        app.auth.set_password("pw");
        let issued = app.auth.login("pw", Kind::Browser, PeerKey::UNKNOWN, &[]).unwrap();
        let p = app.auth.authenticate(Kind::Browser, issued.token.expose()).unwrap();
        let pd = PageData { app: &app, p: &p, views: &v, reads: &reads, remote: false, form: None, query: None, once: None, at: None, ussd: None, notice: None, token: Default::default(), prefs: Default::default(), secure: false };
        let s = cpu(&pd).into_string();
        for (label, value) in [
            ("CPU Usage", "12.3%"), ("CPU Cores", "6"), ("CPU · hottest sensor", "41.5 °C"), ("Uptime", "1d 2h 3m"),
            ("Memory Total", "3.0 GB"), ("Memory Free", "1.5 GB"), ("Memory Used", "50%"),
        ] {
            assert!(s.contains(&format!("<span>{label}</span><span class=\"v\">{value}</span>")), "{label} {value} in {s}");
        }
        // Memory Free is the available memory (MemAvailable), not MemFree.
        assert!(!s.contains("100 MB"));
        assert!(s.contains("<li class=\"it\" id=\"hc-cores\">"));
        // The app's Processes row: the bloat summary in orange, a link to the list.
        assert!(s.contains("<a class=\"it\" href=\"/ui/health/processes\">"), "{s}");
        assert!(s.contains("<span>Processes</span><span class=\"v c-orange\">4 bloat 2%</span></a>"), "{s}");
        for f in &CPU {
            assert!(s.contains(&(f.render)(&v).into_string()), "{}", f.id);
        }
        // A failed read: dashes, no "Memory Used".
        let reads = [Err("no /proc".to_owned()), Err("no /proc".to_owned())];
        let empty = views(vec![]);
        let pd = PageData { app: &app, p: &p, views: &empty, reads: &reads, remote: false, form: None, query: None, once: None, at: None, ussd: None, notice: None, token: Default::default(), prefs: Default::default(), secure: false };
        let s = cpu(&pd).into_string();
        for label in ["CPU Usage", "CPU Cores", "Uptime", "Memory Total", "Memory Free"] {
            assert!(s.contains(&format!("<span>{label}</span><span class=\"v\">—</span>")), "{label} in {s}");
        }
        assert!(!s.contains("Memory Used"));
        // Without a summary the row still opens the list.
        assert!(s.contains("<span>Processes</span></a>"), "{s}");
    }

    #[test]
    fn traffic_sheet_is_the_card_without_a_link() {
        let v = views(vec![(Topic::Traffic, json!({"real_rx_speed": 52, "real_tx_speed": 69}))]);
        let s = traffic_body(&v).into_string();
        assert!(s.contains("<section class=\"card\" id=\"card-traffic\">") && !s.contains("<a class=\"card\""), "{s}");
        assert!(s.contains("416.0 b/s") && s.contains("A speed test reports throughput over its test period"));
        assert!(dashboard::card_traffic(&v).into_string().starts_with("<a class=\"card\" id=\"card-traffic\" href=\"/ui/traffic\">"));
    }
}
