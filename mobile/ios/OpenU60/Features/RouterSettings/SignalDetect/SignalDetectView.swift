import SwiftUI

struct SignalDetectView: View {
    /// Owned as `@State` so a re-evaluated `NavigationLink` destination cannot replace the view
    /// model (and lose the running sweep) while the screen is on screen.
    @State private var viewModel: SignalDetectViewModel

    init(viewModel: SignalDetectViewModel) {
        _viewModel = State(initialValue: viewModel)
    }

    var body: some View {
        List {
            if let msg = viewModel.message {
                Section {
                    Text(msg)
                        .font(.subheadline)
                        .foregroundStyle(viewModel.messageIsError ? .red : .green)
                        .textSelection(.enabled)
                }
            }

            Section("Controls") {
                if viewModel.status.running {
                    HStack {
                        Text("Progress")
                        Spacer()
                        Text("\(viewModel.status.progress)%")
                            .foregroundStyle(.secondary)
                    }
                    ProgressView(value: Double(viewModel.status.progress), total: 100)
                        .accessibilityLabel("Detection progress")
                        .accessibilityValue("\(viewModel.status.progress) percent")

                    Button("Stop Detection", role: .destructive) {
                        Task { await viewModel.stopDetection() }
                    }
                } else {
                    Button("Start Signal Detection") {
                        Task { await viewModel.startDetection() }
                    }
                    .disabled(viewModel.isLoading)
                }
            }

            if !viewModel.status.results.isEmpty {

                Section("Results") {
                    ForEach(viewModel.status.results) { result in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                Text(result.type)
                                    .font(.headline)
                                Text("Band \(result.band)")
                                    .font(.subheadline)
                                    .foregroundStyle(.secondary)
                            }
                            HStack(spacing: 12) {
                                Label(result.rsrp, systemImage: "antenna.radiowaves.left.and.right")
                                Label(result.sinr, systemImage: "waveform")
                            }
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            Text("PCI: \(result.pci)  EARFCN: \(result.earfcn)")
                                .font(.caption2)
                                .foregroundStyle(.tertiary)
                        }
                        .padding(.vertical, 2)
                        .accessibilityElement(children: .combine)
                    }
                }
            }
        }
        .navigationTitle("Signal Detection")
        .task { await viewModel.resumeIfRunning() }
        .onDisappear { viewModel.stopPolling() }
        .overlay {
            if viewModel.isLoading {
                ProgressView()
                    .padding()
                    .background(Color(.systemBackground).opacity(0.85), in: RoundedRectangle(cornerRadius: 8))
            }
        }
    }
}
