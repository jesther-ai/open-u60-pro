import SwiftUI

struct SpeedTestView: View {
    /// Owned as `@State` so a re-evaluated `NavigationLink` destination cannot replace the view
    /// model (and lose the running test) while the screen is on screen.
    @State private var viewModel: SpeedTestViewModel

    init(viewModel: SpeedTestViewModel) {
        _viewModel = State(initialValue: viewModel)
    }

    var body: some View {
        List {
            if let msg = viewModel.message {
                Section {
                    Text(msg)
                        .font(.subheadline)
                        .foregroundStyle(viewModel.messageIsError ? .red : .green)
                }
            }

            Section("Server") {
                Picker("Server", selection: $viewModel.selectedServerId) {
                    Text("Select a server").tag(nil as Int?)
                    ForEach(viewModel.servers) { server in
                        Text("\(server.sponsor) - \(server.name), \(server.country)")
                            .tag(server.id as Int?)
                    }
                }
                .disabled(viewModel.isRunning)
            }

            Section("Controls") {
                if viewModel.isRunning {
                    HStack {
                        Text(phaseLabel)
                        Spacer()
                        Text("\(viewModel.progress.progress)%")
                            .foregroundStyle(.secondary)
                            .contentTransition(.numericText())
                            .animation(.default, value: viewModel.progress.progress)
                    }
                    ProgressView(value: Double(viewModel.progress.progress), total: 100)
                        .accessibilityLabel(phaseLabel)
                        .accessibilityValue("\(viewModel.progress.progress) percent")

                    HStack {
                        Spacer()
                        if viewModel.progress.phase == "download" {
                            Image(systemName: "arrow.down")
                                .font(.title)
                                .foregroundStyle(.blue)
                        } else if viewModel.progress.phase == "upload" {
                            Image(systemName: "arrow.up")
                                .font(.title)
                                .foregroundStyle(.orange)
                        }
                        Text(String(format: "%.1f", viewModel.progress.liveSpeedMbps))
                            .font(.system(size: 48, weight: .bold, design: .rounded))
                            .contentTransition(.numericText())
                            .animation(.default, value: viewModel.progress.liveSpeedMbps)
                        Text("Mbps")
                            .font(.title3)
                            .foregroundStyle(.secondary)
                        Spacer()
                    }
                    .padding(.vertical, 8)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("Current speed")
                    .accessibilityValue(String(format: "%.1f megabits per second", viewModel.progress.liveSpeedMbps))

                    Button("Stop Test", role: .destructive) {
                        Task { await viewModel.stopTest() }
                    }
                } else {
                    Button("Start Speed Test") {
                        Task { await viewModel.startTest() }
                    }
                    .disabled(viewModel.isLoading || viewModel.selectedServerId == nil)
                }
            }

            if viewModel.progress.phase == "complete" {
                Section("Results") {
                    if let ping = viewModel.progress.pingMs {
                        LabeledContent("Ping", value: String(format: "%.1f ms", ping))
                    }
                    if let jitter = viewModel.progress.jitterMs {
                        LabeledContent("Jitter", value: String(format: "%.1f ms", jitter))
                    }
                    if let download = viewModel.progress.downloadMbps {
                        LabeledContent("Download", value: String(format: "%.2f Mbps", download))
                    }
                    if let upload = viewModel.progress.uploadMbps {
                        LabeledContent("Upload", value: String(format: "%.2f Mbps", upload))
                    }
                }

                Section("Transfer") {
                    LabeledContent("Downloaded", value: formatBytes(viewModel.progress.downloadBytes))
                    LabeledContent("Uploaded", value: formatBytes(viewModel.progress.uploadBytes))
                    if !viewModel.progress.server.isEmpty {
                        LabeledContent("Server", value: viewModel.progress.server)
                    }
                }
            }
        }
        .navigationTitle("Speed Test")
        .task {
            await viewModel.loadServers()
            await viewModel.resumeIfRunning()
        }
        .onDisappear { viewModel.stopPolling() }
        .overlay {
            if viewModel.isLoading && !viewModel.isRunning {
                ProgressView()
                    .padding()
                    .background(Color(.systemBackground).opacity(0.85), in: RoundedRectangle(cornerRadius: 8))
            }
        }
    }

    private var phaseLabel: String {
        switch viewModel.progress.phase {
        case "latency": return "Testing Latency..."
        case "download": return "Downloading..."
        case "upload": return "Uploading..."
        default: return viewModel.progress.phase.capitalized
        }
    }

    private func formatBytes(_ bytes: Int) -> String {
        if bytes < 1024 { return "\(bytes) B" }
        let kb = Double(bytes) / 1024
        if kb < 1024 { return String(format: "%.1f KB", kb) }
        let mb = kb / 1024
        if mb < 1024 { return String(format: "%.1f MB", mb) }
        let gb = mb / 1024
        return String(format: "%.2f GB", gb)
    }
}
