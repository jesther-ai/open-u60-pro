import SwiftUI

struct USBModeSheetView: View {
    @Bindable var viewModel: USBConnectionViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            VStack(spacing: 24) {
                Spacer()

                Image(systemName: "cable.connector")
                    .font(.system(size: 64))
                    .foregroundStyle(.blue)
                    .accessibilityHidden(true)

                Text("USB-C Connected")
                    .font(.title2.bold())

                Text("A USB-C cable is attached to your U60 Pro.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 40)

                if viewModel.usbStatus.powerbankActive {
                    Label("Fast charging is active", systemImage: "bolt.fill")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(.orange)
                        .padding(.horizontal, 32)
                } else {
                    if let msg = viewModel.message {
                        Text(msg)
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(viewModel.messageIsError ? .red : .green)
                            .textSelection(.enabled)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 32)
                    }

                    Button {
                        Task {
                            await viewModel.enablePowerbank()
                            dismiss()
                        }
                    } label: {
                        Group {
                            if viewModel.isLoading {
                                ProgressView()
                                    .tint(.white)
                            } else {
                                Label("Fast Charging", systemImage: "bolt.fill")
                            }
                        }
                        .frame(maxWidth: .infinity)
                        .frame(minHeight: 44)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(.orange)
                    .padding(.horizontal, 40)
                    .disabled(viewModel.isLoading)
                    .accessibilityLabel("Fast Charging")

                    Text("Charge your phone using the U60 Pro battery")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Button("Dismiss") {
                    dismiss()
                }
                .padding(.top, 8)

                Spacer()
                Spacer()
            }
            .navigationTitle("USB Mode")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium])
    }
}
