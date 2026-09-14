import SwiftUI

struct TabBarView: View {
    let client: AgentClient
    let authManager: AuthManager
    @AppStorage("poll_interval") private var pollInterval: Double = 2.0
    @Environment(\.scenePhase) private var scenePhase

    private enum Tab: Hashable { case dashboard, sms, tools, router, settings }
    @State private var selectedTab: Tab = .dashboard

    @State private var dashboardVM: DashboardViewModel
    @State private var smsVM: SMSViewModel
    @State private var usbVM: USBConnectionViewModel

    private struct DashboardPollKey: Equatable {
        let interval: Double
        let active: Bool
    }

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
        _dashboardVM = State(initialValue: DashboardViewModel(client: client, authManager: authManager))
        _smsVM = State(initialValue: SMSViewModel(client: client, authManager: authManager))
        _usbVM = State(initialValue: USBConnectionViewModel(client: client, authManager: authManager))
    }

    var body: some View {
        TabView(selection: $selectedTab) {
            DashboardView(viewModel: dashboardVM, isAuthenticated: authManager.isAuthenticated,
                         client: client, authManager: authManager)
                .tabItem {
                    Label("Dashboard", systemImage: "gauge.with.needle")
                }
                .tag(Tab.dashboard)

            SMSListView(viewModel: smsVM, client: client, authManager: authManager)
                .tabItem {
                    Label("SMS", systemImage: "message")
                }
                .tag(Tab.sms)

            ToolsListView(client: client, authManager: authManager)
                .tabItem {
                    Label("Tools", systemImage: "wrench.and.screwdriver")
                }
                .tag(Tab.tools)

            RouterSettingsListView(client: client, authManager: authManager)
                .tabItem {
                    Label("Router", systemImage: "wifi.router")
                }
                .tag(Tab.router)

            SettingsView(client: client)
                .tabItem {
                    Label("Settings", systemImage: "gear")
                }
                .tag(Tab.settings)
        }
        .onChange(of: dashboardPollKey, initial: true) { _, key in
            if key.active {
                dashboardVM.startPolling(interval: key.interval)
            } else {
                dashboardVM.stopPolling()
            }
        }
        .onChange(of: isForeground, initial: true) { _, foreground in
            if foreground {
                usbVM.startPolling()
            } else {
                usbVM.stopPolling()
            }
        }
        .onDisappear {
            dashboardVM.stopPolling()
            usbVM.stopPolling()
        }
        .sheet(isPresented: $usbVM.showModeSheet) {
            USBModeSheetView(viewModel: usbVM)
        }
    }

    // MARK: - Polling gates

    /// Polling only runs in `.active`. Pausing on `.inactive` as well covers the app switcher,
    /// iPad multitasking and the lock transition, where the scene stays alive but nothing the
    /// user can read is on screen; the cost is one extra immediate refresh when a short Control
    /// Centre pull ends, which is what a returning user wants anyway.
    private var isForeground: Bool { scenePhase == .active }

    /// Restarting on any change of this key preserves the previous behaviour: the loop restarts
    /// when the configured interval changes and stops when the dashboard is not on screen.
    private var dashboardPollKey: DashboardPollKey {
        DashboardPollKey(interval: pollInterval, active: isForeground && selectedTab == .dashboard)
    }
}
