import SwiftUI

struct OperatorCardView: View {
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
                Image(systemName: "cellularbars")
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(operatorInfo.signalBar > 0 ? .primary : .secondary)
                    .accessibilityLabel(Text("Signal"))
            }
        }
    }
}
