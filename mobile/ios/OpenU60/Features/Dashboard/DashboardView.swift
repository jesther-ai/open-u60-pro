import SwiftUI

struct DashboardView: View {
    var viewModel: DashboardViewModel
    let isAuthenticated: Bool
    let client: AgentClient
    let authManager: AuthManager

    @State private var signalMonitorVM: SignalMonitorViewModel?
    @State private var networkModeVM: NetworkModeViewModel?
    @State private var showNetworkModeSheet = false
    @State private var showBatteryDetailSheet = false
    @State private var showCPUDetailSheet = false
    @State private var showAllDevices = true
    @State private var showWiFiShare = false
    @State private var longPressCount = 0

    init(viewModel: DashboardViewModel, isAuthenticated: Bool, client: AgentClient, authManager: AuthManager) {
        self.viewModel = viewModel
        self.isAuthenticated = isAuthenticated
        self.client = client
        self.authManager = authManager
    }

    var body: some View {
        let banners = bannerConfigs
        NavigationStack {
            ScrollView {
                // Deliberately eager: WiFiShareCardView keeps the fetched credentials, the reveal
                // toggle and the generated QR in its own @State, and a lazy stack is free to drop
                // that subview once it leaves the realized window. Seven cards cost nothing to
                // build up front.
                VStack(spacing: 16) {
                    ForEach(banners) { config in
                        SIMAlertBanner(config: config)
                            .transition(.move(edge: .top).combined(with: .opacity))
                    }
                    OperatorCardScope(viewModel: viewModel)
                        .onLongPressGesture {
                            longPressCount += 1
                            showNetworkModeSheet = true
                        }
                    NavigationLink {
                        if let signalMonitorVM {
                            SignalMonitorDestination(dashboard: viewModel, viewModel: signalMonitorVM)
                        }
                    } label: {
                        SignalCardScope(viewModel: viewModel)
                    }
                    .buttonStyle(.plain)
                    CellularCardScope(viewModel: viewModel)
                    HStack(spacing: 16) {
                        BatteryCardScope(viewModel: viewModel)
                            .onLongPressGesture {
                                longPressCount += 1
                                showBatteryDetailSheet = true
                            }
                        CPUCardScope(viewModel: viewModel)
                            .onLongPressGesture {
                                longPressCount += 1
                                showCPUDetailSheet = true
                            }
                    }
                    WiFiCardScope(viewModel: viewModel, showWiFiShare: $showWiFiShare)
                    if showWiFiShare {
                        WiFiShareCardScope(
                            viewModel: viewModel,
                            client: client,
                            authManager: authManager,
                            isExpanded: $showWiFiShare
                        )
                        .transition(.opacity.combined(with: .move(edge: .top)))
                    }
                    DevicesCardScope(viewModel: viewModel, showAllDevices: $showAllDevices)
                }
                .padding()
                .animation(.easeInOut, value: banners)
            }
            .background(Color(.systemGroupedBackground))
            .sensoryFeedback(.impact(weight: .medium), trigger: longPressCount)
            .task {
                if signalMonitorVM == nil {
                    signalMonitorVM = SignalMonitorViewModel(client: client, authManager: authManager)
                }
                if networkModeVM == nil {
                    networkModeVM = NetworkModeViewModel(client: client, authManager: authManager)
                }
            }
            .sheet(isPresented: $showNetworkModeSheet) {
                NavigationStack {
                    if let networkModeVM {
                        NetworkModeView(viewModel: networkModeVM)
                    }
                }
                .presentationDetents([.medium])
            }
            .sheet(isPresented: $showBatteryDetailSheet) {
                BatteryDetailScope(viewModel: viewModel)
                    .presentationDetents([.medium])
            }
            .sheet(isPresented: $showCPUDetailSheet) {
                CPUDetailScope(viewModel: viewModel, client: client)
                    .presentationDetents([.large, .medium])
            }
            .navigationTitle("Dashboard")
            .refreshable { await viewModel.refresh() }
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    LastUpdatedScope(viewModel: viewModel)
                }
                ToolbarItem(placement: .topBarLeading) {
                    connectionIndicator
                }
            }
        }
    }

    private var bannerConfigs: [SIMAlertBanner.Config] {
        var configs: [SIMAlertBanner.Config] = []
        if viewModel.simPukRequired {
            configs.append(SIMAlertBanner.Config(
                id: "puk",
                icon: "exclamationmark.lock.fill",
                title: "SIM PUK Required",
                message: "Too many wrong PIN attempts. Go to Router > SIM Card to enter your PUK.",
                color: .red
            ))
        }
        if viewModel.simPinRequired {
            configs.append(SIMAlertBanner.Config(
                id: "pin",
                icon: "lock.fill",
                title: "SIM PIN Required",
                message: "Your SIM card is locked. Go to Router > SIM Card to enter your PIN.",
                color: .orange
            ))
        }
        if viewModel.isAirplaneMode {
            configs.append(SIMAlertBanner.Config(
                id: "airplane",
                icon: "airplane",
                title: "Airplane Mode",
                message: "Cellular radio is off. The modem is powered down — no signal or data.",
                color: .blue
            ))
        }
        if viewModel.isMobileDataOff && !viewModel.isAirplaneMode {
            configs.append(SIMAlertBanner.Config(
                id: "data",
                icon: "antenna.radiowaves.left.and.right.slash",
                title: "Mobile Data Off",
                message: "Cellular radio is on but data is disabled. Go to Router > Mobile Network to enable it.",
                color: .orange
            ))
        }
        return configs
    }

    private var connectionIndicator: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(isAuthenticated ? .green : .red)
                .frame(width: 10, height: 10)
            Text(isAuthenticated ? "Connected" : "Offline")
                .font(.caption)
                .fontWeight(.medium)
                .foregroundStyle(isAuthenticated ? .green : .red)
        }
        .fixedSize()
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(
            Capsule()
                .fill((isAuthenticated ? Color.green : Color.red).opacity(0.12))
        )
    }
}

// MARK: - Observation scopes

// Each wrapper below reads the DashboardViewModel properties for exactly one card inside its own
// body. @Observation installs tracking per body evaluation, not per property, so
// `let battery = viewModel.battery` in DashboardView.body would still subscribe the whole
// dashboard to `battery`. Only a separate View type moves the read into its own invalidation
// unit; passing the view model itself down reads nothing and so tracks nothing.

private struct OperatorCardScope: View {
    let viewModel: DashboardViewModel

    var body: some View {
        OperatorCardView(
            operatorInfo: viewModel.operatorInfo,
            nrSignal: viewModel.nrSignal,
            lteSignal: viewModel.lteSignal
        )
    }
}

private struct SignalCardScope: View {
    let viewModel: DashboardViewModel

    var body: some View {
        SignalCardView(
            operatorInfo: viewModel.operatorInfo,
            nrSignal: viewModel.nrSignal,
            lteSignal: viewModel.lteSignal,
            wcdmaSignal: viewModel.wcdmaSignal,
            isAirplaneMode: viewModel.isAirplaneMode
        )
    }
}

private struct CellularCardScope: View {
    let viewModel: DashboardViewModel

    var body: some View {
        CellularCardView(
            wanIPv4: viewModel.wanIPv4,
            wanIPv6: viewModel.wanIPv6,
            speed: viewModel.speed,
            trafficStats: viewModel.trafficStats,
            isTrafficAvailable: viewModel.isTrafficAvailable
        )
    }
}

private struct BatteryCardScope: View {
    let viewModel: DashboardViewModel

    var body: some View {
        BatteryCardView(battery: viewModel.battery)
    }
}

private struct CPUCardScope: View {
    let viewModel: DashboardViewModel

    var body: some View {
        CPUCardView(systemInfo: viewModel.systemInfo, thermal: viewModel.thermal)
    }
}

private struct WiFiCardScope: View {
    let viewModel: DashboardViewModel
    @Binding var showWiFiShare: Bool

    var body: some View {
        WiFiCardView(wifiStatus: viewModel.wifiStatus, showWiFiShare: $showWiFiShare)
    }
}

private struct WiFiShareCardScope: View {
    let viewModel: DashboardViewModel
    let client: AgentClient
    let authManager: AuthManager
    @Binding var isExpanded: Bool

    var body: some View {
        WiFiShareCardView(
            wifiStatus: viewModel.wifiStatus,
            client: client,
            authManager: authManager,
            isExpanded: $isExpanded
        )
    }
}

private struct DevicesCardScope: View {
    let viewModel: DashboardViewModel
    @Binding var showAllDevices: Bool

    var body: some View {
        DevicesCardView(connectedDevices: viewModel.connectedDevices, showAllDevices: $showAllDevices)
    }
}

private struct LastUpdatedScope: View {
    let viewModel: DashboardViewModel

    var body: some View {
        LastUpdatedView(date: viewModel.lastUpdated)
    }
}

private struct BatteryDetailScope: View {
    let viewModel: DashboardViewModel

    var body: some View {
        BatteryDetailSheet(battery: viewModel.battery)
    }
}

private struct CPUDetailScope: View {
    let viewModel: DashboardViewModel
    let client: AgentClient

    var body: some View {
        CPUDetailSheet(systemInfo: viewModel.systemInfo, thermal: viewModel.thermal, client: client)
            // Killing bloat frees ~225 MB at once, and system info only reloads on the 60s tier.
            // Nothing inside a sheet can reach pull-to-refresh, so the sheet stack asks for one.
            .environment(\.processKillRefresh, { await viewModel.refresh() })
    }
}

// MARK: - Signal Monitor destination

/// Holds the dashboard's signal-fetch suspension for exactly as long as the Signal Monitor is on
/// screen. That screen polls `/api/network/signal` itself, so the dashboard must not poll it too.
/// The flag keeps suspend and resume balanced per appearance: SwiftUI runs `onAppear` again when
/// the user returns to this tab with the screen still pushed, and an unmatched call would leave
/// the counter stuck.
private struct SignalMonitorDestination: View {
    let dashboard: DashboardViewModel
    let viewModel: SignalMonitorViewModel

    @State private var isSuspended = false

    var body: some View {
        SignalMonitorView(viewModel: viewModel)
            .onAppear {
                guard !isSuspended else { return }
                isSuspended = true
                dashboard.suspendSignalFetch()
            }
            .onDisappear {
                guard isSuspended else { return }
                isSuspended = false
                dashboard.resumeSignalFetch()
            }
    }
}

// MARK: - Banner

private struct SIMAlertBanner: View {
    struct Config: Identifiable, Equatable {
        let id: String
        let icon: String
        let title: String
        let message: String
        let color: Color
    }

    let config: Config

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: config.icon)
                .font(.title2)
                .foregroundStyle(config.color)
            VStack(alignment: .leading, spacing: 2) {
                Text(config.title).font(.subheadline.bold()).textSelection(.enabled)
                Text(config.message).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
            Spacer()
        }
        .accessibilityElement(children: .combine)
        .padding(12)
        .background(config.color.opacity(0.1), in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(config.color.opacity(0.3), lineWidth: 1))
    }
}
