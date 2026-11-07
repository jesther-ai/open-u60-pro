# Network integration review

## Review follow-up (2026-10-28, local changes)

This increment is not deployed. Earlier physical-device results below do not
validate these changes; no device network, agent process or boot hook was changed.

- Startup retries Mihomo again after Relay is usable, if the first bounded start
  attempts failed. Every owner core switch cancels this process's boot restore;
  a retained marker after a failed deletion cannot override that cancellation.
- Relay cancellation clears its visible restoring state and wakes retry waits
  immediately. An owner-operation generation is rechecked under the network lock
  before scanning/joining and between association/DHCP waits. Old restore results
  cannot overwrite a newer owner's status, including replacement by the same BSS.
- Width changes distinguish requested and confirmed device state. An unsuccessful
  or delayed request must not discard the negotiated candidate ceiling. A trial
  that started with measured downlink demand has a bounded fallback to a known
  usable width, rather than waiting indefinitely for successful byte samples.
  Shared radio airtime alone is not evidence of this connection's traffic demand:
  a link that has never supplied enough downlink data and has no known-good
  baseline still needs user diagnosis; no synthetic probes are introduced.
- The launcher distinguishes the main service from same-executable DHCP/CLI
  helpers, verifies process start time, and retains its single-launch lock.
  This does not change setup.sh's separate installation stop loop; its broader
  executable matching and PID-reuse window remain a follow-up hardening item.
- Restart-policy copy now reflects saved intent, not the obsolete boot-off rule.
  Dormant saved intent has an explicit stop action. Wi-Fi's authenticated device
  list keeps full MACs/hostnames by the owner's explicit choice; other device
  lists retain masking. CLAUDE.md records the exception.

### Local validation of this increment

- Complete host Rust suite: 1,145 unit tests passed, 3 ignored, plus the startup
  integration test passed. Used a clean job-local Cargo target directory because
  the shared target initially refused linking/copying executable outputs.
- During final repetition, the existing PIN no-echo test matched four digits
  inside a random token. Its assertion now removes only complete CSRF/once/CSP
  random values before checking rendered text/headers; production PIN handling
  is unchanged. The subsequent complete suite passed.
- Installer: 17 tests passed, including executing the generated shell against
  controlled proc fixtures; dashboard adapter: 4 tests passed.
- Chromium against actual rendered fixtures: 320/390/1440 px, CSP, modal/error
  handling, duplicate-submit prevention and no-JavaScript fallback passed. This
  is host browser acceptance, not LAN/device forwarding validation.
- Host Clippy completed: 19 binary warnings, 23 test warnings including the same
  19; none in the added recovery/width paths. Shell syntax and diff whitespace
  checks passed. CSS decreased from 49,146 to 48,633 bytes (519-byte headroom).
- ARM64 musl release rebuilt successfully. `llvm-size -A` Total: 8,533,816 bytes;
  deterministic `gzip -n -9`: 4,358,909 bytes. The pre-fix artifact measured with
  the same tools was 8,529,516 / 4,354,020 (+4,300 / +4,889 bytes).
  `aarch64-linux-musl-size` was unavailable, so the installed LLVM ELF-capable
  equivalent was used rather than presenting a different measurement as GNU's.
  Release SHA-256: `24072cdc15e19d6404b2fa4640e7659952725f0379671b202488ff40209ef6ce`.

### Remaining physical acceptance (requires agreed downtime)

Keep authorized USB ADB and a private backup of the current executable, launcher
and owned recovery files. Never display credentials or unrelated SSIDs/BSSIDs.
Use a controlled upstream and a LAN client; do not alter country codes, vendor
boot-sync services, cellular intent or unrelated firewall rules.

1. Restore Relay + Mihomo with cellular already off: make startup temporarily
   unavailable, then bring up the controlled AP. Require usable Wi-Fi and a
   running core without enabling cellular; verify actual LAN TCP/UDP forwarding.
2. Cold boot with a 5 GHz upstream; separately start with the upstream absent.
   Check bounded retry/status and preserved AP management access. Historical
   2.4 GHz success is not acceptance for these cases.
3. Stop during retry sleep, scan and association. Require immediate cleared
   reconnect status, bounded rollback, no later reconnect and no renewed intent.
   Replace with another network, then the same BSS, to check stale-work rejection.
4. A visible AP with deliberately invalid credentials must not leave residual
   STA/helpers/routes. Record AP stop/start frequency and client interruption;
   repeated association failure still uses the existing bounded retry policy.
5. Local token: valid over ADB loopback, refused over LAN, events refused on both;
   never put the token in output or screenshots. Confirm full Wi-Fi MAC display
   only after login and masking on the other ordinary device lists.

Do not run these steps unattended or treat local fixture tests as device proof.
No commit or deployment is part of this follow-up.

## Runtime RA cutover and physical reboot (2026-10-27)

Supersedes snapshot restoration in the older recovery sections. Normal Relay
now writes no DHCP/UCI RA options: typed netifd runtime data controls the existing
single odhcpd. Release uses JSON null (inert typed tombstones), so odhcpd renders
the vendor's CURRENT config. Reboot drops runtime data; stale cellular DNS is
not compared or restored. The old UCI repair code remains migration-only.

- A real OpenWrt test first failed because normal Relay still generated UCI
  deltas. The runtime adapter passes that regression plus actual RA packet tests
  for vendor reload/commit, current DNS/MTU, unrelated runtime data, same-boot
  crash cleanup and replacement of netifd/previous-boot journals.
- Interface-data DNS was rejected after wire capture proved it appends to UCI
  DNS. Relay suppresses RDNSS; dual-stack clients resolve A/AAAA through existing
  DHCPv4 DNS. This is IPv6 data/NAT66 support, not IPv6-only-client support.
- B31 initially had empty system resolvers while cellular was disconnected.
  Added tagged Wi-Fi resolvers to the volatile netifd resolver target, preserving
  its two-hop symlinks, current vendor data, ownership and permissions. Cleanup
  removes tagged lines only, never a historical DNS snapshot. Runtime refresh
  errors are visible; DNS cleanup has a separate journal so it cannot strand
  unrelated routing/AP cleanup. No persistent DNS UCI or LAN restart is used.
- Actual B31's firmware netifd/odhcpd in private namespaces passed packet
  assertions: Relay ULA only, lifetime 90, MTU 1280, no stale RDNSS; release used
  newly changed vendor DNS/MTU. Production DHCP file bytes were unchanged.
- Physical W (2.4 GHz) and a synthetic real-br-lan client acquired DHCPv4 and
  SLAAC/default route; native and downstream NAT66 HTTPS returned 200. Selected
  saved Hysteria2 node in global mode temporarily: downstream IPv4/IPv6 HTTPS
  and UDP to Quad9 port 9953 succeeded on both families. Proxy mode/selections
  were restored. Cloudflare port 5353 timed out and is NOT accepted as UDP proof;
  ordinary DNS/port53 success is also not counted as non-DNS data-plane proof.
- With cellular explicitly off, station loss cleaned Relay without recovery
  errors or enabling cellular; Mihomo stayed enabled. Scan/reconnect restored
  both families and non-53 UDP. Physical AP power loss remains distinct from
  this `wpa_cli disconnect` simulation.
- Rebooted the B31 while Relay had a valid dual-stack lease/runtime RA journal.
  Formal agent autostarted on 9090; no stale RA/route/wireless/DNS journal remained,
  LAN runtime data was empty and scan/connect/disconnect worked afterwards.
  Postboot native IPv4/IPv6 HTTPS returned 200. Only the single existing boot
  hook remains; no factory boot-sync service was disabled or restarted.
- Device has no registered cellular data network during this round. Therefore
  real cellular IPv6 reconnection/Internet fallback is NOT accepted. A deliberate
  QA enable command returned vendor ubus error while its readback changed to 1;
  always check readback, not just that acknowledgement. Test-entry cellular
  enabled state is restored; Relay/core end off, with saved Wi-Fi/profiles intact.
- Offline boot reset the device epoch to firmware 2025 despite correct
  Asia/Shanghai TZif. Verified HTTPS Date anchors restored actual UTC after
  reboot. Autonomous offline-boot time recovery remains outside this RA fix;
  no Mihomo NTP workaround or eight-hour epoch offset was introduced.

Current validation: 1,068 host unit tests + 1 startup integration pass; seven
installer tests pass; browser regressions at 320/390/1440 px and no-JS fallback
pass; isolated OpenWrt lifecycle/RA packets and real Mihomo bridge transitions
pass. Clippy completes with existing warnings (none in the RA/DNS additions).
The first concurrent VM core run hit the isolated script-worker resource limit;
its independent pipeline test and a full serialized repeat passed. This is
recorded, not hidden as a uniformly green stress test.

Final focused review separated volatile DNS cleanup from route rollback into
`dns.json`; a DNS-file failure cannot halt independent AP/routes teardown. A
physical repeat on that exact release connected W with native dual-stack HTTPS,
observed its separate DNS journal, disconnected without retained state and
removed only tagged resolver lines. The final ARM64 section Total is 8,422,912
bytes; deterministic `gzip -n -9` is 4,302,282 bytes. Final release SHA matches
the formal installed executable. On-device login returns 200 and unauthenticated
private APIs return 401; installed factory-admin auth and boot hook are unchanged.

Private evidence/rollback is on the computer in
`/Users/g/Desktop/Workspace/u60-pro/device-backups/20261027-012827-runtime-ra`.
Exact owned device QA/install stages were archived and removed, not user files.
The broader coordinator proposal remains deferred; there is no second network
daemon, DHCPv6 client on cellular, main-route flush or persistent RA write fight.

## Final B31 deployment and device cleanup (2026-10-26)

User requested deployment and removal of obsolete device test/install files.
The steps below supersede the earlier not-yet-deployed status for the editor,
download and recovery increment.

- Entry: formal agent PID 23678, Mihomo already off, Relay unassociated but its
  IPv6/RA/wireless recovery journals and `u60sta0` remained. Cellular enable=1.
- Archived the previous binary, launch/boot files, DHCP configuration, recovery
  journals and profile inputs privately on the computer before changing anything.
  Stopped only the formal agent and its verified orphaned ubus listener while
  holding the launcher lock. No vendor daemon was disabled or restarted.
- Ran the new release's `network-recover` only after the owning agent stopped.
  Actual B31 recovery succeeded: the station, owned IPv4/IPv6 routing/firewall
  effects and all recovery journals were removed; the vendor's newer
  `ra_useleasetime=1` was preserved. Both wlan0 and wlan2 report ENABLED.
- Installed using `setup.sh --build --serial 0123456789ABCDEF --factory-password
  --no-ssh`. New formal PID 11415 matches the locally built release SHA-256.
  Exactly one rc.local hook remains; repeated launcher invocation keeps the same
  PID. Factory admin authentication is retained. No device reboot was performed.
- Login on port 9090 returns 200, private APIs return 401 without credentials,
  protected UI/dashboard routes redirect to login. Subscription source, generated
  configuration, core, dashboard index, saved Wi-Fi, stable ULA, rc.local and
  ethers hashes remain unchanged. Cellular stays enabled and connected on both
  families; routes use rmnet, and vendor daemon sync reports success.
- After verification and computer-side archival, removed all three known
  `/data/zte-agent-install-*` directories, the final deployment/recovery helper
  directory and orphaned `/tmp/zte-agent-relay` session files. The leftover
  control socket was confirmed absent from `/proc/net/unix` before unlinking.
  Removed allocation: 23,712 KiB. No known U60 test binary/stage or ADB forward
  remains. Production runtime log/lock/PID files are not test artifacts.
- Kept `/data/zte-agent`, `/data/local/tmp/start_zte_agent.sh`, the installed core,
  Zashboard, geodata/licenses, profile data and saved Wi-Fi. The relay persistent
  directory now contains only saved-wifi.json and ula-prefix. No mobile/public
  website changes. Rollback archives and verification reports are on the computer
  in `/Users/g/Desktop/Workspace/u60-pro/device-backups/20261026-212719` (private).

Physical recovery of the reported retained state is now verified. Repeated
connect -> cellular off/on -> disconnect acceptance on this newly installed
release, authenticated browser workflows and a cold boot remain separate tests;
deployment/cleanup did not toggle cellular or reconnect Relay merely to test it.

## Editor, download and recovery follow-up (2026-10-26)

This section describes local changes, not a deployment. The live B31 agent has
not been restarted for this increment; its user-selected cellular state and
formal profile/configuration were left unchanged.

- Add/Edit is a native dialog with a visible, wrapping subscription URL, full
  width selects, consistent input sizing and separate primary/secondary actions.
  Existing profiles support edit/delete; selected running profiles cannot be
  deleted. Commit rechecks this restriction under lifecycle locks, including a
  concurrent start between validation and save.
- Nonce-scoped Datastar initialization adds spinners and duplicate-submit guards
  for Clash start/stop, profile downloads/updates, Wi-Fi scan/connect/disconnect
  and cleanup retry. A named `action` input shadowed `form.action`; the browser
  regression reproduced missed interception and now verifies the attribute-based
  target. Error messages are inserted as text, not arbitrary server HTML. Form
  values and CSRF/single-use tokens survive enhanced error handling.
- Subscriptions/geodata share the bounded HTTPS downloader. While the core runs,
  downloads use authenticated loopback HTTP CONNECT on 127.0.0.1:17893, with no
  silent direct retry. The managed listener replaces untrusted inbound listeners;
  its credential is not reflected by the dashboard config bridge. Core readiness
  includes rejection of unauthenticated proxy requests. Off-core relay downloads
  use upstream DHCP DNS without changing system DNS or enabling cellular.
- GeoSite/GeoIP/ASN preparation tries official release and official raw delivery
  with their own SHA-256 sidecars; cached files are reused only after verification.
  Failure retains the saved configuration. Actual B31 tests found both direct
  GitHub failures on one uplink and upstream-node failures through the owner's
  current rules; routing a request through Clash does not prove node connectivity.
- B31's cellular toggle did more than commit temporary UCI: it rewrote RA
  options. The retained journal stopped at RA restore with `ra_useleasetime=1`
  where both original and relay values were `0`. Real OpenWrt reproduced the
  same failure before the fix. Teardown now preserves external values per key;
  active maintenance journals a changed committed baseline before reasserting
  the relay overlay. Pending user changes are not adopted/committed. An external
  same-key UCI delta chain is preserved verbatim, not reverted for normalization.
- Generic cleanup still stops on dependency failures but now reports a sanitized
  reason and suggests panel retry. The larger coordinator, dependency-group and
  cross-boot design remains a proposal in
  [relay-cellular-recovery-plan.md](relay-cellular-recovery-plan.md), not a claim
  that these broader reliability changes have already been implemented.

Validation for this increment:

- Host: 1,062 unit tests plus one auth-startup integration test pass. Three helper/
  isolated tests are intentionally ignored in the normal host run. Clippy finishes
  with existing warnings, none in the changed network/profile implementations.
- Browser: actual rendered pages and CSP at 320/390/1440 px. Modal editing,
  visible URL, full-row selects, local/remote switching, retained failed input,
  one POST per double-submit, scan/connect/disconnect/core spinners, displayed
  cleanup errors and no-JavaScript editor fallbacks pass.
- Linux/OpenWrt: actual UCI/odhcp6c/odhcpd and netfilter lifecycle passes vendor
  commit, changed RA keys, active overlay refresh and pending-edit preservation.
  Actual pinned core passes cellular -> Wi-Fi IPv4 -> IPv6 -> cellular -> offline
  -> reconnect -> stalled-core recovery, TCP/UDP and fragmented UDP, plus the
  authenticated CONNECT inbound and clean rollback.
- Physical B31: isolated Rust test downloads geodata with checksum and the private
  subscription through a temporary Mihomo HTTP inbound; bad proxy auth fails
  without fallback. The test's temporary rule is MATCH,DIRECT: it validates the
  core inbound/downloader path, not the owner's remote proxy node. Repeating with
  the owner's profile currently fails upstream TLS/node connectivity. The test
  never changes the formal core, profile, cellular switch or route tables; stage
  files and forwarded ports are removed afterward.
- The physical off/on-cellular sequence after installing this fix is still an
  acceptance step. The currently retained B31 recovery state is not reported as
  cleaned. Stop the live core before an agreed agent/recovery cutover; do not run
  a separate `network-recover` writer concurrently with the formal agent.
- Final local ARM64 release section Total: 8,413,892 bytes; gzip -n -9:
  4,298,701 bytes. CSS: 49,057 bytes (within the 49,152-byte budget). Seven
  installer tests, four Zashboard adapter tests, shell syntax and diff whitespace
  checks pass. Mobile apps and the public website are unchanged.

## Profile management follow-up (2026-10-26)

This increment adds Rust-backed subscription/file/paste profiles, manual and
interval refresh, atomic selection, global Merge/JS preprocessing, remembered
upstream Wi-Fi and downstream DHCPv4 reservations. It does not alter mobile apps,
the public website, cellular intent/fallback policy or vendor daemon startup.
This section supersedes the older single-YAML-only UI description below.

Review and real-test fixes:

- Slow subscription/extension validation originally risked holding the core
  operation lock and preventing relay-loss restart. A separate profile mutex
  now serializes downloads/validation/install; the shared network/core locks
  are taken only for commit/restart. Failed downloads/JS/core validation preserve
  the previous manifest; failed startup restores the prior selected config.
- dnsmasq `read-ethers` does not strip inline comments: the first implementation
  treated a fixed-IP-plus-label as a hostname and the real client got a dynamic
  lease. Labels now occupy separate comment lines. The same production API test
  changed from red to green with real dnsmasq/udhcpc: requested `.60` lease,
  duplicate rejection, remove, original file bytes and unchanged daemon PID.
- With cellular off on the live B31, system resolv.conf was empty and loopback
  DNS refused connections, although Wi-Fi DHCP DNS worked. Subscription fetch
  now uses the active relay's resolver snapshot for HTTPS address resolution,
  keeping SNI/certificate checks. It never rewrites system DNS or turns data on.
  Both host and B31 downloaded the private subscription successfully afterward.
- The Linux core acceptance runner's fixed six-second delay could observe an
  intermediate empty/incompatible netfilter chain during a normal core restart.
  It now waits at most 25 seconds for both rules and actual dual-stack LAN TCP
  replies, then checks UDP/fragmented traffic as before. This is not a relaxation
  to accepting a missing chain or merely a running process.

Validation:

- Host: 1,055 unit tests and one auth-startup integration test pass; three
  intentionally ignored helper/QA cases. Clippy completes with 23 existing test
  target warnings (19 shared with the binary), none in the new modules.
- Browser: actual Rust-rendered profile, reservation and relay forms at 320,
  390 and 1440 px; no horizontal overflow or JavaScript errors. Forms work
  without JavaScript. Session/CSRF/Origin/once, YAML upload bounds, escaping and
  non-echoing private credentials have route tests.
- Linux ARM64 VM: real pinned Mihomo on a bridge, two-profile switching while
  running, Merge then Script rule order, cellular -> Wi-Fi v4 -> dual stack ->
  fallback -> offline -> reconnect -> stalled controller; TCP/UDP both families,
  fragmented UDP, persistent LAN UDP reconnection and complete routing rollback.
  OpenWrt container separately verifies real fixed-address DHCP leases.
- Physical B31, without stopping the installed panel or active networks:
  production release QuickJS executes the owner's existing global script;
  17 filtered tests (script/profile/private Wi-Fi store/reservation parsing)
  pass, plus real HTTPS subscription download with relay DNS. Private source
  and script are not copied into the repository. The processed current script
  also passes the actual Mihomo v1.19.33 validator in the Linux VM.
- Final ARM64 release section Total: 8,379,956 bytes; gzip -n -9: 4,282,243 bytes.
  Prior installed baseline: 7,463,128 / 3,806,057 bytes. QuickJS/config management
  adds about 895 KiB section total and 465 KiB gzip. CSS remains 49,026 bytes,
  below 48 KiB. Installer's seven tests, shell syntax and diff whitespace pass.

Deployment follow-up (2026-10-26, user authorized): installed the local ARM64
release with `setup.sh --build --serial 0123456789ABCDEF --factory-password
--no-ssh`. On entry the device had already rebooted: old panel PID 5185, both
extensions off, cellular enable=0. Deployment did not reboot or change these
network choices. New `/data/zte-agent` PID 23678 matches the local release SHA
and loads the factory admin verifier. Exactly one unchanged rc.local hook starts
the agent; a repeated launcher call leaves the same PID. Original source.yaml,
Mihomo executable, dashboard index and rc.local checksums are unchanged. Rollback
inputs remain in `/data/zte-agent-install-20261026-182109-64963`; upload helpers
were removed. One verified orphaned old-agent ubus listener was removed; the
new agent's listener remains and vendor daemon sync reports success.

Installed panel checks: 9090 listens on all LAN addresses; login returns 200,
new private APIs return 401 without credentials, and protected web pages redirect
to login. Checks used USB forwarding and the router's own LAN-address request.
This Mac's 192.168.0.1 route went through its local Clash utun, not U60, so this
is not a claim of successful physical-hotspot browser acceptance. All temporary
ADB forwards and `/tmp/u60-profile-validation-20261026` files were removed.
No production subscription/global-script content was imported or replaced during
deployment. Physical remembered-password reconnect, phone DHCP renewal,
authenticated new web forms and a cold boot of this new release remain acceptance
steps; timed refresh has deadline/backoff unit tests, not a 24-hour soak.

Limits: complete HTTPS YAML subscriptions only (no Base64 node conversion);
manual refresh by default, optional 1/6/12/24/72/168-hour intervals; no proxy/relay
autostart; fixed IPv4 requires the client's per-SSID MAC to remain stable.
Global editors are write-only: blank preserves saved text, checkboxes disable,
and explicit `{}`/identity main replaces text. PROCESS-NAME is deliberately not
implemented for hotspot-client applications. `/etc/ethers` reservations must be
reviewed if the LAN subnet is changed or the official panel rewrites that file.

Date: 2026-10-25. Review basis: all tracked and untracked integration changes
relative to `734fe7eff816d6af922501c8a0e298ed6d38004c` (local HEAD). No integration
commit or production deployment was made. Later temporary B31 tests are recorded
in the final follow-up. The user's requirements in this chat are
the spec: Rust-backed Mihomo lifecycle and Router switch, running-only Zashboard
entry with automatic backend setup, a separate Wi-Fi Relay browser tab, compact
navigation and redesigned Relay buttons, without changing iOS/Android UI.

The initial review and measurements below precede upstream IPv6/NAT66. The
2026-10-26 follow-up at the end records the expanded user spec, latest fixes
and validation; do not read the initial IPv4-only limitation as current status.

## Standards

The parallel Standards review used `CLAUDE.md`, `CONTRIBUTING.md` and the review
skill's code-smell baseline. Three actionable bridge/lifecycle issues were fixed:

| Severity | Finding | Resolution |
| --- | --- | --- |
| P1 | Upgraded HTTP connections released total/per-peer permits while WebSockets remained open. | `httpd::ConnectionLifetime` owns the permits through the upgraded task; actual listener tests cover both caps. |
| P1 | Live WSS could survive HTTPS Reset after the HTTP upgrade. | The upgraded task observes the TLS listener stop signal and drops the bridge immediately. |
| P2 | Mihomo prefers a nonempty WebSocket query token over Authorization, so stale browser tokens could override server authentication. | Strip all decoded `token` query keys before forwarding; the server-only credential never goes to the browser. |

Final read-only re-review found no additional actionable issues in the scoped
bridge/listener and shared networking changes. Recovery tests and safeguards
below were also added; hardware behavior remains unverified.

## Spec

The parallel Spec review found three actionable routing/recovery issues, all fixed:

| Severity | Finding | Resolution |
| --- | --- | --- |
| P1 | Mihomo installation held the shared routing lock through multiple long downloads, delaying relay supervision/fallback. | A separate operation mutex serializes core operations without holding the routing lock during downloads/config checks. |
| P1 | AP channel restoration could run while STA was still associated to the upstream on the same radio. | Journal detach before credentialed reconfigure; require driver-confirmed disconnection before AP restoration, for both normal and same-boot crash recovery. |
| P2 | DNS interception could break the existing HTTPS management hostname. | Relay preserves local dnsmasq lookups and reroutes nonlocal router DNS to the hotel resolver; Mihomo forces the current management name to the actual LAN address. |

The broader safety pass also journaled roam-mode restoration, retained recovery
on uncertain driver state, and rejected invalid/conflicting DHCP DNS addresses.
The final read-only re-review found no further actionable spec defects. Required
browser controls exist; `mobile/ios/`, `mobile/android/` and the root `web/` site
are unchanged. The official Mihomo binary remains a separate program; Rust owns
its integration, not a rewrite of its proxy protocols.

## Verification

- Rust: `cargo test -p zte-agent`: 997 passed, 0 failed, 2 pre-existing ignored.
- JavaScript: `node --test zte-agent/tests/mihomo-dashboard.cjs`: 4 passed.
- Browser: actual Rust-rendered UI fixtures at 320/375/390/1440 px, 24 page cases
  without horizontal overflow; mobile current-label navigation and Relay actions
  checked in light/dark themes. Thirteen additional pinned-distribution cases
  cover direct entry/reload/bookmarks, stale/corrupt/blocked storage, HTTP/HTTPS,
  IPv6 origins and prefilled Setup with a deliberately unavailable mock backend.
- Actual Rust HTTP/TLS/WebSocket bridge tests use a strict loopback mock core;
  browser dashboard tests separately exercise all four streaming connections.
  Neither is a real-core forwarding or U60 test.
- Clippy succeeds with 23 existing warnings. Repository-wide formatting check
  still reports extensive baseline differences; unrelated files were not reformatted.
- ARM64 musl release: section Total 7,294,776 bytes; gzip -9 3,719,845 bytes.
  CSS is 49,026 bytes, below the 48 KiB limit. `git diff --check` passes.

The optional dashboard GitHub update check is intentionally blocked by same-origin
CSP. Failed-backend Setup browser cases deliberately return HTTP 502; they are
negative tests, not observations of a failing router.

## Outstanding Acceptance

No uncorrected actionable defect remains from these scoped local reviews, but
the integration remains experimental. B31 must still demonstrate real-core
TPROXY startup, LAN TCP/UDP and IPv6 node exits, relay NAT through both hotspot
bands, hotel captive-portal authentication, DNS, feature enable/disable order,
link/DHCP/core loss, vendor reload, suspend and crash/restart recovery.

At the time of this initial review, relay did not provide native IPv6 Wi-Fi
egress (superseded by the follow-up). Direct traffic is not forced through a
proxy node; loss of relay/core can restore direct/cellular traffic.
This is not a fail-closed anonymity mode. Earlier device probes proved only STA
association, DHCP and retained APs, not full forwarding or portal acceptance.

Totals: Standards 3 actionable findings fixed (worst P1); Spec 3 actionable
findings fixed (worst P1). Hardware acceptance is pending on both axes.

## IPv6 Follow-up (2026-10-26)

The user expanded the spec to permit Mihomo to use IPv6 from a capable Wi-Fi
upstream and provide downstream NAT66. This follow-up reviews the new IPv6
module and its parent lifecycle, DNS cleanup and browser state. The fixed
baseline is still `734fe7eff816d6af922501c8a0e298ed6d38004c`; reviews include
untracked WIP modules, not merely committed `...HEAD` differences.

### Standards

The parallel Standards review found no remaining documented-standard violation.
Two concrete correctness issues discovered on this axis were fixed:

| Severity | Finding | Resolution |
| --- | --- | --- |
| P1 | `odhcp6c -h` normally exits 1, so capability checking rejected every real startup. | Remove the successful-help assumption; owned client spawn handles unavailable executables with rollback. Full-start testing now covers the actual gate. |
| P2 | Delayed odhcp6c script children can outlive the client and alter a later connection's fixed lease file. | Random per-session lease paths prevent stale writes/deletions from reaching another session; reconnect test invokes stale callbacks explicitly. |

Process/network work remains off the async runtime, the existing odhcpd remains
the sole RA advertiser, secrets remain private, and owned rules/UCI options have
journaled recovery. No new CSS or mobile-app changes were needed.

### Spec

The parallel Spec review identified the following actionable issues, all fixed:

| Severity | Finding | Resolution |
| --- | --- | --- |
| P2 | IPv4-only relay still allowed router ULA destinations over rmnet IPv6. | Reject rmnet IPv6 regardless of destination; local/Tailnet interfaces are unaffected. |
| P2 | Same-path renewal after kernel address/route expiry could disconnect working IPv4 relay. | Failed lifetime refresh rebuilds only the owned IPv6 transaction; lost-route test covers this path. |
| P2 | Upstream RDNSS could point back to router/LAN/Tailnet or the assigned upstream address. | Filter conflicting resolvers before building DNS DNAT rules. |
| P1 | Normal DoH disable or pre-recovery startup committed the DHCP package, persisting temporary RA settings. | DNS cleanup no longer commits the package; lifecycle test checks the persistent file and RA rollback. |
| P2 | SLAAC address prefixes were incorrectly assumed on-link even without the RA L flag. | Only explicit on-link RA entries produce connected routes, with finite lifetimes. |
| P2 | Blanket ULA fallthrough sent an upstream-private DNS/node destination toward SIM instead of Wi-Fi. | Fall through only explicit LAN/Tailnet exemptions; unknown upstream ULA uses Wi-Fi. |
| P2 | On-link routes omitted the RA-advertised MTU. | Apply and renew MTU on both on-link and default routes. |

Final scoped re-review found no additional actionable defects. This is not a
claim of production readiness or exhaustive correctness.

### Latest Verification

- Rust: 1013 passed, 0 failed, 2 pre-existing ignored on macOS; separate Linux-only
  ignored full-start/lifecycle test: 1 passed in a disposable OpenWrt chroot.
- Real Linux TCP/UDP NAT66/native IPv6, upstream ULA routing, lifetime command
  updates, Mihomo policy precedence and full reverse rollback pass.
- Actual OpenWrt odhcp6c/Rust callbacks, UCI recovery and an odhcpd RA packet
  capture pass. The test chroot substitutes service-manager reload; it does not
  run B31's Wi-Fi driver or prove device forwarding/portal authorization.
- Browser: 28 page/viewport cases without overflow; dashboard JS: 4 passed.
- Clippy: succeeds with 23 baseline warnings. ARM64 static stripped section Total
  7,389,824 bytes; gzip -9 3,767,868 bytes. CSS stays 49,026 bytes.
- Details and reproducible harness: [relay-ipv6-validation.md](relay-ipv6-validation.md).

Totals for the IPv6 follow-up: Standards axis 2 correctness findings fixed
(worst P1), no remaining documented violations; Spec axis 7 findings fixed
(worst P1). B31 dual-stack forwarding, real Mihomo nodes, hotel authorization,
vendor reloads and crash recovery remain acceptance gaps on both axes.

## Real Bridge and B31 Follow-up (2026-10-26)

The user clarified that relay loss may fall back to already-enabled cellular;
the integration must never enable a user-disabled modem. There is no persistent
cellular kill switch. Actual B31 testing now supplements the earlier VM evidence.

### Standards

Final parallel review found no new documented-standard violations. Two geodata
correctness findings were fixed: dependency detection no longer mistakes node
labels or `GEOIP,LAN` for database requirements, and cached databases are verified
against recorded digests rather than accepting truncated nonempty files. Downloads
and repair run outside the shared network lock. The clean-directory first-save
flow has fixture tests; unrestricted real GitHub downloads remain environment-dependent.

### Spec

| Severity | Finding | Resolution/evidence |
| --- | --- | --- |
| P1 | Actual B31 bridge-netfilter loses the early TPROXY socket; rule hits alone falsely suggested working interception. | Replace TPROXY with owned gVisor TUN routing. Actual Linux bridge, both bridge hooks on, default-DROP firewall, seven uplink stages and fragmented UDP pass. B31 isolated and physical proxy paths also pass. |
| P2 | A one-time local address snapshot misses vendor renumbering. | Active core watcher refreshes local exclusions, excluding its own TUN. Add/remove local GUA TCP/UDP regression passes; restart preserves selections/mode. |
| P2 | DNS URL `#interface` fragments can bypass the managed uplink. | Validate decoded final bare fragment against known proxy/group names; preserve parameter-only suffixes and reject unknown interface bindings. |
| P2 | First valid subscription save times out downloading absent rule databases. | Prepare required official databases with bounded downloads/checksums before validation; preserve the old YAML on failure and report dependency errors. |
| P1 | B31 cellular-data toggle commits the whole DHCP package, persisting temporary RA settings; old rollback then stopped at its first option. | Same-boot surgical repair of owned persisted RA options via an isolated package alias, inode/content checks, atomic replacement and per-option delta cleanup. VM vendor-commit test preserves unrelated committed and pending settings; physical recovery succeeds with cellular still disabled. |
| P1 | RA conflict prevents IPv4 route rollback, stranding a disconnected STA route. | Always attempt independent IPv4 rollback; retain failed IPv6/interface journals for safe retry instead of erasing evidence. |
| P1 | With cellular off and Relay lost, B31 has no GUA/ULA; Mihomo's automatic IPv6 capability probe suppresses its private IPv6 TUN and restart fails. | Set the pinned core's `SKIP_SYSTEM_IPV6_CHECK=true` for validation/start. This does not enable modem/kernel WAN IPv6. Regression removes all fixture global/ULA interface addresses, rather than merely deleting defaults. |

### Latest Evidence

- Host Rust: 1022 passed, 0 failed, 2 pre-existing ignored. Dashboard JS: 4 passed.
- Real bridge/core VM: seven transitions, TCP/UDP IPv4/IPv6, UDP 1400/2000,
  persistent UDP, offline/no-cellular state, local GUA refresh and rollback pass.
- B31 namespace/core: actual bridge and seven transitions pass. This remains
  distinct from physical Wi-Fi and paid-node tests.
- B31 physical: upstream `W`, IPv4/IPv6/NAT66 and real RA work. With stable
  interception and the supplied Hysteria2 node, both HTTPS families return 200;
  UDP to a public non-53 service succeeds in both families. User confirms phone
  browsing; band and IPv6 score are unspecified.
- Manual cellular-off plus simulated station disconnect: enable remains 0,
  modem stays disconnected, and Internet correctly becomes unavailable. The
  committed-RA issue discovered here was repaired without enabling cellular.
- Final repeat with no GUA/ULA: core successfully starts offline, stays enabled
  after the controlled uplink restart, and reconnecting `W` restores HTTP 200
  plus non-53 UDP in both families with marked-rule counters. Transitions take
  roughly 15-25 seconds in this test and interrupt existing flows; this is not
  seamless failover or automatic Wi-Fi reconnection.
- No production/autostart deployment or commit; iOS/Android/root `web/` unchanged.
- Final ARM64 stripped static release: section Total 7,446,104 bytes;
  `gzip -9` 3,796,919 bytes. CSS remains 49,026 bytes. Clippy succeeds with
  23 baseline warnings, none newly reported in the network modules.

Open acceptance: both hotspot bands explicitly, physical AP power-loss versus
station-disconnect simulation, cellular-on fallback on hardware, hotel captive
login, sustained throughput/CPU/power, vendor reload/suspend/reboot races and a
second B31. A file lock plus before/after checks cannot exclude every independent
vendor writer; conflicts retain recovery state rather than claiming race freedom.

## Full Agent and Port 9090 Follow-up (2026-10-26)

### Standards

No remaining actionable standards finding in the final read-only review. New
device operations remain on blocking paths; the cellular-control lock order is
network then cellular, never the reverse. DHCP callback files use validated
session nonces. Recovery actions retain normal session/CSRF/once protections.

### Spec

| Severity | Finding | Resolution/evidence |
| --- | --- | --- |
| P1 | A user data-off request during the connectivity watchdog's two-second PDN pause can be overwritten by its automatic enable. | Shared cellular control and a data/airplane-intent generation cancel stale automatic restoration. User-off and ordinary network-mode changes have separate regression cases. |
| P2 | Probe failures on a Wi-Fi/portal upstream can trigger recovery of a healthy SIM. | Reset pending failures/deadlines while Relay owns the upstream or cellular is disabled/unreadable; recheck at effect execution. Config/WAN events still update the machine. |
| P2 | Delayed IPv4 DHCP callbacks can overwrite/delete the next connection's shared lease file. | Per-session nonce file and callback environment; stale bound/deconfig regression passes. |
| P2 | Controller/TUN ready can coexist with a failed TCP or UDP DNS bind. | Check port availability and require the managed local-name answer over all four TCP/UDP and IPv4/IPv6 loopback paths before redirecting clients. Real-core VM exercises occupied ports, then successful readiness at every uplink stage. |
| P2 | The Web UI hides cleanup after retained recovery state reports disconnected/off. | Explicit Recovery required and cleanup-only buttons for both modules; rendered-page/POST tests prove they do not scan or associate. |

The data/airplane cancellation guarantee covers writes through the agent, not
uncoordinated vendor applications writing during the watchdog's own PDN-off gap.
The original connectivity watchdog remains off by default. Tests do not enable
its disruptive modem-recovery ladder on the user's hardware.

### Full Panel Acceptance

- The actual production executable runs manually from `/tmp/u60-panel/agent`
  on `0.0.0.0:9090`, not the ignored loopback-only libtest harness. No boot entry,
  init service, modem setting or existing installation was replaced.
- A new private agent password protects the panel; no subscription/password is
  recorded here. User-owned configuration files for optional services were absent.
- Real browser login, Router, Relay and Mihomo pages pass at 390 px without
  horizontal overflow. Actual Clash switch on/off, Zashboard direct entry without
  Setup, no stored core secret and rejection of stale off-state URLs all pass.
- Browser forms perform scan, connect to the authorized `W`, wait for native
  IPv6/NAT66, enable Clash, open Zashboard, disable Clash and disconnect Relay.
  All complete without JS errors or retained recovery journals. A separate
  DHCP client on real br-lan gets HTTP 200 from the port-9090 login page.
- After testing the panel remains running; Relay and Clash are off, cellular
  remains enable=1/dual-stack connected, main APs restore channels 1/48 and no
  network test namespace/TUN/STA remains. DHCP UCI is restored; wireless changes
  are only vendor client-count fields, not configuration changes.
- GitHub downloads from the device are unreliable. The already SHA-verified
  pinned dashboard and licenses were staged over USB for this panel acceptance;
  this is not a claim that fresh online installation passed on this network.

Latest checks: 1030 host tests pass, 2 pre-existing ignored; actual bridge/core
VM passes all stages with DNS readiness and collision checks; Clippy retains
23 baseline warnings. ARM64 section Total 7,458,920 bytes, gzip -9 3,802,923 bytes;
CSS 49,026 bytes. No mobile-app or root public-site changes; no commit made.

## Factory Admin Password Follow-up (2026-10-26)

The owner requested the official admin password as the agent's default. Startup
now uses B31's existing SHA-256 verifier when `ZTE_AGENT_PASSWORD` is unset;
nonempty explicit overrides keep precedence. Missing/ambiguous/unsupported
factory credentials and empty overrides abort before background services or
listeners. No plaintext copy, official configuration write, shared session token
or Mihomo-secret change is involved. Restart after an official password change;
the verifier is deliberately immutable during a running process.

- Five startup unit tests cover factory browser/bearer login, confirmation,
  rejection of the digest as a password, hex case, admin section selection,
  explicit override precedence, duplicate initialization and invalid credentials.
- A production-entry-point integration test covers six invalid environment/UCI
  cases: exit 1, no listener, no background ubus service and no leaked verifier.
- Host suite: 1035 unit tests plus one integration test pass on three consecutive
  runs, with two pre-existing ignored tests. The first full run saw one transient
  HTTP 502 in the existing TLS Mihomo mock bridge test; its isolated rerun and
  the three full reruns passed. The transient's cause remains unconfirmed.
- B31 ARM64: all 59 auth tests pass. The production binary rejects an empty
  override with exit 1. The manual port-9090 agent is updated and running with
  the factory-password source; login form HTTP 200, unauthenticated API 401,
  previous temporary password 401 and previous bearer token 401.
- The factory verifier was independently accepted by the official panel's
  SHA-256 challenge flow; that temporary vendor session was logged out. The
  official password plaintext was not retrieved or guessed. Successful user
  login with the actual official password remains a manual acceptance step.
- Official auth configuration checksum is unchanged; cellular remains enabled
  and dual-stack connected, Relay/Mihomo remain off and vendor sync succeeds.
  Zashboard assets remain installed. No autostart entry or persistent service is
  installed; this is still the temporary `/tmp/u60-panel` deployment.

Clippy retains 23 baseline warnings and none in the new startup code. ARM64
release section Total is 7,467,604 bytes; gzip -9 is 3,807,502 bytes, increases of
8,684 and 4,579 bytes respectively. New Rust files pass rustfmt;
`git diff --check` passes. No mobile/public-site changes or commit.

## Persistent Release Installation and Cleanup (2026-10-26)

At the owner's request, `setup.sh --build --serial ... --factory-password --no-ssh`
now performs the local ARM64 musl release installation. The persistent executable
is `/data/zte-agent`; `/data/local/tmp/start_zte_agent.sh` unsets any inherited
password override and is called once from `/etc/rc.local`, before `exit 0`.
The original boot file's other bytes are unchanged. No ZTE init service is disabled.

The installer stages and hashes the binary before replacing it, checks the new
login page and unauthenticated API rejection before adding the boot hook, handles
B31 adbd's missing remote exit status, and stops only known executable paths and
their own orphaned ubus listeners. The launcher locks startup through exec and
detects an existing executable; four concurrent calls retained exactly one PID.
Seven installer tests cover shell syntax, password quoting, boot-file preservation
and idempotence, CLI refusals and old-adbd failure/output propagation. Initial
installation and a second release installation both pass on B31.

Cleanup removed 102 files from the three owned `/tmp/u60-panel`,
`/tmp/u60-live-qa` and `/tmp/u60-network-qa-tun` test trees (181,800 KiB), eight
unused temporary RA locks, two idle orphaned test event listeners, generated
Mihomo check/runtime YAML and duplicate same-version install backups. No owned
test namespace, process, mount or test artifact remains in the inspected paths.
The official core, dashboard, geodata, licenses, user's unchanged `source.yaml`,
core cache and relay ULA prefix remain. Only the original pre-install
`rc.local.before` is retained under the first installation backup directory.

Final checks: the persistent binary matches the local release checksum; login
HTTP 200 and unauthenticated API HTTP 401; factory-admin auth source; vendor sync
success; cellular enable=1 and dual-stack connected; Relay/Mihomo still off.
The device was not rebooted, so actual cold-boot acceptance remains untested.
Current release section Total is 7,463,128 bytes; gzip -9 is 3,806,057 bytes.
No new Rust change, mobile/public-site change or commit in this installation step.

## Profile Actions and Fixed-IP Navigation (2026-10-26)

Profile-list action groups now have 1rem side and bottom padding, including when
buttons wrap on narrow screens. A Playwright bounding-box assertion first failed
with zero bottom/left inset and now passes at 320, 390 and 1440 px. The redundant
fixed-client-IP shortcut is removed from Relay; Router > LAN / DHCP retains the
canonical entry. A rendered-page regression covers idle, connected and recovery
Relay states and confirms navigation does not mutate reservations.

- Host checks: 1063 unit tests and one auth-startup integration test pass; three
  tests remain intentionally ignored. Browser forms, action spacing, busy states,
  CSP and no-JavaScript fallback pass at all three widths.
- ARM64 release section Total: 8,413,892 bytes; gzip -n -9: 4,298,617 bytes.
  CSS: 49,123 bytes, still within the 48 KiB budget.
- Installed on B31 using setup.sh's local release build and factory-password
  mode. The device executable matches the build, the launcher retains one
  process, and the single boot hook is unchanged. Login returns 200, the
  unauthenticated API returns 401, and protected profiles redirect to login.
  The device's served CSS matches the new source byte for byte.
- All 57 snapshotted configuration/asset files are unchanged, including saved
  Wi-Fi, Mihomo source, fixed-IP bindings and official authentication settings.
  Relay and Mihomo remain off; both APs retain channels 1/36 and vendor sync
  succeeds. Cellular remains enable=1 but disconnected, as it was before this
  UI-only deployment; no cellular control was exercised.
- The new installer rollback is verified and kept privately on the computer;
  its redundant device copy is removed (8,224 KiB). Temporary ADB forwarding is
  removed. No reboot or authenticated on-device visual acceptance was performed.

## Error Feedback and REALITY Clock Diagnosis (2026-10-26)

The browser feedback regression reproduced zero spacing between an asynchronously
appended error and the submit buttons. Retry cleared the message text but left
its red container visible. Network-form errors now have 1rem vertical margin;
a valid retry removes only that form's previous result. Validation refusals keep
the old feedback, unrelated form/core errors remain, and a new failed response
creates a fresh alert. Server-rendered errors and no-JavaScript fallback remain
supported. Tests pass at 320, 390 and 1440 px. This follow-up is not deployed yet:
the owner's Relay and Mihomo sessions are active and were not interrupted.

A reported VLESS/REALITY delay failure was reproduced on the production core:
the node failed in about 0.35 seconds while DIRECT passed the same HTTPS probe.
Debug output identified REALITY authentication failure. The endpoint resolved
to the same IPv4 on the computer and router, with no AAAA answer; the runtime
node fields matched its saved profile exactly. The computer's working node had
different SNI and short-id values, but this was not the cause: all four parameter
combinations failed in a loopback-controller-only test core, then all passed when
only that isolated core's NTP offset was enabled, without writing system time.

The device's epoch was 28,800 seconds ahead. At the owner's explicit request,
system UTC was corrected against fresh, certificate-verified HTTPS Date headers
from Apple and Microsoft (both agreed with the computer within two seconds).
The original production node then passed three consecutive probes: 1095, 1065
and 742 ms. The production agent/core PIDs, configuration hash, timezone and
active Relay/TUN interfaces were unchanged. No node override, proxy selection,
cellular toggle or network restart was used. The proposed per-core NTP change
was withdrawn; managed core NTP remains disabled in favor of the requested
system-clock correction.

All diagnostic device files, temporary core processes and ADB forwards were
removed. Temporary computer-side credential/configuration/log copies were also
removed after retaining a sanitized verification report. Vendor SNTP is still configured for
automatic time with a +8 setting while the OS timezone is UTC. That is a
recurrence risk, not proof of the original writer; no persistent time-service
setting, RTC or boot script was changed, and reboot/cellular-resync persistence
has not been verified.

### Display Timezone Follow-up

The owner reported an incorrect screen clock after UTC correction. Inspection
found `/tmp/TZ` and UCI system timezone set to UTC, a missing `/tmp/localtime`
behind `/etc/localtime`, and no installed zoneinfo directory. Changing `/tmp/TZ`
alone did not affect newly started `date`; installing the standard Asia/Shanghai
TZif made local display UTC+8 while `date -u` and the epoch stayed correct.

The verified 561-byte TZif is now the persistent `/etc/localtime` regular file
(`/etc` is writable overlay; `/usr/share` is read-only). UCI system timezone is
`CST-8` and zonename `Asia/Shanghai`. Only `zte_topsw_devui` was restarted, never
disabled; agent/core PIDs and active Relay/TUN stayed unchanged, and vendor boot
sync still succeeds. The production node passes afterward (829 ms). Physical
screen confirmation remains pending; the older SNTP daemon still reports its
cached UTC display. Vendor SNTP settings were not changed, and cold-boot/carrier
resync behavior remains unverified. Backup and sanitized receipts are private
on the computer under `device-backups/20261026-225113-clock-timezone`.

Final pending UI build: 1064 unit tests and one auth integration test pass, three
tests intentionally ignored; full browser QA passes. ARM64 section Total remains
8,413,892 bytes, gzip -n -9 is 4,298,624 bytes, CSS is 49,138 bytes. No UI release
deployment occurred while the owner's network extensions were active.


## Matrix Review Follow-up (2026-10-27)

Code-review regressions fixed in the Rust implementation:

- Runtime RA journals now record withdrawal provenance. A later external
  `ra_lifetime=0` survives cleanup; retries only release our journaled zero.
- Active profile changes and rollback capture the same validated runtime
  mode/log-level as uplink changes. Slow profile-library reads no longer hold
  the mutex read by asynchronous dashboard handlers.
- IPv6 reconciles current LAN prefixes/local addresses independently of the
  Wi-Fi lease. Session-owned routes to retained LAN prefixes remain on br-lan,
  bounded by observed valid lifetime and a two-hour ceiling. No vendor address
  or DNS baseline is restored; old-boot runtime state is still discarded.
- B31 has no authoritative external cellular-intent revision. The optional
  disruptive connectivity-watchdog effects are safety-blocked, with an API
  warning, rather than claiming an agent-only mutex protects official-panel
  or touchscreen off requests. Manual controls and normal route fallback are
  unchanged; the watchdog was already off by default.
- Physical B31 BusyBox nslookup rejects inline comments after nameserver
  addresses. Relay ownership markers now use a separate adjacent comment line;
  cleanup also recognizes the old inline format.
- Reboot without cellular reset UTC to January 2025, breaking verified HTTPS.
  Startup now restores a local build/previous verified UTC lower bound. Relay
  connection and core startup over Wi-Fi verify two independent HTTPS Date
  responses (Apple and Microsoft, no redirects/proxy/TLS bypass), repair UTC
  and persist an anchor. It never dials cellular. The lower bound is explicitly
  not precise current time; offline elapsed time remains unknowable without
  a working RTC. Portal/time failures leave IPv4 relay active with a warning.

Current physical evidence and final acceptance counts are recorded separately;
no successful unit test is a substitute for actual hotspot radio/client traffic.


### Physical matrix evidence (same date)

USB-authorized B31 testing uses the ignored, loopback-only physical harness
while the formal agent is stopped; there are never two active network owners.
The release `/data/zte-agent` remains the callback executable. Reboot runs the
formal factory-authenticated agent through its unchanged rc.local launcher.
This is actual radio STA + actual br-lan/veth client traffic, not a phone radio
client; phone 2.4/5 GHz acceptance is explicitly deferred by the owner.

- Cellular is manually disabled throughout traffic testing (the owner's SIM
  has no remaining data allowance). Internet/cellular fallback throughput is
  excluded, not passed. Every inspected loss/reboot state retains enable=0.
- Saved upstream W: direct and an existing UDP-capable proxy in global mode
  pass router/client DNS, IPv4/IPv6 HTTPS and non-53 UDP on both families.
- Explicit Relay disconnect and simulated STA loss with core off/on converge
  without retained journals; verified HTTPS fails offline, then resumes after
  scan/reconnect. Core runtime global mode survives the transitions. Firmware
  serves its own management HTML for offline plain HTTP, so HTTP 200 alone is
  not used as Internet evidence.
- Active Relay reboot first reproduced the clock reset to January 2025.
  Active Relay+core reboot after the fix brings back the formal panel in about
  78 seconds, one instance, unauthenticated API 401, successful vendor sync,
  no STA/TUN/owned journals, and cellular still disabled. Precise UTC is restored
  after reconnection (about one second from the host); all eight direct and all
  eight proxy data-plane checks then pass. Time is only a lower bound offline.
- Some first-attempt public IPv4 UDP and Apple IPv6 HTTPS probes fail, followed
  by successful repeats; a second IPv6 HTTPS target also succeeds. These are
  retained as intermittent observations, not silently counted as first-pass
  successes or definitively attributed to the upstream. This is not a claim
  of packet-loss-free or seamless socket migration.

Host checks: 1070 unit tests + one startup integration pass, three intentionally
ignored; seven installer tests pass. Browser network UI passes 320/390/1440 and
no-JavaScript fallback. Clippy retains 23 baseline test-target warnings, none in
these changed network modules. Disposable OpenWrt lifecycle/RA packet tests and
actual Mihomo bridge transitions pass, including the new retained-prefix and
profile-runtime regressions. The broad 20-cycle VM/10-cycle physical endurance
threshold in the target design has not been completed.


Final delivery uses `setup.sh --build --adb --factory-password --no-ssh`.
Device executable hash matches the final local ARM64 release; section Total
8,452,748 bytes, gzip -n -9 4,316,916 bytes. Final formal agent is one process,
Relay/Mihomo off, cellular manually off, UTC within one second of the host.
Both main APs are enabled. Temporary physical harness, veth/client namespace,
USB forwards and this task's redundant on-device install backups are removed;
validated rollbacks and private evidence remain on the computer. Profiles,
saved Wi-Fi, source YAML, DHCP/network configuration and boot launcher are
preserved; only a scheduled subscription `attempted` timestamp changed.

The final extra probe batch again saw intermittent public-target failures
(Apple IPv6, one proxied TLS EOF, non-53 IPv4 UDP), followed by successful
retries. Microsoft verified HTTPS passes both families from router and client;
Cloudflare passes through the proxy but native upstream access can fail.
These results do not justify a blanket "all packets/tests are flawless" claim.
The owner explicitly deferred phone-based radio acceptance while away.
