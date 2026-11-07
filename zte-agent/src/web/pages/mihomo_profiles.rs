//! Private profile editor and explicit subscription URL disclosure. Requests
//! never put source YAML or subscription URLs in browser navigation URLs.

use axum::extract::Request;
use axum::http::Method;
use axum::response::Response;
use maud::{html, Markup};
use serde_json::{json, Value};
use std::sync::Arc;

use super::network::{flash_line, MIHOMO};
use crate::handlers::AppState;
use crate::httpd::{api, gate::Lane, guard, Ctx};
use crate::web::{
    job::{self, OnceJob, Sent},
    kit,
    render::{self, once_field},
    screen::PageData,
    session::{self, Form},
    store::{Token, SETTLE_SLOW},
    upload, CspNonce,
};

pub const CHANGE: &str = "/ui/router/mihomo/profiles";
pub const FILE: &str = "/ui/router/mihomo/file";
pub const EDITOR: &str = "/ui/router/mihomo/editor";
const EXTENSIONS_PAGE: &str = "/ui/router/mihomo?extensions=1";
const EXTENSIONS_SAVED: &str = "Global Merge & Script saved. All profiles validated and the changes were applied. Clash stays off if it was off.";

fn schedule(id: &str, current: u64) -> Markup {
    html! {
        select.net-control id=(id) name="interval_hours" {
            @for (hours, label) in [(0,"Manual only"),(1,"Every hour"),(6,"Every 6 hours"),(12,"Every 12 hours"),(24,"Every day"),(72,"Every 3 days"),(168,"Every week")] {
                option value=(hours) selected[hours == current] { (label) }
            }
        }
    }
}

pub fn editor_read(app: &AppState, query: Option<&str>) -> Result<Value, String> {
    let id = session::query_param(query, "edit").or_else(|| session::query_param(query, "delete"));
    let Some(id) = id else {
        return Ok(Value::Null);
    };
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Invalid profile ID".into());
    }
    #[cfg(test)]
    if let Some(result) = super::device::get(app, "/ui/test/profile-editor") {
        return result;
    }
    app.mihomo.profile_editor(&id)
}

pub fn extensions_read(app: &AppState, _: Option<&str>) -> Result<Value, String> {
    #[cfg(test)]
    if let Some(result) = super::device::get(app, "/ui/test/extensions-editor") {
        return result;
    }
    app.mihomo.extensions_editor()
}

pub fn panel(pd: &PageData) -> Markup {
    let Some(data) = pd.read(1) else {
        return html! { p.msg.err { "Profile library unavailable" } };
    };
    let hidden = || html! { (pd.csrf()) (once_field(&pd.once())) };
    let editor = pd.read(2).filter(|v| !v.is_null());
    let adding = pd.param("add").as_deref() == Some("1");
    let deleting = pd.param("delete").is_some();
    html! {
        p.actions { a.btn href=(format!("{MIHOMO}?add=1")) data-dialog="profile-new" { (kit::icon(kit::Icon::Plus)) "Add configuration" } }
        (kit::section(Some("Configuration profiles"), Some(html! {
            p.foot { "Switching or editing the active profile briefly reconnects Clash. Invalid downloads, Merge or Script keep the old configuration. Downloads use Clash when running; otherwise the current uplink. Cellular data is never turned on." }
        }), html! {
            @if let Some(profiles) = data["profiles"].as_array().filter(|p| !p.is_empty()) {
                @for profile in profiles {
                    li {
                        div.it { span.cap { strong { (profile["name"].as_str().unwrap_or("Profile")) } small { (if profile["remote"].as_bool() == Some(true) { "Remote subscription" } else { "Local configuration" }) } } @if data["selected"] == profile["id"] { span.v { "Selected" } } }
                        @if let Some(updated) = profile["updated"].as_u64().filter(|n| *n > 0 && *n < 253_402_300_800) {
                            p.foot { "Last updated: " (httpdate::fmt_http_date(std::time::UNIX_EPOCH + std::time::Duration::from_secs(updated))) }
                        }
                        @if let Some(error) = profile["error"].as_str() { p.foot.err { (error) } }
                        form method="post" action=(CHANGE) {
                            (hidden()) input type="hidden" name="id" value=(profile["id"].as_str().unwrap_or(""));
                            p.actions.net-profile-actions {
                                @if data["selected"] != profile["id"] { button.btn type="submit" name="action" value="select" { "Use profile" } }
                                @if profile["remote"].as_bool() == Some(true) { button.btn type="submit" name="action" value="refresh" { "Update now" } }
                                a.btn href=(format!("{MIHOMO}?edit={}", profile["id"].as_str().unwrap_or(""))) { "Edit" }
                                a.btn href=(format!("{MIHOMO}?delete={}", profile["id"].as_str().unwrap_or(""))) { "Delete" }
                            }
                        }
                    }
                }
            } @else { li.it { "No saved profiles" } }
        }))
        (profile_dialog("profile-new", &Value::Null, hidden(), adding, None))
        @if let Some(editor) = editor {
            @if deleting {
                dialog.net-modal #profile-delete open data-auto aria-labelledby="delete-title" {
                    h2 #delete-title { "Delete configuration?" }
                    p { (editor["name"].as_str().unwrap_or("Profile")) }
                    p.foot { "This removes the saved source and subscription URL. Turn off Clash or select another profile before deleting a running profile." }
                    form method="post" action=(CHANGE) {
                        (hidden()) input type="hidden" name="id" value=(editor["id"].as_str().unwrap_or(""));
                        input type="hidden" name="action" value="delete";
                        p.actions { a.btn href=(MIHOMO) data-close { "Cancel" } button.btn type="submit" { "Delete configuration" } }
                    }
                }
            } @else { (profile_dialog("profile-edit",editor,hidden(),true,None)) }
        } @else if pd.param("edit").is_some() || deleting {
            p.msg.err { "Cannot load this configuration. Refresh the profile list." }
        }
        @if let Some(extensions) = pd.read(3) {
            (extensions_form(extensions, hidden(), pd.param("extensions").as_deref() == Some("1"), None))
        } @else {
            p.msg.err role="alert" { "Cannot load saved global extensions. Refresh before editing; the saved configuration is unchanged." }
        }
    }
}

fn extensions_form(data: &Value, hidden: Markup, open: bool, result: Option<(bool, &str)>) -> Markup {
    let merge = data["merge"].as_str().unwrap_or("");
    let script = data["script"].as_str().unwrap_or("");
    let open = open || !merge.is_empty() || !script.is_empty();
    let saved = !matches!(result, Some((false, _)));
    html! {
        details.relay-notes #global-extensions open[open] {
            summary { "Global Merge & Script" }
            form.rf.net-extensions method="post" action=(CHANGE) autocomplete="off" {
                (hidden) input type="hidden" name="action" value="extensions";
                (kit::section(Some("Applies to every profile"), Some(html! {
                    p.foot { "Source YAML -> Global Merge -> Global Script -> U60 network safeguards -> Mihomo validation. Saved content is shown here. Every profile must validate before anything is saved. Empty editors keep saved content; uncheck to disable. Paste {} or an identity main to replace saved content." }
                    p.foot { "Merge replaces lists; prepend-rules / append-rules add ordered entries (also proxies and proxy-groups). DNS merges one level, hosts replace. Scripts use synchronous main(config, profileName), without Node.js, network or filesystem access. Errors stop updates, not silently skip the script." }
                    p.foot { "U60 owns controller, TUN, DNS listen/redir-host and IPv6. PROCESS-NAME cannot identify hotspot clients. Use domain/IP rules; use a fixed LAN IP for per-device rules." }
                }), html! {
                    li.it { label { input type="checkbox" name="merge_enabled" value="1" checked[data["merge_enabled"].as_bool() == Some(true)]; " Enable global Merge" span data-net-saved="merge" hidden[merge.is_empty() || !saved] { " (saved)" } } }
                    li.it.fld { label for="global-merge" { "Merge YAML" } textarea.net-yaml #global-merge name="merge" rows="8" maxlength="131072" autocomplete="off" spellcheck="false" { (merge) } }
                    li.it { label { input type="checkbox" name="script_enabled" value="1" checked[data["script_enabled"].as_bool() == Some(true)]; " Enable global Script" span data-net-saved="script" hidden[script.is_empty() || !saved] { " (saved)" } } }
                    li.it.fld { label for="global-script" { "JavaScript" } textarea.net-yaml #global-script name="script" rows="12" maxlength="131072" autocomplete="off" spellcheck="false" placeholder="function main(config, profileName) { return config; }" { (script) } }
                    li {
                        button.it.cv.snd type="submit" data-busy-label="Validating all profiles and applying extensions..." { "Validate all & apply extensions" }
                        div data-net-feedback {
                            @if let Some((ok, text)) = result {
                                p class=(if ok { "msg ok net-result" } else { "msg err net-result" }) role=(if ok { "status" } else { "alert" }) { (text) }
                            }
                        }
                    }
                }))
            }
        }
    }
}

fn failed_extensions(
    app: &AppState,
    principal: &crate::auth::Principal,
    token: &str,
    nonce: &str,
    data: &Value,
    error: &str,
) -> Response {
    let hidden = html! { (render::csrf_field(&app.auth,principal)) (once_field(token)) };
    render::html(400, render::page(&render::PageCtx {
        title: "Global Merge & Script",
        tab: Some(kit::Tab::Router),
        nonce,
        network_forms: true,
        lead: kit::back(MIHOMO, "Mihomo"),
        ..Default::default()
    }, extensions_form(data, hidden, true, Some((false, error)))))
}

fn profile_dialog(
    id: &str,
    data: &Value,
    hidden: Markup,
    open: bool,
    error: Option<&str>,
) -> Markup {
    let editing = data["id"].as_str().is_some_and(|s| !s.is_empty());
    let local = data["source_type"].as_str() == Some("local");
    let title = format!("{id}-title");
    html! {
        dialog.net-modal id=(id) open[open] data-auto=[open.then_some("")] aria-labelledby=(&title) {
            p.net-eyebrow { "MIHOMO / PROFILES" }
            h2 id=(&title) { (if editing { "Edit configuration" } else { "Add configuration" }) }
            form.net-profile method="post" action=(EDITOR) enctype="multipart/form-data" autocomplete="off" {
                (hidden)
                input type="hidden" name="action" value=(if editing { "edit" } else { "add" });
                input type="hidden" name="id" value=(data["id"].as_str().unwrap_or(""));
                div.net-fields {
                    div { label for=(format!("{id}-type")) { "Type" }
                        div.net-select {
                            select.net-control id=(format!("{id}-type")) name="source_type" {
                                option value="remote" selected[!local] { "Remote subscription" }
                                option value="local" selected[local] { "Local configuration" }
                            }
                        }
                    }
                    div { label for=(format!("{id}-name")) { "Name" } input.net-control type="text" id=(format!("{id}-name")) name="name" maxlength="128" required value=(data["name"].as_str().unwrap_or("")) placeholder="e.g. Personal subscription"; }
                    div.net-remote { label for=(format!("{id}-url")) { "Subscription URL" }
                        textarea.net-control id=(format!("{id}-url")) name="url" rows="3" maxlength="4096" inputmode="url" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="https://example.com/config.yaml" { (data["url"].as_str().unwrap_or("")) }
                    }
                    div.net-remote { label for=(format!("{id}-interval")) { "Automatic update" }
                        div.net-select { (schedule(&format!("{id}-interval"),data["interval_hours"].as_u64().unwrap_or(0))) }
                    }
                    div.net-local { label for=(format!("{id}-file")) { "Import YAML file" }
                        input.net-control id=(format!("{id}-file")) type="file" name="yaml_file" accept=".yaml,.yml,.json,text/yaml,application/yaml";
                    }
                    div.net-local { label for=(format!("{id}-yaml")) { "Paste YAML" }
                        textarea.net-control.net-yaml id=(format!("{id}-yaml")) name="yaml" rows="8" maxlength="524288" autocomplete="off" spellcheck="false" {}
                        @if editing { p.foot { "Leave empty to keep the saved local configuration." } }
                        p.foot { "Complete Mihomo YAML, up to 512 KiB. A chosen file takes precedence over pasted YAML." }
                    }
                }
                p.foot.net-remote { "Downloaded securely over HTTPS. Your link is visible only in this editor, never in logs." }
                @if let Some(error) = error { p.msg.err.net-result role="alert" { (error) } }
                p.actions { a.btn.btn-quiet href=(MIHOMO) data-close { "Cancel" } button.btn type="submit" { (if editing { "Save changes" } else { "Save configuration" }) } }
            }
        }
    }
}

fn failed_editor(
    app: &AppState,
    principal: &crate::auth::Principal,
    token: &str,
    nonce: &str,
    data: &Value,
    error: &str,
) -> Response {
    let hidden = html! { (render::csrf_field(&app.auth,principal)) (once_field(token)) };
    let body = profile_dialog("profile-edit", data, hidden, true, Some(error));
    render::html(
        400,
        render::page(
            &render::PageCtx {
                title: "Configuration",
                tab: Some(kit::Tab::Router),
                nonce,
                network_forms: true,
                lead: kit::back(MIHOMO, "Mihomo"),
                ..Default::default()
            },
            body,
        ),
    )
}

fn json_result(result: Result<(), String>, once: Option<String>) -> Response {
    let mut response = match result {
        Ok(()) => api::json_reply(200, &json!({"ok":true})),
        Err(error) => api::json_reply(400, &json!({"ok":false,"error":error,"once":once})),
    };
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

fn extensions_result(app: &AppState, result: Result<(), String>, once: Option<String>) -> Response {
    if result.is_err() {
        return json_result(result, once);
    }
    let saved = extensions_read(app, None);
    let message = if saved.is_ok() { EXTENSIONS_SAVED } else {
        "Global extensions saved, but the saved text could not be reloaded. Refresh to verify it before editing again."
    };
    let mut response = api::json_reply(200, &json!({"ok":true,"message":message,"once":once,"extensions":saved.ok()}));
    response.headers_mut().insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
    response
}

pub async fn post(ctx: Ctx, req: Request) -> Response {
    let nonce = CspNonce::of(&req);
    let path = req.uri().path().to_owned();
    let (parts, body) = req.into_parts();
    let enhanced = api::header(&parts.headers, "x-u60-form") == Some("1");
    let Some(principal) = session::find_session(&ctx.app.auth, &parts.headers) else {
        return session::reply_unread(
            &parts.headers,
            body,
            session::login_redirect(&Method::POST, &path, None),
        )
        .await;
    };
    if !guard::ui_post_allowed(&api::req_headers(&parts.headers)) {
        return session::reply_unread(&parts.headers, body, render::form_refused(&nonce, MIHOMO))
            .await;
    }
    let (form, uploaded) = if path == EDITOR {
        match upload::read_profile(&ctx, &parts.headers, body, &nonce).await {
            Ok(value) => value,
            Err(r) => return r,
        }
    } else if path == FILE {
        match upload::read_config(&ctx, &parts.headers, body, &nonce).await {
            Ok((form, yaml)) => (form, Some(yaml)),
            Err(r) => return r,
        }
    } else {
        if !api::header(&parts.headers, "content-type")
            .is_some_and(|s| s.split(';').next() == Some("application/x-www-form-urlencoded"))
        {
            return session::reply_unread(&parts.headers, body, render::bad_request(&nonce)).await;
        }
        match api::read_capped(
            &parts.headers,
            body,
            crate::mihomo::MAX_CONFIG * 3 + 16384,
            ctx.body_wait,
        )
        .await
        {
            Ok(bytes) => (Form::parse(&bytes), None),
            Err(_) => {
                return api::close(render::error(
                    413,
                    "Form too large",
                    "Configuration or extensions exceeded the limit.",
                    &nonce,
                ))
            }
        }
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &principal, form.get("csrf")) {
        return render::form_refused(&nonce, MIHOMO);
    }
    let get = |key| form.get(key).unwrap_or("");
    let action = if path == FILE { "add" } else { get("action") };
    if path == EDITOR && !matches!(action, "add" | "edit") {
        return render::bad_request(&nonce);
    }
    let interval = match get("interval_hours") {
        "" => 0,
        value => match value.parse::<u64>() {
            Ok(value) => value,
            Err(_) => return render::bad_request(&nonce),
        },
    };
    let editor_post = matches!(action, "add" | "edit") && path != FILE;
    let extensions_post = action == "extensions";
    let source_type = if path == FILE {
        "local"
    } else {
        get("source_type")
    };
    let data = json!({"action":action,"id":get("id"),"name":get("name"),"url":get("url"),"source_type":source_type,
        "yaml":uploaded.as_deref().unwrap_or_else(|| get("yaml")),"interval_hours":interval,
        "extensions":{"merge_enabled":get("merge_enabled")=="1","script_enabled":get("script_enabled")=="1","merge":get("merge"),"script":get("script")}});
    let digest = job::digest(&[&path, &data.to_string()]);
    let token = get("once").to_owned();
    let owner = principal.id;
    let flash = Arc::clone(&ctx.flash);
    let once = Arc::clone(&ctx.once);
    let copied_data = data.clone();
    let copied_principal = principal.clone();
    let copied_nonce = nonce.clone();
    let success = if extensions_post { EXTENSIONS_SAVED } else {
        "Profile operation completed. The first profile is selected automatically; use Use profile to switch to another. Clash stays off if it was off."
    };
    let home = if extensions_post { EXTENSIONS_PAGE } else { MIHOMO };
    let work: OnceJob = Box::new(move |app, claim| {
        claim.disarm();
        let result = super::call(
            app,
            crate::server::Method::Post,
            "/api/mihomo/profiles",
            &data,
        );
        claim.set(match &result {
            Ok(_) => Token::Sent(claim.digest()),
            Err((s, e)) => Token::Failed(claim.digest(), *s, e.clone()),
        });
        if enhanced {
            let next_token = (result.is_err() || extensions_post).then(|| once.fresh(&app.auth, owner));
            if extensions_post {
                return extensions_result(app, result.map(|_| ()).map_err(|(_, e)| e), next_token);
            }
            return json_result(result.map(|_| ()).map_err(|(_, e)| e), next_token);
        }
        if let Err((_, error)) = &result {
            if extensions_post {
                return failed_extensions(app, &copied_principal, &once.fresh(&app.auth, owner), &copied_nonce, &data["extensions"], error);
            }
            if editor_post {
                return failed_editor(
                    app,
                    &copied_principal,
                    &once.fresh(&app.auth, owner),
                    &copied_nonce,
                    &data,
                    error,
                );
            }
        }
        flash_line(
            &flash,
            app,
            owner,
            home,
            result.map(|_| success.into()).map_err(|(_, e)| e),
        )
    });
    let result = match job::once(
        &ctx,
        owner,
        &nonce,
        &token,
        digest,
        Lane::Slow,
        SETTLE_SLOW,
        work,
    )
    .await
    {
        Sent::Ran(r) => return r,
        Sent::Done(Ok(())) => Ok(success.into()),
        Sent::Done(Err((_, e))) => Err(e),
        Sent::Expired => Err("Form expired. Refresh and try again.".into()),
        Sent::Unknown => {
            Err("The operation may still be running. Check profiles before retrying.".into())
        }
        Sent::Busy => Err("Router busy. Nothing changed.".into()),
    };
    // Unknown still owns its token, including on the no-JS error page.
    let next = match ctx.once.get(&token, owner, ctx.app.auth.now()) {
        Some(Token::Failed(..)) => Some(ctx.once.fresh(&ctx.app.auth, owner)),
        Some(Token::Sent(_)) if extensions_post && result.is_ok() => Some(ctx.once.fresh(&ctx.app.auth, owner)),
        _ => None,
    };
    if enhanced {
        if extensions_post {
            return job::run(&ctx, Lane::General, &nonce, MIHOMO, Box::new(move |app| {
                extensions_result(app, result.map(|_| ()), next)
            })).await;
        }
        return json_result(result.map(|_| ()), next);
    }
    if let Err(error) = &result {
        if extensions_post {
            return failed_extensions(&ctx.app, &principal, next.as_deref().unwrap_or(&token), &nonce, &copied_data["extensions"], error);
        }
        if editor_post {
            return failed_editor(
                &ctx.app,
                &principal,
                next.as_deref().unwrap_or(&token),
                &nonce,
                &copied_data,
                error,
            );
        }
    }
    flash_line(&ctx.flash, &ctx.app, owner, home, result)
}
