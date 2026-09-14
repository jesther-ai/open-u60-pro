package com.openu60.feature.dashboard

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.openu60.core.model.*
import com.openu60.core.network.AgentClient
import com.openu60.core.network.AgentError
import com.openu60.core.network.AuthManager
import com.openu60.core.network.AuthState
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class DashboardViewModel @Inject constructor(
    private val client: AgentClient,
    private val authManager: AuthManager,
) : ViewModel() {

    // Signal
    val nrSignal = MutableStateFlow(NRSignal.empty)
    val lteSignal = MutableStateFlow(LTESignal.empty)
    val wcdmaSignal = MutableStateFlow(WCDMASignal.empty)
    val operatorInfo = MutableStateFlow(OperatorInfo.empty)

    // Device
    val battery = MutableStateFlow(BatteryStatus.empty)
    val thermal = MutableStateFlow(ThermalStatus.empty)
    val systemInfo = MutableStateFlow(SystemInfo.empty)

    // Traffic
    val speed = MutableStateFlow(TrafficSpeed.zero)
    val trafficStats = MutableStateFlow(TrafficStats.empty)

    // Network
    val wanIPv4 = MutableStateFlow("")
    val wanIPv6 = MutableStateFlow("")
    val wifiStatus = MutableStateFlow(WifiStatus.empty)
    val connectedDevices = MutableStateFlow<List<ConnectedDevice>>(emptyList())

    // Status flags
    val isAirplaneMode = MutableStateFlow(false)
    val isMobileDataOff = MutableStateFlow(false)
    val simPinRequired = MutableStateFlow(false)
    val simPukRequired = MutableStateFlow(false)

    // UI state
    val isLoading = MutableStateFlow(false)
    val error = MutableStateFlow<String?>(null)
    val lastUpdated = MutableStateFlow(0L)

    val authState: StateFlow<AuthState> = authManager.authState

    private var pollingJob: Job? = null
    private var previousTraffic = TrafficStats.empty
    private var cpuCores = 1

    init {
        viewModelScope.launch {
            authManager.authState.collect { state ->
                if (state == AuthState.LOGGED_IN) {
                    startPolling()
                } else {
                    stopPolling()
                }
            }
        }
        viewModelScope.launch {
            authManager.autoLogin()
        }
    }

    fun refresh() {
        viewModelScope.launch { fetchAll() }
    }

    private fun startPolling() {
        pollingJob?.cancel()
        pollingJob = viewModelScope.launch {
            while (isActive) {
                fetchAll()
                delay(authManager.pollInterval * 1000L)
            }
        }
    }

    private fun stopPolling() {
        pollingJob?.cancel()
        pollingJob = null
    }

    private suspend fun fetchAll() {
        isLoading.value = true
        error.value = null
        try {
            fetchSignal()

            coroutineScope {
                val batteryJob = async { fetchBattery() }
                val thermalJob = async { fetchThermal() }
                val trafficJob = async { fetchTraffic() }
                val clientsJob = async { fetchClients() }
                val wanJob = async { fetchWan() }
                val wifiJob = async { fetchWifi() }
                val systemJob = async { fetchSystem() }
                val modemJob = async { fetchModemStatus() }
                val dataJob = async { fetchMobileDataStatus() }
                val simJob = async { fetchSimStatus() }

                batteryJob.await()
                thermalJob.await()
                trafficJob.await()
                clientsJob.await()
                wanJob.await()
                wifiJob.await()
                systemJob.await()
                modemJob.await()
                dataJob.await()
                simJob.await()
            }

            lastUpdated.value = System.currentTimeMillis()
        } catch (e: AgentError.Unauthorized) {
            if (authManager.reauthenticate()) {
                try {
                    fetchSignal()
                } catch (_: Exception) {
                    error.value = "Authentication failed"
                }
            } else {
                error.value = "Session expired. Please log in again."
            }
        } catch (e: Exception) {
            error.value = e.message ?: "Unknown error"
        }
        isLoading.value = false
    }

    private suspend fun fetchSignal() {
        try {
            val data = client.getJSON("/api/network/signal")
            val result = SignalParser.parseNetInfo(data)
            nrSignal.value = result.nr
            lteSignal.value = result.lte
            wcdmaSignal.value = result.wcdma
            operatorInfo.value = result.operatorInfo
        } catch (e: AgentError.Unauthorized) {
            throw e
        } catch (_: Exception) {}
    }

    private suspend fun fetchBattery() {
        try {
            val battData = client.getJSON("/api/device/battery-info")
            var bat = DeviceParser.parseBattery(battData)

            try {
                val chargerData = client.getJSON("/api/device/charger")
                val chargeCtrl = try { client.getJSON("/api/device/charge-control") } catch (_: Exception) { null }
                bat = DeviceParser.parseCharger(chargerData, bat, chargeCtrl)
            } catch (_: Exception) {}

            try {
                val rawBatt = client.getJSON("/api/battery")
                val currentUA = DeviceParser.asInt(rawBatt["current_ua"])
                val voltageUV = DeviceParser.asInt(rawBatt["voltage_uv"])
                if (currentUA != null) {
                    bat = bat.copy(currentMA = currentUA / 1000)
                }
                if (voltageUV != null) {
                    bat = bat.copy(voltageMV = voltageUV / 1000)
                }
            } catch (_: Exception) {}

            battery.value = bat
        } catch (_: Exception) {}
    }

    private suspend fun fetchThermal() {
        try {
            val data = client.getJSON("/api/device/thermal")
            thermal.value = DeviceParser.parseThermal(data)
        } catch (_: Exception) {}
    }

    private suspend fun fetchTraffic() {
        try {
            // Tier 1: Agent-computed speed endpoint
            try {
                val speedData = client.getJSON("/api/network/speed")
                val sRx = DeviceParser.asDouble(speedData["rx_bytes_per_sec"])
                val sTx = DeviceParser.asDouble(speedData["tx_bytes_per_sec"])
                if (sRx != null && sTx != null) {
                    speed.value = TrafficSpeed(downloadBytesPerSec = sRx, uploadBytesPerSec = sTx)
                }
            } catch (_: Exception) {}

            // Tier 2: Agent traffic endpoint (interface list)
            try {
                val trafficData = client.getJSON("/api/network/traffic")
                val rxTotal = DeviceParser.asLong(trafficData["rx_bytes"])
                val txTotal = DeviceParser.asLong(trafficData["tx_bytes"])
                if (rxTotal != null && txTotal != null) {
                    val current = TrafficStats(
                        rxBytes = rxTotal, txBytes = txTotal,
                        timestamp = System.currentTimeMillis(), source = "agent_traffic",
                    )
                    if (speed.value == TrafficSpeed.zero) {
                        speed.value = DeviceParser.computeSpeed(previousTraffic, current)
                    }
                    trafficStats.value = current
                    previousTraffic = current
                    return
                }
            } catch (_: Exception) {}

            // Tier 3: wwandst
            try {
                val wwData = client.getJSON("/api/network/speeds")
                val wwTraffic = DeviceParser.parseWwandstTraffic(wwData)
                if (wwTraffic != null) {
                    if (speed.value == TrafficSpeed.zero) {
                        speed.value = DeviceParser.computeSpeed(previousTraffic, wwTraffic)
                    }
                    trafficStats.value = wwTraffic
                    previousTraffic = wwTraffic
                    return
                }
            } catch (_: Exception) {}

            // Tier 4: rmnet delta
            try {
                val rmData = client.getJSON("/api/network/rmnet")
                val rxBytes = DeviceParser.asLong(rmData["rx_bytes"])
                val txBytes = DeviceParser.asLong(rmData["tx_bytes"])
                if (rxBytes != null && txBytes != null) {
                    val current = TrafficStats(
                        rxBytes = rxBytes, txBytes = txBytes,
                        timestamp = System.currentTimeMillis(), source = "rmnet",
                    )
                    if (speed.value == TrafficSpeed.zero) {
                        speed.value = DeviceParser.computeSpeed(previousTraffic, current)
                    }
                    trafficStats.value = current
                    previousTraffic = current
                    return
                }
            } catch (_: Exception) {}
        } catch (_: Exception) {}
    }

    private suspend fun fetchClients() {
        try {
            val data = client.getJSON("/api/network/clients")
            var devices = DeviceParser.parseHostHints(data)
            try {
                val leases = client.getJSONArray("/api/network/clients")
                devices = DeviceParser.enrichWithDHCP(devices, leases)
            } catch (_: Exception) {}
            connectedDevices.value = devices
        } catch (_: Exception) {}
    }

    private suspend fun fetchWan() {
        try {
            val wan4 = client.getJSON("/api/network/wan")
            wanIPv4.value = DeviceParser.parseWanIPv4(wan4)
        } catch (_: Exception) {}
        try {
            val wan6 = client.getJSON("/api/network/wan6")
            wanIPv6.value = DeviceParser.parseWanIPv6(wan6)
        } catch (_: Exception) {}
    }

    private suspend fun fetchWifi() {
        try {
            val data = client.getJSON("/api/wifi/status")
            wifiStatus.value = DeviceParser.parseWifiStatus(data)
        } catch (_: Exception) {}
    }

    private suspend fun fetchSystem() {
        try {
            val cpuData = client.getJSON("/api/cpu")
            val cores = DeviceParser.asInt(cpuData["cores"])
            if (cores != null && cores > 0) cpuCores = cores
            val usage = DeviceParser.asDouble(cpuData["usage_percent"])
            if (usage != null) {
                systemInfo.value = systemInfo.value.copy(
                    cpuUsagePercent = usage,
                    cpuUsageIsEstimate = false,
                    cpuCores = cpuCores,
                )
            }
        } catch (_: Exception) {}

        try {
            val sysData = client.getJSON("/api/device/system")
            val info = DeviceParser.parseSystemInfo(sysData, cpuCores)
            val current = systemInfo.value
            systemInfo.value = if (!current.cpuUsageIsEstimate) {
                info.copy(
                    cpuUsagePercent = current.cpuUsagePercent,
                    cpuUsageIsEstimate = false,
                    cpuCores = cpuCores,
                )
            } else {
                info.copy(cpuCores = cpuCores)
            }
        } catch (_: Exception) {}
    }

    private suspend fun fetchModemStatus() {
        try {
            val data = client.getJSON("/api/modem/status")
            val mode = data["operate_mode"] as? String ?: ""
            isAirplaneMode.value = mode.isNotEmpty() && mode != "ONLINE"
        } catch (_: Exception) {}
    }

    private suspend fun fetchMobileDataStatus() {
        try {
            val data = client.getJSON("/api/modem/data")
            val enabled = DeviceParser.asInt(data["enable"])
            if (enabled != null) {
                isMobileDataOff.value = enabled == 0
            }
        } catch (_: Exception) {}
    }

    private suspend fun fetchSimStatus() {
        try {
            val data = client.getJSON("/api/sim/info")
            val simStates = data["sim_states"] as? String ?: ""
            val modemState = data["modem_main_state"] as? String ?: ""
            simPinRequired.value = simStates.contains("PIN", ignoreCase = true)
                || modemState.contains("SIM PIN", ignoreCase = true)
            simPukRequired.value = simStates.contains("PUK", ignoreCase = true)
                || modemState.contains("SIM PUK", ignoreCase = true)
        } catch (_: Exception) {}
    }

    override fun onCleared() {
        super.onCleared()
        stopPolling()
    }
}
