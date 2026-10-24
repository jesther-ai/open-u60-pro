<div align="center">

<p>
<img src="mobile/ios/screenshots/dashboard.png" alt="Dashboard" width="250">&nbsp;&nbsp;<img src="mobile/ios/screenshots/connection-signal.png" alt="Connection and signal" width="250">&nbsp;&nbsp;<img src="mobile/ios/screenshots/esim-profiles.png" alt="eSIM profiles" width="250">
</p>

# ZTE U60 Pro Toolkit

**Unlock the full potential of your ZTE U60 Pro (MU5250) 5G mobile router.**

On-device agent with a REST API, live updates and a built-in web UI, plus native mobile
companion apps, for signal monitoring, band locking, config backup/decryption, network
customization, and more.

[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/device-ZTE%20U60%20Pro%20(MU5250)-orange.svg)]()
[![Qualcomm](https://img.shields.io/badge/chipset-Snapdragon%20X75-red.svg)]()
[![OS](https://img.shields.io/badge/firmware-ZWRT%20(OpenWrt%2023.05)-green.svg)]()
[![Website](https://img.shields.io/badge/website-open--u60--pro.vercel.app-blue.svg)](https://open-u60-pro.vercel.app)
[![Release](https://img.shields.io/github/v/release/jesther-ai/open-u60-pro)](https://github.com/jesther-ai/open-u60-pro/releases/latest)

**Download:** [latest release](https://github.com/jesther-ai/open-u60-pro/releases/latest) · **What's new:** [CHANGELOG.md](CHANGELOG.md) · **Screenshots:** [by feature](#screenshots) · **Web UI:** [in your browser](#web-ui)

</div>

---

## Device

| | |
|---|---|
| **Model** | ZTE U60 Pro (MU5250) |
| **Hardware** | `MU5250_HW1.0` |
| **Firmware** | `CN_ZTE_MU5250V1.0.0B27` (Oct 31, 2025) |
| **Chipset** | Qualcomm Snapdragon X75 (SDX75 / SDXPINN) |
| **CPU** | 4x Cortex-A55 (ARMv8.2-A) @ 2.2 GHz |
| **RAM** | 1.6 GB |
| **Storage** | 8 GB eMMC (Longsys JS08AC), 69 partitions, A/B slots |
| **Modem** | 5G-A Sub-6 + mmWave, Cat 22 LTE |
| **NR Bands** | n1/2/3/5/7/8/18/20/26/28/29/38/40/41/48/66/71/75/77/78/79 |
| **LTE Bands** | 1/2/3/4/5/7/8/18/19/20/26/28/29/32/34/38/39/40/41/42/43/48/66/71 |
| **WiFi** | WiFi 7 (802.11be), 2x2 MIMO, EHT160 |
| **WiFi Chipset** | Qualcomm WCN7851 (`qcacld32`) |
| **WiFi Radios** | 2.4 GHz (ch 1-13, EHT40, 19 dBm) · 5 GHz (ch 36-165, EHT160, 18 dBm) |
| **Battery** | 10,000 mAh Li-ion, 4.5V max, PM7550B fuel gauge |
| **Charging** | USB-PD, 15W (5V/3A), fast charge |
| **USB** | USB-C (PD sink, OTG/powerbank) |
| **Display** | 3.5" IPS LCD (Sitronix ST77926), 320x480, RGB565, DRM/KMS |
| **UI Toolkit** | LVGL with FreeType + LodePNG, assets at `/usr/ui/` |
| **Backlight** | AWINIC AW9523B (I2C `1-005b`), sysfs `/sys/class/leds/led:lcd/brightness` (0-255) |
| **Touch** | Sitronix (I2C `1-0055`) -> `/dev/input/event3` |
| **OS** | ZWRT (OpenWrt 23.05.4 r24012-d8dd03c46f) |
| **Kernel** | Linux 5.15.170-perf, SMP PREEMPT, aarch64 |
| **PMICs** | PMX75 + PM7550BA + PMG1110 |
| **SIM** | Single nano-SIM (no built-in eSIM; eSIM cards such as the 9eSIM B1 work in the slot) |
| **NFC** | Quick device pairing |
| **Bluetooth** | Qualcomm WCN7850 (BT 5.3+), UART transport — disabled (services stopped, module unloaded) |
| **Clients** | Up to 64 (32 per radio) |

## What's Included

### On-Device Agent (`zte-agent`)

A lightweight Rust HTTP server that runs directly on the router (port 9090, on the router's network and, if enabled, your tailnet). It turns ubus calls, AT commands and sysfs reads into a typed REST API for the mobile apps, streams live updates (`GET /api/events`, see [events.md](zte-agent/docs/events.md)), and serves a browser UI at `http://192.168.0.1:9090/` (see [Web UI](#web-ui)).

```bash
# Cross-compile for the device
cargo build --release --target aarch64-unknown-linux-musl -p zte-agent
```

**183 endpoints across 22 categories:**

| Category | # | Capabilities |
|---|---|---|
| Auth | 1 | Log in with the agent password (bearer token; rate limited) |
| Device | 18 | System info, battery, CPU, memory, thermal, charger, charge control, reboot, factory reset, power save, fast boot, auto sleep |
| Network | 9 | Signal strength (RSRP/SINR/RSRQ), speed, traffic stats, WAN/LAN status, rmnet, connected clients |
| Modem | 16 | Airplane mode, mobile data toggle, network mode (2G-5G), operator scan, manual registration, data usage, no-service and connection-watchdog policies |
| SMS | 5 | List, send, delete, mark read, storage capacity |
| SIM | 7 | SIM info, IMEI, PIN management, PUK unlock |
| Cell/Band | 19 | NR/LTE band locking, cell locking, neighbor scan, STC, signal quality detection |
| Router | 29 | DNS, LAN/DHCP, firewall, NAT, DMZ, UPnP, port forwarding, QoS, domain filter, APN profiles, VPN/ALG |
| WiFi | 4 | Status, SSID/password/channel/TX power, guest WiFi |
| USB | 3 | USB mode switching, powerbank control |
| Telephony | 15 | Voice calls (dial/hangup/answer/DTMF/mute), USSD codes, SIM Toolkit menus |
| eSIM | 13 | eSIM profiles (built-in SGP.22 LPA): list, switch (verified card → modem → data), rename, download from QR with preview/cancel, delete, operator receipts, card info |
| Speed Test | 4 | Server list, run test, progress tracking |
| DoH Proxy | 6 | DNS-over-HTTPS proxy, cache management |
| LAN Test | 3 | WiFi ping, download/upload throughput measurement |
| SMS Forwarding | 10 | Auto-forward rules, CRUD, test, log management, retry held forwards |
| System | 2 | Process list, kill bloatware |
| Scheduler | 5 | Scheduled/recurring jobs for any API action |
| Live updates | 1 | `GET /api/events`: Server-Sent Events of signal, traffic, battery, SIM, eSIM progress and more |
| Live Activity | 3 | Register/unregister the iOS Live Activity, status |
| Tailscale | 8 | Remote management: setup, login URL, enable/disable, config, logout, update |
| AT Terminal | 2 | Send AT commands, port info |

### Web UI (built into the agent)

Open `http://192.168.0.1:9090/` on the router's Wi-Fi for a dashboard, connection & signal, and the most-used settings, with nothing to install. See [Web UI](#web-ui).

### Mobile Companion Apps

Native apps that connect directly over WiFi -- no computer needed.

| | iOS | Android |
|---|---|---|
| **Framework** | SwiftUI | Jetpack Compose |
| **Min Version** | iOS 16.0 | Android 8.0 (API 26) |
| **Dependencies** | None (Apple frameworks only) | OkHttp, Hilt, Vico, kotlinx.serialization |
| **Features** | BandLock, Call, Clients, Config, Dashboard, DeviceInfo, Login, RouterSettings, Scheduler, Signal, SIM/STK/USSD, SMS, Tools, USBMode | BandLock, Clients, Config, Dashboard, DeviceInfo, Login, RouterSettings, Scheduler, Signal, SIM/STK, SMS, Tools, USBMode |
| **Tabs** | Dashboard, SMS, Tools, Router, Settings | Dashboard, SMS, Tools, Router, Settings |
| **Path** | `mobile/ios/OpenU60/` (109 Swift files) | `mobile/android/OpenU60/` (90 Kotlin files) |

## Screenshots

From the iOS app on iPhone. Personal identifiers (ICCID digits, cell IDs) are masked in these images.

### Dashboard and signal

The Dashboard shows the operator, network type, live traffic, battery, CPU temperature and connected devices. Tap **Connection & signal** for 5G NR and LTE readings (RSRP, RSRQ, SINR, RSSI), band, channel, bandwidth, the operator's speed cap, and a signal history chart.

| Dashboard | Connection & signal |
|:---:|:---:|
| <img src="mobile/ios/screenshots/dashboard.png" alt="Dashboard with operator, 5G NSA, live traffic, battery, CPU temperature, and connected devices" width="220"> | <img src="mobile/ios/screenshots/connection-signal.png" alt="Connection and signal screen with 5G NR and LTE anchor readings, data session, and signal history chart" width="220"> |

### eSIM profiles

With an eSIM card such as the [9eSIM B1](docs/9esim-b1.md) in the SIM slot, **Router → eSIM Profiles** lists the operator profiles on the card. Swipe right on a profile to switch to it; the app shows each step until mobile data is back. Setup: [eSIM profiles](#esim-profiles-optional).

| Profiles on the card | Switching profile |
|:---:|:---:|
| <img src="mobile/ios/screenshots/esim-profiles.png" alt="eSIM Profiles screen listing four operator profiles with the active one marked" width="220"> | <img src="mobile/ios/screenshots/esim-switching.png" alt="Profile switch in progress: switch profile and reload SIM done, connecting to network" width="220"> |

### Battery and power

Battery details, USB-C mode (charge your phone from the router's battery), and device controls: charge limit, power-save mode, fast boot and auto sleep.

| Battery details | USB-C mode | Device controls |
|:---:|:---:|:---:|
| <img src="mobile/ios/screenshots/battery-details.png" alt="Battery details: capacity, charging status, voltage, current, power, temperature, time to full" width="220"> | <img src="mobile/ios/screenshots/usb-mode.png" alt="USB Mode sheet showing USB-C connected with a Fast Charging button" width="220"> | <img src="mobile/ios/screenshots/device-controls.png" alt="Device controls: charge limit at 85 percent, power-save mode, fast boot, auto sleep" width="220"> |

### Lock Screen

Live modem signal on the Lock Screen and in the Dynamic Island: see [iOS Live Activities](#ios-live-activities).

## Why Use This Instead of the Official ZTE App?

The stock ZTE U60 Pro runs **44 proprietary daemons** consuming **225 MB of RAM** just for device management. Many of these are unnecessary (TR-069 remote management, MQTT telemetry, Samba, NFC, diagnostics) and phone home to ZTE servers.

`zte-agent` replaces the need for the official ZTE management app with a single ~5 MB binary:

| | zte-agent | ZTE Official Stack |
|---|---|---|
| **Processes** | 1 | 44 |
| **Memory (RSS)** | ~0.8 MB | 225 MB |
| **Binary size** | ~5 MB (~2.5 MB gzipped) | ~50+ MB combined |
| **Threads** | ~10 | ~130+ |
| **Telemetry** | None | Phones home to `iot.zte.com.cn` etc. |
| **App** | Open-source native iOS/Android | Closed-source, Chinese-only |

The official ZTE stack's top memory consumers:

| Process | RAM | Purpose |
|---------|-----|---------|
| `zte_topsw_devui` | 22.8 MB | LCD UI daemon |
| `zte_topsw_get_brand` | 14.2 MB | Carrier branding |
| `zte_topsw_data` | 10.9 MB | Mobile data management |
| `zte_topsw_tr069` + subs | 23.2 MB | ISP remote management |
| `zte_topsw_diag` | 7.1 MB | Diagnostics/logging |

zte-agent provides equivalent API access to the same underlying ubus/AT services using **0.35% of the memory** -- freeing ~224 MB of RAM for actual routing and WiFi workloads.

## Why a Built-In eSIM Manager Instead of lpac?

The eSIM feature first ran [lpac](https://github.com/estkme-group/lpac), the open-source eSIM manager, as a separate program on the router. To reach the card through the modem, lpac needs its own copy of GLib, libqmi, libqrtr, curl, OpenSSL and their dependencies (31 shared libraries in all), so it had to be cross-built into a 23.5 MB bundle and installed next to the agent. The agent then started a new lpac process for every card command, including every profile list the app asked for.

`zte-agent` now has the same eSIM code built in: lpac's card library translated to Rust ([`euicc/`](euicc/)) plus a small driver that talks to the modem directly. It adds about 130 KB to the agent.

Measured on the router (U60 Pro, 9eSIM B1, 2026-10-19):

| | Built into zte-agent | lpac bundle |
|---|---|---|
| **Install** | Nothing extra | Separate 23.5 MB bundle in `/data/lpac` (own musl loader + 31 shared libraries), built with a 1.4 GB cross-build setup |
| **Size on the router** | ~130 KB inside the agent binary | 23.5 MB |
| **Per card command** | A thread in the agent: ~2–3 ms CPU, no extra memory | A new process: ~20 ms CPU, ~10.7 MB peak memory |
| **`chip info`** | 0.15 s | 0.26 s |
| **`profile list`** | 0.25 s | 0.27 s (the card itself sets the pace) |
| **Operator server TLS** | Checked against the GSMA eSIM root only, host name checked, no redirects or proxy | Not checked by upstream lpac (`CURLOPT_SSL_VERIFYPEER` off); needed a patch |
| **Cancelling a download** | Cancel tells the operator "postponed", so the QR code stays usable; a timeout is reported as a timeout | Every stop is reported as "rejected by the user" |
| **Busy card** | Named (`catBusy`, `undefinedError`), so the agent knows to retry | "unknown" until patched |
| **Stopping a stuck command** | Time limit per step, inside the agent | SIGINT, then SIGKILL after 15 s |
| **License** | Agent MIT; eSIM library LGPL-2.1 | lpac's command-line tool is AGPL-3.0 |

The results are the same as lpac's. On the router, `chip info`, `profile list` and `notification list` gave identical output. About 1 million generated cases matched the C library. A download sends byte-for-byte the same card commands and server requests. Delete, download, switching and receipts have been tested on a real card (see [`zte-agent/docs/esim.md`](zte-agent/docs/esim.md)).

lpac is still the better tool elsewhere. It supports many card readers and modems (PC/SC, AT, MBIM, QMI) and more commands, such as SM-DS discovery, and it gets upstream fixes. The agent covers only what the app needs on this router, through the modem's QMI over QRTR, and is pinned to lpac commit `c7c8d4f`: upstream fixes have to be ported by hand. For a USB card reader on a computer, use lpac.

## Device Impact

> **Warning** -- The agent exposes device control endpoints that modify firmware settings.
> Destructive actions (factory reset, reboot) require confirmation in the mobile app.
> The web UI asks for confirmation before changes that can cut the connection or are destructive,
> and reboot and factory reset also need the agent password.

### Filesystem Layout

```
/data/                              (writable /data partition)
├── zte-agent                       <- on-device REST agent
├── zte-agent-tls/                  <- LAN HTTPS certificates (only once turned on; 0700)
└── local/tmp/
    ├── start_zte_agent.sh          <- agent boot script
    ├── dropbear                    <- SSH binary (if installed)
    └── start_dropbear.sh          <- SSH boot script (if installed)

/etc/                               (read-only rootfs)
├── rc.local                        <- boot hooks appended here
└── dropbear/
    ├── authorized_keys             <- SSH public key (if installed)
    └── dropbear_rsa_host_key       <- generated host key (if installed)
```

## Quick Start

### Prerequisites

```bash
# 1. Install Rust -- see https://rustup.rs
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. Add musl cross-compilation target
rustup target add aarch64-unknown-linux-musl

# 3. Install musl cross-linker
# macOS:
brew install filosottile/musl-cross/musl-cross
# Ubuntu/Debian:
sudo apt install musl-tools gcc-aarch64-linux-gnu

# 4. Install ADB
# macOS:
brew install android-platform-tools
# Ubuntu/Debian:
sudo apt install android-tools-adb

# 5. Build the agent
cargo build --release --target aarch64-unknown-linux-musl -p zte-agent
```

### First-Time Setup

For a factory-fresh device that has never had zte-agent installed:

**Option A -- Browser (no tools needed)**

Visit [open-u60-pro.vercel.app](https://open-u60-pro.vercel.app), enter your router password and desired agent password, connect USB, and click Deploy. Requires Chrome or Edge.

**Option B -- Command line**

```bash
./setup.sh <router-password> <agent-password>
```

Enables ADB, builds and pushes the agent, creates boot scripts, and optionally sets up SSH for wireless deploys.

### Deploy Agent (subsequent updates)

```bash
./deploy.sh yourpassword
```

This builds, deploys the binary to the device via SSH (port 2222), updates the boot password, and restarts the agent.

### eSIM profiles (optional)

With an eSIM card in the SIM slot (tested with a [9eSIM B1](docs/9esim-b1.md)), the agent manages its operator profiles itself, as a GSMA SGP.22 eSIM manager built in (its card code is a Rust translation of [lpac](https://github.com/estkme-group/lpac)'s library); nothing else needs installing ([why not lpac](#why-a-built-in-esim-manager-instead-of-lpac)):

- **List** profiles and card details (EID, firmware, free space).
- **Switch** the active profile. The agent verifies each step (card → modem identity → mobile data) and reports why a switch didn't complete; it never switches back on its own. Internet returns in about 15–45 s; Wi‑Fi stays on.
- **Add** a profile from the operator's QR code or activation code, with a preview of the operator and profile name before anything is written, and cancel.
- **Rename** profiles (nicknames) and **delete** inactive ones.
- **Operator receipts:** the card's add, switch and remove notifications are sent to the operator's server one by one, right after the change once data is up, and retried while the router has mobile data.
- **Safety:**
  - one eSIM operation at a time;
  - checks that it's still the same card before any change;
  - no switching while you have airplane mode on;
  - downloads only with mobile data.
  - operator servers are reached over TLS checked against the GSMA eSIM root certificate that the card trusts.

Switching, adding, renaming and deleting require an agent password. In the apps: Router → eSIM Profiles. For diagnostics on the router, `/data/zte-agent esim-native chip info` (or `profile list`) prints one card command's result; it waits for nothing and answers busy while the agent uses the card. API and behaviour: [`zte-agent/docs/esim.md`](zte-agent/docs/esim.md). An lpac bundle in `/data/lpac` from earlier versions is no longer used and can be deleted.

### Connect Mobile App

1. Connect your phone to the router's WiFi
2. Open OpenU60 app
3. Set agent URL: `http://192.168.0.1:9090`
4. Enter the agent password you set above
5. No app? Open http://192.168.0.1:9090/ in a browser instead (see [Web UI](#web-ui)).

## Web UI

The agent also serves a web app, so any phone or computer on the router's Wi-Fi can check and manage the router without installing anything.

1. Connect to the router's Wi-Fi.
2. Open **http://192.168.0.1:9090/** in a browser.
3. Log in with the **agent password**: the one you gave `setup.sh`, the browser installer or `deploy.sh` (`ZTE_AGENT_PASSWORD` in the agent's start script). Without an agent password the web UI only explains how to set one.

It looks and works like the iOS app, with the same five tabs, screens and words. What stays in the apps is listed [below](#not-in-the-browser-use-the-apps).

- **Dashboard:** operator and network type, signal, live traffic, battery, CPU and battery temperature, Wi-Fi and the number of connected devices. Each card opens its details, as in the app:
  - **Connection & signal:** 5G NR, LTE and WCDMA readings (RSRP, RSRQ, SINR, RSSI, band, channel, bandwidth, carrier aggregation), the data session, a 2-minute signal chart, the WAN addresses and **Network mode**.
  - **Live traffic**, **Battery Details** (capacity, status, voltage, current, power, temperature, time to full or empty) and **CPU & temperatures** (every temperature sensor; **CPU, memory & processes** shows cores, uptime and memory, and the live process list, where you can kill bloat daemons after a confirmation; **Kill All Bloat** leaves the SMS daemon and the boot-critical daemons running).
  - **Wi-Fi & devices:** the Wi-Fi networks and the live list of connected devices (name, IPv4 address and the last 2 octets of the MAC address), and **Share Wi-Fi** with the password (hidden until you ask) and a QR code to join.
  - **USB-C:** while a cable is attached, a card offers **Fast Charging** (charge your phone from the router's battery) or **Dismiss** (until the cable is unplugged).
- **SMS:** your conversations (search, storage filter, unread counts), reading and replying, **New Message** and deleting a conversation (after a confirmation). **SMS Forwarding** (gear icon, also under Tools): turn forwarding on or off, add, edit, test, switch and delete rules (Telegram, webhook, SMS, ntfy, Discord, Slack; saved tokens and webhook URLs are never shown, leave a field empty to keep it), and the **Forward Log** with **Retry** and **Clear**.
- **Tools:**
  - **Automations** (scheduled actions: create, edit, switch on or off, delete) and **SMS Forwarding**.
  - **Speed Test** (asks first: it uses mobile data), **Enable ADB** (asks first), **USB Mode** (the cable's state and the **Fast Charging (Powerbank)** switch).
  - **Band Lock** (NR and LTE bands; asks first), **AT Terminal** (asks before commands that can cut the connection; long digit runs such as an IMEI are masked).
  - **Device Info** (the IMEI shows only its last 4 digits) and **Connected Devices** (with search).
  - The app's placeholders for TTL Settings, Enable SSH and Device Explorer.
- **Router:**
  - **Cellular:** Mobile Network (mobile data, airplane mode, connection mode, data roaming, network selection with a scan and manual registration), Network Mode (Auto, 5G NSA, 5G SA only, 4G only, 4G + 3G, 3G only), Cell Lock (with the neighbour scan), Smart Tower Connect, Signal Detection, SIM Card (PIN, PUK and unlock code; the ICCID, IMSI and number are masked), eSIM Profiles (switch, rename, delete, operator receipts, and **Add eSIM** from a photo of the QR code or the code) and SIM Services (USSD and the SIM Toolkit).
  - **Connectivity:** WiFi, Guest WiFi, APN, LAN / DHCP, DNS (with DNS over HTTPS and its cache).
  - **Security and quality:** Firewall (on/off and level, NAT, UPnP, DMZ, port forwarding), Telemetry Blocker, VPN Passthrough, QoS.
  - **Device Controls:** charge limit (50–100 %), Power-save Mode, Fast Boot, Auto Sleep with its idle timeout, and **Reboot Router** and **Factory Reset**, which ask for the agent password (wrong passwords count like failed logins).
  - Changes that can cut the connection or are destructive ask for confirmation first (mobile data off, roaming on, airplane mode on, a network mode, a band or cell lock, WiFi, APN, LAN, DNS and firewall changes, deletes); over Tailscale the question also says how it affects your remote connection. Mobile data, airplane mode, network mode and eSIM changes show their progress until they have settled. Passwords are never shown (leave the field empty to keep the current one), and after a change that restarts WiFi or moves the router's address the page tells you where to reconnect.
- **Settings:** Session (**Log out**), Polling (how often live pages update the signal, 1–10 s), Appearance (System, Light or Dark), **HTTPS & Install as App** (see [below](#install-it-as-an-app)), About and Legal. The refresh interval and the theme are saved in the browser.

You stay logged in for 12 hours without use, and at most 7 days. Restarting the agent logs every browser out.

### Install it as an app

Browsers install web apps only from HTTPS pages, so the agent can serve the web UI over HTTPS on your LAN with a certificate from the router's own certificate authority. No internet, account or domain is needed; each phone or computer trusts that authority once.

1. **Turn it on:** Settings › **HTTPS & Install as App** › **Turn On HTTPS** (it shows the router's date, which the certificates are dated from). The web UI is then also at **https://192.168.0.1:9443/** and **https://u60.home.arpa:9443/**; plain http on port 9090 keeps working.
2. **Trust the router's certificate authority on each device** (Settings › **Install on Your Devices** has the downloads, the fingerprint to compare and these steps):
   - **iPhone/iPad:** in Safari tap **Profile for iPhone and iPad** › **Allow**; Settings › **Profile Downloaded** › **Install**; then Settings › General › About › **Certificate Trust Settings** › turn on full trust for "U60 Pro local CA …".
   - **Android:** tap **Certificate (.crt)** to download it; Settings › Security & privacy › More security & privacy › Encryption & credentials › **Install a certificate** › **CA certificate** › Install anyway, confirm with your screen lock, pick the file (names differ by maker: search Settings for "CA certificate").
   - **Mac:** open the downloaded `.crt` (Keychain Access adds it), double-click "U60 Pro local CA …" › Trust › **Always Trust**.
   - **Windows:** open the `.crt` › **Install Certificate** › Current User › Place all certificates in the following store › **Trusted Root Certification Authorities**.
   - **Firefox:** version 120 and later uses the certificates you add to Windows, macOS or Android. On Linux (or with that setting off): Settings › Privacy & Security › Certificates › View Certificates… › Authorities › Import…, and trust it for websites.
   - **Check the fingerprint** each device shows (iPhone: Profile Downloaded › More Details, before Install; Mac: Keychain Access; Android: Trusted credentials › User; Windows: Details › Thumbprint, SHA-1) against one that did not come over plain http: the Install page opened over HTTPS on a device that already trusts the CA, or `sha256sum /data/zte-agent-tls/ca.crt` over SSH. Someone on your Wi-Fi could otherwise swap the http page and the file for their own.
3. **Install:** open `https://192.168.0.1:9443/` (or `https://u60.home.arpa:9443/`) and log in, then **Chrome:** menu › **Install** (Android: Add to Home screen › Install); **Safari on iPhone:** ••• › Share › View More › **Add to Home Screen**; **Safari on Mac:** File › **Add to Dock**.

- The certificate authority can only vouch for this router: its LAN address when you turned HTTPS on and `u60.home.arpa` (and names under it), never for other websites or devices. Its root key is destroyed after setup; the key the router keeps is limited the same way. If you change the router's LAN address later, `https://u60.home.arpa:9443/` keeps working; for the new address, reset HTTPS and trust the new authority. See [SECURITY.md](SECURITY.md).
- The certificate lasts 397 days. It renews itself only when the agent has a verified clock (a configured Live Activity provider); otherwise Settings shows **Renew Certificate** in its last 30 days, about every 13 months. Devices keep trusting the same authority.
- `u60.home.arpa` comes from the router's DNS and works only over HTTPS. A device that uses its own DNS (Android's Private DNS set to a provider, DNS over HTTPS in the browser, a VPN) may not find it; the address always works.
- The http and https addresses keep separate logins (and a home-screen app has its own), so log in once on the one you use.
- **Reset HTTPS** (Settings) deletes the certificate authority and stops HTTPS (open https pages stop working too). Remove the old one from your devices; turning HTTPS on again needs each device to trust the new one.
- Without HTTPS you can still add the page to the home screen: Safari's ••• › Share › View More › **Add to Home Screen** works over http (log in once inside it); Chrome for Android only makes a shortcut that opens as a normal tab.

### With and without JavaScript

Everything works with JavaScript off: logging in and out, every page and every setting.

- The dashboard and its detail pages show a snapshot with its age ("over 1 min old" after a minute) and a **Refresh** link.
- A change that shows its progress (mobile data, airplane mode, network mode, a scan, an eSIM switch or download, a speed test, a signal detection) reloads its page every 3 s until it is done.
- The theme (Settings › Appearance) works without JavaScript too. With it, under System, iPhone's status bar also follows a light/dark switch made while a page is open.

With JavaScript, the dashboard, its detail pages, the progress pages and the pages that follow the router (SMS, SIM Card, eSIM Profiles, SIM Services, Signal Detection) update live. This uses Datastar, which the agent serves itself, so no internet is needed.

- Updates stop while the page is hidden or the phone is locked, and the router samples nothing while no one is watching. Settings › Polling sets how often the signal updates while a live page is open.
- "No updates for 15 s" means the connection dropped.
- Live updates pause after 30 minutes; tap **Resume** to continue.

### Security

On port 9090 the web UI is plain HTTP on your LAN, like the apps' connection to the agent: your password and session cookie are not encrypted in transit, and anyone who can watch traffic on the router's network can read them. Turn on HTTPS ([above](#install-it-as-an-app)) and use the https address to encrypt the browser's traffic.

- Use a strong Wi-Fi password and never forward port 9090 to the internet.
- The agent refuses connections that arrive over mobile data, even with the router's firewall off or at Low; Wi-Fi, USB and Tailscale keep working. `ZTE_AGENT_ALLOW_MOBILE=1` in the start script turns this off.
- For remote access, use Tailscale, which is encrypted. The web UI also answers on the router's Tailscale name.
- Sessions use an `HttpOnly`, `SameSite=Strict` cookie, and every form carries a CSRF token.
- Requests for unknown host names are refused (DNS-rebinding defence). To use another local DNS name, list it in `ZTE_AGENT_UI_HOSTS` (comma-separated) in the start script.
- Repeated wrong passwords are rate limited.
- Lost a phone, or think the password leaked? Set a new agent password (`./deploy.sh <new-password>`): the agent restarts and every app and browser is logged out.

See [SECURITY.md](SECURITY.md) and [zte-agent/docs/web-ui.md](zte-agent/docs/web-ui.md).

### Not in the browser (use the apps)

- The Lock Screen / Dynamic Island **Live Activity** (iOS); the live dashboard is the browser's equivalent.
- **Scanning an eSIM QR code** with the live camera: take or choose a photo of it, or type the code.
- Anything that needs the phone's **location**.
- **Phone calls** (the app's dialer).
- **LAN Speed Test** (it measures on the phone) and **Config Decrypt/Encrypt** (it handles a config file with credentials on the phone).
- The app's **Gateway** and **Keychain** settings: the browser is already on the router, and its password manager can keep the agent password.

## Project Structure

```
u60-Pro-rs/
├── zte-agent/                 On-device agent: REST API + browser UI (port 9090)
│   ├── build.rs               Web UI assets: Datastar hash check, icons, gzip, manifest
│   ├── docs/                  esim.md, events.md, live-activity.md, web-ui.md
│   └── src/
│       ├── main.rs            Entry point: event bus, background services, listener
│       ├── server.rs          /api route table, auth precheck, slow/general lanes
│       ├── handlers.rs, *_ext.rs, sms.rs, sim.rs, ...   Endpoint handlers (ubus, AT, sysfs)
│       ├── httpd/             HTTP listener: axum on hyper http1, one tokio thread, blocking-work gate
│       ├── auth/              Bearer tokens, browser sessions, login rate limit (boot clock)
│       ├── events/            Event hub, demand-driven sampler, GET /api/events (SSE)
│       ├── web/               Browser UI under /ui/: maud pages, sessions + CSRF, live updates
│       │   └── assets/        app.css, vendored Datastar 1.0.4
│       └── esim*.rs, stk/, tailscale/, doh/, ...
├── euicc/                     eSIM card library (Rust translation of lpac's libeuicc, LGPL-2.1)
│
├── web/                       Landing page, docs and browser installer (open-u60-pro.vercel.app)
│   ├── src/
│   │   ├── app/               Next.js app router (page, layout, globals)
│   │   ├── components/        Landing sections, UI primitives
│   │   └── data/              Feature data, API endpoints, device specs
│   └── public/                Static assets (images, icons)
│
└── mobile/                    Native companion apps
    ├── README.md              Mobile apps documentation
    ├── ios/OpenU60/           SwiftUI app
    │   ├── Core/              Networking, crypto, models
    │   ├── Features/
    │   │   ├── BandLock/      Band locking
    │   │   ├── Call/          Voice calls
    │   │   ├── Clients/       Connected devices
    │   │   ├── Config/        Config backup/restore
    │   │   ├── Dashboard/     Signal cards, WiFi card, status
    │   │   ├── DeviceInfo/    Hardware + firmware info
    │   │   ├── Login/         Auth + keychain
    │   │   ├── RouterSettings/
    │   │   │   ├── APN/       APN profile management
    │   │   │   ├── CellLock/  Cell locking
    │   │   │   ├── Device/    Reboot, USB, battery, charge policy
    │   │   │   ├── DNS/       DNS settings
    │   │   │   ├── Firewall/  Firewall rules
    │   │   │   ├── LAN/       DHCP settings
    │   │   │   ├── MobileNetwork/  Network mode, SA/NSA
    │   │   │   ├── NetworkMode/    Network selection
    │   │   │   ├── QoS/       Traffic shaping
    │   │   │   ├── Schedule/  Scheduled reboot/wifi
    │   │   │   ├── SignalDetect/   Signal quality detection
    │   │   │   ├── SIM/       SIM card PIN/PUK management
    │   │   │   │   └── STK/   SIM Toolkit & USSD
    │   │   │   ├── STC/       Smart Tower Connect
    │   │   │   ├── Telemetry/ Telemetry blocking
    │   │   │   ├── VPN/       VPN settings
    │   │   │   └── WiFi/      WiFi configuration
    │   │   ├── Scheduler/     Scheduled/recurring jobs
    │   │   ├── Settings/      App settings
    │   │   ├── Signal/        Signal monitor
    │   │   ├── SMS/           SMS compose/read/conversations
    │   │   ├── Tools/         Band lock, clients, config, etc.
    │   │   └── USBMode/       USB mode detection/switching
    │   └── Navigation/        5-tab bar (Dashboard, SMS, Tools, Router, Settings)
    └── android/OpenU60/       Jetpack Compose app
        ├── core/              Network, crypto, models, DI
        ├── feature/           bandlock, clients, config, dashboard,
        │                      deviceinfo, login, router, scheduler,
        │                      settings, signal, sms, tools, usb
        └── navigation/        NavHost + bottom bar
```

## Signal Thresholds

| Metric | Excellent | Good | Fair | Poor |
|---|---|---|---|---|
| **RSRP** (dBm) | >= -80 | >= -100 | >= -110 | < -110 |
| **SINR** (dB) | >= 20 | >= 10 | >= 0 | < 0 |
| **RSRQ** (dB) | >= -10 | >= -15 | >= -20 | < -20 |

## iOS Live Activities

Show the modem's signal on your iPhone Lock Screen and Dynamic Island. The layout
adapts to LTE, LTE carrier aggregation (4G+), and 5G, with signal bars, RSRP,
SINR, and sample age. Readings describe the modem's cellular connection.

<p align="center">
  <img src="mobile/ios/screenshots/live-activity-lock-screen.png" alt="OpenU60 Lock Screen Live Activity showing 4G+ LTE carrier aggregation, -72 dBm RSRP, 9 dB SINR, and a 12-second sample age" width="430">
</p>

### Enable the feature

1. **Prepare Apple signing.** Use your own Apple Developer team, an explicit app
   bundle ID with Push Notifications enabled, a matching provisioning profile,
   and an APNs authentication key (`AuthKey_<KEY_ID>.p8`). Update the app and
   extension bundle IDs/signing in
   [project.yml](mobile/ios/OpenU60/project.yml). The ActivityKit manager,
   Live Activity extension, and Live Activities Info.plist flags are included.
   The current iOS project targets iOS 17 or later.
2. **Configure the modem.** Copy your `.p8` and an edited
   [example configuration](zte-agent/config/live-activity.example.json) to
   `/data/zte-agent-apns/`. Set `enabled` to `true`, fill in your Team ID, Key ID,
   containing-app bundle ID, and key path. Match `environment` to the installed
   build: `sandbox` for development, `production` for TestFlight/App Store.
   Keep the directory root-owned with mode `700` and both files mode `600`.
3. **Enable the agent sender.** Preserve the existing password and startup
   settings, add the following environment variable to the agent launch script,
   and restart only `zte-agent` to load the configuration:

   ```sh
   export ZTE_AGENT_LIVE_ACTIVITY_CONFIG=/data/zte-agent-apns/live-activity.private.json
   ```

4. **Start on the iPhone.** Install your signed app, connect to the modem, log in,
   and tap **Dashboard → Live signal → Start**. Allow Live Activities in iPhone
   Settings. Lock the phone to see the card, or expand the Dynamic Island on a
   supported iPhone. Tap **Stop** in the app to end the session.

The modem sends directly to Apple Push Notification service (APNs); no hosted
backend is required. Both the modem and iPhone need internet access for push
updates. Keep private keys out of Git and the app bundle; self-hosting developers
use their own credentials. See the [full setup and API guide](zte-agent/docs/live-activity.md)
for file provisioning, signing, and configuration details.

### Freshness and recovery

- The default send interval is **15 seconds**, with readings marked stale after
  **30 seconds**. Apple controls delivery, so this is not a guaranteed refresh
  rate. Sample age should reset when a newly sampled update reaches the phone.
- Sessions last **one hour**. Currently the agent holds **one registration**;
  starting an activity on another phone replaces it. Agent restarts clear that
  registration; reopen the app to register again, or use Stop/Start if needed.
- SIM removal/ready and WAN events trigger agent recovery. It verifies internet
  access and UTC before resuming delivery, retaining an unexpired registration.
- **Apple accepted an update** confirms submission, not phone delivery. During
  physical testing after a SIM swap, agent sends recovered but the iPhone stayed
  stale until Wi-Fi was toggled off/on. If this happens, reconnect the iPhone's
  Wi-Fi and check sample age. Fully automatic phone-side recovery is not yet
  verified; see the [SIM-swap test findings](zte-agent/docs/sim-swap-ubus-2026-09-15.md).

## License & Disclaimers

This project is licensed under the [MIT License](LICENSE), except:

- `euicc/` (the eSIM card library linked into zte-agent) is a Rust translation of [lpac](https://github.com/estkme-group/lpac)'s libeuicc and stays under the **LGPL-2.1-only** ([`euicc/LICENSE`](euicc/LICENSE)).

[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) lists everything the zte-agent binary contains and is published with every release.

**Disclaimer:** This software is provided "as is", without warranty of any kind. Use it at your own risk. The authors are not responsible for any damage to your device, voided warranties, or other consequences arising from the use of this software.

- **Not affiliated with ZTE Corporation.** This is an independent community project.
- Intended for use on devices you personally own.
- Reverse engineering was performed solely for interoperability and educational purposes.
- Encryption keys and protocol details are sourced from publicly available community research.
- No proprietary source code from ZTE is included in this repository.
