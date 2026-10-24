//! Request-header checks shared by the JSON API and the web UI, free of any
//! HTTP framework: a Host allowlist (DNS-rebinding defence) and exact
//! Origin / Referer / `Sec-Fetch-Site` checks for state-changing requests.

use std::net::{Ipv4Addr, Ipv6Addr};

use crate::server::Method;

/// The headers the checks look at (first occurrence of each).
#[derive(Clone, Copy, Default)]
pub struct ReqHeaders<'a> {
    pub host: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub referer: Option<&'a str>,
    pub sec_fetch_site: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Authority {
    V4(Ipv4Addr),
    V6(Ipv6Addr),
    /// Lowercased, one trailing '.' removed.
    Name(String),
}

/// `host[:port]` or `[v6][:port]`. Rejects empty input, whitespace, '@',
/// bad brackets, a bad port and characters no host name has. IPv4 must be
/// dotted-quad (`Ipv4Addr::from_str`); shorthand such as `127.1` or
/// `0x7f.0.0.1` is not an address, and since its last label is numeric it is
/// not a name either.
pub fn parse_authority(s: &str) -> Option<(Authority, Option<u16>)> {
    if s.is_empty() || s.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control() || b == b'@') {
        return None;
    }
    if let Some(rest) = s.strip_prefix('[') {
        let (inside, after) = rest.split_once(']')?;
        let port = match after {
            "" => None,
            p => Some(parse_port(p.strip_prefix(':')?)?),
        };
        return Some((Authority::V6(inside.parse().ok()?), port));
    }
    let (host, port) = match s.split_once(':') {
        Some((h, p)) => (h, Some(parse_port(p)?)),
        None => (s, None),
    };
    if host.contains(['[', ']']) {
        return None;
    }
    let host = host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    if let Ok(v4) = host.parse::<Ipv4Addr>() {
        return Some((Authority::V4(v4), port));
    }
    let valid_char = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.';
    if host.is_empty() || !host.bytes().all(valid_char) || host.split('.').any(str::is_empty) {
        return None;
    }
    // A last label that is a number (decimal or 0x hex) makes browsers treat
    // the host as an IPv4 address in some other notation.
    let last = host.rsplit('.').next().unwrap_or(host);
    let numeric = last.bytes().all(|b| b.is_ascii_digit())
        || last
            .strip_prefix("0x")
            .is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
    if numeric {
        return None;
    }
    Some((Authority::Name(host.to_owned()), port))
}

fn parse_port(p: &str) -> Option<u16> {
    if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    p.parse().ok()
}

/// Host names the agent answers to besides IP literals (and, for loopback
/// peers, `localhost` and the Tailscale names; over the HTTPS listener,
/// the LAN HTTPS name).
pub struct HostPolicy {
    extra: Vec<String>,
    /// The LAN HTTPS name (`tls`, `u60.home.arpa`).
    local: Option<String>,
}

impl HostPolicy {
    /// `ZTE_AGENT_UI_HOSTS`: comma-separated exact names (e.g. a local DNS
    /// name), and `local`: the LAN HTTPS name, answered only over HTTPS
    /// (`allows_via`): another network's DNS can answer for a `home.arpa`
    /// name too (a rebinding target over http), but it cannot present the
    /// router's certificate for it.
    pub fn from_env_with(local: &str) -> Self {
        let extra: Vec<String> = std::env::var("ZTE_AGENT_UI_HOSTS")
            .map(|v| v.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        HostPolicy { local: Some(local.to_ascii_lowercase()), ..Self::new(extra) }
    }

    /// Each entry is parsed like a Host header, so it matches exactly what
    /// `allows` compares; a port is ignored there too. IP literals always
    /// pass, and an entry that is no host name is reported and dropped.
    pub fn new(extra: Vec<String>) -> Self {
        let mut names = Vec::new();
        for raw in extra.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
            match parse_authority(raw) {
                Some((Authority::Name(name), _)) => names.push(name),
                Some(_) => {}
                None => eprintln!("[httpd] ZTE_AGENT_UI_HOSTS: ignoring {raw:?} (not a host name)"),
            }
        }
        HostPolicy { extra: names, local: None }
    }

    /// `allows`, and over the HTTPS listener (`secure`, which runs only
    /// while HTTPS is on) also the LAN HTTPS name.
    pub fn allows_via(&self, host_header: &str, loopback: bool, secure: bool, ts_names: &dyn Fn() -> Vec<String>) -> bool {
        self.allows(host_header, loopback, ts_names)
            || (secure && matches!(parse_authority(host_header), Some((Authority::Name(n), _)) if Some(&n) == self.local.as_ref()))
    }

    /// Any IP literal or a configured name; `localhost` and the Tailscale
    /// names only from a `loopback` peer (Tailscale clients arrive as
    /// 127.0.0.1, so a LAN browser asking for those single-label names was
    /// sent there by DNS). The port is ignored. `ts_names` is only called
    /// for a loopback peer's name that is neither `localhost` nor configured.
    pub fn allows(&self, host_header: &str, loopback: bool, ts_names: &dyn Fn() -> Vec<String>) -> bool {
        match parse_authority(host_header) {
            None => false,
            Some((Authority::V4(_) | Authority::V6(_), _)) => true,
            Some((Authority::Name(name), _)) => {
                self.extra.contains(&name)
                    || (loopback && (name == "localhost" || ts_names().contains(&name)))
            }
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OriginVerdict {
    Same,
    Cross,
    Absent,
}

/// `scheme://authority` (anything after the authority is the caller's
/// business) compared with `http://<Host>`, port defaulting to 80.
fn same_as_host(scheme: &str, authority: &str, host: Option<&str>) -> OriginVerdict {
    if !scheme.eq_ignore_ascii_case("http") {
        return OriginVerdict::Cross;
    }
    let (Some(origin), Some(host)) = (parse_authority(authority), host.and_then(parse_authority)) else {
        return OriginVerdict::Cross;
    };
    if origin.0 == host.0 && origin.1.unwrap_or(80) == host.1.unwrap_or(80) {
        OriginVerdict::Same
    } else {
        OriginVerdict::Cross
    }
}

/// `Origin` against the Host header. `null`, a path or anything malformed
/// counts as cross-origin.
pub fn origin_verdict(h: &ReqHeaders) -> OriginVerdict {
    let Some(origin) = h.origin else {
        return OriginVerdict::Absent;
    };
    match origin.split_once("://") {
        Some((scheme, authority)) => same_as_host(scheme, authority, h.host),
        None => OriginVerdict::Cross,
    }
}

/// The `scheme://authority` prefix of `Referer`, by the same rules.
pub fn referer_verdict(h: &ReqHeaders) -> OriginVerdict {
    let Some(referer) = h.referer else {
        return OriginVerdict::Absent;
    };
    let Some((scheme, rest)) = referer.split_once("://") else {
        return OriginVerdict::Cross;
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    same_as_host(scheme, &rest[..end], h.host)
}

/// `Sec-Fetch-Site` present and neither `same-origin` nor `none`.
pub fn fetch_site_foreign(h: &ReqHeaders) -> bool {
    h.sec_fetch_site.is_some_and(|v| {
        let v = v.trim();
        !v.eq_ignore_ascii_case("same-origin") && !v.eq_ignore_ascii_case("none")
    })
}

/// Anything but GET that a browser marks as coming from another site. The
/// apps send neither header, so they are never refused.
pub fn api_mutation_refused(m: Method, h: &ReqHeaders) -> bool {
    m != Method::Get && (origin_verdict(h) == OriginVerdict::Cross || fetch_site_foreign(h))
}

/// Web UI form posts: same-origin by `Origin`, or by `Referer` when the
/// browser sent no `Origin`. The CSRF token is still always required.
pub fn ui_post_allowed(h: &ReqHeaders) -> bool {
    !fetch_site_foreign(h)
        && match origin_verdict(h) {
            OriginVerdict::Same => true,
            OriginVerdict::Cross => false,
            OriginVerdict::Absent => referer_verdict(h) != OriginVerdict::Cross,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn ts_enabled() -> Vec<String> {
        ["u60-pro", "u60-pro-1.tailnet.ts.net", "u60-pro-1"].map(String::from).to_vec()
    }

    fn policy() -> HostPolicy {
        HostPolicy::new(vec![" Router.Lan. ".into(), "".into()])
    }

    #[test]
    fn hosts_accepted() {
        let p = policy();
        for host in [
            "192.168.0.1:9090",
            "192.168.0.1",
            "10.9.8.7",
            "[fe80::1]:9090",
            "[::1]",
            "LOCALHOST",
            "localhost:9090",
            "localhost.",
            "router.lan",
            "ROUTER.LAN:9090",
            "u60-pro",
            "u60-pro-1.tailnet.ts.net:9090",
            "U60-PRO-1",
        ] {
            assert!(p.allows(host, true, &ts_enabled), "refused {host}");
        }
        // From the LAN: IP literals and configured names only.
        for host in ["192.168.0.1:9090", "[fe80::1]:9090", "router.lan"] {
            assert!(p.allows(host, false, &ts_enabled), "refused {host} from the LAN");
        }
        for host in ["localhost:9090", "u60-pro", "u60-pro-1.tailnet.ts.net:9090", "U60-PRO-1"] {
            assert!(!p.allows(host, false, &ts_enabled), "accepted {host} from the LAN");
        }
    }

    #[test]
    fn hosts_rejected() {
        let p = policy();
        for host in [
            "evil.com:9090",
            "evil.com",
            "192.168.0.1.nip.io",
            "127.1",
            "0x7f.0.0.1",
            "0x7f000001",
            "2130706433",
            "",
            "a b",
            "user@h",
            "user@192.168.0.1",
            "[fe80::1",
            "fe80::1]",
            "fe80::1",
            "[fe80::1]x",
            "[nothex]",
            "host:",
            "host:99999",
            "host:+80",
            "host:80:80",
            "a..b",
            ".",
            "ho/st",
            "ho\\st",
            "ho%st",
        ] {
            assert!(!p.allows(host, true, &ts_enabled), "accepted {host:?}");
        }
        // Tailscale names only while Tailscale is enabled.
        for host in ["u60-pro", "u60-pro-1.tailnet.ts.net", "u60-pro-1"] {
            assert!(!p.allows(host, true, &Vec::new), "accepted {host} with Tailscale disabled");
        }
    }

    #[test]
    fn ts_names_not_called_for_ip_literals_or_localhost() {
        let p = policy();
        let calls = Cell::new(0);
        let names = || {
            calls.set(calls.get() + 1);
            ts_enabled()
        };
        for host in ["192.168.0.1:9090", "[fe80::1]", "localhost", "router.lan", "127.1", "a b"] {
            let _ = p.allows(host, true, &names);
        }
        assert!(!p.allows("u60-pro", false, &names));
        assert_eq!(calls.get(), 0);
        assert!(p.allows("u60-pro", true, &names));
        assert!(!p.allows("evil.com", true, &names));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn parse_authority_normalises() {
        assert_eq!(parse_authority("Example.COM.:8080"), Some((Authority::Name("example.com".into()), Some(8080))));
        assert_eq!(parse_authority("[FE80::1]"), Some((Authority::V6("fe80::1".parse().unwrap()), None)));
        assert_eq!(parse_authority("10.0.0.1:80"), Some((Authority::V4(Ipv4Addr::new(10, 0, 0, 1)), Some(80))));
        assert_eq!(parse_authority("example.com.."), None);
    }

    /// The LAN HTTPS name answers only over the HTTPS listener (another
    /// network's DNS can answer for it over http); names around it never
    /// (DNS-rebinding protection otherwise).
    #[test]
    fn the_local_name_is_allowed_over_https_only() {
        let p = HostPolicy::from_env_with("u60.home.arpa");
        for host in ["u60.home.arpa", "U60.HOME.ARPA:9443", "u60.home.arpa.:9090"] {
            assert!(p.allows_via(host, false, true, &Vec::new), "{host}");
            assert!(!p.allows_via(host, false, false, &Vec::new) && !p.allows_via(host, true, false, &Vec::new), "{host} over http");
            assert!(!p.allows(host, false, &Vec::new), "{host}");
        }
        for host in ["home.arpa", "x.u60.home.arpa", "u60.home.arpa.evil.example", "evil.example"] {
            assert!(!p.allows_via(host, false, true, &Vec::new), "{host}");
        }
        assert!(p.allows_via("192.168.0.1:9443", false, true, &Vec::new));
    }

    #[test]
    fn configured_names_parsed_like_host() {
        let p = HostPolicy::new(
            ["Router.LAN:9090", "nas.lan.", "10.0.0.9", "2130706433", "a b", "user@h", ""]
                .map(String::from)
                .to_vec(),
        );
        assert_eq!(p.extra, ["router.lan", "nas.lan"]);
        assert!(p.allows("router.lan", false, &Vec::new));
        assert!(p.allows("NAS.lan:9090", false, &Vec::new));
    }

    fn h<'a>(host: &'a str, origin: Option<&'a str>, referer: Option<&'a str>, site: Option<&'a str>) -> ReqHeaders<'a> {
        ReqHeaders { host: Some(host), origin, referer, sec_fetch_site: site }
    }

    #[test]
    fn origin_rules() {
        use OriginVerdict::*;
        let o = |host, origin| origin_verdict(&h(host, Some(origin), None, None));
        assert_eq!(o("192.168.0.1:9090", "http://192.168.0.1:9090"), Same);
        assert_eq!(o("Router.Lan:9090", "http://router.lan:9090"), Same);
        assert_eq!(o("192.168.0.1:9090", "http://192.168.0.1:9091"), Cross);
        assert_eq!(o("192.168.0.1:9090", "http://192.168.0.1"), Cross);
        assert_eq!(o("192.168.0.1:9090", "https://192.168.0.1:9090"), Cross);
        assert_eq!(o("192.168.0.1:9090", "null"), Cross);
        assert_eq!(o("192.168.0.1:9090", "http://192.168.0.1:9090/path"), Cross);
        assert_eq!(o("192.168.0.1:9090", "http://evil.com:9090"), Cross);
        assert_eq!(o("192.168.0.1:9090", "garbage"), Cross);
        assert_eq!(o("example:80", "http://example"), Same);
        assert_eq!(o("example", "http://example:80"), Same);
        assert_eq!(o("[fe80::1]:9090", "http://[fe80::1]:9090"), Same);
        assert_eq!(origin_verdict(&h("example", None, None, None)), Absent);
        // No Host to compare with.
        let no_host = ReqHeaders { origin: Some("http://example"), ..ReqHeaders::default() };
        assert_eq!(origin_verdict(&no_host), Cross);
    }

    #[test]
    fn referer_rules() {
        use OriginVerdict::*;
        let r = |referer| referer_verdict(&h("192.168.0.1:9090", None, Some(referer), None));
        assert_eq!(r("http://192.168.0.1:9090/ui/settings?x=1"), Same);
        assert_eq!(r("http://192.168.0.1:9090"), Same);
        assert_eq!(r("http://192.168.0.1:9090?q"), Same);
        assert_eq!(r("http://evil.com/ui/"), Cross);
        assert_eq!(r("https://192.168.0.1:9090/"), Cross);
        assert_eq!(r("/ui/settings"), Cross);
        assert_eq!(referer_verdict(&h("x", None, None, None)), Absent);
    }

    #[test]
    fn ui_post_rules() {
        let host = "192.168.0.1:9090";
        let same = "http://192.168.0.1:9090";
        assert!(ui_post_allowed(&h(host, Some(same), None, None)));
        assert!(ui_post_allowed(&h(host, Some(same), None, Some("same-origin"))));
        assert!(!ui_post_allowed(&h(host, Some("http://evil.com"), None, None)));
        assert!(!ui_post_allowed(&h(host, Some("null"), None, None)));
        // Missing Origin: a same-origin Referer passes, a cross one is refused.
        assert!(ui_post_allowed(&h(host, None, Some("http://192.168.0.1:9090/ui/"), None)));
        assert!(!ui_post_allowed(&h(host, None, Some("http://evil.com/"), None)));
        assert!(ui_post_allowed(&h(host, None, None, None)));
        // Sec-Fetch-Site from another site refuses even with a same Origin.
        assert!(!ui_post_allowed(&h(host, Some(same), None, Some("same-site"))));
        assert!(!ui_post_allowed(&h(host, Some(same), None, Some("cross-site"))));
        assert!(ui_post_allowed(&h(host, Some(same), None, Some("none"))));
    }

    #[test]
    fn api_mutation_rules() {
        let host = "192.168.0.1:9090";
        let cross = h(host, Some("http://evil.com"), None, None);
        assert!(!api_mutation_refused(Method::Get, &cross));
        assert!(api_mutation_refused(Method::Post, &cross));
        assert!(api_mutation_refused(Method::Put, &cross));
        assert!(api_mutation_refused(Method::Delete, &cross));
        assert!(api_mutation_refused(Method::Post, &h(host, None, None, Some("same-site"))));
        assert!(api_mutation_refused(Method::Post, &h(host, None, None, Some("cross-site"))));
        // The apps send neither header.
        assert!(!api_mutation_refused(Method::Post, &h(host, None, None, None)));
        assert!(!api_mutation_refused(Method::Post, &h(host, Some("http://192.168.0.1:9090"), None, Some("same-origin"))));
        // The API ignores Referer.
        assert!(!api_mutation_refused(Method::Post, &h(host, None, Some("http://evil.com/"), None)));
    }
}
