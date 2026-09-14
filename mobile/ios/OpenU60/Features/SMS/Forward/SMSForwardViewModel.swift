import SwiftUI

@Observable
@MainActor
final class SMSForwardViewModel {
    var config = SmsForwardConfig()
    var lastForwardedId: Int = 0
    var log: [ForwardLogEntry] = []
    var isLoading = false
    var message: String?
    var messageIsError = false
    var presentedSheet: Sheet?

    enum Sheet: Identifiable {
        case add
        case edit(ForwardRule)
        var id: String {
            switch self {
            case .add: "add"
            case .edit(let r): "edit-\(r.id)"
            }
        }
    }

    private let client: AgentClient
    private let authManager: AuthManager

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    // MARK: - Config

    func refresh() async {
        isLoading = true
        message = nil
        do {
            let data = try await client.getJSON("/api/sms/forward/config")
            if let configDict = data["config"] as? [String: Any] {
                config = SMSForwardParser.parseConfig(configDict)
            }
            lastForwardedId = (data["last_forwarded_id"] as? Int)
                ?? (data["last_forwarded_id"] as? NSNumber)?.intValue
                ?? 0
        } catch {
            report(error, as: "Failed to load")
        }
        isLoading = false
    }

    func updateConfig(enabled: Bool, pollIntervalSecs: Int, markRead: Bool, deleteAfter: Bool) async {
        isLoading = true
        do {
            let body: [String: Any] = [
                "enabled": enabled,
                "poll_interval_secs": pollIntervalSecs,
                "mark_read_after_forward": markRead,
                "delete_after_forward": deleteAfter,
            ]
            let _ = try await client.putJSON("/api/sms/forward/config", body: body)
            config.enabled = enabled
            config.pollIntervalSecs = pollIntervalSecs
            config.markReadAfterForward = markRead
            config.deleteAfterForward = deleteAfter
            showMessage("Settings saved", isError: false)
        } catch {
            report(error, as: "Failed")
        }
        isLoading = false
    }

    func toggleEnabled(_ enabled: Bool) async {
        let previous = config.enabled
        config.enabled = enabled  // optimistic
        do {
            let _ = try await client.putJSON("/api/sms/forward/config", body: [
                "enabled": enabled,
                "poll_interval_secs": config.pollIntervalSecs,
                "mark_read_after_forward": config.markReadAfterForward,
                "delete_after_forward": config.deleteAfterForward,
            ])
            config.enabled = enabled  // re-assert in case refresh() overwrote during await
        } catch {
            config.enabled = previous  // revert on failure
            report(error, as: "Failed")
        }
    }

    // MARK: - Rules

    func createRule(name: String, filter: SmsFilter, destination: ForwardDestination) async {
        isLoading = true
        do {
            var body: [String: Any] = [
                "name": name,
                "filter": SMSForwardParser.filterToDict(filter),
                "destination": SMSForwardParser.destinationToDict(destination),
            ]
            body["enabled"] = true
            let _ = try await client.postJSON("/api/sms/forward/rules", body: body)
            showMessage("Rule created", isError: false)
            await refresh()
        } catch {
            report(error, as: "Failed")
        }
        isLoading = false
    }

    func updateRule(id: Int, name: String, enabled: Bool, filter: SmsFilter, destination: ForwardDestination) async {
        isLoading = true
        do {
            let body: [String: Any] = [
                "id": id,
                "name": name,
                "enabled": enabled,
                "filter": SMSForwardParser.filterToDict(filter),
                "destination": SMSForwardParser.destinationToDict(destination),
            ]
            let _ = try await client.putJSON("/api/sms/forward/rules", body: body)
            showMessage("Rule updated", isError: false)
            await refresh()
        } catch {
            report(error, as: "Failed")
        }
        isLoading = false
    }

    func deleteRule(id: Int) async {
        do {
            let _ = try await client.deleteJSON("/api/sms/forward/rules", body: ["id": id])
            config.rules.removeAll { $0.id == id }
            showMessage("Rule deleted", isError: false)
        } catch {
            report(error, as: "Failed")
        }
    }

    func toggleRule(id: Int, enabled: Bool) async {
        do {
            let _ = try await client.putJSON("/api/sms/forward/rules/toggle", body: ["id": id, "enabled": enabled])
            if let idx = config.rules.firstIndex(where: { $0.id == id }) {
                config.rules[idx].enabled = enabled
            }
        } catch {
            report(error, as: "Failed")
        }
    }

    // MARK: - Test

    func testDestination(_ destination: ForwardDestination) async {
        isLoading = true
        do {
            let body: [String: Any] = [
                "destination": SMSForwardParser.destinationToDict(destination),
            ]
            let _ = try await client.postJSON("/api/sms/forward/test", body: body)
            showMessage("Test message sent", isError: false)
        } catch {
            report(error, as: "Test failed")
        }
        isLoading = false
    }

    // MARK: - Log

    func fetchLog() async {
        isLoading = true
        do {
            let data = try await client.getJSONArray("/api/sms/forward/log")
            log = data.compactMap { SMSForwardParser.parseLogEntry($0) }
        } catch {
            report(error, as: "Failed to load log")
        }
        isLoading = false
    }

    func clearLog() async {
        do {
            let _ = try await client.postJSON("/api/sms/forward/log/clear")
            log = []
            showMessage("Log cleared", isError: false)
        } catch {
            report(error, as: "Failed")
        }
    }

    func retryForward(index: Int) async {
        do {
            let _ = try await client.postJSON("/api/sms/forward/retry", body: ["index": index])
            showMessage("Retry succeeded", isError: false)
            await fetchLog()
        } catch {
            guard !error.isCancellation else { return }
            report(error, as: "Retry failed")
            await fetchLog()
        }
    }

    // MARK: - Helpers

    private func showMessage(_ text: String, isError: Bool) {
        message = text
        messageIsError = isError
    }

    /// Surfaces a failure, except for cancellation — a torn-down screen is not an error.
    private func report(_ error: Error, as prefix: String) {
        guard !error.isCancellation else { return }
        showMessage("\(prefix): \(error.localizedDescription)", isError: true)
    }
}
