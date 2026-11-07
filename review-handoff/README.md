# 评审交接：Wi‑Fi 中继（5 GHz、速度、上游自适应带宽）、Mihomo 吞吐、重启自动恢复、本地管理 token

日期：2026-10-27 至 2026-10-28。设备：ZTE U60 Pro（B31，SDX75，qcacld‑3.0 v5.2.1.110D）。
这个目录只是交接材料，不要提交。

## 0. 先读这里

- 所有改动都**没有提交**。工作区里本来就有大量此前未提交、未跟踪的工作（`wifi_relay.rs`、`mihomo.rs`、`wifi_relay/` 等整个文件都是 `??`），所以 `git diff` 区分不出这次的改动。
- 有基线可比的文件，这里给了差异：
  - [wifi_relay.rs.diff](wifi_relay.rs.diff)：`zte-agent/src/wifi_relay.rs`，对比 2026-10-27 10:54 第一次改动前的副本（`/private/tmp/u60-scan-baseline.MI95OD/`）
  - [network-extensions.md.diff](network-extensions.md.diff)：`zte-agent/docs/network-extensions.md`，同一基线
- 其他文件没有基线，下面第 2 节按文件、按函数列出了这次改动的位置。`wifi_relay/scan.rs`、`width.rs`、`llstats.rs`、`resume.rs` 是本次新建的，整份都要看。
- 用户的目标（原话的意思）：
  1. 支持中继 5 GHz Wi‑Fi，提升中继后的网速，从根本上解决问题。
  2. 2.4 GHz 和 5 GHz 都要自动选出最佳速率，不能只针对某一个网络调。**只做上游一侧**，热点（AP）一侧不动。
  3. 给 agent 一个本地 token，方便用 ADB 自动操作面板。
  4. agent 重启、更新、路由器重启后，自动恢复中继和 Mihomo。
  5. `setup.sh` 更新时不用先关中继和 Mihomo。
- 用户明确说过先不做：Mihomo 的国内直连内核旁路。另外，AP 侧自适应也不做。

## 1. 研究结论（带证据）

### 1.1 5 GHz 扫描丢网（根因，来自 `wlan.ko` 反汇编）
- `scm_scan_start_req` 会把多信道 STA 扫描里的部分 5 GHz 信道删掉：和某个 AP 共用同一个 MAC、并且 `policy_mgr_is_any_dfs_beaconing_session_present` 报告为真的那些信道。**任何 160 MHz 的 5 GHz AP 都算**（不管它在不在 DFS 信道上）。删信道的逻辑在 `policy_mgr_scan_trim_5g_chnls_for_dfs_ap`。
- 主热点在 36 信道 160 MHz 时：
  - 每次全扫描都看不到 36–64；
  - 只扫 36–48 的请求被直接拒绝（`Reject 0 channel Scan`、`INTERNAL_FAILURE`）；
  - 149–165 在另一个 MAC 上，不受影响。
- **只扫一个信道的请求会跳过这段逻辑。** INI 里的 `ch_list_trim_conc`、`skip_6g_and_indoor_freq_scan` 都是 0，和这件事无关。
- wpa_supplicant 会把被拒绝的扫描也报告成"完成"，结果是用内核缓存的 BSS 表补出来的。所以只能保留 `age` 落在本次扫描时间内的条目。读 BSS 表用 `BSS RANGE=` 分页（`SCAN_RESULTS` 的回复只有 4 KB）。
- 现场数据：`5180,5220` 被拒绝 3/3；`2412,5220` 3/3 都没有新的 5 GHz 条目；单扫 `5220` 3/3 都在 age 0 s 找到。新的扫描序列 3/3 找到，用时 3.04–3.07 s。

### 1.2 5 GHz 上游时"Hotspot channel change was not confirmed"
- 运行时用 `hostapd_cli` 改 `wlan2` 的参数在 B31 上不可靠（实测）：
  - 44 信道 20 MHz：`enable` 被拒绝；
  - 40/80 MHz：停在 `HT_SCAN`，然后变成 `DISABLED`，驱动没收到 `START AP`；
  - 160 MHz 能起来，但要做 60 s 的 DFS CAC；
  - STA 已连接时，连 160 MHz 也会失败。
- 改法：5 GHz 上游时不碰 `wlan2`，让驱动自己的策略（`gWlanMccToSccSwitchMode=3`，强制 SCC，不重启 SAP）用 CSA 把 AP 切到 STA 所在的信道。实测 3 s 内从 36/160 切到 44/80，没有 CAC。
- 2.4 GHz 上游时仍然把 `wlan0` 对齐到上游信道（20 MHz）。
- 恢复方法：之前我的脚本把 `wlan2` 搞挂过一次，大约 8 分钟。用厂商自己的 `/etc/init.d/hostapd-daemon.init restart wlan2` 恢复了。

### 1.3 W_Game（5 GHz）关联失败
- 只有 `freq_list` 时，它只限制"选哪个网络"。supplicant 自己的连接扫描仍是全扫描，热点在 160 MHz 时同样丢掉 5 GHz，30 s 后报"association failed"。
- 加上 `scan_freq` 以后，连接扫描只要 75 ms，约 1 s 完成关联。

### 1.4 速度分层（5 GHz 上游，关掉 Mihomo，6 路并行 HTTP，测 12 s）
- U60 本机 249 Mbit/s；Mac 直连上游 250；Mac 经中继 213–246。
- 转发时 CPU0 的软中断占 71–88%，CPU2–3 约 87% 空闲。
- 试过 RPS（`rps_cpus=f`）：234/251/254，但 5 次里有 2 次掉到 71/51，**放弃**。内核没有 flow offload（`CONFIG_NF_FLOW_TABLE` 没开）。
- 用户最初看到"W 只有 5.77 Mbit/s"，是因为 W 是那台路由器的 2.4 GHz 网络，见 1.5。

### 1.5 2.4 GHz 慢（根因）
- 驱动以 40 MHz（1 + 5 信道）关联 STA，而同一个 AP 上的 Mac 用的是 20 MHz。AP 发给 U60 的速率主要是 HE‑MCS 1、1 SS。
- 同一测试：Mac 82–91 Mbit/s，U60 32–36。
- `iwpriv u60sta0 chwidth 0`（20 MHz，不重新关联）后 92–117 Mbit/s；切回 40 MHz 是 34；再到 20 MHz 是 104。AP 的速率升到 HE‑MCS 9–11、2 SS。
- 已排除：关 STA 省电、关 2.4/5 GHz AP（DBS 关闭）、蜂窝共存、蓝牙（软关闭）。`pm_cinfo` 显示 DBS，STA 和 AP 在 MAC 0 上同信道（SCC），没有 MCC。
- `iwpriv chwidth` 的编码：0/1/2/3 = 20/40/80/160（`hdd_we_set_ch_width` 表）。

### 1.6 带宽要靠测，不能写死规则；用什么指标打分
- 只看 `rx bitrate`（最后一帧的速率）不够：失败重传的帧占了空口时间却没送达数据，`rx bitrate` 看不出来。
- 改用 qcacld 的链路层统计：
  - 先发 QCA vendor 命令 14 `LL_STATS_SET`（OUI 0x1374，嵌套 VENDOR_DATA，属性 197|0x8000）。`iw vendor send` 不行，内核要求数据带 nested 标志，所以在 Rust 里直接发 genetlink。
  - 再读 debugfs `/sys/kernel/debug/u60sta0/ll_stats` 里该电台的 `rx_time`。
- 分数 = 每秒空口接收时间里送达的下行比特数。在 W 上每个带宽测 10 s：
  - 40 MHz：0 和 10 Mbit/s，每空口秒 0 和 42；
  - 20 MHz：59 和 90，每空口秒 101 和 129。
- 这个固件的每 peer 按速率接收计数是空的（`rxmpdu` 为 0）。HTT stats 通过 `/sys/kernel/wifi/txrx_stats` 读。

### 1.7 Mihomo 吞吐（经 5 GHz 中继，DIRECT）
- gVisor TCP 66–68 Mbit/s；`mixed`（内核 TCP + gVisor UDP）128–139；不开 Mihomo 213–246。部署后实测 119–131。
- 改成 `mixed` 后要处理两件事：
  - 内核 TCP 栈回复客户端时用的是 TUN 子网地址，被中继的兜底路由表带去了 Wi‑Fi。所以加了优先级 `18629` 的规则：`198.18.0.0/30` 和 `fdfe:dcba:9876::/126` 走 `main`。
  - 核心把每个被拦截的 TCP 连接注入 TUN 发往自己的地址，而 `U60_MH_IN` 原来会拒绝它，结果 TCP 全部 0 Mbit/s。现在只放行 `-i u60mh0 -p tcp -d <TUN 地址>`。
- 对运行中的核心用 `PATCH /configs` 换栈，会删掉 TUN 并让它保持关闭。所以换栈只能通过干净地重启核心。

### 1.8 这次的发现：中继辅助进程在 agent 退出后还活着（根因：SELinux）
- 现象：第一次带着中继更新后，恢复连续 6 次在关联阶段失败。旧 agent 的 `wpa_supplicant` 和 `odhcp6c` 还在（父进程变成 1），旧 supplicant 占着控制 socket。新 supplicant 一启动就退出（变成僵尸），`wpa_cli` 的命令全发给了旧进程。旧 `odhcp6c` 还会继续调用新的 agent 程序当脚本。
- 根因：B31 让 `wpa_supplicant` 跑在 `wpad.subj`、`odhcp6c` 跑在 `odhcp6c.subj`；agent、busybox 的 `udhcpc` 和 Mihomo 都在 `sys.subj` 或不切换域。exec 时只要 SELinux 域变了，内核就**总是**清掉 parent‑death signal（`selinux_bprm_committing_creds`），宽容模式（`enforce=0`）下也一样。
- 已排除：AT_SECURE 都是 0，不是 setuid/文件 capability 造成的。
- 第二次、第三次更新都复现了（每次都是"stopping 2 helper(s)"）。Mihomo 和 `udhcpc` 每次都正常随 agent 退出。

### 1.9 其他确认过的事实
- 开机同步栅栏：`ubus call zwrt_topsw_daemon.sync get_sync_info` 返回 `"noSyncModuleName": "sync success"`。冷启动时，agent 运行起来之前（开机 54 s 以内）栅栏就已经放开了。
- 路由器上的 `wget` 是 uclient-fetch，不能加自定义请求头，所以本地 token 只能通过 `adb forward` 加电脑上的 curl 来用。`adb forward` 进来的连接在 agent 看来是 loopback。
- `127.0.0.1:19090` 是单元测试里 Mihomo 控制器 mock 用的端口（`mihomo::CONTROLLER`）。在开发机上占用它，`httpd::tls::tests::actual_mihomo_bridge…` 会失败，所以文档里的转发端口用 18090。
- 早先观察到、但没处理：拆中继时有一次 netfilter NAT 内核 WARNING（`nf_nat_setup_info`，dnsmasq）。

## 2. 改动清单（按文件）

### 2.1 中继（`zte-agent/src/wifi_relay*`）
- **`wifi_relay/scan.rs`（新，3 个回归测试）**：
  - `plan()`：根据 `iw phy <phy> channels` 生成扫描计划：2.4 GHz 一个请求，每个启用的 5 GHz 信道（4900..=5899）各一个请求。
  - `run()`：先 `BSS_FLUSH 0`，然后逐个请求 `SCAN freq=`，`FAIL-BUSY` 时重试，等待 `CTRL-EVENT-SCAN-RESULTS/FAILED`，最长 2000 + 400 ms × n。一个请求都没完成就报错。
  - `table()`：用 `BSS RANGE= MASK=0x21a87` 分页读 BSS 表。
  - `Ctrl`：UnixDatagram 连接 supplicant 并 ATTACH，每次请求前清空事件队列，Drop 时 DETACH。
  - 保留 3 个纯回归测试，覆盖扫描计划、模拟驱动删信道和扫描拒绝；一次性真机诊断入口已移除，真机扫描走正式 API。
- **`wifi_relay/width.rs`（新，15 个测试）**：上游带宽调节器 `Tuner`。
  - 参数：`SAMPLES=4`（取均值；中位数会掩盖不稳定的 40 MHz 链路），`GAIN_PERCENT=115`，`SETTLE_TICKS=2`，`REVIEW=15 min` 倍增到 `MAX_REVIEW=4 h`，`NEIGHBOUR_DBM=-82`，`REMEMBERED=32`。
  - 规则：从协商带宽或预测/记住的带宽开始；下行忙（≥ 1 Mbit/s）的 tick 才采样；每次试下一档更窄的带宽，快 15% 以上才保留，否则停止并回到最快的一档。换档后跳过 2 个 tick。稳定后定期从最宽一档重测（更宽一档一直输就把间隔加倍，赢一次就回到 15 min）；稳定速率跌到一半时立即重测。
  - `predict()`：根据扫描到的邻居（包括隐藏网络）选起始带宽：主信道周围最宽的、没有 -82 dBm 以上其他网络重叠的块。主信道上的网络不算。
  - `remembered()/remember()`：`widths.json`，按扫描 id 记，最新的在前，最多 32 条，0600。
  - 其余：`airtime_rate()`（空口时间不足 100 ms 不算）、`parse_channel()`、`parse_rx_rate()`、`chwidth_code()`。
- **`wifi_relay/llstats.rs`（新，2 个测试）**：
  - Linux 下用原始 genetlink 打开 LL stats（`enable`）；非 Linux 是返回错误的桩。
  - `read`：读 debugfs。`rx_airtime`：选主信道上 `on_time` 最大的那个电台。测试用的是真机输出。
- **`wifi_relay/resume.rs`（新，2 个测试）**：
  - `resume.json` 保存 `{ssid, security, bssid, ipv6}`，**不含密码**，0600。
  - `pick()`：找 SSID 和加密方式都相同、且受支持的网络，同一个 BSS 优先，否则选信号最强的。开放网络的"同名双胞胎"不会顶替 WPA2 网络。
- **`wifi_relay.rs`**（见 diff）：
  - 扫描：`scan()` 改用 `scan` 模块；`fresh_bss()` 共用的遍历函数；`parse_bss()`；`neighbours()`（最多 256 条）；`decode_ssid` 处理 `\"`。
  - 关联：`wpa_network()` 写入 `freq_list` + `scan_freq`。
  - 信道对齐：`aligned_hotspot()` 只在 2.4 GHz 时返回 `wlan0`；`align_radio` 固定 20 MHz；`radio_phy()`。
  - `connect` 的早期失败也会发布到状态里（`failed` 闭包），页面不再显示上一次的错误。
  - 带宽：`LinkWidth`、`start_width()`（记住的带宽 > 预测，打开 LL stats，比协商值窄就设置）、`tune_width()`（watcher 每 3 s 调一次，按空口时间打分；读不到统计时退回 `rx bitrate` 并重置调节器；稳定后记住）、`set_width()`。快照里加了 `width`。
  - 重启恢复（本轮）：
    - `recover_idle()`：watcher 启动时的恢复逻辑抽成函数，恢复线程也调用它。
    - `wants_restore()`、`restore()`：等待序列 `RESTORE_WAITS = [0,10,20,30,60,60,120,120,300,300]` s（约 17 min）。每次尝试：扫描 → `pick` → `join(.., Origin::Restore)`。密码不在了就停止；被用户取消就静默结束。
    - `connect_remembered()` 只做确认检查，然后调用新的 `join(id, password, ipv6, Origin)`。`join` 拿到网络锁以后：
      - `Owner`：先 `resume::clear()`；成功后，如果密码已被记住（或是开放网络），写入 `resume.json`。
      - `Restore`：如果已有会话，或 `resume.json` 已经不等于目标，就返回 `RESTORE_CANCELLED`；wpa2 网络没有已保存的密码就返回 `RESTORE_NO_PASSWORD`。
    - `disconnect()` 先清掉 `resume.json`。watcher 因掉线拆除中继时不清。
    - 状态：`restoring`（快照里，正在恢复的 SSID）；`autostart` 在 `status_with_clock()` 里读 `resume.json`。
  - 残留进程（本轮）：
    - `recover()` 第一步调用 `stop_strays()`：找出 `wpa_supplicant`（`-i u60sta0 -c /tmp/zte-agent-relay/{wpa,recover}.conf`）、`odhcp6c`（`-p /tmp/zte-agent-relay/odhcp6c.pid … u60sta0`）、`udhcpc -i u60sta0` 中父进程不是自己的，先 SIGTERM，1 s 后 SIGKILL，再等 1 s 还在就报错。
    - 匹配逻辑是纯函数 `stray(args)`，有测试。非 Linux 下 `stop_strays` 直接返回 Ok。
    - `prepare()` 里 ping 成功后，再确认**自己的** supplicant 子进程还活着，防止别的进程回的 PONG 被当成自己的。
- **`net_tools.rs`**：只改了 `owned_child` 的注释，说明 SELinux 换域的例外。

### 2.2 Mihomo（`zte-agent/src/mihomo.rs`）
- 吞吐（前一轮）：
  - TUN `"stack":"mixed"`（约第 906 行）；
  - `LOCAL_PREF = "18629"`，在 proxy plan 里，每个协议族在 fwmark 规则之前加 `ip rule pref 18629 to <TUN 子网> lookup main`（约第 1199 行）；
  - `U60_MH_IN` 第一条是 `-i u60mh0 -p tcp -d <TUN 地址> -j ACCEPT`，然后才是 `-i u60mh0 -j REJECT` 和 `-d <地址> -j REJECT`（约第 1060–1106 行）；
  - 启动时检查 18629 没被占用（约第 495 行）；相关测试也已更新。
- 重启恢复（本轮）：
  - `WANTED = "enabled"` 标记文件，放在 `/data/zte-agent-mihomo/`。
  - 公开的 `enable()` 成功后写标记，失败就删除。`disable()` 先删标记。
  - 内部的 `disable_locked`（核心退出、上游切换、配置操作）不动标记。
  - `restore(local_name)`：最多尝试 4 次，等待 [0,5,15,30] s。用户已经开着或关掉了就直接返回；最终失败时把错误写到 status 的 `error`。
  - status 的 `autostart` 现在等于 `wants_restore()`。
  - 测试：`owner_choice_outlives_restarts_until_turned_off_or_failing`。

### 2.3 认证 / 启动（本轮）
- `auth/mod.rs`：
  - `LOCAL = SessionId(0)`，会话表的 id 从 1 开始，所以不会冲突。
  - `AuthState.local: OnceLock<[u8;32]>` 存的是 SHA‑256("local\0" ‖ token)。
  - `issue_local()`：每个进程只能调用一次，没有随机数时报错。
  - `authenticate_local()`：常数时间比较。
  - `Principal::is_local()`。
  - 2 个测试：不是 bearer token 也不是 cookie；不过期；`revoke_all` 后仍有效；`touch_session` 返回 Revoked；没有随机数时失败。
- `auth/startup.rs`：`LOCAL_TOKEN = /tmp/zte-agent-run/admin-token`；`write_local_token()` 用 `net_tools::write_private`（0600，目录 0700）。测试：权限正确，每次启动替换，旧 token 失效。
- `server.rs::precheck`：bearer 验证失败、并且请求来自 loopback 时，再试 `authenticate_local`。本地 principal 访问 `/api/events` 返回 403 "event streams need a login token"。测试：loopback 可以；LAN 返回 401；事件流 403；跨源变更仍然被拒绝。
- `main.rs`：
  - 认证配置好之后写本地 token（失败只记一行日志，不含 token）。
  - `restore_network()`：两边都没有要恢复的就不起线程。否则起一个 `net-restore` 线程：`wait_for_boot()`（每 5 s 轮询同步栅栏，最多 3 min）→ `wifi_relay.recover_idle()` → `mihomo.restore()` → `wifi_relay.restore()`。
  - 顺序的理由：中继的旧路由还在时启动 Mihomo，`enable` 会看到 `route get 1.1.1.1` 走 `u60sta0`，把核心绑到 `u60sta0` 上。

### 2.4 网页（`web/pages/network.rs`、`web/tests/network.rs`）
- 已连接时显示 "UPSTREAM LINK"：频段和带宽，注明是自动选择的（前一轮）。
- 本轮：
  - 已连接时加一行 "AFTER A RESTART"："Reconnects automatically"，或 "Stays off; connect with Remember on to reconnect"。
  - 恢复进行中：状态显示 "Reconnecting"，标题是正在恢复的 SSID，按钮是 "Stop reconnecting"（就是断开，会清掉 `resume.json`）。
  - 测试：`relay_says_whether_it_comes_back_after_a_restart_and_can_stop_reconnecting`、`relay_shows_the_automatically_chosen_upstream_width`。

### 2.5 脚本 / 文档
- `setup.sh`：
  - 删掉了"先断开中继、关闭 Mihomo"的检查，改成中继或 Mihomo 开着时只打印一行 info；
  - `install.sh` heredoc 里同样的 `[ ! -e … ]` 检查也删了，注释改成"子进程随 agent 退出，其余由日志撤销"。
  - 其他 hunk 是此前就存在的未提交工作。
- `SECURITY.md`：新增本地 token 一条，说明它只是 root 可读、只接受 loopback、不能开事件流、为什么没有扩大权限，Tailscale 客户端也走 loopback 但读不到这个文件。另加重启恢复一条：保存什么、不保存密码、如何清除。
- `CLAUDE.md`：Auth 那条补了本地 token；Basic Debugging 里加了 `adb forward tcp:18090` + curl 的示例。
- `docs/network-extensions.md`：
  - 中继一节重写：扫描根因、信道对齐、关联扫描、速度分层、RPS、2.4 GHz 根因、带宽规则、起始带宽和打分方式；
  - Mihomo 拦截一节：`mixed`、18629、INPUT 放行、测量数据；
  - Recovery：SELinux 例外和残留进程清理；
  - 新增 "After a restart" 小节和现场验证；
  - 修正了两处过时的说法（"没有冷启动重连"、"冷启动未验证"）。

## 3. 请重点挑战的设计决定

1. 只恢复密码已被记住的网络（或开放网络），`resume.json` 不存密码。用户连接时取消勾选"记住"，就不会恢复。
2. 恢复意图只由用户操作清除：断开、连到别的网络、关闭 Mihomo、或用户自己开 Mihomo 失败。中途掉线、恢复失败、核心崩溃都保留意图。
3. 中继掉线后 agent 运行期间**不会**自动重连（沿用原设计，用户只要求了重启场景）。
4. 扫不到上游时不会 join，热点不受影响（创建的 STA 接口只用于扫描）。但如果**扫得到却关联失败**（比如这次的残留进程问题），上游是 2.4 GHz 时，每次尝试都会把 `wlan0` 切到上游信道再回滚（dmesg 里每次两次 AP stop/start），最多 10 次、约 17 分钟，2.4 GHz 热点的客户端会反复断开。要不要减少次数，或者关联失败后拉长间隔？
5. 恢复用的 `scan()` 不持有网络锁，到 `join()` 才在锁内复查。中间窗口里用户的操作由 `join` 里的复查兜住。
6. `stop_strays` 按命令行匹配，然后发信号：存在极小的 PID 复用窗口。另外，它也会在 `network-recover` CLI 和每次 `prepare()` 时运行（每次都扫一遍 `/proc`）。
7. 本地 token：不过期，`revoke_all` 后仍然有效，进程内唯一。root 本来就能读到密码校验值，所以没有扩大权限。
8. 开机最多等 3 分钟同步栅栏，然后不管怎样都开始恢复。厂商之后如果重载防火墙，可能冲掉 Mihomo 的规则，这一点没有处理，原有文档已经说明"防火墙重载不会持续修复"。
9. 带宽调节器的参数（15% 增益、均值 4 个样本、复查间隔、-82 dBm 邻居阈值）是根据 W 和 W_Game 的现场数据定的。

## 4. 验证

- `cargo test --locked -p zte-agent`：1125 个通过、3 个忽略，另有 1 个启动集成测试通过。
- 本次新增的测试：`scan` 当时 4（后移除一次性真机诊断，保留 3 个回归测试）、`width` 15、`llstats` 2、`resume` 2，加上上面提到的 auth、startup、server、mihomo、web 测试。
- clippy：改动的文件里没有新警告（`auth/mod.rs:654` 的 `chunks_exact` 是原有的）；aarch64‑musl 目标同样检查过。
- ARM64 musl release（Python ELF 解析，统计有标志的段，与之前的测量方法一致）：
  - 本轮最终版本：段 Total 8,529,516 字节，gzip ‑9 4,354,020 字节；
  - 比本轮开始时部署的版本（8,507,800 / 4,342,746）多 21,716 / 11,274 字节。
  - 更早几轮的数字见 network-extensions.md。
- 现场：
  - 5 GHz：W_Game 连上，AP 用 CSA 移动，STA 160 MHz，用户实测 140 Mbit/s；后来 249–251 Mbit/s（链路上限）。
  - 2.4 GHz：W 从 32–36 提升到 92–117 Mbit/s；调节器会自动从 40 MHz 切到 20 MHz，并写入 `widths.json`。
  - 重连时直接从记住的 20 MHz 开始（本轮确认）。
  - Mihomo：66 → 119–131 Mbit/s。
  - 带着中继和 Mihomo 更新：共 3 次。第 1 次（旧 agent → 新 agent）因残留进程恢复失败，修复后又做了 2 次，每次 Mihomo 恢复，中继第 1 次尝试就连回 W。
  - 冷启动：54 s agent 已在运行，约 60 s Mihomo 恢复，69 s W 关联，94 s 状态全部正常（`autostart` 都是 true，无错误）；223.5.5.5 和 HTTP 经 `u60sta0` 正常。1.1.1.1 的 ICMP 不通，其他目标都通。
  - 本地 token：经 `adb forward` 访问 status 正常；错误 token 401；事件流 403。LAN 拒绝只有单元测试，开发机当时连不到 192.168.0.1。

## 5. 未完成 / 风险

- 未测：5 GHz 上游的重启恢复；开机时上游 AP 不在；用户在恢复过程中点 Stop。
- 未审计：agent 启动的其他换 SELinux 域的守护进程（例如 tailscaled、DoH 相关）是否同样会残留。只修了中继这三个。
- agent 运行期间掉线不会自动重连。
- 第一次从旧版本升级：旧 agent 不写恢复文件，所以那一次是**我手动**按当时状态写的 `resume.json`（W，BSS 没有输出）和 Mihomo 标记。
- 设备当前状态：中继连着 W（2.4 GHz），Mihomo 开着，两个恢复文件都在。`/tmp/u60-probe` 调试文件已删除。`/data/zte-agent-install-*` 是 setup.sh 的正常备份。

## 6. 复现命令

```sh
cargo test --locked -p zte-agent
cargo test --locked -p zte-agent wifi_relay:: -- --nocapture
# 真机扫描使用已认证的 POST /api/wifi-relay/scan，不再运行独立测试二进制。

# 部署（中继和 Mihomo 可以开着）
export PATH="/tmp/u60-cross-tools:$PATH"
export CC_aarch64_unknown_linux_musl=/tmp/u60-cross-tools/aarch64-linux-musl-gcc
export AR_aarch64_unknown_linux_musl=/tmp/u60-cross-tools/aarch64-linux-musl-ar
./setup.sh --build --adb --serial "$ANDROID_SERIAL" --factory-password --no-ssh

# 恢复过程和残留进程
adb shell 'grep -E "^\[(relay|mihomo|restore)\]" /tmp/zte-agent.log'

# 用本地 token 调 API（不要打印 token）
adb forward tcp:18090 tcp:9090
TOKEN=$(adb shell cat /tmp/zte-agent-run/admin-token | tr -d '\r\n')
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:18090/api/wifi-relay/status
adb forward --remove tcp:18090
```

## 7. 上游带宽自适应：方案评估和下一步（2026-10-28）

> **已被取代：** 执行请以 [width-v2-final-handoff.md](width-v2-final-handoff.md)（2026-10-29）为准。下面保留作为历史评估。

这一节评估 Codex 对带宽调节器提出的意见。依据是 2026-10-28 08:50 时的代码：`width.rs` 已经包含另一个 agent 根据评审加入的 `Controller`（读回确认、`APPLY_*` 重试）和试验超时 `TRIAL_LIMIT = 45 s`。下文的行号对应这个时点，代码之后可能还会变。

### 7.1 总体判断

- 调 STA 的信道带宽这条路是对的：变量可控、有效，只用真实业务流量，不发探测包。2.4 GHz 从 40 MHz 改到 20 MHz 后，吞吐从 32–36 提升到 92–117 Mbit/s。
- 但"自动选到最佳速率"只在 2.4 GHz（40/20 两档）基本成立。5 GHz 有四档（160/80/40/20），现在的搜索和复查逻辑有缺陷，见 7.2。更准确的说法是"上游带宽自适应"，不能承诺"始终选到实际最快"。

### 7.2 已核实的问题（按严重程度）

1. **复查可能把好的档位换成差的，并一直卡住（只影响 5 GHz，最严重）。**
   - 复查一律从最宽档开始（`restart()`，第 168 行）；窄一档没有快 15% 以上就结束（`tick()` 第 125 行）。这次比较**不看复查前的稳定档**。
   - 例子：开始时按扫描邻居预测出 40 MHz，实测也很好。15 分钟后复查：先测 160，再测 80。如果干扰落在 40 以外的次信道上，160 和 80 都会受影响，80 不会比 160 快 15%，于是停在 160。
   - 之后每次复查都重复这个结果（最宽档赢了，复查间隔还会重置回 15 min），原来好得多的 40 MHz 再也测不到。"速率跌到一半"的检测是和 160 自己比的，救不回来。
   - 次信道有干扰时，所有更宽的档位通常一起变差，所以"宽档之间比不出差别、更窄档好得多"在现实中很常见，不是构造出来的极端情况。
2. **回滚锚点被覆盖（Codex 第 1 条，已核实）。** `restart()` 把稳定档存成试验的回滚点，但 `tick()` 第 132 行测完第一档后执行 `self.trial = Some((now + TRIAL_LIMIT, self.current, mean))`，用刚测完的候选覆盖了它。复查中途下载结束、试验超时时，会回到刚测的那一档（可能很差），而不是复查前的稳定档。
3. **"输一档就停"会漏掉更窄的好档（Codex 第 3 条，已核实）。** 原因同第 1 条：搜索从宽到窄，假设性能单调变化，无线环境不保证这一点。

### 7.3 Codex 的意见：建议采纳

1. **固定回滚锚点：** 最优先。实现时建议和下一条合成一个机制：**锚点 + 每次复查只挑战一个候选**：
   - 稳定档（锚点）和它的分数，在整个复查期间不变；
   - 每次复查只测一个候选，和锚点比，至少快 15% 才替换；超时、没数据、试验失败都回到锚点；
   - 候选按顺序轮换：先测和锚点相邻的档位，再测其余的；某个候选刚输过，它自己的下次复查时间就往后推；
   - 早停：候选前 1–2 个样本不到锚点的一半，就立即退回，把试差档的成本压到几秒；
   - 复查时最好也重新测一下锚点本身，因为环境会变。
2. **按窗口求和打分：** 窗口分数 = Σ有效接收字节 × 8 / Σ有效空口时间。现在 `tick()` 第 112 行是 4 个 tick 比值的算术平均，刚过 100 ms 门槛的小样本和空口时间充足的大样本权重一样。改动小，统计上也是对的。退回 `rx bitrate` 打分时，按时间或字节加权。
3. **低流量时按有上限的时间窗口累计证据，并丢弃过旧的样本：** 不要求每个 tick 都独立达标（≥ 1 Mbit/s、≥ 100 ms 空口时间）。中间断开太久的样本不要拼成一次比较。和第 2 条一起改。
4. **分数不等于网速：** 分数衡量的是接收空口效率。5 GHz 上 160 和 80 MHz 的实际吞吐都是 249–251 Mbit/s，瓶颈在别处。页面目前只写了 "chosen automatically"，没有夸大，保持这样。

### 7.4 建议部分采纳

- **radio 级 `rx_time` 混入其他流量（Codex 第 2 条）：** 确实存在。`llstats::rx_airtime()`（第 172 行）取的是整个无线电的接收时间；STA 和热点同信道共用无线电，热点客户端的上传、同信道其他网络的帧都会算进去。`ll_stats` 里每个信道的 `rx_time`、`cca_busy_time` 也一样，不是这条连接独有的；每 peer 的计数在这个固件上是空的。
  - 先做对照验证：STA 空闲，只让热点客户端上传，看 radio `rx_time` 涨不涨。
  - 我的判断：客户端上行的 ACK 和下载量大致成正比，所以这个偏差主要是把各档之间的差距压小，让调节器更保守，不太会让它选错。优先级排在 7.3 之后。
  - 缓解办法只能是"降低这个样本的权重"，见 7.5 第 2 条。
- **缓存带上可信度（Codex 第 5 条）：** 只加三样：打分来源（airtime 还是 PHY 速率）、时间、当时的信号强度。信号差很多（例如超过 10 dB）或记录很旧时，只当起点，不当结论。不做复杂的置信度模型。
- **页面显示调节状态（Codex 第 5 条）：** 价值有限。要做就只改文字，因为 `app.css` 只剩 6 字节余量。

### 7.5 可能是负优化，不建议做

1. **每次复查都扫满四档：** 每 15 分钟让用户在差的档位上待接近一分钟（每档约 2 个跳过的 tick 加 4 个样本，每 tick 3 s）。
2. **"热点忙就丢掉样本"：** 中继场景下，热点客户端本身就是流量的来源，几乎一直在忙，结果会是永远测不出来。只能降低权重，不能丢弃。
3. **降低 1 Mbit/s 和 100 ms 的门槛：** 噪声变大，会来回切换。应该靠 7.3 第 3 条的窗口累计解决。
4. **把 CCA busy、重传率揉成一个综合分：** 难解释，也难调参。只能作为辅助证据。
5. **用整个 radio 的空口时间判断"有没有需求"：** 里面混着别的流量（Codex 自己也指出了）。
6. **用户手动的"重新评估"按钮：** 用户不知道什么时候该点，价值低。
7. **没有上限的"停滞自救"：** 上游断网或停在认证页面时，客户端也会不停重试，这时换带宽没用，只会来回切。如果要做：用 `u60sta0` 自己的 `tx_packets` 作为需求证据（只算本接口，不是整个 radio），并严格限制次数，例如 30 分钟最多一次。
8. **缩短 15 分钟的复查间隔：** 每次复查都要付出试验成本。

### 7.6 带宽之外，收益更大的优化（按实际收益排序）

1. **Mihomo 路径的损耗：** 开着 Mihomo 时是 119–131 Mbit/s，不开时 213–246，差了约 45%，是目前最大的损失。主要办法是让 DIRECT 流量绕过 TUN，也就是国内直连的内核旁路，**用户已经明确说先不做**。不解决这一项，其他优化在开 Mihomo 时都被这个上限挡住。
2. **选频段比调带宽收益大：** W（2.4 GHz，调到 20 MHz 后约 100 Mbit/s）对比 W_Game（5 GHz，约 250）。
   - 同一个 SSID 双频的 AP：信号足够时（例如 ≥ -70 dBm），`pick()` 和连接时优先选 5 GHz 的 BSS。
   - SSID 不同的（像 W 和 W_Game）：如果 BSSID 只差最后几位，在扫描结果里提示"同一台路由器有 5 GHz 网络"。只提示，不自动切。
3. **5 GHz 转发时 CPU0 的软中断（71–88%）：** 经中继 213–246，直连 250，差距已经不大，RPS 又不稳定，暂不建议再投入。
4. **上行没有测量：** 现在只按下行打分，上传为主的场景（视频通话、备份）不受益。不能用 radio 的 `tx_time`：同信道时它主要是热点发给客户端的流量。暂时可以接受。

### 7.7 建议顺序

1. 先把第 5 节和上一轮评审提出的可靠性问题修完（Mihomo 恢复的补偿、点 Stop 后立即更新状态、启动器识别主服务、连接前的文案、MAC 显示由用户决定）。
2. 然后在 `Tuner` 里一次性改：锚点 + 每次复查一个挑战者 + 早停 + 窗口打分 + 样本新鲜度。先写测试，至少覆盖：
   - 7.2 第 1 条"复查后卡在 160"的场景（预测 40 好，160 和 80 一样差）；
   - Codex 的锚点场景（稳定 20 → 试 40 很差 → 回 20 时下载结束 → 超时必须回 20）；
   - 不单调的分数（160：300，80：280，40：500，20：250）最终能找到 40；
   - 早停：候选前两个样本不到锚点一半时立即退回；
   - 小样本和大样本混合时，窗口分数和逐 tick 平均不同。
3. 真机：在 W_Game 上记录长时间的调节日志（每次切换的原因、分数、持续时间），确认没有来回切换；W 上确认仍会稳定在 20 MHz。
4. 动手前确认没有别的 agent 还在改 `width.rs` 和 `wifi_relay.rs`。

安全约束（评审和继续操作时都要遵守）：
- 不要打印 `/tmp/zte-agent-relay/wpa.conf`、`saved-wifi.json` 的密码或 token。
- 不要输出无关网络的 SSID/BSSID。
- 不要改国家码。
- 不要禁用 `zte_topsw_daemon.conf` 里列出的守护进程（见 CLAUDE.md）。
