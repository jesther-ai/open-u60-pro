# Tailscale as Remote Management in zte-agent — Feasibility & Design Investigation

**Device:** ZTE U60 Pro (Qualcomm SDX75 "sdxpinn", 4× Cortex-A55 @ 2.2 GHz, 1.6 GB RAM, 8 GB eMMC, OpenWrt 23.05.4 userland on vendor kernel `5.15.170-perf`, CGNAT cellular WAN `100.119.13.253/30`, live global IPv6)
**Tailscale version referenced throughout:** stable **1.98.9** (published 2026-07-14). **Report date:** 2026-07-17.

---

## 1. Verdict

**Feasible, and a strong fit.** Recommended shape: the zte-agent spawns and supervises the **official static arm64 `tailscaled` v1.98.9 as a sidecar child process** stored on `/data/tailscale/`, started in **`--tun=userspace-networking` mode**, driven by **shelling out to the `tailscale` CLI** (exactly mirroring the existing `ubus::call` pattern — sync, `std::process::Command`, no new crates). The binary is pure-Go and fully static (verified: `CGO_ENABLED=0`, `netgo,osusergo` tags, no `PT_INTERP`/`PT_DYNAMIC`, zero GLIBC refs, `GOARM64=v8.0` ≤ Cortex-A55; Go needs only Linux ≥3.2 vs the device's 5.15.170), so musl is irrelevant. Userspace mode is *safe by construction* on this QCMAP-owned, CGNAT-addressed device: verified at source level, it creates no TUN, no routes, no `ip rule`s, no netfilter rules, and no resolv.conf changes, while inbound tailnet connections (IPv4 *and* IPv6/MagicDNS) are terminated by gVisor netstack and redialed to `127.0.0.1:<same port>` — which reaches the agent's existing `0.0.0.0:9090` listener with its bearer-token auth intact. RAM (1.6 GB total, ~225 MB used by the stock ZTE stack) and flash (8 GB eMMC) leave ample headroom for tailscaled's ~60–75 MB idle RSS and ~38 MB binary. Kernel-TUN mode is a likely-available later upgrade (Qualcomm's reference `sdxpinn` OpenWrt defconfig ships `CONFIG_TUN=y` and even `CONFIG_WIREGUARD=y`), but it must be gated on an on-device probe and requires explicit CGNAT-DROP-rule mitigation. The genuinely novel risk — no prior art exists — is the ZTE autosleep subsystem, handled by holding a wakelock via the ubus API the agent already wraps.

---

## 2. Options Considered

| | (a) Sidecar: supervise stock tailscaled | (b) Embed libtailscale in agent | (c) OpenWrt package | (d) Pure-Rust / plain WireGuard | (e) headscale |
|---|---|---|---|---|---|
| **Binary size** | 0 bytes added to the 2.4 MB agent; 38.5 MB `tailscaled` + 29.6 MB CLI on `/data` (or 25.4 MB self-built multicall; 6.6 MB xz-at-rest) | Go runtime *inside* the agent: agent grows ~10×, killing the "~2.3 MB" selling point (README:111) | 1.58.2 frozen in the 23.05 feed; ipk infra irrelevant here | tailscale-rs v0.4.0 is small, but see right | n/a (server-side) |
| **RAM** | ~60–75 MB RSS in a separate, killable process | Same footprint **inside** the agent — a Go OOM kills the whole agent | same as (a) | low | n/a |
| **TUN dependency** | None in userspace mode | None (tsnet/netstack) | Hard `+kmod-tun` dep — uninstallable on the vendor kernel (vermagic mismatch) | boringtun needs TUN | n/a |
| **Maintenance** | Audited official builds; binary-swap upgrades (GL.iNet-proven: `glinet-tailscale-updater`, HA add-on) | Documented aarch64+musl c-archive linker bugs (golang/go#62556, rust-lang/rust#97117); Go runtime steals signal handling; no releases tagged | Rots (GL.iNet lesson: vendor-frozen versions spawn a community-updater ecosystem) | tailscale-rs is `TS_RS_EXPERIMENT=this_is_unstable_software`, DERP-only, "do not build production software"; plain WireGuard has **no NAT traversal/control plane** — dead behind CGNAT without your own rendezvous infra | You now operate a control plane; enforces client minimums (v0.29.1 needs client ≥1.80), unlike Tailscale's "never break old clients" pledge |
| **Fit with no-tokio sync agent** | Perfect: `std::process::Command` shell-outs mirror `ubus.rs`; supervision mirrors `event_bus.rs::run_loop` | Poor: cgo + Rust runtime cohabitation; tsnet API can't do subnet routes or lifecycle control | n/a | jtdowney/tailscale-localapi crate needs tokio+hyper — disqualified anyway | orthogonal |

**Pick: (a) sidecar.** It is what every shipping precedent does (GL.iNet `gl_tailscale` wrapper, Home Assistant s6-supervised add-on), it keeps the agent pure-Rust/2.4 MB, isolates a Go OOM from the agent process, gives the full feature set (subnet routes, Tailscale SSH, serve), and upgrades are an atomic binary swap on `/data`. (e) headscale stays a note: the hosted free plan (post-April-2026 "pricing v4": 6 users, unlimited user devices, **50 free tagged resources**) covers this router at $0; revisit headscale only for a no-third-party requirement. (d) tailscale-rs is the credible 2027 successor to watch once NAT traversal lands and the experiment gate drops.

**Binary flavor within (a):** ship the **official static tarball binaries unmodified** (audited, and `tailscale ssh` is reported broken in symlinked multicall mode, issue #12125). If flash pressure appears, the fallback is a self-built multicall (`go build -tags ts_include_cli,ts_omit_aws,ts_omit_bird,ts_omit_tap,ts_omit_kube,ts_omit_taildrop,ts_omit_tpm,ts_omit_webclient,... -ldflags="-s -w"` → measured **25,362,594 bytes** at v1.98.9, xz −9 = 6,577,636 bytes; requires Go ≥1.26.5). **Do not UPX**: open segfault bug when packed daemon+CLI run simultaneously (#8613), and the image decompresses into non-evictable anonymous RAM.

---

## 3. Resource Budget

| Resource | Need | Have | Verdict |
|---|---|---|---|
| Flash (`/data`) | 38.5 MB `tailscaled` + 29.6 MB `tailscale` CLI (official 1.98.9) **or** 25.4 MB multicall; state file ~1–2 KB; keep `--statedir` log churn (`tailscaled.log1.txt`/`log2.txt`, written even with `--no-logs-no-support`, FR #9549) on tmpfs | 8 GB eMMC; **free space on /data unverified** — checklist item #1 | Almost certainly fine; confirm `df` |
| RAM | ~60–75 MB RSS idle (measured 72.3 MB on GL-MT300N-V2, tailscale#18013); 85–100+ MB under bulk transfer (#7272, #16258 traced to unbounded wireguard-go buffer pools) | **1.6 GB** (README.md:33 — the "RAM unknown" concern from earlier findings was refuted; stock ZTE stack uses ~225 MB) | >1 GB headroom; OOM pressure on the sync-barrier `zte_topsw_*` daemons is negligible |
| tmpfs | `--statedir`, socket, logs: a few MB | `/tmp` is tmpfs | fine |
| CPU | Go crypto + netstack; management-plane traffic is trivial. A 128 MB MIPS box did 2–3 Mbit/s (#17915); 4× A55 @2.2 GHz will do tens of Mbit/s in userspace mode | 4× Cortex-A55 | fine for management; do not advertise as exit node (metered link anyway) |
| Idle cellular data | **~1–5 MB/day**, not "tens of MB/day": since PR #1175 (Jan 2021, still default) periodic STUN and disco keepalives **stop entirely** after 45 s of tunnel idle; the idle baseline is one DERP keepalive frame/~60 s + ~1 control keep-alive/min. Cadence resumes (20–26 s STUN + 3 s heartbeats) only while a session is active. `--no-logs-no-support` removes logtail uploads | metered plan | acceptable; 24 h measurement in checklist |

Optional hygiene (not load-bearing at 1.6 GB): start tailscaled with `GOGC=25 GOMEMLIMIT=192MiB`. Note GOMEMLIMIT is a *soft* limit (NanoKVM#660 saw kills despite it) — it is a bound, not a guarantee.

---

## 4. Full Lifecycle Design

### 4.1 Install

- **Transport:** extend `setup.sh` with an optional Tailscale step, reusing its existing SSH/ADB push machinery: download `https://pkgs.tailscale.com/stable/tailscale_1.98.9_arm64.tgz` (34,254,455 bytes) on the workstation, verify sha256 (record the pinned hash in the repo), extract, push the two binaries. Alternatively the agent itself downloads via `ureq` on demand (`POST /api/tailscale/install`) — same code path as the upgrade flow (§4.8).
- **Layout:**
  ```
  /data/tailscale/
    bin/tailscaled            # 38.5 MB, chmod 755
    bin/tailscale             # 29.6 MB CLI
    tailscaled.state          # node+machine keys, prefs (~1–2 KB, 0600) — THE identity
    tailscaled.state.bak      # post-enrollment backup (§4.6)
  /tmp/tailscale/             # created at spawn (tmpfs)
    tailscaled.sock           # LocalAPI socket
    (statedir: log1/log2 churn lands here, not on flash)
  /data/local/tmp/tailscale_config.json   # agent's own config (mirrors doh_config.json)
  ```
- Rationale for the split (verified): `--state` on `/data` so identity survives reboot (OpenWrt's classic lost-state-on-tmpfs failure, openwrt/packages#19774); `--statedir` on `/tmp` because tailscaled writes log files there even with logging disabled (#9549). **Exception:** if you later enable `tailscale serve` with TLS, move `--statedir` to `/data/tailscale/statedir` so the Let's Encrypt cert cache persists (re-issuing every boot risks LE rate limits, ~34 h lockouts).

### 4.2 Boot / start

**The agent spawns tailscaled as a supervised child. Not a separate rc.local line.** Rationale: rc.local processes get no supervision at all on this box (the agent itself is a nohup'd rc.local orphan — there is no procd service for us, deliberately, given the daemon.conf sync-barrier minefield). The agent is the only supervisor that exists; it already owns the identical pattern for `ubus listen` (`event_bus.rs::run_loop`: spawn → wait → reap → sleep → respawn), and only the agent can gate startup on WAN/NTP, hold the wakelock, and expose status.

**Startup gates before first spawn (auto_start):**
1. **Clock sane** — control-plane TLS fails with a 1970 clock ("certificate not yet valid" class; #11518 documents the DNS/NTP deadlock). Gate: `libc::time` year ≥ 2025, or retry loop. Never let the router itself use Tailscale DNS.
2. **WAN connected** — `ubus call zwrt_data get_wwaniface` shows `connect_status == "connected"`. This matches the community "delayed start" workaround for boot-order routing races (openwrt/packages#23480).

**Exact spawn (ship configuration):**
```
GOGC=25 GOMEMLIMIT=192MiB /data/tailscale/bin/tailscaled \
  --tun=userspace-networking \
  --state=/data/tailscale/tailscaled.state \
  --statedir=/tmp/tailscale \
  --socket=/tmp/tailscale/tailscaled.sock \
  --port=41641 \
  --no-logs-no-support \
  >/tmp/tailscaled.log 2>&1
```
Every CLI invocation carries `--socket=/tmp/tailscale/tailscaled.sock`.

**Why `--tun=userspace-networking` for v1 (all verified):**
- Zero OS mutation: no TUN, no `router.New` (fake no-op router), no table-52 routes, no ip rules, no iptables/nftables, no DNS configurator (`cmd/tailscaled/tailscaled.go` `tryEngine` onlyNetstack branch). Nothing for QCMAP to fight; the CGNAT DROP-rule hazard (§6) cannot occur.
- Inbound works: gVisor netstack terminates peer TCP/UDP to the node's Tailscale IPv4 **or IPv6** address and redials `127.0.0.1:<same port>` — reaching the agent's `0.0.0.0:9090` (main.rs:31). The old post-start inbound flake (#2642) was an ECN bug **fixed in v1.20.2 (2022)** — not a current concern; still gate readiness on status, not process presence.
- Sidesteps the (probable but unprobed) CONFIG_TUN question entirely.
- Cost: services see source IP `127.0.0.1` (keep the agent's bearer auth mandatory — no peer-IP trust; #5870), TCP/UDP only, lower throughput (irrelevant for management).

**Post-start prefs, applied idempotently after every daemon start** (HA add-on pattern — treat "process up" and "backend Running" as separate states):
```
tailscale --socket=... set --accept-dns=false --accept-routes=false --hostname=u60-pro
```
`--accept-dns=false` is mandatory hygiene: MagicDNS otherwise clobbers `/etc/resolv.conf` on OpenWrt-style systems (#15174, openwrt/packages#26761) — doubly important since the agent runs its own DoH proxy. In userspace mode no DNS configurator runs anyway, but set it so a future kernel-mode switch can't regress.

**Kernel-TUN upgrade path (v2, gated on checklist):** Qualcomm's CLO reference build for this exact SoC+userspace combo (`build.config.msm.sdxpinn`: `MSM_ARCH=sdxpinn`, `PREFERRED_USERSPACE=owrt`, perf_defconfig = `generic_csm_defconfig` + `vendor/sdxpinn.config`) ships **`CONFIG_TUN=y` and `CONFIG_WIREGUARD=y`**, and the sdxpinn fragment does not unset them — so TUN is very likely built-in on the U60 Pro; only ZTE-specific stripping remains possible. If the probe confirms TUN and you want real peer IPs + kernel-speed subnet routing, switch to kernel mode **only with**: the `disable-linux-cgnat-drop-rule` nodeAttr in the tailnet policy (capver 136, first stable v1.98.2 — converts the `! -i tailscale0 -s 100.64.0.0/10 -j DROP` that tailscaled inserts at **position 1 of filter/INPUT** into RETURN), because the carrier WAN 100.119.13.253 is inside that range; note `--netfilter-mode=off` is **not** zero-state (it still installs ip rules at pref 5210/5230/5250/5270 and table-52 per-peer /32 routes — a /32 collision with a carrier endpoint is a small but real risk), and iptables backend choice must follow what `iptables -V` reveals (legacy vs nf_tables) — QCMAP is legacy-iptables on this firmware.

### 4.3 Enroll

**Primary flow — auth key from the mobile app:**
1. User mints a key in the admin console (or the app does via its own credentials): **one-off (single-use) + preauthorized + tagged `tag:router`**. Tagging *at authentication time* disables node-key expiry by default (official behavior since 2022-03-10) — this is the whole 180-day-lockout fix. One-off means the key is dead after use even if it leaks from a request log.
2. App calls `POST /api/tailscale/setup {"auth_key": "tskey-auth-..."}` (over LAN, or over the tunnel of another already-working transport).
3. Agent writes the key to `/tmp/ts-authkey` (tmpfs, mode 0600), runs
   `tailscale --socket=... up --auth-key=file:/tmp/ts-authkey --timeout=60s`
   (the `file:` form keeps the key out of `/proc/*/cmdline`), then deletes the file. **The key is never written to `/data`, never stored in `tailscale_config.json`, never logged.**
4. Agent polls status until `BackendState == "Running"`, then backs up the state file (§4.6) and persists `{"enabled": true}`.

**Critical rule (verified, bug still open):** `--auth-key` is passed **only** when no valid state exists (`tailscaled.state` absent, or `BackendState == NeedsLogin` with empty `HaveNodeKey`). Re-running `up --auth-key` on every boot with an expired key **blocks an already-authenticated node from coming online** — tailscale#16987 (open, no fixed release as of 2026-04), #19501 closed as its duplicate; this is the pfSense-class footgun. The boot path is: state file exists → spawn daemon, apply `set` prefs, done — no `up` at all.

**Alternative flow — interactive login URL (no key handling at all):** verification refuted the "LocalAPI is unavoidable" finding — `tailscale up --json` performs the full interactive enrollment and emits `upOutputJSON{AuthURL, QR, BackendState, Error}` (it internally drives `watch-ipn-bus` + `login-interactive`, sidestepping still-open #15002 where BrowseToURL isn't populated by watching alone). Agent endpoint `GET /api/tailscale/login-url`: spawn `tailscale up --json` on a worker thread, parse `AuthURL` from stdout, cache it; the mobile app opens it in a browser; agent watches for `Running`. Downside: the device joins tagged only if the policy `tagOwners` + `up --advertise-tags=tag:router` are set, and user-owned nodes keep 180-day expiry unless disabled in the console — prefer the tagged-key flow.

**No OAuth client secret on the device.** An `auth_keys`-scoped OAuth secret is a tailnet-wide credential; if programmatic key minting is wanted, do it in the mobile app or workstation (`POST /api/v2/oauth/token` → `POST /api/v2/tailnet/-/keys` with `{"capabilities":{"devices":{"create":{"reusable":false,"ephemeral":false,"preauthorized":true,"tags":["tag:router"]}}},"expirySeconds":3600}`). Never ephemeral: ephemeral nodes are garbage-collected ~30–60 min after going offline — fatal for a device that sleeps or loses cellular coverage.

### 4.4 Steady state — monitoring & the state machine

**Interface choice (verification-corrected):** neither surface is *officially* stable — `tailscale status --json` prints "WARNING: format subject to change" and is literally a re-serialization of the same `ipnstate.Status` struct LocalAPI serves; but maintainers treat shipped JSON as frozen-in-practice (#17619, closed: "we're stuck with whatever JSON output we originally decided"), and the top-level fields are empirically identical from v1.90.0 through v1.98.8. On a 1.6 GB device, forking the 29.6 MB CLI every 30 s is acceptable (the documented CLI-fork OOM vector applies to ≤128 MB routers). **Decision: CLI shell-out for everything** (matches `ubus::call`, one code path, pinned binary version); a ~120-line `UnixStream` HTTP/1.1 LocalAPI client for `/localapi/v0/status` is a later optimization, not a v1 requirement.

**Monitor thread** (spawned from `TailscaleManager::start`, mirrors DoH's `AtomicBool running` + JoinHandle):
- Every 30 s (60 s when healthy for a while): `tailscale --socket=... status --json --peers=false`, hard-killed after 20 s if hung.
- Parse **only** established top-level fields with serde ignore-unknown + defaults: `BackendState`, `Health` (`[]string`, empty = healthy), `AuthURL`, `TailscaleIPs`, `ClientVersion`, `Self.{Online, KeyExpiry, Expired, Relay, CurAddr, LastHandshake, DNSName}`.

**State machine** (BackendState strings, exact: `NoState`, `NeedsLogin`, `NeedsMachineAuth`, `Stopped`, `Starting`, `Running`):

| Observation | Meaning | Agent action | Surfaced as |
|---|---|---|---|
| child reaped / CLI can't reach socket | daemon dead | respawn with backoff (5 s → 60 s cap, jittered); run `tailscaled --cleanup` first after a crash | `"respawning"` |
| `NeedsLogin`, state file **absent** | never enrolled / uninstalled | wait for setup | `"awaiting_setup"` |
| `NeedsLogin`, state file **present** | spurious logout (#18677) or corrupt state | restart daemon once; if repeated → try `.bak` state (§4.6); else escalate | `"needs_login"` + alert |
| `NeedsMachineAuth` | device approval pending (shouldn't occur with preauthorized key) | poll | `"pending_approval"` |
| `Stopped` | `WantRunning=false` (user ran disable) | only `tailscale up` if `config.enabled` says so | `"disabled"` |
| `Starting` > 120 s | wedged connect | rebind/restun nudge → restart | `"connecting"` |
| `Running`, `Health == []` | healthy | record `Relay` (DERP region) and `CurAddr` (`""` = relayed, expected default behind CGNAT-to-CGNAT; carrier IPv6 gives direct-path odds) | `"healthy"` |
| `Running`, `Health != []` | degraded | include strings verbatim; if persists 2 cycles → nudge/restart | `"degraded"` |

`GET /api/tailscale/status` returns `{installed, enabled, backend_state, healthy, health: [...], tailscale_ips, dns_name, relay, direct: bool, key_expiry: null|ts, client_version, login_url: null|url}`.

**Scheduler interaction:** `route()` is re-entered by the scheduler (scheduler.rs:269), so new endpoints are schedulable by default. Blocklist `/api/tailscale/setup`, `/logout`, and `/update` from the scheduler the same way auth/scheduler paths are (scheduler.rs:124-129) — a scheduled logout is a remote-lockout device.

### 4.5 Reconnect — what self-heals vs. what the supervisor owns

**tailscaled self-heals (v1.98.x, verified in source):** `net/netmon` watches rtnetlink and detects wake-from-sleep via wall-clock jumps, synthesizing a major-change event that forces magicsock rebind + re-STUN; DERP fallback is automatic and connections upgrade opportunistically; wake/suspend fixes shipped in 1.82, 1.90.4 (eventbus lockup #17677), and 1.98.x (wireguard-go wake fix). Cellular IP flaps *usually* recover unaided.

**Known self-heal gaps (all issues verified open):** Linux post-suspend wedges needing a nudge (#10688); stale endpoints after WAN IP change (#7342, triaged "very few"); slow control-plane reconnect (#13379; #19199's fix PR #19200 unmerged); and — the U60-specific trap — netmon's `majorTimeJumpThreshold = 10 min` means **short autosleep suspends don't trigger a forced rebind**, while CGNAT UDP mappings expire in well under a minute → stale NAT state with no recovery trigger.

**Supervisor policy (in order of escalation):**
1. **Crash respawn:** event_bus blueprint with exponential backoff (5 s → 60 s cap).
2. **WAN-flap hook:** the agent already subscribes to `router_event_wan_connect_status` (main.rs:53). On reconnect: wait 30 s → check status → if `Self.Online == false` or `Health != []`: run `tailscale debug rebind` then `tailscale debug restun` (LocalAPI-backed, no daemon restart — verified to exist; unstable namespace, acceptable for a pinned version) → recheck in 30 s → still bad: SIGTERM (never `kill -9`; the daemon is agent-owned, no procd involvement) and respawn.
3. **Suspend/resume:** monitor loop detects a resume as a wall-clock gap > 2× poll interval → proactively rebind/restun, escalate to restart if unhealthy.
4. **Wakelock policy (recommend: default ON while Tailscale is enabled).** The sleep mechanism is full Linux autosleep (`echo mem > /sys/power/autosleep`); a suspended SoC cannot answer DERP frames and there is **no network wake source** in the ZTE wake list — a sleeping node is unreachable, defeating the feature. Hold `ubus call zwrt_zte_sleep_faw.wakelock createWakelock` while enabled (plumbing already exists in `zte-agent/src/device_ext.rs:183-235`), re-create in `auto_start`, `destroyWakelock` on disable. Expose `hold_wakelock: bool` (default true) for users who prefer battery over reachability. Note the probed unit already has `sleepSwitch='0'` (auto-sleep off), so the default costs nothing there. Nuance from verification: keepalives at ~1 pkt/min do **not** hold the cellular radio in RRC-connected (5–20 s inactivity release) — the wakelock's cost is AP power, not radio time. The SDX75 modem *may* wake the AP on inbound packets (MHI/IPA wake interrupts) — checklist measures this; if inbound-wake works, the wakelock could become session-scoped.
5. **Timeout discipline:** every `up` carries `--timeout=30s` (default is block-forever); every CLI child gets a kill-after-20 s guard so a wedged CLI can't hang a worker thread.

### 4.6 Expiry & keys

- **Primary defense:** tagged enrollment → `keyExpiryDisabled: true` by default. Verify post-enroll: `Self.KeyExpiry` is *absent* from status JSON when disabled (`*time.Time` with `omitempty` — verified in ipnstate source).
- **Belt & braces:** weekly check — if `KeyExpiry` is present (someone re-enabled expiry, or the node was enrolled untagged), alert at T-14 days through the existing sms_forward/notification channel; the app then drives the login-url re-auth flow. Treat `Self.Expired == true` as an immediate alert.
- **State backup:** after first reaching `Running`, copy `tailscaled.state` → `tailscaled.state.bak` (0600). Recovery ladder for the "stuck NeedsLogin with state present" signature (#9382 class): restart daemon → restore `.bak` → surface "re-enrollment required" (user must also delete the stale machine in the admin console, else the node re-registers as a *new* machine with a new 100.x IP).
- Auth keys max at 90 days and are consumed at enrollment — they play no role in steady state. `tailscale down` (not `logout`) is the "disable remote access" verb: it keeps credentials, so re-enable needs no interaction.

### 4.7 Exposure & security

- **Default exposure: agent `:9090` over the tailnet only.** Works as-is (loopback redial → `0.0.0.0:9090`). **Keep bearer-token auth mandatory** — in userspace mode every request arrives from `127.0.0.1`, so there is no peer-IP identity; WireGuard provides transport encryption, the token provides authn. Set `ZTE_AGENT_PASSWORD` always (unauthenticated-when-unset mode is now remotely reachable).
- **SSH:** dropbear reachability depends on its bind — CLAUDE.md's "SSH: Port 2222 at 192.168.0.1" may mean a LAN-IP-only bind, in which case the netstack redial to `127.0.0.1:2222` gets `connection refused` (the exact "could not connect to local backend server" failure string from #13931). Checklist item; fixes in order of preference: (1) dropbear also on loopback, (2) subnet route, (3) Tailscale SSH (`tailscale set --ssh`, tailnet-side port 22 only, doesn't touch dropbear, ACL-governed, no key management — but test busybox shell spawning on-device; restarting tailscaled drops live sessions).
- **Optional subnet route** `192.168.0.0/24` (ZTE web UI at 192.168.0.1:80): works even in userspace mode (netstack terminates and re-originates TCP/UDP; lower throughput, fine for a web UI). Add `autoApprovers` so enrollment needs no console click.
- **ACL policy (replace the default allow-all — a fresh tailnet exposes 9090 to every tailnet device otherwise):**
```jsonc
{
  "tagOwners": { "tag:router": ["autogroup:admin"] },
  "acls": [
    { "action": "accept",
      "src":    ["jesther@tradestockapps.com"],
      "dst":    ["tag:router:9090,2222,22,80"] }
  ],
  "autoApprovers": { "routes": { "192.168.0.0/24": ["tag:router"] } },
  // Only if/when kernel-TUN mode ships (>= v1.98.2):
  "nodeAttrs": [
    { "target": ["tag:router"], "attr": ["disable-linux-cgnat-drop-rule"] }
  ],
  "ssh": [
    { "action": "accept", "src": ["jesther@tradestockapps.com"],
      "dst": ["tag:router"], "users": ["root"] }
  ],
  "tests": [
    { "src": "jesther@tradestockapps.com", "accept": ["tag:router:9090"] }
  ]
}
```
  (SSH-rule pitfall: `autogroup:self` never matches tagged devices — explicit `users` required.)
- **`tailscale serve` (optional, off by default):** `serve --bg --https=443 http://127.0.0.1:9090` adds ts.net TLS + `Tailscale-User-Login` identity headers (free per-user authz for the agent). Costs: MagicDNS+HTTPS must be enabled tailnet-wide, machine names are published to the public **Certificate Transparency ledger** (name the node blandly), and the cert cache needs a persistent statedir. Default: skip — WireGuard already encrypts, the token already authenticates.
- **Funnel: explicitly rejected.** Anonymous public-internet exposure with zero tailnet identity — the opposite of this feature. Ensure the `funnel` nodeAttr is **not** granted to `tag:router` (the default policy grants it to `autogroup:member`).
- `--no-logs-no-support` stays on (privacy + metered link); verify with `tailscale bugreport` printing `BUG-NO-LOGS-NO-SUPPORT-...`.

### 4.8 Upgrade & uninstall

- **Version pinning:** `tailscale_config.json` records `"pinned_version": "1.98.9"`. Tailscale publicly commits to never breaking old clients against the hosted control plane, so upgrades are on your schedule (security fixes), not forced.
- **Never `tailscale update` / `set --auto-update`:** the tarball fallback hardcodes `/usr/sbin` and dies on the read-only rootfs (#11348, #10632). Agent-managed flow:
  1. `ureq` GET `https://pkgs.tailscale.com/stable/tailscale_<ver>_arm64.tgz`; verify sha256 against a hash the app/operator supplies (or the `.sig` if you implement verification).
  2. Extract to `/data/tailscale/bin/*.new`; smoke-test `tailscaled.new --version` executes.
  3. SIGTERM daemon (state file preserves identity across the swap) → rename current → `*.prev`, `*.new` → current → respawn → require `Running` within 120 s **or roll back to `*.prev`** and respawn.
  4. Never trigger over the tunnel without the rollback watchdog armed; prefer initiating from LAN. Restart kills live Tailscale SSH sessions.
- **Uninstall (full):** `tailscale logout` (invalidates node key; server-side machine entry should also be deleted in the console or `DELETE /api/v2/device/{id}` from the app) → SIGTERM → `tailscaled --cleanup` (no-op in userspace mode, harmless) → `destroyWakelock` → `rm -rf /data/tailscale /tmp/tailscale` → delete/disable `tailscale_config.json`. No rc.local line exists to remove (agent-spawned). "Disable" (keep enrollment) = `tailscale down` + kill daemon + `enabled:false`.

---

## 5. Rust Integration Sketch

New module `zte-agent/src/tailscale/` (mod.rs + config.rs), mirroring `doh/`. Zero new dependencies; adds an estimated ~40–70 KB to the 2.4 MB binary (deploy.sh prints the delta). Wiring: `mod tailscale;` in main.rs, `pub tailscale: Arc<tailscale::TailscaleManager>` in `AppState` (handlers.rs), `state.tailscale.auto_start(&state)` in `main()` after `doh.auto_start()`, match arms in `route()`.

**Endpoints (module-owned handler style, uniform `(u16, Value)` signature):**

| Method+Path | Purpose |
|---|---|
| `GET  /api/tailscale/status` | full state-machine view (§4.4 payload) |
| `POST /api/tailscale/setup` | body `{"auth_key": "..."}` — one-time enrollment (blocklisted from scheduler) |
| `GET  /api/tailscale/login-url` | interactive-flow alternative: returns cached `AuthURL` |
| `POST /api/tailscale/enable` | persist enabled, spawn+supervise, acquire wakelock |
| `POST /api/tailscale/disable` | `tailscale down`, kill child, release wakelock, persist |
| `PUT  /api/tailscale/config` | patch `{hostname, advertise_routes, hold_wakelock, tailscale_ssh}` → re-apply via `tailscale set` |
| `POST /api/tailscale/logout` | uninstall step 1 (blocklisted from scheduler) |
| `POST /api/tailscale/update` | body `{"version": "...", "sha256": "..."}` — swap-with-rollback (blocklisted) |

```rust
// zte-agent/src/tailscale/config.rs — mirrors doh/config.rs exactly
const CONFIG_PATH: &str = "/data/local/tmp/tailscale_config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsConfig {
    pub enabled: bool,
    #[serde(default = "default_hostname")]
    pub hostname: String,
    #[serde(default)]
    pub advertise_routes: Vec<String>,   // e.g. ["192.168.0.0/24"]
    #[serde(default = "default_true")]
    pub hold_wakelock: bool,
    #[serde(default)]
    pub tailscale_ssh: bool,
    #[serde(default = "default_version")]
    pub pinned_version: String,
}
// Default / load() swallowing errors / save() pretty-printed — as doh/config.rs:47-57.

// zte-agent/src/tailscale/mod.rs
const BIN_DIR: &str = "/data/tailscale/bin";
const STATE_FILE: &str = "/data/tailscale/tailscaled.state";
const RUN_DIR: &str = "/tmp/tailscale";
const SOCK: &str = "/tmp/tailscale/tailscaled.sock";

pub struct TailscaleManager {
    config: Mutex<TsConfig>,
    running: AtomicBool,                       // supervisor loop gate
    child_pid: Mutex<Option<u32>>,
    status: Mutex<Value>,                      // last parsed status for GET
    login_url: Mutex<Option<String>>,
}

/// `tailscale <args>` -> parsed JSON stdout. Mirrors ubus::call (ubus.rs:6-30).
fn cli_json(args: &[&str]) -> Result<Value, String> {
    let out = Command::new(format!("{BIN_DIR}/tailscale"))
        .arg(format!("--socket={SOCK}"))
        .args(args)
        .output()
        .map_err(|e| format!("spawn tailscale: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("parse: {e}"))
}

impl TailscaleManager {
    /// main() calls this once. Gates on clock+WAN, then supervises.
    pub fn auto_start(self: &Arc<Self>, state: &Arc<AppState>) {
        if !self.config.lock().unwrap().enabled { return; }
        if !Path::new(&format!("{BIN_DIR}/tailscaled")).exists() { return; }
        self.running.store(true, Ordering::Relaxed);
        if self.config.lock().unwrap().hold_wakelock {
            let _ = crate::ubus::call("zwrt_zte_sleep_faw.wakelock",
                                      "createWakelock", json!({}));
        }
        let mgr = Arc::clone(self);
        std::thread::spawn(move || mgr.supervise());   // event_bus.rs:45-63 blueprint
    }

    fn supervise(&self) {
        let mut backoff = 5u64;
        while self.running.load(Ordering::Relaxed) {
            wait_for_clock_and_wan();                       // §4.2 gates
            let _ = std::fs::create_dir_all(RUN_DIR);
            match self.spawn_tailscaled() {
                Ok(mut child) => {
                    *self.child_pid.lock().unwrap() = Some(child.id());
                    self.apply_prefs_when_ready();          // set --accept-dns=false ...
                    self.monitor_until_exit(&mut child);    // 30s status poll + nudges
                    let _ = child.wait();                   // reap
                    backoff = 5;
                }
                Err(e) => eprintln!("[tailscale] spawn failed: {e}"),
            }
            if !self.running.load(Ordering::Relaxed) { break; }
            std::thread::sleep(Duration::from_secs(backoff));
            backoff = (backoff * 2).min(60);
        }
    }

    fn spawn_tailscaled(&self) -> std::io::Result<std::process::Child> {
        Command::new(format!("{BIN_DIR}/tailscaled"))
            .args([
                "--tun=userspace-networking",
                &format!("--state={STATE_FILE}"),
                &format!("--statedir={RUN_DIR}"),
                &format!("--socket={SOCK}"),
                "--port=41641",
                "--no-logs-no-support",
            ])
            .env("GOGC", "25").env("GOMEMLIMIT", "192MiB")
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
    }

    /// POST /api/tailscale/setup — only path that ever passes an auth key (#16987).
    pub fn setup(&self, body: &[u8]) -> (u16, Value) {
        let key = match serde_json::from_slice::<Value>(body)
            .ok().and_then(|v| v["auth_key"].as_str().map(String::from)) {
            Some(k) => k,
            None => return (400, json!({"ok": false, "error": "missing 'auth_key'"})),
        };
        if Path::new(STATE_FILE).exists() && self.backend_state() == "Running" {
            return (409, json!({"ok": false, "error": "already enrolled"}));
        }
        let keyfile = format!("{RUN_DIR}/authkey");
        if let Err(e) = std::fs::write(&keyfile, &key) {
            return (500, json!({"ok": false, "error": format!("write key: {e}")}));
        }
        let res = cli_json(&["up", "--json", "--timeout=60s",
                             &format!("--auth-key=file:{keyfile}")]);
        let _ = std::fs::remove_file(&keyfile);            // key never persists
        match res {
            Ok(_) => { let _ = std::fs::copy(STATE_FILE,
                           format!("{STATE_FILE}.bak"));    // §4.6 backup
                       (200, json!({"ok": true})) }
            Err(e) => (502, json!({"ok": false, "error": e})),
        }
    }
}

// server.rs route() additions:
//   (&Method::Get,  "/api/tailscale/status")    => tailscale::status(state),
//   (&Method::Post, "/api/tailscale/setup")     => state.tailscale.setup(body),
//   (&Method::Post, "/api/tailscale/enable")    => tailscale::enable(state),
//   (&Method::Post, "/api/tailscale/disable")   => tailscale::disable(state),
//   (&Method::Get,  "/api/tailscale/login-url") => tailscale::login_url(state),
//   (&Method::Put,  "/api/tailscale/config")    => tailscale::config_set(state, body),
//   (&Method::Post, "/api/tailscale/logout")    => tailscale::logout(state),
//   (&Method::Post, "/api/tailscale/update")    => tailscale::update(state, body),
// scheduler.rs: add setup/logout/update to the scheduling blocklist (cf. scheduler.rs:124-129).
```

Status parsing: define a `#[derive(Deserialize)] #[serde(default)]` struct with only `BackendState: String`, `Health: Vec<String>`, `AuthURL: String`, `TailscaleIPs: Vec<String>`, `ClientVersion: Value`, `Self_: PeerStatus` (`#[serde(rename = "Self")]`, fields `Online: bool`, `KeyExpiry: Option<String>`, `Relay: String`, `CurAddr: String`) — unknown fields ignored, matching the drift-tolerance strategy. `check-reboot.sh` gets a `pidof tailscaled` line next to the existing `pidof zte-agent` check.

---

## 6. Risks & Mitigations

| Risk | Reality (verified, with numbers) | Mitigation |
|---|---|---|
| **QCMAP iptables interference** | Refuted as "clobber": QCMAP source shows only exact-match `iptables -D` of its *own* rules on WAN events — no `-F`/`-X` on IP tables (only ebtables L2 flushes). Foreign chains survive data-call bounces. Residual: QCMAP re-INSERTs its rules at position 1 (ordering churn), ZTE build could deviate, and tailscaled never re-installs removed rules (#5424, open). | Userspace mode = **zero rules exist to clobber**. If kernel mode later: run the canary-rule + data-call-bounce diff (checklist), keep the WAN-event status check as cheap insurance. |
| **Autosleep kills the tunnel** | Full-system autosleep (`echo mem > /sys/power/autosleep`); no network wake source; netmon skips forced rebind for sleeps <10 min (`majorTimeJumpThreshold`) while CGNAT UDP mappings die in <1 min → silent stale-NAT. No prior art anywhere for Tailscale + Qualcomm autosleep. | Default: hold `zwrt_zte_sleep_faw` wakelock while enabled (agent already wraps it; probed unit has `sleepSwitch=0` anyway). On detected resume/WAN-flap: `tailscale debug rebind` + `restun`, then restart if still unhealthy. Measure inbound-packet AP wake on-device — may allow relaxing to session-scoped wakelock. |
| **Metered idle data cost** | ~1–5 MB/day idle (DERP keepalive 1 frame/~60 s + control keep-alive ~1/min; periodic STUN and disco **stop after 45 s idle** — default since PR #1175, 2021). The old "STUN every 25 s forever" figure is obsolete. Active polling re-arms 20–26 s STUN + 3 s heartbeats. | `--no-logs-no-support` (kills logtail uploads); 24 h rmnet-counter measurement in checklist; don't run exit-node/subnet bulk traffic on the metered link. |
| **/data flash wear** | `tailscaled.state` (~1–2 KB) rewritten only on netmap/key changes; but statedir `log1/log2` files are written **even with logging disabled** (#9549). eMMC, not NOR — wear is minor; filling /data is the real risk. | `--state` on /data, `--statedir` on /tmp (unless serve TLS needs persistent certs); agent monitors `df /data`. |
| **Clock skew at boot (no RTC)** | Control TLS fails "certificate not yet valid"; #11518 documents the Tailscale-DNS + broken-clock deadlock. | Gate spawn on sane year/NTP; `--accept-dns=false` always; carrier NITZ time as sanity source. |
| **tailscaled OOM** | 60–75 MB RSS idle; >85 MB under bulk (128 MB devices die: #7272; even a 512 MB router died at 32 MB/s sustained: GuNanOvO#17). GOMEMLIMIT is soft (NanoKVM#660: killed despite it). | 1.6 GB RAM makes this a non-issue for management traffic; `GOGC=25 GOMEMLIMIT=192MiB` as hygiene; sidecar isolation means an OOM kills tailscaled, not the agent; supervisor respawns. |
| **CLI JSON / interface drift** | `--json` officially "subject to change" but frozen-in-practice (#17619); top-level `ipnstate.Status` fields identical v1.90.0→v1.98.8; LocalAPI "v0 … not necessarily stable". `tailscale debug` namespace explicitly unstable. | Pin the binary version; parse only established top-level fields, serde ignore-unknown; upgrades are deliberate (test rebind/restun still exist post-upgrade before relying on them). |
| **CGNAT 100.64/10 conflict** (kernel mode only) | tailscaled inserts `! -i tailscale0 -s 100.64.0.0/10 -j DROP` at **position 1 of filter/INPUT** (both backends) — ahead of ESTABLISHED accepts; WAN 100.119.13.253 is in-range (#12829 "can brick it", #18762). `--netfilter-mode=off` is NOT zero-state: still installs pref-5210/5230/5250/5270 ip rules + table-52 /32 routes. | Ship userspace mode (immune by construction). Kernel mode later only with nodeAttr `disable-linux-cgnat-drop-rule` (≥v1.98.2) or `disable-ipv4` (IPv6-only tailnet addressing), tested with console access. |
| **Enrollment footgun** | Re-passing an expired `--auth-key` at boot blocks an authenticated node (#16987 open; #19501 dup). | `--auth-key` only in `POST /setup` when no valid state; boot = spawn only, no `up`. |
| **Dropbear unreachable via loopback redial** | Netstack redials 127.0.0.1; a `192.168.0.1:2222`-only bind refuses ("could not connect to local backend server", #13931's error path). | Checklist verifies bind; fallbacks: loopback bind, subnet route, or Tailscale SSH. |
| **UPX-packed binaries** | Open segfault when packed daemon+CLI run concurrently (#8613); decompresses into non-evictable anon RAM. | Use official/stripped binaries; store xz on /data if flash-tight. |
| **Sleeping node deleted from tailnet** | Ephemeral nodes are GC'd ~30–60 min offline. | Never use ephemeral keys for this device (OAuth-minted keys default ephemeral — force `ephemeral=false`). |

---

## 7. Open Questions / On-Device Checklist

Every remaining unknown, with the exact command (SSH `root@192.168.0.1 -p 2222`). Items 1–10 are pre-install probes; 11+ require the pushed binaries. Copy-paste commands are in **Appendix A** below. Summary of what each settles:

1. RAM/disk headroom (`/proc/meminfo`, `df /data /tmp`) — confirms README's 1.6 GB and unknown /data free space.
2. Kernel TUN + WireGuard (`/dev/net/tun`, `/proc/misc`, `ip tuntap add`, `/proc/config.gz`) — decides the kernel-mode upgrade path.
3. Dropbear bind address — decides SSH exposure strategy.
4. Loopback dial test for 2222/9090 — simulates exactly what netstack does.
5. Static-binary execution gate (`tailscaled --version` on-device) — closes the last musl/vendor-kernel gap.
6. First login (`tailscale up` → URL printed) — proves control-plane TLS from this network.
7. Zero-OS-mutation proof: iptables/nft/ip-rule/route/resolv.conf diff across userspace tailscaled start — validates "safe by construction".
8. QCMAP canary: custom chains + data-call bounce diff — validates the "no clobber" verification for ZTE's build specifically.
9. `iptables -V` backend (legacy vs nf_tables) — constrains future kernel-mode firewall-mode choice.
10. Sleep config, wakelock API behavior, suspend statistics — validates the wakelock design.
11. Autosleep reachability A/B (with/without wakelock; does inbound DERP wake the AP?) — the go/no-go for the wakelock-optional mode.
12. WAN-flap recovery timing; whether `debug rebind`/`restun` suffices vs restart.
13. `tailscale netcheck` — NAT hardness, DERP home region, IPv6 direct-path viability.
14. 24 h idle rmnet byte counters — the real metered-cost number.
15. Current WAN IP still in 100.64/10; which 100.64/10 endpoints the router actually talks to (DROP-rule blast radius if kernel mode is ever used).
16. Clock/NTP behavior after reboot — sizing the startup gate.
17. Tailscale SSH shell spawn under busybox (if that feature is wanted).

---

## 8. Sources

**Verified locally (2026-07-17):** `pkgs.tailscale.com/stable/tailscale_1.98.9_arm64.tgz` (34,254,455 B; ELF inspection: no PT_INTERP/PT_DYNAMIC, CGO_ENABLED=0, netgo/osusergo, GOARM64=v8.0); self-built multicall v1.98.9 = 25,362,594 B.

**Tailscale docs:** kb/1112+concepts/userspace-networking · docs/reference/faq/other-vpns (inbound→127.0.0.1 wording) · kb/1207 small-tailscale · kb/1278 tailscaled flags · kb/1085 auth-keys · kb/1028 key-expiry · blog/tagged-key-expiry · kb/1215 oauth-clients · kb/1241 tailscale-up · kb/1242+1312 serve · kb/1223 funnel · kb/1193 tailscale-ssh · kb/1019 subnets · docs/reference/netfilter-modes · docs/features/firewall-mode · docs/reference/troubleshooting/network-configuration/cgnat-conflicts · docs/features/client/update · docs/features/logging · docs/reference/connection-types · blog/pricing-v4 · blog/community-projects ("never break any client") · blog/tailscale-rs-rust-tsnet-library-preview.

**Tailscale source (github.com/tailscale/tailscale):** `util/linuxfw/iptables_runner.go` (CGNAT DROP @ INPUT pos 1) · `wgengine/router/osrouter/router_linux.go` (ip rules 5210–5270 survive netfilter-off) · `cmd/tailscaled/tailscaled.go` (onlyNetstack: no router/DNS) · `wgengine/netstack/netstack.go` (loopback redial) · `net/netmon/netmon.go` (time-jump wake detection; 10-min threshold) · `ipn/localapi/localapi.go` · `ipn/ipnstate/ipnstate.go` (KeyExpiry omitempty) · `ipn/backend.go` (Notify masks) · `cmd/tailscale/cli/up.go` (`up --json` AuthURL) · `tailcfg/tailcfg.go` (disable-linux-cgnat-drop-rule, capver 136) · `net/tlsdial/tlsdial.go` (baked-in LE roots).

**Issues (state as of 2026-07-17):** #16987 open + #19501 dup (auth-key boot footgun) · #12829/#18762 (CGNAT DROP) · #10688/#19199+PR#19200/#7342/#13379 (reconnect gaps) · #17677 fixed 1.90.4 · #2642 fixed v1.20.2 (ECN) · #5870/#7848 (loopback source IP) · #13931 (local backend refused) · #7272/#18013/#16258 (RSS/OOM) · #9549 (statedir logs) · #9382 (state corruption) · #18677 (spurious logout) · #11348/#10632 (update on RO rootfs) · #11518 (clock/DNS deadlock) · #8613 (UPX) · #12125 (multicall ssh) · #5424 (no rule re-install) · #17619 (JSON versioning) · #15002 (BrowseToURL) · #604/#6148 + PR #1175 (idle STUN stop) · golang/go#62556 · rust-lang/rust#97117.

**Kernel provenance:** git.codelinaro.org clo/kernel-platform-2-0-os/kernel/msm-5.15 — `build.config.msm.sdxpinn` (PREFERRED_USERSPACE=owrt), `arch/arm64/configs/generic_csm_defconfig` (CONFIG_TUN=y, CONFIG_WIREGUARD=y), `arch/arm64/configs/vendor/sdxpinn.config`.

**Prior art:** docs.gl-inet.com Tailscale guide · github.com/Admonstrator/glinet-tailscale-updater · github.com/RemoteToHome-io/gl-tailscale-fix · github.com/hassio-addons/app-tailscale · github.com/GuNanOvO/openwrt-tailscale (+issue #17) · github.com/iamromulan/quectel-rgmii-toolkit (Tailscale on SDXPINN/SDX75) · QCMAP source via github.com/randcd-APY/QuectelShare (`QCMAP_NATALG.cpp`, `QCMAP_Backhaul_WWAN.cpp`) · openwrt/packages#19774/#23480/#26761 · sipeed/NanoKVM#660.

**In-repo:** `CLAUDE.md`, `README.md` (1.6 GB RAM, 8 GB eMMC, ~225 MB ZTE stack), `probe_results_2026-04-11.md` (sleep subsystem), `device_report.json`, `zte-agent/src/{main,server,handlers,ubus,event_bus,device_ext}.rs`, `zte-agent/src/doh/{mod,config}.rs`, `setup.sh`, `deploy.sh`, `scripts/check-reboot.sh`.
---

## Appendix A — On-Device Probe Commands (copy-paste)

Run over SSH (`ssh -p 2222 root@192.168.0.1`). Items 1–12 are pre-install; 13+ need the tailscale binaries pushed to `/data/tailscale/bin/`.

**1.**
```sh
head -3 /proc/meminfo; free; df -k /data /tmp /zteoverlay /; cat /proc/$(pidof zte_topsw_daemon)/oom_score_adj 2>/dev/null
```

**2.**
```sh
ls -l /dev/net/tun; grep -w tun /proc/misc; ip tuntap add mode tun name tuntest0 && echo TUN_OK && ip tuntap del mode tun name tuntest0; zcat /proc/config.gz 2>/dev/null | grep -E 'CONFIG_TUN=|CONFIG_WIREGUARD=|CONFIG_NF_TABLES' || echo 'no /proc/config.gz'
```

**3.**
```sh
cat /etc/modules.conf; ls /etc/modules.d/ 2>/dev/null; ls /lib/modules/$(uname -r)/ 2>/dev/null | head
```

**4.**
```sh
netstat -tln 2>/dev/null | grep ':2222'; ps w | grep '[d]ropbear'   # expect 0.0.0.0:2222 or :::2222; '192.168.0.1:2222' alone breaks SSH-over-tailnet loopback redial
```

**5.**
```sh
echo | nc 127.0.0.1 2222   # expect SSH-2.0-dropbear banner (proves loopback dial works, which is what netstack does)
```

**6.**
```sh
wget -qO- http://127.0.0.1:9090/ >/dev/null 2>&1; echo agent_loopback_rc=$?   # any HTTP response (even 401/404) proves the 9090 listener answers on loopback
```

**7.**
```sh
iptables -V; which iptables iptables-legacy nft; cat /etc/resolv.conf; ls /etc/ssl/certs 2>/dev/null | head -3
```

**8.**
```sh
ip -4 addr show dev rmnet_data0; ip -6 addr show dev rmnet_data0; ubus call zwrt_data get_wwaniface '{"source_module":"zte_topsw_data","cid":1}'   # confirm WAN IP still in 100.64/10 and IPv6 global present
```

**9.**
```sh
ip rule list; ip route show table all | head -30; netstat -rn   # baseline policy routing before any tailscale experiment
```

**10.**
```sh
uci show zwrt_sleep; cat /sys/power/autosleep; cat /sys/power/wake_lock 2>/dev/null; cat /sys/power/suspend_stats/success 2>/dev/null
```

**11.**
```sh
ubus call zwrt_zte_sleep_faw.wakelock createWakelock '{}'; cat /sys/power/wake_lock; ubus call zwrt_zte_sleep_faw.wakelock destroyWakelock '{}'   # verify wakelock API behaves as documented
```

**12.**
```sh
date -u; ps w | grep '[n]tpd'; uci show system 2>/dev/null | grep -i ntp   # who syncs the clock, and how fast after boot (rerun right after a reboot)
```

**13.**
```sh
# after: scp -P 2222 tailscaled tailscale root@192.168.0.1:/data/tailscale/bin/ ; then:
```

**14.**
```sh
chmod +x /data/tailscale/bin/tailscaled /data/tailscale/bin/tailscale; /data/tailscale/bin/tailscaled --version; echo exec_rc=$?   # Gate 1: static binary executes on the vendor kernel
```

**15.**
```sh
iptables-save >/tmp/ipt.b; ip6tables-save >/tmp/ipt6.b 2>/dev/null; nft list ruleset >/tmp/nft.b 2>/dev/null; ip rule show >/tmp/rule.b; ip route show table all >/tmp/rt.b; cp /etc/resolv.conf /tmp/resolv.b   # snapshot BEFORE starting tailscaled
```

**16.**
```sh
mkdir -p /tmp/tailscale /data/tailscale && /data/tailscale/bin/tailscaled --tun=userspace-networking --state=/data/tailscale/tailscaled.state --statedir=/tmp/tailscale --socket=/tmp/tailscale/tailscaled.sock --no-logs-no-support >/tmp/tailscaled.log 2>&1 & sleep 10; /data/tailscale/bin/tailscale --socket=/tmp/tailscale/tailscaled.sock up --hostname=u60-test --timeout=60s   # Gate 2: prints a login URL => control-plane TLS handshake works from this network
```

**17.**
```sh
iptables-save >/tmp/ipt.a; ip6tables-save >/tmp/ipt6.a 2>/dev/null; nft list ruleset >/tmp/nft.a 2>/dev/null; ip rule show >/tmp/rule.a; ip route show table all >/tmp/rt.a; for f in ipt ipt6 nft rule rt; do echo "== $f =="; diff /tmp/$f.b /tmp/$f.a; done; diff /tmp/resolv.b /etc/resolv.conf   # ALL diffs must be empty => userspace mode is zero-OS-mutation on this firmware
```

**18.**
```sh
/data/tailscale/bin/tailscale --socket=/tmp/tailscale/tailscaled.sock netcheck   # NAT hardness, DERP home region, UDP blocked?, IPv6 direct-path viability
```

**19.**
```sh
# from a phone on the tailnet after enrolling: open http://<100.x.y.z>:9090 and ssh -p 2222 root@<100.x.y.z> ; record whether connection is 'direct' or 'via DERP(...)' in: /data/tailscale/bin/tailscale --socket=/tmp/tailscale/tailscaled.sock status
```

**20.**
```sh
iptables -N ts-canary 2>/dev/null; iptables -A ts-canary -j RETURN; iptables -I INPUT 1 -j ts-canary; iptables -t nat -N ts-canary-post 2>/dev/null; iptables -t nat -A POSTROUTING -j ts-canary-post; iptables-save >/tmp/fw.b; ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":1,"enable":0,"sub_id":1}'; sleep 15; ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":1,"enable":1,"sub_id":1}'; sleep 20; iptables-save >/tmp/fw.a; diff /tmp/fw.b /tmp/fw.a   # QCMAP clobber test: canary chains must survive the data-call bounce (run from LAN, not remotely). Cleanup: iptables -D INPUT -j ts-canary; iptables -t nat -D POSTROUTING -j ts-canary-post; iptables -X ts-canary; iptables -t nat -X ts-canary-post
```

**21.**
```sh
S0=$(cat /sys/power/suspend_stats/success 2>/dev/null); echo mem > /sys/power/autosleep; sleep 600; S1=$(cat /sys/power/suspend_stats/success 2>/dev/null); echo "suspends in 10min: $((S1-S0))"; dmesg | grep -iE 'PM: suspend (entry|exit)' | tail; echo off > /sys/power/autosleep   # while a tailnet peer runs: ping -i 1 <router-ts-ip> | ts  -- does the node go offline during suspend, and does inbound DERP traffic wake the AP?
```

**22.**
```sh
ubus call zwrt_zte_sleep_faw.wakelock createWakelock '{}'; S0=$(cat /sys/power/suspend_stats/success 2>/dev/null); echo mem > /sys/power/autosleep; sleep 600; S1=$(cat /sys/power/suspend_stats/success 2>/dev/null); echo "suspends WITH wakelock: $((S1-S0))"; echo off > /sys/power/autosleep; ubus call zwrt_zte_sleep_faw.wakelock destroyWakelock '{}'   # expect 0 suspends and zero ping gaps from the peer
```

**23.**
```sh
ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":1,"enable":0,"sub_id":1}'; sleep 10; ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":1,"enable":1,"sub_id":1}'; date +%T; until /data/tailscale/bin/tailscale --socket=/tmp/tailscale/tailscaled.sock status --json --peers=false | grep -q '"Online": *true'; do sleep 2; done; date +%T   # WAN-flap self-heal time; if stuck >120s try: /data/tailscale/bin/tailscale --socket=/tmp/tailscale/tailscaled.sock debug rebind && ... debug restun, and record which level fixes it
```

**24.**
```sh
R0=$(cat /sys/class/net/rmnet_data0/statistics/rx_bytes); T0=$(cat /sys/class/net/rmnet_data0/statistics/tx_bytes); date; sleep 86400; R1=$(cat /sys/class/net/rmnet_data0/statistics/rx_bytes); T1=$(cat /sys/class/net/rmnet_data0/statistics/tx_bytes); date; echo "24h idle delta: rx=$(( (R1-R0)/1024 ))KiB tx=$(( (T1-T0)/1024 ))KiB"   # run once with tailscaled idle+enrolled and once without, subtract; expect ~1-5 MB/day attributable to tailscaled
```
