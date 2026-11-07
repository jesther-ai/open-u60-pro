# Relay and cellular recovery plan

Date: 2026-10-27 (America/Los_Angeles).
Scope: Rust agent, Wi-Fi Relay, cellular fallback, IPv6 RA/NAT66, Mihomo and
the device web UI. No companion-app UI changes. This is a target design and
acceptance plan, not a claim that all stages have been implemented or deployed.

## Recommended Refinement After the Reboot Failure

Status: runtime RA/DNS cutover implemented and physically validated on B31,
including reboot while connected. The broader coordinator in section 4 remains
proposed, not required for this focused cutover.

The latest B31 reboot left no STA/TUN and restored every saved LAN RA option
except `dns`: the old cellular DNS list was absent in the new boot. This is
valid new vendor state, not evidence of a relay configuration leak. The former
all-options-equal-to-old-baseline gate nevertheless blocked scanning and cleanup.
A disposable OpenWrt/UCI test reproduced that error. The local hotfix now passes
that test and the full client/lifecycle test, but it is only an interim repair.

Recommended steady-state model: observe current uplinks, select one, remove only
our obsolete effects and build the required runtime effects. Do not restore old
addresses, gateways, upstream DNS or cellular intent. A cellular PDN is vendor
managed, not an odhcp6c lease: read its current status and wait for vendor events,
not disconnect/reconnect it merely because Wi-Fi was removed. If cellular was
disabled, remain offline when Wi-Fi disappears.

Wi-Fi uses the existing fresh odhcp6c RA/DHCPv6 observations; cellular uses the
vendor's current IPv6 address, routes, DNS and enable state. A valid observation
need not be renewed on every transition. No usable v6 means IPv4-only while
discovery continues; no Wi-Fi-to-cellular v6 bypass while Wi-Fi owns the uplink.
Mihomo follows the same selected uplink without owning either discovery client
or the cellular switch. IPv6 discovery failure must not strand working IPv4.

The existing odhcpd remains the sole RA daemon. Real OpenWrt and B31 firmware
packet tests verified `network.interface.lan.set_data` typed scalar/list flags,
prefix filtering, vendor reloads and release to current vendor settings. `{}`
does NOT remove keys. JSON null deactivates the typed odhcpd override; it is an
inert runtime tombstone, not deletion from netifd's data map. Unrelated keys are
never replaced. Nonempty preexisting overrides are refused, not snapshotted.

DNS runtime data APPENDS to UCI DNS, confirmed on wire, so do not use it. Relay
sets `ra_dns=false` and dual-stack clients use their existing DHCPv4 DNS for both
A/AAAA. This supports IPv6 data/NAT66 but deliberately not IPv6-only clients or
upstreams. Old learned client RDNSS may linger until its advertised lifetime;
the existing scoped DNS interception and cellular IPv6 guard remain necessary.
When cellular has no resolver, inject tagged current Wi-Fi resolver lines into
the volatile `/tmp/resolv.conf`, not UCI or a historical DNS snapshot. Remove
only those lines on cleanup; retain current vendor content and re-observe edits.

On Wi-Fi loss, withdraw relay-specific RA state and remove owned routes/NAT,
then let odhcpd render the current vendor state. Across reboot, runtime data is
reconstructed from current observations, never from a prior boot's lease.
Keep old snapshots only for a one-time, verified migration of any previously
committed relay values; retire that mechanism after migration. Genuine failed
cleanup still requires an explicit error, not deletion of evidence or a hidden
fallback. Do not globally flush routes, restart LAN/netifd, start a second RA
daemon, or convert cellular LAN to NAT66 merely to avoid migration work.

Primary references inspected:
- https://github.com/openwrt/odhcp6c/blob/master/README.md
- https://github.com/openwrt/odhcpd/blob/master/README.md
- https://github.com/openwrt/netifd/blob/master/ubus.c
- https://github.com/openwrt/netifd/blob/master/interface.c

The remaining sections describe the broader target and migration constraints;
they are not permission to keep expanding the snapshot-restore implementation.

## 1. User policy and invariants

- A usable, selected Wi-Fi relay is the preferred uplink. Enabling cellular
  while Wi-Fi is usable does not make cellular the preferred uplink.
- Losing/disconnecting Wi-Fi permits fallback to cellular that is ALREADY
  enabled. Recovery never enables cellular, changes airplane mode, or restores
  a stale cellular switch value saved when Relay connected.
- User intent and connectivity are separate. Cellular enabled but registering
  is not cellular disabled. A missing default route is not permission to dial.
- All user control surfaces count: agent UI, official panel and device screen.
  External vendor events must be observed; an agent-only mutex is insufficient.
- With cellular disabled and Wi-Fi unavailable, remain offline. Preserve local
  hotspot management and USB recovery. Do not reboot to manufacture a route.
- While Wi-Fi is selected, IPv6 uses Wi-Fi too. Without usable upstream IPv6,
  withdraw the relay IPv6 path rather than letting it escape through cellular.
  Remove that scoped restriction on relay teardown so normal cellular IPv6
  can resume if the user has enabled it.
- Mihomo is orthogonal: it follows the selected uplink, never owns the cellular
  on/off switch. Preserve proxy intent and valid config across an uplink change;
  restart/rebind only when needed, with explicit failure reporting.
- Core-off is direct routing, not a kill switch. A proxy download failure does
  not silently retry direct while the core is enabled.
- Hotspot AP configuration belongs to the vendor/user. Relay borrows only the
  minimum radio runtime state; preserve access to the management panel.

## 2. Evidence and why previous fixes were insufficient

The B31 reproduction was: connect Relay, disable cellular, enable cellular,
disconnect Relay. The remaining `ipv6-network.json` stopped at
`@u60-relay-ra restore`; IPv4 routing had already been removed, but IPv6 and
wireless recovery journals remained.

The vendor had committed DHCP settings and rewritten individual LAN RA options.
In the captured same-boot snapshot, `ra_useleasetime` was originally/applied `0`
but had become `1`. Some other options returned to their vendor defaults while
relay options such as the prefix filter were still persisted. There were no
pending UCI deltas. The old all-or-nothing ownership check refused the first
different value, preventing the remaining rollback.

The real OpenWrt lifecycle test was extended with that overwrite followed by a
package commit. It reproduced the same cleanup error before the fix. Updating
only the package-commit path was insufficient: both active RA maintenance and
teardown must handle partial vendor rewrites, including a disconnect before the
next watchdog tick.

Current architectural pressure points:

1. Relay, IPv6, Mihomo and modem control have separate state and timing. The
   shared network mutex serializes some effects, but not vendor processes.
2. A generic undo stack stops at one failed step. This is correct for dependent
   steps, but cannot express independent IPv4, IPv6, radio and core cleanup.
3. An IPv6 settings refresh error can currently tear down an otherwise working
   IPv4 Relay. Transient contention and permanent unsafe state need separation.
4. Recovery errors previously hid the failed step, making ordinary retry look
   like an ADB-only emergency.

## 3. State model: intent, observation and owned effects

Keep three distinct records, rather than adding more booleans to `connected`.

| Record | Examples | Authority |
| --- | --- | --- |
| Intent | selected relay, explicit disconnect, requested IPv6, proxy on/off | Latest user action; modem intent also read from vendor |
| Observation | association, lease generation, v4/v6 route, carrier state, core readiness | Fresh device observations, never inferred from a request succeeding |
| Ownership | session/boot ID, installed rules, RA values, AP runtime changes, remaining cleanup | Agent journal, verified against actual state |

Derive the effective uplink: `Wifi`, `Cellular`, `Offline` or `Transitioning`.
Expose Relay separately as `Off`, `Connecting`, `Active`, `Disconnecting` or
`RecoveryPending`. Keep `CaptivePortal/InternetUnverified` distinct from Wi-Fi
association and DHCP success; one failed Internet probe must not silently start
using metered cellular behind an authenticated hotel's Wi-Fi.

Every accepted user operation gets an ID and intent revision. Old DHCP callbacks,
downloads and delayed operations may report results but cannot override a newer
disconnect or cellular-off action. Repeated disconnect/recovery is idempotent.

| Relay | Cellular switch/observation | Expected result |
| --- | --- | --- |
| Usable | Off | Wi-Fi; no modem enable call |
| Usable | On, connected | Wi-Fi stays preferred; cellular remains available |
| Lost/disconnected | On, connected | Cellular after withdrawing owned relay routing |
| Lost/disconnected | Off | Offline; local hotspot/panel still reachable |
| Lost/disconnected | On, not connected | Wait for vendor connection; no toggle loop |
| v4 usable, v6 unavailable | Either | Wi-Fi IPv4; no cellular IPv6 bypass |
| Dual-stack usable | Either | Wi-Fi v4/v6; downstream NAT66 and core IPv6 |

Do not add boot auto-connect or an unbounded reconnect loop in this change.
Automatic upstream-Wi-Fi reconnection is a separate policy, not recovery.

## 4. One Rust coordination module, not another network daemon

Introduce a deep `NetworkCoordinator` module with a small interface:

```text
request(UserCommand) -> OperationId
observe(DeviceEvent)
snapshot() -> NetworkSnapshot
```

Keep existing `WifiRelay`, IPv6, Mihomo and modem implementations behind it.
One serialized worker plans and applies network transitions. Do not build a
generic workflow engine, replace OpenWrt/netifd, or add a second LAN RA daemon.

The coordinator's internal seam has two real adapters: device effects via
bounded commands/ubus, and a deterministic fault-injection adapter for tests.
Pure planning maps `(intent, observations, ownership)` to a small ordered plan.
Tests cross the same interface as web/device-event callers.

- Reuse the shared lock during migration; explicitly define lock ordering.
- Keep subscription HTTPS, JS transforms and YAML validation outside the
  network-effect critical section. Recheck intent revision before commit.
- Use ubus/process events plus bounded convergence checks during active work.
  Do not add permanent polling when Relay and core are off.
- Coalesce duplicate observations and stale events. Bounded waits return an
  operation state, not success based only on a command's exit code.
- Modem user setters retain their existing serialization/generation guard.
  A network reconcile observes modem state; it never calls modem enable as
  part of Wi-Fi or proxy recovery.

## 5. Configuration ownership and RA handling

Use per-resource ownership, not a whole-device snapshot restore.

| Resource | Recovery rule |
| --- | --- |
| Cellular switch, APN, connection mode | Never restore from a Relay snapshot |
| Vendor main routes/firewall | Observe; never flush or replace wholesale |
| Agent route tables, priorities, chains | Exact owned objects only; already absent is success after a reliable read |
| RA runtime options | Refuse existing non-null writers; release still-owned keys to null and read current vendor config |
| Volatile DNS file | Add/remove tagged current Wi-Fi resolver lines only; separate cleanup journal |
| AP runtime channel/mode | Restore only while still owned; preserve later user/vendor changes |
| Saved Wi-Fi/profiles | Preserve; not disposable test/session state |

Runtime RA records only its applied typed values and boot/owner marker, never a
vendor baseline. Matching values are released; changed external values survive.
If every owned key disappears after netifd reinitialization, an active session
can rebuild its overlay from its live lease. Conflicting partial changes degrade
IPv6 after safe withdrawal rather than starting a UCI reassert/commit loop.

RA state is a runtime overlay (`ra-runtime.json`), never a UCI snapshot. The
earlier persisted-UCI snapshot (`ra.json`) and its recovery path were removed once
the only affected router had been upgraded; a stray `ra.json` is ignored.

The volatile resolver update preserves symlinks/permissions, checks inode and
content before atomic replacement and retries bounded concurrent vendor writes.
No snapshot can overwrite a newer cellular resolver list. A separate DNS journal
allows independent routing/AP cleanup even if DNS removal needs a retry.

Across reboot, never replay old AP runtime or RA data. All new runtime resources
are gone; discard their old-boot journals. Do not blindly delete an
unknown-version journal.

## 6. Disconnect and recovery ordering

Treat teardown as dependency groups, not "continue blindly after every error".

1. Mark disconnect intent, invalidate the lease generation and reject new
   connection work. Stop/reap owned DHCP clients; disassociate and confirm the
   STA will not silently rejoin while restoring AP state.
2. Withdraw relay RA where still owned. Quiesce dangerous new traffic selection
   toward a disappearing upstream, then remove exact owned redirect, policy,
   NAT and forwarding objects in dependency order.
3. Complete independent IPv4 cleanup even if RA persistence needs a retry.
   Keep only the resources genuinely required for unfinished IPv6 cleanup.
4. Restore owned radio/AP runtime state once the STA is safely detached. An
   unrelated UCI conflict must not unnecessarily prevent hotspot restoration.
5. Re-observe the effective uplink; rebind/restart Mihomo once per actual
   transition, preserving proxy intent/config. Retire stale sockets/UDP flows.
6. Verify the final kernel/config/process state, then delete completed journal
   entries. Report `RecoveryPending` for real residue, not `Off` or success.

Bounded retry is for missing/busy tools, temporary vendor races and already-gone
objects after reliable observation. Never retry a permission error forever.
Keep actionable error categories (RA conflict, busy UCI, chain referenced,
unresponsive core), without SSIDs, subscription keys or credentials in logs.

`network-recover` must not be run independently against a live agent: its
process-local mutex does not serialize with that agent. The target interface
should request in-process recovery, or refuse CLI recovery while the formal
agent is live. For an operator cutover, stop the owning agent and its children
before replaying journals; never run two cleanup writers concurrently.

## 7. Web UX and downloads

- Start/stop core, subscription save/update, Wi-Fi scan/connect/disconnect and
  cleanup retry all show a spinner, disable duplicate submit, and give an
  accessible operation label. Preserve native form behavior without JS.
- A network interruption is "outcome unknown": retain the operation ID/token
  and ask the page to check state; do not replay the operation automatically.
- Eventually use operation status rather than a single long HTTP request;
  keep that work separate from the current loading-indicator patch.
- Display current uplink, cellular switch, IPv6 availability, core readiness
  and recovery residue separately. "Connected" does not prove Internet access.
- Agent downloads use the authenticated loopback-only core inbound while core
  is running, including GeoSite/GeoIP/ASN and subscriptions. Core rules/mode
  still apply; DIRECT is not the same as bypassing the core.
- With core off, use current uplink/DNS. When core is on but its node fails,
  return a sanitized error and retain previous config; no silent direct retry.

## 8. Acceptance matrix and release gates

Automated tests must exercise events in both orders, not only the happy path:

1. Relay connect -> cellular off -> on -> explicit disconnect, with core off/on.
2. Relay connect -> cellular off -> Wi-Fi loss: modem remains off, LAN UI works.
3. Relay connect -> cellular on -> Wi-Fi loss: actual traffic uses cellular.
4. Cellular toggles while Wi-Fi remains usable: Wi-Fi remains preferred; owned
   RA overlay converges; newer vendor baseline survives a later disconnect.
5. IPv4-only, dual-stack, IPv6 renewal/renumber/expiry, upstream router loss;
   test hotspot TCP/UDP and router/core egress, not only route-table text.
6. Vendor rewrites RA before/after commit; same-key and unrelated committed/
   pending edits; restore retry after crash at every persisted step.
7. Duplicate clicks, delayed old DHCP callbacks, stop during connect, hung
   command/core, unavailable netfilter table, failed write, process death.
8. Restart/reboot with active journals; refuse stale AP restore; verify no
   orphan child/chain/rule/journal and no duplicate boot launcher.

Three layers:

- Pure Rust transition/property tests: cellular-off is never undone; old
  revisions never commit; repeated cleanup converges.
- Disposable Linux/OpenWrt VM: real UCI, odhcp6c/odhcpd, bridge netfilter,
  routing and actual pinned Mihomo. Include injected vendor DHCP commits.
- B31: controlled upstream Wi-Fi, USB ADB, a LAN client with its own mobile
  data disabled, explicit agreement for network interruptions. Record before/
  after cellular switch, owned-object counts and vendor daemon sync status.

Suggested acceptance threshold: 20 VM cycles per transition class, then 10 B31
cycles of the reported sequence with core off and on. Zero unintended modem
enable calls, no monotonic increase in owned objects or processes, no leftover
recovery state after successful cleanup. Use bounded latency targets measured
on B31, not a claim of instantaneous or seamless TCP session migration.

## 9. Delivery stages

P0: complete this reported-bug fix and loading/editor/download regressions;
reproduce the B31 RA overwrite in the VM, preserve external values, verify the
actual retained B31 state, then stage a release. Do not silently change cellular.

P1: consolidate transition ownership into the small coordinator, add intent
revisions and typed operation/error snapshots, and replace duplicated sequencing
tests with interface-level tests. Keep vendor modem and AP implementations.

P2: resource-group cleanup, bounded IPv6-only degradation, typed RA list journal,
cross-boot acceptance and safe live/CLI recovery arbitration. Ship only after the
failure matrix passes; do not call the entire target design "done" after P0.

The simplest robust direction is one policy owner plus small device adapters,
not more independent watchdogs, global firewall resets or saved cellular-state
restoration. It reduces conflicting writers while reusing working B31 behavior.
