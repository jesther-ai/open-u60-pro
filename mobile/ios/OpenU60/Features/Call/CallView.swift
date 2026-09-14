import SwiftUI

struct CallView: View {
    /// Owned as `@State` so a re-evaluation of the presenting `fullScreenCover` closure cannot
    /// swap in a fresh view model (and a fresh poll loop) mid-call.
    @State private var viewModel: CallViewModel
    @Environment(\.dismiss) private var dismiss

    init(viewModel: CallViewModel) {
        _viewModel = State(initialValue: viewModel)
    }

    var body: some View {
        VStack(spacing: 0) {
            // Top bar
            HStack {
                Spacer()
                Button("Close") { dismiss() }
                    .padding()
            }

            Spacer()

            switch viewModel.callState {
            case .idle:
                idleView
            case .dialing:
                activeCallHeader(status: "Dialing...")
                Spacer()
                hangupButton
            case .alerting:
                activeCallHeader(status: "Ringing...")
                Spacer()
                hangupButton
            case .active:
                activeCallHeader(status: formattedDuration)
                Spacer()
                inCallControls
                Spacer()
                hangupButton
            case .incoming(let from):
                incomingView(from: from)
            }

            Spacer()

            if let error = viewModel.error {
                Text(error)
                    .foregroundStyle(.red)
                    .font(.caption)
                    .padding()
            }
        }
        .background(.background)
        .task { await viewModel.syncWithRouter() }
        .onDisappear { viewModel.stopPolling() }
    }

    // MARK: - Idle (Dialer)

    private var idleView: some View {
        VStack(spacing: 24) {
            // Number display
            HStack {
                Text(viewModel.phoneNumber.isEmpty ? " " : viewModel.phoneNumber)
                    .font(.system(size: 36, weight: .light, design: .monospaced))
                    .lineLimit(1)
                    .minimumScaleFactor(0.5)

                if !viewModel.phoneNumber.isEmpty {
                    Button {
                        viewModel.deleteDigit()
                    } label: {
                        Image(systemName: "delete.backward")
                            .font(.title2)
                            .foregroundStyle(.secondary)
                    }
                    .accessibilityLabel("Delete last digit")
                }
            }
            .frame(height: 44)
            .padding(.horizontal, 32)

            DialPadView { digit in
                viewModel.appendDigit(digit)
            }

            // Call button
            Button {
                Task { await viewModel.dial() }
            } label: {
                Image(systemName: "phone.fill")
                    .font(.title)
                    .foregroundStyle(.white)
                    .frame(width: 72, height: 72)
                    .background(.green, in: Circle())
            }
            .accessibilityLabel("Call")
            .disabled(viewModel.phoneNumber.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    // MARK: - Active Call Header

    private func activeCallHeader(status: String) -> some View {
        VStack(spacing: 8) {
            Text(viewModel.phoneNumber)
                .font(.title)
                .fontWeight(.light)
            Text(status)
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
        .padding(.top, 40)
    }

    // MARK: - In-Call Controls

    private var inCallControls: some View {
        VStack(spacing: 24) {
            if viewModel.showKeypad {
                DialPadView { digit in
                    viewModel.appendDigit(digit)
                }

                Button("Hide Keypad") {
                    viewModel.showKeypad = false
                }
                .font(.subheadline)
            } else {
                HStack(spacing: 40) {
                    callControlButton(
                        icon: viewModel.isMuted ? "mic.slash.fill" : "mic.fill",
                        label: "Mute",
                        isActive: viewModel.isMuted
                    ) {
                        Task { await viewModel.toggleMute() }
                    }

                    callControlButton(
                        icon: "number",
                        label: "Keypad",
                        isActive: false
                    ) {
                        viewModel.showKeypad = true
                    }

                    callControlButton(
                        icon: "speaker.wave.3.fill",
                        label: "Speaker",
                        isActive: false
                    ) {
                        // Speaker toggle placeholder for Phase 2 audio bridge
                    }
                }
            }
        }
    }

    private func callControlButton(icon: String, label: String, isActive: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 6) {
                Image(systemName: icon)
                    .font(.title2)
                    .frame(width: 56, height: 56)
                    .background(isActive ? AnyShapeStyle(.white) : AnyShapeStyle(.fill.tertiary), in: Circle())
                    .foregroundStyle(isActive ? AnyShapeStyle(.black) : AnyShapeStyle(.primary))
                Text(label)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .buttonStyle(.plain)
    }

    // MARK: - Incoming Call

    private func incomingView(from: String) -> some View {
        VStack(spacing: 16) {
            Text(from)
                .font(.title)
                .fontWeight(.light)
            Text("Incoming Call")
                .font(.subheadline)
                .foregroundStyle(.secondary)

            Spacer()

            HStack(spacing: 60) {
                // Decline
                Button {
                    Task { await viewModel.hangup() }
                } label: {
                    Image(systemName: "phone.down.fill")
                        .font(.title)
                        .foregroundStyle(.white)
                        .frame(width: 72, height: 72)
                        .background(.red, in: Circle())
                }
                .accessibilityLabel("Decline")

                // Answer
                Button {
                    Task { await viewModel.answer() }
                } label: {
                    Image(systemName: "phone.fill")
                        .font(.title)
                        .foregroundStyle(.white)
                        .frame(width: 72, height: 72)
                        .background(.green, in: Circle())
                }
                .accessibilityLabel("Answer")
            }
        }
        .padding(.top, 40)
    }

    // MARK: - Hangup Button

    private var hangupButton: some View {
        Button {
            Task { await viewModel.hangup() }
        } label: {
            Image(systemName: "phone.down.fill")
                .font(.title)
                .foregroundStyle(.white)
                .frame(width: 72, height: 72)
                .background(.red, in: Circle())
        }
        .accessibilityLabel("End call")
        .padding(.bottom, 40)
    }

    // MARK: - Helpers

    private var formattedDuration: String {
        let total = Int(viewModel.callDuration)
        let minutes = total / 60
        let seconds = total % 60
        return String(format: "%d:%02d", minutes, seconds)
    }
}
