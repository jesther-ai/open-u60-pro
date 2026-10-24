//! SMS tab: Messages (the app's `SMSListView`, drawn in `pages::sms`, which
//! also serves conversations, opening, sending, New Message and deleting)
//! and SMS Forwarding (`pages::forward`), which Tools lists too: its page,
//! its delete question, its Forward Log with the clear question, and their
//! actions exist once per mount.

use crate::events::Topic;
use crate::server::Method;
use crate::web::action::{Action, After, Field, Kind};
use crate::web::forward::{public_log, public_read, CONFIG, LOG_CLEAR, RULES, TOGGLE};
use crate::web::kit::{Icon, Tab};
use crate::web::pages::forward;
use crate::web::pages::sms;
use crate::web::screen::{Bar, Block, Read, Screen, Search};

pub static SMS: Screen = Screen {
    path: sms::LIST,
    id: "sms",
    title: "Messages",
    tab: Tab::Sms,
    bar: Bar::Icons(&[(sms::FORWARD, Icon::Gear, "SMS Forwarding")], &[(sms::NEW, Icon::Compose, "New Message")]),
    topics: &[Topic::Sms],
    live: &sms::FRAGMENTS,
    blocks: &[Block::Custom(sms::list)],
    arg: Some(|v, _| sms::received(v)),
    // Web additions: the app returns to the list without a word.
    saved: &[("sent", "Message sent"), ("deleted", "Conversation deleted")],
    // The app's search bar (the stream only says "New message").
    search: Search::Live,
    ..Screen::BLANK
};

// --- SMS Forwarding ---

/// Read without its secrets (`forward::public`).
const READS: &[Read] = &[Read::Fn(public_read)];

const FORWARD_BASE: Screen = Screen { title: "SMS Forwarding", reads: READS, saved: &forward::SAVED, saved_ok: true, ..Screen::BLANK };

/// Pushed from Messages' gear.
pub static FORWARD: Screen = Screen {
    path: "/ui/sms/forward",
    id: "smsforward",
    tab: Tab::Sms,
    bar: Bar::Back(sms::LIST, "Messages"),
    blocks: &[Block::Custom(|pd| forward::page(pd, &forward::SMS))],
    ..FORWARD_BASE
};

/// Tools › Automation › SMS Forwarding.
pub static TOOLS_FORWARD: Screen = Screen {
    path: "/ui/tools/sms-forward",
    id: "toolsforward",
    tab: Tab::Tools,
    bar: Bar::Back("/ui/tools", "Tools"),
    blocks: &[Block::Custom(|pd| forward::page(pd, &forward::TOOLS))],
    ..FORWARD_BASE
};

/// The delete question reads the rule's name, so it is a screen (404 for a
/// rule that is gone, as Edit Rule); its form posts to the action of the
/// same path.
pub static DELETE: Screen = Screen {
    path: "/ui/sms/forward/rules/delete",
    id: "fwdelete",
    title: "Confirm",
    tab: Tab::Sms,
    bar: Bar::Back("/ui/sms/forward", "SMS Forwarding"),
    reads: READS,
    blocks: &[Block::Custom(|pd| forward::page_delete(pd, &forward::SMS))],
    status: Some(forward::delete_status),
    ..Screen::BLANK
};

pub static TOOLS_DELETE: Screen = Screen {
    path: "/ui/tools/sms-forward/rules/delete",
    id: "toolsfwdelete",
    tab: Tab::Tools,
    bar: Bar::Back("/ui/tools/sms-forward", "SMS Forwarding"),
    blocks: &[Block::Custom(|pd| forward::page_delete(pd, &forward::TOOLS))],
    ..DELETE
};

/// Forward Log (the app's `SMSForwardLogView`): "Clear" trails, as the
/// app's toolbar button (asked first on the web).
pub static LOG: Screen = Screen {
    path: "/ui/sms/forward/log",
    id: "smsfwdlog",
    title: "Forward Log",
    tab: Tab::Sms,
    bar: Bar::BackLink("/ui/sms/forward", "SMS Forwarding", "/ui/sms/forward/log/clear", "Clear"),
    reads: &[Read::Fn(public_log)],
    blocks: &[Block::Custom(|pd| forward::log(pd, &forward::SMS))],
    saved: &forward::LOG_SAVED,
    saved_ok: true,
    ..Screen::BLANK
};

pub static TOOLS_LOG: Screen = Screen {
    path: "/ui/tools/sms-forward/log",
    id: "toolsfwdlog",
    tab: Tab::Tools,
    bar: Bar::BackLink("/ui/tools/sms-forward", "SMS Forwarding", "/ui/tools/sms-forward/log/clear", "Clear"),
    blocks: &[Block::Custom(|pd| forward::log(pd, &forward::TOOLS))],
    ..LOG
};

/// "Clear the forward log?"; its form posts to the action of the same path.
pub static CLEAR: Screen = Screen {
    path: "/ui/sms/forward/log/clear",
    id: "fwclear",
    title: "Confirm",
    tab: Tab::Sms,
    bar: Bar::Back("/ui/sms/forward/log", "Forward Log"),
    blocks: &[Block::Custom(|pd| forward::ask_clear(pd, &forward::SMS))],
    ..Screen::BLANK
};

pub static TOOLS_CLEAR: Screen = Screen {
    path: "/ui/tools/sms-forward/log/clear",
    id: "toolsfwclear",
    tab: Tab::Tools,
    bar: Bar::Back("/ui/tools/sms-forward/log", "Forward Log"),
    blocks: &[Block::Custom(|pd| forward::ask_clear(pd, &forward::TOOLS))],
    ..CLEAR
};

const RULE_ID: Field = Field { name: "id", kind: Kind::Id(u32::MAX) };
const ON_OFF: Field = Field { name: "enabled", kind: Kind::Choice(&["0", "1"]) };

/// "Enabled" applies at once (the app's toggle); the body keeps the
/// config's other values as they are now.
const ENABLED: Action = Action {
    path: "/ui/sms/forward/enabled",
    screen: &FORWARD,
    fields: &[ON_OFF],
    confirm: &[],
    confirm_at: None,
    unchanged: None,
    reads: READS,
    call: (Method::Put, CONFIG),
    body: forward::enabled_body,
    after: After::Back(|_| String::new()),
    again: None,
    once: false,
};

/// "Save Settings": the two after-forward switches.
const SETTINGS: Action = Action {
    path: "/ui/sms/forward/settings",
    fields: &[Field { name: "mark_read", kind: Kind::Check }, Field { name: "delete_after", kind: Kind::Check }],
    body: forward::settings_body,
    after: After::Saved("settings"),
    ..ENABLED
};

/// A rule's switch (`{"id","enabled"}`, as the app).
const TOGGLE_RULE: Action = Action {
    path: "/ui/sms/forward/rules/toggle",
    fields: &[RULE_ID, ON_OFF],
    reads: &[],
    call: (Method::Put, TOGGLE),
    body: forward::toggle_body,
    after: After::Back(forward::at_rule),
    ..ENABLED
};

/// Delete a rule (the app's swipe deletes at once; the web asks first).
const DELETE_RULE: Action = Action {
    path: "/ui/sms/forward/rules/delete",
    fields: &[RULE_ID],
    confirm_at: Some(|v| format!("/ui/sms/forward/rules/delete?id={}", v.int("id"))),
    reads: &[],
    call: (Method::Delete, RULES),
    body: forward::delete_body,
    after: After::Saved("rule-deleted"),
    ..ENABLED
};

/// "Clear" (the app clears at once; the web asks first). Held forwards
/// stay (the agent keeps them).
const CLEAR_LOG: Action = Action {
    path: "/ui/sms/forward/log/clear",
    screen: &LOG,
    fields: &[],
    confirm_at: Some(|_| "/ui/sms/forward/log/clear".to_owned()),
    reads: &[],
    call: (Method::Post, LOG_CLEAR),
    body: |_, _| Some(serde_json::json!({})),
    after: After::Saved("log-cleared"),
    ..ENABLED
};

pub static FW_ENABLED: Action = ENABLED;
pub static FW_SETTINGS: Action = SETTINGS;
pub static FW_TOGGLE: Action = TOGGLE_RULE;
pub static FW_DELETE: Action = DELETE_RULE;
pub static FW_CLEAR: Action = CLEAR_LOG;

pub static TOOLS_FW_ENABLED: Action = Action { path: "/ui/tools/sms-forward/enabled", screen: &TOOLS_FORWARD, ..ENABLED };
pub static TOOLS_FW_SETTINGS: Action = Action { path: "/ui/tools/sms-forward/settings", screen: &TOOLS_FORWARD, ..SETTINGS };
pub static TOOLS_FW_TOGGLE: Action = Action { path: "/ui/tools/sms-forward/rules/toggle", screen: &TOOLS_FORWARD, ..TOGGLE_RULE };
pub static TOOLS_FW_DELETE: Action = Action {
    path: "/ui/tools/sms-forward/rules/delete",
    screen: &TOOLS_FORWARD,
    confirm_at: Some(|v| format!("/ui/tools/sms-forward/rules/delete?id={}", v.int("id"))),
    ..DELETE_RULE
};
pub static TOOLS_FW_CLEAR: Action = Action {
    path: "/ui/tools/sms-forward/log/clear",
    screen: &TOOLS_LOG,
    confirm_at: Some(|_| "/ui/tools/sms-forward/log/clear".to_owned()),
    ..CLEAR_LOG
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Each mount's pages, questions and actions agree on its paths.
    #[test]
    fn mounts_agree() {
        for (m, back, page, delete, actions, (log, clear, clear_action)) in [
            (
                &forward::SMS,
                ("/ui/sms", "Messages"),
                &FORWARD,
                &DELETE,
                [&FW_ENABLED, &FW_SETTINGS, &FW_TOGGLE, &FW_DELETE],
                (&LOG, &CLEAR, &FW_CLEAR),
            ),
            (
                &forward::TOOLS,
                ("/ui/tools", "Tools"),
                &TOOLS_FORWARD,
                &TOOLS_DELETE,
                [&TOOLS_FW_ENABLED, &TOOLS_FW_SETTINGS, &TOOLS_FW_TOGGLE, &TOOLS_FW_DELETE],
                (&TOOLS_LOG, &TOOLS_CLEAR, &TOOLS_FW_CLEAR),
            ),
        ] {
            assert_eq!(log.path, m.log);
            assert_eq!(log.path, format!("{}/log", m.base));
            assert_eq!((log.tab, clear.tab), (m.tab, m.tab));
            let clear_path = format!("{}/clear", m.log);
            assert!(matches!(log.bar, Bar::BackLink(href, "SMS Forwarding", to, "Clear") if href == m.base && to == clear_path));
            assert!(matches!(clear.bar, Bar::Back(href, "Forward Log") if href == m.log));
            assert_eq!((clear.path, clear_action.path), (clear_path.as_str(), clear_path.as_str()));
            assert_eq!(clear_action.screen.path, m.log);
            assert_eq!(clear_action.confirm_at.unwrap()(&crate::web::action::Valid::of(&[])), clear_path);
            assert_eq!(page.path, m.base);
            assert_eq!((page.tab, delete.tab), (m.tab, m.tab));
            assert_eq!(delete.path, format!("{}/rules/delete", m.base));
            assert!(matches!(delete.bar, Bar::Back(href, "SMS Forwarding") if href == m.base));
            assert!(matches!(page.bar, Bar::Back(href, label) if (href, label) == back));
            for (a, rest) in actions.iter().zip(["/enabled", "/settings", "/rules/toggle", "/rules/delete"]) {
                assert_eq!(a.path, format!("{}{rest}", m.base));
                assert_eq!(a.screen.path, m.base);
            }
            let v = crate::web::action::Valid::of(&[("id", "7")]);
            assert_eq!(actions[3].confirm_at.unwrap()(&v), format!("{}/rules/delete?id=7", m.base));
        }
    }
}
