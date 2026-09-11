import SwiftUI

struct DialPadView: View {
    var onDigit: (String) -> Void

    private static let rows: [[DialKey]] = [
        [.init("1", sub: ""), .init("2", sub: "ABC"), .init("3", sub: "DEF")],
        [.init("4", sub: "GHI"), .init("5", sub: "JKL"), .init("6", sub: "MNO")],
        [.init("7", sub: "PQRS"), .init("8", sub: "TUV"), .init("9", sub: "WXYZ")],
        [.init("*", sub: ""), .init("0", sub: "+"), .init("#", sub: "")],
    ]

    var body: some View {
        Grid(horizontalSpacing: 24, verticalSpacing: 16) {
            ForEach(Self.rows, id: \.self) { row in
                GridRow {
                    ForEach(row) { key in
                        Button {
                            onDigit(key.digit)
                        } label: {
                            VStack(spacing: 2) {
                                Text(key.digit)
                                    .font(.title)
                                    .fontWeight(.light)
                                if !key.sub.isEmpty {
                                    Text(key.sub)
                                        .font(.system(size: 10, weight: .medium))
                                        .tracking(2)
                                }
                            }
                            .frame(width: 72, height: 72)
                            .background(.fill.tertiary, in: Circle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(key.digit)
                    }
                }
            }
        }
    }
}

private struct DialKey: Identifiable, Hashable {
    let digit: String
    let sub: String
    var id: String { digit }

    init(_ digit: String, sub: String) {
        self.digit = digit
        self.sub = sub
    }
}
