//! Tools › Band Lock (iOS `BandLockView`): "Unlock All Bands", then a grid
//! of the app's 5G NR bands and one of its LTE bands, each with "Apply".
//!
//! - Nothing is read, as in the app (the agent has no read of the current
//!   lock); the choice starts empty, or as it was when a question was
//!   cancelled or a lock applied (`?nr=…`, `?lte=…`: band numbers only).
//! - Each grid is a GET form of its question (`pages::confirm`): the bands
//!   go in the URL, the question's form carries a single-use token, and
//!   the post checks every band against the app's list.
//! - A chip is "B<n>" and its common name, as the app's; the app's
//!   long-press facts (duplex, widest channel, downlink and uplink) are the
//!   chip's `title` and a "Band details" list under each grid.

use maud::{html, Markup};

use crate::web::bands::{lock_facts, LOCK_LTE, LOCK_NR};
use crate::web::kit::{self, Icon};
use crate::web::screen::PageData;

pub const PATH: &str = "/ui/tools/band-lock";
pub const NR: &str = "/ui/tools/band-lock/nr";
pub const LTE: &str = "/ui/tools/band-lock/lte";
pub const UNLOCK: &str = "/ui/tools/band-lock/reset";

/// One technology's grid, its Apply button and its band facts.
fn grid(pd: &PageData, nr: bool, title: &str, action: &str, name: &str, list: &[u16], apply: &str) -> Markup {
    let chosen: Vec<u16> = crate::web::session::Form::parse(pd.query.unwrap_or("").as_bytes())
        .pairs()
        .iter()
        .filter(|(k, _)| k == name)
        .filter_map(|(_, v)| v.parse().ok())
        .collect();
    html! {
        form.bl method="get" action=(action) {
            (kit::section(Some(title), None, html! {
                li.it.fld {
                    fieldset.chips.c5 {
                        legend.sr { (title) }
                        div {
                            @for b in list {
                                @let facts = lock_facts(nr, *b);
                                label title=[facts.as_ref().map(|(n, f)| format!("Band {b} \u{2014} {n} \u{b7} {f}"))] {
                                    input.sr type="checkbox" name=(name) value=(b) checked[chosen.contains(b)];
                                    span { "B" (b) @if let Some((n, _)) = &facts { small { (n) } } }
                                }
                            }
                        }
                    }
                }
                li { button.it.cv.snd type="submit" { (kit::icon(Icon::Lock)) " " (apply) } }
                li.it.fld {
                    details {
                        summary { "Band details" }
                        ul.devs {
                            @for b in list {
                                @if let Some((n, f)) = lock_facts(nr, *b) { li { b { "Band " (b) " \u{2014} " (n) } small { (f) } } }
                            }
                        }
                    }
                }
            }))
        }
    }
}

pub fn page(pd: &PageData) -> Markup {
    html! {
        (kit::section(None, None, html! {
            li { a.it.bt.danger href=(UNLOCK) { (kit::icon(Icon::LockOpen)) span { "Unlock All Bands" } } }
        }))
        (grid(pd, true, "5G NR Bands", NR, "nr", &LOCK_NR, "Apply NR Lock"))
        (grid(pd, false, "LTE Bands", LTE, "lte", &LOCK_LTE, "Apply LTE Lock"))
    }
}
