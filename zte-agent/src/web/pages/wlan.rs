//! Router › WiFi (iOS `WiFiSettingsView`) and Guest WiFi
//! (`GuestWiFiSettingsView`): the app's forms, read at page load (no
//! stream: the read carries the passwords), applied through
//! `pages::netcfg` (asked first: WiFi restarts and devices disconnect).
//!
//! - Passwords are never drawn (owner decision): the read keeps only
//!   whether one is set, the field is empty with "Leave empty to keep", and
//!   an empty field keeps the router's.
//! - Only what changed is sent, against the state read when sending, so a
//!   value the router holds that the web would refuse is never rewritten by
//!   an Apply that leaves it. The agent strips ' " ; $ ` \ | < > & from every
//!   value, so a new network name or password with one is refused (the app
//!   would send it, changed).
//! - Checks (web additions; the app checks nothing): a network name of 1 to
//!   32 bytes, a WPA password of 8 to 63 printable characters or 64 hex
//!   digits (WPA3 only the first: it takes no hex key), a password for a
//!   secured network, the app's 5 GHz
//!   channel/bandwidth table (the app resets a mismatch to Auto). Choices
//!   are the app's lists plus the router's value when it is not one of
//!   them (kept unless another is chosen; the app shows it as Auto).
//! - The fields of a radio that is off, and everything while WiFi is off,
//!   are hidden by CSS (as the app hides them; without `:has()` they show)
//!   and not sent. Footers follow the values the page was drawn with.
//! - After an Apply that restarts WiFi (every Guest WiFi Apply does: the
//!   agent reloads all of Wi-Fi), "Reconnect to …" (no device read).
//! - The question says when a network becomes open (no password).

use maud::{html, Markup};
use serde_json::{Map, Value};

use super::confirm::{asking, nothing, Bad, Question};
use super::netcfg::{
    check as checkbox, choice, failed, field, foot, form_error, options, password_row, select_row, switch, text, text_row, truthy, val, Done,
    Vals, KEPT,
};
use crate::handlers::AppState;
use crate::server::Method;
use crate::web::fmt;
use crate::web::kit;
use crate::web::render::once_field;
use crate::web::screen::{FormState, PageData};
use crate::web::store::Claim;

pub const WIFI: &str = "/ui/router/wifi";
pub const APPLY: &str = "/ui/router/wifi/apply";
pub const APPLIED: &str = "/ui/router/wifi/applied";
pub const GUEST: &str = "/ui/router/guest-wifi";
pub const GUEST_APPLY: &str = "/ui/router/guest-wifi/apply";
pub const GUEST_APPLIED: &str = "/ui/router/guest-wifi/applied";

/// The app's lists (`WiFiConfig`).
const CH_2G: [&str; 12] = ["auto", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"];
const CH_5G: [&str; 25] = [
    "auto", "36", "40", "44", "48", "52", "56", "60", "64", "100", "104", "108", "112", "116", "120", "124", "128", "132", "136", "140", "149",
    "153", "157", "161", "165",
];
const BW_2G: [&str; 3] = ["auto", "EHT20", "EHT40"];
const BW_5G: [&str; 5] = ["auto", "EHT20", "EHT40", "EHT80", "EHT160"];
const TX: [&str; 10] = ["10", "20", "30", "40", "50", "60", "70", "80", "90", "100"];
const ENC: [&str; 7] = ["none", "psk+tkip", "psk+ccmp", "psk2+ccmp", "psk-mixed+ccmp", "sae", "sae-mixed"];

fn enc_label(e: &str) -> String {
    match e {
        "none" => "None",
        "psk+tkip" => "WPA-PSK (TKIP)",
        "psk+ccmp" => "WPA-PSK (AES)",
        "psk2+ccmp" => "WPA2-PSK (AES)",
        "psk-mixed+ccmp" => "WPA/WPA2 Mixed",
        "sae" => "WPA3-SAE",
        "sae-mixed" => "WPA2/WPA3 Mixed",
        _ => e,
    }
    .to_owned()
}

fn bw_label(bw: &str) -> String {
    match bw {
        "auto" => "Auto",
        "EHT20" => "20 MHz",
        "EHT40" => "40 MHz",
        "EHT80" => "80 MHz",
        "EHT160" => "160 MHz",
        _ => bw,
    }
    .to_owned()
}

fn ch_label(ch: &str) -> String {
    if ch == "auto" {
        "Auto".into()
    } else {
        format!("Ch {ch}")
    }
}

fn tx_label(tx: &str) -> String {
    format!("{tx}%")
}

/// The app's `bandwidths5g(for:)`: the 5 GHz bandwidths a channel allows.
fn bandwidths_5g(ch: &str) -> &'static [&'static str] {
    match ch.parse::<u32>() {
        Ok(165) => &["auto", "EHT20"],
        Ok(c) if c >= 149 => &["auto", "EHT20", "EHT40", "EHT80"],
        Ok(c) if c >= 132 => &["auto", "EHT20", "EHT40"],
        _ => &BW_5G,
    }
}

/// UCI's channel "" or "0" is Auto (the app's `normalizeChannel`).
fn norm_channel(ch: &str) -> String {
    if ch.is_empty() || ch == "0" {
        "auto".into()
    } else {
        ch.to_owned()
    }
}

/// TX power of 20 % or less (the app's footer).
fn low(tx: &str) -> bool {
    tx.parse::<u32>().is_ok_and(|t| t <= 20)
}

/// One radio's fields: the form's names are the agent's, but `radio` (on).
struct Band {
    label: &'static str,
    radio: &'static str,
    disabled: &'static str,
    ssid: &'static str,
    key: &'static str,
    channel: &'static str,
    htmode: &'static str,
    txpower: &'static str,
    encryption: &'static str,
    hidden: &'static str,
    has_key: &'static str,
    channels: &'static [&'static str],
    bws: &'static [&'static str],
    five: bool,
}

const BANDS: [Band; 2] = [
    Band {
        label: "2.4 GHz",
        radio: "r2",
        disabled: "radio2_disabled",
        ssid: "ssid_2g",
        key: "key_2g",
        channel: "channel_2g",
        htmode: "htmode_2g",
        txpower: "txpower_2g",
        encryption: "encryption_2g",
        hidden: "hidden_2g",
        has_key: "has_key_2g",
        channels: &CH_2G,
        bws: &BW_2G,
        five: false,
    },
    Band {
        label: "5 GHz",
        radio: "r5",
        disabled: "radio5_disabled",
        ssid: "ssid_5g",
        key: "key_5g",
        channel: "channel_5g",
        htmode: "htmode_5g",
        txpower: "txpower_5g",
        encryption: "encryption_5g",
        hidden: "hidden_5g",
        has_key: "has_key_5g",
        channels: &CH_5G,
        bws: &BW_5G,
        five: true,
    },
];

const STRIPPED: [char; 10] = ['\'', '"', ';', '$', '`', '\\', '|', '<', '>', '&'];

/// A network name's message, if it cannot be used (`router`: the current
/// one, which may keep characters the agent would strip from a new one).
fn ssid_error(ssid: &str, router: &str) -> Option<&'static str> {
    if ssid.trim().is_empty() {
        Some("Enter a network name.")
    } else if ssid.len() > 32 {
        Some("Use at most 32 bytes (accented letters and emoji take more than one).")
    } else if ssid.chars().any(char::is_control) {
        Some("Use one line, without control characters.")
    } else if ssid != router && ssid.contains(&STRIPPED[..]) {
        Some("The router can't store these characters: ' \" ; $ ` \\ | < > &")
    } else {
        None
    }
}

/// A WPA password: 8 to 63 printable ASCII characters, or 64 hex digits.
fn wpa_ok(key: &str) -> bool {
    (key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())) || ((8..=63).contains(&key.len()) && key.bytes().all(|b| (0x20..=0x7e).contains(&b)))
}

/// WPA3 (SAE, alone or mixed): the network takes only a passphrase, not a
/// 64-hex key (hostapd would start it without SAE).
fn sae(enc: &str) -> bool {
    enc.starts_with("sae")
}

const SAE_KEY: &str = "WPA3 needs a password of 8 to 63 characters.";

/// A typed password's message, if it cannot be used with `enc`.
fn key_error(key: &str, enc: &str) -> Option<&'static str> {
    if sae(enc) && key.len() == 64 && wpa_ok(key) {
        Some(SAE_KEY)
    } else if !wpa_ok(key) {
        Some("Use 8 to 63 characters (letters, digits, spaces or symbols), or 64 hexadecimal digits.")
    } else if key.contains(&STRIPPED[..]) {
        Some("The router can't store these characters in a password: ' \" ; $ ` \\ | < > &")
    } else {
        None
    }
}

const NO_PASSWORD: &str = "Enter a password for this security type.";

/// "1" or "0" (the agent's switches).
fn bit(on: bool) -> &'static str {
    if on {
        "1"
    } else {
        "0"
    }
}

/// A status read without its passwords: only whether each is set
/// (`has_key*`). The app's check: no `htmode_2g` is a failed load.
fn without_keys(mut st: Value, keys: &[(&str, &str)], required: &str) -> Result<Value, String> {
    let Some(o) = st.as_object_mut().filter(|o| o.contains_key(required)) else { return Err("unexpected answer".into()) };
    for (key, has) in keys {
        let set = o.remove(*key).is_some_and(|k| !text(&k).is_empty());
        o.insert((*has).into(), set.into());
    }
    Ok(st)
}

/// WiFi's read at page load (`GET /api/wifi/status` without the keys).
pub fn settings(app: &AppState, _: Option<&str>) -> Result<Value, String> {
    without_keys(super::wifi::status(app)?, &[("key_2g", "has_key_2g"), ("key_5g", "has_key_5g")], "htmode_2g")
}

/// Guest WiFi's (`GET /api/wifi/guest` without the key).
pub fn guest_settings(app: &AppState, _: Option<&str>) -> Result<Value, String> {
    without_keys(super::wifi::guest_status(app)?, &[("key", "has_key")], "ssid")
}

// --- WiFi ---

pub fn page(pd: &PageData) -> Markup {
    let Some(st) = pd.read(0) else {
        return html! { p.msg.err role="alert" { "Failed to load WiFi settings" } };
    };
    let flag = |name: &str, router: bool| pd.form.map_or(router, |f| f.value(name) == Some("1"));
    html! {
        (form_error(pd))
        form.rf.wsf method="post" action=(APPLY) autocomplete="off" {
            (pd.csrf())
            (once_field(&pd.once()))
            (kit::section(None, None, switch("wifi", "WiFi", flag("wifi", truthy(&st["wifi_onoff"])), "it", "sr")))
            div.wb {
                @for b in &BANDS { (band(pd, st, b)) }
                (kit::section(Some("Advanced"), None, switch("wifi6_switch", "WiFi 7 (802.11be)", flag("wifi6_switch", truthy(&st["wifi6_switch"])), "it", "sr")))
            }
            (kit::section(None, Some(foot("Applying changes will briefly disconnect WiFi while settings are restarted")), html! {
                li { button.it.cv.snd type="submit" { "Apply" } }
            }))
        }
    }
}

fn band(pd: &PageData, st: &Value, b: &Band) -> Markup {
    let f = pd.form;
    let typed = |name: &str, router: String| f.and_then(|f| f.value(name)).map_or(router, str::to_owned);
    let err = |name: &str| f.and_then(|f| f.error(name));
    let on = f.map_or(!truthy(&st[b.disabled]), |f| f.value(b.radio) == Some("1"));
    let (ch0, bw0, tx0, enc0) = (norm_channel(&text(&st[b.channel])), text(&st[b.htmode]), text(&st[b.txpower]), text(&st[b.encryption]));
    let (bw, tx) = (typed(b.htmode, bw0.clone()), typed(b.txpower, tx0.clone()));
    let footers = html! {
        @if b.five {
            @match bw.as_str() {
                "EHT160" => (foot("160 MHz: channels 36\u{2013}64 or 100\u{2013}128")),
                "EHT80" => (foot("80 MHz: channels 36\u{2013}64, 100\u{2013}128, or 149\u{2013}161")),
                "EHT20" => (foot("20 MHz on 5 GHz is very narrow \u{2014} some clients may fail to connect or have poor performance. Use 80 MHz or wider for best compatibility.")),
                _ => {}
            }
            @if low(&tx) { (foot("Low TX power on 5 GHz may prevent clients from connecting, especially with wider bandwidths")) }
        } @else if low(&tx) {
            (foot("Low TX power may reduce range and prevent some clients from connecting."))
        }
    };
    html! {
        div.band {
            (kit::section(Some(b.label), Some(footers), html! {
                (switch(b.radio, "Radio", on, "it", "sr ro"))
                (text_row(b.ssid, "SSID", &typed(b.ssid, text(&st[b.ssid])), err(b.ssid), "it fld bd", 32, None))
                (password_row(b.key, "Password", st[b.has_key].as_bool() == Some(true), f.and_then(|f| f.value(b.key)) == Some(KEPT), err(b.key), "it fld bd"))
                (select_row(b.channel, "Channel", &options(b.channels, &ch0, ch_label), &typed(b.channel, ch0.clone()), err(b.channel), "it bd"))
                (select_row(b.htmode, "Bandwidth", &options(b.bws, &bw0, bw_label), &bw, err(b.htmode), "it bd"))
                (select_row(b.txpower, "TX Power", &options(&TX, &tx0, tx_label), &tx, err(b.txpower), "it bd"))
                (select_row(b.encryption, "Encryption", &options(&ENC, &enc0, enc_label), &typed(b.encryption, enc0.clone()), err(b.encryption), "it bd"))
                (switch(b.hidden, "Hidden SSID", f.map_or(truthy(&st[b.hidden]), |f| f.value(b.hidden) == Some("1")), "it bd", "sr"))
            }))
        }
    }
}

/// The posted fields as typed (to show the form again): at most 64
/// characters each.
fn as_typed(pairs: &[(String, String)], names: &[&'static str]) -> Vals {
    names.iter().map(|n| (*n, field(pairs, n).unwrap_or("").chars().take(64).collect())).collect()
}

/// A password for a live band: the one typed (checked), else the one kept
/// from a refused post, else the router's must do.
fn key_of(typed: &str, kept: &str, router: &str, enc: &str, errors: &mut Vec<(&'static str, String)>, name: &'static str) -> String {
    if enc == "none" {
        return String::new();
    }
    let key = if typed.is_empty() { kept } else { typed };
    if key.is_empty() {
        if !wpa_ok(router) {
            errors.push((name, NO_PASSWORD.into()));
        } else if sae(enc) && router.len() == 64 {
            errors.push((name, format!("{SAE_KEY} Enter one: the router\u{2019}s is a 64-digit key.")));
        }
        return String::new();
    }
    match key_error(key, enc) {
        Some(e) => {
            errors.push((name, e.into()));
            String::new()
        }
        None => key.to_owned(),
    }
}

/// The WiFi form, checked against the router's state (read now): its
/// choices, and whether a secured network has a password.
pub fn check(app: &AppState, pairs: &[(String, String)], kept: &Vals) -> Result<Vals, Bad> {
    let cur = match super::wifi::status(app) {
        Ok(c) => c,
        Err(e) => {
            let mut values = as_typed(pairs, &["ssid_2g", "ssid_5g", "channel_2g", "channel_5g", "htmode_2g", "htmode_5g", "txpower_2g", "txpower_5g", "encryption_2g", "encryption_5g"]);
            for n in ["wifi", "wifi6_switch", "r2", "r5", "hidden_2g", "hidden_5g"] {
                values.push((n, checkbox(pairs, n)));
            }
            return Err(Bad::Fields(FormState { values, errors: vec![("form", format!("Failed to load WiFi settings: {}", fmt::truncate(&e, 120)))] }));
        }
    };
    let on = checkbox(pairs, "wifi");
    let mut vals: Vals = vec![("wifi", on.clone()), ("wifi6_switch", checkbox(pairs, "wifi6_switch"))];
    let mut errors = Vec::new();
    for b in &BANDS {
        let r_on = checkbox(pairs, b.radio);
        let channel = choice(pairs, b.channel, b.channels, &norm_channel(&text(&cur[b.channel])))?;
        let htmode = choice(pairs, b.htmode, b.bws, &text(&cur[b.htmode]))?;
        let txpower = choice(pairs, b.txpower, &TX, &text(&cur[b.txpower]))?;
        let enc = choice(pairs, b.encryption, &ENC, &text(&cur[b.encryption]))?;
        let ssid: String = field(pairs, b.ssid).unwrap_or("").chars().take(64).collect();
        let live = on == "1" && r_on == "1";
        let mut key = String::new();
        if live {
            if let Some(e) = ssid_error(&ssid, &text(&cur[b.ssid])) {
                errors.push((b.ssid, e.to_owned()));
            }
            key = key_of(field(pairs, b.key).unwrap_or(""), val(kept, b.key), &text(&cur[b.key]), &enc, &mut errors, b.key);
            let listed = b.channels.contains(&channel.as_str()) && b.bws.contains(&htmode.as_str());
            if b.five && listed && channel != "auto" && !bandwidths_5g(&channel).contains(&htmode.as_str()) {
                let max = bandwidths_5g(&channel).last().copied().unwrap_or("auto");
                errors.push((b.htmode, format!("Channel {channel} supports up to {}.", bw_label(max))));
            }
        }
        vals.extend([
            (b.radio, r_on),
            (b.ssid, ssid),
            (b.key, key),
            (b.channel, channel),
            (b.htmode, htmode),
            (b.txpower, txpower),
            (b.encryption, enc),
            (b.hidden, checkbox(pairs, b.hidden)),
        ]);
    }
    if errors.is_empty() {
        Ok(vals)
    } else {
        Err(Bad::Fields(FormState { values: vals, errors }))
    }
}

/// What differs from the router's state, under the agent's names: nothing
/// of a radio that is off, nothing but the switch while WiFi is off; a
/// password only when one was typed.
pub fn body(vals: &[(&'static str, String)], cur: &Value) -> Map<String, Value> {
    let mut b = Map::new();
    let mut set = |k: &str, v: &str| {
        b.insert(k.into(), Value::String(v.into()));
    };
    let on = val(vals, "wifi") == "1";
    if on != truthy(&cur["wifi_onoff"]) {
        set("wifi_onoff", bit(on));
    }
    if !on {
        return b;
    }
    let w7 = val(vals, "wifi6_switch") == "1";
    if w7 != truthy(&cur["wifi6_switch"]) {
        set("wifi6_switch", bit(w7));
    }
    for band in &BANDS {
        let r_on = val(vals, band.radio) == "1";
        if r_on == truthy(&cur[band.disabled]) {
            set(band.disabled, bit(!r_on));
        }
        if !r_on {
            continue;
        }
        for k in [band.ssid, band.htmode, band.txpower, band.encryption] {
            if val(vals, k) != text(&cur[k]) {
                set(k, val(vals, k));
            }
        }
        if val(vals, band.channel) != norm_channel(&text(&cur[band.channel])) {
            set(band.channel, val(vals, band.channel));
        }
        let hidden = val(vals, band.hidden) == "1";
        if hidden != truthy(&cur[band.hidden]) {
            set(band.hidden, bit(hidden));
        }
        if val(vals, band.encryption) != "none" && !val(vals, band.key).is_empty() {
            set(band.key, val(vals, band.key));
        }
    }
    b
}

/// The line of a network that is (or becomes) open.
fn open(ssid: &str) -> String {
    format!("\u{201c}{ssid}\u{201d} is an open network: anyone nearby can join it, without a password, and reach this router.")
}

const RESTART: &str = "WiFi restarts to apply the changes: every device on it disconnects for a moment, this browser too if it is on the router\u{2019}s WiFi.";

pub fn question(app: &AppState, vals: &[(&'static str, String)]) -> Question {
    let cur = match super::wifi::status(app) {
        Ok(c) => c,
        Err(e) => return nothing("Failed to load WiFi settings", vec![fmt::truncate(&e, 120)]),
    };
    let b = body(vals, &cur);
    if b.is_empty() {
        return nothing("Nothing to apply", vec!["These are the router\u{2019}s settings already.".into()]);
    }
    if val(vals, "wifi") != "1" {
        let lines = vec![
            "WiFi turns off: every device on it loses its connection, this browser too if it is on the router\u{2019}s WiFi.".into(),
            "To turn it back on, use the router\u{2019}s screen, or this page from a device connected by USB.".into(),
        ];
        return asking("Turn off WiFi?", lines, "Turn Off WiFi", false, false);
    }
    if b.keys().all(|k| k.starts_with("txpower_")) {
        let lines = vec!["TX power changes at once; WiFi does not restart.".into()];
        return Question { title: "Apply WiFi settings?".into(), lines, label: Some("Apply"), danger: false };
    }
    let mut lines = Vec::new();
    for band in &BANDS {
        let ssid = val(vals, band.ssid);
        let r_on = val(vals, band.radio) == "1";
        if b.contains_key(band.disabled) {
            lines.push(if r_on {
                format!("The {} network \u{201c}{ssid}\u{201d} turns on.", band.label)
            } else {
                format!("The {} network turns off: devices on it lose their connection.", band.label)
            });
        }
        if !r_on {
            continue;
        }
        let new_key = b.contains_key(band.key);
        if !b.contains_key(band.disabled) && (b.contains_key(band.ssid) || new_key || b.contains_key(band.encryption)) {
            let with = if new_key { " with the new password" } else { "" };
            lines.push(format!("On {}, devices must join \u{201c}{ssid}\u{201d} again{with}.", band.label));
        }
        if b.contains_key(band.hidden) && val(vals, band.hidden) == "1" {
            lines.push(format!("\u{201c}{ssid}\u{201d} is hidden: devices must type its name to join."));
        }
        if val(vals, band.encryption) == "none" && (b.contains_key(band.encryption) || b.contains_key(band.disabled)) {
            lines.push(open(ssid));
        }
    }
    lines.push(RESTART.into());
    asking("Apply WiFi settings?", lines, "Apply", false, false)
}

pub fn send(app: &AppState, vals: &[(&'static str, String)], claim: &mut Claim) -> Result<Done, (u16, String)> {
    let cur = super::wifi::status(app).map_err(|e| (503, failed(&e)))?;
    let b = body(vals, &cur);
    if b.is_empty() {
        return Ok(Done::Line(None, "No changes to apply.".into()));
    }
    claim.disarm();
    let data = super::call(app, Method::Put, "/api/wifi/settings", &Value::Object(b)).map_err(|(status, e)| (status, failed(&e)))?;
    if data["note"].as_str() == Some("no changes") {
        return Ok(Done::Line(None, "No changes to apply.".into()));
    }
    if data["hot"].as_bool() == Some(true) {
        return Ok(Done::Line(Some(true), "WiFi settings applied".into()));
    }
    Ok(Done::Page(Vec::new()))
}

/// "Reconnect to …" after an Apply that restarts WiFi (from the sent
/// draft; without one, the general words).
pub fn applied(vals: &[(&'static str, String)]) -> (String, Markup) {
    let on = vals.is_empty() || val(vals, "wifi") == "1";
    let live: Vec<&Band> = BANDS.iter().filter(|b| val(vals, b.radio) == "1" && !val(vals, b.ssid).is_empty()).collect();
    let mut names: Vec<&str> = live.iter().map(|b| val(vals, b.ssid)).collect();
    names.dedup();
    let title = match (on, names.as_slice()) {
        (false, _) => "WiFi is off".to_owned(),
        (true, [one]) => format!("Reconnect to \u{201c}{one}\u{201d}"),
        _ => "Reconnect to WiFi".to_owned(),
    };
    let body = html! {
        // The app's line for an Apply; WiFi turned off does not restart.
        @if on {
            p.msg.ok role="status" { "WiFi settings applied \u{2014} WiFi will restart briefly" }
        } @else {
            p.msg.ok role="status" { "WiFi turned off" }
        }
        div.narrow {
            ul.consequences {
                @if on {
                    li { "WiFi is restarting. If this device is on the router\u{2019}s WiFi, it disconnects for a moment." }
                    @for b in &live {
                        li {
                            "Join \u{201c}" (val(vals, b.ssid)) "\u{201d} (" (b.label) ")"
                            @if val(vals, if b.five { "set_5g" } else { "set_2g" }) == "1" { " with the new password" }
                            " if it does not reconnect by itself."
                        }
                    }
                    @if vals.is_empty() { li { "Join the router\u{2019}s WiFi again if this device does not reconnect by itself." } }
                } @else {
                    li { "Devices on the router\u{2019}s WiFi lost their connection. Turn WiFi back on from the router\u{2019}s screen, or here from a device connected by USB." }
                }
            }
            p.actions { a.btn href=(WIFI) { "Back to WiFi Settings" } }
        }
    };
    (title, body)
}

// --- Guest WiFi ---

/// The app's Auto-Shutoff choices (minutes).
const TIMES: [(&str, &str); 8] = [
    ("0", "No Limit"),
    ("30", "30 min"),
    ("60", "1 hour"),
    ("120", "2 hours"),
    ("240", "4 hours"),
    ("480", "8 hours"),
    ("720", "12 hours"),
    ("1440", "24 hours"),
];
const TIME_VALUES: [&str; 8] = ["0", "30", "60", "120", "240", "480", "720", "1440"];

fn time_label(m: &str) -> String {
    TIMES.iter().find(|(v, _)| *v == m).map_or_else(|| format!("{m} min"), |(_, l)| (*l).to_owned())
}

/// The minutes, "0" when unset.
fn minutes(v: &Value) -> String {
    fmt::int(v).unwrap_or(0).to_string()
}

/// The app's countdown words ("1h 05m 09s remaining").
fn remaining(secs: i64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s remaining")
    } else {
        format!("{m}m {s:02}s remaining")
    }
}

pub fn guest_page(pd: &PageData) -> Markup {
    let Some(st) = pd.read(0) else {
        return html! { p.msg.err role="alert" { "Failed to load guest WiFi settings" } };
    };
    let f = pd.form;
    let typed = |name: &str, router: String| f.and_then(|f| f.value(name)).map_or(router, str::to_owned);
    let flag = |name: &str, router: bool| f.map_or(router, |f| f.value(name) == Some("1"));
    let err = |name: &str| f.and_then(|f| f.error(name));
    let (g2, g5) = (flag("g2", !truthy(&st["disabled_2g"])), flag("g5", !truthy(&st["disabled_5g"])));
    let (time0, enc0) = (minutes(&st["guest_active_time"]), text(&st["encryption"]));
    let time = typed("gtime", time0.clone());
    let left = fmt::int(&st["remaining_seconds"]).unwrap_or(-1);
    let timed = time != "0";
    // The app's: a timer that ran out turned the guest network off.
    let expired = timed && !g2 && !g5 && left <= 0;
    let times: Vec<&str> = TIME_VALUES.to_vec();
    let footers = html! {
        @if timed { (foot(&format!("Guest WiFi will automatically turn off after {}", time_label(&time)))) }
        @if left > 0 { p.foot.c-orange { (remaining(left)) " \u{b7} " a href=(GUEST) { "Refresh" } } }
    };
    html! {
        (form_error(pd))
        form.rf.gw method="post" action=(GUEST_APPLY) autocomplete="off" {
            (pd.csrf())
            (once_field(&pd.once()))
            (kit::section(Some("Radio Bands"), expired.then(|| html! { p.foot.c-orange { "Timer expired \u{2014} guest WiFi was automatically disabled" } }), html! {
                (switch("g2", "2.4 GHz", g2, "it", "sr gb"))
                (switch("g5", "5 GHz", g5, "it", "sr gb"))
            }))
            div.gn {
                (kit::section(Some("Network"), None, html! {
                    (text_row("gssid", "SSID", &typed("gssid", text(&st["ssid"])), err("gssid"), "it fld", 32, None))
                    (password_row("gkey", "Password", st["has_key"].as_bool() == Some(true), f.and_then(|f| f.value("gkey")) == Some(KEPT), err("gkey"), "it fld"))
                    (select_row("genc", "Encryption", &options(&ENC, &enc0, enc_label), &typed("genc", enc0.clone()), None, "it"))
                    (switch("ghid", "Hidden SSID", flag("ghid", truthy(&st["hidden"])), "it", "sr"))
                    (switch("giso", "Client Isolation", flag("giso", truthy(&st["isolate"])), "it", "sr"))
                }))
            }
            // The app shows the timer with a band on, or while it runs or ran out.
            div class=[(!expired && left <= 0).then_some("gn")] {
                (kit::section(Some("Timer"), Some(footers), select_row("gtime", "Auto-Shutoff", &options(&times, &time0, time_label), &time, None, "it")))
            }
            (kit::section(None, None, html! { li { button.it.cv.snd type="submit" { "Apply" } } }))
        }
    }
}

/// The Guest WiFi form, checked against the router's state (read now).
pub fn check_guest(app: &AppState, pairs: &[(String, String)], kept: &Vals) -> Result<Vals, Bad> {
    let cur = match super::wifi::guest_status(app) {
        Ok(c) => c,
        Err(e) => {
            let mut values = as_typed(pairs, &["gssid", "genc", "gtime"]);
            for n in ["g2", "g5", "ghid", "giso"] {
                values.push((n, checkbox(pairs, n)));
            }
            return Err(Bad::Fields(FormState { values, errors: vec![("form", format!("Failed to load guest WiFi settings: {}", fmt::truncate(&e, 120)))] }));
        }
    };
    let (g2, g5) = (checkbox(pairs, "g2"), checkbox(pairs, "g5"));
    let time = choice(pairs, "gtime", &TIME_VALUES, &minutes(&cur["guest_active_time"]))?;
    let enc = choice(pairs, "genc", &ENC, &text(&cur["encryption"]))?;
    let ssid: String = field(pairs, "gssid").unwrap_or("").chars().take(64).collect();
    let mut errors = Vec::new();
    let mut key = String::new();
    if g2 == "1" || g5 == "1" {
        if let Some(e) = ssid_error(&ssid, &text(&cur["ssid"])) {
            errors.push(("gssid", e.to_owned()));
        }
        key = key_of(field(pairs, "gkey").unwrap_or(""), val(kept, "gkey"), &text(&cur["key"]), &enc, &mut errors, "gkey");
    }
    let vals: Vals = vec![
        ("g2", g2),
        ("g5", g5),
        ("gssid", ssid),
        ("gkey", key),
        ("genc", enc),
        ("ghid", checkbox(pairs, "ghid")),
        ("giso", checkbox(pairs, "giso")),
        ("gtime", time),
    ];
    if errors.is_empty() {
        Ok(vals)
    } else {
        Err(Bad::Fields(FormState { values: vals, errors }))
    }
}

/// What differs from the guest network's state, under the agent's names
/// (`guest_disabled_2g` "0" is on); the network's fields only while a band
/// is on, a password only when one was typed.
pub fn guest_body(vals: &[(&'static str, String)], cur: &Value) -> Map<String, Value> {
    let mut b = Map::new();
    let mut set = |k: &str, v: &str| {
        b.insert(k.into(), Value::String(v.into()));
    };
    let (g2, g5) = (val(vals, "g2") == "1", val(vals, "g5") == "1");
    for (on, key, router) in [(g2, "guest_disabled_2g", "disabled_2g"), (g5, "guest_disabled_5g", "disabled_5g")] {
        if on == truthy(&cur[router]) {
            set(key, bit(!on));
        }
    }
    if g2 || g5 {
        for (name, key, router) in [("gssid", "guest_ssid", "ssid"), ("genc", "guest_encryption", "encryption")] {
            if val(vals, name) != text(&cur[router]) {
                set(key, val(vals, name));
            }
        }
        for (name, key, router) in [("ghid", "guest_hidden", "hidden"), ("giso", "guest_isolate", "isolate")] {
            let on = val(vals, name) == "1";
            if on != truthy(&cur[router]) {
                set(key, bit(on));
            }
        }
        if val(vals, "genc") != "none" && !val(vals, "gkey").is_empty() {
            set("guest_key", val(vals, "gkey"));
        }
    }
    if val(vals, "gtime") != minutes(&cur["guest_active_time"]) {
        set("guest_active_time", val(vals, "gtime"));
    }
    b
}

const GUEST_RESTART: &str =
    "WiFi restarts to apply the changes: every device on the router\u{2019}s WiFi, guests included, disconnects for a moment, this browser too if it is on WiFi.";

pub fn guest_question(app: &AppState, vals: &[(&'static str, String)]) -> Question {
    let cur = match super::wifi::guest_status(app) {
        Ok(c) => c,
        Err(e) => return nothing("Failed to load guest WiFi settings", vec![fmt::truncate(&e, 120)]),
    };
    let b = guest_body(vals, &cur);
    if b.is_empty() {
        return nothing("Nothing to apply", vec!["These are the router\u{2019}s settings already.".into()]);
    }
    let was = !truthy(&cur["disabled_2g"]) || !truthy(&cur["disabled_5g"]);
    let now = val(vals, "g2") == "1" || val(vals, "g5") == "1";
    let ssid = val(vals, "gssid");
    let time = val(vals, "gtime");
    let mut lines = Vec::new();
    let title = match (was, now) {
        (true, false) => {
            lines.push("The guest network turns off: devices on it lose their connection.".into());
            "Turn off Guest WiFi?"
        }
        (false, true) => {
            lines.push(format!("The guest network \u{201c}{ssid}\u{201d} turns on."));
            "Turn on Guest WiFi?"
        }
        _ => {
            for (key, label) in [("guest_disabled_2g", "2.4 GHz"), ("guest_disabled_5g", "5 GHz")] {
                match b.get(key).and_then(Value::as_str) {
                    Some("0") => lines.push(format!("The {label} guest network turns on.")),
                    Some(_) => lines.push(format!("The {label} guest network turns off: devices on it lose their connection.")),
                    None => {}
                }
            }
            let new_key = b.contains_key("guest_key");
            if now && (b.contains_key("guest_ssid") || new_key || b.contains_key("guest_encryption")) {
                let with = if new_key { " with the new password" } else { "" };
                lines.push(format!("Guests must join \u{201c}{ssid}\u{201d} again{with}."));
            }
            "Apply Guest WiFi settings?"
        }
    };
    if now && val(vals, "genc") == "none" && (b.contains_key("guest_encryption") || !was) {
        lines.push(open(ssid));
    }
    if now && (b.contains_key("guest_active_time") || !was) && time != "0" {
        lines.push(format!("Guest WiFi turns off by itself after {}.", time_label(time)));
    }
    lines.push(GUEST_RESTART.into());
    asking(title, lines, "Apply", false, false)
}

/// The guest network's changes; then (the agent reloads all of Wi-Fi) the
/// reconnect page, with the names of the router's own networks that are on
/// (read before sending; none if that read fails).
pub fn send_guest(app: &AppState, vals: &[(&'static str, String)], claim: &mut Claim) -> Result<Done, (u16, String)> {
    let cur = super::wifi::guest_status(app).map_err(|e| (503, failed(&e)))?;
    let b = guest_body(vals, &cur);
    if b.is_empty() {
        return Ok(Done::Line(None, "No changes to apply.".into()));
    }
    let mut main = Vals::new();
    if let Some(st) = super::wifi::status(app).ok().filter(|st| truthy(&st["wifi_onoff"])) {
        for band in &BANDS {
            let ssid = text(&st[band.ssid]);
            if !truthy(&st[band.disabled]) && !ssid.is_empty() {
                main.push((if band.five { "main_5g" } else { "main_2g" }, ssid));
            }
        }
    }
    claim.disarm();
    let data = super::call(app, Method::Put, "/api/wifi/guest", &Value::Object(b)).map_err(|(status, e)| (status, failed(&e)))?;
    if data["note"].as_str() == Some("no changes") {
        return Ok(Done::Line(None, "No changes to apply.".into()));
    }
    Ok(Done::Page(main))
}

/// After a Guest WiFi Apply (from the sent draft; without one, the general
/// words): every Wi-Fi network restarts, so "Reconnect to …" the router's
/// own network, and the guest network's name.
pub fn guest_applied(vals: &[(&'static str, String)]) -> (String, Markup) {
    let mut names: Vec<&str> = ["main_2g", "main_5g"].iter().map(|k| val(vals, k)).filter(|n| !n.is_empty()).collect();
    names.dedup();
    let title = match names.as_slice() {
        [one] => format!("Reconnect to \u{201c}{one}\u{201d}"),
        _ => "Reconnect to WiFi".to_owned(),
    };
    let guests = val(vals, "g2") == "1" || val(vals, "g5") == "1";
    let body = html! {
        p.msg.ok role="status" { "Guest WiFi settings applied" }
        div.narrow {
            ul.consequences {
                li { "WiFi is restarting, guest network included. If this device is on the router\u{2019}s WiFi, it disconnects for a moment." }
                @for n in &names { li { "Join \u{201c}" (n) "\u{201d} if it does not reconnect by itself." } }
                @if names.is_empty() { li { "Join the router\u{2019}s WiFi again if this device does not reconnect by itself." } }
                @if guests && !val(vals, "gssid").is_empty() {
                    li {
                        "Guests join \u{201c}" (val(vals, "gssid")) "\u{201d}"
                        @if val(vals, "set_g") == "1" { " with the new password" }
                        "."
                    }
                }
            }
            p.actions { a.btn href=(GUEST) { "Back to Guest WiFi" } }
        }
    };
    (title, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status() -> Value {
        json!({
            "wifi_onoff": "1", "wifi6_switch": "0", "radio2_disabled": "0", "radio5_disabled": "1",
            "ssid_2g": "Fake & Net", "ssid_5g": "Fake-5G", "key_2g": "FAKEKEY-2g-000", "key_5g": "",
            "channel_2g": "0", "channel_5g": "36", "htmode_2g": "HE20", "htmode_5g": "EHT80", "txpower_2g": "", "txpower_5g": "100",
            "encryption_2g": "psk2+ccmp", "encryption_5g": "none", "hidden_2g": "", "hidden_5g": "0",
        })
    }

    fn vals(over: &[(&'static str, &str)]) -> Vals {
        let mut v: Vals = vec![
            ("wifi", "1".into()), ("wifi6_switch", String::new()), ("r2", "1".into()), ("ssid_2g", "Fake & Net".into()), ("key_2g", String::new()),
            ("channel_2g", "auto".into()), ("htmode_2g", "HE20".into()), ("txpower_2g", String::new()), ("encryption_2g", "psk2+ccmp".into()),
            ("hidden_2g", String::new()), ("r5", String::new()), ("ssid_5g", "Fake-5G".into()), ("key_5g", String::new()), ("channel_5g", "36".into()),
            ("htmode_5g", "EHT80".into()), ("txpower_5g", "100".into()), ("encryption_5g", "none".into()), ("hidden_5g", String::new()),
        ];
        for (k, x) in over {
            v.iter_mut().find(|(n, _)| n == k).unwrap().1 = (*x).to_owned();
        }
        v
    }

    /// Only what changed: the router's values the app's lists lack (HE20,
    /// an empty TX power, channel "0" as Auto, an SSID with "&") are kept
    /// as they are; a password only when typed; nothing of a radio that
    /// is off; only the switch while WiFi is off.
    #[test]
    fn bodies_send_only_changes() {
        let cur = status();
        assert!(body(&vals(&[]), &cur).is_empty());
        let b = body(&vals(&[("key_2g", "new-fake-pass"), ("hidden_2g", "1"), ("ssid_5g", "Other")]), &cur);
        assert_eq!(Value::Object(b), json!({"key_2g": "new-fake-pass", "hidden_2g": "1"}));
        let b = body(&vals(&[("r5", "1"), ("txpower_2g", "20")]), &cur);
        assert_eq!(Value::Object(b), json!({"radio5_disabled": "0", "txpower_2g": "20"}));
        let b = body(&vals(&[("wifi", ""), ("ssid_2g", "Changed")]), &cur);
        assert_eq!(Value::Object(b), json!({"wifi_onoff": "0"}));
        // An open network never gets a password.
        let b = body(&vals(&[("r5", "1"), ("key_5g", "typed-but-open")]), &cur);
        assert_eq!(Value::Object(b), json!({"radio5_disabled": "0"}));
    }

    #[test]
    fn names_and_passwords_are_checked() {
        assert_eq!(ssid_error("", "x"), Some("Enter a network name."));
        assert!(ssid_error(&"é".repeat(17), "x").is_some(), "34 bytes");
        assert_eq!(ssid_error(&"a".repeat(32), "x"), None);
        assert!(ssid_error("Fake & New", "Fake").is_some(), "the agent would strip &");
        assert_eq!(ssid_error("Fake & Net", "Fake & Net"), None, "unchanged is never sent");
        assert!(ssid_error("two\nlines", "x").is_some());
        for ok in ["12345678", &"a".repeat(63), &"0f".repeat(32), "pass word!"] {
            assert_eq!(key_error(ok, "psk2+ccmp"), None, "{ok}");
        }
        for bad in ["1234567", &"x".repeat(64), &"a".repeat(65), "pässword1", "fake$pass"] {
            assert!(key_error(bad, "psk2+ccmp").is_some(), "{bad}");
        }
        // WPA3 takes a passphrase only.
        for enc in ["sae", "sae-mixed"] {
            assert_eq!(key_error(&"0f".repeat(32), enc), Some(SAE_KEY));
            assert_eq!(key_error("pass word!", enc), None);
        }
        let mut errors = Vec::new();
        assert_eq!(key_of("", "", &"0f".repeat(32), "sae", &mut errors, "key_2g"), "");
        assert!(errors[0].1.starts_with(SAE_KEY), "{errors:?}");
        errors.clear();
        assert_eq!(key_of("", "", &"0f".repeat(32), "psk2+ccmp", &mut errors, "key_2g"), "");
        assert!(errors.is_empty());
        assert_eq!(bandwidths_5g("165"), ["auto", "EHT20"]);
        assert_eq!(bandwidths_5g("149"), ["auto", "EHT20", "EHT40", "EHT80"]);
        assert_eq!(bandwidths_5g("136"), ["auto", "EHT20", "EHT40"]);
        assert_eq!(bandwidths_5g("36"), BW_5G);
        assert_eq!(bandwidths_5g("auto"), BW_5G);
        assert_eq!(remaining(3909), "1h 05m 09s remaining");
        assert_eq!(remaining(65), "1m 05s remaining");
    }

    /// The result page: the app's restart line for an Apply, words of its
    /// own when WiFi was turned off (nothing restarts then).
    #[test]
    fn applied_page_says_what_happened() {
        let (title, body) = applied(&vals(&[("wifi", "")]));
        let html = body.into_string();
        assert_eq!(title, "WiFi is off");
        assert!(html.contains("<p class=\"msg ok\" role=\"status\">WiFi turned off</p>"), "{html}");
        assert!(!html.contains("restart"), "{html}");
        let (title, body) = applied(&vals(&[]));
        let html = body.into_string();
        assert_eq!(title, "Reconnect to \u{201c}Fake & Net\u{201d}");
        assert!(html.contains("WiFi settings applied \u{2014} WiFi will restart briefly") && !html.contains("WiFi turned off"), "{html}");
    }

    #[test]
    fn guest_bodies_send_only_changes() {
        let cur = json!({"disabled_2g": "1", "disabled_5g": "1", "ssid": "Fake-Guest", "key": "FAKEGUEST-000", "encryption": "psk2+ccmp",
                         "hidden": "0", "isolate": "1", "guest_active_time": "0"});
        let v = |g2: &str, key: &str, time: &str| -> Vals {
            vec![("g2", g2.into()), ("g5", String::new()), ("gssid", "Fake-Guest".into()), ("gkey", key.into()), ("genc", "psk2+ccmp".into()),
                 ("ghid", String::new()), ("giso", "1".into()), ("gtime", time.into())]
        };
        assert!(guest_body(&v("", "", "0"), &cur).is_empty());
        assert_eq!(Value::Object(guest_body(&v("1", "", "60"), &cur)), json!({"guest_disabled_2g": "0", "guest_active_time": "60"}));
        assert_eq!(Value::Object(guest_body(&v("1", "new-guest-pass", "0"), &cur)), json!({"guest_disabled_2g": "0", "guest_key": "new-guest-pass"}));
    }
}
