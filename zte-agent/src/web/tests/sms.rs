//! The SMS tab through the whole router: Messages and its search, a
//! conversation, opening (mark read), replying, New Message and deleting,
//! against the fake SMS device of each test's `AppState`
//! (`web::sms::fake`). Every number and text is made up (+1 555 555 01xx,
//! the fictional range).

use serde_json::json;

use super::*;
use crate::events::Topic;
use crate::web::pages::sms::digest;
use crate::web::sms::{encode_ucs2, fake, normalize};
use crate::web::store::Token;

const SEND: &str = "/ui/sms/send";
const OPEN: &str = "/ui/sms/open";
const NEW: &str = "/ui/sms/new";
const DEL: &str = "/ui/sms/delete";
/// Fixture numbers: never in a URL, a title, a fragment id or a stream.
const NUMBERS: [&str; 4] = ["5555550100", "5555550101", "5555550102", "EXAMPLE-SHOP"];

fn item(id: u64, number: &str, text: &str, date: &str, tag: &str) -> Value {
    json!({"id": id, "number": number, "content": encode_ucs2(text), "date": date, "tag": tag})
}

/// Three conversations in the internal store (one unread, one read, an
/// alphanumeric sender with markup in its text) and one on the SIM.
fn device() -> fake::Device {
    let mut d = fake::Device::with(json!([
        item(12, "+15555550100", "See you at 5", "24,03,02,10,00,00,+0", "1"),
        item(11, "5555550100", "Earlier hello", "24,03,01,09,00,00,+0", "0"),
        item(10, "+15555550101", "Fake code 000000", "24,02,01,08,00,00,+0", "0"),
        item(9, "EXAMPLE-SHOP", "Sale <b>now</b>", "24,01,01,08,00,00,+0", "1"),
        item(8, "+15555550100", "Mine", "24,03,01,09,30,00,+0", "2"),
    ]));
    d.sim = Ok(json!({"messages": [item(3, "+15555550102", "From the SIM card", "24,01,02,08,00,00,+0", "1")]}));
    d.capacity = Ok(json!({"sms_nv_total": "100", "sms_nvused_total": "4"}));
    d
}

fn key(c: &Ctx, number: &str) -> String {
    c.app.auth.opaque_key("sms", &normalize(number)).unwrap()
}

fn calls(c: &Ctx) -> Vec<(&'static str, Value)> {
    fake::with(&c.app, |d| d.calls.clone()).unwrap()
}

/// No fixture number in any attribute, in `<title>`, or in a location.
fn assert_no_numbers_in_urls(html: &str) {
    let title = &html[html.find("<title>").unwrap()..html.find("</title>").unwrap()];
    for attr in ["href=\"", "action=\"", "id=\"", "data-init=\"", "name=\"t\" value=\"", "form=\""] {
        for v in html.split(attr).skip(1).map(|s| s.split('"').next().unwrap()) {
            assert!(!NUMBERS.iter().any(|n| v.contains(n)), "{attr}{v}");
        }
    }
    assert!(!NUMBERS.iter().any(|n| title.contains(n)), "{title}");
}

/// Search Messages (posted, so the text is in no URL).
async fn search(c: &Ctx, cookie: &str, csrf: &str, store: &str, q: &str) -> Reply {
    post(c, &format!("/ui/sms{store}"), peer(60, 4), cookie, &format!("csrf={csrf}&q={q}")).await
}

/// Messages (`SMSListView`): "All" merges both stores, newest first; an
/// unread row is a POST form (opening marks it read), a read row a link,
/// both by an opaque key; the storage filter, the storage row (strings
/// too), the search over numbers and texts (posted: never in a URL), the
/// header (always), the empty and error states.
#[tokio::test]
async fn messages_follow_the_app() {
    let (c, cookie, _) = signed_in(60).await;
    let _dev = fake::set(&c.app, device());
    let r = get_with(&c, "/ui/sms", peer(60, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert!(html.contains("<title>Messages · U60 Pro</title>"));
    let (k0, k1, k2, shop) = (key(&c, "+15555550100"), key(&c, "+15555550101"), key(&c, "+15555550102"), key(&c, "EXAMPLE-SHOP"));
    for want in [
        "<p class=\"snap\" id=\"live-status\">Snapshot from page load",
        "<a href=\"/ui/sms\">Refresh</a>",
        "<div id=\"sms-new\"></div>",
        "<form class=\"srch\" id=\"srch\" method=\"post\" action=\"/ui/sms\" role=\"search\"><input type=\"hidden\" name=\"csrf\"",
        "<input type=\"search\" name=\"q\" value=\"\" placeholder=\"Search messages\" aria-label=\"Search messages\" autocomplete=\"off\"",
        "<nav class=\"seg\" aria-label=\"Storage\"><a href=\"/ui/sms\" aria-current=\"true\">All</a>\
         <a href=\"/ui/sms?store=internal\">Internal</a><a href=\"/ui/sms?store=sim\">SIM</a></nav>",
        "<li class=\"it\"><span class=\"muted\">Storage</span><span class=\"tm\">4/100</span></li>",
        "<h2>4 Conversations</h2>",
        // Unread: bold (`ur`), the count with "unread" for screen readers, a POST.
        "<form class=\"sf\" method=\"post\" action=\"/ui/sms/open\"><input type=\"hidden\" name=\"csrf\"",
        &format!("<input type=\"hidden\" name=\"t\" value=\"{k0}\"><button class=\"it cv ur\" type=\"submit\">"),
        "<span class=\"nm\">+15555550100</span>",
        "<span class=\"pv\">See you at 5</span><span class=\"ub\">1<span class=\"sr\"> unread</span></span>",
        // Read: a link, no count.
        &format!("<a class=\"it cv\" href=\"/ui/sms/t/{k1}#end\">"),
        "<span class=\"pv\">Fake code 000000</span></span></span></a>",
        "<span class=\"pv\">Sale &lt;b&gt;now&lt;/b&gt;</span>",
        // A screen reader's link list names each row's "Delete".
        &format!("<a class=\"del\" href=\"/ui/sms/t/{k1}/delete\">Delete<span class=\"sr\"> conversation with +15555550101</span></a>"),
        "Calls are only in the OpenU60 app.",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let csrf = r.input("csrf");
    // Newest first, the SIM's message merged in.
    let at = |k: &str| html.find(&format!("value=\"{k}\"")).or_else(|| html.find(&format!("/ui/sms/t/{k}#"))).unwrap();
    assert!(at(&k0) < at(&k1) && at(&k1) < at(&k2) && at(&k2) < at(&shop), "{html}");
    assert_no_numbers_in_urls(&html);
    assert!(calls(&c).is_empty(), "the list reads only");
    // Its stream carries the arrivals counter, as hex.
    assert!(html.contains("@get('/ui/live/sms/0/"), "{html}");

    // Storage filter: the SIM alone, the internal store alone.
    let sim = get_with(&c, "/ui/sms?store=sim", peer(60, 3), &cookie).await.text();
    assert!(sim.contains("<h2>1 Conversations</h2>") && sim.contains("<a href=\"/ui/sms?store=sim\" aria-current=\"true\">SIM</a>"));
    assert!(sim.contains("<input type=\"hidden\" name=\"store\" value=\"sim\">") && sim.contains("From the SIM card"));
    let internal = get_with(&c, "/ui/sms?store=internal", peer(60, 3), &cookie).await.text();
    assert!(internal.contains("<h2>3 Conversations</h2>") && !internal.contains("From the SIM card"));
    // An unknown store is All.
    assert!(get_with(&c, "/ui/sms?store=2", peer(60, 3), &cookie).await.text().contains("<h2>4 Conversations</h2>"));

    // A search in the URL is ignored (it would be in the history).
    let html = get_with(&c, "/ui/sms?q=hello", peer(60, 3), &cookie).await.text();
    assert!(html.contains("<h2>4 Conversations</h2>") && html.contains("name=\"q\" value=\"\""), "{html}");
    assert!(html.contains("<button class=\"btn sm\" type=\"submit\">Search</button></form>"), "no Cancel without a search");

    // Search (posted): any message of a conversation, or its number; case-insensitive.
    for (q, n, want) in [
        ("hello", 1, "+15555550100"),
        ("MINE", 1, "+15555550100"),
        ("550101", 1, "+15555550101"),
        ("shop", 1, "EXAMPLE-SHOP"),
        ("%20fake%20", 1, "+15555550101"),
        ("%20", 4, "EXAMPLE-SHOP"),
    ] {
        let r = search(&c, &cookie, &csrf, "", q).await;
        r.assert_secure_html(200);
        let html = r.text();
        assert!(html.contains(&format!("<h2>{n} Conversations</h2>")) && html.contains(want), "{q}: {html}");
    }
    // The search is in no URL: the store items post the search form to
    // their store's page; the refresh link, rows and stream carry none.
    let r = search(&c, &cookie, &csrf, "?store=sim", "5555550102").await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<form class=\"srch\" id=\"srch\" method=\"post\" action=\"/ui/sms?store=sim\" role=\"search\">",
        "<input type=\"search\" name=\"q\" value=\"5555550102\"",
        // The app's Cancel: the store's whole list.
        "<button class=\"btn sm\" type=\"submit\">Search</button><a class=\"nl\" href=\"/ui/sms?store=sim\">Cancel</a></form>",
        "<nav class=\"seg\" aria-label=\"Storage\"><button form=\"srch\" formaction=\"/ui/sms\">All</button>\
         <button form=\"srch\" formaction=\"/ui/sms?store=internal\">Internal</button>\
         <button form=\"srch\" formaction=\"/ui/sms?store=sim\" aria-current=\"true\">SIM</button></nav>",
        "<a href=\"/ui/sms?store=sim\">Refresh</a>",
        &format!("<input type=\"hidden\" name=\"t\" value=\"{k2}\">"),
        "<h2>1 Conversations</h2>",
        "@get('/ui/live/sms/0/",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert_no_numbers_in_urls(&html);
    // No hits: the app's header with 0, then its empty state.
    let none = search(&c, &cookie, &csrf, "", "%3Cnothing%3E").await.text();
    assert!(none.contains("<h2>0 Conversations</h2>") && !none.contains("No Messages"), "{none}");
    assert!(none.contains("<strong>No Results for “&lt;nothing&gt;”</strong><p>Check the spelling or try a new search.</p>"), "{none}");
    // Refused: no CSRF token, another origin; signed out, the login leads to Messages.
    post(&c, "/ui/sms", peer(60, 5), &cookie, "q=hello").await.assert_secure_html(403);
    let b = format!("csrf={csrf}&q=hello");
    let evil = form("/ui/sms", peer(60, 5), &cookie, &b).header("origin", "http://evil.example").body(Body::from(b.clone())).unwrap();
    send(&c, evil).await.assert_secure_html(403);
    post(&c, "/ui/sms", peer(60, 5), "", "q=5555550102").await.assert_see_other("/ui/login?next=%2Fui%2Fsms&expired=1");
    // Signed out, the way back never carries a search typed into the URL.
    get(&c, "/ui/sms?q=5555550102&store=sim", peer(60, 5)).await.assert_see_other("/ui/login?next=%2Fui%2Fsms%3Fstore%3Dsim");
    assert!(calls(&c).is_empty(), "a search reads only");

    // Empty: the app's empty state; capacity unknown: no storage row.
    let mut empty = fake::Device::with(json!([]));
    empty.capacity = Err("ubus timeout".into());
    let _dev = fake::set(&c.app, empty.clone());
    let html = get_with(&c, "/ui/sms", peer(60, 6), &cookie).await.text();
    assert!(html.contains("<h2>0 Conversations</h2><ul class=\"list\"></ul></section><div class=\"empty\">"), "{html}");
    assert!(html.contains("<strong>No Messages</strong><p>SMS messages will appear here</p>"), "{html}");
    assert!(!html.contains(">Storage</span>") && !html.contains("msg err"));
    // One store failing: the other's messages and a note; both: the error, never "No Messages".
    let _dev = fake::set(&c.app, fake::Device { sim: Err("SIM busy".into()), ..device() });
    let html = get_with(&c, "/ui/sms", peer(60, 6), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Couldn't read SIM messages: SIM busy</p>") && html.contains("<h2>3 Conversations</h2>"), "{html}");
    let _dev = fake::set(&c.app, fake::Device { internal: Err("ubus <timeout>".into()), sim: Err("SIM busy".into()), ..empty });
    let r = get_with(&c, "/ui/sms", peer(60, 6), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed: ubus &lt;timeout&gt;</p>") && !html.contains("No Messages"), "{html}");
    assert!(html.contains("<h2>0 Conversations</h2>"));
}

/// The new-message notice: the page's stream starts from the arrivals at
/// page load (in its URL) and says "New message" for later ones; its links
/// reload the page they are on, and nothing in it names anyone.
#[tokio::test]
async fn messages_stream_a_new_message_notice() {
    let (c, cookie, _) = signed_in(61).await;
    let _dev = fake::set(&c.app, device());
    let sms_event = || c.app.hub.on_bus_event("zwrt_wms_status_event", &json!({"wms_status": "new_sms_received"}));
    sms_event();
    let page = get_with(&c, "/ui/sms", peer(61, 2), &cookie).await.text();
    assert!(page.contains("@get('/ui/live/sms/1/") && page.contains("<div id=\"sms-new\"></div>"), "{page}");
    let resp = open_live(&c, &live_url(&page, "sms"), peer(61, 3), &cookie).await;
    assert!(Topic::ALL.iter().all(|t| (c.app.hub.demand(*t) > 0) == (*t == Topic::Sms)));
    let mut b = resp.into_body().into_data_stream();
    let first = frame(&mut b).await.unwrap();
    assert!(first.contains("<div id=\"sms-new\"></div>"), "{first}");
    assert!(first.contains("<span class=\"lost\"> No updates for 15 s · <a href=\"\">Refresh</a>"), "{first}");
    sms_event();
    let mut seen = String::new();
    for _ in 0..4 {
        seen.push_str(&frame(&mut b).await.unwrap());
        if seen.contains("New message") {
            break;
        }
    }
    assert!(seen.contains("<div id=\"sms-new\"><p class=\"msg info\" role=\"status\">New message · <a href=\"\">Refresh</a></p></div>"), "{seen}");
    sms_event();
    let mut seen = String::new();
    for _ in 0..4 {
        seen.push_str(&frame(&mut b).await.unwrap());
        if seen.contains("2 new messages") {
            break;
        }
    }
    assert!(seen.contains("2 new messages · <a href=\"\">Refresh</a>"), "{seen}");
    assert!(!seen.contains("<form") && !NUMBERS.iter().any(|n| seen.contains(n)));
    drop(b);
    // The counter is required, as hex; a conversation's page uses the same stream.
    for uri in [format!("/ui/live/sms/{KEY}"), format!("/ui/live/sms/1x/{KEY}"), format!("/ui/live/sms/{KEY}0/{KEY}")] {
        assert_eq!(open_live(&c, &uri, peer(61, 4), &cookie).await.status(), 404, "{uri}");
    }
    let thread = get_with(&c, &format!("/ui/sms/t/{}", key(&c, "+15555550101")), peer(61, 5), &cookie).await.text();
    assert!(thread.contains("@get('/ui/live/sms/3/") && thread.contains("<div id=\"sms-new\"></div>"), "{thread}");
}

/// A conversation (`SMSConversationView`): by its opaque key, titled
/// "Conversation" (the number only in the page), bubbles by tag; a GET
/// never marks anything read. Opening an unread one from the list marks
/// exactly its unread messages read (`tag` 0), then shows it.
#[tokio::test]
async fn opening_marks_read_and_a_conversation_page_never_does() {
    let (c, cookie, _) = signed_in(62).await;
    let mut d = device();
    d.internal = Ok(json!({"messages": [
        item(12, "+15555550100", "See you at 5", "24,03,02,10,00,00,+0", "1"),
        item(11, "5555550100", "Earlier hello", "24,03,01,09,00,00,+0", "1"),
        item(8, "+15555550100", "Mine", "24,03,01,09,30,00,+0", "2"),
        item(7, "+15555550100", "Lost", "24,03,01,09,40,00,+0", "3"),
        item(6, "+15555550100", "Draft", "24,03,01,09,50,00,+0", "4"),
        item(10, "+15555550101", "Fake code 000000", "24,02,01,08,00,00,+0", "0"),
    ]}));
    let _dev = fake::set(&c.app, d);
    let k0 = key(&c, "+15555550100");
    let r = get_with(&c, &format!("/ui/sms/t/{k0}"), peer(62, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<title>Conversation · U60 Pro</title>",
        "<div class=\"lead\"><a class=\"nl back\" href=\"/ui/sms\">Messages</a></div><h1>+15555550100</h1>",
        &format!("<a href=\"/ui/sms/t/{k0}#end\">Refresh</a>"),
        "<li class=\"b in\"><p>Earlier hello</p>",
        "<li class=\"b out\"><p>Mine</p>",
        "<li class=\"b out failed\"><p>Lost</p><small><span class=\"nd\" role=\"img\" aria-label=\"Not delivered\">",
        "<li class=\"b out draft\"><p>Draft</p>",
        "<li class=\"b in\"><p>See you at 5</p>",
        "<form class=\"mr\" method=\"post\" action=\"/ui/sms/open\">",
        // The bubbles scroll in a box that starts at its end (the newest).
        "<div class=\"sc\"><div><ol class=\"bb\"><li class=\"b in\"><p>Earlier hello</p>",
        // The reply bar: the notice, then the form, which lands its answer on itself.
        "</ol></div></div><div id=\"end\"><div id=\"sms-new\"></div><form class=\"reply\" method=\"post\" action=\"/ui/sms/send#end\">",
        "<input type=\"hidden\" name=\"once\" value=\"",
        "\" autocomplete=\"off\"><input type=\"hidden\" name=\"t\"",
        "<textarea name=\"text\" rows=\"1\" placeholder=\"Message\" aria-label=\"Message\" maxlength=\"765\" required></textarea>",
        "<button class=\"send\" type=\"submit\" aria-label=\"Send\">",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // Times in the router's own zone at each message's date (summer and
    // winter differ in a zone with DST), the date on hover.
    for date in ["24,03,02,10,00,00,+0", "24,03,01,09,00,00,+0"] {
        let t = crate::web::sms::parse_date(date).unwrap();
        let off = crate::web::sms::local_offset(t);
        let time = format!("<span title=\"{}\">{}</span>", crate::web::sms::day(t, off), crate::web::sms::clock(t, off));
        assert!(html.contains(&time), "{time} in {html}");
    }
    // Oldest first.
    let at = |w: &str| html.find(w).unwrap();
    assert!(at("Earlier hello") < at("Mine") && at("Mine") < at("Lost") && at("Draft") < at("See you at 5"));
    assert_no_numbers_in_urls(&html);
    assert!(calls(&c).is_empty(), "a GET marks nothing read");
    // Opening it: its two unread messages, then the conversation.
    let csrf = r.input("csrf");
    post(&c, OPEN, peer(62, 3), &cookie, &format!("csrf={csrf}&t={k0}")).await.assert_see_other(&format!("/ui/sms/t/{k0}#end"));
    assert_eq!(calls(&c), [("/api/sms/read", json!({"id": "11;12;", "tag": 0}))]);
    // Nothing unread: nothing sent; the SIM's conversation in its store.
    let k1 = key(&c, "+15555550101");
    post(&c, OPEN, peer(62, 3), &cookie, &format!("csrf={csrf}&t={k1}")).await.assert_see_other(&format!("/ui/sms/t/{k1}#end"));
    let k2 = key(&c, "+15555550102");
    post(&c, OPEN, peer(62, 3), &cookie, &format!("csrf={csrf}&t={k2}&store=sim")).await.assert_see_other(&format!("/ui/sms/t/{k2}?store=sim#end"));
    assert_eq!(calls(&c).len(), 2);
    assert_eq!(calls(&c)[1], ("/api/sms/read", json!({"id": "3;", "tag": 0})));
    // Refused: no session, no CSRF token, another origin; a bad key or store.
    post(&c, OPEN, peer(62, 4), "", &format!("csrf={csrf}&t={k0}")).await.assert_see_other("/ui/login?next=%2Fui%2Fsms&expired=1");
    post(&c, OPEN, peer(62, 4), &cookie, &format!("t={k0}")).await.assert_secure_html(403);
    let body = format!("csrf={csrf}&t={k0}");
    let evil = form(OPEN, peer(62, 4), &cookie, &body).header("origin", "http://evil.example").body(Body::from(body.clone())).unwrap();
    send(&c, evil).await.assert_secure_html(403);
    for bad in ["&t=5555550100", "&t=0123456789ABCDEF", "", &format!("&t={k0}&store=2")] {
        post(&c, OPEN, peer(62, 4), &cookie, &format!("csrf={csrf}{bad}")).await.assert_secure_html(400);
    }
    assert_eq!(calls(&c).len(), 2);
    // A key nobody has (or from before a restart): the page says so.
    let r = get_with(&c, "/ui/sms/t/0123456789abcdef", peer(62, 5), &cookie).await;
    r.assert_secure_html(404);
    assert!(r.text().contains("<strong>Conversation not found</strong>") && r.text().contains("<a class=\"btn\" href=\"/ui/sms\">Back to Messages</a>"));
    for bad in ["/ui/sms/t/5555550100", "/ui/sms/t/0123456789ABCDEF", "/ui/sms/t/", &format!("/ui/sms/t/{k0}/x")] {
        get_with(&c, bad, peer(62, 5), &cookie).await.assert_secure_html(404);
    }
    // Signed out: back to the conversation after the login (its key only).
    get(&c, &format!("/ui/sms/t/{k0}"), peer(62, 6)).await.assert_see_other(&format!("/ui/login?next=%2Fui%2Fsms%2Ft%2F{k0}"));
}

/// Long conversations: the last 100 messages, "Earlier messages" before
/// them, and "Newest messages" back.
#[tokio::test]
async fn a_long_conversation_pages_back() {
    let (c, cookie, _) = signed_in(63).await;
    let msgs: Vec<Value> = (1..=105u64).map(|i| item(i, "+15555550100", &format!("Fake {i}"), &format!("24,01,01,00,{:02},{:02},+0", i / 60, i % 60), "0")).collect();
    let _dev = fake::set(&c.app, fake::Device::with(Value::Array(msgs)));
    let k0 = key(&c, "+15555550100");
    let html = get_with(&c, &format!("/ui/sms/t/{k0}"), peer(63, 2), &cookie).await.text();
    assert_eq!(html.matches("<li class=\"b in\">").count(), 100);
    assert!(!html.contains("<p>Fake 5</p>") && html.contains("<p>Fake 6</p>") && html.contains("<p>Fake 105</p>"));
    let earlier = format!("/ui/sms/t/{k0}?before=6");
    assert!(html.contains(&format!("<p class=\"pg\"><a href=\"{earlier}\">Earlier messages</a></p>")) && !html.contains("Newest messages"));
    let html = get_with(&c, &earlier, peer(63, 2), &cookie).await.text();
    assert_eq!(html.matches("<li class=\"b in\">").count(), 5);
    assert!(html.contains(&format!("<a href=\"/ui/sms/t/{k0}#end\">Newest messages</a>")) && !html.contains("Earlier messages"));
}

/// The reply bar: one send per single-use token. A valid text claims the
/// token right before the call (the body as the app's); a replay of the
/// same send sends nothing and shows what the first post did (a real
/// double click too); the token with another text, an invalid text or an
/// unknown token sends nothing, keeps the text and leaves the token as it
/// was; a device failure keeps the text with "Failed to send: …"; a busy
/// router gives the token back (also to a request dropped while it waits)
/// and keeps the text. Every answer carries a new token.
#[tokio::test]
async fn a_reply_is_sent_once() {
    let (c, cookie, sid) = signed_in(64).await;
    let _dev = fake::set(&c.app, device());
    let k0 = key(&c, "+15555550100");
    let page = get_with(&c, &format!("/ui/sms/t/{k0}"), peer(64, 2), &cookie).await;
    let (csrf, tok) = (page.input("csrf"), page.input("once"));
    assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Fresh));
    let to = format!("/ui/sms/t/{k0}#end");
    let body = |tok: &str, text: &str| format!("csrf={csrf}&once={tok}&t={k0}&text={}", enc(text));
    // Sent: to the longest number variant, as the app's request.
    post(&c, SEND, peer(64, 3), &cookie, &body(&tok, "Hi there\r\nsecond line")).await.assert_see_other(&to);
    let sent = calls(&c);
    assert_eq!(sent.len(), 1);
    let (path, b) = &sent[0];
    assert_eq!((*path, b["number"].as_str(), b["id"].as_str(), b["encode_type"].as_str()), ("/api/sms/send", Some("+15555550100"), Some("-1"), Some("GSM7_default")));
    assert_eq!(b["message_body"], encode_ucs2("Hi there\nsecond line"));
    let time = b["sms_time"].as_str().unwrap();
    assert!(time.len() >= 20 && time.split(';').count() == 7, "{time}");
    let hi = digest(Some(&k0), "", "Hi there\nsecond line");
    assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Sent(hi)));
    // A double click or a reload: nothing is sent again.
    post(&c, SEND, peer(64, 3), &cookie, &body(&tok, "Hi there\r\nsecond line")).await.assert_see_other(&to);
    assert_eq!(calls(&c).len(), 1);
    // The used token with another text (an old copy of the form, restored
    // by the browser): an expired form with the text, never "sent".
    let r = post(&c, SEND, peer(64, 3), &cookie, &body(&tok, "Hi there")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("<span>This form expired, so the message was not sent. Send it again.</span>"), "{}", r.text());
    assert!(r.text().contains(">Hi there</textarea>") && r.input("once") != tok);
    assert_eq!((calls(&c).len(), c.once.get(&tok, sid, c.app.auth.now())), (1, Some(Token::Sent(hi))));
    // Too big a form: refused before anything is read.
    let big = body(&tok, &"a".repeat(9000));
    post(&c, SEND, peer(64, 3), &cookie, &big).await.assert_secure_html(413);
    assert_eq!(calls(&c).len(), 1);

    // Invalid text: the conversation again (400), the text kept, a new
    // token; the old one is still unused.
    let tok = get_with(&c, &format!("/ui/sms/t/{k0}"), peer(64, 2), &cookie).await.input("once");
    for (text, msg) in [("  \r\n ", "Enter a message."), (&*"a".repeat(766), "Enter at most 765 characters.")] {
        let r = post(&c, SEND, peer(64, 4), &cookie, &body(&tok, text)).await;
        r.assert_secure_html(400);
        let html = r.text();
        assert!(html.contains("<p class=\"sfail\" role=\"alert\"><svg") && html.contains(&format!("<span>{msg}</span>")), "{html}");
        assert!(r.input("once") != tok && html.contains("<title>Conversation · U60 Pro</title>"));
        assert!(!html.contains("@get("), "the answer to a post has no stream");
        assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Fresh));
    }
    let r = post(&c, SEND, peer(64, 4), &cookie, &body(&tok, &"Ж".repeat(336))).await;
    assert!(r.text().contains("Enter at most 335 characters (this text needs Unicode).") && r.text().contains(&"Ж".repeat(336)));
    assert_eq!(calls(&c).len(), 1);
    // An unknown or expired token: "This form expired", the text kept, nothing sent.
    let r = post(&c, SEND, peer(64, 4), &cookie, &body("0000000000000000", "Keep <me>")).await;
    r.assert_secure_html(400);
    let html = r.text();
    assert!(html.contains("<span>This form expired, so the message was not sent. Send it again.</span>"), "{html}");
    assert!(html.contains("maxlength=\"765\" required>Keep &lt;me&gt;</textarea>"), "{html}");
    // Another session's token is unknown to this one.
    let other = format!("zte_ui_session={}", login(&c, 64, 9).await);
    let theirs = get_with(&c, &format!("/ui/sms/t/{k0}"), peer(64, 9), &other).await.input("once");
    let r = post(&c, SEND, peer(64, 4), &cookie, &body(&theirs, "Hi")).await;
    assert!(r.status == 400 && r.text().contains("This form expired"));
    assert_eq!(calls(&c).len(), 1);

    // The device rejects it: the text kept, the app's words; a replay shows
    // the same failure and sends nothing.
    fake::with(&c.app, |d| d.reply = (200, json!({"ok": true, "data": {"result": 4}})));
    let r = post(&c, SEND, peer(64, 5), &cookie, &body(&tok, "Retry me")).await;
    r.assert_secure_html(502);
    let html = r.text();
    assert!(html.contains("<span>Failed to send: device rejected (result=4)</span>") && html.contains(">Retry me</textarea>"), "{html}");
    assert_eq!(calls(&c).len(), 2);
    let again = post(&c, SEND, peer(64, 5), &cookie, &body(&tok, "Retry me")).await;
    again.assert_secure_html(502);
    assert!(again.text().contains("<span>Failed to send: device rejected (result=4)</span>") && again.text().contains(">Retry me</textarea>"));
    assert_eq!(calls(&c).len(), 2);
    // The API route's error.
    fake::with(&c.app, |d| d.reply = (503, json!({"ok": false, "error": "ubus <down>"})));
    let r = post(&c, SEND, peer(64, 5), &cookie, &body(&r.input("once"), "Retry me")).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("<span>Failed to send: ubus &lt;down&gt;</span>"), "{}", r.text());
    assert_eq!(calls(&c).len(), 3);

    // A replay while the first post is still sending waits for its
    // outcome; unknown (the token gone): the text with a warning.
    fake::with(&c.app, |d| d.reply = (200, json!({"ok": true, "data": {"result": 3}})));
    let twice = digest(Some(&k0), "", "Twice");
    for (outcome, want) in [
        (Some(Token::Failed(twice, 502, "Failed to send: device rejected (result=1)".into())), Some((502, "<span>Failed to send: device rejected (result=1)</span>"))),
        (Some(Token::Fresh), Some((503, "<span>Message not sent</span>"))),
        (None, Some((409, "<span>This message may not have been sent. Check the conversation before sending it again.</span>"))),
        (Some(Token::Sent(twice)), None),
    ] {
        let tok = get_with(&c, &format!("/ui/sms/t/{k0}"), peer(64, 6), &cookie).await.input("once");
        c.once.update(&tok, sid, c.app.auth.now(), |t| *t = Token::Sending(twice));
        let (c2, cookie2, b) = (c.clone(), cookie.clone(), body(&tok, "Twice"));
        let replay = tokio::spawn(async move { post(&c2, SEND, peer(64, 7), &cookie2, &b).await });
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!replay.is_finished(), "it waits for the first post");
        match &outcome {
            Some(t) => {
                c.once.update(&tok, sid, c.app.auth.now(), |x| *x = t.clone());
            }
            None => c.once.remove(&tok, Some(sid), c.app.auth.now()),
        }
        let r = replay.await.unwrap();
        match want {
            Some((status, w)) => {
                r.assert_secure_html(status);
                assert!(r.text().contains(w) && r.text().contains(">Twice</textarea>"), "{}", r.text());
            }
            None => r.assert_see_other(&to),
        }
        // Another text with a token that is sending: expired at once.
        if outcome.is_none() {
            let tok = get_with(&c, &format!("/ui/sms/t/{k0}"), peer(64, 6), &cookie).await.input("once");
            c.once.update(&tok, sid, c.app.auth.now(), |t| *t = Token::Sending(twice));
            let r = post(&c, SEND, peer(64, 7), &cookie, &body(&tok, "Other")).await;
            assert!(r.status == 400 && r.text().contains("This form expired") && r.text().contains(">Other</textarea>"), "{}", r.text());
        }
    }
    assert_eq!(calls(&c).len(), 3);

    // A real double click: two posts of one form while the device is
    // sending; one send, both lead to the conversation.
    fake::with(&c.app, |d| d.wait = Some(Duration::from_millis(300)));
    let tok = get_with(&c, &format!("/ui/sms/t/{k0}"), peer(64, 7), &cookie).await.input("once");
    let clicks: Vec<_> = (0..2)
        .map(|i| {
            let (c2, cookie2, b) = (c.clone(), cookie.clone(), body(&tok, "Double"));
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50 * i)).await;
                post(&c2, SEND, peer(64, 7), &cookie2, &b).await
            })
        })
        .collect();
    for click in clicks {
        click.await.unwrap().assert_see_other(&to);
    }
    assert_eq!(calls(&c).len(), 4);
    fake::with(&c.app, |d| d.wait = None);

    // The router busy: nothing sent, the token given back; the text kept
    // on a page that reads nothing (the conversation's reply bar).
    let tok = get_with(&c, &format!("/ui/sms/t/{k0}"), peer(64, 8), &cookie).await.input("once");
    let mut busy = c.clone();
    busy.gate = Arc::new(Gate::new(0, 0, Duration::from_millis(20)));
    let r = post(&busy, SEND, peer(64, 8), &cookie, &body(&tok, "Busy <b>")).await;
    r.assert_secure_html(503);
    let html = r.text();
    assert!(html.contains("<span>Failed to send: the router is busy. Try again in a few seconds.</span>"), "{html}");
    assert!(html.contains(">Busy &lt;b&gt;</textarea>") && html.contains("<h1>Conversation</h1>") && !html.contains("+15555550100"), "{html}");
    assert!(r.input("once") != tok && !html.contains("class=\"b ") && !html.contains("@get("));
    assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Fresh));
    // An invalid text then: the same page with its reason.
    let r = post(&busy, SEND, peer(64, 8), &cookie, &body(&tok, " ")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("<span>Enter a message.</span>") && r.text().contains("<h1>Conversation</h1>"));
    // A request dropped while it waits for the router: the token given back.
    let mut slow = c.clone();
    slow.gate = Arc::new(Gate::new(0, 0, Duration::from_secs(30)));
    let (cookie2, b) = (cookie.clone(), body(&tok, "Abandoned"));
    let waiting = tokio::spawn(async move { post(&slow, SEND, peer(64, 8), &cookie2, &b).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Sending(digest(Some(&k0), "", "Abandoned"))));
    waiting.abort();
    assert!(waiting.await.is_err());
    assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Fresh));
    assert_eq!(calls(&c).len(), 4);
    // The conversation gone (or the list unreadable) by then: nothing sent, the token given back.
    let _dev = fake::set(&c.app, fake::Device::with(json!([])));
    post(&c, SEND, peer(64, 8), &cookie, &body(&tok, "Gone")).await.assert_secure_html(404);
    assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Fresh));
    let _dev = fake::set(&c.app, fake::Device { internal: Err("ubus down".into()), sim: Err("ubus down".into()), ..device() });
    let r = post(&c, SEND, peer(64, 8), &cookie, &body(&tok, "Unreadable")).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("Failed: ubus down") && r.text().contains(">Unreadable</textarea>"), "{}", r.text());
    assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Fresh));
    assert!(calls(&c).is_empty());
    // A conversation opened while the list is unreadable: why, a way to
    // try again, no reply bar (nothing to reply to, no token spent).
    let r = get_with(&c, &format!("/ui/sms/t/{k0}"), peer(64, 8), &cookie).await;
    r.assert_secure_html(503);
    let html = r.text();
    assert!(html.contains(r#"<p class="msg err" role="alert">Failed: ubus down</p>"#), "{html}");
    assert!(html.contains(&format!(r#"<p class="ctr"><a class="btn" href="/ui/sms/t/{k0}#end">Try again</a></p>"#)), "{html}");
    assert!(!html.contains("class=\"reply\"") && !html.contains("name=\"once\""), "{html}");

    // Refused before anything is read: no session, no CSRF, a bad key.
    let _dev = fake::set(&c.app, device());
    post(&c, SEND, peer(64, 9), "", &body(&tok, "x")).await.assert_see_other("/ui/login?next=%2Fui%2Fsms&expired=1");
    post(&c, SEND, peer(64, 9), &cookie, &format!("once={tok}&t={k0}&text=x")).await.assert_secure_html(403);
    post(&c, SEND, peer(64, 9), &cookie, &format!("csrf={csrf}&once={tok}&t=5555550100&text=x")).await.assert_secure_html(400);
    assert!(calls(&c).is_empty());
    assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Fresh));
}

/// New Message (`SMSComposeView`): "Cancel", the number, the text with the
/// app's counter, Send. A valid post sends the app's request to the typed
/// number (without its formatting) once and leads to Messages ("Message
/// sent"). Every refusal sends nothing, keeps both values and issues a new
/// token, leaving the posted one as it was: blank or bad fields and too
/// long a text (web additions), an unknown token or a used one with other
/// values, a device failure (its replay too), a busy router (the token
/// given back). A replay whose first post's outcome is unknown never says
/// "sent".
#[tokio::test]
async fn a_new_message_is_sent_once() {
    let (c, cookie, sid) = signed_in(65).await;
    let _dev = fake::set(&c.app, device());
    let r = get_with(&c, NEW, peer(65, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<title>New Message · U60 Pro</title>",
        "<div class=\"lead\"><a class=\"nl\" href=\"/ui/sms\">Cancel</a></div><h1>New Message</h1>",
        "<form class=\"cmp\" method=\"post\" action=\"/ui/sms/send\"><input type=\"hidden\" name=\"csrf\"",
        "<input type=\"tel\" name=\"number\" value=\"\" placeholder=\"Phone Number\" aria-label=\"Phone Number\" \
         autocomplete=\"off\" maxlength=\"32\" required autofocus>",
        "<textarea name=\"text\" rows=\"3\" placeholder=\"Message\" aria-label=\"Message\" maxlength=\"765\" required>\
         </textarea></li></ul><p class=\"foot\">0/765 (0 SMS)</p>",
        "<button class=\"it cv snd\" type=\"submit\">Send</button>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("@get(") && !html.contains("sfail"));
    let (csrf, tok) = (r.input("csrf"), r.input("once"));
    assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Fresh));
    let body = |tok: &str, number: &str, text: &str| format!("csrf={csrf}&once={tok}&number={}&text={}", enc(number), enc(text));
    let sent = "/ui/sms?saved=sent";
    // Sent: to the typed number (trimmed), as the app's request; Messages says so.
    post(&c, SEND, peer(65, 3), &cookie, &body(&tok, " +15555550199 ", "Fake hello \u{1F600}")).await.assert_see_other(sent);
    let done = calls(&c);
    assert_eq!(done.len(), 1);
    let (path, b) = &done[0];
    assert_eq!((*path, b["number"].as_str(), b["id"].as_str(), b["encode_type"].as_str()), ("/api/sms/send", Some("+15555550199"), Some("-1"), Some("UNICODE")));
    assert_eq!(b["message_body"], encode_ucs2("Fake hello \u{1F600}"));
    let list = get_with(&c, sent, peer(65, 3), &cookie).await.text();
    assert!(list.contains("<p class=\"msg info\" role=\"status\">Message sent</p>"), "{list}");
    // A double click or a reload: nothing is sent again.
    post(&c, SEND, peer(65, 3), &cookie, &body(&tok, "+15555550199", "Fake hello \u{1F600}")).await.assert_see_other(sent);
    assert_eq!(calls(&c).len(), 1);
    // The used token with another text or number (an old copy of the
    // form): an expired form with the values, never "Message sent".
    for (number, text) in [("+15555550199", "Fake hello"), ("+15555550100", "Fake hello \u{1F600}")] {
        let r = post(&c, SEND, peer(65, 3), &cookie, &body(&tok, number, text)).await;
        r.assert_secure_html(400);
        assert!(r.text().contains("<span>This form expired, so the message was not sent. Send it again.</span>"), "{}", r.text());
        assert!(r.text().contains(&format!("required>{text}</textarea>")) && r.input("number") == number);
    }
    assert_eq!(calls(&c).len(), 1);
    // Formatting from autofill is dropped: the digits are sent.
    let tok = get_with(&c, NEW, peer(65, 3), &cookie).await.input("once");
    post(&c, SEND, peer(65, 3), &cookie, &body(&tok, "+1 (555) 555-0100", "Fake hi")).await.assert_see_other(sent);
    assert_eq!(calls(&c)[1].1["number"], "+15555550100");

    // Invalid fields: the form again (400) with both values, the counter
    // of the kept text and a new token; the posted token is still unused.
    let tok = get_with(&c, NEW, peer(65, 4), &cookie).await.input("once");
    let (long, wide) = ("a".repeat(766), "Ж".repeat(336));
    for (number, text, msg, count) in [
        ("  ", "Fake text", "Enter a phone number.", "9/765 (1 SMS)"),
        ("+1 555/555 0100", "Fake text", "Enter at most 32 digits, letters, spaces, +, * or #.", "9/765 (1 SMS)"),
        ("+15555550100", " \r\n ", "Enter a message.", "3/765 (1 SMS)"),
        ("+15555550100", &long, "Enter at most 765 characters.", "766/765 (6 SMS)"),
        ("+15555550100", &wide, "Enter at most 335 characters (this text needs Unicode).", "336/335 (6 SMS)"),
    ] {
        let r = post(&c, SEND, peer(65, 4), &cookie, &body(&tok, number, text)).await;
        r.assert_secure_html(400);
        let html = r.text();
        assert!(html.contains(&format!("<p class=\"sfail\" role=\"alert\"><span>{msg}</span></p>")), "{html}");
        assert!(html.contains(&format!("<p class=\"foot\">{count}</p>")), "{count} in {html}");
        assert!(html.contains(&format!("required>{}</textarea>", text.replace("\r\n", "\n"))));
        assert_eq!(r.input("number"), crate::web::sms::clean_number(number));
        assert!(r.input("once") != tok && html.contains("<title>New Message · U60 Pro</title>") && !html.contains("@get("));
        assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Fresh));
        assert_no_numbers_in_urls(&html);
    }
    // An unknown or expired token: "This form expired", both values kept.
    let r = post(&c, SEND, peer(65, 5), &cookie, &body("0000000000000000", "+15555550100", "Keep <me>")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("<span>This form expired, so the message was not sent. Send it again.</span>"), "{}", r.text());
    assert!(r.text().contains("required>Keep &lt;me&gt;</textarea>") && r.input("number") == "+15555550100");
    assert_eq!(calls(&c).len(), 2);

    // The device rejects it: the app's words, the values kept; a replay
    // shows the same and sends nothing. The API route's error, escaped.
    fake::with(&c.app, |d| d.reply = (200, json!({"ok": true, "data": {"result": 4}})));
    for _ in 0..2 {
        let r = post(&c, SEND, peer(65, 5), &cookie, &body(&tok, "+15555550100", "Retry me")).await;
        r.assert_secure_html(502);
        assert!(r.text().contains("<span>Failed to send: device rejected (result=4)</span>") && r.text().contains(">Retry me</textarea>"));
        assert_eq!(r.input("number"), "+15555550100");
    }
    assert_eq!(calls(&c).len(), 3);
    fake::with(&c.app, |d| d.reply = (503, json!({"ok": false, "error": "ubus <down>"})));
    let tok = get_with(&c, NEW, peer(65, 5), &cookie).await.input("once");
    let r = post(&c, SEND, peer(65, 5), &cookie, &body(&tok, "+15555550100", "Retry me")).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("<span>Failed to send: ubus &lt;down&gt;</span>"), "{}", r.text());
    assert_eq!(calls(&c).len(), 4);

    // The router busy: nothing sent, the form again with the values, the token given back.
    fake::with(&c.app, |d| d.reply = (200, json!({"ok": true, "data": {"result": 3}})));
    let tok = get_with(&c, NEW, peer(65, 6), &cookie).await.input("once");
    let mut busy = c.clone();
    busy.gate = Arc::new(Gate::new(0, 0, Duration::from_millis(20)));
    let r = post(&busy, SEND, peer(65, 6), &cookie, &body(&tok, "+15555550100", "Busy")).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("<span>Failed to send: the router is busy. Try again in a few seconds.</span>") && r.text().contains(">Busy</textarea>"));
    assert_eq!((r.input("number"), c.once.get(&tok, sid, c.app.auth.now())), ("+15555550100".to_owned(), Some(Token::Fresh)));
    assert_eq!(calls(&c).len(), 4);

    // A replay while the first post is sending waits for its outcome; an
    // unknown one (the token gone) leads to Messages without "sent".
    let twice = digest(None, "+15555550100", "Twice");
    for (outcome, want) in [
        (Some(Token::Sent(twice)), Ok(sent)),
        (None, Ok("/ui/sms")),
        (Some(Token::Failed(twice, 502, "Failed to send: device rejected (result=1)".into())), Err("<span>Failed to send: device rejected (result=1)</span>")),
        (Some(Token::Fresh), Err("<span>Message not sent</span>")),
    ] {
        let tok = get_with(&c, NEW, peer(65, 7), &cookie).await.input("once");
        c.once.update(&tok, sid, c.app.auth.now(), |t| *t = Token::Sending(twice));
        let (c2, cookie2, b) = (c.clone(), cookie.clone(), body(&tok, "+15555550100", "Twice"));
        let replay = tokio::spawn(async move { post(&c2, SEND, peer(65, 7), &cookie2, &b).await });
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!replay.is_finished(), "it waits for the first post");
        match &outcome {
            Some(t) => {
                c.once.update(&tok, sid, c.app.auth.now(), |x| *x = t.clone());
            }
            None => c.once.remove(&tok, Some(sid), c.app.auth.now()),
        }
        let r = replay.await.unwrap();
        match want {
            Ok(to) => r.assert_see_other(to),
            Err(w) => assert!(r.text().contains(w) && r.text().contains(">Twice</textarea>"), "{}", r.text()),
        }
    }
    assert_eq!(calls(&c).len(), 4);

    // Refused before anything is checked: no session, no CSRF token (the
    // way back is New Message), another origin; a key that is not one, or
    // neither key nor number.
    let tok = get_with(&c, NEW, peer(65, 8), &cookie).await.input("once");
    post(&c, SEND, peer(65, 8), "", &body(&tok, "+15555550100", "x")).await.assert_see_other("/ui/login?next=%2Fui%2Fsms&expired=1");
    let r = post(&c, SEND, peer(65, 8), &cookie, &format!("once={tok}&number=%2B15555550100&text=x")).await;
    r.assert_secure_html(403);
    assert!(r.text().contains("<a class=\"btn\" href=\"/ui/sms/new\">Try again</a>"), "{}", r.text());
    let b = body(&tok, "+15555550100", "x");
    let evil = form(SEND, peer(65, 8), &cookie, &b).header("origin", "http://evil.example").body(Body::from(b.clone())).unwrap();
    send(&c, evil).await.assert_secure_html(403);
    for bad in [format!("{b}&t=5555550100"), format!("csrf={csrf}&once={tok}&text=x")] {
        post(&c, SEND, peer(65, 8), &cookie, &bad).await.assert_secure_html(400);
    }
    assert_eq!(calls(&c).len(), 4);
    assert_eq!(c.once.get(&tok, sid, c.app.auth.now()), Some(Token::Fresh));
}

/// Deleting a conversation (the app's swipe; confirmed on the web, in
/// Android's words): each row's "Delete" opens the question, which names
/// the count and the number in the page only. The POST deletes every id of
/// the conversation ("a;b;") only with `confirm=1` and the fingerprint
/// asked about, then Messages says "Conversation deleted"; a changed
/// conversation is asked about again; a failure is Messages with "Delete
/// failed: …"; while a store cannot be read nothing is deleted (it may
/// hold more of the conversation).
#[tokio::test]
async fn deleting_a_conversation_is_confirmed() {
    let (c, cookie, _) = signed_in(66).await;
    let _dev = fake::set(&c.app, device());
    let (k0, k1, k2) = (key(&c, "+15555550100"), key(&c, "+15555550101"), key(&c, "+15555550102"));
    let ask = format!("/ui/sms/t/{k0}/delete");
    let list = get_with(&c, "/ui/sms", peer(66, 2), &cookie).await.text();
    let named = |n: &str| format!("Delete<span class=\"sr\"> conversation with {n}</span></a></li>");
    assert!(list.contains(&format!("</button></form><a class=\"del\" href=\"{ask}\">{}", named("+15555550100"))), "{list}");
    assert!(list.contains(&format!("</a><a class=\"del\" href=\"/ui/sms/t/{k1}/delete\">{}", named("+15555550101"))), "{list}");
    assert_eq!(list.matches("<a class=\"del\"").count(), 4);
    let sim = get_with(&c, "/ui/sms?store=sim", peer(66, 2), &cookie).await.text();
    assert!(sim.contains(&format!("<a class=\"del\" href=\"/ui/sms/t/{k2}/delete?store=sim\">{}", named("+15555550102"))), "{sim}");
    // The question: the number in the page only.
    let r = get_with(&c, &ask, peer(66, 3), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<title>Delete conversation? · U60 Pro</title>",
        "<div class=\"lead\"><a class=\"nl back\" href=\"/ui/sms\">Messages</a></div><h1>Confirm</h1>",
        "<h2 class=\"q\">Delete conversation?</h2>",
        "<li>This will delete all 3 messages from +15555550100.</li>",
        "<form class=\"panel\" method=\"post\" action=\"/ui/sms/delete\">",
        &format!("<input type=\"hidden\" name=\"t\" value=\"{k0}\"><input type=\"hidden\" name=\"fp\" value=\"3-12\">\
                  <input type=\"hidden\" name=\"confirm\" value=\"1\">"),
        "<button class=\"btn btn-danger\" type=\"submit\">Delete</button><a class=\"btn btn-quiet\" href=\"/ui/sms\">Cancel</a>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert_no_numbers_in_urls(&html);
    assert!(calls(&c).is_empty(), "the question changes nothing");
    let csrf = r.input("csrf");
    let del = |extra: &str| format!("csrf={csrf}&t={k0}{extra}");
    // Without `confirm=1`: the question; nothing sent.
    post(&c, DEL, peer(66, 4), &cookie, &del("&fp=3-12")).await.assert_see_other(&ask);
    // A message arrived meanwhile: asked again with the new count; a
    // missing fingerprint is a changed one; nothing deleted.
    let arrived = item(13, "+15555550100", "Arrived meanwhile", "24,03,03,10,00,00,+0", "1");
    fake::with(&c.app, |d| d.internal.as_mut().unwrap()["messages"].as_array_mut().unwrap().push(arrived));
    for extra in ["&fp=3-12&confirm=1", "&confirm=1"] {
        let r = post(&c, DEL, peer(66, 4), &cookie, &del(extra)).await;
        r.assert_secure_html(409);
        let html = r.text();
        assert!(html.contains("<p class=\"msg err\" role=\"alert\">The conversation changed. Review and try again.</p>"), "{html}");
        assert!(html.contains("This will delete all 4 messages from +15555550100.") && r.input("fp") == "4-13");
    }
    assert!(calls(&c).is_empty());
    // Confirmed: every id of the conversation, oldest first; then Messages.
    post(&c, DEL, peer(66, 4), &cookie, &del("&fp=4-13&confirm=1")).await.assert_see_other("/ui/sms?saved=deleted");
    assert_eq!(calls(&c), [("/api/sms/delete", json!({"id": "11;8;12;13;"}))]);
    let html = get_with(&c, "/ui/sms?saved=deleted", peer(66, 4), &cookie).await.text();
    assert!(html.contains("<p class=\"msg info\" role=\"status\">Conversation deleted</p>"), "{html}");
    // The SIM's: the store kept, on the question and on the way back.
    let r = get_with(&c, &format!("/ui/sms/t/{k2}/delete?store=sim"), peer(66, 5), &cookie).await;
    let html = r.text();
    assert!(html.contains("This will delete all 1 messages from +15555550102.") && html.contains("<a class=\"nl back\" href=\"/ui/sms?store=sim\">"));
    assert!(html.contains("name=\"fp\" value=\"1-3\"><input type=\"hidden\" name=\"store\" value=\"sim\">"), "{html}");
    let to = post(&c, DEL, peer(66, 5), &cookie, &format!("csrf={csrf}&t={k2}&store=sim&fp=1-3&confirm=1")).await;
    to.assert_see_other("/ui/sms?store=sim&saved=deleted");
    assert_eq!(calls(&c)[1], ("/api/sms/delete", json!({"id": "3;"})));
    // The device fails: Messages with the app's words (a result line for
    // this session, `?r=`), escaped.
    fake::with(&c.app, |d| d.reply = (503, json!({"ok": false, "error": "sqlite delete failed: <busy>"})));
    let r = post(&c, DEL, peer(66, 6), &cookie, &format!("csrf={csrf}&t={k1}&fp=1-10&confirm=1")).await;
    assert_eq!(r.status, 303);
    let at = r.header("location").unwrap().to_owned();
    assert!(at.starts_with("/ui/sms?r=") && at.len() == 26, "{at}");
    let html = get_with(&c, &at, peer(66, 6), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Delete failed: sqlite delete failed: &lt;busy&gt;</p>"), "{html}");
    assert!(html.contains("<title>Messages · U60 Pro</title>") && html.contains("@get('/ui/live/sms/"));
    assert_eq!(calls(&c).len(), 3);
    // The list unreadable: nothing sent; the question says why.
    let _dev = fake::set(&c.app, fake::Device { internal: Err("ubus down".into()), sim: Err("ubus down".into()), ..device() });
    let r = post(&c, DEL, peer(66, 7), &cookie, &format!("csrf={csrf}&t={k1}&fp=1-10&confirm=1")).await;
    let html = get_with(&c, r.header("location").unwrap(), peer(66, 7), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Delete failed: ubus down</p>"), "{html}");
    let r = get_with(&c, &format!("/ui/sms/t/{k1}/delete"), peer(66, 7), &cookie).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Failed: ubus down</p>") && !r.text().contains("action=\"/ui/sms/delete\""));
    assert!(calls(&c).is_empty());
    // The SIM unreadable under All: the question says so and has no form;
    // a post deletes nothing. Under Internal, it can be deleted.
    let _dev = fake::set(&c.app, fake::Device { sim: Err("SIM busy".into()), ..device() });
    let r = get_with(&c, &ask, peer(66, 7), &cookie).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Failed: Couldn't read SIM messages: SIM busy</p>"), "{}", r.text());
    assert!(!r.text().contains("action=\"/ui/sms/delete\""));
    let r = post(&c, DEL, peer(66, 7), &cookie, &del("&fp=3-12&confirm=1")).await;
    let html = get_with(&c, r.header("location").unwrap(), peer(66, 7), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Delete failed: Couldn't read SIM messages: SIM busy</p>"), "{html}");
    assert!(calls(&c).is_empty());
    let r = post(&c, DEL, peer(66, 7), &cookie, &del("&store=internal&fp=3-12&confirm=1")).await;
    r.assert_see_other("/ui/sms?store=internal&saved=deleted");
    assert_eq!(calls(&c), [("/api/sms/delete", json!({"id": "11;8;12;"}))]);

    // A conversation nobody has: not found, nothing sent.
    let _dev = fake::set(&c.app, device());
    get_with(&c, "/ui/sms/t/0123456789abcdef/delete", peer(66, 8), &cookie).await.assert_secure_html(404);
    let r = post(&c, DEL, peer(66, 8), &cookie, &format!("csrf={csrf}&t=0123456789abcdef&fp=1-1&confirm=1")).await;
    r.assert_secure_html(404);
    assert!(r.text().contains("<strong>Conversation not found</strong>"));
    // Refused: no session, no CSRF token, another origin; a bad key or store.
    post(&c, DEL, peer(66, 9), "", &del("&fp=3-12&confirm=1")).await.assert_see_other("/ui/login?next=%2Fui%2Fsms&expired=1");
    post(&c, DEL, peer(66, 9), &cookie, &format!("t={k0}&fp=3-12&confirm=1")).await.assert_secure_html(403);
    let b = del("&fp=3-12&confirm=1");
    let evil = form(DEL, peer(66, 9), &cookie, &b).header("origin", "http://evil.example").body(Body::from(b.clone())).unwrap();
    send(&c, evil).await.assert_secure_html(403);
    for bad in [format!("csrf={csrf}&t=5555550100&confirm=1"), format!("csrf={csrf}&confirm=1"), del("&store=2&confirm=1")] {
        post(&c, DEL, peer(66, 9), &cookie, &bad).await.assert_secure_html(400);
    }
    for bad in ["/ui/sms/t/5555550100/delete", "/ui/sms/t/0123456789ABCDEF/delete", &format!("{ask}/x"), "/ui/sms/t//delete"] {
        get_with(&c, bad, peer(66, 9), &cookie).await.assert_secure_html(404);
    }
    assert!(calls(&c).is_empty());
    // Signed out: back to the question after the login (its key only).
    get(&c, &ask, peer(66, 10)).await.assert_see_other(&format!("/ui/login?next=%2Fui%2Fsms%2Ft%2F{k0}%2Fdelete"));
}

/// A single-use token lasts an hour: after that its form has expired (the
/// values kept, nothing sent).
#[tokio::test]
async fn a_send_form_expires_after_an_hour() {
    let clock = FakeClock::at_secs(1000);
    let c = ctx_at(clock.clone());
    let cookie = format!("zte_ui_session={}", login(&c, 67, 1).await);
    let _dev = fake::set(&c.app, device());
    let r = get_with(&c, NEW, peer(67, 2), &cookie).await;
    let body = format!("csrf={}&once={}&number=%2B15555550100&text=Late", r.input("csrf"), r.input("once"));
    clock.advance(Duration::from_secs(3600));
    let r = post(&c, SEND, peer(67, 3), &cookie, &body).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("<span>This form expired, so the message was not sent. Send it again.</span>"), "{}", r.text());
    assert!(r.text().contains(">Late</textarea>") && r.input("number") == "+15555550100");
    assert!(calls(&c).is_empty());
}
