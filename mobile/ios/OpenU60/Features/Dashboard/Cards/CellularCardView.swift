import SwiftUI

struct CellularCardView: View, Equatable {
    let wanIPv4: String
    let wanIPv6: String
    let speed: TrafficSpeed
    let trafficStats: TrafficStats
    let isTrafficAvailable: Bool

    var body: some View {
        CardView {
            VStack(alignment: .leading, spacing: 10) {
                Text("Cellular Connection")
                    .font(.headline)

                if !wanIPv4.isEmpty {
                    HStack {
                        Text("WAN IP")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Text(wanIPv4)
                            .font(.caption.monospacedDigit())
                    }
                }
                if !wanIPv6.isEmpty {
                    HStack {
                        Text("WAN IPv6")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Text(wanIPv6)
                            .font(.caption2.monospacedDigit())
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }

                Divider()

                HStack {
                    VStack(spacing: 4) {
                        Image(systemName: "arrow.down.circle.fill")
                            .foregroundStyle(.green)
                        let dl = DeviceParser.speedComponents(speed.downloadBytesPerSec)
                        if isTrafficAvailable {
                            HStack(spacing: 0) {
                                Text(dl.number, format: .number.precision(.fractionLength(dl.decimalPlaces)))
                                Text(dl.unit)
                            }
                            .font(.title3.weight(.bold).monospacedDigit())
                            .transaction { $0.animation = nil }
                        } else {
                            Text("—").font(.title3.bold())
                        }
                        Text("Download")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity)

                    Divider()

                    VStack(spacing: 4) {
                        Image(systemName: "arrow.up.circle.fill")
                            .foregroundStyle(.blue)
                        let ul = DeviceParser.speedComponents(speed.uploadBytesPerSec)
                        if isTrafficAvailable {
                            HStack(spacing: 0) {
                                Text(ul.number, format: .number.precision(.fractionLength(ul.decimalPlaces)))
                                Text(ul.unit)
                            }
                            .font(.title3.weight(.bold).monospacedDigit())
                            .transaction { $0.animation = nil }
                        } else {
                            Text("—").font(.title3.bold())
                        }
                        Text("Upload")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity)
                }

                Text(!isTrafficAvailable ? "Live traffic unavailable"
                     : trafficStats.source == "wwandst" ? "Live cellular traffic · all devices"
                     : "Estimated cellular traffic · all devices")
                    .font(.caption2)
                    .foregroundStyle(.secondary)

                Divider()

                HStack {
                    Text("Total DL")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    let dlTotal = DeviceParser.bytesComponents(trafficStats.rxBytes)
                    HStack(spacing: 0) {
                        AnimatedNumber(value: dlTotal.number, decimalPlaces: dlTotal.decimalPlaces,
                                       font: .caption, textColor: .primary)
                        Text(dlTotal.unit)
                            .font(.caption.monospacedDigit())
                            .contentTransition(.opacity)
                            .animation(.easeInOut(duration: 0.4), value: dlTotal.unit)
                    }
                }
                HStack {
                    Text("Total UL")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    let ulTotal = DeviceParser.bytesComponents(trafficStats.txBytes)
                    HStack(spacing: 0) {
                        AnimatedNumber(value: ulTotal.number, decimalPlaces: ulTotal.decimalPlaces,
                                       font: .caption, textColor: .primary)
                        Text(ulTotal.unit)
                            .font(.caption.monospacedDigit())
                            .contentTransition(.opacity)
                            .animation(.easeInOut(duration: 0.4), value: ulTotal.unit)
                    }
                }
            }
        }
    }
}
