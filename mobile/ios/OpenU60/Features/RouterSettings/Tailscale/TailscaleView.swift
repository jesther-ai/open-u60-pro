import SwiftUI

private let tailscaleAdminURL = URL(string: "https://login.tailscale.com/admin/machines")!
private let tailscaleKeysURL = URL(string: "https://login.tailscale.com/admin/settings/keys")!

private let remoteSessionParagraph = "You're connected to this router over Tailscale right now. This will drop your connection immediately, and you won't be able to reconnect remotely. Continue only if you have local network or physical access to the router."

struct TailscaleView: View {
    @Bindable var viewModel: TailscaleViewModel
    @Environment(\.openURL) private var openURL

    var body: some View {
        List {
            if let msg = viewModel.message {
                Section {
                    Text(msg)
                        .font(.subheadline)
                        .foregroundStyle(viewModel.messageIsError ? .red : .green)
                        .textSelection(.enabled)
                }
            }

            alertSections
            statusSection

            if viewModel.phase == .healthy || viewModel.phase == .degraded {
                connectionDetailsSection
                advisoriesSection
            }

            if viewModel.status.enrolled, viewModel.phase != .updating, viewModel.phase != .notInstalled {
                settingsSection
            }

            if viewModel.phase != .updating {
                maintenanceSection
            }

            if viewModel.status.enrolled {
                dangerZoneSection
            }
        }
        .navigationTitle("Tailscale")
        .refreshable { await viewModel.refresh() }
        .overlay {
            if viewModel.isLoading {
                ProgressView()
                    .padding()
                    .background(Color(.systemBackground).opacity(0.85), in: RoundedRectangle(cornerRadius: 8))
            }
        }
        .task { await viewModel.refresh() }
        .sheet(isPresented: $viewModel.showEnrollSheet, onDismiss: { viewModel.authKeyInput = "" }) {
            TailscaleEnrollSheet(viewModel: viewModel)
        }
        // Advanced input is sheet-scoped but lives on the VM, and it outranks the
        // fetched release — a cancelled edit would silently pin the next update to
        // a version the re-opened (collapsed) disclosure no longer shows.
        .sheet(isPresented: $viewModel.showUpdateSheet, onDismiss: {
            viewModel.advancedVersion = ""
            viewModel.advancedSHA256 = ""
        }) {
            TailscaleUpdateSheet(viewModel: viewModel)
        }
        .sheet(isPresented: $viewModel.showLogoutPasswordConfirm) {
            PasswordConfirmView(
                title: "Log out of Tailscale?",
                message: logoutMessage,
                confirmLabel: "Log Out"
            ) {
                await viewModel.logout()
            }
        }
        .alert("Turn off remote access?", isPresented: $viewModel.showDisableConfirm) {
            Button("Cancel", role: .cancel) {}
            Button("Turn Off", role: .destructive) { Task { await viewModel.disable() } }
        } message: {
            Text(disableMessage)
        }
        .alert(
            viewModel.pendingConfigConfirm?.title ?? "",
            isPresented: $viewModel.showConfigConfirm,
            presenting: viewModel.pendingConfigConfirm
        ) { confirm in
            Button("Cancel", role: .cancel) { viewModel.cancelPendingConfig() }
            Button(confirm.confirmLabel) { Task { await viewModel.confirmPendingConfig() } }
        } message: { confirm in
            Text(confirm.message)
        }
    }

    // MARK: - Blocking alerts

    @ViewBuilder
    private var alertSections: some View {
        if viewModel.passwordGateBlocked {
            Section {
                TailscaleAlert(title: "Remote access needs an agent password", severity: .critical) {
                    Text("This router's agent has no password, so its API is open to anyone who can reach it. Turning on Tailscale would expose the router's full management API to every device on your tailnet — over the tunnel every request arrives from 127.0.0.1 with no peer identity, so the agent can't tell your laptop from anyone else's.\n\nSet ZTE_AGENT_PASSWORD on the router and restart the agent, then come back.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    if let detail = viewModel.passwordGateMessage {
                        Text(detail)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                }
            }
        }

        if let warning = viewModel.logoutWarning {
            Section {
                TailscaleAlert(title: "Logged out, but this router's key was NOT invalidated:", severity: .critical) {
                    Text(warning)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                    Text("It may still be a live machine on your tailnet. Remove it in the admin console.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                Button("Open Admin Console") { openURL(tailscaleAdminURL) }
                Button("I've removed it") { viewModel.dismissLogoutWarning() }
            }
        }

        if viewModel.status.wakelockFailing {
            Section {
                TailscaleAlert(title: "The router may go to sleep", severity: .warning) {
                    Text("\"Keep router awake\" is on, but the router refused the wakelock. It may sleep and become unreachable even though it looks connected.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
            }
        }

        if viewModel.status.expired {
            Section {
                TailscaleAlert(title: "Node key expired", severity: .critical) {
                    Text("Remote access is down until you sign in again.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
            }
        }

        if let days = viewModel.status.daysUntilKeyExpiry {
            Section {
                TailscaleAlert(title: "This router's key expires in \(days) days", severity: .warning) {
                    Text("When it expires, remote access stops and can only be restored from the local network. Disable key expiry for this machine in your Tailscale admin console.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                Button("Open Admin Console") { openURL(tailscaleAdminURL) }
            }
        }

        if viewModel.isRemoteSession {
            Section {
                TailscaleAlert(title: "You're managing this router over Tailscale", severity: .warning) {
                    Text("Turning off, logging out, or updating will drop this connection.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    // MARK: - Status

    @ViewBuilder
    private var statusSection: some View {
        Section {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    Text(viewModel.phase.label)
                        .font(.headline)
                        .foregroundStyle(viewModel.phase.severity.color)
                    if viewModel.phase.showsSpinner {
                        ProgressView().controlSize(.small)
                    }
                }
                Text(viewModel.statusDescription)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }

            // `error` is the one field whose key is absent everywhere else.
            if viewModel.phase == .starting, let detail = viewModel.status.error, !detail.isEmpty {
                DisclosureGroup("Details") {
                    Text(detail)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                }
            }

            if viewModel.phase == .unknown {
                detailRow("Backend", viewModel.status.backendState)
            }

            primaryActions
        } footer: {
            if viewModel.phase == .needsLogin {
                Text("If this router re-registers as a new machine, delete the old entry in your Tailscale admin console.")
            }
        }
    }

    @ViewBuilder
    private var primaryActions: some View {
        switch viewModel.phase {
        case .notInstalled:
            Button("Install Tailscale") { viewModel.showUpdateSheet = true }
                .disabled(viewModel.actionsDisabled)
        case .notSetUp, .awaitingSetup:
            Button("Set Up Remote Access") { viewModel.showEnrollSheet = true }
                .disabled(viewModel.enrollDisabled)
        case .turnedOff:
            Button("Turn On Remote Access") { Task { await viewModel.enableRemoteAccess() } }
                .disabled(viewModel.enrollDisabled)
        case .needsLogin:
            Button("Sign In Again") { viewModel.showEnrollSheet = true }
                .disabled(viewModel.enrollDisabled)
        case .pendingApproval:
            Button("Open Admin Console") { openURL(tailscaleAdminURL) }
        case .awaitingBrowserLogin:
            Button("Open Sign-In Page") {
                if let raw = viewModel.loginURL, let url = URL(string: raw) { openURL(url) }
            }
            .disabled(viewModel.loginURL == nil)
            Button("Copy Link") {
                if let raw = viewModel.loginURL { viewModel.copyToClipboard(raw) }
            }
            .disabled(viewModel.loginURL == nil)
        case .degraded:
            if viewModel.degradedIsPersistent {
                Button("Sign In Again") { viewModel.showEnrollSheet = true }
                    .disabled(viewModel.enrollDisabled)
            }
        default:
            EmptyView()
        }
    }

    // MARK: - Connection details

    @ViewBuilder
    private var connectionDetailsSection: some View {
        Section("Connection Details") {
            detailRow("Tailscale IP", viewModel.status.primaryIP, copyable: true)
            if viewModel.status.tailscaleIPs.count > 1 {
                detailRow("Tailscale IPv6", viewModel.status.tailscaleIPs[1], copyable: true)
            }
            detailRow("MagicDNS name", viewModel.status.displayDNSName, copyable: true)
            detailRow("Path", viewModel.status.pathDescription)
            detailRow("Key expiry", viewModel.status.displayKeyExpiry)
            detailRow("Version", viewModel.status.clientVersion)
        }
    }

    @ViewBuilder
    private var advisoriesSection: some View {
        if !viewModel.status.health.isEmpty {
            Section {
                ForEach(viewModel.status.health, id: \.self) { advisory in
                    Text(advisory)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
            } header: {
                Text("Advisories")
            } footer: {
                Text("These are reported by Tailscale and are often benign. They don't affect whether the router is reachable.")
            }
        }
    }

    // MARK: - Settings

    @ViewBuilder
    private var settingsSection: some View {
        Section {
            VStack(alignment: .leading, spacing: 4) {
                TextField("Hostname", text: $viewModel.editHostname)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                if let error = viewModel.hostnameError {
                    Text(error).font(.caption).foregroundStyle(.red)
                }
            }

            VStack(alignment: .leading, spacing: 4) {
                TextField("Advertise routes", text: $viewModel.editRoutes, prompt: Text("192.168.0.0/24"))
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                if let error = viewModel.routesError {
                    Text(error).font(.caption).foregroundStyle(.red)
                }
            }

            Toggle("Tailscale SSH", isOn: $viewModel.editSSH)

            VStack(alignment: .leading, spacing: 4) {
                Toggle("Keep router awake", isOn: $viewModel.editWakelock)
                Text("Keeps the router awake so it stays reachable. Turning this off lets the router sleep, and it will become unreachable even though it looks connected.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Button {
                Task { await viewModel.applyConfig() }
            } label: {
                Text("Apply")
                    .frame(maxWidth: .infinity)
            }
            .disabled(!viewModel.canApplyConfig)
        } header: {
            Text("Settings")
        } footer: {
            Text("This router doesn't accept DNS or subnet routes from your tailnet.")
        }
    }

    // MARK: - Maintenance

    @ViewBuilder
    private var maintenanceSection: some View {
        Section {
            LabeledContent("Installed", value: viewModel.status.installed ? "Yes" : "No")
            // "" does NOT mean not installed — it means no version this app
            // installed is pinned.
            LabeledContent(
                "Pinned version",
                value: viewModel.status.pinnedVersion.isEmpty ? "Not managed by this app" : viewModel.status.pinnedVersion
            )
            LabeledContent("Running version", value: viewModel.status.clientVersion ?? "--")

            if viewModel.status.versionMismatch {
                Text("Installed \(viewModel.status.pinnedVersion) but running \(viewModel.status.clientVersion ?? "--") — a version swap may not have taken effect.")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }

            Button(viewModel.status.installed ? "Update Tailscale" : "Install Tailscale") {
                viewModel.showUpdateSheet = true
            }
            .disabled(viewModel.actionsDisabled)

            if let last = viewModel.status.lastUpdate {
                VStack(alignment: .leading, spacing: 4) {
                    Text(last.ok ? "Updated to \(last.version)." : "Update failed: \(last.error ?? "Unknown error")")
                        .font(.subheadline)
                        .foregroundStyle(last.ok ? .green : .red)
                        .textSelection(.enabled)
                    Text(Date(timeIntervalSince1970: TimeInterval(last.finishedAt)).formatted(date: .abbreviated, time: .shortened))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        } header: {
            Text("Maintenance")
        } footer: {
            Text("Update from Wi-Fi on the same network as the router. Updating restarts Tailscale and will drop a remote connection.")
        }
    }

    // MARK: - Danger zone

    @ViewBuilder
    private var dangerZoneSection: some View {
        Section {
            Button("Turn Off Remote Access", role: .destructive) {
                viewModel.showDisableConfirm = true
            }
            .disabled(viewModel.actionsDisabled)

            Button("Log Out of Tailscale", role: .destructive) {
                viewModel.showLogoutPasswordConfirm = true
            }
            .disabled(viewModel.actionsDisabled || viewModel.status.updating)
        } header: {
            Text("Danger Zone")
        } footer: {
            VStack(alignment: .leading, spacing: 4) {
                if viewModel.phase == .healthy {
                    Text("If you're connected over Tailscale, this will end that connection.")
                }
                Text("Turning off keeps this router's sign-in. Logging out deletes it.")
            }
        }
    }

    // MARK: - Helpers

    private var disableMessage: String {
        let body = "The router disconnects from your tailnet. Its sign-in is kept, so you can turn it back on later — but only from the local network if you lose remote access."
        return viewModel.isRemoteSession ? "\(body)\n\n\(remoteSessionParagraph)" : body
    }

    private var logoutMessage: String {
        let body = "This permanently deletes this router's Tailscale identity.\n\n• Remote access stops immediately.\n• Reconnecting requires a new auth key or browser sign-in.\n• This can't be undone."
        return viewModel.isRemoteSession ? "\(body)\n\n\(remoteSessionParagraph)" : body
    }

    private func detailRow(_ label: String, _ value: String?, copyable: Bool = false) -> some View {
        let resolved = value.flatMap { $0.isEmpty ? nil : $0 }
        return LabeledContent(label) {
            Text(resolved ?? "--")
                .font(.body.monospaced())
                .foregroundStyle(.secondary)
        }
        .contentShape(Rectangle())
        .onTapGesture {
            if copyable, let resolved { viewModel.copyToClipboard(resolved) }
        }
    }
}

// MARK: - Alert card

private struct TailscaleAlert<Content: View>: View {
    let title: String
    let severity: TailscaleSeverity
    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.subheadline.bold())
                .foregroundStyle(severity.color)
            content()
        }
    }
}

// MARK: - Enrollment sheet

private enum TailscaleEnrollMethod: Hashable {
    case authKey
    case browser
}

private struct TailscaleEnrollSheet: View {
    @Bindable var viewModel: TailscaleViewModel
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    @State private var method: TailscaleEnrollMethod = .authKey
    @State private var revealKey = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text("Connect this router to your tailnet so you can reach it from anywhere.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }

                // The sheet covers the status card, so it has to carry the card's
                // own progress. Tracking `op` rather than `message` is what makes
                // it appear: pre-warm is skipped whenever the node is already
                // enabled, leaving enrolling and verifying with no message at all.
                if viewModel.op != .none {
                    Section {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text(viewModel.phase.label)
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                if let msg = viewModel.message {
                    Section {
                        Text(msg)
                            .font(.subheadline)
                            .foregroundStyle(viewModel.messageIsError ? .red : .green)
                            .textSelection(.enabled)
                    }
                }

                Section {
                    Picker("Method", selection: $method) {
                        Text("Auth key (recommended)").tag(TailscaleEnrollMethod.authKey)
                        Text("Browser sign-in").tag(TailscaleEnrollMethod.browser)
                    }
                    .pickerStyle(.segmented)
                }

                if method == .authKey {
                    authKeySections
                } else {
                    browserSections
                }
            }
            .navigationTitle("Set Up Remote Access")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }

    @ViewBuilder
    private var authKeySections: some View {
        Section {
            Text("Create a one-off, pre-authorized key tagged tag:router in the Tailscale admin console. Tagging is what stops the key from expiring — an untagged router stops working after about 180 days and can only be fixed from the local network.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
            Button("Open Tailscale Keys Page") { openURL(tailscaleKeysURL) }
        }

        Section("Auth key") {
            HStack {
                // No .textContentType — it would invite the OS to offer to save a
                // tailnet-wide bearer credential to the keychain.
                if revealKey {
                    TextField("Auth key", text: $viewModel.authKeyInput, prompt: Text("tskey-auth-..."))
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } else {
                    SecureField("Auth key", text: $viewModel.authKeyInput, prompt: Text("tskey-auth-..."))
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }
                Button {
                    revealKey.toggle()
                } label: {
                    Image(systemName: revealKey ? "eye.slash" : "eye")
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
            }
        }

        Section {
            Button {
                Task { await viewModel.setupWithAuthKey() }
            } label: {
                Text("Connect")
                    .frame(maxWidth: .infinity)
            }
            .disabled(viewModel.enrollDisabled || viewModel.authKeyInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    @ViewBuilder
    private var browserSections: some View {
        Section {
            Text("This router is added under your account, and its key will expire (default 180 days) unless you disable key expiry for it in the admin console.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }

        Section {
            Button {
                Task { await viewModel.signInWithBrowser() }
            } label: {
                Text("Sign In With Tailscale")
                    .frame(maxWidth: .infinity)
            }
            .disabled(viewModel.enrollDisabled)
        }
    }
}

// MARK: - Update sheet

private struct TailscaleUpdateSheet: View {
    @Bindable var viewModel: TailscaleViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    if viewModel.releaseFetching {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text("Checking for the latest Tailscale release…")
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                    } else if let version = viewModel.releaseVersion {
                        Text("Latest release: Tailscale \(version).")
                            .font(.subheadline)
                    } else if let error = viewModel.releaseError {
                        Text(error)
                            .font(.subheadline)
                            .foregroundStyle(.red)
                        Button("Retry") { Task { await viewModel.loadLatestRelease() } }
                    }
                } footer: {
                    Text("Version and checksum come from pkgs.tailscale.com. The router verifies the download against this checksum before installing.")
                }

                Section {
                    Text("The router downloads this over its own connection. If the router is on mobile data, this uses about 34 MB of your data plan.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)

                    if viewModel.isRemoteSession {
                        Text("You're connected over Tailscale. Updating restarts the daemon and will drop this connection and any Tailscale SSH sessions. Do this from the local network.")
                            .font(.subheadline)
                            .foregroundStyle(.orange)
                    }

                    // The 120s "must reach Running" health gate runs only when the
                    // node was running AND enrolled, so an unenrolled update is
                    // never rolled back.
                    if !viewModel.status.enrolled {
                        Text("This router isn't enrolled, so the automatic rollback watchdog won't run for this update.")
                            .font(.subheadline)
                            .foregroundStyle(.orange)
                    }
                }

                Section {
                    DisclosureGroup("Advanced") {
                        VStack(alignment: .leading, spacing: 4) {
                            TextField("Version", text: $viewModel.advancedVersion, prompt: Text(viewModel.releaseVersion ?? "1.98.9"))
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                            if let error = viewModel.advancedVersionError {
                                Text(error).font(.caption).foregroundStyle(.red)
                            }
                        }
                        VStack(alignment: .leading, spacing: 4) {
                            TextField("SHA-256", text: $viewModel.advancedSHA256)
                                .font(.body.monospaced())
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                            if let error = viewModel.advancedSHA256Error {
                                Text(error).font(.caption).foregroundStyle(.red)
                            }
                        }
                    }
                }

                Section {
                    Button {
                        Task { await viewModel.startUpdate() }
                    } label: {
                        Text(viewModel.status.installed ? "Update" : "Install")
                            .frame(maxWidth: .infinity)
                    }
                    .disabled(!viewModel.canStartUpdate)
                }
            }
            .navigationTitle(viewModel.status.installed ? "Update Tailscale" : "Install Tailscale")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
            .task { await viewModel.loadLatestRelease() }
        }
    }
}
