import Foundation
import Observation

/// Manages authentication state and session tokens for the zte-agent API.
@Observable
@MainActor
final class AuthManager {
    enum AuthState: Equatable {
        case idle
        case authenticating
        case authenticated
        case failed(String)
    }

    var state: AuthState = .idle

    var sessionToken: String {
        client.token ?? ""
    }

    var isAuthenticated: Bool { state == .authenticated }

    /// How long a failed silent re-auth suppresses the next attempt. Without it an unreachable
    /// router turns every poll tick into another login request.
    private static let reauthCooldown: Duration = .seconds(10)

    private let client: AgentClient

    @ObservationIgnored
    private var reauthTask: Task<Bool, Never>?

    @ObservationIgnored
    private var lastReauthFailure: ContinuousClock.Instant?

    init(client: AgentClient) {
        self.client = client
        client.onUnauthorized = { [weak self] in
            guard let self else { return false }
            return await self.reauthenticate()
        }
    }

    /// Perform login with plaintext password via the agent.
    func login(password: String) async {
        state = .authenticating
        do {
            try await client.login(password: password)
            lastReauthFailure = nil
            state = .authenticated
        } catch {
            state = error.isCancellation ? .idle : .failed(error.localizedDescription)
        }
    }

    /// Clear the current session.
    func logout() {
        reauthTask?.cancel()
        reauthTask = nil
        lastReauthFailure = nil
        client.token = nil
        state = .idle
    }

    /// Re-authenticate silently using stored credentials.
    /// Does NOT change auth state on failure.
    ///
    /// Concurrent callers — the client's 401 hook fanning out across a dashboard tick, plus any
    /// poll loop — share one in-flight attempt, and a failed attempt is not retried until
    /// `reauthCooldown` has elapsed.
    ///
    /// `ignoringCooldown` is for a caller that is itself a bounded, explicitly sequenced retry —
    /// the launch auto-login — rather than an open-ended stream of ticks. Everything else must
    /// leave it alone or the throttle stops throttling.
    func reauthenticate(ignoringCooldown: Bool = false) async -> Bool {
        if let inFlight = reauthTask {
            return await inFlight.value
        }
        if !ignoringCooldown, let lastReauthFailure,
           lastReauthFailure.duration(to: .now) < Self.reauthCooldown {
            return false
        }

        let attempt = Task { [weak self] in
            guard let self else { return false }
            return await self.performReauthentication()
        }
        reauthTask = attempt
        let succeeded = await attempt.value
        reauthTask = nil
        return succeeded
    }

    private func performReauthentication() async -> Bool {
        guard let password = await KeychainHelper.loadInBackground(key: "router_password") else {
            lastReauthFailure = .now
            return false
        }
        do {
            try await client.login(password: password)
            lastReauthFailure = nil
            state = .authenticated
            return true
        } catch {
            // A cancelled request says nothing about the credentials, so it must not start a
            // cooldown that would block the next genuine attempt.
            if !error.isCancellation {
                lastReauthFailure = .now
            }
            return false
        }
    }
}

// MARK: - Keychain Helper

enum KeychainHelper {
    private static let service = "com.openu60.app"

    static func save(key: String, value: String) {
        let base = query(for: key)
        SecItemDelete(base as CFDictionary)
        var addQuery = base
        addQuery[kSecValueData as String] = Data(value.utf8)
        SecItemAdd(addQuery as CFDictionary, nil)
    }

    static func load(key: String) -> String? {
        var lookup = query(for: key)
        lookup[kSecReturnData as String] = true
        lookup[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        let status = SecItemCopyMatching(lookup as CFDictionary, &result)
        guard status == errSecSuccess, let data = result as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    /// `load` off the caller's actor. `SecItemCopyMatching` is a synchronous round trip to
    /// securityd, which is not work the main thread should be doing on a failing poll tick.
    static func loadInBackground(key: String) async -> String? {
        await Task.detached(priority: .userInitiated) { load(key: key) }.value
    }

    static func delete(key: String) {
        SecItemDelete(query(for: key) as CFDictionary)
    }

    private static func query(for key: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: key,
            kSecAttrService as String: service
        ]
    }
}
