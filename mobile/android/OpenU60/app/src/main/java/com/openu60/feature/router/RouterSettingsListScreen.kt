package com.openu60.feature.router

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RouterSettingsListScreen(
    onNavigateToMobileNetwork: () -> Unit,
    onNavigateToNetworkMode: () -> Unit,
    onNavigateToCellLock: () -> Unit,
    onNavigateToSTC: () -> Unit,
    onNavigateToSignalDetect: () -> Unit,
    onNavigateToSIM: () -> Unit,
    onNavigateToSTK: () -> Unit,
    onNavigateToWiFi: () -> Unit,
    onNavigateToGuestWiFi: () -> Unit,
    onNavigateToAPN: () -> Unit,
    onNavigateToLAN: () -> Unit,
    onNavigateToDNS: () -> Unit,
    onNavigateToFirewall: () -> Unit,
    onNavigateToTelemetryBlocker: () -> Unit,
    onNavigateToVPNPassthrough: () -> Unit,
    onNavigateToTailscale: () -> Unit,
    onNavigateToQoS: () -> Unit,
    onNavigateToDeviceControl: () -> Unit,
    onNavigateToScheduleReboot: () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(title = { Text("Router Settings") })
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // Cellular
            SectionHeader("Cellular")
            SettingsItem(Icons.Default.CellTower, "Mobile Network", onClick = onNavigateToMobileNetwork)
            SettingsItem(Icons.Default.SettingsInputAntenna, "Network Mode", onClick = onNavigateToNetworkMode)
            SettingsItem(Icons.Default.Lock, "Cell Lock", onClick = onNavigateToCellLock)
            SettingsItem(Icons.Default.Hub, "STC", onClick = onNavigateToSTC)
            SettingsItem(Icons.Default.Radar, "Signal Detection", onClick = onNavigateToSignalDetect)
            SettingsItem(Icons.Default.SimCard, "SIM Card", onClick = onNavigateToSIM)
            SettingsItem(Icons.Default.Dialpad, "SIM Services (STK)", onClick = onNavigateToSTK)

            Spacer(modifier = Modifier.height(8.dp))

            // Connectivity
            SectionHeader("Connectivity")
            SettingsItem(Icons.Default.Wifi, "WiFi", onClick = onNavigateToWiFi)
            SettingsItem(Icons.Default.WifiTethering, "Guest WiFi", onClick = onNavigateToGuestWiFi)
            SettingsItem(Icons.Default.Language, "APN", onClick = onNavigateToAPN)
            SettingsItem(Icons.Default.Router, "LAN / DHCP", onClick = onNavigateToLAN)
            SettingsItem(Icons.Default.Dns, "DNS", onClick = onNavigateToDNS)

            Spacer(modifier = Modifier.height(8.dp))

            // Security
            SectionHeader("Security")
            SettingsItem(Icons.Default.Shield, "Firewall", onClick = onNavigateToFirewall)
            SettingsItem(Icons.Default.VisibilityOff, "Telemetry Blocker", onClick = onNavigateToTelemetryBlocker)
            SettingsItem(Icons.Default.VpnKey, "VPN Passthrough", onClick = onNavigateToVPNPassthrough)
            SettingsItem(Icons.Default.Cloud, "Tailscale", onClick = onNavigateToTailscale)

            Spacer(modifier = Modifier.height(8.dp))

            // Quality
            SectionHeader("Quality")
            SettingsItem(Icons.Default.Speed, "QoS", onClick = onNavigateToQoS)

            Spacer(modifier = Modifier.height(8.dp))

            // System
            SectionHeader("System")
            SettingsItem(Icons.Default.SettingsPower, "Device Controls", onClick = onNavigateToDeviceControl)
            SettingsItem(Icons.Default.Schedule, "Scheduled Reboot", onClick = onNavigateToScheduleReboot)
        }
    }
}

@Composable
private fun SectionHeader(title: String) {
    Text(
        title,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(bottom = 4.dp),
    )
}

@Composable
private fun SettingsItem(
    icon: ImageVector,
    title: String,
    onClick: () -> Unit,
) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
    ) {
        Row(
            modifier = Modifier.padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                icon,
                contentDescription = null,
                modifier = Modifier.size(24.dp),
                tint = MaterialTheme.colorScheme.primary,
            )
            Spacer(modifier = Modifier.width(16.dp))
            Text(
                title,
                style = MaterialTheme.typography.bodyLarge,
                fontWeight = FontWeight.Medium,
                modifier = Modifier.weight(1f),
            )
            Icon(
                Icons.Default.ChevronRight,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
