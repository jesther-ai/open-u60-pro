# DFS upstream acceptance — 2026-11-03

Device: U60 Pro B31, CN self-managed PHY. Upstream: `W_Game`, WPA2,
5260 MHz (channel 52), approximately −46…−49 dBm. All management and probes
used authorized USB ADB; the development Mac stayed on its own Internet link.

## Failure and fix

The old scan result reported `security=wpa2`, `supported=false` for 5260 MHz.
The relay's static non-DFS channel whitelist caused this. It now accepts
2.4/5 GHz frequencies enabled by `iw phy <phy> channels`, including DFS,
and rechecks that list before connecting. Disabled channels remain unavailable.
This device enables 1–13, 36–64 and 149–165; 14, 100–144 and 169–177 are
disabled by the PHY and are not overridden. 6 GHz is outside this relay's scope.

Removing the whitelist alone did not fix association. With the hotspot at
36/160, repeated API connects failed before authentication. Kernel evidence:

```text
hdd_validate_channel_and_bandwidth: BW 3 not possible in 2.4GHz band
hdd_softap_set_channel_change: Invalid CH and BW combo
wlan_hdd_cm_handle_sap_sta_dfs_conc: Set channel with CSA IE failed, can't allow STA
wlan_hdd_cm_connect: sap-sta conc will fail, can't allow sta
```

Manually staging the hotspot at 36/80, then at 36/20, made the same saved
WPA2 connection succeed. The driver subsequently moved the hotspot to 52/160.
The implementation automates the 20 MHz staging with `hostapd_cli chan_switch`:
save actual hotspot primary/width/center in the wireless undo journal, select
an enabled non-DFS 5 GHz staging channel, confirm the actual channel/state,
then associate. Runtime radar detection remains enabled; persistent wireless
configuration is unchanged. Recovery detaches STA before restoring the hotspot,
and waits for the original hotspot's CAC to complete.

## Validation

- Regression `dfs_upstream_is_selectable_and_keeps_its_association_frequency`
  failed on the original whitelist. It now covers 5260/5280/5300/5320/5500/5700
  MHz, including suppression of cached BSS entries on unavailable channels.
- Staging/recovery test exercises the actual command adapter, verifies channel
  parameters, waits through simulated CAC, and rejects `chan_switch` `FAIL`.
- `cargo test --locked -p zte-agent -- --test-threads=1`: **1229 passed,
  3 ignored**, including the startup integration test. An initial parallel run
  had one HTTPS/Mihomo bridge failure; that test passed alone and both serial
  full runs passed (1228 before adding the staging test, 1229 afterward).
- Release deployment used `setup.sh --build --adb --serial … --factory-password
  --no-ssh`; factory authentication and anonymous API rejection were verified.
  Final deployment backup: `/data/zte-agent-install-20261103-135838-61394`.
- Two API connections with the final binary succeeded from the original
  36/160 hotspot, without manual channel staging. STA and hotspot ended at
  **52/160**, hostapd `ENABLED`, lease obtained, API error null.
- Explicit disconnect restored **36/160**, waited for approximately 60 seconds
  of CAC, returned success, and left `recovery_required=false`.
- TERM/restart of the verified main agent recovered the journal, completed CAC,
  and automatically rejoined `W_Game`. Final API: connected, 5260 MHz, 160 MHz,
  autostart true, error null, recovery_required false. Mihomo running/error null.
- On-device [Internet check](../../scripts/check-relay-internet.sh), binding
  probes to `u60sta0`, passed both after reconnection and after agent restart:
  gateway **5/5**, public IPv4 `223.5.5.5` **5/5**, DNS resolution successful,
  HTTPS to `www.baidu.com` **200 over IPv4 and IPv6**. The IPv4 route explicitly
  selected table 60631 and `u60sta0`. One earlier four-packet public ping lost
  one packet; the two later five-packet runs had no loss.

Repeat from the development computer:

```sh
adb push scripts/check-relay-internet.sh /tmp/check-relay-internet.sh
adb shell sh /tmp/check-relay-internet.sh
```

Release SHA-256:
`d1af52da0d3e1850c1187462be5bc7781dd65e3a971478b9029b0d2beedf5a3b`.
File size **8,604,872 bytes**; ELF section Total **8,610,077 bytes**;
gzip (`mtime=0`) **4,402,146 bytes**. Section Total was computed from ELF64
section headers because GNU/LLVM size was unavailable locally. Relative to the
original release: file +12,640 bytes, section Total +12,556, gzip +7,372.

## Coverage limits and final state

Channel 52 is the DFS channel exercised on hardware. Acceptance on other
enabled channels is covered by software tests, not individual RF tests.
Cold device boot, real radar-triggered channel changes, long-duration load,
and downstream-client forwarding after this change were not tested. The Mac
was intentionally kept off the U60 hotspot during the final tests.

Device left connected to `W_Game` at 52/160, hotspot enabled, remembered-network
autostart enabled, Mihomo running. Temporary on-device diagnostic scripts and
all test ADB forwards were removed. The reusable check remains in the repo.


## Later simplification and per-network UI acceptance

The later revision replaces DFS-only preparation with one 5 GHz preparation
path for all upstreams (`wifi_relay/hotspot.rs`). 2.4 GHz keeps disable/set/enable:
manual CSA 6/40 → 1/20 worked, but its requested return to 6/40 remained 6/20.
The vendor hotspot restart restored 40; the regular relay rollback subsequently
restored 6/40 successfully. Default upstream width now stays negotiated.

Additional observed results:

- Auto history narrowed W from 40 to 20 MHz and produced public-IP loss and
  DNS/HTTPS failure. Restoring 40 gave gateway 6/6 and HTTPS 200. Negotiated mode
  later passed gateway/public 5/5, DNS, IPv4/IPv6 HTTPS 200 on W; W_Game160 also
  passed the same checks. First W join after global Wi-Fi OFF/ON timed out on
  join beacons; subsequent W_Game→W succeeded. Do not claim every reload case.
- Direct firmware Wi-Fi OFF while W was active cancelled restart intent,
  removed relay journals/interfaces, and did not resurrect either hotspot.
  API Wi-Fi ON then recreated 6/40 and 36/160. Wi-Fi OFF during restart's CAC
  also completed cleanup with autostart false/recovery_required false.
- Invalid connect selection left the existing connection and autostart intact.
- Default automatic tuning was disabled after the real failure above. The web
  now saves off/auto per SSID+security, independent of saving the password.
  Legacy credentials default off. Stored tuning is the runtime source of truth.
- Setting inactive W to auto left W_Game off. Active W_Game off→auto reconnected
  successfully (about 92 seconds including CAC), and agent restart automatically
  rejoined with auto preserved. Active auto→off likewise succeeded. Both W and
  W_Game were set back to off at the end.
- Web tests cover mode rendering/preselection, body forwarding, invalid values,
  CSRF, single-use replay, no-JS feedback and credential escaping. Full suite:
  1238 passed, 3 ignored before the cosmetic mobile refinement; 39 network-page
  tests passed again after refinement. Clippy diagnostics in modified files: 0;
  strict whole-repo Clippy remains blocked by pre-existing unrelated warnings.
- Real rendered HTML in headless Chrome reproduced a 630px page at 320/375/390/430
  mobile widths. After full-width stacked selects and min-width/grid fixes,
  document width equals viewport at all four sizes. Bandwidth is directly above
  Connect to Wi-Fi; the Auto option no longer carries the experimental label.
- All temporary device test scripts, the wireless QA snapshot and restart log
  were deleted without backup. `/data/local/tmp/start_zte_agent.sh` is the actual
  service launcher and remains. setup.sh retains its normal installation rollback
  artifacts; they are not test scripts.

The current lifecycle and rationale are in [relay-lifecycle.md](relay-lifecycle.md).


Final delivered mobile revision: setup backup `/data/zte-agent-install-20261103-151638-31665`;
SHA-256 `db5a9dd94dbe097c6c6c0edccde20bdc29d0935c8cc82ee64da45bc27f4d5c25`.
File 8,630,064 bytes, ELF section Total 8,635,813, gzip 4,414,967; app.css 48,757
bytes (below 49,152 cap). The final full test run again passed 1238 tests with
3 ignored. Final responsive checks passed 320/375/390/430 px after CSS compaction.
