import SwiftUI

@Observable
@MainActor
final class SettingsViewModel {
    /// Bound to the text field. Only reaches `UserDefaults` and the client once it parses as a
    /// complete IPv4 address, so the client never chases half-typed input like "192.16".
    var gatewayIP: String {
        didSet {
            guard gatewayIP != oldValue else { return }
            detectionFailed = false
            applyGatewayIfValid()
        }
    }
    var pollInterval: Double {
        didSet { UserDefaults.standard.set(pollInterval, forKey: "poll_interval") }
    }
    var darkModeOverride: Int {
        didSet { UserDefaults.standard.set(darkModeOverride, forKey: "dark_mode_override") }
    }

    var passwordInput: String = ""
    var showSavedConfirmation: Bool = false
    var isDetectingGateway: Bool = false
    var detectionFailed: Bool = false
    private(set) var hasStoredPassword: Bool = false

    private let client: AgentClient
    /// Last address actually written to `UserDefaults` and the client's base URL.
    private var committedGatewayIP: String

    init(client: AgentClient) {
        self.client = client
        let saved = UserDefaults.standard.string(forKey: "gateway_ip") ?? "192.168.0.1"
        self.gatewayIP = saved
        self.committedGatewayIP = saved
        let stored = UserDefaults.standard.double(forKey: "poll_interval")
        self.pollInterval = stored > 0 ? stored : 2.0
        self.darkModeOverride = UserDefaults.standard.integer(forKey: "dark_mode_override")
    }

    // MARK: - Gateway

    /// Called when the field loses focus or the user submits: normalises the text so the address
    /// on screen is always the address requests are going to.
    func commitGatewayIP() {
        applyGatewayIfValid()
        if gatewayIP != committedGatewayIP {
            gatewayIP = committedGatewayIP
        }
    }

    func autoDetectGateway() async {
        isDetectingGateway = true
        detectionFailed = false
        defer { isDetectingGateway = false }

        let startingIP = committedGatewayIP
        let candidates = ["192.168.0.1", "192.168.1.1", "192.168.2.1", "10.0.0.1"]
        for ip in candidates {
            if Task.isCancelled { break }
            client.baseURL = Self.agentURL(for: ip)
            if await client.ping(), await isAgentReachable() {
                gatewayIP = ip
                return
            }
        }

        // Nothing answered: point the client back at the committed address rather than leaving it
        // stranded on the last candidate we probed. If the user committed a different address
        // while we were probing, that one is now the committed address and it wins — along with
        // the failure notice, which would be about a search they have already moved past.
        client.baseURL = Self.agentURL(for: committedGatewayIP)
        detectionFailed = committedGatewayIP == startingIP
    }

    /// `ping()` only proves *something* is listening on the port. Confirm it speaks the agent's
    /// API: either it answers with the JSON envelope, or it rejects us for lack of a session.
    ///
    /// Session recovery is suppressed for the probe. Port 9090 is a common default for unrelated
    /// services, and any of them would answer 401 here; letting the client's hook run would POST
    /// the stored router password to whatever host happened to reply.
    private func isAgentReachable() async -> Bool {
        do {
            _ = try await client.getJSON("/api/device", retryOnUnauthorized: false)
            return true
        } catch {
            return error.isUnauthorized
        }
    }

    private func applyGatewayIfValid() {
        let trimmed = gatewayIP.trimmingCharacters(in: .whitespaces)
        guard trimmed != committedGatewayIP, Self.isValidIPv4(trimmed) else { return }
        committedGatewayIP = trimmed
        UserDefaults.standard.set(trimmed, forKey: "gateway_ip")
        client.baseURL = Self.agentURL(for: trimmed)
    }

    private static func agentURL(for ip: String) -> String {
        "http://\(ip):9090"
    }

    private static func isValidIPv4(_ value: String) -> Bool {
        let parts = value.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 4 else { return false }
        return parts.allSatisfy { part in
            guard !part.isEmpty, part.count <= 3,
                  part.allSatisfy({ $0.isASCII && $0.isNumber }),
                  let octet = Int(part) else { return false }
            return octet <= 255
        }
    }

    // MARK: - Password

    func savePassword() {
        guard !passwordInput.isEmpty else { return }
        KeychainHelper.save(key: "router_password", value: passwordInput)
        passwordInput = ""
        hasStoredPassword = true
        showSavedConfirmation = true
    }

    func clearPassword() {
        KeychainHelper.delete(key: "router_password")
        passwordInput = ""
        hasStoredPassword = false
    }

    /// Reads the Keychain off the main thread, on appear rather than from `body`.
    func refreshStoredPasswordState() async {
        hasStoredPassword = await Task.detached {
            KeychainHelper.load(key: "router_password") != nil
        }.value
    }
}
