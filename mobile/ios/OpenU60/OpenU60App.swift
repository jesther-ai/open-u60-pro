import SwiftUI

@main
struct OpenU60App: App {
    @State private var authManager: AuthManager
    @State private var isAttemptingAutoLogin = true
    @AppStorage("gateway_ip") private var gatewayIP: String = "192.168.0.1"
    @AppStorage("dark_mode_override") private var darkModeOverride: Int = 0

    private let client: AgentClient

    init() {
        let savedIP = UserDefaults.standard.string(forKey: "gateway_ip") ?? "192.168.0.1"
        let agentClient = AgentClient(baseURL: "http://\(savedIP):9090")
        self.client = agentClient
        _authManager = State(initialValue: AuthManager(client: agentClient))

        let navAppearance = UINavigationBarAppearance()
        navAppearance.configureWithTransparentBackground()
        UINavigationBar.appearance().standardAppearance = navAppearance
        UINavigationBar.appearance().compactAppearance = navAppearance
        UINavigationBar.appearance().scrollEdgeAppearance = navAppearance

        let tabAppearance = UITabBarAppearance()
        tabAppearance.configureWithOpaqueBackground()
        UITabBar.appearance().standardAppearance = tabAppearance
        UITabBar.appearance().scrollEdgeAppearance = tabAppearance
    }

    var body: some Scene {
        WindowGroup {
            rootView
                .preferredColorScheme(colorScheme)
                .onChange(of: gatewayIP) {
                    client.baseURL = "http://\(gatewayIP):9090"
                }
        }
    }

    @ViewBuilder
    private var rootView: some View {
        if authManager.isAuthenticated {
            TabBarView(client: client, authManager: authManager)
        } else if isAttemptingAutoLogin {
            VStack(spacing: 16) {
                Image(systemName: "wifi.router.fill")
                    .font(.system(size: 64))
                    .foregroundStyle(.blue)
                    .accessibilityHidden(true)
                Text("ZTE U60 Pro")
                    .font(.title.bold())
                ProgressView()
                    .accessibilityLabel("Signing in")
            }
            .task {
                if KeychainHelper.load(key: "router_password") != nil {
                    if await attemptAutoLogin() { return }
                }
                isAttemptingAutoLogin = false
            }
        } else {
            LoginView(authManager: authManager)
        }
    }

    // MARK: - Auto-login

    /// Stored credentials should not be abandoned because Wi-Fi had not settled at launch, so the
    /// silent login gets a few quick tries. The window keeps a genuinely unreachable agent from
    /// holding the splash screen: once a request has burned its timeout there is nothing transient
    /// left to wait out, and the password screen is the better answer.
    private static let autoLoginAttempts = 3
    private static let autoLoginRetryDelay: TimeInterval = 1
    private static let autoLoginWindow: TimeInterval = 8

    @MainActor
    private func attemptAutoLogin() async -> Bool {
        let deadline = Date().addingTimeInterval(Self.autoLoginWindow)
        for attempt in 0..<Self.autoLoginAttempts {
            // Bypasses the re-auth cooldown, which throttles open-ended poll-tick storms and would
            // otherwise turn every try after the first into a no-op that issues no request at all.
            if await authManager.reauthenticate(ignoringCooldown: true) { return true }
            // The delay has to fit inside the window too. A try that failed instantly raced the
            // interface coming up and is worth repeating; one that consumed most of the window
            // reached the timeout, and a second timeout would strand the splash screen for
            // twice as long as the window allows.
            guard attempt + 1 < Self.autoLoginAttempts,
                  Date().addingTimeInterval(Self.autoLoginRetryDelay) < deadline else { break }
            do {
                try await Task.sleep(for: .seconds(Self.autoLoginRetryDelay))
            } catch {
                break
            }
        }
        return false
    }

    private var colorScheme: ColorScheme? {
        switch darkModeOverride {
        case 1: return .light
        case 2: return .dark
        default: return nil
        }
    }
}
