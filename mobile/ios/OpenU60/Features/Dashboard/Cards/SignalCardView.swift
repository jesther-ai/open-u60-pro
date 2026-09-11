import SwiftUI

struct SignalCardView: View, Equatable {
    let operatorInfo: OperatorInfo
    let nrSignal: NRSignal
    let lteSignal: LTESignal
    var wcdmaSignal: WCDMASignal = .empty
    var isAirplaneMode: Bool = false

    /// Everything that changes the card's *layout*, as opposed to the values inside it. Compares
    /// the carrier fields that make up `LTECarrier.id` directly, so driving the animation no
    /// longer builds an array of interpolated id strings on every body pass.
    private struct LayoutKey: Equatable {
        let showNR: Bool
        let showLTE: Bool
        let show3G: Bool
        /// Swaps the placeholder between the airplane label and "No signal data", which is a
        /// layout change of its own: it flips while every `show*` flag stays false.
        let isAirplaneMode: Bool
        let nrCarriers: [LTECarrier]
        let lteCarriers: [LTECarrier]

        static func == (lhs: LayoutKey, rhs: LayoutKey) -> Bool {
            lhs.showNR == rhs.showNR
                && lhs.showLTE == rhs.showLTE
                && lhs.show3G == rhs.show3G
                && lhs.isAirplaneMode == rhs.isAirplaneMode
                && sameIdentity(lhs.nrCarriers, rhs.nrCarriers)
                && sameIdentity(lhs.lteCarriers, rhs.lteCarriers)
        }

        private static func sameIdentity(_ lhs: [LTECarrier], _ rhs: [LTECarrier]) -> Bool {
            lhs.count == rhs.count && zip(lhs, rhs).allSatisfy {
                $0.label == $1.label && $0.band == $1.band
                    && $0.pci == $1.pci && $0.earfcn == $1.earfcn
            }
        }
    }

    var body: some View {
        let showNR = operatorInfo.showNR(nr: nrSignal)
        let showLTE = operatorInfo.showLTE(lte: lteSignal)
        let show3G = operatorInfo.show3G(nr: nrSignal, lte: lteSignal, wcdma: wcdmaSignal)
        let layout = LayoutKey(
            showNR: showNR, showLTE: showLTE, show3G: show3G,
            isAirplaneMode: isAirplaneMode,
            nrCarriers: nrSignal.sccCarriers, lteCarriers: lteSignal.sccCarriers
        )

        CardView {
            VStack(alignment: .leading, spacing: 8) {
                Text("Signal")
                    .font(.headline)

                if showNR {
                    carrierRow(
                        icon: "antenna.radiowaves.left.and.right", tech: "5G NR",
                        band: nrSignal.band, freq: BandConfig.nrFrequency(band: nrSignal.band),
                        technology: .nr, bandwidth: nrSignal.bandwidth,
                        isSCC: false, isPCC: !nrSignal.sccCarriers.isEmpty,
                        rsrp: nrSignal.rsrp, sinr: nrSignal.sinr,
                        pci: nrSignal.pci
                    )
                    ForEach(nrSignal.sccCarriers) { scc in
                        carrierRow(
                            icon: "antenna.radiowaves.left.and.right", tech: "5G NR",
                            band: scc.band, freq: BandConfig.nrFrequency(band: scc.band),
                            technology: .nr, bandwidth: scc.bandwidth,
                            isSCC: true, anchorLabel: nil, rsrp: scc.rsrp, sinr: scc.sinr,
                            pci: scc.pci
                        )
                    }
                }

                if showLTE {
                    if showNR { Divider() }
                    let lteCaActive = lteSignal.caState != "0" && !lteSignal.sccCarriers.isEmpty
                    carrierRow(
                        icon: "cellularbars", tech: "LTE",
                        band: lteSignal.band, freq: BandConfig.lteFrequency(band: lteSignal.band),
                        technology: .lte, bandwidth: lteSignal.bandwidth,
                        isSCC: false, isPCC: !showNR && lteCaActive,
                        anchorLabel: showNR ? "Anchor" : nil,
                        rsrp: lteSignal.rsrp, sinr: lteSignal.sinr,
                        pci: lteSignal.pci
                    )
                    if lteSignal.caState != "0" {
                        ForEach(lteSignal.sccCarriers) { scc in
                            carrierRow(
                                icon: "cellularbars", tech: "LTE",
                                band: scc.band, freq: BandConfig.lteFrequency(band: scc.band),
                                technology: .lte, bandwidth: scc.bandwidth,
                                isSCC: true, anchorLabel: nil, rsrp: scc.rsrp, sinr: scc.sinr,
                                pci: scc.pci
                            )
                        }
                    }
                }

                if show3G {
                    if showNR || showLTE { Divider() }
                    wcdmaRow
                }

                if !showNR && !showLTE && !show3G {
                    if isAirplaneMode {
                        Label("Airplane Mode", systemImage: "airplane")
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .foregroundStyle(.secondary)
                    } else {
                        Text("No signal data")
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .animation(.smooth, value: layout)
        }
    }

    @ViewBuilder
    private func carrierRow(
        icon: String, tech: String, band: String, freq: String?,
        technology: BandTechnology, bandwidth: String,
        isSCC: Bool, isPCC: Bool = false, anchorLabel: String? = nil,
        rsrp: Double?, sinr: Double?, pci: String
    ) -> some View {
        let spec = technology.spec(for: band)
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Label {
                    HStack(spacing: 4) {
                        if band.isEmpty {
                            Text(tech)
                        } else if let spec {
                            Text("\(tech) \u{00B7} B\(band) (\(spec.commonName), \(spec.duplexMode.rawValue))")
                        } else if let freq {
                            Text("\(tech) \u{00B7} Band \(band) (\(freq))")
                        } else {
                            Text("\(tech) \u{00B7} Band \(band)")
                        }
                    }
                } icon: {
                    Image(systemName: icon)
                }
                .font(.subheadline)
                if isSCC {
                    Text("SCC")
                        .font(.caption2.bold())
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(.secondary.opacity(0.15), in: Capsule())
                        .foregroundStyle(.secondary)
                }
                if isPCC {
                    Text("PCC")
                        .font(.caption2.bold())
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(.blue.opacity(0.15), in: Capsule())
                        .foregroundStyle(.blue)
                }
                if let anchorLabel {
                    Text(anchorLabel)
                        .font(.caption2.bold())
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(.orange.opacity(0.15), in: Capsule())
                        .foregroundStyle(.orange)
                }
                Spacer()
            }
            HStack {
                Text("RSRP")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                animatedDB(rsrp, font: .body.weight(.bold), color: Color.rsrpColor(rsrp))
                Spacer()
                Text("SINR")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                animatedDB(sinr, font: .body, color: Color.sinrColor(sinr))
            }
            if let bwText = bandwidthText(bandwidth: bandwidth, spec: spec) {
                Text(bwText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if !pci.isEmpty {
                Text("PCI \(pci)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    private func bandwidthText(bandwidth: String, spec: BandSpec?) -> String? {
        let bwNum = Int(bandwidth.trimmingCharacters(in: .letters.union(.whitespaces)))
        if let bwNum, let spec {
            return "BW \(bwNum) MHz \u{00B7} Max \(spec.maxBandwidthMHz) MHz"
        } else if let bwNum {
            return "BW \(bwNum) MHz"
        }
        return nil
    }

    @ViewBuilder
    private var wcdmaRow: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Label("3G WCDMA", systemImage: "antenna.radiowaves.left.and.right.circle")
                    .font(.subheadline)
                Spacer()
            }
            HStack {
                Text("RSCP")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                animatedDB(wcdmaSignal.rscp, font: .body.weight(.bold), color: Color.rscpColor(wcdmaSignal.rscp))
                Spacer()
                Text("Ec/Io")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                animatedDB(wcdmaSignal.ecio, font: .body, color: Color.ecioColor(wcdmaSignal.ecio))
            }
        }
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    @ViewBuilder
    private func animatedDB(_ value: Double?, font: Font, color: Color) -> some View {
        if let v = value {
            AnimatedNumber(value: Int(v), font: font, textColor: color, suffix: " dB")
        } else {
            Text("--").font(font.monospacedDigit()).foregroundStyle(.secondary)
        }
    }
}
