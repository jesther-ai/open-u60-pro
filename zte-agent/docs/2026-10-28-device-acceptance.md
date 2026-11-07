# 2026-10-28 清理、安装与真机验收

设备：用户通过 USB ADB 授权的 B31 U60 Pro。用户明确选择本轮仅做软件重启，不宣称电源冷启动；同意电脑临时连接 U60，并配合关闭/恢复真实 5 GHz 上游。未改国家码、蜂窝开关或厂商启动同步服务。

## 清理

- 私密备份目录：`/Users/g/Desktop/Workspace/u60-pro/device-backups/20261028-review-acceptance/`。原程序/启动脚本/rc.local/恢复配置先备份到 `device-before.tar`。
- 设备未发现独立 QA/测试二进制；11 个旧安装备份目录、33 个文件逐个 SHA-256 校验归档后删除，释放 91,328 KiB。保留最近旧备份及本次新部署回滚目录。
- 仓库删除 6 个一次性真机/私人输入诊断入口、`examples/stk_probe.rs`、仅供其使用的测试辅助函数和废弃 `U60_PHYSICAL_QA` 开关。保留普通单元、启动认证、安装器、浏览器及隔离 OpenWrt/真实 core 回归。
- 原文件位于 `repo-test-cleanup/manifest.json` 所列私密备份；Python 测试缓存也归档后清理。

## 安装优化与部署

- `setup.sh` 使用 `scripts/install-agent.py` 统一生成 launcher/installer，共用身份检查和锁；只停止已验证的 main，不把 DHCP/CLI 同 executable 回调当主服务。
- TERM 前复查 starttime；只等待原进程，只清理已验证且仍是原 starttime 的 ubus listener。无 killall、无 SIGKILL 升级。
- 同 stage 重试保留原始回滚输入；失败保留上传材料。备份 0600；重复启动不产生第二个 main。
- 边界：POSIX shell 的最后一次身份检查到 kill 之间不是原子操作；双文件替换/启动失败不自动回滚，明确失败并保留备份。
- 26 项安装器测试通过；清理后 Rust 完整回归 1,142 通过、3 忽略，加 1 个启动集成通过。
- 真机 `setup.sh --build --adb --serial … --factory-password --no-ssh` 成功，备份目录 `/data/zte-agent-install-20261028-072441-88322`，开机钩子仅一条，重复 launcher 后 PID 不变。
- 安装时保持中继/Mihomo 开启，新 agent 清理两个旧辅助进程；5 GHz 第一次恢复成功，Mihomo 恢复。
- Release SHA-256：`24072cdc15e19d6404b2fa4640e7659952725f0379671b202488ff40209ef6ce`，安装后相同。`llvm-size -A` Total 8,533,816 B，`gzip -n -9` 4,358,909 B；无 GNU cross-size，使用 LLVM ELF 等价测量。

## 已验证

- 电脑真实连接 U60 热点，源地址 `192.168.0.44`：本地 token 请求 API 和 events 均 401。ADB loopback 同 token 请求 API 200、events 403；匿名 API 401。token 未输出或写入报告。
- 5 GHz 上游 STA：44 信道/160 MHz；驱动将 AP 切换到 44/80 MHz；Mihomo 运行。
- 更新前后，真实 LAN 客户端经中继/Mihomo 的 IPv4、IPv6 HTTPS 各取得 200。电脑自身代理的 fake-IP DNS 会干扰验收，因此绑定 `en0`、禁用应用代理并用 U60 DNS 结果显式解析；不把错误路径的失败当 U60 结果。

## 软件重启：发现失败，未通过

- 新 boot 已确认。Mihomo 自动恢复；5 GHz Relay 多次关联失败。不是密码猜测：驱动首次失败日志为 `wlan_hdd_cm_handle_sap_sta_dfs_conc: wait for qdf_event failed, STA not allowed!!`，随后 `sap-sta conc will fail` 和 `Channel switch in progress!!`。
- 该 boot 的热点从 44/160 MHz 启动。停止恢复后单次手动 5 GHz 连接仍失败。2.4 GHz 对照尝试又留下无线回滚错误，wlan0 为 DISABLED；已归档日志/无线journal后通过厂商单接口 hostapd 重启恢复热点，再通过正式 disconnect 清理 journal。
- 厂商热点重新从 36/160 MHz 启动、完成 DFS 后，单次 5 GHz 连接成功。证据指向 AP/STA 并发信道切换的启动状态差异，但尚未证明完整根因或实施自动修复。
- 没有把厂商热点重启加入 agent，没有更改 AP 持久带宽或国家码。一次成功恢复不能覆盖首次重启失败。
- 第一版临时观察脚本过早读到 reboot 命令后、真正重启前的旧状态；该条不作为通过证据。后续脚本必须观察 boot_id 改变。

## 尚在进行

真实 W_Game AP 已由用户关闭，扫描确认缺席；保留恢复意图后已软件重启。等待新 boot 的恢复状态，再验证取消和上游回来后的保持关闭行为。最终设备/电脑状态及结果在本节完成后补充。
