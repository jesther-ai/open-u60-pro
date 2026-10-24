//! Router › eSIM Profiles (the list, Switch, Delete, Rename, Send now)
//! through the whole router. Device writes go to `pages::device`, page-load
//! reads to its `gets`, topics to `pages::fake`. Every value is a made-up
//! fixture: ICCIDs 8900000000000000xxx, an EID of zeros, the test APN.

use serde_json::json;

use super::*;
use crate::events::hub::ReadOut;
use crate::events::Topic;
use crate::web::pages::{device, fake};

const LIST: &str = "/ui/router/esim";
const SWITCH: &str = "/ui/router/esim/switch";
const DELETE: &str = "/ui/router/esim/delete";
const RENAME: &str = "/ui/router/esim/rename";
const RECEIPTS: &str = "/ui/router/esim/receipts";
const EID: &str = "00000000000000000000000000000000";
const ICCIDS: [&str; 4] = ["8900000000000000101", "8900000000000000202", "8900000000000000303", "8900000000000000404"];

fn calls(c: &Ctx) -> Vec<(String, Value)> {
    device::calls(&c.app)
}

fn tailnet(port: u16) -> Peer {
    Peer(SocketAddr::from(([127, 0, 0, 1], port)))
}

fn to(r: &Reply) -> String {
    assert_eq!(r.status, 303, "{}", r.text());
    r.header("location").unwrap().to_owned()
}

/// Four profiles as `GET /api/esim/profiles` gives them: the active one,
/// two that warn (the router's manual APN; a travel profile with roaming
/// off) and a test profile that cannot be switched to; two receipts wait.
fn profiles() -> Value {
    json!({
        "profiles": [
            {"iccid": ICCIDS[0], "state": "enabled", "nickname": "GOMO u60", "provider": "GOMO", "name": "GOMO", "class": "operational"},
            {"iccid": ICCIDS[1], "state": "disabled", "nickname": "DITO u60", "provider": "DITO", "name": "DITO", "class": "operational",
             "switch_warnings": ["manual_apn"]},
            {"iccid": ICCIDS[2], "state": "disabled", "nickname": null, "provider": "Travel - PHL", "name": "Klook_app", "class": "operational",
             "switch_warnings": ["manual_apn", "roaming_off"]},
            {"iccid": ICCIDS[3], "state": "disabled", "nickname": null, "provider": "Example", "name": "Test profile", "class": "test"},
        ],
        "enabled_iccid": ICCIDS[0], "observed_at": 1_761_200_000, "observed_seq": 3, "generation": 7, "agent_instance": 1,
        "updating": false, "stale": false, "error": null, "pending_receipts": 2,
        "manual_apn": {"name": "Example", "apn": "internet.example"},
    })
}

fn chip() -> Value {
    json!({"eid": EID, "firmware": "4.2.0", "free_nvm_bytes": 934_475, "sgp22_version": "2.2.2"})
}

fn device_with(replies: Vec<(&'static str, (u16, Value))>, extra: Vec<(&'static str, Result<Value, String>)>) -> device::Device {
    let mut gets = vec![("/api/esim/profiles", Ok(profiles())), ("/api/esim/chip", Ok(chip()))];
    gets.extend(extra);
    device::Device { gets, replies, ..Default::default() }
}

/// The key of profile `i` in the list's links.
fn key_of(html: &str, i: usize) -> String {
    let name = ["GOMO u60", "DITO u60", "Klook_app", "Test profile"][i];
    let at = html.find(&format!("Options for {name}")).unwrap_or_else(|| panic!("{name} in {html}"));
    html[at..].split("/ui/router/esim/rename?p=").nth(1).unwrap()[..16].to_owned()
}

/// The next frame that patches `id` (heartbeats may come first).
async fn patch(b: &mut axum::body::BodyDataStream, id: &str) -> String {
    loop {
        let f = frame(b).await.unwrap_or_else(|| panic!("a patch of {id}"));
        if f.contains(&format!("id=\"{id}\"")) {
            return f;
        }
    }
}

fn page_csrf(html: &str) -> String {
    html.split("name=\"csrf\" value=\"").nth(1).unwrap().split('"').next().unwrap().to_owned()
}

fn once(html: &str) -> String {
    html.split("name=\"once\" value=\"").nth(1).unwrap().split('"').next().unwrap().to_owned()
}

/// The list as served: the app's cards and words, identifiers masked
/// (ICCIDs to their last 4, the EID until asked for), profiles by opaque
/// keys; the stream patches the same fragments.
#[tokio::test]
async fn profiles_follow_the_app_with_identifiers_masked() {
    let (c, cookie, _) = signed_in(160).await;
    let _dev = device::set(&c.app, device_with(vec![], vec![]));
    let r = get_with(&c, LIST, peer(160, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>eSIM Profiles</h1>", "<a class=\"nl back\" href=\"/ui/router\">Router</a>",
        "<a class=\"nl ic\" href=\"/ui/router/esim/add\" aria-label=\"Add eSIM\">",
        "<a class=\"nl ic\" href=\"/ui/router/esim\" aria-label=\"Refresh profiles and connection\">",
        "<h2>On your eSIM</h2>", "<span class=\"cnt\">4 profiles</span>", "Last read ",
        "<small class=\"active\">", "Active</small><b>GOMO u60</b><small class=\"cap\">GOMO</small><small class=\"cap num\">…0101</small>",
        "<h2 class=\"esub\">Switch to another profile</h2>", "<b>Klook_app</b><small class=\"cap\">Travel - PHL</small>",
        "aria-label=\"Unavailable\"", "Add eSIM</a>", "Switching pauses internet for about a minute. Your router’s Wi-Fi stays on.",
        "<b>Operator receipts</b><small class=\"cap\">2 waiting · Retried automatically</small>", "Why are these waiting?",
        "action=\"/ui/router/esim/receipts\"", "Send now</button>", "<b>eSIM card</b><small class=\"cap\">Card details &amp; identifiers</small>",
        "<span>Firmware</span><span class=\"v\">4.2.0</span>", "<span>Free memory</span><span class=\"v\">913 KB</span>",
        "•••• •••• •••• …0000", "href=\"/ui/router/esim?eid=1#chip\" aria-label=\"Reveal EID\"", "@get('/ui/live/esim/",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // The active profile is not switched to or deleted; the test profile not switched to.
    let k: Vec<String> = (0..4).map(|i| key_of(&html, i)).collect();
    assert!(k.iter().all(|k| k.len() == 16 && k.bytes().all(|b| b.is_ascii_hexdigit())));
    assert!(!html.contains(&format!("{SWITCH}?p={}", k[0])) && !html.contains(&format!("{DELETE}?p={}", k[0])));
    assert!(html.contains(&format!("{SWITCH}?p={}", k[1])) && html.contains(&format!("{DELETE}?p={}", k[1])));
    assert!(!html.contains(&format!("{SWITCH}?p={}", k[3])) && html.contains(&format!("{DELETE}?p={}", k[3])));
    for secret in ICCIDS.iter().copied().chain([EID]) {
        assert!(!html.contains(secret), "{secret}");
    }
    // Delete profile with the app's trash icon.
    let del = html.split(&format!("href=\"{DELETE}?p={}\"", k[1])).nth(1).unwrap();
    assert!(del.split("</a>").next().unwrap().contains("M4.5 6.5h15") && del.contains("Delete profile"), "{del}");
    // The stream: the same fragments, never the EID or an ICCID (this
    // browser's 1 s refresh preference does not change the screen's own).
    let resp = open_live(&c, &live_url(&html, "esim"), peer(160, 3), &format!("{cookie}; zte_ui_refresh=1000")).await;
    for t in [Topic::EsimSwitch, Topic::EsimDownload, Topic::Modem, Topic::Signal] {
        assert_eq!(c.app.hub.demand(t), 1, "{t:?}");
    }
    // The connection card's signal every 10 s (the app's idle 12 s), not 2.
    assert_eq!(c.app.hub.cadence(Topic::Signal), Some(std::time::Duration::from_secs(10)));
    let mut b = resp.into_body().into_data_stream();
    let first = frame(&mut b).await.unwrap();
    for (id, part) in frame_parts(&first).iter().filter(|(id, _)| id.starts_with("esim-")) {
        assert!(html.contains(part.as_str()), "{id} differs from the page");
    }
    // A download: busy (the page's controls dim); once it has added a
    // profile, the list says it changed.
    let offer = |t: Topic, v: Value| c.app.hub.offer(t, ReadOut { value: Some(v), error: None }, std::time::Instant::now());
    offer(Topic::EsimDownload, json!({"state": "running", "step": "es9p_authenticate_client", "started_at": 1_761_200_100}));
    let next = patch(&mut b, "esim-dl").await;
    assert!(next.contains("<div class=\"busy\" id=\"esim-dl\">") && !next.contains("Changed"), "{next}");
    offer(Topic::EsimDownload, json!({"state": "done", "provider": "Example Travel", "iccid": "8900000000000000505", "started_at": 1_761_200_100}));
    let next = patch(&mut b, "esim-upd").await;
    assert!(next.contains("<b>Example Travel added.</b>") && next.contains("Changed · <a href=\"/ui/router/esim\">Refresh</a>"), "{next}");
    assert!(!next.contains("class=\"busy\" id=\"esim-dl\"") && !next.contains("0505"), "{next}");
    // A switch starts elsewhere: its card follows, the list says "Updating…".
    let sw = json!({"state": "reloading", "target_iccid": ICCIDS[1], "generation": 8, "sim_epoch": 0});
    c.app.hub.offer(Topic::EsimSwitch, ReadOut { value: Some(sw), error: None }, std::time::Instant::now());
    let next = frame(&mut b).await.unwrap();
    assert!(next.contains("<b>Reloading SIM</b>") && next.contains("Switch to DITO u60") && next.contains("Updating…"), "{next}");
    assert!(!next.contains(ICCIDS[1]) && !next.contains(EID), "{next}");
    // It ends without data, which comes up later: the card says connected
    // (the app's next check); the list changed.
    offer(Topic::EsimSwitch, json!({"state": "no_data", "target_iccid": ICCIDS[1], "generation": 8, "sim_epoch": 0}));
    let next = patch(&mut b, "esim-conn").await;
    assert!(next.contains("<b>Connection needs attention</b>") && next.contains("Changed · "), "{next}");
    offer(Topic::Modem, json!({"data": {"connect_status": "ipv4_ipv6_connected"}}));
    let next = patch(&mut b, "esim-conn").await;
    assert!(next.contains("Switch to DITO u60") && next.contains("<b>Mobile data connected</b>"), "{next}");
    drop(b);
    // Loaded while the SIM reloads (the agent answers with the list read
    // before the switch): the target shows as active once its enable went
    // through; not while the outcome is unknown.
    let _sw = fake::set(&c.app, json!({"state": "reloading", "target_iccid": ICCIDS[1], "generation": 9, "note": null}));
    let html = get_with(&c, LIST, peer(160, 5), &cookie).await.text();
    assert!(html.contains("Active</small><b>DITO u60</b>") && !html.contains("Active</small><b>GOMO u60</b>"), "{html}");
    let _sw = fake::set(&c.app, json!({"state": "reloading", "target_iccid": ICCIDS[1], "generation": 9, "note": "enable error: timed out"}));
    let html = get_with(&c, LIST, peer(160, 5), &cookie).await.text();
    assert!(html.contains("Active</small><b>GOMO u60</b>"), "{html}");
    // While it runs, the controls are dimmed (CSS keyed on the busy card).
    assert!(html.contains("class=\"card econn esw busy\" id=\"esim-conn\""), "{html}");
    let css = include_str!("../assets/app.css");
    assert!(css.contains("body:has(#esim-conn.busy, #esim-dl.busy) :is(.prow, a[href=\"/ui/router/esim/add\"]"), "dimmed while busy");
    drop(_sw);
    // Asked for: the EID whole, on this page load only.
    let shown = get_with(&c, &format!("{LIST}?eid=1"), peer(160, 4), &cookie).await.text();
    assert!(shown.contains(&format!("<span class=\"v mono\" translate=\"no\">{EID}</span>")), "{shown}");
    assert!(shown.contains("<details class=\"card chip\" id=\"chip\" open>") && shown.contains("aria-label=\"Hide EID\""), "{shown}");
}

/// No profiles: the app's empty card; a failed read: its error card.
#[tokio::test]
async fn empty_and_failed_lists() {
    let (c, cookie, _) = signed_in(161).await;
    let none = json!({"profiles": [], "observed_at": 0, "generation": 1, "updating": false, "error": null});
    let _dev = device::set(&c.app, device::Device { gets: vec![("/api/esim/profiles", Ok(none)), ("/api/esim/chip", Err("busy".into()))], ..Default::default() });
    let html = get_with(&c, LIST, peer(161, 2), &cookie).await.text();
    assert!(html.contains("<b>No profiles yet</b>") && html.contains("Add one with the QR code from your operator."), "{html}");
    assert!(html.contains("<div class=\"esim none\">") && !html.contains("eSIM card"), "{html}");
    assert!(html.contains("<a class=\"btn btn-wide teal\" href=\"/ui/router/esim/add\">"), "the app's prominent teal button: {html}");
    // Not while a download runs (the app's `!download.isBusy`).
    let _dl = fake::set(&c.app, json!({"state": "running", "step": "es9p_authenticate_client"}));
    let html = get_with(&c, LIST, peer(161, 2), &cookie).await.text();
    assert!(!html.contains("No profiles yet") && html.contains("Connecting to the operator…"), "{html}");
    drop(_dl);
    let stale = json!({"profiles": profiles()["profiles"], "observed_at": 0, "generation": 7, "updating": false,
        "error": {"kind": "sim_reloading", "message": "euicc_init failed", "at": 1}});
    let _dev = device::set(&c.app, device::Device { gets: vec![("/api/esim/profiles", Ok(stale))], ..Default::default() });
    let html = get_with(&c, LIST, peer(161, 3), &cookie).await.text();
    assert!(html.contains("Couldn’t refresh profiles") && html.contains("The eSIM is still settling after the switch. Showing the list read at an earlier time."), "{html}");
    let _dev = device::set(&c.app, device::Device { gets: vec![("/api/esim/profiles", Err("eSIM manager busy; retry shortly".into()))], ..Default::default() });
    let html = get_with(&c, LIST, peer(161, 4), &cookie).await.text();
    assert!(html.contains("Couldn't read eSIM profiles: eSIM manager busy; retry shortly") && html.contains("<span class=\"cnt\">Not read yet</span>"), "{html}");
}

/// Switch: the app's dialog with its warnings (and, remotely, that the
/// connection drops and nothing switches back); one enable per token; the
/// operation page follows the switch; a replay lands on it again.
#[tokio::test]
async fn switch_asks_then_follows_the_switch_once() {
    let (c, cookie, _) = signed_in(162).await;
    let _dev = device::set(&c.app, device_with(vec![], vec![]));
    let list = get_with(&c, LIST, peer(162, 2), &cookie).await.text();
    let (dito, klook, active, test) = (key_of(&list, 1), key_of(&list, 2), key_of(&list, 0), key_of(&list, 3));
    let r = get_with(&c, &format!("{SWITCH}?p={klook}"), peer(162, 3), &cookie).await;
    r.assert_secure_html(200);
    let q = r.text();
    for want in [
        "<h1>Confirm</h1>", "<h2 class=\"q\">Switch to Klook_app?</h2>",
        "The router disconnects from GOMO u60 and connects with Klook_app. Internet is down for about a minute. Wi-Fi stays on.",
        "The router is set to the manual APN “internet.example”. If Klook_app needs a different APN, change it in Mobile Network after switching, or it may have no data.",
        "Klook_app looks like a travel (roaming) profile and Data Roaming is off, so it will have no data until you turn Data Roaming on.",
        "<button class=\"btn\" type=\"submit\">Switch to Klook_app</button>", "href=\"/ui/router/esim\">Cancel</a>",
    ] {
        assert!(q.contains(want), "{want} in {q}");
    }
    assert!(!q.contains("connected remotely") && !q.contains(ICCIDS[2]));
    // Remotely: the remote lines.
    let remote = get_with(&c, &format!("{SWITCH}?p={dito}"), tailnet(4100), &cookie).await.text();
    assert!(remote.contains(super::super::pages::confirm::REMOTE) && remote.contains("The router does not switch back by itself"), "{remote}");
    assert!(!remote.contains("travel (roaming)"));
    // Nothing to ask: the active profile, a test profile, an unknown key.
    for (p, title) in [(&active, "GOMO u60 is already active."), (&test, "Test profile can’t be switched to here.")] {
        let page = get_with(&c, &format!("{SWITCH}?p={p}"), peer(162, 4), &cookie).await.text();
        assert!(page.contains(&format!("<h2 class=\"q\">{title}</h2>")) && !page.contains("name=\"once\""), "{page}");
    }
    let gone = get_with(&c, &format!("{SWITCH}?p=0123456789abcdef"), peer(162, 4), &cookie).await;
    assert_eq!(gone.status, 404);
    assert!(gone.text().contains("This profile is no longer on your eSIM."));
    // A post without `confirm=1` goes to the question; nothing is sent.
    let (csrf, token) = (page_csrf(&q), once(&q));
    let r = post(&c, SWITCH, peer(162, 5), &cookie, &format!("csrf={csrf}&p={klook}&once={token}")).await;
    assert_eq!(to(&r), format!("{SWITCH}?p={klook}"));
    assert!(calls(&c).is_empty());
    // Two posts at once: one enable, both on the operation page.
    let body = format!("csrf={csrf}&p={klook}&once={token}&confirm=1");
    let (a, b) = tokio::join!(post(&c, SWITCH, peer(162, 5), &cookie, &body), post(&c, SWITCH, peer(162, 5), &cookie, &body));
    assert_eq!(to(&a), format!("/ui/op/{token}"));
    assert_eq!(to(&b), format!("/ui/op/{token}"));
    assert_eq!(calls(&c), [("/api/esim/enable".to_owned(), json!({"iccid": ICCIDS[2]}))]);
    let r = post(&c, SWITCH, peer(162, 5), &cookie, &body).await;
    assert_eq!(to(&r), format!("/ui/op/{token}"));
    assert_eq!(calls(&c).len(), 1);
    // The operation page: the app's card and steps; a way back, never a switch back.
    let _sw = fake::set(&c.app, json!({"state": "card_confirmed", "target_iccid": ICCIDS[2], "generation": 8}));
    let op = get_with(&c, &format!("/ui/op/{token}"), peer(162, 6), &cookie).await;
    op.assert_secure_html(200);
    let op = op.text();
    for want in [
        "<h1 id=\"op-title\">Registering on network</h1>", "Switch to Klook_app", "Reload SIM</span><small>Done</small>",
        "Connect to network</span><small>In progress</small>", "Wi-Fi stays connected · Usually 15–45 seconds",
        "href=\"/ui/router/esim\">Back to eSIM Profiles</a>", "@get('/ui/live/op/",
    ] {
        assert!(op.contains(want), "{want} in {op}");
    }
    assert!(!op.contains(ICCIDS[2]) && !op.to_ascii_lowercase().contains("switch back"), "{op}");
    // Its stream: only the switch and the signal, until the switch ends
    // (with "Check again" when it needs attention); nothing sampled after.
    let resp = open_live(&c, &live_url(&op, "op"), peer(162, 6), &cookie).await;
    for t in Topic::ALL {
        let want = u16::from(matches!(t, Topic::EsimSwitch | Topic::Signal));
        assert_eq!(c.app.hub.demand(t), want, "{t:?}");
    }
    let mut b = resp.into_body().into_data_stream();
    let _ = frame(&mut b).await.unwrap();
    let end = json!({"state": "not_applied", "reason": "card_busy", "target_iccid": ICCIDS[2], "generation": 8});
    c.app.hub.offer(Topic::EsimSwitch, ReadOut { value: Some(end), error: None }, std::time::Instant::now());
    let mut last = String::new();
    while let Some(f) = frame(&mut b).await {
        last.push_str(&f);
    }
    assert!(last.contains("<b>Switch needs attention</b>") && last.contains("href=\"\">Check again</a>"), "{last}");
    assert!(last.contains("<p class=\"live\" id=\"live-status\"></p>") && !last.contains(ICCIDS[2]), "{last}");
    assert!(Topic::ALL.into_iter().all(|t| c.app.hub.demand(t) == 0));
    let _sw = fake::set(&c.app, json!({"state": "no_data", "reason": "roaming_off", "target_iccid": ICCIDS[2], "generation": 8}));
    let done = get_with(&c, &format!("/ui/op/{token}"), peer(162, 6), &cookie).await.text();
    assert!(done.contains("<h1 id=\"op-title\">Connection needs attention</h1>") && !done.contains("/ui/live/op/"), "{done}");
    assert!(done.contains("This profile roams on a local network and Data Roaming is off. Turn on Data Roaming in Mobile Network."));
}

/// The agent's refusals: busy reads as the app's busy card, others as
/// "Switch failed: …" with the app's guidance; nothing is offered or sent
/// while a switch or a download runs.
#[tokio::test]
async fn switch_refusals_read_as_the_app() {
    let (c, cookie, _) = signed_in(163).await;
    let busy = (409, json!({"ok": false, "error": "eSIM manager busy; retry shortly"}));
    let _dev = device::set(&c.app, device_with(vec![("/api/esim/enable", busy)], vec![]));
    let list = get_with(&c, LIST, peer(163, 2), &cookie).await.text();
    let dito = key_of(&list, 1);
    let q = get_with(&c, &format!("{SWITCH}?p={dito}"), peer(163, 3), &cookie).await.text();
    let body = format!("csrf={}&p={dito}&once={}&confirm=1", page_csrf(&q), once(&q));
    let r = post(&c, SWITCH, peer(163, 3), &cookie, &body).await;
    let line = to(&r);
    assert!(line.starts_with("/ui/router/esim?r="), "{line}");
    let html = get_with(&c, &line, peer(163, 3), &cookie).await.text();
    assert!(html.contains("<b>eSIM in use</b><p>The eSIM is busy; try again in a moment.</p>") && !html.contains("Reload profiles"), "{html}");
    // Airplane mode: the app's words and guidance.
    let lpm = (409, json!({"ok": false, "error": "the modem is in airplane / low-power mode; turn it back on before switching profiles"}));
    let _dev = device::set(&c.app, device_with(vec![("/api/esim/enable", lpm)], vec![]));
    let q = get_with(&c, &format!("{SWITCH}?p={dito}"), peer(163, 4), &cookie).await.text();
    let body = format!("csrf={}&p={dito}&once={}&confirm=1", page_csrf(&q), once(&q));
    let html = get_with(&c, &to(&post(&c, SWITCH, peer(163, 4), &cookie, &body).await), peer(163, 4), &cookie).await.text();
    for want in [
        "<b>Can’t complete this right now</b><p>Switch failed: the modem is in airplane / low-power mode",
        "Open Router Settings → Mobile Network to check the modem mode.", "href=\"/ui/router/esim\">Reload profiles</a>",
        "Reloading only reads the list. It doesn’t repeat your action.",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // A switch runs: the question offers nothing and a post sends nothing.
    let _sw = fake::set(&c.app, json!({"state": "reloading", "generation": 9}));
    let q2 = get_with(&c, &format!("{SWITCH}?p={dito}"), peer(163, 5), &cookie).await.text();
    assert!(q2.contains("<h2 class=\"q\">The eSIM is busy; try again in a moment.</h2>") && !q2.contains("name=\"once\""), "{q2}");
    let token = c.once.fresh(&c.app.auth, c.app.auth.authenticate(Kind::Browser, cookie.trim_start_matches("zte_ui_session=")).unwrap().id);
    let n = calls(&c).len();
    let body = format!("csrf={}&p={dito}&once={token}&confirm=1", page_csrf(&q));
    let r = post(&c, SWITCH, peer(163, 5), &cookie, &body).await;
    assert!(to(&r).starts_with("/ui/router/esim?r="));
    assert_eq!(calls(&c).len(), n);
    // The token was given back (nothing sent): it still works once the switch ends.
    assert_eq!(c.once.get(&token, c.app.auth.authenticate(Kind::Browser, cookie.trim_start_matches("zte_ui_session=")).unwrap().id, c.app.auth.now()), Some(crate::web::store::Token::Fresh));
}

/// Delete: the app's dialog (inactive profiles only), one call, its line;
/// Rename: the sheet, the 64-byte check, its lines, nothing for no change.
#[tokio::test]
async fn delete_and_rename_follow_the_app() {
    let (c, cookie, _) = signed_in(164).await;
    let deleted = (200, json!({"ok": true, "data": {"deleted_iccid": ICCIDS[1], "operator_notified": false}}));
    let _dev = device::set(&c.app, device_with(vec![("/api/esim/delete", deleted)], vec![]));
    let list = get_with(&c, LIST, peer(164, 2), &cookie).await.text();
    let (gomo, dito) = (key_of(&list, 0), key_of(&list, 1));
    let q = get_with(&c, &format!("{DELETE}?p={dito}"), peer(164, 3), &cookie).await.text();
    for want in [
        "<h2 class=\"q\">Delete DITO u60 permanently?</h2>",
        "This can’t be undone. You’d need a new QR code from DITO to get this eSIM back.",
        "<button class=\"btn btn-danger\" type=\"submit\">Delete DITO u60</button>",
    ] {
        assert!(q.contains(want), "{want} in {q}");
    }
    let active = get_with(&c, &format!("{DELETE}?p={gomo}"), peer(164, 3), &cookie).await.text();
    assert!(active.contains("<h2 class=\"q\">GOMO u60 is active.</h2>") && !active.contains("name=\"once\""), "{active}");
    let body = format!("csrf={}&p={dito}&once={}&confirm=1", page_csrf(&q), once(&q));
    let (a, b) = tokio::join!(post(&c, DELETE, peer(164, 4), &cookie, &body), post(&c, DELETE, peer(164, 4), &cookie, &body));
    assert_eq!(to(&a), to(&b));
    assert_eq!(calls(&c), [("/api/esim/delete".to_owned(), json!({"iccid": ICCIDS[1]}))]);
    let html = get_with(&c, &to(&a), peer(164, 4), &cookie).await.text();
    assert!(html.contains("<b>Profile updated</b><p>DITO u60 deleted. The operator will be notified on the next eSIM change.</p>"), "{html}");
    // Rename: the sheet.
    let r = get_with(&c, &format!("{RENAME}?p={dito}"), peer(164, 5), &cookie).await;
    r.assert_secure_html(200);
    let sheet = r.text();
    for want in [
        "<h1>Rename profile</h1>", "<div class=\"lead\"><a class=\"nl\" href=\"/ui/router/esim\">Cancel</a></div>", "<h2>Nickname</h2>",
        "name=\"nickname\" value=\"DITO u60\" placeholder=\"DITO\" maxlength=\"64\"", "Leave empty to use the operator’s name.",
        "<span class=\"num\">8 / 64 bytes</span>", "Names with emoji or accented letters may use more than one byte per character.", ">Save</button>",
    ] {
        assert!(sheet.contains(want), "{want} in {sheet}");
    }
    let rename = |nick: &str, token: &str| format!("csrf={}&p={dito}&once={token}&nickname={}", page_csrf(&sheet), enc(nick));
    // Over 64 bytes (32 two-byte letters and one more): the sheet again, nothing sent.
    let long = "é".repeat(33);
    let r = post(&c, RENAME, peer(164, 5), &cookie, &rename(&long, &once(&sheet))).await;
    assert_eq!(r.status, 400);
    assert!(r.text().contains("Use one line, up to 64 bytes.") && r.text().contains("66 / 64 bytes") && r.text().contains("aria-invalid=\"true\""), "{}", r.text());
    let n = calls(&c).len();
    // Unchanged: back, nothing sent.
    let r = post(&c, RENAME, peer(164, 5), &cookie, &rename("DITO u60", &once(&sheet))).await;
    assert_eq!(to(&r), LIST);
    assert_eq!(calls(&c).len(), n);
    // A new name, then none: the app's lines.
    let r = post(&c, RENAME, peer(164, 5), &cookie, &rename("  Home  ", &once(&sheet))).await;
    let html = get_with(&c, &to(&r), peer(164, 5), &cookie).await.text();
    assert!(html.contains("<p>Renamed to Home.</p>"), "{html}");
    assert_eq!(calls(&c).last().unwrap(), &("/api/esim/nickname".to_owned(), json!({"iccid": ICCIDS[1], "nickname": "Home"})));
    let sheet2 = get_with(&c, &format!("{RENAME}?p={dito}"), peer(164, 6), &cookie).await.text();
    let body = format!("csrf={}&p={dito}&once={}&nickname=", page_csrf(&sheet2), once(&sheet2));
    let html = get_with(&c, &to(&post(&c, RENAME, peer(164, 6), &cookie, &body).await), peer(164, 6), &cookie).await.text();
    assert!(html.contains("<p>Nickname cleared.</p>"), "{html}");
}

/// Send now: the agent sends in the background; the line after the wait
/// says what the count came to.
#[tokio::test]
async fn send_now_reports_the_receipts() {
    let (c, cookie, _) = signed_in(165).await;
    let left = ("/api/esim/notifications", Ok(json!({"count": 1, "pending": []})));
    let _dev = device::set(&c.app, device_with(vec![("/api/esim/notifications/process", (202, json!({"ok": true, "data": {}})))], vec![left]));
    let list = get_with(&c, LIST, peer(165, 2), &cookie).await.text();
    let body = format!("csrf={}&once={}", page_csrf(&list), once(&list));
    let html = get_with(&c, &to(&post(&c, RECEIPTS, peer(165, 3), &cookie, &body).await), peer(165, 3), &cookie).await.text();
    assert!(html.contains("<b>Operator receipts</b><p>Some receipts couldn&#39;t be sent yet. The router keeps retrying.</p>")
        || html.contains("<b>Operator receipts</b><p>Some receipts couldn't be sent yet. The router keeps retrying.</p>"), "{html}");
    assert_eq!(calls(&c), [("/api/esim/notifications/process".to_owned(), json!({}))]);
    let sent = ("/api/esim/notifications", Ok(json!({"count": 0, "pending": []})));
    let _dev = device::set(&c.app, device_with(vec![], vec![sent]));
    let list = get_with(&c, LIST, peer(165, 4), &cookie).await.text();
    let body = format!("csrf={}&once={}", page_csrf(&list), once(&list));
    let html = get_with(&c, &to(&post(&c, RECEIPTS, peer(165, 4), &cookie, &body).await), peer(165, 4), &cookie).await.text();
    assert!(html.contains("<b>Operator receipts</b><p>Operator receipts sent.</p>"), "{html}");
}

/// Every eSIM post refuses a missing CSRF token or a foreign origin and
/// needs a session; nothing is sent.
#[tokio::test]
async fn esim_posts_check_session_and_csrf() {
    let (c, cookie, _) = signed_in(166).await;
    let _dev = device::set(&c.app, device_with(vec![], vec![]));
    let list = get_with(&c, LIST, peer(166, 2), &cookie).await.text();
    let dito = key_of(&list, 1);
    for path in [SWITCH, DELETE, RENAME, RECEIPTS] {
        let body = format!("p={dito}&once={}&confirm=1&nickname=x", once(&list));
        assert_eq!(post(&c, path, peer(166, 3), &cookie, &body).await.status, 403, "{path}");
        let r = form(path, peer(166, 3), &cookie, &format!("csrf={}&{body}", page_csrf(&list)))
            .header("origin", "http://evil.example")
            .body(Body::from(format!("csrf={}&{body}", page_csrf(&list))))
            .unwrap();
        assert_eq!(send(&c, r).await.status, 403, "{path}");
        let r = post(&c, path, peer(166, 4), "", &format!("csrf={}&{body}", page_csrf(&list))).await;
        assert!(to(&r).starts_with("/ui/login"), "{path}");
        // A key that is not one: 400.
        if path != RECEIPTS {
            let r = post(&c, path, peer(166, 5), &cookie, &format!("csrf={}&p=89000&once=x&confirm=1", page_csrf(&list))).await;
            assert_eq!(r.status, 400, "{path}");
        }
    }
    assert!(calls(&c).is_empty());
}
