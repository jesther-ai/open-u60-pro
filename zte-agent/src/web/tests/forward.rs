//! SMS Forwarding through the whole router, in both mounts: the page, its
//! switches, Save Settings, deleting a rule, New Rule / Edit Rule with
//! Save and Test. The config and rules are each test `AppState`'s own (the
//! agent's handlers, in memory); a test send is answered by
//! `web::forward::fake`. Every secret is a made-up fixture and must never
//! be in a page.

use serde_json::json;

use super::*;
use crate::server::{route, Method};
use crate::web::forward::fake;

const SMS_FWD: &str = "/ui/sms/forward";
const TOOLS_FWD: &str = "/ui/tools/sms-forward";
const TOKEN: &str = "123456:FAKE-TOKEN";
const SECRETS: [&str; 14] = [
    TOKEN, "FAKE-SECRET", "FAKE-PATH", "tk_FAKE", "FAKE-DISCORD", "FAKE-SLACK", "999:NEW-FAKE", "888:OTHER-FAKE", "777:ROTATED-FAKE",
    "FAKE-PW", "FAKE-Q", "FAKE-HDR", "FAKE-LINE", "FAKE-ADDED",
];

/// The handlers save the config to the router's path; a build machine
/// has none, so nothing is written.
fn no_router_disk() {
    assert!(!std::path::Path::new("/data/local/tmp").exists(), "would write the router's forwarding config");
}

/// An API route in-process, on the blocking pool (as the gate runs it).
async fn api(c: &Ctx, method: Method, path: &'static str, body: Value) -> Value {
    no_router_disk();
    let app = Arc::clone(&c.app);
    let gate = crate::httpd::gate::Gate::new(1, 1, Duration::from_secs(1));
    let (status, v) = gate.run(crate::httpd::gate::Lane::General, move || route(&method, path, &app, body.to_string().as_bytes())).await.unwrap();
    assert!(status < 400, "{path}: {v}");
    v["data"].clone()
}

async fn config(c: &Ctx) -> Value {
    api(c, Method::Get, "/api/sms/forward/config", Value::Null).await["config"].clone()
}

/// Four rules: Telegram, a webhook with a header (disabled), ntfy with a
/// token, SMS; the poll interval 45 s.
async fn rules(c: &Ctx) {
    for (name, enabled, filter, destination) in [
        ("Fake <b>bot</b>", true, json!({"type": "all"}), json!({"type": "telegram", "bot_token": TOKEN, "chat_id": "-100123", "silent": false})),
        (
            "Hook",
            false,
            json!({"type": "sender", "patterns": ["BANK", "+1555*"]}),
            json!({"type": "webhook", "url": "https://example.com/hook/FAKE-PATH", "method": "POST",
                   "headers": [{"name": "Authorization", "value": "Bearer FAKE-SECRET"}]}),
        ),
        (
            "Push",
            true,
            json!({"type": "content", "keywords": ["code"]}),
            json!({"type": "ntfy", "url": "https://ntfy.example.com", "topic": "fake-topic", "token": "tk_FAKE"}),
        ),
        ("Text", true, json!({"type": "all"}), json!({"type": "sms", "forward_number": "+15555550123"})),
    ] {
        let body = json!({"name": name, "enabled": enabled, "filter": filter, "destination": destination});
        api(c, Method::Post, "/api/sms/forward/rules", body).await;
    }
    api(c, Method::Put, "/api/sms/forward/config", json!({"poll_interval_secs": 45})).await;
}

fn assert_no_secrets(html: &str) {
    for s in SECRETS {
        assert!(!html.contains(s), "{s} in {html}");
    }
    // No secret field ever has a value (a token's, a URL's, a header's).
    for tag in html.split("<input").skip(1).map(|t| &t[..t.find('>').unwrap()]) {
        let secret = ["tg_token\"", "nt_token\"", "wh_url\"", "dc_url\"", "sl_url\"", "wh_hv"].iter().any(|n| tag.contains(&format!("name=\"{n}")));
        assert!(!secret || !tag.contains("value="), "{tag}");
    }
}

/// The page in both mounts (its tab, its way back, its forms' targets),
/// as the app's: Settings, the rules with their summaries (a webhook by
/// its host), New Rule, Forward Log, no secret anywhere.
#[tokio::test]
async fn the_page_in_both_tabs() {
    let (c, cookie, _) = signed_in(80).await;
    rules(&c).await;
    for (base, tab, back) in [(SMS_FWD, "/ui/sms", "Messages"), (TOOLS_FWD, "/ui/tools", "Tools")] {
        let r = get_with(&c, base, peer(80, 2), &cookie).await;
        r.assert_secure_html(200);
        let html = r.text();
        assert_no_secrets(&html);
        for want in [
            "<title>SMS Forwarding · U60 Pro</title>",
            &format!("<div class=\"lead\"><a class=\"nl back\" href=\"{tab}\">{back}</a></div><h1>SMS Forwarding</h1>"),
            &format!("<a href=\"{tab}\" aria-current=\"page\">"),
            "<h2>Settings</h2>",
            &format!("<form class=\"sf\" method=\"post\" action=\"{base}/enabled\">"),
            "<input type=\"hidden\" name=\"enabled\" value=\"1\"><button class=\"sw\" type=\"submit\" role=\"switch\" aria-checked=\"false\" aria-label=\"Enabled\">",
            "<input class=\"sr\" id=\"fw-mr\" type=\"checkbox\" role=\"switch\" name=\"mark_read\" value=\"1\" form=\"fwset\">",
            "name=\"delete_after\" value=\"1\" form=\"fwset\">",
            &format!("<form id=\"fwset\" method=\"post\" action=\"{base}/settings\">"),
            "<button class=\"it cv snd\" type=\"submit\">Save Settings</button>",
            "<h2>Rules</h2>",
            &format!("<li class=\"cr fr\" id=\"r1\"><a class=\"it cv\" href=\"{base}/rules/1\">"),
            "<span class=\"nm\">Fake &lt;b&gt;bot&lt;/b&gt;</span><span class=\"rs\">All messages</span><span class=\"rs rd\">Telegram (chat: -100123)</span>",
            "<span class=\"rs\">Sender: BANK, +1555*</span><span class=\"rs rd\">Webhook (example.com)</span>",
            "<span class=\"rs\">Keywords: code</span><span class=\"rs rd\">ntfy (fake-topic)</span>",
            "<span class=\"rs rd\">SMS (+15555550123)</span>",
            // Rule 2 is off: its switch turns it on.
            &format!("<form class=\"sf\" method=\"post\" action=\"{base}/rules/toggle\">"),
            "<input type=\"hidden\" name=\"id\" value=\"2\"><input type=\"hidden\" name=\"enabled\" value=\"1\"><button class=\"sw\" type=\"submit\" role=\"switch\" aria-checked=\"false\" aria-label=\"Hook\">",
            "<input type=\"hidden\" name=\"id\" value=\"1\"><input type=\"hidden\" name=\"enabled\" value=\"0\"><button class=\"sw\" type=\"submit\" role=\"switch\" aria-checked=\"true\" aria-label=\"Fake &lt;b&gt;bot&lt;/b&gt;\">",
            &format!("<a class=\"del\" href=\"{base}/rules/delete?id=2\">Delete<span class=\"sr\"> rule Hook</span></a>"),
            &format!("<a class=\"it bt\" href=\"{base}/rules/new\">"),
            &format!("<a class=\"it\" href=\"{base}/log\"><svg"),
            "<span>Forward Log</span>",
        ] {
            assert!(html.contains(want), "{want} in {html}");
        }
        assert!(!html.contains("No Rules") && !html.contains("Last Forwarded ID"), "{html}");
        // The question names the rule; the nav bar leads back to this mount's page.
        let q = get_with(&c, &format!("{base}/rules/delete?id=1"), peer(80, 3), &cookie).await;
        q.assert_secure_html(200);
        let q = q.text();
        for want in [
            &format!("<a class=\"nl back\" href=\"{base}\">SMS Forwarding</a></div><h1>Confirm</h1>"),
            "<h2 class=\"q\">Delete “Fake &lt;b&gt;bot&lt;/b&gt;”?</h2>",
            "New messages are no longer forwarded with this rule.",
            &format!("<form class=\"panel\" method=\"post\" action=\"{base}/rules/delete\">"),
            "<input type=\"hidden\" name=\"id\" value=\"1\"><input type=\"hidden\" name=\"confirm\" value=\"1\">",
            &format!("<a class=\"btn btn-quiet\" href=\"{base}\">Cancel</a>"),
            &format!("<a href=\"{tab}\" aria-current=\"page\">"),
        ] {
            assert!(q.contains(want), "{want} in {q}");
        }
        // A rule that is gone: only Cancel, 404 as Edit Rule.
        let r = get_with(&c, &format!("{base}/rules/delete?id=99"), peer(80, 3), &cookie).await;
        r.assert_secure_html(404);
        let gone = r.text();
        assert!(gone.contains("Rule not found. It may have been deleted.") && !gone.contains("name=\"confirm\""), "{gone}");
    }
    // No rules: the app's empty state; a forwarded message: its id.
    let (c, cookie, _) = signed_in(81).await;
    let html = get_with(&c, SMS_FWD, peer(81, 2), &cookie).await.text();
    assert!(html.contains("<strong>No Rules</strong><p>Add forwarding rules to automatically forward SMS messages to Telegram, Discord, webhooks, and more.</p>"), "{html}");
    assert!(!html.contains("<h2>Rules</h2>"));
}

/// "Enabled" applies at once and "Save Settings" saves the two switches;
/// both send the config's other values back as they are (the poll
/// interval too). A rule's switch sends `{"id","enabled"}` and returns to
/// its row; a delete is asked first.
#[tokio::test]
async fn switches_settings_and_delete() {
    let (c, cookie, _) = signed_in(82).await;
    rules(&c).await;
    let csrf = get_with(&c, SMS_FWD, peer(82, 2), &cookie).await.input("csrf");
    post(&c, &format!("{SMS_FWD}/enabled"), peer(82, 3), &cookie, &format!("csrf={csrf}&enabled=1")).await.assert_see_other(SMS_FWD);
    let cfg = config(&c).await;
    assert_eq!((cfg["enabled"].as_bool(), cfg["poll_interval_secs"].as_u64()), (Some(true), Some(45)));
    let r = post(&c, &format!("{TOOLS_FWD}/settings"), peer(82, 3), &cookie, &format!("csrf={csrf}&mark_read=1")).await;
    r.assert_see_other(&format!("{TOOLS_FWD}?saved=settings"));
    let cfg = config(&c).await;
    assert_eq!(
        (cfg["enabled"].as_bool(), cfg["poll_interval_secs"].as_u64(), cfg["mark_read_after_forward"].as_bool(), cfg["delete_after_forward"].as_bool()),
        (Some(true), Some(45), Some(true), Some(false))
    );
    let html = get_with(&c, &format!("{TOOLS_FWD}?saved=settings"), peer(82, 3), &cookie).await.text();
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">Settings saved</p>"), "{html}");
    assert!(html.contains("name=\"mark_read\" value=\"1\" checked form=\"fwset\">"), "{html}");
    // A rule's switch.
    let r = post(&c, &format!("{SMS_FWD}/rules/toggle"), peer(82, 4), &cookie, &format!("csrf={csrf}&id=2&enabled=1")).await;
    r.assert_see_other(&format!("{SMS_FWD}#r2"));
    assert_eq!(config(&c).await["rules"][1]["enabled"], true);
    let r = post(&c, &format!("{SMS_FWD}/rules/toggle"), peer(82, 4), &cookie, &format!("csrf={csrf}&id=9&enabled=1")).await;
    r.assert_secure_html(404);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Failed: rule not found</p>"), "{}", r.text());
    // Delete: asked first, then gone, "Rule deleted".
    let del = format!("{TOOLS_FWD}/rules/delete");
    post(&c, &del, peer(82, 5), &cookie, &format!("csrf={csrf}&id=1")).await.assert_see_other(&format!("{del}?id=1"));
    assert_eq!(config(&c).await["rules"].as_array().unwrap().len(), 4);
    post(&c, &del, peer(82, 5), &cookie, &format!("csrf={csrf}&id=1&confirm=1")).await.assert_see_other(&format!("{TOOLS_FWD}?saved=rule-deleted"));
    let left: Vec<u64> = config(&c).await["rules"].as_array().unwrap().iter().map(|r| r["id"].as_u64().unwrap()).collect();
    assert_eq!(left, [2, 3, 4]);
    let r = post(&c, &del, peer(82, 5), &cookie, &format!("csrf={csrf}&id=1&confirm=1")).await;
    assert!(r.status == 404 && r.text().contains("Failed: rule not found"));
    // Refused: another origin, no token; signed out, nothing changes.
    for path in [format!("{SMS_FWD}/enabled"), format!("{TOOLS_FWD}/rules/toggle")] {
        post(&c, &path, peer(82, 6), &cookie, "enabled=0&id=2").await.assert_secure_html(403);
        assert_eq!(post(&c, &path, peer(82, 6), "", &format!("csrf={csrf}&enabled=0&id=2")).await.status, 303);
    }
    assert_eq!(config(&c).await["enabled"], true);
}

/// The rule form's page, with a fresh single-use token.
async fn rule_form(c: &Ctx, cookie: &str, path: &str) -> Reply {
    let r = get_with(c, path, peer(83, 2), cookie).await;
    r.assert_secure_html(200);
    r
}

fn body(r: &Reply, pairs: &[(&str, &str)]) -> String {
    let mut out = format!("csrf={}&once={}", r.input("csrf"), r.input("once"));
    for (k, v) in pairs {
        out.push_str(&format!("&{k}={}", enc(v)));
    }
    out
}

/// New Rule and Edit Rule as the app's form: secrets are password fields
/// that are never filled ("Leave empty to keep"), the rest as saved.
#[tokio::test]
async fn rule_forms_never_show_secrets() {
    let (c, cookie, _) = signed_in(83).await;
    rules(&c).await;
    let html = rule_form(&c, &cookie, &format!("{TOOLS_FWD}/rules/new")).await.text();
    for want in [
        "<title>New Rule · U60 Pro</title>",
        &format!("<div class=\"lead\"><a class=\"nl\" href=\"{TOOLS_FWD}\">Cancel</a></div><h1>New Rule</h1>"),
        "<a href=\"/ui/tools\" aria-current=\"page\">",
        &format!("<form class=\"rf\" id=\"rf\" method=\"post\" action=\"{TOOLS_FWD}/rules/save\">"),
        "<button class=\"sr\" type=\"submit\" tabindex=\"-1\" aria-hidden=\"true\">Save</button>",
        "<h2>Name</h2>",
        "placeholder=\"Rule name\" aria-label=\"Rule name\"",
        "<input class=\"sr\" type=\"radio\" id=\"ft-all\" name=\"ft\" value=\"all\" checked><span>All Messages</span>",
        "<span>Sender + Content</span>",
        "<label for=\"patterns\">Sender patterns (comma-separated)</label>",
        "<label for=\"keywords\">Keywords (comma-separated)</label>",
        "<input class=\"sr\" type=\"radio\" id=\"dt-telegram\" name=\"dt\" value=\"telegram\" checked><span>Telegram</span>",
        "<input id=\"tg_token\" type=\"password\" name=\"tg_token\" placeholder=\"Required\" autocomplete=\"new-password\" data-1p-ignore data-lpignore=\"true\" data-bwignore>",
        "<input id=\"wh_url\" type=\"text\" name=\"wh_url\" inputmode=\"url\" placeholder=\"Required\" autocomplete=\"off\"",
        "<li class=\"it fld dx d-ntfy\"><label for=\"nt_url\">Server URL</label><input id=\"nt_url\" type=\"text\" name=\"nt_url\" value=\"https://ntfy.sh\"",
        "<option value=\"POST\" selected>POST</option><option value=\"PUT\">PUT</option>",
        "<input type=\"text\" name=\"wh_hn0\" value=\"\" placeholder=\"Name\" aria-label=\"Header 1 name\"",
        "<input type=\"password\" name=\"wh_hv1\" aria-label=\"Header 2 value\" placeholder=\"Value\" autocomplete=\"new-password\" data-1p-ignore",
        "<button class=\"it bt\" type=\"submit\" name=\"add\" value=\"1\"><svg",
        "<span>Add Header</span>",
        &format!("formaction=\"{TOOLS_FWD}/rules/test\"><svg"),
        "<span>Test Destination</span>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("Leave empty to keep") && !html.contains("name=\"id\"") && !html.contains("name=\"kd\""));
    assert!(!html.contains("name=\"wh_hn2\""), "two header rows");
    // Edit Rule (the webhook): shown values, the header's name, the URL's host only.
    let r = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/2")).await;
    let html = r.text();
    assert_no_secrets(&html);
    for want in [
        "<title>Edit Rule · U60 Pro</title>",
        "<input type=\"hidden\" name=\"id\" value=\"2\">",
        "name=\"name\" value=\"Hook\"",
        "id=\"ft-sender\" name=\"ft\" value=\"sender\" checked>",
        "name=\"patterns\" value=\"BANK, +1555*\"",
        "id=\"dt-webhook\" name=\"dt\" value=\"webhook\" checked>",
        "<input id=\"wh_url\" type=\"text\" name=\"wh_url\" inputmode=\"url\" placeholder=\"Leave empty to keep\" autocomplete=\"off\"",
        "<small>Saved: example.com</small>",
        "name=\"wh_hn0\" value=\"Authorization\"",
        "name=\"wh_hv0\" aria-label=\"Header 1 value\" placeholder=\"Leave empty to keep\" autocomplete=\"new-password\"",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // ntfy: the token kept, with a way to remove it.
    let html = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/3")).await.text();
    assert_no_secrets(&html);
    assert!(html.contains("name=\"nt_url\" value=\"https://ntfy.example.com\"") && html.contains("name=\"nt_token_clear\" value=\"1\""), "{html}");
    // Unknown rule: 404; a bad id is no page.
    let r = get_with(&c, &format!("{SMS_FWD}/rules/9"), peer(83, 3), &cookie).await;
    r.assert_secure_html(404);
    assert!(r.text().contains("Rule not found"));
    for bad in ["0", "x", "1x", "-1"] {
        get_with(&c, &format!("{SMS_FWD}/rules/{bad}"), peer(83, 3), &cookie).await.assert_secure_html(404);
    }
}

/// Save: a new rule in the app's wire format; an edit keeps the secrets
/// left empty (same type and server) and the rule's switch; refused
/// fields come back as typed (never a secret), and a secret typed then is
/// kept for the next post by the form's token, not by the page.
#[tokio::test]
async fn save_creates_updates_and_keeps_secrets() {
    let (c, cookie, _) = signed_in(84).await;
    rules(&c).await;
    let save = format!("{SMS_FWD}/rules/save");
    // Edit the disabled webhook: empty URL and header value keep theirs.
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/2")).await;
    let b = body(&f, &[("id", "2"), ("name", "Hook 2"), ("ft", "all"), ("dt", "webhook"), ("wh_method", "PUT"), ("wh_hn0", "Authorization"), ("wh_hn1", "X-New"), ("wh_hv1", "1")]);
    post(&c, &save, peer(84, 2), &cookie, &b).await.assert_see_other(&format!("{SMS_FWD}?saved=rule-updated"));
    let rule = &config(&c).await["rules"][1];
    assert_eq!(rule["enabled"], false, "the rule's switch is kept");
    assert_eq!(
        rule["destination"],
        json!({"type": "webhook", "url": "https://example.com/hook/FAKE-PATH", "method": "PUT",
               "headers": [{"name": "Authorization", "value": "Bearer FAKE-SECRET"}, {"name": "X-New", "value": "1"}]})
    );
    assert_eq!((rule["name"].as_str(), &rule["filter"]), (Some("Hook 2"), &json!({"type": "all"})));
    // A replay of that post (a double click) saves nothing more and says the same.
    post(&c, &save, peer(84, 2), &cookie, &b).await.assert_see_other(&format!("{SMS_FWD}?saved=rule-updated"));
    // Another URL needs the header value again: refused, nothing saved.
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/2")).await;
    let b = body(&f, &[("id", "2"), ("name", "Hook 3"), ("ft", "all"), ("dt", "webhook"), ("wh_url", "https://example.com/other"), ("wh_hn0", "Authorization")]);
    let r = post(&c, &save, peer(84, 3), &cookie, &b).await;
    r.assert_secure_html(400);
    let html = r.text();
    assert_no_secrets(&html);
    assert!(html.contains("Enter the value again: the URL changed.") && html.contains("name=\"name\" value=\"Hook 3\""), "{html}");
    // Nothing is kept for the new URL, so the field does not say so.
    assert!(html.contains("name=\"wh_hv0\" aria-label=\"Header 1 value\" placeholder=\"Value\""), "{html}");
    assert_eq!(config(&c).await["rules"][1]["name"], "Hook 2");
    // New Rule (Telegram) with the name missing: the token typed is not
    // in the answer, but the next post of that form keeps it.
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/new")).await;
    let b = body(&f, &[("name", ""), ("ft", "content"), ("keywords", "otp, code"), ("dt", "telegram"), ("tg_token", "999:NEW-FAKE"), ("tg_chat", "77"), ("tg_silent", "1")]);
    let r = post(&c, &save, peer(84, 4), &cookie, &b).await;
    r.assert_secure_html(400);
    let html = r.text();
    assert_no_secrets(&html);
    for want in [
        "<p class=\"msg err\" role=\"alert\">Check the highlighted fields.</p>",
        "<p class=\"ferr\" id=\"name-err\">Required.</p>",
        "name=\"keywords\" value=\"otp, code\"",
        "name=\"tg_chat\" value=\"77\"",
        "name=\"tg_silent\" value=\"1\" checked>",
        "<input id=\"tg_token\" type=\"password\" name=\"tg_token\" placeholder=\"Leave empty to keep\"",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert_ne!(r.input("once"), f.input("once"), "a new token");
    let b = body(&r, &[("name", "Codes"), ("ft", "content"), ("keywords", "otp, code"), ("dt", "telegram"), ("tg_chat", "77"), ("tg_silent", "1")]);
    post(&c, &save, peer(84, 4), &cookie, &b).await.assert_see_other(&format!("{SMS_FWD}?saved=rule-created"));
    let created = &config(&c).await["rules"][4];
    assert_eq!(
        created,
        &json!({"id": 5, "name": "Codes", "enabled": true, "filter": {"type": "content", "keywords": ["otp", "code"]},
                "destination": {"type": "telegram", "bot_token": "999:NEW-FAKE", "chat_id": "77", "silent": true}})
    );
    // Another session's draft is never used: a token of mine, my draft only.
    let other = format!("zte_ui_session={}", login(&c, 84, 9).await);
    let r2 = rule_form(&c, &other, &format!("{SMS_FWD}/rules/new")).await;
    let b = format!("csrf={}&once={}&name=X&ft=all&dt=telegram&tg_chat=1", r2.input("csrf"), r.input("once"));
    let r = post(&c, &save, peer(84, 5), &other, &b).await;
    assert_eq!(r.status, 400);
    assert!(r.text().contains("This form expired, so the rule was not saved. Save it again."), "{}", r.text());
    assert_eq!(config(&c).await["rules"].as_array().unwrap().len(), 5);
    // Expired or missing token: nothing saved.
    let r = post(&c, &save, peer(84, 6), &cookie, &format!("csrf={}&once=00&name=X&ft=all&dt=sms&sms_number=1", f.input("csrf"))).await;
    assert_eq!(r.status, 400);
    assert_eq!(config(&c).await["rules"].as_array().unwrap().len(), 5);
    // Bad requests and refusals.
    for b in ["dt=email&ft=all", "dt=sms&ft=regex", "dt=sms&ft=all&id=abc", "dt=webhook&ft=all&wh_method=GET"] {
        post(&c, &save, peer(84, 7), &cookie, &format!("csrf={}&{b}", f.input("csrf"))).await.assert_secure_html(400);
    }
    // Refused: the way back is the form's page (of its mount).
    for (path, base) in [(save.clone(), SMS_FWD), (format!("{TOOLS_FWD}/rules/test"), TOOLS_FWD)] {
        let b = "name=X&ft=all&dt=sms&sms_number=1&id=2";
        let r = post(&c, &path, peer(84, 8), &cookie, b).await;
        r.assert_secure_html(403);
        assert!(r.text().contains(&format!("href=\"{base}/rules/2\"")), "{}", r.text());
        let evil = form(&path, peer(84, 8), &cookie, b).header("origin", "http://evil.example").body(Body::from(b)).unwrap();
        send(&c, evil).await.assert_secure_html(403);
        post(&c, &path, peer(84, 8), "", b).await.assert_see_other(&format!("/ui/login?next={}&expired=1", enc(base)));
    }
    assert_eq!(config(&c).await["rules"].as_array().unwrap().len(), 5);
}

/// Test: the destination with the secrets kept (same type and server),
/// sent once per token in the slow lane; the form again with the app's
/// result ("Test message sent", "Test failed: …") and a new token. An SMS
/// destination is asked first. Another server needs the secret again
/// and sends nothing.
#[tokio::test]
async fn test_destination() {
    let (c, cookie, _) = signed_in(85).await;
    rules(&c).await;
    let _fake = fake::set(&c.app, (200, json!({"ok": true, "data": {"status": "sent"}})));
    let test = format!("{SMS_FWD}/rules/test");
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/1")).await;
    let b = body(&f, &[("id", "1"), ("name", "Changed, unsaved"), ("ft", "all"), ("dt", "telegram"), ("tg_chat", "-100999")]);
    let r = post(&c, &test, peer(85, 2), &cookie, &b).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert_no_secrets(&html);
    assert!(html.contains("<p class=\"msg ok\" role=\"status\">Test message sent</p>") && html.contains("value=\"Changed, unsaved\""), "{html}");
    assert_eq!(fake::tests(&c.app), [json!({"destination": {"type": "telegram", "bot_token": TOKEN, "chat_id": "-100999", "silent": false}})]);
    // A double click: one test.
    let again = post(&c, &test, peer(85, 2), &cookie, &b).await;
    assert!(again.status == 200 && again.text().contains("Test message sent"));
    assert_eq!(fake::tests(&c.app).len(), 1);
    // Nothing was saved.
    assert_eq!(config(&c).await["rules"][0]["name"], "Fake <b>bot</b>");
    // ntfy moved to another server: the token again, nothing sent.
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/3")).await;
    let b = body(&f, &[("id", "3"), ("name", "Push"), ("ft", "all"), ("dt", "ntfy"), ("nt_url", "https://evil.example"), ("nt_topic", "t")]);
    let r = post(&c, &test, peer(85, 3), &cookie, &b).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("<p class=\"ferr\" id=\"nt_token-err\">Enter it again: the server changed.</p>"), "{}", r.text());
    assert!(r.text().contains("<input id=\"nt_token\" type=\"password\" name=\"nt_token\" placeholder=\"Optional\""), "{}", r.text());
    assert_eq!(fake::tests(&c.app).len(), 1);
    // The device's failure, with the app's prefix, and a new token.
    let _fake = fake::set(&c.app, (502, json!({"ok": false, "error": "telegram: http status: 401"})));
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/1")).await;
    let b = body(&f, &[("id", "1"), ("name", "Bot"), ("ft", "all"), ("dt", "telegram"), ("tg_chat", "1")]);
    let r = post(&c, &test, peer(85, 4), &cookie, &b).await;
    r.assert_secure_html(502);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Test failed: telegram: http status: 401</p>"), "{}", r.text());
    assert_ne!(r.input("once"), f.input("once"));
    let replay = post(&c, &test, peer(85, 4), &cookie, &b).await;
    assert!(replay.status == 502 && replay.text().contains("Test failed: telegram: http status: 401"));
    assert_eq!(fake::tests(&c.app).len(), 1);
    // SMS: asked first (it costs an SMS); Cancel shows the form again; confirmed, sent once.
    let _fake = fake::set(&c.app, (200, json!({"ok": true, "data": {"status": "sent"}})));
    let f = rule_form(&c, &cookie, &format!("{TOOLS_FWD}/rules/new")).await;
    let b = body(&f, &[("name", "T"), ("ft", "all"), ("dt", "sms"), ("sms_number", "+1 555 555 0123")]);
    let q = post(&c, &format!("{TOOLS_FWD}/rules/test"), peer(85, 5), &cookie, &b).await;
    q.assert_secure_html(200);
    let html = q.text();
    for want in [
        "<h2 class=\"q\">Send a test SMS?</h2>",
        "<li>This sends a real SMS to +15555550123, which your plan may charge.</li>",
        &format!("<form class=\"panel\" method=\"post\" action=\"{TOOLS_FWD}/rules/test\">"),
        "<input type=\"hidden\" name=\"sms_number\" value=\"+1 555 555 0123\" autocomplete=\"off\">",
        "<button class=\"btn\" type=\"submit\" name=\"confirm\" value=\"1\">Send Test SMS</button>",
        "<button class=\"btn btn-quiet\" type=\"submit\" name=\"back\" value=\"1\">Cancel</button>",
        "<a href=\"/ui/tools\" aria-current=\"page\">",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(fake::tests(&c.app).is_empty());
    let hidden = |extra: &str| {
        let mut b = format!("csrf={}", q.input("csrf"));
        for k in ["once", "name", "ft", "dt", "sms_number"] {
            b.push_str(&format!("&{k}={}", enc(&q.input(k))));
        }
        b + extra
    };
    let back = post(&c, &format!("{TOOLS_FWD}/rules/test"), peer(85, 5), &cookie, &hidden("&back=1")).await;
    back.assert_secure_html(200);
    assert!(back.text().contains("name=\"sms_number\" value=\"+1 555 555 0123\"") && back.text().contains("<h1>New Rule</h1>"));
    assert!(fake::tests(&c.app).is_empty());
    for _ in 0..2 {
        let r = post(&c, &format!("{TOOLS_FWD}/rules/test"), peer(85, 5), &cookie, &hidden("&confirm=1")).await;
        assert!(r.status == 200 && r.text().contains("Test message sent"), "{}", r.text());
    }
    assert_eq!(fake::tests(&c.app), [json!({"destination": {"type": "sms", "forward_number": "+15555550123"}})]);
    // The slow lane busy: the form again, nothing sent, the token given back.
    let mut busy = c.clone();
    busy.gate = Arc::new(Gate::new(2, 0, Duration::from_millis(20)));
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/1")).await;
    let b = body(&f, &[("id", "1"), ("name", "Bot"), ("ft", "all"), ("dt", "telegram"), ("tg_chat", "1")]);
    let r = post(&busy, &test, peer(85, 6), &cookie, &b).await;
    r.assert_secure_html(503);
    assert!(r.text().contains("Test failed: the router is busy. Try again in a few seconds."), "{}", r.text());
    assert_eq!(fake::tests(&c.app).len(), 1);
    let r = post(&c, &test, peer(85, 6), &cookie, &b).await;
    assert!(r.status == 200 && r.text().contains("Test message sent"));
    assert_eq!(fake::tests(&c.app).len(), 2);
}

/// A log as the agent answers it (newest first): a held forward, a failed
/// one whose error quotes its URI, a forwarded one, and one without a time
/// (the app's parser drops it). `content` and `date` are the message's
/// own and never shown.
fn log_fixture() -> Vec<Value> {
    let entry = |ts: i64, sms: u64, rule: u64, sender: &str, ok: bool, held: bool, error: Option<&str>| {
        json!({
            "timestamp": ts, "sms_id": sms, "sender": sender, "content_preview": "Your code is 000000",
            "rule_name": "Fake <b>bot</b>", "destination_type": "webhook", "success": ok, "error": error,
            "rule_id": rule, "content": "FAKE-CONTENT of the message", "date": "26,10,22,09,15,00,+8", "pending": held,
        })
    };
    vec![
        entry(1_792_000_300, 12, 3, "+15555550123", false, true, Some("no WAN connectivity; held for retry")),
        entry(1_792_000_200, 11, 2, "EXAMPLE-BANK", false, false, Some("webhook: bad uri: example.com/hook/FAKE-PATH is missing scheme")),
        entry(1_792_000_100, 10, 1, "+15555550100", true, false, None),
        json!({"sms_id": 9, "sender": "+15555550199", "content": "FAKE-CONTENT"}),
    ]
}

const SENT: (u16, &str) = (200, r#"{"ok": true, "data": {"status": "sent"}}"#);

fn reply((status, body): (u16, &str)) -> (u16, Value) {
    (status, serde_json::from_str(body).unwrap())
}

/// Forward Log in both tabs, as the app's: newest first; held ("Held, will
/// retry", its error grey, no Retry), failed (a cross, the error red, with
/// a URI it quotes dropped; Retry), forwarded (a check); the time in the
/// router's zone as the SMS pages; never the message's text or date. A
/// failed read: "Failed to load log: …"; none: the app's empty state.
#[tokio::test]
async fn forward_log_in_both_tabs() {
    let (c, cookie, _) = signed_in(86).await;
    let fake = fake::set_log(&c.app, reply(SENT), Some(log_fixture()));
    let stamp = |t: i64| crate::web::sms::stamp(t, crate::web::sms::local_offset(t));
    for (base, tab) in [(SMS_FWD, "/ui/sms"), (TOOLS_FWD, "/ui/tools")] {
        let r = get_with(&c, &format!("{base}/log"), peer(86, 2), &cookie).await;
        r.assert_secure_html(200);
        let html = r.text();
        assert_no_secrets(&html);
        assert!(!html.contains("FAKE-CONTENT") && !html.contains("26,10,22") && !html.contains("+15555550199"), "{html}");
        for want in [
            "<title>Forward Log · U60 Pro</title>",
            &format!(
                "<div class=\"lead\"><a class=\"nl back\" href=\"{base}\">SMS Forwarding</a></div><h1>Forward Log</h1>\
                 <div class=\"trail\"><a class=\"nl\" href=\"{base}/log/clear\">Clear</a></div>"
            ),
            &format!("<a href=\"{tab}\" aria-current=\"page\">"),
            "<div class=\"l1\"><span class=\"nm\">+15555550123</span><span class=\"c-orange hl\"><svg",
            "</svg>Held, will retry</span></div>",
            "<span class=\"rs\">no WAN connectivity; held for retry</span>",
            "<span class=\"nm\">EXAMPLE-BANK</span><span class=\"c-red\" role=\"img\" aria-label=\"Failed\"><svg",
            "<span class=\"rs er\">webhook: bad uri</span>",
            "<span class=\"nm\">+15555550100</span><span class=\"c-green\" role=\"img\" aria-label=\"Forwarded\"><svg",
            "<span class=\"rs\">Fake &lt;b&gt;bot&lt;/b&gt; → webhook</span><span class=\"pv\">Your code is 000000</span>",
            &format!("<div class=\"l2\"><span class=\"tm\">{}</span></div>", stamp(1_792_000_100)),
            &format!("<span class=\"tm\">{}</span><form class=\"sf\" method=\"post\" action=\"{base}/log/retry\">", stamp(1_792_000_200)),
            "<input type=\"hidden\" name=\"i\" value=\"1\"><input type=\"hidden\" name=\"sms\" value=\"11\">\
             <input type=\"hidden\" name=\"rule\" value=\"2\"><input type=\"hidden\" name=\"ts\" value=\"1792000200\">\
             <button class=\"btn sm\" type=\"submit\">Retry<span class=\"sr\"> forward from EXAMPLE-BANK</span></button>",
        ] {
            assert!(html.contains(want), "{want} in {html}");
        }
        assert_eq!(r.input("once").len(), 16);
        assert_eq!(html.matches("<li class=\"it lg\">").count(), 3);
        assert_eq!(html.matches("/log/retry").count(), 1, "only a failure that is not held");
        let at = |s: &str| html.find(s).unwrap();
        assert!(at("+15555550123") < at("EXAMPLE-BANK") && at("EXAMPLE-BANK") < at("+15555550100"), "newest first");
    }
    fake::with(&c.app, |d| d.log_error = Some("Failed <to> lock".into()));
    let html = get_with(&c, &format!("{SMS_FWD}/log"), peer(86, 3), &cookie).await.text();
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">Failed to load log: Failed &lt;to&gt; lock</p>"), "{html}");
    // The agent's own log (empty): no entries, no token.
    drop(fake);
    let html = get_with(&c, &format!("{TOOLS_FWD}/log"), peer(86, 4), &cookie).await.text();
    assert!(html.contains("<strong>No Log Entries</strong><p>Forwarded messages will appear here</p>"), "{html}");
    assert!(!html.contains("name=\"once\""));
}

/// A Retry's answer: the log with its result line (`?r=`).
async fn retried(c: &Ctx, cookie: &str, log: &str, r: &Reply) -> String {
    assert_eq!(r.status, 303, "{}", r.text());
    let to = r.header("location").unwrap().to_owned();
    assert!(to.starts_with(&format!("{log}?r=")), "{to}");
    let html = get_with(c, &to, peer(87, 9), cookie).await.text();
    assert_no_secrets(&html);
    html
}

const OK_LINE: &str = "<p class=\"msg ok\" role=\"status\">Retry succeeded</p>";

/// Retry: by the entry's fingerprint (where it is now; "The log changed.
/// Try again." once it is gone or was retried), once per single-use token
/// (a double click retries once and shows the first's result), in the
/// slow lane; "Retry failed: …" with the agent's error (a URI dropped).
#[tokio::test]
async fn retry_once_by_fingerprint() {
    let (c, cookie, _) = signed_in(87).await;
    let _fake = fake::set_log(&c.app, reply(SENT), Some(log_fixture()));
    let (log, retry) = (format!("{TOOLS_FWD}/log"), format!("{TOOLS_FWD}/log/retry"));
    let body = |p: &Reply| {
        let mut b = format!("csrf={}&once={}", p.input("csrf"), p.input("once"));
        for k in ["i", "sms", "rule", "ts"] {
            b.push_str(&format!("&{k}={}", p.input(k)));
        }
        b
    };
    let page = get_with(&c, &log, peer(87, 2), &cookie).await;
    let b = body(&page);
    let html = retried(&c, &cookie, &log, &post(&c, &retry, peer(87, 2), &cookie, &b).await).await;
    assert!(html.contains(OK_LINE), "{html}");
    assert_eq!(fake::retries(&c.app), [json!({"index": 1})]);
    // A double click: the first's result, one retry.
    let html = retried(&c, &cookie, &log, &post(&c, &retry, peer(87, 2), &cookie, &b).await).await;
    assert!(html.contains(OK_LINE));
    assert_eq!(fake::retries(&c.app).len(), 1);
    // The same token for another entry (an old copy of the page): nothing.
    let other = b.replace("&i=1", "&i=0");
    let html = retried(&c, &cookie, &log, &post(&c, &retry, peer(87, 2), &cookie, &other).await).await;
    assert!(html.contains("This page expired, so nothing was retried. Try again."), "{html}");
    // A new entry came first: the entry is retried where it is now.
    let page = get_with(&c, &log, peer(87, 3), &cookie).await;
    fake::with(&c.app, |d| d.log.as_mut().unwrap().insert(0, json!({"timestamp": 1_792_000_400, "sms_id": 13, "success": true})));
    let html = retried(&c, &cookie, &log, &post(&c, &retry, peer(87, 3), &cookie, &body(&page)).await).await;
    assert!(html.contains(OK_LINE));
    assert_eq!(fake::retries(&c.app)[1], json!({"index": 2}));
    // Retried meanwhile (its time changes) or cleared: nothing is sent.
    let page = get_with(&c, &log, peer(87, 4), &cookie).await;
    fake::with(&c.app, |d| d.log.as_mut().unwrap()[2]["timestamp"] = json!(1_792_000_500));
    let html = retried(&c, &cookie, &log, &post(&c, &retry, peer(87, 4), &cookie, &body(&page)).await).await;
    assert!(html.contains("<p class=\"msg err\" role=\"alert\">The log changed. Try again.</p>"), "{html}");
    assert_eq!(fake::retries(&c.app).len(), 2);
    // The agent's failure, its URI dropped; a replay shows it again.
    fake::with(&c.app, |d| {
        d.log = Some(log_fixture());
        d.reply = reply((502, r#"{"ok": false, "error": "webhook: bad uri: example.com/hook/FAKE-PATH is missing scheme"}"#));
    });
    let page = get_with(&c, &log, peer(87, 5), &cookie).await;
    for _ in 0..2 {
        let html = retried(&c, &cookie, &log, &post(&c, &retry, peer(87, 5), &cookie, &body(&page)).await).await;
        assert!(html.contains("<p class=\"msg err\" role=\"alert\">Retry failed: webhook: bad uri</p>"), "{html}");
    }
    assert_eq!(fake::retries(&c.app).len(), 3);
    // The slow lane busy: nothing sent, the token given back.
    fake::with(&c.app, |d| d.reply = reply(SENT));
    let mut busy = c.clone();
    busy.gate = Arc::new(Gate::new(2, 0, Duration::from_millis(20)));
    let page = get_with(&c, &log, peer(87, 6), &cookie).await;
    let html = retried(&c, &cookie, &log, &post(&busy, &retry, peer(87, 6), &cookie, &body(&page)).await).await;
    assert!(html.contains("Retry failed: the router is busy. Try again in a few seconds."), "{html}");
    assert_eq!(fake::retries(&c.app).len(), 3);
    let html = retried(&c, &cookie, &log, &post(&c, &retry, peer(87, 6), &cookie, &body(&page)).await).await;
    assert!(html.contains(OK_LINE));
    assert_eq!(fake::retries(&c.app).len(), 4);
    // An unknown token, another session's: nothing.
    let page = get_with(&c, &log, peer(87, 7), &cookie).await;
    let html = retried(&c, &cookie, &log, &post(&c, &retry, peer(87, 7), &cookie, &body(&page).replace(&page.input("once"), "00")).await).await;
    assert!(html.contains("This page expired"));
    let theirs = format!("zte_ui_session={}", login(&c, 87, 8).await);
    let csrf = get_with(&c, &log, peer(87, 8), &theirs).await.input("csrf");
    let b = body(&page).replace(&page.input("csrf"), &csrf);
    let r = post(&c, &retry, peer(87, 8), &theirs, &b).await;
    assert!(r.header("location").unwrap().starts_with(&format!("{log}?r=")));
    assert_eq!(fake::retries(&c.app).len(), 4);
    // Bad fields, refusals: nothing.
    for bad in ["&i=x", "&ts=", "&sms=-1", "&i=100000"] {
        let k = &bad[1..bad.find('=').unwrap() + 1];
        let b = body(&page).split('&').filter(|p| !p.starts_with(k)).collect::<Vec<_>>().join("&") + bad;
        post(&c, &retry, peer(87, 7), &cookie, &b).await.assert_secure_html(400);
    }
    let r = post(&c, &retry, peer(87, 7), &cookie, &body(&page).replace("csrf=", "x=")).await;
    r.assert_secure_html(403);
    assert!(r.text().contains(&format!("href=\"{log}\"")));
    let evil = form(&retry, peer(87, 7), &cookie, &body(&page)).header("origin", "http://evil.example").body(Body::from(body(&page))).unwrap();
    send(&c, evil).await.assert_secure_html(403);
    post(&c, &retry, peer(87, 7), "", &body(&page)).await.assert_see_other(&format!("/ui/login?next={}&expired=1", enc(&log)));
    assert_eq!(fake::retries(&c.app).len(), 4);
}

/// "Clear" asks first (the app clears at once), then the agent clears
/// (keeping held forwards) and the log says "Log cleared".
#[tokio::test]
async fn clear_asks_first() {
    let (c, cookie, _) = signed_in(88).await;
    for (base, tab) in [(SMS_FWD, "/ui/sms"), (TOOLS_FWD, "/ui/tools")] {
        let (log, clear) = (format!("{base}/log"), format!("{base}/log/clear"));
        let q = get_with(&c, &clear, peer(88, 2), &cookie).await;
        q.assert_secure_html(200);
        let html = q.text();
        for want in [
            &format!("<div class=\"lead\"><a class=\"nl back\" href=\"{log}\">Forward Log</a></div><h1>Confirm</h1>"),
            &format!("<a href=\"{tab}\" aria-current=\"page\">"),
            "<h2 class=\"q\">Clear the forward log?</h2>",
            "<li>Forwarded and failed entries are removed, except failures of a message that is still held.</li>",
            "<li>Held forwards are kept and still sent once the connection returns.</li>",
            &format!("<form class=\"panel\" method=\"post\" action=\"{clear}\">"),
            "<input type=\"hidden\" name=\"confirm\" value=\"1\"><p class=\"actions\"><button class=\"btn btn-danger\" type=\"submit\">Clear</button>",
            &format!("<a class=\"btn btn-quiet\" href=\"{log}\">Cancel</a>"),
        ] {
            assert!(html.contains(want), "{want} in {html}");
        }
        let csrf = q.input("csrf");
        post(&c, &clear, peer(88, 3), &cookie, &format!("csrf={csrf}")).await.assert_see_other(&clear);
        let r = post(&c, &clear, peer(88, 3), &cookie, &format!("csrf={csrf}&confirm=1")).await;
        r.assert_see_other(&format!("{log}?saved=log-cleared"));
        no_router_disk();
        let html = get_with(&c, &format!("{log}?saved=log-cleared"), peer(88, 4), &cookie).await.text();
        assert!(html.contains("<p class=\"msg ok\" role=\"status\">Log cleared</p>"), "{html}");
        assert!(html.contains("<strong>No Log Entries</strong>"));
    }
}

const GONE: &str = "This form expired, so the rule was not saved. Enter the secret fields again, then save.";

/// Secrets typed into a form are kept as long as its token (an hour), and
/// a form whose draft is gone (pushed out of the store) says so and saves
/// nothing rather than fall back to the rule's own secret.
#[tokio::test]
async fn typed_secrets_last_as_long_as_the_form() {
    let clock = FakeClock::at_secs(1000);
    let c = ctx_at(clock.clone());
    let cookie = format!("zte_ui_session={}", login(&c, 89, 1).await);
    rules(&c).await;
    let save = format!("{SMS_FWD}/rules/save");
    // A new token typed, the name left out: refused; the answer's form has a draft.
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/1")).await;
    let b = body(&f, &[("id", "1"), ("name", ""), ("ft", "all"), ("dt", "telegram"), ("tg_token", "999:NEW-FAKE"), ("tg_chat", "1")]);
    let r = post(&c, &save, peer(89, 2), &cookie, &b).await;
    r.assert_secure_html(400);
    assert_no_secrets(&r.text());
    assert_eq!(r.input("kd"), "telegram");
    // 31 minutes later: saved with the token typed, not the rule's.
    clock.advance(Duration::from_secs(31 * 60));
    let b = body(&r, &[("id", "1"), ("name", "Bot"), ("ft", "all"), ("dt", "telegram"), ("tg_chat", "1"), ("kd", "telegram")]);
    post(&c, &save, peer(89, 3), &cookie, &b).await.assert_see_other(&format!("{SMS_FWD}?saved=rule-updated"));
    assert_eq!(config(&c).await["rules"][0]["destination"]["bot_token"], "999:NEW-FAKE");
    // The same, with the answer's draft pushed out: refused, nothing saved.
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/1")).await;
    let b = body(&f, &[("id", "1"), ("name", ""), ("ft", "all"), ("dt", "telegram"), ("tg_token", "888:OTHER-FAKE"), ("tg_chat", "1")]);
    let r = post(&c, &save, peer(89, 4), &cookie, &b).await;
    c.drafts.remove(&r.input("once"), None, c.app.auth.now());
    let b = body(&r, &[("id", "1"), ("name", "Bot 2"), ("ft", "all"), ("dt", "telegram"), ("tg_chat", "1"), ("kd", "telegram")]);
    let gone = post(&c, &save, peer(89, 5), &cookie, &b).await;
    gone.assert_secure_html(400);
    assert!(gone.text().contains(&format!("<p class=\"msg err\" role=\"alert\">{GONE}</p>")), "{}", gone.text());
    assert!(!gone.text().contains("name=\"kd\""));
    let rule = &config(&c).await["rules"][0];
    assert_eq!((rule["name"].as_str(), rule["destination"]["bot_token"].as_str()), (Some("Bot"), Some("999:NEW-FAKE")));
    // Its token was given back: the same post once the secret is typed again saves.
    let b = body(&r, &[("id", "1"), ("name", "Bot 2"), ("ft", "all"), ("dt", "telegram"), ("tg_chat", "1"), ("tg_token", "888:OTHER-FAKE")]);
    post(&c, &save, peer(89, 5), &cookie, &b).await.assert_see_other(&format!("{SMS_FWD}?saved=rule-updated"));
    assert_eq!(config(&c).await["rules"][0]["destination"]["bot_token"], "888:OTHER-FAKE");
}

/// A header that says where a request goes (`Host`, …) is refused on Save
/// and Test: a `Host` row would send the kept `Authorization` value to
/// another site on the same address. Nothing is sent or saved.
#[tokio::test]
async fn headers_that_redirect_are_refused() {
    let (c, cookie, _) = signed_in(90).await;
    rules(&c).await;
    let _fake = fake::set(&c.app, (200, json!({"ok": true, "data": {"status": "sent"}})));
    for action in ["test", "save"] {
        let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/2")).await;
        let b = body(&f, &[("id", "2"), ("name", "Hook"), ("ft", "all"), ("dt", "webhook"), ("wh_hn0", "Authorization"), ("wh_hn1", "Host"), ("wh_hv1", "attacker.example")]);
        let r = post(&c, &format!("{SMS_FWD}/rules/{action}"), peer(90, 2), &cookie, &b).await;
        r.assert_secure_html(400);
        assert!(r.text().contains("<p class=\"ferr\">This header can't be set.</p>"), "{}", r.text());
        assert_no_secrets(&r.text());
    }
    assert!(fake::tests(&c.app).is_empty());
    assert_eq!(config(&c).await["rules"][1]["destination"]["headers"], json!([{"name": "Authorization", "value": "Bearer FAKE-SECRET"}]));
}

/// A form shown again never holds a secret: an ntfy URL typed with a
/// password, a "Name: value" line in a header's name box (taken as the
/// header), a stored ntfy URL's query; a draft holds only what was typed,
/// so a token changed in the app between Test and Save is not written
/// back; a URL typed before says "Entered: <host>"; "Add Header" draws
/// another row and keeps what was typed.
#[tokio::test]
async fn forms_never_echo_or_bring_back_secrets() {
    let (c, cookie, _) = signed_in(91).await;
    rules(&c).await;
    let _fake = fake::set(&c.app, (200, json!({"ok": true, "data": {"status": "sent"}})));
    let (save, test) = (format!("{SMS_FWD}/rules/save"), format!("{SMS_FWD}/rules/test"));
    let new = format!("{SMS_FWD}/rules/new");
    // Refused input that holds a secret is not echoed.
    let f = rule_form(&c, &cookie, &new).await;
    let b = body(&f, &[("name", "N"), ("ft", "all"), ("dt", "ntfy"), ("nt_url", "https://u:FAKE-PW@ntfy.example.com/?auth=FAKE-Q"), ("nt_topic", "t")]);
    let r = post(&c, &save, peer(91, 2), &cookie, &b).await;
    r.assert_secure_html(400);
    assert_no_secrets(&r.text());
    assert!(r.text().contains("name=\"nt_url\" value=\"https://ntfy.example.com/\""), "{}", r.text());
    let b = body(&f, &[("name", "W"), ("ft", "all"), ("dt", "webhook"), ("wh_url", "https://example.com/h"), ("wh_hn0", "X-Bad Bearer FAKE-HDR"), ("wh_hv0", "v")]);
    let r = post(&c, &save, peer(91, 2), &cookie, &b).await;
    r.assert_secure_html(400);
    assert_no_secrets(&r.text());
    assert!(r.text().contains("Not a valid header name.") && r.text().contains("name=\"wh_hn0\" value=\"\""), "{}", r.text());
    // "Authorization: Bearer …" in the name box is that header.
    let b = body(&r, &[("name", "W"), ("ft", "all"), ("dt", "webhook"), ("wh_url", "https://example.com/h"), ("wh_hn0", "Authorization: Bearer FAKE-LINE")]);
    post(&c, &save, peer(91, 3), &cookie, &b).await.assert_see_other(&format!("{SMS_FWD}?saved=rule-created"));
    assert_eq!(config(&c).await["rules"][4]["destination"]["headers"], json!([{"name": "Authorization", "value": "Bearer FAKE-LINE"}]));
    // A stored ntfy URL with a password and a query shows without them and keeps them.
    let stored = "https://nuser:FAKE-PW@ntfy2.example.com/base?auth=FAKE-Q";
    let rule = json!({"name": "P2", "enabled": true, "filter": {"type": "all"}, "destination": {"type": "ntfy", "url": stored, "topic": "t"}});
    api(&c, Method::Post, "/api/sms/forward/rules", rule).await;
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/6")).await;
    assert_no_secrets(&f.text());
    assert_eq!(f.input("nt_url"), "https://ntfy2.example.com/base");
    let b = body(&f, &[("id", "6"), ("name", "P2"), ("ft", "all"), ("dt", "ntfy"), ("nt_url", "https://ntfy2.example.com/base"), ("nt_topic", "t")]);
    post(&c, &save, peer(91, 4), &cookie, &b).await.assert_see_other(&format!("{SMS_FWD}?saved=rule-updated"));
    assert_eq!(config(&c).await["rules"][5]["destination"]["url"], stored);
    // Test with the saved token; the token changes in the app; Save from
    // the test's answer keeps the new one.
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/1")).await;
    let pairs = [("id", "1"), ("name", "Bot"), ("ft", "all"), ("dt", "telegram"), ("tg_chat", "1")];
    let r = post(&c, &test, peer(91, 5), &cookie, &body(&f, &pairs)).await;
    assert!(r.status == 200 && r.text().contains("Test message sent") && !r.text().contains("name=\"kd\""), "{}", r.text());
    let rotated = json!({"type": "telegram", "bot_token": "777:ROTATED-FAKE", "chat_id": "1", "silent": false});
    api(&c, Method::Put, "/api/sms/forward/rules", json!({"id": 1, "name": "Bot", "enabled": true, "filter": {"type": "all"}, "destination": rotated})).await;
    post(&c, &save, peer(91, 5), &cookie, &body(&r, &pairs)).await.assert_see_other(&format!("{SMS_FWD}?saved=rule-updated"));
    assert_eq!(config(&c).await["rules"][0]["destination"]["bot_token"], "777:ROTATED-FAKE");
    // A URL typed into New Rule, tested: "Entered: <host>"; Save keeps it.
    let f = rule_form(&c, &cookie, &new).await;
    let r = post(&c, &test, peer(91, 6), &cookie, &body(&f, &[("name", "D"), ("ft", "all"), ("dt", "discord"), ("dc_url", "https://discord.com/api/webhooks/1/FAKE-DISCORD")])).await;
    assert_eq!(r.status, 200);
    assert_no_secrets(&r.text());
    assert!(r.text().contains("<small>Entered: discord.com</small>") && r.text().contains("placeholder=\"Leave empty to keep\""), "{}", r.text());
    let b = body(&r, &[("name", "D"), ("ft", "all"), ("dt", "discord"), ("kd", "discord")]);
    post(&c, &save, peer(91, 6), &cookie, &b).await.assert_see_other(&format!("{SMS_FWD}?saved=rule-created"));
    assert_eq!(config(&c).await["rules"][6]["destination"]["webhook_url"], "https://discord.com/api/webhooks/1/FAKE-DISCORD");
    // "Add Header": the form again with a third row, the value typed kept; nothing saved or used up.
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/2")).await;
    let b = body(&f, &[("id", "2"), ("name", "Hook"), ("ft", "all"), ("dt", "webhook"), ("wh_hn0", "Authorization"), ("wh_hn1", "X-New"), ("wh_hv1", "FAKE-ADDED"), ("add", "1")]);
    let r = post(&c, &save, peer(91, 7), &cookie, &b).await;
    r.assert_secure_html(200);
    assert_no_secrets(&r.text());
    assert!(r.text().contains("name=\"wh_hn2\"") && !r.text().contains("name=\"wh_hn3\"") && r.text().contains("name=\"wh_hn1\" value=\"X-New\""), "{}", r.text());
    assert_eq!(fake::tests(&c.app).len(), 2);
    let b = body(&r, &[("id", "2"), ("name", "Hook"), ("ft", "all"), ("dt", "webhook"), ("wh_hn0", "Authorization"), ("wh_hn1", "X-New"), ("wh_hn2", "X-Three"), ("wh_hv2", "3"), ("kd", "webhook")]);
    post(&c, &save, peer(91, 7), &cookie, &b).await.assert_see_other(&format!("{SMS_FWD}?saved=rule-updated"));
    assert_eq!(
        config(&c).await["rules"][1]["destination"]["headers"],
        json!([{"name": "Authorization", "value": "Bearer FAKE-SECRET"}, {"name": "X-New", "value": "FAKE-ADDED"}, {"name": "X-Three", "value": "3"}])
    );
}

/// The config's read: "Last Forwarded ID" once a message was forwarded;
/// a failed read gives the app's "Failed to load: …" on the page and the
/// delete question, Edit Rule's "Failed to load", and "Failed: …" /
/// "Test failed: …" on Save and Test, nothing changed or sent.
#[tokio::test]
async fn config_reads_last_id_and_failures() {
    let (c, cookie, _) = signed_in(92).await;
    rules(&c).await;
    let _fake = fake::set(&c.app, (200, json!({"ok": true, "data": {"status": "sent"}})));
    let read = json!({"ok": true, "data": {"config": {"enabled": true, "rules": []}, "last_forwarded_id": 9}});
    fake::with(&c.app, |d| d.config = Some((200, read)));
    let html = get_with(&c, TOOLS_FWD, peer(92, 2), &cookie).await.text();
    assert!(html.contains("<li class=\"it\"><span class=\"muted\">Last Forwarded ID</span><span class=\"tm num\">9</span></li>"), "{html}");
    let f = rule_form(&c, &cookie, &format!("{SMS_FWD}/rules/new")).await;
    fake::with(&c.app, |d| d.config = Some((500, json!({"ok": false, "error": "Failed <to> read"}))));
    let page = get_with(&c, SMS_FWD, peer(92, 3), &cookie).await;
    assert!(page.text().contains("<p class=\"msg err\" role=\"alert\">Failed to load: Failed &lt;to&gt; read</p>"), "{}", page.text());
    assert!(page.text().contains("<span>New Rule</span>") && page.text().contains("<span>Forward Log</span>"));
    let q = get_with(&c, &format!("{SMS_FWD}/rules/delete?id=1"), peer(92, 3), &cookie).await.text();
    assert!(q.contains("Failed to load: Failed &lt;to&gt; read") && !q.contains("name=\"confirm\""), "{q}");
    let edit = get_with(&c, &format!("{SMS_FWD}/rules/1"), peer(92, 3), &cookie).await;
    edit.assert_secure_html(503);
    assert!(edit.text().contains("<strong>Failed to load</strong><p>Failed &lt;to&gt; read</p>"), "{}", edit.text());
    for (action, want) in [("save", "Failed: Failed &lt;to&gt; read"), ("test", "Test failed: Failed &lt;to&gt; read")] {
        let b = body(&f, &[("name", "X"), ("ft", "all"), ("dt", "telegram"), ("tg_token", "999:NEW-FAKE"), ("tg_chat", "1")]);
        let r = post(&c, &format!("{SMS_FWD}/rules/{action}"), peer(92, 4), &cookie, &b).await;
        r.assert_secure_html(503);
        assert!(r.text().contains(&format!("<p class=\"msg err\" role=\"alert\">{want}</p>")), "{}", r.text());
        assert_no_secrets(&r.text());
    }
    assert!(fake::tests(&c.app).is_empty());
    fake::with(&c.app, |d| d.config = None);
    assert_eq!(config(&c).await["rules"].as_array().unwrap().len(), 4);
}
