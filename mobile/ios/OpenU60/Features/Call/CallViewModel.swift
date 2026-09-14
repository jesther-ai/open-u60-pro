import SwiftUI
import os

private let logger = Logger(subsystem: "com.zte.companion", category: "Call")

enum CallState: Equatable {
    case idle
    case dialing
    case alerting
    case active
    case incoming(from: String)
}

@Observable
@MainActor
final class CallViewModel {
    var phoneNumber: String = ""
    var callState: CallState = .idle
    var isMuted: Bool = false
    var callDuration: TimeInterval = 0
    var error: String?
    var showKeypad: Bool = false

    private let client: AgentClient
    private let authManager: AuthManager
    private let pollLoop = PollingLoop()
    private let durationLoop = PollingLoop()

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    // MARK: - Number Input

    func appendDigit(_ digit: String) {
        phoneNumber.append(digit)
        if callState == .active {
            Task { await sendDTMF(digit) }
        }
    }

    func deleteDigit() {
        guard !phoneNumber.isEmpty else { return }
        phoneNumber.removeLast()
    }

    // MARK: - Call Actions

    func dial() async {
        let number = phoneNumber.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !number.isEmpty else { return }
        error = nil
        callState = .dialing

        do {
            let _ = try await client.postJSON("/api/call/dial", body: ["number": number])
            // State will be updated by polling
            startPolling()
        } catch {
            callState = .idle
            guard !error.isCancellation else { return }
            self.error = error.localizedDescription
            // The POST can fail client-side (timeout) after the router already placed the
            // call, so keep polling and let the router's status decide the state.
            startPolling()
        }
    }

    func hangup() async {
        error = nil
        let _ = try? await client.postJSON("/api/call/hangup")
        callState = .idle
        isMuted = false
        stopDurationTimer()
        stopPolling()
    }

    func answer() async {
        error = nil
        let _ = try? await client.postJSON("/api/call/answer")
        startPolling()
    }

    func sendDTMF(_ digits: String) async {
        let _ = try? await client.postJSON("/api/call/dtmf", body: ["digits": digits])
    }

    func toggleMute() async {
        let newMuted = !isMuted
        do {
            let result = try await client.postJSON("/api/call/mute", body: ["enabled": newMuted])
            if let muted = result["muted"] as? Bool {
                isMuted = muted
            } else {
                isMuted = newMuted
            }
        } catch {
            // keep current state
        }
    }

    // MARK: - Polling

    func startPolling() {
        pollLoop.start(interval: .seconds(2)) { [weak self] in
            guard let self else { return .failure }
            return await self.pollCallStatus()
        }
    }

    /// Stops status polling without touching the call itself. Dismissing the screen
    /// leaves an active call up on the router, exactly as before.
    func stopPolling() {
        pollLoop.stop()
    }

    /// One-shot reconciliation when the screen appears.
    ///
    /// Closing the call screen leaves an active call up on the router but tears down this view
    /// model, so re-opening it would otherwise show an idle dial pad with no way to reach a call
    /// that is still connected. Poll once to find out, and only stay polling if there is something
    /// to watch — an idle dialer has no reason to hit the router every two seconds.
    func syncWithRouter() async {
        guard !pollLoop.isRunning else { return }
        _ = await pollCallStatus()
        if callState != .idle { startPolling() }
    }

    private func pollCallStatus() async -> PollingLoop.Outcome {
        let result: [String: Any]
        do {
            result = try await client.getJSON("/api/call/status")
        } catch {
            return error.isCancellation ? .success : .failure
        }

        guard let calls = result["calls"] as? [[String: Any]] else { return .success }

        if calls.isEmpty {
            if callState != .idle {
                callState = .idle
                isMuted = false
                stopDurationTimer()
            }
            return .success
        }

        guard let first = calls.first,
              let stat = first["stat"] as? String else { return .success }

        let number = first["number"] as? String ?? ""
        let dir = first["dir"] as? String ?? "mo"

        switch stat {
        case "dialing":
            callState = .dialing
        case "alerting":
            callState = .alerting
        case "active":
            if callState != .active {
                callState = .active
                startDurationTimer()
            }
        case "incoming", "waiting":
            callState = .incoming(from: number.isEmpty ? (dir == "mt" ? "Unknown" : number) : number)
        case "held":
            break // keep current state
        case "releasing":
            callState = .idle
            isMuted = false
            stopDurationTimer()
        default:
            break
        }

        return .success
    }

    // MARK: - Duration Timer

    private func startDurationTimer() {
        let start = Date()
        callDuration = 0
        durationLoop.start(interval: .seconds(1)) { [weak self] in
            guard let self else { return .failure }
            self.callDuration = Date().timeIntervalSince(start)
            return .success
        }
    }

    private func stopDurationTimer() {
        durationLoop.stop()
        callDuration = 0
    }
}
