//! Router › APN (iOS `APNView`, `APNFormSheet`, `APNAutoDetailSheet`): the
//! mode, the manual profiles (add, edit, activate, delete) and the auto
//! profiles (read-only), read at page load (no stream). Every change is
//! asked first (`pages::netcfg`; the app asks none: each can cut mobile
//! data, which a remote browser is told).
//!
//! - The page's read drops the profiles' passwords (only whether one is
//!   set); New / Edit APN never fills its password field ("Leave empty to
//!   keep", owner decision): an empty field keeps the stored one, merged on
//!   the router at the post.
//! - The app's row taps and swipes: a manual row opens Edit APN; its "•••"
//!   menu has Activate and Delete (not for the active one, as the app's
//!   disabled swipe); an auto row opens Auto APN.
//! - Checks: the app's ("Name and APN are required", "An APN with this
//!   name already exists", "Cannot delete the active APN"), plus lengths,
//!   one line, and an APN of letters, digits, dots, hyphens and
//!   underscores (web additions).

use maud::{html, Markup};
use serde_json::{json, Value};

use super::confirm::{asking, nothing, Bad, Question};
use super::netcfg::{choice, failed, field, form_error, options, password_row, select_row, switch, text_row, truthy, val, Done, Kind, Vals, KEPT};
use crate::handlers::AppState;
use crate::server::Method;
use crate::web::fmt;
use crate::web::kit::{self, Act, Icon};
use crate::web::render::once_field;
use crate::web::screen::{FormState, PageData};
use crate::web::session::percent_encode;
use crate::web::store::Claim;

pub const PATH: &str = "/ui/router/apn";
pub const NEW: &str = "/ui/router/apn/new";
pub const EDIT: &str = "/ui/router/apn/edit";
pub const AUTO: &str = "/ui/router/apn/auto";
pub const MODE: &str = "/ui/router/apn/mode";
pub const SAVE: &str = "/ui/router/apn/save";
pub const ACTIVATE: &str = "/ui/router/apn/activate";
pub const DELETE: &str = "/ui/router/apn/delete";

const MODE_API: &str = "/api/router/apn/mode";
const LIST_API: &str = "/api/router/apn/profiles";
const AUTO_API: &str = "/api/router/apn/auto-profiles";

const PDP: [&str; 3] = ["1", "2", "3"];
const AUTH: [&str; 3] = ["0", "1", "2"];

fn pdp_label(v: &str) -> String {
    match v {
        "1" => "IPv4",
        "2" => "IPv6",
        _ => "IPv4v6",
    }
    .to_owned()
}

fn auth_label(v: &str) -> String {
    match v {
        "1" => "PAP",
        "2" => "CHAP",
        _ => "None",
    }
    .to_owned()
}

/// A firmware profile id ("manu1"): a link carries it.
fn id_ok(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One profile (the app's `APNParser`). No `Debug`: it holds the password.
struct Profile {
    id: String,
    name: String,
    apn: String,
    pdp: i64,
    auth: i64,
    user: String,
    password: String,
    active: bool,
}

impl Profile {
    /// The name, else the APN (the app's "Current APN").
    fn shown(&self) -> &str {
        if self.name.is_empty() {
            &self.apn
        } else {
            &self.name
        }
    }

    /// For a page: no password, only whether there is one.
    fn page_value(&self) -> Value {
        json!({"id": self.id, "name": self.name, "apn": self.apn, "pdp": self.pdp.to_string(), "auth": self.auth.to_string(),
               "user": self.user, "has_pass": !self.password.is_empty(), "active": self.active})
    }
}

/// A list's profiles; empty slots (no name and no APN) left out.
fn parse(list: &Value) -> Vec<Profile> {
    let s = |v: &Value| v.as_str().unwrap_or("").to_owned();
    list["apnListArray"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = s(&item["profileId"]);
            let (name, apn) = (s(&item["profilename"]), s(&item["wanapn"]));
            (!id.is_empty() && (!name.is_empty() || !apn.is_empty())).then(|| Profile {
                id,
                name,
                apn,
                pdp: fmt::int(&item["pdpType"]).unwrap_or(3),
                auth: fmt::int(&item["pppAuthMode"]).unwrap_or(0),
                user: s(&item["username"]),
                password: s(&item["password"]),
                active: truthy(&item["isEnable"]),
            })
        })
        .collect()
}

/// The manual profiles, passwords included (for a post only).
fn manual(app: &AppState) -> Result<Vec<Profile>, String> {
    super::api_get(app, LIST_API).map(|l| parse(&l))
}

/// The page's read: the mode and both lists, without passwords.
pub fn read(app: &AppState, _: Option<&str>) -> Result<Value, String> {
    let mode = super::api_get(app, MODE_API)?;
    let list = |path| super::api_get(app, path).map(|l| parse(&l).iter().map(Profile::page_value).collect::<Vec<_>>());
    Ok(json!({"manual": fmt::int(&mode["apn_mode"]) == Some(1), "profiles": list(LIST_API)?, "auto": list(AUTO_API)?}))
}

fn str_of<'a>(p: &'a Value, k: &str) -> &'a str {
    p[k].as_str().unwrap_or("")
}

fn row(p: &Value, manual: bool) -> Markup {
    let id = percent_encode(str_of(p, "id"));
    let name = Some(str_of(p, "name")).filter(|n| !n.is_empty()).unwrap_or("Unnamed");
    let active = p["active"] == true;
    let href = format!("{}?id={id}", if manual { EDIT } else { AUTO });
    html! {
        li.it.ap {
            a href=(href) {
                span.cvb {
                    span.l1 { b.nm { (name) } @if active { small.c-green { "Active" } } }
                    small { (str_of(p, "apn")) }
                    small { (pdp_label(str_of(p, "pdp"))) " / " (auth_label(str_of(p, "auth"))) }
                }
            }
            // The app's swipes (Activate; Delete, disabled for the active one).
            @if manual && !active {
                details.pmenu {
                    summary aria-label=(format!("Options for {name}")) { (kit::icon(Icon::Ellipsis)) }
                    div {
                        a href=(format!("{ACTIVATE}?id={id}")) { (kit::icon(Icon::Check)) "Activate" }
                        a.danger href=(format!("{DELETE}?id={id}")) { (kit::icon(Icon::Trash)) "Delete" }
                    }
                }
            }
        }
    }
}

pub fn page(pd: &PageData) -> Markup {
    let d = match pd.reads.first() {
        Some(Ok(d)) => d,
        Some(Err(e)) => return html! { p.msg.err role="alert" { "Failed to load APN: " (fmt::truncate(e, 120)) } },
        None => return html! {},
    };
    let manual = d["manual"] == true;
    let list = |k: &str| d[k].as_array().cloned().unwrap_or_default();
    let (profiles, auto) = (list("profiles"), list("auto"));
    let current = profiles.iter().chain(&auto).find(|p| p["active"] == true).map(|p| {
        let name = str_of(p, "name");
        if name.is_empty() { str_of(p, "apn") } else { name }.to_owned()
    });
    let (on, off) = (format!("{MODE}?manual=1"), format!("{MODE}?manual=0"));
    html! {
        @if let Some(n) = current {
            (kit::section(Some("Current APN"), None, html! { li.it { span { b { (n) } } span.c-green { (kit::icon(Icon::Check)) } } }))
        }
        (kit::section(Some("APN Mode"), None, kit::switch_row(&html! {}, "Manual APN", Some(manual), Act::Link(&on), Act::Link(&off))))
        @if manual {
            (kit::section(Some("Manual Profiles"), None, html! { li { a.it.bt href=(NEW) { (kit::icon(Icon::Plus)) span { "Add APN" } } } }))
            @if !profiles.is_empty() { (kit::section(None, None, html! { @for p in &profiles { (row(p, true)) } })) }
        } @else if !auto.is_empty() {
            (kit::section(Some("Auto Profiles"), None, html! { @for p in &auto { (row(p, false)) } }))
        }
    }
}

/// The profile of `?id=` in the page's read (Edit APN, Auto APN).
fn asked<'a>(pd: &'a PageData, list: &str) -> Option<&'a Value> {
    let id = pd.param("id")?;
    pd.read(0)?[list].as_array()?.iter().find(|p| str_of(p, "id") == id)
}

/// Edit APN and Auto APN: `404` for a profile that is not there (its read
/// failed: `503`).
pub fn edit_status(pd: &PageData) -> u16 {
    status(pd, "profiles")
}

pub fn auto_status(pd: &PageData) -> u16 {
    status(pd, "auto")
}

fn status(pd: &PageData, list: &str) -> u16 {
    match (pd.param("id"), pd.reads.first()) {
        (Some(_), Some(Err(_))) => 503,
        _ if asked(pd, list).is_some() => 200,
        _ => 404,
    }
}

/// Edit APN: the profile's form; without an `id` (never linked) only the
/// not-found state, so the `404` draws no form that would add a profile.
pub fn edit_form(pd: &PageData) -> Markup {
    if pd.param("id").is_none() {
        return not_found();
    }
    form(pd)
}

fn not_found() -> Markup {
    html! { (kit::empty_state(Icon::SimCard, "APN not found", "It may have been deleted.")) p.ctr { a.nl href=(PATH) { "Back to APN Settings" } } }
}

/// New APN and Edit APN (the app's sheet; "Cancel" leads back).
pub fn form(pd: &PageData) -> Markup {
    let editing = pd.param("id").is_some();
    let blank = json!({"pdp": "3", "auth": "0"});
    let p = if editing {
        match (pd.reads.first(), asked(pd, "profiles")) {
            (Some(Err(e)), _) => return html! { p.msg.err role="alert" { "Failed to load APN: " (fmt::truncate(e, 120)) } },
            (_, Some(p)) => p,
            _ => return not_found(),
        }
    } else {
        &blank
    };
    let f = pd.form;
    let typed = |name: &str, key: &str| f.and_then(|f| f.value(name)).map_or_else(|| str_of(p, key).to_owned(), str::to_owned);
    let err = |name: &str| f.and_then(|f| f.error(name));
    let (pdp0, auth0) = (str_of(p, "pdp"), str_of(p, "auth"));
    html! {
        (form_error(pd))
        form.rf.apf method="post" action=(SAVE) autocomplete="off" {
            (pd.csrf())
            (once_field(&pd.once()))
            @if editing { input type="hidden" name="id" value=(str_of(p, "id")); }
            (kit::section(Some("Profile"), None, html! {
                (text_row("name", "Name", &typed("name", "name"), err("name"), "it fld", 64, None))
                (text_row("apn", "APN", &typed("apn", "apn"), err("apn"), "it fld", 100, None))
            }))
            (kit::section(Some("Connection"), None, html! {
                (select_row("pdp", "PDP Type", &options(&PDP, pdp0, pdp_label), &typed("pdp", "pdp"), None, "it"))
                (select_row("auth", "Auth Mode", &options(&AUTH, auth0, auth_label), &typed("auth", "auth"), None, "it"))
            }))
            // Shown while Auth Mode is not None (CSS; without `:has()` always).
            div.apc {
                (kit::section(Some("Credentials"), None, html! {
                    (text_row("user", "Username", &typed("user", "user"), err("user"), "it fld", 64, None))
                    (password_row("pass", "Password", p["has_pass"] == true, f.and_then(|f| f.value("pass")) == Some(KEPT), err("pass"), "it fld"))
                }))
            }
            (kit::section(None, None, switch("def", "Set as Default", f.map_or(p["active"] == true, |f| f.value("def") == Some("1")), "it", "sr")))
            (kit::section(None, None, html! { li { button.it.cv.snd type="submit" { (if editing { "Save" } else { "Add APN" }) } } }))
        }
    }
}

/// Auto APN (read-only, "Done" leads back).
pub fn auto_page(pd: &PageData) -> Markup {
    let Some(p) = asked(pd, "auto") else {
        return match pd.reads.first() {
            Some(Err(e)) => html! { p.msg.err role="alert" { "Failed to load APN: " (fmt::truncate(e, 120)) } },
            _ => html! { (kit::empty_state(Icon::SimCard, "APN not found", "It may have been removed.")) },
        };
    };
    let or_dash = |k: &str| Some(str_of(p, k)).filter(|v| !v.is_empty()).unwrap_or("\u{2014}").to_owned();
    html! {
        (kit::section(Some("Profile"), None, html! { (kit::kv_row(None, "Name", &or_dash("name"))) (kit::kv_row(None, "APN", &or_dash("apn"))) }))
        (kit::section(Some("Connection"), None, html! {
            (kit::kv_row(None, "PDP Type", &pdp_label(str_of(p, "pdp"))))
            (kit::kv_row(None, "Auth Mode", &auth_label(str_of(p, "auth"))))
        }))
        @if str_of(p, "auth") != "0" { (kit::section(Some("Credentials"), None, kit::kv_row(None, "Username", &or_dash("user")))) }
        (kit::section(None, None, kit::kv_row(None, "Status", if p["active"] == true { "Active" } else { "Inactive" })))
    }
}

// --- Changes (`pages::netcfg`) ---

pub fn mode_fields(pairs: &[(String, String)]) -> Result<Vals, Bad> {
    match field(pairs, "manual") {
        Some(v @ ("0" | "1")) => Ok(vec![("manual", v.to_owned())]),
        _ => Err(Bad::Request),
    }
}

pub fn id_fields(pairs: &[(String, String)]) -> Result<Vals, Bad> {
    match field(pairs, "id") {
        Some(id) if id_ok(id) => Ok(vec![("id", id.to_owned())]),
        _ => Err(Bad::Request),
    }
}

/// One line of at most `max` characters.
fn line_ok(s: &str, max: usize) -> bool {
    s.chars().count() <= max && !s.chars().any(char::is_control)
}

/// New / Edit APN's form, checked against the manual profiles (read now).
pub fn check(app: &AppState, pairs: &[(String, String)], kept: &Vals) -> Result<Vals, Bad> {
    let id = field(pairs, "id").unwrap_or("");
    if !id.is_empty() && !id_ok(id) {
        return Err(Bad::Request);
    }
    let get = |n: &str, max: usize| -> String { field(pairs, n).unwrap_or("").trim().chars().take(max + 1).collect() };
    let (name, apn, user) = (get("name", 64), get("apn", 100), get("user", 64));
    let typed_pass = field(pairs, "pass").unwrap_or("");
    let list = manual(app);
    let editing = list.as_ref().ok().and_then(|l| l.iter().find(|p| p.id == id));
    let (pdp0, auth0) = editing.map_or((String::new(), String::new()), |p| (p.pdp.to_string(), p.auth.to_string()));
    let pdp = choice(pairs, "pdp", &PDP, &pdp0)?;
    let auth = choice(pairs, "auth", &AUTH, &auth0)?;
    let mut errors: Vec<(&'static str, String)> = Vec::new();
    for (k, v) in [("name", &name), ("apn", &apn)] {
        if v.is_empty() {
            errors.push((k, "Name and APN are required".into()));
        }
    }
    if !line_ok(&name, 64) {
        errors.push(("name", "Use one line, up to 64 characters.".into()));
    }
    if apn.len() > 100 || !apn.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b)) {
        errors.push(("apn", "Use up to 100 letters, digits, dots, hyphens and underscores.".into()));
    }
    if !line_ok(&user, 64) {
        errors.push(("user", "Use one line, up to 64 characters.".into()));
    }
    let pass = if typed_pass.is_empty() { val(kept, "pass") } else { typed_pass };
    let pass = if line_ok(pass, 64) {
        pass.to_owned()
    } else {
        errors.push(("pass", "Use one line, up to 64 characters.".into()));
        String::new()
    };
    match &list {
        Err(e) => errors.push(("form", format!("Failed to load APN: {}", fmt::truncate(e, 120)))),
        Ok(_) if !id.is_empty() && editing.is_none() => errors.push(("form", "This APN no longer exists.".into())),
        Ok(l) if !name.is_empty() && l.iter().any(|p| p.name == name && p.id != id) => {
            errors.push(("name", "An APN with this name already exists".into()));
        }
        Ok(_) => {}
    }
    let vals: Vals = vec![
        ("id", id.to_owned()),
        ("name", name.chars().take(64).collect()),
        ("apn", apn.chars().take(100).collect()),
        ("pdp", pdp),
        ("auth", auth),
        ("user", user.chars().take(64).collect()),
        ("pass", pass),
        ("def", super::netcfg::check(pairs, "def")),
    ];
    if errors.is_empty() {
        Ok(vals)
    } else {
        Err(Bad::Fields(FormState { values: vals, errors }))
    }
}

const RECONNECT: &str = "Mobile data may reconnect, which drops the connection for a moment.";
const NO_DATA: &str = "If the operator does not accept it, the router has no mobile data until another APN is chosen.";

pub fn question(kind: Kind, app: &AppState, vals: &[(&'static str, String)], remote: bool) -> Question {
    if kind == Kind::ApnMode {
        let manual = val(vals, "manual") == "1";
        let (title, what, label) = if manual {
            ("Switch to manual APN?", "The router connects with the APN you activate under Manual Profiles.", "Use Manual APN")
        } else {
            ("Switch to automatic APN?", "The router chooses the APN for your SIM by itself.", "Use Automatic APN")
        };
        return asking(title, vec![what.into(), RECONNECT.into()], label, remote, true);
    }
    let list = match manual(app) {
        Ok(l) => l,
        Err(e) => return nothing("Failed to load APN", vec![fmt::truncate(&e, 120)]),
    };
    let id = val(vals, "id");
    let found = list.iter().find(|p| p.id == id);
    match (kind, found) {
        (Kind::ApnSave, None) if id.is_empty() => {
            let mut lines = vec![format!("A new APN profile with the APN {} is added.", val(vals, "apn"))];
            let def = val(vals, "def") == "1";
            if def {
                lines.extend(["It becomes the default: mobile data reconnects with it, which drops the connection for a moment.".into(), NO_DATA.into()]);
            }
            asking(format!("Add \u{201c}{}\u{201d}?", val(vals, "name")), lines, "Add APN", remote, def)
        }
        (_, None) => nothing("APN not found", vec!["It may have been deleted.".into()]),
        (Kind::ApnSave, Some(p)) => {
            let live = p.active || val(vals, "def") == "1";
            let lines = if live {
                vec!["Mobile data reconnects with the new settings, which drops the connection for a moment.".into(), NO_DATA.into()]
            } else {
                vec!["Nothing changes on the connection until this APN is activated.".into()]
            };
            asking(format!("Save \u{201c}{}\u{201d}?", val(vals, "name")), lines, "Save", remote, live)
        }
        (Kind::ApnUse, Some(p)) if p.active => nothing(format!("\u{201c}{}\u{201d} is already active", p.shown()), Vec::new()),
        (Kind::ApnUse, Some(p)) => asking(
            format!("Use \u{201c}{}\u{201d}?", p.shown()),
            vec![format!("Mobile data reconnects with the APN {}, which drops the connection for a moment.", p.apn), NO_DATA.into()],
            "Activate",
            remote,
            true,
        ),
        (_, Some(p)) if p.active => nothing("Cannot delete the active APN", vec!["Activate another APN first.".into()]),
        (_, Some(p)) => asking(format!("Delete \u{201c}{}\u{201d}?", p.shown()), vec!["The APN profile is removed from the router.".into()], "Delete", remote, false),
    }
}

/// Activate profile `id` (the app's call).
fn activate(app: &AppState, id: &str) -> Result<Value, (u16, String)> {
    super::call(app, Method::Post, "/api/router/apn/profiles/activate", &json!({"profileId": id}))
}

pub fn send(kind: Kind, app: &AppState, vals: &[(&'static str, String)], claim: &mut Claim) -> Result<Done, (u16, String)> {
    let call = |method, path, body: Value| super::call(app, method, path, &body).map_err(|(status, e)| (status, failed(&e)));
    if kind == Kind::ApnMode {
        let manual = val(vals, "manual") == "1";
        claim.disarm();
        call(Method::Put, MODE_API, json!({"apn_mode": i64::from(manual)}))?;
        return Ok(Done::Line(Some(true), format!("APN mode set to {}", if manual { "manual" } else { "auto" })));
    }
    let list = manual(app).map_err(|e| (503, failed(&e)))?;
    let id = val(vals, "id");
    let found = list.iter().find(|p| p.id == id);
    match kind {
        Kind::ApnSave => {
            let (name, apn) = (val(vals, "name"), val(vals, "apn"));
            let int = |k: &str| val(vals, k).parse::<i64>().unwrap_or(0);
            let mut body = json!({"profilename": name, "wanapn": apn, "pdpType": int("pdp"), "pppAuthMode": int("auth"),
                                  "username": val(vals, "user"), "password": val(vals, "pass")});
            let def = val(vals, "def") == "1";
            if id.is_empty() {
                claim.disarm();
                call(Method::Post, LIST_API, body)?;
                if def {
                    // The app's: read the list again, find the new profile, activate it.
                    let new = manual(app).ok().and_then(|l| l.into_iter().find(|p| p.name == name && p.apn == apn));
                    return Ok(match new.map(|p| activate(app, &p.id)) {
                        Some(Ok(_)) => Done::Line(Some(true), "APN added".into()),
                        Some(Err((_, e))) => Done::Line(None, format!("APN added. Setting it as default failed: {}", fmt::truncate(&e, 120))),
                        None => Done::Line(None, "APN added. It could not be found to set it as default.".into()),
                    });
                }
                return Ok(Done::Line(Some(true), "APN added".into()));
            }
            let Some(p) = found else { return Err((404, "APN not found".into())) };
            body["profileId"] = id.into();
            if val(vals, "pass").is_empty() {
                // Left empty: the stored password, merged here (never in a page).
                body["password"] = p.password.clone().into();
            }
            claim.disarm();
            call(Method::Put, LIST_API, body)?;
            if def && !p.active {
                if let Err((_, e)) = activate(app, id) {
                    return Ok(Done::Line(None, format!("APN updated. Activating it failed: {}", fmt::truncate(&e, 120))));
                }
            }
            Ok(Done::Line(Some(true), "APN updated".into()))
        }
        Kind::ApnUse => {
            if found.is_none() {
                return Err((404, "APN not found".into()));
            }
            claim.disarm();
            activate(app, id).map_err(|(status, e)| (status, failed(&e)))?;
            Ok(Done::Line(Some(true), "APN activated".into()))
        }
        _ => match found {
            None => Err((404, "APN not found".into())),
            Some(p) if p.active => Err((409, "Cannot delete the active APN".into())),
            Some(_) => {
                claim.disarm();
                call(Method::Post, "/api/router/apn/profiles/delete", json!({"profileId": id}))?;
                Ok(Done::Line(Some(true), "APN deleted".into()))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app's parser: empty slots out, defaults IPv4v6 / None, numbers
    /// as numbers or strings; the page's value carries no password.
    #[test]
    fn profiles_follow_the_app() {
        let list = json!({"apnListArray": [
            {"profileId": "manu1", "profilename": "Fake", "wanapn": "fake.apn.example", "pdpType": "1", "pppAuthMode": 2, "username": "u", "password": "FAKEPW", "isEnable": "1"},
            {"profileId": "manu2", "profilename": "", "wanapn": ""},
            {"profileId": "", "profilename": "No id", "wanapn": "x"},
            {"profileId": "manu3", "wanapn": "other.example"},
        ]});
        let p = parse(&list);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].pdp, p[0].auth, p[0].active), (1, 2, true));
        assert_eq!((p[1].pdp, p[1].auth, p[1].active, p[1].shown()), (3, 0, false, "other.example"));
        let v = p[0].page_value();
        assert_eq!(v["has_pass"], true);
        assert!(!v.to_string().contains("FAKEPW"));
        assert!(id_ok("manu1") && !id_ok("") && !id_ok("a b") && !id_ok(&"a".repeat(33)));
    }
}
