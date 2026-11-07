# Wi-Fi relay IPv6 / NAT66 assessment

Date: 2026-10-25. Scope: design and primary-source research, not an implemented
feature or hardware acceptance. No router settings or production code changed.

Historical assessment: this describes the IPv4-only baseline before the user's
subsequent request to implement upstream IPv6 and downstream NAT66. The local
Rust implementation now exists; see [network-extensions.md](network-extensions.md)
for its actual behavior and current acceptance limits. The feasibility/benefit
analysis below remains useful, but its "current implementation" section is not
the latest implementation status. Later read-only B31 checks confirmed odhcp6c,
odhcpd, iproute2 6.3.0 and an IPv6 NAT table/MASQUERADE userspace target; no B31
IPv6 forwarding or hotel IPv6 Internet acceptance has yet passed.

## Verdict

Stateful IPv6 masquerading is feasible in Linux/OpenWrt, but is not a one-rule
extension to this relay. Recommend an **optional, capability-gated, default-off
experimental mode**, after IPv4 relay acceptance. For hotel Wi-Fi plus an
IPv4-connected Mihomo node, its incremental benefit is usually modest; for
native IPv6 DIRECT destinations it can be useful. These are engineering
judgments, not device measurements. OpenWrt's own NAT6 guide assumes a working
router IPv6 connection and downstream addressing, and prefers alternatives where
available. [1]

Separate two deliverables:

1. **Router-native upstream IPv6:** lets Mihomo reach an IPv6-only node without
   providing downstream NAT66. LAN TCP/UDP successfully intercepted by TPROXY
   and handled as Mihomo `DIRECT` also becomes a core-originated connection,
   so it can benefit from this stage. That depends on working client IPv6
   addressing and TPROXY; it does not cover bypassed traffic, other protocols
   or Mihomo being off. This is a smaller scope, although upstream firewall,
   route ownership, DNS and recovery still need implementation. Mihomo's
   `Direct` adapter explicitly opens its own TCP/UDP sockets. [20]
2. **Downstream IPv6 relay:** adds LAN RA/SLAAC, routing, firewall and NAT66 so
   clients can use native IPv6 DIRECT paths. This is the larger scope.

Mihomo documents `ip-version` for the node's outbound connection separately from
the destination address being proxied. An IPv6-capable proxy exit reached over
IPv4 is not evidence of native Wi-Fi IPv6, and a router reaching an IPv6 node is
not evidence of downstream NAT66. Protocol/server capabilities still matter. [2]

## Current implementation

Source inspection of `src/wifi_relay.rs` shows deliberate IPv4-only behavior:

- `prepare()` writes `disable_ipv6=1` on the owned `u60sta0` STA interface.
- `input_guards()` drops **all** IPv6 INPUT from that interface; unlike the
  IPv4 branch, it does not admit established traffic or configuration traffic.
  Enabling RA acceptance alone would therefore still leave RA/NDP blocked.
- `relay_plan()` installs `U60_RELAY_V6`: nonlocal LAN IPv6 forwarding is
  rejected; router IPv6 OUTPUT via `rmnet+` is also rejected except ULA
  destinations. The purpose is to prevent silent cellular IPv6 bypass.
- Status explicitly returns `ipv6_relay: false`. There is no upstream IPv6
  lease/lifetime model or LAN RA ownership in this module.
- `connect()` requires a usable IPv4 DHCP lease. Supporting an IPv6-only hotel
  uplink also needs a different connection state model, not just NAT66 rules.

`src/mihomo.rs::proxy_plan()` separately installs IPv4/IPv6 TCP/UDP TPROXY and
DNS interception. Captured proxy traffic can be delivered locally before the
relay's FORWARD guard; this does not add native IPv6 upstream connectivity.
Removing the relay guards or just setting `ipv6: true` is not a safe upgrade.

## Expected benefit by scenario

| Verified upstream and workload | Benefit from downstream NAT66 | Better interpretation / limitation |
| --- | --- | --- |
| Hotel offers only IPv4 | None | NAT66 cannot create an IPv6 Internet uplink. |
| Working dual-stack Wi-Fi, clients need IPv6 without Mihomo or bypass it | Moderate, workload-dependent | Enables IPv6 destinations without an upstream delegated prefix; clients may still prefer IPv4. |
| IPv6 TCP/UDP is already captured and handled as Mihomo DIRECT | Low additional benefit over router-native IPv6 | A core-originated DIRECT connection can use native IPv6 without downstream NAT66, conditional on working TPROXY/client IPv6. |
| Most traffic uses an IPv4 Mihomo node with an IPv6-capable exit | Usually low | Proxying an IPv6 destination and using IPv6 to reach the node are different layers. [2] |
| Mihomo node itself is IPv6-only | No additional node-access benefit over router-native IPv6 | Implement native router egress first; downstream NAT66 is not required for this use case. |
| Wi-Fi is IPv6-only, client applications require IPv4 | Not a complete solution | NAT64/464XLAT or an appropriate proxy path is a separate design; NAT66 translates IPv6 to IPv6. [3][4] |
| Hotel delegates a usable routed prefix | Avoid NAT66 where possible | Route a delegated LAN /64 with a stateful firewall instead. [5] |
| Hotel gives only an on-link /64, no delegation | Potential NAT66 use case | OpenWrt also suggests IPv6 relay; RA/DHCPv6/NDP relay is a separate alternative, not the existing IPv4 AP+STA function. [1][6] |
| Goal is faster Wi-Fi or unrestricted P2P/inbound | No assured benefit | Translation does not improve the radio or restore end-to-end inbound reachability. Do not promise throughput, latency or hardware offload. [7] |

ULA-only LAN clients deserve special attention. RFC 6724 gives ULA a different
address-selection label from global IPv6. Its destination Rule 5 prefers
matching source/destination labels before Rule 6 compares precedence. With a
ULA IPv6 source and working IPv4, some clients will therefore prefer IPv4 for
dual-stack names. NAT66 working does not imply most traffic switches to IPv6;
test actual Android/iOS behavior. Do not invent an unassigned public prefix to
manipulate this preference. [8]

## Required design, not just a MASQUERADE rule

1. **Probe target capabilities and upstream separately.** Linux v5.15 source
   has IPv6 NAT/conntrack and MASQUERADE support, but it is configurable. This
   does not establish that B31 shipped the required modules/userspace backend.
   Verify the device's actual IPv6 NAT table, MASQUERADE target, conntrack and
   routing features, without flushing vendor tables. Linux's IPv6 MASQUERADE
   implementation selects an egress address and drops if it cannot obtain one.
   Require a usable upstream IPv6 address, nonexpired default route and
   successful IPv6 Internet checks, not merely a link-local address or the
   existence of `/proc/sys/net/ipv6`. The address is commonly global unicast;
   an upstream that itself translates a ULA is possible, so verified
   reachability is the deciding criterion. [9][10]
2. **Own upstream configuration safely.** Enable IPv6 only on the owned STA;
   verify forwarding and accept RA while forwarding (`accept_ra=2` if using the
   kernel RA client). Linux normally ignores RA on a forwarding interface
   unless this override is used. Admit valid RA/NDP and necessary ICMPv6 under
   narrow interface rules; keep hotel access to router services denied.
   DHCPv6, if used, needs its own controlled exception. Track default-router,
   address, prefix and DNS lifetimes. Do not let an automatically learned
   default route silently override the vendor/cellular main-table policy.
   Do not assume a vendor `accept_ra_rt_table` sysctl exists; it is absent from
   the cited upstream v5.15 sysctl documentation. [11][12]
3. **Establish one LAN RA/addressing owner.** Identify the live vendor RA
   service and existing cellular prefixes before changing anything. The agent
   currently has no RA manager; starting a second advertiser can leave
   competing defaults or stale cellular global addresses. Use a stable,
   properly generated RFC 4193 ULA /48 and a LAN /64, with RA/SLAAC and usable
   DNS. Do not rely on a DHCPv6-address-only LAN: SLAAC is the portable baseline;
   Android's current AOSP `IpClient` explicitly provisions IPv6 using RA and
   autoconfiguration (and also contains newer DHCPv6-PD support, so avoid the
   blanket claim that Android has no DHCPv6). Address and default-router
   lifetimes are distinct; do not assume a ULA prefix alone makes the existing
   advertiser announce a usable default route for NAT66. [1][5][13][14][15]
4. **Keep routing, NAT and leak prevention coordinated.** Use owned IPv6
   rules/tables and narrow LAN-ULA-to-STA masquerading with stateful return
   rules. Preserve local, link-local, multicast and management reachability;
   reject unintended cellular egress for both forwarded and local traffic.
   Deal explicitly with any old cellular LAN source prefix rather than
   translating ULA alone and assuming clients stopped using the old prefix.
   Keep Mihomo's marked local TPROXY route ahead of relay policy, do not loop
   node connections, and test all feature enable/disable orders. Include
   dnsmasq/router-originated IPv6 DNS and external client DNS, not just packets
   associated with AAAA answers; receiving an AAAA answer and routing a DNS
   query over IPv6 are different things. Preserve the existing undo journal
   and shared network lock, without changing boot-sync vendor daemons.
5. **Preserve essential ICMPv6 and PMTU.** Validate scoped RA/NDP per RFC 4861,
   and permit necessary ICMPv6 errors in INPUT and transit paths. Packet Too
   Big filtering can black-hole larger transfers; a small ping or TCP page
   load is not acceptance for IPv6 UDP/QUIC or PMTU. NAT66 is not NPTv6:
   RFC 6296 describes stateless, one-to-one prefix mapping without port
   mapping, not sharing one hotel address among clients. [3][12][16][17]
6. **Treat portal and lifecycle state as part of the feature.** An IPv4 portal
   login is not proof of an authorized IPv6 path. RFC 8910 permits portal API
   discovery via IPv4 DHCP, DHCPv6 and RA, and recommends consistent URIs;
   RFC 8908 notes that enforcement/API identity can depend on the client's IP
   addresses. Inspect actual hotel behavior and test both families before
   advertising usable IPv6 Internet. This is not evidence that this hotel
   implements CAPPORT or authorizes the families differently. On loss or
   expiry, withdraw the applicable default-router advertisement, enforce the
   selected fallback policy and restore owned state. A zero RA Router
   Lifetime withdraws default-router status, not automatically every SLAAC
   address. [18][19][12]

NAT is not a security boundary by itself. Stateful filtering must remain, and
neither NAT66 nor the current relay implies a fail-closed proxy/anonymity mode.
Extra conntrack state, NAT processing, RA lifecycle and vendor interactions add
complexity; CPU, battery, speed and offload effects require measurement. [7][9]

## Staged recommendation and acceptance gates

1. Finish real-device IPv4 relay, portal and Mihomo acceptance first.
2. Add read-only IPv6 capability/upstream status, then optional router-native
   Wi-Fi IPv6 with owned routes, input exceptions, DNS policy and rollback.
3. Only if native DIRECT IPv6 is wanted and the target passes the probes, add
   downstream RA/SLAAC + NAT66 behind a separate default-off experimental
   switch. Prefer a routed delegated prefix when available. UI should
   distinguish `unsupported`, `no upstream IPv6`, `configured/unverified`,
   `portal restricted` and `verified`; do not label a merely enabled rule
   "IPv6 connected".
4. Acceptance must cover LAN IPv6 TCP and UDP DIRECT, proxy IPv6 destinations
   over IPv4 node transport, IPv6-only node transport, actual source/exit
   addresses, DNS over both families, portal before/after login, large-packet
   PMTU/QUIC, RA/address/route expiry, AP/vendor reload, SIM fallback, both
   feature start orders, core failure and crash/restart undo. Ensure no
   unintended `rmnet*` traffic while the Wi-Fi IPv6 policy is selected.

**Evidence gaps:** no connected ADB device is available in the current session;
no new device probes were performed. B31 NAT66 modules/backend, current LAN RA
owner/prefixes, Wi-Fi IPv6 forwarding and performance remain unverified.
`JinJiangRewards` IPv6 availability, RA/default route, DHCPv6-PD and portal
authorization have not been measured. Earlier IPv4 association/DHCP probes do
not establish any of these. Thus the justified priority is **medium/optional**,
not a claim that IPv6 relay already works or must be enabled for Mihomo.

## Primary sources

[1]: https://openwrt.org/docs/guide-user/network/ipv6/ipv6.nat6
[2]: https://wiki.metacubex.one/config/proxies/
[3]: https://www.rfc-editor.org/rfc/rfc6296.html#section-1
[4]: https://www.rfc-editor.org/rfc/rfc6877.html#section-5
[5]: https://www.rfc-editor.org/rfc/rfc7084.html#section-4.3
[6]: https://openwrt.org/docs/guide-user/network/ipv6/configuration
[7]: https://www.rfc-editor.org/rfc/rfc5902.html#section-2
[8]: https://www.rfc-editor.org/rfc/rfc6724.html#section-6
[9]: https://github.com/torvalds/linux/blob/v5.15/net/ipv6/netfilter/Kconfig
[10]: https://github.com/torvalds/linux/blob/v5.15/net/netfilter/nf_nat_masquerade.c
[11]: https://github.com/torvalds/linux/blob/v5.15/Documentation/networking/ip-sysctl.rst
[12]: https://www.rfc-editor.org/rfc/rfc4861.html
[13]: https://www.rfc-editor.org/rfc/rfc4193.html#section-3
[14]: https://www.rfc-editor.org/rfc/rfc4862.html#section-5.5.3
[15]: https://android.googlesource.com/platform/packages/modules/NetworkStack/+/refs/heads/main/src/android/net/ip/IpClient.java
[16]: https://www.rfc-editor.org/rfc/rfc4890.html#section-4.3.1
[17]: https://www.rfc-editor.org/rfc/rfc8201.html#section-6
[18]: https://www.rfc-editor.org/rfc/rfc8910.html#section-2
[19]: https://www.rfc-editor.org/rfc/rfc8908.html#section-4
[20]: https://github.com/MetaCubeX/mihomo/blob/v1.19.33/adapter/outbound/direct.go

The cited Linux v5.15 sources establish implementation mechanics, not the
vendor kernel configuration. The AOSP main-branch source establishes current
implementation behavior, not a guarantee about every deployed Android release.
