import SwiftUI
import os

private let logger = Logger(subsystem: "com.zte.companion", category: "USBConnection")

@Observable
@MainActor
final class USBConnectionViewModel {
    var usbStatus: USBStatus = .empty
    var showModeSheet: Bool = false
    var isLoading: Bool = false
    var message: String?
    var messageIsError: Bool = false

    /// Cable-attach detection runs app-wide from every tab, and its only job is to beat the user
    /// to the sheet after they plug something in. A slow cadence keeps that useful while cutting
    /// the background chatter it costs.
    nonisolated static let defaultPollInterval: TimeInterval = 10

    private let client: AgentClient
    private let authManager: AuthManager
    private let poller: PollingLoop

    /// `nil` until the first successful poll. It survives poll restarts and scene phase changes,
    /// which is what keeps a backgrounded-then-resumed app from re-presenting the sheet over a
    /// cable that was never unplugged.
    private var lastCableAttached: Bool?

    /// A cable already attached when the first poll lands is worth one sheet — it is the main
    /// discovery path for powerbank mode — but only one, however many times that first poll is
    /// retried. Later plug-ins are genuine transitions and present on their own.
    private var hasPresentedForInitialState: Bool = false

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
        self.poller = PollingLoop()
    }

    func startPolling(interval: TimeInterval = USBConnectionViewModel.defaultPollInterval) {
        poller.start(interval: .seconds(interval)) { [weak self] in
            guard let self else { return .failure }
            let reachedAgent = await self.refresh()
            return reachedAgent ? .success : .failure
        }
    }

    func stopPolling() {
        poller.stop()
    }

    /// - Returns: true when the agent answered, so the poll loop can back off when it does not.
    @discardableResult
    func refresh() async -> Bool {
        async let usbData = fetchUSB()
        async let chargerData = fetchCharger()
        let (usb, charger) = await (usbData, chargerData)

        guard let usb else { return false }

        let status = DeviceParser.parseUSBStatus(usb, chargerData: charger)

        if status.cableAttached, shouldPresentSheet() {
            logger.debug("USB cable attached, presenting mode sheet")
            showModeSheet = true
        }
        lastCableAttached = status.cableAttached

        if status != usbStatus { usbStatus = status }
        return true
    }

    func enablePowerbank() async {
        isLoading = true
        message = nil
        do {
            let _ = try await client.putJSON("/api/usb/powerbank", body: ["state": 1])
            usbStatus.powerbankActive = true
            message = "Fast charging enabled"
            messageIsError = false
        } catch {
            message = "Failed: \(error.localizedDescription)"
            messageIsError = true
        }
        isLoading = false
    }

    func disablePowerbank() async {
        isLoading = true
        message = nil
        do {
            let _ = try await client.putJSON("/api/usb/powerbank", body: ["state": 0])
            usbStatus.powerbankActive = false
            message = "Fast charging disabled"
            messageIsError = false
        } catch {
            message = "Failed: \(error.localizedDescription)"
            messageIsError = true
        }
        isLoading = false
    }

    // MARK: - Sheet presentation

    /// Whether an observed attached cable warrants the sheet. Called only when the cable is
    /// attached, and before `lastCableAttached` is updated.
    private func shouldPresentSheet() -> Bool {
        guard let wasAttached = lastCableAttached else {
            // First reading of the session: the cable was already attached when the app came up,
            // which is the ordering the sheet is most useful for. Present it once and never again
            // from this branch, so a re-run of that first poll cannot re-open what was dismissed.
            if hasPresentedForInitialState { return false }
            hasPresentedForInitialState = true
            return true
        }
        return !wasAttached
    }

    // MARK: - Fetch

    private func fetchUSB() async -> [String: Any]? {
        try? await client.getJSON("/api/usb/status")
    }

    private func fetchCharger() async -> [String: Any]? {
        try? await client.getJSON("/api/device/charger")
    }
}
