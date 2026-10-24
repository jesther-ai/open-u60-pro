//! Router › Device Controls (its switches, the idle timeout, Reboot Router
//! and Factory Reset with the router password) and the Settings tab's
//! preferences (theme, refresh interval) through the whole router.
//! Page-load reads come from `pages::device`'s `gets`, the changes go to
//! `pages::device`. Every value is a made-up fixture.

use serde_json::json;

use super::*;
use crate::events::Topic;
use crate::web::assets::{THEME_DARK_CSS, THEME_LIGHT_CSS};
use crate::web::pages::device;

const REBOOT: &str = "/ui/router/device/reboot";
const RESET: &str = "/ui/router/device/factory-reset";
const PS: &str = "/ui/router/device/power-save";
const FB: &str = "/ui/router/device/fast-boot";
const AS: &str = "/ui/router/device/auto-sleep";
const SETTINGS: &str = "/ui/settings";

fn calls(c: &Ctx) -> Vec<(String, Value)> {
    device::calls(&c.app)
}

fn tailnet(port: u16) -> Peer {
    Peer(SocketAddr::from(([127, 0, 0, 1], port)))
}

/// `location` of a 303.
fn to(r: &Reply) -> String {
    assert_eq!(r.status, 303, "{}", r.text());
    r.header("location").unwrap().to_owned()
}

fn has(html: &str, wants: &[&str]) {
    for want in wants {
        assert!(html.contains(want), "{want} in {html}");
    }
}

/// The screenshot's router: charge limit 85 % with an 11 % gap, power-save
/// off, Fast Boot on, Auto Sleep as given.
fn router(sleep: Value) -> device::Device {
    device::Device {
        gets: vec![
            ("/api/device/charge-control", Ok(json!({"charge_limit_enabled": true, "charge_limit": 85, "hysteresis": 11}))),
            ("/api/device/power-save", Ok(json!({"power_saver_mode": "0"}))),
            ("/api/device/fast-boot", Ok(json!({"fast_boot": "1"}))),
            ("/api/device/auto-sleep", Ok(sleep)),
        ],
        ..Default::default()
    }
}

/// The page as in `device-controls.png`, in the app's order and words: the
/// charge limit, the three switches, the idle timeout while Auto Sleep is
/// on, Reboot Router and Factory Reset, each with its footer; one
/// single-use token for every form.
#[tokio::test]
async fn device_controls_follow_the_app() {
    let (c, cookie, _) = signed_in(220).await;
    let _dev = device::set(&c.app, router(json!({"enabled": true, "timeout": "10", "status": "0"})));
    let r = get_with(&c, DEV, peer(220, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    let order = [
        "<form class=\"rf chl\" method=\"post\" action=\"/ui/router/device/charge-limit\">",
        "<input class=\"sr\" id=\"enabled\" type=\"checkbox\" role=\"switch\" name=\"enabled\" value=\"1\" checked>",
        "<label for=\"limit\">Stop at</label>", "<option value=\"85\" selected>85%</option>",
        "<label for=\"hysteresis\">Resume gap</label>", "<option value=\"11\" selected>11%</option>",
        "<button class=\"it cv snd\" type=\"submit\">Save</button>",
        "Charging stops at 85% and resumes at 74%.", "Default: 5% \u{2014} good balance for most users.",
        "<input type=\"hidden\" name=\"on\" value=\"1\"><button class=\"sw\" type=\"submit\" role=\"switch\" aria-checked=\"false\" aria-label=\"Power-save Mode\">",
        "Restricts data communication speed to reduce consumption and extend battery life.",
        "<input type=\"hidden\" name=\"on\" value=\"0\"><button class=\"sw\" type=\"submit\" role=\"switch\" aria-checked=\"true\" aria-label=\"Fast Boot\">",
        "When enabled, powering off suspends to RAM for near-instant boot.",
        "<input type=\"hidden\" name=\"timeout\" value=\"10\"><input type=\"hidden\" name=\"on\" value=\"0\"><button class=\"sw\" type=\"submit\" role=\"switch\" aria-checked=\"true\" aria-label=\"Auto Sleep\">",
        "<label for=\"tmo\">Idle Timeout</label><form class=\"sf\" method=\"post\" action=\"/ui/router/device/auto-sleep\">",
        "<option value=\"10\" selected>10 min</option>", "<option value=\"60\">1 hour</option>", "<option value=\"120\">2 hours</option>",
        "Automatically enters low-power sleep when no WiFi clients are connected and the idle timeout expires. Press the power button to wake.",
        "<a class=\"it bt\" href=\"/ui/router/device/reboot\">Reboot Router</a>", "The router will restart. This takes about 60 seconds.",
        "<a class=\"it bt danger\" href=\"/ui/router/device/factory-reset\">Factory Reset</a>",
        "This will erase all settings and restore factory defaults. This cannot be undone.",
    ];
    let at: Vec<usize> = order.iter().map(|w| html.find(w).unwrap_or_else(|| panic!("{w} in {html}"))).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{order:?} at {at:?}");
    let tokens: Vec<&str> = html.split("name=\"once\" value=\"").skip(1).map(|t| t.split('"').next().unwrap()).collect();
    assert_eq!(tokens.len(), 5, "{html}");
    assert!(tokens.iter().all(|t| *t == tokens[0]));
    assert!(!html.contains("Coming to the web UI") && !html.contains("Failed"));
    // Charge Limit off: the app's other footer; Auto Sleep off: no timeout.
    let _dev = device::set(&c.app, device::Device {
        gets: vec![
            ("/api/device/charge-control", Ok(json!({"charge_limit_enabled": false, "charge_limit": 83, "hysteresis": 5}))),
            ("/api/device/power-save", Ok(json!({"power_saver_mode": "1"}))),
            ("/api/device/fast-boot", Ok(json!({"fast_boot": "0"}))),
            ("/api/device/auto-sleep", Ok(json!({"enabled": false, "timeout": "", "status": ""}))),
        ],
        ..Default::default()
    });
    let html = get_with(&c, DEV, peer(220, 3), &cookie).await.text();
    has(&html, &[
        "Stops charging when battery reaches the set level. Extends battery lifespan.",
        // A limit off the app's 5 % steps is kept, shown first.
        "<select id=\"limit\" name=\"limit\"><option value=\"83\" selected>83%</option><option value=\"50\">50%</option>",
        "aria-checked=\"true\" aria-label=\"Power-save Mode\"", "aria-checked=\"false\" aria-label=\"Fast Boot\"",
        // The app's default timeout goes with the switch.
        "<input type=\"hidden\" name=\"timeout\" value=\"5\"><input type=\"hidden\" name=\"on\" value=\"1\"><button class=\"sw\"",
    ]);
    assert!(!html.contains("Idle Timeout") && !html.contains("name=\"enabled\" value=\"1\" checked"), "{html}");
    // A read that fails: the app's line with the error, the switch unknown.
    let _dev = device::set(&c.app, device::Device {
        gets: vec![("/api/device/power-save", Err("ubus call zwrt_mc.device.manager get_device_info failed: fake".into()))],
        ..Default::default()
    });
    let html = get_with(&c, DEV, peer(220, 4), &cookie).await.text();
    has(&html, &[
        "<p class=\"msg err\" role=\"alert\">Failed to load power-save settings: ubus call zwrt_mc.device.manager get_device_info failed: fake</p>",
        "<span>Power-save Mode</span><span class=\"v\">Unavailable</span>", "aria-label=\"Turn on Power-save Mode\">On</button>",
    ]);
    // The charge limit unread: no form (its Save would write the defaults).
    let _dev = device::set(&c.app, device::Device {
        gets: vec![("/api/device/charge-control", Err("charge control read failed: fake".into()))],
        ..Default::default()
    });
    let html = get_with(&c, DEV, peer(220, 5), &cookie).await.text();
    has(&html, &[
        "<p class=\"msg err\" role=\"alert\">Failed to load charge control status: charge control read failed: fake</p>",
        "<li class=\"it\"><span>Charge Limit</span><span class=\"v\">Unavailable</span></li>",
        "Stops charging when battery reaches the set level. Extends battery lifespan.",
    ]);
    assert!(!html.contains("action=\"/ui/router/device/charge-limit\"") && !html.contains("name=\"limit\""), "{html}");
}

/// The switches and the idle timeout post at once with the page's token
/// (the app's bodies; a double click sends once) and lead back to the
/// page; a failure is the app's "Failed: …"; nothing without the session,
/// the CSRF token or a value a form sends.
#[tokio::test]
async fn device_switches_send_once_with_the_app_bodies() {
    let (c, cookie, _) = signed_in(221).await;
    let _dev = device::set(&c.app, router(json!({"enabled": true, "timeout": "10"})));
    let page = get_with(&c, DEV, peer(221, 2), &cookie).await;
    let (csrf, token) = (page.input("csrf"), page.input("once"));
    let body = format!("csrf={csrf}&once={token}&on=1");
    let (a, b) = tokio::join!(post(&c, PS, peer(221, 2), &cookie, &body), post(&c, PS, peer(221, 2), &cookie, &body));
    a.assert_see_other(DEV);
    b.assert_see_other(DEV);
    post(&c, PS, peer(221, 2), &cookie, &body).await.assert_see_other(DEV);
    assert_eq!(calls(&c), [("/api/device/power-save".to_owned(), json!({"deviceInfoList": {"power_saver_mode": "1"}}))]);
    let sent = |path: &'static str, fields: &'static str| {
        let c = c.clone();
        let (cookie, csrf) = (cookie.clone(), csrf.clone());
        async move {
            let token = get_with(&c, DEV, peer(221, 3), &cookie).await.input("once");
            post(&c, path, peer(221, 3), &cookie, &format!("csrf={csrf}&once={token}&{fields}")).await
        }
    };
    sent(FB, "on=0").await.assert_see_other(DEV);
    sent(AS, "on=0&timeout=10").await.assert_see_other(DEV);
    sent(AS, "timeout=60").await.assert_see_other(DEV);
    sent(AS, "on=1&timeout=20").await.assert_see_other(DEV);
    assert_eq!(calls(&c)[1..], [
        ("/api/device/fast-boot".to_owned(), json!({"fast_boot": "0"})),
        ("/api/device/auto-sleep".to_owned(), json!({"enabled": false})),
        ("/api/device/auto-sleep".to_owned(), json!({"enabled": true, "timeout": "60"})),
        ("/api/device/auto-sleep".to_owned(), json!({"enabled": true, "timeout": "20"})),
    ]);
    // Values no form sends; no session; no CSRF token; no single-use token.
    for (path, fields) in [(PS, "on=2"), (FB, ""), (AS, "on=1"), (AS, "timeout=0"), (AS, "timeout=1441"), (AS, "on=1&timeout=5%3B")] {
        sent(path, fields).await.assert_secure_html(400);
    }
    post(&c, PS, peer(221, 4), "", &format!("csrf={csrf}&on=1")).await.assert_see_other("/ui/login?next=%2Fui%2Frouter%2Fdevice&expired=1");
    post(&c, PS, peer(221, 4), &cookie, "on=1").await.assert_secure_html(403);
    let r = post(&c, PS, peer(221, 4), &cookie, &format!("csrf={csrf}&on=1")).await;
    let html = get_with(&c, &to(&r), peer(221, 4), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">This page expired, so nothing was sent. Try again.</p>"), "{html}");
    assert_eq!(calls(&c).len(), 5);
    // The router refuses: the app's line, the switch as the router reads it.
    let mut dev = router(json!({"enabled": false, "timeout": "5"}));
    dev.replies = vec![("/api/device/fast-boot", (503, json!({"ok": false, "error": "ubus call zwrt_mc.device.manager set_device_info failed: fake"})))];
    let _dev = device::set(&c.app, dev);
    let r = sent(FB, "on=0").await;
    let html = get_with(&c, &to(&r), peer(221, 5), &cookie).await.text();
    has(&html, &[
        "<p class=\"msg err\" role=\"alert\">Failed: ubus call zwrt_mc.device.manager set_device_info failed: fake</p>",
        "aria-checked=\"true\" aria-label=\"Fast Boot\"",
    ]);
}

/// Over Tailscale, turning Auto Sleep on is asked first (the router then
/// sleeps out of reach); turning it off, the timeout and the other
/// switches are not. A local browser is never asked.
#[tokio::test]
async fn auto_sleep_on_is_asked_over_tailscale() {
    let (c, cookie, _) = signed_in(222).await;
    let _dev = device::set(&c.app, router(json!({"enabled": false, "timeout": "30"})));
    let html = get_with(&c, DEV, tailnet(42222), &cookie).await.text();
    let ask = "/ui/router/device/auto-sleep?on=1&amp;timeout=30";
    has(&html, &[&format!("<a class=\"sw\" href=\"{ask}\" role=\"switch\" aria-checked=\"false\" aria-label=\"Auto Sleep\">")]);
    let q = get_with(&c, &ask.replace("&amp;", "&"), tailnet(42222), &cookie).await;
    q.assert_secure_html(200);
    let qh = q.text();
    has(&qh, &[
        "<h1>Confirm</h1>", "<h2 class=\"q\">Turn on Auto Sleep?</h2>",
        "<li>When no Wi-Fi device has been connected for 30 min, the router goes to sleep. Press its power button to wake it.</li>",
        "<li>You are connected remotely: while the router sleeps, this page can\u{2019}t be reached until its power button is pressed.</li>",
        "<input type=\"hidden\" name=\"on\" value=\"1\"><input type=\"hidden\" name=\"timeout\" value=\"30\">",
        "<button class=\"btn btn-danger\" type=\"submit\">Turn On Auto Sleep</button>", "href=\"/ui/router/device\">Cancel</a>",
    ]);
    let csrf = q.input("csrf");
    // A remote post without the answer goes to the question; nothing sent.
    let token = get_with(&c, DEV, tailnet(42222), &cookie).await.input("once");
    let r = post(&c, AS, tailnet(42222), &cookie, &format!("csrf={csrf}&once={token}&on=1&timeout=30")).await;
    r.assert_see_other("/ui/router/device/auto-sleep?on=1&timeout=30");
    assert!(calls(&c).is_empty());
    let r = post(&c, AS, tailnet(42222), &cookie, &format!("csrf={csrf}&once={}&on=1&timeout=30&confirm=1", q.input("once"))).await;
    r.assert_see_other(DEV);
    assert_eq!(calls(&c), [("/api/device/auto-sleep".to_owned(), json!({"enabled": true, "timeout": "30"}))]);
    // Off and the other switches: at once, also remotely.
    let token = get_with(&c, DEV, tailnet(42222), &cookie).await.input("once");
    post(&c, PS, tailnet(42222), &cookie, &format!("csrf={csrf}&once={token}&on=1")).await.assert_see_other(DEV);
    // Locally: no question; the question page has nothing to ask.
    let html = get_with(&c, DEV, peer(222, 2), &cookie).await.text();
    assert!(!html.contains(ask) && html.contains("<input type=\"hidden\" name=\"timeout\" value=\"30\"><input type=\"hidden\" name=\"on\" value=\"1\">"));
    get_with(&c, "/ui/router/device/auto-sleep?on=1&timeout=30", peer(222, 2), &cookie).await.assert_see_other(DEV);
    let token = get_with(&c, DEV, peer(222, 2), &cookie).await.input("once");
    post(&c, AS, peer(222, 2), &cookie, &format!("csrf={csrf}&once={token}&on=1&timeout=30")).await.assert_see_other(DEV);
    assert_eq!(calls(&c).len(), 3);
}

/// Reboot Router and Factory Reset: the app's sheets as pages; the router
/// password checked on the router (never echoed; wrong ones count like
/// failed logins, and the same limit holds for both); one call per token;
/// the answer is the "Rebooting…" page itself.
#[tokio::test]
async fn reboot_and_factory_reset_need_the_router_password() {
    let (c, cookie, _) = signed_in(223).await;
    let _dev = device::set(&c.app, device::Device::default());
    let r = get_with(&c, REBOOT, peer(223, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    has(&html, &[
        "<div class=\"lead\"><a class=\"nl\" href=\"/ui/router/device\">Cancel</a></div><h1>Reboot Router</h1>",
        "<form class=\"rf req\" method=\"post\" action=\"/ui/router/device/reboot\">",
        "<p class=\"cap\">Enter your router password to confirm reboot.</p>",
        "<input id=\"rpw\" type=\"password\" name=\"password\" placeholder=\"Router Password\" autocomplete=\"current-password\" required>",
        "<p class=\"foot\">The router will restart. This takes about 60 seconds.</p>",
        "<button class=\"it cv snd danger\" type=\"submit\">Reboot</button>",
    ]);
    assert!(!html.contains("connected remotely"));
    let token = r.input("once");
    assert!(get_with(&c, REBOOT, tailnet(42223), &cookie).await.text().contains("You are connected remotely: if the connection drops"));
    let reset = get_with(&c, RESET, peer(223, 2), &cookie).await.text();
    has(&reset, &[
        "<h1>Factory Reset</h1>", "<p class=\"cap\">This will erase ALL settings. Enter your router password to confirm.</p>",
        "<button class=\"it cv snd danger\" type=\"submit\">Factory Reset</button>",
        "This will erase all settings and restore factory defaults. This cannot be undone.",
        // Never filled by the browser or a password manager: a deliberate step.
        "<input id=\"rpw\" type=\"password\" name=\"password\" placeholder=\"Router Password\" autocomplete=\"off\" required data-1p-ignore=\"true\" data-lpignore=\"true\" data-bwignore=\"true\">",
    ]);
    let remote = get_with(&c, RESET, tailnet(42223), &cookie).await.text();
    assert!(remote.contains("You are connected remotely: after a factory reset the router may not come back on your tailnet"), "{remote}");
    let csrf = page_csrf(&html);
    // Wrong: nothing sent, the password not echoed.
    let r = post(&c, REBOOT, peer(223, 3), &cookie, &format!("csrf={csrf}&once={token}&password=fake-wrong-1")).await;
    r.assert_secure_html(401);
    has(&r.text(), &["<p class=\"ferr\" id=\"rpw-err\">Incorrect password</p>", "aria-invalid=\"true\""]);
    assert!(!r.text().contains("fake-wrong-1"));
    // The session and CSRF first.
    post(&c, REBOOT, peer(223, 3), &cookie, &format!("once={token}&password={}", enc(PW))).await.assert_secure_html(403);
    post(&c, RESET, peer(223, 3), "", &format!("csrf={csrf}&password={}", enc(PW))).await.assert_see_other(&format!("/ui/login?next={}&expired=1", enc(RESET)));
    // The right password without the sheet's token.
    let r = post(&c, REBOOT, peer(223, 4), &cookie, &format!("csrf={csrf}&password={}", enc(PW))).await;
    assert!(r.status == 409 && r.text().contains("This page expired, so nothing was sent."), "{}", r.text());
    assert!(calls(&c).is_empty());
    // Right: once for a double click, both answered with the page.
    let body = format!("csrf={csrf}&once={token}&password={}", enc(PW));
    let (a, b) = tokio::join!(post(&c, REBOOT, peer(223, 4), &cookie, &body), post(&c, REBOOT, peer(223, 4), &cookie, &body));
    for r in [a, b] {
        r.assert_secure_html(200);
        has(&r.text(), &[
            "<h1>Rebooting\u{2026}</h1>", "<p class=\"msg ok\" role=\"status\">Router is rebooting...</p>",
            "The router comes back in about a minute. A restart logs every browser out, so you will log in again.",
            "<a class=\"btn\" href=\"/ui/\">Reload</a>",
        ]);
        assert!(!r.text().contains("<form") && !r.text().contains(PW));
    }
    assert_eq!(calls(&c), [("/api/device/reboot".to_owned(), json!({}))]);
    // Factory reset: its call and page; a failure shows the sheet again.
    let token = get_with(&c, RESET, peer(223, 5), &cookie).await.input("once");
    let r = post(&c, RESET, peer(223, 5), &cookie, &format!("csrf={csrf}&once={token}&password={}", enc(PW))).await;
    r.assert_secure_html(200);
    has(&r.text(), &["<h1>Resetting\u{2026}</h1>", "<p class=\"msg ok\" role=\"status\">Factory reset initiated...</p>", "join its default Wi-Fi network"]);
    assert_eq!(calls(&c)[1], ("/api/device/factory-reset".to_owned(), json!({})));
    let fail = (503, json!({"ok": false, "error": "ubus call zwrt_bsp.power factory_reset failed: fake"}));
    let _dev = device::set(&c.app, device::Device { replies: vec![("/api/device/factory-reset", fail)], ..Default::default() });
    let token = get_with(&c, RESET, peer(223, 5), &cookie).await.input("once");
    let r = post(&c, RESET, peer(223, 5), &cookie, &format!("csrf={csrf}&once={token}&password={}", enc(PW))).await;
    r.assert_secure_html(503);
    has(&r.text(), &["<p class=\"msg err\" role=\"alert\">Failed: ubus call zwrt_bsp.power factory_reset failed: fake</p>", "placeholder=\"Router Password\""]);
    assert!(!r.text().contains(PW));
    // Wrong passwords at a sheet count like logins: then even a login waits.
    let mut limited = None;
    for _ in 0..40 {
        let r = post(&c, RESET, peer(223, 6), &cookie, &format!("csrf={csrf}&password=wrong")).await;
        if r.status == 429 {
            limited = Some(r);
            break;
        }
    }
    let r = limited.expect("rate limited");
    assert!(r.header("retry-after").is_some() && r.text().contains("Too many attempts. Try again in "), "{}", r.text());
    let (pre, login_csrf) = login_form(&c, peer(223, 6)).await;
    let login = format!("login_csrf={}&next=%2Fui%2F&password={}", enc(&login_csrf), enc(PW));
    assert_eq!(post(&c, "/ui/login", peer(223, 6), &format!("zte_ui_pre={pre}"), &login).await.status, 429);
    assert_eq!(calls(&c).len(), 1);
}

/// The CSRF token in a page.
fn page_csrf(html: &str) -> String {
    html.split("name=\"csrf\" value=\"").nth(1).unwrap().split('"').next().unwrap().to_owned()
}

/// Settings: Polling and Appearance as the app's, each a cookie of this
/// browser (fixed values only); the theme draws every page, signed in or
/// not, in its scheme.
#[tokio::test]
async fn settings_preferences_are_cookies() {
    let (c, cookie, _) = signed_in(224).await;
    let html = get_with(&c, SETTINGS, peer(224, 2), &cookie).await.text();
    let order = [
        "<h2>Session</h2>", "<h2>Polling</h2>", "<form class=\"sf\" method=\"post\" action=\"/ui/settings/refresh\">",
        "<label for=\"ms\">Refresh interval</label><select id=\"ms\" name=\"ms\"><option value=\"1000\">1.0s</option>",
        "<option value=\"2000\" selected>2.0s</option>", "<option value=\"10000\">10.0s</option></select><button class=\"btn sm\" type=\"submit\">Save</button>",
        "Live upload and download use a separate 1-second refresh, matching the modem\u{2019}s update rate.",
        "<h2>Appearance</h2>", "<div class=\"seg th\" role=\"group\" aria-label=\"Theme\">",
        "<button type=\"submit\" name=\"theme\" value=\"system\" aria-pressed=\"true\">System</button>",
        "<button type=\"submit\" name=\"theme\" value=\"light\" aria-pressed=\"false\">Light</button>",
        "<button type=\"submit\" name=\"theme\" value=\"dark\" aria-pressed=\"false\">Dark</button>",
        "<h2>About</h2>", "<h2>Legal</h2>",
    ];
    let at: Vec<usize> = order.iter().map(|w| html.find(w).unwrap_or_else(|| panic!("{w} in {html}"))).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{order:?} at {at:?}");
    assert_eq!(html.matches("<option value=").count(), 19);
    let system = "<meta name=\"color-scheme\" content=\"light dark\"><meta name=\"theme-color\" media=\"(prefers-color-scheme: light)\" content=\"#f2f2f7\">";
    assert!(html.contains(&format!("<link rel=\"stylesheet\" href=\"{APP_CSS}\">{system}")), "{html}");
    let csrf = page_csrf(&html);
    // Dark: the cookie, then every page in it.
    let r = post(&c, "/ui/settings/theme", peer(224, 3), &cookie, &format!("csrf={csrf}&theme=dark")).await;
    r.assert_see_other(SETTINGS);
    assert_eq!(r.cookies(), ["zte_ui_theme=dark; Path=/ui/; Max-Age=31536000; HttpOnly; SameSite=Strict"]);
    let dark = format!(
        "<link rel=\"stylesheet\" href=\"{APP_CSS}\"><meta name=\"color-scheme\" content=\"dark\"><meta name=\"theme-color\" content=\"#000000\"><link rel=\"stylesheet\" href=\"{THEME_DARK_CSS}\">"
    );
    let dark_cookie = format!("{cookie}; zte_ui_theme=dark");
    for uri in [SETTINGS, "/ui/", DEV, "/ui/nope", "/ui/logout"] {
        let r = get_with(&c, uri, peer(224, 4), &dark_cookie).await;
        assert!(r.text().contains(&dark) && !r.text().contains("light dark"), "{uri}: {}", r.text());
        assert!(r.header("content-length").is_none_or(|n| n == r.body.len().to_string()), "{uri}");
    }
    assert!(get_with(&c, SETTINGS, peer(224, 4), &dark_cookie).await.text().contains("value=\"dark\" aria-pressed=\"true\""));
    // Signed out too (the login page), and a post's error page.
    let login = get(&c, "/ui/login", peer(224, 5)).await;
    assert!(!login.text().contains(THEME_DARK_CSS));
    let login = get_with(&c, "/ui/login", peer(224, 5), "zte_ui_theme=dark").await.text();
    assert!(login.contains(&dark), "{login}");
    let r = post(&c, "/ui/settings/theme", peer(224, 5), &dark_cookie, &format!("csrf={csrf}&theme=sepia")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains(&dark));
    // Its stylesheet is served (and cached) like app.css; what is not a
    // page is never changed (assets, live streams).
    // The page shown while no agent password is set, too.
    let unset = get_with(&ctx(None), "/ui/", peer(224, 5), "zte_ui_theme=dark").await;
    assert!(unset.text().contains(&dark), "{}", unset.text());
    let css = get(&c, THEME_DARK_CSS, peer(224, 5)).await;
    assert_eq!((css.status, css.header("content-type")), (200, Some("text/css; charset=utf-8")));
    assert_eq!(get_with(&c, APP_CSS, peer(224, 5), "zte_ui_theme=dark").await.body, get(&c, APP_CSS, peer(224, 5)).await.body);
    let live = open_live(&c, &format!("/ui/live/signal/{KEY}"), peer(224, 5), &dark_cookie).await;
    assert_eq!(live.headers()["content-type"], "text/event-stream");
    let mut b = live.into_body().into_data_stream();
    assert!(frame(&mut b).await.unwrap().starts_with("event: datastar-patch-elements\n"));
    drop(b);
    let light = format!("<meta name=\"color-scheme\" content=\"light\"><meta name=\"theme-color\" content=\"#f2f2f7\"><link rel=\"stylesheet\" href=\"{THEME_LIGHT_CSS}\">");
    assert!(get_with(&c, "/ui/", peer(224, 5), &format!("{cookie}; zte_ui_theme=light")).await.text().contains(&light));
    // A cookie value no form sets is ignored.
    assert!(get_with(&c, "/ui/", peer(224, 5), &format!("{cookie}; zte_ui_theme=%3Cb%3E")).await.text().contains(system));
    // System clears the cookie.
    let r = post(&c, "/ui/settings/theme", peer(224, 6), &dark_cookie, &format!("csrf={csrf}&theme=system")).await;
    r.assert_see_other(SETTINGS);
    assert_eq!(r.cookies(), ["zte_ui_theme=; Path=/ui/; Max-Age=0; HttpOnly; SameSite=Strict"]);
    // The refresh interval: the app's values only; the default clears it.
    let r = post(&c, "/ui/settings/refresh", peer(224, 7), &cookie, &format!("csrf={csrf}&ms=1500")).await;
    r.assert_see_other(SETTINGS);
    assert_eq!(r.cookies(), ["zte_ui_refresh=1500; Path=/ui/; Max-Age=31536000; HttpOnly; SameSite=Strict"]);
    let html = get_with(&c, SETTINGS, peer(224, 7), &format!("{cookie}; zte_ui_refresh=1500")).await.text();
    assert!(html.contains("<option value=\"1500\" selected>1.5s</option>") && !html.contains("<option value=\"2000\" selected>"));
    let r = post(&c, "/ui/settings/refresh", peer(224, 7), &cookie, &format!("csrf={csrf}&ms=2000")).await;
    assert_eq!(r.cookies(), ["zte_ui_refresh=; Path=/ui/; Max-Age=0; HttpOnly; SameSite=Strict"]);
    for bad in ["ms=1250", "ms=500", "ms=10500", "ms=%2B1500", "ms=", ""] {
        let r = post(&c, "/ui/settings/refresh", peer(224, 7), &cookie, &format!("csrf={csrf}&{bad}")).await;
        r.assert_secure_html(400);
        assert!(r.cookies().is_empty(), "{bad}");
    }
    // The session, the origin and the CSRF token first.
    post(&c, "/ui/settings/theme", peer(224, 8), "", &format!("csrf={csrf}&theme=dark")).await.assert_see_other("/ui/login?next=%2Fui%2Fsettings&expired=1");
    let r = post(&c, "/ui/settings/refresh", peer(224, 8), &cookie, "ms=1500").await;
    r.assert_secure_html(403);
    assert!(r.cookies().is_empty());
}

/// The refresh interval is the signal cadence a live page of this browser
/// asks for, only while it is open; a screen with its own keeps it.
#[tokio::test]
async fn refresh_interval_sets_the_live_signal_cadence_while_open() {
    let (c, cookie, _) = signed_in(225).await;
    let slow = format!("{cookie}; zte_ui_refresh=5000");
    let resp = open_live(&c, &format!("/ui/live/signal/{KEY}"), peer(225, 2), &slow).await;
    assert_eq!(c.app.hub.demand(Topic::Signal), 1);
    assert_eq!(c.app.hub.cadence(Topic::Signal), Some(Duration::from_secs(5)));
    let mut b = resp.into_body().into_data_stream();
    assert!(frame(&mut b).await.is_some());
    // Another page of a browser without the preference: the fastest wins.
    let other = open_live(&c, &format!("/ui/live/dashboard/{KEY}"), peer(225, 3), &cookie).await;
    assert_eq!(c.app.hub.cadence(Topic::Signal), Some(Duration::from_secs(2)));
    drop(other);
    for _ in 0..50 {
        if c.app.hub.demand(Topic::Signal) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(c.app.hub.cadence(Topic::Signal), Some(Duration::from_secs(5)));
    drop(b);
    for _ in 0..50 {
        if c.app.hub.demand(Topic::Signal) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(c.app.hub.demand(Topic::Signal), 0);
    // A page that reads nothing on its own while closed: no sampling.
    let fast = format!("{cookie}; zte_ui_refresh=1000");
    assert_eq!(get_with(&c, "/ui/signal", peer(225, 4), &fast).await.status, 200);
    assert_eq!(c.app.hub.demand(Topic::Signal), 0);
}

/// The login page: the app's header, the web's field and button.
#[tokio::test]
async fn login_page_has_the_app_header() {
    let c = ctx(Some(PW));
    let html = get(&c, "/ui/login", peer(226, 1)).await.text();
    has(&html, &[
        "<div class=\"lh\"><svg class=\"i\" viewBox=\"0 0 24 24\" aria-hidden=\"true\">",
        "<h1>ZTE U60 Pro</h1><p>Router Companion</p></div>",
        "<label for=\"password\">Agent password</label>", "<button class=\"btn btn-wide\" type=\"submit\">Log in</button>",
    ]);
    assert!(html.contains("<title>Log in · U60 Pro</title>"));
    // No brand over the app's header (the app's login has no title); other
    // signed-out pages keep it.
    assert!(html.contains("<header class=\"bar\"></header>") && !html.contains("class=\"brand\""), "{html}");
    assert!(get(&c, "/ui/nope", peer(226, 1)).await.text().contains("<span class=\"brand\">U60 Pro</span>"));
}
