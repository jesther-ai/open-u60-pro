# CLAUDE.md — ZTE U60 Pro Project Guidelines

## Device Overview
- **Model**: ZTE U60 Pro 5G CPE router
- **OS**: OpenWrt 23.05.4 on Qualcomm SDX75 (aarch64, musl libc)
- **SSH**: Port 2222 at 192.168.0.1
- **Filesystem**: Read-only rootfs, writable overlay at `/zteoverlay/`, writable `/data`

## Architecture
- `zte-agent/` — Rust agent on the device, port 9090 (LAN, plus the tailnet when Tailscale is on): JSON API under `/api/` for the apps, browser UI under `/ui/` (`/` redirects there)
  - HTTP (`src/httpd/`): axum on hyper http1, one current-thread tokio runtime on the main thread. There is no tiny_http and there are no `http-N` worker threads. The accept loop itself refuses connections from the mobile-data side (`httpd::mobile`, owner decision 53): only loopback (Tailscale), the router's own addresses and peers whose most specific local subnet belongs to a non-`rmnet*` interface are served. The interface table is read on the blocking pool (awaited at most 0.5 s); a failed lookup keeps the last table 10 min, then serves everything; `ZTE_AGENT_ALLOW_MOBILE=1` turns it off.
  - LAN HTTPS (`src/tls/`, `httpd::tls`, owner decision 59): off until the owner turns it on in Settings; the router's own CA, name-constrained to the LAN address at Turn On (/32) and `u60.home.arpa` only (root key dropped after signing one issuing CA with the same constraints; files in `/data/zte-agent-tls/`, dir 0700, keys 0600, never logged/rendered/served), HTTPS on `0.0.0.0:9443` (`ZTE_AGENT_TLS_BIND`) with the same router, caps (shared) and mobile check as :9090, `__Host-` cookies over HTTPS, `u60.home.arpa` via `/tmp/hosts/zte-agent` + SIGHUP to dnsmasq and a Host only over HTTPS; Reset ends open HTTPS connections; no timers (issue again at start, on a TLS connection, after a LAN apply; renew automatically only on the verified clock, which needs a Live Activity provider; else Settings › Renew); no HSTS.
  - Never run blocking work (ubus, uci, AT, APDU, blocking HTTP) on the runtime thread. `/api` calls go `server::precheck` → `httpd::gate` (General lane = `ZTE_AGENT_THREADS`, default 4, clamped 1–32; Slow lane 2; 7 s wait → 503 `agent busy, retry`) → `server::route()` on the blocking pool. Debug builds panic on a device call from the runtime thread. Put new long-running routes in `server::lane()` as Slow.
  - Auth (`src/auth/`): random 64-hex bearer tokens (1 h idle / 24 h max) and browser sessions (12 h / 7 d), expiry on CLOCK_BOOTTIME, login rate limit (429 + Retry-After; loopback = all Tailscale clients, blocks capped at 10 s). Never log passwords, tokens or cookies. Automation over ADB uses the per-start local token in `/tmp/zte-agent-run/admin-token` (0600, loopback peers only, so through `adb forward`; no event streams; never print it), see Basic Debugging.
  - Live data (`src/events/`): event hub plus a demand-driven sampler (`hub-fast`, `hub-ubus`) feeding `GET /api/events` (`zte-agent/docs/events.md`) and the UI's live pages. Nothing may be sampled or polled while no stream is open.
  - Web UI (`src/web/`, `zte-agent/docs/web-ui.md`): maud pages that work without JavaScript; never use `PreEscaped` (a test checks). Datastar 1.0.4 is vendored and SHA-pinned in `build.rs` (keep `.gitattributes`); the only other script is `assets/theme.js` (System theme only, owner decision 60). `app.css` ≤ 48 KiB (a test checks). Never render IMEI, IMSI or a full ICCID (last 4 at most). The authenticated Wi-Fi connected-device page and its live fragment show full MACs and hostnames (owner choice); other device lists mask MACs to the last 2 octets and mask MAC-derived hostnames. Fixed client IP reservations show full MACs for binding verification. SMS numbers and text only on authenticated SMS pages, never in `<title>`, URLs, `next`, logs or fragment ids. Never render Wi-Fi, guest Wi-Fi or APN passwords, or forwarding secrets, except the Wi-Fi password on the no-store Share Wi-Fi page.
  - Measure the release binary for every change: `aarch64-linux-musl-size -A` Total and gzip size.
- `mobile/ios/` — SwiftUI companion app
- `mobile/android/` — Jetpack Compose companion app
- `web/` — Next.js landing page, docs and browser installer (open-u60-pro.vercel.app); not the on-router web UI

---

## Deployment

Run commands from the repository root (`open-u60-pro/`). Use `setup.sh` for
normal deployments; `deploy.sh` is the older SSH/password-based path.
Prerequisites: Rust, the `aarch64-unknown-linux-musl` target, `adb`, and
`aarch64-linux-musl-gcc` on `PATH`.

```sh
adb devices -l
export ANDROID_SERIAL="<serial from adb devices -l>"
./setup.sh --build --adb --serial "$ANDROID_SERIAL" --factory-password --no-ssh
```

This builds the current checkout as a release, installs `/data/zte-agent`, and
starts it through `/data/local/tmp/start_zte_agent.sh`. Factory-password mode
uses the official router admin password and requires already-authorized ADB.
The management page is `http://192.168.0.1:9090/ui/`.

On this development Mac, an alternative musl toolchain uses existing Zig
wrappers. If using them, first confirm the files exist, then set these variables
before running the build or installer; `/tmp` paths are not permanent:

```sh
export PATH="/tmp/u60-cross-tools:$PATH"
export CC_aarch64_unknown_linux_musl=/tmp/u60-cross-tools/aarch64-linux-musl-gcc
export AR_aarch64_unknown_linux_musl=/tmp/u60-cross-tools/aarch64-linux-musl-ar
```

To compile without deploying:

```sh
cargo build --locked --release --target aarch64-unknown-linux-musl -p zte-agent
```

## Basic Debugging

Use the same `ANDROID_SERIAL` when inspecting the device. Read agent output,
identify the running executable, or inspect recent system logs:

```sh
adb shell 'tail -n 100 /tmp/zte-agent.log'
adb shell 'p=$(cat /tmp/zte-agent-run/pid); readlink "/proc/$p/exe"'
adb shell 'logread | tail -n 100'
```

Use `adb shell 'tail -f /tmp/zte-agent.log'` for live agent output; Ctrl-C ends
the log stream. To call the API without the admin password (the agent accepts
its local token only over loopback, which an ADB forward is):

```sh
adb forward tcp:18090 tcp:9090
TOKEN=$(adb shell cat /tmp/zte-agent-run/admin-token | tr -d '\r\n')
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:18090/api/wifi-relay/status
```

Run unit tests on the development computer, not the router:

```sh
cargo test --locked -p zte-agent
cargo test --locked -p zte-agent wifi_relay:: -- --nocapture
```

Replace `wifi_relay::` with the module or test name being investigated.

---

## CRITICAL: ZTE Daemon Sync Barrier

`zte_topsw_daemon` is the master daemon. It reads `/etc/config/zte_topsw_daemon.conf` and **waits for ALL listed daemons to register** before releasing the boot sequence.

### If you disable a daemon listed in daemon.conf via init.d, the device will:
- **Display stuck on ZTE boot logo** (UI never renders)
- **WAN never connects** (mobile data call never initiated)
- **Touchscreen unresponsive** (mtdev2tuio bridge never starts)

All other daemons appear to run fine — making the root cause very hard to diagnose.

### Daemons in daemon.conf (NEVER disable via init.d):
```
zte_topsw_mc, zte_router, zte_topsw_data, zte_topsw_nwinfo,
zte_topsw_mdm, zte_topsw_sleep_faw, zte_topsw_apn, zte_topsw_wms,
zte_topsw_key, zte_topsw_led, zte_topsw_tr098db, zte_dm,
zte_topsw_fota_result, zte_topsw_devui, zte_topsw_wlan, zte_smart_manage
```

### Safe to disable via init.d (NOT in daemon.conf):
```
zte_topsw_diag, zte_topsw_samba, zte_topsw_nfc, zte_topsw_get_brand,
zte_topsw_jwxk_query, zte_topsw_tr069_sub, zte_mqtt_sdk_st,
zte_topsw_dua, zte-topsw-tunnel
```

### How to properly disable a daemon.conf daemon:
Comment it out in `/etc/config/zte_topsw_daemon.conf` (prefix with `#`). Do NOT use `/etc/init.d/<name> disable`.

### Overlay Whiteouts
`/etc/init.d/<name> disable` creates **whiteout character devices** in `/zteoverlay/etc-upper_a/rc.d/` that silently delete the ROM symlinks. These are invisible in normal `ls /etc/rc.d/` but persist across reboots.

- **Check**: `ls -la /zteoverlay/etc-upper_a/rc.d/ | grep '^c'`
- **Fix**: `rm /zteoverlay/etc-upper_a/rc.d/<whiteout_file>`

### Verify sync status after boot:
```sh
ubus call zwrt_topsw_daemon.sync get_sync_info '{}'
# Should return: "noSyncModuleName": "sync success"
```

---

## Recovery Commands

### Display stuck on logo
```sh
# Start touchscreen driver + bridge
sh /usr/bin/mtdev2tuio.sh
# Restart UI daemon
kill -9 $(pidof zte_topsw_devui); /usr/bin/zte_topsw_devui &
```

### WAN not connecting
```sh
# Enable IPv4 data call
ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":1,"enable":1,"sub_id":1}'
# Enable IPv6
ubus call zwrt_qcmap_cli set_qcliiface '{"source_module":"zte_topsw_data","type":2,"enable":1,"sub_id":1}'
```

### Check data call status
```sh
ubus call zwrt_data get_wwaniface '{"source_module":"zte_topsw_data","cid":1}'
# Look for: "enable": 1, "connect_status": "connected"
```

---

## Other Device Notes

- **Airplane mode bug**: `nwinfo_set_mode ONLINE` does NOT recover modem from LPM. Only fix: reboot.
- **Charge policy bug**: Wall mode `enable` STOPS charging, `disable` STARTS charging (inverted).
- **procd respawn**: `kill -9` may trigger procd respawn. Use `/etc/init.d/<name> stop` instead.
- **Touchscreen**: Sitronix at I2C `1-0055`, kernel module `sitronix-ts.ko` loaded by `/usr/bin/mtdev2tuio.sh`.
- **eSIM**: No built-in eUICC. The user-tested 9eSIM B1 in the SIM slot is managed by the agent in-process (`euicc/` crate, a Rust translation of lpac's libeuicc, over QMI-over-QRTR, node 3, slot 1): list, switch, download, delete — see `zte-agent/docs/esim.md`. lpac itself is no longer used (removed 2026-10-19; the old `/data/lpac` bundle is archived on the router as `/data/lpac.removed`). The old phone-Bluetooth B1 code was removed. Do not reverse-engineer the qcril/RIL internals to forward SIM Toolkit screens.
- **IMEI**: Hardware-locked (QFPROM fused), not modifiable.
- **5 GHz DFS window**: after each 160 MHz 5 GHz start, `/sbin/zte_wifi_dfs_hdlr.sh` sets `iwpriv wlan2 disableDfsCS 1` for 599 s. Any 160 MHz AP here spans DFS, and a STA connect in that window wedges the AP in `Channel switch in progress` until a hotspot restart. The relay turns the switch back on before each join (owner decision 62, `wifi_relay/dfs_switch.rs`; `zte-agent/docs/network-extensions.md`).
- **`iwpriv u60sta0 chwidth` is not a held setting**: a narrower width lapses silently after about 100-260 s (5 GHz downlink either goes back to wide or collapses, e.g. 80 MHz fell from about 310 to 45 Mbit/s at 190 s; 2.4 GHz 20 MHz collapses to about 3 Mbit/s) while `iw` still reads the narrow width. Re-sending the same value does nothing; set another width, then the target. The setting also survives relay reconnects, interface re-creation and band changes, so a fresh connection can inherit it (`zte-agent/docs/network-extensions.md`, stage B).
- **A width readback does not prove the data path**: after an `iwpriv u60sta0 chwidth` change, `iw` can read the new width while the upstream gateway stops answering ping (seen after narrowing 40→20 at connect and after a rollback; it lasted about 60 s, until the next refresh). `arping` still gets replies in that state, so check with ICMP. Setting the wider width for 1 s and then the target recovers it; the width tuner checks after each narrowing, refresh and rollback (`zte-agent/docs/network-extensions.md`, health probes).
- **The STA width cannot be capped at association**: the vendor `wpa_supplicant` has no HT/VHT overrides, and an upstream 2.11 build that has them was tested: it sent `disable_ht40=1` (HT40 bit cleared in the connect request) and qcacld still associated at 40 MHz. Upstream hostap also lacks the vendor `DRIVER` private commands (`SETROAMMODE`) the relay uses, so do not swap the system supplicant. Debugfs `ll_stats` per-rate counters are TX-only (RX is all zero), so the AP's real downlink width cannot be read either (`zte-agent/docs/network-extensions.md`, driver limits).
