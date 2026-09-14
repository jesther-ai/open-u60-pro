import SwiftUI

struct LastUpdatedView: View {
    let date: Date?

    @State private var elapsed: Int = 0

    /// Held in `@State` so the run-loop timer survives body re-evaluation instead of being torn
    /// down and rebuilt on every render. `.common` mode keeps it ticking during scrolling.
    @State private var ticker = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    private var text: String {
        guard date != nil else { return "--" }
        if elapsed < 60 {
            return "\(elapsed)s ago"
        }
        return "\(elapsed / 60)m \(elapsed % 60)s ago"
    }

    private var accessibilityText: String {
        guard date != nil else { return "Never updated" }
        if elapsed < 60 {
            return "Updated \(elapsed) seconds ago"
        }
        return "Updated \(elapsed / 60) minutes \(elapsed % 60) seconds ago"
    }

    var body: some View {
        Text(text)
            .font(.caption2.monospacedDigit())
            .foregroundStyle(.secondary)
            .contentTransition(.numericText())
            .animation(.default, value: elapsed)
            .accessibilityLabel(accessibilityText)
            .onReceive(ticker) { _ in
                updateElapsed()
            }
            .onChange(of: date) {
                updateElapsed()
            }
    }

    private func updateElapsed() {
        guard let date else {
            elapsed = 0
            return
        }
        elapsed = max(0, Int(Date().timeIntervalSince(date)))
    }
}
