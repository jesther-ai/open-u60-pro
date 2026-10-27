# Responsive web console

The browser console lives at `/console/` on the same HTTP and optional LAN HTTPS listeners as the existing `/ui/` application. `/` and `/ui/` retain their existing behavior. The console’s About page links to the full server-rendered UI for its additional screens and HTTPS configuration.

## Interface

The five main sections are Dashboard, Network, SMS, Tools and Settings. Phones use fixed bottom navigation and horizontally scrollable secondary tabs. Tablet and desktop use expandable sidebar groups. The same navigation element moves to a document-level dock on phones, outside the sticky header, scrolling panels and page animations. Visual-viewport changes keep the dock and dialogs in view when a keyboard reduces or pans the visible area. Short touch-screen landscape layouts retain the dock. Content reserves space for the dock and safe areas.

Light, Dark and System appearance apply across every screen. Page and dialog transitions use opacity and transforms; rapid navigation settles on the latest route. Reduced-motion preferences and hidden tabs skip or finish transitions. Live numbers retain their latest accessible value while changed digits animate. Addresses, identifiers and editable fields stay literal. Forms preserve drafts across routes and SSE updates.

## Authentication and transport

Static assets are under `/console/a/`. The shell contains no device readings. Its Host allowlist and strict CSP apply before serving it; without an agent password, the console directs users to the existing setup page. Device reads, writes and streaming use the existing authenticated JSON API and its Host/Origin checks, login rate limits and blocking-work lanes. The console does not replace the upstream random-token auth implementation, CLOCK_BOOTTIME expiry, browser-cookie/CSRF protections, LAN admission checks or TLS listener.

The bearer token is kept in this tab’s `sessionStorage`; the password is cleared after sign-in and is never persisted. Navigation and reloads retain the tab session. Explicit lock or expiry clears the token, readings and drafts. A late unauthorized response from an older login cannot lock a newer session. eSIM ICCIDs are masked in text and use opaque DOM keys; full identifiers needed for commands stay in authenticated in-memory state.

## Live data

`GET /api/dashboard/events` is an authenticated axum SSE response. It sends a snapshot followed by changed readings in `{readings: {name: {ok, data, error}}}` envelopes, plus comment heartbeats. Its async response does not hold an API lane or introduce another HTTP listener. Streams are capped at eight, end on expiry/revocation and reconnect after their maximum lifetime. Dropping the last subscription discards saved readings. Collectors sleep without polling or sampling when they have no viewers.

Base readings include device, WAN/WAN6, signal, modem/mobile state, charging, speed, battery, Wi-Fi/guest and remembered clients. `sms=1` opts into SMS data only on message pages. Optional `topics` are `clients`, `routing`, `esim`, `operators`, `forwarding`, `health`, `radio`, `speedtest` and `terminal`. Subscribers receive only their selected optional data; the shared snapshot is discarded when its last subscriber leaves. Firmware notifications and successful API mutations accelerate sampling. Existing `/api/events` and `/ui/` streaming remain available.

WAN IPv4 and IPv6 use the separate firmware interfaces and display all reported addresses; delegated prefixes are not mislabeled as interface addresses. Missing addresses are explicit. Device rates derive from verified Wi-Fi association byte counters using monotonic elapsed time. AP RX is device upload; AP TX is device download. Observed peaks cover continuously monitored associations and reset after counter resets, reassociation, interface changes or monitoring gaps. Link rates, observed rates and configured caps are separate. Firmware without per-device caps reports null/“Not reported”.

The eSIM topic reads saved agent state only; it never opens a card channel or sends receipts. Explicit profile reads and normal page loading use the existing eSIM API. Profile changes have fresh-state checks and readbacks; downloads require an operator preview before installation. Confirmation, cancellation and dismissal include job identity so stale previews cannot operate on another job. Uncertain actions are never automatically retried. A late command response cannot overwrite newer SSE progress.

## Source and checks

- `zte-agent/ui/`: dependency-free HTML, CSS and browser modules.
- `src/console.rs`: static shell/assets and Host/CSP handling.
- `src/dashboard.rs`, `dashboard_extra.rs`, `client_details.rs`: demand-driven readings, bounded async SSE and client measurements.
- `ui/tests/`: pure Node validation checks and Playwright browser fixtures. They never contact a router or mutate real settings.

Run pure checks with Node (`actions`, `advanced`, `cellular`, `clients-model`, `esim-model`, `routing-model`). Browser suites (`dashboard`, `controls`, `workspaces`, `routing`, `clients`, `esim`, `session`, `numbers`, `navigation`) use Playwright with Chrome; `navigation` also uses WebKit. `PLAYWRIGHT_WEBKIT_EXECUTABLE` can select an installed bundle outside Playwright’s default path. Rust checks use `cargo test -p zte-agent --locked` and the normal aarch64-musl release build.

## Public screenshots

`ui/screenshots/` contains rendered captures of this console using fictional readings, names and profile identifiers. WAN addresses use RFC documentation ranges; device MACs use local demonstration values. No live probe captures, SMS bodies, credentials, activation codes, EIDs or personal device identifiers are included. The existing iOS screenshot gallery remains in `mobile/ios/screenshots/`.
