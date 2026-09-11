package com.openu60.feature.router.device

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.openu60.core.model.DeviceParser
import com.openu60.core.network.AgentClient
import com.openu60.core.network.AgentError
import com.openu60.core.network.AuthManager
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

data class DeviceControlState(
    val chargeControlLoaded: Boolean = false,
    val powerSaveLoaded: Boolean = false,
    val fastBootLoaded: Boolean = false,
    val autoSleepLoaded: Boolean = false,
    val chargeLimitEnabled: Boolean = false,
    val chargeLimit: Int = 100,
    val hysteresis: Int = 5,
    val powerSave: Boolean = false,
    val fastBoot: Boolean = false,
    val autoSleepEnabled: Boolean = false,
    val autoSleepTimeout: String = "5",
    val isLoading: Boolean = false,
    val message: String? = null,
    val messageIsError: Boolean = false,
    val showRebootConfirm: Boolean = false,
    val showResetConfirm: Boolean = false,
)

@HiltViewModel
class DeviceControlViewModel @Inject constructor(
    private val agentClient: AgentClient,
    private val authManager: AuthManager,
) : ViewModel() {

    private val _state = MutableStateFlow(DeviceControlState())
    val state: StateFlow<DeviceControlState> = _state.asStateFlow()

    fun refresh() {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, message = null)
            try {
                // Charge control
                try {
                    val resp = agentClient.getJSON("/api/device/charge-control")
                    @Suppress("UNCHECKED_CAST")
                    val data = resp["data"] as? Map<String, Any?> ?: resp
                    _state.value = _state.value.copy(
                        chargeLimitEnabled = data["charge_limit_enabled"] as? Boolean ?: false,
                        chargeLimit = DeviceParser.asInt(data["charge_limit"]) ?: 100,
                        hysteresis = DeviceParser.asInt(data["hysteresis"]) ?: 5,
                        chargeControlLoaded = true,
                    )
                } catch (_: Exception) {}

                // Power save
                try {
                    val psData = agentClient.getJSON("/api/device/power-save")
                    _state.value = _state.value.copy(
                        powerSave = DeviceParser.asBool(psData["power_saver_mode"]),
                        powerSaveLoaded = true,
                    )
                } catch (_: Exception) {}

                // Fast boot
                try {
                    val fbData = agentClient.getJSON("/api/device/fast-boot")
                    _state.value = _state.value.copy(
                        fastBoot = DeviceParser.asBool(fbData["fast_boot"]),
                        fastBootLoaded = true,
                    )
                } catch (_: Exception) {}

                // Auto sleep
                try {
                    val asData = agentClient.getJSON("/api/device/auto-sleep")
                    @Suppress("UNCHECKED_CAST")
                    val asInner = asData["data"] as? Map<String, Any?> ?: asData
                    _state.value = _state.value.copy(
                        autoSleepEnabled = asInner["enabled"] as? Boolean ?: false,
                        autoSleepTimeout = asInner["timeout"]?.toString() ?: "5",
                        autoSleepLoaded = true,
                    )
                } catch (_: Exception) {}

                _state.value = _state.value.copy(isLoading = false)
            } catch (e: AgentError.Unauthorized) {
                if (authManager.reauthenticate()) refresh() else setError(e.message)
            } catch (e: Exception) {
                setError(e.message)
            }
        }
    }

    fun setChargeLimit(enabled: Boolean, limit: Int, hysteresis: Int? = null) {
        val prevEnabled = _state.value.chargeLimitEnabled
        val prevLimit = _state.value.chargeLimit
        val prevHysteresis = _state.value.hysteresis
        _state.value = _state.value.copy(
            chargeLimitEnabled = enabled,
            chargeLimit = limit,
            hysteresis = hysteresis ?: _state.value.hysteresis,
            message = null,
        )
        viewModelScope.launch {
            try {
                val body = mutableMapOf<String, Any>(
                    "charge_limit_enabled" to enabled,
                    "charge_limit" to limit,
                )
                if (hysteresis != null) body["hysteresis"] = hysteresis
                val result = agentClient.putJSON("/api/device/charge-control", body)
                @Suppress("UNCHECKED_CAST")
                val data = result["data"] as? Map<String, Any?>
                val newEnabled = data?.get("charge_limit_enabled") as? Boolean ?: enabled
                val newLimit = DeviceParser.asInt(data?.get("charge_limit")) ?: limit
                val newHysteresis = DeviceParser.asInt(data?.get("hysteresis")) ?: _state.value.hysteresis
                _state.value = _state.value.copy(
                    chargeLimitEnabled = newEnabled,
                    chargeLimit = newLimit,
                    hysteresis = newHysteresis,
                )
            } catch (e: AgentError.Unauthorized) {
                _state.value = _state.value.copy(chargeLimitEnabled = prevEnabled, chargeLimit = prevLimit, hysteresis = prevHysteresis)
                if (authManager.reauthenticate()) setChargeLimit(enabled, limit, hysteresis) else setError(e.message)
            } catch (e: Exception) {
                _state.value = _state.value.copy(chargeLimitEnabled = prevEnabled, chargeLimit = prevLimit, hysteresis = prevHysteresis)
                setError(e.message)
            }
        }
    }

    fun togglePowerSave(enabled: Boolean) {
        val prev = _state.value.powerSave
        _state.value = _state.value.copy(powerSave = enabled, message = null)
        viewModelScope.launch {
            try {
                agentClient.putJSON("/api/device/power-save", mapOf(
                    "deviceInfoList" to mapOf("power_saver_mode" to if (enabled) "1" else "0"),
                ))
            } catch (e: AgentError.Unauthorized) {
                _state.value = _state.value.copy(powerSave = prev)
                if (authManager.reauthenticate()) togglePowerSave(enabled) else setError(e.message)
            } catch (e: Exception) {
                _state.value = _state.value.copy(powerSave = prev)
                setError(e.message)
            }
        }
    }

    fun toggleFastBoot(enabled: Boolean) {
        val prev = _state.value.fastBoot
        _state.value = _state.value.copy(fastBoot = enabled, message = null)
        viewModelScope.launch {
            try {
                agentClient.putJSON("/api/device/fast-boot", mapOf(
                    "fast_boot" to if (enabled) "1" else "0",
                ))
            } catch (e: AgentError.Unauthorized) {
                _state.value = _state.value.copy(fastBoot = prev)
                if (authManager.reauthenticate()) toggleFastBoot(enabled) else setError(e.message)
            } catch (e: Exception) {
                _state.value = _state.value.copy(fastBoot = prev)
                setError(e.message)
            }
        }
    }

    fun setAutoSleep(enabled: Boolean, timeout: String? = null) {
        val prevEnabled = _state.value.autoSleepEnabled
        val prevTimeout = _state.value.autoSleepTimeout
        _state.value = _state.value.copy(
            autoSleepEnabled = enabled,
            autoSleepTimeout = timeout ?: _state.value.autoSleepTimeout,
            message = null,
        )
        viewModelScope.launch {
            try {
                val body = mutableMapOf<String, Any>("enabled" to enabled)
                if (timeout != null) body["timeout"] = timeout
                agentClient.putJSON("/api/device/auto-sleep", body)
            } catch (e: AgentError.Unauthorized) {
                _state.value = _state.value.copy(autoSleepEnabled = prevEnabled, autoSleepTimeout = prevTimeout)
                if (authManager.reauthenticate()) setAutoSleep(enabled, timeout) else setError(e.message)
            } catch (e: Exception) {
                _state.value = _state.value.copy(autoSleepEnabled = prevEnabled, autoSleepTimeout = prevTimeout)
                setError(e.message)
            }
        }
    }

    fun showRebootConfirm() { _state.value = _state.value.copy(showRebootConfirm = true) }
    fun dismissRebootConfirm() { _state.value = _state.value.copy(showRebootConfirm = false) }
    fun showResetConfirm() { _state.value = _state.value.copy(showResetConfirm = true) }
    fun dismissResetConfirm() { _state.value = _state.value.copy(showResetConfirm = false) }

    fun reboot() {
        _state.value = _state.value.copy(showRebootConfirm = false, isLoading = true, message = null)
        viewModelScope.launch {
            try {
                agentClient.postJSON("/api/device/reboot")
                _state.value = _state.value.copy(isLoading = false, message = "Rebooting...", messageIsError = false)
            } catch (e: AgentError.Unauthorized) {
                if (authManager.reauthenticate()) reboot() else setError(e.message)
            } catch (e: Exception) {
                setError(e.message)
            }
        }
    }

    fun factoryReset() {
        _state.value = _state.value.copy(showResetConfirm = false, isLoading = true, message = null)
        viewModelScope.launch {
            try {
                agentClient.postJSON("/api/device/factory-reset")
                _state.value = _state.value.copy(isLoading = false, message = "Factory reset initiated...", messageIsError = false)
            } catch (e: AgentError.Unauthorized) {
                if (authManager.reauthenticate()) factoryReset() else setError(e.message)
            } catch (e: Exception) {
                setError(e.message)
            }
        }
    }

    private fun setError(msg: String?) {
        _state.value = _state.value.copy(isLoading = false, message = msg ?: "Unknown error", messageIsError = true)
    }
}
