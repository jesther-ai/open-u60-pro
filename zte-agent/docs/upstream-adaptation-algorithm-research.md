# Wi-Fi 上游 STA 带宽自适应：一手算法研究与适用边界

## 0. 结论先行

本研究只服务于 **U60 Pro 的 Wi-Fi 上游 STA 带宽约束自适应**，不提出热点 AP 的选信道、带宽、MCS 或重启操作。没有进行设备探测、实现修改或构建；外部检索仅使用通用算法、驱动、字段名称，没有发送仓库内容、SSID/BSSID 或设备输出。

最值得借鉴的是以下组合，而不是移植某个驱动的速率控制器：

1. **Intel iwl-mvm-rs 的“保留当前配置，单独试一个候选，证据够再晋升，否则返回旧配置”**，辅以有界历史与上下文变化后清空统计。[IWL-RS]
2. **Minstrel 的“有效交付与发送成本一起考虑、给探索单独预算、保留可靠退路、不能凭一个相邻候选失败排除其余候选”**。它的反馈是发送端逐速率 ACK/attempt，不能由本机 STA 的最后一帧 RX PHY rate 或 radio RX time 等价替代。[MIN-DOC][MIN-LEGACY][MIN-HT]
3. **ACS 的“先验证传感器、按真实占用而非邻居数量解释环境”**。但 hostapd survey ACS 是给定带宽的 AP 启动选信道，不是固定上游 AP 下的 STA 带宽搜索，也不直接最大化用户吞吐。[ACS-SRC]
4. **保守在线选择的“先保证基线与探索预算”思想**。借鉴预算纪律即可；被动业务流量、关联样本、带宽切换和远端 AP 收敛不满足论文默认模型，不能宣称继承最优性或置信保证。[CB]

**优先级最高的限制不是如何调 15% 阈值，而是动作与分数是否可解释。** 如果不能确认动作只作用于 STA，或没有可比较的新鲜观测，正确结果应当是“保留稳定档／本次未知”，不是用更多统计把未知包装成最佳带宽。

---

## 1. 研究范围、版本与本机约束

### 1.1 本地输入与证据等级

已阅读 [交接 README](../../review-handoff/README.md) 第 0、1.5、1.6、2.1、3、7 节，以及 [width.rs](../src/wifi_relay/width.rs)、[llstats.rs](../src/wifi_relay/llstats.rs)。本次读到的实现使用 radio `rx_time` 与本接口接收字节构造分数，`predict()` 使用扫描邻居与固定 RSSI 阈值；`Tuner` 有四个样本、15% 晋升门槛、从最宽开始复查等规则，`Controller` 有读回确认与试验超时。这里只把这些当作待评估设计，**不把源码注释中的原因归属当作已经证明的无线事实**。

下文区分三类证据：

- **公开源码／规范性字段定义**：对应列明版本，能证明该源码的行为及该接口的声明语义。
- **任务内设备信息**：来自交接材料或主代理于 2026-10-28 的只读检查汇报；本文未独立复测，不等同于公开资料结论。
- **工程建议／推导**：本文基于上述边界提出的方案，不是标准要求，也不是已经通过真机实验的参数。

### 1.2 不从 SDX75 名称推出 Wi-Fi 实现

Qualcomm 官方 X75 产品简报描述的是 **5G Modem-RF System**，包括蜂窝制式、载波聚合、上下行能力；它不是 U60 Pro 的 Wi-Fi 芯片、驱动分支或调宽接口规格。[X75] 因此不能据“SDX75”“qcacld”或一个 PCI ID 就断言某个具体无线芯片、Minstrel 可用、固件具备某种 RX 统计。本文不替主代理做这类硬件身份判断。

算法参考固定为 Linux **v6.12**，旧版 Minstrel 对照固定为 **v5.4**；hostapd 使用 AOSP **android-15.0.0_r1** 源码。QCA 参考分支固定到 qcacld `c679e05584c4ca427b4baf0a677f56bd42c3264a`、wlan-fw-api `fb62f0801f8655d4b0a66e6811902715c84c4f8c`。它们是公开厂商代码，不声称与设备的 `5.2.1.110D` 二进制完全一致。[MIN-HT][MIN-LEGACY][ACS-SRC][QCA-WEXT][QCA-WMI]

### 1.3 主代理汇报对算法的限制（不是本文探测结论）

任务内最新只读汇报显示：

| 本地观察 | 对研究方案的约束 |
|---|---|
| 当前设备为厂商 5.15 内核；STA 与一个热点存在同信道并发 | 不假设原生 mac80211 速率控制可切换；STA 命令名不等于 radio/AP 无副作用。 |
| `iw survey dump` 两次读取相隔约 70 秒，计数未变；噪声值一致 | 目前不能用这些快照作在线 ACS 占用增量。数值相同本身不能证明传感器永远无用；必须先确定更新条件。 |
| LL radio/channel 时长递增；接口 WMM AC 与 per-rate TX 非零，per-rate RX 全零 | 可研究 LL 环境统计及上传保护；不能构造 Minstrel 式下行逐速率成功概率。全零不能解释成下行全失败。 |
| `iw station dump` 的部分包/字节/重试字段与其他计数明显不一致 | 不能因标准字段存在，就认为驱动提供了有效实现；每个字段单独验证。 |
| `get_chwidth` 返回值与接口信息、RX/TX PHY 显示冲突 | 不把该 getter 当作独立的实时带宽确认。 |
| MIB／热点 LL 读取不可用，ethtool 不提供所需统计 | 方案不能依赖这些当前不存在的观测；也不能承诺能精准扣除热点流量。 |

这些是约束清单，不是在本文新增一轮设备验收。具体日志与受控验证由主代理持有。

---

## 2. 先定义控制边界和观测语义

### 2.1 AP 选信道、STA 带宽约束、发送端速率控制是三件事

- **AP 选信道**：hostapd ACS 从允许信道中选 BSS 的工作信道，并配置主／次／中心信道；搜索的宽度来自 AP 配置。[ACS-SRC]
- **STA 接收带宽约束**：在支持相应信令的实现中，接收端 operating-mode 改变可限制对端向它发送时的带宽。Linux `ieee80211_vht_handle_opmode()` 更新 `cur_max_bandwidth`，与能力和 BSS 宽度取约束后的结果，再通知 `rate_control_rate_update()`；这里不直接指定对端 MCS。[MAC-VHT]
- **发送端速率控制**：Minstrel、iwl-mvm-rs 用本发送端 ACK/Block ACK、尝试次数等选择 TX rate。STA→AP 上传与 AP→STA 下载是不同发送端的决策；本机驱动的 TX retries 不等于上游 AP 的下行重传率。[MIN-HT][IWL-RS][NL80211]

**可迁移的是控制结构，不是测量能力。** 外层调宽器只应改变已验证的 STA 约束，让远端 AP 自己选择 MCS/NSS/GI/聚合；不假设 AP 使用 Minstrel、不知道其收敛时间，更不能用固定两个 tick 证明已收敛。

### 2.2 QCA 公共代码还提示了两个动作陷阱

在固定的 QCA 参考版本中：

1. `hdd_we_set_ch_width()` 向 `adapter->vdev_id` 发 `WMI_VDEV_PARAM_CHWIDTH`，但随后还更新 SME 的 `channelBondingMode5GHz` 和 `channelBondingMode24GHz`；该函数只接受 20/40/80，默认分支报错，而非本地材料记载的四档。这说明 **分支差异实际存在**；不能拿公开函数名证明设备上所有档位都合法，也不能从 vdev 参数断言完全不触及共享配置。[QCA-WEXT]
2. `WE_GET_CHWIDTH` 调 `wma_cli_get_command()`；后者的对应分支只是 `ret = intr[vdev_id].config.chwidth`。setter 经 `wma_cli_set2_command()` 投递异步消息，配置缓存另行更新。因此在此公开实现中，getter 是 **host 配置缓存读取**，setter 返回也不是“上游 AP 已接收信令并以新宽度发送”的证明。[QCA-WEXT][QCA-WMA]

这提供了本机 getter 冲突的合理排查方向，**不是本机根因定论**。真正上线前仍需核验：本机命令的范围、宽度映射、异步完成语义、AP 不变、读回来源，以及 RX 实际观测与配置上限的区别。单帧 PHY 宽度可以小于配置上限，不能反过来用一次窄帧证明上限已变。[MAC-VHT][MIN-HT]

### 2.3 字段速查：不要混作用域、单位和方向

| 字段／来源 | 一手定义 | 可以支持什么；不能推出什么 |
|---|---|---|
| `NL80211_STA_INFO_RX_BITRATE` | “last unicast data frame rx rate” | 最后一帧诊断，不是区间平均，不包含不可解码帧的速率分布。[NL80211] |
| `NL80211_STA_INFO_RX_DURATION` | “aggregate PPDU duration for all frames received from the station”，微秒 | 明确的 peer 作用域，比 radio RX time 更接近目标；但字段定义本身仍不保证所有未能识别发送者的失败 PPDU 都被计入。[NL80211] |
| station `TX_RETRIES`、`TX_FAILED` | 向该 station 发出的 MPDU 重试／失败 | 在上游 STA 接口上是本机发送方向，不是 AP 下行失败。[NL80211] |
| survey `TIME_RX/TIME_TX` | radio 接收／发送时间，可是 on-channel 或 global | 不是 per-peer；`TIME_BUSY` 是主信道感知忙，包括活动或能量检测。另有单独 `TIME_BSS_RX`，也不能自动等同一个 peer。[NL80211] |
| QCA／HAL radio `rx_time` | “msecs the radio is in active receive”；32 位累计值 | 只保证声明的 radio active-RX 语义，未保证是上游 peer、所有失败帧或仅成功帧。[HAL-LL][QCA-WMI] |
| QCA channel `rx_time` | channel 记录中的 active-RX 毫秒累计值 | 收窄到 channel，不等于收窄到 STA／peer。debugfs 是否打印还受 `ll_stats_per_chan_rx_tx_time` 开关影响。[QCA-WMI][QCA-DEBUGFS] |
| QCA channel `cca_busy_time` | 明写 “Includes rx_time but not tx_time” | **该定义不含自身 TX**；不能照搬 ACS 的 busy−TX 公式，更不能将 CCA 与 RX time 当同一分母。[QCA-WMI] |
| Linux 接口 `rx_bytes/rx_packets` | 接口收到的 good bytes/packets，后续仍可能在主机丢弃 | 是接口交付统计，不是应用 goodput，更不直接给出无线失败原因。真实驱动 offload 路径仍须验证。[NET-STATS] |

累计字段需以同一计数 epoch 的差分使用；32 位值会回绕，重关联／重置也会打断历史。QCA peer 统计明确声明每次连接重新开始。不能把重置后的下跳当负流量，或在 radio 身份切换后继续拼接差分。[HAL-LL][QCA-WMI]

### 2.4 WMM AC、per-rate TX 和 pending 能补哪些信息

AOSP `wifi_wmm_ac_stat` 与 QCA `wmi_wmm_ac_stats` 的主要声明一致：[HAL-LL][QCA-WMI]

- `tx_mpdu`：**收到 ACK 的成功单播数据 MPDU**；不是所有发送尝试。
- `rx_mpdu`：收到的单播数据 MPDU，不是字节、airtime 或 AP 尝试次数。
- `mpdu_lost`：无 ACK 的数据包损失；`retries`、`retries_short/long` 是发送方向的重试相关计数。
- `contention_time_min/max/avg`：数据包竞争时间，单位 **微秒**；`contention_num_samples`：用于竞争统计的数据包数。它不是端到端 RTT、应用时延或 AP 下行队列时延。
- QCA 扩展 `tx_pending_msdu` 的原文仅为 **“number of pending pkts”**；debugfs 从每 AC 的结构直接打印。可作为待验证的队列积压指示，不应差分成发送量，也不能拿本机待发队列证明 AP 有待发下行数据。[QCA-WMI][QCA-DEBUGFS]
- per-peer `rate_stats[]` 中的 `tx_mpdu/retries/mpdu_lost` 是本机逐发送速率的统计；非零可帮助建立 **上传防退化**观察，不能凭此得到下行逐速率成功概率。[HAL-LL][QCA-WMI]

**分母不能猜。** AOSP 头文件中的 MPDU 例子写到，首次 16-MPDU 聚合发出、收到 5 个确认时，`tx_mpdu` 增 5、`retries` 增 16；剩余 MPDU 后续重发再逐个增加。单凭字段名，很容易误把该字段与 Minstrel `attempts` 或“额外重发次数”互换。先核验设备版本的首次尝试、最终失败、跨速率归账和重置方式，再讨论 `success/attempts`；未经验证不要用 `tx_mpdu + retries` 自动拼出成功率。[HAL-LL][MIN-HT]

**工程建议**：第一阶段可观察同一 STA 的成功发送进度、重试相关计数趋势、`mpdu_lost`、有效样本数和 pending，作为试验的上传伤害否决条件。竞争平均值必须确认是累计还是窗口统计；不能把两次 min/max 相减，累计平均也只有在窗口定义与计数一致时才可能还原区间均值。现有定义并未给出这些固件更新细节，因此不能声称已得到可靠的上传损失概率。[QCA-WMI][HAL-LL]

---

## 3. hostapd survey-based ACS：借传感器纪律，不借决策目标

### 3.1 实际算法与关键函数

以下以 [固定版本 `src/ap/acs.c`][ACS-SRC] 的执行代码为准：

- `acs_survey_is_sufficient()`：要求 noise floor、channel time，以及 busy 或 RX time 至少一项；TX time 是可选项。
- `acs_survey_interference_factor()`：有 busy 则用 busy，缺 busy 才退到 RX；若存在 TX，则从分子和 total 各减 TX。
- `acs_survey_chan_interference_factor()`：对有效 survey 的 interference factor 取平均。
- `acs_find_ideal_chan()`：从配置确定 `n_chans`，例如 80 MHz 为 4、160 MHz 为 8，再调用 `acs_find_ideal_chan_mode()` 选信道，而非枚举不同宽度做用户吞吐竞赛。

令 `N` 为当前 survey 噪声，`Nmin` 为最低噪声，`B` 为 busy（缺失时 RX），`T` 为 channel time；若有 TX 计数 `X`，先令 `B←B−X, T←T−X`。执行表达式为：

```text
I = 10^(N/5) + (T != 0 ? B/T : 0) × 2^(10^(N/10) − 10^(Nmin/10))
```

**溯源注意**：Linux Wireless ACS 说明页及源码顶部说明里的指数内是加号，固定版本实际函数用减号。不要把说明页公式当已核对的执行行为；这也是经验性干扰指标，源码自身还有 noise multiplier 的 TODO，不是吞吐定理。[ACS-DOC][ACS-SRC]

### 3.2 多信道处理并非“宽度越小分数越低”的通用比较

`acs_find_ideal_chan_mode()` 检查合法主／次信道及 DFS 等限制，合并候选块内 survey；该版本最终除以 `total_weight`。2.4 GHz 又加入 ±5 MHz、±10 MHz 邻近信道权重，默认还有 1/6/11 偏置。因此不能只引用旧概述中的“相加”，就把 20/40/80/160 的裸分数横向比较。[ACS-SRC]

ACS 多轮扫描再分析，或委托驱动 ACS offload；这是 AP 设置流程。对已连接的 STA 原样增加离信道扫描，既改变任务边界，也引入探索成本，不能称为无额外代价。[ACS-SRC]

### 3.3 可借鉴与不适用

**可借鉴**：有效字段检查、单位／分母一致、检测陈旧快照、保留信道几何、用占用证据解释环境。扫描 BSS 位置和强度最多是候选顺序的弱先验；beacon 存在与业务占用不是同一测量，未扫到邻居也不证明空闲。[ACS-SRC][NL80211]

**不适用**：不能搬来改热点主信道；不能把 interference factor 当 STA 最优宽度；不能把 QCA LL CCA 与 nl80211 survey 无条件同义。尤其 QCA 参考 WMI 声明 CCA 不含 TX，若再次减 TX，可能得到失真的甚至负的负载指标。[QCA-WMI][ACS-SRC]

对本机的直接结论是：主代理看到的静止 survey 暂时不能支撑在线占用比较；可继续评估递增的 LL channel 数据，但其作用域和语义必须另建映射，不能只换字段名。

---

## 4. Minstrel／Minstrel-HT：吞吐估计、探索成本与“置信度”

### 4.1 优化的是期望交付／时间，不是 PHY rate 最大

经典 Minstrel 的文档用成功概率、包长度和发送时长估计 throughput。其关键反馈是某个发送 rate 的成功／尝试，不是只看 RSSI 推一个 MCS。[MIN-DOC]

Linux v6.12 `minstrel_ht_get_tp_avg()` 加入平均 A-MPDU 长度，将每包开销摊薄。忽略定点单位后的结构是：

```text
T̂(rate) ∝ p_filtered(rate)
           / [rate_duration + overhead / average_ampdu_length]
```

源码中概率低于 10% 时 throughput 记零；用于排序的概率截到 90%，注释说明是抑制碰撞相关误包波动的影响。这两个数字是排序启发式，**绝不是 90% 统计置信水平，也不是碰撞原因已经被识别**。[MIN-HT]

`minstrel_ht_tx_status()` 对 A-MPDU 使用 `ampdu_len`、`ampdu_ack_len`，各尝试 rate 增加 `try_count × ampdu_len`，成功计入最后有效 rate；没有有效聚合反馈时直接跳过。这个反馈链解释了它为什么能做本地 rate 排序，也解释了我们为什么不能用 `STA bytes / radio rx_time` 声称“实现了 Minstrel 同等估计”。[MIN-HT]

### 4.2 平滑与统计置信是不同概念

- v5.4 legacy `minstrel_calc_rate_stats()` 对 `success/attempts` 做 EWMA；没有尝试则不制造一次失败样本。[MIN-LEGACY]
- v6.12 HT 的 `minstrel_ht_calc_rate_stats()` 调 `minstrel_filter_avg_add()`，是带前两次输出的噪声滤波平均，不能将所有现代 Minstrel 概括为一个 EWMA。累计成功／尝试以及上个周期的计数仍保留。[MIN-HT][MIN-HT-H]
- 这些源码没有为“候选比当前快 15%”给出统计检验，也没有把四个窗口转成某个显著性水平。滤波减小抖动不等于排除了样本偏差、串行相关、远端负载漂移。[MIN-HT][CB]

**可借鉴**：保留分子、分母、样本量、观测时段、最后更新时间和数据来源，允许“不足以判断”；不要只存一个平滑后的漂亮数字。此为工程迁移，不是说 Minstrel 已实现本任务所需的置信比较。

### 4.3 探索有预算，而且慢探针更贵

历史 Minstrel 文档的默认 probing 占比是 10%，并讨论慢 rate 会占更多 airtime、将慢探针放在多速率重试链后面等方法；这是逐包 retry-chain 探索，不是每隔一段时间把整条链路切到差宽度几十秒。[MIN-DOC][MIN-LEGACY]

v6.12 HT 的实际机制不同：

- `MINSTREL_SAMPLE_INTERVAL = HZ / 50`，即约 20 ms 一次探测机会；`minstrel_ht_get_rate()` 在有发送包时决定是否探测，探测 rate 的 `count = 1`。
- 多速率硬件按 `INC, JUMP, INC, JUMP, INC, SLOW` 调度类别，并从可支持 rate 中抽样；不是每次线性降一档到失败就永久排除后续。
- 默认统计更新间隔为 `HZ / 20`；每个 MRR segment 还有 6000 µs 的重试时间预算。
- `minstrel_ht_tx_status()` 有快速退化防护：超过 30 次尝试且成功少于四分之一时降级候选。[MIN-HT][MIN-HT-H]

**可借鉴**：给探索的时间／受影响流量单独记账，差候选可快速放弃，仍让非相邻候选以后获得机会；稳定配置与探测配置分开。

**不能照搬**：20 ms、10%、30 次或 25% 成功率都依赖逐包发送反馈与廉价 retry-chain 回退。我们的切宽有配置确认、潜在信令与 AP 收敛成本；一次试宽影响期间全部业务，不可能等价为一个单包探针。[MIN-HT][QCA-WMA]

### 4.4 适用技术代际也有限

所读 v6.12 `minstrel_mcs_groups` 覆盖 legacy、HT 20/40、VHT 20/40/80；没有 HE rate group，亦不提供本任务需要的 HE 160 MHz 通用搜索模型。**不能据“Linux 有 Minstrel-HT”便建议将 qcacld/HE 链路切到它。** 借鉴目标函数和探索纪律即可。[MIN-HT][MIN-HT-H]

---

## 5. iwl-mvm-rs：最值得借的锚点／单挑战者结构

### 5.1 统计和搜索机制

固定 Linux v6.12 的 `_rs_collect_tx_data()` 保留每 rate 最近 **62 次发送尝试**，用位图淘汰旧记录；成功率是成功数／尝试数，throughput 为该成功率乘预计吞吐表。不是以接口字节直接测得的网速。[IWL-RS]

`constants.h` 在本版本定义至少 **3 次失败或 8 次成功**才允许使用吞吐估计；不是网上常见的“3/6”通用常数。`rs_rate_scale_perform()` 在不够历史时等待；聚合状态改变时更新预计吞吐表并清空旧窗口。[IWL-CONST][IWL-RS]

搜索同时包含当前 column 内的邻近 MCS 与不同 column（SISO/MIMO、GI、天线等）探索。最有用的结构是：

1. 保存当前 `last_tpt` 与 active table。
2. 为 search table 清空历史，只测这个候选。
3. 有足够样本后比较；`window->average_tpt > lq_sta->last_tpt` 才换 active table。
4. 否则恢复旧 table，结束本次 search；不会把刚测过的坏 table 当永久回滚点。[IWL-RS]

这支持“稳定锚点 + 单挑战者”的工程选择，也支持把切换瞬间／聚合变化前后的数据隔离。但是该比较本身仍是启发式，不提供置信区间，不能从其门槛推出本任务也只要 8 个 tick。[IWL-RS][IWL-CONST]

### 5.2 它不是通用四档带宽搜索的现成答案

`rs_bw_from_sta_bw()` 主要从 peer bandwidth 导出宽度，并处理 160 MHz 与 NSS 能力限制。`rs_tweak_rate_tbl()` 有特定 VHT SISO 路径：80 MHz MCS0 下调到 20 MHz MCS4，确认 20 MHz MCS5 等条件后再回 80 MHz；该路径还受开关控制。这不是遍历 20/40/80/160 的全局搜索。[IWL-RS]

此外 `iwl_mvm_rs_rate_init()` 明确按 `iwl_mvm_has_tlc_offload()` 选择固件 `rs-fw` 或 host 算法。不能将 host `rs.c` 描述成所有现代 Intel 网卡的实际决策器，更不能推断 Qualcomm 固件内部也相同。[IWL-RS][IWL-FW]

**可落地的部分**：稳定配置不被试验覆盖；候选单独收集证据；达到最小有效信息后再决定；模式变化使旧证据失效；失败快速回旧配置。**不可落地的部分**：硬件预计吞吐表、逐 rate ACK 窗口、Intel 专用带宽跳转条件。

---

## 6. 少量在线选择理论：保守预算，不堆算法

Wu 等人的原始论文 *Conservative Bandits*（ICML 2016）要求在整个学习过程保持相对默认 action 的累计收益约束，而不是最后总账达标。随机模型中使用类似：

```text
Σ(s≤t) μ[action_s] ≥ (1−α) × μ[baseline] × t， 对所有 t
```

算法先用置信下界判断探索是否会花穿预算，否则选默认 action。论文 §3 的基本随机设定是固定 arm 均值与独立 sub-Gaussian 噪声；Algorithm 1 与 Remark 1 明确考虑所有时刻的置信事件。[CB]

**本任务只借三点**：稳定档是默认动作；探索要有独立上限；证据不足时回默认动作。不能直接继承理论保证，原因包括：

- 带宽动作影响 AP 内部状态，切换有成本与延迟，不是即时、独立回报。
- TCP、业务结束、WAN/CPU 瓶颈以及共享空口造成相关与非平稳样本；没有同时观测的反事实基线。
- 接口吞吐或 radio-airtime 比值不一定是所要优化的用户收益，归一化分数也不能修复作用域错误。[CB][NET-STATS][QCA-WMI]

**工程建议**：先用“每次试验 wall-clock 截止 + 滚动周期试验时间/次数上限 + 每候选冷却”的可审计预算。若没有可信丢失字节估计，就诚实称为时间/次数预算，不宣称已保证吞吐损失不超过 α。若以后加入置信界，应以可比较的独立或有模型的窗口为单位，并处理反复查看数据的停止规则；普通固定样本区间在每 tick 都查看时，不自动仍有标称覆盖率。[CB]

---

## 7. 面向 U60 Pro 的最小可用设计建议

以下是研究导出的设计建议，**不是实现补丁或已经验证的默认参数**。

### 7.1 外层慢控制器与两个独立“门”

**动作门**先确认候选属于本关联合法范围、仅影响 STA、有可验证回退路径；否则不启动。**证据门**再判断窗口是否新鲜、计数作用域已知、同一来源可比较；否则不晋升。二者独立，不能让“分数很高”掩盖未确认动作，也不能让“命令成功”掩盖无观测。[QCA-WEXT][QCA-WMA][IWL-RS]

建议状态流：

```text
稳定 → 新鲜锚点窗口 → 单挑战者申请 → 确认/有界收敛等待
     → 候选有效窗口 → 晋升，或回滚，或未知
```

锚点是本轮开始前确认的稳定宽度；在试验中不可变。timeout、业务消失、统计重置、应用失败或上传保护触发，都不把候选覆盖成锚点。回滚本身也需要确认并有截止，不能无限重试。[IWL-RS][QCA-WMA]

### 7.2 最多四个候选，不假设单调，但不每次扫满

每次只挑战一个候选，优先相邻但保留所有合法宽度的后续机会；每候选维护自己的最近结果、证据时间、下次可试时间。失败冷却不阻塞其他候选，测量失败记“未知”而不是“性能输”。缓存只能作为新会话起点或排序先验，不能排除其他宽度。[MIN-HT][IWL-RS]

这是有成本的探索，不承诺始终最快或一定收敛到全局最优：若持续没有可比较业务与探索预算，保留稳定档就是合理行为。不要通过频繁从最宽开始复查来补偿信息不足。[CB]

### 7.3 选分数前，先承认可观测性上限

优先级建议：

1. **经本机验证的上游 peer 接收时间与同口径交付量**：可研究 link-delivery-per-observed-airtime；仍要说明失败 PPDU 计数边界，不能自动叫净空口效率。[NL80211]
2. **STA 接口接收字节／wall time**：可诚实称为接口观测速率。只有业务需求和其他瓶颈在比较窗口中足够可比时，才支持宽度优劣；低需求、WAN/CPU 限速时没有容量辨识力。[NET-STATS]
3. **STA 字节／radio 或 channel RX time**：至多是混合环境代理量；不能单独成为高置信晋升证据，不能凭它解释下行重传。[QCA-WMI]
4. **最后 RX PHY rate**：诊断和弱先验，不与上述分数无缝替换。最后一帧的值给整个 tick 按字节加权，也不会恢复没有观察到的逐包速率分布。[NL80211]

若只有第 3/4 类且混流不能界定，应降低自动决策权限，允许长期不选新档；不要把来源切换当普通连续样本。SSID/BSSID、信号强度、接口身份和缓存时间等本地上下文也不应出现在外部请求中。

### 7.4 窗口求和正确在哪里，不能修复什么

若分子分母作用域和窗口对齐，池化效率应为：

```text
E = 8 × Σ Δbytes / Σ Δairtime
```

而不是各 tick 比值的等权平均。它等价于以 airtime 加权每个 tick 的效率；用它是为了回答“整段有效观测总共交付多少/用了多少观测时间”。这是代数恒等式，**不表示分母已经是上游 airtime，也不表示得到容量或置信度**。

观测速率则使用 `8 × ΣΔbytes / ΣΔwall_time`。不要把 quiet、scan、切换、计数回绕/重置或来源变化前后的窗口随意拼起来；窗口要有累计信息量下限和最大年龄，而不仅是固定四个 tick。[HAL-LL][QCA-WMI][IWL-RS]

对 >=1 Mbit/s 的逐 tick 筛选也要谨慎：如果差候选导致低交付，恰好把这些低值全丢掉，会形成选择偏差。但在没有独立下行需求证据时，也不能把业务自然结束写成零分。建议结果分类为“有效比较／证据不足／硬失败”，证据不足先回锚点，避免重新启动无界自救；radio busy 或本机 `tx_pending_msdu` 都不是 AP 下行 backlog 的证明。[QCA-WMI][NET-STATS]

### 7.5 晋升、早停与防退化

- **晋升**：候选与新鲜锚点同来源、上下文可比、有足够有效信息、收益超过实用差值，且上传保护无明显恶化才允许。15% 可暂做迟滞工程参数，不应称为统计显著性。[IWL-RS][CB]
- **早停**：硬掉线、动作失败、有效上传队列/交付保护触发可立即退。仅凭最初 1–2 个低样本，不应在 AP 尚未收敛时给候选永久定性；安全回退可以保守，性能结论必须承认未知。[MIN-HT][IWL-RS]
- **漂移**：每次比较先刷新锚点；必要时用 A→B→A 复查环境，若 A 的前后表现也大幅变化，就判未知。该方法减少时间漂移混淆，但不是随机对照的因果证明，且多一次切换也要收费。[CB]
- **成本**：收敛等待、候选测量、回滚和失败重试全部计入试验时间；idle 不凭后台计时启动实验。不要移植 Minstrel 的 20 ms 节奏。[MIN-HT][QCA-WMA]
- **并发**：不能用“热点忙就全部丢样本”让中继场景永远不可学习；也不能只给混流打一个随意折扣，就宣称偏差被修正。无法区分作用域时，优先换观测／保留稳定档。[QCA-WMI]

---

## 8. 对交接第 7 节几项主张的独立评估

| 主张 | 研究判断 |
|---|---|
| 固定锚点、单挑战者、每候选退避 | 推荐；与 iwl active/search table、Minstrel 限制探索成本相容，但退避周期仍需本机定标。[IWL-RS][MIN-HT] |
| 不单调的带宽分数不应“输一档就停止所有后续搜索” | 推荐保留其他候选机会；没有一手来源给出固定环境下跨宽度性能必然单峰的保证。Minstrel 实际维护跨 group 探索，而非仅凭一个邻档作全域排除。[MIN-HT] |
| 比值改窗口求和 | 对池化比率是正确的；不解决 radio/peer 作用域错配、截尾低样本、漂移或吞吐因果解释。见 §7.4。[QCA-WMI] |
| 初始 1–2 个样本不到锚点一半就退 | 可作为保守的伤害止损候选，不能机械复用为性能定论；要先区分应用确认、收敛、有效业务与真实停滞。[MIN-HT][IWL-RS] |
| radio 混流“主要压小差距，不太会选错” | **没有通用保证，甚至排序可逆**。下面的反例只需最简单的分母污染，不依赖具体固件计数细节。 |
| `rx bitrate` 按时间/字节加权可作完整替代 | 不成立为完整测量；它仅是最后一帧，不代表该时间段全部成功帧，更不包含丢失帧。[NL80211] |
| 同 SSID 且 ≥−70 dBm 就优先 5 GHz | 最多是另一个选网策略的可测启发式，不属于本任务的宽度控制。公开 supplicant 的 BSS 排序综合安全性、调整后 SNR、预计吞吐和条件化 band 偏好，未提供 −70 dBm 的通用最优保证。[WPA-SCAN] |
| BSSID 尾部接近即可提示同一台路由器 | 本次一手资料没有提供这种身份保证；不能把数值相似当设备归属证明。本文不建议增加“同路由器”确定性提示，更不据此自动切网。[WPA-SCAN] |

### 混流可以翻转排序：仅作数学反例，不是设备数据

即使额外假设一个最简单的模型 `A_radio = A_peer + A_other`（真实固件是否满足还需验证），也不推出污染只会压缩差距。

在等长 1 秒窗口中，构造：

- 候选 A：交付 100 Mbit，peer RX 0.5 秒，其他 RX 0.4 秒。真实 peer 比率为 200 Mbit/空口秒，radio 代理为约 111。
- 候选 B：交付 90 Mbit，peer RX 0.6 秒，其他 RX 0 秒。真实 peer 比率为 150，radio 代理也为 150。

于是本来 A 优于 B，混合分数却选 B。该反例是直接代数推导，只用于否定“总会保守、不改变排序”；不声称本机观测恰好如此。就算额外流量在某次实验里主要来自 TCP ACK，也仍须证明其占用比例、上下文和两个候选之间关系，不可当通用不变量。[QCA-WMI][NET-STATS]

同理，一次重复切换后吞吐升高可以支持“这个场景值得进一步验证”，但没有独立的 peer 错误计数、相同业务与资源瓶颈控制，不能认定是次信道干扰、重传下降或 AP 固定算法缺陷的唯一原因。[MIN-HT][NL80211][QCA-WMI]

---

## 9. 交给设备验证与方案设计的最小清单

以下只列验收问题，本研究未执行任何设备操作：

1. **动作范围**：实际 ioctl 的 20/40/80/160 映射及非法值；是 RX operating-mode 约束、TX 限制、vdev 配置还是共享 PHY/SME 设置；热点配置、信道和在线客户端均不受变更影响。[QCA-WEXT][MAC-VHT]
2. **确认链**：区分请求已提交、host 缓存、实际配置、远端发送行为；确认失效时不能让调节器永远等待或错误收窄“协商上限”。[QCA-WMA]
3. **传感器契约**：每字段记录 scope、方向、单位、更新条件、回绕/重置、是否 offload 后漏记；既要检测不存在，也要检测“存在但恒零/陈旧”。[NL80211][HAL-LL][QCA-WMI]
4. **方向隔离**：WMM/per-rate TX 先作为本 STA 上传保护候选；下行 RX 统计缺失时不伪造下行成功概率，CCA 不当自身需求。[QCA-WMI]
5. **状态机对抗用例**：非单调四档、好锚点→坏候选→业务结束、apply 成功但 readback 陈旧、回滚失败、跨宽度累计窗口、计数重置、候选冷却互不阻塞、只有混流而无自身业务、环境漂移导致锚点也变差。源于上述动作与观测边界，而非要求增加新设备探测。
6. **对用户的承诺**：称“上游带宽自适应／按当前可观测业务择优”，不称“始终最快”“已准确测得下行重传”或“保证不影响热点”，后者必须有本机动作隔离证据才可承诺。

---

## Sources：可点击的一手资料与定位

- **[ACS-SRC]** [hostapd `src/ap/acs.c`，AOSP android-15.0.0_r1][ACS-SRC]：`acs_survey_interference_factor`、`acs_survey_is_sufficient`、`acs_survey_chan_interference_factor`、`acs_find_ideal_chan_mode`、`acs_find_ideal_chan`、`acs_scan_complete`、`acs_init`。主要代码定位约 355、455、772、1009 行。
- **[ACS-DOC]** [Linux Wireless：ACS 官方项目文档][ACS-DOC]。与执行公式有差异时，本文明确以所列源码版本为准。
- **[MIN-DOC]** [Linux Wireless：Minstrel 官方项目文档][MIN-DOC]。用于经典算法动机与探索成本，不当成现代默认参数说明。
- **[MIN-LEGACY]** [Linux v5.4 legacy Minstrel 源码][MIN-LEGACY]：`minstrel_calc_rate_stats`、`minstrel_get_rate`、`minstrel_update_stats`。
- **[MIN-HT] / [MIN-HT-H]** [Linux v6.12 Minstrel-HT 源码][MIN-HT]及[头文件][MIN-HT-H]：`minstrel_ht_get_tp_avg`、`minstrel_ht_calc_rate_stats`、`minstrel_filter_avg_add`、`minstrel_ht_tx_status`、`minstrel_ht_get_rate`、`minstrel_mcs_groups`；约 500、731、766、1282、1595 行。
- **[IWL-RS] / [IWL-CONST] / [IWL-FW]** [Linux v6.12 iwlwifi/mvm/rs.c][IWL-RS]、[constants.h][IWL-CONST]、[rs-fw.c][IWL-FW]：`_rs_collect_tx_data`、`rs_rate_scale_perform`、`rs_get_next_column`、`rs_tweak_rate_tbl`、`iwl_mvm_rs_rate_init`；主要定位 635、1544、2051、4110 行。
- **[NL80211]** [Linux v6.12 nl80211 UAPI][NL80211]：`enum nl80211_sta_info` 与 `enum nl80211_survey_info`，约 3823、4629 行。
- **[MAC-VHT]** [Linux v6.12 mac80211/vht.c][MAC-VHT]：`ieee80211_vht_handle_opmode`、`__ieee80211_vht_handle_opmode`、`_ieee80211_sta_cur_vht_bw`。仅用于公开 mac80211 的控制边界，不代表本机 QCA 固件路径。
- **[NET-STATS]** [Linux v6.12：Interface statistics 官方文档][NET-STATS]：`rx_packets`、`rx_bytes` 与 driver-defined statistics。
- **[HAL-LL]** [AOSP Android 13 Wi-Fi HAL link_layer_stats.h][HAL-LL]：`wifi_channel_stat`、`wifi_radio_stat`、MPDU accounting 示例、`wifi_rate_stat`、`wifi_wmm_ac_stat`、`wifi_iface_stat`。
- **[QCA-WMI]** [Qualcomm 公共 wlan-fw-api 固定 commit 的 wmi_unified.h][QCA-WMI]：`wmi_channel_stats`（约 8116 行）、`wmi_radio_link_stats`、`wmi_rate_stats`、`wmi_wmm_ac_stats`（约 8314 行）。此文件很大，可使用网页查找结构名。
- **[QCA-DEBUGFS]** [Qualcomm 公共 qcacld 固定 commit 的 debugfs LL 统计输出][QCA-DEBUGFS]：`hdd_debugfs_process_iface_stats`、`hdd_debugfs_process_peer_stats`、`hdd_debugfs_process_radio_stats`。
- **[QCA-WEXT]** [同 commit 的 wlan_hdd_wext.c][QCA-WEXT]：`hdd_we_set_ch_width`（约 4305 行）、`WE_GET_CHWIDTH`（约 6349 行）。
- **[QCA-WMA]** [同 commit 的 wma_main.c][QCA-WMA]：`wma_cli_get_command`（约 340 行）、`wma_cli_set2_command`（约 576 行）、配置缓存更新（约 1488 行）。
- **[CB]** Wu, Shariff, Lattimore, Szepesvári，[*Conservative Bandits*, ICML 2016，PMLR 48:1254–1262][CB]；[出版方论文 PDF][CB-PDF]，§2.1、§3–3.3、Algorithm 1 与 Remark 1。本文已阅读原文相关页，不以二手摘要代替模型假设。
- **[WPA-SCAN]** [AOSP android-15.0.0_r1 wpa_supplicant/scan.c][WPA-SCAN]：`wpa_scan_result_compar`，BSS 排序语境。
- **[X75]** [Qualcomm Snapdragon X75 5G Modem-RF System 官方产品简报 Rev. C][X75]，第一页产品与规格定义。

[ACS-SRC]: https://android.googlesource.com/platform/external/wpa_supplicant_8/+/refs/tags/android-15.0.0_r1/src/ap/acs.c
[ACS-DOC]: https://wireless.docs.kernel.org/en/latest/en/users/documentation/acs.html
[MIN-DOC]: https://wireless.docs.kernel.org/en/latest/en/developers/documentation/mac80211/ratecontrol/minstrel.html
[MIN-LEGACY]: https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/refs/tags/v5.4/net/mac80211/rc80211_minstrel.c
[MIN-HT]: https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/refs/tags/v6.12/net/mac80211/rc80211_minstrel_ht.c
[MIN-HT-H]: https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/refs/tags/v6.12/net/mac80211/rc80211_minstrel_ht.h
[IWL-RS]: https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/refs/tags/v6.12/drivers/net/wireless/intel/iwlwifi/mvm/rs.c
[IWL-CONST]: https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/refs/tags/v6.12/drivers/net/wireless/intel/iwlwifi/mvm/constants.h
[IWL-FW]: https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/refs/tags/v6.12/drivers/net/wireless/intel/iwlwifi/mvm/rs-fw.c
[NL80211]: https://github.com/torvalds/linux/blob/v6.12/include/uapi/linux/nl80211.h
[MAC-VHT]: https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/refs/tags/v6.12/net/mac80211/vht.c
[NET-STATS]: https://docs.kernel.org/6.12/networking/statistics.html
[HAL-LL]: https://android.googlesource.com/platform/hardware/libhardware_legacy/+/refs/tags/android-13.0.0_r1/include/hardware_legacy/link_layer_stats.h
[QCA-WMI]: https://android.googlesource.com/kernel/msm-modules/wlan-fw-api/+/fb62f0801f8655d4b0a66e6811902715c84c4f8c/fw/wmi_unified.h
[QCA-DEBUGFS]: https://android.googlesource.com/kernel/msm-modules/qcacld/+/c679e05584c4ca427b4baf0a677f56bd42c3264a/core/hdd/src/wlan_hdd_debugfs_llstat.c
[QCA-WEXT]: https://android.googlesource.com/kernel/msm-modules/qcacld/+/c679e05584c4ca427b4baf0a677f56bd42c3264a/core/hdd/src/wlan_hdd_wext.c
[QCA-WMA]: https://android.googlesource.com/kernel/msm-modules/qcacld/+/c679e05584c4ca427b4baf0a677f56bd42c3264a/core/wma/src/wma_main.c
[CB]: https://proceedings.mlr.press/v48/wu16.html
[CB-PDF]: https://proceedings.mlr.press/v48/wu16.pdf
[WPA-SCAN]: https://android.googlesource.com/platform/external/wpa_supplicant_8/+/refs/tags/android-15.0.0_r1/wpa_supplicant/scan.c
[X75]: https://docs.qualcomm.com/bundle/publicresource/87-27161-1_REV_C_Snapdragon_X75_5G_Modem-RF_System_Product_Brief.pdf

研究日期：**2026-10-28**。
