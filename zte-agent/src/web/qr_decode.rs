//! The QR codes in an uploaded photo (eSIM › Add eSIM: the app's "Choose QR
//! from Photos" and its camera scan; a phone browser's file input offers
//! the camera too), read on the router with bounded work:
//!
//! 1. JPEG or PNG by their magic bytes, never by the declared type;
//! 2. the size from the headers before any pixel is decoded: at most
//!    12,000 px a side and 50 MP (a 48 MP phone photo); a JPEG must be
//!    baseline or progressive DCT (a lossless one has no reduced-scale
//!    decode) with at most 64 scans (each scan walks every block); a
//!    progressive one, whose coefficients are held whole (2 bytes per
//!    component and pixel: up to 6 bytes a pixel), at most 4 MP; a PNG at
//!    most 16 MP, read row by row (an interlaced one whole, within 16 MiB);
//! 3. a first pass at about 1 MP: a JPEG decoded at 1/8, 1/4, 1/2 or full
//!    scale by its own IDCT (so a 12 MP photo never exists at full size), a
//!    PNG box-averaged while it is read; grey (alpha over white); rqrr;
//! 4. when that reads no code and the photo has more detail (at least
//!    twice the pixels, known from the header), one pass at up to 3.2 MP (a
//!    12 MP photo at half scale: a small code in a big photo).
//!
//! The decode runs in a child process (`zte-agent qr-decode --child`, the
//! photo on its stdin, the codes back on its stdout), one at a time: rqrr's
//! work grows steeply with finder-like patterns (a crafted 4 KB image runs
//! for hours) and cannot be interrupted, and a decoder may ask for more
//! memory than the router has (an abort, not a panic). The child lowers its
//! priority and limits itself (CPU seconds; on Linux its address space);
//! the caller kills it at its time limit, which frees the slot. jpeg-decoder
//! starts up to one thread per colour component, inside the child. The
//! bytes are dropped once handed over; nothing is stored or logged, since
//! the payloads are activation codes (the child's stderr is discarded).
//!
//! `zte-agent qr-decode <photo>` times one decode on the router, in its own
//! process and then as the web UI runs it (codes are counted, never printed).

use std::io::{Cursor, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Why no codes could be read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// Neither a JPEG nor a PNG.
    NotImage,
    /// Over the size limits, or bigger than an upload may be.
    TooLarge,
    /// A broken or unsupported image (a CMYK, 12-bit or lossless JPEG, a
    /// decoder error, a crash of the decoding process).
    Unreadable,
    /// Another decode is running, or no decoding process could start.
    Busy,
    /// Still decoding after the caller's limit (the process was stopped).
    TimedOut,
}

/// The largest upload decoded (phone photos are 2–6 MB).
pub const MAX_BYTES: usize = 8 << 20;
/// Payloads kept per photo (enough to tell "one code" from "several").
const MAX_CODES: usize = 4;
/// An activation code is at most 512 bytes; longer payloads are not one.
const MAX_PAYLOAD: usize = 1024;

const MAX_SIDE: u32 = 12_000;
const MAX_PIXELS: u64 = 50_000_000;
const MAX_PROGRESSIVE: u64 = 4_000_000;
const MAX_SCANS: usize = 64;
const MAX_PNG_PIXELS: u64 = 16_000_000;
const PNG_LIMIT: usize = 16 << 20;
/// Pixels of the first and the second pass.
const PASS1: u64 = 1_000_000;
const PASS2: u64 = 3_200_000;

/// The decoding process's own limits: CPU seconds (all its threads; the
/// caller stops it sooner), address space (Linux; a decode peaks near
/// 40 MB) and a lower priority than the agent's.
const CHILD_CPU_SECS: u64 = 30;
#[cfg(target_os = "linux")]
const CHILD_MEMORY: u64 = 128 << 20;
const CHILD_NICE: i32 = 10;
/// The child's answer: `QRD1`, a status byte (0: codes follow; else a
/// `Fail`), the count, then each code as a big-endian u16 length and its
/// bytes.
const MAGIC: &[u8; 4] = b"QRD1";
/// How often the caller looks whether the child has ended.
const POLL: Duration = Duration::from_millis(15);

static DECODING: AtomicBool = AtomicBool::new(false);

/// The QR payloads in `bytes` (decoded codes only, each once, at most 4;
/// none when the photo has none), decoded in a child process stopped after
/// `limit`.
pub fn codes(bytes: Vec<u8>, limit: Duration) -> Result<Vec<String>, Fail> {
    crate::httpd::gate::debug_assert_may_block();
    run(&DECODING, child_command(), bytes, limit)
}

/// One decode at a time: held while the child runs, freed on every way out.
struct Slot(&'static AtomicBool);

impl Slot {
    fn take(slot: &'static AtomicBool) -> Option<Slot> {
        (!slot.swap(true, Ordering::Acquire)).then_some(Slot(slot))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The agent itself as the decoding process (`/proc/self/exe` on Linux:
/// still this binary if the file was replaced since), with no environment.
#[cfg(not(test))]
fn child_command() -> Command {
    let exe = if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe().unwrap_or_else(|_| "zte-agent".into())
    };
    let mut c = Command::new(exe);
    c.args(["qr-decode", "--child"]).env_clear();
    c
}

/// Tests: the test binary, running only `tests::child_process`, which acts
/// as the child when `tests::CHILD_ENV` is set.
#[cfg(test)]
fn child_command() -> Command {
    let mut c = Command::new(std::env::current_exe().expect("the test binary"));
    c.args(["web::qr_decode::tests::child_process", "--exact", "--nocapture", "--test-threads=1", "-q"]).env_clear().env(tests::CHILD_ENV, "1");
    c
}

/// Tests: hold the decoder (the next upload's decode is "Busy").
#[cfg(test)]
pub fn hold() -> Option<impl Drop> {
    Slot::take(&DECODING)
}

fn run(slot: &'static AtomicBool, cmd: Command, bytes: Vec<u8>, limit: Duration) -> Result<Vec<String>, Fail> {
    if bytes.len() > MAX_BYTES {
        return Err(Fail::TooLarge);
    }
    let Some(_held) = Slot::take(slot) else { return Err(Fail::Busy) };
    isolated(cmd, bytes, limit)
}

/// `bytes` through the decoding process `cmd`: written on a thread of its
/// own (a child that stops reading cannot hold the caller), then the
/// child's answer once it ends, or `TimedOut` once `limit` has passed (the
/// child is killed and reaped).
fn isolated(mut cmd: Command, bytes: Vec<u8>, limit: Duration) -> Result<Vec<String>, Fail> {
    let deadline = Instant::now() + limit;
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|_| Fail::Busy)?;
    let stop = |mut child: std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let (Some(mut input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
        stop(child);
        return Err(Fail::Busy);
    };
    // Ends with the write, or on a broken pipe once the child has gone.
    let feed = crate::system::try_spawn_named("qr-feed", move || {
        let _ = input.write_all(&bytes);
    });
    let Ok(feed) = feed else {
        stop(child);
        return Err(Fail::Busy);
    };
    let ended = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => break false,
        }
    };
    if !ended {
        stop(child);
        let _ = feed.join();
        return Err(Fail::TimedOut);
    }
    let _ = feed.join();
    let mut out = Vec::new();
    let _ = output.take(1 << 16).read_to_end(&mut out);
    answer(&out).unwrap_or(Err(Fail::Unreadable))
}

/// The child's answer, as written by `frame`; `None` when there is none
/// (it crashed, or was killed by its limits).
fn answer(out: &[u8]) -> Option<Result<Vec<String>, Fail>> {
    let at = out.windows(MAGIC.len()).position(|w| w == MAGIC)? + MAGIC.len();
    let (&status, rest) = out.get(at..)?.split_first()?;
    let fail = match status {
        0 => None,
        1 => Some(Fail::NotImage),
        2 => Some(Fail::TooLarge),
        _ => Some(Fail::Unreadable),
    };
    if let Some(f) = fail {
        return Some(Err(f));
    }
    let (&n, mut rest) = rest.split_first()?;
    let mut found = Vec::new();
    for _ in 0..usize::from(n).min(MAX_CODES) {
        let len = usize::from(u16::from_be_bytes([*rest.first()?, *rest.get(1)?]));
        let code = rest.get(2..2 + len).filter(|_| len <= MAX_PAYLOAD)?;
        found.push(String::from_utf8(code.to_vec()).ok()?);
        rest = &rest[2 + len..];
    }
    Some(Ok(found))
}

/// `answer`'s input.
fn frame(found: &Result<Vec<String>, Fail>) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    match found {
        Ok(codes) => {
            let codes: Vec<&String> = codes.iter().filter(|c| c.len() <= MAX_PAYLOAD).take(MAX_CODES).collect();
            out.extend([0, codes.len() as u8]);
            for c in codes {
                out.extend_from_slice(&(c.len() as u16).to_be_bytes());
                out.extend_from_slice(c.as_bytes());
            }
        }
        Err(Fail::NotImage) => out.push(1),
        Err(Fail::TooLarge) => out.push(2),
        Err(_) => out.push(3),
    }
    out
}

/// `zte-agent qr-decode --child`: limits itself, reads the photo from
/// stdin, writes `frame` to stdout.
fn child() -> i32 {
    limit_self();
    let mut bytes = Vec::new();
    let found = match std::io::stdin().lock().take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes) {
        Err(_) => Err(Fail::Unreadable),
        Ok(n) if n > MAX_BYTES => Err(Fail::TooLarge),
        Ok(_) => scan(&bytes),
    };
    drop(bytes);
    let mut out = std::io::stdout().lock();
    i32::from(out.write_all(&frame(&found)).and_then(|()| out.flush()).is_err())
}

/// The child's limits (best effort: a platform without one runs without it).
fn limit_self() {
    let cap = |secs: u64| libc::rlimit { rlim_cur: secs as libc::rlim_t, rlim_max: secs as libc::rlim_t };
    // SAFETY: plain system calls on this process, with valid arguments.
    unsafe {
        libc::setrlimit(libc::RLIMIT_CPU, &cap(CHILD_CPU_SECS));
        #[cfg(target_os = "linux")]
        libc::setrlimit(libc::RLIMIT_AS, &cap(CHILD_MEMORY));
        libc::setpriority(libc::PRIO_PROCESS, 0, CHILD_NICE);
    }
}

/// `zte-agent qr-decode <photo>`: one decode in this process, timed, with
/// the process's peak memory (`VmHWM`), then one as the web UI runs it (a
/// child process); the codes are counted, never shown. `--child` is the
/// web UI's decoding process.
pub fn cli(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("usage: zte-agent qr-decode <photo.jpg|png>");
        return 2;
    };
    if path == "--child" {
        return child();
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{path}: {e}");
            return 1;
        }
    };
    let (len, hwm) = (bytes.len(), peak_kib());
    let count = |found: &Result<Vec<String>, Fail>, ms: u128| match found {
        Ok(c) => {
            let lpa = c.iter().filter(|s| s.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("LPA:"))).count();
            format!("{} code(s), {lpa} eSIM activation code(s), {ms} ms", c.len())
        }
        Err(e) => format!("{e:?} after {ms} ms"),
    };
    let started = Instant::now();
    let found = if len > MAX_BYTES { Err(Fail::TooLarge) } else { scan(&bytes) };
    println!("{len} bytes: {}", count(&found, started.elapsed().as_millis()));
    if let (Some(before), Some(after)) = (hwm, peak_kib()) {
        println!("peak memory {after} KiB (at start {before} KiB)");
    }
    let started = Instant::now();
    let isolated = codes(bytes, Duration::from_secs(120));
    println!("as the web UI decodes it (child process): {}", count(&isolated, started.elapsed().as_millis()));
    i32::from(found.is_err() || isolated.is_err())
}

/// The process's peak resident memory, Linux only.
fn peak_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// The whole pipeline, blocking, in this process (no slot, no limit).
pub fn scan(b: &[u8]) -> Result<Vec<String>, Fail> {
    let png = if b.starts_with(&[0xff, 0xd8, 0xff]) {
        false
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        true
    } else {
        return Err(Fail::NotImage);
    };
    let decode = |budget| if png { png_grey(b, budget) } else { jpeg_grey(b, budget) };
    let (first, (w, h)) = decode(PASS1)?;
    let first_px = (first.w * first.h) as u64;
    let found = read(first);
    // A finer pass only if the photo has at least twice the pixels (from
    // its size: nothing is decoded for it otherwise).
    if !found.is_empty() || pass_pixels(w, h, PASS2, png) < 2 * first_px {
        return Ok(found);
    }
    let (second, _) = decode(PASS2)?;
    Ok(read(second))
}

/// The IDCT scale (1, 2, 4 or 8) of a JPEG pass of `budget` pixels.
fn idct_scale(w: u32, h: u32, budget: u64) -> u32 {
    [1, 2, 4, 8].into_iter().find(|f| u64::from(w.div_ceil(*f)) * u64::from(h.div_ceil(*f)) <= budget).unwrap_or(8)
}

/// The pixels a pass of `budget` reads of a `w` x `h` image.
fn pass_pixels(w: u32, h: u32, budget: u64, png: bool) -> u64 {
    let f = if png { 1 } else { idct_scale(w, h, budget) };
    let (w, h) = (w.div_ceil(f) as usize, h.div_ceil(f) as usize);
    let k = shrink_factor(w, h, budget);
    (w.div_ceil(k) * h.div_ceil(k)) as u64
}

/// A grey image, row by row.
struct Grey {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

/// Box-averages rows of grey samples by an integer factor, as they arrive.
struct Shrink {
    f: usize,
    w: usize,
    h: usize,
    acc: Vec<u32>,
    out: Vec<u8>,
}

impl Shrink {
    /// To at most `budget` pixels.
    fn new(w: usize, h: usize, budget: u64) -> Shrink {
        let f = shrink_factor(w, h, budget);
        let (ow, oh) = (w.div_ceil(f), h.div_ceil(f));
        Shrink { f, w, h, acc: vec![0; ow], out: Vec::with_capacity(ow * oh) }
    }

    /// Row `y`: its samples of `n` bytes each.
    fn row(&mut self, y: usize, line: &[u8], n: usize) {
        for (x, p) in line.chunks_exact(n).take(self.w).enumerate() {
            self.acc[x / self.f] += u32::from(grey(p));
        }
        if y % self.f == self.f - 1 || y + 1 == self.h {
            let rows = (y % self.f + 1) as u32;
            for (i, a) in self.acc.iter_mut().enumerate() {
                let cols = (self.w - i * self.f).min(self.f) as u32;
                self.out.push((*a / (rows * cols)) as u8);
                *a = 0;
            }
        }
    }

    fn done(self) -> Grey {
        let w = self.acc.len();
        Grey { w, h: self.out.len() / w, px: self.out }
    }
}

/// The box size that brings `w` x `h` to at most `budget` pixels.
fn shrink_factor(w: usize, h: usize, budget: u64) -> usize {
    (1..).find(|f| (w.div_ceil(*f) * h.div_ceil(*f)) as u64 <= budget).unwrap_or(1)
}

/// One 8-bit sample of 1 to 4 channels as grey (BT.601 luma; alpha over
/// white, so a code on a transparent background reads).
fn grey(p: &[u8]) -> u8 {
    let (l, a) = match *p {
        [l] => (u32::from(l), 255),
        [l, a] => (u32::from(l), u32::from(a)),
        [r, g, b, ..] => ((u32::from(r) * 77 + u32::from(g) * 150 + u32::from(b) * 29) >> 8, p.get(3).map_or(255, |a| u32::from(*a))),
        [] => (255, 255),
    };
    ((l * a + 255 * (255 - a)) / 255) as u8
}

fn check(w: u32, h: u32) -> Result<(), Fail> {
    if w == 0 || h == 0 {
        Err(Fail::Unreadable)
    } else if w > MAX_SIDE || h > MAX_SIDE || u64::from(w) * u64::from(h) > MAX_PIXELS {
        Err(Fail::TooLarge)
    } else {
        Ok(())
    }
}

/// The scans (SOS markers) before the image's end, walking the segments
/// and the entropy-coded data between them.
fn scan_count(b: &[u8]) -> usize {
    let (mut i, mut n) = (2, 0);
    while i + 1 < b.len() {
        if b[i] != 0xff {
            i += 1;
            continue;
        }
        match b[i + 1] {
            // A fill byte.
            0xff => i += 1,
            // Stuffing, TEM, restart markers, SOI: no length.
            0x00 | 0x01 | 0xd0..=0xd8 => i += 2,
            0xd9 => break,
            m => {
                n += usize::from(m == 0xda);
                let Some(len) = b.get(i + 2..i + 4) else { break };
                i += 2 + usize::from(u16::from_be_bytes([len[0], len[1]])).max(2);
            }
        }
    }
    n
}

/// A JPEG decoded at the IDCT scale (1/8 … 1) that fits `budget`, as grey;
/// and its full size.
fn jpeg_grey(b: &[u8], budget: u64) -> Result<(Grey, (u32, u32)), Fail> {
    use jpeg_decoder::{CodingProcess, Decoder, PixelFormat};
    let mut d = Decoder::new(Cursor::new(b));
    d.read_info().map_err(|_| Fail::Unreadable)?;
    let info = d.info().ok_or(Fail::Unreadable)?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    check(w, h)?;
    // Lossless JPEGs have no reduced-scale decode (full-size buffers before
    // any limit is checked): refused, as anything but 8-bit grey or RGB.
    let progressive = match info.coding_process {
        CodingProcess::DctSequential => false,
        CodingProcess::DctProgressive => true,
        CodingProcess::Lossless => return Err(Fail::Unreadable),
    };
    if progressive && u64::from(w) * u64::from(h) > MAX_PROGRESSIVE {
        return Err(Fail::TooLarge);
    }
    let n = match info.pixel_format {
        PixelFormat::L8 => 1,
        PixelFormat::RGB24 => 3,
        _ => return Err(Fail::Unreadable),
    };
    if scan_count(b) > MAX_SCANS {
        return Err(Fail::Unreadable);
    }
    let f = idct_scale(w, h, budget);
    let (sw, sh) = d.scale(w.div_ceil(f) as u16, h.div_ceil(f) as u16).map_err(|_| Fail::Unreadable)?;
    let (sw, sh) = (usize::from(sw), usize::from(sh));
    d.set_max_decoding_buffer_size(sw * sh * n);
    let px = d.decode().map_err(|_| Fail::Unreadable)?;
    drop(d);
    if px.len() < sw * sh * n {
        return Err(Fail::Unreadable);
    }
    let mut shrink = Shrink::new(sw, sh, budget);
    for (y, line) in px.chunks_exact(sw * n).enumerate() {
        shrink.row(y, line, n);
    }
    Ok((shrink.done(), (w, h)))
}

/// A PNG box-averaged to `budget` while it is read, as grey; and its size.
fn png_grey(b: &[u8], budget: u64) -> Result<(Grey, (u32, u32)), Fail> {
    use png::{ColorType, Transformations};
    let mut d = png::Decoder::new_with_limits(Cursor::new(b), png::Limits { bytes: PNG_LIMIT });
    d.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    d.set_ignore_text_chunk(true);
    d.set_ignore_iccp_chunk(true);
    let mut r = d.read_info().map_err(|_| Fail::Unreadable)?;
    let (w, h) = r.info().size();
    check(w, h)?;
    if u64::from(w) * u64::from(h) > MAX_PNG_PIXELS {
        return Err(Fail::TooLarge);
    }
    let n = match r.output_color_type().0 {
        ColorType::Grayscale => 1,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        // EXPAND turns palettes into RGB(A).
        ColorType::Indexed => return Err(Fail::Unreadable),
    };
    let size = (w, h);
    let (w, h) = (w as usize, h as usize);
    let mut shrink = Shrink::new(w, h, budget);
    if r.info().interlaced {
        // Adam7 rows come pass by pass: the whole frame first.
        let len = r.output_buffer_size().filter(|s| *s <= PNG_LIMIT).ok_or(Fail::TooLarge)?;
        let mut frame = vec![0; len];
        r.next_frame(&mut frame).map_err(|_| Fail::Unreadable)?;
        for (y, line) in frame.chunks_exact(w * n).take(h).enumerate() {
            shrink.row(y, line, n);
        }
    } else {
        for y in 0..h {
            let row = r.next_row().map_err(|_| Fail::Unreadable)?.ok_or(Fail::Unreadable)?;
            shrink.row(y, row.data(), n);
        }
    }
    Ok((shrink.done(), size))
}

/// The codes rqrr decodes in `g` (each once, at most `MAX_CODES`).
fn read(g: Grey) -> Vec<String> {
    let Grey { w, h, px } = g;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| px[y * w + x]);
    drop(px);
    let mut out = Vec::new();
    for grid in img.detect_grids() {
        if let Ok((_, s)) = grid.decode() {
            if s.len() <= MAX_PAYLOAD && !out.contains(&s) && out.len() < MAX_CODES {
                out.push(s);
            }
        }
    }
    out
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Obviously fake activation codes (the fixture rule).
    pub const CODE: &str = "LPA:1$example.com$FAKE-MATCHING-ID";
    pub const OTHER: &str = "LPA:1$smdp.example.com$FAKE-0000-0000-MATCHING-ID";

    /// A grey "photo": a lit card with the code `text` (module size `m`
    /// px, turned by `deg`) at the centre `(cx, cy)` (fractions) on a
    /// textured background, mild noise; a second code (`OTHER`) beside it
    /// with `two`.
    pub fn photo(w: usize, h: usize, text: &str, m: f64, deg: f64, two: bool) -> Vec<u8> {
        use qrcodegen::{QrCode, QrCodeEcc};
        let codes: Vec<QrCode> = [text, OTHER].iter().map(|t| QrCode::encode_text(t, QrCodeEcc::Medium).unwrap()).collect();
        let (sin, cos) = deg.to_radians().sin_cos();
        let side = |q: &QrCode| f64::from(q.size() + 8) * m;
        let centres: Vec<(f64, f64)> = if two {
            let s = side(&codes[0]).max(side(&codes[1]));
            vec![(w as f64 / 2.0 - s * 0.7, h as f64 / 2.0), (w as f64 / 2.0 + s * 0.7, h as f64 / 2.0)]
        } else {
            vec![(w as f64 / 2.0, h as f64 / 2.0)]
        };
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut px = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f64, y as f64);
                let on_card = centres.iter().zip(&codes).find_map(|(&(cx, cy), q)| {
                    let s = side(q);
                    let u = (fx - cx) * cos + (fy - cy) * sin + s / 2.0;
                    let v = -(fx - cx) * sin + (fy - cy) * cos + s / 2.0;
                    let inside = (-s * 0.1..s * 1.1).contains(&u) && (-s * 0.1..s * 1.1).contains(&v);
                    inside.then(|| {
                        let (mx, my) = ((u / m).floor() as i32 - 4, (v / m).floor() as i32 - 4);
                        if q.get_module(mx, my) { 30 } else { (230.0 - 30.0 * (u + v) / (2.0 * s)).clamp(150.0, 230.0) as i32 }
                    })
                });
                let base = on_card.unwrap_or_else(|| (90.0 + 30.0 * (fx * 0.013 + fy * 0.002).sin()) as i32);
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                px[y * w + x] = (base + (seed >> 59) as i32 - 16).clamp(0, 255) as u8;
            }
        }
        px
    }

    pub fn jpeg(grey: &[u8], w: usize, h: usize, rgb: bool, progressive: bool) -> Vec<u8> {
        use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
        let mut out = Vec::new();
        let mut e = Encoder::new(&mut out, 88);
        e.set_progressive(progressive);
        if rgb {
            e.set_sampling_factor(SamplingFactor::F_2_2);
            let px: Vec<u8> = grey.iter().flat_map(|&g| [g.saturating_add(6), g, g.saturating_sub(8)]).collect();
            e.encode(&px, w as u16, h as u16, ColorType::Rgb).unwrap();
        } else {
            e.encode(grey, w as u16, h as u16, ColorType::Luma).unwrap();
        }
        out
    }

    pub fn png(grey: &[u8], w: usize, h: usize, rgba: bool) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = ::png::Encoder::new(&mut out, w as u32, h as u32);
            e.set_color(if rgba { ::png::ColorType::Rgba } else { ::png::ColorType::Grayscale });
            e.set_depth(::png::BitDepth::Eight);
            let mut wr = e.write_header().unwrap();
            if rgba {
                // Dark modules opaque, the rest transparent: reads as on white.
                let px: Vec<u8> = grey.iter().flat_map(|&g| if g < 80 { [0, 0, 0, 255] } else { [0, 0, 0, 0] }).collect();
                wr.write_image_data(&px).unwrap();
            } else {
                wr.write_image_data(grey).unwrap();
            }
        }
        out
    }

    #[test]
    fn reads_codes_in_photos() {
        // A phone-sized photo, turned: read in the first pass (1/4 scale).
        let (w, h) = (2016, 1512);
        let img = jpeg(&photo(w, h, CODE, 14.0, 23.0, false), w, h, true, false);
        assert_eq!(scan(&img), Ok(vec![CODE.to_owned()]));
        // A small code: only the finer pass reads it.
        let small = jpeg(&photo(w, h, CODE, 3.0, 0.0, false), w, h, true, false);
        assert_eq!(scan(&small), Ok(vec![CODE.to_owned()]));
        // Two codes: both, each once.
        let two = jpeg(&photo(w, h, CODE, 9.0, 0.0, true), w, h, false, false);
        let mut found = scan(&two).unwrap();
        found.sort();
        assert_eq!(found, [CODE, OTHER]);
        // None.
        let blank = jpeg(&vec![128; 640 * 480], 640, 480, false, false);
        assert_eq!(scan(&blank), Ok(vec![]));
        // PNG: grey, and a transparent screenshot of a code.
        let (w, h) = (700, 900);
        let g = photo(w, h, CODE, 9.0, 0.0, false);
        assert_eq!(scan(&png(&g, w, h, false)), Ok(vec![CODE.to_owned()]));
        assert_eq!(scan(&png(&g, w, h, true)), Ok(vec![CODE.to_owned()]));
        // A small progressive JPEG is read.
        let p = jpeg(&photo(800, 600, CODE, 8.0, 10.0, false), 800, 600, true, true);
        assert_eq!(scan(&p), Ok(vec![CODE.to_owned()]));
    }

    #[test]
    fn refuses_what_it_should_not_decode() {
        assert_eq!(scan(b"GIF89a....."), Err(Fail::NotImage));
        assert_eq!(scan(b""), Err(Fail::NotImage));
        assert_eq!(scan(&[0xff, 0xd8, 0xff, 0xe0, 0, 0]), Err(Fail::Unreadable));
        assert_eq!(scan(b"\x89PNG\r\n\x1a\n\0\0"), Err(Fail::Unreadable));
        // Sizes come from the headers, before any pixel: a declared
        // 12,001 px side or 60 MP is refused without decoding.
        let mut big = jpeg(&vec![200; 64 * 64], 64, 64, false, false);
        let sof = big.windows(2).position(|p| p == [0xff, 0xc0]).unwrap();
        big[sof + 5..sof + 9].copy_from_slice(&[0x2e, 0xe1, 0x10, 0x00]); // 12001 x 4096
        assert_eq!(scan(&big), Err(Fail::TooLarge));
        big[sof + 5..sof + 9].copy_from_slice(&[0x1f, 0x40, 0x1f, 0x40]); // 8000 x 8000
        assert_eq!(scan(&big), Err(Fail::TooLarge));
        // A progressive JPEG over 4 MP.
        let mut prog = jpeg(&vec![200; 64 * 64], 64, 64, false, true);
        let sof = prog.windows(2).position(|p| p == [0xff, 0xc2]).unwrap();
        prog[sof + 5..sof + 9].copy_from_slice(&[0x08, 0x00, 0x08, 0x00]); // 2048 x 2048
        assert_eq!(scan(&prog), Err(Fail::TooLarge));
        // A truncated JPEG body is an error, not a panic.
        let ok = jpeg(&photo(320, 240, CODE, 4.0, 0.0, false), 320, 240, true, false);
        assert!(matches!(scan(&ok[..ok.len() / 2]), Err(Fail::Unreadable) | Ok(_)));
        // A PNG over 16 MP (by its header).
        let mut p = png(&[255; 16], 4, 4, false);
        p[16..24].copy_from_slice(&[0, 0, 0x11, 0x94, 0, 0, 0x0f, 0xa0]); // 4500 x 4000
        let crc = crc32fast::hash(&p[12..29]).to_be_bytes();
        p[29..33].copy_from_slice(&crc);
        assert_eq!(scan(&p), Err(Fail::TooLarge));
    }

    #[test]
    fn grey_and_shrink() {
        assert_eq!((grey(&[0]), grey(&[255, 0]), grey(&[0, 0, 0, 255]), grey(&[0, 0, 0, 0])), (0, 255, 0, 255));
        assert_eq!(grey(&[255, 255, 255]), 255);
        // 5 x 3 to at most 4 pixels: factor 3, 2 x 1, partial blocks averaged.
        let mut s = Shrink::new(5, 3, 4);
        for y in 0..3 {
            s.row(y, &[30, 30, 30, 90, 90], 1);
        }
        let g = s.done();
        assert_eq!((g.w, g.h, g.px), (2, 1, vec![30, 90]));
    }

    /// The decoding process of `child_command` in tests: this test, run
    /// alone by the test binary with `CHILD_ENV` set (else it does nothing).
    pub const CHILD_ENV: &str = "ZTE_QR_DECODE_CHILD";

    #[test]
    fn child_process() {
        if std::env::var_os(CHILD_ENV).is_some() {
            std::process::exit(child());
        }
    }

    /// Finder-like squares (`m` px modules) tiled edge to edge: rqrr tries
    /// every pair of them, so its work grows steeply with the image's side.
    pub fn finder_grid(side: usize, m: usize) -> Vec<u8> {
        let tile = 10 * m;
        (0..side * side)
            .map(|i| {
                let (x, y) = ((i % side) % tile / m, (i / side) % tile / m);
                let ring = (x as i32 - 3).abs().max((y as i32 - 3).abs());
                if x >= 7 || y >= 7 || ring == 2 { 255 } else { 0 }
            })
            .collect()
    }

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    /// Decodes run in a child process, one at a time: the answer comes back
    /// over its stdout; a child that runs over its limit is killed (the slot
    /// is free at once), one that crashes or answers nothing is
    /// "Unreadable", one that cannot start leaves the router "Busy".
    #[test]
    fn decodes_in_a_child_process_with_a_limit() {
        static SLOT: AtomicBool = AtomicBool::new(false);
        let long = Duration::from_secs(60);
        let img = jpeg(&photo(640, 480, CODE, 6.0, 0.0, false), 640, 480, false, false);
        assert_eq!(run(&SLOT, child_command(), img.clone(), long), Ok(vec![CODE.to_owned()]));
        assert_eq!(run(&SLOT, child_command(), b"GIF89a....".to_vec(), long), Err(Fail::NotImage));
        assert_eq!(run(&SLOT, child_command(), img[..img.len() / 3].to_vec(), long), Err(Fail::Unreadable));
        // A crafted image rqrr would read for hours: stopped at the limit.
        let grid = png(&finder_grid(700, 2), 700, 700, false);
        let started = Instant::now();
        assert_eq!(run(&SLOT, child_command(), grid, Duration::from_millis(1500)), Err(Fail::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(!SLOT.load(Ordering::Acquire));
        assert_eq!(run(&SLOT, child_command(), img.clone(), long), Ok(vec![CODE.to_owned()]));
        // A child that reads, then hangs; one that never reads (the photo's
        // writer must not hold the caller); one that dies; none at all.
        let started = Instant::now();
        assert_eq!(run(&SLOT, sh("cat >/dev/null; sleep 30"), vec![0; 1 << 20], Duration::from_millis(300)), Err(Fail::TimedOut));
        assert_eq!(run(&SLOT, sh("sleep 30"), vec![0; 4 << 20], Duration::from_millis(300)), Err(Fail::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert_eq!(run(&SLOT, sh("cat >/dev/null; kill -9 $$"), img.clone(), long), Err(Fail::Unreadable));
        assert_eq!(run(&SLOT, sh("cat >/dev/null; printf 'QRD1\\000\\002\\000\\001'"), img.clone(), long), Err(Fail::Unreadable));
        assert_eq!(run(&SLOT, Command::new("/nonexistent/zte-agent"), img.clone(), long), Err(Fail::Busy));
        // One at a time; oversized uploads are not handed over.
        let held = Slot::take(&SLOT).unwrap();
        assert_eq!(run(&SLOT, child_command(), img.clone(), long), Err(Fail::Busy));
        drop(held);
        assert_eq!(run(&SLOT, child_command(), vec![0; MAX_BYTES + 1], long), Err(Fail::TooLarge));
        assert!(!SLOT.load(Ordering::Acquire));
    }

    /// The child's answer: codes or the reason, read past anything before
    /// it; a cut or missing answer is none.
    #[test]
    fn answers_round_trip() {
        let codes = Ok(vec![CODE.to_owned(), "WIFI:S:x;;".to_owned()]);
        assert_eq!(answer(&frame(&codes)), Some(codes.clone()));
        let mut noisy = b"\nrunning 1 test\n".to_vec();
        noisy.extend(frame(&codes));
        assert_eq!(answer(&noisy), Some(codes));
        for f in [Fail::NotImage, Fail::TooLarge, Fail::Unreadable] {
            assert_eq!(answer(&frame(&Err(f))), Some(Err(f)));
        }
        assert_eq!(answer(&frame(&Ok(vec![]))), Some(Ok(vec![])));
        let whole = frame(&Ok(vec![CODE.to_owned()]));
        assert_eq!(answer(&whole[..whole.len() - 1]), None);
        assert_eq!(answer(b""), None);
        assert_eq!(answer(b"QRD"), None);
    }

    /// A PNG written by hand: Adam7-interlaced 8-bit grey, or only its
    /// header for an RGB image of `w` x `h`.
    fn png_interlaced(grey: Option<&[u8]>, w: usize, h: usize) -> Vec<u8> {
        let mut raw = Vec::new();
        if let Some(g) = grey {
            for (x0, y0, dx, dy) in [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)] {
                if x0 >= w || y0 >= h {
                    continue;
                }
                for y in (y0..h).step_by(dy) {
                    raw.push(0);
                    raw.extend((x0..w).step_by(dx).map(|x| g[y * w + x]));
                }
            }
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(&raw).unwrap();
        let idat = z.finish().unwrap();
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let crc = crc32fast::hash(&out[start..]);
            out.extend_from_slice(&crc.to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(w as u32).to_be_bytes());
        ihdr.extend_from_slice(&(h as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, if grey.is_some() { 0 } else { 2 }, 0, 0, 1]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &idat);
        chunk(b"IEND", &[]);
        out
    }

    /// Hostile and unusual images, each answered quickly: a lossless JPEG
    /// (no reduced-scale decode), a JPEG with thousands of scans, CMYK, an
    /// interlaced PNG too big to hold; palette, 16-bit and interlaced PNGs
    /// with a code are read.
    #[test]
    fn hostile_and_unusual_images() {
        let started = Instant::now();
        // Lossless (SOF3) declaring 6000 x 6000: refused after the header.
        let mut lossless = jpeg(&vec![200; 64 * 64], 64, 64, false, false);
        let sof = lossless.windows(2).position(|p| p == [0xff, 0xc0]).unwrap();
        lossless[sof + 1] = 0xc3;
        lossless[sof + 5..sof + 9].copy_from_slice(&[0x17, 0x70, 0x17, 0x70]);
        let mut d = jpeg_decoder::Decoder::new(Cursor::new(&lossless));
        d.read_info().unwrap();
        assert_eq!(d.info().unwrap().coding_process, jpeg_decoder::CodingProcess::Lossless);
        assert_eq!(scan(&lossless), Err(Fail::Unreadable));
        // Its one scan repeated 2,000 times (a few KB): refused unread.
        let base = jpeg(&vec![200; 64 * 64], 64, 64, false, false);
        let sos = base.windows(2).position(|p| p == [0xff, 0xda]).unwrap();
        let mut many = base[..sos].to_vec();
        for _ in 0..2000 {
            many.extend_from_slice(&base[sos..base.len() - 2]);
        }
        many.extend_from_slice(&[0xff, 0xd9]);
        assert_eq!((scan_count(&base), scan_count(&many)), (1, 2000));
        assert_eq!(scan(&many), Err(Fail::Unreadable));
        let prog = jpeg(&photo(320, 240, CODE, 4.0, 0.0, false), 320, 240, true, true);
        assert!((2..=MAX_SCANS).contains(&scan_count(&prog)), "{}", scan_count(&prog));
        // CMYK.
        let mut cmyk = Vec::new();
        jpeg_encoder::Encoder::new(&mut cmyk, 80).encode(&[0; 32 * 32 * 4], 32, 32, jpeg_encoder::ColorType::Cmyk).unwrap();
        assert_eq!(scan(&cmyk), Err(Fail::Unreadable));
        // Interlaced: 3000 x 3000 RGB is 27 MB whole: refused by its header.
        assert_eq!(scan(&png_interlaced(None, 3000, 3000)), Err(Fail::TooLarge));
        let (w, h) = (500, 400);
        let g = photo(w, h, CODE, 6.0, 0.0, false);
        assert_eq!(scan(&png_interlaced(Some(&g), w, h)), Ok(vec![CODE.to_owned()]));
        // Palette and 16-bit grey.
        let mut pal = Vec::new();
        {
            let mut e = ::png::Encoder::new(&mut pal, w as u32, h as u32);
            e.set_color(::png::ColorType::Indexed);
            e.set_depth(::png::BitDepth::Eight);
            e.set_palette(vec![0, 0, 0, 255, 255, 255]);
            let idx: Vec<u8> = g.iter().map(|&v| u8::from(v >= 80)).collect();
            e.write_header().unwrap().write_image_data(&idx).unwrap();
        }
        assert_eq!(scan(&pal), Ok(vec![CODE.to_owned()]));
        let mut deep = Vec::new();
        {
            let mut e = ::png::Encoder::new(&mut deep, w as u32, h as u32);
            e.set_color(::png::ColorType::Grayscale);
            e.set_depth(::png::BitDepth::Sixteen);
            let px: Vec<u8> = g.iter().flat_map(|&v| [v, v]).collect();
            e.write_header().unwrap().write_image_data(&px).unwrap();
        }
        assert_eq!(scan(&deep), Ok(vec![CODE.to_owned()]));
        assert!(started.elapsed() < Duration::from_secs(30), "{:?}", started.elapsed());
    }

    /// The second pass is decoded only when it has at least twice the
    /// pixels, known from the header.
    #[test]
    fn the_finer_pass_is_planned_from_the_size() {
        // 12 MP: 1/4 then 1/2 scale.
        assert_eq!((pass_pixels(4032, 3024, PASS1, false), pass_pixels(4032, 3024, PASS2, false)), (762_048, 3_048_192));
        // 1 MP: the same both times (skipped).
        assert_eq!(pass_pixels(1000, 1000, PASS1, false), pass_pixels(1000, 1000, PASS2, false));
        // A PNG screenshot: 1179 x 2556 box-averaged 2x, then whole.
        assert_eq!((pass_pixels(1179, 2556, PASS1, true), pass_pixels(1179, 2556, PASS2, true)), (590 * 1278, 1179 * 2556));
    }
}
