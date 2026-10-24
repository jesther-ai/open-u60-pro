//! Browser sessions: the session and pre-login cookies, the `Session`
//! extractor, the `next` rule, form bodies and the CSRF check for posts.
//!
//! Cookies are read only under `/ui/` and bearer headers are ignored there;
//! the JSON API never reads cookies.
//!
//! **The marker hop** (owner decision 61). The session cookie is
//! `SameSite=Strict`, so a page opened from outside the site (Chrome for
//! Android's installed web app and home-screen shortcuts, a link in another
//! app: navigations Chrome counts as cross-site) arrives without it. Login
//! also sets a marker cookie (`zte_ui_signed_in=1`: no secret, the same
//! lifetime, `SameSite=Lax`, so it does come along). A GET page that
//! arrives with the marker but without the session cookie (`without_session`)
//! answers with a small page that opens the same URL once more, flagged
//! `hop=1` (`hop_page`, a meta refresh: a navigation the page itself starts,
//! so it is same-site and the browser sends the `Strict` cookie). There the
//! session either works (`303` to the URL without the flag) or is gone (the
//! login, and the marker is cleared). Never twice in a row (the flag, and a
//! same-site request or one that brought the session cookie never hops),
//! only to the request's own path, never for posts, live streams or the API.

use axum::body::Body;
use axum::extract::FromRequestParts;
use axum::http::header::{HeaderMap, HeaderValue, CONTENT_TYPE, LOCATION, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::response::Response;

use super::render;
use crate::auth::{AuthState, Kind, Principal};
use crate::httpd::api::{self, ReadErr};
use crate::httpd::guard::ui_post_allowed;
use crate::httpd::{Ctx, Peer};

pub const SESSION_COOKIE: &str = "zte_ui_session";
pub const PRE_COOKIE: &str = "zte_ui_pre";
/// Well-formed session cookie values looked at per request. Cookies are not
/// port-scoped: a page on this host's port 80 can set more of them with
/// deeper paths, which browsers send first. A request path matches at most
/// about a dozen path prefixes, so the live cookie stays within this many.
const MAX_SESSION_COOKIES: usize = 16;
/// Every `/ui` form body (login, logout, actions) is at most this long.
pub const FORM_MAX: usize = 8 * 1024;
/// The pre-login cookie lives as long as a login form token.
const PRE_MAX_AGE: u64 = 15 * 60;
/// Longest accepted `next` value.
const NEXT_MAX: usize = 200;

pub const CLEAR_SESSION: &str = "zte_ui_session=; Path=/ui/; Max-Age=0; HttpOnly; SameSite=Strict";
pub const CLEAR_PRE: &str = "zte_ui_pre=; Path=/ui/; Max-Age=0; HttpOnly; SameSite=Strict";
/// "This browser has a session" (the marker hop, above): a fixed value, no
/// secret.
pub const MARKER_COOKIE: &str = "zte_ui_signed_in";
pub const CLEAR_MARKER: &str = "zte_ui_signed_in=; Path=/ui/; Max-Age=0; HttpOnly; SameSite=Lax";
/// The query flag of the hop's request.
const HOP: &str = "hop=1";
/// Longest URL a hop goes to.
const HOP_MAX: usize = 400;

/// `Set-Cookie` for a new session; `Max-Age` is the session's absolute lifetime.
pub fn set_session(token: &str, max_age_secs: u64) -> String {
    format!("{SESSION_COOKIE}={token}; Path=/ui/; Max-Age={max_age_secs}; HttpOnly; SameSite=Strict")
}

/// `Set-Cookie` for the marker, with the session cookie's lifetime.
pub fn set_marker(max_age_secs: u64) -> String {
    format!("{MARKER_COOKIE}=1; Path=/ui/; Max-Age={max_age_secs}; HttpOnly; SameSite=Lax")
}

/// The request carries the marker.
pub fn has_marker(headers: &HeaderMap) -> bool {
    cookies(headers, MARKER_COOKIE).any(|v| v == "1")
}

/// `Set-Cookie` for the pre-login cookie the login form token is bound to.
pub fn set_pre(value: &str) -> String {
    format!("{PRE_COOKIE}={value}; Path=/ui/; Max-Age={PRE_MAX_AGE}; HttpOnly; SameSite=Strict")
}

/// Every value of cookie `name`, in header order, over all `Cookie` headers.
pub fn cookies<'a>(headers: &'a HeaderMap, name: &'a str) -> impl Iterator<Item = &'a str> {
    headers
        .get_all("cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(move |pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k.trim() == name).then(|| v.trim())
        })
}

/// The first well-formed (64 lowercase hex) presented session cookie values;
/// junk is skipped before anything is hashed.
pub fn session_cookies(headers: &HeaderMap) -> Vec<&str> {
    cookies(headers, SESSION_COOKIE).filter(|v| is_hex64(v)).take(MAX_SESSION_COOKIES).collect()
}

/// The session of the first presented cookie that authenticates (any
/// position); slides its idle deadline.
pub fn find_session(auth: &AuthState, headers: &HeaderMap) -> Option<Principal> {
    session_cookies(headers).into_iter().find_map(|v| auth.authenticate(Kind::Browser, v))
}

/// The first well-formed (64 lowercase hex) pre-login cookie value.
pub fn pre_cookie(headers: &HeaderMap) -> Option<&str> {
    cookies(headers, PRE_COOKIE).find(|v| is_hex64(v))
}

fn is_hex64(v: &str) -> bool {
    v.len() == 64 && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A `next` target is only ever a GET page of this UI: it starts with
/// `/ui/`, is short, uses a plain character set, has no dot segment (the
/// browser would resolve `/ui/x/../live/…` before fetching it) and is not
/// the login, logout, live, asset or operation path (an operation belongs to
/// its old session).
pub fn is_safe_next(s: &str) -> bool {
    const EXCLUDED: [&str; 5] = ["/ui/login", "/ui/logout", "/ui/live/", "/ui/a/", "/ui/op/"];
    s.starts_with("/ui/")
        && s.len() <= NEXT_MAX
        && !s.starts_with("//")
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_-.?=&%".contains(&b))
        && !has_dot_segment(s)
        && !EXCLUDED.iter().any(|p| s.starts_with(p))
}

/// A `.` or `..` segment in the path part, also percent-encoded (`%2e`):
/// browsers decode that one before resolving.
fn has_dot_segment(s: &str) -> bool {
    let path = s.split('?').next().unwrap_or(s);
    path.as_bytes().windows(3).any(|w| w[0] == b'%' && w[1] == b'2' && w[2].eq_ignore_ascii_case(&b'e'))
        || path.split('/').any(|seg| seg == "." || seg == "..")
}

/// `s` if it passes the `next` rule, otherwise the dashboard.
pub fn safe_next(s: Option<&str>) -> &str {
    s.filter(|s| is_safe_next(s)).unwrap_or("/ui/")
}

/// Everything but unreserved characters as `%XX`.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The first value of `key` in a query string, percent-decoded.
pub fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then(|| api::percent_decode(v))
    })
}

/// `303 See Other` to `location` (a server-built path).
pub fn see_other(location: &str) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::SEE_OTHER;
    if let Ok(v) = HeaderValue::from_str(location) {
        resp.headers_mut().insert(LOCATION, v);
    }
    resp
}

pub fn add_cookie(resp: &mut Response, cookie: &str) {
    if let Ok(v) = HeaderValue::from_str(cookie) {
        resp.headers_mut().append(SET_COOKIE, v);
    }
}

/// Where a request without a session goes. A GET page comes back to itself
/// after the login (if it passes the `next` rule). A post is never run: the
/// login leads back to the page of the form, with "nothing was changed".
pub fn login_redirect(method: &Method, path: &str, query: Option<&str>) -> Response {
    if method == Method::GET || method == Method::HEAD {
        // Search text never goes along (it can be a phone number).
        let query: Vec<&str> =
            query.unwrap_or("").split('&').filter(|p| !p.is_empty() && *p != "q" && !p.starts_with("q=")).collect();
        let target = match query.is_empty() {
            false => format!("{path}?{}", query.join("&")),
            true => path.to_owned(),
        };
        if is_safe_next(&target) {
            return see_other(&format!("/ui/login?next={}", percent_encode(&target)));
        }
        return see_other("/ui/login");
    }
    see_other(&format!("/ui/login?next={}&expired=1", percent_encode(form_page(path))))
}

/// The page that shows the form posting to `path` (`routes::ACTIONS`).
fn form_page(path: &str) -> &str {
    match super::routes::form_page(path) {
        Some(page) => page,
        None if is_safe_next(path) => path,
        None => "/ui/",
    }
}

/// The query without the hop's flag (and without `hop` at all).
fn without_hop(query: Option<&str>) -> Option<String> {
    let kept: Vec<&str> = query.unwrap_or("").split('&').filter(|p| !p.is_empty() && *p != "hop" && !p.starts_with("hop=")).collect();
    (!kept.is_empty()).then(|| kept.join("&"))
}

fn hopped(query: Option<&str>) -> bool {
    query.unwrap_or("").split('&').any(|p| p == HOP)
}

/// Where a hop goes: the request's own page with the flag (and without a
/// search, which can be a phone number, as `login_redirect`), if it is a
/// plain `/ui/` path (the `next` rule's character set, no dot segment, not
/// a live stream or an asset).
fn hop_target(path: &str, query: Option<&str>) -> Option<String> {
    let query: Vec<&str> = query.unwrap_or("").split('&').filter(|p| !p.is_empty() && *p != "q" && !p.starts_with("q=")).collect();
    let target = match query.is_empty() {
        true => format!("{path}?{HOP}"),
        false => format!("{path}?{}&{HOP}", query.join("&")),
    };
    let ok = target.starts_with("/ui/")
        && target.len() <= HOP_MAX
        && target.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_-.?=&%".contains(&b))
        && !has_dot_segment(&target)
        && !target.starts_with("/ui/live/")
        && !target.starts_with("/ui/a/");
    ok.then_some(target)
}

/// `Sec-Fetch-*` say this is not a page navigation (a fetch, an iframe);
/// browsers without them navigate.
fn not_a_navigation(headers: &HeaderMap) -> bool {
    api::header(headers, "sec-fetch-mode").is_some_and(|m| m != "navigate")
        || api::header(headers, "sec-fetch-dest").is_some_and(|d| d != "document")
}

/// A GET page that found no session: the hop (the module doc) when it can
/// help, else `to` (the login) with the query without the hop's flag. The
/// session is known to be gone when the request is the hop itself, brought
/// a session cookie, or came from this site (`Sec-Fetch-Site`: the browser
/// would have sent the `Strict` cookie); then the marker is cleared too.
pub fn without_session(
    headers: &HeaderMap,
    method: &Method,
    path: &str,
    query: Option<&str>,
    nonce: &str,
    to: impl FnOnce(Option<&str>) -> Response,
) -> Response {
    let marker = has_marker(headers);
    let site = api::header(headers, "sec-fetch-site");
    let gone = hopped(query) || !session_cookies(headers).is_empty() || matches!(site, Some("same-origin" | "same-site"));
    let query = without_hop(query);
    if marker && !gone && method == Method::GET && !not_a_navigation(headers) {
        if let Some(target) = hop_target(path, query.as_deref()) {
            return render::hop_page(&target, nonce);
        }
    }
    let mut resp = to(query.as_deref());
    if marker && gone {
        add_cookie(&mut resp, CLEAR_MARKER);
    }
    resp
}

/// A page with a session reached by the hop: `303` to it without the
/// flag (same-site, so the cookie goes along again).
pub fn after_hop(path: &str, query: Option<&str>) -> Option<Response> {
    hopped(query).then(|| match without_hop(query) {
        Some(q) => see_other(&format!("{path}?{q}")),
        None => see_other(path),
    })
}

/// Connected remotely: Tailscale's userspace networking makes every tailnet
/// request come from the loopback address.
pub fn is_remote(peer: Option<&Peer>) -> bool {
    peer.is_some_and(|p| match p.0.ip() {
        std::net::IpAddr::V4(a) => a.is_loopback(),
        std::net::IpAddr::V6(a) => a.is_loopback() || a.to_ipv4_mapped().is_some_and(|v| v.is_loopback()),
    })
}

/// An authenticated browser session (request extractor) for GET pages.
/// Without one the request is answered with `without_session` (the hop or
/// `login_redirect`).
///
/// Not for POST handlers: an extractor cannot reach the body, and a reply
/// that leaves a posted body unread may reach the client as a reset. A POST
/// handler looks the session up with `find_session` and answers a missing
/// one through `reply_unread`, as `pages::login::logout` does.
pub struct Session(pub Principal);

impl FromRequestParts<Ctx> for Session {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Self, Response> {
        debug_assert!(
            parts.method == Method::GET || parts.method == Method::HEAD,
            "Session extractor on a {} route; see its doc",
            parts.method
        );
        let (path, query) = (parts.uri.path(), parts.uri.query());
        match find_session(&ctx.app.auth, &parts.headers) {
            Some(p) => match after_hop(path, query) {
                Some(clean) => Err(clean),
                None => Ok(Session(p)),
            },
            None => {
                let nonce = parts.extensions.get::<super::CspNonce>().map(|n| n.0.as_str()).unwrap_or_default();
                Err(without_session(&parts.headers, &parts.method, path, query, nonce, |q| login_redirect(&parts.method, path, q)))
            }
        }
    }
}

/// A posted form from a page of this UI: same origin (by `Origin`, else
/// `Referer`) and the session's CSRF token.
pub fn verify_post(auth: &AuthState, headers: &HeaderMap, p: &Principal, csrf: Option<&str>) -> bool {
    ui_post_allowed(&api::req_headers(headers)) && csrf.is_some_and(|c| auth.check_csrf(p, c))
}

/// An `application/x-www-form-urlencoded` body; the first value of a
/// repeated field wins.
pub struct Form(Vec<(String, String)>);

impl Form {
    pub fn parse(body: &[u8]) -> Form {
        let text = String::from_utf8_lossy(body);
        Form(
            text.split('&')
                .filter(|p| !p.is_empty())
                .map(|pair| {
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    (api::percent_decode(k), api::percent_decode(v))
                })
                .collect(),
        )
    }

    /// Fields read some other way (a multipart upload's text parts).
    pub fn from_pairs(pairs: Vec<(String, String)>) -> Form {
        Form(pairs)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Every field as posted, in order.
    pub fn pairs(&self) -> &[(String, String)] {
        &self.0
    }
}

/// Read a form body of at most `FORM_MAX` bytes. Anything else is answered
/// with an error page: another content type 400, too long 413 (the rest is
/// drained so the connection can stay), a body that does not arrive 400.
pub async fn read_form(ctx: &Ctx, headers: &HeaderMap, body: Body, nonce: &str) -> Result<Form, Response> {
    let form_type = api::header(headers, "content-type").is_some_and(|ct| {
        ct.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/x-www-form-urlencoded")
    });
    if !form_type {
        let resp = render::bad_request(nonce);
        return Err(api::finish_early(headers, body.into_data_stream(), 0, resp).await);
    }
    match api::read_capped(headers, body, FORM_MAX, ctx.body_wait).await {
        Ok(bytes) => Ok(Form::parse(&bytes)),
        Err(ReadErr::TooLarge { read, rest }) => {
            let resp = render::error(413, "Form too large", "That form was too large to accept.", nonce);
            Err(api::finish_early(headers, rest, read, resp).await)
        }
        Err(ReadErr::Failed) => Err(api::close(render::bad_request(nonce))),
    }
}

/// Answer without reading the body (drained if small, else the connection closes).
pub async fn reply_unread(headers: &HeaderMap, body: Body, resp: Response) -> Response {
    api::finish_early(headers, body.into_data_stream(), 0, resp).await
}

/// `Content-Type` of a response, if any.
pub fn content_type(resp: &Response) -> Option<&str> {
    resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_rule() {
        for ok in ["/ui/", "/ui/signal", "/ui/settings?saved=roaming", "/ui/x%2Fy-z_1.2&a=b", "/ui/x?p=../y", "/ui/v1.2/..x"] {
            assert!(is_safe_next(ok), "{ok}");
        }
        let long = format!("/ui/{}", "a".repeat(197));
        assert_eq!(long.len(), 201);
        for bad in [
            "//evil", "/\\evil", "/ui/\\evil", "/ui/live/dashboard", "/ui/login", "/ui/login?next=/ui/",
            "/ui/logout", "/ui/a/x", "/ui/op/0123456789abcdef", "/ui", "/api/device", "http://evil/ui/",
            "/ui/<script>", "/ui/a b", "/ui/#frag", long.as_str(), "",
            // Dot segments: the browser resolves them before fetching.
            "/ui/x/../live/dashboard", "/ui/x/../op/0123456789abcdef", "/ui/./login", "/ui/../api/device",
            "/ui/%2e%2e/api/device", "/ui/%2E%2E/ui/login", "/ui/.%2e/api", "/ui/x/..", "/ui/x/.?a=b",
        ] {
            assert!(!is_safe_next(bad), "{bad}");
        }
        assert!(is_safe_next(&long[..200]));
        assert_eq!(safe_next(Some("//evil")), "/ui/");
        assert_eq!(safe_next(None), "/ui/");
        assert_eq!(safe_next(Some("/ui/signal")), "/ui/signal");
    }

    fn location(r: &Response) -> &str {
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        r.headers()[LOCATION].to_str().unwrap()
    }

    #[test]
    fn signed_out_redirects() {
        assert_eq!(location(&login_redirect(&Method::GET, "/ui/", None)), "/ui/login?next=%2Fui%2F");
        assert_eq!(
            location(&login_redirect(&Method::GET, "/ui/settings", Some("saved=roaming"))),
            "/ui/login?next=%2Fui%2Fsettings%3Fsaved%3Droaming"
        );
        // A search (it can be a phone number) is left out.
        assert_eq!(
            location(&login_redirect(&Method::GET, "/ui/sms", Some("q=5555550100&store=sim"))),
            "/ui/login?next=%2Fui%2Fsms%3Fstore%3Dsim"
        );
        assert_eq!(location(&login_redirect(&Method::GET, "/ui/sms", Some("q=55&q"))), "/ui/login?next=%2Fui%2Fsms");
        // Not a valid `next`: plain login.
        assert_eq!(location(&login_redirect(&Method::GET, "/ui/logout", None)), "/ui/login");
        assert_eq!(location(&login_redirect(&Method::HEAD, "/ui/op/00ff", None)), "/ui/login");
        // A post is never replayed: back to its form's page, flagged expired.
        assert_eq!(
            location(&login_redirect(&Method::POST, "/ui/router/mobile-network/apply", None)),
            "/ui/login?next=%2Fui%2Frouter%2Fmobile-network&expired=1"
        );
        assert_eq!(
            location(&login_redirect(&Method::POST, "/ui/router/device/charge-limit", None)),
            "/ui/login?next=%2Fui%2Frouter%2Fdevice&expired=1"
        );
        assert_eq!(location(&login_redirect(&Method::POST, "/ui/op/x", None)), "/ui/login?next=%2Fui%2F&expired=1");
    }

    #[test]
    fn cookies_parsed_from_every_header() {
        let v = |c: char| c.to_string().repeat(64);
        let (one, two, three) = (v('a'), v('b'), v('c'));
        let mut headers = HeaderMap::new();
        let first = format!("a=1; zte_ui_session={one} ;zte_ui_pre=xyz; zte_ui_session=junk");
        headers.append("cookie", HeaderValue::from_str(&first).unwrap());
        let second = format!("zte_ui_session={}; zte_ui_session={two}; zte_ui_session={three}", one.to_uppercase());
        headers.append("cookie", HeaderValue::from_str(&second).unwrap());
        assert_eq!(session_cookies(&headers), [one.as_str(), two.as_str(), three.as_str()]);
        assert_eq!(pre_cookie(&headers), None);
        // At most 16 values.
        let many = (0..20).map(|_| format!("zte_ui_session={one}")).collect::<Vec<_>>().join("; ");
        let headers = HeaderMap::from_iter([(axum::http::header::COOKIE, HeaderValue::from_str(&many).unwrap())]);
        assert_eq!(session_cookies(&headers).len(), 16);
        let pre = "ab".repeat(32);
        let headers = HeaderMap::from_iter([(
            axum::http::header::COOKIE,
            HeaderValue::from_str(&format!("zte_ui_pre=AB{}; zte_ui_pre={pre}", &pre[2..])).unwrap(),
        )]);
        assert_eq!(pre_cookie(&headers), Some(pre.as_str()));
    }

    #[test]
    fn remote_peers_are_loopback() {
        let peer = |ip: &str| Peer(std::net::SocketAddr::new(ip.parse().unwrap(), 1));
        assert!(is_remote(Some(&peer("127.0.0.1"))) && is_remote(Some(&peer("::1"))) && is_remote(Some(&peer("::ffff:127.0.0.1"))));
        assert!(!is_remote(Some(&peer("192.168.0.20"))) && !is_remote(None));
    }

    /// The marker hop (owner decision 61).
    #[test]
    fn hop_targets_are_the_requests_own_page() {
        assert_eq!(hop_target("/ui/", None).as_deref(), Some("/ui/?hop=1"));
        assert_eq!(hop_target("/ui/router/wifi", Some("t=00ff")).as_deref(), Some("/ui/router/wifi?t=00ff&hop=1"));
        // A search (it can be a phone number) is left out, as for `next`.
        assert_eq!(hop_target("/ui/sms", Some("q=5555550100&store=sim")).as_deref(), Some("/ui/sms?store=sim&hop=1"));
        let long = format!("/ui/{}", "a".repeat(400));
        for (path, query) in [
            ("/ui/live/dashboard/00ff", None),
            ("/ui/a/app.0123456789.css", None),
            ("/ui/x/../api/device", None),
            ("/ui/%2e%2e/api", None),
            ("/ui/x", Some("a=\"><b")),
            ("/ui/x", Some("a=1;url=//evil")),
            ("/api/device", None),
            (long.as_str(), None),
        ] {
            assert_eq!(hop_target(path, query), None, "{path} {query:?}");
        }
        assert_eq!(without_hop(Some("a=1&hop=1&b=2&hop")).as_deref(), Some("a=1&b=2"));
        assert_eq!(without_hop(Some("hop=1")), None);
    }

    fn hop_headers(cookies: &str, site: Option<&'static str>, mode: &'static str, dest: &'static str) -> HeaderMap {
        let mut h = HeaderMap::new();
        if !cookies.is_empty() {
            h.append("cookie", HeaderValue::from_str(cookies).unwrap());
        }
        if let Some(site) = site {
            h.append("sec-fetch-site", HeaderValue::from_static(site));
        }
        h.append("sec-fetch-mode", HeaderValue::from_static(mode));
        h.append("sec-fetch-dest", HeaderValue::from_static(dest));
        h
    }

    fn set_cookies(r: &Response) -> Vec<&str> {
        r.headers().get_all(axum::http::header::SET_COOKIE).iter().map(|v| v.to_str().unwrap()).collect()
    }

    #[tokio::test]
    async fn a_launch_from_outside_hops_once_to_find_the_strict_cookie() {
        let login = |q: Option<&str>| login_redirect(&Method::GET, "/ui/sms", q);
        let go = |h: &HeaderMap, method: &Method, query: Option<&str>| without_session(h, method, "/ui/sms", query, "00ff", login);
        let marker = "zte_ui_signed_in=1";
        // Launched from outside the site (an installed app, a link in
        // another app): the marker came, the Strict session cookie did not.
        for site in [Some("none"), Some("cross-site"), None] {
            let r = go(&hop_headers(marker, site, "navigate", "document"), &Method::GET, Some("store=sim"));
            assert_eq!(r.status(), StatusCode::OK, "{site:?}");
            assert!(set_cookies(&r).is_empty());
            let body = String::from_utf8(axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap().to_vec()).unwrap();
            assert!(body.contains(r#"<meta http-equiv="refresh" content="0; url=/ui/sms?store=sim&amp;hop=1">"#), "{body}");
            assert!(body.contains(r#"<a class="btn" href="/ui/sms?store=sim&amp;hop=1">Continue</a>"#), "{body}");
        }
        // The hop itself, a request from this site, or one that brought a
        // (stale) session cookie: the session is gone; login, marker cleared.
        let gone = [
            (hop_headers(marker, Some("same-origin"), "navigate", "document"), Some("store=sim&hop=1")),
            (hop_headers(marker, Some("same-origin"), "navigate", "document"), Some("store=sim")),
            (hop_headers(marker, Some("same-site"), "navigate", "document"), Some("store=sim")),
            (hop_headers(&format!("{marker}; zte_ui_session={}", "a".repeat(64)), Some("none"), "navigate", "document"), Some("store=sim")),
        ];
        for (h, query) in &gone {
            let r = go(h, &Method::GET, *query);
            assert_eq!(r.headers()[LOCATION], "/ui/login?next=%2Fui%2Fsms%3Fstore%3Dsim", "{query:?}");
            assert_eq!(set_cookies(&r), [CLEAR_MARKER]);
        }
        // Never for a post, a fetch, a frame, or without the marker.
        let no_hop = [
            (hop_headers(marker, Some("cross-site"), "navigate", "document"), Method::POST),
            (hop_headers(marker, Some("none"), "cors", "empty"), Method::GET),
            (hop_headers(marker, Some("none"), "navigate", "iframe"), Method::GET),
            (hop_headers("", Some("none"), "navigate", "document"), Method::GET),
            (hop_headers("zte_ui_signed_in=0", Some("none"), "navigate", "document"), Method::GET),
        ];
        for (h, method) in &no_hop {
            let r = go(h, method, Some("store=sim"));
            assert_eq!(r.status(), StatusCode::SEE_OTHER, "{method} {h:?}");
        }
        // With the session found on the hop: the page without the flag.
        assert_eq!(after_hop("/ui/sms", Some("store=sim&hop=1")).map(|r| r.headers()[LOCATION].clone()).unwrap(), "/ui/sms?store=sim");
        assert_eq!(after_hop("/ui/", Some("hop=1")).map(|r| r.headers()[LOCATION].clone()).unwrap(), "/ui/");
        assert!(after_hop("/ui/", Some("store=sim")).is_none() && after_hop("/ui/", None).is_none());
        assert_eq!(set_marker(604_800), "zte_ui_signed_in=1; Path=/ui/; Max-Age=604800; HttpOnly; SameSite=Lax");
        assert_eq!(CLEAR_MARKER, "zte_ui_signed_in=; Path=/ui/; Max-Age=0; HttpOnly; SameSite=Lax");
    }

    #[test]
    fn cookie_strings_are_exact() {
        assert_eq!(
            set_session("0f", 604_800),
            "zte_ui_session=0f; Path=/ui/; Max-Age=604800; HttpOnly; SameSite=Strict"
        );
        assert_eq!(set_pre("0f"), "zte_ui_pre=0f; Path=/ui/; Max-Age=900; HttpOnly; SameSite=Strict");
    }

    #[test]
    fn form_and_query_parsing() {
        let f = Form::parse(b"csrf=ab&next=%2Fui%2Fsignal&password=a+b%26c&csrf=zz&flag");
        assert_eq!((f.get("csrf"), f.get("next"), f.get("password")), (Some("ab"), Some("/ui/signal"), Some("a b&c")));
        assert_eq!((f.get("flag"), f.get("nope")), (Some(""), None));
        assert_eq!(query_param(Some("x=1&next=%2Fui%2F&next=2"), "next").as_deref(), Some("/ui/"));
        assert_eq!(query_param(None, "next"), None);
        assert_eq!(percent_encode("/ui/a?b=c&d"), "%2Fui%2Fa%3Fb%3Dc%26d");
    }
}
