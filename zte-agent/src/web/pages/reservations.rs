use crate::web::{kit, render::once_field, screen::PageData};
use maud::{html, Markup};

pub const PATH: &str = "/ui/router/reservations";
pub const APPLY: &str = "/ui/router/reservations/apply";

pub fn page(pd: &PageData) -> Markup {
    let Some(data) = pd.read(0) else {
        return html! { p.msg.err { "Cannot read DHCP reservations" } };
    };
    html! {
        p.foot { "Give a U60 hotspot client the same IPv4 address each time, with cellular or Wi-Fi relay. This does not set the upstream router's lease. Keep the phone's private Wi-Fi MAC fixed for this network; reconnect the client to renew its lease. Existing leases are not forcibly disconnected. IPv6 continues to use SLAAC/NAT66." }
        (kit::section(Some("Fixed client IPs"), None, html! {
            @if let Some(entries) = data["entries"].as_array().filter(|e| !e.is_empty()) {
                @for entry in entries {
                    li {
                        div.it.row { span.dn { (entry["name"].as_str().unwrap_or("Client")) } span.v { (entry["ip"].as_str().unwrap_or("")) } }
                        form.form method="post" action=(APPLY) {
                            (pd.csrf()) (once_field(&pd.once()))
                            input type="hidden" name="action" value="delete";
                            input type="hidden" name="id" value=(entry["id"].as_str().unwrap_or(""));
                            div.row { code.mono { (entry["mac"].as_str().unwrap_or("")) } button.btn.btn-quiet.bad type="submit" { "Remove reservation" } }
                        }
                    }
                }
            } @else { li.it { "No agent-managed reservations" } }
        }))
        form.rf method="post" action=(APPLY) autocomplete="off" {
            (pd.csrf()) (once_field(&pd.once())) input type="hidden" name="action" value="add";
            (kit::section(Some("Reserve an address"), Some(html! { p.foot { "Use the client's Wi-Fi MAC from its settings and a free address in U60's LAN. Official-panel reservations are preserved. Saved MAC addresses are shown in full so you can check the binding." } }), html! {
                li.it.fld { label for="fixed-name" { "Client name" } input #fixed-name type="text" name="name" maxlength="64" required; }
                li.it.fld { label for="fixed-mac" { "Wi-Fi MAC" } input #fixed-mac type="text" name="mac" maxlength="17" placeholder="02:00:00:00:00:01" required; }
                li.it.fld { label for="fixed-ip" { "Reserved IPv4" } input #fixed-ip type="text" name="ip" maxlength="15" placeholder="192.168.0.60" required; }
                li { button.it.cv.snd type="submit" { "Save reservation" } }
            }))
        }
        p.actions { a.btn href="/ui/relay" { "Wi-Fi Relay" } a.btn href="/ui/router/lan" { "LAN / DHCP" } }
    }
}
