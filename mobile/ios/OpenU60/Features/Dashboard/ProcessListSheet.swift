import SwiftUI

struct ProcessListSheet: View {
    let client: AgentClient

    @State private var processes: [ProcessInfo] = []
    @State private var bloatCount = 0
    @State private var bloatCpuPct = 0.0
    @State private var bloatRssKb = 0
    @State private var isLoading = false
    @State private var error: String?
    @State private var banner: String?
    @State private var showKillAllConfirm = false
    @Environment(\.dismiss) private var dismiss
    @Environment(\.processKillRefresh) private var refreshHost

    var body: some View {
        NavigationStack {
            Group {
                if isLoading && processes.isEmpty {
                    ProgressView("Loading processes...")
                } else {
                    List {
                        if bloatCount > 0 {
                            Section {
                                HStack {
                                    Label("Bloat Daemons", systemImage: "exclamationmark.triangle")
                                        .foregroundStyle(.orange)
                                    Spacer()
                                    VStack(alignment: .trailing) {
                                        Text("\(bloatCount) processes")
                                            .font(.caption)
                                        Text(String(format: "%.1f%% CPU, %@ RSS", bloatCpuPct, formatKB(bloatRssKb)))
                                            .font(.caption2)
                                            .foregroundStyle(.secondary)
                                    }
                                }
                            }
                        }

                        if let error {
                            Section {
                                Text(error)
                                    .foregroundStyle(.red)
                                    .font(.caption)
                            }
                        }

                        if let banner {
                            Section {
                                Text(banner)
                                    .foregroundStyle(.green)
                                    .font(.caption)
                            }
                        }

                        Section(header: Text("Top Processes")) {
                            ForEach(processes) { proc in
                                processRow(proc)
                                    .swipeActions(edge: .trailing) {
                                        if proc.isBloat {
                                            Button(role: .destructive) {
                                                Task { await killSingle(proc.pid) }
                                            } label: {
                                                Label("Kill", systemImage: "xmark.circle")
                                            }
                                        }
                                    }
                            }
                        }
                    }
                }
            }
            .navigationTitle("Processes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Done") { dismiss() }
                }
                if bloatCount > 0 {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("Kill All Bloat", role: .destructive) {
                            showKillAllConfirm = true
                        }
                        .foregroundStyle(.red)
                    }
                }
            }
            .confirmationDialog("Kill all bloat daemons?", isPresented: $showKillAllConfirm, titleVisibility: .visible) {
                Button("Kill All Bloat", role: .destructive) {
                    Task { await killAll() }
                }
            } message: {
                Text("This will SIGKILL \(bloatCount) bloat daemons. They will return on reboot.")
            }
            .task {
                while !Task.isCancelled {
                    await refresh()
                    do {
                        try await Task.sleep(for: .seconds(3))
                    } catch {
                        break
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func processRow(_ proc: ProcessInfo) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(proc.name)
                    .font(.body)
                    .foregroundStyle(proc.isBloat ? .orange : .primary)
                Text("PID \(proc.pid) \u{00B7} \(proc.state)")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 2) {
                Text(String(format: "%.1f%%", proc.cpuPct))
                    .font(.body.monospacedDigit())
                    .foregroundStyle(proc.cpuPct > 10 ? .red : (proc.cpuPct > 2 ? .orange : .secondary))
                Text(formatKB(proc.rssKb))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func refresh() async {
        isLoading = true
        defer { isLoading = false }
        do {
            let result: ProcessListResponse = try await client.get("/api/system/top")
            processes = result.processes
            bloatCount = result.bloatCount
            bloatCpuPct = result.bloatCpuPct
            bloatRssKb = result.bloatRssKb
            error = nil
        } catch {
            guard !error.isCancellation else { return }
            self.error = error.localizedDescription
        }
    }

    private func killSingle(_ pid: Int) async {
        do {
            let body = ["pids": [pid]] as [String: Any]
            let data = try await client.postJSON("/api/system/kill-bloat", body: body)
            let freed = data["freed_rss_kb"] as? Int ?? 0
            banner = "Killed PID \(pid), freed \(formatKB(freed))"
            await refresh()
            await refreshHost?()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func killAll() async {
        do {
            let data = try await client.postJSON("/api/system/kill-bloat", body: ["all": true])
            let freed = data["freed_rss_kb"] as? Int ?? 0
            let killedArr = data["killed"] as? [[String: Any]] ?? []
            banner = "Killed \(killedArr.count) daemons, freed \(formatKB(freed))"
            await refresh()
            await refreshHost?()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func formatKB(_ kb: Int) -> String {
        if kb >= 1024 {
            return String(format: "%.1f MB", Double(kb) / 1024.0)
        }
        return "\(kb) KB"
    }
}

// MARK: - Host refresh

/// A refresh of the screen this sheet stack was presented over, injected by that screen.
/// Killing bloat frees ~225 MB in one go, but the values behind the sheet come from the
/// dashboard's slow poll tier, and neither pull-to-refresh nor a tab switch is reachable from
/// inside a sheet, so the freed memory would not show for up to a minute.
private struct ProcessKillRefreshKey: EnvironmentKey {
    static var defaultValue: (@MainActor () async -> Void)? { nil }
}

extension EnvironmentValues {
    var processKillRefresh: (@MainActor () async -> Void)? {
        get { self[ProcessKillRefreshKey.self] }
        set { self[ProcessKillRefreshKey.self] = newValue }
    }
}
