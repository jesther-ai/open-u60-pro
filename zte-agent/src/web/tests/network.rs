use super::*;
use crate::web::pages::{device, network};
use serde_json::json;

fn devices(c: &Ctx, running: bool) -> device::Guard {
    device::set(
        &c.app,
        device::Device {
            gets: vec![
                ("/ui/test/extensions-editor", Ok(json!({"merge_enabled":false,"script_enabled":true,"merge":"ipv6: true\n","script":"function main(config) { return config; }"}))),
                ("/api/mihomo/profiles", Ok(json!({"selected":"one","profiles":[{"id":"one","name":"Personal","remote":true,"interval_hours":24},{"id":"two","name":"Travel <test>","remote":false}],"merge_saved":true,"script_saved":true,"script_enabled":true}))),
                (
                    "/api/mihomo/status",
                    Ok(
                        json!({"installed":true,"configured":true,"running":running,"autostart":running,"version":"1.19.33","scope":"lan_tcp_udp_dual_stack_tun","dashboard_version":"3.29.1"}),
                    ),
                ),
                (
                    "/api/wifi-relay/status",
                    Ok(
                        json!({"connected":false,"networks":[{"id":"1234567890abcdef","ssid":"<script>hotel</script>","frequency":2427,"signal":-43,"security":"open","supported":true}]}),
                    ),
                ),
            ],
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn global_extensions_editor_prefills_saved_source_and_stays_open() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 234, 1).await);
    let _d = devices(&c, true);
    let page = get_with(&c, network::MIHOMO, peer(234, 2), &cookie).await;
    page.assert_secure_html(200);
    assert!(page.text().contains(">function main(config) { return config; }</textarea>"), "Saved script must remain editable after a new page load");
    assert!(page.text().contains(">ipv6: true\n</textarea>"));
    assert!(page.text().contains("<details class=\"relay-notes\" id=\"global-extensions\" open>"));
    assert!(page.text().contains("</button><div data-net-feedback></div></li>"));
    browser_fixture(&c, "mihomo-extensions", &page).await;
}

#[tokio::test]
async fn global_extensions_save_reports_success_and_renews_form_token() {
    use crate::web::pages::mihomo_profiles as profiles;
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 233, 1).await);
    let _d = devices(&c, true);
    let page = get_with(&c, network::MIHOMO, peer(233, 2), &cookie).await;
    let body = format!("csrf={}&once={}&action=extensions&script_enabled=1&script=function+main(c)%7Breturn+c%7D", page.input("csrf"), page.input("once"));
    let response = send(&c, form(profiles::CHANGE, peer(233, 2), &cookie, &body)
        .header("x-u60-form", "1").body(Body::from(body.clone())).unwrap()).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.headers["cache-control"], "no-store");
    let result: Value = serde_json::from_str(&response.text()).unwrap();
    assert_eq!(result["ok"], true);
    assert!(result["message"].as_str().is_some_and(|s| s.contains("saved")));
    let token = result["once"].as_str().expect("Success must permit another edit without reloading");
    assert_ne!(token, page.input("once"));
    assert_eq!(result["extensions"]["script"], "function main(config) { return config; }");
    let replay = send(&c, form(profiles::CHANGE, peer(233, 2), &cookie, &body)
        .header("x-u60-form", "1").body(Body::from(body.clone())).unwrap()).await;
    let replay: Value = serde_json::from_str(&replay.text()).unwrap();
    assert_eq!(replay["ok"], true);
    assert!(replay["once"].is_string());
    assert_eq!(device::calls(&c.app).len(), 1, "Replay must not save twice");
    let next_body = body.replace(&page.input("once"), token).replace("return+c", "return+Object.assign(c,{ipv6:true})");
    let next = send(&c, form(profiles::CHANGE, peer(233, 2), &cookie, &next_body)
        .header("x-u60-form", "1").body(Body::from(next_body)).unwrap()).await;
    assert_eq!(next.status, 200);
    assert_eq!(device::calls(&c.app).len(), 2);
}

#[tokio::test]
async fn global_extensions_no_js_success_returns_to_open_editor_with_feedback() {
    use crate::web::pages::mihomo_profiles as profiles;
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 231, 1).await);
    let _d = devices(&c, true);
    let page = get_with(&c, network::MIHOMO, peer(231, 2), &cookie).await;
    let body = format!("csrf={}&once={}&action=extensions&script_enabled=1&script=function+main(c)%7Breturn+c%7D", page.input("csrf"), page.input("once"));
    let response = post(&c, profiles::CHANGE, peer(231, 2), &cookie, &body).await;
    assert_eq!(response.status, 303);
    let location = response.headers["location"].to_str().unwrap();
    assert!(location.starts_with("/ui/router/mihomo?extensions=1&r="));
    assert!(!location.contains("function"));
    let saved = get_with(&c, location, peer(231, 2), &cookie).await;
    saved.assert_secure_html(200);
    assert!(saved.text().contains("Global Merge &amp; Script saved."));
    assert!(saved.text().contains("id=\"global-extensions\" open>"));
    assert!(saved.text().contains(">function main(config) { return config; }</textarea>"));
    browser_fixture(&c, "mihomo-extensions-saved", &saved).await;
}

#[tokio::test]
async fn global_extensions_no_js_failure_preserves_input_and_explains_error() {
    use crate::web::pages::mihomo_profiles as profiles;
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 232, 1).await);
    let _d = device::set(&c.app, device::Device {
        gets: vec![
            ("/api/mihomo/status", Ok(json!({"installed":true}))),
            ("/api/mihomo/profiles", Ok(json!({"profiles":[]}))),
            ("/ui/test/extensions-editor", Ok(json!({"merge":"","script":""}))),
        ],
        replies: vec![("/api/mihomo/profiles", (503, json!({"ok":false,"error":"Script validation failed <safely>"})))],
        ..Default::default()
    });
    let page = get_with(&c, network::MIHOMO, peer(232, 2), &cookie).await;
    let body = format!("csrf={}&once={}&action=extensions&script_enabled=1&script=function+main(c)%7Bthrow+Error(%27%3C%2Ftextarea%3E%27)%7D", page.input("csrf"), page.input("once"));
    let response = post(&c, profiles::CHANGE, peer(232, 2), &cookie, &body).await;
    response.assert_secure_html(400);
    assert!(response.text().contains("Script validation failed &lt;safely&gt;"));
    let escaped = maud::html! { ("function main(c){throw Error('</textarea>')}") }.into_string();
    assert!(response.text().contains(&escaped));
    assert!(response.text().contains("id=\"global-extensions\" open>"));
    assert_ne!(response.input("once"), page.input("once"));
    browser_fixture(&c, "mihomo-extensions-error", &response).await;
    let retry = body.replace(&page.input("once"), &response.input("once"));
    let response = send(&c, form(profiles::CHANGE, peer(232, 2), &cookie, &retry)
        .header("x-u60-form", "1").body(Body::from(retry)).unwrap()).await;
    assert_eq!(response.status, 400);
    let result: Value = serde_json::from_str(&response.text()).unwrap();
    assert_eq!(result["error"], "Script validation failed <safely>");
    assert!(result["once"].is_string());
    assert!(!response.text().contains("throw Error"));
}

#[tokio::test]
async fn global_extensions_read_failure_does_not_show_an_empty_save_form() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 230, 1).await);
    let _d = device::set(&c.app, device::Device {
        gets: vec![
            ("/api/mihomo/status", Ok(json!({"installed":true}))),
            ("/api/mihomo/profiles", Ok(json!({"profiles":[]}))),
            ("/ui/test/extensions-editor", Err("Cannot read saved scripts".into())),
        ],
        ..Default::default()
    });
    let page = get_with(&c, network::MIHOMO, peer(230, 2), &cookie).await;
    assert!(page.text().contains("Cannot load saved global extensions"));
    assert!(!page.text().contains("id=\"global-script\""));
}

#[tokio::test]
async fn profiles_support_switch_update_schedule_paste_and_private_extensions() {
    use crate::web::pages::mihomo_profiles as profiles;
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}",login(&c,251,1).await);
    let _d = devices(&c,true);
    let page = get_with(&c,network::MIHOMO,peer(251,2),&cookie).await;
    page.assert_secure_html(200);
    for text in ["Use profile","Update now","Edit","Delete","Automatic update","Add configuration","Import YAML file","Paste YAML","Global Merge &amp; Script","Travel &lt;test&gt;"] { assert!(page.text().contains(text),"{text}"); }
    assert!(page.text().contains("name=\"url\" rows=\"3\" maxlength=\"4096\""));
    browser_fixture(&c,"mihomo-profiles",&page).await;
    for data in ["action=select&id=two","action=refresh&id=one","action=schedule&id=one&interval_hours=6","action=add&name=Example&url=https%3A%2F%2Fexample.com%2Fprivate-subscription","action=extensions&script_enabled=1&script=function+main(c)%7Breturn+c%7D"] {
        let page = get_with(&c,network::MIHOMO,peer(251,2),&cookie).await;
        let form = format!("csrf={}&once={}&{data}",page.input("csrf"),page.input("once"));
        let result = post(&c,profiles::CHANGE,peer(251,2),&cookie,&form).await;
        assert_eq!(result.status,303);
        assert!(!result.text().contains("private-subscription"));
        post(&c,profiles::CHANGE,peer(251,2),&cookie,&form).await;
    }
    let calls = device::calls(&c.app);
    assert_eq!(calls.len(),5);
    assert!(calls.iter().all(|(path,_)| path == "/api/mihomo/profiles"));
    assert_eq!(calls[2].1["interval_hours"],6);
    assert_eq!(calls[4].1["extensions"]["script_enabled"],true);
    assert_eq!(post(&c,profiles::CHANGE,peer(251,2),&cookie,"action=select&id=two").await.status,403);
}

#[tokio::test]
async fn profile_editor_prefills_only_selected_url_and_retains_failed_inputs() {
    use crate::web::pages::mihomo_profiles as profiles;
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}",login(&c,254,1).await);
    let _d = device::set(&c.app,device::Device {
        gets:vec![
            ("/api/mihomo/status",Ok(json!({"installed":true}))),
            ("/api/mihomo/profiles",Ok(json!({"profiles":[]}))),
            ("/ui/test/profile-editor",Ok(json!({"id":"one","name":"Personal <test>","source_type":"remote","url":"https://example.com/?token=private-url","interval_hours":6,"yaml":"private-node"}))),
        ],replies:vec![("/api/mihomo/profiles",(503,json!({"ok":false,"error":"Download failed safely"})))],..Default::default()
    });
    let list = get_with(&c,network::MIHOMO,peer(254,2),&cookie).await;
    assert!(!list.text().contains("private-url"));
    let page = get_with(&c,&format!("{}?edit=one",network::MIHOMO),peer(254,2),&cookie).await;
    page.assert_secure_html(200);
    for text in ["Edit configuration","Personal &lt;test&gt;","inputmode=\"url\"",">https://example.com/?token=private-url</textarea>","value=\"6\" selected"] { assert!(page.text().contains(text),"{text}"); }
    assert!(!page.text().contains("private-node"));
    browser_fixture(&c,"mihomo-edit",&page).await;
    let add = get_with(&c,&format!("{}?add=1",network::MIHOMO),peer(254,2),&cookie).await;
    browser_fixture(&c,"mihomo-add",&add).await;
    let delete = get_with(&c,&format!("{}?delete=one",network::MIHOMO),peer(254,2),&cookie).await;
    assert!(delete.text().contains("Delete configuration?"));
    assert!(!delete.text().contains("private-url"));
    browser_fixture(&c,"mihomo-delete",&delete).await;
    let make_body = |once:&str, fields:&[(&str,&str)]| {
        let mut body=String::new();
        let csrf=page.input("csrf");
        for (key,value) in [("csrf",csrf.as_str()),("once",once)].into_iter().chain(fields.iter().copied()) {
            body.push_str(&format!("--editor\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{value}\r\n"));
        }
        body.push_str("--editor--\r\n"); body
    };
    let fields=[("action","edit"),("id","one"),("source_type","remote"),("name","Changed"),("url","https://example.com/?token=private-url"),("interval_hours","12")];
    let first_token=page.input("once");
    let body=make_body(&first_token,&fields);
    let send_form = |body:String,enhanced:bool| req("POST",profiles::EDITOR,peer(254,2)).header("cookie",&cookie).header("origin",ORIGIN)
        .header("content-type","multipart/form-data; boundary=editor").header("x-u60-form",if enhanced {"1"} else {"0"}).body(Body::from(body)).unwrap();
    let failed=send(&c,send_form(body.clone(),true)).await;
    assert_eq!(failed.status,400);
    let result:serde_json::Value=serde_json::from_str(&failed.text()).unwrap();
    assert_eq!(result["error"],"Download failed safely");
    assert_ne!(result["once"],first_token);
    assert!(!failed.text().contains("private-url"));
    send(&c,send_form(body,false)).await;
    assert_eq!(device::calls(&c.app).len(),1,"replay does not duplicate edits");
    let failed=send(&c,send_form(make_body(result["once"].as_str().unwrap(),&fields),false)).await;
    assert_eq!(failed.status,400);
    assert!(failed.text().contains("value=\"Changed\"") && failed.text().contains("Download failed safely"));
    browser_fixture(&c,"mihomo-edit-error",&failed).await;
    assert_eq!(device::calls(&c.app)[0].1["interval_hours"],12);
    let bad = make_body(&first_token,&[("action","edit"),("action","delete")]);
    assert_eq!(send(&c,send_form(bad,true)).await.status,400);
    assert_eq!(device::calls(&c.app).len(),2);
    assert_eq!(crate::web::routes::form_page(profiles::EDITOR),Some(network::MIHOMO));
}

#[tokio::test]
async fn mihomo_failures_keep_retry_controls_and_escape_error_text() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 238, 1).await);
    let _d = device::set(&c.app, device::Device {
        gets: vec![
            ("/api/mihomo/status", Ok(json!({"installed":true,"configured":true,"running":false,"error":"Core failed <safely>"}))),
            ("/api/mihomo/profiles", Ok(json!({"selected":"one","profiles":[
                {"id":"one","name":"Personal","remote":true,"error":"Subscription failed <safely>"},
                {"id":"two","name":"Other","remote":true,"error":"Other subscription failed"}
            ]}))),
        ],
        ..Default::default()
    });
    let page = get_with(&c, network::MIHOMO, peer(238, 2), &cookie).await;
    page.assert_secure_html(200);
    for text in ["Core failed &lt;safely&gt;", "Subscription failed &lt;safely&gt;", "Update now"] {
        assert!(page.text().contains(text), "{text}");
    }
    browser_fixture(&c, "mihomo-errors", &page).await;
}

#[tokio::test]
async fn remembered_wifi_and_fixed_ips_are_private_and_csrf_protected() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}",login(&c,252,1).await);
    let _d = device::set(&c.app,device::Device { gets:vec![
        ("/api/wifi-relay/status",Ok(json!({"networks":[{"id":"scan","ssid":"Saved Wi-Fi","security":"wpa2","supported":true,"remembered":true,"width_mode":"off"}],"saved_networks":[{"id":"saved","ssid":"Saved Wi-Fi","security":"wpa2","remembered":true,"width_mode":"off"}]}))),
        ("/api/router/reservations",Ok(json!({"entries":[{"id":"opaque","name":"Phone <test>","mac":"02:aa:bb:00:01:02","ip":"192.168.0.60"}]}))),
    ],..Default::default() });
    let page = get_with(&c,network::RELAY,peer(252,2),&cookie).await;
    assert!(page.text().contains("Remember this network") && page.text().contains("Saved Wi-Fi networks"));
    assert!(page.text().contains("<span class=\"badge gr\">Saved</span>"));
    browser_fixture(&c,"relay-saved",&page).await;
    let form = format!("csrf={}&once={}&id=saved",page.input("csrf"),page.input("once"));
    post(&c,network::FORGET,peer(252,2),&cookie,&form).await;
    assert_eq!(device::calls(&c.app)[0].0,"/api/wifi-relay/forget");
    let page = get_with(&c,crate::web::pages::reservations::PATH,peer(252,2),&cookie).await;
    page.assert_secure_html(200);
    assert!(page.text().contains("02:aa:bb:00:01:02"));
    assert!(page.text().contains("<div class=\"it row\"><span class=\"dn\">Phone &lt;test&gt;</span>"));
    assert!(page.text().contains("<form class=\"form\" method=\"post\" action=\"/ui/router/reservations/apply\">"));
    assert!(page.text().contains("<div class=\"row\"><code class=\"mono\">02:aa:bb:00:01:02</code><button class=\"btn btn-quiet bad\""));
    assert!(!page.text().contains("value=\"02:"));
    browser_fixture(&c,"fixed-client-ip",&page).await;
    let form = format!("csrf={}&once={}&action=delete&id=opaque",page.input("csrf"),page.input("once"));
    post(&c,crate::web::pages::reservations::APPLY,peer(252,2),&cookie,&form).await;
    assert_eq!(device::calls(&c.app)[1].0,"/api/router/reservations");
    assert_eq!(post(&c,network::FORGET,peer(252,2),&cookie,"id=saved").await.status,403);
    get(&c,crate::web::pages::reservations::PATH,peer(252,3)).await
        .assert_see_other("/ui/login?next=%2Fui%2Frouter%2Freservations");
}

#[tokio::test]
async fn wifi_displays_full_macs_on_page_and_live_fragment_only() {
    use crate::events::Topic;
    let (c, cookie, _) = signed_in(236).await;
    let data = json!({
        "wifi_onoff":"1", "radio2_disabled":"0", "ssid_2g":"Test Wi-Fi",
        "hosts": {
            "02:aa:bb:00:01:02":{"name":"Mac <test>","ipaddrs":["192.168.0.44"]},
            "02:aa:bb:00:01:03":{"name":"Device-02aabb000103","ipaddrs":["192.168.0.45"]}
        }
    });
    offer_all(&c, &data);
    let _fake = crate::web::pages::fake::set(&c.app, data);
    let page = get_with(&c,"/ui/wifi",peer(236,2),&cookie).await;
    page.assert_secure_html(200);
    for text in ["02:aa:bb:00:01:02", "Mac &lt;test&gt;", "Device-02aabb000103"] {
        assert!(page.text().contains(text), "missing {text}");
    }
    browser_fixture(&c,"wifi-full-macs",&page).await;
    let response = open_live(&c,&live_url(&page.text(),"wifi"),peer(236,3),&cookie).await;
    let mut stream = response.into_body().into_data_stream();
    let update = frame(&mut stream).await.unwrap();
    assert!(update.contains("02:aa:bb:00:01:02"));
    assert!(c.app.hub.demand(Topic::Clients) > 0);
    let other = get_with(&c,"/ui/tools/devices",peer(236,4),&cookie).await;
    assert!(!other.text().contains("02:aa:bb") && !other.text().contains("02aabb000103"));
    get(&c,"/ui/wifi",peer(236,5)).await.assert_see_other("/ui/login?next=%2Fui%2Fwifi");
}

#[tokio::test]
async fn fixed_client_ips_stay_in_lan_settings_not_the_relay_page() {
    use crate::web::pages::{lan, reservations};
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 239, 1).await);
    for status in [json!({"connected":false}), json!({"connected":true}), json!({"recovery_required":true})] {
        let _d = device::set(&c.app, device::Device {
            gets: vec![
                ("/api/wifi-relay/status", Ok(status)),
                ("/api/router/lan", Ok(json!({"lan_ipaddr":"192.168.0.1","lan_netmask":"255.255.255.0","dhcp_enable":"1","dhcp_start":"100","dhcp_end":"192.168.0.200","dhcp_lease_time":"12h"}))),
            ],
            ..Default::default()
        });
        let relay = get_with(&c, network::RELAY, peer(239, 2), &cookie).await;
        relay.assert_secure_html(200);
        assert!(!relay.text().contains(reservations::PATH));
        let lan = get_with(&c, lan::PATH, peer(239, 2), &cookie).await;
        lan.assert_secure_html(200);
        assert!(lan.text().contains(&format!("href=\"{}\"", reservations::PATH)));
        assert!(device::calls(&c.app).is_empty(), "Navigation must not change reservations");
    }
}

#[tokio::test]
async fn profile_file_upload_is_bounded_and_ignores_filename() {
    use crate::web::pages::mihomo_profiles as profiles;
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}",login(&c,253,1).await);
    let _d = devices(&c,false);
    let page = get_with(&c,network::MIHOMO,peer(253,2),&cookie).await;
    let multipart = |csrf:&str, once:&str, contents:&str| {
        let mut out = Vec::new();
        for (name,filename,content) in [("csrf",None,csrf),("once",None,once),("name",None,"Imported"),("yaml",Some("../../private.yaml"),contents)] {
            out.extend_from_slice(format!("--u60test\r\nContent-Disposition: form-data; name=\"{name}\"{}\r\n\r\n{content}\r\n",filename.map(|n|format!("; filename=\"{n}\"")).unwrap_or_default()).as_bytes());
        }
        out.extend_from_slice(b"--u60test--\r\n"); out
    };
    let data = multipart(&page.input("csrf"),&page.input("once"),"secret: private-node\nrules: ['MATCH,DIRECT']");
    let request = req("POST",profiles::FILE,peer(253,2)).header("cookie",&cookie).header("origin",ORIGIN)
        .header("content-type","multipart/form-data; boundary=u60test").body(Body::from(data)).unwrap();
    let response = send(&c,request).await;
    assert_eq!(response.status,303);
    assert!(!response.text().contains("private-node"));
    let calls = device::calls(&c.app);
    assert_eq!(calls.len(),1);
    assert_eq!(calls[0].1["yaml"],"secret: private-node\nrules: ['MATCH,DIRECT']");
    assert!(!calls[0].1.to_string().contains("../../"));
    let data = multipart("bad","bad",&"a".repeat(crate::mihomo::MAX_CONFIG + 8192));
    let request = req("POST",profiles::FILE,peer(253,2)).header("cookie",&cookie).header("origin",ORIGIN)
        .header("content-type","multipart/form-data; boundary=u60test").body(Body::from(data)).unwrap();
    assert_eq!(send(&c,request).await.status,413);
    assert_eq!(device::calls(&c.app).len(),1);
}

// Optional browser QA uses the real rendered replies and vendored assets,
// without starting router services or touching the host's /data directory.
async fn browser_fixture(c: &Ctx, name: &str, reply: &Reply) {
    let Ok(dir) = std::env::var("U60_WEB_QA_DIR") else { return };
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    let headers: Vec<_> = reply.headers.iter().map(|(k, v)| (k.as_str(), v.to_str().unwrap())).collect();
    std::fs::write(dir.join(format!("{name}.json")), serde_json::to_vec(&json!({"status":reply.status,"headers":headers,"html":reply.text()})).unwrap())
        .unwrap();
    for a in &ASSETS {
        let reply = get(c, a.path, peer(245, 1)).await;
        let path = dir.join(a.path.trim_start_matches('/'));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &reply.body).unwrap();
    }
}

#[tokio::test]
async fn router_switch_and_dashboard_visibility_follow_running_state() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 241, 1).await);
    let _d = devices(&c, false);
    let r = get_with(&c, "/ui/router", peer(241, 2), &cookie).await;
    r.assert_secure_html(200);
    assert!(r.text().contains("Clash proxy"));
    assert!(r.text().contains("<form method=\"post\" action=\"/ui/router/mihomo/switch\" data-net-switch>"));
    assert!(r.text().contains("<small data-net-state data-idle-label=\"Off\" role=\"status\">Off</small>"));
    assert!(r.text().contains("data-busy-label=\"Starting Clash...\""));
    assert!(!r.text().contains("Open Zashboard"));
    assert!(!r.text().contains("href=\"/ui/router/mihomo/open\""));
    browser_fixture(&c, "router-off", &r).await;
    let setup = get_with(&c, network::MIHOMO, peer(241, 2), &cookie).await;
    browser_fixture(&c, "mihomo-setup", &setup).await;
    let body = format!("csrf={}&once={}&enable=1", r.input("csrf"), r.input("once"));
    let on = post(&c, network::SWITCH, peer(241, 2), &cookie, &body).await;
    on.assert_see_other("/ui/router");
    assert_eq!(device::calls(&c.app), [("/api/mihomo/switch".into(), json!({"enabled":true}))]);
    post(&c, network::SWITCH, peer(241, 2), &cookie, &body).await;
    assert_eq!(device::calls(&c.app).len(), 1, "same token must not start twice");
    drop(_d);
    let _d = devices(&c, true);
    let r = get_with(&c, "/ui/router", peer(241, 2), &cookie).await;
    assert!(r.text().contains("Open Zashboard"));
    assert!(r.text().contains("data-idle-label=\"Running\" role=\"status\">Running</small>"));
    assert!(r.text().contains("data-busy-label=\"Stopping Clash...\""));
    browser_fixture(&c, "router-on", &r).await;
}

fn assert_current_restart_copy(page: &str) {
    for stale in [
        "Off after an agent restart or reboot.",
        "saved networks never auto-connect on boot",
        "Reboot or agent restart leaves relay off.",
        "Reconnects automatically",
    ] {
        assert!(!page.contains(stale), "Stale restart claim returned: {stale}");
    }
}

#[tokio::test]
async fn mihomo_restart_row_tracks_saved_intent_not_running_state() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 225, 1).await);
    for running in [false, true] {
        for (autostart, label) in [
            (json!(true), "Automatic start enabled"),
            (json!(false), "Stays off"),
            (Value::Null, "Unknown"),
        ] {
            let _d = device::set(&c.app, device::Device {
                gets: vec![("/api/mihomo/status", Ok(json!({"running":running,"autostart":autostart})))],
                ..Default::default()
            });
            let page = get_with(&c, network::MIHOMO, peer(225, 2), &cookie).await;
            page.assert_secure_html(200);
            let text = page.text();
            assert!(text.contains(&format!("<span>After a restart</span><span class=\"v\">{label}</span>")));
            assert!(text.contains(&format!("<span>Clash proxy</span><span class=\"v\">{}</span>", if running { "Running" } else { "Off" })));
            assert!(text.contains("Switching off clears that choice."));
            assert_eq!(text.contains("Stop automatic start &amp; keep off"), !running && autostart == json!(true));
            assert_current_restart_copy(&text);
        }
    }
}

#[tokio::test]
async fn dormant_restart_choices_have_one_guarded_stop_action_even_during_recovery() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 223, 1).await);
    for recovery in [false, true] {
        let _d = device::set(&c.app, device::Device {
            gets: vec![
                ("/api/mihomo/status", Ok(json!({"running":false,"autostart":true,"recovery_required":recovery}))),
                ("/api/wifi-relay/status", Ok(json!({"connected":false,"autostart":true,"recovery_required":recovery}))),
            ],
            ..Default::default()
        });
        let core = get_with(&c, network::MIHOMO, peer(223, 2), &cookie).await;
        core.assert_secure_html(200);
        let text = core.text();
        assert_eq!(text.matches(&format!("action=\"{}\"", network::SWITCH)).count(), 1);
        assert!(text.contains("name=\"enable\" value=\"0\""));
        assert_eq!(text.contains("Stop automatic start &amp; keep off"), !recovery);
        assert_eq!(text.contains("Retry proxy cleanup"), recovery);
        assert_eq!(post(&c, network::SWITCH, peer(223, 2), &cookie, "enable=0").await.status, 403);
        let body = format!("csrf={}&once={}&enable=0", core.input("csrf"), core.input("once"));
        for _ in 0..2 {
            post(&c, network::SWITCH, peer(223, 2), &cookie, &body).await.assert_see_other("/ui/router");
        }
        assert_eq!(device::calls(&c.app), [("/api/mihomo/switch".into(), json!({"enabled":false}))]);

        let relay = get_with(&c, network::RELAY, peer(223, 2), &cookie).await;
        relay.assert_secure_html(200);
        let text = relay.text();
        assert_eq!(text.matches(&format!("action=\"{}\"", network::DISCONNECT)).count(), 1);
        assert_eq!(text.contains("Disconnect &amp; clear restart choice"), !recovery);
        assert_eq!(text.contains("Retry relay cleanup"), recovery);
        assert!(text.contains("A restart connection choice is still saved."));
        assert_eq!(post(&c, network::DISCONNECT, peer(223, 2), &cookie, "").await.status, 403);
        let body = format!("csrf={}&once={}", relay.input("csrf"), relay.input("once"));
        for _ in 0..2 {
            post(&c, network::DISCONNECT, peer(223, 2), &cookie, &body).await.assert_see_other(network::RELAY);
        }
        assert_eq!(device::calls(&c.app), [
            ("/api/mihomo/switch".into(), json!({"enabled":false})),
            ("/api/wifi-relay/disconnect".into(), json!({})),
        ], "Stop actions must use existing handlers and single-use tokens");
    }
    let _d = device::set(&c.app, device::Device {
        gets: vec![("/api/wifi-relay/status", Ok(json!({"connected":false,"autostart":false})))],
        ..Default::default()
    });
    let relay = get_with(&c, network::RELAY, peer(223, 2), &cookie).await;
    assert!(!relay.text().contains(network::DISCONNECT), "No saved choice or cleanup means no stop action");
}

#[tokio::test]
async fn relay_connection_form_explains_resume_policy_before_connect() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 224, 1).await);
    let _d = devices(&c, false);
    let page = get_with(&c, network::RELAY, peer(224, 2), &cookie).await;
    page.assert_secure_html(200);
    let text = page.text();
    let connect_form = text.split("<form class=\"relay-connect\"").nth(1).unwrap().split("</form>").next().unwrap();
    for policy in [
        "After a successful connection, the last selected remembered or open network can reconnect after restart.",
        "Disconnect clears that choice",
        "a dropped link does not reconnect automatically while the agent runs",
    ] {
        assert!(connect_form.contains(policy), "Connection form omits policy: {policy}");
    }
    assert!(text.contains("Upgrading an older agent cannot infer that choice from saved credentials."));
    assert!(text.contains("while relay is off and no startup restoration is pending."));
    assert_current_restart_copy(&text);
    assert!(device::calls(&c.app).is_empty(), "Reading restart policy must not connect");
}

#[tokio::test]
async fn standalone_relay_tab_escapes_ssids_and_never_echoes_passwords() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 242, 1).await);
    let _d = devices(&c, false);
    let r = get_with(&c, network::RELAY, peer(242, 2), &cookie).await;
    r.assert_secure_html(200);
    assert!(r.text().contains("href=\"/ui/relay\" aria-current=\"page\""));
    assert!(r.text().contains("&lt;script&gt;hotel&lt;/script&gt;"));
    assert!(!r.text().contains("<script>hotel"));
    let body = format!("csrf={}&once={}&network=1234567890abcdef&password=private_wifi_password&confirmed=1", r.input("csrf"), r.input("once"));
    let sent = post(&c, network::CONNECT, peer(242, 2), &cookie, &body).await;
    assert_eq!(sent.status, 303);
    assert!(!sent.text().contains("private_wifi_password"));
    let calls = device::calls(&c.app);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1["password"], "private_wifi_password");
    assert_eq!(calls[0].1["confirmed"], true);
    assert_eq!(calls[0].1["ipv6"], false);
}

#[tokio::test]
async fn relay_connect_options_default_on_but_unchecked_choices_are_respected() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 235, 1).await);
    let _d = devices(&c, false);
    let page = get_with(&c, network::RELAY, peer(235, 2), &cookie).await;
    for name in ["remember", "ipv6", "confirmed"] {
        let input = format!("<input type=\"checkbox\" name=\"{name}\" value=\"1\"");
        let attributes = page.text().split(&input).nth(1).unwrap().split('>').next().unwrap().to_owned();
        assert!(attributes.split_whitespace().any(|a| a == "checked"), "{name} must default on");
    }
    let form = format!("csrf={}&once={}&network=1234567890abcdef&confirmed=1&ipv6=1&remember=1", page.input("csrf"), page.input("once"));
    assert_eq!(post(&c, network::CONNECT, peer(235, 2), &cookie, &form).await.status, 303);
    for name in ["remember", "ipv6", "confirmed"] { assert_eq!(device::calls(&c.app)[0].1[name], true); }

    let page = get_with(&c, network::RELAY, peer(235, 2), &cookie).await;
    let form = format!("csrf={}&once={}&network=1234567890abcdef&confirmed=1", page.input("csrf"), page.input("once"));
    assert_eq!(post(&c, network::CONNECT, peer(235, 2), &cookie, &form).await.status, 303);
    for name in ["remember", "ipv6"] { assert_eq!(device::calls(&c.app)[1].1[name], false); }

    let page = get_with(&c, network::RELAY, peer(235, 2), &cookie).await;
    let form = format!("csrf={}&once={}&network=1234567890abcdef&ipv6=1&remember=1", page.input("csrf"), page.input("once"));
    assert_eq!(post(&c, network::CONNECT, peer(235, 2), &cookie, &form).await.status, 400);
    assert_eq!(device::calls(&c.app).len(), 2, "unchecked consent must not connect");
}

fn relay_form_with<'a>(html: &'a str, marker: &str) -> &'a str {
    html.split("<form").skip(1)
        .map(|form| form.split("</form>").next().unwrap())
        .find(|form| form.contains(marker))
        .unwrap_or_else(|| panic!("No form containing {marker}"))
}

#[tokio::test]
async fn relay_width_forms_show_current_and_saved_choices_without_exposing_credentials() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 220, 1).await);
    for (active, saved) in [("off", "auto"), ("auto", "off")] {
        let _d = device::set(&c.app, device::Device {
            gets: vec![("/api/wifi-relay/status", Ok(json!({
                "connected":true,"ssid":"Hotel <test>","network_id":"scan-current","saved_network_id":"saved-current","width_mode":active,
                "networks":[
                    {"id":"scan-current","ssid":"Hotel <test>","security":"wpa2","supported":true,"remembered":false,"width_mode":saved},
                    {"id":"scan-other","ssid":"Hotel <test>","security":"wpa2","supported":true,"remembered":true,"width_mode":"off"}
                ],
                "saved_networks":[
                    {"id":"saved-current","ssid":"Hotel <test>","security":"wpa2","remembered":false,"width_mode":saved},
                    {"id":"saved-other","ssid":"Hotel <test>","security":"wpa2","remembered":true,"width_mode":"off","password":"must-not-render"},
                    {"id":"saved-open","ssid":"Lobby","security":"open","remembered":true,"width_mode":"off"}
                ]
            })))],
            ..Default::default()
        });
        let page = get_with(&c, network::RELAY, peer(220, 2), &cookie).await;
        page.assert_secure_html(200);
        let html = page.text();
        let connect = relay_form_with(&html, &format!("action=\"{}\"", network::CONNECT));
        assert!(connect.contains("<option value=\"saved\" selected>Use saved choice</option>"));
        assert_eq!(connect.matches("<select").count(), 1);
        assert!(connect.contains("Saved bandwidth: Keep negotiated bandwidth"));
        assert!(connect.contains("Each Wi-Fi remembers its choice."));
        assert!(connect.find("name=\"confirmed\"").unwrap() < connect.find("id=\"relay-connect-width\"").unwrap());
        assert!(connect.find("id=\"relay-connect-width\"").unwrap() < connect.find("Connect to Wi-Fi").unwrap());
        let current = relay_form_with(&html, "id=\"relay-active-width\"");
        assert!(current.contains(&format!("<option value=\"{active}\" selected>")));
        assert!(current.contains("name=\"id\" value=\"saved-current\""));
        assert!(!current.contains("scan-current"), "Tuning requires the persistent ID, not the scan ID");
        assert!(current.contains("Applying reconnects this Wi-Fi and briefly interrupts internet access."));
        assert!(current.contains("name=\"confirmed\" value=\"1\" checked required"));
        let stored = relay_form_with(&html, "id=\"relay-saved-width-0\"");
        assert!(stored.contains(&format!("<option value=\"{saved}\" selected>")), "Stored policy must not be replaced by the active mode");
        assert!(stored.contains("Applying reconnects this Wi-Fi"));
        let other = relay_form_with(&html, "id=\"relay-saved-width-1\"");
        assert!(other.contains("<option value=\"off\" selected>Keep negotiated bandwidth</option>"));
        assert!(other.contains("Saved for the next connection."));
        assert!(!other.contains("Applying reconnects this Wi-Fi"), "Equal SSIDs must not mark a different persistent network active");
        assert_eq!(html.matches(&format!("action=\"{}\"", network::TUNING)).count(), 4);
        for text in ["Hotel &lt;test&gt;", "Password not saved; enter it when connecting", "Password remembered; leave it empty when connecting", "Open network; no password needed", "Auto tuning"] {
            assert!(html.contains(text), "{text}");
        }
        assert!(!html.contains("Hotel <test>") && !html.contains("must-not-render"));
        assert!(device::calls(&c.app).is_empty(), "Rendering policies must not save or reconnect");
        browser_fixture(&c, &format!("relay-width-{active}"), &page).await;
    }
}

#[tokio::test]
async fn relay_connect_forwards_explicit_width_or_defers_to_the_saved_choice() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 219, 1).await);
    let _d = devices(&c, false);
    for (index, mode) in [None, Some("saved"), Some("off"), Some("auto")].into_iter().enumerate() {
        let page = get_with(&c, network::RELAY, peer(219, 2), &cookie).await;
        let mut body = format!("csrf={}&once={}&network=1234567890abcdef&password=private-width-password&confirmed=1&ipv6=1", page.input("csrf"), page.input("once"));
        if let Some(mode) = mode { body.push_str(&format!("&width_mode={mode}")); }
        let sent = post(&c, network::CONNECT, peer(219, 2), &cookie, &body).await;
        assert_eq!(sent.status, 303);
        assert!(sent.header("location").unwrap().starts_with("/ui/relay?r="));
        assert!(!sent.text().contains("private-width-password"));
        post(&c, network::CONNECT, peer(219, 2), &cookie, &body).await;
        let calls = device::calls(&c.app);
        assert_eq!(calls.len(), index + 1, "Replaying the form must not reconnect twice");
        assert_eq!(calls[index], ("/api/wifi-relay/connect".into(), json!({
            "network":"1234567890abcdef","password":"private-width-password","confirmed":true,"ipv6":true,"remember":false,
            "width_mode":mode.filter(|mode| *mode != "saved")
        })));
    }
}

#[tokio::test]
async fn relay_tuning_forwards_persistent_id_and_policy_once_with_no_js_feedback() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 218, 1).await);
    let _d = device::set(&c.app, device::Device {
        gets: vec![("/api/wifi-relay/status", Ok(json!({
            "connected":true,"network_id":"scan-current","saved_network_id":"saved-current","width_mode":"off",
            "saved_networks":[{"id":"saved-other","ssid":"Other","remembered":false,"width_mode":"auto"}]
        })))],
        ..Default::default()
    });
    for (index, (select, id, mode)) in [
        ("relay-active-width", "saved-current", "auto"),
        ("relay-saved-width-0", "saved-other", "off"),
    ].into_iter().enumerate() {
        let page = get_with(&c, network::RELAY, peer(218, 2), &cookie).await;
        let html = page.text();
        let form = relay_form_with(&html, &format!("id=\"{select}\""));
        let field = |name| form.split(&format!("name=\"{name}\" value=\"")).nth(1).unwrap().split('"').next().unwrap().to_owned();
        assert_eq!(field("id"), id);
        let body = format!("csrf={}&once={}&id={}&width_mode={mode}&confirmed={}", field("csrf"), field("once"), field("id"), field("confirmed"));
        let sent = post(&c, network::TUNING, peer(218, 2), &cookie, &body).await;
        assert_eq!(sent.status, 303);
        let location = sent.header("location").unwrap();
        assert!(location.starts_with("/ui/relay?r="));
        post(&c, network::TUNING, peer(218, 2), &cookie, &body).await;
        let calls = device::calls(&c.app);
        assert_eq!(calls.len(), index + 1);
        assert_eq!(calls[index], ("/api/wifi-relay/tuning".into(), json!({"id":id,"width_mode":mode,"confirmed":true})));
        let result = get_with(&c, location, peer(218, 2), &cookie).await;
        result.assert_secure_html(200);
        assert!(result.text().contains("Wi-Fi bandwidth choice saved."));
    }
}

#[tokio::test]
async fn relay_width_posts_reject_invalid_modes_and_missing_confirmation_before_calling() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 217, 1).await);
    let _d = devices(&c, false);
    let page = get_with(&c, network::RELAY, peer(217, 2), &cookie).await;
    let tokens = format!("csrf={}&once={}&network=scan&id=saved", page.input("csrf"), page.input("once"));
    for path in [network::CONNECT, network::TUNING] {
        for mode in ["observe", "unexpected", "", "<script>bad</script>"] {
            let body = format!("{tokens}&confirmed=1&width_mode={}", enc(mode));
            let refused = post(&c, path, peer(217, 2), &cookie, &body).await;
            refused.assert_secure_html(400);
            assert!(refused.text().contains("Invalid bandwidth choice"));
            assert!(!refused.text().contains("<script>bad</script>"));
        }
        let body = format!("{tokens}&width_mode=off");
        let refused = post(&c, path, peer(217, 2), &cookie, &body).await;
        refused.assert_secure_html(400);
        assert!(refused.text().contains("Confirmation required"));
    }
    for suffix in ["", "&width_mode=saved"] {
        let body = format!("{tokens}&confirmed=1{suffix}");
        assert_eq!(post(&c, network::TUNING, peer(217, 2), &cookie, &body).await.status, 400);
    }
    assert!(device::calls(&c.app).is_empty());
    let valid = format!("{tokens}&confirmed=1&width_mode=off");
    assert_eq!(post(&c, network::TUNING, peer(217, 2), &cookie, &valid).await.status, 303);
    assert_eq!(device::calls(&c.app).len(), 1, "Validation must not consume the once token");
}

#[tokio::test]
async fn relay_tuning_requires_csrf_and_a_once_token_before_saving_or_reconnecting() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 216, 1).await);
    let _d = devices(&c, false);
    let page = get_with(&c, network::RELAY, peer(216, 2), &cookie).await;
    let choice = "id=saved&width_mode=auto&confirmed=1";
    post(&c, network::TUNING, peer(216, 2), "", choice).await
        .assert_see_other("/ui/login?next=%2Fui%2Frelay&expired=1");
    for csrf in [None, Some("invalid")] {
        let mut body = format!("once={}&{choice}", page.input("once"));
        if let Some(csrf) = csrf { body.push_str(&format!("&csrf={csrf}")); }
        let refused = post(&c, network::TUNING, peer(216, 2), &cookie, &body).await;
        refused.assert_secure_html(403);
        assert!(refused.text().contains("href=\"/ui/relay\""));
    }
    let body = format!("csrf={}&{choice}", page.input("csrf"));
    assert_eq!(post(&c, network::TUNING, peer(216, 2), &cookie, &body).await.status, 303);
    assert!(device::calls(&c.app).is_empty());
    let body = format!("{body}&once={}", page.input("once"));
    assert_eq!(post(&c, network::TUNING, peer(216, 2), &cookie, &body).await.status, 303);
    assert_eq!(device::calls(&c.app).len(), 1);
}

#[tokio::test]
async fn relay_ipv6_consent_and_nat66_state_are_explicit() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 247, 1).await);
    let _d = devices(&c, false);
    let r = get_with(&c, network::RELAY, peer(247, 2), &cookie).await;
    assert!(r.text().contains("name=\"ipv6\" value=\"1\" checked"));
    let body = format!("csrf={}&once={}&network=1234567890abcdef&confirmed=1&ipv6=1",r.input("csrf"),r.input("once"));
    assert_eq!(post(&c,network::CONNECT,peer(247,2),&cookie,&body).await.status,303);
    assert_eq!(device::calls(&c.app)[0].1["ipv6"],true);
    drop(_d);
    let _d=device::set(&c.app,device::Device{gets:vec![("/api/wifi-relay/status",Ok(json!({"connected":true,"ssid":"Hotel","ipv6":{"state":"configured","upstream":true,"nat66":true,"address":"2001:db8::2","lan_prefix":"fd12:3456:789a::/64"}})))],..Default::default()});
    let r=get_with(&c,network::RELAY,peer(247,2),&cookie).await;
    assert!(r.text().contains("Configured (internet not verified)"));
    assert!(r.text().contains("fd12:3456:789a::/64"));
    browser_fixture(&c,"relay-ipv6",&r).await;
}

#[tokio::test]
async fn relay_shows_upstream_width_with_each_controller_state() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 248, 1).await);
    for (frequency, width, band) in [(2412, 20, "2.4 GHz"), (5220, 160, "5 GHz")] {
        for state in ["From history", "Waiting for traffic", "Comparing", "Settled", "Not enough evidence", "Cannot confirm", "Paused: hotspot changed", "Observing"] {
            let status = json!({"connected":true,"frequency":frequency,"width":width,"width_state":state});
            let _d = device::set(&c.app, device::Device { gets: vec![("/api/wifi-relay/status", Ok(status))], ..Default::default() });
            let r = get_with(&c, network::RELAY, peer(248, 2), &cookie).await;
            r.assert_secure_html(200);
            let shown = format!("<span>{band} \u{b7} {width} MHz \u{b7} {state}</span>");
            assert!(r.text().contains("UPSTREAM LINK") && r.text().contains(&shown), "{shown}");
            assert!(!r.text().contains("chosen automatically"));
        }
    }
}

#[tokio::test]
async fn relay_shows_only_band_and_width_without_a_string_state() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 248, 1).await);
    for (status, shown) in [
        (json!({"connected":true,"frequency":2412,"width":20}), "2.4 GHz \u{b7} 20 MHz"),
        (json!({"connected":true,"frequency":5220,"width":160,"width_state":null}), "5 GHz \u{b7} 160 MHz"),
        (json!({"connected":true,"frequency":2412,"width":40,"width_state":42}), "2.4 GHz \u{b7} 40 MHz"),
    ] {
        let _d = device::set(&c.app, device::Device { gets: vec![("/api/wifi-relay/status", Ok(status))], ..Default::default() });
        let r = get_with(&c, network::RELAY, peer(248, 2), &cookie).await;
        assert!(r.text().contains("UPSTREAM LINK") && r.text().contains(&format!("<span>{shown}</span>")), "{shown}");
        assert!(!r.text().contains("chosen automatically"));
    }
}

#[tokio::test]
async fn relay_hides_width_state_without_a_connected_link_width() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 248, 1).await);
    for status in [
        json!({"connected":true,"frequency":2437,"width_state":"Comparing"}),
        json!({"connected":true,"frequency":2437,"width":null,"width_state":"Comparing"}),
        json!({"connected":true,"width":20,"width_state":"Comparing"}),
        json!({"connected":false,"frequency":2437,"width":20,"width_state":"Comparing"}),
    ] {
        let _d = device::set(&c.app, device::Device { gets: vec![("/api/wifi-relay/status", Ok(status))], ..Default::default() });
        let r = get_with(&c, network::RELAY, peer(248, 2), &cookie).await;
        assert!(!r.text().contains("UPSTREAM LINK"));
        assert!(!r.text().contains("Comparing"));
    }
}

#[tokio::test]
async fn relay_width_state_is_escaped_as_plain_text() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 248, 1).await);
    let status = json!({"connected":true,"frequency":2412,"width":20,"width_state":"<script>test & state</script>"});
    let _d = device::set(&c.app, device::Device { gets: vec![("/api/wifi-relay/status", Ok(status))], ..Default::default() });
    let r = get_with(&c, network::RELAY, peer(248, 2), &cookie).await;
    assert!(r.text().contains("2.4 GHz \u{b7} 20 MHz \u{b7} &lt;script&gt;test &amp; state&lt;/script&gt;"));
    assert!(!r.text().contains("<script>test & state</script>"));
}

#[tokio::test]
async fn relay_says_whether_it_comes_back_after_a_restart_and_can_stop_reconnecting() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 250, 1).await);
    for (autostart, text) in [
        (json!(true), "Automatic reconnect enabled"),
        (json!(false), "Stays off; no restart choice saved"),
        (Value::Null, "Unknown"),
    ] {
        let status = json!({"connected":true,"ssid":"W","address":"192.168.2.124","autostart":autostart});
        let _d = device::set(&c.app, device::Device { gets: vec![("/api/wifi-relay/status", Ok(status))], ..Default::default() });
        let r = get_with(&c, network::RELAY, peer(250, 2), &cookie).await;
        assert!(r.text().contains("AFTER A RESTART") && r.text().contains(text), "{text}");
        assert_current_restart_copy(&r.text());
    }
    let status = json!({"connected":false,"restoring":"W <test>","autostart":true});
    let _d = device::set(&c.app, device::Device { gets: vec![("/api/wifi-relay/status", Ok(status))], ..Default::default() });
    let r = get_with(&c, network::RELAY, peer(250, 2), &cookie).await;
    let page = r.text();
    assert!(page.contains("Reconnecting") && page.contains(">W &lt;test&gt;<"), "the network being restored is named and escaped");
    assert!(page.contains("Stop reconnecting cancels this attempt and clears the restart choice."));
    assert!(page.contains("Stop reconnecting</button>"), "the owner can stop a restore");
    assert!(!page.contains("AFTER A RESTART"), "no link rows while not connected");
    assert_current_restart_copy(&page);
    let body = format!("csrf={}&once={}", r.input("csrf"), r.input("once"));
    post(&c, network::DISCONNECT, peer(250, 2), &cookie, &body).await.assert_see_other(network::RELAY);
    assert_eq!(device::calls(&c.app), [("/api/wifi-relay/disconnect".into(), json!({}))]);
}

#[tokio::test]
async fn relay_ipv6_unavailable_states_never_claim_internet_or_nat66_success() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 248, 1).await);
    for (state, label) in [
        ("detecting", "Discovering upstream IPv6"),
        ("no_upstream", "No usable upstream IPv6; IPv4 only"),
        ("unsupported", "Unavailable; IPv4 only"),
        ("expired", "Upstream IPv6 expired; IPv4 only"),
        ("error", "IPv6 setup failed; IPv4 only"),
        ("off", "Off"),
    ] {
        let _d = device::set(&c.app, device::Device {
            gets: vec![("/api/wifi-relay/status", Ok(json!({"connected":true,"ssid":"Hotel","ipv6":{"state":state,"upstream":false,"nat66":false}})))],
            ..Default::default()
        });
        let r = get_with(&c, network::RELAY, peer(248, 2), &cookie).await;
        assert!(r.text().contains(label), "{state}");
        assert!(!r.text().contains("Configured (internet not verified)"));
        assert!(!r.text().contains("LAN IPv6 PREFIX"));
    }
}

#[tokio::test]
async fn relay_redesign_keeps_six_named_tabs_and_explicit_network_selection() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 246, 1).await);
    let _d = device::set(
        &c.app,
        device::Device {
            gets: vec![(
                "/api/wifi-relay/status",
                Ok(json!({"connected":false,"networks":[
                    {"id":"hotel24","ssid":"JinJiangRewards","frequency":2427,"signal":-43,"security":"open","supported":true},
                    {"id":"hotel5","ssid":"JinJiangRewards","frequency":5180,"signal":-58,"security":"open","supported":true},
                    {"id":"office","ssid":"Office Guest","frequency":5200,"signal":-67,"security":"wpa2","supported":true},
                    {"id":"wpa3","ssid":"Private Network","frequency":5220,"signal":-74,"security":"unsupported","supported":false}
                ]})),
            )],
            ..Default::default()
        },
    );
    let r = get_with(&c, network::RELAY, peer(246, 2), &cookie).await;
    r.assert_secure_html(200);
    let html = r.text();
    assert_eq!(html.matches("type=\"radio\" name=\"network\"").count(), 4);
    assert!(!html.contains("<span class=\"badge gr\">Saved</span>"));
    assert!(html.contains("value=\"wpa3\" required disabled"));
    assert!(!html.contains("<select name=\"network\""), "Network selection remains explicit radio cards");
    assert_eq!(html.matches("<select").count(), 1, "Only bandwidth uses a selector");
    assert!(html.contains("class=\"relay-btn relay-primary\""));
    assert!(html.contains("<details class=\"relay-notes\"><summary>Compatibility &amp; safety</summary>"));
    for label in ["Dashboard", "SMS", "Tools", "Router", "Settings"] {
        assert!(html.contains(&format!("title=\"{label}\"")), "icon-only tabs must keep accessible names and hover titles");
    }
    browser_fixture(&c, "relay-scan", &r).await;
    drop(_d);
    let _d = device::set(&c.app, device::Device { gets: vec![("/api/wifi-relay/status", Ok(json!({"connected":false,"networks":[]})))], ..Default::default() });
    let r = get_with(&c, network::RELAY, peer(246, 2), &cookie).await;
    assert!(r.text().contains("Your next connection starts here."));
    assert!(!r.text().contains("action=\"/ui/relay/connect\""));
    browser_fixture(&c, "relay-empty", &r).await;
}

#[tokio::test]
async fn relay_clock_recheck_clears_warning_with_success_feedback_and_runs_once() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 228, 1).await);
    let warning = "Cannot verify UTC over Wi-Fi HTTPS; check upstream/portal access";
    let d = device::set(&c.app, device::Device {
        gets: vec![("/api/wifi-relay/status", Ok(json!({"connected":true,"ssid":"W","address":"192.168.2.124","clock_error":warning})))],
        ..Default::default()
    });
    let page = get_with(&c, network::RELAY, peer(228, 2), &cookie).await;
    page.assert_secure_html(200);
    assert!(page.text().contains("<div class=\"msg info\" role=\"status\"><strong>Wi-Fi connected; time not verified.</strong>"));
    assert!(page.text().contains("data-busy-label=\"Verifying time over Wi-Fi...\""));
    assert!(page.text().contains("This check does not reconnect Wi-Fi."));
    assert!(!page.text().contains("Clock check:"));
    browser_fixture(&c, "relay-clock-warning", &page).await;
    let body = format!("csrf={}&once={}", page.input("csrf"), page.input("once"));
    let sent = post(&c, network::CLOCK, peer(228, 2), &cookie, &body).await;
    assert_eq!(sent.status, 303);
    let location = sent.header("location").unwrap().to_owned();
    assert!(location.starts_with("/ui/relay?r="));
    assert_eq!(post(&c, network::CLOCK, peer(228, 2), &cookie, &body).await.status, 303);
    assert_eq!(device::calls(&c.app), [("/api/wifi-relay/clock".into(), json!({}))]);
    drop(d);
    let _d = device::set(&c.app, device::Device {
        gets: vec![("/api/wifi-relay/status", Ok(json!({"connected":true,"ssid":"W","address":"192.168.2.124","clock_error":null})))],
        ..Default::default()
    });
    let result = get_with(&c, &location, peer(228, 2), &cookie).await;
    result.assert_secure_html(200);
    assert!(result.text().contains("Time verified over Wi-Fi. Relay connection was not restarted."));
    assert!(!result.text().contains(warning));
    assert!(!result.text().contains("Recheck time"));
    assert!(result.text().contains("Disconnect relay"));
    browser_fixture(&c, "relay-clock-success", &result).await;
}

#[tokio::test]
async fn relay_clock_failure_keeps_connection_and_shows_escaped_reason() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 227, 1).await);
    let error = "Clock sources disagree <test>";
    let _d = device::set(&c.app, device::Device {
        gets: vec![("/api/wifi-relay/status", Ok(json!({"connected":true,"ssid":"W","clock_error":error})))],
        replies: vec![("/api/wifi-relay/clock", (503, json!({"ok":false,"error":error})))],
        ..Default::default()
    });
    let page = get_with(&c, network::RELAY, peer(227, 2), &cookie).await;
    let body = format!("csrf={}&once={}", page.input("csrf"), page.input("once"));
    let sent = post(&c, network::CLOCK, peer(227, 2), &cookie, &body).await;
    assert_eq!(sent.status, 303);
    let result = get_with(&c, sent.header("location").unwrap(), peer(227, 2), &cookie).await;
    result.assert_secure_html(200);
    assert!(result.text().contains("Clock sources disagree &lt;test&gt;"));
    assert!(!result.text().contains("Clock sources disagree <test>"));
    assert!(result.text().contains("Wi-Fi connected; time not verified."));
    assert!(result.text().contains("Recheck time"));
    assert!(result.text().contains("Disconnect relay"));
    assert_ne!(result.input("once"), page.input("once"));
    post(&c, network::CLOCK, peer(227, 2), &cookie, &body).await;
    assert_eq!(device::calls(&c.app).len(), 1, "A failed check must not be silently retried");
    browser_fixture(&c, "relay-clock-failure", &result).await;
}

#[tokio::test]
async fn relay_clock_action_is_guarded_while_off_and_requires_once_token() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 226, 1).await);
    let _d = device::set(&c.app, device::Device {
        gets: vec![("/api/wifi-relay/status", Ok(json!({"connected":false,"clock_error":"Previous warning"})))],
        ..Default::default()
    });
    let page = get_with(&c, network::RELAY, peer(226, 2), &cookie).await;
    assert!(!page.text().contains("Previous warning"));
    assert!(!page.text().contains("Recheck time"));
    post(&c, network::CLOCK, peer(226, 2), &cookie, &format!("csrf={}", page.input("csrf"))).await;
    assert!(device::calls(&c.app).is_empty());
    let api = "/api/wifi-relay/clock";
    assert!(matches!(crate::server::lane(crate::server::Method::Post, api), crate::httpd::gate::Lane::Slow));
    let bearer = c.app.auth.login(PW, Kind::Bearer, PeerKey::from(peer(226, 2).0), &[]).unwrap();
    let response = send(&c, req("POST", api, peer(226, 2))
        .header("authorization", format!("Bearer {}", bearer.token.expose())).body(Body::from("{}")).unwrap()).await;
    assert_eq!(response.status, 503);
    let result: Value = serde_json::from_str(&response.text()).unwrap();
    assert_eq!(result["error"], "Connect Wi-Fi relay before checking time");
    assert_eq!(send(&c, req("POST", api, peer(226, 2)).body(Body::from("{}")).unwrap()).await.status, 401);
    let no_password = ctx(None);
    assert_eq!(send(&no_password, req("POST", api, peer(226, 2)).body(Body::from("{}")).unwrap()).await.status, 403);
}

#[tokio::test]
async fn connected_relay_keeps_authentication_notice_without_a_hotel_login_card() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 245, 1).await);
    let _d = device::set(
        &c.app,
        device::Device {
            gets: vec![("/api/wifi-relay/status", Ok(json!({"connected":true,"ssid":"JinJiangRewards","address":"10.20.0.2"})))],
            ..Default::default()
        },
    );
    let r = get_with(&c, network::RELAY, peer(245, 2), &cookie).await;
    r.assert_secure_html(200);
    assert!(r.text().contains("Connected (internet not verified)"));
    assert!(r.text().contains("Network authentication may still be required."));
    for removed in ["portal-heading", "Hotel Wi-Fi?", "Open Wi-Fi login", "neverssl.com", "turn Clash off"] {
        assert!(!r.text().contains(removed), "Removed portal UI returned: {removed}");
    }
    for retained in ["10.20.0.2", "Disconnect relay", "Upstream networks", "cellular"] {
        assert!(r.text().contains(retained), "Relay control or state missing: {retained}");
    }
    browser_fixture(&c, "relay-connected", &r).await;
}

#[tokio::test]
async fn network_posts_require_session_csrf_origin_and_once_token() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 243, 1).await);
    let _d = devices(&c, false);
    for path in [network::CONFIG, network::CONNECT, network::CLOCK, network::TUNING] {
        assert_eq!(post(&c, path, peer(243, 2), "", "yaml=x").await.status, 303);
        assert_eq!(post(&c, path, peer(243, 2), &cookie, "yaml=x").await.status, 403);
        let page = get_with(&c, if path == network::CONFIG { network::MIHOMO } else { network::RELAY }, peer(243, 2), &cookie).await;
        let data = format!("csrf={}&once={}&confirmed=1&yaml=x", page.input("csrf"), page.input("once"));
        let request = form(path, peer(243, 2), &cookie, &data).header("origin", "http://evil.invalid").body(Body::from(data)).unwrap();
        assert_eq!(send(&c, request).await.status, 403);
    }
    assert!(device::calls(&c.app).is_empty());
}

#[tokio::test]
async fn disabled_dashboard_is_not_reachable_even_by_old_links() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 244, 1).await);
    assert_eq!(get_with(&c, "/ui/mihomo/dashboard/", peer(244, 2), &cookie).await.status, 409);
    assert_eq!(get_with(&c, "/ui/mihomo/api/version", peer(244, 2), &cookie).await.status, 409);
    get_with(&c, network::OPEN, peer(244, 2), &cookie).await.assert_see_other("/ui/router");
    assert_eq!(get(&c, "/ui/mihomo/api/version", peer(244, 2)).await.status, 401);
}

#[tokio::test]
async fn retained_network_ownership_has_explicit_cleanup_actions_without_reconnecting() {
    let c = ctx(Some(PW));
    let cookie = format!("zte_ui_session={}", login(&c, 249, 1).await);
    let _d = device::set(&c.app, device::Device {
        gets: vec![
            ("/api/wifi-relay/status", Ok(json!({"connected":false,"recovery_required":true,"error":"Cleanup incomplete","networks":[]}))),
            ("/api/mihomo/status", Ok(json!({"running":false,"installed":true,"configured":true,"recovery_required":true,"error":"Cleanup incomplete"}))),
        ],
        ..Default::default()
    });
    let relay = get_with(&c, network::RELAY, peer(249, 2), &cookie).await;
    assert!(relay.text().contains("Recovery required"));
    assert!(relay.text().contains("Retry relay cleanup"));
    assert!(!relay.text().contains("Relay off"));
    let body = format!("csrf={}&once={}", relay.input("csrf"), relay.input("once"));
    post(&c, network::DISCONNECT, peer(249, 2), &cookie, &body).await.assert_see_other(network::RELAY);
    assert_eq!(device::calls(&c.app), [("/api/wifi-relay/disconnect".into(), json!({}))]);
    let core = get_with(&c, network::MIHOMO, peer(249, 2), &cookie).await;
    assert!(core.text().contains("Retry proxy cleanup"));
    let body = format!("csrf={}&once={}&enable=0", core.input("csrf"), core.input("once"));
    post(&c, network::SWITCH, peer(249, 2), &cookie, &body).await.assert_see_other("/ui/router");
    assert_eq!(device::calls(&c.app).last().unwrap(), &("/api/mihomo/switch".into(), json!({"enabled":false})));
}

#[test]
fn conflicting_device_setters_refuse_during_network_transactions() {
    let app = AppState::new();
    let _network = app.network_lock.lock().unwrap();
    for path in ["/api/wifi/settings", "/api/wifi/guest", "/api/router/lan", "/api/usb/mode", "/api/router/firewall/switch", "/api/router/domain-filter"] {
        let (status, _) = crate::server::route(&crate::server::Method::Put, path, &app, b"{}");
        assert_eq!(status, 409, "{path}");
    }
}
