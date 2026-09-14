import SwiftUI

// MARK: - SingleDigitReel

/// One odometer column.
///
/// The strip holds a single 0-9 run plus one wrap-around slot. `position` accumulates the signed
/// distance rolled so far and `ReelOffset` folds it back into the strip on every frame, so the
/// column can roll indefinitely while only 11 slots are ever laid out and no snap-back is needed.
struct SingleDigitReel: View, Equatable {
    let digit: Int
    let font: Font
    let textColor: Color
    let isAnimated: Bool
    let animationDuration: Double

    private static let digitsPerSet = 10
    private static let slotCount = digitsPerSet + 1
    private static let glyphs = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]

    @State private var position: Int

    init(digit: Int, font: Font, textColor: Color, isAnimated: Bool, animationDuration: Double) {
        self.digit = digit
        self.font = font
        self.textColor = textColor
        self.isAnimated = isAnimated
        self.animationDuration = animationDuration

        _position = State(initialValue: digit)
    }

    var body: some View {
        Text(verbatim: "8")
            .font(font.monospacedDigit())
            .foregroundStyle(.clear)
            .overlay {
                GeometryReader { proxy in
                    let slotHeight = proxy.size.height
                    VStack(spacing: 0) {
                        ForEach(0..<Self.slotCount, id: \.self) { index in
                            Text(verbatim: Self.glyphs[index % Self.digitsPerSet])
                                .frame(width: proxy.size.width, height: slotHeight)
                        }
                    }
                    .font(font.monospacedDigit())
                    .foregroundStyle(textColor)
                    .modifier(ReelOffset(position: Double(position),
                                         period: Double(Self.digitsPerSet),
                                         slotHeight: slotHeight))
                }
            }
            .clipped()
            .onChange(of: digit) { oldValue, newValue in
                let delta = Self.shortestDelta(from: oldValue, to: newValue)
                guard delta != 0 else { return }
                if isAnimated {
                    withAnimation(.easeInOut(duration: animationDuration)) {
                        position += delta
                    }
                } else {
                    position += delta
                }
            }
    }

    static func == (lhs: SingleDigitReel, rhs: SingleDigitReel) -> Bool {
        lhs.digit == rhs.digit
            && lhs.isAnimated == rhs.isAnimated
            && lhs.animationDuration == rhs.animationDuration
            && lhs.textColor == rhs.textColor
            && lhs.font == rhs.font
    }

    /// Computes the shortest path on the mod-10 ring.
    /// Positive = forward (rolling down), negative = backward (rolling up).
    private static func shortestDelta(from: Int, to: Int) -> Int {
        let forward = (to - from + digitsPerSet) % digitsPerSet   // e.g. 9→0: (0-9+10)%10 = 1
        let backward = forward - digitsPerSet                     // e.g. 9→0: 1-10 = -9
        return abs(forward) <= abs(backward) ? forward : backward
    }
}

// MARK: - ReelOffset

/// Scrolls the strip to `position`, folded back into a single 0-9 run.
///
/// The strip repeats every `period` slots, so folding is invisible: slot `period` renders the same
/// glyph as slot 0 and the visible window is always inside `0...period`, whatever `position` is.
private struct ReelOffset: GeometryEffect {
    var position: Double
    let period: Double
    let slotHeight: CGFloat

    var animatableData: Double {
        get { position }
        set { position = newValue }
    }

    func effectValue(size: CGSize) -> ProjectionTransform {
        var slot = position.truncatingRemainder(dividingBy: period)
        if slot < 0 { slot += period }
        return ProjectionTransform(CGAffineTransform(translationX: 0, y: -CGFloat(slot) * slotHeight))
    }
}
