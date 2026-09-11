import SwiftUI

// MARK: - DigitElement

/// One column of a formatted number.
///
/// `id` is the column's place value: a digit uses its base-10 exponent, a separator uses
/// `separatorBase` plus the exponent of the place immediately to its right. Identity therefore
/// survives a width change (999 -> 1,000, or 9.99 -> 10.0 when the precision adapts), so every
/// column keeps its own reel instead of being re-matched to a different place value.
struct DigitElement: Identifiable, Equatable {
    enum Kind: Equatable {
        case digit(Int)
        case separator(String)
    }

    static let separatorBase = 10_000

    let id: Int
    let kind: Kind
}

// MARK: - AnimatedNumber

/// Displays an integer or double with per-digit odometer-reel animations.
struct AnimatedNumber: View, Equatable {
    private enum Value: Equatable {
        case integer(Int, separator: String?)
        case decimal(Double, decimalPlaces: Int)
    }

    private let value: Value
    private let isNegative: Bool
    var font: Font = .system(size: 32, weight: .bold, design: .rounded)
    var textColor: Color = .primary
    var animationDuration: Double = 0.4
    var prefix: String?
    var suffix: String?

    @State private var isAnimated = false

    /// Integer initializer (existing behavior).
    init(
        value: Int,
        font: Font = .system(size: 32, weight: .bold, design: .rounded),
        textColor: Color = .primary,
        animationDuration: Double = 0.4,
        prefix: String? = nil,
        suffix: String? = nil,
        separator: String? = nil
    ) {
        self.isNegative = value < 0
        self.font = font
        self.textColor = textColor
        self.animationDuration = animationDuration
        self.prefix = prefix
        self.suffix = suffix
        self.value = .integer(abs(value), separator: separator)
    }

    /// Double initializer for percentage-style animated values.
    init(
        value: Double,
        decimalPlaces: Int = 1,
        font: Font = .system(size: 32, weight: .bold, design: .rounded),
        textColor: Color = .primary,
        animationDuration: Double = 0.4,
        prefix: String? = nil,
        suffix: String? = nil
    ) {
        self.isNegative = value < 0
        self.font = font
        self.textColor = textColor
        self.animationDuration = animationDuration
        self.prefix = prefix
        self.suffix = suffix
        self.value = .decimal(abs(value), decimalPlaces: decimalPlaces)
    }

    var body: some View {
        let currentElements = buildElements()

        HStack(spacing: 0) {
            if let prefix {
                Text(prefix)
                    .font(font)
                    .foregroundStyle(textColor)
            }

            if isNegative {
                Text(verbatim: "-")
                    .font(font)
                    .foregroundStyle(textColor)
            }

            ForEach(currentElements) { element in
                switch element.kind {
                case .digit(let digit):
                    SingleDigitReel(
                        digit: digit,
                        font: font,
                        textColor: textColor,
                        isAnimated: isAnimated,
                        animationDuration: animationDuration
                    )
                    .equatable()
                    .transition(.opacity.combined(with: .scale))
                case .separator(let separator):
                    Text(separator)
                        .font(font)
                        .foregroundStyle(textColor)
                        .transition(.opacity)
                }
            }

            if let suffix {
                Text(suffix)
                    .font(font)
                    .foregroundStyle(textColor)
            }
        }
        .animation(isAnimated ? .easeInOut(duration: animationDuration) : nil, value: currentElements.count)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spokenValue(for: currentElements))
        .onAppear {
            isAnimated = true
        }
    }

    static func == (lhs: AnimatedNumber, rhs: AnimatedNumber) -> Bool {
        lhs.value == rhs.value
            && lhs.isNegative == rhs.isNegative
            && lhs.animationDuration == rhs.animationDuration
            && lhs.prefix == rhs.prefix
            && lhs.suffix == rhs.suffix
            && lhs.textColor == rhs.textColor
            && lhs.font == rhs.font
    }

    // MARK: - Element Building

    private func buildElements() -> [DigitElement] {
        switch value {
        case .integer(let absValue, let separator):
            return Self.buildIntElements(absValue, separator: separator)
        case .decimal(let absValue, let decimalPlaces):
            return Self.buildDoubleElements(absValue, decimalPlaces: decimalPlaces)
        }
    }

    /// Emits digits with en_US thousands grouping applied inline, so no formatter is needed.
    private static func buildIntElements(_ absValue: Int, separator: String?) -> [DigitElement] {
        let digits = String(absValue)
        let count = digits.count

        var elements = [DigitElement]()
        elements.reserveCapacity(separator == nil ? count : count + (count - 1) / 3)

        var exponent = count - 1
        for character in digits {
            if let separator, exponent % 3 == 2, exponent != count - 1 {
                elements.append(DigitElement(id: DigitElement.separatorBase + exponent,
                                             kind: .separator(separator)))
            }
            elements.append(DigitElement(id: exponent, kind: .digit(character.wholeNumberValue ?? 0)))
            exponent -= 1
        }
        return elements
    }

    private static let decimalFormats = ["%.0f", "%.1f", "%.2f", "%.3f"]

    private static func buildDoubleElements(_ absValue: Double, decimalPlaces: Int) -> [DigitElement] {
        let places = max(decimalPlaces, 0)
        let format = places < decimalFormats.count ? decimalFormats[places] : "%.\(places)f"
        let formatted = String(format: format, absValue)

        var elements = [DigitElement]()
        elements.reserveCapacity(formatted.count)

        // The trailing `places` characters are the fraction, preceded by the point itself.
        var exponent = formatted.count - places - (places > 0 ? 2 : 1)
        for character in formatted {
            if let digit = character.wholeNumberValue {
                elements.append(DigitElement(id: exponent, kind: .digit(digit)))
                exponent -= 1
            } else {
                elements.append(DigitElement(id: DigitElement.separatorBase + exponent,
                                             kind: .separator(String(character))))
            }
        }
        return elements
    }

    // MARK: - Accessibility

    /// The reels are decorative once flattened, so the whole number is exposed as one label.
    private func spokenValue(for elements: [DigitElement]) -> String {
        var text = prefix ?? ""
        if isNegative { text += "-" }
        for element in elements {
            switch element.kind {
            case .digit(let digit): text += String(digit)
            case .separator(let separator): text += separator
            }
        }
        if let suffix { text += suffix }
        return text
    }
}
