//! Settings › Appearance and Polling (iOS `SettingsView`): per-browser
//! preferences in two cookies that hold fixed values only, nothing about
//! the session or the router.
//!
//! - **Theme** (`zte_ui_theme=light|dark`; System clears it): every page
//!   of this browser in that colour scheme. The page shell is drawn for the
//!   system's scheme; the `guard` middleware swaps its colour-scheme tags
//!   for the theme's (`themed`), which adds a small stylesheet with that
//!   scheme's colours (`theme-light.css` / `theme-dark.css`, made from
//!   `app.css` by `build.rs`). No JavaScript: the one small script of the
//!   System theme (`assets/theme.js`, owner decision 60) is left out.
//! - **Refresh interval** (`zte_ui_refresh=<ms>`, 1.0–10.0 s by 0.5 s; the
//!   2.0 s default clears it): the signal cadence a live page of this
//!   browser asks the hub for (`signal_hint`; the fastest open stream
//!   wins, as with the apps' `signal_ms`). It changes nothing while no
//!   page is open, and a screen with a cadence of its own (eSIM Profiles)
//!   keeps it.
//!
//! Both are set by a POST with the session's CSRF token (no single-use
//! token: a repeated post sets the same value), answered with `303` to
//! Settings.

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{HeaderMap, CONTENT_LENGTH};
use axum::http::Method;
use axum::response::Response;

use super::{render, session, CspNonce};
use crate::httpd::Ctx;

pub const THEME_COOKIE: &str = "zte_ui_theme";
pub const REFRESH_COOKIE: &str = "zte_ui_refresh";
pub const THEME: &str = "/ui/settings/theme";
pub const REFRESH: &str = "/ui/settings/refresh";
const SETTINGS: &str = "/ui/settings";
/// One year.
const MAX_AGE: u64 = 365 * 24 * 3600;

/// The app's `dark_mode_override`: System, Light, Dark.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    pub fn key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }

    fn of(key: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.key() == key)
    }
}

/// The app's default refresh interval (and the hub's signal cadence).
pub const REFRESH_DEFAULT: u32 = 2000;

/// The app's slider: 1.0–10.0 s in steps of 0.5 s, in milliseconds.
pub fn refresh_ok(ms: u32) -> bool {
    (1000..=10_000).contains(&ms) && ms.is_multiple_of(500)
}

/// "2.0s" (the app's `%.1f` and unit).
pub fn refresh_label(ms: u32) -> String {
    format!("{}.{}s", ms / 1000, ms % 1000 / 100)
}

/// This browser's preferences (`Prefs::of`); the defaults without cookies.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Prefs {
    pub theme: Theme,
    pub refresh_ms: u32,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { theme: Theme::System, refresh_ms: REFRESH_DEFAULT }
    }
}

impl Prefs {
    /// The first valid value of each cookie (a page on another port of
    /// this host can add cookies of the same name; junk is skipped).
    pub fn of(headers: &HeaderMap) -> Prefs {
        let theme = session::cookies(headers, THEME_COOKIE).find_map(Theme::of).unwrap_or_default();
        let refresh_ms = session::cookies(headers, REFRESH_COOKIE)
            .find_map(|v| v.parse::<u32>().ok().filter(|ms| refresh_ok(*ms) && v.bytes().all(|b| b.is_ascii_digit())))
            .unwrap_or(REFRESH_DEFAULT);
        Prefs { theme, refresh_ms }
    }

    /// The signal cadence for this browser's live pages (`None`: the
    /// default).
    pub fn signal_hint(self) -> Option<std::time::Duration> {
        (self.refresh_ms != REFRESH_DEFAULT).then(|| std::time::Duration::from_millis(self.refresh_ms.into()))
    }
}

/// `Set-Cookie` for a preference: `Some(value)` for a year, `None` (the
/// default) clears it.
fn cookie(name: &str, value: Option<&str>) -> String {
    match value {
        Some(v) => format!("{name}={v}; Path=/ui/; Max-Age={MAX_AGE}; HttpOnly; SameSite=Strict"),
        None => format!("{name}=; Path=/ui/; Max-Age=0; HttpOnly; SameSite=Strict"),
    }
}

/// The posts of the Settings page.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Post {
    Theme,
    Refresh,
}

impl Post {
    pub fn of(path: &str) -> Option<Post> {
        match path {
            THEME => Some(Post::Theme),
            REFRESH => Some(Post::Refresh),
            _ => None,
        }
    }

    /// The page of its form.
    pub fn form(self) -> &'static str {
        SETTINGS
    }
}

/// `POST /ui/settings/{theme,refresh}`: the session (else the login,
/// "nothing was changed"), the form, origin and CSRF token, then the value
/// (one the form offers, else `400`), set as its cookie with `303` back to
/// Settings.
pub async fn post(ctx: Ctx, req: Request, k: Post) -> Response {
    let nonce = CspNonce::of(&req);
    let (parts, body) = req.into_parts();
    let Some(p) = session::find_session(&ctx.app.auth, &parts.headers) else {
        let to_login = session::login_redirect(&Method::POST, parts.uri.path(), None);
        return session::reply_unread(&parts.headers, body, to_login).await;
    };
    let form = match session::read_form(&ctx, &parts.headers, body, &nonce).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    if !session::verify_post(&ctx.app.auth, &parts.headers, &p, form.get("csrf")) {
        return render::form_refused(&nonce, SETTINGS);
    }
    let set = match k {
        Post::Theme => match form.get("theme").and_then(Theme::of) {
            Some(Theme::System) => cookie(THEME_COOKIE, None),
            Some(t) => cookie(THEME_COOKIE, Some(t.key())),
            None => return render::bad_request(&nonce),
        },
        Post::Refresh => {
            let ms = form.get("ms").filter(|v| v.bytes().all(|b| b.is_ascii_digit())).and_then(|v| v.parse::<u32>().ok());
            match ms.filter(|ms| refresh_ok(*ms)) {
                Some(REFRESH_DEFAULT) => cookie(REFRESH_COOKIE, None),
                Some(ms) => cookie(REFRESH_COOKIE, Some(&ms.to_string())),
                None => return render::bad_request(&nonce),
            }
        }
    };
    let mut resp = session::see_other(SETTINGS);
    session::add_cookie(&mut resp, &set);
    resp
}

/// A page in this browser's theme: the shell's colour-scheme tags (drawn
/// for the system's scheme, with the System script carrying this
/// response's CSP `nonce`) swapped for the theme's, which link its
/// stylesheet after `app.css` and load no script. Anything but a whole
/// HTML page, and every page under the System theme, is left as it is.
pub async fn themed(resp: Response, theme: Theme, nonce: &str) -> Response {
    let html = session::content_type(&resp).is_some_and(|ct| ct.starts_with("text/html"));
    if theme == Theme::System || !html {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    // Pages are built whole in memory: this only takes the bytes back.
    let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
        return render::internal("");
    };
    let page = match std::str::from_utf8(&bytes) {
        Ok(s) => s.replacen(&render::scheme(Theme::System, nonce), &render::scheme(theme, nonce), 1),
        Err(_) => return Response::from_parts(parts, Body::from(bytes)),
    };
    parts.headers.remove(CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(cookie: &str) -> HeaderMap {
        HeaderMap::from_iter([(axum::http::header::COOKIE, HeaderValue::from_str(cookie).unwrap())])
    }

    #[test]
    fn cookies_hold_fixed_values_only() {
        assert_eq!(Prefs::of(&HeaderMap::new()), Prefs::default());
        let p = Prefs::of(&headers("zte_ui_theme=sepia; zte_ui_theme=dark; zte_ui_refresh=+1500; zte_ui_refresh=1500"));
        assert_eq!(p, Prefs { theme: Theme::Dark, refresh_ms: 1500 });
        assert_eq!(p.signal_hint(), Some(std::time::Duration::from_millis(1500)));
        for bad in ["900", "10500", "1250", "x", ""] {
            assert_eq!(Prefs::of(&headers(&format!("zte_ui_refresh={bad}"))).refresh_ms, REFRESH_DEFAULT, "{bad}");
        }
        assert_eq!(Prefs::default().signal_hint(), None);
        assert_eq!((1000..=10_000).filter(|ms| refresh_ok(*ms)).count(), 19);
        assert_eq!((refresh_label(1000), refresh_label(2500), refresh_label(10_000)), ("1.0s".into(), "2.5s".into(), "10.0s".into()));
        assert_eq!(cookie(THEME_COOKIE, Some("dark")), "zte_ui_theme=dark; Path=/ui/; Max-Age=31536000; HttpOnly; SameSite=Strict");
        assert_eq!(cookie(REFRESH_COOKIE, None), "zte_ui_refresh=; Path=/ui/; Max-Age=0; HttpOnly; SameSite=Strict");
    }
}
