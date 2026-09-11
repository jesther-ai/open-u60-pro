package com.openu60.feature.tools.speedtest

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.hilt.navigation.compose.hiltViewModel
import com.openu60.core.components.AnimatedNumber

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SpeedTestScreen(
    onBack: () -> Unit,
    viewModel: SpeedTestViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsState()

    LaunchedEffect(Unit) { viewModel.loadServers() }

    val isRunning = state.phase !in listOf("idle", "complete", "cancelled", "error")

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Speed Test") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            // Message banner
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

            // Server picker
            ServerPicker(
                servers = state.servers,
                selectedServerId = state.selectedServerId,
                enabled = !isRunning && !state.isLoading,
                onSelect = { viewModel.selectServer(it) },
            )

            // Live speed readout
            if (isRunning) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(24.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            if (state.phase == "download") {
                                Icon(
                                    imageVector = Icons.Default.KeyboardArrowDown,
                                    contentDescription = "Downloading",
                                    tint = MaterialTheme.colorScheme.primary,
                                    modifier = Modifier.size(36.dp),
                                )
                            } else if (state.phase == "upload") {
                                Icon(
                                    imageVector = Icons.Default.KeyboardArrowUp,
                                    contentDescription = "Uploading",
                                    tint = MaterialTheme.colorScheme.tertiary,
                                    modifier = Modifier.size(36.dp),
                                )
                            }
                            AnimatedNumber(
                                value = state.liveSpeedMbps,
                                decimalPlaces = 1,
                                style = MaterialTheme.typography.displaySmall.copy(
                                    fontSize = 48.sp,
                                    fontWeight = FontWeight.Bold,
                                ),
                                color = MaterialTheme.colorScheme.primary,
                            )
                        }
                        Text(
                            "Mbps",
                            style = MaterialTheme.typography.titleMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        if (state.serverName.isNotBlank()) {
                            Spacer(modifier = Modifier.height(4.dp))
                            Text(
                                state.serverName,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }

            // Phase + progress
            if (isRunning) {
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(
                            state.phase.replaceFirstChar { it.uppercase() },
                            style = MaterialTheme.typography.titleSmall,
                            fontWeight = FontWeight.Medium,
                        )
                        Spacer(modifier = Modifier.height(8.dp))
                        LinearProgressIndicator(
                            progress = { state.progress / 100f },
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Spacer(modifier = Modifier.height(4.dp))
                        AnimatedNumber(
                            value = state.progress,
                            suffix = "%",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }

            // Start / Stop button
            Button(
                onClick = { if (isRunning) viewModel.stopTest() else viewModel.startTest() },
                enabled = !state.isLoading || isRunning,
                modifier = Modifier.fillMaxWidth(),
                colors = if (isRunning) ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.error,
                ) else ButtonDefaults.buttonColors(),
            ) {
                Text(if (isRunning) "Stop Test" else "Start Test")
            }

            // Results cards after completion
            if (state.phase == "complete") {
                ResultsSection(state)
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ServerPicker(
    servers: List<SpeedTestServerInfo>,
    selectedServerId: Int?,
    enabled: Boolean,
    onSelect: (Int) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val selected = servers.find { it.id == selectedServerId }

    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { if (enabled) expanded = it },
    ) {
        OutlinedTextField(
            value = selected?.let { "${it.name}, ${it.country}" } ?: "Select server...",
            onValueChange = {},
            readOnly = true,
            enabled = enabled,
            label = { Text("Server") },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.fillMaxWidth().menuAnchor(),
        )
        ExposedDropdownMenu(
            expanded = expanded,
            onDismissRequest = { expanded = false },
        ) {
            servers.forEach { server ->
                DropdownMenuItem(
                    text = {
                        Column {
                            Text("${server.name}, ${server.country}", fontWeight = FontWeight.Medium)
                            Text(server.sponsor, style = MaterialTheme.typography.bodySmall)
                        }
                    },
                    onClick = {
                        onSelect(server.id)
                        expanded = false
                    },
                )
            }
        }
    }
}

@Composable
private fun ResultsSection(state: SpeedTestState) {
    // Ping / Jitter
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text("Latency", style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Bold)
            Spacer(modifier = Modifier.height(8.dp))
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
                ResultValue(label = "Ping", value = state.pingMs?.let { String.format("%.1f ms", it) } ?: "--")
                ResultValue(label = "Jitter", value = state.jitterMs?.let { String.format("%.1f ms", it) } ?: "--")
            }
        }
    }

    // Download
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text("Download", style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Bold)
            Spacer(modifier = Modifier.height(8.dp))
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
                ResultValue(
                    label = "Speed",
                    value = state.downloadMbps?.let { String.format("%.2f Mbps", it) } ?: "--",
                )
                ResultValue(
                    label = "Data",
                    value = formatBytes(state.downloadBytes),
                )
            }
        }
    }

    // Upload
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text("Upload", style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Bold)
            Spacer(modifier = Modifier.height(8.dp))
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
                ResultValue(
                    label = "Speed",
                    value = state.uploadMbps?.let { String.format("%.2f Mbps", it) } ?: "--",
                )
                ResultValue(
                    label = "Data",
                    value = formatBytes(state.uploadBytes),
                )
            }
        }
    }
}

@Composable
private fun ResultValue(label: String, value: String) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Text(value, fontWeight = FontWeight.Bold, textAlign = TextAlign.Center)
        Text(label, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

private fun formatBytes(bytes: Long): String = when {
    bytes >= 1_000_000_000 -> String.format("%.2f GB", bytes / 1_000_000_000.0)
    bytes >= 1_000_000 -> String.format("%.1f MB", bytes / 1_000_000.0)
    bytes >= 1_000 -> String.format("%.0f KB", bytes / 1_000.0)
    else -> "$bytes B"
}
