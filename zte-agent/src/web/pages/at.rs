//! Tools › AT Terminal (iOS `ATTerminalView`): raw AT commands to the
//! modem.
//!
//! - The port line (`GET /api/at/port`, read at page load), the app's quick
//!   commands as links that only fill the field (`?cmd=`, those commands
//!   only; the app's "IMEI" chip is left out, owner decision), the history
//!   (newest first, in a box that scrolls, so the field stays in view), the
//!   timeout (1–30 s) and the field.
//! - The history is this session's last 20 commands in RAM (`store::AtLog`),
//!   dropped on logout; the app keeps it in the screen's memory. Commands
//!   and answers are stored with every run of 14 or more digits masked to
//!   its last 4 (an IMEI, IMSI or ICCID; owner decision: 14–20) and SIM
//!   PINs and PUKs hidden (`hide_pins`), then cut to 4 KiB.
//! - "Send" carries a single-use token (a double click sends once) and runs
//!   in the slow lane. The app's checks: a command starts with "AT", else
//!   a local error entry (the command stays in the field); one of its
//!   dangerous patterns asks first, with the app's words ("Send Anyway";
//!   Cancel keeps the command in the field). Web additions: at most 256
//!   characters, no control characters, no backslash (the agent's `echo -e`
//!   would turn one into other characters); the patterns are matched on
//!   the command as the agent sends it (`at_terminal::sanitize`), without
//!   spaces, which is also what the history shows; more commands that cut
//!   the connection ask first (`dangerous`).
//!   Copy buttons need JavaScript: the text is selectable instead.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::Request;
use axum::http::Method as HttpMethod;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::{json, Value};

use crate::auth::Principal;
use crate::handlers::AppState;
use crate::httpd::gate::Lane;
use crate::httpd::Ctx;
use crate::server::Method;
use crate::web::job::{self, OnceJob, Sent};
use crate::web::kit::{self, Icon};
use crate::web::render::{self, PageCtx};
use crate::web::screen::{self, FormState, Notice, PageData, Req};
use crate::web::session::{self, percent_encode};
use crate::web::store::{AtEntry, AtLog, Token};
use crate::web::tabs::tools;
use crate::web::{fmt, sms};

pub const PATH: &str = "/ui/tools/at";
pub const SEND: &str = "/ui/tools/at/send";
pub const CLEAR: &str = "/ui/tools/at/clear";

/// The app's quick commands without "AT+CGSN" (IMEI).
const QUICK: [(&str, &str); 7] = [
    ("AT", "Ping"),
    ("ATI", "Device info"),
    ("AT+CSQ", "Signal quality"),
    ("AT+COPS?", "Operator"),
    ("AT+CPIN?", "SIM status"),
    ("AT+CGDCONT?", "APN config"),
    ("AT+CLCC", "Active calls"),
];

/// The app's dangerous patterns (on the command in capitals).
const DANGER: [&str; 7] = ["CFUN=0", "CFUN=4", "+CRESET", "&F", "+NVWR", "+QPOWD", "+COPS="];
/// Web additions (decision 23: what cuts the connection asks first):
/// detaching or deactivating the data connection, SIM locks and PINs.
const DANGER_WEB: [&str; 4] = ["+CGATT=0", "+CGACT=0", "+CLCK=", "+CPWD="];

const MAX_CMD: usize = 256;
const MAX_RESP: usize = 4096;
const EXPIRED: &str = "This form expired, so the command was not sent. Send it again.";

/// `s` with every run of at least `min` ASCII digits replaced by `f(run)`.
fn digit_runs(s: &str, min: usize, f: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(|c: char| c.is_ascii_digit()) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let (run, after) = tail.split_at(tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len()));
        if run.len() >= min {
            out.push_str(&f(run));
        } else {
            out.push_str(run);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Every run of 14 or more ASCII digits but its last 4 as "•".
pub fn mask(s: &str) -> String {
    digit_runs(s, 14, |run| format!("{}{}", "\u{2022}".repeat(run.len() - 4), &run[run.len() - 4..]))
}

/// SIM PINs and PUKs: from the first "+CPIN=", "+CPIN2=", "+CLCK=" or
/// "+CPWD=" on (any case), every run of 4 or more digits as "••••" (a
/// command, and its echo in the answer).
fn hide_pins(s: &str) -> String {
    let up = s.to_ascii_uppercase();
    let from = ["+CPIN=", "+CPIN2=", "+CLCK=", "+CPWD="].iter().filter_map(|m| up.find(m).map(|i| i + m.len())).min();
    match from {
        Some(i) => format!("{}{}", &s[..i], digit_runs(&s[i..], 4, |_| "\u{2022}".repeat(4))),
        None => s.to_owned(),
    }
}

/// What a history entry may show of `s`.
fn shown(s: &str) -> String {
    mask(&hide_pins(s))
}

/// Whether a command asks first: one of the app's patterns in it as
/// typed, or one of the app's or the web's in it as the agent sends it
/// (`sent`), in capitals and without spaces (a modem ignores them, V.250);
/// or `+CFUN=` with anything but 1 or "?" (4: radio off, 1,1: a modem
/// reset, others: vendor modes).
fn dangerous(typed: &str, sent: &str) -> bool {
    let typed = typed.to_uppercase();
    let sent: String = sent.chars().filter(|c| !c.is_ascii_whitespace()).collect::<String>().to_ascii_uppercase();
    let cfun = sent.match_indices("+CFUN=").any(|(i, m)| !matches!(sent[i + m.len()..].split('+').next(), Some("1" | "?")));
    cfun || DANGER.iter().any(|d| typed.contains(d)) || DANGER.iter().chain(&DANGER_WEB).any(|d| sent.contains(d))
}

/// At most `max` bytes (on a character boundary), then "…".
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &s[..end])
}

/// "HH:MM:SS" now, in the router's time zone.
fn clock() -> String {
    let t = sms::now_unix();
    let s = (t + sms::local_offset(t)).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// A history entry; what it shows is masked (before the cut, so a cut
/// never leaves part of an identifier in clear).
fn entry(cmd: &str, resp: &str, port: &str, ms: u64, err: bool, timeout: u8) -> AtEntry {
    AtEntry { cmd: shown(cmd), resp: cut(&shown(resp), MAX_RESP), port: port.to_owned(), ms, clock: clock(), err, timeout }
}

/// The send's answer as the app records it.
fn record(cmd: &str, timeout: u8, r: &Result<Value, (u16, String)>) -> AtEntry {
    match r {
        Ok(d) => {
            let s = |k: &str| d[k].as_str().unwrap_or("").to_owned();
            entry(cmd, &s("response"), &s("port"), d["elapsed_ms"].as_u64().unwrap_or(0), false, timeout)
        }
        Err((_, e)) => entry(cmd, &format!("Error: {}", fmt::truncate(e, 120)), "", 0, true, timeout),
    }
}

/// The page: port, quick commands, history, then the timeout and field
/// (typed values kept when it is shown again).
pub fn page(pd: &PageData) -> Markup {
    let port = pd.read(0);
    let available = port.is_some_and(|p| p["available"] == true);
    let name = port.and_then(|p| p["port"].as_str()).filter(|n| !n.is_empty());
    let history = pd.at.map(|log| log.list(pd.p.id, pd.app.auth.now())).unwrap_or_default();
    let quick = pd.param("cmd").filter(|c| QUICK.iter().any(|(q, _)| q == c));
    let typed = pd.form.and_then(|f| f.value("cmd")).map(str::to_owned).or(quick).unwrap_or_default();
    let timeout = pd.form.and_then(|f| f.value("timeout")).and_then(|t| t.parse::<u8>().ok());
    let timeout = timeout.or_else(|| history.first().map(|e| e.timeout)).unwrap_or(3);
    let port_label = match name {
        Some(n) if available => format!("Port {n} available"),
        Some(n) => format!("Port {n} unavailable"),
        None => "No port detected".to_owned(),
    };
    html! {
        div.atp {
            span.dot.bad[!available] {}
            span.muted aria-label=(port_label) { (name.unwrap_or("No port detected")) }
            @if !history.is_empty() {
                form.sf method="post" action=(CLEAR) { (pd.csrf()) button.lk type="submit" { "Clear" } }
            }
        }
        nav.atq aria-label="Quick commands" {
            @for (cmd, label) in QUICK {
                a href=(format!("{PATH}?cmd={}#cmd", percent_encode(cmd))) { code { (cmd) } small { (label) } }
            }
        }
        @if history.is_empty() {
            (kit::empty_state(Icon::Terminal, "No Commands Sent", "Enter an AT command below or tap a quick command above."))
        } @else {
            ol.ath aria-label="History" {
                @for e in &history {
                    li class=[e.err.then_some("er")] {
                        b.mono { "> " (e.cmd) }
                        pre { (e.resp) }
                        small { @if !e.port.is_empty() { (e.port) " \u{b7} " } (e.ms) "ms \u{b7} " (e.clock) }
                    }
                }
            }
        }
        form.atf method="post" action=(SEND) {
            (pd.csrf())
            (render::once_field(&pd.once()))
            p.to {
                label.lb for="to" { "Timeout:" }
                input #to type="number" name="timeout" min="1" max="30" value=(timeout) aria-label="Command timeout in seconds";
                "s"
            }
            div.reply {
                input #cmd type="text" name="cmd" value=(typed) placeholder="AT command..." aria-label="AT command"
                    maxlength=(MAX_CMD) autocapitalize="characters" autocomplete="off" autocorrect="off" spellcheck="false";
                button.send type="submit" aria-label="Send command" { (kit::icon(Icon::PaperPlane)) }
            }
        }
    }
}

/// `POST …/clear`: this session's history.
pub fn clear(ctx: &Ctx, owner: crate::auth::SessionId) {
    ctx.at.clear(owner);
}

/// The app's "Dangerous Command" alert, naming the command as it is sent
/// (masked as in the history; the hidden field keeps it as typed):
/// "Send Anyway" posts the command with `confirm=1`; Cancel posts it back
/// into the field as typed (`edit=1`).
#[allow(clippy::too_many_arguments)]
fn danger_page(ctx: &Ctx, p: &Principal, nonce: &str, cmd: &str, sent: &str, timeout: u8, token: &str, remote: bool) -> Response {
    let body = html! {
        div.narrow {
            h2.q { "Dangerous Command" }
            ul.consequences {
                // As the history shows it: PINs and PUKs as "••••", long digit runs masked.
                li { "The command \"" (shown(sent)) "\" may disrupt connectivity or require a reboot. Are you sure?" }
                @if remote { li { (super::confirm::REMOTE) } }
            }
            form.panel method="post" action=(SEND) {
                (render::csrf_field(&ctx.app.auth, p))
                (render::once_field(token))
                input type="hidden" name="cmd" value=(cmd);
                input type="hidden" name="timeout" value=(timeout);
                p.actions {
                    button.btn.btn-danger type="submit" name="confirm" value="1" { "Send Anyway" }
                    button.btn.btn-quiet type="submit" name="edit" value="1" { "Cancel" }
                }
            }
        }
    };
    let pc = PageCtx {
        title: "Dangerous Command",
        tab: Some(kit::Tab::Tools),
        nonce,
        lead: kit::back(PATH, "AT Terminal"),
        heading: Some(html! { h1 { "Confirm" } }),
        ..PageCtx::default()
    };
    render::html(200, render::page(&pc, body))
}

/// `POST …/send`.
pub async fn post(ctx: Ctx, req: Request) -> Response {
    let mut r = Req { query: None, ..Req::of(&ctx, &req) };
    let nonce = r.nonce.clone();
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&HttpMethod::POST, PATH, None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&nonce, PATH);
    }
    let cmd = form.get("cmd").unwrap_or("").trim().to_owned();
    if cmd.chars().count() > MAX_CMD || cmd.chars().any(char::is_control) {
        return render::bad_request(&nonce);
    }
    let timeout = form.get("timeout").and_then(|t| t.trim().parse::<i64>().ok()).unwrap_or(3).clamp(1, 30) as u8;
    r.typed = Some(FormState { values: vec![("cmd", cmd.clone()), ("timeout", timeout.to_string())], errors: Vec::new() });
    let (c, pp) = (&ctx, p.clone());
    let show = move |r: Req, notice: Option<Notice>, status| screen::show(c, pp.clone(), r, &tools::AT, notice, status);
    if form.get("edit") == Some("1") {
        return show(r, None, 200).await;
    }
    if cmd.is_empty() {
        return session::see_other(PATH);
    }
    let now = ctx.app.auth.now();
    let refused = if !cmd.to_ascii_uppercase().starts_with("AT") {
        Some("Error: Command must start with AT")
    } else if cmd.contains('\\') {
        Some("Error: Command must not contain a backslash")
    } else {
        None
    };
    if let Some(e) = refused {
        ctx.at.push(p.id, entry(&cmd, e, "", 0, true, timeout), now);
        // The command stays in the field, as the app's.
        return show(r, None, 200).await;
    }
    // What the agent sends (and what the history shows).
    let sent = crate::at_terminal::sanitize(&cmd);
    let token = form.get("once").unwrap_or("").to_owned();
    if dangerous(&cmd, &sent) && form.get("confirm") != Some("1") {
        // An expired or used form says so now, not after "Send Anyway".
        if !matches!(ctx.once.get(&token, p.id, now), Some(Token::Fresh)) {
            return show(r, Some(Notice::Flash(Some(false), EXPIRED.into())), 400).await;
        }
        return danger_page(&ctx, &p, &nonce, &cmd, &sent, timeout, &token, r.remote);
    }
    let digest = job::digest(&[SEND, &sent, &timeout.to_string()]);
    let (log, owner, c): (Arc<AtLog>, _, String) = (Arc::clone(&ctx.at), p.id, sent);
    let work: OnceJob = Box::new(move |app: &AppState, claim| {
        claim.disarm();
        let res = super::call(app, Method::Post, "/api/at/send", &json!({"command": c, "timeout": timeout}));
        log.push(owner, record(&c, timeout, &res), app.auth.now());
        claim.set(match &res {
            Ok(_) => Token::Sent(claim.digest()),
            Err((status, e)) => Token::Failed(claim.digest(), *status, e.clone()),
        });
        session::see_other(PATH)
    });
    // The permit wait, the command's own timeout and some margin.
    let limit = crate::httpd::GATE_WAIT + Duration::from_secs(u64::from(timeout) + 5);
    match job::once(&ctx, p.id, &nonce, &token, digest, Lane::Slow, limit, work).await {
        Sent::Ran(resp) => resp,
        Sent::Done(_) => session::see_other(PATH),
        Sent::Expired => show(r, Some(Notice::Flash(Some(false), EXPIRED.into())), 400).await,
        Sent::Unknown => {
            let msg = "The command may still be running. Check the history before sending it again.";
            show(r, Some(Notice::Flash(None, msg.into())), 409).await
        }
        Sent::Busy => {
            let msg = "Failed: the router is busy. Try again in a few seconds.";
            show(r, Some(Notice::Flash(Some(false), msg.into())), 503).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs of 14 or more digits keep their last 4; shorter ones (a phone
    /// number, a PLMN, a signal value) stay.
    #[test]
    fn identifiers_are_masked() {
        assert_eq!(mask("+CGSN: 000000000000042\r\nOK"), "+CGSN: \u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}0042\r\nOK");
        assert_eq!(mask("8900100000009998877F"), format!("{}8877F", "\u{2022}".repeat(15)));
        assert_eq!(mask("001019999911111"), format!("{}1111", "\u{2022}".repeat(11)));
        assert_eq!(mask("+CSQ: 20,99 +1000000004 1234567890123"), "+CSQ: 20,99 +1000000004 1234567890123");
        assert_eq!(mask("12345678901234567890123"), format!("{}0123", "\u{2022}".repeat(19)));
        assert_eq!(mask(""), "");
        assert_eq!(cut("abcdef", 4), "abcd\u{2026}");
        assert_eq!(cut("ab\u{e9}", 3), "ab\u{2026}");
        assert_eq!(cut("abc", 3), "abc");
    }

    #[test]
    fn records_follow_the_app() {
        let ok = Ok(json!({"response": "+CIMI: 001019999911111\r\nOK", "port": "/dev/smd11", "elapsed_ms": 312}));
        let e = record("AT+CIMI", 5, &ok);
        assert_eq!((e.cmd.as_str(), e.port.as_str(), e.ms, e.err, e.timeout), ("AT+CIMI", "/dev/smd11", 312, false, 5));
        assert!(e.resp.contains("1111") && !e.resp.contains("0010199999"), "{}", e.resp);
        assert_eq!(e.clock.len(), 8);
        let e = record("AT", 3, &Err((503, "no serial port found".into())));
        assert_eq!((e.resp.as_str(), e.err), ("Error: no serial port found", true));
        let long = Ok(json!({"response": "x".repeat(10_000)}));
        assert_eq!(record("AT", 3, &long).resp.len(), MAX_RESP + "\u{2026}".len());
        // Masked before the cut: an IMEI across byte 4096 shows no digits
        // but its last ones, and a masked answer is still at most 4 KiB.
        let across = Ok(json!({"response": format!("{}000000000000042", "x".repeat(4083))}));
        let e = record("AT", 3, &across);
        assert!(!e.resp.contains("00") && e.resp.len() <= MAX_RESP + "\u{2026}".len(), "{}", &e.resp[4000..]);
        let digits = Ok(json!({"response": "000000000000042,".repeat(1000)}));
        assert!(record("AT", 3, &digits).resp.len() <= MAX_RESP + "\u{2026}".len());
    }

    /// PINs and PUKs never reach the history, in the command or its echo;
    /// other digits stay.
    #[test]
    fn pins_are_hidden() {
        let b = "\u{2022}\u{2022}\u{2022}\u{2022}";
        assert_eq!(hide_pins("AT+CPIN=\"1234\""), format!("AT+CPIN=\"{b}\""));
        assert_eq!(hide_pins("at+cpin=\"12345678\",\"4321\""), format!("at+cpin=\"{b}\",\"{b}\""));
        assert_eq!(hide_pins("AT+CLCK=\"SC\",1,\"1234\""), format!("AT+CLCK=\"SC\",1,\"{b}\""));
        assert_eq!(hide_pins("AT+CPWD=\"SC\",\"1234\",\"9876\""), format!("AT+CPWD=\"SC\",\"{b}\",\"{b}\""));
        assert_eq!(hide_pins("AT+CPIN2=5678"), format!("AT+CPIN2={b}"));
        assert_eq!(hide_pins("AT+CPIN?\r\n+CPIN: READY"), "AT+CPIN?\r\n+CPIN: READY");
        assert_eq!(hide_pins("AT+CSQ\r\n+CSQ: 1234,99"), "AT+CSQ\r\n+CSQ: 1234,99");
        let e = record("AT+CPIN=\"1234\"", 3, &Ok(json!({"response": "AT+CPIN=\"1234\"\r\r\nOK\r\n"})));
        assert!(!e.cmd.contains("1234") && !e.resp.contains("1234"), "{} {}", e.cmd, e.resp);
    }

    /// The app's patterns as typed; the app's and the web's as sent
    /// (without the characters the agent strips, without spaces).
    #[test]
    fn dangerous_commands_ask_first() {
        let check = |typed: &str| dangerous(typed, &crate::at_terminal::sanitize(typed));
        for yes in [
            "AT+CFUN=0", "at+cfun=4", "AT+CFUN=$4", "AT+CFUN=`4", "AT+CFUN = 4", "AT+CFUN=1,1", "AT+CFUN=7", "AT+CSQ;+CFUN=0",
            "AT+COPS=2", "AT+COPS`=2", "AT+COPS =2", "AT&F", "AT+CRESET", "AT+CGATT=0", "AT+CGACT=0,1", "AT+CLCK=\"SC\",1,\"1234\"",
            "AT+CPWD=\"SC\",\"1\",\"2\"",
        ] {
            assert!(check(yes), "{yes}");
        }
        for no in ["AT", "ATI", "AT+CSQ", "AT+COPS?", "AT+CFUN?", "AT+CFUN=?", "AT+CFUN=1", "AT+CGATT?", "AT+CPIN?", "AT+CGDCONT?"] {
            assert!(!check(no), "{no}");
        }
    }
}
