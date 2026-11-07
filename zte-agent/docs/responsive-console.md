# Responsive web console

The console at `/console/` is the sole production browser UI. `/` and retired `/ui/` bookmarks redirect to it; known old pages map to the matching hash route without forwarding query strings. Old UI form submissions, asset URLs and live streams return 410 and cannot change settings. Shared security/certificate helpers and archived renderer fixtures remain internally; the legacy rendering module is compiled for tests only.

## Interface

The five main sections are Dashboard, Network, SMS, Tools and Settings. Phones use fixed bottom navigation and horizontally scrollable secondary tabs. Tablet and desktop use expandable sidebar groups. The same navigation element moves to a document-level dock on phones, outside the sticky header, scrolling panels and page animations. Visual-viewport changes keep the dock and dialogs in view when a keyboard reduces or pans the visible area. Short touch-screen landscape layouts retain the dock. Content reserves space for the dock and safe areas.

Light, Dark and System appearance apply across every screen. Page and dialog transitions use opacity and transforms; rapid navigation settles on the latest route. Reduced-motion preferences and hidden tabs skip or finish transitions. Live numbers retain their latest accessible value while changed digits animate. Addresses, identifiers and editable fields stay literal. Forms preserve drafts across routes and SSE updates.

## Authentication and transport

Static assets are under `/console/a/`. The shell contains no device readings. Its Host allowlist and strict CSP apply before serving it; without an agent password, the console directs users to the standalone `/console/setup` page. Device reads, writes and streaming use the existing authenticated JSON API and its Host/Origin checks, login rate limits and blocking-work lanes. The console does not replace the upstream random-token auth implementation, CLOCK_BOOTTIME expiry, LAN admission checks or TLS listener.

The bearer token is kept in this tab’s `sessionStorage`; the password is cleared after sign-in and is never persisted. Navigation and reloads retain the tab session. Explicit lock or expiry clears the token, readings and drafts. A late unauthorized response from an older login cannot lock a newer session. eSIM ICCIDs are masked in text and use opaque DOM keys; full identifiers needed for commands stay in authenticated in-memory state.

## Live data

`GET /api/dashboard/events` is an authenticated axum SSE response. It sends a snapshot followed by changed readings in `{readings: {name: {ok, data, error}}}` envelopes, plus comment heartbeats. Its async response does not hold an API lane or introduce another HTTP listener. Streams are capped at eight, end on expiry/revocation and reconnect after their maximum lifetime. Dropping the last subscription discards saved readings. Collectors sleep without polling or sampling when they have no viewers.

Base readings include device, WAN/WAN6, signal, modem/mobile state, charging, speed, battery, Wi-Fi/guest and remembered clients. `sms=1` opts into SMS data only on message pages. Optional `topics` are `clients`, `routing`, `esim`, `operators`, `forwarding`, `health`, `power`, `https`, `radio`, `speedtest` and `terminal`. Subscribers receive only their selected optional data; the shared snapshot is discarded when its last subscriber leaves. Firmware notifications and successful API mutations accelerate sampling. The existing `/api/events` API remains available; `/ui/` streaming is retired.

WAN IPv4 and IPv6 use the separate firmware interfaces and display all reported addresses; delegated prefixes are not mislabeled as interface addresses. Missing addresses are explicit. Device rates derive from verified Wi-Fi association byte counters using monotonic elapsed time. AP RX is device upload; AP TX is device download. Observed peaks cover continuously monitored associations and reset after counter resets, reassociation, interface changes or monitoring gaps. Link rates, observed rates and configured caps are separate. Firmware without per-device caps reports null/“Not reported”.

The eSIM topic reads saved agent state only; it never opens a card channel or sends receipts. Explicit profile reads and normal page loading use the existing eSIM API. Profile changes have fresh-state checks and readbacks; downloads require an operator preview before installation. Confirmation, cancellation and dismissal include job identity so stale previews cannot operate on another job. Uncertain actions are never automatically retried. A late command response cannot overwrite newer SSE progress.

## Battery & Power

Tools → Battery & Power subscribes to the optional `power` topic (five-second sampling while viewed). `battery_details` combines a read-only allowlist of Linux power-supply measurements with cached firmware battery/charger data and current charge policy. No charging controls are changed by viewing the page. Identifiers and writable sysfs fields are excluded.

Battery and USB power use measured voltage × current with µV/µA conversion, independently of unreliable driver `power_now`. Battery flow and USB input are separate. Kernel time estimates use seconds; the existing firmware estimates use minutes. Nonpositive/sentinel estimates are unavailable, and charging estimates are suppressed outside charging/full state. Raw source fields remain available in a collapsed details section. Missing data is not zero; interrupted streams retain values with a stale caption, and locking or leaving the page clears its optional data.

## Wi-Fi Relay and Clash

Three pages under **Network** drive the optional Rust extensions described in [network-extensions.md](network-extensions.md). None of them polls while idle; Relay re-reads its status every five seconds only while a restore is in progress, and every change asks for confirmation first. A request that gets no reply is shown as uncertain and the status is read again instead of assuming success. All text from the router is rendered with `textContent`.

- **Wi-Fi Relay** (`#network/relay`): scan, pick an upstream, connect and disconnect, explicit clock check, saved networks (forget, per-network bandwidth choice) and the compatibility notes. Passwords go only in the request body.
- **Clash / Mihomo** (`#network/mihomo`): install the pinned core, start and stop, up to 16 configuration profiles (add by subscription URL, file or paste; edit, refresh, select, delete) and the global Merge and Script. A subscription URL is fetched only when the editor is opened; saved YAML is never returned to the browser.
- **Clash proxies** (`#network/proxies`): routing mode, node choice per selector group (rolled back on failure) and delay tests (three at a time, at most 30 nodes per group). The Mihomo controller secret never leaves the router; the agent performs these fixed operations itself.

Clash-specific routes: `POST /api/mihomo/dashboard`, `GET /api/mihomo/extensions`, `GET /api/mihomo/profiles/{id}`, `GET /api/mihomo/proxies`, `POST /api/mihomo/proxies/select`, `POST /api/mihomo/proxies/delay` and `POST /api/mihomo/mode`. Profile and extension reads use the general lane and remain available while Clash is off. Proxy reads and controller changes use the slow lane and return 409 while Clash is off. Dashboard passes use a dedicated authenticated precheck and a general-lane running-state check; they also return 409 while Clash is off. Old `/ui/relay` and `/ui/router/mihomo` bookmarks redirect to these pages.

**Open Zashboard** (Clash / Mihomo and Clash proxies pages, shown only while Clash runs) opens the pinned dashboard in a new tab for connections, logs, traffic, rules and providers. The page opens the tab first, so the click still counts as a user gesture, then trades its bearer token for a short-lived `zte_ui_zashboard` cookie with `POST /api/mihomo/dashboard`. Over HTTP the cookie is scoped to `/ui/mihomo/`; HTTPS translates it to a Secure `__Host-` cookie with `Path=/`. Server-side checks restrict it to the dashboard and bridge, and the JSON API never accepts it. Lock console calls `POST /api/auth/logout` to revoke the session and its passes; if the router cannot confirm logout, the locked tab reports that existing dashboard access may remain. Open streams end when their pass expires, is replaced or evicted, or its session is revoked. If the browser blocks the tab, a link appears instead.

## Source and checks

- `zte-agent/ui/`: dependency-free HTML, CSS and browser modules.
- `src/console.rs`: static shell/assets and Host/CSP handling.
- `src/dashboard.rs`, `dashboard_extra.rs`, `client_details.rs`: demand-driven readings, bounded async SSE and client measurements.
- `ui/tests/`: pure Node validation checks and Playwright browser fixtures. They never contact a router or mutate real settings.

Run pure checks with Node (`actions`, `advanced`, `battery-model`, `cellular`, `clients-model`, `esim-model`, `routing-model`, `relay-model`, `mihomo-model`). Browser suites (`https`, `battery`, `dashboard`, `controls`, `workspaces`, `routing`, `clients`, `esim`, `session`, `numbers`, `navigation`, `relay`, `mihomo`, `proxies`, `network-session`) use Playwright with Chrome; `navigation` also uses WebKit. `PLAYWRIGHT_WEBKIT_EXECUTABLE` can select an installed bundle outside Playwright’s default path. Rust checks use `cargo test -p zte-agent --locked` and the normal aarch64-musl release build.

## Public screenshots

`ui/screenshots/` contains rendered captures of this console using fictional readings, names and profile identifiers. WAN addresses use RFC documentation ranges; device MACs use local demonstration values. No live probe captures, SMS bodies, credentials, activation codes, EIDs or personal device identifiers are included. The existing iOS screenshot gallery remains in `mobile/ios/screenshots/`.

## HTTPS and installed shortcuts

Settings → HTTPS & Install subscribes to `topics=https` for five-second status updates while viewed. The existing TLS service exposes confirmed bearer-authenticated `POST /api/https/on`, `/renew` and `/reset` commands through the slow work lane. Host/Origin admission and login limits remain unchanged. Certificate/profile downloads remain public while HTTPS is enabled; private keys are never served. The new screen includes expiry, router-clock trust, authority fingerprints and platform trust/install instructions.

Both `/console/manifest.webmanifest` and the old `/ui/manifest.webmanifest` keep the installed application id `/ui/`, start at `/console/` and cover the root scope. Public icon aliases serve only generated icons, never legacy scripts or styles. The production router no longer mounts the server-rendered interface. Its archived tests use an explicit test-only harness; production retirement and HTTPS API security are tested through `httpd::app`.
