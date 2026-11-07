# Disposable IPv6 relay acceptance

These tests mutate Linux networking and OpenWrt UCI. **Never run them directly
on the U60, a host, a shared container or with host networking.** The runners
require an otherwise empty Docker network namespace and only bind read-only
test artifacts. `--privileged` is needed for nested namespaces and chroot
mounts; use a disposable development Docker VM.

## Exact rule plan and TCP/UDP traffic

From the repository root, with Docker and Rust installed:

```sh
QA="$(mktemp -d)"
export QA
U60_IPV6_QA_PLAN="$QA/plan.json" cargo test -p zte-agent export_plans_for_isolated_linux_acceptance
docker build -t u60-relay-ipv6-qa:local zte-agent/tests/relay-ipv6
docker run --rm --privileged --network none \
  --sysctl net.ipv6.conf.all.disable_ipv6=0 \
  -v "$PWD/zte-agent/tests/relay-ipv6:/qa:ro" \
  -v "$QA/plan.json:/plan.json:ro" u60-relay-ipv6-qa:local
```

The plan is exported from the production Rust generators, not reimplemented in
Python. The test checks IPv4-only cellular IPv6 rejection, downstream ULA and
retained-global-source TCP/UDP NAT66, router-native IPv6 TCP/UDP including an
upstream ULA destination, finite-lifetime update commands, Mihomo marked-route
precedence and reverse rollback without changing cellular main routes.
The internal DAD step is emulated by checking actual kernel address flags.
There is **no actual Mihomo core, node or Internet service** in this test.

## Complete OpenWrt client/lifecycle

Requires an ARM64 Linux Docker VM, an already configured ARM64 musl Rust linker,
and Python 3.12+ for safe package extraction. On macOS, the Homebrew Python is
usually `/opt/homebrew/bin/python3`; the system Python may be too old.

```sh
python3 zte-agent/tests/relay-ipv6/prepare_openwrt.py "$QA"
cargo build -p zte-agent --release --target aarch64-unknown-linux-musl
cargo test -p zte-agent --release --target aarch64-unknown-linux-musl \
  --no-run --message-format=json > "$QA/test-artifacts.jsonl"
AGENT_TESTS="$(python3 -c '
import json, os
with open(os.environ["QA"] + "/test-artifacts.jsonl") as f:
    for line in f:
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == "zte-agent" and item.get("profile", {}).get("test") and item.get("executable"):
            print(item["executable"])
')"
test -n "$AGENT_TESTS"
docker run --rm --privileged --network none \
  --sysctl net.ipv6.conf.all.disable_ipv6=0 \
  -v "$PWD/zte-agent/tests/relay-ipv6:/qa:ro" \
  -v "$QA:/cache:ro" \
  -v "$PWD/target/aarch64-unknown-linux-musl/release/zte-agent:/agent-bin:ro" \
  -v "$AGENT_TESTS:/agent-tests:ro" \
  --entrypoint /bin/sh u60-relay-ipv6-qa:local /qa/openwrt.sh
```

The manifest pins official OpenWrt 23.05.5 rootfs and ip-full dependencies by
SHA-256. The network-disabled runner unpacks a disposable chroot, sends real
RAs over veth, and runs the explicitly ignored Rust lifecycle test against real
`odhcp6c`, the actual release executable as its callback, UCI and kernel routes/
firewall. It covers off/no-upstream/start/configure/refresh, recovery when an
owned finite route is lost, disconnect, reconnect, stale callbacks, IPv6 lease
withdrawal and restoring the original configuration/main routes.

Use the same command with `-e U60_OPENWRT_QA_MODE=runtime-ra` to run
`ra_runtime.py`. It uses actual netifd, one odhcpd and a bridge client to assert
RA prefix/lifetime/MTU/RDNSS, vendor edits+commits, same-boot crash cleanup,
unrelated runtime data and fresh configuration after netifd replacement.
MTU cases assert 1480 from the upstream, a changed upstream value of 1400,
and a 1360 LAN cap on a 1480 upstream. The full lifecycle test also changes
the LAN link MTU while a real odhcp6c session is running and verifies the
supervisor rebuilds the advertised value without persisting DHCP edits.
Normal RA operation creates no UCI delta or persistent DHCP write. JSON null
releases typed overrides; it does not delete keys from netifd's runtime map.

`b31-ra-runtime.sh` is the explicitly gated physical-firmware equivalent:
push only its named test executable/script into `/tmp/u60-ra-runtime-qa`, then
invoke over USB. It uses private mount/network namespaces, private config and
ubus; it never operates physical br-lan, Wi-Fi or SIM. Pull results/*.pcap and
assert decoded packet fields before accepting the adapter. Cleanup unmounts and
removes namespaces; archive evidence on the computer and remove the owned QA
directory afterwards. It must not be run as an ordinary test or boot hook.

The test also invokes the real Rust DoH DNS cleanup while RA deltas are active,
asserting that `/etc/config/dhcp` is not committed and recovery leaves no relay
deltas. In this chroot only the `dnsmasq restart` action is stubbed. There is no
procd, so odhcpd reload signals the real daemon directly. Netfilter userspace
comes from Debian; the DHCPv6 client/UCI/odhcpd come from OpenWrt. This does not
verify the B31 service manager, Wi-Fi driver, kernel modules or hardware NAT.

Ordinary `cargo test` never executes the mutating test. It is Linux-only and
requires both an explicit environment marker and a disposable-chroot marker.

## Actual core on a real bridge

Supply the pinned ARM64 Mihomo executable as `$QA/mihomo` (v1.19.33, verify the
archive SHA-256 in `src/mihomo.rs`) and the cross-built test artifact above:

```sh
docker run --rm --privileged --network none \
  --sysctl net.ipv6.conf.all.disable_ipv6=0 \
  -v "$PWD/zte-agent/tests/relay-ipv6:/qa:ro" \
  -v "$QA/plan.json:/plan.json:ro" \
  -v "$QA/mihomo:/core-bin:ro" \
  -v "$AGENT_TESTS:/agent-tests:ro" \
  u60-relay-ipv6-qa:local python3 /qa/real_core.py
```

This runner creates an actual Linux bridge with a member veth, asserts both
bridge-netfilter switches remain on, and applies default-DROP INPUT/FORWARD.
A veth only named `br-lan` is not an adequate substitute: it missed the B31
TPROXY/bridge failure. The current managed TUN uses `mixed` (kernel TCP,
gVisor UDP), not the earlier all-gVisor stack. The runner exercises whichever
stack the supplied current test artifact generates. Its acceptance cases cover
dual-stack TCP, UDP 1400/2000 bytes (including source fragmentation), persistent
UDP across uplink changes, local-address addition/removal, offline cellular state
and controller failure. Earlier passing gVisor runs do not establish that a new
mixed-stack build passes; rerun with matching current artifacts. Cleanup asserts
the TUN and table/rule references disappear.
It uses DIRECT nodes and controlled network namespaces, not paid proxy servers.
The core test also creates two profiles, switches them while running, applies
global Merge+Script, and asserts the managed rule order before uplink transitions.
Address-change acceptance waits for bounded rule and actual traffic convergence,
not a fixed sleep that can observe the core restart's intermediate firewall state.

## DHCP reservations

`reservations.sh` runs only in a disposable ARM64 OpenWrt container, with
`--privileged --network none` and the cross-built test binary mounted at
`/agent-tests`. It starts real dnsmasq+udhcpc on a veth/bridge, invokes production
Rust add/remove, asserts the requested fixed lease, refuses a duplicate address,
and checks unrelated `/etc/ethers` bytes and the dnsmasq PID survive unchanged.
No physical client or host network is used. The B31 client-renewal and UI test
still needs a user-connected hotspot client.

`b31-isolated.sh` is a separately gated USB runner for the intended physical B31.
It builds four temporary namespaces and a private chroot, binds only `/dev/null`
and `/dev/net/tun`, and uses the same actual bridge/core lifecycle. It does not
change physical Wi-Fi/SIM routes. Prepare its named artifacts and inspect the
script before running; it deliberately refuses preexisting namespace/root names.

## Include the real port-9090 panel

Loopback acceptance alone is insufficient for browser/agent integration. This is
a manual QA procedure, not the normal `setup.sh` installation/update workflow.
For a separately authorized manual round, start the actual release executable on
9090 with a private agent password and no init.d/boot-entry changes. Before
starting, check for an existing listener/process and saved optional-service
configurations; do not replace an owner installation or enable modem recovery
policies for QA. Current agents attempt to restore Mihomo's enabled marker and
Relay's last successfully connected remembered/open network. Saved credentials
alone, including on upgrades from older agents, are not a resume choice. Off and
Disconnect clear the respective choice; runtime relay loss does not auto-reconnect.
Keep credentials in a mode-0600 file, not command history, logs or this repo.

Verify from a LAN client (not merely ADB forwarding): login, Router, Relay and
Mihomo setup; then exercise the real browser scan/connect/disconnect forms,
Clash start/stop, Zashboard direct entry without Setup, and old-link refusal when
off. Require dual-stack TCP/UDP local DNS readiness, inspect recovery state and
confirm the original AP geometry/DHCP settings and cellular enable value after
cleanup. An authenticated desktop browser through USB can supplement this with
viewport/JS/WebSocket checks but does not prove radio client connectivity.

For the manual process started by this QA round, use its recorded PID only.
Before stopping it, turn both extensions off through the API/UI and verify no
owned journals remain; only then stop that process. After an actual process
crash, with no live extension process, run the same executable's
`network-recover`. Do not run another agent or recovery concurrently with active
network extensions. Record clearly whether the panel is left running for the
owner, and where the private agent password is stored.
