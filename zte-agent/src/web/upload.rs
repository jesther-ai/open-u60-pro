//! Photo uploads (eSIM › Add eSIM): a bounded `multipart/form-data` reader.
//!
//! - The boundary comes from the `Content-Type` header: 1–70 characters of
//!   RFC 2046's set, else `400`.
//! - The body is at most `MAX_BODY` (a photo of `qr_decode::MAX_BYTES` and
//!   room for the text fields): a declared length over it is refused before
//!   anything is read, and bytes are counted as they arrive, so nothing is
//!   allocated past it (`413`, the rest drained or the connection closed).
//! - A body that pauses for `Ctx::body_wait` between reads, or has not
//!   arrived after `TOTAL_WAIT`, is refused (`400`; a phone on a slow link
//!   still gets two minutes for 8 MiB).
//! - One upload at a time, from reading it until its photo is decoded
//!   (`Held`): another waits up to `SLOT_WAIT` (a double click's second post
//!   then finds the first's single-use token), else gets the busy page. So
//!   at most one photo is in RAM (and one more while a decode that ran over
//!   its limit finishes; the decoder refuses others meanwhile).
//! - At most 8 parts; part headers at most 1 KiB; text fields at most 1 KiB
//!   of UTF-8 each (an activation code is at most 512 bytes). Anything else
//!   is `400`.
//!
//! The bytes are never logged or stored: the caller hands the photo to the
//! decoder, which drops it when it is done; text fields (an activation code,
//! a confirmation code) live only in the request's `Form`.

use std::ops::Range;
use std::time::Duration;

use axum::body::Body;
use axum::http::HeaderMap;
use axum::response::Response;
use futures_util::StreamExt;
use tokio::sync::{Semaphore, SemaphorePermit};

use super::qr_decode;
use super::render;
use super::session::Form;
use crate::httpd::api::{self, ReadErr};
use crate::httpd::Ctx;

/// The largest photo (8 MiB; phone photos are 2–6 MB).
pub const MAX_FILE: usize = qr_decode::MAX_BYTES;
/// The photo and room for the text fields and part headers.
pub const MAX_BODY: usize = MAX_FILE + 64 * 1024;
const MAX_TEXT: usize = 1024;
const MAX_PARTS: usize = 8;
const MAX_HEAD: usize = 1024;
const TOTAL_WAIT: Duration = Duration::from_secs(120);
/// How long an upload waits for the one before it.
const SLOT_WAIT: Duration = if cfg!(test) { Duration::from_millis(300) } else { Duration::from_secs(10) };

static SLOT: Semaphore = Semaphore::const_new(1);

/// The upload slot: held while an upload (or its photo) is in RAM.
pub struct Held(#[allow(dead_code)] SemaphorePermit<'static>);

/// A read upload: its text fields, and its file parts with content.
pub struct Upload {
    pub form: Form,
    body: Vec<u8>,
    /// Non-empty file parts: field name, bytes in `body`.
    files: Vec<(String, Range<usize>)>,
    held: Held,
}

impl Upload {
    /// The names of the file parts that hold something (a file input left
    /// empty sends a part without content).
    pub fn files(&self) -> Vec<&str> {
        self.files.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// The first file part's bytes, in the body's own buffer (no copy;
    /// the rest of the body is dropped), and the slot, to hold until the
    /// photo is decoded.
    pub fn into_file(self) -> Option<(Vec<u8>, Held)> {
        let Upload { mut body, files, held, .. } = self;
        let (_, r) = files.into_iter().next()?;
        body.truncate(r.end);
        body.drain(..r.start);
        Some((body, held))
    }
}

/// Read a multipart upload posted to the page `back` (the way back on its
/// error pages). The caller has found the session and checked the origin;
/// the CSRF token is one of the text fields.
pub async fn read(ctx: &Ctx, headers: &HeaderMap, body: Body, nonce: &str, back: &str) -> Result<Upload, Response> {
    let Some(boundary) = boundary(headers) else {
        return Err(api::finish_early(headers, body.into_data_stream(), 0, render::bad_request(nonce)).await);
    };
    let Ok(Ok(permit)) = tokio::time::timeout(SLOT_WAIT, SLOT.acquire()).await else {
        let msg = "The router is reading another photo. Try again in a moment.";
        let resp = render::html(503, render::error_page("Router busy", msg, nonce, Some(back)));
        return Err(api::finish_early(headers, body.into_data_stream(), 0, resp).await);
    };
    let held = Held(permit);
    let bytes = match read_body(headers, body, ctx.body_wait, TOTAL_WAIT).await {
        Ok(b) => b,
        Err(ReadErr::TooLarge { read, rest }) => {
            drop(held);
            let msg = "Photos can be up to 8 MB. Choose a smaller image or enter the code manually.";
            let resp = render::html(413, render::error_page("Photo too large", msg, nonce, Some(back)));
            return Err(api::finish_early(headers, rest, read, resp).await);
        }
        Err(ReadErr::Failed) => return Err(api::close(render::bad_request(nonce))),
    };
    match parse(&bytes, &boundary) {
        Some((text, files)) => Ok(Upload { form: Form::from_pairs(text), body: bytes, files, held }),
        None => Err(render::bad_request(nonce)),
    }
}

/// The boundary of a `multipart/form-data` content type.
fn boundary(headers: &HeaderMap) -> Option<String> {
    let ct = api::header(headers, "content-type")?;
    let mut params = ct.split(';');
    if !params.next()?.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    let b = params.find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("boundary").then(|| v.trim())
    })?;
    let b = b.strip_prefix('"').and_then(|b| b.strip_suffix('"')).unwrap_or(b);
    // RFC 2046 bchars, not ending in a space.
    let ok = (1..=70).contains(&b.len())
        && !b.ends_with(' ')
        && b.bytes().all(|c| c.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&c));
    ok.then(|| b.to_owned())
}

/// The whole body if it is at most `MAX_BODY` bytes, arrives without a
/// pause of `idle` and within `total`.
async fn read_body(headers: &HeaderMap, body: Body, idle: Duration, total: Duration) -> Result<Vec<u8>, ReadErr> {
    let mut rest = body.into_data_stream();
    let declared = api::header(headers, "content-length").and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > MAX_BODY as u64) {
        return Err(ReadErr::TooLarge { read: 0, rest });
    }
    let mut buf = Vec::with_capacity(declared.unwrap_or(0) as usize);
    let end = tokio::time::Instant::now() + total;
    loop {
        let by = end.min(tokio::time::Instant::now() + idle);
        match tokio::time::timeout_at(by, rest.next()).await {
            Ok(Some(Ok(chunk))) => {
                if buf.len() + chunk.len() > MAX_BODY {
                    return Err(ReadErr::TooLarge { read: (buf.len() + chunk.len()) as u64, rest });
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(buf),
            Ok(Some(Err(_))) | Err(_) => return Err(ReadErr::Failed),
        }
    }
}

/// Text fields and non-empty file parts of a multipart body; `None` for
/// anything malformed or over the limits.
#[allow(clippy::type_complexity)]
fn parse(body: &[u8], boundary: &str) -> Option<(Vec<(String, String)>, Vec<(String, Range<usize>)>)> {
    let delim = format!("--{boundary}");
    let d = delim.as_bytes();
    // Browsers send no preamble.
    let mut pos = body.starts_with(d).then_some(d.len())?;
    let (mut text, mut files) = (Vec::new(), Vec::new());
    for _ in 0..=MAX_PARTS {
        let rest = &body[pos..];
        if rest.starts_with(b"--") {
            return Some((text, files));
        }
        let start = pos + rest.starts_with(b"\r\n").then_some(2)?;
        let window = &body[start..body.len().min(start + MAX_HEAD + 4)];
        let head_len = window.windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = std::str::from_utf8(&window[..head_len]).ok()?;
        let content = start + head_len + 4;
        let end = content + next_delimiter(&body[content..], d)?;
        let (name, file) = disposition(head)?;
        if file {
            if end > content {
                files.push((name, content..end));
            }
        } else {
            if end - content > MAX_TEXT {
                return None;
            }
            text.push((name, std::str::from_utf8(&body[content..end]).ok()?.to_owned()));
        }
        pos = end + 2 + d.len();
    }
    None
}

/// Where "\r\n" and the delimiter `d` start in `hay`.
fn next_delimiter(hay: &[u8], d: &[u8]) -> Option<usize> {
    memchr::memchr_iter(b'\r', hay).find(|&i| hay[i..].starts_with(b"\r\n") && hay[i + 2..].starts_with(d))
}

/// The field name of a part and whether it is a file (`filename` given),
/// from its `Content-Disposition: form-data; name="…"[; filename="…"]`.
fn disposition(head: &str) -> Option<(String, bool)> {
    let value = head.split("\r\n").find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case("content-disposition").then_some(v)
    })?;
    let params = split_params(value);
    let mut it = params.iter();
    if !it.next()?.eq_ignore_ascii_case("form-data") {
        return None;
    }
    let (mut name, mut file) = (None, false);
    for p in it {
        let (k, v) = p.split_once('=')?;
        match k.trim().to_ascii_lowercase().as_str() {
            "name" => name = Some(unquote(v.trim())),
            "filename" | "filename*" => file = true,
            _ => {}
        }
    }
    let name = name.filter(|n| !n.is_empty() && n.len() <= 32)?;
    Some((name, file))
}

/// `;`-separated parameters, a `;` inside quotes kept (a file name may hold one).
fn split_params(v: &str) -> Vec<&str> {
    let (mut out, mut from, mut quoted, mut escaped) = (Vec::new(), 0, false, false);
    for (i, c) in v.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                out.push(v[from..i].trim());
                from = i + 1;
            }
            _ => {}
        }
    }
    out.push(v[from..].trim());
    out
}

fn unquote(v: &str) -> String {
    match v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(q) => q.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => v.to_owned(),
    }
}

/// Tests: hold the upload slot (the next upload waits, then is refused).
#[cfg(test)]
pub fn hold() -> Option<Held> {
    SLOT.try_acquire().ok().map(Held)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A body that stops (15 s without a byte) or trickles (2 minutes in
    /// all) is refused.
    #[tokio::test(start_paused = true)]
    async fn slow_bodies_time_out() {
        use axum::body::Bytes;
        use futures_util::stream;
        let (idle, total) = (Duration::from_secs(15), Duration::from_secs(120));
        let chunk = || Ok::<_, std::io::Error>(Bytes::from_static(b"--x\r\n"));
        let stalled = stream::once(async move { chunk() }).chain(stream::pending());
        let started = tokio::time::Instant::now();
        let r = read_body(&HeaderMap::new(), Body::from_stream(stalled), idle, total).await;
        assert!(matches!(r, Err(ReadErr::Failed)) && started.elapsed() == idle, "{:?}", started.elapsed());
        let trickle = stream::unfold((), move |()| async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Some((chunk(), ()))
        });
        let started = tokio::time::Instant::now();
        let r = read_body(&HeaderMap::new(), Body::from_stream(trickle), idle, total).await;
        assert!(matches!(r, Err(ReadErr::Failed)) && started.elapsed() == total, "{:?}", started.elapsed());
    }

    const B: &str = "----WebKitFormBoundaryFAKE0123";

    /// A body as a browser sends it: (name, Some(filename) for a file, content).
    pub fn body(boundary: &str, parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, file, content) in parts {
            out.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"").as_bytes());
            if let Some(f) = file {
                out.extend_from_slice(format!("; filename=\"{f}\"\r\nContent-Type: image/jpeg").as_bytes());
            }
            out.extend_from_slice(b"\r\n\r\n");
            out.extend_from_slice(content);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        out
    }

    fn headers(ct: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("content-type", ct.parse().unwrap());
        h
    }

    #[test]
    fn boundaries_follow_rfc_2046() {
        assert_eq!(boundary(&headers(&format!("multipart/form-data; boundary={B}"))).as_deref(), Some(B));
        assert_eq!(boundary(&headers("Multipart/Form-Data; charset=utf-8; boundary=\"a b'c\"")).as_deref(), Some("a b'c"));
        for bad in [
            "application/x-www-form-urlencoded", "multipart/form-data", "multipart/form-data; boundary=",
            "multipart/form-data; boundary=a<b", "multipart/mixed; boundary=abc", "multipart/form-data; boundary=\"ab \"",
        ] {
            assert_eq!(boundary(&headers(bad)), None, "{bad}");
        }
        let long = format!("multipart/form-data; boundary={}", "a".repeat(71));
        assert_eq!(boundary(&headers(&long)), None);
    }

    /// Text fields, a file, an empty file input, CRLFs and the delimiter's
    /// look-alikes inside the file.
    #[test]
    fn parses_fields_and_files() {
        let photo: &[u8] = b"\xff\xd8\r\n--not-it\r\r\n\r\n----WebKitFormBoundaryFAKE012\xff\xd9";
        let b = body(B, &[("csrf", None, b"abc"), ("once", None, b"0123"), ("scan", Some(""), b""), ("photo", Some("a;b \"x\".jpg"), photo), ("code", None, b"")]);
        let (text, files) = parse(&b, B).unwrap();
        assert_eq!(text, [("csrf".into(), "abc".into()), ("once".into(), "0123".into()), ("code".into(), String::new())]);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "photo");
        assert_eq!(&b[files[0].1.clone()], photo);
    }

    #[test]
    fn refuses_what_does_not_fit() {
        let ok = body(B, &[("a", None, b"x")]);
        assert!(parse(&ok, B).is_some());
        // A text field over 1 KiB; too many parts; no closing delimiter; a
        // preamble; a part without a name or disposition; a header over 1 KiB.
        assert!(parse(&body(B, &[("code", None, &[b'a'; 1025])]), B).is_none());
        assert!(parse(&body(B, &[("code", None, &[b'a'; 1024])]), B).is_some());
        let many: Vec<(&str, Option<&str>, &[u8])> = (0..9).map(|_| ("a", None, &b"x"[..])).collect();
        assert!(parse(&body(B, &many), B).is_none());
        assert!(parse(&ok[..ok.len() - 8], B).is_none());
        assert!(parse(&[b"x\r\n".as_slice(), &ok].concat(), B).is_none());
        let nameless = format!("--{B}\r\nContent-Disposition: form-data\r\n\r\nx\r\n--{B}--\r\n");
        assert!(parse(nameless.as_bytes(), B).is_none());
        let other = format!("--{B}\r\nContent-Type: text/plain\r\n\r\nx\r\n--{B}--\r\n");
        assert!(parse(other.as_bytes(), B).is_none());
        let long = format!("--{B}\r\nContent-Disposition: form-data; name=\"a\"\r\nX-Pad: {}\r\n\r\nx\r\n--{B}--\r\n", "p".repeat(1100));
        assert!(parse(long.as_bytes(), B).is_none());
        // Not UTF-8 in a text field.
        assert!(parse(&body(B, &[("code", None, b"\xff")]), B).is_none());
    }

    #[test]
    fn into_file_keeps_only_the_photo() {
        let b = body(B, &[("csrf", None, b"abc"), ("photo", Some("p.png"), b"\x89PNG-bytes")]);
        let (text, files) = parse(&b, B).unwrap();
        static ONE: Semaphore = Semaphore::const_new(1);
        let up = Upload { form: Form::from_pairs(text), body: b, files, held: Held(ONE.try_acquire().unwrap()) };
        assert_eq!(up.files(), ["photo"]);
        assert_eq!(up.form.get("csrf"), Some("abc"));
        let (photo, held) = up.into_file().unwrap();
        assert_eq!(photo, b"\x89PNG-bytes");
        assert!(ONE.try_acquire().is_err());
        drop(held);
        assert!(ONE.try_acquire().is_ok());
    }
}
