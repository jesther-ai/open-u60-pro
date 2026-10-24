//! Router › SIM Card (and its sheets) and SIM Services (USSD, the SIM
//! Toolkit) through the whole router. Device writes go to
//! `pages::device`, page-load reads to its `gets`, topics to `pages::fake`.
//! Every value is a made-up fixture: PINs 1234/5678, PUK 12345678,
//! identifiers of the test network (MCC 001, MNC 01).

use serde_json::json;

use super::*;
use crate::events::hub::ReadOut;
use crate::events::Topic;
use crate::web::pages::{device, fake};

const SIM: &str = "/ui/router/sim";
const PIN: &str = "/ui/router/sim/pin";
const LOCK_ON: &str = "/ui/router/sim/pin-lock/enable";
const LOCK_OFF: &str = "/ui/router/sim/pin-lock/disable";
const CHANGE: &str = "/ui/router/sim/change-pin";
const PUK: &str = "/ui/router/sim/puk";
const NCK: &str = "/ui/router/sim/nck";
const SVC: &str = "/ui/router/sim-services";

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

/// A SIM as `GET /api/sim/info` gives it (made-up values).
fn sim(state: &str, pin_status: &str, pins: u8) -> Value {
    json!({
        "sim_states": state, "modem_main_state": "", "sim_iccid": "89001010000000004242", "sim_imsi": "001010000004242",
        "msisdn": "+10000004242", "mdm_mcc": "001", "mdm_mnc": "01", "Operator": "Test Net", "current_sim_slot": "1",
        "spn_name_data": "0054006500730074002000530049004D", "pin_status": pin_status, "pinnumber": pins.to_string(), "puknumber": "10",
        "sim_epoch": 3, "sim_phase": "online",
    })
}

fn trials(n: u8) -> (&'static str, Result<Value, String>) {
    ("/api/sim/lock-trials", Ok(json!({"available_trials": n})))
}

/// The page as served: masked identifiers, the app's sections, the sheets
/// as links, SIM Lock from its read; the stream patches the same HTML.
#[tokio::test]
async fn sim_card_follows_the_app_with_identifiers_masked() {
    let (c, cookie, _) = signed_in(150).await;
    let _dev = device::set(&c.app, device::Device { gets: vec![trials(3)], ..Default::default() });
    let _fake = fake::set(&c.app, sim("sim ready", "1", 3));
    let r = get_with(&c, SIM, peer(150, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>SIM Card</h1>", "<a class=\"nl back\" href=\"/ui/router\">Router</a>",
        "<span>Status</span><span class=\"v c-green\">Ready</span>", "<span>ICCID</span><span class=\"v\">…4242</span>",
        "<span>IMSI</span><span class=\"v\">001/01 …4242</span>", "<span>MSISDN</span><span class=\"v\">…4242</span>",
        "<span>SPN</span><span class=\"v\">Test SIM</span>", "<span>Operator</span><span class=\"v\">Test Net</span>",
        "<h2>PIN Management</h2>", "<span class=\"v c-green\">Enabled</span>", "href=\"/ui/router/sim/pin-lock/disable\"",
        "<span>Disable PIN Lock</span>", "href=\"/ui/router/sim/change-pin\"", "<h2>SIM Lock</h2>",
        "<span>Unlock Attempts</span><span class=\"v\">3</span>", "href=\"/ui/router/sim/nck\"", "@get('/ui/live/sim/4/",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    for secret in ["89001010000000004242", "001010000004242", "10000004242", "0000004242"] {
        assert!(!html.contains(secret), "{secret}");
    }
    let resp = open_live(&c, &live_url(&html, "sim"), peer(150, 3), &cookie).await;
    assert_eq!((c.app.hub.demand(Topic::Sim), c.app.hub.demand(Topic::SimIdentity)), (1, 0));
    let mut b = resp.into_body().into_data_stream();
    let first = frame(&mut b).await.unwrap();
    for (id, part) in frame_parts(&first).iter().filter(|(id, _)| id.starts_with("sim-")) {
        assert!(html.contains(part.as_str()), "{id} differs from the page");
    }
    // The SIM locks while the page is open: its banner, no lock rows.
    c.app.hub.offer(Topic::Sim, ReadOut { value: Some(sim("wait pin", "1", 2)), error: None }, std::time::Instant::now());
    let next = frame(&mut b).await.unwrap();
    assert!(next.contains("<b>SIM PIN Required</b>") && next.contains("href=\"/ui/router/sim/pin\">Enter PIN</a>"), "{next}");
    assert!(!next.contains("Disable PIN Lock") && !next.contains("0000004242"), "{next}");
    // Another SIM (identity epoch 4): a Refresh for SIM Lock.
    let mut other = sim("sim ready", "1", 3);
    other["sim_epoch"] = json!(4);
    c.app.hub.offer(Topic::Sim, ReadOut { value: Some(other), error: None }, std::time::Instant::now());
    let next = frame(&mut b).await.unwrap();
    assert!(next.contains("<div id=\"sim-chg\"><p class=\"msg info\" role=\"status\">The SIM changed. <a href=\"\">Refresh</a></p></div>"), "{next}");
    drop(b);
    // No unlock attempts: no Enter Unlock Code.
    let _dev = device::set(&c.app, device::Device { gets: vec![trials(0)], ..Default::default() });
    let html = get_with(&c, SIM, peer(150, 4), &cookie).await.text();
    assert!(html.contains("<span>Unlock Attempts</span><span class=\"v\">0</span>") && !html.contains("Enter Unlock Code"));
}

/// Enter PIN: a form with an empty secure field and a single-use token;
/// the checks never echo; one call per token with the app's body; the
/// app's line; a failure shows the sheet again, empty, with the error;
/// a SIM that does not wait for a PIN gets nothing sent.
#[tokio::test]
async fn enter_pin_sends_once_and_never_echoes() {
    let (c, cookie, _) = signed_in(151).await;
    let _fake = fake::set(&c.app, sim("wait pin", "1", 1));
    let _dev = device::set(&c.app, device::Device::default());
    let r = get_with(&c, PIN, peer(151, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>Enter PIN</h1>", "<div class=\"lead\"><a class=\"nl\" href=\"/ui/router/sim\">Cancel</a></div>",
        "<form class=\"rf req\" method=\"post\" action=\"/ui/router/sim/pin\">",
        "<input id=\"pin\" type=\"password\" name=\"pin\" placeholder=\"PIN\" inputmode=\"numeric\" maxlength=\"8\" required autocomplete=\"one-time-code\"",
        "<p class=\"foot\">1 attempt remaining</p>", "This is the last attempt: a wrong PIN locks the SIM until its PUK is entered.",
        "<input type=\"hidden\" name=\"left\" value=\"1\">",
        "<button class=\"it cv snd\" type=\"submit\">Unlock</button>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("/ui/live/") && !html.contains(super::super::pages::confirm::REMOTE));
    let (csrf, token) = (r.input("csrf"), r.input("once"));
    // Too short: the app's message, nothing kept, nothing sent.
    let r = post(&c, PIN, peer(151, 3), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&left=1&pin=987")).await;
    r.assert_secure_html(400);
    // (Never as a value: random tokens may contain the digits.)
    assert!(r.text().contains("PIN must be at least 4 digits") && !r.text().contains("\"987\"") && !r.text().contains(">987<"), "{}", r.text());
    assert!(calls(&c).is_empty());
    // Drawn for other attempts than the SIM has now (another tab, the app):
    // nothing sent, the sheet as it is now (its last-attempt warning), the
    // token kept for it.
    for stale in ["left=2", "left=", "x=1"] {
        let r = post(&c, PIN, peer(151, 3), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&{stale}&pin=1234")).await;
        r.assert_secure_html(409);
        let html = r.text();
        assert!(html.contains("The attempts left have changed, so nothing was sent.") && html.contains("This is the last attempt") && !html.contains("1234\""), "{html}");
        assert!(html.contains("<input type=\"hidden\" name=\"left\" value=\"1\">"), "{html}");
    }
    assert!(calls(&c).is_empty());
    // Two posts of one form: one call, both to SIM Card with the app's line.
    let body = format!("csrf={csrf}&once={token}&confirm=1&left=1&pin=1234");
    let (a, b) = tokio::join!(post(&c, PIN, peer(151, 4), &cookie, &body), post(&c, PIN, peer(151, 4), &cookie, &body));
    let (la, lb) = (to(&a), to(&b));
    assert!(la.starts_with("/ui/router/sim?r=") && lb.starts_with("/ui/router/sim?r="), "{la} {lb}");
    assert_eq!(calls(&c), [("/api/sim/pin/verify".to_owned(), json!({"pin_num": "1234", "puk_num": "", "pin_encode_flag": "0"}))]);
    let html = get_with(&c, &la, peer(151, 4), &cookie).await.text();
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">PIN verified</p>"), "{html}");
    // A wrong PIN: the sheet again with the error, empty, a new token.
    let _dev = device::set(&c.app, device::Device {
        replies: vec![("/api/sim/pin/verify", (503, json!({"ok": false, "error": "ubus call zwrt_zte_mdm.api sim_verify_pin_puk failed: <x>"})))],
        ..Default::default()
    });
    let token = get_with(&c, PIN, peer(151, 5), &cookie).await.input("once");
    let r = post(&c, PIN, peer(151, 5), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&left=1&pin=5678")).await;
    r.assert_secure_html(503);
    let html = r.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed: ubus call zwrt_zte_mdm.api sim_verify_pin_puk failed: &lt;x&gt;</p>"), "{html}");
    assert!(!html.contains("5678") && r.input("once") != token);
    for h in r.headers.values() {
        assert!(!h.to_str().unwrap_or("").contains("5678"));
    }
    // A replay of the failed post: the same failure, nothing sent again.
    let r = post(&c, PIN, peer(151, 5), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&left=1&pin=5678")).await;
    assert!(r.text().contains("Failed: ubus call") && calls(&c).len() == 1, "{}", r.text());
    // An answer that says it failed is a failure (not "PIN verified").
    let _dev = device::set(&c.app, device::Device { replies: vec![("/api/sim/pin/verify", (200, json!({"ok": true, "data": {"result": "failure"}})))], ..Default::default() });
    let token = get_with(&c, PIN, peer(151, 6), &cookie).await.input("once");
    let r = post(&c, PIN, peer(151, 6), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&left=1&pin=1234")).await;
    assert!(r.text().contains("Failed: failure"), "{}", r.text());
    // The last wrong PIN locks the SIM (while the call runs): the sheet
    // says it now needs its PUK and leads to Enter PUK.
    let _wait = fake::set(&c.app, sim("wait pin", "1", 1));
    let _dev = device::set(&c.app, device::Device {
        replies: vec![("/api/sim/pin/verify", (503, json!({"ok": false, "error": "wrong PIN (fake)"})))],
        wait: Some(std::time::Duration::from_millis(400)),
        ..Default::default()
    });
    let token = get_with(&c, PIN, peer(151, 6), &cookie).await.input("once");
    let body = format!("csrf={csrf}&once={token}&confirm=1&left=1&pin=4321");
    let lock = async {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        fake::set(&c.app, sim("wait puk", "1", 0))
    };
    let (r, _puk) = tokio::join!(post(&c, PIN, peer(151, 6), &cookie, &body), lock);
    let html = r.text();
    assert!(html.contains("Failed: wrong PIN (fake)") && html.contains("The SIM is locked after too many wrong PINs and now needs its PUK."), "{html}");
    assert!(html.contains("<a class=\"btn\" href=\"/ui/router/sim/puk\">Enter PUK</a>") && !html.contains("4321"), "{html}");
    assert_eq!(calls(&c).len(), 1);
    let html = get_with(&c, PIN, peer(151, 6), &cookie).await.text();
    assert!(html.contains("now needs its PUK") && html.contains("href=\"/ui/router/sim/puk\">Enter PUK</a>"), "{html}");
    // The SIM is ready by now: the sheet says so and a post sends nothing.
    let _fake = fake::set(&c.app, sim("sim ready", "1", 3));
    let _dev = device::set(&c.app, device::Device::default());
    let r = get_with(&c, PIN, peer(151, 7), &cookie).await;
    assert!(r.text().contains("The SIM is not waiting for its PIN.") && !r.text().contains("<form class=\"rf"), "{}", r.text());
    let token = get_with(&c, LOCK_OFF, peer(151, 7), &cookie).await.input("once");
    let r = post(&c, PIN, peer(151, 7), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&pin=1234")).await;
    r.assert_secure_html(409);
    assert!(r.text().contains("The SIM is not waiting for its PIN.") && calls(&c).is_empty());
    // An unknown token: nothing sent.
    let r = post(&c, PIN, peer(151, 8), &cookie, &format!("csrf={csrf}&once=00000000000000ff&confirm=1&pin=1234")).await;
    assert!(r.text().contains("This page expired, so nothing was sent.") && calls(&c).is_empty(), "{}", r.text());
}

/// The lock, change and PUK sheets are their own questions: what happens,
/// the attempts, the remote line for a remote browser, a red button; the
/// app's bodies; the new PIN typed twice.
#[tokio::test]
async fn pin_lock_change_and_puk_ask_and_send_the_apps_bodies() {
    let (c, cookie, _) = signed_in(152).await;
    let _fake = fake::set(&c.app, sim("sim ready", "0", 3));
    let _dev = device::set(&c.app, device::Device::default());
    let r = get_with(&c, LOCK_ON, tailnet(15201), &cookie).await;
    let html = r.text();
    for want in [
        "<h1>Enable PIN Lock</h1>", "<input type=\"hidden\" name=\"left\" value=\"3\">",
        "The SIM will ask for this PIN every time the router starts.", "A wrong PIN uses an attempt.",
        super::super::pages::confirm::REMOTE, "<button class=\"it cv snd danger\" type=\"submit\">Enable</button>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // The app shows the attempts only where a PIN unlocks (Enter PIN, PUK, NCK).
    assert!(!html.contains("attempts remaining"), "{html}");
    assert!(!get_with(&c, LOCK_ON, peer(152, 2), &cookie).await.text().contains(super::super::pages::confirm::REMOTE));
    // The lock is off: Disable has nothing to do.
    assert!(get_with(&c, LOCK_OFF, peer(152, 2), &cookie).await.text().contains("The PIN lock is already off."));
    let csrf = r.input("csrf");
    let r = post(&c, LOCK_ON, peer(152, 3), &cookie, &format!("csrf={csrf}&once={}&confirm=1&left=3&pin=1234", r.input("once"))).await;
    assert!(to(&r).starts_with("/ui/router/sim?r="));
    assert_eq!(calls(&c)[0], ("/api/sim/pin/mode".to_owned(), json!({"pin_num_m": "1234", "pin_mode": 1, "pin_encode_flag": "0"})));
    let html = get_with(&c, &to(&r), peer(152, 3), &cookie).await.text();
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">PIN lock enabled</p>"));
    // Change PIN: the new PIN twice.
    let r = get_with(&c, CHANGE, peer(152, 4), &cookie).await;
    assert!(r.text().contains("<h2>Current PIN</h2>") && r.text().contains("placeholder=\"Confirm New PIN\""), "{}", r.text());
    let token = r.input("once");
    let r = post(&c, CHANGE, peer(152, 4), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&left=3&old=1234&new=5678&new2=5687")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("The new PINs do not match.") && !r.text().contains("5678") && !r.text().contains("1234\""));
    let r = post(&c, CHANGE, peer(152, 4), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&left=3&old=1234&new=5678&new2=5678")).await;
    assert!(to(&r).starts_with("/ui/router/sim?r="));
    assert_eq!(calls(&c)[1], ("/api/sim/pin/change".to_owned(), json!({"pin_num": "1234", "new_pin_num": "5678", "pin_encode_flag": "0"})));
    // PUK: only while the SIM waits for it; the app's body.
    assert!(get_with(&c, PUK, peer(152, 5), &cookie).await.text().contains("The SIM is not waiting for its PUK."));
    let _fake = fake::set(&c.app, json!({"sim_states": "wait puk", "pinnumber": "0", "puknumber": "1"}));
    let r = get_with(&c, PUK, peer(152, 5), &cookie).await;
    let html = r.text();
    for want in [
        "placeholder=\"PUK Code\"", "<p class=\"foot\">1 attempt remaining</p>",
        "This is the last attempt: a wrong PUK blocks the SIM for good.", "<h2>New PIN</h2>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let body = format!("csrf={csrf}&once={}&confirm=1&left={}&puk=12345678&new=4321&new2=4321", r.input("once"), r.input("left"));
    assert!(to(&post(&c, PUK, peer(152, 5), &cookie, &body).await).starts_with("/ui/router/sim?r="));
    assert_eq!(calls(&c)[2], ("/api/sim/pin/verify".to_owned(), json!({"pin_num": "4321", "puk_num": "12345678", "pin_encode_flag": "0"})));
    // The same post again: where the first one led, nothing sent again.
    assert!(to(&post(&c, PUK, peer(152, 6), &cookie, &body).await).starts_with("/ui/router/sim?r="));
    assert_eq!(calls(&c).len(), 3);
    // No PUK attempts left, or a count that cannot be read: nothing to do.
    let _fake = fake::set(&c.app, json!({"sim_states": "wait puk", "pinnumber": "0", "puknumber": "0"}));
    assert!(get_with(&c, PUK, peer(152, 7), &cookie).await.text().contains("No PUK attempts are left."));
    let _fake = fake::set(&c.app, json!({"sim_states": "wait puk", "pinnumber": "0"}));
    let r = get_with(&c, PUK, peer(152, 7), &cookie).await;
    assert!(r.text().contains("role=\"alert\">The attempts left can't be read right now") && !r.text().contains("<form class=\"rf"), "{}", r.text());
    let token = get_with(&c, "/ui/router/cell-lock", peer(152, 7), &cookie).await.input("once");
    let r = post(&c, PUK, peer(152, 7), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&left=&puk=12345678&new=4321&new2=4321")).await;
    r.assert_secure_html(409);
    assert_eq!(calls(&c).len(), 3);
}

/// SIM Unlock: offered while attempts are left; the code is only posted.
#[tokio::test]
async fn sim_unlock_sends_the_code_once() {
    let (c, cookie, _) = signed_in(153).await;
    let _fake = fake::set(&c.app, sim("sim ready", "0", 3));
    let _dev = device::set(&c.app, device::Device { gets: vec![trials(1)], ..Default::default() });
    let r = get_with(&c, NCK, peer(153, 2), &cookie).await;
    let html = r.text();
    for want in [
        "<h1>SIM Unlock</h1>", "<input id=\"nck\" type=\"text\" name=\"nck\" placeholder=\"Unlock Code (NCK)\"",
        "<p class=\"foot\">1 attempt remaining</p>", "This is the last attempt",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let (csrf, token) = (r.input("csrf"), r.input("once"));
    // Not from the sheet (no `confirm=1`): to the sheet, nothing sent.
    post(&c, NCK, peer(153, 3), &cookie, &format!("csrf={csrf}&once={token}&nck=0000000000000042")).await.assert_see_other(NCK);
    let r = post(&c, NCK, peer(153, 3), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&left=1&nck=")).await;
    assert!(r.text().contains("Unlock code is required"));
    // The attempts changed since the sheet was drawn: nothing sent.
    let _dev = device::set(&c.app, device::Device { gets: vec![trials(2)], ..Default::default() });
    let r = post(&c, NCK, peer(153, 3), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&left=1&nck=0000000000000042")).await;
    r.assert_secure_html(409);
    assert!(r.text().contains("The attempts left have changed") && r.text().contains("<p class=\"foot\">2 attempts remaining</p>") && !r.text().contains("0000000000000042"), "{}", r.text());
    assert!(calls(&c).is_empty());
    let _dev = device::set(&c.app, device::Device { gets: vec![trials(1)], ..Default::default() });
    let r = post(&c, NCK, peer(153, 3), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&left=1&nck=0000000000000042")).await;
    let html = get_with(&c, &to(&r), peer(153, 3), &cookie).await.text();
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">SIM unlocked successfully</p>"));
    assert_eq!(calls(&c), [("/api/sim/unlock".to_owned(), json!({"nck": "0000000000000042"}))]);
    // None left: nothing to do.
    let _dev = device::set(&c.app, device::Device { gets: vec![trials(0)], ..Default::default() });
    assert!(get_with(&c, NCK, peer(153, 4), &cookie).await.text().contains("No unlock attempts are left."));
}

/// A toolkit snapshot as `GET /api/stk/session` gives it (made up).
fn stk(kind: &str, id: &str, extra: Value) -> Value {
    let mut v = json!({"type": kind, "title": "Test SIM", "text": "", "items": [], "root_menu": true, "command_id": id,
                       "busy": false, "status": "ready", "reason": "", "source": "qualcomm_ril", "supported": true,
                       "input": {"min": 0, "max": 0, "yes_no": false, "hidden": false, "digits_only": true}});
    if let (Some(v), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
        v.extend(extra.clone());
    }
    v
}

/// USSD: the code sent once (a double post lands on the same answer),
/// the answer with Reply and End Session while its session is open, the
/// app's messages; nothing in a URL but the token.
#[tokio::test]
async fn ussd_sends_once_and_keeps_the_answer_for_the_session() {
    let (c, cookie, _) = signed_in(154).await;
    let _fake = fake::set(&c.app, stk("menu", "1", json!({"items": [{"id": 1, "label": "Balance"}]})));
    let _dev = device::set(&c.app, device::Device {
        gets: vec![("/api/stk/menu", Ok(Value::Null))],
        replies: vec![("/api/ussd/send", (200, json!({"ok": true, "data": {"response": "Balance 5.00. 1 Plans 2 Exit", "raw_response": "x", "status": 1, "dcs": 15, "session_active": true}})))],
        ..Default::default()
    });
    let r = get_with(&c, SVC, peer(154, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>SIM Services</h1>", "<h2>USSD</h2>", "placeholder=\"USSD code (e.g. *100#)\" inputmode=\"tel\"",
        "Send carrier service codes to check balance, data plans, etc.", "<button class=\"send\" type=\"submit\" aria-label=\"Send\">",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert_eq!(html.matches("name=\"once\" value=\"").count(), 2);
    let (csrf, token) = (r.input("csrf"), r.input("once"));
    let r = post(&c, &format!("{SVC}/ussd/send"), peer(154, 3), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&code=")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Enter a USSD code</p>"));
    let body = format!("csrf={csrf}&once={token}&confirm=1&code=*100%23");
    let send = format!("{SVC}/ussd/send");
    let (a, b) = tokio::join!(post(&c, &send, peer(154, 3), &cookie, &body), post(&c, &send, peer(154, 3), &cookie, &body));
    let want = format!("{SVC}?u={token}");
    assert_eq!((to(&a), to(&b)), (want.clone(), want.clone()));
    assert_eq!(calls(&c), [("/api/ussd/send".to_owned(), json!({"code": "*100#"}))]);
    let r = get_with(&c, &want, peer(154, 4), &cookie).await;
    let html = r.text();
    for want in [
        "value=\"*100#\"", "<h2>Response</h2>", "<span class=\"tx\">Balance 5.00. 1 Plans 2 Exit</span>",
        "action=\"/ui/router/sim-services/ussd/reply\"", "placeholder=\"Reply\"", "<span>End Session</span>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // Another session never sees it.
    let other = format!("zte_ui_session={}", login(&c, 154, 9).await);
    assert!(!get_with(&c, &want, peer(154, 9), &other).await.text().contains("Balance 5.00"));
    // Reply, then End Session: the app's line, the answer gone.
    let _dev = device::set(&c.app, device::Device {
        gets: vec![("/api/stk/menu", Ok(Value::Null))],
        replies: vec![("/api/ussd/respond", (200, json!({"ok": true, "data": {"response": "Plan A", "session_active": false}})))],
        ..Default::default()
    });
    let body = format!("csrf={csrf}&once={}&confirm=1&u={token}&code=*100%23&reply=1", r.input("once"));
    let at = to(&post(&c, &format!("{SVC}/ussd/reply"), peer(154, 5), &cookie, &body).await);
    assert_eq!(calls(&c), [("/api/ussd/respond".to_owned(), json!({"reply": "1"}))]);
    let html = get_with(&c, &at, peer(154, 5), &cookie).await.text();
    assert!(html.contains("<span class=\"tx\">Plan A</span>") && !html.contains("ussd/reply") && html.contains("value=\"*100#\""), "{html}");
    let token = get_with(&c, &want, peer(154, 6), &cookie).await.input("once");
    let body = format!("csrf={csrf}&once={token}&confirm=1&u={}&code=*100%23", want.rsplit('=').next().unwrap());
    let html = get_with(&c, &to(&post(&c, &format!("{SVC}/ussd/cancel"), peer(154, 6), &cookie, &body).await), peer(154, 6), &cookie).await.text();
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">USSD session ended</p>") && !html.contains("<h2>Response</h2>"), "{html}");
    assert_eq!(calls(&c)[1], ("/api/ussd/cancel".to_owned(), json!({})));
    // A code with other characters: refused, nothing sent.
    let token = get_with(&c, SVC, peer(154, 7), &cookie).await.input("once");
    let r = post(&c, &format!("{SVC}/ussd/send"), peer(154, 7), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&code=*100%23%3Breboot")).await;
    assert!(r.text().contains("A USSD code has only digits, *, # and +.") && calls(&c).len() == 2);
    // A SIM code carrying PINs: refused, never sent or shown again.
    let r = post(&c, &format!("{SVC}/ussd/send"), peer(154, 7), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&code=**04*1234*5678*5678%23")).await;
    r.assert_secure_html(400);
    let html = r.text();
    // (With their delimiters: the page's random hex tokens may hold the digits.)
    assert!(html.contains("Use Router \u{203a} SIM Card to change the PIN or enter the PUK.") && !html.contains("*1234") && !html.contains("5678*") && !html.contains("*5678"), "{html}");
    assert_eq!(calls(&c).len(), 2);
    // A send that fails keeps the answer shown (the app's); answers are
    // shown as they are (a voucher number whole).
    let _dev = device::set(&c.app, device::Device {
        gets: vec![("/api/stk/menu", Ok(Value::Null))],
        replies: vec![("/api/ussd/send", (200, json!({"ok": true, "data": {"response": "Voucher 00000000000000042", "session_active": false}})))],
        ..Default::default()
    });
    let token = get_with(&c, SVC, peer(154, 8), &cookie).await.input("once");
    let shown = to(&post(&c, &send, peer(154, 8), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&code=*100%23")).await);
    let page = get_with(&c, &shown, peer(154, 8), &cookie).await;
    assert!(page.text().contains("<span class=\"tx\">Voucher 00000000000000042</span>") && page.text().contains(&format!("name=\"u\" value=\"{token}\"")), "{}", page.text());
    let _dev = device::set(&c.app, device::Device {
        gets: vec![("/api/stk/menu", Ok(Value::Null))],
        replies: vec![("/api/ussd/send", (503, json!({"ok": false, "error": "network busy"})))],
        ..Default::default()
    });
    let r = post(&c, &send, peer(154, 8), &cookie, &format!("csrf={csrf}&once={}&confirm=1&u={token}&code=*101%23", page.input("once"))).await;
    let html = get_with(&c, &to(&r), peer(154, 8), &cookie).await.text();
    assert!(html.contains("role=\"alert\">Failed: network busy</p>") && html.contains("Voucher 00000000000000042") && html.contains("value=\"*101#\""), "{html}");
    // Logging out drops the session's answers.
    let me = c.app.auth.authenticate(crate::auth::Kind::Browser, cookie.trim_start_matches("zte_ui_session=")).unwrap().id;
    assert!(c.ussd.get(&token, me, c.app.auth.now()).is_some());
    let csrf_out = get_with(&c, "/ui/logout", peer(154, 8), &cookie).await.input("csrf");
    post(&c, "/ui/logout", peer(154, 8), &cookie, &format!("csrf={csrf_out}")).await.assert_see_other("/ui/login");
    assert!(c.ussd.get(&token, me, c.app.auth.now()).is_none());
}

/// The SIM Toolkit: the page opens the session (`/api/stk/menu`), then
/// follows the `stk` topic; items are buttons of `#stkf` with the
/// command; a selection is sent once with the app's body; a failure
/// shows its line and does not reconnect (`held=1`); a live change
/// patches the section; a text input is the page's own form.
#[tokio::test]
async fn sim_toolkit_selects_once_and_follows_the_worker() {
    let (c, cookie, _) = signed_in(155).await;
    let menu = stk("menu", "1700000000001", json!({"items": [{"id": 1, "label": "Balance"}, {"id": 2, "label": "Plans"}]}));
    let _fake = fake::set(&c.app, menu);
    let _dev = device::set(&c.app, device::Device { gets: vec![("/api/stk/menu", Err("menu read".into()))], ..Default::default() });
    let r = get_with(&c, SVC, peer(155, 2), &cookie).await;
    let html = r.text();
    for want in [
        "<section class=\"group\" id=\"stk\"><h2>Test SIM</h2>",
        "<button class=\"it bt\" type=\"submit\" form=\"stkf\" formaction=\"/ui/router/sim-services/stk/select\" name=\"c\" value=\"1700000000001.1\">Balance</button>",
        "<form id=\"stkf\" method=\"post\" action=\"/ui/router/sim-services/stk/select\"><input type=\"hidden\" name=\"csrf\"",
        "<p class=\"foot\">Carrier-provided services from your SIM card</p>", "@get('/ui/live/simservices/3179fcad002/",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("http-equiv"), "not busy: no reload");
    let (csrf, token) = (r.input("csrf"), r.input("once"));
    let body = format!("csrf={csrf}&once={token}&confirm=1&c=1700000000001.2");
    let select = format!("{SVC}/stk/select");
    let (a, b) = tokio::join!(post(&c, &select, peer(155, 3), &cookie, &body), post(&c, &select, peer(155, 3), &cookie, &body));
    a.assert_see_other(SVC);
    b.assert_see_other(SVC);
    assert_eq!(calls(&c), [("/api/stk/select".to_owned(), json!({"command_id": "1700000000001", "item_id": 2}))]);
    // The SIM works: dimmed buttons, and a reload without JavaScript.
    let _fake = fake::set(&c.app, stk("status", "1700000000002", json!({"busy": true, "reason": "Waiting for the SIM", "status": "waiting"})));
    let html = get_with(&c, SVC, peer(155, 4), &cookie).await.text();
    let reload = "<noscript><meta http-equiv=\"refresh\" content=\"3; url=/ui/router/sim-services?t=";
    assert!(html.contains("Waiting for the SIM") && html.contains(reload), "{html}");
    // A submenu arrives live: the section is patched with its buttons.
    let resp = open_live(&c, &live_url(&html, "simservices"), peer(155, 5), &cookie).await;
    assert_eq!(c.app.hub.demand(Topic::Stk), 1);
    let mut s = resp.into_body().into_data_stream();
    assert!(frame(&mut s).await.unwrap().contains("Waiting for the SIM"));
    let sub = stk("menu", "1700000000003", json!({"root_menu": false, "items": [{"id": 5, "label": "1 GB"}]}));
    c.app.hub.offer(Topic::Stk, ReadOut { value: Some(sub), error: None }, std::time::Instant::now());
    let next = frame(&mut s).await.unwrap();
    for want in ["value=\"1700000000003.5\">1 GB</button>", "formaction=\"/ui/router/sim-services/stk/back\" name=\"c\" value=\"1700000000003\">Back</button>", "End SIM Session"] {
        assert!(next.contains(want), "{want} in {next}");
    }
    assert!(!next.contains("<form") && !next.contains("<input") && !next.contains("csrf"));
    drop(s);
    // A failure: its line, and the page does not reconnect (`held=1`).
    let _dev = device::set(&c.app, device::Device {
        replies: vec![("/api/stk/back", (409, json!({"ok": false, "error": "stale or missing command_id; reload the SIM menu"})))],
        gets: vec![("/api/stk/menu", Err("must not be read".into()))],
        ..Default::default()
    });
    let token = get_with(&c, SVC, peer(155, 6), &cookie).await.input("once");
    let r = post(&c, &format!("{SVC}/stk/back"), peer(155, 6), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&c=1700000000003")).await;
    let at = to(&r);
    assert!(at.starts_with("/ui/router/sim-services?held=1&r="), "{at}");
    let html = get_with(&c, &at, peer(155, 6), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed: stale or missing command_id; reload the SIM menu</p>"), "{html}");
    assert_eq!(calls(&c), [("/api/stk/back".to_owned(), json!({"command_id": "1700000000003"}))]);
    // The app's failure state: the menu cleared (not the worker's, which
    // a second tap could repeat), only Reload SIM Menu, no stream or reload.
    let _fake = fake::set(&c.app, stk("menu", "1700000000003", json!({"root_menu": false, "items": [{"id": 5, "label": "1 GB"}]})));
    let html = get_with(&c, &at, peer(155, 6), &cookie).await.text();
    for want in [
        "<section class=\"group\" id=\"stk\"><h2>SIM Toolkit</h2><ul class=\"list\"><li class=\"it\"><span class=\"muted\">Reload the SIM menu to reconnect.</span></li></ul></section>",
        "<a class=\"it bt\" href=\"/ui/router/sim-services\">Reload SIM Menu</a>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("1 GB") && !html.contains("End SIM Session") && !html.contains("/ui/live/") && !html.contains("http-equiv"), "{html}");
    // A secret text the SIM asks for: the page's form, never filled; the
    // text only in the post.
    let ask = stk("input", "1700000000004", json!({"text": "Enter code", "root_menu": false, "input": {"min": 4, "max": 8, "hidden": true, "digits_only": true, "yes_no": false}}));
    let _fake = fake::set(&c.app, ask);
    let _dev = device::set(&c.app, device::Device::default());
    let r = get_with(&c, SVC, peer(155, 7), &cookie).await;
    let html = r.text();
    for want in [
        "<section class=\"group own\" id=\"stk\">", "<form class=\"rf stkin\" method=\"post\" action=\"/ui/router/sim-services/stk/respond\">",
        "<input type=\"hidden\" name=\"c\" value=\"1700000000004\">", "<input id=\"stkr\" type=\"password\" name=\"text\" placeholder=\"SIM response\" maxlength=\"8\" inputmode=\"tel\" autocomplete=\"one-time-code\"",
        "<small>Enter 4–8 characters</small>", "<button class=\"it cv snd\" type=\"submit\">Send Response</button>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let r = post(&c, &format!("{SVC}/stk/respond"), peer(155, 7), &cookie, &format!("csrf={csrf}&once={}&confirm=1&c=1700000000004&text=4242", r.input("once"))).await;
    r.assert_see_other(SVC);
    assert_eq!(calls(&c), [("/api/stk/respond".to_owned(), json!({"command_id": "1700000000004", "text": "4242"}))]);
    // A value no page sends: 400, nothing sent.
    post(&c, &format!("{SVC}/stk/select"), peer(155, 8), &cookie, &format!("csrf={csrf}&once={token}&confirm=1&c=17.999")).await.assert_secure_html(400);
    assert_eq!(calls(&c).len(), 1);
}

/// Every SIM post: refused without a session (back to its page's login),
/// without the CSRF token or cross-site (403); nothing sent.
#[tokio::test]
async fn sim_posts_check_session_and_csrf() {
    let (c, cookie, _) = signed_in(156).await;
    let _dev = device::set(&c.app, device::Device::default());
    let csrf = get_with(&c, "/ui/logout", peer(156, 2), &cookie).await.input("csrf");
    for (path, page) in [
        (PIN, PIN), (LOCK_ON, LOCK_ON), (LOCK_OFF, LOCK_OFF), (CHANGE, CHANGE), (PUK, PUK), (NCK, NCK),
        ("/ui/router/sim-services/ussd/send", SVC), ("/ui/router/sim-services/ussd/reply", SVC), ("/ui/router/sim-services/ussd/cancel", SVC),
        ("/ui/router/sim-services/stk/select", SVC), ("/ui/router/sim-services/stk/respond", SVC), ("/ui/router/sim-services/stk/back", SVC),
        ("/ui/router/sim-services/stk/cancel", SVC),
    ] {
        post(&c, path, peer(156, 3), "", &format!("csrf={csrf}&pin=1234")).await.assert_see_other(&format!("/ui/login?next={}&expired=1", enc(page)));
        post(&c, path, peer(156, 3), &cookie, "pin=1234").await.assert_secure_html(403);
        let body = format!("csrf={csrf}&pin=1234");
        let r = form(path, peer(156, 3), &cookie, &body).header("origin", "http://evil.example").body(Body::from(body.clone())).unwrap();
        send(&c, r).await.assert_secure_html(403);
    }
    assert!(calls(&c).is_empty());
}
