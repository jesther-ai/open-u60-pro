package com.openu60.feature.dashboard

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.openu60.R
import com.openu60.core.components.AnimatedNumber
import com.openu60.core.model.DeviceParser
import com.openu60.core.network.AuthState

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DashboardScreen(
    onNavigateToSignal: () -> Unit,
    onNavigateToLogin: () -> Unit,
    viewModel: DashboardViewModel = hiltViewModel(),
) {
    val authState by viewModel.authState.collectAsState()
    val isLoading by viewModel.isLoading.collectAsState()
    val error by viewModel.error.collectAsState()
    val nrSignal by viewModel.nrSignal.collectAsState()
    val lteSignal by viewModel.lteSignal.collectAsState()
    val operatorInfo by viewModel.operatorInfo.collectAsState()
    val battery by viewModel.battery.collectAsState()
    val thermal by viewModel.thermal.collectAsState()
    val speed by viewModel.speed.collectAsState()
    val trafficStats by viewModel.trafficStats.collectAsState()
    val wanIPv4 by viewModel.wanIPv4.collectAsState()
    val wanIPv6 by viewModel.wanIPv6.collectAsState()
    val wifiStatus by viewModel.wifiStatus.collectAsState()
    val systemInfo by viewModel.systemInfo.collectAsState()
    val connectedDevices by viewModel.connectedDevices.collectAsState()
    val isAirplaneMode by viewModel.isAirplaneMode.collectAsState()
    val isMobileDataOff by viewModel.isMobileDataOff.collectAsState()
    val simPinRequired by viewModel.simPinRequired.collectAsState()
    val simPukRequired by viewModel.simPukRequired.collectAsState()

    if (authState != AuthState.LOGGED_IN) {
        Box(
            modifier = Modifier.fillMaxSize(),
            contentAlignment = Alignment.Center,
        ) {
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Icon(
                    Icons.Default.Router,
                    contentDescription = null,
                    modifier = Modifier.size(64.dp),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(modifier = Modifier.height(16.dp))
                Text(stringResource(R.string.status_not_connected), style = MaterialTheme.typography.titleMedium)
                Spacer(modifier = Modifier.height(8.dp))
                Text(
                    stringResource(R.string.dashboard_login_prompt),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(modifier = Modifier.height(16.dp))
                Button(onClick = onNavigateToLogin) {
                    Text(stringResource(R.string.action_login))
                }
            }
        }
        return
    }

    PullToRefreshBox(
        isRefreshing = isLoading,
        onRefresh = { viewModel.refresh() },
        modifier = Modifier.fillMaxSize(),
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            // Error card
            if (error != null) {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.errorContainer,
                    ),
                ) {
                    Text(
                        error!!,
                        modifier = Modifier.padding(16.dp),
                        color = MaterialTheme.colorScheme.onErrorContainer,
                    )
                }
            }

            // Airplane mode banner
            if (isAirplaneMode) {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = Color(0xFFFFF3E0),
                    ),
                ) {
                    Row(
                        modifier = Modifier.padding(16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(Icons.Default.AirplanemodeActive, contentDescription = null, tint = Color(0xFFE65100))
                        Spacer(modifier = Modifier.width(12.dp))
                        Text(stringResource(R.string.dashboard_airplane_mode), color = Color(0xFFE65100))
                    }
                }
            }

            // Mobile data off banner
            if (isMobileDataOff && !isAirplaneMode) {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = Color(0xFFFFF3E0),
                    ),
                ) {
                    Row(
                        modifier = Modifier.padding(16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(Icons.Default.MobiledataOff, contentDescription = null, tint = Color(0xFFE65100))
                        Spacer(modifier = Modifier.width(12.dp))
                        Text(stringResource(R.string.dashboard_mobile_data_off), color = Color(0xFFE65100))
                    }
                }
            }

            // SIM PIN/PUK required banner
            if (simPukRequired) {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.errorContainer,
                    ),
                ) {
                    Row(
                        modifier = Modifier.padding(16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(Icons.Default.SimCardAlert, contentDescription = null, tint = MaterialTheme.colorScheme.error)
                        Spacer(modifier = Modifier.width(12.dp))
                        Text(stringResource(R.string.dashboard_sim_puk_required), color = MaterialTheme.colorScheme.error)
                    }
                }
            } else if (simPinRequired) {
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = Color(0xFFFFF3E0),
                    ),
                ) {
                    Row(
                        modifier = Modifier.padding(16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(Icons.Default.SimCardAlert, contentDescription = null, tint = Color(0xFFE65100))
                        Spacer(modifier = Modifier.width(12.dp))
                        Text(stringResource(R.string.dashboard_sim_pin_required), color = Color(0xFFE65100))
                    }
                }
            }

            // Operator + network type header
            val displayType = operatorInfo.displayNetworkType(nrSignal.isConnected, lteSignal)
            if (operatorInfo.provider.isNotBlank()) {
                Text(
                    "${operatorInfo.provider} - $displayType",
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            // Signal + Battery row
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                val rsrp = nrSignal.rsrp ?: lteSignal.rsrp
                val sccCount = nrSignal.sccCarriers.size + lteSignal.sccCarriers.size
                val signalSubtitle = buildString {
                    append(stringResource(signalQualityLabelRes(rsrp)))
                    if (sccCount > 0) append(" +${sccCount}CA")
                }
                DashboardCard(
                    modifier = Modifier.weight(1f),
                    icon = Icons.Default.SignalCellular4Bar,
                    title = stringResource(if (nrSignal.isConnected) R.string.dashboard_nr_signal else R.string.dashboard_lte_signal),
                    value = if (rsrp != null) "${rsrp.toInt()} dBm" else "--",
                    subtitle = signalSubtitle,
                    valueColor = rsrpColor(rsrp),
                    onClick = onNavigateToSignal,
                    valueContent = if (rsrp != null) {
                        { AnimatedNumber(value = rsrp.toInt(), suffix = " dBm", color = rsrpColor(rsrp), style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.Bold)) }
                    } else null,
                )
                val chargingLabel = when (battery.charging) {
                    "charging" -> stringResource(R.string.dashboard_charging)
                    "stopped" -> stringResource(R.string.dashboard_charge_stopped)
                    else -> stringResource(R.string.dashboard_discharging)
                }
                val currentStr = battery.currentMA?.let { "${it}mA" } ?: ""
                val battSubtitle = listOfNotNull(
                    chargingLabel,
                    currentStr.ifEmpty { null },
                ).joinToString(" ")
                DashboardCard(
                    modifier = Modifier.weight(1f),
                    icon = Icons.Default.BatteryStd,
                    title = stringResource(R.string.dashboard_battery),
                    value = "${battery.capacity}%",
                    subtitle = battSubtitle,
                    valueColor = batteryColor(battery.capacity),
                    valueContent = { AnimatedNumber(value = battery.capacity, suffix = "%", color = batteryColor(battery.capacity), style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.Bold)) },
                )
            }

            // CPU + Cellular row
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                val cpuLabel = "%.0f%%".format(systemInfo.cpuUsagePercent)
                val cpuColor = if (systemInfo.cpuUsagePercent > 80) Color(0xFFF44336) else Color.Unspecified
                val uptimeStr = formatUptime(systemInfo.uptime)
                DashboardCard(
                    modifier = Modifier.weight(1f),
                    icon = Icons.Default.Memory,
                    title = "CPU",
                    value = cpuLabel,
                    subtitle = "${thermal.cpuTemp.toInt()}\u00B0C  $uptimeStr",
                    valueColor = cpuColor,
                    valueContent = { AnimatedNumber(value = systemInfo.cpuUsagePercent.toInt(), suffix = "%", color = cpuColor, style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.Bold)) },
                )
                val speedComp = DeviceParser.speedComponents(speed.downloadBytesPerSec)
                DashboardCard(
                    modifier = Modifier.weight(1f),
                    icon = Icons.Default.SwapVert,
                    title = stringResource(R.string.dashboard_cellular),
                    value = DeviceParser.formatSpeed(speed.downloadBytesPerSec),
                    subtitle = DeviceParser.formatBytes(trafficStats.rxBytes + trafficStats.txBytes),
                    valueContent = { AnimatedNumber(value = speedComp.number, decimalPlaces = speedComp.decimalPlaces, suffix = speedComp.unit, style = MaterialTheme.typography.headlineSmall.copy(fontWeight = FontWeight.Bold)) },
                )
            }

            // WiFi card
            if (wifiStatus.wifiOn) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Icon(
                                Icons.Default.Wifi,
                                contentDescription = null,
                                modifier = Modifier.size(18.dp),
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Spacer(modifier = Modifier.width(6.dp))
                            Text("WiFi", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        Spacer(modifier = Modifier.height(8.dp))
                        if (wifiStatus.ssid2g.isNotBlank() && !wifiStatus.radio2gDisabled) {
                            Text("2.4G: ${wifiStatus.ssid2g}", style = MaterialTheme.typography.bodyMedium)
                        }
                        if (wifiStatus.ssid5g.isNotBlank() && !wifiStatus.radio5gDisabled) {
                            Text("5G: ${wifiStatus.ssid5g}", style = MaterialTheme.typography.bodyMedium)
                        }
                        Text(
                            "${connectedDevices.size} ${stringResource(R.string.dashboard_clients)}",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }

            // WAN IPs
            if (wanIPv4.isNotBlank() || wanIPv6.isNotBlank()) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Icon(
                                Icons.Default.Language,
                                contentDescription = null,
                                modifier = Modifier.size(18.dp),
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Spacer(modifier = Modifier.width(6.dp))
                            Text("WAN", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        Spacer(modifier = Modifier.height(8.dp))
                        if (wanIPv4.isNotBlank()) {
                            Text("IPv4: $wanIPv4", style = MaterialTheme.typography.bodySmall)
                        }
                        if (wanIPv6.isNotBlank()) {
                            Text(
                                "IPv6: $wanIPv6",
                                style = MaterialTheme.typography.bodySmall,
                                maxLines = 1,
                            )
                        }
                    }
                }
            }

            // Connected devices preview
            if (connectedDevices.isNotEmpty()) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Icon(
                                Icons.Default.Devices,
                                contentDescription = null,
                                modifier = Modifier.size(18.dp),
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Spacer(modifier = Modifier.width(6.dp))
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                AnimatedNumber(
                                    value = connectedDevices.size,
                                    style = MaterialTheme.typography.labelMedium,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                                Text(
                                    " ${stringResource(R.string.dashboard_devices)}",
                                    style = MaterialTheme.typography.labelMedium,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                        Spacer(modifier = Modifier.height(8.dp))
                        connectedDevices.take(5).forEach { device ->
                            Text(
                                "${device.displayName}  ${device.ipAddress}",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        if (connectedDevices.size > 5) {
                            Text(
                                "+${connectedDevices.size - 5} ${stringResource(R.string.dashboard_more)}",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.primary,
                            )
                        }
                    }
                }
            }

            // NR band info with carriers
            val nrBand = nrSignal.band
            if (nrBand.isNotBlank()) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(stringResource(R.string.dashboard_nr_band), style = MaterialTheme.typography.labelMedium)
                            if (nrSignal.sccCarriers.isNotEmpty()) {
                                Spacer(modifier = Modifier.width(6.dp))
                                Text(
                                    "${1 + nrSignal.sccCarriers.size} CC",
                                    style = MaterialTheme.typography.labelSmall,
                                    fontWeight = FontWeight.Bold,
                                    color = Color(0xFF2196F3),
                                    modifier = Modifier
                                        .background(Color(0xFF2196F3).copy(alpha = 0.15f), shape = MaterialTheme.shapes.small)
                                        .padding(horizontal = 5.dp, vertical = 1.dp),
                                )
                            }
                        }
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(
                                "n$nrBand",
                                style = MaterialTheme.typography.titleMedium,
                                fontWeight = FontWeight.Bold,
                            )
                            if (nrSignal.sccCarriers.isNotEmpty()) {
                                Spacer(modifier = Modifier.width(6.dp))
                                Text(
                                    "PCC",
                                    style = MaterialTheme.typography.labelSmall,
                                    fontWeight = FontWeight.Bold,
                                    color = Color(0xFF2196F3),
                                    modifier = Modifier
                                        .background(Color(0xFF2196F3).copy(alpha = 0.15f), shape = MaterialTheme.shapes.small)
                                        .padding(horizontal = 5.dp, vertical = 1.dp),
                                )
                            }
                        }
                        nrSignal.sccCarriers.forEach { scc ->
                            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(top = 2.dp)) {
                                Text(
                                    "n${scc.band}",
                                    style = MaterialTheme.typography.bodySmall,
                                )
                                Spacer(modifier = Modifier.width(4.dp))
                                Text(
                                    "SCC",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier
                                        .background(MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.12f), shape = MaterialTheme.shapes.small)
                                        .padding(horizontal = 4.dp, vertical = 1.dp),
                                )
                                if (scc.rsrp != null) {
                                    Spacer(modifier = Modifier.width(4.dp))
                                    Text("${scc.rsrp.toInt()} dBm", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                }
                            }
                        }
                    }
                }
            }

            // LTE band info with carriers
            val lteBand = lteSignal.band
            val lteCaActive = lteSignal.caState != "0" && lteSignal.sccCarriers.isNotEmpty()
            val showNRBand = nrBand.isNotBlank()
            if (lteBand.isNotBlank()) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(stringResource(R.string.dashboard_lte_band), style = MaterialTheme.typography.labelMedium)
                            if (showNRBand) {
                                Spacer(modifier = Modifier.width(6.dp))
                                Text(
                                    stringResource(R.string.dashboard_anchor),
                                    style = MaterialTheme.typography.labelSmall,
                                    fontWeight = FontWeight.Bold,
                                    color = Color(0xFFFF9800),
                                    modifier = Modifier
                                        .background(Color(0xFFFF9800).copy(alpha = 0.15f), shape = MaterialTheme.shapes.small)
                                        .padding(horizontal = 5.dp, vertical = 1.dp),
                                )
                            }
                            if (lteCaActive) {
                                Spacer(modifier = Modifier.width(4.dp))
                                Text(
                                    "${1 + lteSignal.sccCarriers.size} CC",
                                    style = MaterialTheme.typography.labelSmall,
                                    fontWeight = FontWeight.Bold,
                                    color = Color(0xFFFF9800),
                                    modifier = Modifier
                                        .background(Color(0xFFFF9800).copy(alpha = 0.15f), shape = MaterialTheme.shapes.small)
                                        .padding(horizontal = 5.dp, vertical = 1.dp),
                                )
                            }
                        }
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(
                                "B$lteBand",
                                style = MaterialTheme.typography.titleMedium,
                                fontWeight = FontWeight.Bold,
                            )
                            if (lteCaActive && !showNRBand) {
                                Spacer(modifier = Modifier.width(6.dp))
                                Text(
                                    "PCC",
                                    style = MaterialTheme.typography.labelSmall,
                                    fontWeight = FontWeight.Bold,
                                    color = Color(0xFF2196F3),
                                    modifier = Modifier
                                        .background(Color(0xFF2196F3).copy(alpha = 0.15f), shape = MaterialTheme.shapes.small)
                                        .padding(horizontal = 5.dp, vertical = 1.dp),
                                )
                            }
                        }
                        if (lteCaActive) {
                            lteSignal.sccCarriers.forEach { scc ->
                                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(top = 2.dp)) {
                                    Text(
                                        "B${scc.band}",
                                        style = MaterialTheme.typography.bodySmall,
                                    )
                                    Spacer(modifier = Modifier.width(4.dp))
                                    Text(
                                        "SCC",
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        modifier = Modifier
                                            .background(MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.12f), shape = MaterialTheme.shapes.small)
                                            .padding(horizontal = 4.dp, vertical = 1.dp),
                                    )
                                    if (scc.rsrp != null) {
                                        Spacer(modifier = Modifier.width(4.dp))
                                        Text("${scc.rsrp.toInt()} dBm", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DashboardCard(
    modifier: Modifier = Modifier,
    icon: ImageVector,
    title: String,
    value: String,
    subtitle: String,
    valueColor: Color = Color.Unspecified,
    onClick: (() -> Unit)? = null,
    valueContent: (@Composable () -> Unit)? = null,
) {
    Card(
        modifier = modifier,
        onClick = onClick ?: {},
        enabled = onClick != null,
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    icon,
                    contentDescription = null,
                    modifier = Modifier.size(18.dp),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(modifier = Modifier.width(6.dp))
                Text(
                    title,
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(modifier = Modifier.height(8.dp))
            if (valueContent != null) {
                valueContent()
            } else {
                Text(
                    value,
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.Bold,
                    color = if (valueColor != Color.Unspecified) valueColor else MaterialTheme.colorScheme.onSurface,
                )
            }
            Text(
                subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

private fun rsrpColor(rsrp: Double?): Color {
    if (rsrp == null) return Color.Unspecified
    return when {
        rsrp >= -80 -> Color(0xFF4CAF50)
        rsrp >= -100 -> Color(0xFFFFEB3B)
        rsrp >= -110 -> Color(0xFFFF9800)
        else -> Color(0xFFF44336)
    }
}

private fun signalQualityLabelRes(rsrp: Double?): Int {
    if (rsrp == null) return R.string.dashboard_no_signal
    return when {
        rsrp >= -80 -> R.string.dashboard_excellent
        rsrp >= -100 -> R.string.dashboard_good
        rsrp >= -110 -> R.string.dashboard_fair
        else -> R.string.dashboard_poor
    }
}

private fun batteryColor(capacity: Int): Color {
    return when {
        capacity > 50 -> Color(0xFF4CAF50)
        capacity > 20 -> Color(0xFFFF9800)
        else -> Color(0xFFF44336)
    }
}

private fun formatUptime(seconds: Int): String {
    if (seconds <= 0) return ""
    val days = seconds / 86400
    val hours = (seconds % 86400) / 3600
    val mins = (seconds % 3600) / 60
    return when {
        days > 0 -> "${days}d ${hours}h"
        hours > 0 -> "${hours}h ${mins}m"
        else -> "${mins}m"
    }
}
