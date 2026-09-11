import SwiftUI
import os

private let logger = Logger(subsystem: "com.zte.companion", category: "Signal")

@Observable
@MainActor
final class SignalMonitorViewModel {
    var nrSignal: NRSignal = .empty
    var lteSignal: LTESignal = .empty
    var wcdmaSignal: WCDMASignal = .empty
    var operatorInfo: OperatorInfo = .empty
    var history: [SignalSnapshot] = []
    var isLoading: Bool = false
    var lastUpdated: Date?
    var error: String?

    /// The live chart is the reason this screen exists, so it samples at the same cadence the
    /// dashboard does. The view stops the loop on disappear and while the app is backgrounded.
    nonisolated static let defaultPollInterval: TimeInterval = 2.0

    private let client: AgentClient
    private let authManager: AuthManager
    private let poller: PollingLoop

    /// The chart plots the newest 60 samples; older ones are dropped so a screen left open
    /// overnight does not grow the array without bound.
    private let maxHistoryPoints = 60

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
        self.poller = PollingLoop()
    }

    func startPolling(interval: TimeInterval = SignalMonitorViewModel.defaultPollInterval) {
        poller.start(interval: .seconds(interval)) { [weak self] in
            guard let self else { return .failure }
            return await self.refresh() ? .success : .failure
        }
    }

    func stopPolling() {
        poller.stop()
    }

    /// - Returns: true when the agent answered, so the poll loop can back off when it does not.
    @discardableResult
    func refresh() async -> Bool {
        do {
            let data = try await client.getJSON("/api/network/signal")
            let (nr, lte, wcdma, op) = SignalParser.parseNetInfo(data)
            if nr != nrSignal { nrSignal = nr }
            if lte != lteSignal { lteSignal = lte }
            if wcdma != wcdmaSignal { wcdmaSignal = wcdma }
            if op != operatorInfo { operatorInfo = op }

            history.append(SignalSnapshot(
                timestamp: Date(),
                nrRSRP: nr.rsrp,
                lteRSRP: lte.rsrp,
                wcdmaRSCP: wcdma.rscp
            ))
            if history.count > maxHistoryPoints {
                history.removeFirst(history.count - maxHistoryPoints)
            }

            if error != nil { error = nil }
            lastUpdated = Date()
            return true
        } catch {
            // A cancelled poll is not a failure: leave the last good reading and timestamp alone.
            guard !error.isCancellation else { return false }
            logger.error("refresh: \(error.localizedDescription)")
            self.error = error.localizedDescription
            lastUpdated = Date()
            return false
        }
    }
}
