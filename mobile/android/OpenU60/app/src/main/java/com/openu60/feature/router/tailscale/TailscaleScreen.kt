package com.openu60.feature.router.tailscale

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.UriHandler
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.openu60.core.model.TailscaleRelease
import com.openu60.core.model.TailscaleStatus
import com.openu60.core.model.TailscaleValidator
import java.text.SimpleDateFormat
import java.time.Duration
import java.time.Instant
import java.util.Date
import java.util.Locale

private const val URL_ADMIN_MACHINES = "https://login.tailscale.com/admin/machines"
private const val URL_KEYS = "https://login.tailscale.com/admin/settings/keys"

private const val COPY_ADMIN_LINK = "Open Admin Console"

private const val COPY_GATE_TITLE = "Remote access needs an agent password"
private const val COPY_GATE_BODY =
    "This router's agent has no password, so its API is open to anyone who can reach it. Turning on " +
        "Tailscale would expose the router's full management API to every device on your tailnet — over " +
        "the tunnel every request arrives from 127.0.0.1 with no peer identity, so the agent can't tell " +
        "your laptop from anyone else's.\n\nSet ZTE_AGENT_PASSWORD on the router and restart the agent, " +
        "then come back."

private const val COPY_LOGOUT_WARNING_PREFIX = "Logged out, but this router's key was NOT invalidated:"
private const val COPY_LOGOUT_WARNING_SUFFIX =
    "It may still be a live machine on your tailnet. Remove it in the admin console."
private const val COPY_LOGOUT_WARNING_ACK = "I've removed it"

private const val COPY_WAKELOCK_ALERT_TITLE = "The router may go to sleep"
private const val COPY_WAKELOCK_ALERT_BODY =
    "\"Keep router awake\" is on, but the router refused the wakelock. It may sleep and become " +
        "unreachable even though it looks connected."

private const val COPY_EXPIRED_TITLE = "Node key expired"
private const val COPY_EXPIRED_BODY = "Remote access is down until you sign in again."

private const val COPY_EXPIRING_BODY =
    "When it expires, remote access stops and can only be restored from the local network. Disable key " +
        "expiry for this machine in your Tailscale admin console."

private const val COPY_REMOTE_BANNER_TITLE = "You're managing this router over Tailscale"
private const val COPY_REMOTE_BANNER_BODY = "Turning off, logging out, or updating will drop this connection."

private const val COPY_REMOTE_SESSION_PARAGRAPH =
    "You're connected to this router over Tailscale right now. This will drop your connection " +
        "immediately, and you won't be able to reconnect remotely. Continue only if you have local " +
        "network or physical access to the router."

private const val COPY_NEEDS_LOGIN_FOOTER =
    "If this router re-registers as a new machine, delete the old entry in your Tailscale admin console."
private const val COPY_HEALTHY_TURN_OFF_FOOTER = "If you're connected over Tailscale, this will end that connection."

private const val COPY_ADVISORIES_FOOTER =
    "These are reported by Tailscale and are often benign. They don't affect whether the router is reachable."
private const val COPY_SETTINGS_FOOTER = "This router doesn't accept DNS or subnet routes from your tailnet."
private const val COPY_WAKELOCK_FOOTER =
    "Keeps the router awake so it stays reachable. Turning this off lets the router sleep, and it will " +
        "become unreachable even though it looks connected."
private const val COPY_MAINTENANCE_FOOTER =
    "Update from Wi-Fi on the same network as the router. Updating restarts Tailscale and will drop a " +
        "remote connection."
private const val COPY_DANGER_FOOTER = "Turning off keeps this router's sign-in. Logging out deletes it."

private const val COPY_ENROLL_TITLE = "Set Up Remote Access"
private const val COPY_ENROLL_INTRO = "Connect this router to your tailnet so you can reach it from anywhere."
private const val COPY_ENROLL_AUTHKEY_TAB = "Auth key (recommended)"
private const val COPY_ENROLL_BROWSER_TAB = "Browser sign-in"
private const val COPY_ENROLL_AUTHKEY_GUIDANCE =
    "Create a one-off, pre-authorized key tagged tag:router in the Tailscale admin console. Tagging is " +
        "what stops the key from expiring — an untagged router stops working after about 180 days and " +
        "can only be fixed from the local network."
private const val COPY_ENROLL_KEYS_LINK = "Open Tailscale Keys Page"
private const val COPY_ENROLL_BROWSER_GUIDANCE =
    "This router is added under your account, and its key will expire (default 180 days) unless you " +
        "disable key expiry for it in the admin console."

private const val COPY_DISABLE_TITLE = "Turn off remote access?"
private const val COPY_DISABLE_BODY =
    "The router disconnects from your tailnet. Its sign-in is kept, so you can turn it back on later — " +
        "but only from the local network if you lose remote access."

private const val COPY_LOGOUT_TITLE = "Log out of Tailscale?"
private const val COPY_LOGOUT_BODY =
    "This permanently deletes this router's Tailscale identity.\n\n• Remote access stops immediately.\n" +
        "• Reconnecting requires a new auth key or browser sign-in.\n• This can't be undone."

private const val COPY_SSH_CONFIRM_BODY =
    "Devices on your tailnet that your ACLs permit will be able to open a shell on this router. Access " +
        "is governed by your tailnet ACLs, which this app can't show you."
private const val COPY_ROUTE_CONFIRM_BODY =
    "This exposes that entire subnet — every device on your LAN — to your tailnet. Subnet routes must " +
        "also be approved in the Tailscale admin console before they carry traffic."
private const val COPY_SLEEP_CONFIRM_BODY =
    "Without the wakelock the router's CPU sleeps and the node becomes unreachable, even though it will " +
        "still look connected. Remote access will be unreliable."

private const val COPY_RELEASE_FETCHING = "Checking for the latest Tailscale release…"
private const val COPY_RELEASE_SOURCE =
    "Version and checksum come from pkgs.tailscale.com. The router verifies the download against this " +
        "checksum before installing."
private const val COPY_UPDATE_DATA_WARNING =
    "The router downloads this over its own connection. If the router is on mobile data, this uses about " +
        "34 MB of your data plan."
private const val COPY_UPDATE_REMOTE_WARNING =
    "You're connected over Tailscale. Updating restarts the daemon and will drop this connection and any " +
        "Tailscale SSH sessions. Do this from the local network."
private const val COPY_UPDATE_UNENROLLED_WARNING =
    "This router isn't enrolled, so the automatic rollback watchdog won't run for this update."

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TailscaleScreen(
    onBack: () -> Unit,
    viewModel: TailscaleViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsState()
    val clipboard = LocalClipboardManager.current
    val uriHandler = LocalUriHandler.current

    LaunchedEffect(Unit) { viewModel.refresh() }

    val copy: (String) -> Unit = { value ->
        clipboard.setText(AnnotatedString(value))
        viewModel.noteCopiedToClipboard()
    }

    if (state.showEnrollSheet) {
        EnrollDialog(
            connectEnabled = state.op == TailscaleOp.NONE,
            onOpenKeysPage = { uriHandler.openUri(URL_KEYS) },
            onDismiss = { viewModel.dismissEnrollSheet() },
            onAuthKey = { viewModel.setup(it) },
            onBrowserLogin = { viewModel.startBrowserLogin() },
        )
    }

    if (state.showUpdateSheet) {
        UpdateDialog(
            isInstall = !state.status.installed,
            fetching = state.releaseFetching,
            release = state.release,
            releaseError = state.releaseError,
            remoteSession = state.isRemoteSession,
            enrolled = state.status.enrolled,
            onRetry = { viewModel.fetchLatestRelease() },
            onDismiss = { viewModel.dismissUpdateSheet() },
            onConfirm = { version, sha -> viewModel.update(version, sha) },
        )
    }

    if (state.showDisableConfirm) {
        ConfirmDialog(
            title = COPY_DISABLE_TITLE,
            message = withRemoteSession(COPY_DISABLE_BODY, state.isRemoteSession),
            confirmLabel = "Turn Off",
            onDismiss = { viewModel.dismissDisableConfirm() },
            onConfirm = { viewModel.disable() },
        )
    }

    if (state.showLogoutConfirm) {
        val message = withRemoteSession(COPY_LOGOUT_BODY, state.isRemoteSession)
        if (state.hasSavedPassword) {
            PasswordConfirmDialog(
                title = COPY_LOGOUT_TITLE,
                message = message,
                confirmLabel = "Log Out",
                verify = viewModel::verifyPassword,
                onDismiss = { viewModel.dismissLogoutConfirm() },
                onConfirm = { viewModel.logout() },
            )
        } else {
            ConfirmDialog(
                title = COPY_LOGOUT_TITLE,
                message = message,
                confirmLabel = "Log Out",
                onDismiss = { viewModel.dismissLogoutConfirm() },
                onConfirm = { viewModel.logout() },
            )
        }
    }

    state.configConfirms.firstOrNull()?.let { confirm ->
        ConfirmDialog(
            title = when (confirm) {
                is TailscaleConfigConfirm.AllowSsh -> "Allow Tailscale SSH?"
                is TailscaleConfigConfirm.AdvertiseRoute -> "Advertise ${confirm.cidr}?"
                is TailscaleConfigConfirm.AllowSleep -> "Let the router sleep?"
            },
            message = when (confirm) {
                is TailscaleConfigConfirm.AllowSsh -> COPY_SSH_CONFIRM_BODY
                is TailscaleConfigConfirm.AdvertiseRoute -> COPY_ROUTE_CONFIRM_BODY
                is TailscaleConfigConfirm.AllowSleep -> COPY_SLEEP_CONFIRM_BODY
            },
            confirmLabel = when (confirm) {
                is TailscaleConfigConfirm.AllowSsh -> "Allow SSH"
                is TailscaleConfigConfirm.AdvertiseRoute -> "Advertise"
                is TailscaleConfigConfirm.AllowSleep -> "Allow Sleep"
            },
            onDismiss = { viewModel.dismissConfigConfirms() },
            onConfirm = { viewModel.acceptConfigConfirm() },
        )
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Tailscale") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        PullToRefreshBox(
            isRefreshing = state.isLoading,
            onRefresh = { viewModel.refresh() },
            modifier = Modifier.fillMaxSize().padding(padding),
        ) {
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                state.message?.let { msg ->
                    Card(
                        colors = CardDefaults.cardColors(
                            containerColor = if (state.messageIsError) MaterialTheme.colorScheme.errorContainer
                            else MaterialTheme.colorScheme.primaryContainer,
                        ),
                    ) {
                        Text(
                            msg,
                            modifier = Modifier.padding(16.dp),
                            color = if (state.messageIsError) MaterialTheme.colorScheme.onErrorContainer
                            else MaterialTheme.colorScheme.onPrimaryContainer,
                        )
                    }
                }

                AlertsSection(state, uriHandler = uriHandler, onAckLogout = { viewModel.ackLogoutWarning() })

                StatusCard(state, viewModel, uriHandler, copy)

                val connected = state.phase == TailscalePhase.HEALTHY || state.phase == TailscalePhase.DEGRADED
                if (connected) {
                    ConnectionDetailsCard(state.status, copy)
                    if (state.status.health.isNotEmpty()) AdvisoriesCard(state.status.health)
                }

                // build_status short-circuits during an update, so every live field is null —
                // rendering them would flash a false "disconnected".
                if (state.status.enrolled &&
                    state.phase != TailscalePhase.NOT_INSTALLED &&
                    state.phase != TailscalePhase.UPDATING
                ) {
                    SettingsCard(state, viewModel)
                }

                if (state.phase != TailscalePhase.UPDATING) {
                    MaintenanceCard(state, viewModel, copy)
                }

                if (state.status.enrolled) {
                    DangerZoneCard(state, viewModel)
                }
            }
        }
    }
}

private fun withRemoteSession(body: String, remoteSession: Boolean): String =
    if (remoteSession) "$body\n\n$COPY_REMOTE_SESSION_PARAGRAPH" else body

/** Null unless key_expiry is set and less than 14 days out. Null expiry means "never" — the good outcome. */
private fun daysUntilKeyExpiry(status: TailscaleStatus): Int? {
    val at = status.keyExpiryAt ?: return null
    val seconds = Duration.between(Instant.now(), at).seconds
    if (seconds <= 0 || seconds >= 14L * 86_400) return null
    return ((seconds + 86_399) / 86_400).toInt()
}

private fun formatKeyExpiry(status: TailscaleStatus): String {
    val raw = status.keyExpiry ?: return "Never"
    val at = status.keyExpiryAt ?: return raw
    return SimpleDateFormat("MMM d, yyyy HH:mm", Locale.getDefault()).format(Date.from(at))
}

@Composable
private fun AlertsSection(
    state: TailscaleState,
    uriHandler: UriHandler,
    onAckLogout: () -> Unit,
) {
    state.passwordGateMessage?.let { serverMessage ->
        // No retry and no login prompt: a 403 here is not an auth failure and cannot be
        // fixed through the API.
        AlertCard(
            title = COPY_GATE_TITLE,
            severity = TailscaleSeverity.CRITICAL,
            body = COPY_GATE_BODY,
            detail = serverMessage,
        )
    }

    state.logoutWarning?.let { warning ->
        AlertCard(
            title = COPY_LOGOUT_WARNING_PREFIX,
            severity = TailscaleSeverity.CRITICAL,
            body = COPY_LOGOUT_WARNING_SUFFIX,
            detail = warning,
        ) {
            Row {
                TextButton(onClick = { uriHandler.openUri(URL_ADMIN_MACHINES) }) { Text(COPY_ADMIN_LINK) }
                TextButton(onClick = onAckLogout) { Text(COPY_LOGOUT_WARNING_ACK) }
            }
        }
    }

    if (state.status.wakelockFailing) {
        AlertCard(COPY_WAKELOCK_ALERT_TITLE, TailscaleSeverity.WARNING, COPY_WAKELOCK_ALERT_BODY)
    }

    if (state.status.expired) {
        AlertCard(COPY_EXPIRED_TITLE, TailscaleSeverity.CRITICAL, COPY_EXPIRED_BODY)
    }

    daysUntilKeyExpiry(state.status)?.let { days ->
        AlertCard("This router's key expires in $days days", TailscaleSeverity.WARNING, COPY_EXPIRING_BODY) {
            TextButton(onClick = { uriHandler.openUri(URL_ADMIN_MACHINES) }) { Text(COPY_ADMIN_LINK) }
        }
    }

    if (state.isRemoteSession) {
        AlertCard(COPY_REMOTE_BANNER_TITLE, TailscaleSeverity.WARNING, COPY_REMOTE_BANNER_BODY)
    }
}

@Composable
private fun StatusCard(
    state: TailscaleState,
    viewModel: TailscaleViewModel,
    uriHandler: UriHandler,
    onCopy: (String) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    state.phase.label,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                    color = severityColor(state.phase.severity),
                )
                if (state.phase.showsSpinner) {
                    Spacer(modifier = Modifier.width(8.dp))
                    CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
                }
            }
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                state.phaseDescription,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            if (state.phase == TailscalePhase.UNKNOWN) {
                Spacer(modifier = Modifier.height(8.dp))
                InfoRow("Backend", state.status.backendState.orEmpty())
            }

            // status.error only exists while state == "starting"; every other phase's key is absent.
            val startupError = state.status.error
            if (state.phase == TailscalePhase.STARTING && !startupError.isNullOrEmpty()) {
                var expanded by remember { mutableStateOf(false) }
                TextButton(onClick = { expanded = !expanded }, contentPadding = PaddingValues(0.dp)) {
                    Text(if (expanded) "Hide Details" else "Details")
                }
                if (expanded) {
                    Text(
                        startupError,
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        modifier = Modifier.clickable { onCopy(startupError) },
                    )
                }
            }

            val enabled = state.actionsEnabled
            when (state.phase) {
                TailscalePhase.NOT_INSTALLED -> PrimaryAction("Install Tailscale", enabled) { viewModel.openUpdateSheet() }
                TailscalePhase.NOT_SET_UP, TailscalePhase.AWAITING_SETUP ->
                    PrimaryAction("Set Up Remote Access", enabled && !state.passwordGateBlocked) { viewModel.openEnrollSheet() }
                TailscalePhase.TURNED_OFF ->
                    PrimaryAction("Turn On Remote Access", enabled && !state.passwordGateBlocked) { viewModel.enable() }
                TailscalePhase.NEEDS_LOGIN -> {
                    PrimaryAction("Sign In Again", enabled && !state.passwordGateBlocked) { viewModel.openEnrollSheet() }
                    Text(
                        COPY_NEEDS_LOGIN_FOOTER,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                TailscalePhase.AWAITING_BROWSER_LOGIN -> {
                    val url = state.status.loginUrl
                    Spacer(modifier = Modifier.height(12.dp))
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Button(onClick = { url?.let { uriHandler.openUri(it) } }, enabled = !url.isNullOrEmpty()) {
                            Text("Open Sign-In Page")
                        }
                        TextButton(onClick = { url?.let(onCopy) }, enabled = !url.isNullOrEmpty()) {
                            Text("Copy Link")
                        }
                    }
                }
                TailscalePhase.PENDING_APPROVAL ->
                    PrimaryAction("Open Admin Console", true) { uriHandler.openUri(URL_ADMIN_MACHINES) }
                TailscalePhase.DEGRADED ->
                    if (state.degradedActionVisible) {
                        PrimaryAction("Sign In Again", enabled && !state.passwordGateBlocked) { viewModel.openEnrollSheet() }
                    }
                else -> {}
            }
        }
    }
}

@Composable
private fun ColumnScope.PrimaryAction(label: String, enabled: Boolean, onClick: () -> Unit) {
    Spacer(modifier = Modifier.height(12.dp))
    Button(onClick = onClick, enabled = enabled, modifier = Modifier.fillMaxWidth()) { Text(label) }
}

@Composable
private fun ConnectionDetailsCard(status: TailscaleStatus, onCopy: (String) -> Unit) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text("Connection", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
            Spacer(modifier = Modifier.height(8.dp))
            CopyableInfoRow("Tailscale IP", status.primaryIp, onCopy)
            if (status.tailscaleIps.size > 1) {
                CopyableInfoRow("Tailscale IPv6", status.tailscaleIps[1], onCopy)
            }
            CopyableInfoRow("MagicDNS name", status.displayDnsName, onCopy)
            // Neutral: relayed is expected behind CGNAT-to-CGNAT, not a fault.
            InfoRow("Path", status.pathDescription)
            InfoRow("Key expiry", formatKeyExpiry(status))
            InfoRow("Version", status.clientVersion.orEmpty())
        }
    }
}

@Composable
private fun AdvisoriesCard(health: List<String>) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text("Advisories", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
            Spacer(modifier = Modifier.height(8.dp))
            health.forEach { advisory ->
                Text(
                    advisory,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(vertical = 2.dp),
                )
            }
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                COPY_ADVISORIES_FOOTER,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun SettingsCard(state: TailscaleState, viewModel: TailscaleViewModel) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text("Settings", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
            Spacer(modifier = Modifier.height(8.dp))
            OutlinedTextField(
                value = state.editHostname,
                onValueChange = viewModel::setHostname,
                label = { Text("Hostname") },
                singleLine = true,
                isError = state.hostnameError != null,
                supportingText = state.hostnameError?.let { error -> @Composable { Text(error) } },
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(modifier = Modifier.height(8.dp))
            OutlinedTextField(
                value = state.editRoutes,
                onValueChange = viewModel::setRoutes,
                label = { Text("Advertise routes") },
                placeholder = { Text("192.168.0.0/24") },
                singleLine = true,
                isError = state.routesError != null,
                supportingText = state.routesError?.let { error -> @Composable { Text(error) } },
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(modifier = Modifier.height(8.dp))
            ToggleRow("Tailscale SSH", state.editSsh) { viewModel.setSsh(it) }
            ToggleRow("Keep router awake", state.editWakelock) { viewModel.setWakelock(it) }
            Text(
                COPY_WAKELOCK_FOOTER,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(modifier = Modifier.height(12.dp))
            Button(
                onClick = { viewModel.requestApplyConfig() },
                enabled = state.actionsEnabled && state.configPatch.isNotEmpty(),
            ) { Text("Apply") }
            Spacer(modifier = Modifier.height(8.dp))
            Text(
                COPY_SETTINGS_FOOTER,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun MaintenanceCard(state: TailscaleState, viewModel: TailscaleViewModel, onCopy: (String) -> Unit) {
    val status = state.status
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text("Maintenance", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
            Spacer(modifier = Modifier.height(8.dp))
            InfoRow("Installed", if (status.installed) "Yes" else "No")
            // "" does NOT mean "not installed" — it means no install this app performed.
            InfoRow("Pinned version", status.pinnedVersion.ifEmpty { "Not managed by this app" })
            InfoRow("Running version", status.clientVersion.orEmpty())

            if (status.versionMismatch) {
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    "Installed ${status.pinnedVersion} but running ${status.clientVersion} — a version " +
                        "swap may not have taken effect.",
                    style = MaterialTheme.typography.bodySmall,
                    color = severityColor(TailscaleSeverity.WARNING),
                )
            }

            Spacer(modifier = Modifier.height(12.dp))
            Button(onClick = { viewModel.openUpdateSheet() }, enabled = state.actionsEnabled) {
                Text(if (status.installed) "Update Tailscale" else "Install Tailscale")
            }

            status.lastUpdate?.let { last ->
                Spacer(modifier = Modifier.height(8.dp))
                if (last.ok) {
                    Text(
                        "Updated to ${last.version}.",
                        style = MaterialTheme.typography.bodySmall,
                        color = severityColor(TailscaleSeverity.GOOD),
                    )
                } else {
                    // Verbatim and copyable — these are diagnostic and specific.
                    val detail = last.error ?: "unknown error"
                    Text(
                        "Update failed: $detail",
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.clickable { onCopy(detail) },
                    )
                }
            }

            Spacer(modifier = Modifier.height(8.dp))
            Text(
                COPY_MAINTENANCE_FOOTER,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun DangerZoneCard(state: TailscaleState, viewModel: TailscaleViewModel) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text("Danger Zone", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
            Spacer(modifier = Modifier.height(4.dp))
            TextButton(onClick = { viewModel.requestDisable() }, enabled = state.actionsEnabled) {
                Text("Turn Off Remote Access", color = MaterialTheme.colorScheme.error)
            }
            if (state.phase == TailscalePhase.HEALTHY) {
                Text(
                    COPY_HEALTHY_TURN_OFF_FOOTER,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            TextButton(
                onClick = { viewModel.requestLogout() },
                enabled = state.actionsEnabled && !state.status.updating,
            ) {
                Text("Log Out of Tailscale", color = MaterialTheme.colorScheme.error)
            }
            Spacer(modifier = Modifier.height(4.dp))
            Text(
                COPY_DANGER_FOOTER,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

// MARK: - Dialogs

@Composable
private fun EnrollDialog(
    connectEnabled: Boolean,
    onOpenKeysPage: () -> Unit,
    onDismiss: () -> Unit,
    onAuthKey: (String) -> Unit,
    onBrowserLogin: () -> Unit,
) {
    var useAuthKey by remember { mutableStateOf(true) }
    // The key lives here and nowhere else: dismissing the dialog is what clears it.
    var authKey by remember { mutableStateOf("") }
    var reveal by remember { mutableStateOf(false) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(COPY_ENROLL_TITLE) },
        text = {
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(COPY_ENROLL_INTRO, style = MaterialTheme.typography.bodyMedium)
                TabRow(selectedTabIndex = if (useAuthKey) 0 else 1) {
                    Tab(
                        selected = useAuthKey,
                        onClick = { useAuthKey = true },
                        text = { Text(COPY_ENROLL_AUTHKEY_TAB) },
                    )
                    Tab(
                        selected = !useAuthKey,
                        onClick = { useAuthKey = false },
                        text = { Text(COPY_ENROLL_BROWSER_TAB) },
                    )
                }
                if (useAuthKey) {
                    Text(
                        COPY_ENROLL_AUTHKEY_GUIDANCE,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    TextButton(onClick = onOpenKeysPage, contentPadding = PaddingValues(0.dp)) {
                        Text(COPY_ENROLL_KEYS_LINK)
                    }
                    OutlinedTextField(
                        value = authKey,
                        onValueChange = { authKey = it },
                        label = { Text("Auth key") },
                        placeholder = { Text("tskey-auth-...") },
                        singleLine = true,
                        visualTransformation = if (reveal) VisualTransformation.None else PasswordVisualTransformation(),
                        trailingIcon = {
                            IconButton(onClick = { reveal = !reveal }) {
                                Icon(
                                    if (reveal) Icons.Default.VisibilityOff else Icons.Default.Visibility,
                                    contentDescription = if (reveal) "Hide auth key" else "Show auth key",
                                )
                            }
                        },
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                        modifier = Modifier.fillMaxWidth(),
                    )
                } else {
                    Text(
                        COPY_ENROLL_BROWSER_GUIDANCE,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        },
        confirmButton = {
            if (useAuthKey) {
                TextButton(
                    onClick = { onAuthKey(authKey) },
                    enabled = connectEnabled && authKey.isNotBlank(),
                ) { Text("Connect") }
            } else {
                TextButton(onClick = onBrowserLogin) { Text("Sign In With Tailscale") }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
private fun UpdateDialog(
    isInstall: Boolean,
    fetching: Boolean,
    release: TailscaleRelease?,
    releaseError: String?,
    remoteSession: Boolean,
    enrolled: Boolean,
    onRetry: () -> Unit,
    onDismiss: () -> Unit,
    onConfirm: (String, String) -> Unit,
) {
    var advanced by remember { mutableStateOf(false) }
    var manualVersion by remember { mutableStateOf("") }
    var manualSha by remember { mutableStateOf("") }

    // Typed input wins over the fetched values — Advanced is also the fallback when the lookup fails.
    val version = manualVersion.trim().ifEmpty { release?.version.orEmpty() }
    val sha = manualSha.trim().ifEmpty { release?.sha256.orEmpty() }
    val versionError = manualVersion.trim().takeIf { it.isNotEmpty() }?.let { TailscaleValidator.version(it) }
    val shaError = manualSha.trim().takeIf { it.isNotEmpty() }?.let { TailscaleValidator.sha256(it) }
    val canConfirm = !fetching && version.isNotEmpty() && sha.isNotEmpty() &&
        versionError == null && shaError == null

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (isInstall) "Install Tailscale" else "Update Tailscale") },
        text = {
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                when {
                    fetching -> Row(verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
                        Spacer(modifier = Modifier.width(8.dp))
                        Text(COPY_RELEASE_FETCHING, style = MaterialTheme.typography.bodyMedium)
                    }
                    release != null -> {
                        Text(
                            "Latest release: Tailscale ${release.version}.",
                            style = MaterialTheme.typography.bodyMedium,
                            fontWeight = FontWeight.Bold,
                        )
                        Text(
                            COPY_RELEASE_SOURCE,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    // A lookup failure is this phone's internet, never the router's — don't render
                    // it as an agent error.
                    releaseError != null -> {
                        Text(
                            releaseError,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                        TextButton(onClick = onRetry, contentPadding = PaddingValues(0.dp)) { Text("Retry") }
                    }
                }

                Text(
                    COPY_UPDATE_DATA_WARNING,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (remoteSession) {
                    Text(
                        COPY_UPDATE_REMOTE_WARNING,
                        style = MaterialTheme.typography.bodySmall,
                        color = severityColor(TailscaleSeverity.WARNING),
                    )
                }
                if (!enrolled) {
                    Text(
                        COPY_UPDATE_UNENROLLED_WARNING,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }

                TextButton(onClick = { advanced = !advanced }, contentPadding = PaddingValues(0.dp)) {
                    Text("Advanced")
                }
                if (advanced) {
                    OutlinedTextField(
                        value = manualVersion,
                        onValueChange = { manualVersion = it },
                        label = { Text("Version") },
                        singleLine = true,
                        isError = versionError != null,
                        supportingText = versionError?.let { error -> @Composable { Text(error) } },
                        modifier = Modifier.fillMaxWidth(),
                    )
                    OutlinedTextField(
                        value = manualSha,
                        onValueChange = { manualSha = it },
                        label = { Text("SHA-256") },
                        singleLine = true,
                        isError = shaError != null,
                        supportingText = shaError?.let { error -> @Composable { Text(error) } },
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { onConfirm(version, sha) }, enabled = canConfirm) {
                Text(if (isInstall) "Install" else "Update")
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
private fun ConfirmDialog(
    title: String,
    message: String,
    confirmLabel: String,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = { Text(message) },
        confirmButton = {
            TextButton(onClick = onConfirm) { Text(confirmLabel, color = MaterialTheme.colorScheme.error) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** Local-only gate against accidental taps and borrowed phones — the password is never sent. */
@Composable
private fun PasswordConfirmDialog(
    title: String,
    message: String,
    confirmLabel: String,
    verify: (String) -> Boolean,
    onDismiss: () -> Unit,
    onConfirm: () -> Unit,
) {
    var value by remember { mutableStateOf("") }
    var wrong by remember { mutableStateOf(false) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(message, style = MaterialTheme.typography.bodyMedium)
                OutlinedTextField(
                    value = value,
                    onValueChange = { value = it; wrong = false },
                    label = { Text("Router password") },
                    singleLine = true,
                    isError = wrong,
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                    supportingText = if (wrong) {
                        @Composable { Text("Incorrect password.") }
                    } else {
                        null
                    },
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = { if (verify(value)) onConfirm() else wrong = true },
                enabled = value.isNotBlank(),
            ) { Text(confirmLabel, color = MaterialTheme.colorScheme.error) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

// MARK: - Shared bits

@Composable
private fun severityColor(severity: TailscaleSeverity): Color = when (severity) {
    TailscaleSeverity.NEUTRAL -> MaterialTheme.colorScheme.onSurfaceVariant
    TailscaleSeverity.GOOD -> Color(0xFF2E7D32)
    TailscaleSeverity.WARNING -> Color(0xFFE65100)
    TailscaleSeverity.CRITICAL -> MaterialTheme.colorScheme.error
}

@Composable
private fun AlertCard(
    title: String,
    severity: TailscaleSeverity,
    body: String,
    detail: String? = null,
    content: @Composable ColumnScope.() -> Unit = {},
) {
    val critical = severity == TailscaleSeverity.CRITICAL
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = if (critical) MaterialTheme.colorScheme.errorContainer
            else MaterialTheme.colorScheme.surfaceVariant,
        ),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                title,
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.Bold,
                color = if (critical) MaterialTheme.colorScheme.onErrorContainer else severityColor(severity),
            )
            Spacer(modifier = Modifier.height(4.dp))
            Text(
                body,
                style = MaterialTheme.typography.bodySmall,
                color = if (critical) MaterialTheme.colorScheme.onErrorContainer
                else MaterialTheme.colorScheme.onSurfaceVariant,
            )
            detail?.let {
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    color = if (critical) MaterialTheme.colorScheme.onErrorContainer
                    else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            content()
        }
    }
}

@Composable
private fun CopyableInfoRow(label: String, value: String?, onCopy: (String) -> Unit) {
    val shown = value?.takeIf { it.isNotBlank() }
    InfoRow(label, shown.orEmpty(), onClick = shown?.let { v -> { onCopy(v) } })
}

@Composable
private fun InfoRow(label: String, value: String, onClick: (() -> Unit)? = null) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(vertical = 2.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(label, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(value.ifBlank { "--" }, style = MaterialTheme.typography.bodyMedium, fontFamily = FontFamily.Monospace)
    }
}

@Composable
private fun ToggleRow(label: String, checked: Boolean, onToggle: (Boolean) -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, style = MaterialTheme.typography.bodyLarge)
        Switch(checked = checked, onCheckedChange = onToggle)
    }
}
