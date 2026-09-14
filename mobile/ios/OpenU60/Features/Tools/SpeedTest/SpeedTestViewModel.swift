import SwiftUI

struct SpeedTestServer: Identifiable {
    let id: Int
    let name: String
    let country: String
    let sponsor: String
}

struct SpeedTestProgress {
    var phase: String = "idle"
    var progress: Int = 0
    var liveSpeedMbps: Double = 0
    var pingMs: Double?
    var jitterMs: Double?
    var downloadMbps: Double?
    var uploadMbps: Double?
    var downloadBytes: Int = 0
    var uploadBytes: Int = 0
    var server: String = ""
    var error: String?
}

@Observable
@MainActor
final class SpeedTestViewModel {
    var servers: [SpeedTestServer] = []
    var selectedServerId: Int?
    var progress: SpeedTestProgress = SpeedTestProgress()
    var isRunning: Bool = false
    var isLoading: Bool = false
    var message: String?
    var messageIsError: Bool = false

    private let client: AgentClient
    private let authManager: AuthManager
    private let pollLoop = PollingLoop(maxDelay: .seconds(5), maxDoublings: 2)
    private var pollFailures = 0
    private let maxPollFailures = 10

    /// Phases the device reports while a test is still in flight.
    private static let activePhases: Set<String> = ["latency", "download", "upload"]

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    func loadServers() async {
        isLoading = true
        do {
            let items = try await client.getJSONArray("/api/speedtest/servers")
            servers = items.compactMap { dict -> SpeedTestServer? in
                guard let id = (dict["id"] as? Int) ?? (dict["id"] as? String).flatMap({ Int($0) }),
                      let name = dict["name"] as? String,
                      let country = dict["country"] as? String,
                      let sponsor = dict["sponsor"] as? String else { return nil }
                return SpeedTestServer(id: id, name: name, country: country, sponsor: sponsor)
            }
            if selectedServerId == nil, let first = servers.first {
                selectedServerId = first.id
            }
        } catch {
            if !error.isCancellation {
                showMessage("Failed to load servers: \(error.localizedDescription)", isError: true)
            }
        }
        isLoading = false
    }

    func startTest() async {
        guard let serverId = selectedServerId else {
            showMessage("Select a server first", isError: true)
            return
        }

        isLoading = true
        message = nil
        progress = SpeedTestProgress()

        do {
            let _ = try await client.postJSON("/api/speedtest/start", body: ["server_id": serverId])
            isRunning = true
            showMessage("Speed test started", isError: false)
            startPolling()
        } catch {
            if !error.isCancellation {
                showMessage("Failed to start: \(error.localizedDescription)", isError: true)
            }
        }

        isLoading = false
    }

    func stopTest() async {
        stopPolling()
        isRunning = false

        do {
            let _ = try await client.postJSON("/api/speedtest/stop")
            showMessage("Speed test stopped", isError: false)
        } catch {
            if !error.isCancellation {
                showMessage("Failed to stop: \(error.localizedDescription)", isError: true)
            }
        }
    }

    /// Re-attaches to a test the device is still running, so leaving and returning to the screen
    /// doesn't orphan it. A test that reached a terminal phase while the screen was away is
    /// settled here instead, so its result isn't lost.
    func resumeIfRunning() async {
        guard !pollLoop.isRunning else { return }
        guard let data = try? await client.getJSON("/api/speedtest/progress") else { return }
        guard let phase = data["phase"] as? String else { return }
        // A phase the device isn't actively working on is only ours to adopt if we believe a
        // test of ours is in flight; otherwise it belongs to a run from before this screen.
        guard Self.activePhases.contains(phase) || isRunning else { return }

        apply(data)
        guard !settleIfFinished() else { return }
        isRunning = true
        startPolling()
    }

    // MARK: - Polling

    private func startPolling() {
        pollFailures = 0
        pollLoop.start(interval: .seconds(1)) { [weak self] in
            guard let self else { return .failure }
            return await self.pollProgress()
        }
    }

    /// Stops progress polling without stopping the test on the device.
    func stopPolling() {
        pollLoop.stop()
    }

    private func pollProgress() async -> PollingLoop.Outcome {
        let data: [String: Any]
        do {
            data = try await client.getJSON("/api/speedtest/progress")
        } catch {
            if error.isCancellation { return .success }
            pollFailures += 1
            if pollFailures >= maxPollFailures {
                isRunning = false
                stopPolling()
                showMessage("Lost connection to device", isError: true)
            }
            return .failure
        }

        pollFailures = 0
        apply(data)
        settleIfFinished()

        return .success
    }

    /// Applies the end-of-test side effects when `progress.phase` is terminal.
    /// - Returns: whether the test is finished.
    @discardableResult
    private func settleIfFinished() -> Bool {
        switch progress.phase {
        case "complete":
            showMessage("Speed test complete", isError: false)
        case "error":
            showMessage(progress.error ?? "Speed test failed", isError: true)
        case "cancelled":
            showMessage("Speed test cancelled", isError: false)
        default:
            return false
        }

        isRunning = false
        stopPolling()
        return true
    }

    private func apply(_ data: [String: Any]) {
        progress.phase = data["phase"] as? String ?? progress.phase
        progress.progress = data["progress"] as? Int ?? progress.progress
        progress.liveSpeedMbps = data["live_speed_mbps"] as? Double ?? progress.liveSpeedMbps
        progress.pingMs = data["ping_ms"] as? Double
        progress.jitterMs = data["jitter_ms"] as? Double
        progress.downloadMbps = data["download_mbps"] as? Double
        progress.uploadMbps = data["upload_mbps"] as? Double
        progress.downloadBytes = data["download_bytes"] as? Int ?? progress.downloadBytes
        progress.uploadBytes = data["upload_bytes"] as? Int ?? progress.uploadBytes
        progress.server = data["server"] as? String ?? progress.server
        progress.error = data["error"] as? String
    }

    private func showMessage(_ text: String, isError: Bool) {
        message = text
        messageIsError = isError
    }
}
