//! Rolling digits on live updates (owner decision 57): the web copy of the
//! iOS app's `AnimatedNumber` (`.contentTransition(.numericText(value:))`,
//! 0.4 s ease-in-out). When a live patch changes a number, its changed
//! digits roll in, up when the value went up and down when it went down;
//! the other digits, the sign and the unit stay where they are.
//!
//! Server side only, no script beyond Datastar:
//! - A page load, a snapshot without JavaScript and a stream's first frame
//!   draw every number as plain text (`Views::roll` is `None`, or the
//!   stream has not drawn that number yet): nothing rolls on a first render.
//! - On a stream, a number whose text changed is drawn twice: the whole
//!   text for screen readers (`.sr`, read once), and an `aria-hidden` copy
//!   in which each changed digit is an `<i>` with an id made of the number's
//!   key, the digit's place and the digit itself, and `data-u` (rolls up) or
//!   `data-d` (rolls down) holding the digit it replaces (CSS shows it
//!   leaving). Datastar's morph keeps an element only for the same id, so a
//!   changed digit is always a new element and its CSS animation runs; an
//!   unchanged digit is plain text and never moves.
//! - Until the number changes again it is drawn the same way (the same
//!   ids and attributes), so a fragment re-sent for another reason, or a
//!   number that did not change, starts no animation and costs no patch.
//! - CSS (`.rl`) clips the column to the line, uses tabular digits and
//!   turns the animation off under `prefers-reduced-motion`.

use maud::{html, Markup};

use super::render::Views;

/// One digit of a drawn number that rolled at its last change: the digit
/// it replaced (`None`: a new place, e.g. 99 -> 100) and whether it rolled up.
type Roll = Option<(Option<u8>, bool)>;

/// How a number was last drawn on this stream.
struct Drawn {
    text: String,
    /// Per digit place (0 = the last digit).
    rolls: Vec<Roll>,
}

/// The numbers one live stream has drawn, by key (`Views::roll`).
#[derive(Default)]
pub struct Rolls {
    nums: Vec<(String, Drawn)>,
}

/// The digits of `text` with their places counted from the last digit.
fn digits(text: &str) -> Vec<u8> {
    text.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').rev().collect()
}

/// The value of a number's text: from its first to its last digit, without
/// group separators, negative after a minus sign. `None` for anything else
/// ("1m 5s").
fn value(text: &str) -> Option<f64> {
    let first = text.find(|c: char| c.is_ascii_digit())?;
    let last = text.rfind(|c: char| c.is_ascii_digit())?;
    let v: f64 = text[first..=last].replace(',', "").parse().ok()?;
    let negative = text[..first].ends_with(['-', '\u{2212}']);
    Some(if negative { -v } else { v })
}

impl Rolls {
    /// How `text` (the number `key`) is drawn now: `None` as plain text.
    fn draw(&mut self, key: &str, text: &str, always_up: bool) -> Option<&[Roll]> {
        let at = self.nums.iter().position(|(k, _)| k == key);
        let new = digits(text);
        if new.is_empty() {
            // No number ("—"): the next one appears without rolling.
            if let Some(i) = at {
                self.nums.swap_remove(i);
            }
            return None;
        }
        let i = match at {
            None => {
                self.nums.push((key.to_owned(), Drawn { text: text.to_owned(), rolls: vec![None; new.len()] }));
                return None;
            }
            Some(i) => i,
        };
        let drawn = &mut self.nums[i].1;
        if drawn.text != text {
            let old = digits(&drawn.text);
            let up = always_up || value(text).zip(value(&drawn.text)).is_none_or(|(n, o)| n >= o);
            drawn.rolls = new
                .iter()
                .enumerate()
                .map(|(p, d)| match old.get(p) {
                    Some(o) if o == d => None,
                    o => Some((o.copied(), up)),
                })
                .collect();
            drawn.text = text.to_owned();
        }
        drawn.rolls.iter().any(Option::is_some).then_some(&drawn.rolls[..])
    }
}

/// A number on a live page (`text`: its formatted text, unit included):
/// plain text, or on a stream after it changed, rolling digits with the
/// text once for screen readers. `key`: lowercase letters, unique on its
/// page (digit ids are made from it).
pub fn roll(v: &Views, key: &str, text: &str) -> Markup {
    draw(v, key, text, false, true)
}

/// `roll` for a number that only counts up (the age badge: the iOS
/// `LastUpdatedView` rolls up), inside an element whose label screen
/// readers already read.
pub fn roll_up_labelled(v: &Views, key: &str, text: &str) -> Markup {
    draw(v, key, text, true, false)
}

fn draw(v: &Views, key: &str, text: &str, always_up: bool, sr: bool) -> Markup {
    debug_assert!(!key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase()), "roll key {key:?}");
    let Some(rolls) = &v.roll else { return html! { (text) } };
    let mut rolls = rolls.borrow_mut();
    let Some(marks) = rolls.draw(key, text, always_up) else { return html! { (text) } };
    let mut place = marks.len();
    let mut run = String::new();
    // Plain text, then the rolling digit that follows it.
    let mut parts: Vec<(String, Option<Digit>)> = Vec::new();
    for c in text.chars() {
        let Some(d) = c.to_digit(10) else {
            run.push(c);
            continue;
        };
        place -= 1;
        match marks[place] {
            None => run.push(c),
            Some((from, up)) => {
                let from = from.map(|f| char::from(b'0' + f).to_string()).unwrap_or_default();
                let digit = Digit { id: format!("{key}{place}{d}"), c, up: up.then(|| from.clone()), down: (!up).then_some(from) };
                parts.push((std::mem::take(&mut run), Some(digit)));
            }
        }
    }
    parts.push((run, None));
    html! {
        @if sr { span.sr { (text) } }
        span.rl aria-hidden="true" {
            @for (plain, digit) in &parts {
                (plain)
                @if let Some(d) = digit { i id=(d.id) data-u=[&d.up] data-d=[&d.down] { (d.c) } }
            }
        }
    }
}

/// A rolling digit: its id, the digit, and the digit it replaces (empty
/// for a new place) as `data-u` (up) or `data-d` (down).
struct Digit {
    id: String,
    c: char,
    up: Option<String>,
    down: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boottime::BootInstant;
    use std::cell::RefCell;
    use std::time::Duration;

    fn live() -> Views {
        let mut v = Views::new(BootInstant::from_duration(Duration::from_secs(1)), Vec::new());
        v.roll = Some(RefCell::new(Rolls::default()));
        v
    }

    #[test]
    fn plain_on_pages_and_on_a_streams_first_frame() {
        let page = Views::new(BootInstant::from_duration(Duration::from_secs(1)), Vec::new());
        assert_eq!(roll(&page, "rx", "416.0 b/s").into_string(), "416.0 b/s");
        let v = live();
        assert_eq!(roll(&v, "rx", "416.0 b/s").into_string(), "416.0 b/s");
        // Unchanged: still plain.
        assert_eq!(roll(&v, "rx", "416.0 b/s").into_string(), "416.0 b/s");
    }

    #[test]
    fn changed_digits_roll_by_direction_and_place() {
        let v = live();
        roll(&v, "rsrp", "-77 dBm");
        // -77 -> -78: lower, so the last digit rolls down from 7.
        let s = roll(&v, "rsrp", "-78 dBm").into_string();
        assert_eq!(
            s,
            "<span class=\"sr\">-78 dBm</span><span class=\"rl\" aria-hidden=\"true\">-7\
             <i id=\"rsrp08\" data-d=\"7\">8</i> dBm</span>"
        );
        // Drawn the same until it changes again: no new patch, no restart.
        assert_eq!(roll(&v, "rsrp", "-78 dBm").into_string(), s);
        // -78 -> -69: higher, both digits roll up.
        let s = roll(&v, "rsrp", "-69 dBm").into_string();
        assert!(s.contains("-<i id=\"rsrp16\" data-u=\"7\">6</i><i id=\"rsrp09\" data-u=\"8\">9</i> dBm"), "{s}");
        // A longer number: the new place has no digit to replace.
        roll(&v, "cpu", "99.5%");
        let s = roll(&v, "cpu", "100.0%").into_string();
        assert!(s.contains("<i id=\"cpu31\" data-u=\"\">1</i><i id=\"cpu20\" data-u=\"9\">0</i><i id=\"cpu10\" data-u=\"9\">0</i>.<i id=\"cpu00\" data-u=\"5\">0</i>%"), "{s}");
        // Only the unit changed: plain text.
        roll(&v, "dl", "1.40 MB");
        assert_eq!(roll(&v, "dl", "1.40 GB").into_string(), "1.40 GB");
        // A dash forgets the number: the next one appears plain.
        assert_eq!(roll(&v, "dl", "\u{2014}").into_string(), "\u{2014}");
        assert_eq!(roll(&v, "dl", "2.00 GB").into_string(), "2.00 GB");
    }

    #[test]
    fn values_and_the_age_badge() {
        assert_eq!(value("-77 dBm"), Some(-77.0));
        assert_eq!(value("1,024 KB"), Some(1024.0));
        assert_eq!(value("+12mA"), Some(12.0));
        assert_eq!(value("1m 5s ago"), None);
        assert_eq!(value("\u{2014}"), None);
        let v = live();
        roll_up_labelled(&v, "age", "9s ago");
        // Back to 0 after a reading: it still rolls up (iOS `numericText()`),
        // and the badge's own label is what screen readers read.
        let s = roll_up_labelled(&v, "age", "0s ago").into_string();
        assert_eq!(s, "<span class=\"rl\" aria-hidden=\"true\"><i id=\"age00\" data-u=\"9\">0</i>s ago</span>");
    }

    #[test]
    fn escaped_and_unique_per_key() {
        let v = live();
        roll(&v, "a", "<1>");
        let s = roll(&v, "a", "<2>").into_string();
        assert!(s.starts_with("<span class=\"sr\">&lt;2&gt;</span>") && s.contains("&lt;<i id=\"a02\" data-u=\"1\">2</i>&gt;"), "{s}");
        // Keys are independent.
        roll(&v, "b", "5");
        assert!(roll(&v, "b", "6").into_string().contains("id=\"b06\""));
        assert!(roll(&v, "a", "<2>").into_string().contains("id=\"a02\""));
    }
}
