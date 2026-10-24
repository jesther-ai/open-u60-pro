//! Settings tab: the app's settings page (`SettingsView`), in its order.
//! The gateway and Keychain sections have no web counterpart (the browser
//! is already on the agent; its password manager keeps the password);
//! "Session" with "Log out" takes their place (as the Android app's).
//! "Polling" and "Appearance" are this browser's preferences
//! (`web::prefs`), each a form that sets its cookie; their footers add only
//! "Saved in this browser." to the app's (the setting is per browser).

use maud::{html, Markup};

use crate::web::kit::{self, Tab};
use crate::web::pages::https;
use crate::web::prefs::{self, Theme};
use crate::web::screen::{Bar, Block, PageData, Screen};

pub static SETTINGS: Screen = Screen {
    path: "/ui/settings",
    id: "settings",
    title: "Settings",
    tab: Tab::Settings,
    blocks: &[
        Block::Custom(session),
        Block::Custom(polling),
        Block::Custom(appearance),
        // LAN HTTPS (a web addition: `pages::https`).
        Block::Custom(https::section),
        Block::Rows(
            "About",
            &[
                ("App", "OpenU60"),
                ("Device", "ZTE U60 Pro (MU5250)"),
                ("API", concat!("zte-agent web UI ", env!("CARGO_PKG_VERSION"))),
            ],
        ),
        Block::Custom(legal),
    ],
    ..Screen::BLANK
};

/// Settings › HTTPS › "Install on Your Devices" (`pages::https`).
pub static INSTALL: Screen = Screen {
    path: https::PAGE,
    id: "install",
    title: "Install on Your Devices",
    tab: Tab::Settings,
    bar: Bar::Back("/ui/settings", "Settings"),
    blocks: &[Block::Custom(https::install)],
    ..Screen::BLANK
};

fn session(_: &PageData) -> Markup {
    kit::section(Some("Session"), None, html! {
        li.it { span { "Signed in in this browser" } }
        li { a.it.danger href="/ui/logout" { span { "Log out" } } }
    })
}

const SAVED_HERE: &str = "Saved in this browser.";

/// The app's slider as a list of its values (a range input shows no value
/// without JavaScript) and "Save".
fn polling(pd: &PageData) -> Markup {
    let cur = pd.prefs.refresh_ms;
    let footer = html! {
        p.foot { "Live upload and download use a separate 1-second refresh, matching the modem\u{2019}s update rate." }
        p.foot { (SAVED_HERE) }
    };
    kit::section(Some("Polling"), Some(footer), html! {
        li.it {
            form.sf method="post" action=(prefs::REFRESH) {
                (pd.csrf())
                label for="ms" { "Refresh interval" }
                select #ms name="ms" {
                    @for ms in (1000..=10_000).step_by(500) {
                        option value=(ms) selected[ms == cur] { (prefs::refresh_label(ms)) }
                    }
                }
                button.btn.sm type="submit" { "Save" }
            }
        }
    })
}

/// The app's segmented "Theme": three buttons of one form, the current one
/// pressed.
fn appearance(pd: &PageData) -> Markup {
    let cur = pd.prefs.theme;
    kit::section(Some("Appearance"), Some(html! { p.foot { (SAVED_HERE) } }), html! {
        li.it {
            form.sf method="post" action=(prefs::THEME) {
                (pd.csrf())
                div.seg.th role="group" aria-label="Theme" {
                    @for t in Theme::ALL {
                        button type="submit" name="theme" value=(t.key()) aria-pressed=(if t == cur { "true" } else { "false" }) { (t.label()) }
                    }
                }
            }
        }
    })
}

fn legal(_: &PageData) -> Markup {
    kit::section(Some("Legal"), None, html! {
        li.it {
            p.cap {
                "This app is not affiliated with, endorsed by, or sponsored by ZTE Corporation. \
                 ZTE and U60 Pro are trademarks of ZTE Corporation."
            }
        }
        li { a.it href="https://open-u60-pro.vercel.app/privacy" rel="noopener noreferrer" { span { "Privacy Policy" } } }
    })
}
