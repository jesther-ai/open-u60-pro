package com.openu60.feature.router.tailscale

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.openu60.core.model.DeviceParser
import com.openu60.core.model.TailscaleParser
import com.openu60.core.model.TailscaleRelease
import com.openu60.core.model.TailscaleStatus
import com.openu60.core.model.TailscaleValidator
import com.openu60.core.network.AgentClient
import com.openu60.core.network.AgentError
import com.openu60.core.network.AuthManager
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import java.net.URI
import java.util.concurrent.TimeUnit
import javax.inject.Inject

private const val PATH_STATUS = "/api/tailscale/status"
private const val PATH_SETUP = "/api/tailscale/setup"
private const val PATH_LOGIN_URL = "/api/tailscale/login-url"
private const val PATH_ENABLE = "/api/tailscale/enable"
private const val PATH_DISABLE = "/api/tailscale/disable"
private const val PATH_CONFIG = "/api/tailscale/config"
private const val PATH_LOGOUT = "/api/tailscale/logout"
private const val PATH_UPDATE = "/api/tailscale/update"

private const val RELEASE_BASE = "https://pkgs.tailscale.com/stable"
private const val RELEASE_INDEX_URL = "$RELEASE_BASE/?mode=json"

/** GET /status serves a 10s cache, so a faster poll returns the byte-identical object. */
private const val POLL_ACTIVE_MS = 10_000L
private const val POLL_IDLE_MS = 30_000L

private const val PREWARM_CAP_POLLS = 18
private const val ENABLE_CAP_POLLS = 18
private const val VERIFY_CAP_POLLS = 24
private const val BROWSER_LOGIN_CAP_POLLS = 31
private const val DISABLE_CAP_POLLS = 3
private const val UPDATE_CAP_POLLS = 40
private const val MAX_POLL_FAILURES = 6

/** The agent's own monitor rebinds at 2 bad cycles and restarts at 4 — no action before then. */
private const val DEGRADED_RECOVERING_POLLS = 4

private val PREWARM_READY_STATES =
    setOf("awaiting_setup", "needs_login", "connecting", "paused", "healthy", "degraded", "pending_approval")
private val ENABLE_TERMINAL_STATES =
    setOf("healthy", "degraded", "needs_login", "awaiting_setup", "pending_approval")

private const val COPY_PREWARM_PROGRESS = "Starting Tailscale… this can take up to 2 minutes on first run."
private const val COPY_PREWARM_TIMEOUT =
    "Tailscale isn't starting. Check that the router has mobile data and its clock is set."
private const val COPY_ENROLL_SUCCESS = "This router is now on your tailnet."
private const val COPY_ENROLL_PENDING_APPROVAL =
    "Setup completed. Approve this router in your Tailscale admin console."
private const val COPY_ENROLL_VERIFY_ELAPSED =
    "Setup didn't complete. Enter a fresh auth key — one-off keys can't be reused."
private const val COPY_BROWSER_LOGIN_ELAPSED = "Sign-in didn't complete in time. Try again."
private const val COPY_ENABLE_ACCEPTED = "Turning on remote access…"
private const val COPY_ENABLE_ELAPSED = "Still starting — check back shortly."
private const val COPY_DISABLE_SUCCESS = "Remote access is off."
private const val COPY_CONFIG_APPLIED = "Settings applied."
private const val COPY_CONFIG_PERSISTED = "Saved. These will apply the next time Tailscale starts."
private const val COPY_CONFIG_REJECTED = "Tailscale rejected the change. Nothing was saved."
private const val COPY_LOGOUT_SUCCESS = "Logged out. This router has been removed from your tailnet."
private const val COPY_LOGOUT_AMBIGUOUS =
    "Logged out. If this router still appears in your admin console, remove it there."
private const val COPY_LOGOUT_BLOCKED = "Can't log out while an update is running."
private const val COPY_UPDATE_IN_PROGRESS =
    "Updating Tailscale. This can take several minutes. Don't power off the router."
private const val COPY_UPDATE_UNKNOWN =
    "Update finished, but the result is unknown (the agent may have restarted). Check the running version below."
private const val COPY_UPDATE_VERSION_UNCHANGED = "Update reported success but the version didn't change."
private const val COPY_RELEASE_FAILED =
    "Couldn't reach pkgs.tailscale.com to look up the latest release. Check this phone's internet " +
        "connection, or enter a version and checksum manually under Advanced."
private const val COPY_RELEASE_NO_ARM64 =
    "The latest Tailscale release doesn't publish an arm64 build, which this router needs. Enter a " +
        "version and checksum manually under Advanced."
private const val COPY_RELEASE_BAD_SHA =
    "pkgs.tailscale.com returned a checksum this app couldn't read. Enter a version and checksum " +
        "manually under Advanced."
private const val COPY_LOST_CONNECTION = "Lost connection to the router."
private const val COPY_COPIED = "Copied to clipboard"

enum class TailscaleSeverity { NEUTRAL, GOOD, WARNING, CRITICAL }

/**
 * Ordered to match the iOS enum and the spec's phase table — the derivation is duplicated in two
 * languages with no shared source, so a side-by-side diff has to stay readable.
 */
enum class TailscalePhase(
    val label: String,
    val severity: TailscaleSeverity,
    val showsSpinner: Boolean,
) {
    NOT_INSTALLED("Not installed", TailscaleSeverity.NEUTRAL, false),
    UPDATING("Updating", TailscaleSeverity.NEUTRAL, true),
    NOT_SET_UP("Not set up", TailscaleSeverity.NEUTRAL, false),
    TURNED_OFF("Off", TailscaleSeverity.NEUTRAL, false),
    TURNING_OFF("Turning off…", TailscaleSeverity.NEUTRAL, true),
    LOGGING_OUT("Logging out…", TailscaleSeverity.NEUTRAL, true),
    STARTING("Starting…", TailscaleSeverity.NEUTRAL, true),
    ENROLLING("Setting up…", TailscaleSeverity.NEUTRAL, true),
    VERIFYING_ENROLLMENT("Finishing setup…", TailscaleSeverity.NEUTRAL, true),
    AWAITING_BROWSER_LOGIN("Waiting for sign-in", TailscaleSeverity.NEUTRAL, true),
    AWAITING_SETUP("Not signed in", TailscaleSeverity.NEUTRAL, false),
    NEEDS_LOGIN("Sign-in required", TailscaleSeverity.CRITICAL, false),
    PENDING_APPROVAL("Waiting for approval", TailscaleSeverity.WARNING, true),
    PAUSED("Reconnecting…", TailscaleSeverity.NEUTRAL, true),
    HEALTHY("Connected", TailscaleSeverity.GOOD, false),
    DEGRADED("Connection problem", TailscaleSeverity.WARNING, true),
    UNKNOWN("Unknown", TailscaleSeverity.NEUTRAL, false),
}

enum class TailscaleOp {
    NONE,
    ENABLING,
    PREWARMING,
    ENROLLING,
    VERIFYING_ENROLLMENT,
    AWAITING_BROWSER_LOGIN,
    DISABLING,
    LOGGING_OUT,
    UPDATING,
    SAVING_CONFIG,
}

sealed class TailscaleConfigConfirm {
    data object AllowSsh : TailscaleConfigConfirm()
    data class AdvertiseRoute(val cidr: String) : TailscaleConfigConfirm()
    data object AllowSleep : TailscaleConfigConfirm()
}

data class TailscaleState(
    val status: TailscaleStatus = TailscaleStatus.empty,
    val op: TailscaleOp = TailscaleOp.NONE,
    val isLoading: Boolean = false,
    val message: String? = null,
    val messageIsError: Boolean = false,
    val initialLoadDone: Boolean = false,
    val hasSavedPassword: Boolean = false,
    val degradedPolls: Int = 0,
    val isRemoteSession: Boolean = false,
    val passwordGateMessage: String? = null,
    val logoutWarning: String? = null,
    val editHostname: String = "",
    val editRoutes: String = "",
    val editSsh: Boolean = false,
    val editWakelock: Boolean = true,
    val hostnameError: String? = null,
    val routesError: String? = null,
    val configConfirms: List<TailscaleConfigConfirm> = emptyList(),
    val showEnrollSheet: Boolean = false,
    val showUpdateSheet: Boolean = false,
    val showDisableConfirm: Boolean = false,
    val showLogoutConfirm: Boolean = false,
    val releaseFetching: Boolean = false,
    val release: TailscaleRelease? = null,
    val releaseError: String? = null,
) {
    val phase: TailscalePhase get() = derivePhase(status, op)

    /** Not fixable from the API, so it disables the enable-path actions instead of retrying. */
    val passwordGateBlocked: Boolean get() = passwordGateMessage != null

    val actionsEnabled: Boolean get() =
        !isLoading && op == TailscaleOp.NONE && phase != TailscalePhase.UPDATING

    val phaseDescription: String get() = when (phase) {
        TailscalePhase.NOT_INSTALLED ->
            "Tailscale isn't installed on this router yet."
        TailscalePhase.UPDATING ->
            "Installing Tailscale. This can take several minutes over mobile data. Don't power off the router."
        TailscalePhase.NOT_SET_UP ->
            "Connect this router to your tailnet to reach it from anywhere."
        TailscalePhase.TURNED_OFF ->
            "Remote access is off. This router's sign-in is saved."
        TailscalePhase.TURNING_OFF ->
            "Disconnecting from your tailnet."
        TailscalePhase.LOGGING_OUT ->
            "Removing this router's Tailscale identity."
        TailscalePhase.STARTING ->
            "Waiting for the router's clock and mobile data. This can take up to 2 minutes."
        TailscalePhase.ENROLLING ->
            "Connecting this router to your tailnet. This can take up to 2 minutes."
        TailscalePhase.VERIFYING_ENROLLMENT ->
            "The router is still working. Checking whether setup completed."
        TailscalePhase.AWAITING_BROWSER_LOGIN ->
            "Open the sign-in page and approve this router. This link is valid for about 5 minutes."
        TailscalePhase.AWAITING_SETUP ->
            "Tailscale is running but this router hasn't joined a tailnet yet."
        TailscalePhase.NEEDS_LOGIN ->
            if (status.expired) "This router's node key expired. Sign in again to restore remote access."
            else "This router's sign-in is no longer valid. Sign in again to restore remote access."
        TailscalePhase.PENDING_APPROVAL ->
            "Approve this router in the Tailscale admin console. Nothing can be done from this app."
        TailscalePhase.PAUSED ->
            "Tailscale is reconnecting. This usually clears within a minute."
        TailscalePhase.HEALTHY ->
            status.displayDnsName ?: "This router is on your tailnet."
        TailscalePhase.DEGRADED ->
            if (degradedPolls <= DEGRADED_RECOVERING_POLLS)
                "The router is signed in but not reachable. Recovering automatically."
            else "The router is signed in but still not reachable."
        TailscalePhase.UNKNOWN ->
            "The router reported a state this app doesn't recognize."
    }

    val degradedActionVisible: Boolean get() =
        phase == TailscalePhase.DEGRADED && degradedPolls > DEGRADED_RECOVERING_POLLS

    /**
     * Only the changed fields: /config is schedulable and a web dashboard exists, so a
     * full-state write can silently revert someone else's concurrent change. Values are real
     * JSON types — TsConfigPatch is native serde_json, unlike the stringly-typed ubus endpoints.
     */
    val configPatch: Map<String, Any?> get() {
        val patch = mutableMapOf<String, Any?>()
        val trimmedHostname = editHostname.trim()
        if (trimmedHostname != status.hostname) patch["hostname"] = trimmedHostname
        val routes = TailscaleValidator.parseRoutes(editRoutes)
        if (routes != status.advertiseRoutes) patch["advertise_routes"] = routes
        if (editWakelock != status.holdWakelock) patch["hold_wakelock"] = editWakelock
        if (editSsh != status.tailscaleSsh) patch["tailscale_ssh"] = editSsh
        return patch
    }
}

/**
 * First match wins. `state` alone is insufficient: build_status returns "disabled" before the CLI
 * is ever called, so a fresh install and a deliberate turn-off are the same string and only
 * `enrolled` separates them. The enabled-guards on paused/respawning cover the POST /disable race,
 * where the daemon is still alive for a moment after the desired state flipped.
 */
private fun derivePhase(status: TailscaleStatus, op: TailscaleOp): TailscalePhase {
    if (op == TailscaleOp.UPDATING || status.updating || status.state == "updating") {
        return TailscalePhase.UPDATING
    }
    if (status.state == "not_installed") return TailscalePhase.NOT_INSTALLED

    if (op == TailscaleOp.ENROLLING) return TailscalePhase.ENROLLING
    if (op == TailscaleOp.VERIFYING_ENROLLMENT) return TailscalePhase.VERIFYING_ENROLLMENT
    if (op == TailscaleOp.AWAITING_BROWSER_LOGIN &&
        status.state != "healthy" &&
        status.state != "pending_approval"
    ) {
        return TailscalePhase.AWAITING_BROWSER_LOGIN
    }
    if (op == TailscaleOp.PREWARMING || op == TailscaleOp.ENABLING) return TailscalePhase.STARTING
    if (op == TailscaleOp.DISABLING) return TailscalePhase.TURNING_OFF
    if (op == TailscaleOp.LOGGING_OUT) return TailscalePhase.LOGGING_OUT

    return when (status.state) {
        "disabled" -> if (status.enrolled) TailscalePhase.TURNED_OFF else TailscalePhase.NOT_SET_UP
        "paused" -> if (status.enabled) TailscalePhase.PAUSED else TailscalePhase.TURNING_OFF
        "respawning" -> if (status.enabled) TailscalePhase.STARTING else TailscalePhase.TURNING_OFF
        "starting" -> TailscalePhase.STARTING
        "connecting" -> TailscalePhase.STARTING
        "awaiting_setup" -> TailscalePhase.AWAITING_SETUP
        "needs_login" -> TailscalePhase.NEEDS_LOGIN
        "pending_approval" -> TailscalePhase.PENDING_APPROVAL
        "healthy" -> TailscalePhase.HEALTHY
        "degraded" -> TailscalePhase.DEGRADED
        else -> TailscalePhase.UNKNOWN
    }
}

@HiltViewModel
class TailscaleViewModel @Inject constructor(
    private val agentClient: AgentClient,
    private val authManager: AuthManager,
) : ViewModel() {

    private val _state = MutableStateFlow(TailscaleState())
    val state: StateFlow<TailscaleState> = _state.asStateFlow()

    private var pollingJob: Job? = null
    private var pollFailures = 0
    private var reauthAttempted = false

    /**
     * The circuit breaker has fired. An op's cap copy ("Tailscale isn't starting", "Setup didn't
     * complete") must not overwrite it — when the router stopped answering at all, that is the
     * real story and the cap copy sends the user chasing the wrong thing.
     */
    private val connectionLost: Boolean get() = pollFailures >= MAX_POLL_FAILURES

    // Must not go through AgentClient: that is bound to the router and attaches the agent's
    // bearer token, which must never be sent to pkgs.tailscale.com.
    private val releaseClient = OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(20, TimeUnit.SECONDS)
        .build()

    init {
        _state.value = _state.value.copy(hasSavedPassword = authManager.savedPassword.isNotBlank())
    }

    override fun onCleared() {
        super.onCleared()
        stopPolling()
    }

    fun refresh() {
        viewModelScope.launch {
            val firstLoad = !_state.value.initialLoadDone
            // Clearing up front is what makes a stale failure banner clearable: on success nothing
            // reinstates it, and on failure fetchStatus writes the fresh one.
            _state.value = _state.value.copy(isLoading = true, message = null)
            fetchStatus(reportFailure = true)
            _state.value = _state.value.copy(isLoading = false)
            // An update outlives this ViewModel, so a re-entered screen resumes the watch
            // rather than assuming the poll survived.
            if (firstLoad && _state.value.status.updating && _state.value.op == TailscaleOp.NONE) {
                setOp(TailscaleOp.UPDATING)
                watchUpdate(null)
            }
            if (_state.value.op == TailscaleOp.NONE) startPolling()
        }
    }

    // MARK: - Enrollment

    fun openEnrollSheet() {
        if (!_state.value.actionsEnabled || _state.value.passwordGateBlocked) return
        _state.value = _state.value.copy(showEnrollSheet = true)
    }

    fun dismissEnrollSheet() {
        _state.value = _state.value.copy(showEnrollSheet = false)
    }

    /**
     * The key is a parameter and never enters TailscaleState — a tailnet-wide bearer credential
     * has no business in observable state, and it is never persisted, logged or retried.
     */
    fun setup(authKey: String) {
        val key = authKey.trim()
        if (key.isEmpty() || _state.value.op != TailscaleOp.NONE) return
        viewModelScope.launch {
            stopPolling()
            try {
                dismissEnrollSheet()
                if (!prewarm()) return@launch
                setOp(TailscaleOp.ENROLLING)
                showMessage(null, false)
                try {
                    val data = withReauth { agentClient.postJSONSlow(PATH_SETUP, mapOf("auth_key" to key)) }
                    // The 200 body IS the refreshed status payload — no follow-up GET.
                    adoptStatus(TailscaleParser.parse(data))
                    setOp(TailscaleOp.NONE)
                    reportEnrollmentOutcome()
                } catch (e: AgentError.ApiError) {
                    setOp(TailscaleOp.NONE)
                    when (e.status) {
                        403 -> blockPasswordGate(e.serverMessage)
                        502 -> setError(
                            "Setup failed: ${e.serverMessage}. Enter a fresh auth key — " +
                                "one-off keys can't be reused."
                        )
                        else -> setError(e.serverMessage)
                    }
                } catch (_: AgentError.Timeout) {
                    // Not an error: /setup blocks for up to ~90s server-side and the enrollment is
                    // very likely still landing. Re-sending the key would 409 or burn a fresh one.
                    setOp(TailscaleOp.VERIFYING_ENROLLMENT)
                    val done = awaitStatus(VERIFY_CAP_POLLS) {
                        it.state == "healthy" || it.state == "pending_approval"
                    }
                    setOp(TailscaleOp.NONE)
                    when {
                        done -> reportEnrollmentOutcome()
                        !connectionLost -> setError(COPY_ENROLL_VERIFY_ELAPSED)
                    }
                } catch (e: Exception) {
                    setOp(TailscaleOp.NONE)
                    setError(e.message)
                }
            } finally {
                startPolling()
            }
        }
    }

    fun startBrowserLogin() {
        if (_state.value.op != TailscaleOp.NONE) return
        viewModelScope.launch {
            stopPolling()
            try {
                dismissEnrollSheet()
                if (!prewarm()) return@launch
                setOp(TailscaleOp.AWAITING_BROWSER_LOGIN)
                showMessage(null, false)
                try {
                    val data = withReauth { agentClient.postJSONSlow(PATH_LOGIN_URL) }
                    // 202 and 200 are indistinguishable through the raw-JSON API, so branch on
                    // the payload: `login_url as? String ?: ""` would silently drop the pending case.
                    val pending = DeviceParser.asBool(data["pending"])
                    val url = TailscaleParser.asString(data["login_url"])
                    if (!pending && !url.isNullOrEmpty()) adoptLoginUrl(url)
                } catch (e: AgentError.ApiError) {
                    setOp(TailscaleOp.NONE)
                    if (e.status == 403) blockPasswordGate(e.serverMessage) else setError(e.serverMessage)
                    return@launch
                } catch (_: AgentError.Timeout) {
                    // Fall through to polling: the URL surfaces via /status either way.
                } catch (e: Exception) {
                    setOp(TailscaleOp.NONE)
                    setError(e.message)
                    return@launch
                }

                // needs_login is deliberately NOT terminal here: it is the *starting* state of the
                // re-auth path, so exiting on it would kill the flow before the URL appears.
                val done = awaitStatus(BROWSER_LOGIN_CAP_POLLS) {
                    it.state == "healthy" || it.state == "pending_approval"
                }
                setOp(TailscaleOp.NONE)
                when {
                    done -> reportEnrollmentOutcome()
                    !connectionLost -> setError(COPY_BROWSER_LOGIN_ELAPSED)
                }
            } finally {
                startPolling()
            }
        }
    }

    /**
     * Never call /setup or /login-url against a cold daemon: ensure_daemon_ready can burn ~180s
     * inside the request. Pre-warming also surfaces the 403 password gate before the user pastes
     * a secret, and the needs_login re-auth path 409s without it.
     */
    private suspend fun prewarm(): Boolean {
        if (_state.value.status.enabled) return true
        setOp(TailscaleOp.PREWARMING)
        showMessage(COPY_PREWARM_PROGRESS, false)
        try {
            withReauth { agentClient.postJSON(PATH_ENABLE) }
        } catch (e: AgentError.ApiError) {
            setOp(TailscaleOp.NONE)
            handleEnablePathError(e)
            return false
        } catch (e: Exception) {
            setOp(TailscaleOp.NONE)
            setError(e.message)
            return false
        }
        clearPasswordGate()

        if (!awaitStatus(PREWARM_CAP_POLLS) { it.state in PREWARM_READY_STATES }) {
            setOp(TailscaleOp.NONE)
            if (!connectionLost) setError(COPY_PREWARM_TIMEOUT)
            return false
        }
        setOp(TailscaleOp.NONE)
        // Already enrolled and connected — there is nothing left to enroll.
        if (_state.value.status.state == "healthy") {
            showMessage(null, false)
            return false
        }
        return true
    }

    private fun reportEnrollmentOutcome() {
        when (_state.value.status.state) {
            "healthy" -> showMessage(COPY_ENROLL_SUCCESS, false)
            "pending_approval" -> showMessage(COPY_ENROLL_PENDING_APPROVAL, false)
        }
    }

    // MARK: - Enable / disable

    fun enable() {
        if (_state.value.op != TailscaleOp.NONE || _state.value.passwordGateBlocked) return
        viewModelScope.launch {
            stopPolling()
            try {
                setOp(TailscaleOp.ENABLING)
                showMessage(COPY_ENABLE_ACCEPTED, false)
                try {
                    withReauth { agentClient.postJSON(PATH_ENABLE) }
                } catch (e: AgentError.ApiError) {
                    setOp(TailscaleOp.NONE)
                    handleEnablePathError(e)
                    return@launch
                } catch (e: Exception) {
                    setOp(TailscaleOp.NONE)
                    setError(e.message)
                    return@launch
                }
                clearPasswordGate()
                // respawning and a transient paused are both expected here for up to ~2 minutes
                // and are not failures.
                val settled = awaitStatus(ENABLE_CAP_POLLS) { it.state in ENABLE_TERMINAL_STATES }
                setOp(TailscaleOp.NONE)
                if (!connectionLost) showMessage(if (settled) null else COPY_ENABLE_ELAPSED, false)
            } finally {
                startPolling()
            }
        }
    }

    fun requestDisable() {
        if (!_state.value.actionsEnabled) return
        _state.value = _state.value.copy(showDisableConfirm = true)
    }

    fun dismissDisableConfirm() {
        _state.value = _state.value.copy(showDisableConfirm = false)
    }

    fun disable() {
        _state.value = _state.value.copy(showDisableConfirm = false)
        if (_state.value.op != TailscaleOp.NONE) return
        viewModelScope.launch {
            stopPolling()
            try {
                setOp(TailscaleOp.DISABLING)
                try {
                    withReauth { agentClient.postJSONSlow(PATH_DISABLE) }
                } catch (e: AgentError.ApiError) {
                    setOp(TailscaleOp.NONE)
                    setError(e.serverMessage)
                    return@launch
                } catch (e: Exception) {
                    setOp(TailscaleOp.NONE)
                    setError(e.message)
                    return@launch
                }
                showMessage(COPY_DISABLE_SUCCESS, false)
                // kill_child only SIGTERMs and the supervisor clears child_pid afterwards, so
                // /status keeps reporting a live daemon for a moment. TURNING_OFF absorbs it.
                awaitStatus(DISABLE_CAP_POLLS) { it.state == "disabled" }
                setOp(TailscaleOp.NONE)
            } finally {
                startPolling()
            }
        }
    }

    // MARK: - Settings

    fun setHostname(value: String) {
        _state.value = _state.value.copy(editHostname = value, hostnameError = null)
    }

    fun setRoutes(value: String) {
        _state.value = _state.value.copy(editRoutes = value, routesError = null)
    }

    fun setSsh(value: Boolean) {
        _state.value = _state.value.copy(editSsh = value)
    }

    fun setWakelock(value: Boolean) {
        _state.value = _state.value.copy(editWakelock = value)
    }

    fun requestApplyConfig() {
        val s = _state.value
        val hostnameError = TailscaleValidator.hostname(s.editHostname)
        val routesError = TailscaleValidator.routes(s.editRoutes)
        if (hostnameError != null || routesError != null) {
            _state.value = s.copy(hostnameError = hostnameError, routesError = routesError)
            return
        }
        if (s.configPatch.isEmpty()) return

        val confirms = mutableListOf<TailscaleConfigConfirm>()
        if (s.editSsh && !s.status.tailscaleSsh) confirms.add(TailscaleConfigConfirm.AllowSsh)
        TailscaleValidator.parseRoutes(s.editRoutes)
            .filterNot { it in s.status.advertiseRoutes }
            .forEach { confirms.add(TailscaleConfigConfirm.AdvertiseRoute(it)) }
        if (!s.editWakelock && s.status.holdWakelock) confirms.add(TailscaleConfigConfirm.AllowSleep)

        if (confirms.isEmpty()) applyConfig() else _state.value = s.copy(configConfirms = confirms)
    }

    fun acceptConfigConfirm() {
        val rest = _state.value.configConfirms.drop(1)
        _state.value = _state.value.copy(configConfirms = rest)
        if (rest.isEmpty()) applyConfig()
    }

    fun dismissConfigConfirms() {
        _state.value = _state.value.copy(configConfirms = emptyList())
    }

    private fun applyConfig() {
        val patch = _state.value.configPatch
        if (patch.isEmpty() || _state.value.op != TailscaleOp.NONE) return
        viewModelScope.launch {
            stopPolling()
            try {
                setOp(TailscaleOp.SAVING_CONFIG)
                _state.value = _state.value.copy(isLoading = true)
                try {
                    val data = withReauth { agentClient.putJSONSlow(PATH_CONFIG, patch) }
                    // The 200 body IS the refreshed status payload — no follow-up GET.
                    val status = TailscaleParser.parse(data)
                    adoptStatus(status)
                    seedBuffers(status)
                    // The agent only shells `tailscale set` when the daemon is live and enrolled;
                    // otherwise it silently persists for next start. Both return 200.
                    val applied = status.enabled && status.daemonRunning && status.enrolled
                    showMessage(if (applied) COPY_CONFIG_APPLIED else COPY_CONFIG_PERSISTED, false)
                } catch (e: AgentError.ApiError) {
                    if (e.status == 502) {
                        // Transactional: the patch was applied to a candidate copy and nothing persisted.
                        setError(COPY_CONFIG_REJECTED)
                        fetchStatus()
                        seedBuffers(_state.value.status)
                    } else {
                        setError(e.serverMessage)
                    }
                } catch (e: Exception) {
                    setError(e.message)
                }
                setOp(TailscaleOp.NONE)
                _state.value = _state.value.copy(isLoading = false)
            } finally {
                startPolling()
            }
        }
    }

    // MARK: - Logout

    fun requestLogout() {
        if (!_state.value.actionsEnabled || _state.value.status.updating) return
        _state.value = _state.value.copy(showLogoutConfirm = true)
    }

    fun dismissLogoutConfirm() {
        _state.value = _state.value.copy(showLogoutConfirm = false)
    }

    /** A UX gate against accidental taps and borrowed phones — never transmitted, never server-enforced. */
    fun verifyPassword(input: String): Boolean {
        val saved = authManager.savedPassword
        return saved.isNotBlank() && input == saved
    }

    fun logout() {
        _state.value = _state.value.copy(showLogoutConfirm = false)
        if (_state.value.op != TailscaleOp.NONE) return
        viewModelScope.launch {
            stopPolling()
            try {
                setOp(TailscaleOp.LOGGING_OUT)
                try {
                    val data = withReauth { agentClient.postJSONSlow(PATH_LOGOUT) }
                    // Logout returns 200 even when it fails; `warning` is the only failure channel.
                    val warning = TailscaleParser.asString(data["warning"])
                    if (warning.isNullOrEmpty()) {
                        showMessage(COPY_LOGOUT_SUCCESS, false)
                    } else {
                        _state.value = _state.value.copy(logoutWarning = warning)
                        showMessage(null, false)
                    }
                } catch (e: AgentError.ApiError) {
                    if (e.status == 409) setError(COPY_LOGOUT_BLOCKED) else setError(e.serverMessage)
                } catch (_: AgentError.Timeout) {
                    // The local identity is destroyed unconditionally, so this is not a failure —
                    // but we never saw the body, so we cannot claim the node key was invalidated.
                    showMessage(COPY_LOGOUT_AMBIGUOUS, false)
                } catch (e: Exception) {
                    setError(e.message)
                }
                setOp(TailscaleOp.NONE)
                fetchStatus()
            } finally {
                startPolling()
            }
        }
    }

    fun ackLogoutWarning() {
        _state.value = _state.value.copy(logoutWarning = null)
    }

    fun noteCopiedToClipboard() {
        showMessage(COPY_COPIED, false)
    }

    // MARK: - Install / update

    fun openUpdateSheet() {
        if (!_state.value.actionsEnabled) return
        _state.value = _state.value.copy(showUpdateSheet = true)
        fetchLatestRelease()
    }

    fun dismissUpdateSheet() {
        _state.value = _state.value.copy(showUpdateSheet = false)
    }

    fun fetchLatestRelease() {
        viewModelScope.launch {
            _state.value = _state.value.copy(releaseFetching = true, releaseError = null, release = null)
            val result = resolveLatestRelease()
            _state.value = _state.value.copy(
                releaseFetching = false,
                release = result.getOrNull(),
                releaseError = if (result.isFailure) {
                    result.exceptionOrNull()?.message ?: COPY_RELEASE_FAILED
                } else {
                    null
                },
            )
        }
    }

    fun update(version: String, sha256: String) {
        if (_state.value.op != TailscaleOp.NONE) return
        viewModelScope.launch {
            stopPolling()
            try {
                dismissUpdateSheet()
                setOp(TailscaleOp.UPDATING)
                showMessage(COPY_UPDATE_IN_PROGRESS, false)
                var watching: String? = version
                try {
                    withReauth {
                        agentClient.postJSON(PATH_UPDATE, mapOf("version" to version, "sha256" to sha256))
                    }
                } catch (e: AgentError.ApiError) {
                    // 409 means an update is already running — reconcile from status rather than
                    // erroring. It isn't necessarily ours, so we can't cross-check the version.
                    if (e.status == 409) {
                        watching = null
                    } else {
                        setOp(TailscaleOp.NONE)
                        setError(e.serverMessage)
                        return@launch
                    }
                } catch (e: Exception) {
                    setOp(TailscaleOp.NONE)
                    setError(e.message)
                    return@launch
                }
                watchUpdate(watching)
            } finally {
                startPolling()
            }
        }
    }

    private suspend fun watchUpdate(requestedVersion: String?) {
        repeat(UPDATE_CAP_POLLS) {
            delay(POLL_ACTIVE_MS)
            fetchStatus()
            if (!_state.value.status.updating) {
                setOp(TailscaleOp.NONE)
                reportUpdateOutcome(requestedVersion)
                return
            }
        }
        // Still updating at the cap: not a failure. Drop the local op so status.updating alone
        // holds the phase, and let a later entry pick the watch back up.
        setOp(TailscaleOp.NONE)
    }

    private fun reportUpdateOutcome(requestedVersion: String?) {
        val status = _state.value.status
        val last = status.lastUpdate
        when {
            // last_update is in-memory only, so an agent restart during the update erases it.
            last == null -> showMessage(COPY_UPDATE_UNKNOWN, true)
            !last.ok -> showMessage("Update failed: ${last.error ?: "unknown error"}", true)
            requestedVersion != null && status.pinnedVersion != requestedVersion ->
                showMessage(COPY_UPDATE_VERSION_UNCHANGED, true)
            else -> showMessage("Updated to ${last.version}.", false)
        }
    }

    private suspend fun resolveLatestRelease(): Result<TailscaleRelease> = withContext(Dispatchers.IO) {
        val indexBody = try {
            httpGetText(RELEASE_INDEX_URL)
        } catch (_: Exception) {
            return@withContext Result.failure(ReleaseLookupException(COPY_RELEASE_FAILED))
        }
        val index = runCatching { Json.parseToJsonElement(indexBody) as? JsonObject }.getOrNull()
            ?: return@withContext Result.failure(ReleaseLookupException(COPY_RELEASE_FAILED))

        // The device is aarch64 and the agent's download URL hardcodes arm64 — any other arch
        // would download a tarball whose checksum can never match.
        val arm64 = (index["Tarballs"] as? JsonObject)?.get("arm64").asJsonString().orEmpty()
        if (arm64.isBlank()) {
            return@withContext Result.failure(ReleaseLookupException(COPY_RELEASE_NO_ARM64))
        }
        val version = index["TarballsVersion"].asJsonString()?.trim().orEmpty()
        if (TailscaleValidator.version(version) != null) {
            return@withContext Result.failure(ReleaseLookupException(COPY_RELEASE_FAILED))
        }

        val shaBody = try {
            httpGetText("$RELEASE_BASE/tailscale_${version}_arm64.tgz.sha256")
        } catch (_: Exception) {
            return@withContext Result.failure(ReleaseLookupException(COPY_RELEASE_FAILED))
        }
        // Today the body is a bare hash, but "<hash>  <filename>" is the usual sidecar shape.
        val sha = shaBody.trim().split(Regex("\\s+")).firstOrNull().orEmpty().lowercase()
        if (TailscaleValidator.sha256(sha) != null) {
            return@withContext Result.failure(ReleaseLookupException(COPY_RELEASE_BAD_SHA))
        }
        Result.success(TailscaleRelease(version = version, sha256 = sha))
    }

    /** No Authorization header, HTTPS only — this call leaves the router's trust boundary. */
    private fun httpGetText(url: String): String {
        val request = Request.Builder().url(url).get().build()
        releaseClient.newCall(request).execute().use { response ->
            if (!response.isSuccessful) throw ReleaseLookupException("HTTP ${response.code}")
            return response.body?.string() ?: throw ReleaseLookupException("empty body")
        }
    }

    // MARK: - Polling

    private fun startPolling() {
        pollingJob?.cancel()
        pollingJob = viewModelScope.launch {
            while (isActive) {
                delay(pollIntervalMs())
                fetchStatus()
            }
        }
    }

    private fun stopPolling() {
        pollingJob?.cancel()
        pollingJob = null
    }

    private fun pollIntervalMs(): Long {
        val s = _state.value
        if (s.op != TailscaleOp.NONE) return POLL_ACTIVE_MS
        return when (s.phase) {
            TailscalePhase.UPDATING,
            TailscalePhase.STARTING,
            TailscalePhase.PAUSED,
            TailscalePhase.TURNING_OFF,
            TailscalePhase.LOGGING_OUT,
            TailscalePhase.ENROLLING,
            TailscalePhase.VERIFYING_ENROLLMENT,
            TailscalePhase.AWAITING_BROWSER_LOGIN,
            TailscalePhase.PENDING_APPROVAL,
            TailscalePhase.DEGRADED,
            TailscalePhase.UNKNOWN,
            -> POLL_ACTIVE_MS
            else -> POLL_IDLE_MS
        }
    }

    /** Bounded: `state` can legitimately sit in connecting/starting forever. */
    private suspend fun awaitStatus(maxPolls: Int, predicate: (TailscaleStatus) -> Boolean): Boolean {
        repeat(maxPolls) {
            delay(POLL_ACTIVE_MS)
            fetchStatus()
            if (predicate(_state.value.status)) return true
            if (pollFailures >= MAX_POLL_FAILURES) return false
        }
        return false
    }

    /**
     * The poll loop stays silent on failure — the circuit breaker speaks for it, and a screen that
     * times out by design would otherwise flash errors. Only refresh() reports: that copy is the
     * refresh surface, and every op cap carries its own copy already guarded by `connectionLost`.
     */
    private suspend fun fetchStatus(reportFailure: Boolean = false): Boolean =
        try {
            adoptStatus(TailscaleParser.parse(withReauth { agentClient.getJSON(PATH_STATUS) }))
            pollFailures = 0
            true
        } catch (e: CancellationException) {
            // Cancelling the poll loop must not read as a router failure.
            throw e
        } catch (e: Exception) {
            pollFailures++
            if (pollFailures >= MAX_POLL_FAILURES) {
                stopPolling()
                showMessage(COPY_LOST_CONNECTION, true)
            } else if (reportFailure) {
                setError("Failed to load Tailscale status: ${e.message}")
            }
            false
        }

    /**
     * Bearer tokens expire after an hour and this screen long-polls across that boundary. A 401 is
     * rejected by the agent's auth middleware before any handler runs, so replaying the call —
     * including /setup — cannot have consumed an auth key. Guarded so an endpoint that keeps
     * returning 401 can't loop while reauth keeps succeeding.
     */
    private suspend fun <T> withReauth(block: suspend () -> T): T =
        try {
            block().also { reauthAttempted = false }
        } catch (e: AgentError.Unauthorized) {
            if (reauthAttempted || !authManager.reauthenticate()) throw e
            reauthAttempted = true
            block().also { reauthAttempted = false }
        }

    // MARK: - State plumbing

    private fun adoptStatus(status: TailscaleStatus) {
        val s = _state.value
        var next = s.copy(
            status = status,
            degradedPolls = if (status.state == "degraded") s.degradedPolls + 1 else 0,
            isRemoteSession = computeRemoteSession(status),
        )
        // Seed the edit buffers once. Re-seeding from a poll would erase the user's typing.
        if (!s.initialLoadDone) {
            next = next.copy(
                initialLoadDone = true,
                editHostname = status.hostname,
                editRoutes = status.advertiseRoutes.joinToString(", "),
                editSsh = status.tailscaleSsh,
                editWakelock = status.holdWakelock,
            )
        }
        _state.value = next
    }

    private fun adoptLoginUrl(url: String) {
        _state.value = _state.value.copy(status = _state.value.status.copy(loginUrl = url))
    }

    private fun seedBuffers(status: TailscaleStatus) {
        _state.value = _state.value.copy(
            editHostname = status.hostname,
            editRoutes = status.advertiseRoutes.joinToString(", "),
            editSsh = status.tailscaleSsh,
            editWakelock = status.holdWakelock,
            hostnameError = null,
            routesError = null,
        )
    }

    /**
     * Heuristic and it fails open: it can't see a tailnet-side proxy, a custom DNS alias, or a
     * subnet route to the LAN address through the tunnel. It supplements the always-on wording in
     * the destructive confirms, never replaces it.
     */
    private fun computeRemoteSession(status: TailscaleStatus): Boolean {
        val host = runCatching { URI(agentClient.baseURL).host }.getOrNull() ?: return false
        if (status.tailscaleIps.any { it == host }) return true
        if (status.displayDnsName?.equals(host, ignoreCase = true) == true) return true
        val octets = host.split('.')
        val nums = octets.mapNotNull { part -> part.toIntOrNull()?.takeIf { it in 0..255 } }
        // 100.64.0.0/10 — the CGNAT range tailnet addresses are drawn from.
        return octets.size == 4 && nums.size == 4 && nums[0] == 100 && nums[1] in 64..127
    }

    private suspend fun handleEnablePathError(e: AgentError.ApiError) {
        when (e.status) {
            403 -> blockPasswordGate(e.serverMessage)
            // The same "update in progress" condition is a 409 on setup/login-url/logout but a
            // 500 here, so a 500 from /enable must not be read as permanent.
            500 -> {
                fetchStatus()
                if (_state.value.status.updating) showMessage(COPY_UPDATE_IN_PROGRESS, false)
                else setError(e.serverMessage)
            }
            else -> setError(e.serverMessage)
        }
    }

    private fun blockPasswordGate(serverMessage: String) {
        _state.value = _state.value.copy(passwordGateMessage = serverMessage, message = null)
    }

    private fun clearPasswordGate() {
        _state.value = _state.value.copy(passwordGateMessage = null)
    }

    private fun setOp(op: TailscaleOp) {
        _state.value = _state.value.copy(op = op)
    }

    private fun showMessage(text: String?, isError: Boolean) {
        _state.value = _state.value.copy(message = text, messageIsError = isError)
    }

    private fun setError(msg: String?) {
        _state.value = _state.value.copy(
            isLoading = false,
            message = msg ?: "Unknown error",
            messageIsError = true,
        )
    }
}

private class ReleaseLookupException(message: String) : Exception(message)

private fun JsonElement?.asJsonString(): String? = (this as? JsonPrimitive)?.contentOrNull
