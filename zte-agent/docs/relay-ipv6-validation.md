# IPv6 relay / NAT66 validation

## 2026-10-27 runtime RA update

The historical results below predate the runtime RA/DNS cutover. The new release
is installed via setup.sh with factory-password auth and one rc.local hook.
Full current evidence, limitations and the physical reboot outcome are in
[network-integration-review.md](network-integration-review.md#runtime-ra-cutover-and-physical-reboot-2026-10-27).
Normal Relay no longer changes DHCP/UCI RA options or restores old cellular DNS.
Actual OpenWrt+B31 RA packets, W AP+STA, downstream NAT66, real Mihomo dual-stack
HTTPS/non-53 UDP, manual-cellular-off link loss and reboot-then-reconnect pass.
Cellular itself has no data registration in this round; real cellular IPv6
fallback and long-running RF-loss/phone-band tests remain unverified.
The final host run passes 1,068 unit tests and one startup integration test.

## Historical validation

Updated: 2026-10-26. Local WIP against
`734fe7eff816d6af922501c8a0e298ed6d38004c`, including new untracked modules.
No integration commit or production/autostart installation has been made.
An explicitly gated, 30-minute temporary test agent has exercised the connected
B31's physical Relay and core. This is partial acceptance, not production readiness.

## Implemented contract

The Relay connection form defaults optional upstream IPv6/NAT66 on, with an
explicit off checkbox. Usable upstream addresses and a live default router,
target capabilities and safe LAN RA ownership are required. Supported IPv6
configures native Wi-Fi IPv6 for router/Mihomo sockets and downstream SLAAC
with a stable random ULA /64 plus stateful NAT66. Otherwise IPv4 relay remains
available and cellular IPv6 is rejected. IPv6-only upstreams are not supported:
the parent relay still requires an IPv4 DHCP lease.

Mihomo's managed configuration permits IPv6 and its higher-priority marked TUN
policy remains before relay routing. This does not make every request use a
proxy node: DIRECT/private exclusions remain, and real node UDP/IPv6 capability
is independent of NAT66. The UI says configured / Internet not verified; neither
SLAAC nor a default route proves hotel authorization or Internet access.

## Evidence and limits

| Layer | Observed result | What this does not prove |
| --- | --- | --- |
| macOS Rust tests | 1022 passed, 0 failed, 2 pre-existing ignored | Target kernel/driver behavior |
| Linux-only full lifecycle | 1 explicitly ignored test run and passed in a disposable OpenWrt chroot | Physical AP+STA, service manager or real Internet |
| Rule-plan traffic | Real TCP/UDP from downstream ULA and retained global sources is NAT66-translated to the Wi-Fi source; router-native global and upstream ULA traffic works | Real Mihomo protocols/nodes, performance or physical-device NAT |
| Policy and rollback | Actual generated Mihomo marked route selects the owned TUN before relay policy; finite route/address updates and reverse undo succeed; cellular main routes unchanged | Firmware's own reload/suspend races |
| DHCPv6 discovery | Official OpenWrt odhcp6c invokes the real Rust executable, parsing source/default/on-link/DNS/MTU; no main-table default/global address is learned by discovery | Hotel policy or B31 client modifications |
| LAN RA | Packet captured from actual OpenWrt odhcpd advertises only the relay ULA /64 and router lifetime 90; original UCI restored without relay deltas; external conflict preserved | Phone SLAAC, B31 odhcpd/netifd integration, immediate client deprecation |
| Browser | Actual Rust-rendered pages, 28 page/viewport cases at 320/375/390/1440 px, no overflow; pinned mock dashboard connects without Setup | A router, real core or authorized hotel session |
| JavaScript | Dashboard adapter tests: 4 passed | Full upstream dashboard feature compatibility |
| Build/lint | ARM64 musl stripped static ELF; Clippy succeeds with 23 baseline warnings; diff whitespace check passes | A clean repository-wide formatting baseline |
| Real core / real Linux bridge | Seven uplink stages, persistent UDP, IPv4/IPv6 TCP and 1400/2000-byte UDP, local address renumbering, no-cellular-route offline state, stalled controller restart and exact cleanup pass; bridge filtering remains enabled | Actual radio driver or paid-node transport |
| B31 isolated bridge | Same production Rust/core runs on B31, actual bridge, INPUT/FORWARD default DROP; cellular/relay/dual-stack/offline/reconnect transitions and rollback pass | Physical cellular switch, RF loss or phone behavior |
| Physical B31 | `W` connects, upstream IPv6 configures, actual odhcpd advertises ULA, native and downstream IPv4/NAT66 work with core off; stable core on with supplied Hysteria2 node gives HTTP 200 over both families and real non-port-53 UDP replies over both families | Every node/protocol, throughput, portal authorization or long-run recovery |
| Phone | User reports basic browsing works while connected to the U60 test hotspot | IPv6 score and which/both radio bands were not specified |

The full lifecycle test runs the production `Upstream::start/tick/stop` methods,
not merely hand-built commands. It tests optional-off/no-upstream, configuration,
same-path refresh, rebuilding a deleted finite default, disconnect/reconnect,
stale previous-session update/start/stop callbacks, lease withdrawal and exact
original LAN configuration/main-route restoration. It executes actual Rust DoH
cleanup while temporary RA options exist and confirms the persistent DHCP file
was not committed. The Linux-only mutating test is not run by ordinary tests.

The isolated environment uses an ARM64 Linux VM kernel and Debian netfilter
userspace, with official OpenWrt 23.05.5 client/UCI/odhcpd. There is no procd in
the chroot; reload sends SIGHUP to the real odhcpd. Lifecycle QA stubs only the
dnsmasq restart action, not the Rust cleanup or UCI commands. All containers
use `--network none`, disposable namespaces and read-only artifact bindings;
no host networking or physical router settings are used.

Repeatable rule-plan/traffic and full lifecycle runners are in
[tests/relay-ipv6](../tests/relay-ipv6/README.md). Official OpenWrt artifacts are
SHA-256-pinned in that directory. Separate one-off callback/RA packet probes
were also run locally; they are supplemental evidence, not a claim that every
probe is covered by the committed runner.

## B31 findings and physical test scope

B31 is OpenWrt 23.05.4, Linux 5.15.194-perf, not an Android userspace despite ADB.
The stock kernel lacks `xt_addrtype`; exact local-address exclusions replace it.
Actual br-lan bridge traffic exposed an additional failure: TPROXY marked packets
reached INPUT but the bridge/L3 transition discarded their assigned proxy socket.
Disabling global bridge netfilter was only a diagnostic and was restored.
The implementation now uses owned TUN routing, verified with both bridge hooks on.

Physical acceptance uses a test-only loopback HTTP server and only the network
watchdogs, not the normal agent's other services. The 30-minute gate automatically
stops Mihomo and disconnects Relay. USB forwards carry authentication; no
subscription URL, node credential or Wi-Fi password is written to this log.
The real br-lan synthetic client receives actual DHCP/RA rather than static
fixture addresses. Explicit rule counters and stable core PID distinguish proxy
success from fail-open traffic during restart gaps.

The original wireless/DHCP UCI view was restored after the first test round;
unrelated vendor deltas were not reverted. Both global bridge filter switches
remain enabled. No modem enable command has been called. Initial before/after
readbacks both showed cellular enable=1. Manual-cellular-off and physical RF-loss
acceptance were subsequently coordinated: user-disabled cellular remained 0
through station disconnect, recovery and reconnect. The first pass exposed a
vendor DHCP package commit and the second exposed the core's no-global-address
IPv6 probe; both gained targeted fixes/regressions. Physical AP power-loss is
still distinct from the station-disconnect simulation.
JinJiangRewards captive authentication has not been accepted.

Final physical repeat: the patched core starts with cellular disabled and no
upstream GUA/ULA; station loss cleans Relay and restarts the core without changing
cellular enable=0. After an explicit scan/reconnect to `W`, both HTTPS families
return 200 and both non-53 UDP probes receive answers through stable marked TUN
rules. Allow roughly 15-25 seconds for the observed cleanup/restart/IPv6 path
transition, not an 8-second fixed sleep; transient status=off during a controlled
restart is not evidence of a permanent core failure.

## Release-size budget

The final ARM64 production executable is statically linked and stripped:

- `llvm-size -A` section Total: 7,446,104 bytes.
- `gzip -9` output: 3,796,919 bytes.
- Existing `app.css`: 49,026 bytes, below 48 KiB; no CSS added for IPv6.

Cross-compilation on macOS uses external temporary Zig musl wrappers, without
changing repository build configuration. Repository-wide `cargo fmt --check`
has extensive pre-existing differences; only the new IPv6 Rust module is
formatted, without mass-reformatting unrelated code.

## Device acceptance gates

1. Keep USB recovery available and use a controlled dual-stack AP first.
   Verify optional-off and no-IPv6 upstream still provide IPv4 relay, and no
   router/LAN IPv6 connection, including ULA resolvers, leaves rmnet.
2. Verify U60-native IPv6, actual Mihomo DIRECT TCP/UDP and IPv6-only node
   transport, source/exit addresses and selected upstream. Exercise both feature
   enable orders and switching Mihomo off while retaining native NAT66.
3. Connect phones through each U60 hotspot band. Verify SLAAC, DNS/RDNSS,
   TCP/UDP NAT66 with both the new ULA and retained global addresses, PMTU and
   low-MTU UDP, and rejection of unsolicited hotel-side management/forwarding.
4. Renew, renumber and expire IPv6 while IPv4 remains usable. Check RA/router
   withdrawal, DNS fallback, policy precedence, no global main-table changes,
   exact option restoration and no unintended committed UCI deltas.
5. Test link loss, client exit, agent crash/restart, vendor reload, suspend and
   reboot. Recovery must preserve external edits and retain evidence rather
   than overwrite conflicting state. These paths can require USB intervention.
6. Authorize JinJiangRewards on a phone connected to the U60, initially without
   Clash. Verify IPv4 and IPv6 authorization separately before enabling proxy
   rules; an HTTP page or DHCP lease alone is insufficient.

Throughput, power/temperature, QUIC/games, long-idle reliability and a second
B31 device remain unverified. Loss of the selected relay can restore cellular
service and incur charges; this is not an anonymity kill switch.
