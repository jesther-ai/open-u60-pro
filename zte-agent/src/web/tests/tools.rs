//! The Tools tab through the whole router: the tool list, the app's
//! placeholders and the notes for tools that stay in the app, Device Info,
//! Connected Devices with its search, and Automations (list, New / Edit
//! Automation with its single-use token, the switch, delete). The jobs are
//! each test `AppState`'s own scheduler (the agent's handlers, in memory;
//! they try to save to `/data/local/tmp`, which a build machine does not
//! have). Every identifier is a made-up fixture.

use serde_json::json;

use super::*;
use crate::web::pages::automations::{local_input, TEMPLATES};

const AUTO: &str = "/ui/tools/automations";
const DEVICES: &str = "/ui/tools/devices";

fn jobs(c: &Ctx) -> Vec<Value> {
    assert!(!std::path::Path::new("/data/local/tmp").exists(), "would write the router's scheduler file");
    crate::scheduler::jobs_list(&c.app).1["data"].as_array().unwrap().clone()
}

fn create(c: &Ctx, body: Value) -> u64 {
    let (status, v) = crate::scheduler::jobs_create(&c.app, body.to_string().as_bytes());
    assert_eq!(status, 201, "{v}");
    v["data"]["id"].as_u64().unwrap()
}

/// The list in the app's sections and order, with the Android subtitles;
/// every row is a link.
#[tokio::test]
async fn tools_list_follows_the_app() {
    let (c, cookie, _) = signed_in(100).await;
    let html = get_with(&c, "/ui/tools", peer(100, 2), &cookie).await.text();
    let sections = [
        ("Automation", vec!["Automations", "SMS Forwarding"]),
        (
            "Network Tools",
            vec!["Speed Test", "LAN Speed Test", "Enable ADB", "USB Mode", "Band Lock", "AT Terminal", "Device Info", "Connected Devices"],
        ),
        ("Config", vec!["Config Decrypt/Encrypt"]),
        ("Shell Access Required", vec!["TTL Settings", "Enable SSH", "Device Explorer"]),
    ];
    let mut at = 0;
    for (section, rows) in sections {
        at += html[at..].find(&format!("<h2>{section}</h2>")).unwrap_or_else(|| panic!("{section}"));
        let list = &html[at..at + html[at..].find("</ul>").unwrap()];
        assert_eq!(list.matches("<li").count(), rows.len(), "{section}");
        let mut pos = 0;
        for r in rows {
            pos += list[pos..].find(&format!("<span>{r}")).unwrap_or_else(|| panic!("{r} in order"));
        }
    }
    for (href, sub) in [
        ("/ui/tools/automations", "Schedule automated tasks"),
        ("/ui/tools/sms-forward", "Auto-forward SMS to Telegram, webhooks, etc."),
        ("/ui/tools/lan-speedtest", "Test WiFi link to router"),
        ("/ui/tools/usb", "USB mode and powerbank control"),
        ("/ui/tools/device-info", "SIM, IMEI, WAN IPs"),
        ("/ui/tools/devices", "View connected clients and DHCP leases"),
        ("/ui/tools/config", "Offline ZXHN config file tool"),
        ("/ui/tools/ttl", "Requires ADB USB connection"),
    ] {
        let row = html.split(&format!("<a class=\"it\" href=\"{href}\">")).nth(1).unwrap_or_else(|| panic!("{href}"));
        assert!(row[..row.find("</a>").unwrap()].contains(&format!("<small>{sub}</small>")), "{href}: {row}");
    }
    for (href, sub) in [
        ("/ui/tools/speedtest", "Test WAN throughput"),
        ("/ui/tools/adb", "Enable USB debug mode"),
        ("/ui/tools/band-lock", "Lock NR/LTE bands"),
        ("/ui/tools/at", "Send raw AT commands to modem"),
    ] {
        let row = html.split(&format!("<a class=\"it\" href=\"{href}\">")).nth(1).unwrap_or_else(|| panic!("{href}"));
        assert!(row[..row.find("</a>").unwrap()].contains(&format!("<small>{sub}</small>")), "{href}: {row}");
    }
    // Every row works or opens its note.
    assert!(!html.contains("Coming to the web UI"));
}

/// The app's placeholders and the notes for the tools that stay in the
/// app: static pages pushed from Tools (they read nothing, so they show
/// with the router busy), no form, no stream.
#[tokio::test]
async fn placeholders_and_notes() {
    let (mut c, cookie, _) = signed_in(101).await;
    c.gate = Arc::new(Gate::new(0, 0, Duration::from_millis(20)));
    for (uri, title, text) in [
        ("/ui/tools/ttl", "TTL Settings", "Set TTL override via iptables. Requires shell access."),
        ("/ui/tools/ssh", "Enable SSH", "Install and start dropbear SSH server. Requires ADB USB connection."),
        ("/ui/tools/explorer", "Device Explorer", "Browse filesystem and collect device info. Requires ADB USB connection."),
        ("/ui/tools/lan-speedtest", "LAN Speed Test", "LAN Speed Test is in the OpenU60 app."),
        ("/ui/tools/lan-speedtest", "LAN Speed Test", "Does not use internet data."),
        ("/ui/tools/config", "Config Tool", "Config Decrypt/Encrypt is in the OpenU60 app."),
    ] {
        let r = get_with(&c, uri, peer(101, 2), &cookie).await;
        r.assert_secure_html(200);
        let html = r.text();
        assert!(html.contains(&format!("<h1>{title}</h1>")) && html.contains(&format!("<strong>{title}</strong>")), "{uri}");
        assert!(html.contains(text), "{uri}: {text}");
        assert!(html.contains("<a class=\"nl back\" href=\"/ui/tools\">Tools</a>") && html.contains("<a href=\"/ui/tools\" aria-current=\"page\">"));
        assert!(!html.contains("<form") && !html.contains("/ui/a/datastar-"), "{uri}");
    }
    // New Automation reads nothing either; the screens that read wait for the router.
    assert_eq!(get_with(&c, "/ui/tools/automations/new", peer(101, 3), &cookie).await.status, 200);
    for uri in [AUTO, "/ui/tools/automations/1", "/ui/tools/device-info", DEVICES] {
        assert_eq!(get_with(&c, uri, peer(101, 3), &cookie).await.status, 503, "{uri}");
    }
}

/// Device Info: the app's four sections; the IMEI's last 4 digits only, no
/// SIM read, no stream.
#[tokio::test]
async fn device_info_shows_the_imei_tail_only() {
    let (c, cookie, _) = signed_in(102).await;
    let v = json!({
        "imei": "000000000000042", "sim_iccid": "8900100000009998877", "sim_imsi": "001019999911111",
        "ipv4-address": [{"address": "192.168.0.1"}], "signalbar": "2", "simcard_roam": "1",
        "wan": {"ipv4-address": [{"address": "10.20.30.40"}]},
        "wan6": {"ipv6-address": [{"address": "fe80::1"}, {"address": "2001:db8::42"}, {"address": "2001:db8::43"}]},
    });
    let _fake = super::super::pages::fake::set(&c.app, v);
    let r = get_with(&c, "/ui/tools/device-info", peer(102, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h2>Device</h2><ul class=\"list\"><li class=\"it\"><span class=\"muted\">IMEI</span><span class=\"num\">\u{2026}0042</span></li></ul>",
        "<span class=\"muted\">Roaming</span><span class=\"c-orange\">Roaming</span>",
        "<span class=\"muted\">Signal</span><span class=\"tn c-orange\"><svg",
        "<span class=\"muted num\">2/5</span>",
        "<span class=\"muted\">IPv4</span><span class=\"num\">10.20.30.40</span>",
        "<span class=\"muted\">IPv6</span><span class=\"num\">2001:db8::42</span></li><li class=\"it\"><span class=\"muted\">IPv6</span><span class=\"num\">2001:db8::43</span>",
        "<h2>LAN</h2><ul class=\"list\"><li class=\"it\"><span class=\"muted\">Gateway</span><span class=\"num\">192.168.0.1</span>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    for gone in ["000000000000042", "fe80::1", "8900100000009998877", "9998877", "001019999911111", "/ui/live/"] {
        assert!(!html.contains(gone), "{gone}");
    }
    // Nothing read: "--" (one IPv6 row), home, no bars.
    drop(_fake);
    let _fake = super::super::pages::fake::set(&c.app, json!({}));
    let html = get_with(&c, "/ui/tools/device-info", peer(102, 3), &cookie).await.text();
    assert_eq!(html.matches("<span class=\"num\">--</span>").count(), 4, "{html}");
    assert!(html.contains("<span class=\"c-green\">Home</span>") && html.contains(">0/5</span>") && html.contains("tn muted"));
}

/// Connected Devices: live from `clients`, the rows masked as on Wi-Fi &
/// devices; the search is posted (CSRF), filters what is shown and is a
/// snapshot; its text is in no URL.
#[tokio::test]
async fn connected_devices_and_their_search() {
    use crate::events::Topic;
    let (c, cookie, _) = signed_in(103).await;
    let v = json!({
        "hosts": {
            "aa:bb:cc:dd:ee:42": {"name": "alice-laptop-42", "ipaddrs": ["192.168.0.42"], "ip6addrs": ["2001:db8::42"]},
            "aa:bb:cc:dd:ee:43": {"name": "tv", "ipaddrs": ["192.168.0.43"]},
        },
        "dhcp_leases": [],
    });
    let _fake = super::super::pages::fake::set(&c.app, v.clone());
    offer_all(&c, &v);
    let r = get_with(&c, DEVICES, peer(103, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>Connected Devices</h1>", "<a class=\"nl back\" href=\"/ui/tools\">Tools</a>", "@get('/ui/live/devices/",
        "<form class=\"srch\" method=\"post\" action=\"/ui/tools/devices\" role=\"search\"><input type=\"hidden\" name=\"csrf\"",
        "placeholder=\"Search devices\"", "<h2>2 Devices</h2>", "<b class=\"dn\">alice-laptop-42</b>",
        "192.168.0.42 <span class=\"mono\">••:••:••:••:ee:42</span>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("aa:bb:cc") && !html.contains("2001:db8") && !html.contains(">Cancel<"));
    let csrf = r.input("csrf");
    // The search: filtered, a snapshot (no stream), "Cancel" back to the list.
    let r = post(&c, DEVICES, peer(103, 3), &cookie, &format!("csrf={csrf}&q=+TV+")).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert!(html.contains("<h2>1 Devices</h2>") && html.contains("<b class=\"dn\">tv</b>") && !html.contains("alice"), "{html}");
    assert!(html.contains("name=\"q\" value=\"TV\"") && html.contains("<a class=\"nl\" href=\"/ui/tools/devices\">Cancel</a>"));
    assert!(!html.contains("/ui/live/") && !html.contains("/ui/a/datastar-"));
    assert_eq!(c.app.hub.demand(Topic::Clients), 0);
    let html = post(&c, DEVICES, peer(103, 3), &cookie, &format!("csrf={csrf}&q=aa%3Abb")).await.text();
    assert!(html.contains("No Results for \u{201c}aa:bb\u{201d}"), "{html}");
    // Refused without the token or a session; the text never in a URL.
    post(&c, DEVICES, peer(103, 4), &cookie, "q=tv").await.assert_secure_html(403);
    let r = post(&c, DEVICES, peer(103, 4), "", &format!("csrf={csrf}&q=tv")).await;
    r.assert_see_other("/ui/login?next=%2Fui%2Ftools%2Fdevices&expired=1");
    get(&c, "/ui/tools/devices?q=tv", peer(103, 5)).await.assert_see_other("/ui/login?next=%2Fui%2Ftools%2Fdevices");
    // A `q` in the URL is ignored.
    let html = get_with(&c, "/ui/tools/devices?q=tv", peer(103, 6), &cookie).await.text();
    assert!(html.contains("<h2>2 Devices</h2>"), "{html}");
}

// --- Automations ---

/// No jobs: the app's empty state; "New Automation".
#[tokio::test]
async fn automations_empty_and_new_form() {
    let (c, cookie, _) = signed_in(104).await;
    let html = get_with(&c, AUTO, peer(104, 2), &cookie).await.text();
    for want in [
        "<h1>Automations</h1>", "<a class=\"nl back\" href=\"/ui/tools\">Tools</a>", "<strong>No Automations</strong>",
        "Schedule actions like airplane mode, reboot, or WiFi toggles to run automatically on the device.",
        "<a class=\"it bt\" href=\"/ui/tools/automations/new\">",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // The form: the app's defaults; Cancel back to the list; the router's zone.
    let r = get_with(&c, "/ui/tools/automations/new", peer(104, 3), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<div class=\"lead\"><a class=\"nl\" href=\"/ui/tools/automations\">Cancel</a></div><h1>New Automation</h1>",
        "<form class=\"rf af\" method=\"post\" action=\"/ui/tools/automations/save\">", "name=\"once\" value=\"",
        "placeholder=\"Automation name\"", "<option value=\"airplane\" selected>Airplane Mode ON</option>",
        "<option value=\"data\">Mobile Data OFF</option>", "<option value=\"guest\">Guest WiFi OFF</option>",
        "<option value=\"reboot\">Reboot</option>", "<option value=\"powersave\">Power Save ON</option>",
        "id=\"st-recurring\" name=\"st\" value=\"recurring\" checked", "<span>One-time</span>",
        "<input id=\"time\" type=\"time\" name=\"time\" value=\"23:00\">",
        "name=\"days\" value=\"0\" checked><span>Mon</span>", "name=\"days\" value=\"4\" checked><span>Fri</span>",
        "name=\"days\" value=\"5\"><span>Sat</span>", "type=\"datetime-local\" name=\"at\"",
        "<span>Reverse action at</span><input class=\"sr\" id=\"restore\" type=\"checkbox\" role=\"switch\" name=\"restore\" value=\"1\">",
        "<input id=\"rtime\" type=\"time\" name=\"rtime\" value=\"06:30\">", "Times are the router's local time (UTC",
        "<button class=\"it cv snd\" type=\"submit\">Save</button>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("Custom:") && !html.contains("/ui/a/datastar-") && !html.contains("connected remotely"));
    // A remote browser is told what cuts its connection when it runs.
    let tailnet = Peer(std::net::SocketAddr::from(([127, 0, 0, 1], 40104)));
    let html = get_with(&c, "/ui/tools/automations/new", tailnet, &cookie).await.text();
    assert!(html.contains("<p class=\"foot\">You are connected remotely: Airplane Mode ON and Mobile Data OFF cut this connection when they run."), "{html}");
}

/// Create (once, with the app's body), edit (the switch kept), the switch,
/// delete (asked first, naming the restore that then does not run).
#[tokio::test]
async fn automations_create_edit_toggle_delete() {
    let (c, cookie, _) = signed_in(105).await;
    let page = get_with(&c, "/ui/tools/automations/new", peer(105, 2), &cookie).await;
    let (csrf, once) = (page.input("csrf"), page.input("once"));
    let body = format!(
        "csrf={csrf}&once={once}&name=Night+%3Cb%3E&action=data&st=recurring&time=22%3A30&days=5&days=6&restore=1&rtime=07%3A05&at=2026-01-01T00%3A00"
    );
    post(&c, "/ui/tools/automations/save", peer(105, 3), &cookie, &body).await.assert_see_other("/ui/tools/automations?saved=created");
    // A double click: one job, the second post shows what the first did.
    post(&c, "/ui/tools/automations/save", peer(105, 3), &cookie, &body).await.assert_see_other("/ui/tools/automations?saved=created");
    let j = jobs(&c);
    assert_eq!(j.len(), 1);
    assert_eq!(j[0]["name"], "Night <b>");
    assert_eq!(j[0]["action"], json!({"method": "PUT", "path": "/api/modem/data", "body": {"cid": 1, "enable": 0, "connect_status": "disconnected"}}));
    assert_eq!(j[0]["schedule"], json!({"type": "recurring", "time": "22:30", "days": [5, 6]}));
    assert_eq!(j[0]["restore"], json!({"time": "07:05", "body": {"cid": 1, "enable": 1}}));
    let id = j[0]["id"].as_u64().unwrap();
    let cr = j[0]["created_at"].as_i64().unwrap();
    let html = get_with(&c, &format!("{AUTO}?saved=created"), peer(105, 4), &cookie).await.text();
    for want in [
        "<p class=\"msg ok\" role=\"status\">Automation created</p>", "<h2>Automations</h2>",
        &format!("<li class=\"cr fr\" id=\"j{id}\"><a class=\"it cv\" href=\"/ui/tools/automations/{id}\">"),
        "<span class=\"nm\">Night &lt;b&gt;</span><span class=\"rs\">Weekends at 22:30 \u{2192} 07:05</span><span class=\"rs rd\">Mobile Data OFF</span>",
        "<span class=\"sd\"><form class=\"sf\" method=\"post\" action=\"/ui/tools/automations/toggle\">",
        &format!("<input type=\"hidden\" name=\"id\" value=\"{id}\"><input type=\"hidden\" name=\"c\" value=\"{cr}\">"),
        "<input type=\"hidden\" name=\"enabled\" value=\"0\"><button class=\"sw\" type=\"submit\" role=\"switch\" aria-checked=\"true\" aria-label=\"Night &lt;b&gt;\">",
        &format!("<a class=\"del\" href=\"/ui/tools/automations/delete?id={id}\">Delete"),
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("class=\"dot") && !html.contains("No Automations"));
    // A switch for another job with this id (the agent reuses a deleted
    // newest job's id; another creation time): nothing changes.
    let r = post(&c, "/ui/tools/automations/toggle", peer(105, 5), &cookie, &format!("csrf={csrf}&id={id}&c={}&enabled=0", cr - 1)).await;
    r.assert_secure_html(404);
    assert!(r.text().contains("Failed: job not found"));
    assert_eq!(jobs(&c)[0]["enabled"], true);
    // The switch: off, back at the row.
    post(&c, "/ui/tools/automations/toggle", peer(105, 5), &cookie, &format!("csrf={csrf}&id={id}&c={cr}&enabled=0"))
        .await
        .assert_see_other(&format!("{AUTO}#j{id}"));
    assert_eq!(jobs(&c)[0]["enabled"], false);
    // Edit: prefilled; a one-time job now, the switch kept off.
    let page = get_with(&c, &format!("{AUTO}/{id}"), peer(105, 6), &cookie).await;
    page.assert_secure_html(200);
    let html = page.text();
    for want in [
        "<h1>Edit Automation</h1>", &format!("name=\"id\" value=\"{id}\"><input type=\"hidden\" name=\"c\" value=\"{cr}\">"),
        "name=\"name\" value=\"Night &lt;b&gt;\"",
        "<option value=\"data\" selected>", "value=\"22:30\"", "name=\"days\" value=\"5\" checked", "name=\"days\" value=\"0\">",
        "name=\"restore\" value=\"1\" checked", "value=\"07:05\"",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    // Noon three days on (never a time that a clock change makes twice).
    let at = crate::web::pages::automations::parse_local(&format!("{}T12:00", &local_input(crate::web::sms::now_unix() + 3 * 86_400)[..10])).unwrap();
    let body = format!(
        "csrf={csrf}&once={}&id={id}&c={cr}&name=Trip&action=reboot&st=once&time=22%3A30&at={}&restore=1&rtime=07%3A05",
        page.input("once"),
        enc(&local_input(at))
    );
    // The form of another job with this id: not saved, said so (its token kept).
    let other = body.replace(&format!("&c={cr}&"), &format!("&c={}&", cr + 1));
    let r = post(&c, "/ui/tools/automations/save", peer(105, 7), &cookie, &other).await;
    r.assert_secure_html(404);
    assert!(r.text().contains("Automation not found") && jobs(&c)[0]["name"] == "Night <b>");
    post(&c, "/ui/tools/automations/save", peer(105, 7), &cookie, &body).await.assert_see_other(&format!("{AUTO}?saved=updated"));
    let j = &jobs(&c)[0];
    assert_eq!((j["name"].clone(), j["enabled"].clone()), (json!("Trip"), json!(false)));
    assert_eq!(j["action"], json!({"method": "POST", "path": "/api/device/reboot"}));
    assert_eq!(j["schedule"], json!({"type": "once", "at": at}));
    assert!(j.get("restore").is_none(), "Reboot has no restore: {j}");
    // Delete: asked first (the app deletes at once), then gone.
    let q = format!("{AUTO}/delete?id={id}");
    post(&c, "/ui/tools/automations/delete", peer(105, 8), &cookie, &format!("csrf={csrf}&id={id}&c={cr}")).await.assert_see_other(&q);
    assert_eq!(jobs(&c).len(), 1);
    let html = get_with(&c, &q, peer(105, 8), &cookie).await.text();
    assert!(html.contains("<h2 class=\"q\">Delete \u{201c}Trip\u{201d}?</h2>") && html.contains("It no longer runs."), "{html}");
    assert!(html.contains(&format!("name=\"id\" value=\"{id}\"><input type=\"hidden\" name=\"c\" value=\"{cr}\">")), "{html}");
    assert!(html.contains("href=\"/ui/tools/automations\">Cancel"));
    // A question about another job with this id (its own is gone): that
    // job is deleted already; nothing is sent, the job there now stays.
    post(&c, "/ui/tools/automations/delete", peer(105, 8), &cookie, &format!("csrf={csrf}&id={id}&c=1&confirm=1"))
        .await
        .assert_see_other(&format!("{AUTO}?saved=deleted"));
    assert_eq!(jobs(&c).len(), 1);
    // Two posts of the question at once (a double click, two tabs): one
    // delete, and both land on "Automation deleted" (never "job not found").
    let body = format!("csrf={csrf}&id={id}&c={cr}&confirm=1");
    let del = "/ui/tools/automations/delete";
    let (a, b) = tokio::join!(post(&c, del, peer(105, 8), &cookie, &body), post(&c, del, peer(105, 8), &cookie, &body));
    a.assert_see_other(&format!("{AUTO}?saved=deleted"));
    b.assert_see_other(&format!("{AUTO}?saved=deleted"));
    assert!(jobs(&c).is_empty());
    // A later replay too.
    post(&c, del, peer(105, 8), &cookie, &body).await.assert_see_other(&format!("{AUTO}?saved=deleted"));
    // Gone: the question and the form say so (404), the switch fails.
    let r = get_with(&c, &q, peer(105, 9), &cookie).await;
    r.assert_secure_html(404);
    assert!(r.text().contains("Automation not found. It may have been deleted.") && !r.text().contains(">Delete</button>"));
    let r = get_with(&c, &format!("{AUTO}/{id}"), peer(105, 9), &cookie).await;
    r.assert_secure_html(404);
    assert!(r.text().contains("Automation not found") && r.text().contains("Back to Automations"));
    let r = post(&c, "/ui/tools/automations/toggle", peer(105, 9), &cookie, &format!("csrf={csrf}&id={id}&c={cr}&enabled=1")).await;
    r.assert_secure_html(404);
    assert!(r.text().contains("<p class=\"msg err\" role=\"alert\">Failed: job not found</p>"), "{}", r.text());
}

/// The checks re-render the form as typed (400) with a new token, and
/// nothing is created; a value no form sends is 400; an unknown or other
/// form's token saves nothing.
#[tokio::test]
async fn automations_checks_and_tokens() {
    let (c, cookie, _) = signed_in(106).await;
    let page = get_with(&c, "/ui/tools/automations/new", peer(106, 2), &cookie).await;
    let (csrf, once) = (page.input("csrf"), page.input("once"));
    let past = enc(&local_input(crate::web::sms::now_unix() - 120));
    let r = post(&c, "/ui/tools/automations/save", peer(106, 3), &cookie, &format!("csrf={csrf}&once={once}&name=+&action=guest&st=recurring&time=99%3A00")).await;
    r.assert_secure_html(400);
    let html = r.text();
    for want in [
        "Check the highlighted fields.", "<p class=\"ferr\" id=\"name-err\">Enter a name.</p>",
        "<p class=\"ferr\" id=\"time-err\">Enter a time.</p>", "<p class=\"ferr\" id=\"days-err\">Select at least one day.</p>",
        "value=\"99:00\"", "<option value=\"guest\" selected>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert_ne!(r.input("once"), once);
    // The same token again (given back, as nothing was sent): a one-time job in the past.
    let r = post(&c, "/ui/tools/automations/save", peer(106, 3), &cookie, &format!("csrf={csrf}&once={once}&name=N&action=reboot&st=once&at={past}")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("Choose a time in the future."), "{}", r.text());
    assert!(jobs(&c).is_empty());
    for bad in ["action=nope&st=once", "action=reboot&st=weekly", "action=reboot&st=recurring&days=9", "st=once"] {
        post(&c, "/ui/tools/automations/save", peer(106, 4), &cookie, &format!("csrf={csrf}&once={once}&name=N&{bad}")).await.assert_secure_html(400);
    }
    let r = post(&c, "/ui/tools/automations/save", peer(106, 5), &cookie, &format!("csrf={csrf}&once=00&name=N&action=reboot&st=recurring&time=01%3A00&days=0")).await;
    r.assert_secure_html(400);
    assert!(r.text().contains("This form expired, so the automation was not saved. Save it again."));
    post(&c, "/ui/tools/automations/save", peer(106, 6), &cookie, &format!("once={once}&name=N&action=reboot&st=once")).await.assert_secure_html(403);
    post(&c, "/ui/tools/automations/save", peer(106, 6), "", &format!("csrf={csrf}")).await.assert_see_other(
        "/ui/login?next=%2Fui%2Ftools%2Fautomations&expired=1",
    );
    assert!(jobs(&c).is_empty());
    // A valid post with the token: created; the same token with other values: expired.
    let ok = format!("csrf={csrf}&once={once}&name=N&action=reboot&st=recurring&time=01%3A00&days=0");
    post(&c, "/ui/tools/automations/save", peer(106, 7), &cookie, &ok).await.assert_see_other(&format!("{AUTO}?saved=created"));
    let r = post(&c, "/ui/tools/automations/save", peer(106, 7), &cookie, &format!("{ok}&days=1")).await;
    r.assert_secure_html(400);
    assert_eq!(jobs(&c).len(), 1);
}

/// A job the app did not write (another body, an API client's) is
/// "Custom: METHOD path", kept as it is with its own restore; the list
/// shows its request and the last run's dot.
#[tokio::test]
async fn custom_automations_are_kept() {
    let (c, cookie, _) = signed_in(107).await;
    let id = create(&c, json!({
        "name": "Data on", "action": {"method": "PUT", "path": "/api/modem/data", "body": {"cid": 1, "enable": 1}},
        "schedule": {"type": "recurring", "time": "06:00", "days": [0, 1, 2, 3, 4, 5, 6]},
        "restore": {"time": "23:00", "body": {"cid": 1, "enable": 0}},
    }));
    let html = get_with(&c, AUTO, peer(107, 2), &cookie).await.text();
    assert!(html.contains("<span class=\"rs\">Every day at 06:00 \u{2192} 23:00</span><span class=\"rs rd\">PUT /api/modem/data</span>"), "{html}");
    let page = get_with(&c, &format!("{AUTO}/{id}"), peer(107, 3), &cookie).await;
    let html = page.text();
    assert!(html.contains("<li class=\"it\"><span class=\"muted\">Action</span><span class=\"dn\">Custom: PUT /api/modem/data</span></li>"), "{html}");
    assert!(!html.contains("<select") && html.contains("name=\"restore\" value=\"1\" checked"));
    let body = format!(
        "csrf={}&once={}&id={id}&c={}&name=Data+on&st=recurring&time=06%3A15&days=0&restore=1&rtime=22%3A00",
        page.input("csrf"),
        page.input("once"),
        page.input("c")
    );
    post(&c, "/ui/tools/automations/save", peer(107, 4), &cookie, &body).await.assert_see_other(&format!("{AUTO}?saved=updated"));
    let j = &jobs(&c)[0];
    assert_eq!(j["action"], json!({"method": "PUT", "path": "/api/modem/data", "body": {"cid": 1, "enable": 1}}));
    assert_eq!(j["restore"], json!({"time": "22:00", "body": {"cid": 1, "enable": 0}}));
    assert_eq!(j["schedule"], json!({"type": "recurring", "time": "06:15", "days": [0]}));
    // A template's labels match the app's.
    assert_eq!(TEMPLATES.map(|t| t.label), ["Airplane Mode ON", "Mobile Data OFF", "Guest WiFi OFF", "Reboot", "Power Save ON"]);
}
