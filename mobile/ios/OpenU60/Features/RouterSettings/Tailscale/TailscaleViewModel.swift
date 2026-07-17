import SwiftUI

enum TailscaleSeverity {
    case neutral, good, warning, critical

    var color: Color {
        switch self {
        case .neutral: Color.secondary
        case .good: Color.green
        case .warning: Color.orange
        case .critical: Color.red
        }
    }
}

enum TailscalePhase {
    case notInstalled
    case updating
    case notSetUp
    case turnedOff
    case turningOff
    case loggingOut
    case starting
    case enrolling
    case verifyingEnrollment
    case awaitingBrowserLogin
    case awaitingSetup
    case needsLogin
    case pendingApproval
    case paused
    case healthy
    case degraded
    case unknown

    var label: String {
        switch self {
        case .notInstalled: "Not installed"
        case .updating: "Updating"
        case .notSetUp: "Not set up"
        case .turnedOff: "Off"
        case .turningOff: "Turning off…"
        case .loggingOut: "Logging out…"
        case .starting: "Starting…"
        case .enrolling: "Setting up…"
        case .verifyingEnrollment: "Finishing setup…"
        case .awaitingBrowserLogin: "Waiting for sign-in"
        case .awaitingSetup: "Not signed in"
        case .needsLogin: "Sign-in required"
        case .pendingApproval: "Waiting for approval"
        case .paused: "Reconnecting…"
        case .healthy: "Connected"
        case .degraded: "Connection problem"
        case .unknown: "Unknown"
        }
    }

    var severity: TailscaleSeverity {
        switch self {
        case .needsLogin: .critical
        case .pendingApproval, .degraded: .warning
        case .healthy: .good
        default: .neutral
        }
    }

    var showsSpinner: Bool {
        switch self {
        case .updating, .turningOff, .loggingOut, .starting, .enrolling,
             .verifyingEnrollment, .awaitingBrowserLogin, .pendingApproval,
             .paused, .degraded:
            true
        default:
            false
        }
    }
}

/// Client-side operations the server state cannot express on its own.
enum TailscaleOp {
    case none
    case enabling
    case prewarming
    case enrolling
    case verifyingEnrollment
    case awaitingBrowserLogin
    case disabling
    case loggingOut
    case updating
    case savingConfig
}

/// Config transitions dangerous enough to confirm before sending.
enum TailscaleConfigConfirm: Identifiable {
    case ssh
    case route(String)
    case wakelock

    var id: String {
        switch self {
        case .ssh: "ssh"
        case .route(let cidr): "route:\(cidr)"
        case .wakelock: "wakelock"
        }
    }

    var title: String {
        switch self {
        case .ssh: "Allow Tailscale SSH?"
        case .route(let cidr): "Advertise \(cidr)?"
        case .wakelock: "Let the router sleep?"
        }
    }

    var message: String {
        switch self {
        case .ssh:
            "Devices on your tailnet that your ACLs permit will be able to open a shell on this router. Access is governed by your tailnet ACLs, which this app can't show you."
        case .route:
            "This exposes that entire subnet — every device on your LAN — to your tailnet. Subnet routes must also be approved in the Tailscale admin console before they carry traffic."
        case .wakelock:
            "Without the wakelock the router's CPU sleeps and the node becomes unreachable, even though it will still look connected. Remote access will be unreliable."
        }
    }

    var confirmLabel: String {
        switch self {
        case .ssh: "Allow SSH"
        case .route: "Advertise"
        case .wakelock: "Allow Sleep"
        }
    }
}

@Observable
@MainActor
final class TailscaleViewModel {
    var status: TailscaleStatus = .empty
    var isLoading: Bool = false
    var message: String?
    var messageIsError: Bool = false

    var op: TailscaleOp = .none

    // Config edit buffers. Seeded on first load and after a successful apply
    // only — never from a poll, which would erase typing mid-edit.
    var editHostname: String = ""
    var editRoutes: String = ""
    var editSSH: Bool = false
    var editWakelock: Bool = true

    /// Never persisted, never logged, cleared on every outcome.
    var authKeyInput: String = ""

    // Blocking alerts
    var passwordGateBlocked: Bool = false
    var passwordGateMessage: String?
    var logoutWarning: String?

    // Sheets and confirms
    var showEnrollSheet: Bool = false
    var showUpdateSheet: Bool = false
    var showDisableConfirm: Bool = false
    var showLogoutPasswordConfirm: Bool = false
    /// Owned by the alert so a button tap can't leave it re-presenting itself.
    var showConfigConfirm: Bool = false
    var pendingConfigConfirm: TailscaleConfigConfirm?

    // Release lookup (pkgs.tailscale.com)
    var releaseFetching: Bool = false
    var releaseVersion: String?
    var releaseSHA256: String?
    var releaseError: String?
    var advancedVersion: String = ""
    var advancedSHA256: String = ""

    /// The interactive login URL only ever lives in the daemon's memory — the
    /// agent clears it the moment the backend reaches Running. Never cache it
    /// beyond the flow.
    var loginURL: String?

    private let client: AgentClient
    private let authManager: AuthManager
    private nonisolated(unsafe) var pollTask: Task<Void, Never>?
    private var initialLoadDone = false
    private var degradedPolls = 0
    private var pollFailures = 0
    private var requestedUpdateVersion: String?
    private var acknowledgedConfirms: Set<String> = []

    private let maxPollFailures = 6

    /// The circuit breaker has fired. An op's cap copy must not overwrite
    /// "Lost connection to the router." — when the router stopped answering at
    /// all, that is the real story, and the cap copy sends the user chasing the
    /// wrong thing.
    private var connectionLost: Bool { pollFailures >= maxPollFailures }

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    deinit {
        pollTask?.cancel()
    }

    // MARK: - Phase

    var phase: TailscalePhase { Self.derivePhase(status, op) }

    /// `state` alone is insufficient: build_status returns "disabled" before any
    /// CLI call, so a fresh router and a deliberately turned-off one report the
    /// same state and are told apart only by `enrolled`.
    static func derivePhase(_ status: TailscaleStatus, _ op: TailscaleOp) -> TailscalePhase {
        if op == .updating || status.updating || status.state == "updating" { return .updating }
        if status.state == "not_installed" { return .notInstalled }

        if op == .enrolling { return .enrolling }
        if op == .verifyingEnrollment { return .verifyingEnrollment }
        if op == .awaitingBrowserLogin,
           status.state != "healthy", status.state != "pending_approval" {
            return .awaitingBrowserLogin
        }
        if op == .prewarming || op == .enabling { return .starting }
        if op == .disabling { return .turningOff }
        if op == .loggingOut { return .loggingOut }

        switch status.state {
        case "disabled": return status.enrolled ? .turnedOff : .notSetUp
        // paused/respawning with enabled==false is the POST /disable race: the
        // daemon is SIGTERMed but child_pid is cleared later by the supervisor.
        case "paused": return status.enabled ? .paused : .turningOff
        case "respawning": return status.enabled ? .starting : .turningOff
        case "starting": return .starting
        case "connecting": return .starting
        case "awaiting_setup": return .awaitingSetup
        case "needs_login": return .needsLogin
        case "pending_approval": return .pendingApproval
        case "healthy": return .healthy
        case "degraded": return .degraded
        default: return .unknown
        }
    }

    var statusDescription: String {
        switch phase {
        case .notInstalled:
            return "Tailscale isn't installed on this router yet."
        case .updating:
            return "Installing Tailscale. This can take several minutes over mobile data. Don't power off the router."
        case .notSetUp:
            return "Connect this router to your tailnet to reach it from anywhere."
        case .turnedOff:
            return "Remote access is off. This router's sign-in is saved."
        case .turningOff:
            return "Disconnecting from your tailnet."
        case .loggingOut:
            return "Removing this router's Tailscale identity."
        case .starting:
            return "Waiting for the router's clock and mobile data. This can take up to 2 minutes."
        case .enrolling:
            return "Connecting this router to your tailnet. This can take up to 2 minutes."
        case .verifyingEnrollment:
            return "The router is still working. Checking whether setup completed."
        case .awaitingBrowserLogin:
            return "Open the sign-in page and approve this router. This link is valid for about 5 minutes."
        case .awaitingSetup:
            return "Tailscale is running but this router hasn't joined a tailnet yet."
        case .needsLogin:
            return status.expired
                ? "This router's node key expired. Sign in again to restore remote access."
                : "This router's sign-in is no longer valid. Sign in again to restore remote access."
        case .pendingApproval:
            return "Approve this router in the Tailscale admin console. Nothing can be done from this app."
        case .paused:
            return "Tailscale is reconnecting. This usually clears within a minute."
        case .healthy:
            return status.displayDNSName ?? "This router is on your tailnet."
        case .degraded:
            return degradedIsPersistent
                ? "The router is signed in but still not reachable."
                : "The router is signed in but not reachable. Recovering automatically."
        case .unknown:
            return "The router reported a state this app doesn't recognize."
        }
    }

    /// The agent's monitor loop self-heals a degraded node, so the first few
    /// polls offer no action.
    var degradedIsPersistent: Bool { degradedPolls > 4 }

    // MARK: - Derived UI state

    var actionsDisabled: Bool { isLoading || op != .none || phase == .updating }

    var enrollDisabled: Bool { actionsDisabled || passwordGateBlocked }

    var hostnameError: String? { TailscaleValidator.hostname(editHostname) }

    var routesError: String? { TailscaleValidator.routes(editRoutes) }

    /// Only the changed fields. `/config` is schedulable and a web dashboard
    /// exists, so a full-state write could silently revert a concurrent change.
    var configDiff: [String: Any] {
        var patch: [String: Any] = [:]
        let hostname = editHostname.trimmingCharacters(in: .whitespaces)
        if hostname != status.hostname { patch["hostname"] = hostname }
        let routes = TailscaleValidator.parseRoutes(editRoutes)
        if routes != status.advertiseRoutes { patch["advertise_routes"] = routes }
        if editWakelock != status.holdWakelock { patch["hold_wakelock"] = editWakelock }
        if editSSH != status.tailscaleSSH { patch["tailscale_ssh"] = editSSH }
        return patch
    }

    var canApplyConfig: Bool {
        !actionsDisabled && hostnameError == nil && routesError == nil && !configDiff.isEmpty
    }

    /// Manual Advanced input wins over the fetched release.
    var effectiveVersion: String {
        let manual = advancedVersion.trimmingCharacters(in: .whitespaces)
        return manual.isEmpty ? (releaseVersion ?? "") : manual
    }

    var effectiveSHA256: String {
        let manual = advancedSHA256.trimmingCharacters(in: .whitespaces)
        return manual.isEmpty ? (releaseSHA256 ?? "") : manual
    }

    var advancedVersionError: String? {
        advancedVersion.trimmingCharacters(in: .whitespaces).isEmpty
            ? nil : TailscaleValidator.version(advancedVersion)
    }

    var advancedSHA256Error: String? {
        advancedSHA256.trimmingCharacters(in: .whitespaces).isEmpty
            ? nil : TailscaleValidator.sha256(advancedSHA256)
    }

    var canStartUpdate: Bool {
        !isLoading && op == .none && !releaseFetching
            && TailscaleValidator.version(effectiveVersion) == nil
            && TailscaleValidator.sha256(effectiveSHA256) == nil
    }

    /// Heuristic, and it fails open: it misses a tailnet-side reverse proxy, a
    /// custom DNS alias, or a subnet route to the LAN address riding the tunnel.
    /// It supplements the always-on wording in destructive confirms.
    var isRemoteSession: Bool {
        guard let host = URLComponents(string: client.baseURL)?.host else { return false }
        if status.tailscaleIPs.contains(host) { return true }
        if let dns = status.displayDNSName, host.caseInsensitiveCompare(dns) == .orderedSame { return true }
        // 100.64.0.0/10
        let octets = host.split(separator: ".")
        guard octets.count == 4, let first = Int(octets[0]), let second = Int(octets[1]) else { return false }
        return first == 100 && (64...127).contains(second)
    }

    // MARK: - Loading

    func refresh() async {
        isLoading = true
        message = nil
        await fetchStatus(reportFailure: true)
        isLoading = false
        // The breaker stops the poll loop permanently, and that loop is the only
        // thing that reconciles an op it cannot see the end of (.updating), so an
        // explicit refresh has to revive it. startPolling() is idempotent.
        startPolling()
    }

    private func startPolling() {
        stopPolling()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let interval = self?.pollInterval else { return }
                try? await Task.sleep(for: .seconds(interval))
                if Task.isCancelled { return }
                await self?.pollTick()
            }
        }
    }

    private func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }

    /// The agent serves a 10s status cache, so a faster poll returns the
    /// identical object and progress looks frozen.
    private var pollInterval: TimeInterval {
        if op != .none { return 10 }
        switch phase {
        case .updating, .starting, .paused, .turningOff, .loggingOut, .enrolling,
             .verifyingEnrollment, .awaitingBrowserLogin, .pendingApproval,
             .degraded, .unknown:
            return 10
        case .healthy, .turnedOff, .notSetUp, .notInstalled, .awaitingSetup, .needsLogin:
            return 30
        }
    }

    private func pollTick() async {
        // A multi-minute update outlives the screen, so the poll loop — not a
        // task held across the push — is what reconciles it.
        let wasUpdating = op == .updating
        await fetchStatus()
        // A tick this loop no longer owns must not act: startPolling() cancels the
        // previous task, and finishUpdate() here would settle an op it can't see.
        if Task.isCancelled || connectionLost { return }
        if wasUpdating && !status.updating { finishUpdate() }
    }

    /// The poll loop stays silent on failure — the circuit breaker speaks for it,
    /// and a screen that times out by design would otherwise flash errors. The
    /// breaker lives here rather than in pollTick so it outranks `reportFailure`
    /// on every path, refresh() included.
    private func fetchStatus(reportFailure: Bool = false) async {
        do {
            let data = try await withReauth { try await self.client.getJSON("/api/tailscale/status") }
            adopt(TailscaleParser.parse(data))
            pollFailures = 0
        } catch {
            // Cancelling the poll loop must not read as a router failure.
            if isCancellation(error) { return }
            pollFailures += 1
            if connectionLost {
                stopPolling()
                showMessage("Lost connection to the router.", isError: true)
            } else if reportFailure {
                showMessage("Failed to load Tailscale status: \(error.localizedDescription)", isError: true)
            }
        }
    }

    /// URLSession surfaces task cancellation as URLError.cancelled, which AgentClient
    /// wraps as .networkError — indistinguishable from a real fault without this.
    private func isCancellation(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        if (error as? URLError)?.code == .cancelled { return true }
        if case AgentError.networkError(let underlying) = error {
            return (underlying as? URLError)?.code == .cancelled
        }
        return false
    }

    private func adopt(_ next: TailscaleStatus) {
        status = next
        if !initialLoadDone {
            initialLoadDone = true
            seedEditBuffers()
            if next.updating { op = .updating }
        }
        degradedPolls = phase == .degraded ? degradedPolls + 1 : 0
    }

    private func seedEditBuffers() {
        editHostname = status.hostname
        editRoutes = status.advertiseRoutes.joined(separator: ", ")
        editSSH = status.tailscaleSSH
        editWakelock = status.holdWakelock
    }

    // MARK: - Enrollment

    /// Never call /setup or /login-url against a cold daemon: ensure_daemon_ready
    /// runs inside the request and costs up to 180s, which turns /setup into a
    /// ~278s block. Pre-warming also surfaces the password gate before the user
    /// pastes a secret, and the needs_login re-auth path 409s without it.
    /// Returns false when the caller must abort.
    private func preWarm() async -> Bool {
        if status.enabled { return true }

        op = .prewarming
        guard await postEnable() else {
            if op == .prewarming { op = .none }
            return false
        }
        showMessage("Starting Tailscale… this can take up to 2 minutes on first run.", isError: false)

        let warm: Set<String> = ["awaiting_setup", "needs_login", "connecting", "paused",
                                 "healthy", "degraded", "pending_approval"]
        let deadline = Date().addingTimeInterval(180)
        while Date() < deadline {
            try? await Task.sleep(for: .seconds(10))
            await fetchStatus()
            if status.state == "healthy" {
                op = .none
                message = nil
                showEnrollSheet = false
                return false
            }
            if warm.contains(status.state) {
                op = .none
                return true
            }
        }
        op = .none
        if !connectionLost {
            showMessage("Tailscale isn't starting. Check that the router has mobile data and its clock is set.", isError: true)
        }
        return false
    }

    func setupWithAuthKey() async {
        // The agent's mutual exclusion is one-directional — /setup does not
        // block /login-url — so the client must serialize enrollment itself.
        guard op == .none else { return }
        let key = authKeyInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !key.isEmpty else { return }
        guard await preWarm() else { return }

        authKeyInput = ""
        // Pre-warm's progress copy — and any error from a previous attempt — is
        // spent. The sheet renders `.enrolling` from `op` alone.
        message = nil
        op = .enrolling
        do {
            let data = try await withReauth {
                try await self.client.postJSONSlow("/api/tailscale/setup", body: ["auth_key": key])
            }
            op = .none
            // The 200 body IS the refreshed status payload — no follow-up GET.
            adopt(TailscaleParser.parse(data))
            showEnrollSheet = false
            if status.state == "pending_approval" {
                showMessage("Setup completed. Approve this router in your Tailscale admin console.", isError: false)
            } else {
                showMessage("This router is now on your tailnet.", isError: false)
            }
        } catch {
            // A client timeout is not a failure: `up` has a 90s hard kill and is
            // very likely still succeeding. Never re-send the key — one-off keys
            // die on use and a retry hits 409.
            if isTimeout(error) {
                await verifyEnrollment()
                return
            }
            op = .none
            guard let api = apiError(error) else {
                showMessage("Failed: \(error.localizedDescription)", isError: true)
                return
            }
            switch api.status {
            case 403:
                blockOnPasswordGate(api.message)
            case 502:
                showMessage("Setup failed: \(api.message). Enter a fresh auth key — one-off keys can't be reused.", isError: true)
            default:
                showMessage(api.message, isError: true)
            }
        }
    }

    private func verifyEnrollment() async {
        op = .verifyingEnrollment
        let deadline = Date().addingTimeInterval(240)
        while Date() < deadline {
            try? await Task.sleep(for: .seconds(10))
            await fetchStatus()
            if status.state == "healthy" {
                op = .none
                showEnrollSheet = false
                showMessage("This router is now on your tailnet.", isError: false)
                return
            }
            if status.state == "pending_approval" {
                op = .none
                showEnrollSheet = false
                showMessage("Setup completed. Approve this router in your Tailscale admin console.", isError: false)
                return
            }
        }
        op = .none
        if !connectionLost {
            showMessage("Setup didn't complete. Enter a fresh auth key — one-off keys can't be reused.", isError: true)
        }
    }

    func signInWithBrowser() async {
        guard op == .none else { return }
        guard await preWarm() else { return }

        authKeyInput = ""
        loginURL = nil
        op = .awaitingBrowserLogin
        showEnrollSheet = false
        message = nil

        do {
            let data = try await withReauth { try await self.client.postJSONSlow("/api/tailscale/login-url") }
            // 202 and 200 are indistinguishable here — both collapse into the
            // client's 200..299 success arm — so branch on the payload.
            if let url = data["login_url"] as? String, !url.isEmpty { loginURL = url }
        } catch {
            if !isTimeout(error) {
                op = .none
                if let api = apiError(error), api.status == 403 {
                    blockOnPasswordGate(api.message)
                } else if let api = apiError(error) {
                    showMessage(api.message, isError: true)
                } else {
                    showMessage("Failed: \(error.localizedDescription)", isError: true)
                }
                return
            }
        }

        let deadline = Date().addingTimeInterval(310)
        while Date() < deadline {
            guard op == .awaitingBrowserLogin else { return }
            try? await Task.sleep(for: .seconds(10))
            await fetchStatus()
            // Mirror the field, never latch it: the agent drops the URL the moment
            // the backend reaches Running and whenever the daemon exits, and a
            // consumed URL must stop being tappable. The agent's own cache seeds
            // every payload, so a still-live URL is never dropped here.
            loginURL = status.loginURL.flatMap { $0.isEmpty ? nil : $0 }
            if status.state == "healthy" {
                finishBrowserLogin("This router is now on your tailnet.")
                return
            }
            if status.state == "pending_approval" {
                finishBrowserLogin("Setup completed. Approve this router in your Tailscale admin console.")
                return
            }
        }
        op = .none
        loginURL = nil
        if !connectionLost {
            showMessage("Sign-in didn't complete in time. Try again.", isError: true)
        }
    }

    private func finishBrowserLogin(_ text: String) {
        op = .none
        loginURL = nil
        showEnrollSheet = false
        showMessage(text, isError: false)
    }

    // MARK: - Enable / disable

    /// POST /api/tailscale/enable. Returns false when the caller must abort.
    /// A 500 here is never permanent: the same "update in progress" condition
    /// yields 409 on setup/login-url/logout but 500 here, so reconcile from
    /// /status rather than matching the string.
    private func postEnable() async -> Bool {
        do {
            let _ = try await withReauth { try await self.client.postJSON("/api/tailscale/enable") }
            passwordGateBlocked = false
            passwordGateMessage = nil
            return true
        } catch {
            guard let api = apiError(error) else {
                showMessage("Failed: \(error.localizedDescription)", isError: true)
                return false
            }
            switch api.status {
            case 403:
                blockOnPasswordGate(api.message)
            case 500:
                await fetchStatus()
                if status.updating {
                    op = .updating
                } else {
                    showMessage(api.message, isError: true)
                }
            default:
                showMessage(api.message, isError: true)
            }
            return false
        }
    }

    func enableRemoteAccess() async {
        guard op == .none else { return }
        op = .enabling
        guard await postEnable() else {
            if op == .enabling { op = .none }
            return
        }
        showMessage("Turning on remote access…", isError: false)

        // `respawning` for ~2 min and a transient `paused` are both expected
        // here — the supervisor gates on a sane clock and a connected WAN, and
        // the monitor loop self-heals a persisted WantRunning=false.
        let terminal: Set<String> = ["healthy", "degraded", "needs_login", "awaiting_setup", "pending_approval"]
        let deadline = Date().addingTimeInterval(180)
        while Date() < deadline {
            try? await Task.sleep(for: .seconds(10))
            await fetchStatus()
            if terminal.contains(status.state) {
                op = .none
                // The status card carries the outcome from here. Leaving the
                // progress banner up would contradict it.
                message = nil
                return
            }
        }
        op = .none
        if !connectionLost {
            showMessage("Still starting — check back shortly.", isError: false)
        }
    }

    func disable() async {
        guard op == .none else { return }
        showDisableConfirm = false
        op = .disabling
        do {
            let _ = try await withReauth { try await self.client.postJSONSlow("/api/tailscale/disable") }
            showMessage("Remote access is off.", isError: false)
        } catch {
            if !isTimeout(error) {
                op = .none
                if let api = apiError(error) {
                    showMessage(api.message, isError: true)
                } else {
                    showMessage("Failed: \(error.localizedDescription)", isError: true)
                }
                return
            }
        }

        // kill_child only SIGTERMs and child_pid is cleared by the supervisor
        // afterwards, so /status reports daemon_running:true for a moment. The
        // .turningOff phase absorbs exactly that — never render it as a failure.
        let deadline = Date().addingTimeInterval(30)
        while Date() < deadline {
            try? await Task.sleep(for: .seconds(10))
            await fetchStatus()
            if status.state == "disabled" { break }
        }
        op = .none
    }

    // MARK: - Config

    func applyConfig() async {
        guard op == .none, hostnameError == nil, routesError == nil else { return }
        let patch = configDiff
        guard !patch.isEmpty else { return }

        if let confirm = nextConfirm(for: patch) {
            pendingConfigConfirm = confirm
            showConfigConfirm = true
            return
        }
        await sendConfig(patch)
    }

    func confirmPendingConfig() async {
        guard let confirm = pendingConfigConfirm else { return }
        acknowledgedConfirms.insert(confirm.id)
        pendingConfigConfirm = nil
        await applyConfig()
    }

    /// Cancelling abandons the whole apply, so the acknowledgements go too.
    func cancelPendingConfig() {
        pendingConfigConfirm = nil
        acknowledgedConfirms.removeAll()
    }

    private func nextConfirm(for patch: [String: Any]) -> TailscaleConfigConfirm? {
        if patch["tailscale_ssh"] as? Bool == true, !acknowledgedConfirms.contains("ssh") {
            return .ssh
        }
        if let routes = patch["advertise_routes"] as? [String] {
            for route in routes
            where !status.advertiseRoutes.contains(route) && !acknowledgedConfirms.contains("route:\(route)") {
                return .route(route)
            }
        }
        if patch["hold_wakelock"] as? Bool == false, !acknowledgedConfirms.contains("wakelock") {
            return .wakelock
        }
        return nil
    }

    private func sendConfig(_ patch: [String: Any]) async {
        op = .savingConfig
        isLoading = true
        defer {
            isLoading = false
            op = .none
        }
        do {
            // Real JSON types: TsConfigPatch is native serde. The "1"/"0" strings
            // the neighbouring ubus screens send would fail to deserialize here.
            let data = try await withReauth {
                try await self.client.putJSONSlow("/api/tailscale/config", body: patch)
            }
            adopt(TailscaleParser.parse(data))
            seedEditBuffers()
            acknowledgedConfirms.removeAll()
            // The agent only shells `tailscale set` when enabled && daemon_up &&
            // the state file exists; otherwise it silently persists the patch for
            // later. Both return 200, so approximate rather than claim "applied".
            if status.enabled && status.daemonRunning && status.enrolled {
                showMessage("Settings applied.", isError: false)
            } else {
                showMessage("Saved. These will apply the next time Tailscale starts.", isError: false)
            }
        } catch {
            if let api = apiError(error), api.status == 502 {
                // The endpoint is transactional — the patch is applied to a
                // candidate copy and nothing persists on failure.
                showMessage("Tailscale rejected the change. Nothing was saved.", isError: true)
                await fetchStatus()
                seedEditBuffers()
            } else if let api = apiError(error) {
                showMessage(api.message, isError: true)
            } else {
                showMessage("Failed: \(error.localizedDescription)", isError: true)
            }
        }
    }

    // MARK: - Logout

    func logout() async {
        guard op == .none else { return }
        op = .loggingOut
        do {
            let data = try await withReauth { try await self.client.postJSONSlow("/api/tailscale/logout") }
            op = .none
            // The warning is the ONLY failure channel — logout returns 200 even
            // when the node key was not invalidated, which leaves a live machine
            // on the tailnet while the local identity is already destroyed.
            if let warning = data["warning"] as? String, !warning.isEmpty {
                logoutWarning = warning
            } else {
                showMessage("Logged out. This router has been removed from your tailnet.", isError: false)
            }
        } catch {
            op = .none
            if isTimeout(error) {
                // The local identity is destroyed unconditionally, so this is not
                // a failure — but we never saw `warning`, so we cannot claim the
                // node key was invalidated.
                showMessage("Logged out. If this router still appears in your admin console, remove it there.", isError: false)
            } else if let api = apiError(error), api.status == 409 {
                showMessage("Can't log out while an update is running.", isError: true)
            } else if let api = apiError(error) {
                showMessage(api.message, isError: true)
                return
            } else {
                showMessage("Failed: \(error.localizedDescription)", isError: true)
                return
            }
        }
        await fetchStatus()
        seedEditBuffers()
    }

    func verifyPassword(_ input: String) -> Bool {
        guard let stored = KeychainHelper.load(key: "router_password") else { return false }
        return input == stored
    }

    func dismissLogoutWarning() {
        logoutWarning = nil
    }

    func copyToClipboard(_ value: String) {
        UIPasteboard.general.string = value
        showMessage("Copied to clipboard", isError: false)
    }

    // MARK: - Install / update

    func loadLatestRelease() async {
        releaseFetching = true
        releaseError = nil
        do {
            let release = try await TailscaleReleaseFetcher.resolveLatest()
            releaseVersion = release.version
            releaseSHA256 = release.sha256
        } catch {
            releaseVersion = nil
            releaseSHA256 = nil
            releaseError = error.localizedDescription
        }
        releaseFetching = false
    }

    func startUpdate() async {
        guard op == .none else { return }
        let version = effectiveVersion
        let sha256 = effectiveSHA256
        guard TailscaleValidator.version(version) == nil,
              TailscaleValidator.sha256(sha256) == nil else { return }

        isLoading = true
        defer { isLoading = false }
        do {
            // Default client — the handler returns 202 immediately.
            let _ = try await withReauth {
                try await self.client.postJSON("/api/tailscale/update", body: ["version": version, "sha256": sha256])
            }
            beginUpdate(requested: version)
        } catch {
            guard let api = apiError(error) else {
                showMessage("Failed: \(error.localizedDescription)", isError: true)
                return
            }
            if api.status == 409 {
                // Reconcile from status rather than erroring — match on endpoint
                // and status, never on the string.
                await fetchStatus()
                if status.updating {
                    beginUpdate(requested: nil)
                } else {
                    showMessage(api.message, isError: true)
                }
            } else {
                showMessage(api.message, isError: true)
            }
        }
    }

    private func beginUpdate(requested: String?) {
        requestedUpdateVersion = requested
        op = .updating
        showUpdateSheet = false
        showMessage("Updating Tailscale. This can take several minutes. Don't power off the router.", isError: false)
    }

    private func finishUpdate() {
        op = .none
        let requested = requestedUpdateVersion
        requestedUpdateVersion = nil

        // last_update is in-memory only and is NOT persisted, so an agent restart
        // during the update leaves it null. That is an unknown outcome, never a
        // success.
        guard let result = status.lastUpdate else {
            showMessage("Update finished, but the result is unknown (the agent may have restarted). Check the running version below.", isError: true)
            return
        }
        guard result.ok else {
            showMessage("Update failed: \(result.error ?? "Unknown error")", isError: true)
            return
        }
        if let requested, status.pinnedVersion != requested {
            showMessage("Update reported success but the version didn't change.", isError: true)
        } else {
            showMessage("Updated to \(result.version).", isError: false)
        }
    }

    // MARK: - Helpers

    /// Retries once after a silent re-auth, and only on the typed 401. This
    /// screen long-polls across the 3600s token TTL, but it also times out by
    /// design — the Dashboard idiom of re-authing on any error would spray
    /// spurious logins. A 403 must never reach this: it is not an auth failure.
    private func withReauth<T>(_ call: () async throws -> T) async throws -> T {
        do {
            return try await call()
        } catch AgentError.unauthorized {
            guard await authManager.reauthenticate() else { throw AgentError.unauthorized }
            return try await call()
        }
    }

    private func apiError(_ error: Error) -> (status: Int, message: String)? {
        guard let agentError = error as? AgentError,
              case .apiError(let status, let message) = agentError else { return nil }
        return (status, message)
    }

    private func isTimeout(_ error: Error) -> Bool {
        guard let agentError = error as? AgentError, case .timeout = agentError else { return false }
        return true
    }

    private func blockOnPasswordGate(_ serverMessage: String) {
        passwordGateBlocked = true
        passwordGateMessage = serverMessage
        // The card is the gate's only explanation and it renders on the list
        // behind the enroll sheet. The gate is terminal and not fixable from the
        // API, so the sheet can do nothing but hide the reason its own buttons
        // just went dead — close it. Dismissal clears the pasted key too.
        message = nil
        showEnrollSheet = false
    }

    private func showMessage(_ text: String, isError: Bool) {
        message = text
        messageIsError = isError
    }
}
