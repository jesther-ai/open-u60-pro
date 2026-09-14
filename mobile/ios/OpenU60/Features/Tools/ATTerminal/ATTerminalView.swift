import SwiftUI
import UIKit

struct ATTerminalView: View {
    @Bindable var viewModel: ATTerminalViewModel

    private let quickCommands: [(cmd: String, label: String)] = [
        ("AT", "Ping"),
        ("ATI", "Device info"),
        ("AT+CSQ", "Signal quality"),
        ("AT+COPS?", "Operator"),
        ("AT+CPIN?", "SIM status"),
        ("AT+CGDCONT?", "APN config"),
        ("AT+CGSN", "IMEI"),
        ("AT+CLCC", "Active calls"),
    ]

    /// Fixed-format, so it needs a fixed locale — and `static` so it isn't rebuilt with the view.
    private static let timestampFormatter: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "HH:mm:ss"
        return f
    }()

    private var portAccessibilityLabel: String {
        guard let port = viewModel.portName else { return "No port detected" }
        return viewModel.portAvailable ? "Port \(port) available" : "Port \(port) unavailable"
    }

    var body: some View {
        VStack(spacing: 0) {
            // Port status
            HStack(spacing: 8) {
                Circle()
                    .fill(viewModel.portAvailable ? .green : .red)
                    .frame(width: 10, height: 10)
                    .accessibilityHidden(true)
                Text(viewModel.portName ?? "No port detected")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(portAccessibilityLabel)
                Spacer()
                if !viewModel.history.isEmpty {
                    Button("Clear") {
                        viewModel.clearHistory()
                    }
                    .font(.caption)
                }
            }
            .padding(.horizontal)
            .padding(.vertical, 8)

            // Quick-insert chips
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(quickCommands, id: \.cmd) { item in
                        Button {
                            viewModel.insertCommand(item.cmd)
                        } label: {
                            VStack(spacing: 2) {
                                Text(item.cmd)
                                    .font(.caption.monospaced())
                                Text(item.label)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background(.fill.tertiary)
                            .clipShape(Capsule())
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal)
            }
            .padding(.bottom, 8)

            Divider()

            // History
            if viewModel.history.isEmpty {
                ContentUnavailableView {
                    Label("No Commands Sent", systemImage: "terminal")
                } description: {
                    Text("Enter an AT command below or tap a quick command above.")
                }
                .frame(maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(viewModel.history) { entry in
                            historyEntryView(entry)
                        }
                    }
                    .padding()
                }
            }

            Divider()

            // Bottom input bar
            VStack(spacing: 8) {
                HStack(spacing: 8) {
                    HStack(spacing: 4) {
                        Text("Timeout:")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Stepper("\(viewModel.timeout)s", value: $viewModel.timeout, in: 1...30)
                            .labelsHidden()
                            .accessibilityLabel("Command timeout in seconds")
                        Text("\(viewModel.timeout)s")
                            .font(.caption.monospacedDigit())
                            .frame(width: 28, alignment: .trailing)
                    }
                    Spacer()
                }
                .padding(.horizontal)

                HStack(spacing: 8) {
                    TextField("AT command...", text: $viewModel.currentCommand)
                        .textFieldStyle(.roundedBorder)
                        .font(.body.monospaced())
                        .textInputAutocapitalization(.characters)
                        .autocorrectionDisabled()
                        .submitLabel(.send)
                        .onSubmit { viewModel.send() }

                    Button {
                        viewModel.send()
                    } label: {
                        if viewModel.isLoading {
                            ProgressView()
                                .frame(width: 20, height: 20)
                        } else {
                            Image(systemName: "paperplane.fill")
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .accessibilityLabel("Send command")
                    .disabled(viewModel.currentCommand.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || viewModel.isLoading)
                }
                .padding(.horizontal)
                .padding(.bottom, 8)
            }
        }
        .navigationTitle("AT Terminal")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await viewModel.checkPort()
        }
        .alert("Dangerous Command", isPresented: $viewModel.showDangerConfirm) {
            Button("Cancel", role: .cancel) {
                viewModel.pendingDangerousCommand = nil
            }
            Button("Send Anyway", role: .destructive) {
                viewModel.confirmDangerousSend()
            }
        } message: {
            Text("The command \"\(viewModel.pendingDangerousCommand ?? "")\" may disrupt connectivity or require a reboot. Are you sure?")
        }
    }

    @ViewBuilder
    private func historyEntryView(_ entry: ATHistoryEntry) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            // Command line
            HStack {
                Text("> \(entry.command)")
                    .font(.body.monospaced().bold())
                    .foregroundStyle(entry.isError ? .red : .primary)
                Spacer()
                Button {
                    UIPasteboard.general.string = entry.response
                } label: {
                    Image(systemName: "doc.on.doc")
                        .font(.caption)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .accessibilityLabel("Copy response")
            }

            // Response
            Text(entry.response)
                .font(.caption.monospaced())
                .foregroundStyle(entry.isError ? .red : .secondary)
                .textSelection(.enabled)

            // Metadata
            HStack(spacing: 12) {
                if !entry.port.isEmpty {
                    Text(entry.port)
                }
                Text("\(entry.elapsedMs)ms")
                Text(Self.timestampFormatter.string(from: entry.timestamp))
            }
            .font(.caption2)
            .foregroundStyle(.tertiary)
        }
        .padding(10)
        .background(.fill.quinary)
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }
}
