import SwiftUI

struct DNSSettingsView: View {
    @Bindable var viewModel: DNSSettingsViewModel

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

            // MARK: - DNS Mode

            Section {
                Picker("Mode", selection: $viewModel.selectedMode) {
                    ForEach(DNSMode.allCases) { mode in
                        Text(mode.label).tag(mode)
                    }
                }
                .pickerStyle(.segmented)
            } header: {
                Text("DNS Mode")
            } footer: {
                Text(modeFooter)
            }

            // MARK: - DNS Servers

            if viewModel.selectedMode != .auto {
                Section("DNS Servers") {
                    TextField("Primary", text: $viewModel.editPrimary)
                        .keyboardType(.decimalPad)
                        .textContentType(.URL)
                        .autocorrectionDisabled()

                    TextField("Secondary", text: $viewModel.editSecondary)
                        .keyboardType(.decimalPad)
                        .textContentType(.URL)
                        .autocorrectionDisabled()
                }
            }

            // MARK: - DoH Upstream

            if viewModel.selectedMode == .doh {
                Section("DoH Upstream") {
                    TextField("URL", text: $viewModel.editUpstream)
                        .keyboardType(.URL)
                        .textContentType(.URL)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                }
            }

            if viewModel.selectedMode == .dot {
                Section("DNS over TLS") {
                    TextField("Server (host:853)", text: $viewModel.editDotEndpoint)
                        .keyboardType(.URL)
                        .textContentType(.URL)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)

                    TextField("TLS certificate name", text: $viewModel.editDotServerName)
                        .keyboardType(.URL)
                        .textContentType(.URL)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                }
            }

            // MARK: - Apply

            Section {
                Button {
                    Task { await viewModel.apply() }
                } label: {
                    Text("Apply")
                        .frame(maxWidth: .infinity)
                }
                .disabled(viewModel.isLoading)
            }

            // MARK: - Quick Setup

            Section {
                Button("Cloudflare (1.1.1.1)") {
                    viewModel.applyPreset(primary: "1.1.1.1", secondary: "1.0.0.1",
                                          upstream: "https://1.1.1.1/dns-query")
                }
                Button("Google (8.8.8.8)") {
                    viewModel.applyPreset(primary: "8.8.8.8", secondary: "8.8.4.4",
                                          upstream: "https://8.8.8.8/dns-query")
                }
                Button("Quad9 (9.9.9.9)") {
                    viewModel.applyPreset(primary: "9.9.9.9", secondary: "149.112.112.112",
                                          upstream: "https://9.9.9.9:5053/dns-query")
                }
            } header: {
                Text("Quick Setup")
            } footer: {
                Text("Fills DNS fields. Tap Apply to save.")
            }

            // MARK: - DoH Cache

            if viewModel.doh.enabled {
                Section("DoH Cache") {
                    LabeledContent("Entries", value: "\(viewModel.doh.cacheEntries)")
                    LabeledContent("Hits", value: "\(viewModel.doh.cacheHits)")
                    LabeledContent("Misses", value: "\(viewModel.doh.cacheMisses)")
                    LabeledContent("Hit Ratio", value: String(format: "%.1f%%", viewModel.doh.hitRatio))

                    Button("Inspect Cache") {
                        viewModel.showCacheInspector = true
                    }
                    .disabled(viewModel.cacheEntries.isEmpty)

                    Button("Clear Cache") {
                        Task { await viewModel.clearCache() }
                    }
                    .disabled(viewModel.isLoading || viewModel.doh.cacheEntries == 0)
                }
            }
        }
        .navigationTitle("DNS Settings")
        .refreshable { await viewModel.refresh() }
        .overlay {
            if viewModel.isLoading {
                ProgressView()
                    .padding()
                    .background(Color(.systemBackground).opacity(0.85), in: RoundedRectangle(cornerRadius: 8))
            }
        }
        .task { await viewModel.refresh() }
        .sheet(isPresented: $viewModel.showCacheInspector) {
            DoHCacheInspectorView(entries: viewModel.cacheEntries) {
                Task { await viewModel.refreshCache() }
            }
        }
    }

    private var modeFooter: String {
        switch viewModel.selectedMode {
        case .auto: "Use DNS servers assigned by your ISP."
        case .custom: "Use custom DNS servers."
        case .doh: "Encrypt DNS queries over HTTPS."
        case .dot: "Encrypt DNS queries over TLS."
        }
    }

}
