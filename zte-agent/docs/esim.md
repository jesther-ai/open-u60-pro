# eSIM profiles

The agent manages the profiles on an eSIM card in the router's SIM slot (for example a 9eSIM B1): list them, switch which one is enabled, download new ones from an activation code (the operator's QR code), rename and delete them. It is its own GSMA SGP.22 LPA, in-process: card commands go over the modem's QMI UIM service (QMI over QRTR), downloads and receipts use HTTPS from the router's own connection. Nothing needs installing besides the agent. The SIM Toolkit menu is not used.

| Part | Where | License |
|---|---|---|
| Card commands, notifications, card info, profile download (ES10b/c, ES8+ metadata, ES9+) | [`euicc/`](../../euicc/), a Rust translation of [lpac](https://github.com/estkme-group/lpac)'s libeuicc | LGPL-2.1-only |
| QMI UIM over QRTR (open/close logical channel, send APDU; no libqmi) | `zte-agent/src/qrtr_uim.rs`, based on lpac's MIT QRTR driver | MIT |
| Card commands and receipts, in lpac's command and result format | `zte-agent/src/esim_native.rs` | MIT |
| Profile download procedure (SGP.22) | `zte-agent/src/esim_download.rs` | MIT |

| Variable | Default | Meaning |
|---|---|---|
| `ZTE_AGENT_ESIM_QRTR_NODE` | `3` | QRTR node of the modem's UIM service (`qrtr-lookup`, service 11) |
| `ZTE_AGENT_ESIM_UIM_SLOT` | `1` | SIM slot |
| `ZTE_AGENT_ESIM_LOCK` | `/tmp/esim.lock` | Lock file shared with `zte-agent esim-native` runs |

Until 2026-10-19 the agent ran lpac itself, a separately built bundle in `/data/lpac`. The in-process code replaced it once it matched lpac's results (below). The bundle's build recipe, `lpac-qrtr-build/`, is in the git history.

## How it works

- **Same results as lpac.** Commands, data and errors keep lpac's names and shape, so the views and the apps didn't change when lpac went. Error reasons use the SGP.22 result names (`catBusy`, `profileNotInDisabledState`, …). On the same scripted card and server, a download sends byte-for-byte the same card commands and HTTPS requests as lpac.
- **Download cancel reasons.** lpac sent endUserRejection for every stop; the agent uses SGP.22's reasons:

  | Event | Reason sent |
  |---|---|
  | Preview declined | endUserRejection |
  | Preview not answered in 2 minutes | timeout |
  | Cancel | postponed (the operator keeps the QR code usable) |
  | Network or HTTP failure | timeout |
  | Server-reported failure | only the card session is cancelled (the server has already ended) |
  | Install interrupted | loadBppExecutionError |

  A failure the card reports while installing needs no cancel: its installation result goes to the operator as a receipt.
- **Cancel timing.** A Cancel takes effect at the next step; an ES9+ request already running ends within its timeout (30 s, 120 s for the profile package). Once installing starts, Cancel is refused (409).
- **TLS.** SM-DP+ servers present TLS certificates issued by the GSMA eSIM root CI ("GSM Association - RSP2 Root CI1"), which no web CA list contains. The agent embeds that root (`zte-agent/certs/`) and trusts nothing else; it checks the host name, follows no redirects and uses no proxy. All servers the tested card uses pass. (Upstream lpac doesn't check these certificates at all.)
- **Time limits.** Each card command runs on a worker thread with a hard limit (30 s; 45 s for one receipt; 300 s for a download); nothing else touches the card until it has finished (the shared lock stays held).
- **Diagnostics.** `zte-agent esim-native <command>` (`chip info`, `profile list`, `notification list`, …) runs one command and prints its result as one JSON line in lpac's format. It takes the shared lock without waiting, so it answers busy while the agent holds the card.
- **Verified:** about 1 million generated cases comparing the Rust card code with the C library; live `chip info`, `profile list`, `notification list` identical to lpac; switches with receipts; a nickname write; refusals named correctly; a download refused by the server (unknown matching ID, 4 s, card session cancelled) and a Cancel while connecting to an unreachable server (took effect when the 15 s connect limit ran out). No profile or receipt changed in either. Re-adding an installed profile from its used QR code: the server sent the preview and, after Install, the profile package; the card refused it at `store_metadata` with `install_failed_due_to_iccid_already_exists_on_euicc` (15 s, preview included). The failure receipt was delivered, the installed copy was untouched and free memory was unchanged. Delete and re-add of the same (used) Klook profile with the agent `native-5` (no lpac): the delete took effect and its receipt was delivered 2 s later; the same QR code then downloaded again (preview 4 s after the start, install 15 s, 24 s in all), the install receipt was delivered, and free memory returned to the value after the first install (934475 bytes). The nickname did not survive, as it is stored with the profile.

## API

All routes use the agent's bearer authentication and `{ok,data}` / `{ok:false,error}` envelopes. The agent runs one card command at a time (30 s limit for card commands, 300 s for a download); a request made while another is running gets 409. Enable, delete, download, rename and sending receipts require an agent password (403 otherwise). `GET /api/esim/profiles` also returns `pending_receipts`.

| Method | Path | Result |
|---|---|---|
| GET | `/api/esim/profiles` | `profiles`: `iccid`, `isdp_aid`, `state` (`enabled`/`disabled`), `nickname`, `provider`, `name`, `class`, and on profiles that can be switched to `switch_warnings` (see Before switching); plus `enabled_iccid`, `observed_at`, `observed_seq`, `generation`, `agent_instance`, `updating`, `stale`, `error` (see Reading during a switch), `manual_apn` |
| GET | `/api/esim/chip` | `eid`, `firmware`, `sgp22_version`, `profile_version`, `free_nvm_bytes`, `free_ram_bytes`, `card_changed` |
| POST | `/api/esim/enable` | Body `{"iccid": "..."}`. Returns `enabled_iccid`, `previous_iccid`, `reconnecting: true`, `generation`. If the card command fails or times out the reply carries `outcome: "unknown"`: the switch may still have happened, see `/api/esim/switch`. If the card refuses because it is busy, the reply is 202 with `retrying: true` and the agent keeps trying (state `card_busy`) |
| GET | `/api/esim/switch` | Last switch: `state`, `target_iccid`, `previous_iccid`, `generation`, `started_at`, `card_confirmed_at`, `modem_confirmed_at`, `connected_at`, `finished_at`, `note`, `reason` (for `no_data`: `roaming_off` or `data_off`, else null); `sim_epoch`, `sim_phase`, `agent_instance` (see SIM identity event) |
| POST | `/api/esim/delete` | Body `{"iccid": "..."}`. Disabled profiles only. Returns `deleted_iccid`, `operator_notified` |
| POST | `/api/esim/nickname` | Body `{"iccid": "...", "nickname": "..."}` (at most 64 bytes, no control characters; empty clears it). Returns `iccid`, `nickname`, `generation` |
| GET | `/api/esim/notifications` | Operator receipts waiting on the card: `count`, `pending` (`seq`, `operation`, `address`, `iccid`, `attempts`, `last_error`, `gave_up`), `listed_at`, `last_attempt_at`, `last_sent_at`, `retrying`, `stale` |
| POST | `/api/esim/notifications/process` | Sends the waiting receipts now in the background (202) and restarts the retry loop, including receipts that gave up |
| POST | `/api/esim/qr` | Raw JPEG or PNG body (maximum 8 MiB). Reads one eSIM activation QR and returns `activation_code`, `confirmation_code_required`. Requires an agent password; the image is decoded in a bounded child process, then discarded without being stored or logged |
| POST | `/api/esim/download` | Body `{"activation_code": "LPA:1$...", "confirmation_code": "...", "preview": true}`. Starts a background download; 202 with the status below |
| GET | `/api/esim/download` | Status: `state` (`idle`, `running`, `confirm`, `cancelling`, `done`, `failed`, `cancelled`), `step` (last progress step, with lpac's names), `provider`, `name`, `iccid`, `error`, `started_at`, `finished_at` |
| POST | `/api/esim/download/confirm` | Body `{"accept": true}` or `false`, while `state` is `confirm` |
| POST | `/api/esim/download/cancel` | Cancels a running download |
| DELETE | `/api/esim/download` | Dismisses a finished result (status back to `idle`); 409 while a download runs |

### Download

The activation code must have the SGP.22 form `LPA:1$<server>$<matching ID>[$<OID>[$1]]`; a final `1` means the operator also issued a confirmation code, which the request must then include. The agent runs the download in a background thread. With preview on (the default), it stops once the operator's server has sent the profile metadata: the status moves to `confirm` with `provider` and `name` filled in, and the download waits for the answer from `/download/confirm`. An unanswered preview is cancelled after 2 minutes. Cancel stops the download at its next step (see Cancel timing above). A downloaded profile is disabled until it is enabled. A finished result (`done`, `failed`, `cancelled`) is reported for 10 minutes, then the status reads `idle`; the result is also lost when the agent restarts.

The responsive console can upload an operator QR image before starting the download. It accepts JPEG or PNG only, caps the body at 8 MiB and sends the image only to the authenticated U60. The async listener receives the bounded body without taking a worker permit; decoding runs on the slow lane in the existing low-priority child process with CPU, memory and 15-second limits. Exactly one valid eSIM activation payload must be present. The photo is dropped after decoding, and neither it nor the decoded activation code is written to disk or logs.

After a successful download or delete the agent sends the operator receipts (see [Operator receipts](#operator-receipts)). Activation codes are never logged.

### Safety checks

- **Card identity.** Before enable, delete, download and rename the agent reads the card's EID. If it differs from the card the saved list belongs to (the card was swapped), the list is dropped, `generation` goes up and the request gets 409 "a different eSIM card is in the slot".
- **Radio.** `enable` is refused (409) while the modem is in airplane / low-power mode (`zte_nwinfo.sys_info.operate_mode` not `ONLINE`), unless the no-service policy parked it: switching to another network is then the way out, and the firmware's SIM reload brings the radio back.
- **Download.** Refused (409) when the router has no mobile data connection; the start reply includes `free_nvm_bytes`.
- **Shared lock.** Every card command holds the agent's own flag and a non-blocking `flock` on `/tmp/esim.lock` (`ZTE_AGENT_ESIM_LOCK`). `zte-agent esim-native` takes the same lock, so the two never overlap; while a manual run holds it the agent answers "busy".

### Operator receipts

After installing, enabling, disabling or deleting a profile the card keeps an SGP.22 notification for the operator's server until it is delivered. The agent sends each receipt on its own (45 s limit), so one failure doesn't hold back the others. The first successful profile read after the agent starts lists them. After a download or delete the agent sends them. After a switch it sends them 5 s after the data connection is confirmed and tries the failed ones once more 30 s later (right after the attach a request can stall); a switch that ended without data only lists them. A server that fails or hangs is skipped for the rest of that round, so one unreachable operator holds the card for at most 45 s per round. Unsent receipts are retried every 10 minutes while the router has mobile data (at most 24 rounds). A receipt that failed 10 times is reported with `gave_up: true` and left on the card; it is never deleted unsent. `POST /api/esim/notifications/process` resets the attempt counts and tries again.

### Reading during a switch

For several seconds after `enable` (3–8 s in most switches, about 25 s in one test) the modem is reloading the SIM and the eUICC's management application cannot be selected: opening the channel (`euicc_init`) fails with QMI error 80 (`SimFileNotFound`). The app saw this as "Could not read eSIM profiles" in 3 of 14 switches. The agent therefore:

- keeps the last good profile list. While a switch reloads the SIM, `GET /api/esim/profiles` answers from it with `updating: true` and does not touch the card. It does the same while another eSIM operation holds the card (instead of 409). A failed read returns the last good list with `stale: true` and `error: {kind, message, at}`. `kind` is `sim_reloading` (within 5 minutes of a switch), `management_unavailable`, `read_failed`, `busy` (the card is still held by an earlier command) or `not_configured` (an invalid `ZTE_AGENT_ESIM_*` setting). A successful read clears the error;
- re-reads the card itself after the switch: from 5 s, every 3 s once `get_sim_info` reports the SIM ready, for up to 60 s;
- refuses enable, delete and download while the SIM reloads (409);
- numbers each change of the card's contents or selection (`generation`) and each agent run (`agent_instance`, start time in ms), so clients can drop answers older than what they have shown.

`GET /api/esim/switch` reports the verified outcome. `state` starts at `card_busy` when the card refused the enable because it was busy (see below), then moves `reloading` → `card_confirmed` (fresh card read shows the target enabled) → `modem_confirmed` (`get_sim_info` `sim_iccid` equals the target) and ends as:

- `connected` (`get_wwaniface` `connect_status` connected);
- `no_data` (card and modem switched, no data within 150 s). `reason` says why when the firmware shows it: `roaming_off` when the SIM's country code differs from the network's (a travel eSIM) and data roaming is off, or `data_off` when mobile data is switched off;
- `modem_unconfirmed` (the modem kept the old identity);
- `card_unconfirmed` (the card stayed unreadable for 60 s);
- `not_applied` (the card still has another profile enabled; `reason: card_busy` when it kept refusing).

**Busy card.** The card can refuse an enable with SGP.22 `catBusy` (5: its SIM Toolkit is in a proactive session), `undefinedError` (127), or a code without a name; the errors read `catBusy`, `undefinedError` and `unknown result N`. Nothing changes on the card. The agent answers 202, reports `card_busy` and retries every 10 s for up to 240 s, then verifies the switch as usual. A second switch request in that time gets 409.

Apart from that, the agent never retries a profile switch or switches back by itself. It now checks the new CID’s data path before reporting `connected`; the firmware’s connected string alone is insufficient. See "Data handover recovery" below.

### Switching

`enable` only accepts a profile that exists, is disabled and has class `operational`. It sends EnableProfile with the refresh flag set, which makes the card tell the modem to reload the SIM. The router's mobile data then drops while the modem attaches with the new profile (20–45 s in testing); Wi-Fi and the agent API stay up. The firmware picks the APN from its ICCID list. For five minutes after a switch the no-service policy and the connection watchdog treat the router as busy and do not act.

ICCIDs and the EID are digit strings longer than 64-bit integers can hold: clients must keep them as strings.

### Before switching

`GET /api/esim/profiles` warns about what a switch would run into, read-only (the agent never changes these settings). While a switch reloads the SIM (`updating: true`) the reply carries neither `switch_warnings` nor `manual_apn`.

- `manual_apn: {name, apn}` when the router is in manual APN mode, and `manual_apn` in `switch_warnings` of each disabled operational profile: the new profile would dial that APN too (seen live: a travel profile's `e-ideas`, then `gomo.ph`, carried over to the next profile).
- `roaming_off` in `switch_warnings` when data roaming is off and the profile looks like one from another country. This is a heuristic: it compares the country code in the ICCIDs (`89` + E.164 code; US issuers' `8901…` counts as 1 and Canadian `89302…` as 302) with the loaded profile, and whether that profile is at home (its MCC equals the network's). Without a registered network there is no `roaming_off`. Next to a roaming profile, only profiles from the same country are flagged.

`observed_seq` counts good reads of the card. Clients order lists of the same `generation` by it rather than by `observed_at`, because the router's clock jumps by hours when the network sets the time after an attach.

### SIM identity event

The agent tracks the loaded SIM's identity by its ICCID, whether it changes through the agent's own switch, the card's menu or a physical swap. The same SIM reloading (a low-power cycle, a refresh that keeps the profile) is not a change. Each phase is published inside the agent and with `ubus send zte_agent.sim_identity '<payload>'` for other processes; no firmware daemon listens to it, and the agent sends no firmware events itself.

| Phase | When |
|---|---|
| `leaving` | The firmware's `switch_slot_begin` or `sim hot remove`, or the agent's own switch; carries the outgoing identity |
| `changed` | A ready SIM with a different ICCID; `epoch` goes up by one |
| `online` | After the change, a default route, DNS, a WAN-up event if the ICCID changed, no WAN script running and 8 s without WAN events (`settled: true`), or a default route for 30 s (`settled: false`). `identity_changed: false` when the same SIM came back |
| `stalled` | No `online` 150 s after `leaving`; `reason`: `sim_not_ready`, `no_wan`, `roaming_off` or `data_off`. A later `online` can still follow |

Payload: `phase`, `epoch`, `source` (`agent`, `card`, `hotswap` or `unknown`), `generation` (the switch's, for agent switches), `plmn` (`515-02`), `operator`, `iccid_tail` (last 4 digits), `since_leaving_ms`, and `identity_changed`/`settled` or `reason`. It never carries the full ICCID, the IMSI, the phone number, an IP address or the APN.

On each phase the agent drops its cached SIM and WAN values, then shows the phase on `GET /api/network/signal`, `/api/sim/info` and `/api/esim/switch` as `sim_epoch` (changes counted since the agent started), `sim_phase` (`idle` until the first phase) and `agent_instance`. The apps refresh their SIM and network screens when that triple changes. The Live Activity pauses on `leaving` and checks the connection again on `online`. Idle, the tracker uses no timers and runs no commands.

## Tested

Card details, setup and limits for the tested card: [docs/9esim-b1.md](../../docs/9esim-b1.md). The tests below up to agent `esim-15` ran lpac; the in-process results are under [How it works](#how-it-works).

U60 Pro (B27), 9eSIM B1 (eUICC firmware 4.2.0, SGP.22 2.2.2), three operator profiles, 2026-10-18:

- `chip info` and `profile list` left the connection untouched (about 0.3 s and 30 ms of CPU each; lpac peaks at about 10 MB RAM and exits).
- Switching Smart → GOMO took 25 s to internet and GOMO → DITO 43 s, each with one firmware radio restart and no reboot.
- With the reload handling (2026-10-18 21:40): Smart → DITO, list read at 3.5 s answered from the saved list (`updating`), card and modem confirmed at 6.6 s, data at 31 s. DITO → Smart: the card stayed unreadable for about 25 s (three `euicc_init` failures in the background re-read), reads at 3.5 s and 12.7 s still got the saved list and no error, modem confirmed at 28 s, data at 43 s.
- Delete and download were exercised only on their refusal and failure paths: the active profile is refused, a download to an unresolvable server fails at `es9p_initiate_authentication`, and a download to an unresponsive address is cancelled within 16 s. (A real download followed on 2026-10-19; see below.)

Agent `esim-8`, 2026-10-19:

- **Operator receipts.** The switching tests had left 35 enable/disable receipts on the card for three servers (`glo.prod.ondemandconnectivity.com`, `sci.prod.ondemandconnectivity.com`, `rsp-0001.linksfield.net`). The first attempt failed on TLS (the bundle didn't yet trust the GSMA root). After the fix, `POST /api/esim/notifications/process` delivered all 35 in 76 s (about 2 s each); the count went to 0 and the retry loop stopped.
- **Rename.** Setting "Test" on DITO, reading it back and clearing it worked; `generation` went up each time; a 65-byte name was refused.
- **Shared lock.** While a manual `flock -n` held the lock (then `/tmp/lpac.lock`), `chip` answered busy and `profiles` served the saved list (`updating`, `stale`); after release, `chip` worked again.
- **Download pre-check.** The start reply carried `free_nvm_bytes` (971752); a download to an unresolvable server failed safely; `DELETE /api/esim/download` cleared the result.
- **Receipts after switching** (agent `esim-11`, switches made in the app). Smart → GOMO: modem confirmed at 6 s, data at 21 s. At 26 s the Smart disable receipt was sent and the GOMO enable receipt failed at once (`SSL connect error`); the second try at 60 s sent it. On the earlier DITO → Smart switch, the first request also failed: it hung until the 45 s limit. In both cases the request right after it went through, so the first request after the attach is unreliable and the second try is needed.
- **Download against a real SM-DP+** (agent `esim-11`). A deliberately unknown matching ID on Globe's server (`glo.prod.ondemandconnectivity.com`) ran `es9p_initiate_authentication` (verified TLS), `es10b_authenticate_server` (the card accepted the server's GSMA certificate chain and signed) and `es9p_authenticate_client`. The server answered "The matchingID is not found"; lpac cancelled the session, and the download ended `failed` 6 s after it started. Nothing was installed and no receipt was created. 
- **Real download** (agent `esim-12`, from the iOS app). The test used a Klook travel eSIM QR code. The preview showed provider "Travel - PHL" and name "Klook_app" 4 s after the start; the user confirmed 9 s later. `es10b_load_bound_profile_package` took 16 s, and the whole download took 31 s. The profile was installed disabled, using about 36 KB of card storage (free space 971752 → 934475 bytes). The install receipt reached the operator right after (`operator_notified: true`), and the other profiles and the active connection were untouched. Renaming from the app worked on all three existing profiles.
- **Travel eSIM** (Klook, a Singapore-issued profile, MCC 525, that roams on Globe and Smart). The switch reached `modem_confirmed` at 5 s and ended `no_data` at 150 s. The router had data roaming off, and while roaming is blocked the firmware also turns mobile data off (`enable: 0`). The iOS app's Data Roaming save then re-sent that off value, so data stayed off even with roaming on; fixed in the app, which now turns mobile data on together with roaming. With roaming and mobile data on, the profile connected within 3 s using the provider's APN (`e-ideas`, entered in the app), and the retry loop delivered both receipts. The agent now reports `reason: roaming_off` for this case.
- **Busy card** (2026-10-19, Klook travel profile enabled). Three switches to DITO within about two minutes ended `not_applied`: lpac reported `es10c_enable_profile: unknown`, so the card answered with a code other than 1–4. An enable about two minutes later was accepted (`BF31 03 80 01 00`, result 0). DITO's modem identity was confirmed at 12 s and data at 28 s. The refusal code wasn't captured. Confirmed on the next switch away from Klook (to GOMO, agent `esim-14`): the patched lpac reported `es10c_enable_profile: cat busy` (SGP.22 `catBusy`). The agent retried every 10 s, and attempt 13 was accepted 2 min 3 s after the first. GOMO's card and modem were confirmed 5 s later, and data followed. Both busy spells lasted about two minutes from the first attempt, so the retry window is 240 s. With agent `native-5` after the Klook reinstall: Smart → Klook connected at 63 s (the network showed limited service for about 55 s on the first roaming attach), and Klook → GOMO was refused with `catBusy` 13 times over 131 s, accepted on attempt 14, with data 22 s later and both receipts delivered.
- **SIM identity event** (agent `bridge-1`, 2026-10-20, switches from the iOS app). GOMO → DITO: `leaving` (source `agent`) came with the firmware's `switch_slot_begin` and `switch_slot_end` (the latter seen live for the first time), `changed` 5.0 s later right after the second "sim ready", and `online` (`settled: true`) at 44 s, 9 s after `zte_wan6` and `zte_wan` came up (IPv6 first). One event per phase. `sim_epoch` on the endpoints moved within a second, SMS forwarding counted `ipv6_connected` as WAN up, and the firmware flushed the old connections itself. DITO → GOMO with a Live Activity running: `changed` at 4.7 s, `online` at 31.7 s; the activity paused, resumed with a new APNs connection and kept updating on the Lock Screen. In both switches the first receipt after the attach timed out and went out on the second try, and the network's clock change on attach invalidated an agent login token (the apps log in again by themselves). Idle, the tracker and SMS threads had no wakeups in 30 s.
- **Not exercised live:** the airplane-mode refusal (the modem stayed ONLINE) and the card-swap check (one card).

### Data handover recovery

After card/modem confirmation, the agent waits for a default IPv4 or IPv6 route and idle WAN scripts, then makes a short HTTPS HEAD request to `https://example.com/` using a fresh client bound to the selected CID’s source address (no proxy or connection pool). A valid TLS/HTTP response must remain successful for at least twelve seconds before the switch reports `connected`. Checks run only during the existing bounded switch-verification window, with no idle polling. `network_check` describes checking, settling, recovering, ready, unverified or failed; `data_recovery_attempted` records whether the single recovery was used.

If the new data path stays unresponsive, at most one official `zwrt_data.set_wwaniface` disconnect/reconnect is allowed. The data setting must have been enabled in automatic mode before the switch and still be enabled; the loaded SIM and switch generation must still match. The eSIM operation lock prevents a second profile action during the refresh, and newer agent modem commands cancel a pending recovery. The short disconnect/restore transaction is serialized with agent modem commands so a command queued during recovery is applied afterward and wins. The refresh preserves roaming/mode, never changes APN, does not restart Wi-Fi, and never enables a connection that was deliberately off before switching. An uncertain disconnect is followed by its restore while the captured user intent remains current. A carrier outage may still result in `no_data` / `network_unavailable`; this is not reported as a successful switch with working data.

`ZTE_AGENT_ESIM_RECOVERY=0` disables active checks and data-session recovery; status then indicates configured/unverified data. Profiles now include a `data_off` switch warning when the firmware reports mobile data off. The SIM identity bridge retains its documented routing/identity semantics; eSIM clients should use switch progress for data-path readiness.

Validation on a U60 Pro (2026-10-29): before this change, Smart → DITO intermittently stalled router and forwarded IPv4 HTTPS traffic even though the new SIM/APN, WAN routes and enabled data setting were present. An official mobile-data disconnect/reconnect restored traffic; this does not establish the internal firmware/carrier cause. With this test build, DITO → Smart and Smart → DITO both reached verified readiness after a stable check. Smart → DITO remained reachable through 172 seconds with the Wi-Fi phone still associated. The user also confirmed the Wi-Fi phone could open a new website or app after Smart → DITO without reconnecting Wi-Fi, with phone cellular data disabled. A further Smart → DITO repeat and GOMO → DITO also passed router/USB checks through approximately three minutes, with phone internet confirmed without reconnecting Wi-Fi. DITO → GOMO passed router/USB checks with the phone still associated; phone application connectivity was not separately confirmed for that direction. None of these successful final live runs needed automatic recovery; its one-attempt budget, preference preservation and command ordering were verified with deterministic tests.
