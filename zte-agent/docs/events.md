# Live updates: `GET /api/events`

A Server-Sent Events (SSE) stream of JSON for the apps and other integrations, so they can stop polling. On connect it sends the current value of every topic asked for, then only changes. One sampler inside the agent reads values that have no firmware event (signal, traffic, battery, …) once for all open streams, and only while at least one stream wants them. With no stream open nothing is sampled: no timers, no commands.

The web UI's live pages use the same hub through their own cookie-authenticated streams under `/ui/` (see [web-ui.md](web-ui.md)); a page may ask for its own signal cadence as `signal_ms` does (eSIM Profiles: 10 s; otherwise the browser's refresh interval from Settings › Polling, 1–10 s, default 2 s; the fastest stream still wins). This page is about `/api/events`.

## Request

```
GET /api/events?topics=signal,traffic&signal_ms=2000
Authorization: Bearer <token from POST /api/auth/login>
```

- **Auth.** The bearer token, as for every `/api` route; never in the query string. Streams need an agent password (`ZTE_AGENT_PASSWORD`): without one the reply is 403, after the Host check every route gets without a password (so a missing or foreign `Host` gets 400 `missing host` or 403 `host not allowed` first). With a password the Host is checked only for browser requests (those with `Origin` or `Sec-Fetch-Site`; native clients send neither), as for every `/api` route: a DNS-rebinding page can send neither the token nor, at the login route, a password guess.
- **`topics`** (required): comma-separated topic names (below). Spaces around names are ignored, duplicates are merged, `%2C` works as a comma. Only the first `topics` parameter counts.
- **`signal_ms`** (optional): the signal cadence this client wants, 1000-10000 ms. With several streams the fastest one wins. Default 2000.
- Other parameters are ignored.

| Status | Body | When |
|---|---|---|
| 200 | the stream | |
| 400 | `{"ok":false,"error":"topics required"}` | `topics` missing or empty |
| 400 | `{"ok":false,"error":"unknown topic: <name>"}` | a name that is not a topic (shown up to 32 characters) |
| 400 | `{"ok":false,"error":"signal_ms must be 1000-10000"}` | |
| 401 | `{"ok":false,"error":"unauthorized"}` | missing or invalid token |
| 403 | `{"ok":false,"error":"agent password required"}` | the agent has no password |
| 503 + `Retry-After: 10` | `{"ok":false,"error":"too many event streams"}` | 8 streams (including open web UI live pages) are already open, none of them this token's to replace |

## Wire format

`200`, `Content-Type: text/event-stream`, `Cache-Control: no-cache`, chunked. Each frame is a standard SSE event (`event:` and one `data:` line of JSON, then a blank line) or a comment line starting with `:`.

The snapshot comes right after `hello`, in topic order (the order of `hello.topics`). In this example `signal` and `traffic` had not been read yet when the stream opened, so the snapshot holds only `sms`; their first values follow as changes.

```
retry: 3000
event: hello
data: {"v":1,"agent_instance":1729500000000,"topics":["signal","traffic","sms"],"cadence_ms":{"signal":2000,"traffic":1000},"keepalive_ms":15000}

event: sms
data: {"seq":1,"age_ms":5323,"error":null,"data":{"received":0}}

event: signal
data: {"seq":1834,"age_ms":120,"error":null,"data":{…exactly the data of GET /api/network/signal…}}

event: traffic
data: {"seq":1835,"age_ms":40,"error":null,"data":{…exactly the data of GET /api/network/speeds…}}

: keep-alive

event: sms
data: {"seq":1902,"age_ms":0,"error":null,"data":{"received":1}}

event: bye
data: {"reason":"token_expired"}
```

### Frames

| Frame | Content |
|---|---|
| `hello` | Always first. `v` (format version, 1), `agent_instance` (identifies this agent run; compare for equality only), `topics` (as accepted, in a fixed order), `cadence_ms` (sampling period of each sampled topic right now), `keepalive_ms` (15000). Carries `retry: 3000` |
| `<topic>` | `seq`: increases with every change (one counter for all topics; compare per topic). `age_ms`: time since the read behind this frame, on the router's boot clock (not affected by wall-clock jumps). `error`: `null`, or why the last read failed (`data` then keeps the last good value, or is `null` if there never was one; `age_ms` is then the age of the failed read, not of `data`). `data`: the value |
| `: keep-alive` | A comment every 15 s; ignore it. It also lets both sides notice a dead connection |
| `bye` | The last frame; the server then ends the response. `reason` below |

There are no `id:` lines and `Last-Event-ID` is ignored: the stream carries state, not a log, so a reconnect simply starts with a fresh snapshot.

### `bye` reasons

| Reason | Meaning | What to do |
|---|---|---|
| `token_expired` | The token expired (1 h without use, or 24 h after login) | Log in again, then reconnect |
| `logged_out` | The token was revoked (for example pushed out of the 32-token table by newer logins) | Log in again, then reconnect |
| `evicted` | A newer stream with the same token replaced this one: at once if it asks for the same topics (a reconnect), otherwise once it is the token's fourth | Do not reconnect: that would end another stream of this token, whose client would reconnect in turn (a loop) |
| `standby` | The router went into standby or is powering off (`mode_power_off*`) | Reconnect when the user next opens the screen |
| `lifetime` | The stream has been open for 30 minutes | Reconnect at once if the screen is still visible |

## Topics

`data` is exactly the `data` of the GET route named, for the same token, so the apps' existing parsers apply. That includes what those routes already return, such as the full ICCID and IMSI of `GET /api/sim/info` and the ICCIDs in `GET /api/esim/switch`. There is no Wi-Fi topic: Wi-Fi status reads run several commands and carry the Wi-Fi keys.

| Topic | `data` | Updates |
|---|---|---|
| `signal` | `GET /api/network/signal` | Read every 2 s (or `signal_ms`), and 0.5 s after a service-status event (once the cached reading from before it has expired). Every read is sent, even if unchanged (a chart point) |
| `traffic` | `GET /api/network/speeds` (the modem's own rates) | Every 1 s |
| `battery` | `GET /api/battery` | Every 5 s |
| `health` | `{"cpu": {"cores": [...], "overall": n}, "thermal": GET /api/device/thermal}` | Every 2 s (files under `/proc` and `/sys` only, no command). CPU usage is measured over the time since the previous read (at least 250 ms: a read sooner than that, such as a page load while a stream samples, gets the previous figure) |
| `clients` | `GET /api/network/clients` | Every 10 s |
| `charger` | `GET /api/device/charger` | Every 10 s, and after charger events and charge-limit or USB changes |
| `battery_info` | `GET /api/device/battery-info` | Every 30 s, and after charger events |
| `wan` | `{"wan": GET /api/network/wan, "wan6": GET /api/network/wan6}` | After WAN and interface events and SIM identity phases |
| `sim` | `GET /api/sim/info` | After SIM card events and SIM identity phases; after PIN/PUK or SIM lock actions and eSIM profile switches made through the API, every 2 s for 20 s |
| `modem` | `{"status": GET /api/modem/status, "data": GET /api/modem/data}` | After service-status events and SIM identity phases; after WAN and interface events and data or mode changes made through the API, every 2 s for 20 s (the data daemon applies changes late) |
| `sim_identity` | The `zte_agent.sim_identity` payload plus `agent_instance` (see [esim.md, SIM identity event](esim.md#sim-identity-event)) | Each phase. Before the first phase since the agent started: `phase` `idle`, `epoch` from the stamp, and `source`, `plmn`, `operator`, `iccid_tail`, `since_leaving_ms` all `null` |
| `service` | `{"network_type": …}`: `NO_SERVICE`, `LIMITED_SERVICE_LTE`, `LIMITED_SERVICE_SA`, `LTE`, `LTE-NSA`, `ENDC`, …, or `null` if the modem reports none | Each `zwrt_servicestatus` event that carries a `network_type`. Read from the modem (`nwinfo_get_netinfo`, the reading behind `signal`, cached for 0.5 s) when first subscribed, and when a stream subscribes and the value is older than 30 s, which corrects a missed event |
| `power_state` | `{"action": …, "parameter": … or null}` from `zwrt_device_power_state_event`, for example `mode_power_off` with `quicken` (fast boot) or `normal`, `mode_power_off_quicken`, `mode_power_on` | Each event. Absent until the first event since the agent started |
| `sms` | `{"received": n}`: SMS received since the agent started (0 at start) | +1 per received SMS. When it goes up, fetch `POST /api/sms/list` |
| `esim_switch` | `GET /api/esim/switch` | Each change of the switch state, and each SIM identity phase (the stamp changes) |
| `esim_download` | `GET /api/esim/download` | Each change; once more when a finished result is forgotten (10 min), and checked again when a stream subscribes while a finished result is kept |
| `processes` | `GET /api/system/top` | Every 3 s (files under `/proc` only, no command). CPU usage per process is measured by a tracker of the hub's own, so `GET /api/system/top` calls (the apps) do not shorten its window, and the other way round; after more than 15 s without a read it samples twice 250 ms apart, so the first figure is not "since the last caller" |
| `usb` | `GET /api/usb/status` | Every 10 s, after charger and powerbank events (`BSP_CHARGER_EVENT`, `BSP_POWERBANK_EVENT`), and after USB mode or powerbank changes made through the API. Whether attaching a USB-C cable raises a charger event is not verified on the device; the 10 s read covers it either way. Streamed for the dashboard's USB-C card; Tools › USB Mode reads it once at page load |
| `speedtest` | `GET /api/speedtest/progress` | Each change of the router's speed test (a test started by any client): every phase, result and percent step (touched about every 0.4 s while downloading; re-read at most every 0.5 s while watched). In memory only: read when first subscribed, then only after a change. Before any test since the agent started: `phase` `idle` |
| `netscan` | `{"scan": GET /api/modem/scan/status, "register": GET /api/modem/register/result}` | Every 2 s while subscribed (the web UI's scan and registration progress pages; the app polls the same routes every 2 s). A part whose read failed is `null` |
| `signal_detect` | `{"progress": GET /api/cell/signal-detect/progress, "results": GET /api/cell/signal-detect/results}` | Every 2 s while subscribed (the web UI's Signal Detection page while a sweep runs). `results` is `null`, and not read, while the device reports a sweep running below 100 %; a part whose read failed is `null` |
| `stk` | `GET /api/stk/session` (the SIM Toolkit worker's state; without a session, `error` is the worker's last error and `data` the last good value) | Each change of what that route answers: a command from the SIM, a reply, the session ending or failing (the worker tells the hub; a read changes nothing). In memory only: read when first subscribed, then only after a change; no sampling. The read is the snapshot the worker publishes with each change (the same answer as the route, never a request to the worker), so it never starts the worker, opens a toolkit session (`GET /api/stk/menu` does) or waits on the RIL socket, and cannot hold up `hub-fast`. Before any worker: `error` "" (no session) |

- A topic that has never been read is not in the snapshot; its first value follows as soon as it is read (normally within a second).
- `wan`, `sim`, `modem` and `service` are re-read when a stream subscribes if the value is older than 30 s (`charger`, `battery_info` and `usb`: 10 s). When the snapshot showed such an old value (or a sampled value older than two periods), the fresh read is sent even if unchanged, so `age_ms` is right again.
- In `wan`, `modem`, `health`, `netscan` and `signal_detect`, a part whose read failed is `null` and `error` names the first failure.
- A failed read of an event-driven topic is retried while subscribed, after 5 s, doubling to 60 s. A read with one failed part (for example `wan6` on a router without IPv6) is retried until the same failure repeats; after that only the next event re-reads it.
- Values that did not change are not sent again (except `signal`, and the fresh read above). Fast changes coalesce: a slow client gets the latest value, never a backlog.

## Limits

| Limit | Value |
|---|---|
| Streams | 8 open at once (503 + `Retry-After: 10`), shared with the web UI's live pages: each open live page (dashboard, its detail sheets, Processes, Wi-Fi & devices, Messages, an SMS conversation, Tools › Connected Devices, Tools › Speed Test, Router › Signal Detection while a sweep runs, Router › SIM Card, Router › SIM Services, Router › eSIM Profiles, a progress page) holds one. Per token: a stream with the same topics as an open one replaces it at once, and a fourth replaces the oldest (`bye evicted`) |
| Lifetime | 30 minutes, then `bye lifetime` |
| Keep-alive | `: keep-alive` every 15 s |
| Token | An open stream counts as use of its token (the 1 h idle limit slides), but never past 24 h after login |
| Standby | Every stream ends with `bye standby`; new streams are accepted (Wi-Fi may come back during standby, USB stays up) |
| Dead clients | The connection ends when a write fails, after 60 s of unacknowledged data, or by TCP keepalive; its sampling stops with it. Over Tailscale the agent's peer is the local `tailscaled`, which acknowledges everything: a tailnet client that vanished is noticed only when `tailscaled` gives up on it, when the client reconnects (its new stream replaces the old one), or at the 30-minute lifetime |
| Connections | 64 in all, 16 per LAN address. All Tailscale clients arrive as 127.0.0.1: that address has no per-address cap, and the last 8 connections are kept for it |

## Client guidance

- **Use a dedicated HTTP client for the stream**, not the one used for requests: no overall (resource) timeout, and a read/idle timeout of at least 45 s (3 × the keep-alive). The apps' request clients would cut the stream (iOS 10 s request / 15 s resource timeouts, Android `readTimeout` 15 s).
- **Browsers:** `EventSource` cannot send an `Authorization` header, so it cannot open `/api/events`. Use `fetch()` with a stream reader, or a native SSE client. The agent sends no CORS headers, so a page from another origin cannot read the stream; the built-in web UI uses its own `/ui/live/*` streams.
- **After `bye`, do not reconnect automatically,** except after `lifetime`: then reconnect at once if the screen is still visible. On `token_expired` or `logged_out` log in first; on `standby` reconnect when the user next opens the screen; on `evicted` stay disconnected. Treat a reason you do not know like `logged_out`.
- **On a network error** (the connection ended without `bye`), reconnect with the same topics, with a backoff starting at 3 s (`retry: 3000`), doubling up to about a minute: the new stream replaces the old one, which the agent may not have noticed is gone. On 503, wait `Retry-After`. On 401 (after an agent restart, say), log in, then reconnect. On 403 `agent password required`, do not retry.
- **One stream per set of topics per token.** A second stream with the same topics ends the first (`bye evicted`); combine what one screen needs into one stream.
- **On every (re)connect** replace your state with the snapshot. When `hello.agent_instance` differs from the last one seen, the agent restarted: drop everything cached from it (tokens included).
- Parse frames by `event:` name and ignore names and comments you do not know; new topics and fields may be added.
- Close the stream when the screen that needs it is not visible: the router then stops sampling.

Example:

```sh
curl -N -H "Authorization: Bearer $TOKEN" 'http://192.168.0.1:9090/api/events?topics=signal,traffic,sms'
```

## Cost on the router

With no stream open: zero. The sampler's two threads (`hub-fast` for files under `/proc` and `/sys` and in-memory state, `hub-ubus` for reads that run `ubus`) wait without a timeout. Service-status, power, SMS and SIM identity events are noted on the event bus's own thread (`event-bus`), which receives them for other parts of the agent anyway: no extra thread, and at most one wakeup per power-state event (power-button standby or wake). While streams are open, each value is read once for all of them, about 122 commands a minute for a dashboard (traffic, signal, clients, charger, battery info, usb) and about 36 for the signal page, compared with about 190 for the iOS app's polling. Reads that run commands never delay the ones that don't.
