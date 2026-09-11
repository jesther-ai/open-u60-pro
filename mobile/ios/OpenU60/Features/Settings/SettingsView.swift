import SwiftUI

struct SettingsView: View {
    @State private var viewModel: SettingsViewModel
    @FocusState private var gatewayFieldFocused: Bool

    init(client: AgentClient) {
        _viewModel = State(initialValue: SettingsViewModel(client: client))
    }

    var body: some View {
        @Bindable var vm = viewModel
        NavigationStack {
            Form {
                Section {
                    HStack {
                        TextField("Gateway IP", text: $vm.gatewayIP)
                            .keyboardType(.decimalPad)
                            .autocorrectionDisabled()
                            .focused($gatewayFieldFocused)
                            .onSubmit { viewModel.commitGatewayIP() }
                            .onChange(of: gatewayFieldFocused) { _, isFocused in
                                if !isFocused { viewModel.commitGatewayIP() }
                            }
                            // Detection walks the client's base URL across candidates; an edit
                            // committed mid-probe would fight it for the same property.
                            .disabled(viewModel.isDetectingGateway)
                        Button("Detect") {
                            gatewayFieldFocused = false
                            Task { await viewModel.autoDetectGateway() }
                        }
                        .buttonStyle(.bordered)
                        .font(.caption)
                        .disabled(viewModel.isDetectingGateway)
                        if viewModel.isDetectingGateway {
                            ProgressView()
                        }
                    }
                } header: {
                    Text("Gateway")
                } footer: {
                    if viewModel.detectionFailed {
                        Text("No agent answered on the common gateway addresses. The previous address is still in use.")
                    }
                }

                Section("Authentication") {
                    if viewModel.hasStoredPassword {
                        HStack {
                            Text("Password stored in Keychain")
                                .foregroundStyle(.secondary)
                            Spacer()
                            Button("Clear", role: .destructive) {
                                viewModel.clearPassword()
                            }
                            .font(.caption)
                        }
                    }
                    SecureField("New Password", text: $vm.passwordInput)
                    Button("Save to Keychain") {
                        withAnimation { viewModel.savePassword() }
                    }
                    .disabled(viewModel.passwordInput.isEmpty)
                }

                Section {
                    VStack(alignment: .leading) {
                        Text("Refresh interval: \(viewModel.pollInterval, specifier: "%.1f")s")
                        Slider(value: $vm.pollInterval, in: 1...10, step: 0.5)
                            .accessibilityLabel("Refresh interval")
                            .accessibilityValue(String(format: "%.1f seconds", viewModel.pollInterval))
                    }
                } header: {
                    Text("Polling")
                } footer: {
                    Text("Live upload and download use a separate 1-second refresh, matching the modem’s update rate.")
                }

                Section("Appearance") {
                    Picker("Theme", selection: $vm.darkModeOverride) {
                        Text("System").tag(0)
                        Text("Light").tag(1)
                        Text("Dark").tag(2)
                    }
                    .pickerStyle(.segmented)
                }

                Section("About") {
                    LabeledContent("App", value: "OpenU60")
                    LabeledContent("Device", value: "ZTE U60 Pro (MU5250)")
                    LabeledContent("API", value: "zte-agent REST")
                }

                Section("Legal") {
                    Text("This app is not affiliated with, endorsed by, or sponsored by ZTE Corporation. ZTE and U60 Pro are trademarks of ZTE Corporation.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Link("Privacy Policy", destination: URL(string: "https://open-u60-pro.vercel.app/privacy")!)
                }
            }
            .navigationTitle("Settings")
            .overlay {
                if viewModel.showSavedConfirmation {
                    savedToast
                }
            }
            .task { await viewModel.refreshStoredPasswordState() }
        }
    }

    private var savedToast: some View {
        Text("Password saved")
            .font(.subheadline.weight(.medium))
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .background(Color(.systemBackground).opacity(0.85), in: RoundedRectangle(cornerRadius: 10))
            .transition(.move(edge: .top).combined(with: .opacity))
            .task {
                try? await Task.sleep(for: .seconds(1.5))
                withAnimation { viewModel.showSavedConfirmation = false }
            }
    }
}
