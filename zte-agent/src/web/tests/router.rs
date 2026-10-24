//! Router › Mobile Network (Apply, scan, registration, Reboot Required),
//! Cell Lock, Smart Tower Connect and Signal Detection through the whole
//! router. Device writes go to `pages::device`, page-load reads to its
//! `gets`, topics to `pages::fake`. Every value is a made-up fixture
//! (PLMN 00101 is the test network).

use serde_json::json;

use super::*;
use crate::web::pages::{device, fake};

const APPLY: &str = "/ui/router/mobile-network/apply";
const SCAN: &str = "/ui/router/mobile-network/scan";
const REG: &str = "/ui/router/mobile-network/register";
const REBOOT: &str = "/ui/router/mobile-network/reboot";
const CELL: &str = "/ui/router/cell-lock";
const STC: &str = "/ui/router/stc";
const SD: &str = "/ui/router/signal-detect";

fn calls(c: &Ctx) -> Vec<(String, Value)> {
    device::calls(&c.app)
}

fn tailnet(port: u16) -> Peer {
    Peer(SocketAddr::from(([127, 0, 0, 1], port)))
}

/// A device whose page-load reads answer with these values.
fn reads(gets: Vec<(&'static str, Result<Value, String>)>) -> device::Device {
    device::Device { gets, ..Default::default() }
}

fn wwan() -> (&'static str, Result<Value, String>) {
    ("/api/modem/data", Ok(json!({"enable": 1, "roam_enable": 0, "connect_mode": 1, "connect_status": "ipv4_connected"})))
}

fn online() -> (&'static str, Result<Value, String>) {
    ("/api/modem/status", Ok(json!({"operate_mode": "ONLINE"})))
}

/// The CSRF token in a page.
fn page_csrf(html: &str) -> String {
    html.split("name=\"csrf\" value=\"").nth(1).unwrap().split('"').next().unwrap().to_owned()
}

/// `location` of a 303.
fn to(r: &Reply) -> String {
    assert_eq!(r.status, 303, "{}", r.text());
    r.header("location").unwrap().to_owned()
}

/// The page as served: the app's sections and controls, the Apply form
/// with what it showed (`data-i`: the router's values), one single-use
/// token for its two forms (Apply, the scan).
#[tokio::test]
async fn mobile_network_page_follows_the_app() {
    let (c, cookie, _) = signed_in(140).await;
    let _dev = device::set(&c.app, reads(vec![wwan(), online()]));
    let _sig = fake::set(&c.app, json!({"net_select_mode": "auto_select"}));
    let r = get_with(&c, MN, peer(140, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h2>Connectivity</h2>", "Connected \u{2014} Ipv4 Connected",
        "<form id=\"mn-scan\" method=\"post\" action=\"/ui/router/mobile-network/scan\"><input type=\"hidden\" name=\"csrf\"",
        "<form class=\"mn\" method=\"post\" action=\"/ui/router/mobile-network/apply\"><input type=\"hidden\" name=\"csrf\"",
        "<input type=\"hidden\" name=\"cm0\" value=\"1\"><input type=\"hidden\" name=\"roam0\" value=\"0\"><input type=\"hidden\" name=\"sel0\" value=\"auto_select\"><fieldset class=\"mnf\">",
        "<h2>Connection Mode</h2>", "id=\"cm-a\" name=\"cm\" value=\"1\" checked data-i><span>Automatic</span>",
        "id=\"cm-m\" name=\"cm\" value=\"0\"><span>Manual</span>",
        "<input class=\"sr\" id=\"roam\" type=\"checkbox\" role=\"switch\" name=\"roam\" value=\"1\"><span class=\"sw\" aria-hidden=\"true\"></span>",
        "Enabling roaming may incur additional charges from your carrier.", "<h2>Network Selection</h2>",
        "id=\"sel-a\" name=\"sel\" value=\"auto_select\" checked data-i><span>Automatic</span>",
        // The scan posts at once from this (local) browser.
        "<input type=\"hidden\" name=\"confirm\" value=\"1\"></form>",
        "<li class=\"man\"><button class=\"it cv bt\" type=\"submit\" form=\"mn-scan\">Scan Networks</button></li>",
        "<button class=\"it cv snd\" type=\"submit\">Apply</button>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let tokens: Vec<&str> = html.split("name=\"once\" value=\"").skip(1).map(|t| t.split('"').next().unwrap()).collect();
    assert!(tokens.len() == 2 && tokens[0] == tokens[1] && tokens[0].len() == 16, "{tokens:?}");
    assert!(!html.contains("/ui/a/datastar-") && !html.contains("style=") && !html.contains("Failed"));
    // Over Tailscale the scan asks first (no `confirm`).
    let remote = get_with(&c, MN, tailnet(40140), &cookie).await.text();
    assert!(remote.contains("<form id=\"mn-scan\"") && !remote.contains("name=\"confirm\""), "{remote}");
    let csrf = page_csrf(&remote);
    let _dev = device::set(&c.app, reads(vec![wwan(), online()]));
    let r = post(&c, SCAN, tailnet(40140), &cookie, &format!("csrf={csrf}&once=0123456789abcdef")).await;
    assert_eq!(to(&r), SCAN);
    let q = get_with(&c, SCAN, tailnet(40140), &cookie).await.text();
    for want in ["<h2 class=\"q\">Scan for networks?</h2>", "Some modems pause mobile data while they search.", "You are connected remotely", ">Scan Networks</button>"] {
        assert!(q.contains(want), "{want} in {q}");
    }
    assert!(calls(&c).is_empty());
    // Manual selection and roaming on, as read.
    let _dev = device::set(&c.app, reads(vec![("/api/modem/data", Ok(json!({"enable": "1", "roam_enable": "1", "connect_mode": "0"}))), online()]));
    let _sig = fake::set(&c.app, json!({"net_select_mode": "manual_select"}));
    let html = get_with(&c, MN, peer(140, 3), &cookie).await.text();
    for want in [
        "name=\"cm\" value=\"0\" checked data-i>", "name=\"roam\" value=\"1\" checked data-i>", "name=\"sel\" value=\"manual_select\" checked data-i>",
        "name=\"roam0\" value=\"1\"", "name=\"sel0\" value=\"manual_select\"", "name=\"cm0\" value=\"0\"",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // Mobile data off but still connected: the switch on (the app's); the
    // load errors above Connectivity, in the app's words.
    let _dev = device::set(&c.app, reads(vec![
        ("/api/modem/data", Ok(json!({"enable": 0, "roam_enable": 0, "connect_mode": 1, "connect_status": "ipv4_connected"}))),
        ("/api/modem/status", Err("no".into())),
    ]));
    let html = get_with(&c, MN, peer(140, 4), &cookie).await.text();
    assert!(html.contains("aria-checked=\"true\" aria-label=\"Mobile Data\"") || html.contains("aria-label=\"Mobile Data\" checked"), "{html}");
    let (err, conn) = (html.find("<p class=\"msg err\" role=\"alert\">Failed to load airplane mode status</p>"), html.find("<h2>Connectivity</h2>"));
    assert!(err.is_some() && err < conn, "{html}");
    let _dev = device::set(&c.app, reads(vec![("/api/modem/data", Err("ubus <down>".into())), ("/api/modem/status", Err("no".into()))]));
    let html = get_with(&c, MN, peer(140, 4), &cookie).await.text();
    assert!(html.contains("role=\"alert\">Failed to load: ubus &lt;down&gt;</p>") && !html.contains("airplane mode status"), "{html}");
}

/// Apply sends only what changed (the router's current values for the
/// rest), once per token, then follows the modem; roaming on is asked
/// first; Automatic selection sends the app's call; Manual alone sends
/// nothing; a failed read sends nothing and gives the token back.
#[tokio::test]
async fn apply_sends_what_changed_once() {
    let (c, cookie, _) = signed_in(141).await;
    let _dev = device::set(&c.app, reads(vec![wwan(), online()]));
    let _sig = fake::set(&c.app, json!({"net_select_mode": "auto_select"}));
    let page = get_with(&c, MN, peer(141, 2), &cookie).await;
    let csrf = page.input("csrf");
    let once = || async { get_with(&c, MN, peer(141, 2), &cookie).await.text().split("name=\"once\" value=\"").nth(2).unwrap().split('"').next().unwrap().to_owned() };
    let fields = |cm: &str, roam: &str, sel: &str| format!("cm={cm}&roam={roam}&sel={sel}&cm0=1&roam0=0&sel0=auto_select");
    // Connection mode Manual: one write, the operation, a replay finds it.
    let token = once().await;
    let body = format!("csrf={csrf}&once={token}&{}", fields("0", "0", "auto_select"));
    let (a, b) = tokio::join!(post(&c, APPLY, peer(141, 3), &cookie, &body), post(&c, APPLY, peer(141, 3), &cookie, &body));
    assert_eq!((to(&a), to(&b)), (format!("/ui/op/{token}"), format!("/ui/op/{token}")));
    assert_eq!(calls(&c), [("/api/modem/data".to_owned(), json!({"cid": 1, "connect_mode": 0, "roam_enable": 0, "enable": 1}))]);
    let op = get_with(&c, &format!("/ui/op/{token}"), peer(141, 3), &cookie).await;
    op.assert_secure_html(200);
    assert!(op.text().contains("<h1 id=\"op-title\">Applying settings</h1>") && op.text().contains("Back to Mobile Network"), "{}", op.text());
    // Roaming on: asked first (a post without `confirm=1` goes there).
    let token = once().await;
    let body = format!("csrf={csrf}&once={token}&{}", fields("1", "1", "auto_select"));
    let q_url = to(&post(&c, APPLY, peer(141, 4), &cookie, &body).await);
    assert_eq!(q_url, format!("{APPLY}?{}", fields("1", "1", "auto_select")));
    let q = get_with(&c, &q_url, peer(141, 4), &cookie).await;
    assert!(q.text().contains("<h2 class=\"q\">Turn on data roaming?</h2>") && q.text().contains("<button class=\"btn btn-danger\" type=\"submit\">Turn on data roaming</button>"));
    assert_eq!(calls(&c).len(), 1);
    let r = post(&c, APPLY, peer(141, 4), &cookie, &format!("csrf={csrf}&once={}&confirm=1&{}", q.input("once"), fields("1", "1", "auto_select"))).await;
    assert!(to(&r).starts_with("/ui/op/"));
    assert_eq!(calls(&c)[1], ("/api/modem/data".to_owned(), json!({"cid": 1, "connect_mode": 1, "roam_enable": 1, "enable": 1})));
    // Automatic selection again: the app's call; Manual alone: nothing sent.
    let body = format!("csrf={csrf}&once={}&cm=1&roam=0&sel=auto_select&cm0=1&roam0=0&sel0=manual_select", once().await);
    let html = get_with(&c, &to(&post(&c, APPLY, peer(141, 5), &cookie, &body).await), peer(141, 5), &cookie).await.text();
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">Settings applied</p>"), "{html}");
    assert_eq!(calls(&c)[2], ("/api/modem/network-mode".to_owned(), json!({"net_select_mode": "auto_select"})));
    let body = format!("csrf={csrf}&once={}&{}", once().await, fields("1", "0", "manual_select"));
    let back = to(&post(&c, APPLY, peer(141, 5), &cookie, &body).await);
    assert!(back.starts_with("/ui/router/mobile-network?sel=manual&r="), "{back}");
    let html = get_with(&c, &back, peer(141, 5), &cookie).await.text();
    // The app's line; Manual stays chosen (Scan shown).
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">Settings applied</p>") && html.contains("value=\"manual_select\" checked>"), "{html}");
    assert_eq!(calls(&c).len(), 3);
    // Over Tailscale, Automatic selection and Manual connection mode ask
    // first, with the remote line; a selection that fails after the data
    // write says the connection settings were applied.
    let both = "cm=0&roam=0&sel=auto_select&cm0=1&roam0=0&sel0=manual_select";
    let q_url = to(&post(&c, APPLY, tailnet(40141), &cookie, &format!("csrf={csrf}&once={}&{both}", once().await)).await);
    let q = get_with(&c, &q_url, tailnet(40141), &cookie).await;
    for want in ["<h2 class=\"q\">Switch to automatic network selection?</h2>", "Mobile data no longer reconnects by itself", "You are connected remotely"] {
        assert!(q.text().contains(want), "{want} in {}", q.text());
    }
    let _dev = device::set(&c.app, device::Device {
        replies: vec![("/api/modem/network-mode", (500, json!({"ok": false, "error": "Invalid argument"})))],
        gets: vec![wwan(), online()],
        ..Default::default()
    });
    let r = post(&c, APPLY, tailnet(40141), &cookie, &format!("csrf={csrf}&once={}&confirm=1&{both}", q.input("once"))).await;
    let html = get_with(&c, &to(&r), tailnet(40141), &cookie).await.text();
    assert!(html.contains("role=\"alert\">Connection settings applied; Automatic selection failed: Invalid argument</p>"), "{html}");
    assert_eq!(calls(&c).len(), 2);
    // The state cannot be read: nothing sent, the token given back.
    let token = once().await;
    let _dev = device::set(&c.app, reads(vec![("/api/modem/data", Err("ubus <down>".into())), online()]));
    let body = format!("csrf={csrf}&once={token}&{}", fields("0", "0", "auto_select"));
    let html = get_with(&c, &to(&post(&c, APPLY, peer(141, 6), &cookie, &body).await), peer(141, 6), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed: ubus &lt;down&gt;</p>"), "{html}");
    assert!(calls(&c).is_empty());
    let _dev = device::set(&c.app, reads(vec![wwan(), online()]));
    assert!(to(&post(&c, APPLY, peer(141, 6), &cookie, &body).await).starts_with("/ui/op/"));
    assert_eq!(calls(&c).len(), 1);
    // A token of another form, or none: nothing sent.
    let body = format!("csrf={csrf}&once=00000000000000ff&{}", fields("0", "0", "auto_select"));
    let html = get_with(&c, &to(&post(&c, APPLY, peer(141, 7), &cookie, &body).await), peer(141, 7), &cookie).await.text();
    assert!(html.contains("This page expired, so nothing was sent. Try again."), "{html}");
    assert_eq!(calls(&c).len(), 1);
}

/// Scan: once, an operation on `netscan` (the app's first read after 2 s,
/// 60 s at most), then the networks found under Manual; each asks before
/// it registers, which follows the result.
#[tokio::test]
async fn scan_lists_networks_and_registers_after_asking() {
    let clock = FakeClock::at_secs(1000);
    let c = ctx_at(clock.clone());
    let session = login(&c, 142, 1).await;
    let cookie = format!("zte_ui_session={session}");
    let _dev = device::set(&c.app, reads(vec![wwan(), online()]));
    let _sig = fake::set(&c.app, json!({"net_select_mode": "auto_select", "scan": {"m_netselect_status": "manual_selected"}}));
    let page = get_with(&c, MN, peer(142, 2), &cookie).await;
    let (csrf, scan_token) = (page.input("csrf"), page.input("once"));
    let r = post(&c, SCAN, peer(142, 3), &cookie, &format!("csrf={csrf}&once={scan_token}&confirm=1")).await;
    let op = to(&r);
    assert_eq!(op, format!("/ui/op/{scan_token}"));
    assert_eq!(calls(&c), [("/api/modem/scan".to_owned(), json!({}))]);
    // The previous scan's "done" is not believed for 2 s.
    let html = get_with(&c, &op, peer(142, 3), &cookie).await.text();
    assert!(html.contains("<h1 id=\"op-title\">Scanning networks</h1>") && html.contains("Scanning for networks\u{2026}"), "{html}");
    assert!(html.contains("http-equiv=\"refresh\"") && html.contains("@get('/ui/live/op/"));
    clock.advance(Duration::from_secs(2));
    let html = get_with(&c, &op, peer(142, 3), &cookie).await.text();
    assert!(html.contains("<h1 id=\"op-title\">Scan complete</h1>") && html.contains("Choose a network under Network Selection."), "{html}");
    assert!(html.contains("href=\"/ui/router/mobile-network?scan=1\">Back to Mobile Network"), "{html}");
    // The networks (the firmware's string), Manual chosen.
    let results = json!({"m_netselect_contents": "2,Test Net,00101,7;1,Other <b>Net</b>,00102,12;3,Barred,00103,7"});
    let _dev = device::set(&c.app, reads(vec![
        wwan(), online(),
        ("/api/modem/scan/status", Ok(json!({"m_netselect_status": "manual_selected"}))),
        ("/api/modem/scan/results", Ok(results)),
    ]));
    let html = get_with(&c, &format!("{MN}?scan=1"), peer(142, 4), &cookie).await.text();
    for want in [
        "name=\"sel\" value=\"manual_select\" checked>", "name=\"sel0\" value=\"auto_select\"",
        "<a class=\"it net\" href=\"/ui/router/mobile-network/register?plmn=00101&amp;rat=7\"><span class=\"cvb\"><span class=\"nm\">Test Net</span><small>00101 \u{b7} 7</small></span><span class=\"c-green\"><svg",
        "<span class=\"nm\">Other &lt;b&gt;Net&lt;/b&gt;</span>",
        "<span class=\"it off net\" aria-disabled=\"true\"><span class=\"cvb\"><span class=\"nm\">Barred</span>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("register?plmn=00103"));
    // Without `scan=1`, or before the scan is done: none.
    assert!(!get_with(&c, MN, peer(142, 4), &cookie).await.text().contains("class=\"it net\""));
    // Registering asks first (the remote line over Tailscale), naming the
    // network as the scan did (a name in the URL is ignored).
    let q = get_with(&c, &format!("{REG}?plmn=00101&rat=7&name=Your%20Carrier"), tailnet(40142), &cookie).await;
    q.assert_secure_html(200);
    for want in [
        "<h2 class=\"q\">Register to Test Net?</h2>", "The modem leaves its current network and registers to Test Net (00101).",
        "You are connected remotely", "<button class=\"btn btn-danger\" type=\"submit\">Register</button>",
        "href=\"/ui/router/mobile-network?scan=1\">Cancel",
    ] {
        assert!(q.text().contains(want), "{want} in {}", q.text());
    }
    for bad in ["?plmn=001&rat=7", "?plmn=00101&rat=", "?plmn=00101&rat=%3Cx%3E", "?plmn=00101", "?plmn=00101&rat=abc", "?plmn=00101&rat=14"] {
        get_with(&c, &format!("{REG}{bad}"), peer(142, 5), &cookie).await.assert_secure_html(400);
    }
    let _sig = fake::set(&c.app, json!({"register": {"m_netselect_result": "manual_success"}}));
    let body = format!("csrf={csrf}&plmn=00101&rat=7&name=Test%20Net&once={}&confirm=1", q.input("once"));
    let op = to(&post(&c, REG, peer(142, 5), &cookie, &body).await);
    assert_eq!(calls(&c), [("/api/modem/register".to_owned(), json!({"m_mcc_mnc": "00101", "m_rat": "7"}))]);
    clock.advance(Duration::from_secs(3));
    let html = get_with(&c, &op, peer(142, 5), &cookie).await.text();
    assert!(html.contains("<h1 id=\"op-title\">Registered to network</h1>"), "{html}");
    // A refused registration, and a scan that fails to start.
    let _sig = fake::set(&c.app, json!({"register": {"m_netselect_result": "fail"}}));
    let q = get_with(&c, &format!("{REG}?plmn=00102&rat=12"), peer(142, 6), &cookie).await;
    assert!(q.text().contains("<h2 class=\"q\">Register to Other &lt;b&gt;Net&lt;/b&gt;?</h2>"), "{}", q.text());
    let op = to(&post(&c, REG, peer(142, 6), &cookie, &format!("csrf={csrf}&plmn=00102&rat=12&once={}&confirm=1", q.input("once"))).await);
    clock.advance(Duration::from_secs(3));
    let html = get_with(&c, &op, peer(142, 6), &cookie).await.text();
    assert!(html.contains("<h1 id=\"op-title\">Registration failed</h1>") && html.contains("role=\"alert\">The modem reports that the registration failed."), "{html}");
    // A network the last scan does not offer (a crafted link, a forbidden
    // one): the question has nothing to do; a post sends nothing.
    for net in ["plmn=99999&rat=7", "plmn=00103&rat=7"] {
        let q = get_with(&c, &format!("{REG}?{net}"), peer(142, 8), &cookie).await;
        assert!(q.text().contains("<h2 class=\"q\">This network is not in the last scan.</h2>") && !q.text().contains(">Register</button>"), "{}", q.text());
        let token = get_with(&c, MN, peer(142, 8), &cookie).await.input("once");
        let r = post(&c, REG, peer(142, 8), &cookie, &format!("csrf={csrf}&{net}&once={token}&confirm=1")).await;
        let html = get_with(&c, &to(&r), peer(142, 8), &cookie).await.text();
        assert!(html.contains("role=\"alert\">This network is not in the last scan. Scan again.</p>"), "{html}");
    }
    assert_eq!(calls(&c).len(), 2);
    let _dev = device::set(&c.app, device::Device { replies: vec![("/api/modem/scan", (503, json!({"ok": false, "error": "busy"})))], gets: vec![wwan(), online()], ..Default::default() });
    let token = get_with(&c, MN, peer(142, 7), &cookie).await.input("once");
    let r = post(&c, SCAN, peer(142, 7), &cookie, &format!("csrf={csrf}&once={token}&confirm=1")).await;
    let html = get_with(&c, &to(&r), peer(142, 7), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Scan failed: busy</p>"), "{html}");
}

/// Airplane mode off is tried twice (the host has no AT port), then
/// "Reboot Required": the app's message, the router password (never
/// echoed; wrong ones count like logins), "Reboot Now".
#[tokio::test]
async fn airplane_off_failing_twice_offers_a_reboot_with_the_password() {
    let (c, cookie, _) = signed_in(143).await;
    let csrf = get_with(&c, "/ui/logout", peer(143, 2), &cookie).await.input("csrf");
    // Its switch posts the page's single-use token; without one nothing is sent.
    let r = post(&c, &format!("{MN}/airplane"), peer(143, 2), &cookie, &format!("csrf={csrf}&mode=ONLINE")).await;
    let html = get_with(&c, &to(&r), peer(143, 2), &cookie).await.text();
    assert!(html.contains("This page expired, so nothing was sent."), "{html}");
    let fail = (500, json!({"ok": false, "error": "AT+CFUN=1 failed (fake)"}));
    let _dev = device::set(&c.app, device::Device { replies: vec![("/api/modem/airplane", fail)], ..Default::default() });
    let token = get_with(&c, MN, peer(143, 2), &cookie).await.input("once");
    // Three posts at once: tried twice (the app's retry) once, all three on
    // the same Reboot Required line.
    let (body, air) = (format!("csrf={csrf}&mode=ONLINE&once={token}"), format!("{MN}/airplane"));
    let (a, b, d) = tokio::join!(
        post(&c, &air, peer(143, 2), &cookie, &body),
        post(&c, &air, peer(143, 2), &cookie, &body),
        post(&c, &air, peer(143, 2), &cookie, &body)
    );
    let page = to(&a);
    assert_eq!(page, format!("{REBOOT}?r={token}"));
    assert_eq!((to(&b), to(&d)), (page.clone(), page.clone()));
    assert_eq!(calls(&c), vec![("/api/modem/airplane".to_owned(), json!({"operate_mode": "ONLINE"})); 2]);
    assert_eq!(to(&post(&c, &air, peer(143, 2), &cookie, &body).await), page);
    assert_eq!(calls(&c).len(), 2);
    let r = get_with(&c, &page, peer(143, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<div class=\"lead\"><a class=\"nl back\" href=\"/ui/router/mobile-network\">Mobile Network</a></div><h1>Reboot Required</h1>",
        "<p class=\"msg err\" role=\"alert\">Failed: ",
        "Due to a firmware limitation, the cellular radio cannot be restored without rebooting. The router will restart (about 60 seconds).",
        "<label for=\"rpw\">Router Password</label><input id=\"rpw\" type=\"password\" name=\"password\" autocomplete=\"current-password\" required>",
        "<button class=\"btn btn-danger\" type=\"submit\">Reboot Now</button>", "href=\"/ui/router/mobile-network\">Cancel",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("connected remotely"));
    let token = r.input("once");
    // Over Tailscale: the remote line.
    let remote = get_with(&c, REBOOT, tailnet(40143), &cookie).await.text();
    assert!(remote.contains("You are connected remotely"), "{remote}");
    let _dev = device::set(&c.app, device::Device::default());
    // Wrong: nothing sent, the password not echoed.
    let r = post(&c, REBOOT, peer(143, 3), &cookie, &format!("csrf={csrf}&password=nope-42")).await;
    r.assert_secure_html(401);
    assert!(r.text().contains("<p class=\"ferr\" id=\"rpw-err\">Incorrect password</p>") && !r.text().contains("nope-42"), "{}", r.text());
    assert!(calls(&c).is_empty());
    // CSRF and the session first.
    post(&c, REBOOT, peer(143, 3), &cookie, &format!("password={}", enc(PW))).await.assert_secure_html(403);
    post(&c, REBOOT, peer(143, 3), "", &format!("csrf={csrf}&password={}", enc(PW))).await.assert_see_other(&format!("/ui/login?next={}&expired=1", enc(REBOOT)));
    assert!(calls(&c).is_empty());
    // The right password without the page's token: nothing sent.
    let r = post(&c, REBOOT, peer(143, 4), &cookie, &format!("csrf={csrf}&password={}", enc(PW))).await;
    assert!(r.status == 409 && r.text().contains("This page expired, so nothing was sent."), "{}", r.text());
    assert!(calls(&c).is_empty());
    // Right: the reboot once for a double click; both answered with the
    // "Rebooting…" page itself (the router may not answer a redirect).
    let body = format!("csrf={csrf}&once={token}&password={}", enc(PW));
    let (a, b) = tokio::join!(post(&c, REBOOT, peer(143, 4), &cookie, &body), post(&c, REBOOT, peer(143, 4), &cookie, &body));
    for r in [a, b] {
        r.assert_secure_html(200);
        let html = r.text();
        assert!(html.contains("<h1>Rebooting\u{2026}</h1>") && html.contains("<p class=\"msg ok\" role=\"status\">Router is rebooting...</p>"), "{html}");
        assert!(html.contains("<a class=\"btn\" href=\"/ui/\">Reload</a>") && !html.contains("<form") && !html.contains(PW), "{html}");
    }
    assert_eq!(calls(&c), [("/api/device/reboot".to_owned(), json!({}))]);
    // Wrong passwords are rate limited like logins: then even the right one waits.
    let mut limited = None;
    for _ in 0..40 {
        let r = post(&c, REBOOT, peer(143, 5), &cookie, &format!("csrf={csrf}&password=wrong")).await;
        if r.status == 429 {
            limited = Some(r);
            break;
        }
    }
    let r = limited.expect("rate limited");
    assert!(r.header("retry-after").is_some() && r.text().contains("Too many attempts. Try again in "), "{}", r.text());
    let token = get_with(&c, REBOOT, peer(143, 5), &cookie).await.input("once");
    assert_eq!(post(&c, REBOOT, peer(143, 5), &cookie, &format!("csrf={csrf}&once={token}&password={}", enc(PW))).await.status, 429);
    assert_eq!(calls(&c).len(), 1);
}

/// Cell Lock: the reading's cell, the lock questions (typed values
/// checked, kept on Cancel and after the lock), the firmware's names, one
/// call per token, the neighbour scan and its lists, Unlock.
#[tokio::test]
async fn cell_lock_asks_locks_once_and_scans() {
    let (c, cookie, _) = signed_in(144).await;
    let cell = json!({"nr5g_pci": "774", "nr5g_action_channel": "513000", "nr5g_action_band": "n41", "lte_pci": 178, "wan_active_channel": "301"});
    let _sig = fake::set(&c.app, cell.clone());
    let r = get_with(&c, CELL, peer(144, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>Cell Lock</h1>", "<h2>Current Cell</h2>", "<span>NR PCI</span><span class=\"v\">774</span>",
        "<span>NR EARFCN</span><span class=\"v\">513000</span>", "<span>NR Band</span><span class=\"v\">n41</span>",
        "<span>LTE PCI</span><span class=\"v\">178</span>", "<span>LTE EARFCN</span><span class=\"v\">301</span>",
        "<span>Locked</span><span class=\"v\">\u{2014}</span>",
        "<form class=\"rf req\" method=\"get\" action=\"/ui/router/cell-lock/nr\">", "<h2>Lock NR Cell</h2>",
        "<input id=\"nr_pci\" type=\"text\" name=\"nr_pci\" value=\"\" placeholder=\"PCI\" inputmode=\"numeric\"",
        "placeholder=\"Band (optional)\"", "<button class=\"it cv snd\" type=\"submit\">Lock NR</button>", "<h2>Lock LTE Cell</h2>",
        "<form class=\"sf\" method=\"post\" action=\"/ui/router/cell-lock/scan\"><input type=\"hidden\" name=\"csrf\"",
        "<button class=\"it cv bt\" type=\"submit\">Scan Neighbors</button>",
        "<a class=\"it bt danger\" href=\"/ui/router/cell-lock/reset\">Unlock Cell</a>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // A reading that says: Yes (orange) with Unlock, No (green) without.
    let _sig = fake::set(&c.app, json!({"cell_lock_status": "1"}));
    assert!(get_with(&c, CELL, peer(144, 2), &cookie).await.text().contains("<span class=\"v c-orange\">Yes</span>"));
    let _sig = fake::set(&c.app, json!({"cell_lock_status": 0}));
    let html = get_with(&c, CELL, peer(144, 2), &cookie).await.text();
    assert!(html.contains("<span class=\"v c-green\">No</span>") && !html.contains("Unlock Cell"), "{html}");
    // Typed values out of range: the form again as typed, a message per field.
    let r = get_with(&c, &format!("{CELL}/nr?nr_pci=2000&nr_earfcn=&nr_band=%3Cb%3E"), peer(144, 3), &cookie).await;
    r.assert_secure_html(400);
    let html = r.text();
    for want in [
        "value=\"2000\"", "aria-invalid=\"true\"", "<p class=\"ferr\" id=\"nr_pci-err\">Enter a whole number from 0 to 1007.</p>",
        "<p class=\"ferr\" id=\"nr_earfcn-err\">PCI and EARFCN are required</p>", "value=\"&lt;b&gt;\"",
        "<p class=\"ferr\" id=\"nr_band-err\">Enter a whole number from 1 to 1024.</p>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    get_with(&c, &format!("{CELL}/lte?lte_pci=504&lte_earfcn=1"), peer(144, 3), &cookie).await.assert_secure_html(400);
    // The question; Cancel keeps what was typed.
    let _dev = device::set(&c.app, device::Device::default());
    let q = get_with(&c, &format!("{CELL}/nr?nr_pci=774&nr_earfcn=513000&nr_band="), peer(144, 4), &cookie).await;
    q.assert_secure_html(200);
    for want in [
        "<h2 class=\"q\">Lock this NR cell?</h2>", "The modem will use only the 5G NR cell with PCI 774 on NR-ARFCN 513000.",
        "<button class=\"btn btn-danger\" type=\"submit\">Lock NR</button>", "href=\"/ui/router/cell-lock?nr_pci=774&amp;nr_earfcn=513000\">Cancel",
    ] {
        assert!(q.text().contains(want), "{want} in {}", q.text());
    }
    assert!(!q.text().contains("connected remotely"));
    let csrf = q.input("csrf");
    let body = format!("csrf={csrf}&nr_pci=774&nr_earfcn=513000&once={}&confirm=1", q.input("once"));
    let nr = format!("{CELL}/nr");
    let (a, b) = tokio::join!(post(&c, &nr, peer(144, 4), &cookie, &body), post(&c, &nr, peer(144, 4), &cookie, &body));
    assert_eq!(calls(&c), [("/api/cell/lock/nr".to_owned(), json!({"lock_nr_pci": "774", "lock_nr_earfcn": "513000"}))]);
    let back = to(&a);
    assert!(back.starts_with("/ui/router/cell-lock?nr_pci=774&nr_earfcn=513000&locked=1&r=") && to(&b).starts_with("/ui/router/cell-lock?nr_pci=774"), "{back}");
    let html = get_with(&c, &back, peer(144, 4), &cookie).await.text();
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">NR cell locked</p>") && html.contains("name=\"nr_pci\" value=\"774\""), "{html}");
    // As the app: locked, with Unlock, even while the reading says not.
    let _sig = fake::set(&c.app, json!({"cell_lock_status": 0}));
    let html = get_with(&c, &back, peer(144, 4), &cookie).await.text();
    assert!(html.contains("<span>Locked</span><span class=\"v c-orange\">Yes</span>") && html.contains("Unlock Cell"), "{html}");
    // LTE and Unlock over Tailscale: the remote line.
    let q = get_with(&c, &format!("{CELL}/lte?lte_pci=178&lte_earfcn=301"), tailnet(40144), &cookie).await;
    assert!(q.text().contains("The modem will use only the LTE cell with PCI 178 on EARFCN 301.") && q.text().contains("You are connected remotely"));
    post(&c, &format!("{CELL}/lte"), tailnet(40144), &cookie, &format!("csrf={csrf}&lte_pci=178&lte_earfcn=301&once={}&confirm=1", q.input("once"))).await;
    let q = get_with(&c, &format!("{CELL}/reset"), tailnet(40144), &cookie).await;
    assert!(q.text().contains("<h2 class=\"q\">Unlock the cell?</h2>") && q.text().contains("band locks may be undone too"), "{}", q.text());
    let r = post(&c, &format!("{CELL}/reset"), tailnet(40144), &cookie, &format!("csrf={csrf}&once={}&confirm=1", q.input("once"))).await;
    let _sig = fake::set(&c.app, json!({"cell_lock_status": 1}));
    let html = get_with(&c, &to(&r), tailnet(40144), &cookie).await.text();
    assert!(html.contains(">Cell lock reset</p>") && html.contains("<span class=\"v c-green\">No</span>") && !html.contains("Unlock Cell"), "{html}");
    let all = calls(&c);
    assert_eq!(all[1], ("/api/cell/lock/lte".to_owned(), json!({"lock_lte_pci": "178", "lock_lte_earfcn": "301"})));
    assert_eq!(all[2], ("/api/cell/lock/reset".to_owned(), json!({})));
    // With a band: its name.
    let q = get_with(&c, &format!("{CELL}/nr?nr_pci=1&nr_earfcn=2&nr_band=78"), peer(144, 5), &cookie).await;
    post(&c, &format!("{CELL}/nr"), peer(144, 5), &cookie, &format!("csrf={csrf}&nr_pci=1&nr_earfcn=2&nr_band=78&once={}&confirm=1", q.input("once"))).await;
    assert_eq!(calls(&c)[3].1, json!({"lock_nr_pci": "1", "lock_nr_earfcn": "2", "lock_nr_cell_band": "78"}));
    // Neighbour scan: once, then both lists.
    let _dev = device::set(&c.app, device::Device {
        gets: vec![
            ("/api/cell/neighbors/nr", Ok(json!({"cell_list": [{"pci": "775", "earfcn": "513000", "band": "41", "rsrp": "-95"}]}))),
            ("/api/cell/neighbors/lte", Ok(json!({"cell_list": [{"pci": 179, "earfcn": 301, "band": 1, "rsrp": -101}]}))),
        ],
        ..Default::default()
    });
    let token = get_with(&c, CELL, peer(144, 6), &cookie).await.input("once");
    let r = post(&c, &format!("{CELL}/scan"), peer(144, 6), &cookie, &format!("csrf={csrf}&once={token}&confirm=1")).await;
    r.assert_see_other("/ui/router/cell-lock?nbr=1");
    assert_eq!(calls(&c), [("/api/cell/neighbors/scan".to_owned(), json!({}))]);
    let html = get_with(&c, &format!("{CELL}?nbr=1"), peer(144, 6), &cookie).await.text();
    for want in [
        "<p class=\"msg ok\" role=\"status\">Found 2 neighbor cell(s)</p>",
        "<b>NR</b><span class=\"muted\">Band 41</span></span><small>PCI: 775\u{a0}\u{a0}EARFCN: 513000\u{a0}\u{a0}RSRP: -95</small>",
        "<b>LTE</b><span class=\"muted\">Band 1</span></span><small>PCI: 179\u{a0}\u{a0}EARFCN: 301\u{a0}\u{a0}RSRP: -101</small>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // A replay leads to the same lists; nothing sent again.
    post(&c, &format!("{CELL}/scan"), peer(144, 6), &cookie, &format!("csrf={csrf}&once={token}&confirm=1")).await.assert_see_other("/ui/router/cell-lock?nbr=1");
    assert_eq!(calls(&c).len(), 1);
    let _dev = device::set(&c.app, device::Device::default());
    assert!(get_with(&c, &format!("{CELL}?nbr=1"), peer(144, 6), &cookie).await.text().contains(">No neighbors found</p>"));
    // The reading fails (no fake: the host has no ubus): the app's line.
    drop(_sig);
    let html = get_with(&c, CELL, peer(144, 7), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed to load cell info: ") && !html.contains("Couldn't update"), "{html}");
}

/// Smart Tower Connect: the state and parameters (either name set), the
/// app's load error, whole numbers only, and the questions of its controls.
#[tokio::test]
async fn stc_shows_its_state_checks_numbers_and_asks() {
    let (c, cookie, _) = signed_in(145).await;
    let r = get_with(&c, STC, peer(145, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    // No device: the app's line, the state unknown, both switches offered.
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed to load STC: "), "{html}");
    assert!(html.contains("<span>STC</span><span class=\"v\">Unavailable</span>"));
    assert!(html.contains("<a class=\"it bt\" href=\"/ui/router/stc/enable\">Enable STC</a>") && html.contains("href=\"/ui/router/stc/disable\">Disable STC</a>"));
    let params = json!({"stc_lte_collect_timer": "30", "stc_nrsa_collect_timer": 40, "lte_whitelist_max": "5", "nrsa_whitelist_max": "6", "stc_enable": "1"});
    let _dev = device::set(&c.app, reads(vec![("/api/cell/stc/params", Ok(params)), ("/api/cell/stc/status", Err("no".into()))]));
    let html = get_with(&c, STC, peer(145, 3), &cookie).await.text();
    for want in [
        "<span>STC</span><span class=\"v c-green\">Enabled</span>", "<h2>Parameters</h2>",
        "name=\"lte_timer\" value=\"30\" placeholder=\"LTE Collect Timer\"", "name=\"nrsa_timer\" value=\"40\"",
        "name=\"lte_max\" value=\"5\"", "name=\"nrsa_max\" value=\"6\"", "<button class=\"it cv snd\" type=\"submit\">Apply Parameters</button>",
        "<span class=\"it bt off\" aria-disabled=\"true\">Enable STC</span>", "<a class=\"it bt\" href=\"/ui/router/stc/disable\">Disable STC</a>",
        "<a class=\"it bt danger\" href=\"/ui/router/stc/reset\">Reset Whitelist</a>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("Failed to load STC"));
    let csrf = get_with(&c, STC, peer(145, 3), &cookie).await.input("csrf");
    let r = post(&c, &format!("{STC}/params"), peer(145, 4), &cookie, &format!("csrf={csrf}&lte_timer=x&nrsa_timer=40&lte_max=100000&nrsa_max=6")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("Enter a whole number from 0 to 99999.") && r.text().contains("name=\"lte_timer\" value=\"x\""), "{}", r.text());
    // Valid: sent under the firmware's names; a device error shows on the page.
    let r = post(&c, &format!("{STC}/params"), peer(145, 4), &cookie, &format!("csrf={csrf}&lte_timer=30&nrsa_timer=40&lte_max=5&nrsa_max=6")).await;
    r.assert_see_other(&format!("{STC}?saved=params"));
    let sent = calls(&c);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, "/api/cell/stc/params");
    let mut d = reads(vec![]);
    d.replies.push(("/api/cell/stc/params", (503, json!({"ok": false, "error": "ubus call failed (fake)"}))));
    let _dev = device::set(&c.app, d);
    let r = post(&c, &format!("{STC}/params"), peer(145, 4), &cookie, &format!("csrf={csrf}&lte_timer=30&nrsa_timer=40&lte_max=5&nrsa_max=6")).await;
    assert!(r.status >= 500 && r.text().contains("<p class=\"msg err\" role=\"alert\">Failed: ubus call failed (fake)"), "{}", r.text());
    // The controls ask first; each sends once.
    let _dev = device::set(&c.app, device::Device::default());
    for (path, title, label, line, api) in [
        ("enable", "Turn on Smart Tower Connect?", "Enable STC", "STC enabled", "/api/cell/stc/enable"),
        ("disable", "Turn off Smart Tower Connect?", "Disable STC", "STC disabled", "/api/cell/stc/disable"),
        ("reset", "Reset the STC whitelist?", "Reset Whitelist", "STC whitelist reset", "/api/cell/stc/reset"),
    ] {
        post(&c, &format!("{STC}/{path}"), peer(145, 5), &cookie, &format!("csrf={csrf}")).await.assert_see_other(&format!("{STC}/{path}"));
        let q = get_with(&c, &format!("{STC}/{path}"), tailnet(40145), &cookie).await;
        assert!(q.text().contains(&format!("<h2 class=\"q\">{title}</h2>")) && q.text().contains("You are connected remotely"), "{}", q.text());
        assert!(q.text().contains(&format!(">{label}</button>")) && q.text().contains("href=\"/ui/router/stc\">Cancel"));
        let r = post(&c, &format!("{STC}/{path}"), peer(145, 5), &cookie, &format!("csrf={csrf}&once={}&confirm=1", q.input("once"))).await;
        let html = get_with(&c, &to(&r), peer(145, 5), &cookie).await.text();
        assert!(html.contains(&format!("<p class=\"msg ok\" role=\"status\">{line}</p>")), "{html}");
        assert_eq!(calls(&c).last().unwrap(), &(api.to_owned(), json!({})));
        // The state it set, as the app (the reading is unavailable here).
        let state = if path == "enable" { "<span class=\"v c-green\">Enabled</span>" } else { "<span class=\"v\">Disabled</span>" };
        assert!(html.contains(state), "{state} in {html}");
    }
    assert_eq!(calls(&c).len(), 3);
}

/// Signal Detection: a fresh page shows no old sweep and streams nothing;
/// Start (once) watches from its start (the old 100 % is not believed for
/// 6 s), streams and reloads without JavaScript while it runs; Stop shows
/// the results; a sweep the device runs is re-attached to.
#[tokio::test]
async fn signal_detection_starts_watches_and_stops() {
    let c = ctx_at(FakeClock::at_secs(1000));
    let session = login(&c, 146, 1).await;
    let cookie = format!("zte_ui_session={session}");
    let done = json!({"progress": {"progress": "100", "running": "0"}, "results": {"record_list": [
        {"type": "NR", "band": "78", "earfcn": "636666", "pci": "12", "rsrp": "-88", "rsrq": "-10", "sinr": "15"},
    ]}});
    let _fake = fake::set(&c.app, done.clone());
    let r = get_with(&c, SD, peer(146, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>Signal Detection</h1>", "<p class=\"snap\" id=\"live-status\">", "<div id=\"sd-msg\"></div>",
        "<li class=\"it\" id=\"sd-run\"></li><li class=\"go\"><form class=\"sf\" method=\"post\" action=\"/ui/router/signal-detect/start\">",
        "<button class=\"it cv snd\" type=\"submit\">Start Signal Detection</button>",
        "<button class=\"it cv snd danger\" type=\"submit\">Stop Detection</button>", "<div id=\"sd-res\"></div>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("/ui/live/") && !html.contains("http-equiv") && !html.contains("Results"), "{html}");
    let _dev = device::set(&c.app, device::Device::default());
    let (csrf, token) = (r.input("csrf"), r.input("once"));
    let started = to(&post(&c, &format!("{SD}/start"), peer(146, 3), &cookie, &format!("csrf={csrf}&once={token}&confirm=1")).await);
    assert!(started.starts_with("/ui/router/signal-detect?since=") && started.contains("&r="), "{started}");
    assert_eq!(calls(&c), [("/api/cell/signal-detect/start".to_owned(), json!({}))]);
    // Within 6 s the old 100 % reads as this sweep starting. (Watched from
    // now, so a slow test machine cannot make it older than 6 s: the
    // redirect's own point is checked to be the start.)
    let now = crate::boottime::Clock::now(&crate::boottime::BootClock).as_millis();
    let point = u64::from_str_radix(started.split("since=").nth(1).unwrap().split('&').next().unwrap(), 16).unwrap();
    assert!(point <= now && now - point < 60_000, "{point} {now}");
    let started = started.replace(&format!("since={point:x}"), &format!("since={now:x}"));
    let html = get_with(&c, &started, peer(146, 3), &cookie).await.text();
    for want in [
        "<div id=\"sd-msg\"></div><p class=\"msg ok\" role=\"status\">Detection started</p>",
        "<li class=\"it fld run\" id=\"sd-run\"><span class=\"row\"><span>Progress</span><span class=\"muted num\">0%</span></span>",
        "@get('/ui/live/signaldetect/", "<noscript><meta http-equiv=\"refresh\" content=\"3; url=/ui/router/signal-detect?r=",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // Started 7 s ago (the page's boot clock): complete, the results, no
    // stream or reload.
    let since = crate::boottime::Clock::now(&crate::boottime::BootClock).as_millis() - 7_000;
    let html = get_with(&c, &format!("{SD}?since={since:x}"), peer(146, 3), &cookie).await.text();
    for want in [
        "<div id=\"sd-msg\"><p class=\"msg ok\" role=\"status\">Detection complete</p></div>", "<h2>Results</h2>",
        "<b>NR</b><span class=\"muted\">Band 78</span>", "PCI: 12\u{a0}\u{a0}EARFCN: 636666",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("/ui/live/") && !html.contains("http-equiv"), "{html}");
    // Stop: the app's line and the results.
    let token = get_with(&c, SD, peer(146, 4), &cookie).await.text().split("name=\"once\" value=\"").nth(2).unwrap().split('"').next().unwrap().to_owned();
    let stopped = to(&post(&c, &format!("{SD}/stop"), peer(146, 4), &cookie, &format!("csrf={csrf}&once={token}&confirm=1")).await);
    assert!(stopped.starts_with("/ui/router/signal-detect?stop=") && stopped.contains("&r="), "{stopped}");
    let html = get_with(&c, &stopped, peer(146, 4), &cookie).await.text();
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">Detection stopped</p>") && html.contains("<h2>Results</h2>"), "{html}");
    assert!(!html.contains("Detection complete"));
    // A sweep the device runs: re-attached (watched as its own), streamed
    // while it runs; the no-JS reload keeps the page's token.
    let _fake = fake::set(&c.app, json!({"progress": {"progress": 40, "running": 1}, "results": null}));
    let r = get_with(&c, SD, peer(146, 5), &cookie).await;
    let page = r.text();
    let reload = format!("content=\"3; url=/ui/router/signal-detect?t={}&amp;since=", r.input("once"));
    assert!(page.contains("<span class=\"muted num\">40%</span>") && page.contains(&reload) && !page.contains("since=0\""), "{page}");
    let again = page.split("content=\"3; url=").nth(1).unwrap().split('"').next().unwrap().replace("&amp;", "&");
    let r2 = get_with(&c, &again, peer(146, 5), &cookie).await;
    assert_eq!(r2.input("once"), r.input("once"));
    let resp = open_live(&c, &live_url(&page, "signaldetect"), peer(146, 6), &cookie).await;
    assert_eq!(c.app.hub.demand(crate::events::Topic::SignalDetect), 1);
    let mut b = resp.into_body().into_data_stream();
    assert!(frame(&mut b).await.unwrap().contains("40%"));
    drop(b);
    // Its end: "Detection complete" and the results (the app's resumeIfRunning).
    let _fake = fake::set(&c.app, done.clone());
    let html = get_with(&c, &again, peer(146, 5), &cookie).await.text();
    assert!(html.contains("Detection complete") && html.contains("<h2>Results</h2>") && !html.contains("http-equiv"), "{html}");
    // A failure to start, in the app's words.
    let _dev = device::set(&c.app, device::Device { replies: vec![("/api/cell/signal-detect/start", (503, json!({"ok": false, "error": "modem busy"})))], ..Default::default() });
    let token = get_with(&c, SD, peer(146, 7), &cookie).await.input("once");
    let r = post(&c, &format!("{SD}/start"), peer(146, 7), &cookie, &format!("csrf={csrf}&once={token}&confirm=1")).await;
    let html = get_with(&c, &to(&r), peer(146, 7), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed to start: modem busy</p>"), "{html}");
}

/// Every Router post with a single-use token: refused without a session
/// (back to its screen's login), without the CSRF token or cross-site
/// (403), and with a value no form sends (400); nothing sent.
#[tokio::test]
async fn router_token_posts_check_session_csrf_and_values() {
    let (c, cookie, _) = signed_in(147).await;
    let _dev = device::set(&c.app, device::Device::default());
    let csrf = get_with(&c, "/ui/logout", peer(147, 2), &cookie).await.input("csrf");
    for (path, screen) in [
        (APPLY, MN), (SCAN, MN), (REG, MN), ("/ui/router/cell-lock/nr", CELL), ("/ui/router/cell-lock/lte", CELL),
        ("/ui/router/cell-lock/reset", CELL), ("/ui/router/cell-lock/scan", CELL), ("/ui/router/stc/enable", STC),
        ("/ui/router/stc/disable", STC), ("/ui/router/stc/reset", STC), ("/ui/router/signal-detect/start", SD),
        ("/ui/router/signal-detect/stop", SD),
    ] {
        post(&c, path, peer(147, 3), "", &format!("csrf={csrf}&confirm=1")).await.assert_see_other(&format!("/ui/login?next={}&expired=1", enc(screen)));
        post(&c, path, peer(147, 3), &cookie, "confirm=1").await.assert_secure_html(403);
        let body = format!("csrf={csrf}&confirm=1");
        let r = form(path, peer(147, 3), &cookie, &body).header("origin", "http://evil.example").body(Body::from(body.clone())).unwrap();
        send(&c, r).await.assert_secure_html(403);
    }
    post(&c, APPLY, peer(147, 4), &cookie, &format!("csrf={csrf}&cm=1&roam=2&sel=auto_select&cm0=1&roam0=0&sel0=auto_select")).await.assert_secure_html(400);
    post(&c, REG, peer(147, 4), &cookie, &format!("csrf={csrf}&plmn=1&rat=7&confirm=1")).await.assert_secure_html(400);
    let r = post(&c, "/ui/router/cell-lock/nr", peer(147, 4), &cookie, &format!("csrf={csrf}&nr_pci=x&nr_earfcn=1&confirm=1")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("<h1>Cell Lock</h1>") && r.text().contains("Enter a whole number from 0 to 1007."));
    assert!(calls(&c).is_empty());
}

/// A full token table (another session's forms) never costs a page its
/// own: Mobile Network's two forms share one token, so the first form
/// (the scan) still posts, and so does a page loaded right after it.
#[tokio::test]
async fn a_full_token_table_keeps_a_new_pages_token() {
    let (c, cookie, _) = signed_in(148).await;
    let other = crate::auth::SessionId::for_test(9_999);
    for i in 0..64 {
        c.once.issue(format!("{i:016x}"), other, c.app.auth.now());
    }
    let _dev = device::set(&c.app, reads(vec![wwan(), online()]));
    let _sig = fake::set(&c.app, json!({"net_select_mode": "auto_select"}));
    let page = get_with(&c, MN, peer(148, 2), &cookie).await;
    let (csrf, token) = (page.input("csrf"), page.input("once"));
    // Another page of this session, then the scan from the first.
    let _cell = get_with(&c, CELL, peer(148, 2), &cookie).await.input("once");
    let r = post(&c, SCAN, peer(148, 3), &cookie, &format!("csrf={csrf}&once={token}&confirm=1")).await;
    assert_eq!(to(&r), format!("/ui/op/{token}"));
    assert_eq!(calls(&c), [("/api/modem/scan".to_owned(), json!({}))]);
}

/// The Router's operations (a scan, a registration, Apply) stream only
/// their topic while pending, then end with the outcome (the status line
/// emptied), and nothing is sampled after.
#[tokio::test]
async fn network_operations_stream_their_topic_until_done() {
    use crate::events::hub::ReadOut;
    use crate::events::Topic;
    use crate::web::ops::{Op, APPLY, NETSCAN, REGISTER};
    let clock = FakeClock::at_secs(1000);
    let c = ctx_at(clock.clone());
    let session = login(&c, 149, 1).await;
    let cookie = format!("zte_ui_session={session}");
    let me = c.app.auth.authenticate(crate::auth::Kind::Browser, &session).unwrap().id;
    let modem = |cm: i64| json!({"status": {"operate_mode": "ONLINE"}, "data": {"connect_mode": cm, "roam_enable": 0}});
    for (n, spec, topic, arg, pending, done, title) in [
        (1u8, &NETSCAN, Topic::Netscan, "", json!({"scan": {"m_netselect_status": "manual_selecting"}}), json!({"scan": {"m_netselect_status": "manual_selected"}}), "Scan complete"),
        (2, &REGISTER, Topic::Netscan, "", json!({"register": {"m_netselect_result": ""}}), json!({"register": {"m_netselect_result": "manual_success"}}), "Registered to network"),
        (3, &APPLY, Topic::Modem, "0:-", modem(1), modem(0), "Settings applied"),
    ] {
        let id = format!("{:016x}", 0x100 + u64::from(n));
        let now = c.app.auth.now();
        c.ops.insert(id.clone(), me, Op { spec, started: now, arg }, now);
        c.app.hub.offer(topic, ReadOut { value: Some(pending), error: None }, std::time::Instant::now());
        let resp = open_live(&c, &format!("/ui/live/op/{id}/{KEY}"), peer(149, n), &cookie).await;
        let mut b = resp.into_body().into_data_stream();
        assert!(frame(&mut b).await.unwrap().contains("s so far"), "{title}");
        assert_eq!(c.app.hub.demand(topic), 1, "{title}");
        assert!(Topic::ALL.iter().filter(|t| **t != topic).all(|t| c.app.hub.demand(*t) == 0), "{title}");
        clock.advance(Duration::from_secs(3));
        c.app.hub.offer(topic, ReadOut { value: Some(done), error: None }, std::time::Instant::now());
        let mut last = String::new();
        while let Some(f) = frame(&mut b).await {
            last = f;
        }
        assert!(last.contains(title) && last.contains("<p class=\"live\" id=\"live-status\"></p>"), "{title}: {last}");
        drop(b);
        assert_eq!(c.app.hub.demand(topic), 0, "{title}");
    }
}
