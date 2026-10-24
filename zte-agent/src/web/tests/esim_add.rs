//! Router › eSIM Profiles › Add eSIM through the whole router: the entry
//! form (a photo or the code, posted as multipart), the app's checks, the
//! preview's start (once), the download card (live), Install profile and
//! Cancel download, and the switch question for the profile it added.
//! Device writes go to `pages::device`, page-load reads to its `gets`,
//! topics to `pages::fake`. Every value is a made-up fixture: activation
//! codes `LPA:1$example.com$FAKE-…`, ICCIDs 8900000000000000xxx.

use serde_json::json;

use super::*;
use crate::web::pages::{device, fake};
use crate::web::qr_decode::tests::{jpeg, photo, png, CODE, OTHER};

const ADD: &str = "/ui/router/esim/add";
const DL: &str = "/ui/router/esim/add?dl=1";
const INSTALL: &str = "/ui/router/esim/add/install";
const CANCEL: &str = "/ui/router/esim/add/cancel";
/// A code that asks for the operator's confirmation code, and one.
const CC_CODE: &str = "LPA:1$example.com$FAKE-CONFIRM-ID$1.3.6.1.4.1.31746$1";
const CC: &str = "FAKE-CC-7319";
const BOUNDARY: &str = "----WebKitFormBoundaryFAKE0123";
/// What no response, header or result line may ever hold.
const SECRETS: [&str; 4] = ["FAKE-MATCHING-ID", "FAKE-CONFIRM-ID", CC, "FAKE-0000"];

/// The router reads one upload and decodes one photo at a time; the tests
/// that post them take turns.
static UPLOADS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type Part<'a> = (&'a str, Option<&'a str>, &'a [u8]);

fn multipart(parts: &[Part]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, file, content) in parts {
        out.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"").as_bytes());
        if let Some(f) = file {
            out.extend_from_slice(format!("; filename=\"{f}\"\r\nContent-Type: application/octet-stream").as_bytes());
        }
        out.extend_from_slice(b"\r\n\r\n");
        out.extend_from_slice(content);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    out
}

fn upload_req(peer: Peer, cookie: &str, body: Vec<u8>) -> axum::http::request::Builder {
    let b = req("POST", ADD, peer)
        .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
        .header("content-length", body.len());
    if cookie.is_empty() {
        b
    } else {
        b.header("cookie", cookie)
    }
}

async fn upload(c: &Ctx, peer: Peer, cookie: &str, parts: &[Part<'_>]) -> Reply {
    let body = multipart(parts);
    let r = upload_req(peer, cookie, body.clone()).header("origin", ORIGIN).body(Body::from(body)).unwrap();
    let reply = send(c, r).await;
    no_secret(&reply);
    reply
}

/// The entry form as a browser posts it: both file inputs (empty unless
/// `photo`, in "Choose QR from Photos"), the code and the confirmation code.
fn fields<'a>(csrf: &'a str, once: &'a str, code: &'a str, cc: &'a str, photo: Option<&'a [u8]>) -> Vec<Part<'a>> {
    vec![
        ("csrf", None, csrf.as_bytes()),
        ("once", None, once.as_bytes()),
        ("scan", Some(""), b""),
        ("photo", Some(if photo.is_some() { "IMG_0001.jpg" } else { "" }), photo.unwrap_or(b"")),
        ("code", None, code.as_bytes()),
        ("cc", None, cc.as_bytes()),
    ]
}

fn no_secret(r: &Reply) {
    let all = format!("{:?}\n{}", r.headers, r.text());
    for s in SECRETS {
        assert!(!all.contains(s), "{s} in {all}");
    }
}

fn to(r: &Reply) -> String {
    assert_eq!(r.status, 303, "{}", r.text());
    r.header("location").unwrap().to_owned()
}

fn calls(c: &Ctx) -> Vec<(String, Value)> {
    device::calls(&c.app)
}

/// The entry page's tokens: (csrf, once).
async fn entry(c: &Ctx, peer: Peer, cookie: &str) -> (String, String) {
    let r = get_with(c, ADD, peer, cookie).await;
    r.assert_secure_html(200);
    (r.input("csrf"), r.input("once"))
}

fn device_with(replies: Vec<(&'static str, (u16, Value))>, data: &str) -> device::Device {
    let gets = vec![
        ("/api/modem/data", Ok(json!({"enable": 1, "connect_status": data}))),
        ("/api/esim/switch", Ok(json!({"state": "idle"}))),
    ];
    device::Device { gets, replies, ..Default::default() }
}

fn started() -> (&'static str, (u16, Value)) {
    ("/api/esim/download", (202, json!({"ok": true, "data": {"state": "running", "step": "starting", "free_nvm_bytes": 934_475}})))
}

fn the_session(c: &Ctx, cookie: &str) -> crate::auth::SessionId {
    c.app.auth.authenticate(Kind::Browser, cookie.trim_start_matches("zte_ui_session=")).unwrap().id
}

/// The entry: the app's header, its "no mobile data" note, one form for
/// the camera, a photo or the code (password fields, never filled), the
/// app's footers; no stream while nothing downloads.
#[tokio::test]
async fn add_esim_follows_the_app() {
    let (c, cookie, _) = signed_in(170).await;
    let _dev = device::set(&c.app, device_with(vec![], "disconnected"));
    let r = get_with(&c, ADD, peer(170, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    for want in [
        "<h1>Add eSIM</h1>", "<div class=\"trail\"><a class=\"nl\" id=\"eadd-bar\" href=\"/ui/router/esim\">Done</a></div>",
        "<h2>Add a profile to your router</h2>", "Use the eSIM QR code from your operator. You’ll review the profile before installing it.",
        "Your router has no mobile data connection. Connect it before downloading; your phone’s own data can’t be used.",
        "<form class=\"card eform\" method=\"post\" action=\"/ui/router/esim/add\" enctype=\"multipart/form-data\" autocomplete=\"off\"><input type=\"hidden\" name=\"csrf\"",
        "Scan QR code</span><input type=\"file\" name=\"scan\" accept=\"image/jpeg,image/png\" capture=\"environment\">",
        "Choose QR from Photos</span><input type=\"file\" name=\"photo\" accept=\"image/jpeg,image/png\">",
        "Enter code manually</summary>", "<input id=\"code\" type=\"password\" name=\"code\" placeholder=\"LPA:1$…\" maxlength=\"512\" autocomplete=\"one-time-code\"",
        "<input id=\"cc\" type=\"password\" name=\"cc\" placeholder=\"Operator confirmation code\" maxlength=\"64\" autocomplete=\"one-time-code\"",
        "Enter the separate confirmation code supplied by your operator (up to 64 characters).</small>", ">Read profile details</button>",
        "Your router needs internet to download the profile. Keep its Wi-Fi connected. Downloads usually take 10–60 seconds.",
        "Your activation code is private and single-use. It is sent only to your router and isn’t saved by OpenU60.",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("@get(") && !html.contains("http-equiv=\"refresh\"") && !html.contains("<details class=\"eopt\" open>"), "{html}");
    // Connected: no note. A switch running: the app's busy line.
    let _dev = device::set(&c.app, device_with(vec![], "ipv4_ipv6_connected"));
    let html = get_with(&c, ADD, peer(170, 3), &cookie).await.text();
    assert!(!html.contains("no mobile data") && !html.contains("eSIM is busy"), "{html}");
    let mut d = device_with(vec![], "connected");
    d.gets[1] = ("/api/esim/switch", Ok(json!({"state": "reloading"})));
    let _dev = device::set(&c.app, d);
    let html = get_with(&c, ADD, peer(170, 4), &cookie).await.text();
    assert!(html.contains("The eSIM is busy; try again in a moment."), "{html}");
    // The list's "+" and its download card lead here.
    let _dev = device::set(&c.app, device_with(vec![], "connected"));
    assert!(super::super::routes::form_page(ADD) == Some(ADD) && super::super::routes::form_page(INSTALL) == Some(ADD));
}

/// A pasted code: the app's checks first (nothing sent, the token given
/// back, the fields empty), then one preview start per token; codes are in
/// no response, header or line.
#[tokio::test]
async fn a_pasted_code_starts_the_preview_once() {
    let _turn = UPLOADS.lock().await;
    let (c, cookie, _) = signed_in(171).await;
    let _dev = device::set(&c.app, device_with(vec![started()], "connected"));
    let owner = the_session(&c, &cookie);
    let (csrf, token) = entry(&c, peer(171, 2), &cookie).await;
    // Refused by the app's checks, each with its words; nothing sent.
    let cases: [(&str, &str, &str, bool, bool); 6] = [
        ("", "", "Scan or choose a photo of the QR code, or enter the code manually.", false, false),
        ("LPA:1$evil.example/x$FAKE-MATCHING-ID", "", "This QR code isn&#39;t an eSIM activation code.", true, false),
        ("WIFI:T:WPA;S:FAKE-0000;P:x;;", "", "This QR code isn&#39;t an eSIM activation code.", true, false),
        (CC_CODE, "", "Enter the confirmation code from your operator (up to 64 characters).", false, true),
        (CC_CODE, &"x".repeat(65), "Enter the confirmation code from your operator (up to 64 characters).", false, true),
        (CODE, "1\u{7}2", "Enter the confirmation code from your operator (up to 64 characters).", false, true),
    ];
    for (code, cc, want, manual, open_cc) in cases {
        let r = upload(&c, peer(171, 3), &cookie, &fields(&csrf, &token, code, cc, None)).await;
        r.assert_secure_html(400);
        let html = r.text();
        let want = if html.contains(want) { want.to_owned() } else { want.replace("&#39;", "'") };
        assert!(html.contains(&want) && html.contains("Your code isn’t kept, so scan, choose or enter it again."), "{want} in {html}");
        assert_eq!(html.contains("<details class=\"eopt\" open><summary>"), manual || open_cc, "{html}");
        assert!(!html.contains("evil.example") && !html.contains("WIFI:"), "{html}");
        assert_eq!(c.once.get(&token, owner, c.app.auth.now()), Some(crate::web::store::Token::Fresh));
    }
    assert!(calls(&c).is_empty());
    // A photo and a code: one of them, please.
    let both = fields(&csrf, &token, CODE, "", Some(b"\xff\xd8\xff"));
    let r = upload(&c, peer(171, 3), &cookie, &both).await;
    assert!(r.status == 400 && r.text().contains("Use one photo or the code entered manually, not both."), "{}", r.text());
    // The code (trimmed): one start, with the preview; twice at once: still one.
    let pasted = format!("  {CODE} \n");
    let body = fields(&csrf, &token, &pasted, "", None);
    let (a, b) = tokio::join!(upload(&c, peer(171, 4), &cookie, &body), upload(&c, peer(171, 4), &cookie, &body));
    assert_eq!((to(&a), to(&b)), (DL.to_owned(), DL.to_owned()));
    assert_eq!(calls(&c), [("/api/esim/download".to_owned(), json!({"activation_code": CODE, "preview": true}))]);
    let r = upload(&c, peer(171, 4), &cookie, &body).await;
    assert_eq!(to(&r), DL);
    assert_eq!(calls(&c).len(), 1);
    // Another code with the same (used) token: an expired page, nothing sent.
    let other = fields(&csrf, &token, OTHER, "", None);
    let r = upload(&c, peer(171, 4), &cookie, &other).await;
    assert!(r.status == 409 && r.text().contains("This page expired, so nothing was sent."), "{}", r.text());
    // A code that asks for one, with the confirmation code.
    let (_, token) = entry(&c, peer(171, 5), &cookie).await;
    let r = upload(&c, peer(171, 5), &cookie, &fields(&csrf, &token, CC_CODE, &format!(" {CC} "), None)).await;
    assert_eq!(to(&r), DL);
    assert_eq!(calls(&c)[1], ("/api/esim/download".to_owned(), json!({"activation_code": CC_CODE, "preview": true, "confirmation_code": CC})));
    // The token store holds no plain hash of a code: its digest is keyed.
    let unkeyed = crate::web::job::digest(&[ADD, &format!("{CC_CODE}\n{CC}")]);
    assert!(!matches!(c.once.get(&token, owner, c.app.auth.now()), Some(crate::web::store::Token::Sent(d)) if d == unkeyed));
}

/// A photo (the camera's or a file): decoded on the router, its one
/// activation code checked as the app does, then the same start; the
/// app's words for a photo without one, with two, or that is no image.
#[tokio::test]
async fn a_photo_is_read_on_the_router() {
    let _turn = UPLOADS.lock().await;
    let (c, cookie, _) = signed_in(172).await;
    let _dev = device::set(&c.app, device_with(vec![started()], "connected"));
    let (csrf, _) = entry(&c, peer(172, 2), &cookie).await;
    let (w, h) = (640, 480);
    let two = jpeg(&photo(w, h, CODE, 5.0, 0.0, true), w, h, false, false);
    let none = jpeg(&vec![128; w * h], w, h, false, false);
    let wifi = png(&photo(w, h, "WIFI:T:WPA;S:Example;P:FAKE-0000;;", 5.0, 0.0, false), w, h, false);
    for (img, want) in [
        (&two[..], "Choose a photo with one eSIM QR code."),
        (&none[..], "This QR code isn&#39;t an eSIM activation code."),
        (&wifi[..], "This QR code isn&#39;t an eSIM activation code."),
        (b"GIF89a-not-a-photo".as_slice(), "Choose a JPEG or PNG photo, or enter the code manually."),
        (b"\xff\xd8\xff\xe0broken".as_slice(), "Couldn’t read this photo. Choose another image or enter the code manually."),
    ] {
        let (_, token) = entry(&c, peer(172, 3), &cookie).await;
        let r = upload(&c, peer(172, 3), &cookie, &fields(&csrf, &token, "", "", Some(img))).await;
        let html = r.text();
        let want = if html.contains(want) { want.to_owned() } else { want.replace("&#39;", "'") };
        assert!(r.status == 400 && html.contains(&want), "{want} in {html}");
    }
    assert!(calls(&c).is_empty());
    // The decoder busy with another photo; then a crafted photo that runs
    // over the limit (its decoding process is stopped, so the next one runs).
    let held = crate::web::qr_decode::hold().expect("the decoder is free");
    let (_, token) = entry(&c, peer(172, 3), &cookie).await;
    let r = upload(&c, peer(172, 3), &cookie, &fields(&csrf, &token, "", "", Some(&two))).await;
    assert!(r.status == 400 && r.text().contains("The router is reading another photo. Try again in a moment."), "{}", r.text());
    drop(held);
    let grid = png(&crate::web::qr_decode::tests::finder_grid(700, 2), 700, 700, false);
    let (_, token) = entry(&c, peer(172, 3), &cookie).await;
    let started = std::time::Instant::now();
    let r = upload(&c, peer(172, 3), &cookie, &fields(&csrf, &token, "", "", Some(&grid))).await;
    assert!(r.status == 400 && r.text().contains("Couldn’t read this photo."), "{}", r.text());
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
    // The upload slot taken (another upload in RAM): refused after a wait.
    let slot = crate::web::upload::hold().expect("the upload slot is free");
    let (_, token) = entry(&c, peer(172, 3), &cookie).await;
    let r = upload(&c, peer(172, 3), &cookie, &fields(&csrf, &token, "", "", Some(&two))).await;
    assert!(r.status == 503 && r.text().contains("The router is reading another photo."), "{}", r.text());
    drop(slot);
    assert!(calls(&c).is_empty());
    // One code in a camera photo (the "Scan QR code" input), turned a little.
    let img = jpeg(&photo(w, h, CODE, 6.0, 12.0, false), w, h, true, false);
    let (_, token) = entry(&c, peer(172, 4), &cookie).await;
    let mut body = fields(&csrf, &token, "", "", None);
    body[2] = ("scan", Some("image.jpg"), &img);
    let r = upload(&c, peer(172, 4), &cookie, &body).await;
    assert_eq!(to(&r), DL);
    assert_eq!(calls(&c), [("/api/esim/download".to_owned(), json!({"activation_code": CODE, "preview": true}))]);
    // A code that asks for the confirmation code: without one, the app's
    // words and the field opened; with it, sent along.
    let img = png(&photo(w, h, CC_CODE, 5.0, 0.0, false), w, h, false);
    let (_, token) = entry(&c, peer(172, 5), &cookie).await;
    let r = upload(&c, peer(172, 5), &cookie, &fields(&csrf, &token, "", "", Some(&img))).await;
    let html = r.text();
    assert!(r.status == 400 && html.contains("Enter the confirmation code from your operator") && html.contains("<details class=\"eopt\" open>"), "{html}");
    let r = upload(&c, peer(172, 5), &cookie, &fields(&csrf, &token, "", CC, Some(&img))).await;
    assert_eq!(to(&r), DL);
    assert_eq!(calls(&c)[1].1, json!({"activation_code": CC_CODE, "preview": true, "confirmation_code": CC}));
}

/// The upload's own checks: session, origin and CSRF (before anything
/// else), multipart only, the size limit; the agent's refusals in the
/// app's "Can’t start this download" card; nothing starts during a switch.
#[tokio::test]
async fn uploads_are_checked_and_refusals_read_as_the_app() {
    let _turn = UPLOADS.lock().await;
    let (c, cookie, _) = signed_in(173).await;
    let no_data = (409, json!({"ok": false, "error": "the router has no mobile data connection; downloads use the router's own internet"}));
    let _dev = device::set(&c.app, device_with(vec![("/api/esim/download", no_data)], "disconnected"));
    let (csrf, token) = entry(&c, peer(173, 2), &cookie).await;
    let body = fields(&csrf, &token, CODE, "", None);
    // No session: the login, nothing read or sent.
    let r = upload(&c, peer(173, 3), "", &body).await;
    assert!(to(&r).starts_with("/ui/login"), "{r:?}", r = r.text());
    // Another site, by Origin or Sec-Fetch-Site; a wrong CSRF token.
    let bytes = multipart(&body);
    let r = upload_req(peer(173, 3), &cookie, bytes.clone()).header("origin", "http://evil.example").body(Body::from(bytes.clone())).unwrap();
    assert_eq!(send(&c, r).await.status, 403);
    let r = upload_req(peer(173, 3), &cookie, bytes.clone()).header("origin", ORIGIN).header("sec-fetch-site", "cross-site").body(Body::from(bytes.clone())).unwrap();
    assert_eq!(send(&c, r).await.status, 403);
    let forged = fields("00", &token, CODE, "", None);
    assert_eq!(upload(&c, peer(173, 3), &cookie, &forged).await.status, 403);
    // Not multipart; a declared or an actual body over 8 MiB and its slack.
    let r = post(&c, ADD, peer(173, 3), &cookie, &format!("csrf={csrf}&once={token}&code=x")).await;
    assert_eq!(r.status, 400);
    let big = req("POST", ADD, peer(173, 3))
        .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
        .header("origin", ORIGIN)
        .header("cookie", &cookie)
        .header("content-length", crate::web::upload::MAX_BODY + 1)
        .body(Body::from(vec![0u8; 16]))
        .unwrap();
    let r = send(&c, big).await;
    assert!(r.status == 413 && r.text().contains("Photos can be up to 8 MB."), "{}", r.text());
    let huge = vec![b'x'; crate::web::upload::MAX_BODY + 1];
    let r = req("POST", ADD, peer(173, 3))
        .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
        .header("origin", ORIGIN)
        .header("cookie", &cookie)
        .body(Body::from(huge))
        .unwrap();
    assert_eq!(send(&c, r).await.status, 413);
    assert!(calls(&c).is_empty());
    assert_eq!(c.once.get(&token, the_session(&c, &cookie), c.app.auth.now()), Some(crate::web::store::Token::Fresh));
    // The agent: no mobile data. Sent once; the card says why; a replay too.
    let r = upload(&c, peer(173, 4), &cookie, &body).await;
    r.assert_secure_html(409);
    let html = r.text();
    for want in [
        "<b>Can’t start this download</b><p>the router has no mobile data connection; downloads use the router&#39;s own internet</p>",
        "<span>Your router has no mobile data connection. Connect it before downloading; your phone’s own data can’t be used.</span>",
    ] {
        let want = if html.contains(want) { want.to_owned() } else { want.replace("&#39;", "'") };
        assert!(html.contains(&want), "{want} in {html}");
    }
    // The app's note once: at the top while the data connection is down
    // (read at page load), else in the card.
    assert_eq!(html.matches("Connect it before downloading").count(), 1, "{html}");
    assert!(!html.contains("value=\"LPA"), "{html}");
    let r = upload(&c, peer(173, 4), &cookie, &body).await;
    assert!(r.status == 409 && r.text().contains("Can’t start this download"), "{}", r.text());
    assert_eq!(calls(&c).len(), 1);
    // Busy: the agent's 409 reads as the app's line.
    let busy = (409, json!({"ok": false, "error": "eSIM manager busy; retry shortly"}));
    let _dev = device::set(&c.app, device_with(vec![("/api/esim/download", busy)], "connected"));
    let (_, token) = entry(&c, peer(173, 5), &cookie).await;
    let r = upload(&c, peer(173, 5), &cookie, &fields(&csrf, &token, CODE, "", None)).await;
    assert!(r.status == 409 && r.text().contains("<p>The eSIM is busy; try again in a moment.</p>"), "{}", r.text());
    // A switch runs: nothing sent, the token given back.
    let _dev = device::set(&c.app, device_with(vec![started()], "connected"));
    let _sw = fake::set(&c.app, json!({"state": "reloading", "generation": 9}));
    let (_, token) = entry(&c, peer(173, 6), &cookie).await;
    let n = calls(&c).len();
    let r = upload(&c, peer(173, 6), &cookie, &fields(&csrf, &token, CODE, "", None)).await;
    assert!(r.status == 409 && r.text().contains("The eSIM is busy; try again in a moment."), "{}", r.text());
    assert_eq!(calls(&c).len(), n);
    assert_eq!(c.once.get(&token, the_session(&c, &cookie), c.app.auth.now()), Some(crate::web::store::Token::Fresh));
}

/// The download card, as the app's sheet shows it in each state, live
/// while the download runs or waits for its answer; "Switch to it now"
/// leads to the switch question for the profile it added.
#[tokio::test]
async fn the_download_card_follows_the_app() {
    let (c, cookie, _) = signed_in(174).await;
    let profiles = json!({
        "profiles": [
            {"iccid": "8900000000000000101", "state": "enabled", "nickname": "Home", "provider": "Example", "name": "Home", "class": "operational"},
            {"iccid": "8900000000000000505", "state": "disabled", "nickname": null, "provider": "Example Travel", "name": "Fake Plan", "class": "operational"},
        ],
        "observed_at": 1_761_200_000, "generation": 8, "updating": false, "error": null, "pending_receipts": 0,
    });
    let mut d = device_with(vec![], "connected");
    d.gets.push(("/api/esim/profiles", Ok(profiles)));
    let _dev = device::set(&c.app, d);
    // Running: the step's words, the note, Cancel download; live, and
    // without JavaScript a reload that keeps the page's token.
    let _f = fake::set(&c.app, json!({"state": "running", "step": "es9p_authenticate_client"}));
    let r = get_with(&c, ADD, peer(174, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    let once = r.input("once");
    for want in [
        "<div class=\"trail\"><a class=\"nl\" id=\"eadd-bar\" href=\"/ui/router/esim\">Close</a></div>",
        "<form id=\"eaddf\" method=\"post\" action=\"/ui/router/esim/add/install\"><input type=\"hidden\" name=\"csrf\"",
        "<section class=\"card eprog\" id=\"eadd-dl\" aria-live=\"polite\"><div class=\"ehead\"><span class=\"eic\"><span class=\"spin\"></span></span><b>Connecting to the operator…</b>",
        "Keep your router connected to the internet. You can close this screen and return to check progress.",
        "<button class=\"btn btn-quiet bad\" type=\"submit\" form=\"eaddf\" formaction=\"/ui/router/esim/add/cancel\">Cancel download</button>",
        "@get('/ui/live/esimadd/", &format!("content=\"3; url=/ui/router/esim/add?dl=1&amp;t={once}\""),
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    let resp = open_live(&c, &live_url(&html, "esimadd"), peer(174, 3), &cookie).await;
    assert_eq!(c.app.hub.demand(crate::events::Topic::EsimDownload), 1);
    let mut b = resp.into_body().into_data_stream();
    let first = frame(&mut b).await.unwrap();
    let (_, part) = frame_parts(&first).into_iter().find(|(id, _)| id == "eadd-dl").unwrap();
    assert!(html.contains(part.as_str()) && !first.contains("csrf") && !first.contains("once"), "{first}");
    // The nav bar follows: "Done" once the download has ended (the last
    // patch before the stream ends).
    let failed = json!({"state": "failed", "step": "es9p_authenticate_client", "error": "es9p_authenticate_client: no (fake)"});
    let out = crate::events::hub::ReadOut { value: Some(failed), error: None };
    c.app.hub.offer(crate::events::Topic::EsimDownload, out, std::time::Instant::now());
    let mut last = String::new();
    while let Some(f) = frame(&mut b).await {
        last.push_str(&f);
    }
    let bar = frame_parts(&last).into_iter().find(|(id, _)| id == "eadd-bar").map(|(_, p)| p);
    assert_eq!(bar.as_deref(), Some("<a class=\"nl\" id=\"eadd-bar\" href=\"/ui/router/esim\">Done</a>"), "{last}");
    assert_eq!(c.app.hub.demand(crate::events::Topic::EsimDownload), 0);
    drop(b);
    // The preview: the question, Install profile and Cancel; no reload.
    let _f = fake::set(&c.app, json!({"state": "confirm", "step": "preview", "provider": "Example Travel", "name": "Fake Plan"}));
    let html = get_with(&c, ADD, peer(174, 4), &cookie).await.text();
    for want in [
        "<b>Ready to install</b>", "<h2>Install Example Travel?</h2><p class=\"cap\">Fake Plan</p>",
        "The profile will be installed on your router’s eSIM and stay inactive until you switch to it. Confirm within 2 minutes.",
        "<button class=\"btn btn-wide teal\" type=\"submit\" form=\"eaddf\" formaction=\"/ui/router/esim/add/install\">Install profile</button>",
        "formaction=\"/ui/router/esim/add/cancel\">Cancel download</button>", "@get('/ui/live/esimadd/",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("http-equiv=\"refresh\""), "{html}");
    // Installing: it can no longer be cancelled.
    let _f = fake::set(&c.app, json!({"state": "running", "step": "es10b_load_bound_profile_package"}));
    let html = get_with(&c, ADD, peer(174, 4), &cookie).await.text();
    assert!(html.contains("<b>Installing on the eSIM…</b>") && html.contains("Keep the router powered on. Installation can’t be cancelled at this stage."), "{html}");
    assert!(!html.contains("Cancel download"), "{html}");
    // Done: from eSIM Profiles' card (`?dl=1`), Switch to it now or Later;
    // a plain Add eSIM starts over (the app's "+").
    let _f = fake::set(&c.app, json!({"state": "done", "provider": "Example Travel", "name": "Fake Plan", "iccid": "8900000000000000505F"}));
    let html = get_with(&c, DL, peer(174, 5), &cookie).await.text();
    for want in [
        "<b>Example Travel added.</b>", "Added to your router. Your current profile is still active.",
        "<a class=\"btn teal\" href=\"/ui/router/esim/switch?new=1\">Switch to it now</a><a class=\"btn btn-quiet\" href=\"/ui/router/esim\">Later</a>",
        ">Done</a></div>",
    ] {
        assert!(html.contains(want), "{want} in {html}");
    }
    assert!(!html.contains("@get(") && !html.contains("0505") && !html.contains("eaddf\" formaction"), "{html}");
    assert!(get_with(&c, ADD, peer(174, 5), &cookie).await.text().contains("Read profile details"));
    // The switch question for it, by its key; the ICCID in no page.
    let q = get_with(&c, "/ui/router/esim/switch?new=1", peer(174, 6), &cookie).await;
    q.assert_secure_html(200);
    let q = q.text();
    assert!(q.contains("<h2 class=\"q\">Switch to Fake Plan?</h2>") && q.contains("The router disconnects from Home and connects with Fake Plan."), "{q}");
    let key = q.split("name=\"p\" value=\"").nth(1).unwrap()[..16].to_owned();
    assert!(key.bytes().all(|b| b.is_ascii_hexdigit()) && !q.contains("0505"), "{q}");
    // Nothing downloaded: nothing to switch to.
    let _f = fake::set(&c.app, json!({"state": "idle"}));
    assert_eq!(get_with(&c, "/ui/router/esim/switch?new=1", peer(174, 6), &cookie).await.status, 404);
    // Failed: the agent's words, Try again (with the code field open, as
    // the app's); a server's echo of a code is not shown.
    let _f = fake::set(&c.app, json!({"state": "failed", "step": "es9p_authenticate_client", "error": "es9p_authenticate_client: matching ID not found (fake)"}));
    let html = get_with(&c, DL, peer(174, 7), &cookie).await.text();
    assert!(html.contains("<b>Couldn’t add the profile</b>") && html.contains("<p>es9p_authenticate_client: matching ID not found (fake)</p>"), "{html}");
    assert!(html.contains("<a class=\"btn\" href=\"/ui/router/esim/add?manual=1\">Try again</a>"), "{html}");
    let html = get_with(&c, "/ui/router/esim/add?manual=1", peer(174, 7), &cookie).await.text();
    assert!(html.contains("<details class=\"eopt\" open><summary>") && html.contains("Enter code manually</summary>"), "{html}");
    let echo = format!("es9p_authenticate_client: unknown code {CODE}\tfor\nlpa:1$x$FAKE-0000");
    let _f = fake::set(&c.app, json!({"state": "failed", "step": "es9p_authenticate_client", "error": echo}));
    let html = get_with(&c, DL, peer(174, 7), &cookie).await.text();
    assert!(html.contains("<p>es9p_authenticate_client: unknown code … for …</p>") && !html.contains("FAKE") && !html.contains("example.com"), "{html}");
    // Cancelled by the 2-minute limit.
    let _f = fake::set(&c.app, json!({"state": "cancelled", "error": "not confirmed within 2 minutes"}));
    let html = get_with(&c, DL, peer(174, 7), &cookie).await.text();
    assert!(html.contains("<b>Download cancelled</b>") && html.contains("It wasn’t confirmed within 2 minutes."), "{html}");
    // eSIM Profiles' card opens the download as it is.
    let html = get_with(&c, "/ui/router/esim", peer(174, 8), &cookie).await.text();
    assert!(html.contains("<a class=\"card edl\" href=\"/ui/router/esim/add?dl=1\">"), "{html}");
}

/// Install profile and Cancel download: once per token, only while the
/// download waits for them (re-read first); the agent's refusal as the
/// app's note with "Check status".
#[tokio::test]
async fn install_and_cancel_send_once() {
    let (c, cookie, _) = signed_in(175).await;
    let confirm = json!({"state": "confirm", "step": "preview", "provider": "Example Travel"});
    let mut d = device_with(vec![], "connected");
    d.gets.push(("/api/esim/download", Ok(confirm.clone())));
    let _dev = device::set(&c.app, d.clone());
    let _f = fake::set(&c.app, confirm);
    let page = get_with(&c, ADD, peer(175, 2), &cookie).await;
    let (csrf, once) = (page.input("csrf"), page.input("once"));
    let body = format!("csrf={csrf}&once={once}");
    let (a, b) = tokio::join!(post(&c, INSTALL, peer(175, 3), &cookie, &body), post(&c, INSTALL, peer(175, 3), &cookie, &body));
    assert_eq!((to(&a), to(&b)), (DL.to_owned(), DL.to_owned()));
    assert_eq!(calls(&c), [("/api/esim/download/confirm".to_owned(), json!({"accept": true}))]);
    // Cancel with the same (used) token: an expired page.
    let r = post(&c, CANCEL, peer(175, 3), &cookie, &body).await;
    let line = to(&r);
    assert!(line.starts_with("/ui/router/esim/add?dl=1&r="), "{line}");
    assert!(get_with(&c, &line, peer(175, 3), &cookie).await.text().contains("This page expired, so nothing was sent."));
    // Cancel at the preview, with a new page's token.
    let once = get_with(&c, ADD, peer(175, 4), &cookie).await.input("once");
    let r = post(&c, CANCEL, peer(175, 4), &cookie, &format!("csrf={csrf}&once={once}")).await;
    assert_eq!(to(&r), DL);
    assert_eq!(calls(&c)[1], ("/api/esim/download/cancel".to_owned(), json!({})));
    // Moved on: Install while it runs, Cancel while it installs: nothing sent.
    for (state, step, path) in [("running", "es9p_get_bound_profile_package", INSTALL), ("running", "es10b_load_bound_profile_package", CANCEL), ("done", "", INSTALL)] {
        let mut d = d.clone();
        d.gets[2] = ("/api/esim/download", Ok(json!({"state": state, "step": step})));
        let _dev = device::set(&c.app, d);
        let once = get_with(&c, ADD, peer(175, 5), &cookie).await.input("once");
        let r = post(&c, path, peer(175, 5), &cookie, &format!("csrf={csrf}&once={once}")).await;
        assert_eq!(to(&r), DL, "{state} {step}");
        assert!(calls(&c).is_empty(), "{state} {step}");
    }
    // The agent refuses: its words, "Check status"; a replay lands there too.
    let mut d = d.clone();
    d.replies.push(("/api/esim/download/confirm", (409, json!({"ok": false, "error": "the download is no longer waiting"}))));
    let _dev = device::set(&c.app, d);
    let once = get_with(&c, ADD, peer(175, 6), &cookie).await.input("once");
    let body = format!("csrf={csrf}&once={once}");
    let line = to(&post(&c, INSTALL, peer(175, 6), &cookie, &body).await);
    assert_eq!(line, format!("{DL}&r={once}"));
    let html = get_with(&c, &line, peer(175, 6), &cookie).await.text();
    assert!(html.contains("the download is no longer waiting</b><a class=\"btn btn-quiet sm\" href=\"/ui/router/esim/add?dl=1\">Check status</a>"), "{html}");
    assert_eq!(to(&post(&c, INSTALL, peer(175, 6), &cookie, &body).await), line);
    assert_eq!(calls(&c).len(), 1);
    // CSRF and session.
    assert_eq!(post(&c, INSTALL, peer(175, 7), &cookie, &format!("once={once}")).await.status, 403);
    assert!(to(&post(&c, CANCEL, peer(175, 7), "", &body).await).starts_with("/ui/login"));
    assert_eq!(calls(&c).len(), 1);
}
