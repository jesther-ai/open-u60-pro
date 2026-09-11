import SwiftUI

struct ToolsListView: View {
    let client: AgentClient
    let authManager: AuthManager

    // MARK: - Routes

    private enum Route: Hashable {
        case scheduler
        case smsForwarding
        case speedTest
        case lanSpeedTest
        case enableADB
        case usbMode
        case bandLock
        case atTerminal
        case deviceInfo
        case clients
        case config
        case ttlSettings
        case enableSSH
        case deviceExplorer
    }

    // MARK: - Body

    var body: some View {
        NavigationStack {
            List {
                Section("Automation") {
                    NavigationLink(value: Route.scheduler) {
                        Label("Automations", systemImage: "clock.arrow.2.circlepath")
                    }
                    NavigationLink(value: Route.smsForwarding) {
                        Label("SMS Forwarding", systemImage: "envelope.arrow.triangle.branch")
                    }
                }

                Section("Network Tools") {
                    NavigationLink(value: Route.speedTest) {
                        Label("Speed Test", systemImage: "speedometer")
                    }

                    NavigationLink(value: Route.lanSpeedTest) {
                        Label("LAN Speed Test", systemImage: "wifi")
                    }

                    NavigationLink(value: Route.enableADB) {
                        Label("Enable ADB", systemImage: "cable.connector.horizontal")
                    }

                    NavigationLink(value: Route.usbMode) {
                        Label("USB Mode", systemImage: "cable.connector")
                    }

                    NavigationLink(value: Route.bandLock) {
                        Label("Band Lock", systemImage: "lock.fill")
                    }

                    NavigationLink(value: Route.atTerminal) {
                        Label("AT Terminal", systemImage: "terminal.fill")
                    }

                    NavigationLink(value: Route.deviceInfo) {
                        Label("Device Info", systemImage: "info.circle")
                    }

                    NavigationLink(value: Route.clients) {
                        Label("Connected Devices", systemImage: "laptopcomputer.and.iphone")
                    }
                }

                Section("Config") {
                    NavigationLink(value: Route.config) {
                        Label("Config Decrypt/Encrypt", systemImage: "doc.badge.gearshape")
                    }
                }

                Section("Shell Access Required") {
                    NavigationLink(value: Route.ttlSettings) {
                        Label("TTL Settings", systemImage: "number")
                    }

                    NavigationLink(value: Route.enableSSH) {
                        Label("Enable SSH", systemImage: "terminal")
                    }

                    NavigationLink(value: Route.deviceExplorer) {
                        Label("Device Explorer", systemImage: "folder")
                    }
                }
            }
            .navigationTitle("Tools")
            .navigationDestination(for: Route.self) { route in
                destination(for: route)
            }
        }
    }

    // MARK: - Destinations

    @ViewBuilder
    private func destination(for route: Route) -> some View {
        switch route {
        case .scheduler:
            PushedModelHost(SchedulerViewModel(client: client, authManager: authManager)) {
                SchedulerView(viewModel: $0)
            }
        case .smsForwarding:
            PushedModelHost(SMSForwardViewModel(client: client, authManager: authManager)) {
                SMSForwardConfigView(viewModel: $0)
            }
        case .speedTest:
            PushedModelHost(SpeedTestViewModel(client: client, authManager: authManager)) {
                SpeedTestView(viewModel: $0)
            }
        case .lanSpeedTest:
            PushedModelHost(LANSpeedTestViewModel(client: client)) {
                LANSpeedTestView(viewModel: $0)
            }
        case .enableADB:
            EnableADBView(client: client, authManager: authManager)
        case .usbMode:
            PushedModelHost(USBConnectionViewModel(client: client, authManager: authManager)) {
                USBModeView(viewModel: $0)
            }
        case .bandLock:
            PushedModelHost(BandLockViewModel(client: client, authManager: authManager)) {
                BandLockView(viewModel: $0)
            }
        case .atTerminal:
            PushedModelHost(ATTerminalViewModel(client: client, authManager: authManager)) {
                ATTerminalView(viewModel: $0)
            }
        case .deviceInfo:
            PushedModelHost(DeviceInfoViewModel(client: client, authManager: authManager)) {
                DeviceInfoView(viewModel: $0)
            }
        case .clients:
            PushedModelHost(ClientsViewModel(client: client, authManager: authManager)) {
                ClientsView(viewModel: $0)
            }
        case .config:
            ConfigToolView()
        case .ttlSettings:
            PlaceholderView(title: "TTL Settings", icon: "number", description: "Set TTL override via iptables. Requires shell access.")
        case .enableSSH:
            PlaceholderView(title: "Enable SSH", icon: "terminal", description: "Install and start dropbear SSH server. Requires ADB USB connection.")
        case .deviceExplorer:
            PlaceholderView(title: "Device Explorer", icon: "folder", description: "Browse filesystem and collect device info. Requires ADB USB connection.")
        }
    }
}

// MARK: - Pushed model host

/// Holds a pushed screen's view model in `@State` so the model is built once, when the
/// route is actually pushed, and survives re-evaluation of the navigation destination.
///
/// Construction has to happen inside the box: `State.init(wrappedValue:)` is not itself an
/// autoclosure, so building the model in `init` would build and discard one on every
/// re-evaluation of the `.navigationDestination` closure.
private struct PushedModelHost<Model, Content: View>: View {
    @State private var box: LazyModelBox<Model>
    private let content: (Model) -> Content

    init(_ model: @autoclosure @escaping () -> Model, @ViewBuilder content: @escaping (Model) -> Content) {
        _box = State(wrappedValue: LazyModelBox(model))
        self.content = content
    }

    var body: some View {
        content(box.model)
    }
}

/// Defers building a pushed screen's view model until the first time it is read, which only
/// happens for the box `@State` actually keeps.
@MainActor
private final class LazyModelBox<Model> {
    lazy var model: Model = build()

    private let build: () -> Model

    init(_ build: @escaping () -> Model) {
        self.build = build
    }
}
