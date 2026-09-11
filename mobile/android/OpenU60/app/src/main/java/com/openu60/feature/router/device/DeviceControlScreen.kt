package com.openu60.feature.router.device

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.openu60.core.components.AnimatedNumber

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DeviceControlScreen(
    onBack: () -> Unit,
    viewModel: DeviceControlViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsState()

    LaunchedEffect(Unit) { viewModel.refresh() }

    // Reboot confirmation dialog
    if (state.showRebootConfirm) {
        AlertDialog(
            onDismissRequest = { viewModel.dismissRebootConfirm() },
            title = { Text("Reboot Device") },
            text = { Text("Are you sure you want to reboot the device?") },
            confirmButton = {
                TextButton(onClick = { viewModel.reboot() }) {
                    Text("Reboot", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { viewModel.dismissRebootConfirm() }) { Text("Cancel") }
            },
        )
    }

    // Factory reset confirmation dialog
    if (state.showResetConfirm) {
        AlertDialog(
            onDismissRequest = { viewModel.dismissResetConfirm() },
            title = { Text("Factory Reset") },
            text = { Text("This will erase all settings and restore the device to factory defaults. This action cannot be undone.") },
            confirmButton = {
                TextButton(onClick = { viewModel.factoryReset() }) {
                    Text("Reset", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = {
                TextButton(onClick = { viewModel.dismissResetConfirm() }) { Text("Cancel") }
            },
        )
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Device Control") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        val controlsDisabled = state.isLoading

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

                // Charge Limit
                if (state.chargeControlLoaded) {
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(modifier = Modifier.padding(16.dp)) {
                            Text("Charge Limit", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
                            Spacer(modifier = Modifier.height(8.dp))

                            ToggleRow("Charge Limit", state.chargeLimitEnabled) {
                                viewModel.setChargeLimit(it, state.chargeLimit)
                            }

                            if (state.chargeLimitEnabled) {
                                Spacer(modifier = Modifier.height(8.dp))
                                var sliderValue by remember(state.chargeLimit) { mutableFloatStateOf(state.chargeLimit.toFloat()) }
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Text("Stop at ", style = MaterialTheme.typography.bodyLarge)
                                    AnimatedNumber(
                                        value = sliderValue.toInt(),
                                        style = MaterialTheme.typography.bodyLarge,
                                        suffix = "%",
                                    )
                                }
                                Slider(
                                    value = sliderValue,
                                    onValueChange = { sliderValue = it },
                                    onValueChangeFinished = { viewModel.setChargeLimit(true, sliderValue.toInt()) },
                                    valueRange = 50f..100f,
                                    steps = 9,
                                    modifier = Modifier.fillMaxWidth(),
                                )

                                Spacer(modifier = Modifier.height(8.dp))
                                Row(
                                    modifier = Modifier.fillMaxWidth(),
                                    horizontalArrangement = Arrangement.SpaceBetween,
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        Text("Resume gap: ", style = MaterialTheme.typography.bodyLarge)
                                        AnimatedNumber(
                                            value = state.hysteresis,
                                            style = MaterialTheme.typography.bodyLarge,
                                            suffix = "%",
                                        )
                                    }
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        IconButton(
                                            onClick = {
                                                val newVal = (state.hysteresis - 1).coerceAtLeast(1)
                                                viewModel.setChargeLimit(true, state.chargeLimit, newVal)
                                            },
                                            enabled = state.hysteresis > 1,
                                        ) { Text("−", style = MaterialTheme.typography.titleLarge) }
                                        IconButton(
                                            onClick = {
                                                val newVal = (state.hysteresis + 1).coerceAtMost(20)
                                                viewModel.setChargeLimit(true, state.chargeLimit, newVal)
                                            },
                                            enabled = state.hysteresis < 20,
                                        ) { Text("+", style = MaterialTheme.typography.titleLarge) }
                                    }
                                }
                            }

                            Spacer(modifier = Modifier.height(4.dp))
                            if (state.chargeLimitEnabled) {
                                Text(
                                    "Charging stops at ${sliderValue.toInt()}% and resumes at ${sliderValue.toInt() - state.hysteresis}%.\n\n" +
                                        "The resume gap prevents the charger from rapidly switching on and off. " +
                                        "A smaller gap keeps the battery closer to your target but toggles more often. " +
                                        "A larger gap means fewer cycles but more swing.\n\nDefault: 5%",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            } else {
                                Text(
                                    "Stops charging at the set level. Extends battery lifespan.",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                }

                // Other toggles
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text("Power Settings", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
                        Spacer(modifier = Modifier.height(8.dp))

                        if (state.powerSaveLoaded) {
                            ToggleRow("Power Save", state.powerSave) { viewModel.togglePowerSave(it) }
                        }
                        if (state.fastBootLoaded) {
                            ToggleRow("Fast Boot", state.fastBoot) { viewModel.toggleFastBoot(it) }
                        }
                        if (state.autoSleepLoaded) {
                            ToggleRow("Auto Sleep", state.autoSleepEnabled) {
                                viewModel.setAutoSleep(it)
                            }
                            if (state.autoSleepEnabled) {
                                Spacer(modifier = Modifier.height(4.dp))
                                val presets = listOf(5, 10, 20, 30, 60, 120)
                                val currentMinutes = state.autoSleepTimeout.toIntOrNull() ?: 5
                                val currentIndex = presets.indexOf(currentMinutes).coerceAtLeast(0)
                                var sliderIndex by remember(currentIndex) { mutableFloatStateOf(currentIndex.toFloat()) }
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Text("Idle timeout: ", style = MaterialTheme.typography.bodyLarge)
                                    AnimatedNumber(
                                        value = presets[sliderIndex.toInt().coerceIn(0, presets.lastIndex)],
                                        style = MaterialTheme.typography.bodyLarge,
                                        suffix = " min",
                                    )
                                }
                                Slider(
                                    value = sliderIndex,
                                    onValueChange = { sliderIndex = it },
                                    onValueChangeFinished = {
                                        val selected = presets[sliderIndex.toInt().coerceIn(0, presets.lastIndex)]
                                        viewModel.setAutoSleep(true, "$selected")
                                    },
                                    valueRange = 0f..(presets.size - 1).toFloat(),
                                    steps = presets.size - 2,
                                    enabled = !controlsDisabled,
                                    modifier = Modifier.fillMaxWidth(),
                                )
                                Spacer(modifier = Modifier.height(4.dp))
                                Text(
                                    "Sleeps when no WiFi clients are connected and idle timeout expires.",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                }

                // Reboot & Reset
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text("Device Actions", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
                        Spacer(modifier = Modifier.height(12.dp))
                        Button(
                            onClick = { viewModel.showRebootConfirm() },
                            enabled = !controlsDisabled,
                            modifier = Modifier.fillMaxWidth(),
                        ) { Text("Reboot Device") }
                        Spacer(modifier = Modifier.height(8.dp))
                        OutlinedButton(
                            onClick = { viewModel.showResetConfirm() },
                            enabled = !controlsDisabled,
                            modifier = Modifier.fillMaxWidth(),
                            colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        ) { Text("Factory Reset") }
                    }
                }
            }
        }
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
