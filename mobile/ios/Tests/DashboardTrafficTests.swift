import Foundation

// Stand-ins used only by the standalone test executable, never by the iOS target.
@MainActor
final class AgentClient {
    var pending: [CheckedContinuation<[String: Any], Error>] = []
    var paths: [String] = []

    func getJSON(_ path: String) async throws -> [String: Any] {
        paths.append(path)
        if path == "/api/network/speeds" {
            // Deliberately ignore cancellation to simulate a late transport response.
            return try await withCheckedThrowingContinuation { pending.append($0) }
        }
        try await Task.sleep(for: .seconds(3))
        return [:]
    }

    func get<T: Decodable>(_ path: String) async throws -> T {
        paths.append(path)
        try await Task.sleep(for: .seconds(3))
        throw AgentError.serverError("No fixture")
    }

    func answer(_ rate: Int) {
        pending.removeFirst().resume(returning: [
            "real_rx_bytes": 1000, "real_tx_bytes": 2000,
            "real_rx_speed": rate, "real_tx_speed": rate / 2
        ])
    }
}

@MainActor final class AuthManager {}

@main
struct DashboardTrafficTests {
    @MainActor
    static func eventually(_ condition: () -> Bool) async throws {
        for _ in 0..<100 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        preconditionFailure("Traffic did not update independently of slow dashboard requests")
    }

    @MainActor
    static func measureCadence(_ schedule: PollingLoop.Schedule) async throws -> Double {
        let loop = PollingLoop()
        let clock = ContinuousClock()
        var starts: [ContinuousClock.Instant] = []
        var active = 0
        loop.start(interval: .milliseconds(300), schedule: schedule) {
            active += 1
            precondition(active == 1, "Polling requests must not overlap")
            starts.append(clock.now)
            try? await Task.sleep(for: .milliseconds(200))
            active -= 1
            return .success
        }
        for _ in 0..<200 {
            if starts.count >= 3 { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        loop.stop()
        precondition(starts.count >= 3)
        let span = starts[0].duration(to: starts[2]).components
        return Double(span.seconds) + Double(span.attoseconds) / 1e18
    }

    @MainActor
    static func main() async throws {
        let client = AgentClient()
        let model = DashboardViewModel(client: client, authManager: AuthManager())
        model.startPolling()
        try await eventually { client.pending.count == 1 }
        client.answer(25_000_000)
        try await eventually { model.isTrafficAvailable }
        precondition(model.speed.downloadBytesPerSec == 25_000_000)
        precondition(model.lastUpdated == nil) // Slow dashboard still waiting.
        precondition(!client.paths.contains("/api/network/speed"))
        let requestsBefore = client.paths.filter { $0 == "/api/network/speeds" }.count
        model.startPolling(interval: 0.5)
        let manualRefresh = Task { await model.refresh() }
        try await Task.sleep(for: .milliseconds(100))
        precondition(model.isTrafficAvailable, "General refresh must not reset the live traffic display")
        precondition(client.paths.filter { $0 == "/api/network/speeds" }.count == requestsBefore,
                     "Global refresh and manual refresh must not request additional traffic samples")
        manualRefresh.cancel()
        model.stopPolling()
        precondition(!model.isTrafficAvailable)

        model.startPolling()
        try await eventually { client.pending.count == 1 }
        model.stopPolling()
        model.startPolling()
        try await eventually { client.pending.count == 2 }
        client.answer(99_000_000) // Cancelled session must never overwrite the new one.
        try await Task.sleep(for: .milliseconds(50))
        precondition(!model.isTrafficAvailable)
        client.answer(5_000_000)
        try await eventually { model.isTrafficAvailable }
        precondition(model.speed.downloadBytesPerSec == 5_000_000)
        model.stopPolling()
        let fixed = try await measureCadence(.fixedInterval)
        let afterCompletion = try await measureCadence(.afterCompletion)
        precondition(afterCompletion - fixed > 0.2,
                     "Fixed cadence must subtract request duration instead of accumulating drift")
        print("Dashboard traffic independence, cancellation, and fixed-cadence checks passed")
    }
}
