import SwiftUI

struct DeviceControlView: View {
    @Bindable var viewModel: DeviceControlViewModel

    private var controlsDisabled: Bool { viewModel.isLoading }

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

            Section {
                Toggle("Charge Limit", isOn: Binding(
                    get: { viewModel.chargeLimitEnabled },
                    set: { val in
                        viewModel.chargeLimitEnabled = val
                        Task { await viewModel.setChargeLimit(enabled: val, limit: viewModel.chargeLimit) }
                    }
                ))
                    .disabled(controlsDisabled)

                if viewModel.chargeLimitEnabled {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 0) {
                            Text("Stop at ")
                                .font(.subheadline)
                            AnimatedNumber(value: viewModel.chargeLimit, font: .subheadline.monospacedDigit(), textColor: .primary, suffix: "%")
                        }
                        Slider(
                            value: Binding(
                                get: { Double(viewModel.chargeLimit) },
                                set: { viewModel.chargeLimit = Int($0) }
                            ),
                            in: 50...100,
                            step: 5
                        ) {
                            Text("Charge Limit")
                        } onEditingChanged: { editing in
                            if !editing {
                                Task { await viewModel.setChargeLimit(enabled: true, limit: viewModel.chargeLimit) }
                            }
                        }
                        .disabled(controlsDisabled)
                    }

                    Stepper(value: Binding(
                        get: { viewModel.hysteresis },
                        set: { newVal in
                            viewModel.hysteresis = newVal
                            Task { await viewModel.setChargeLimit(enabled: true, limit: viewModel.chargeLimit, hysteresis: newVal) }
                        }
                    ), in: 1...20) {
                        HStack(spacing: 0) {
                            Text("Resume gap: ")
                                .font(.body)
                            AnimatedNumber(value: viewModel.hysteresis, font: .body.monospacedDigit(), textColor: .primary, suffix: "%")
                        }
                    }
                    .disabled(controlsDisabled)
                }
            } footer: {
                if viewModel.chargeLimitEnabled {
                    Text("Charging stops at \(viewModel.chargeLimit)% and resumes at \(viewModel.chargeLimit - viewModel.hysteresis)%.\n\nThe resume gap prevents the charger from rapidly switching on and off. A smaller gap (e.g. 2%) keeps the battery closer to your target but toggles the charger more often. A larger gap (e.g. 10%) means fewer charge cycles but the battery level will swing more.\n\nDefault: 5% — good balance for most users.")
                } else {
                    Text("Stops charging when battery reaches the set level. Extends battery lifespan.")
                }
            }

            Section {
                Toggle("Power-save Mode", isOn: Binding(
                    get: { viewModel.powerSaveEnabled },
                    set: { val in
                        viewModel.powerSaveEnabled = val
                        Task { await viewModel.setPowerSave(enabled: val) }
                    }
                ))
                    .disabled(controlsDisabled)
            } footer: {
                Text("Restricts data communication speed to reduce consumption and extend battery life.")
            }

            Section {
                Toggle("Fast Boot", isOn: Binding(
                    get: { viewModel.fastBootEnabled },
                    set: { val in
                        viewModel.fastBootEnabled = val
                        Task { await viewModel.setFastBoot(enabled: val) }
                    }
                ))
                    .disabled(controlsDisabled)
            } footer: {
                Text("When enabled, powering off suspends to RAM for near-instant boot. Disabling uses full shutdown (saves battery when off).")
            }

            Section {
                Toggle("Auto Sleep", isOn: Binding(
                    get: { viewModel.autoSleepEnabled },
                    set: { val in
                        viewModel.autoSleepEnabled = val
                        Task { await viewModel.setAutoSleep(enabled: val, timeout: val ? viewModel.autoSleepTimeout : nil) }
                    }
                ))
                    .disabled(controlsDisabled)

                if viewModel.autoSleepEnabled {
                    Picker("Idle Timeout", selection: Binding(
                        get: { viewModel.autoSleepTimeout },
                        set: { newVal in
                            viewModel.autoSleepTimeout = newVal
                            Task { await viewModel.setAutoSleep(enabled: true, timeout: newVal) }
                        }
                    )) {
                        Text("5 min").tag("5")
                        Text("10 min").tag("10")
                        Text("20 min").tag("20")
                        Text("30 min").tag("30")
                        Text("1 hour").tag("60")
                        Text("2 hours").tag("120")
                    }
                    .disabled(controlsDisabled)
                }
            } footer: {
                Text("Automatically enters low-power sleep when no WiFi clients are connected and the idle timeout expires. Press the power button to wake.")
            }

            Section {
                Button("Reboot Router") {
                    viewModel.showRebootConfirm = true
                }
                .disabled(controlsDisabled)
            } footer: {
                Text("The router will restart. This takes about 60 seconds.")
            }

            Section {
                Button("Factory Reset", role: .destructive) {
                    viewModel.showFactoryResetConfirm = true
                }
                .disabled(controlsDisabled)
            } footer: {
                Text("This will erase all settings and restore factory defaults. This cannot be undone.")
            }
        }
        .task { await viewModel.refresh() }
        .navigationTitle("Device Controls")
        .overlay {
            if viewModel.isLoading {
                ProgressView()
                    .padding()
                    .background(Color(.systemBackground).opacity(0.85), in: RoundedRectangle(cornerRadius: 8))
            }
        }
        .sheet(isPresented: $viewModel.showRebootConfirm) {
            PasswordConfirmView(
                title: "Reboot Router",
                message: "Enter your router password to confirm reboot.",
                confirmLabel: "Reboot"
            ) {
                await viewModel.reboot()
            }
        }
        .sheet(isPresented: $viewModel.showFactoryResetConfirm) {
            PasswordConfirmView(
                title: "Factory Reset",
                message: "This will erase ALL settings. Enter your router password to confirm.",
                confirmLabel: "Factory Reset"
            ) {
                await viewModel.factoryReset()
            }
        }
    }
}
