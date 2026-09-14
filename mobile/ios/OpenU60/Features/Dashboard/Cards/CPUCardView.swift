import SwiftUI

struct CPUCardView: View, Equatable {
    let systemInfo: SystemInfo
    let thermal: ThermalStatus

    var body: some View {
        CardView {
            VStack(spacing: 8) {
                Image(systemName: "cpu")
                    .font(.title2)
                    .foregroundStyle(
                        systemInfo.cpuUsagePercent > 0
                            ? Color.cpuUsageColor(systemInfo.cpuUsagePercent)
                            : (thermal.cpuTemp > 70 ? .red : .orange)
                    )
                if systemInfo.cpuUsagePercent > 0 {
                    AnimatedNumber(value: systemInfo.cpuUsagePercent, decimalPlaces: 0,
                                   font: .title3.weight(.bold),
                                   textColor: Color.cpuUsageColor(systemInfo.cpuUsagePercent),
                                   suffix: "%")
                } else {
                    Text("--")
                        .font(.title3.monospacedDigit().bold())
                        .foregroundStyle(.secondary)
                }
                AnimatedNumber(value: thermal.cpuTemp, decimalPlaces: 0,
                               font: .caption, textColor: .secondary, suffix: "\u{00B0}C")
                Text("CPU")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity)
        }
    }
}

struct CPUDetailSheet: View {
    let systemInfo: SystemInfo
    let thermal: ThermalStatus
    let client: AgentClient

    @State private var showProcessList = false
    @State private var bloatSummary: String = ""

    var body: some View {
        NavigationStack {
            List {
                row("CPU Usage", icon: "cpu", value: systemInfo.cpuUsagePercent > 0 ? String(format: "%.0f%%", systemInfo.cpuUsagePercent) : "—")
                if systemInfo.cpuUsagePercent > 0 && systemInfo.cpuUsageIsEstimate {
                    row("Usage Source", icon: "info.circle", value: "Estimated")
                }
                row("CPU Cores", icon: "square.grid.2x2", value: "\(systemInfo.cpuCores)")
                row("Temperature", icon: "thermometer.medium", value: String(format: "%.1f \u{00B0}C", thermal.cpuTemp))
                row("Uptime", icon: "clock", value: formatUptime(systemInfo.uptime))
                row("Memory Total", icon: "memorychip", value: formatBytes(systemInfo.memTotal))
                row("Memory Free", icon: "memorychip", value: formatBytes(systemInfo.memFree))
                if systemInfo.memTotal > 0 {
                    let used = Double(systemInfo.memTotal - systemInfo.memFree) / Double(systemInfo.memTotal) * 100
                    row("Memory Used", icon: "chart.bar", value: String(format: "%.0f%%", used))
                }

                Button {
                    showProcessList = true
                } label: {
                    HStack {
                        Label("Processes", systemImage: "list.number")
                        Spacer()
                        if !bloatSummary.isEmpty {
                            Text(bloatSummary)
                                .foregroundStyle(.orange)
                                .font(.caption)
                        }
                        Image(systemName: "chevron.right")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .foregroundStyle(.primary)
            }
            .navigationTitle("CPU & Memory")
            .navigationBarTitleDisplayMode(.inline)
            .sheet(isPresented: $showProcessList) {
                ProcessListSheet(client: client)
            }
            .task {
                await loadBloatSummary()
            }
        }
    }

    private func loadBloatSummary() async {
        do {
            let result: ProcessListResponse = try await client.get("/api/system/top")
            if result.bloatCount > 0 {
                bloatSummary = "\(result.bloatCount) bloat \(String(format: "%.0f", result.bloatCpuPct))%"
            }
        } catch {
            // Silently ignore — summary is optional
        }
    }

    private func row(_ label: String, icon: String, value: String) -> some View {
        HStack {
            Label(label, systemImage: icon)
            Spacer()
            Text(value)
                .foregroundStyle(.secondary)
                .monospacedDigit()
        }
    }

    private func formatUptime(_ seconds: Int) -> String {
        let d = seconds / 86400
        let h = (seconds % 86400) / 3600
        let m = (seconds % 3600) / 60
        if d > 0 { return "\(d)d \(h)h \(m)m" }
        if h > 0 { return "\(h)h \(m)m" }
        return "\(m)m"
    }

    private func formatBytes(_ bytes: UInt64) -> String {
        if bytes >= 1_073_741_824 {
            return String(format: "%.1f GB", Double(bytes) / 1_073_741_824)
        } else if bytes >= 1_048_576 {
            return String(format: "%.0f MB", Double(bytes) / 1_048_576)
        }
        return "\(bytes) B"
    }
}
