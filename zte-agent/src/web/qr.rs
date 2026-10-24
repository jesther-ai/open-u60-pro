//! The Share Wi-Fi join code (iOS `WiFiQRGenerator`): the `WIFI:` payload
//! and its QR code, error correction M as the app's, drawn as inline SVG
//! (the CSP allows no `data:` images and no inline style; the colours are
//! SVG attributes, black on white in both colour schemes, as scanners
//! expect). Encoded by `qrcodegen` (MIT, Project Nayuki).

use std::fmt::Write;

use maud::{html, Markup};
use qrcodegen::{QrCode, QrCodeEcc, QrSegment, Version};

/// The app's security type (`mapEncryption`): "none" is open, anything
/// with SAE is WPA3, everything else WPA; also open for "" (no
/// `encryption` option, which the card calls "Open"; the app says WPA).
pub fn auth_type(encryption: &str) -> &'static str {
    let e = encryption.to_ascii_lowercase();
    if e == "none" || e.is_empty() {
        "nopass"
    } else if e.contains("sae") {
        "SAE"
    } else {
        "WPA"
    }
}

/// A backslash before each of `\ ; , : "` (the app's `escape`).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | ';' | ',' | ':' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `WIFI:T:<type>;S:<ssid>;P:<password>;;`, as the app builds it (no `H:`
/// for a hidden network).
pub fn payload(ssid: &str, password: &str, encryption: &str) -> String {
    format!("WIFI:T:{};S:{};P:{};;", auth_type(encryption), escape(ssid), escape(password))
}

/// The modules of `text` (UTF-8 bytes, ECC M exactly, the smallest
/// version), or `None` when it does not fit a QR code.
pub fn encode(text: &str) -> Option<QrCode> {
    let segs = [QrSegment::make_bytes(text.as_bytes())];
    QrCode::encode_segments_advanced(&segs, QrCodeEcc::Medium, Version::MIN, Version::MAX, None, false).ok()
}

/// The QR code as an `<svg>`: a white square with a 4-module quiet zone
/// and one path of the dark runs of each row.
pub fn svg(qr: &QrCode, label: &str) -> Markup {
    const QUIET: i32 = 4;
    let n = qr.size();
    let mut d = String::new();
    for y in 0..n {
        let mut x = 0;
        while x < n {
            if !qr.get_module(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < n && qr.get_module(x, y) {
                x += 1;
            }
            let _ = write!(d, "M{} {}h{}v1h-{}z", start + QUIET, y + QUIET, x - start, x - start);
        }
    }
    let side = n + 2 * QUIET;
    html! {
        svg.qr viewBox=(format!("0 0 {side} {side}")) role="img" aria-label=(label) shape-rendering="crispEdges" {
            rect width="100%" height="100%" fill="#fff" {}
            path d=(d) fill="#000" {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_escapes_like_the_app() {
        assert_eq!(payload("Home", "pass", "psk2+ccmp"), "WIFI:T:WPA;S:Home;P:pass;;");
        assert_eq!(payload(r#"a\b;c,d:e"f"#, "p;w", "sae-mixed"), r#"WIFI:T:SAE;S:a\\b\;c\,d\:e\"f;P:p\;w;;"#);
        for (enc, t) in [("none", "nopass"), ("NONE", "nopass"), ("sae", "SAE"), ("SAE+psk2", "SAE"), ("psk2", "WPA"), ("", "nopass"), ("psk-mixed", "WPA")] {
            assert_eq!(auth_type(enc), t, "{enc}");
        }
        // An open network still carries an empty password field, as the app's.
        assert_eq!(payload("Cafe", "", "none"), "WIFI:T:nopass;S:Cafe;P:;;");
        // Non-ASCII stays as it is (UTF-8 bytes in the code).
        assert_eq!(escape("Café ☕"), "Café ☕");
    }

    /// The SVG draws exactly the dark modules, inside a quiet zone, with
    /// attributes only (no inline style).
    #[test]
    fn svg_draws_the_modules() {
        let text = payload("Example-Net", "correct horse battery", "psk2+ccmp");
        let qr = encode(&text).unwrap();
        assert_eq!(qr.error_correction_level(), QrCodeEcc::Medium);
        let n = qr.size();
        let s = svg(&qr, "Wi-Fi QR code").into_string();
        assert!(s.starts_with(&format!(
            "<svg class=\"qr\" viewBox=\"0 0 {0} {0}\" role=\"img\" aria-label=\"Wi-Fi QR code\" shape-rendering=\"crispEdges\">\
             <rect width=\"100%\" height=\"100%\" fill=\"#fff\"></rect><path d=\"M",
            n + 8
        )), "{s}");
        assert!(!s.contains("style"));
        // Rebuild the grid from the path runs.
        let d = s.split(" d=\"").nth(1).unwrap().split('"').next().unwrap();
        let mut grid = vec![vec![false; n as usize]; n as usize];
        for run in d.split('M').skip(1) {
            let run = run.strip_suffix('z').unwrap();
            let (xy, rest) = run.split_once('h').unwrap();
            let (x, y) = xy.split_once(' ').unwrap();
            let len: i32 = rest.split('v').next().unwrap().parse().unwrap();
            let (x, y): (i32, i32) = (x.parse::<i32>().unwrap() - 4, y.parse::<i32>().unwrap() - 4);
            for i in 0..len {
                grid[y as usize][(x + i) as usize] = true;
            }
        }
        for y in 0..n {
            for x in 0..n {
                assert_eq!(grid[y as usize][x as usize], qr.get_module(x, y), "({x}, {y})");
            }
        }
        // Too long for any QR code.
        assert!(encode(&"x".repeat(3000)).is_none());
    }
}
