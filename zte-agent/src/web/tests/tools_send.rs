//! The Tools that send something through the whole router: Speed Test
//! (its server list, the question, the single-use start, Stop, the live
//! progress), Enable ADB, Band Lock and AT Terminal. Device calls go to
//! `pages::device` (or, where said, to the agent's own handlers, which
//! reach nothing on a build machine). Every identifier is a made-up
//! fixture.

use serde_json::json;

use super::*;
use crate::web::pages::{device, fake};

const ST: &str = "/ui/tools/speedtest";
const ADB: &str = "/ui/tools/adb";
const BL: &str = "/ui/tools/band-lock";
const AT: &str = "/ui/tools/at";

fn servers(c: &Ctx) {
    c.app.speedtest.set_servers(vec![
        crate::speedtest::SpeedTest::test_server(7, "http://127.0.0.1:9/"),
        crate::speedtest::TestServer {
            id: 8,
            name: "<b>Far</b>".into(),
            ..crate::speedtest::SpeedTest::test_server(8, "http://127.0.0.1:9/")
        },
    ]);
}

/// The calls the device got, as (path, body).
fn calls(c: &Ctx) -> Vec<(String, Value)> {
    device::calls(&c.app)
}

/// The page: the servers (the first chosen), Start (a GET of the
/// question), Stop (an action), the live stream; without servers the
/// app's error line.
#[tokio::test]
async fn speed_test_page_lists_servers_and_streams() {
    let (c, cookie, _) = signed_in(110).await;
    let r = get_with(&c, ST, peer(110, 2), &cookie).await;
    r.assert_secure_html(200);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Failed to load servers: no server list in tests</p>"), "{}", r.text());
    servers(&c);
    let r = get_with(&c, ST, peer(110, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>Speed Test</h1>", "<a class=\"nl back\" href=\"/ui/tools\">Tools</a>",
        "<form id=\"st-go\" method=\"get\" action=\"/ui/tools/speedtest/start\"></form>",
        "<select id=\"srv\" name=\"server\" form=\"st-go\"><option value=\"\">Select a server</option>",
        "<option value=\"7\" selected>Example Net - Testville, Nowhere</option>",
        "<option value=\"8\">Example Net - &lt;b&gt;Far&lt;/b&gt;, Nowhere</option>",
        "<li class=\"it\" id=\"st-run\"></li><li class=\"go\"><button class=\"it cv snd\" type=\"submit\" form=\"st-go\">Start Speed Test</button></li>",
        "<form class=\"sf\" method=\"post\" action=\"/ui/tools/speedtest/stop\"><input type=\"hidden\" name=\"csrf\"",
        "<button class=\"it cv snd danger\" type=\"submit\">Stop Test</button>", "<div id=\"st-msg\"></div>", "<div id=\"st-res\"></div>",
        "@get('/ui/live/speedtest/",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // Idle: no reload without JavaScript.
    assert!(!html.contains("http-equiv=\"refresh\""));
    // Coming back from the question keeps its server.
    assert!(get_with(&c, &format!("{ST}?server=8"), peer(110, 3), &cookie).await.text().contains("<option value=\"8\" selected>"));
    // A test that runs (whoever started it): its progress, Stop, a bounded reload.
    let _fake = fake::set(&c.app, json!({"phase": "upload", "progress": 80, "live_speed_mbps": 12.34, "server": "Example Net (Testville)"}));
    let html = get_with(&c, ST, peer(110, 4), &cookie).await.text();
    for want in [
        "<li class=\"it fld run\" id=\"st-run\"><span class=\"row\"><span>Uploading...</span><span class=\"muted num\">80%</span>",
        "<progress max=\"100\" value=\"80\" aria-label=\"Uploading...\"></progress>", "<span class=\"c-orange\"><svg",
        "<b class=\"num\">12.3</b>", "<noscript><meta http-equiv=\"refresh\" content=\"3; url=/ui/tools/speedtest?since=",
        "<input type=\"hidden\" name=\"server\" value=\"7\"><button class=\"it cv snd danger\"",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // The reload keeps the server and the result line, never anything else.
    let html = get_with(&c, &format!("{ST}?server=8&r=00ff&x=1&since=zz"), peer(110, 4), &cookie).await.text();
    assert!(html.contains("content=\"3; url=/ui/tools/speedtest?server=8&amp;r=00ff&amp;since="), "{html}");
    // Stop: the agent has no test running here; back at the server.
    let csrf = get_with(&c, ST, peer(110, 5), &cookie).await.input("csrf");
    let r = post(&c, "/ui/tools/speedtest/stop", peer(110, 5), &cookie, &format!("csrf={csrf}&server=8")).await;
    let to = r.header("location").unwrap().to_owned();
    assert!(to.starts_with("/ui/tools/speedtest?server=8&stop=1&r="), "{to}");
    let html = get_with(&c, &to, peer(110, 5), &cookie).await.text();
    assert!(html.contains("<p class=\"msg info\" role=\"status\">No speed test is running.</p>") && html.contains("<option value=\"8\" selected>"));
    let r = post(&c, "/ui/tools/speedtest/stop", peer(110, 5), &cookie, &format!("csrf={csrf}&server=0")).await;
    assert!(r.header("location").unwrap().starts_with("/ui/tools/speedtest?stop=1&r="));
}

/// A local speedtest server that answers each request after `delay`.
fn slow_server(delay: Duration) -> String {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                std::thread::sleep(delay);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx");
            });
        }
    });
    base
}

/// One message line, as the app's: "Speed test started", then how the test
/// ends; after Stop, "Speed test stopped" stands (the cancel that follows
/// is not shown); a fresh page shows no old outcome. The agent runs the
/// test against a local server.
#[tokio::test]
async fn speed_test_says_what_this_page_saw() {
    let (c, cookie, _) = signed_in(119).await;
    c.app.speedtest.set_servers(vec![crate::speedtest::SpeedTest::test_server(7, &slow_server(Duration::from_millis(150)))]);
    let q = get_with(&c, "/ui/tools/speedtest/start?server=7", peer(119, 2), &cookie).await;
    let csrf = q.input("csrf");
    let r = post(&c, "/ui/tools/speedtest/start", peer(119, 2), &cookie, &format!("csrf={csrf}&server=7&once={}&confirm=1", q.input("once"))).await;
    let started = r.header("location").unwrap().to_owned();
    assert!(started.starts_with("/ui/tools/speedtest?server=7&since=") && started.contains("&r="), "{started}");
    let html = get_with(&c, &started, peer(119, 3), &cookie).await.text();
    assert!(html.contains("<div id=\"st-msg\"></div><p class=\"msg ok\" role=\"status\">Speed test started</p>"), "{html}");
    assert!(html.contains("<li class=\"it fld run\" id=\"st-run\">") && html.contains("content=\"3; url=/ui/tools/speedtest?server=7&amp;r="), "{html}");
    // Stop: the app's line; the cancel is not shown on that page.
    let r = post(&c, "/ui/tools/speedtest/stop", peer(119, 4), &cookie, &format!("csrf={csrf}&server=7")).await;
    let stopped = r.header("location").unwrap().to_owned();
    for _ in 0..250 {
        if !c.app.speedtest.is_running() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!c.app.speedtest.is_running());
    let html = get_with(&c, &stopped, peer(119, 4), &cookie).await.text();
    assert!(html.contains("<div id=\"st-msg\"></div><p class=\"msg ok\" role=\"status\">Speed test stopped</p>"), "{html}");
    assert!(!html.contains("cancelled") && !html.contains("http-equiv=\"refresh\""), "{html}");
    // The started page (watching from before the start) says how it ended.
    let html = get_with(&c, &started, peer(119, 5), &cookie).await.text();
    assert!(html.contains("<div id=\"st-msg\"><p class=\"msg ok\" role=\"status\">Speed test cancelled</p></div><p class=\"msg ok\" role=\"status\">Speed test started</p>"), "{html}");
    // A fresh page: nothing.
    assert!(get_with(&c, ST, peer(119, 6), &cookie).await.text().contains("<div id=\"st-msg\"></div>"));
}

/// Start: asked first (data use), with a single-use token; a double click
/// starts once; the question says why there is nothing to do.
#[tokio::test]
async fn speed_test_start_asks_and_starts_once() {
    let (c, cookie, _) = signed_in(111).await;
    servers(&c);
    let _dev = device::set(&c.app, device::Device { wait: Some(Duration::from_millis(300)), ..Default::default() });
    let csrf = get_with(&c, ST, peer(111, 2), &cookie).await.input("csrf");
    // A post without `confirm=1` goes to the question.
    post(&c, "/ui/tools/speedtest/start", peer(111, 2), &cookie, &format!("csrf={csrf}&server=7"))
        .await
        .assert_see_other("/ui/tools/speedtest/start?server=7");
    let q = get_with(&c, "/ui/tools/speedtest/start?server=7", peer(111, 3), &cookie).await;
    q.assert_secure_html(200);
    let html = q.text();
    for want in [
        "<div class=\"lead\"><a class=\"nl back\" href=\"/ui/tools/speedtest\">Speed Test</a></div><h1>Confirm</h1>",
        "<h2 class=\"q\">Start the speed test?</h2>", "<li>Server: Example Net - Testville, Nowhere</li>",
        "It downloads for 15 seconds and uploads 10 MB", "<li>Your plan may count this data.</li>",
        "<input type=\"hidden\" name=\"server\" value=\"7\"><input type=\"hidden\" name=\"once\" value=\"",
        "\" autocomplete=\"off\"><input type=\"hidden\" name=\"confirm\" value=\"1\">",
        "<button class=\"btn\" type=\"submit\">Start Speed Test</button>", "href=\"/ui/tools/speedtest?server=7\">Cancel",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let once = q.input("once");
    assert!(calls(&c).is_empty(), "a question sends nothing");
    let body = format!("csrf={csrf}&server=7&once={once}&confirm=1");
    let (a, b) = tokio::join!(
        post(&c, "/ui/tools/speedtest/start", peer(111, 4), &cookie, &body),
        post(&c, "/ui/tools/speedtest/start", peer(111, 4), &cookie, &body)
    );
    // Back on the page with its server chosen, watching from the start.
    for r in [a, b] {
        let to = r.header("location").unwrap().to_owned();
        assert!(to.starts_with("/ui/tools/speedtest?server=7&since=") && to.contains("&r="), "{to}");
        let html = get_with(&c, &to, peer(111, 4), &cookie).await.text();
        assert!(html.contains("<p class=\"msg ok\" role=\"status\">Speed test started</p>"), "{html}");
    }
    assert_eq!(calls(&c), [("/api/speedtest/start".to_owned(), json!({"server_id": 7}))]);
    // The token used, another post with it: nothing sent, said so.
    let r = post(&c, "/ui/tools/speedtest/start", peer(111, 5), &cookie, &format!("csrf={csrf}&server=8&once={once}&confirm=1")).await;
    let to = r.header("location").unwrap().to_owned();
    assert!(to.starts_with("/ui/tools/speedtest?server=8&r="), "{to}");
    assert!(get_with(&c, &to, peer(111, 5), &cookie).await.text().contains("This page expired, so nothing was sent. Try again."));
    assert_eq!(calls(&c).len(), 1);
    // Nothing to do: no form, only Cancel.
    for (uri, title) in [
        ("/ui/tools/speedtest/start", "Select a server first"),
        ("/ui/tools/speedtest/start?server=", "Select a server first"),
        ("/ui/tools/speedtest/start?server=9", "This server is no longer in the list."),
    ] {
        let html = get_with(&c, uri, peer(111, 6), &cookie).await.text();
        assert!(html.contains(&format!("<h2 class=\"q\">{title}</h2>")) && !html.contains("<form"), "{uri}: {html}");
    }
    for bad in ["?server=x", "?server=-1", "?server=1%2C2"] {
        get_with(&c, &format!("/ui/tools/speedtest/start{bad}"), peer(111, 7), &cookie).await.assert_secure_html(400);
    }
    // Refused without a session, the CSRF token or from another origin.
    post(&c, "/ui/tools/speedtest/start", peer(111, 8), "", &body).await.assert_see_other("/ui/login?next=%2Fui%2Ftools%2Fspeedtest&expired=1");
    post(&c, "/ui/tools/speedtest/start", peer(111, 8), &cookie, &format!("server=7&once={once}&confirm=1")).await.assert_secure_html(403);
    let r = form("/ui/tools/speedtest/start", peer(111, 8), &cookie, &body).header("origin", "http://evil.example").body(Body::from(body.clone())).unwrap();
    send(&c, r).await.assert_secure_html(403);
    assert_eq!(calls(&c).len(), 1);
}

/// The agent's own start (no fake device): the test runs, fails against an
/// unreachable server, and the page shows the agent's error.
#[tokio::test]
async fn speed_test_runs_on_the_agent() {
    let (c, cookie, _) = signed_in(112).await;
    servers(&c);
    let q = get_with(&c, "/ui/tools/speedtest/start?server=7", peer(112, 2), &cookie).await;
    let body = format!("csrf={}&server=7&once={}&confirm=1", q.input("csrf"), q.input("once"));
    let to = post(&c, "/ui/tools/speedtest/start", peer(112, 3), &cookie, &body).await.header("location").unwrap().to_owned();
    for _ in 0..500 {
        if !c.app.speedtest.is_running() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let html = get_with(&c, &to, peer(112, 4), &cookie).await.text();
    assert!(html.contains("<div id=\"st-msg\"><p class=\"msg err\" role=\"alert\">all ping attempts failed</p></div>"), "{html}");
    // A fresh page shows no old outcome (the app's neither).
    assert!(get_with(&c, ST, peer(112, 4), &cookie).await.text().contains("<div id=\"st-msg\"></div>"));
    // A second start while one runs is refused without wiping it (agent).
    let _fake = fake::set(&c.app, json!({"phase": "latency", "server": "x"}));
    let html = get_with(&c, "/ui/tools/speedtest/start?server=7", peer(112, 5), &cookie).await.text();
    assert!(html.contains("<h2 class=\"q\">A speed test is already running.</h2>") && !html.contains("<form"), "{html}");
}

/// Enable ADB: the app's page; asked first (owner decision); one call with
/// the app's body; the app's result lines.
#[tokio::test]
async fn enable_adb_asks_and_sends_once() {
    let (c, cookie, _) = signed_in(113).await;
    let html = get_with(&c, ADB, peer(113, 2), &cookie).await.text();
    for want in [
        "<h1>Enable ADB</h1>", "<strong>Enable ADB Debug</strong>", "Sets the USB mode to debug, enabling ADB access over USB-C.",
        "<a class=\"btn btn-wide\" href=\"/ui/tools/adb/enable\"><svg",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("<form"));
    let _dev = device::set(&c.app, device::Device::default());
    let q = get_with(&c, "/ui/tools/adb/enable", peer(113, 3), &cookie).await;
    q.assert_secure_html(200);
    assert!(q.text().contains("<h2 class=\"q\">Enable ADB?</h2>") && q.text().contains("gets a root shell on the router"), "{}", q.text());
    assert!(q.text().contains("<button class=\"btn btn-danger\" type=\"submit\">Enable ADB</button>"));
    let (csrf, once) = (q.input("csrf"), q.input("once"));
    post(&c, "/ui/tools/adb/enable", peer(113, 4), &cookie, &format!("csrf={csrf}")).await.assert_see_other("/ui/tools/adb/enable");
    let body = format!("csrf={csrf}&once={once}&confirm=1");
    let r = post(&c, "/ui/tools/adb/enable", peer(113, 4), &cookie, &body).await;
    let to = r.header("location").unwrap().to_owned();
    assert!(to.starts_with("/ui/tools/adb?r="), "{to}");
    let html = get_with(&c, &to, peer(113, 4), &cookie).await.text();
    // Between the text and the button, as the app's.
    assert!(html.contains("ADB access over USB-C.</p></div><p class=\"msg ok\" role=\"status\">ADB debug mode enabled. Connect USB-C cable to access the device.</p><p class=\"ctr\">"), "{html}");
    // The same post again: what the first did, nothing sent.
    let again = post(&c, "/ui/tools/adb/enable", peer(113, 4), &cookie, &body).await;
    assert!(again.header("location").unwrap().starts_with("/ui/tools/adb?r="));
    assert_eq!(calls(&c), [("/api/usb/mode".to_owned(), json!({"mode": "debug"}))]);
    // A failure: the app's "Failed: …", red.
    let _dev = device::set(&c.app, device::Device { replies: vec![("/api/usb/mode", (503, json!({"ok": false, "error": "ubus <b>down</b>"})))], ..Default::default() });
    let q = get_with(&c, "/ui/tools/adb/enable", peer(113, 5), &cookie).await;
    let r = post(&c, "/ui/tools/adb/enable", peer(113, 5), &cookie, &format!("csrf={csrf}&once={}&confirm=1", q.input("once"))).await;
    let html = get_with(&c, r.header("location").unwrap(), peer(113, 5), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed: ubus &lt;b&gt;down&lt;/b&gt;</p>"), "{html}");
}

/// Band Lock: the app's grids with the bands' facts; each apply and the
/// unlock asked first (with the remote line over Tailscale), sent once
/// with the app's bodies; the app's result lines; the choice kept.
#[tokio::test]
async fn band_lock_grids_questions_and_bodies() {
    let (c, cookie, _) = signed_in(114).await;
    let r = get_with(&c, BL, peer(114, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    let nr = &html[html.find("<h2>5G NR Bands</h2>").unwrap()..html.find("<h2>LTE Bands</h2>").unwrap()];
    assert_eq!(nr.matches("type=\"checkbox\" name=\"nr\"").count(), 19);
    assert_eq!(html.matches("type=\"checkbox\" name=\"lte\"").count(), 28);
    for want in [
        "<a class=\"it bt danger\" href=\"/ui/tools/band-lock/reset\"><svg",
        "<form class=\"bl\" method=\"get\" action=\"/ui/tools/band-lock/nr\">",
        "<label title=\"Band 78 \u{2014} 3500 MHz \u{b7} TDD \u{b7} Max BW 100 MHz \u{b7} DL 3300\u{2013}3800 MHz \u{b7} UL 3300\u{2013}3800 MHz\"><input class=\"sr\" type=\"checkbox\" name=\"nr\" value=\"78\"><span>B78<small>3500 MHz</small></span></label>",
        "<span>B66<small>AWS-3</small></span>", "</svg> Apply NR Lock</button>", "</svg> Apply LTE Lock</button>",
        "<summary>Band details</summary>", "<li><b>Band 29 \u{2014} 700 MHz</b><small>SDL \u{b7} Max BW 10 MHz \u{b7} DL 717\u{2013}728 MHz</small></li>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("/ui/a/datastar-") && !html.contains("checked"));
    let _dev = device::set(&c.app, device::Device::default());
    // NR: the question (no form without a band), then NSA and SA.
    let html = get_with(&c, "/ui/tools/band-lock/nr", peer(114, 3), &cookie).await.text();
    assert!(html.contains("<h2 class=\"q\">Select at least one NR band</h2>") && !html.contains("<form"), "{html}");
    for bad in ["?nr=4", "?nr=x", "?nr=78&nr=999"] {
        get_with(&c, &format!("/ui/tools/band-lock/nr{bad}"), peer(114, 3), &cookie).await.assert_secure_html(400);
    }
    let q = get_with(&c, "/ui/tools/band-lock/nr?nr=78&nr=1&nr=78", peer(114, 4), &cookie).await;
    let html = q.text();
    for want in [
        "<h2 class=\"q\">Apply NR Lock?</h2>", "for NSA and SA: 1, 78.</li>",
        "<input type=\"hidden\" name=\"nr\" value=\"1\"><input type=\"hidden\" name=\"nr\" value=\"78\">",
        "<button class=\"btn btn-danger\" type=\"submit\">Apply NR Lock</button>", "href=\"/ui/tools/band-lock?nr=1&amp;nr=78\">Cancel",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("connected remotely"));
    let (csrf, once) = (q.input("csrf"), q.input("once"));
    let r = post(&c, "/ui/tools/band-lock/nr", peer(114, 5), &cookie, &format!("csrf={csrf}&nr=1&nr=78&once={once}&confirm=1")).await;
    let to = r.header("location").unwrap().to_owned();
    assert!(to.starts_with("/ui/tools/band-lock?nr=1&nr=78&r="), "{to}");
    let html = get_with(&c, &to, peer(114, 5), &cookie).await.text();
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">NR bands locked: 1,78</p>"), "{html}");
    assert!(html.contains("name=\"nr\" value=\"78\" checked>") && html.contains("name=\"nr\" value=\"1\" checked>"));
    assert_eq!(
        calls(&c),
        [
            ("/api/cell/band/nr".to_owned(), json!({"nr5g_type": "nsa", "nr5g_band": "1,78"})),
            ("/api/cell/band/nr".to_owned(), json!({"nr5g_type": "sa", "nr5g_band": "1,78"})),
        ]
    );
    // LTE and Unlock over Tailscale: the remote line.
    let tailnet = Peer(SocketAddr::from(([127, 0, 0, 1], 40114)));
    let q = get_with(&c, "/ui/tools/band-lock/lte?lte=3&lte=20", tailnet, &cookie).await;
    assert!(q.text().contains("these LTE bands: 3, 20.") && q.text().contains("You are connected remotely"), "{}", q.text());
    post(&c, "/ui/tools/band-lock/lte", tailnet, &cookie, &format!("csrf={csrf}&lte=3&lte=20&once={}&confirm=1", q.input("once"))).await;
    let q = get_with(&c, "/ui/tools/band-lock/reset", tailnet, &cookie).await;
    assert!(q.text().contains("<h2 class=\"q\">Unlock all bands?</h2>") && q.text().contains("You are connected remotely"));
    let r = post(&c, "/ui/tools/band-lock/reset", tailnet, &cookie, &format!("csrf={csrf}&once={}&confirm=1", q.input("once"))).await;
    let to = r.header("location").unwrap().to_owned();
    assert!(to.starts_with("/ui/tools/band-lock?r="), "{to}");
    assert!(get_with(&c, &to, tailnet, &cookie).await.text().contains(">All bands unlocked</p>"));
    let calls = calls(&c);
    assert_eq!(calls[2], ("/api/cell/band/lte".to_owned(), json!({"is_lte_band": "1", "lte_band_mask": "3,20", "is_gw_band": "0", "gw_band_mask": ""})));
    assert_eq!(calls[3], ("/api/cell/band/reset".to_owned(), json!({})));
    assert_eq!(calls.len(), 4);
    // NSA failing: SA is not sent; the agent's error.
    let _dev = device::set(&c.app, device::Device { replies: vec![("/api/cell/band/nr", (503, json!({"ok": false, "error": "nope"})))], ..Default::default() });
    let q = get_with(&c, "/ui/tools/band-lock/nr?nr=41", peer(114, 6), &cookie).await;
    let r = post(&c, "/ui/tools/band-lock/nr", peer(114, 6), &cookie, &format!("csrf={csrf}&nr=41&once={}&confirm=1", q.input("once"))).await;
    assert!(get_with(&c, r.header("location").unwrap(), peer(114, 6), &cookie).await.text().contains(">Failed: nope</p>"));
    assert_eq!(device::calls(&c.app).len(), 1);
    // NSA locked, SA failing: said so (the app's "Failed: …" would read as nothing changed).
    let _dev = device::set(&c.app, device::Device { replies: vec![("/api/cell/band/nr#2", (503, json!({"ok": false, "error": "sa down"})))], ..Default::default() });
    let q = get_with(&c, "/ui/tools/band-lock/nr?nr=41&nr=1", peer(114, 6), &cookie).await;
    let r = post(&c, "/ui/tools/band-lock/nr", peer(114, 6), &cookie, &format!("csrf={csrf}&nr=41&nr=1&once={}&confirm=1", q.input("once"))).await;
    let html = get_with(&c, r.header("location").unwrap(), peer(114, 6), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">NR bands locked for NSA only: 1,41. SA lock failed: sa down</p>"), "{html}");
    assert_eq!(device::calls(&c.app).len(), 2);
    // A band the app does not list is refused, also in a post.
    post(&c, "/ui/tools/band-lock/nr", peer(114, 7), &cookie, &format!("csrf={csrf}&nr=4&once={once}&confirm=1")).await.assert_secure_html(400);
}

/// AT Terminal: the port line, the quick commands (no IMEI), a command sent
/// once with its masked answer in the session's history, the app's local
/// checks and its dangerous-command question; logout drops the history.
#[tokio::test]
async fn at_terminal_sends_once_masks_and_forgets_on_logout() {
    let (c, cookie, me) = signed_in(115).await;
    let r = get_with(&c, AT, peer(115, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>AT Terminal</h1>", "<span class=\"dot bad\"></span><span class=\"muted\" aria-label=\"No port detected\">No port detected</span>",
        "<a href=\"/ui/tools/at?cmd=AT%2BCSQ#cmd\"><code>AT+CSQ</code><small>Signal quality</small></a>",
        "<a href=\"/ui/tools/at?cmd=AT%2BCLCC#cmd\"><code>AT+CLCC</code><small>Active calls</small></a>",
        "<strong>No Commands Sent</strong>", "Enter an AT command below or tap a quick command above.",
        "<form class=\"atf\" method=\"post\" action=\"/ui/tools/at/send\">", "name=\"once\" value=\"",
        "<input id=\"to\" type=\"number\" name=\"timeout\" min=\"1\" max=\"30\" value=\"3\"",
        "<input id=\"cmd\" type=\"text\" name=\"cmd\" value=\"\" placeholder=\"AT command...\"",
        "<button class=\"send\" type=\"submit\" aria-label=\"Send command\">",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert_eq!(html.matches("<code>").count(), 7);
    assert!(!html.contains("AT+CGSN") && !html.contains("IMEI") && !html.contains("Clear"));
    // Chips fill the field; other text in the URL does not.
    assert!(get_with(&c, &format!("{AT}?cmd=AT%2BCSQ"), peer(115, 2), &cookie).await.text().contains("name=\"cmd\" value=\"AT+CSQ\""));
    for other in ["AT%2BCGSN", "%3Cscript%3E", "ATD%2B10000000042"] {
        assert!(get_with(&c, &format!("{AT}?cmd={other}"), peer(115, 2), &cookie).await.text().contains("name=\"cmd\" value=\"\""), "{other}");
    }
    let reply = json!({"ok": true, "data": {"command": "AT+CGSN", "response": "\r\n000000000000042\r\n\r\nOK\r\n", "port": "/dev/smd11", "elapsed_ms": 1312}});
    let _dev = device::set(&c.app, device::Device { replies: vec![("/api/at/send", (200, reply))], wait: Some(Duration::from_millis(200)), ..Default::default() });
    let (csrf, once) = (r.input("csrf"), r.input("once"));
    let body = format!("csrf={csrf}&once={once}&timeout=99&cmd=+AT%2BCGSN+");
    let (a, b) = tokio::join!(post(&c, "/ui/tools/at/send", peer(115, 3), &cookie, &body), post(&c, "/ui/tools/at/send", peer(115, 3), &cookie, &body));
    a.assert_see_other(AT);
    b.assert_see_other(AT);
    assert_eq!(device::calls(&c.app), [("/api/at/send".to_owned(), json!({"command": "AT+CGSN", "timeout": 30}))]);
    let html = get_with(&c, AT, peer(115, 4), &cookie).await.text();
    assert!(html.contains(&format!("<li><b class=\"mono\">&gt; AT+CGSN</b><pre>\r\n{}0042\r\n\r\nOK\r\n</pre><small>/dev/smd11 \u{b7} 1312ms \u{b7} ", "\u{2022}".repeat(11))), "{html}");
    assert!(!html.contains("000000000000042") && html.contains(">Clear</button>"));
    // The timeout sent last is kept.
    assert!(html.contains("name=\"timeout\" min=\"1\" max=\"30\" value=\"30\""));
    // Not an AT command: the app's local error, nothing sent, the command
    // still in the field.
    let once = get_with(&c, AT, peer(115, 5), &cookie).await.input("once");
    let r = post(&c, "/ui/tools/at/send", peer(115, 5), &cookie, &format!("csrf={csrf}&once={once}&cmd=reboot")).await;
    r.assert_secure_html(200);
    assert!(r.text().contains("name=\"cmd\" value=\"reboot\""));
    let html = get_with(&c, AT, peer(115, 5), &cookie).await.text();
    assert!(html.contains("<li class=\"er\"><b class=\"mono\">&gt; reboot</b><pre>Error: Command must start with AT</pre><small>0ms"), "{html}");
    // Dangerous: the app's question; Cancel puts it back into the field.
    let r = post(&c, "/ui/tools/at/send", peer(115, 6), &cookie, &format!("csrf={csrf}&once={once}&cmd=at%2Bcfun%3D0&timeout=5")).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h2 class=\"q\">Dangerous Command</h2>",
        "<li>The command &quot;at+cfun=0&quot; may disrupt connectivity or require a reboot. Are you sure?</li>",
        "<input type=\"hidden\" name=\"cmd\" value=\"at+cfun=0\"><input type=\"hidden\" name=\"timeout\" value=\"5\">",
        "<button class=\"btn btn-danger\" type=\"submit\" name=\"confirm\" value=\"1\">Send Anyway</button>",
        "<button class=\"btn btn-quiet\" type=\"submit\" name=\"edit\" value=\"1\">Cancel</button>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let back = post(&c, "/ui/tools/at/send", peer(115, 6), &cookie, &format!("csrf={csrf}&once={once}&cmd=at%2Bcfun%3D0&timeout=5&edit=1")).await;
    back.assert_secure_html(200);
    assert!(back.text().contains("name=\"cmd\" value=\"at+cfun=0\"") && back.text().contains("value=\"5\""));
    assert_eq!(device::calls(&c.app).len(), 1);
    post(&c, "/ui/tools/at/send", peer(115, 6), &cookie, &format!("csrf={csrf}&once={once}&cmd=at%2Bcfun%3D0&timeout=5&confirm=1"))
        .await
        .assert_see_other(AT);
    assert_eq!(device::calls(&c.app)[1].1, json!({"command": "at+cfun=0", "timeout": 5}));
    // A used token with another command: nothing sent, the form kept.
    let r = post(&c, "/ui/tools/at/send", peer(115, 7), &cookie, &format!("csrf={csrf}&once={once}&cmd=ATI")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("This form expired, so the command was not sent. Send it again.") && r.text().contains("value=\"ATI\""));
    // Control characters and overlong commands are refused.
    for bad in ["AT%0D%0AATZ", &format!("AT{}", "A".repeat(300))] {
        post(&c, "/ui/tools/at/send", peer(115, 7), &cookie, &format!("csrf={csrf}&once={once}&cmd={bad}")).await.assert_secure_html(400);
    }
    assert_eq!(device::calls(&c.app).len(), 2);
    // Another session sees nothing of it; Clear empties it; logout drops it.
    let other = login(&c, 115, 9).await;
    assert!(get_with(&c, AT, peer(115, 9), &format!("zte_ui_session={other}")).await.text().contains("No Commands Sent"));
    assert_eq!(c.at.list(me, c.app.auth.now()).len(), 3);
    let logout = get_with(&c, "/ui/logout", peer(115, 8), &cookie).await.input("csrf");
    post(&c, "/ui/logout", peer(115, 8), &cookie, &format!("csrf={logout}")).await.assert_see_other("/ui/login");
    assert!(c.at.list(me, c.app.auth.now()).is_empty());
}

/// The question is asked of the command as the agent sends it (the
/// characters it strips gone, spaces ignored), which is also what is sent
/// and kept; a backslash is refused; an expired form says so before the
/// question.
#[tokio::test]
async fn at_terminal_checks_what_is_sent() {
    let (c, cookie, me) = signed_in(118).await;
    let page = get_with(&c, AT, peer(118, 2), &cookie).await;
    let (csrf, once) = (page.input("csrf"), page.input("once"));
    let _dev = device::set(&c.app, device::Device::default());
    for typed in ["AT%2BCFUN%3D%244", "AT%2BCOPS%60%3D2", "AT%2BCOPS+%3D2", "AT%2BCGATT%3D0"] {
        let r = post(&c, "/ui/tools/at/send", peer(118, 3), &cookie, &format!("csrf={csrf}&once={once}&cmd={typed}")).await;
        r.assert_secure_html(200);
        assert!(r.text().contains("<h2 class=\"q\">Dangerous Command</h2>"), "{typed}");
    }
    let r = post(&c, "/ui/tools/at/send", peer(118, 3), &cookie, &format!("csrf={csrf}&once={once}&cmd=AT%2BCFUN%3D%244")).await;
    let html = r.text();
    assert!(html.contains("<li>The command &quot;AT+CFUN=4&quot; may disrupt connectivity"), "{html}");
    assert!(html.contains("<input type=\"hidden\" name=\"cmd\" value=\"AT+CFUN=$4\">"), "{html}");
    assert!(device::calls(&c.app).is_empty());
    post(&c, "/ui/tools/at/send", peer(118, 3), &cookie, &format!("csrf={csrf}&once={once}&cmd=AT%2BCFUN%3D%244&confirm=1"))
        .await
        .assert_see_other(AT);
    assert_eq!(device::calls(&c.app), [("/api/at/send".to_owned(), json!({"command": "AT+CFUN=4", "timeout": 3}))]);
    assert_eq!(c.at.list(me, c.app.auth.now())[0].cmd, "AT+CFUN=4");
    // A backslash (`echo -e` would turn "\x30" into "0"): refused, kept in the field.
    let once = get_with(&c, AT, peer(118, 4), &cookie).await.input("once");
    let r = post(&c, "/ui/tools/at/send", peer(118, 4), &cookie, &format!("csrf={csrf}&once={once}&cmd=AT%2BCFUN%3D%5Cx30")).await;
    r.assert_secure_html(200);
    assert!(r.text().contains("name=\"cmd\" value=\"AT+CFUN=\\x30\"") && r.text().contains("Error: Command must not contain a backslash"));
    assert_eq!(device::calls(&c.app).len(), 1);
    // A PIN is never kept.
    let r = post(&c, "/ui/tools/at/send", peer(118, 4), &cookie, &format!("csrf={csrf}&once={once}&cmd=AT%2BCPIN%3D%221234%22")).await;
    r.assert_see_other(AT);
    assert_eq!(c.at.list(me, c.app.auth.now())[0].cmd, "AT+CPIN=\"\u{2022}\u{2022}\u{2022}\u{2022}\"");
    // A used token: the danger question is not shown, nothing sent.
    let r = post(&c, "/ui/tools/at/send", peer(118, 5), &cookie, &format!("csrf={csrf}&once={once}&cmd=AT%2BCFUN%3D0")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("This form expired") && !r.text().contains("Dangerous Command"));
    assert_eq!(device::calls(&c.app).len(), 2);
    // A PIN typed into the command: masked in the question, as in the
    // history; the hidden field keeps the command as typed.
    let once = get_with(&c, AT, peer(118, 6), &cookie).await.input("once");
    let r = post(&c, "/ui/tools/at/send", peer(118, 6), &cookie, &format!("csrf={csrf}&once={once}&cmd=AT%2BCLCK%3D%22SC%22%2C1%2C%224321%22")).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert!(html.contains("<li>The command &quot;AT+CLCK=&quot;SC&quot;,1,&quot;\u{2022}\u{2022}\u{2022}\u{2022}&quot;&quot; may disrupt"), "{html}");
    assert_eq!(html.matches("4321").count(), 1, "{html}");
    assert!(html.contains("name=\"cmd\" value=\"AT+CLCK=&quot;SC&quot;,1,&quot;4321&quot;\""), "{html}");
    assert_eq!(device::calls(&c.app).len(), 2);
}

/// The agent's own AT send on a build machine (no port): the error entry.
#[tokio::test]
async fn at_terminal_records_the_agents_error() {
    let (c, cookie, _) = signed_in(116).await;
    let page = get_with(&c, AT, peer(116, 2), &cookie).await;
    let body = format!("csrf={}&once={}&cmd=AT", page.input("csrf"), page.input("once"));
    post(&c, "/ui/tools/at/send", peer(116, 3), &cookie, &body).await.assert_see_other(AT);
    let html = get_with(&c, AT, peer(116, 4), &cookie).await.text();
    assert!(html.contains("<li class=\"er\"><b class=\"mono\">&gt; AT</b><pre>Error: no serial port found</pre>"), "{html}");
    // Clear: the history goes, nothing is sent.
    let csrf = page.input("csrf");
    post(&c, "/ui/tools/at/clear", peer(116, 5), &cookie, &format!("csrf={csrf}")).await.assert_see_other(AT);
    assert!(get_with(&c, AT, peer(116, 5), &cookie).await.text().contains("No Commands Sent"));
}

/// Logging in again in the same browser replaces the session: its AT
/// history goes with it; a wrong password keeps it.
#[tokio::test]
async fn a_replaced_session_loses_its_at_history() {
    let (c, cookie, me) = signed_in(117).await;
    let page = get_with(&c, AT, peer(117, 2), &cookie).await;
    post(&c, "/ui/tools/at/send", peer(117, 2), &cookie, &format!("csrf={}&once={}&cmd=hello", page.input("csrf"), page.input("once"))).await;
    assert_eq!(c.at.list(me, c.app.auth.now()).len(), 1);
    let relog = |pw: &'static str| {
        let c = c.clone();
        let cookie = cookie.clone();
        async move {
            let (pre, token) = login_form(&c, peer(117, 3)).await;
            let body = format!("login_csrf={}&next=%2Fui%2F&password={}", enc(&token), enc(pw));
            post(&c, "/ui/login", peer(117, 3), &format!("zte_ui_pre={pre}; {cookie}"), &body).await.status
        }
    };
    assert_eq!(relog("wrong").await, 401);
    assert_eq!(c.at.list(me, c.app.auth.now()).len(), 1);
    assert_eq!(relog(PW).await, 303);
    assert!(c.at.list(me, c.app.auth.now()).is_empty());
}

/// No permit in time: nothing is sent, "the router is busy", and the token
/// is given back, so the same form sends once later (AT send in the slow
/// lane, Enable ADB in the general one, an automation's save).
#[tokio::test]
async fn a_busy_router_gives_the_token_back() {
    let (mut c, cookie, _) = signed_in(120).await;
    let page = get_with(&c, AT, peer(120, 2), &cookie).await;
    let (csrf, once) = (page.input("csrf"), page.input("once"));
    let q = get_with(&c, "/ui/tools/adb/enable", peer(120, 2), &cookie).await;
    let adb = format!("csrf={csrf}&once={}&confirm=1", q.input("once"));
    let auto = get_with(&c, "/ui/tools/automations/new", peer(120, 2), &cookie).await.input("once");
    let save = format!("csrf={csrf}&once={auto}&name=N&action=reboot&st=recurring&time=01%3A00&days=0");
    let _dev = device::set(&c.app, device::Device::default());
    let free = Arc::clone(&c.gate);
    let body = format!("csrf={csrf}&once={once}&cmd=ATI");
    c.gate = Arc::new(Gate::new(4, 0, Duration::from_millis(20)));
    let r = post(&c, "/ui/tools/at/send", peer(120, 3), &cookie, &body).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("Failed: the router is busy.") && r.text().contains("value=\"ATI\""));
    c.gate = Arc::new(Gate::new(0, 4, Duration::from_millis(20)));
    let to = post(&c, "/ui/tools/adb/enable", peer(120, 4), &cookie, &adb).await.header("location").unwrap().to_owned();
    let r = post(&c, "/ui/tools/automations/save", peer(120, 5), &cookie, &save).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("Failed: the router is busy."));
    c.gate = free;
    assert!(get_with(&c, &to, peer(120, 4), &cookie).await.text().contains("Failed: the router is busy."));
    assert!(device::calls(&c.app).is_empty());
    assert!(crate::scheduler::jobs_list(&c.app).1["data"].as_array().unwrap().is_empty());
    // The same forms again: each sends once.
    post(&c, "/ui/tools/at/send", peer(120, 3), &cookie, &body).await.assert_see_other(AT);
    post(&c, "/ui/tools/adb/enable", peer(120, 4), &cookie, &adb).await;
    post(&c, "/ui/tools/automations/save", peer(120, 5), &cookie, &save).await.assert_see_other("/ui/tools/automations?saved=created");
    assert_eq!(device::calls(&c.app).len(), 2);
    assert_eq!(crate::scheduler::jobs_list(&c.app).1["data"].as_array().unwrap().len(), 1);
}
