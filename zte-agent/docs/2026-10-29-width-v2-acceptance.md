# 2026-10-29 上游带宽 V2 真机验收

本文件按时间追加。下文最初的 G1 失败与 Observe 结束状态是历史快照；之后所有者已判定
G1 通过，完成阶段 B，并选择方案 A 进行有限 Auto 验证。最新进展见文末，不能把一次
短时下载测试当作完整阶段 C 或弱信号、热点客户端混合业务验收。
14:36 的本地 iperf 对照已排除公网路径：不刷新的 20 MHz 约 217 s 后掉速；60 s 刷新
维持了 11 分钟平均吞吐，但短时门槛和候选试探仍未通过。设备继续保留 Observe。

## 授权与范围

用户在了解 G1、阶段 B、阶段 C 的操作与影响后要求开始。本轮通过已授权 USB ADB
操作开发用 B31：部署 Observe、STA 160→80→160 MHz、单次厂商 5 GHz 热点重启。
未重启整台路由器，未关闭上游 AP，未修改国家码、蜂窝开关、热点持久配置或厂商同步服务。

**本轮初次 G1 结论：仅部分通过；当时完整 G1 未通过，阶段 B/C 未开始。**
停止后保留 Observe，未开启自动带宽动作。不能用本轮结果宣称已解决 5 GHz 重启恢复。

## 前置状态与备份

- 原 agent SHA-256：`24072cdc15e19d6404b2fa4640e7659952725f0379671b202488ff40209ef6ce`。
- STA：5220 MHz，44 信道，160 MHz；5 GHz 热点：44/80 MHz，2 个客户端。
  2.4 GHz 热点：11/40 MHz，0 个客户端。开发电脑通过 U60 的 LAN 联网。
- 中继与 Mihomo 均运行且 `autostart=true`。本轮开始时 5 GHz 上游已恢复可用，不能沿用
  前一天验收记录末尾“上游仍关闭”的未收尾描述。
- 私密归档：`/Users/g/Desktop/Workspace/u60-pro/device-backups/20261029-width-v2/device-before-clean.tar`，
  0600，16 个归档条目，含原程序、launcher、rc.local、中继状态目录和两频段 hostapd 配置。
  SHA-256：`19ac010bfcf3ce812af5068b71f8549b99a4951deec5987eb3b4a5b77814c71b`。
  首次 tar 因远端诊断混入 stdout 不能读取，未用作备份；重新生成相对路径、隔离 stderr 的
  归档后用 `tar -tf` 验证成功。
- 设备端安装回滚材料：`/data/zte-agent-install-width-v2-observe-20261029/`。
  `agent.before`、`start.before.sh`、`rc.local.before` 均保留，不打印其中可能存在的秘密。

## 部署前发现并修复的设备兼容问题

B31 的正常热点同时报告 `state=ENABLED` 与 `cac_time_left_seconds=N/A`。阶段 A
原实现把所有非数字值视为仍在 CAC/CSA，会阻止所有自动挑战。

增加 `b31_inactive_cac_is_reported_as_not_applicable`，先运行得到失败，再仅对两个 CAC
剩余时间字段的 `N/A` 视作无倒计时；DFS 状态、正数倒计时、CSA=1 和其他未知值仍阻止挑战。
三个热点解析测试通过；最终全量（4 线程）1189 通过、3 忽略，启动集成 1 通过。

修复后 release：
- aarch64-musl，`llvm-size -A` Total **8,576,164 B**；`gzip -n -9` **4,383,691 B**。
- SHA-256：`2070719d1b20ff49b1c44d81d3ef3ee3a7508f0ebb1509d88a0eeea0487fb505`。

为避免正常安装默认 Auto 在人工校准前抢先动作，本次直接复用
`scripts/install-agent.py` 生成的 launcher/installer，launcher 只增加
`export ZTE_AGENT_WIDTH=observe`。沿用原安装器的进程身份检查、锁、备份与哈希检查，
没有修改安装器逻辑或 rc.local。不是把 `setup.sh` 的默认 Auto 安装当作 Observe 使用。

部署后确认真实运行进程为 `/data/zte-agent`，运行环境为 Observe，设备哈希与本机一致。
中继、Mihomo 自动恢复；API 显示 `width_state=Observing`，ADB 与真实 LAN 登录页均 200，
匿名 API 401。令牌仅在内存中使用，未输出。

## G1-A：手动改变 STA 带宽——通过

保持同一上游、两个热点客户端，Observe 不发出自动带宽命令：

1. 建立基线快照。
2. `iwpriv u60sta0 chwidth 2`，确认 80 MHz；每约 1 秒采集一次，共 12 次。
3. `iwpriv u60sta0 chwidth 3`，确认 160 MHz；同样采集 12 次。
4. 最后再次确保 STA 为 160 MHz。

包括基线共 25 次快照，结果：
- STA 读回包含 80 和 160 MHz。
- 热点始终 **44 信道 / 5220 MHz / 80 MHz / center1=5210 MHz**。
- hostapd 状态、信道、宽度、中心频率字段无变化，配置哈希不变。
- 两个原客户端未消失，connected-time 无回退；原始 MAC 仅用于进程内集合比较，
  输出仅保存数量，不含客户端身份。
- 电脑对 LAN 地址的 ping 收到 51 个回复，无 `Request timeout`；测试结束主动终止 ping，
  不把缺少最终汇总解释为完整丢包率测量。

证据在私密备份目录：`g1-width-results.json`、`g1-run.log`、`g1-lan-ping.log`。

## G1-B：之后重启热点——未通过

执行一次厂商 `/etc/init.d/hostapd-daemon.init restart wlan2`，命令返回 0。
随后观察 `HT_SCAN` → `ENABLED`，两个客户端重新连接，STA、中继与 Mihomo 继续运行。

**热点重启后变为 44/160 MHz（center1=5250），不是重启前的 44/80 MHz。**
因此不满足 G1“热点重启起来的信道和宽度与改之前相同”，完整 G1 不通过。

同时有以下限制，不能直接认定 STA setter 造成了持久污染：
- 重启前实际读取的 hostapd 配置已为 `channel=0`、`eht_oper_chwidth=2`、中心频率索引 0，
  即自动选信道、160 MHz；运行时 44/80 与持久配置本来就不同。
- 重启前后 `/data/vendor/wifi/hostapd-wlan2.conf` SHA-256 均为
  `c7d41d1b1b1d1f1f9ba37990d89a1f8b6e8e283313a0e008faf88ba66c5e08df`。
- 可能是热点重启重新使用持久配置，取代了旧有 SAP–STA 并发策略的运行态，但本轮没有
  做“完全不改 STA 带宽，仅重启热点”的独立对照，因果尚未确认。

没有为凑成通过而手动改热点带宽，亦没有再重启热点或路由器。后续优先单独研究
持久配置、运行态 CSA 与热点重启的关系，再决定是否重跑 G1 和开启 Auto。
证据：`g1-restart-states.json`，厂商脚本原始输出单独保存在私密日志中，不提交。

## 结束状态

- 新版本仍已安装，启动脚本明确 Observe（重启 agent 后也保持），不自动调整 STA 带宽。
- STA 44/160，5 GHz 热点 44/160、2 个客户端；2.4 GHz 热点 11/40。
  **5 GHz 热点运行宽度与本轮开始不同，未声称完整恢复初始运行态。**
- 中继、Mihomo 均运行且无报告错误，管理页面可从真实 LAN 访问。
- rc.local 哈希保持 `09e3cef3605f0b83d942df14cb01541f2b2b349f8a0e308d7df9118488ee5267`。
- daemon 同步：`sync success`、`register success`。
- 没有运行阶段 B 的受控测速矩阵，没有开启阶段 C，也没有启动无人值守长时间观察任务。
  现有 Observe 自然业务窗口日志不能替代评分校准，不对 150/280 Mbit/s 作归因。
- 代码与记录均未提交。

## 后续只读排查：热点重启前 80、重启后 160 的原因

本节全部为只读：读取设备状态、内核日志、厂商脚本，静态反汇编 `wlan.ko`；没有改设备、
没有重启热点/agent/STA。设备读取时间 2026-10-29 03:00 前后，boot 未变（uptime 约 3.8 h）。

### 结论

**80/160 的差别由“热点和 STA 谁先起来”决定，不是 STA setter 留下的持久改动。**

| 顺序 | 驱动行为 | 结果 |
|---|---|---|
| 热点先起（无 STA），之后 STA 连 5 GHz | 热点 ACS 起在 44/160（中心 5250，含 DFS 52–64）。STA 连接时，`wlan_hdd_cm_connect` 内联的 SAP–STA DFS 并发处理把热点带宽逐级降到非 DFS（`get_next_lower_bandwidth` 直到 bonded 状态为 ENABLE），再用 CSA 切到 44/80，等待 10 s | CSA 完成：44/80（本轮开始时的运行态，10-28 记录第 26 行同样如此）；CSA 未完成：P0 卡死 |
| STA 已连上，之后热点重启 | 02:41:04 `ACS request` → `START AP ... 5220 bw 5` → `vdev_start ... center_chan 5210 center_freq2 5250` → `AP started freq 5220 BW 3`，CAC 立即开始又结束 | 信道跟随 STA（44），宽度取持久配置 160（G1-B；E2 证明与 STA 当时的宽度无关） |

- G1-A 期间（02:40）`iwpriv chwidth` 没有触发 STA vdev 重启，也没有热点 CSA；G1 结束时
  STA 已回到 160，热点重启时继承的是 STA 当时的 160。hostapd 配置哈希不变。
- 此处原先推测“热点重启会继承 STA 当时的宽度”，已被下文 E2 否定。
- 因此 G1“重启后与改之前相同”在这台设备上取决于启动顺序；原来的 44/80 本身就是
  一次 CSA 的产物，而不是持久配置（持久配置为自动信道、160）。

### P0 机制（同一条路径的失败分支）

1. ini（`/etc_ro/wlan/WCNSS_qcom_cfg.ini`）含 `gDisableDFSChSwitch=1`、`g_zte_dfs_cac_timeout=3`。
2. 5 GHz 按 `*160` 启动时，`/lib/wifi/qcacld32.sh` 的 `handle_dfs`（以及 `zte_post_wifi.sh`）会启动
   `/sbin/zte_wifi_dfs_hdlr.sh`。在 `wireless.wifi1.channel=0` 时，该脚本执行
   `iwpriv wlan2 disableDfsCS 1`，`sleep 599` 之后再执行 `disableDfsCS 0`（`wireless.zte_mbb.dfs_channel_switch_disable` 本机未设置）。
   `reconf_qcawifi` 会先 `stop_dfs_hdr` 杀掉这个脚本。
3. 反汇编（`wlan.ko` SHA-256 `11fd7a04dd435c835aece59f93accce328bb979445f01679d13038eec01b999e`）：
   `disableDfsCS` → `ucfg_mlme_set_dfs_disable_channel_switch` 写 MLME 配置字节 `+0xd19`；
   `wlansap_roam_process_dfs_chansw_update` 一开始就检查这个字节，为真时只打印
   `sapdfs: DFS channel switch disabled`、`pre_cac_complete_set(false)`、`sme_roam_start_beacon_req` 后返回，
   **不调用 `wlansap_channel_change_request`**。热点不会真正换信道，`wlan_hdd_cm_connect` 等的
   事件 10 s（`0x2710`）超时，`chan_switch_in_progress` 不被清除，之后每次连接都报
   `Channel switch in progress!!`；热点 stop_bss 时才清（02:41:04 `Set chan_switch_in_progress: 0 vdev 1`）。
4. 10-28 私密 dmesg 时间线完全吻合：52.7 s 热点 44/160 → 53.9 s `zte_wifi_dfs_hdlr 14: disableDfsCS 1`
   → 55.8 s CAC 完成 → 66.2 s STA 连接触发 CSA，打印 `DFS channel switch disabled` → 75.2 s
   `wait for qdf_event failed` → 此后一直失败；652.9 s 才有 `disableDfsCS 0`（dmesg 在 672 s 截止，
   之后卡死是否自愈**未知**）。期间热点客户端的 `wlan_hdd_add_key_sap` 也在等 CSA。
5. 推断（本 boot 的开机段 dmesg 已滚出，无法直接证明）：本 boot 是 10-28 末尾那次“上游关闭时重启”，
   STA 在 10 分钟窗口之后才连上，CSA 成功，所以本轮开始时热点是 44/80。

推论（未做实验验证）：当前配置为 5 GHz 160 且 freqlist 只含 36–64、149–165，任何 160 MHz
热点都覆盖 DFS。只要开机后 10 分钟内上游已在、中继就去连 5 GHz，P0 就会稳定复现。

### 读取时的设备状态与风险（修复前；修复后见下一节）

- 热点 44/160（中心 5250，覆盖 DFS），STA 44/160，2 个客户端。02:41 的热点重启走
  `hostapd-daemon.init`，当前 dmesg 中没有 `zte_wifi_dfs_hdlr` 记录，也没有该进程在运行，
  所以开关大概率为 0（无法直接读取）。
- **这正是 P0 的前置状态**：下一次 STA 重连（重新部署或重启 agent，relay 会断开后重连，
  例如 02:37 部署时的 `restored after 1 attempt(s)`；上游抖动）都会走 CSA 降到 44/80 的路径。
  开关为 0 时应当成功；如果在 Wi-Fi 完整重启（整机重启或厂商改 Wi-Fi 设置）后 10 分钟内重连，就会卡死。

### 证据

私密目录 `device-backups/20261029-width-v2/g1-followup-dmesg.private`（0600，本次 dmesg 全量，
覆盖 uptime 2141 s 起；开机段已滚出）。10-28 证据见 `20261028-review-acceptance/5ghz-reboot-dmesg.private`。
厂商文件 SHA-256：`zte_wifi_dfs_hdlr.sh` `8e84c557…2f2b`，`qcacld32.sh` `80a027b3…6532`，
`zteqcawifi.sh` `fa09a294…d8a8`，ini `3094f627…e247`。

## 继续：E3、P0 修复、E2（同日，用户授权）

用户看过上节后决定：P0 由 agent 直接把驱动的 DFS 信道切换开关打开（**所有者决定 62**），
并要求“有必要的话”做 E3、E2。两项都做了。未重启整机，未改热点持久配置、国家码、蜂窝开关或厂商同步服务。

### E3：热点在 44/160 时改 STA 宽度——通过

沿用 G1-A 脚本（输出改为 `e3-*`），热点 44/160（中心 5250）、2 个客户端：
STA 160→80（读回 80、中心 5210）→160，每约 1 s 一次快照，共 25 次。热点信道、宽度、中心频率
和 hostapd 状态始终不变，客户端无缺失、无重连；LAN ping 51 个回复、0 超时。
证据：`e3-width-results.json`、`e3-lan-ping.log`。结论：两种热点状态（44/80、44/160）下，
改 STA 宽度都不会移动热点。

### P0 修复（`wifi_relay/dfs_switch.rs`）

- `join()` 在关联（`association_step`）之前调用 `dfs_switch::allow()`：若 `/sbin/zte_wifi_dfs_hdlr.sh`
  在运行、但还没进入它的长 sleep（即可能还要执行 `disableDfsCS 1`），最多等 10 s；随后执行
  `iwpriv wlan2 disableDfsCS 0`。失败只记日志，不阻止连接；只在厂商脚本运行时记一行成功日志。
- 4 个单元测试（无脚本、脚本在 `sleep 1`/`iwpriv` 阶段、脚本在长 sleep、只认脚本自己的子进程且
  不误认 `grep`）；两处变异（忽略 `sleep 1`、忽略父进程）各被一个测试抓到。
- 全量 `cargo test --locked -p zte-agent -- --test-threads=4`：1193 通过、3 忽略，启动集成 1 通过。
  其中一次全量里 `web::tests::esim_add::a_photo_is_read_on_the_router` 失败，单独重跑 3 次通过、
  再跑全量通过，属于已知的图片解码间歇失败，未修改。clippy 无指向新文件的警告。
- 最终 release：`llvm-size -A` Total **8,584,580 B**，`gzip -n -9` **4,387,195 B**，
  SHA-256 `d7a52424ff4b66b5686c1d8cfa6bf883006422ccedc7f1732fce8358a7e8f77e`。

### 修复的真机验证——通过（模拟厂商窗口，非整机重启）

1. 暂存目录 `/data/zte-agent-install-dfs-switch-20261029/`（`agent.before` 为上一版 Observe 构建
   `2070719d…`，`rc.local.before`），沿用同一个 Observe launcher 与安装器。
2. 热点 44/160、STA 已连接时，手工执行 `iwpriv wlan2 disableDfsCS 1`（与 10-28 失败前厂商脚本
   执行的命令相同），随后安装新 agent。安装重启 agent 会让中继拆掉并重连 STA。
3. 结果（`dfs-fix-deploy.json` 与内核日志）：uptime 15590.75 热点 `chan_switch_in_progress: 1` →
   以 80 MHz（中心 5210）重启 vdev → 43 ms 后清零 → STA `Connect ... SUCCESS`。没有
   `DFS channel switch disabled`、`wait for qdf_event failed`、`Channel switch in progress`。
   两个客户端始终在线，热点停在 44/80。开关在此前被设为 1，期间只有新 agent 会把它设回 0。
4. 之后为加入日志又部署一次最终版本（热点已是 44/80，重连不涉及 CSA）：`restored after 1 attempt(s)`。

未做：真实整机重启（厂商脚本真的在窗口内）这一最终验收，需要所有者同意。

### E2：STA 为 80 时重启热点——推测被否定

STA 设为 80（读回 80）→ 厂商 `hostapd-daemon.init restart wlan2`（返回 0）→ 约 3 s 后 `ENABLED`。
内核日志与 G1-B 相同：`START AP ... 5220 bw 5`、`AP started ... BW 3`，热点 **44/160**（中心 5250），
不是 80。客户端约 11 s 内全部回连；STA 改回 160 后热点保持 44/160。hostapd 配置哈希不变，
本次重启没有启动 `zte_wifi_dfs_hdlr.sh`。证据：`e2-results.json`。

结论：热点重启时信道跟随已连接的 STA，宽度取持久配置（160），与 STA 当时宽度无关；
Auto 改 STA 宽度不会通过热点重启间接改变热点宽度。

### 对 G1 的影响

- G1-A 在热点 44/80、44/160 两种状态下都通过。
- G1-B 的“重启后与改之前相同”不成立，原因是“改之前”的 44/80 本身是一次 STA 连接 CSA 的产物；
  热点重启的结果（STA 信道 + 配置宽度 160）不受 STA 宽度及其改动历史影响。
  是否据此判定 G1 通过，由所有者决定。阶段 B、C 仍未开始。

### 最终状态（本节）

- 运行 `/data/zte-agent`（`d7a52424…`），launcher 仍为 Observe（`72fb8b24…`），`width_state=Observing`。
- 热点 44/160（中心 5250），STA 44/160，2 个客户端；2.4 GHz 热点 `ENABLED`、11 信道。
  热点回到配置宽度，不是本轮开始时的 44/80；下次 STA 重连会经 CSA 降到 44/80，开关已由 agent 打开。
- 中继 `connected`、Mihomo `running`，均无错误；LAN 登录页 200，匿名 API 401。
- rc.local `09e3cef3…`、hostapd-wlan2.conf `c7d41d1b…` 未变；daemon `sync success`、`register success`。
- 无 ADB 转发、无后台观察任务。代码与记录均未提交。

**所有者决定（2026-10-29）：按上述说明判定 G1 通过，开始阶段 B。** 整机重启的 P0 最终验收本次不做。

## 阶段 B（同日，所有者授权：本机 Mac 作测速服务器，2.4 GHz 与 5 GHz 都做）

结果与结论写在 `network-extensions.md` 的“阶段 B 校准结果”，这里只记操作与状态。

- Mac 由所有者手动加入 `W_Game`（`networksetup` 两次加入失败，-3900，未读取钥匙串），
  `iperf3 -s` 绑定其家庭网地址；U60 本机作客户端。设备上另起一个只复制 `[relay` 日志行并加
  uptime 的 tail（`/tmp/u60-stageb-tail.sh`），结束后已停止并删除。
- 5 GHz 矩阵 21 轮（A：160/80/40 × 下载/上传/双向/低速/分段；B：40/80/160 × 下载/上传）。
  80 分段（A）、40 分段后 3 次突发、40 下载/上传（B）受宽度失效影响，分析时剔除。
- 5 GHz 失效实验：设 80 后持续下载 480 s，第 186 s 失效；第 330 s 重发同值无效；第 410 s
  160→80 恢复。
- 经中继 API 连 `W`（2412 MHz，原密码，IPv6 开）；`wlan0` 按设计临时到 1 信道 20 MHz。
  2.4 GHz 矩阵 14 轮，每轮先切到另一档再切回目标档。2.4 GHz 失效实验：设 20 后约第 100 s
  下行塌到约 3 Mbit/s，第 300 s 重发同值无效，第 330 s 40→20 恢复，结束恢复 40。
- 5 GHz 热点上唯一的客户端在 uptime 42705 自行离开（原因码 8，早于塌陷），不是重连造成。
- 经 API 连回 `W_Game`：热点 44/160→44/80 CSA 42 ms 完成，无卡死标记；`wlan0` 回到 11 信道 40 MHz。
  随后发现 2.4 GHz 遗留的 40 MHz 设置让 5 GHz 下载只有 272–287 Mbit/s，`chwidth 3` 无效，
  80→160 后恢复 538 Mbit/s、物理速率 160 MHz。
- `ll_stats` 计时：空闲与 160 MHz 下载时读取均 ≤10 ms。

### 最终状态（阶段 B 之后）

- agent `d7a52424…`，launcher Observe（`width_state=Observing`）；中继 `W_Game` 5220 MHz、
  STA 44/160（已复位），热点 44/80（当时 0 个客户端），2.4 GHz 热点 11 信道 40 MHz。
- 中继 connected、Mihomo running，均无错误；恢复目标 `resume.json` 仍为 `W_Game`、IPv6 开。
- rc.local `09e3cef3…`、hostapd-wlan2.conf `c7d41d1b…` 未变，hostapd-wlan0.conf 修改时间仍为
  2025-01-04；daemon `sync success`、`register success`。
- Mac 的 iperf3 已停止；无 ADB 转发，设备上无本次辅助进程或文件。**Mac 仍连在 `W_Game`，
  需所有者手动切回 U60 热点。** 阶段 C 未开始。代码与记录均未提交。

## 方案 A：窄档定期刷新、连接复位（2026-10-29，续接至 13:44）

### 所有者决定与实现

所有者选择方案 A，并要求测试连接复位。没有授权本轮整机重启或修改热点持久配置。
原会话在已部署 Auto、准备持续下载验证后遇到会话限额；续接只读取旧 transcript，
以当前工作区、正在运行的程序和新采样为准，没有恢复或修改旧提供方会话。

- `width.rs`：Auto 保持低于协商上限的稳定锚点时，每约 60 s 经上限重新施加一次；
  空闲不执行，恢复业务时补做；挑战、回滚、暂停期间不插入。刷新不消耗挑战预算，
  丢弃旧窗口并跳过一个合格 tick，不能把切换瞬态计入候选分数。
- `width_watch.rs`：切到协商上限，等 1 s，再切回锚点，丢弃采样端点；不重连 STA。
  连续相同刷新日志会被去重，**日志条数不是执行次数**；本轮用 `iw` 宽度采样验证动作。
- 新关联成功后、DHCP 和 `LinkWidth` 初始化前，三种模式都先切窄一档、等 1 s、
  再回到协商宽度并读回，清理跨连接保留的限制；20 MHz 没有更窄档时跳过。
  复位失败只记日志，不阻止连接；不清除历史评分。
- 本续接未修改 Rust 实现或评分阈值，只补验证与文档，保留所有既有未提交改动。

### 本地验证与构建

原会话新增 6 个测试，4 处变异均被抓到。续接重新验证当前工作区：

- `cargo test --locked -p zte-agent -- --test-threads=4`：**1199 通过，3 忽略**；
  启动集成另 **1 通过**。宽度相关筛选 `wifi_relay::width`：**52 通过**。
- `cargo clippy --locked -p zte-agent --all-targets`：退出 0；保留仓库既有警告，
  无指向本轮宽度文件的警告。`git diff --check` 通过。
- aarch64-musl release 构建通过。无 `aarch64-linux-musl-size`，继续用
  Command Line Tools `llvm-size -A`：**Total 8,584,616 B**；`gzip -n -9`：**4,388,223 B**。
  比 P0 最终构建分别增加 36 / 1,028 B。
- SHA-256：`dc7ec7e1c0be68a4f1f19ed2c03f4488304cafd13c2f6a475076ac4452be211d`。
  本机产物、设备文件与实际运行 `/data/zte-agent` 一致。

### 连接复位：前后对照通过

原会话先在旧 agent 的 5 GHz 连接手动留下 40 MHz，然后经 API 重连：
`iw` 虽读回 160 MHz，上游 RX 仍为 `516.0 MBit/s 40MHz`，没有复位日志。
部署新版 Observe 后记录 `upstream width reset to the negotiated 160 MHz`，
下载期间 RX 为 `1729.6 MBit/s 160MHz`。这验证了旧版的遗留问题以及新版的复位路径，
不是只看 `iw` 读回就宣称通过。切到 2.4 GHz 后也记录协商 40 MHz 复位。

续接结束再从 2.4 GHz 切回 5 GHz 并恢复 Observe，同样记录 160 MHz 复位；
10 MB HTTPS 完整下载，RX `1441.3 MBit/s 160MHz`。未做弱信号或协商仅 20 MHz 的真机复位矩阵。

### 2.4 GHz Auto 持续保持：未通过，根因尚未隔离

目标是 44 次 30 MB HTTPS 下载，限速 2 MiB/s，持续超过原来约 100 s 的失效点。
续接时设备仍为 Auto / 2412 MHz / 20 MHz，但 `/tmp/u60-load.out` 不存在、负载进程不在。
旧监控只有少量事件并已过期，**没有完成证据，不能算作已跑完 11 分钟**。

重新以前台有界负载、明确开始/逐文件/结束标记执行：

1. 普通 DNS 路径：前三文件平均 2.028 / 0.931 / 1.033 MB/s；第 4 个在 40 s 超时，
   收到 24,158,009 B，平均 0.604 MB/s。随后的独立短时采样出现多次 DNS 解析失败，
   恢复后两个完整文件均约 2.099 MB/s。因此此轮不能隔离宽度效果。
2. 直接解析上游 DNS 并验证可达后，以 `curl --resolve` 固定同一个 HTTPS 域名的地址，
   保持 TLS 校验，指定 `u60sta0`、禁用代理环境；未改系统 DNS 或 Mihomo 配置。
   该轮 uptime **51282.09–51475.12**，约 **193 s**：前 9 文件完整、HTTP 200，
   平均 **1.938–2.100 MB/s**；第 10 文件在 **45.002 s** 超时，只收到 **11,077,887 B**，
   平均 **0.246 MB/s**，随即停止，没有凑满 44 次。
3. 每约 0.5 s 读取 STA 宽度、RX 计数、信号和速率；只保存数值、不保存客户端身份。
   初段一次正常 20→40 挑战约 16.6 s 后判 Lost 并回滚，热点保护未报告字段变化。
   之后两次维护刷新在 uptime **51384.01**、**51444.51** 开始，间隔 **60.50 s**，
   40 MHz 各持续约 **0.99 s** 后回到 20。第二次刷新期间下载仍慢，此连接未明显恢复。
   因而已排除“60 s 刷新根本没执行”，但不能据此证明驱动失效就是全部根因。
4. 每个文件结束时同频热点均为 **1 信道 / 20 MHz / ENABLED**，0 个客户端；
   未触发热点重启或中继重连。采样信号 −44…−32 dBm，仍不是弱信号测试。

**判定：方案 A 的持续下载门槛未通过，完整阶段 C 未通过。**
DNS 失败能解释部分失败，但固定地址后仍慢，所以也不能把全部问题归于 DNS。
公共 CDN、WAN/TCP 与驱动保持效果尚未隔离；没有为得到通过而更改评分阈值或继续无人值守压测。

下一步需在上游 LAN 放受控 iperf 服务端，用同一本地路径对照 Auto 20 MHz 与固定
40 MHz，至少跨越多个刷新周期，并记录吞吐和实际宽度切换。如果本地路径也下降，
再一次只变一个参数研究刷新间隔或切换停留时间。当前 Mac 已在 U60 LAN，不能把它
当作仍位于上游 LAN 的阶段 B 服务端；需要先安排该测试拓扑。

### 收尾状态

- 中继恢复原网络 `W_Game`、5220 MHz，STA **44/160**，`width_state=Observing`；
  启动脚本与实际进程均为 **`ZTE_AGENT_WIDTH=observe`**，不把失败后的 Auto 留作日常配置。
  新版程序保留，所以后续连接复位仍有效；代码默认 Auto 未改。
- `wlan2` **44/80**，`wlan0` **11/40**；中继 connected、Mihomo running，均 autostart、
  无 error 或 recovery-required；Mac 在 U60 LAN，未读取钥匙串或修改其 Wi-Fi。
- 真实 LAN 登录页与 ADB 登录页均 200，匿名 API 401；10 MB 固定地址 HTTPS 成功，
  平均 3.066 MB/s。这个单次值只验证恢复联网，不是 5 GHz 吞吐校准。
- rc.local `09e3cef3…`、hostapd-wlan2.conf `c7d41d1b…` 未变；daemon `sync success`、
  `register success`。未重启整机或单独调用厂商热点重启，未修改国家码、蜂窝开关及厂商
  同步服务；中继跨频段切回时按原有逻辑恢复 `wlan0` 的运行态配置。
- Observe 收尾沿用已验证安装器，设备端回滚材料保留在
  `/data/zte-agent-install-width-observe-final-20261029/`（含此前 Auto launcher）。
  原方案 A 安装目录 `/data/zte-agent-install-width-reapply-20261029/` 仍保留。
- 已核对哈希后删除本轮及旧会话遗留的两个临时负载脚本；无负载进程、无 ADB 转发。
  含设备配置的本轮临时 launcher 已删除，不打印或提交。
- 私密证据：`device-backups/20261029-width-v2/width-reapply-followup-20261029T1315/`，
  目录 0700、文件 0600；含成功/失败原始结果、数值采样、分析器、构建测试日志、
  最终状态及 SHA-256 清单。文件位于仓库外，不提交。
- 所有代码、文档均未提交。真实重启、弱信号、热点客户端路径和数小时混合业务仍未验收。

## 上游 LAN 本地对照（2026-10-29 14:00–14:36）

### 范围、拓扑与事先门槛

所有者要求开始测试，随后手动将 Mac 从 U60 热点切到上游 `W_Game`。命令加入网络曾
返回 macOS `-3900`，没有读取钥匙串；确认 Mac 为 `192.168.2.147`、默认网关 `192.168.2.1`。
Mac 的当前链路为 5 GHz 44/160、信号 −50 dBm、噪声 −89 dBm、协商速率 1297 Mbit/s。
U60 通过已授权 USB ADB 控制，测试时中继到 `W`、2412 MHz；未改 Rust 源码或配置阈值。

路径为 **Mac → 上游 AP 的 5 GHz → 同一路由器的 2.4 GHz → U60 `u60sta0`**：
只用上游私网地址，iperf 绑定 STA 地址与 `--bind-dev u60sta0`，不经过 DNS、CDN、
互联网或 LAN 客户端的 Mihomo 转发。Mac iperf 服务端只绑定上游地址、单次连接后退出；
测试进程持有临时防休眠，结束即清理。Mac / U60 iperf 版本分别为 3.19.1 / 3.21。

- 主要负载：单 TCP reverse 流，目标 **20 Mbit/s**，每秒接收报告；固定 40、固定 20
  各 240 s，Auto 维持 660 s。固定档只在开始前切另一档再切回，不周期刷新。
- 事先门槛：完整执行，且除启动段外每个 **10 s** 接收窗口 **≥15 Mbit/s**。
  原始窗口保留；不因总平均好看而忽略停顿，不在看到结果后改门槛。
- 每约 0.5 s 采集 STA 宽度、RX 字节、速率、信号；每约 10 s 采集两热点几何与客户端
  数量，不保存客户端身份。硬件失效有约百秒延迟，因此本反馈循环必须运行数分钟。

### 固定宽度对照：本地复现保持失效

| 用例 | 时长 | 完整秒窗口加权均值 | 关键结果 |
|---|---:|---:|---|
| Observe 固定 40 MHz | 240 s | 3.48 Mbit/s | 从一开始就低，最差 10 s 为 0.944；不能当作稳定 20M 的健康基线 |
| Observe 固定 20 MHz，不刷新 | 240 s | 18.47 Mbit/s | 前约 210 s 维持 20；210–220 s 为 15.20，220–230 s 为 3.985，230–240 s 为 4.299 |

固定 20 的全过程 `iw` 都是 20 MHz，没有外部宽度变化或关联重建。两组热点采样均保持
`wlan0` 1/20、ENABLED、0 客户端，`wlan2` 44/80。由此本地路径已能重现“宽度读回不变、
保持一段时间后下载下降”，不能再把问题仅归于公网/CDN/DNS。此次约 217 s，而非旧实验
的约 100 s，说明失效时间不是固定常数。

40 MHz 整体很慢仍需排除路径容量瓶颈，因此最后补了同路径、同一 TCP 流、不限速的
Observe 20→40→20 三段（每段开始重新施加宽度，各 30 s）：完整秒窗口加权均值为
**57.39 / 0.84 / 95.06 Mbit/s**（iperf 最终接收汇总为 **58.23 / 0.84 / 95.17**）。
两次 20 差异明显，不能算固定性能倍率；但可以排除“整条本地路径最多只有几兆”。
当前环境下的 40 MHz 明显更差，不适合作为本轮持续负载的健康对照。

### Auto 首次挑战：回滚读回不等于网络已恢复

沿用原程序、只将 launcher 从 Observe 切为 Auto；仍由原安装器做锁、身份检查与备份。
关联复位到 40 后，历史起点自动设 20，初段下载约 20 Mbit/s。

- uptime **54006.39** 开始挑战 40，至 **54033.20** 回到 20，约 **26.81 s**。
  日志判 `Lost reason=early stop`：锚点 E=115.16、R=20.75；候选 E=8.62、R=3.86。
  热点字段无变化，并记录 `rollback confirmed width=20 hotspot_restored=true`。
- 原 TCP 的 10 s 接收窗随后为零，约第 80 s 因 15 s 无数据超时退出，未进入 660 s 维持测试。
  不把“尚未发生周期刷新”的失败当作周期刷新本身无效。
- 在保持原状态下，新建一次 TCP 也在 5 s 连接超时：不仅仅是旧 TCP 恢复慢。
  U60 到上游网关、Mac 各 **4/4 ping 丢失**，但 `wpa_state=COMPLETED`、`iw` 仍是 20；
  Mac 到同一网关 **3/3 成功**，平均 3.69 ms。
- 只手动执行一次 `40 → 等 1 s → 20`（uptime **54232.90** 起），没有重连、重启或
  改路由：网关 **5/5**、Mac **3/3** ping 立即恢复。此单次恢复强烈支持 STA/宽度状态异常，
  尚不能定位到驱动内部的哪个状态，也不能证明每次试探都会发生同样的持久断流。

代码接入有一个与现象一致的恢复盲点：`width_watch::step` 的 demand 只看 RX 增量，
稳定期无 RX 会直接走 `idle()`，到不了状态机的 60 s 刷新。因此“已关联但数据收不到”
可能被当作空闲，无法靠这条维护路径自救。这里区分**已观察的断流**与**代码推导的风险**，
没有在本轮偷偷增加空闲驱动查询、健康探测或修改恢复策略。

### 已稳定锚点的维护刷新：长时吞吐有效，严格短窗门槛仍失败

手动恢复后，继续使用同一 Auto 关联及正常候选冷却，不改缓存、不关闭调优、不手动
周期刷新；新 TCP 运行完整 **660 s**，全程无新的候选挑战：

- 完整秒窗口加权均值 **19.9785 Mbit/s**，最终接收汇总 **20.0008 Mbit/s**；
  每分钟均值均为 **19.923–20.011 Mbit/s**，最后一分钟约 19.993。
- 采到 **11 次**正常维护刷新，间隔 **60.06–60.58 s**，40 MHz 停留采样值
  **0.90–1.40 s**（0.5 s 左右采样精度，命令配置仍是 1 s），随后恢复 20。
- 未复现不刷新组的持续约 4 Mbit/s 塌陷。但 490–500 s 窗口仅 **13.631 Mbit/s**，
  后一个窗口为 **30.723**，说明短时欠速由后续突发补回，均值掩盖了波动。
  事先设定的“每 10 s ≥15”门槛仍为 **FAIL**，不更改结果。
- 66 次热点快照均为 1/20、ENABLED、同频客户端 0；信号 −45…−41 dBm。
  没有证明弱信号、热点客户端转发或混合上传也具有同样表现。

所以证据支持的是“**60 s 刷新在这个本地负载下维持了长期吞吐**”，不是“方案 A / Auto
全部通过”。本轮只有一次 11 分钟维持测试，不是数小时稳定性认证。

### 第二次正常挑战：长停顿可再次出现，但回滚后能够自行恢复

不重新关联、不改候选历史，正常冷却到期后新开 180 s 负载。20→40 的采样区间为
uptime **54998.15–55044.33**，约 **46.18 s**；日志为 `Unknown reason=trial deadline`，
候选没有完成可用评分窗口。采样的 46 s 包含 watcher tick/读回延迟，不等于配置期限被续期。

- 20–60 s 的 10 s 吞吐窗仅 **0.315–0.734 Mbit/s**；几乎不能用，不能只凭“最终回滚”判通过。
- 回到 20 后，70–100 s 的窗口约 **44–59 Mbit/s**，补回积压数据，之后约 20；
  180 s 总均值仍是 **19.981 Mbit/s**，但最差窗口 **0.315**，严格门槛失败。
- 本次自行恢复，没有像第一次那样持久 ping 黑洞。两次共同点是较差候选带来长停顿，
  不把一次未自愈过度推广为每次必然发生。热点字段仍未变化。

### 结论、后续与最终状态

**完整 Auto / 阶段 C 仍未通过，设备恢复 Observe。** 本地受控结果已把研究重点从
“公网测速是否可信”推进到两个可分开的行为：周期维持有效但短时抖动；候选试探带来
长停顿且回滚读回不能证明数据路径恢复。后续优先研究有上限的回滚后健康确认/恢复、
无 RX 与真实空闲的区分，以及严重退化候选为何可能耗尽 45 s 期限；不是先改 E 的胜负阈值。
这些修复需要回归测试和新的真机对照，本轮仅测试与记录，未修改 Rust、提交或整机重启。

- 14:36 恢复原 `W_Game` 5220 MHz，STA **44/160**；进程和 launcher 均 **Observe**，
  API `width_state=Observing`。中继与 Mihomo 均 autostart、正常运行，无 error/recovery-required。
- `wlan0` **11/40**、`wlan2` **44/80**。launcher SHA-256 `72fb8b24…`、
  rc.local `09e3cef3…`、hostapd-wlan0.conf `fc7ff33a…`、hostapd-wlan2.conf `c7d41d1b…`
  与测试前完全一致；agent 仍 `dc7ec7e1…`。daemon sync/register success。
- 上游网关 ping 4/4 成功；ADB 登录页 200、匿名 API 401。当前 Mac 仍在上游 LAN，
  已通知所有者可切回 U60；本轮不声称已从 Mac 验证 U60 LAN 侧管理访问。
- Mac 服务端、设备 iperf、临时防休眠与所有 ADB 转发均停止；没有无人值守负载。
  设备安装回滚材料在 `/data/zte-agent-install-local-width-auto-20261029-141143/` 和
  `/data/zte-agent-install-local-width-observe-20261029-143319/`，均保留。
- 私密证据目录：`device-backups/20261029-width-v2/local-reapply-Vzpm6O/`（仓库外，
  目录 0700、文件 0600），含可重跑测试脚本、逐秒 iperf、STA/热点采样、脱敏决策日志、
  判定摘要、最终状态与文件哈希。无令牌输出，不提交原始网络数据。

### 本轮软件验证

Rust 源文件哈希与测试前一致。正常主机 `umask 022` 下重新运行全量：**1199 通过、
3 忽略，启动集成另 1 通过**。最初在为私密证据设置的 `umask 077` 下运行全量，
`tls::hosts::tests::the_temporary_file_is_new_and_outside_the_directory` 因实际权限
0600、期望 0644 失败（1198 通过）；显式 077 单测重现、022 单测通过，确认与测试
进程的 umask 有关，不宣称是随机失败或本轮带宽改动造成，也未修改无关 TLS 实现。

Release 构建成功，程序 SHA-256 仍 `dc7ec7e1…`；本机没有 `aarch64-linux-musl-size`，
继续用 `llvm-size -A` 得 **Total 8,584,616 B**、`gzip -n -9` **4,388,223 B**。
`git diff --check` 通过。只更新三个验收/设计文档，既有未提交改动保留，未提交。

## 健康探测、数据路径确认与负载保护（2026-10-29 15:50–18:55）

所有者保留 E 方案，要求坏宽度的试探尽快退出、回滚后确认数据路径，并允许为此部署和
真机测试。本节记录先做的探测研究、实现、八次部署和真机结果。设备全程在所有者桌面，
Mac 与 U60 同在上游 LAN（`192.168.2.x`），本地 iperf3，不经公网、Mihomo 或蜂窝。

### 探测研究（Observe，`W` 2.4 GHz 信道 1）

- `W` 广播 HT20/HT40，原生协商 40 MHz；所有者的 iPhone 个人热点只广播 HT20
  （“secondary channel offset: no secondary”），不能做 20/40 对照。
- 固定 20 MHz 不限速 60 s 均值 97 Mbit/s（之后各轮 24–34，原因未查）；40 MHz（原生或
  `iwpriv`）0–11.5 Mbit/s，有流量后接收速率掉到 HE-MCS 0（34.4 Mbit/s），信号 −33…−36 dBm。
- 小包 ping 4 次都没发现坏的 40；60 个 1400 B 的 ping 序列只发现 1 次；3 s 洪泛
  （`ping -f -l 8 -s 1400`）4/4 发现：40 MHz 丢 25–67%、回包只有 20 MHz 的 5–29%，
  20 MHz 丢 0.2%、约 9 Mbit/s。洪泛只走 Wi-Fi 这一跳，回包率受往返时延限制，不是测速。
- `iw survey dump` 在用信道的计数有流量时不增长；`iw station dump` 的 `rx packets`
  正常时也停在 2。两者都不能用于判断。

### 实现

- **挑战前参照、确认后探测：** 普通挑战先在锚点做 2 s 洪泛作参照（发 ≥50 个且丢包
  <5% 才可用，截止时在途的 8 个不算丢）；候选读回确认后立刻再测，丢包 ≥10% 或回包率
  不到参照的 30% 判 Lost，并按 1/2/4/8/24 h 退避（不占普通 15 分钟起的 Lose 退避）。
- **跨重连避开：** 探测判坏的宽度在 agent 进程内按网络 id 记住（最多 32 条，信号差
  >10 dB 不沿用）；新关联直接避开，起点宽度正在退避时从更窄一档开始。
- **试探中撤退：** 候选上连续两个样本交付 <1 Mbit/s（基线 ≥2 Mbit/s），或强信号
  （≥ −55 dBm）有业务却接收 MCS ≤1，就再探测一次；探测不可用时交付塌陷直接判
  Unknown 回滚。探测占用的时间不算“流量停止”。
- **数据路径确认：** 按历史收窄后、每次 60 s 刷新后、每次回滚后立即 ping 网关一个回包
  （`ping -c 1 -i 0.2 -w 2`，正常约 10 ms，不通 2 s）。本关联回过 ICMP 的网关不通时经
  协商上限绕一次（40→1 s→20），最多两次，仍不通显示 `Data path unconfirmed` 并停止挑战；
  从没回过的网关先绕一次，仍不回就当它拦 ICMP，本关联不再检查也不阻止挑战。
- **刷新：** 保持窄档时空闲也每 60 s 重设，仍经协商上限绕回（唯一实测有效的路径）。
- **负载保护：** 相邻两窗 R/E（本 STA 接收空口占比）都 <0.25 且 R 相差超过 2 倍判
  Unknown；需求下降导致的 E 减半不算锚点塌陷。用满空口时 R 的差距是真实容量，照常比较。

### 真机结果

| 轮次 | 场景 | 结果 |
|---|---|---|
| w7 | `W` Auto 挑战 40（当时用 ARP 确认） | 候选探测 0 回包，约 5.6 s 回滚；回滚后网关 ping 不通约 60 s，ARP 照常有回应，直到下一次刷新才恢复 |
| w8 | `W` Auto 挑战 40 | 2 s 洪泛在 40 上通过（丢 2%、回包 61%），真实 TCP 仍停；约 8.7 s 回滚，低速约 8 s（以前 27–46 s） |
| w9 1–4 | `W` 每轮重连后挑战 40 | 候选探测 4/4 判坏（丢 21–73%），读回确认的那个 tick 就回滚，低速 4–6 s，回滚后确认通过 |
| w9 5 | 重连后的新关联 | 不再挑战 40，60 s 全程 20 Mbit/s，最差 10 s 窗 19.8（首次达到 ≥15 门槛） |
| 注入 | 临时丢弃网关 ICMP 回包 | 刷新后的检查不通→绕两次→`Data path unconfirmed`，约 20 s，无卡死；规则删除后 `INPUT` 链与测试前一致 |
| w10 | `W_Game` 5 GHz 挑战 80 | 候选探测通过（不误判），但被误判“traffic stopped”（见下文问题 2） |
| w11 | `W_Game` 挑战 80、40 | 80 正常 Lost；随后 40 凭低负载基线误判 Won 并写入历史（问题 3） |
| w12 | `W_Game`，修复后 | 从 160 起，挑战 80 探测通过、E 486.7 对 637.5 正常 Lost，回滚后无多余挑战，约 50 Mbit/s 平稳 |

### 发现并修正的问题

1. **ARP 发现不了“读回正确但数据不通”。** w7 中网关 ping 100% 丢失时 `arping` 照常有回应
   （AP 很可能用基本速率单独发 ARP，普通数据帧却按宽档发）。确认改用 ICMP。
2. **探测时间被当成流量停止。** 两次探测约占 4 s 且清空计数端点，挑战开始时的计时攒够
   6 s 就判 Unknown。现在每次探测完成都重置该计时；回归测试在撤掉修复时失败、恢复后通过。
3. **负载变化导致 E 误判。** w11 的锚点窗只有 R 3.00（E 157.6），候选窗赶上测速
   R 22.35（E 210.0），40 MHz 判 Won 并写入 `widths.json`。加入上面的负载保护后，这组数
   判 Unknown，同一关联里 R 51 对 26 的正常 Lost 不变，阶段 B 满载 20 对 40 仍判 Won。
   `W_Game` 的历史条目由 40 手工改回 w10 测得的 160（改前文件备份在私密目录）。
4. **5 GHz 刷新经窄档绕回无法验证。** Mac 只在 2.4 GHz，看不出 80 与 160 的吞吐差，
   低负载时 E 也分不开，故撤回该改动，保留实测有效的经协商上限绕回。

### 软件验证与体积

- 宽度相关测试 **72 个**（新增 20 个）；4 线程全量 **1219 通过、3 忽略**，启动集成
  **1 通过**。默认并发全量曾有 QR/eSIM 解码 2 个间歇失败，单独重跑通过，与本轮无关。
- Release SHA-256 `22aa4b46…`（最后一次只把内部表改成具名结构体，体积不变）；`llvm-size -A` **Total 8,597,340 B**、`gzip -n -9`
  **4,394,592 B**。对比改动前设备程序 `7e93b1da…`（8,584,624 / 4,388,195 B）分别增加
  **12,716 B（0.15%）/ 6,397 B（0.15%）**。

### 最终状态（本节）

- agent `22aa4b46…`，默认 Auto；launcher `860e47ea…`、rc.local `09e3cef3…` 与测试前一致；
  中继在 `W` 1/20，网关可达；`widths.json`：`W` 20、`W_Game` 160。
- 回滚材料：改动前程序在私密目录 `device-backups/20261029-width-v2/zte-agent.7e93b1da.before-probe`；
  设备安装目录 `/data/zte-agent-install-20261029-174358-90079` 至 `…-185942-12344`。
- 电量低时 USB 两次断开约 3 分钟（内核 `zte_smblib_charger_rerun_apsd`，agent 无相关
  代码）；之后电量 6%→14%，USB 稳定。
- **仍未验证：** ICMP 确认救回真实故障（只做了注入）、11 分钟以上 Auto 长测、弱信号、
  热点客户端在用时的影响、Mac 在 5 GHz 时的满载对照。私密证据：`local-reapply-Vzpm6O/`
  下 w2–w12、`cycles.py`、`inject.py`、`probe2.py`。未提交。

### 5 GHz 满载验证（同日晚，Mac 连 `W_Game`）

Mac 与 U60 同在 `W_Game`（44/160，−48…−50 dBm），4 流 reverse iperf3，结果是同信道两跳吞吐。
前两部分临时以 Observe 运行（只经环境变量，不改 launcher），第三部分恢复 Auto。

- **固定宽度（各 30 s）：** 160 为 383 / 434 Mbit/s（前后两次，E 约 1080 / 1225），80 为 310
  （E 约 600–670），40 为 197（E 约 328）。E 排序与吞吐一致；E 夸大差距（160 对 80 约 1.7–2 倍，
  吞吐高 24–40%），在 15% 门槛下结论不变。
- **压到 80 不刷新（300 s）：** 前 180 s 为 275–327 Mbit/s，**第 190 s 起塌到 42–51** 并持续到
  结束；`iw` 读回与站点速率全程显示 80 MHz。阶段 B 见到的是退回宽档，这次是塌陷，两种都会发生。
- **压到 80、每 60 s 经 160 刷新（420 s）：** 全程 247–338，均值 312，与固定 80 相同，没有塌陷；
  6 次刷新各短暂经过 160。5 GHz 上的维护刷新由此有了直接证据。
- **Auto 满载 180 s：** 锚点 160（E 1117.65、R 360，空口占比 32%，负载保护不介入），满载下参照
  探测仍可用（发 388 收 384），80 候选探测通过；E 615 对 1118 判 Lost 回滚。真实吞吐：80 上
  310–330，回到 160 后 412–495。该试探在 80 上约 28 s。
- 结束时恢复 Auto，中继回到 `W` 1/20；电量 26%→32%，USB 稳定；无残留测速进程或转发。
  证据：`local-reapply-Vzpm6O/` 下 w13–w15、`fixed5g.py`、`hold5g.py`、`run4.py`。

## 平稳门槛、审查修正部署与驱动限制调查（2026-10-29 20:40–21:40）

所有者要求加上“流量平稳才挑战”，并尝试补齐驱动短板。期间另一个审查会话修正了恢复路径的两个
问题（见 `network-extensions.md`“审查修正”）；本轮在其之上实现平稳门槛，合并构建为 `49c2dc21…` 部署。

### LL stats 调查（Observe，`W_Game` 5 GHz，Mac 同信道）

- 完整 `ll_stats` 含接口级 AC 计数、按对端按速率统计、按（宽度, 中心频点）分的射频信道时间。
  按速率统计**只有发送有数**（含宽度、MCS、重传），接收全为 0。
- 压到 80 不刷新、满载 300 s，每 10 s 取一次：前 171 s 约 300–350 Mbit/s；约 180–190 s 起约 50。
  尽力而为类别的接收 MPDU 每 10 s 从 26–31 万降到 4.1–4.8 万；射频信道记录始终是 80 MHz；
  本机接收空口 5.2–6.0 s → 2.0–2.1 s，信道忙碌保持 8.0–9.0 s；接收速率 HE-MCS 6 → 3，标记仍为 80MHz。
- 结论：读不出 AP 实际发送宽度；失效有“信道很忙、本机解得出的接收骤降”的被动特征；暂不实现。
  原始数据（含 MAC/SSID）只存私密目录 `local-reapply-Vzpm6O/llstats-raw/`。

### 自编 `wpa_supplicant` 实验（`W` 2.4 GHz）

- 在仓库外用 zig 交叉编译 `wpa_supplicant` 2.11（官方 tarball，SHA-256 `912ea06f…`）与 OpenWrt
  `libnl-tiny`：静态 aarch64 musl，剥离后 910 KB（`631b0229…`），含 HT/VHT/HE 覆盖。
- 停掉 agent 后保留其 `udhcpc`，只把系统 supplicant 换成测试版并在复制的配置里加 `disable_ht40=1`
  （配置含密码，只在设备上复制，未输出）。调试日志显示读到覆盖并下发 `htcaps 61`/掩码 `63`，
  但关联后读回与收发速率仍是 **40 MHz**：驱动忽略能力覆盖。
- 测试版没有厂商 `DRIVER` 私有命令；重启 agent 时它还占着控制接口，`SETROAMMODE` 被拒，前 3 次
  恢复失败（“Wi-Fi control refused the runtime change”），停掉测试进程后第 4 次恢复成功。
  VHT 覆盖未测（同一 connect 参数路径，且同样受 `DRIVER` 命令限制）。实验目录已删除。

### 平稳门槛与回归（Auto，`49c2dc21…`）

- `W`：每 5 s 中 2 s 下载的流量 spread 0.43，照常挑战 40：候选探测发 253 收 245（回包率为参照
  的 15%）判坏，回滚并 `avoid width=40 for 1 h`。随后平稳 20 Mbit/s 60 s 内不再挑战；其间一次
  例行刷新后**真实出现网关不通**，`gateway silent … reapply via 40` → `data path recovered width=20
  after 1 reapply`，该 60 s 平均 20.0 Mbit/s。此前该恢复只做过注入。
- `W_Game`：每 3 s 交替 200/5 Mbit/s 得 spread 0.33，挑战 80 正常判负（两窗 R 均约 97，E 507 对 913）；
  重连后每 6 s 交替得 spread **0.74**，`challenge deferred: traffic not steady`，72 s 内无挑战。
- 软件：宽度相关 **79 个**测试（本轮新增 3 个，撤掉门槛时其中 2 个失败）；4 线程全量 **1226 通过、
  3 忽略**，启动集成 1 通过；改动文件 clippy 无警告。Release `49c2dc21…`：Total **8,597,376 B**、
  `gzip -n -9` **4,394,774 B**，对比改动前 `7e93b1da…` 均多约 0.15%。

### 最终状态（本节）

- agent `49c2dc21…`，默认 Auto；launcher `860e47ea…`、rc.local `09e3cef3…` 未变；中继在 `W` 1/20，
  网关可达；`widths.json`：`W` 20、`W_Game` 160；无测试进程、转发或实验文件；电量 81%，USB 稳定。
- 安装目录新增 `/data/zte-agent-install-20261029-212649-23726`。未提交。
