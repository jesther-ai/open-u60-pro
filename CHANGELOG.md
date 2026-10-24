# Changelog

Notable changes to OpenU60 (the `zte-agent` for the ZTE U60 Pro, and the iOS, Android and web apps). Each version has a [GitHub release](https://github.com/jesther-ai/open-u60-pro/releases) with the `zte-agent` binary for the router (aarch64, musl), `THIRD_PARTY_NOTICES.md` and the license texts. `./setup.sh` downloads the binary from the latest release.

## [Unreleased]

### Highlights

- **Web UI in the agent.** Open `http://192.168.0.1:9090/` on the router's Wi-Fi and log in with the agent password. It is a copy of the iOS app with its five tabs: Dashboard, SMS, Tools, Router and Settings (what stays in the apps is listed in the README). It works without JavaScript; with JavaScript the pages update live. It can be added to the home screen on iPhone and Android.
- **Live updates for apps and integrations:** `GET /api/events`, a Server-Sent Events stream. The router samples values only while a stream is open.
- **Safer logins:** random tokens, a login rate limit, and sessions that no longer end when the network changes the router's clock.
- **Install the web UI as an app, over HTTPS on your LAN:** Settings › HTTPS & Install as App turns on `https://192.168.0.1:9443/` (and `https://u60.home.arpa:9443/`) with the router's own certificate authority. Each phone or computer trusts it once (an iPhone profile or a `.crt`, with the steps per platform); it can only vouch for the router's LAN address and `u60.home.arpa`. Offline, no accounts; plain http on port 9090 stays.

### zte-agent

#### Added

- **Web UI** under `/ui/` (`/` redirects there):
  - server-rendered pages, cookie sessions, and a CSRF token in every form;
  - a confirmation page before changes that can cut the connection or are destructive (mobile data off, roaming on, airplane mode on, a network mode, band and cell locks, WiFi, APN, LAN, DNS and firewall changes, deletes); Reboot Router and Factory Reset ask for the agent password;
  - passwords (Wi-Fi, guest Wi-Fi, APN) and SMS forwarding secrets are never put into a page: leave the field empty to keep the current one;
  - a web app manifest and icons for the home screen;
  - live updates with Datastar 1.0.4, served by the agent itself; a number that changes rolls its changed digits up or down like the app's (not with reduced motion, never on a page load);
  - Settings › Appearance also colours the browser's bars and the home-screen app's status bar wherever the browser takes them from the page (Safari, and Chrome in the system's light mode; Chrome for Android keeps its own dark bars in dark mode). Under System, a light/dark switch with a page open also recolours iPhone's status bar, in Safari and in the home-screen app: a small script (`theme.js`, System only, no timer) makes iOS take the colours again; pages work the same without it;
  - Chrome for Android's installed app (and links from other apps) open signed in: login also sets a `SameSite=Lax` marker cookie with no secret (`zte_ui_signed_in=1`), and a page that arrives with the marker but without the `SameSite=Strict` session cookie opens itself once more (a same-site navigation, so the session cookie comes along), never twice and only for its own page;

  See `zte-agent/docs/web-ui.md`.
- **`GET /api/events`** (bearer token; needs an agent password): JSON SSE with 22 topics, a snapshot on connect and then only changes.
  - At most 8 streams in all, shared with open web UI pages, and 3 per token; a stream ends with `bye lifetime` after 30 minutes.
  - Contract: `zte-agent/docs/events.md`.
- **`ZTE_AGENT_UI_HOSTS`:** extra host names the agent answers to (comma-separated), besides IP addresses and, for Tailscale clients, `localhost` and the router's Tailscale names.
- Lockouts are logged: when the global login limit runs dry, and when one address is blocked for a minute or more (with the address, never the password).
- **LAN HTTPS with the router's own certificate authority** (`zte-agent/docs/web-ui.md`, "LAN HTTPS"). Nothing exists until the owner turns it on in Settings (a question with the router's date, single-use token):
  - a root CA and one issuing CA (ECDSA P-256, 10 years; built in DER by the agent and signed with ring and p256, no new crate), both with critical name constraints: the LAN address at Turn On (`/32`, if private) and `u60.home.arpa` permitted, every IPv6 address excluded (and every IPv4 address when none is permitted); pathLen 1 and 0; serverAuth only. The root's key is dropped (and wiped) once it has signed the issuing CA (Android ignores a user-installed root's name constraints but not an intermediate's), so the key the router keeps can vouch for nothing else on any device;
  - a server certificate for the LAN address (`br-lan`) and `u60.home.arpa`, 397 days from two days back (never past the CA), issued again without a restart when the address it should name changes (same dates; at agent start, after a LAN apply, at a TLS connection to a new address; after a LAN address change the name alone, until Reset makes a CA for the new address) or, by the TLS-verified clock only (which needs a Live Activity provider), when fewer than 30 days are left; "Renew Certificate" in Settings otherwise (about every 13 months without one). No timers;
  - files in `/data/zte-agent-tls/` (directory 0700, keys 0600; the root key is never stored); a server pair lost to a power cut or a full disk is issued again from the same CA at start (or at Turn On), so devices keep trusting it; "Reset HTTPS" deletes them, closes the port, ends open HTTPS connections and removes the local name;
  - the listener on `0.0.0.0:9443` (**`ZTE_AGENT_TLS_BIND`**), rustls with ring, TLS 1.2 and 1.3, ALPN `http/1.1`, the same router, mobile-side check and connection caps as :9090 (shared with it); no HSTS;
  - `u60.home.arpa` (**`ZTE_AGENT_TLS_NAME`**, a name under `.home.arpa`) in dnsmasq through `/tmp/hosts/zte-agent` (SIGHUP to the LAN dnsmasq), written again at every start (the temporary file created new, never through a link); answered only over HTTPS;
  - over HTTPS the web UI's cookies are `__Host-zte_ui_*; Path=/; Secure`: the http and https origins of one host never read or overwrite each other's session;
  - downloads `/ui/https/u60-pro-ca.crt` (DER) and `/ui/https/u60-pro-ca.mobileconfig` (iPhone profile), public while HTTPS is on, never cached; Settings shows the CA's SHA-256 (and Windows' SHA-1) fingerprint, where each device shows it and how to check it against a source that does not come over plain http, and the steps for iPhone, Android, Mac, Windows and Firefox; the http login page and Settings point to the https address; a bind that fails is shown with Try Again;
  - **`GET /api/https`** (bearer token): whether it is on and listening, the addresses, the certificate's end, the CA's name, fingerprint and what it can vouch for.
- **The agent refuses connections from the mobile-data side itself**, whatever the router's firewall says (the web UI can turn the firewall off or set it to Low):
  - it serves a connection only from loopback (every Tailscale client), the router's own addresses, or an address in the subnet of a local interface (the Wi-Fi bridge, the USB network: any interface not named `rmnet*`) unless a cellular subnet holds it as specifically or more; any other is closed without a reply. The client's address is what counts, so a port forward or DMZ to the router's address is refused too;
  - refusals are logged: the first, then at most one line a minute with the count (the address and interface, never what was sent);
  - the interface table (`getifaddrs`, read on the blocking pool; a new connection waits at most 0.5 s for it, then the last table decides until the read finishes) is re-read at a connection that needs it when 5 s old, or when a connection would be refused (at most once a second for those); no timer. A failed read keeps the last table for 10 minutes; with none that recent every connection is served and this is logged once (the firewall is then the only guard, and the firewall questions say so);
  - **`ZTE_AGENT_ALLOW_MOBILE=1`** turns it off (logged at startup; another value is ignored and logged).
- **Standby Wi-Fi guard** (on by default). In fast-boot standby the firmware can switch Wi-Fi back on by itself (seen once while charging from a computer: Wi-Fi on, no internet, until the cable was unplugged). When Wi-Fi comes on while the router is in standby, the agent turns it off again the way the firmware's own power-off does (`wireless.zte_mbb.wifi_onoff=0`, then a Wi-Fi reload). The user's Wi-Fi switch (`wifi_onoff_by_user`) is never changed, and a real power-on brings Wi-Fi back as usual.
  - It acts only when the last power event was standby and the newest power mode in `/logfs/key.log` agrees, and not while the firmware is still entering standby (its own power-off turns Wi-Fi off then). An agent started in the first 10 minutes after boot takes the router as on until a standby event. An agent restarted later, with no power event seen yet, goes by key.log, and only if the firmware reported Wi-Fi off before it came on. If key.log shows a power-on whose event was missed, the router is taken as on.
  - At most 3 turn-offs per standby, then it only logs. If the firmware does not report Wi-Fi off within 30 s, it tries once more (counted). Wi-Fi switched on through the app during standby is turned off too.
  - If a turn-off overlaps a real power-on and the firmware has not reported Wi-Fi on 90 s after the power-on, the agent switches Wi-Fi on (`wireless.zte_mbb.wifi_onoff=1`, then a reload) once, if the user's Wi-Fi switch is on.
  - Event-driven: it uses the firmware's power-state events and the Wi-Fi status file the stuck-script guard already watches. It sets a timer only for the 30 s after acting and the 90 s after such a power-on.
  - Each decision is one log line with the time since boot. Turn it off with `export ZTE_AGENT_STANDBY_WIFI_GUARD=0` in the agent's startup script. It is also off, and says so in the log, when inotify is unavailable.

#### Changed

- **Login tokens** are 32 random bytes from the OS (64 hex characters), no longer derived from the password and the login time.
  - They expire after 1 hour without use (each request extends this) and at most 24 hours after login, measured on the boot clock.
  - At most 32 tokens are kept; the least recently used one is dropped.
- **Login rate limit:** after 5 wrong passwords from one address, its logins get `429` with `Retry-After` (1 s, doubling up to 5 min).
  - All Tailscale clients arrive as one address (127.0.0.1); its blocks stop at 10 s, so one phone with an old password cannot lock out every remote user.
  - After 30 wrong passwords from all addresses together, new logins are refused until the limit recovers (one more attempt every 10 s).
  - Tokens and sessions already issued keep working.
- **HTTP server:** tiny_http and its 9 idle threads are replaced by axum/hyper running on the main thread.
  - Device work runs on a small pool that starts threads only when needed.
  - Two new event-hub threads sleep until a live stream is open; the event-bus reader thread is now named `event-bus`.
- **`ZTE_AGENT_THREADS`** now sets how many API calls may work on the device at once (default 4, limited to 1–32). Slow calls (eSIM, Tailscale setup, AT/USSD/STK, scans, speed test) have 2 slots of their own.
- An empty `ZTE_AGENT_PASSWORD` now means no password (logged at startup); before, an empty password was accepted at login.
- **Stuck Wi-Fi script guard is event-driven.** The agent now notices `/sbin/zte_post_wifi.sh` being started (by any firmware path, including the Wi-Fi restart on wake from sleep), checks again when a copy it saw turns 120 s, and stops it if it is still running then, as before. Between Wi-Fi starts it sets no timer. If inotify is unavailable it falls back to the 5-minute check and says so in the log.
  - The launch form of firmware B31 (`zte_post_wifi.sh` with a numeric argument) is recognised.
  - The script's own subshells are no longer counted as separate copies.
  - **A copy that can never finish is stopped after about 6 s instead of 120 s.** That is when `/etc/config/wireless` has a radio off while one of its SSIDs is on (for example 2.4 GHz off), or Wi-Fi switched off altogether (`zte_mbb.wifi_onoff=0`: standby, the app's Wi-Fi switch, the standby guard's turn-off), so the script waits for more access points than can ever start. The agent reads the config again before stopping it, and first lets the script start the monitor for each access point that can run.

#### Behaviour differences apps and scripts may notice

- `GET /` and `HEAD /` answer `303` to `/ui/` (before: `401` or `404` JSON). Other methods on `/` are unchanged.
- A call that waits more than 7 s for a free slot gets `503 {"ok":false,"error":"agent busy, retry"}` with `Retry-After: 1`.
- Request bodies are capped at 64 KiB (login 1 KiB, Live Activity 4 KiB) → `413 {"ok":false,"error":"body too large"}`.
- A body that is cut off or does not arrive within 15 s gets `400 {"ok":false,"error":"incomplete body"}`, and the action is not run.
- At most 64 connections, 16 per LAN address. Tailscale clients (all 127.0.0.1) have no per-address cap, and the last 8 connections are kept for them.
  - A connection over the limit is closed without a reply.
  - Idle keep-alive connections close after 120 s.
- LAN speed test: 2 streams at once; a third gets `503`.
- A browser request from another site that changes something gets `403 {"ok":false,"error":"cross-origin request refused"}`. Browser requests (with `Origin` or `Sec-Fetch-Site`) for a host name the agent does not answer to get `403 host not allowed`, also with a password. The apps send neither header and are not affected.
- Without an agent password, the API only answers requests addressed to an IP address, a `ZTE_AGENT_UI_HOSTS` name, or (from Tailscale clients) `localhost` or a Tailscale name:
  - other names get `403 host not allowed`;
  - a missing `Host` gets `400 missing host`.
- A crash inside a request handler returns `500 {"ok":false,"error":"internal error"}` instead of an empty reply.
- Header names are lowercase, there is no `Server` header, JSON replies carry `X-Content-Type-Options: nosniff`, and replies use `Content-Length` (streams stay chunked).
- Connections from the mobile-data side (an `rmnet*` network, or the internet through a port forward or DMZ to the router) are closed without a reply, also with the router's firewall off. Set `ZTE_AGENT_ALLOW_MOBILE=1` to serve them as before. With `ZTE_AGENT_BIND=[::]:9090`, a LAN client's global IPv6 address outside every local interface's prefix is refused too (IPv4 and link-local work).
- `GET /api/router/lan`: `dhcp_end` is computed from the LAN address and netmask. For a /24 whose range fits the subnet it is unchanged; a range past the subnet ends at its last host (before: an address such as `192.168.0.299`).

#### Fixed

- Logins no longer end when the network sets the router's clock on attach (the 2.2.0 known issue).
- A request declaring a huge `Content-Length` could crash the agent, even without a token.
- `GET /api/router/lan` read `dhcp_end` wrong on networks larger than /24 (it replaced only the last octet: `10.0.0.299` for 10.0.0.1/16 with start 200 and limit 100; now `10.0.1.43`).

#### Removed

- `tiny_http` (and `ascii`, `chunked_transfer`).

#### Licensing

- The binary embeds Datastar 1.0.4 (MIT); its license is in `THIRD_PARTY_NOTICES.md`.
- New crates: axum, axum-core, matchit, mime, tokio-macros, maud, maud_macros, proc-macro2-diagnostics. `THIRD_PARTY_NOTICES.md` now also reproduces the BSD-3-Clause notices of matchit and subtle.

### Mobile apps

- No changes in this release; the apps keep working. See "Behaviour differences" above.
- **For app developers** (follow-ups, not in this release):
  - Handle `429` + `Retry-After` on login as "Too many login attempts, try again in N s", with no automatic retry before then.
  - Android: one shared re-login with a cooldown, instead of one per screen on every `401`, so a phone with an old password cannot use up the global login limit.
  - After a login `401 invalid password`, stop logging in silently until the user enters the password again.
  - Ping `GET /api/lan/ping` instead of `HEAD /` (which now redirects to the web UI). Without a token it answers `401` when a password is set; that still proves the agent is reachable.
  - Treat `503 agent busy, retry` as a transient "Router busy" and retry a GET after `Retry-After`.
  - Adopt `GET /api/events` to replace polling, per events.md "Client guidance".
  - Suggested: an "Open web UI" link in the apps' Settings.

### Known issues

- A few app features stay in the apps (the iOS Live Activity, live-camera QR scanning, calls, LAN Speed Test, Config Decrypt/Encrypt); see the README's "Not in the browser" list.
- Not yet verified on the router: the field names some Router settings send (LAN / DHCP, firewall, port forwarding, DMZ, domain filter; the web sends the app's and the firmware's), and the power-save and auto-sleep bodies.
- Not yet verified on the router: a home-screen web app staying logged in after relaunch, and idle power and memory with the new listener.
- Not yet checked on a real phone: the status bar after a light/dark switch (checked in the iOS 26.5 simulator) and the Android installed app's launch (checked in Chrome with a link from another site).

## [2.2.0] - 2026-10-20

### Highlights

- **eSIM profiles, built in.** With an eSIM card in the SIM slot (tested with a 9eSIM B1), the agent lists, switches, downloads (from the operator's QR code), renames and deletes profiles, and sends the operator receipts. It is its own GSMA SGP.22 LPA: lpac's card library translated to Rust, talking to the card through the modem (QMI over QRTR). Nothing else needs installing, and the apps get new eSIM Profiles screens.
- **SIM identity events.** When the active SIM changes (an eSIM switch, a switch from the card's menu, or a physical swap), the agent publishes `zte_agent.sim_identity` (`leaving` → `changed` → `online`) inside the agent and over `ubus`. The apps, the Live Activity and SMS forwarding refresh themselves. Before a switch, the apps warn about a manual APN or Data Roaming being off.
- **iOS Live Activity.** The router's signal on the Lock Screen, pushed by the agent through Apple's push service. It is off until configured, and keeps working across network and SIM changes.
- **Tailscale remote management**, plus lower power use on the router.

### zte-agent

#### Added

- **eSIM profile management** (`/api/esim/*`, #28–#31):
  - list profiles and card info;
  - switch, verified card → modem identity → data, with a reason when data doesn't come up (for example Data Roaming off), and retries for up to 4 minutes while the card is busy;
  - download from an activation code with a preview, confirm and cancel;
  - delete disabled profiles and rename;
  - operator receipts, sent and retried.

  Operator servers must present TLS certificates from the GSMA eSIM root CI, which is embedded in the agent.
- **SIM identity event** `zte_agent.sim_identity` (#32):
  - phases `leaving`, `changed`, `online` and `stalled`;
  - published in-process and with `ubus send`; the payload carries only the PLMN, the operator name and the last 4 ICCID digits;
  - `sim_epoch`, `sim_phase` and `agent_instance` on `/api/network/signal`, `/api/sim/info` and `/api/esim/switch`.
- **Pre-switch warnings** on `GET /api/esim/profiles` (`manual_apn`, `roaming_off`; read-only), and `observed_seq` for ordering lists without the wall clock (#32).
- **Live Activity sender** (APNs, off unless configured), with recovery after network and time changes. It opens a fresh connection to Apple after every network change (#27, #32).
- **SIM Toolkit** over the RIL socket, default-bearer (QoS) info, and opt-in no-service and connectivity-watchdog policies, off by default (#27).
- **Tailscale remote management** (`/api/tailscale/*`): a supervised `tailscaled` in userspace-networking mode. Enabling it requires an agent password (#13).
- **`/api/device/auto-sleep`** (#25).
- **`zte-agent esim-native <command>`** runs one eSIM card command for diagnostics on the router (#31).

#### Changed

- **Power:** idle wakeups went from 13.2/s to 0.1/s, short-lived caching of `ubus` reads, and fewer processes per app request (#27).
- **SMS forwarding** (#32):
  - an IPv6-only WAN counts as connected;
  - forwards that hit a dead link are held and retried instead of being marked "permanent error";
  - held forwards survive "Clear log".
- **Tailscale:** the WAN-reconnect health check uses a monotonic clock, so the clock jump on attach no longer suppresses it (#32).

#### Removed

- **lpac.** The agent no longer runs lpac and needs no `/data/lpac` bundle; a bundle left by an earlier build can be deleted.
  - The `ZTE_AGENT_LPAC_*` settings are now `ZTE_AGENT_ESIM_QRTR_NODE`, `ZTE_AGENT_ESIM_UIM_SLOT` and `ZTE_AGENT_ESIM_LOCK` (lock file `/tmp/esim.lock`).
  - `GET /api/esim/chip` no longer reports an lpac build (#31).
- **Device identifiers** removed from published files (#27).

#### Licensing

- The binary links the `euicc` crate (LGPL-2.1-only, a Rust translation of lpac's libeuicc). Releases now include `THIRD_PARTY_NOTICES.md`, `LICENSE-LGPL-2.1.txt` and `LICENSE-MIT.txt` (#31).

### Mobile apps (iOS and Android)

- **eSIM:**
  - Router → eSIM Profiles: add (scan or paste a QR code), switch with live progress, rename, delete, receipts and card info. These replace the 9eSIM B1 Bluetooth screens (#28–#30).
  - Switch dialogs show the router's warnings (#32).
- **SIM changes:** dashboard, SIM and device screens refresh by themselves when the SIM changes (#32).
- **Live Activity (iOS):** the "Live signal" card on the dashboard (#27).
- **SMS forwarding:** held forwards show as "Held, will retry" (#32).
- **Dashboard:**
  - cellular traffic from the modem's own counters, which is accurate, and a fix for battery percentages above 100%;
  - 3G/WCDMA signal and auto-sleep controls;
  - hardened networking and session recovery (#25).
- **App icons, signal symbols and Chinese:** app icons, standard signal symbols, and Simplified Chinese translations (#3).
- **Fixes:**
  - turning on Data Roaming on iOS no longer leaves mobile data off;
  - Android's SIM screen used a route that doesn't exist (`/api/sim/lock`) and could crash during a switch;
  - Android keeps 19–20 digit ICCIDs and EIDs exact.

### Web dashboard

- Vercel Analytics (#11).

### Build

- The release workflow cross-compiles the TLS crypto library (`ring`) for the router and test-builds the agent on pull requests that touch it.

### Known issues

- The network sets the router's clock on every attach, often by +8 h, and that ends agent login sessions. The apps log in again by themselves.
- After an eSIM switch, the first operator receipt usually times out and goes out on the next try about 30 s later.

## [2.1.0] - 2026-05-04

- AT terminal, SMS forwarding improvements and UI refinements across all platforms.
- Fix: deleting SIM-stored SMS failed silently.

## [2.0.1] - 2026-04-11

- Fix: `./setup.sh` no longer exits silently when the router is unreachable (#2).

## [2.0.0] - 2026-03-12

- Agent: event bus, SMS forwarding, charge limit, scheduler, speed test, LAN test, process monitor, power-save and fast-boot controls.
- Android app (Jetpack Compose); the apps were renamed to OpenU60; Next.js web dashboard.
- Interactive `./setup.sh` with a pre-built binary download; GitHub Actions release builds.

## [1.0.0] - 2026-03-06

- On-device HTTP agent.

[Unreleased]: https://github.com/jesther-ai/open-u60-pro/compare/v2.2.0...HEAD
[2.2.0]: https://github.com/jesther-ai/open-u60-pro/compare/v2.1.0...v2.2.0
[2.1.0]: https://github.com/jesther-ai/open-u60-pro/compare/v2.0.1...v2.1.0
[2.0.1]: https://github.com/jesther-ai/open-u60-pro/compare/v2.0.0...v2.0.1
[2.0.0]: https://github.com/jesther-ai/open-u60-pro/compare/v1.0.0...v2.0.0
[1.0.0]: https://github.com/jesther-ai/open-u60-pro/releases/tag/v1.0.0
