import SwiftUI

struct WiFiShareCardView: View {
    let wifiStatus: WifiStatus
    let client: AgentClient
    let authManager: AuthManager
    @Binding var isExpanded: Bool

    @State private var wifiConfig: WiFiConfig?
    @State private var guestConfig: GuestWiFiConfig?
    @State private var selectedTab: Tab = .main
    @State private var showPassword = false
    @State private var isLoading = true
    @State private var hasFetched = false
    @State private var qrImage: UIImage?

    enum Tab: String, CaseIterable {
        case main = "Main"
        case guest = "Guest"
    }

    private var availableTabs: [Tab] {
        var tabs: [Tab] = [.main]
        if wifiStatus.guestEnabled { tabs.append(.guest) }
        return tabs
    }

    private var currentSSID: String {
        switch selectedTab {
        case .main:
            if !wifiStatus.radio2gDisabled {
                return wifiConfig?.ssid2g ?? wifiStatus.ssid2g
            }
            return wifiConfig?.ssid5g ?? wifiStatus.ssid5g
        case .guest: return guestConfig?.ssid ?? wifiStatus.guestSsid
        }
    }

    private var currentPassword: String {
        switch selectedTab {
        case .main:
            if !wifiStatus.radio2gDisabled {
                return wifiConfig?.key2g ?? ""
            }
            return wifiConfig?.key5g ?? ""
        case .guest: return guestConfig?.key ?? ""
        }
    }

    private var currentEncryption: String {
        switch selectedTab {
        case .main:
            if !wifiStatus.radio2gDisabled {
                return wifiConfig?.encryption2g ?? wifiStatus.encryption2g
            }
            return wifiConfig?.encryption5g ?? wifiStatus.encryption5g
        case .guest: return guestConfig?.encryption ?? "psk2+ccmp"
        }
    }

    var body: some View {
        CardView {
            VStack(alignment: .leading, spacing: 8) {
                Button {
                    withAnimation { isExpanded.toggle() }
                } label: {
                    HStack {
                        Label("WiFi Credentials", systemImage: "key.radiowaves.forward")
                            .font(.headline)
                        Spacer()
                        Image(systemName: "chevron.up")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .rotationEffect(.degrees(isExpanded ? 0 : 180))
                    }
                }
                .buttonStyle(.plain)

                if isExpanded {
                    Divider()

                    if isLoading {
                        HStack {
                            Spacer()
                            ProgressView()
                            Spacer()
                        }
                        .padding(.vertical, 8)
                    } else {
                        let tabs = availableTabs
                        if tabs.count > 1 {
                            HStack(spacing: 12) {
                                ForEach(tabs, id: \.self) { tab in
                                    Button {
                                        showPassword = false
                                        selectedTab = tab
                                    } label: {
                                        Text(tab.rawValue)
                                            .font(.subheadline.weight(.medium))
                                            .frame(maxWidth: .infinity)
                                            .padding(.vertical, 8)
                                            .background(selectedTab == tab ? Color.accentColor : Color(.secondarySystemFill))
                                            .foregroundStyle(selectedTab == tab ? .white : .primary)
                                            .clipShape(Capsule())
                                    }
                                }
                            }
                        }

                        VStack(spacing: 12) {
                            Text(currentSSID)
                                .font(.subheadline.bold())

                            HStack(spacing: 12) {
                                let password = currentPassword
                                Text(showPassword ? password : String(repeating: "\u{2022}", count: min(password.count, 12)))
                                    .font(.callout.monospaced())
                                    .textSelection(.enabled)

                                Button { showPassword.toggle() } label: {
                                    Image(systemName: showPassword ? "eye.slash" : "eye")
                                        .foregroundStyle(.secondary)
                                }
                                .accessibilityLabel(showPassword ? "Hide password" : "Show password")

                                Button {
                                    UIPasteboard.general.string = currentPassword
                                } label: {
                                    Image(systemName: "doc.on.doc")
                                        .foregroundStyle(.secondary)
                                }
                                .accessibilityLabel("Copy password")
                            }

                            if let qrImage {
                                Image(uiImage: qrImage)
                                    .interpolation(.none)
                                    .resizable()
                                    .scaledToFit()
                                    .frame(maxWidth: 180, maxHeight: 180)
                            }

                            Text("Scan with any phone camera")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity)
                        .task(id: "\(currentSSID)\(currentPassword)\(currentEncryption)") {
                            let ssid = currentSSID
                            let password = currentPassword
                            let encryption = currentEncryption
                            let image = await Task.detached(priority: .userInitiated) {
                                WiFiQRGenerator.generate(ssid: ssid, password: password, encryption: encryption)
                            }.value
                            qrImage = image
                        }
                    }
                }
            }
        }
        .task(id: isExpanded) {
            guard isExpanded, !hasFetched else { return }
            hasFetched = true
            await fetchCredentials()
        }
    }

    private func fetchCredentials() async {
        async let mainFetch: Void = fetchMainWiFi()
        async let guestFetch: Void = fetchGuestWiFi()
        _ = await (mainFetch, guestFetch)

        let tabs = availableTabs
        if !tabs.contains(selectedTab), let first = tabs.first {
            selectedTab = first
        }
        isLoading = false
    }

    private func fetchMainWiFi() async {
        if let data = try? await client.getJSON("/api/wifi/status"),
           data["htmode_2g"] != nil {
            wifiConfig = WiFiParser.parse(data)
            return
        }
        // Agent API is the only supported path

    }

    private func fetchGuestWiFi() async {
        guard wifiStatus.guestEnabled else { return }

        if let data = try? await client.getJSON("/api/wifi/guest"),
           data["ssid"] != nil {
            guestConfig = GuestWiFiParser.parse(data)
            return
        }

        // Agent API is the only supported path

    }
}
