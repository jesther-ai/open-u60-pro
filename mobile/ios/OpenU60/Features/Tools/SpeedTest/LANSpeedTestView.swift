import SwiftUI

struct LANSpeedTestView: View {
    /// Owned as `@State` so a re-evaluated `NavigationLink` destination cannot replace the view
    /// model (and lose the running test) while the screen is on screen.
    @State private var viewModel: LANSpeedTestViewModel

    init(viewModel: LANSpeedTestViewModel) {
        _viewModel = State(initialValue: viewModel)
    }

    var body: some View {
        List {
            if let error = viewModel.error {
                Section {
                    Text(error)
                        .font(.subheadline)
                        .foregroundStyle(.red)
                }
            }

            Section("Controls") {
                if viewModel.isRunning {
                    HStack {
                        Text(phaseLabel)
                        Spacer()
                        Text("\(Int(viewModel.progress * 100))%")
                            .foregroundStyle(.secondary)
                            .contentTransition(.numericText())
                            .animation(.default, value: viewModel.progress)
                    }
                    ProgressView(value: viewModel.progress)
                        .accessibilityLabel(phaseLabel)
                        .accessibilityValue("\(Int(viewModel.progress * 100)) percent")

                    if viewModel.phase == "download" || viewModel.phase == "upload" {
                        HStack {
                            Spacer()
                            if viewModel.phase == "download" {
                                Image(systemName: "arrow.down")
                                    .font(.title)
                                    .foregroundStyle(.blue)
                            } else {
                                Image(systemName: "arrow.up")
                                    .font(.title)
                                    .foregroundStyle(.orange)
                            }
                            Text(String(format: "%.1f", viewModel.liveSpeedMbps))
                                .font(.system(size: 48, weight: .bold, design: .rounded))
                                .contentTransition(.numericText())
                                .animation(.default, value: viewModel.liveSpeedMbps)
                            Text("Mbps")
                                .font(.title3)
                                .foregroundStyle(.secondary)
                            Spacer()
                        }
                        .padding(.vertical, 8)
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel("Current speed")
                        .accessibilityValue(String(format: "%.1f megabits per second", viewModel.liveSpeedMbps))
                    }

                    Button("Stop Test", role: .destructive) {
                        viewModel.stopTest()
                    }
                } else {
                    Button("Start LAN Speed Test") {
                        viewModel.startTest()
                    }
                }
            }

            if viewModel.phase == "complete" {
                Section("Results") {
                    if let ping = viewModel.pingMs {
                        LabeledContent("Ping", value: String(format: "%.1f ms", ping))
                    }
                    if let download = viewModel.downloadMbps {
                        LabeledContent("Download", value: String(format: "%.1f Mbps", download))
                    }
                    if let upload = viewModel.uploadMbps {
                        LabeledContent("Upload", value: String(format: "%.1f Mbps", upload))
                    }
                }
            }

            Section {
                Text("Measures WiFi link speed between this device and the router. Does not use internet data.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        // No `onDisappear` teardown: a tab switch must not abort a transfer that is already
        // paid for. A real pop releases the model and its `deinit` cancels the run.
        .navigationTitle("LAN Speed Test")
    }

    private var phaseLabel: String {
        switch viewModel.phase {
        case "ping": return "Testing Latency..."
        case "download": return "Downloading..."
        case "upload": return "Uploading..."
        default: return viewModel.phase.capitalized
        }
    }
}
