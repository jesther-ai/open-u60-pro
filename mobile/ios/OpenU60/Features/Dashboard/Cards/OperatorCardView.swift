import SwiftUI

struct OperatorCardView: View, Equatable {
    let operatorInfo: OperatorInfo
    let nrSignal: NRSignal
    let lteSignal: LTESignal

    private var displayType: String {
        operatorInfo.networkType.isEmpty
            ? "--"
            : operatorInfo.displayNetworkType(nrConnected: nrSignal.isConnected, lteSignal: lteSignal)
    }

    var body: some View {
        CardView {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(operatorInfo.provider.isEmpty ? "No Operator" : operatorInfo.provider)
                        .font(.headline)
                    Text(displayType)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if operatorInfo.roaming {
                    Label("Roaming", systemImage: "antenna.radiowaves.left.and.right.slash")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
                NetworkTypeIcon(networkType: displayType)
                signalBarsView(bars: operatorInfo.signalBar)
            }
        }
    }

    private func signalBarsView(bars: Int) -> some View {
        HStack(spacing: 2) {
            ForEach(0..<4) { i in
                RoundedRectangle(cornerRadius: 1)
                    .fill(i < bars ? .primary : Color.gray.opacity(0.3))
                    .frame(width: 4, height: CGFloat(8 + i * 4))
            }
        }
    }
}
