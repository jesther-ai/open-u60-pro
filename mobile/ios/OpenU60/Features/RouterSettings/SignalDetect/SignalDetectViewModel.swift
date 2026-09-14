import SwiftUI

@Observable
@MainActor
final class SignalDetectViewModel {
    var status: SignalDetectStatus = .empty
    var isLoading: Bool = false
    var message: String?
    var messageIsError: Bool = false

    private let client: AgentClient
    private let authManager: AuthManager
    private let pollLoop = PollingLoop()

    /// The device can still be reporting the previous sweep's 100% for a moment after a new
    /// sweep starts. A 100% reading is only believed once the counter has been seen below 100
    /// or this much time has passed since the sweep was started.
    private static let staleProgressGrace: Duration = .seconds(6)
    private var sweepStart: ContinuousClock.Instant?
    private var sweepProgressConfirmed = false

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    func startDetection() async {
        isLoading = true
        message = nil
        status.results = []
        status.progress = 0

        do {
            let _ = try await client.postJSON("/api/cell/signal-detect/start")
            status.running = true
            beginSweepGrace()
            showMessage("Detection started", isError: false)
            startPolling()
        } catch {
            if !error.isCancellation {
                showMessage("Failed to start: \(error.localizedDescription)", isError: true)
            }
        }

        isLoading = false
    }

    func stopDetection() async {
        stopPolling()

        do {
            let _ = try await client.postJSON("/api/cell/signal-detect/stop")
            status.running = false
            showMessage("Detection stopped", isError: false)
            await fetchResults()
        } catch {
            if !error.isCancellation {
                showMessage("Failed to stop: \(error.localizedDescription)", isError: true)
            }
        }
    }

    /// Re-attaches to a sweep the device is still running, so leaving and returning to the
    /// screen doesn't orphan it. A sweep that finished while the screen was away is settled
    /// here instead, so the controls don't stay stuck on "in progress".
    func resumeIfRunning() async {
        guard !pollLoop.isRunning else { return }
        guard let data = try? await client.getJSON("/api/cell/signal-detect/progress") else { return }

        let progressStatus = SignalDetectParser.parseProgress(data)

        if progressStatus.progress >= 100 {
            // Only ours to adopt if we believe a sweep of ours was in flight; otherwise this
            // is a leftover reading from a run that predates this screen.
            guard status.running else { return }
            status.progress = progressStatus.progress
            status.running = false
            await fetchResults()
            showMessage("Detection complete", isError: false)
            return
        }

        guard progressStatus.running || status.running else { return }
        status.progress = progressStatus.progress
        status.running = true
        // The device is reporting this sweep's own counter, so no stale-progress grace needed.
        sweepProgressConfirmed = true
        startPolling()
    }

    func fetchResults() async {
        do {
            let data = try await client.getJSON("/api/cell/signal-detect/results")
            status.results = SignalDetectParser.parseResults(data)
        } catch {
            // Results may not be available yet
        }
    }

    // MARK: - Polling

    private func startPolling() {
        pollLoop.start(interval: .seconds(2)) { [weak self] in
            guard let self else { return .failure }
            return await self.pollProgress()
        }
    }

    /// Stops progress polling without stopping the sweep on the device.
    func stopPolling() {
        pollLoop.stop()
    }

    private func pollProgress() async -> PollingLoop.Outcome {
        let data: [String: Any]
        do {
            data = try await client.getJSON("/api/cell/signal-detect/progress")
        } catch {
            return error.isCancellation ? .success : .failure
        }

        let reported = SignalDetectParser.parseProgress(data).progress
        if reported < 100 { sweepProgressConfirmed = true }
        guard sweepProgressConfirmed || sweepGraceElapsed else { return .success }

        status.progress = reported
        if reported >= 100 {
            status.running = false
            stopPolling()
            await fetchResults()
            showMessage("Detection complete", isError: false)
        }
        return .success
    }

    // MARK: - Stale progress guard

    /// Opens the window during which a 100% reading is taken as the previous sweep's.
    private func beginSweepGrace() {
        sweepStart = ContinuousClock.now
        sweepProgressConfirmed = false
    }

    private var sweepGraceElapsed: Bool {
        guard let sweepStart else { return true }
        return ContinuousClock.now - sweepStart >= Self.staleProgressGrace
    }

    private func showMessage(_ text: String, isError: Bool) {
        message = text
        messageIsError = isError
    }
}
