import SwiftUI

struct RouterSettingsListView: View {
    let client: AgentClient
    let authManager: AuthManager

    var body: some View {
        NavigationStack {
            List {
                Section("Cellular") {
                    NavigationLink {
                        MobileNetworkView(viewModel: MobileNetworkViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("Mobile Network", systemImage: "cellularbars")
                    }

                    NavigationLink {
                        NetworkModeView(viewModel: NetworkModeViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("Network Mode", systemImage: "antenna.radiowaves.left.and.right")
                    }

                    NavigationLink {
                        CellLockView(viewModel: CellLockViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("Cell Lock", systemImage: "lock.fill")
                    }

                    NavigationLink {
                        STCView(viewModel: STCViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("Smart Tower Connect", systemImage: "building.2")
                    }

                    NavigationLink {
                        SignalDetectView(viewModel: SignalDetectViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("Signal Detection", systemImage: "waveform.badge.magnifyingglass")
                    }

                    NavigationLink {
                        SIMView(viewModel: SIMViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("SIM Card", systemImage: "simcard.2")
                    }

                    NavigationLink {
                        STKMenuView(viewModel: STKViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("SIM Services", systemImage: "phone.badge.waveform")
                    }
                }

                Section("Connectivity") {
                    NavigationLink {
                        WiFiSettingsView(viewModel: WiFiSettingsViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("WiFi", systemImage: "wifi")
                    }

                    NavigationLink {
                        GuestWiFiSettingsView(viewModel: GuestWiFiSettingsViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("Guest WiFi", systemImage: "wifi.exclamationmark")
                    }

                    NavigationLink {
                        APNView(viewModel: APNViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("APN", systemImage: "simcard")
                    }

                    NavigationLink {
                        LANSettingsView(viewModel: LANSettingsViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("LAN / DHCP", systemImage: "network")
                    }

                    NavigationLink {
                        DNSSettingsView(viewModel: DNSSettingsViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("DNS", systemImage: "globe")
                    }
                }

                Section("Security") {
                    NavigationLink {
                        FirewallSettingsView(viewModel: FirewallSettingsViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("Firewall", systemImage: "flame")
                    }

                    NavigationLink {
                        TelemetryBlockerView(viewModel: TelemetryBlockerViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("Telemetry Blocker", systemImage: "eye.slash")
                    }

                    NavigationLink {
                        VPNPassthroughView(viewModel: VPNPassthroughViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("VPN Passthrough", systemImage: "lock.shield")
                    }

                    NavigationLink {
                        TailscaleView(viewModel: TailscaleViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("Tailscale", systemImage: "network.badge.shield.half.filled")
                    }
                }

                Section("Quality") {
                    NavigationLink {
                        QoSView(viewModel: QoSViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("QoS", systemImage: "speedometer")
                    }
                }

                Section("System") {
                    NavigationLink {
                        DeviceControlView(viewModel: DeviceControlViewModel(client: client, authManager: authManager))
                    } label: {
                        Label("Device Controls", systemImage: "power")
                    }

                }
            }
            .navigationTitle("Router")
        }
    }
}
