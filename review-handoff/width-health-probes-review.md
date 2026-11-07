# 代码审核说明：Wi‑Fi 中继上游带宽自适应（E 方案 + 健康探测）

日期：2026-10-29。本文件未提交，供审核 agent 使用。

改动以前的状态（设备程序 `7e93b1da…`）见 [width-v2-final-handoff.md](width-v2-final-handoff.md)
和 `zte-agent/docs/network-extensions.md` 的“上游带宽自适应 V2”节。本文件说明：算法主干、
为对抗 B31 驱动问题做的调整、本轮新增行为的精确规格、审核时要重点看的地方、测试与真机证据。
设计细节以 `network-extensions.md`“健康探测与数据路径确认”小节为准，真机过程以
`zte-agent/docs/2026-10-29-width-v2-acceptance.md` 末两节为准。

---

## 0. 审核范围

| 文件 | 内容 |
|---|---|
| `zte-agent/src/wifi_relay/width.rs` | 纯状态机（无设备 I/O）：评分、挑战、探测判定、数据路径确认、刷新、退避 |
| `zte-agent/src/wifi_relay/width_watch.rs` | 设备适配层：`Driver` trait、`ping`/`iwpriv`/`iw` 命令、跨重连记忆、`step()` |
| `zte-agent/src/wifi_relay/width/tests.rs` | 状态机测试（`Rig` 驱动） |
| `zte-agent/src/wifi_relay.rs` | 仅一处：relay-watch 循环把租约网关传给 `tune_width(w, gateway)`（约 218 行） |
| 文档 | `network-extensions.md`、验收记录、`CLAUDE.md` 设备注意事项两条 |

`zte-agent/src/wifi_relay/` 整个目录在 git 里是未跟踪文件，**没有 diff 基线**；下文第 4 节逐项列出相对
`7e93b1da…` 的新增和修改。

---

## 1. 做什么，不做什么

- **只调当前上游的带宽**（20/40/80/160 MHz），不选信道、不选频段、不换上游：信道由上游 AP 定；
  U60 上游与热点共用射频，热点跟随上游信道。
- **只能在协商上限内收窄。** 协商宽度 = min(AP 信标声明的工作宽度, STA 能力)，关联时确定。设备上的
  `wpa_supplicant`（v2.11-devel）没有编进 `disable_ht40`/`disable_vht` 等覆盖项，无法在关联时谈窄，只能
  关联后用 `iwpriv u60sta0 chwidth 0/1/2/3` 压窄。2.4 GHz 候选最多 40。
- **模式：** `ZTE_AGENT_WIDTH=auto`（默认）/ `observe`（只测量、记录“会挑战哪一档”，不写宽度、不探测、
  不写历史）/ `off`（不启用 LL stats）。三种模式在新关联时都做连接复位（第 3 节第 3 条）。

---

## 2. 算法主干（V2 既有，本轮少量修改已标注）

### 2.1 时序

1. `wifi_relay.rs::join()`：关联 `COMPLETED` → `reset_width()`（约 532 行，DHCP 之前）→
   `LinkWidth::new()` → DHCP → 路由切换。
2. `relay-watch` 线程约每 3 s 一次 `tune_width(link, gateway)` → `width_watch::step()`；持有 network 锁与
   inner（Session）锁。
3. `step()`：读 `/sys/class/net/u60sta0/statistics/rx_bytes` 判断需求（窗口 ≥1 Mbit/s，或已在收集/试探中
   且有字节）。无需求、不在试探、**且不需要刷新（新）**时不查驱动；否则读回 `iw dev u60sta0 info`，有需求时
   再读 LL stats（接收空口）、`iw station dump`（信号、PHY、**MCS（新）**）、TX 与热点 RX 计数，生成 `Sample`。

### 2.2 起点

`LinkWidth::initialize()`：可用历史（`widths.json`，同网络 id、信号差 ≤10 dB、可信时钟下 ≤30 天）→
否则 `predict()`（扫描邻居 ≥ −82 dBm 且与跨度重叠就减半）→ 否则协商宽度。**新：**起点宽度在跨重连
记忆里仍在退避时，逐档改用更窄的起点。起点窄于协商宽度时，作为无基线的“初始提议”施加，需读回确认、
热点检查，**新：之后立即确认数据路径**。

### 2.3 测量与评分

- 窗口：同 epoch，累计 ≥12 s 合格时间、≥100 ms 接收空口、R ≥1 Mbit/s，最长 30 s，6 s 无合格样本作废；
  原始计数求和，不平均比值。
- `E = 8·ΣΔrx_bytes / ΣΔradio_rx_time`（共享接收空口效率代理），`R = 8·ΣΔrx_bytes / 墙钟秒数`。
  **R/E 正好是本 STA 的接收空口占比。**
- `Score::compare()`：`suspicious()` 为真 → Unknown；否则 E_B ≥ 1.15·E_A → Won；否则 Lost（含平局）。
- `suspicious()`：B 的 R < A 的 80% 而 E 升高；热点 RX 相差 >2 倍且差 >5 Mbit/s；平均信号差 >6 dB；
  发生过扫描；**新：`load_changed()`——两窗 R/E 都 <0.25 且 R 相差 >2 倍。**

### 2.4 挑战调度

- 只有新鲜锚点窗口完成的那个 tick 能发起；每 15 min 最多 2 次，常规间隔 15 min；锚点 E 降到上一窗一半以下
  可提前（**新：`load_changed` 为真时不算塌陷**）。
- 候选排序：未测优先 → 与锚点档位距离近 → 较宽 → 结果较旧 → 历史提示。不从最宽档重扫。
- 退避：Lost 15/30/60/120/240 min；Unknown 5/10/20/40/60 min；操作失败至少 15 min；
  **新：探测判坏 1/2/4/8/24 h（独立级别 `broken_level`）。**

### 2.5 一次挑战的阶段（`Phase`）

```
Stable ─(锚点窗口完成)→ Checking(Before) 热点快照
  → Referencing（新：锚点洪泛参照）→ Applying（读回确认，6 s 重试 ≤3 次，18 s 限时）
  → Checking(Confirmed) 热点检查 →（新：候选洪泛探测）→ Settling(2 个有业务 tick)
  → Measuring（候选窗口）→ compare → Checking(End) 热点检查
  → Won：锚点 := 候选，写历史，Stable
  → 否则 RollingBack（读回确认）→ Checking(RolledBack) → Verifying（新：ping 网关）→ Stable / Paused
```

- 从发起起 45 s 总期限不续期；Measuring 中连续两个 tick E 低于基线一半且空口 ≥100 ms 提前认输；
  零字节样本、6 s 无流量 → Unknown；**新：交付塌陷/MCS 偏低先再探测（第 4.4 节）。**
- 热点保护：Before/Confirmed/End/RolledBack 四点读 hostapd 状态与热点宽度；受保护字段变化 → 回滚并在本次
  关联 `Paused: hotspot changed`；热点未就绪不发起、不花预算。刷新与数据路径确认的绕回**不做**热点检查
  （与 V2 的刷新一致）。
- 所有事件/动作带关联代次，旧代次丢弃。外部宽度变化、主信道/radio/协商上限变化作废窗口或候选历史。

### 2.6 历史

`widths.json`（`/data/zte-agent-relay/`，0600，最多 32 条）：首个合格锚点窗口或 Won 时写；Observe、未确认、
Paused 不写。`saved_utc` 只来自本次启动已验证的网络时钟。

---

## 3. B31 驱动/固件问题与对策

| # | 问题 | 证据 | 对策 | 位置 | 验证 |
|---|---|---|---|---|---|
| 1 | `iwpriv chwidth` 窄档约 100–260 s 后静默失效；`iw` 读回、站点速率仍显示窄档 | 2.4 GHz 20 塌到约 3 Mbit/s；5 GHz 80 有时回到宽档，有时塌陷（今晚 190 s 从 310 掉到 42–51） | 保持窄档时每 60 s 经协商上限重设（`via=negotiated`，间隔 1 s）；**新：空闲也刷新** | `Controller::refresh_due()`、`tick()` 刷新分支、`step()` 空闲分支 | 2.4 GHz 11 min 均值 19.98 Mbit/s；5 GHz 7 min 均值 312（对照组 190 s 塌陷） |
| 2 | 同值重发无效 | 阶段 B | 所有重设都“先设别的宽度再设回” | `Action::Reapply`、`reset()` | 同上 |
| 3 | 设置跨重连、接口重建、换频段保留 | 读回 160、AP 仍按 40 发 | 关联后、DHCP 前：窄一档 → 1 s → 协商宽度，读回确认；三种模式都做 | `reset_width()` | 方案 A 前后对照通过 |
| 4 | **新发现：** 改宽度后读回正确，但网关不通（普通数据帧收不到） | 连接时 40→20 后、一次回滚后，各持续约 60 s 直到下一次刷新；此时 `arping` 照常有回应 | 收窄、刷新、回滚读回后立即 ping 网关；不通经宽档绕一次，≤2 次 | `verify()`、`alive()`、`Device::alive()` | 故障时 ICMP 失败、绕一次恢复各见 2 次；注入验证过串联；部署 `49c2dc21…` 后一次例行刷新后真实再现，agent 绕一次即恢复，60 s 平均 20.0 Mbit/s |
| 5 | **新发现：** 部分 AP 的 2.4 GHz 40 MHz 负载下塌陷（接收 HE-MCS 0），信号 −35 dBm | `W`：20 MHz 97/24–34 Mbit/s，40 MHz 0–11.5 | 洪泛探测关卡、交付塌陷再探测、长退避、跨重连避开 | 第 4.1–4.4、4.8 节 | 4/4 拦下，停顿 4–6 s（原 27–46 s）；重连后不再挑战 |
| 6 | 驱动统计不可靠 | 站点 RX 字节恒 0、`rx packets` 恒 2；`rx bitrate` 是最近一帧且失效时仍显示窄档；`iw survey dump` 在用信道计数不增长 | 决策只用 `/sys` netdev 计数和 LL stats 空口；MCS 只用于触发探测 | `step()`、`measurement()` | `ll_stats` 按速率统计只有发送有数、接收全 0，读不出 AP 实际发送宽度；5 GHz 失效时信道记录不变、本机接收空口骤降而信道忙碌不变（可被动发现，未实现） |
| 7 | `wpa_supplicant` 无 HT/VHT 覆盖 | `strings` 查无 `disable_ht40` 等 | 不能关联时限宽，只能关联后收窄 | — | 自编 2.11 实测：下发清掉 HT40 位的 `htcaps`，qcacld 仍按 40 关联（驱动忽略覆盖）；上游版本还缺厂商 `DRIVER` 命令（`SETROAMMODE`） |
| 8 | LL stats 需 vendor 命令启用，刚重连时读取会失败 | 阶段 B | 首次有业务时启用；失败只报证据不足，不用 PHY 速率决策 | `llstats.rs`（既有） | 既有 |
| 9 | 5 GHz 160 MHz DFS 窗口内加入上游会卡住 AP（宽度逻辑之外） | 2026-10-28 | 加入前打开 DFS 切换开关 | `dfs_switch.rs`（既有） | 既有 |

---

## 4. 本轮新增/修改的精确规格

### 4.1 洪泛探测（`Probe`）

- 命令：`ping -q -f -I u60sta0 -l 8 -s 1400 -w 2 <网关>`，`net_tools::run` 限时 4 s（`PROBE_LIMIT`）。
  需要 root（agent 本就以 root 运行）。网关来自当前 DHCPv4 租约。
- 解析 iputils 摘要行（容许 `+N errors`）。**退出码非零（零回包）时读不到输出，返回 `Probe::default()`
  （0/0/0）。**
- `loss = max(0, sent − received − 8) / sent`（截止时在途的 `-l 8` 不算丢）；`rate = received/ms`。
- `usable()`：`sent ≥ 50 && loss < 5%`。`fails(ref)`：`loss ≥ 10% || rate < 0.3·ref.rate`。
- 只说明“这一跳通不通”：回包率受往返时延限制（约 1000 包/s），分不出健康宽度的快慢（5 GHz 160/80/40
  三档回包几乎相同）。

### 4.2 参照（`Phase::Referencing`）

普通挑战（有基线）在 `Check::Before` 通过后进入 Referencing 并发 `Probe(Reference)`；结果可用才存为
`trial.reference`；任一回包使 `icmp = Some(true)`；重置 `trial.last_traffic`；然后 `Apply(候选)`。
动作若被丢弃，下一 tick 视为无参照继续（`tick()` 中的 Referencing 分支）。初始提议（无基线）不做参照。

### 4.3 候选探测

`Check::Confirmed` 且有参照时发 `Probe(Candidate)`（`trial.probes = 1`）。
`fails` → `trial.broken = true` → `finish(Lost)` → `Check::End` 时 `record_broken()`（1/2/4/8/24 h），
并发 `Action::Avoid` 与日志 `avoid width=… for … h`；通过 → 清空窗口、重置 `last_traffic`。
探测回包不能进入任何窗口：watcher 在 `Probe` 时清空计数端点（`link.last = None`）。

### 4.4 试探中撤退

Settling/Measuring 中每个样本：
`slow = !busy && baseline.r ≥ 2.0`（有字节但 <1 Mbit/s）；`degraded = busy && signal ≥ −55 && rx_mcs ≤ 1`。
连续 2 个可疑样本 → 有参照且 `probes < 2` 时再探测一次；否则 `slow` 时 `finish(Unknown, "delivery collapsed")`；
只有 `degraded` 时不处理，交给 E。

### 4.5 数据路径确认（`Phase::Verifying`、`Action::Alive`）

- 触发：初始提议读回确认后、每次刷新后、回滚确认后，`verify()` 立即发 `Alive(0)`。
- 设备：`ping -q -c 1 -i 0.2 -w 2 -I u60sta0 <网关>`；正常约 10 ms 返回 0，2 s 内无回包返回 1。
  `Cannot run device tool` → `None`（无法检查）。**不用 ARP**（故障状态下网关照回 ARP）。
- `alive()`：
  - `None` → 结束确认。
  - `Some(true)` → `icmp = Some(true)`，结束；绕回过才记 `data path recovered …`。
  - `Some(false)` 且网关本关联没回过 ICMP，已绕过一次（或无法绕）→ `icmp = Some(false)`，本关联不再检查，
    **不阻止挑战**。
  - `Some(false)` 且已绕 2 次（或无法绕）→ `blocked = true`，状态 `Data path unconfirmed`，本关联不再发起挑战。
  - 其余 → `Reapply{锚点, via}`（锚点 < 协商宽度时 via = 协商宽度，否则更窄一档），下一 tick 再查。
- 每个 tick 最多执行一次单包网关确认。刷新失败的常见路径约为两次绕回间隔各 1 s + ping 2 s，
  但这不是持锁上界：两轮共四条宽度命令各限 1 s，ping 的外层限时为 4 s，动作部分名义合计可达 10 s。

### 4.6 刷新

`refresh_due()`：Auto、Stable、无试探、锚点 < 协商宽度、读回 = 锚点、距上次施加 ≥60 s。`step()` 的空闲分支
在它为真时不再跳过读回。仍经协商上限（经更窄一档的版本无法在 5 GHz 验证，已撤回）。刷新后清空窗口、
跳过下一合格样本，再确认数据路径。

### 4.7 负载保护

`load_changed(a, b) = a.r < 0.25·a.e && b.r < 0.25·b.e && max(a.r, b.r) > 2·min(a.r, b.r)`。
依据（均在测试中）：W_Game 误判 Won（A：R 3.00/E 157.6；B：R 22.35/E 210.0）现判 Unknown；同一关联
R 51 对 26 的正常 Lost 不变；阶段 B 满载 W 20 对 40（R/E ≈0.6，R 3.7 倍）仍判 Won。最初设想的“R 相差 >2 倍
就判 Unknown”会把满载下真实的大胜判成 Unknown，所以加了空口占比条件。

### 4.8 跨重连避开

- `width_watch.rs` 的进程级静态 `AVOIDED: Mutex<Vec<Avoided>>`：按（网络 id, 宽度）去重，最多 32 条，
  信号差 >10 dB 不沿用；**不持久化，agent 重启清空**。经 `Driver::avoided()/avoid()` 访问，测试用 Mock 自己的表。
- `initialize()`：起点处于退避中则改用更窄起点；对所有记录调用 `Controller::avoid()`（设 `next_ok`，级别延续，
  再判坏退避更久）。`reset_epoch(true)`（主信道/协商上限/radio 变化）会清空候选，包括这些记录。

### 4.9 平稳门槛（新）

`Window` 逐 tick 累计 R 与 R²，`Score.spread` 为变异系数；`steady()` = `spread ≤ 0.5`。锚点窗口不平稳时，
在预算与间隔检查之后、选候选之前返回，日志 `challenge deferred: traffic not steady`，不写预算、不改
`last_challenge`。`compare()` 中 E 达到 1.15 倍时还要求两窗都平稳，否则 Unknown；判负不受影响。
真机：每 6 s 交替 200/5 Mbit/s 得 0.74 并推迟；每 3 s 交替只得 0.33（3 s 采样平均掉）。

### 4.10 其它

- 探测完成（参照、候选通过）时重置 `last_traffic`：真机上两次探测约 4 s 且清空计数端点，原来会被
  6 s 规则误判 `traffic stopped`（W_Game 上一次健康的 80 MHz 试探因此中止）。
- 新日志：`probe reference …`、`probe passed/failed …`、`avoid width=… for … h`、`gateway silent …`、
  `gateway does not answer ping …`、`data path recovered …`。只含宽度与包数，不含 SSID/BSSID/地址。

---

## 5. 请重点审核

1. **持锁时长。** `tune_width` 在 network 锁与 inner 锁下运行；候选探测失败会在同一次 dispatch 串行执行
   Confirmed 热点检查 + 洪泛 + End 热点检查 + 回滚，动作部分名义限时合计约 11 s（3 + 4 + 3 + 1），
   还不含 channel/LL stats/station 等普通读数。状态 API 读独立 snapshot，不等待 inner；HTTP gate 的 7 s
   只约束等待执行槽位，不限制已开始的工作。当前 Mock 不模拟 I/O 耗时，真实持锁尾延迟仍待设备验证。
2. **阶段不会卡死。** Referencing、Verifying 在动作被丢弃时由 `tick()` 兜底；代次检查；Observe/Off 不写宽度、
   不探测、不 ping、不记避开（`dispatch()` 中 `mode != Auto` 跳过）。
3. **零回包与执行失败无法区分。** 洪泛零回包（退出码 1）与 `ping` 不存在都得到 `Probe::default()`：参照会判为
   不可用（无害），候选探测会判坏（若 `ping` 消失则每次挑战都失败）。是否值得区分？
4. **阈值基于很少的数据：** 探测 10%/30%/5%/50 包；`slow` 1 Mbit/s 与基线 2 Mbit/s；`degraded` −55 dBm/MCS ≤1；
   负载保护 0.25/2 倍。数据见第 7 节。
5. **刷新与确认绕回不做热点检查**，与 V2 刷新一致；但确认失败时最多多绕两次。
6. **`blocked` 的显示：** 之后锚点窗口完成会把状态改回 `Settled`，`blocked` 本身仍保留到关联结束。
7. **静态 `AVOIDED`：** 进程级共享，网络 id 是散列；跨关联沿用的信号来自关联时扫描（`LinkWidth.signal`）。
8. **探测流量：** 每次约 1500 包 ×1428 B 每方向（约 3 MB，只到网关）；每次普通挑战最多 3 次（参照、确认后、
   一次可疑）。会与用户流量共享空口约 2 s。
9. **参照新鲜度：** 候选探测与参照相隔约一个 tick（3–5 s），都在用户流量下测得。

---

## 6. 测试

- 宽度相关 **79 个**（原 52 个；本说明首版新增 20 个，第 10 节审查修正 4 个，平稳门槛 3 个）：
  `cargo test --locked -p zte-agent wifi_relay::width`。
- 4 线程全量：**1226 通过、3 忽略**，启动集成 1 通过。默认并发下 QR/eSIM 图片解码有已知间歇失败，与本轮无关。
- 改动文件 `cargo clippy --all-targets` 无警告。
- Release（`aarch64-unknown-linux-musl`）`49c2dc21…`（已部署）：`llvm-size -A` Total **8,597,376 B**、
  `gzip -n -9` **4,394,774 B**，对比改动前设备程序 `7e93b1da…`（8,584,624 / 4,388,195 B）均多约 0.15%。
- 平稳门槛 3 个：`spread_is_the_coefficient_of_variation_of_per_tick_rates`、
  `bursty_anchor_windows_defer_challenges_without_spending_budget`、`an_unsteady_candidate_window_cannot_win`
  （撤掉门槛时后两个失败）。

新增测试（`width/tests.rs` 16 个，`width_watch.rs` 4 个），按主题：

| 主题 | 测试 |
|---|---|
| 探测关卡 | `a_broken_candidate_fails_its_probe_at_confirmation_and_the_rollback_is_verified`、`a_lossy_reference_turns_probes_off_for_that_trial`、`probe_loss_ignores_packets_still_in_flight_and_iputils_output_parses` |
| 试探中撤退 | `slow_delivery_after_a_passing_probe_is_probed_again_and_fails_in_seconds`、`without_a_gateway_slow_delivery_still_rolls_back_well_before_the_deadline`、`a_low_mcs_at_strong_signal_triggers_one_more_probe_but_e_still_decides`、`time_spent_probing_is_not_mistaken_for_stopped_traffic` |
| 数据路径确认 | `a_silent_gateway_after_rollback_is_reapplied_twice_then_challenges_stop`、`a_silent_gateway_after_the_history_start_recovers_with_one_reapply`、`a_gateway_that_never_answers_ping_gets_one_reapply_then_no_more_checks`、`each_refresh_is_followed_by_one_gateway_check` |
| 刷新 | `a_5ghz_narrow_anchor_is_reapplied_through_the_ceiling_the_only_verified_path`、`a_narrow_anchor_is_refreshed_while_idle_without_other_driver_queries`（watch） |
| 退避与避开 | `a_probe_failure_backs_off_for_hours_and_is_reported_for_later_associations`、`an_avoided_width_is_not_challenged_before_its_time_and_keeps_its_level`、`a_broken_width_is_remembered_across_associations_and_lowers_the_start`（watch） |
| 负载保护 | `a_load_jump_between_idle_windows_is_unknown_but_saturated_gains_still_count`、`an_e_drop_from_lower_demand_does_not_count_as_a_collapse` |
| 接线 | `probes_need_a_gateway_break_the_counter_endpoint_and_only_run_in_auto`、`a_challenge_on_the_device_path_probes_reference_and_candidate_and_checks_the_gateway_after_rollback`（watch） |

`time_spent_probing_is_not_mistaken_for_stopped_traffic` 已确认：撤掉修复时失败，恢复后通过。

---

## 7. 真机证据摘要

设备：U60 Pro（B31）。上游 `W`（2.4 GHz 信道 1，HT20/HT40）与 `W_Game`（5 GHz 44/160）；Mac 作本地 iperf3
服务端（两者同 LAN，不走公网）。

| 场景 | 结果 |
|---|---|
| Observe：`W` 20 对 40 | 20 MHz 97（后续 24–34）Mbit/s；40 MHz 0–11.5，接收 HE-MCS 0 |
| 探测方式对比（坏的 40） | 小包 0/4、ping 序列 1/4、3 s 洪泛 4/4 |
| Auto：`W` 挑战 40（4 轮） | 候选探测丢 21–73% 判坏，读回确认的那个 tick 回滚，低速 4–6 s |
| Auto：洪泛通过但 TCP 停（1 轮） | 约 8.7 s 回滚（当时由 6 s 规则触发） |
| 重连后新关联 | 不再挑战 40；60 s 全程 20 Mbit/s，最差 10 s 窗 19.8 |
| 注入：丢弃网关 ICMP 回包 | 绕两次后 `Data path unconfirmed`，约 20 s，无卡死 |
| 5 GHz 中等负载（Mac 在 2.4 GHz） | 80、40 探测通过；修复前 40 凭低负载基线误判 Won 并写入历史（已手工改回 160） |
| 5 GHz 满载（Mac 在 5 GHz，两跳） | 固定 160/80/40：383–434/310/197 Mbit/s，E 排序一致；Auto 留在 160（80 Lost，E 615 对 1118） |
| 5 GHz 压 80 不刷新 / 每 60 s 刷新 | 190 s 塌到 42–51 / 7 min 保持约 312 |

---

## 8. 已知限制与未验证

- ICMP 确认救回真实“读回正确但不通”的故障：已见 1 次（例行刷新后），样本仍少。
- 平稳门槛只看 3 s 采样：短于约两个 tick 的起伏被平均掉；0.5 的阈值只有测速与人为交替流量的数据。
- 5 GHz 上窄档更快的场景（干扰、弱信号 < −70 dBm、DFS）未测；那里才是自动调宽度的主要价值所在。
- 新版本未做 11 分钟以上的 Auto 长测；未测热点客户端在用时的影响。
- 负载保护在拥挤信道里可能偏保守：别的网络占空口时，满载的 R/E 也可能 <0.25，真实胜负会判成 Unknown。
- 洪泛可能通过而真实 TCP 仍停（见过一次），靠 4.4 节和 6 s 规则兜底，停顿约 9 s。
- 跨重连避开不持久化；agent 重启后对已知坏的宽度仍会再挑战一次（约 4–6 s）。
- E 会夸大差距（5 GHz 160 对 80 约 1.7–2 倍，吞吐高 24–40%），不影响排序。

---

## 9. 设备当前状态与回滚

- agent `49c2dc21…`（含第 10 节修正与平稳门槛），默认 Auto；launcher `860e47ea…`、`/etc/rc.local` `09e3cef3…` 未改；中继在 `W`（1/20）。
- `widths.json`：`W` 20、`W_Game` 160。
- 回滚：改动前程序在仓库外 `device-backups/20261029-width-v2/zte-agent.7e93b1da.before-probe`；设备安装目录
  `/data/zte-agent-install-20261029-174358-90079` 至 `…-212649-23726`。临时切 Observe：用
  `ZTE_AGENT_WIDTH=observe sh /data/local/tmp/start_zte_agent.sh` 重启 agent（不改 launcher）。

---

## 10. 审查修正（2026-10-29，已随 `49c2dc21…` 部署）

- **P1 热点暂停丢失：** Verifying 中恢复绕回只完成第一条命令时，合法读回仍可被接受为锚点，
  但保留确认阶段和已用绕回次数，继续 ping；结束确认通过 `settle()` 保留本关联热点暂停，
  不再重新挑战或写历史。
- **P2 读回失败取消恢复：** Verifying 不再因 `channel=None` 直接结束。ICMP 与宽度读回独立，
  即使锚点等于协商上限、没有 60 s 刷新兜底，也继续有限确认；网关成功、持续失败、不可检查
  分别按原 `Alive` 规则收敛，不重置最多两次的绕回预算。
- 新增回归：状态机 `hotspot_pause_survives_a_partial_gateway_recovery_toggle`、
  `gateway_recovery_continues_without_width_readback_and_stays_bounded`；watcher 的对应
  `…_on_device_path` 两项。修复前 4 项全部失败，修复后中继相关 **161 项通过**。
- 4 线程全量测试合计 **1224 通过、3 忽略**（含启动集成）；all-targets clippy 改动文件无警告。
  ARM64 musl Release Total **8,597,348 B**（+8 B）、gzip **4,394,487 B**（-105 B），
  SHA-256 `0420626a4680a9eafc47b54f9c7652a5b13451611122bc54f78dda0e85b26166`。
- 当时只更新本地源码和文档；之后与平稳门槛一起构建为 `49c2dc21…` 部署（第 9 节）。
