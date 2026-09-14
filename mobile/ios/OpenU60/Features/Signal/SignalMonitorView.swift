import SwiftUI
import Charts

struct SignalMonitorView: View {
    var viewModel: SignalMonitorViewModel

    @Environment(\.scenePhase) private var scenePhase
    @State private var isVisible = false

    /// This screen stays alive on the Dashboard tab's navigation stack while the user is on
    /// another tab, so foreground alone is not enough to resume: returning from the background
    /// would restart a 2s poll of the heaviest endpoint in the app on a screen nobody is looking
    /// at. Both conditions drive one key so the two handlers cannot disagree.
    private struct PollKey: Equatable {
        let foreground: Bool
        let visible: Bool

        var shouldPoll: Bool { foreground && visible }
    }

    private var pollKey: PollKey {
        PollKey(foreground: scenePhase == .active, visible: isVisible)
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 16) {
                let nr = viewModel.nrSignal
                let lte = viewModel.lteSignal
                let wcdma = viewModel.wcdmaSignal
                let showNR = viewModel.operatorInfo.showNR(nr: nr)
                let showLTE = viewModel.operatorInfo.showLTE(lte: lte)
                let show3G = viewModel.operatorInfo.show3G(nr: nr, lte: lte, wcdma: wcdma)

                SignalHistoryChart(history: viewModel.history, showLTE: showLTE, show3G: show3G)
                if showNR {
                    CellularPanel(data: .nr(nr)).equatable()
                }
                if showLTE {
                    CellularPanel(data: .lte(lte, isNSAAnchor: showNR)).equatable()
                }
                if show3G {
                    WCDMAPanel(signal: wcdma)
                }
            }
            .padding()
        }
        .navigationTitle("Signal Monitor")
        .refreshable { await viewModel.refresh() }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                LastUpdatedView(date: viewModel.lastUpdated)
            }
        }
        .onAppear { isVisible = true }
        .onDisappear {
            isVisible = false
            // A pop tears the view down, so the state write above may never round-trip into an
            // onChange. Stopping directly guarantees the loop dies with the screen.
            viewModel.stopPolling()
        }
        .onChange(of: pollKey, initial: true) { _, key in
            if key.shouldPoll {
                viewModel.startPolling()
            } else {
                viewModel.stopPolling()
            }
        }
    }
}

// MARK: - RSRP History Chart

private struct SignalHistoryChart: View {
    let history: [SignalSnapshot]
    let showLTE: Bool
    let show3G: Bool

    var body: some View {
        CardView {
            VStack(alignment: .leading, spacing: 8) {
                Text("Signal History")
                    .font(.headline)

                if history.isEmpty {
                    Text("Collecting data...")
                        .foregroundStyle(.secondary)
                        .frame(height: 150)
                        .frame(maxWidth: .infinity)
                } else {
                    chart
                }
            }
        }
    }

    private var chart: some View {
        Chart {
            ForEach(history) { point in
                if let nrRSRP = point.nrRSRP {
                    LineMark(
                        x: .value("Time", point.timestamp),
                        y: .value("dBm", nrRSRP)
                    )
                    .foregroundStyle(by: .value("Type", "NR"))
                    .interpolationMethod(.catmullRom)
                }
                if showLTE, let lteRSRP = point.lteRSRP {
                    LineMark(
                        x: .value("Time", point.timestamp),
                        y: .value("dBm", lteRSRP)
                    )
                    .foregroundStyle(by: .value("Type", "LTE"))
                    .interpolationMethod(.catmullRom)
                }
                if show3G, let wcdmaRSCP = point.wcdmaRSCP {
                    LineMark(
                        x: .value("Time", point.timestamp),
                        y: .value("dBm", wcdmaRSCP)
                    )
                    .foregroundStyle(by: .value("Type", "3G"))
                    .interpolationMethod(.catmullRom)
                }
            }
        }
        .chartForegroundStyleScale([
            "NR": Color.blue,
            "LTE": Color.orange,
            "3G": Color.purple,
        ])
        .chartYScale(domain: -140...(-40))
        .chartYAxis {
            AxisMarks(values: [-140, -120, -100, -80, -60, -40]) { value in
                AxisGridLine()
                AxisValueLabel {
                    if let v = value.as(Int.self) {
                        Text("\(v)")
                    }
                }
            }
        }
        .chartXAxis {
            AxisMarks(values: .automatic(desiredCount: 4)) { _ in
                AxisGridLine()
                AxisValueLabel(format: .dateTime.minute().second())
            }
        }
        .frame(height: 200)
        .accessibilityLabel("Signal strength history")
    }
}

// MARK: - Cellular Panel

/// The NR and LTE panels differ only in labels, accent and two extra rows, so both render
/// through `CellularPanel`. Feeding it one `Equatable` value gives SwiftUI a comparison
/// boundary: a tick that only moves the other radio's numbers skips this panel's body.
private struct CellularPanelData: Equatable {
    let title: String
    let systemImage: String
    let accent: Color
    let isNR: Bool
    let isConnected: Bool
    let rsrp: Double?
    let rsrq: Double?
    let sinr: Double?
    let rssi: Double?
    let band: String
    let pci: String
    let cellID: String
    let channelLabel: String
    let channel: String
    let bandwidth: String
    let caActive: Bool
    let showsNSAAnchor: Bool
    let showsTotalBandwidth: Bool
    let sccCarriers: [LTECarrier]

    var technology: BandTechnology { isNR ? .nr : .lte }

    var numCC: Int { 1 + sccCarriers.count }

    static func nr(_ signal: NRSignal) -> CellularPanelData {
        CellularPanelData(
            title: "5G NR",
            systemImage: "antenna.radiowaves.left.and.right",
            accent: .blue,
            isNR: true,
            isConnected: signal.isConnected,
            rsrp: signal.rsrp,
            rsrq: signal.rsrq,
            sinr: signal.sinr,
            rssi: signal.rssi,
            band: signal.band,
            pci: signal.pci,
            cellID: signal.cellID,
            channelLabel: "Channel",
            channel: signal.channel,
            bandwidth: signal.bandwidth,
            caActive: !signal.sccCarriers.isEmpty,
            showsNSAAnchor: false,
            showsTotalBandwidth: true,
            sccCarriers: signal.sccCarriers
        )
    }

    static func lte(_ signal: LTESignal, isNSAAnchor: Bool) -> CellularPanelData {
        CellularPanelData(
            title: "LTE",
            systemImage: "cellularbars",
            accent: .orange,
            isNR: false,
            isConnected: signal.isConnected,
            rsrp: signal.rsrp,
            rsrq: signal.rsrq,
            sinr: signal.sinr,
            rssi: signal.rssi,
            band: signal.band,
            pci: signal.pci,
            cellID: signal.cellID,
            channelLabel: "EARFCN",
            channel: signal.earfcn,
            bandwidth: signal.bandwidth,
            caActive: signal.caState != "0" && !signal.sccCarriers.isEmpty,
            showsNSAAnchor: isNSAAnchor,
            showsTotalBandwidth: false,
            sccCarriers: signal.sccCarriers
        )
    }
}

private struct CellularPanel: View, Equatable {
    let data: CellularPanelData

    var body: some View {
        CardView {
            VStack(alignment: .leading, spacing: 8) {
                header

                if data.isConnected {
                    metrics

                    Divider()

                    metadata

                    if !data.sccCarriers.isEmpty {
                        Divider()
                        ForEach(data.sccCarriers) { carrier in
                            SCCCarrierView(carrier: carrier)
                        }
                    }
                }
            }
        }
    }

    private var header: some View {
        HStack {
            Label(data.title, systemImage: data.systemImage)
                .font(.headline)
            if data.showsNSAAnchor {
                badge("NSA Anchor")
            }
            if data.caActive {
                badge("\(data.numCC) CC")
            }
            Spacer()
            if data.isConnected {
                Text(Color.rsrpQuality(data.rsrp))
                    .font(.caption.bold())
                    .foregroundStyle(Color.rsrpColor(data.rsrp))
                    .accessibilityLabel("Signal quality")
                    .accessibilityValue(Color.rsrpQuality(data.rsrp))
            } else {
                Text("Disconnected")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func badge(_ text: String) -> some View {
        Text(text)
            .font(.caption2.bold())
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(data.accent.opacity(0.15), in: Capsule())
            .foregroundStyle(data.accent)
    }

    private var metrics: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                SignalMetricView(label: "RSRP", value: data.rsrp, unit: "dBm", color: Color.rsrpColor(data.rsrp))
                SignalMetricView(label: "RSRQ", value: data.rsrq, unit: "dB", color: Color.rsrqColor(data.rsrq))
            }
            HStack(spacing: 8) {
                SignalMetricView(label: "SINR", value: data.sinr, unit: "dB", color: Color.sinrColor(data.sinr))
                SignalMetricView(label: "RSSI", value: data.rssi, unit: "dBm", color: .primary)
            }
        }
    }

    private var metadata: some View {
        VStack(alignment: .leading, spacing: 4) {
            MetaRow(label: "Band", value: SignalFormat.band(data.band, technology: data.technology, isPCC: data.caActive))
            MetaRow(label: "PCI", value: data.pci)
            MetaRow(label: "Cell ID", value: data.cellID)
            MetaRow(label: data.channelLabel, value: data.channel)
            MetaRow(label: "Bandwidth", value: SignalFormat.bandwidth(data.bandwidth, band: data.band, technology: data.technology))
            MetaRow(label: "CA", value: data.caActive ? "Active (\(data.numCC) CC)" : "Inactive")
            if data.showsTotalBandwidth, data.caActive, let total = totalBandwidth {
                MetaRow(label: "Total BW", value: "\(total) MHz")
            }
        }
    }

    /// PCC bandwidth plus every SCC bandwidth, in MHz. Nil when nothing parsed.
    private var totalBandwidth: Int? {
        let sccValues = data.sccCarriers.compactMap { SignalFormat.bandwidthMHz($0.bandwidth) }
        guard let pcc = SignalFormat.bandwidthMHz(data.bandwidth) else {
            return sccValues.isEmpty ? nil : sccValues.reduce(0, +)
        }
        return pcc + sccValues.reduce(0, +)
    }
}

private struct SCCCarrierView: View {
    let carrier: LTECarrier

    var body: some View {
        let technology: BandTechnology = carrier.label.hasPrefix("5G") ? .nr : .lte

        VStack(alignment: .leading, spacing: 4) {
            Text(carrier.label)
                .font(.caption.bold())
                .foregroundStyle(.secondary)
            HStack(spacing: 12) {
                Text(SignalFormat.carrierBand(carrier.band, technology: technology))
                    .font(.caption2.monospacedDigit())
                Text("PCI \(carrier.pci)")
                    .font(.caption2.monospacedDigit())
                Text("BW \(carrier.bandwidth)")
                    .font(.caption2.monospacedDigit())
            }
            .foregroundStyle(.secondary)
            HStack(spacing: 8) {
                SignalMetricView(label: "RSRP", value: carrier.rsrp, unit: "dBm", color: Color.rsrpColor(carrier.rsrp))
                SignalMetricView(label: "RSRQ", value: carrier.rsrq, unit: "dB", color: Color.rsrqColor(carrier.rsrq))
                SignalMetricView(label: "SINR", value: carrier.sinr, unit: "dB", color: Color.sinrColor(carrier.sinr))
                SignalMetricView(label: "RSSI", value: carrier.rssi, unit: "dBm", color: .primary)
            }
        }
    }
}

// MARK: - WCDMA Panel

private struct WCDMAPanel: View {
    let signal: WCDMASignal

    var body: some View {
        CardView {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Label("WCDMA", systemImage: "antenna.radiowaves.left.and.right.circle")
                        .font(.headline)
                    Spacer()
                    if signal.isConnected {
                        Text(Color.rscpQuality(signal.rscp))
                            .font(.caption.bold())
                            .foregroundStyle(Color.rscpColor(signal.rscp))
                            .accessibilityLabel("Signal quality")
                            .accessibilityValue(Color.rscpQuality(signal.rscp))
                    }
                }
                HStack(spacing: 8) {
                    SignalMetricView(label: "RSCP", value: signal.rscp, unit: "dBm", color: Color.rscpColor(signal.rscp))
                    SignalMetricView(label: "Ec/Io", value: signal.ecio, unit: "dB", color: Color.ecioColor(signal.ecio))
                }
            }
        }
    }
}

// MARK: - Formatting

/// The one place band and bandwidth strings are built. The three renderings deliberately
/// differ: the metadata rows have room for duplex mode and the PCC marker, the SCC rows only
/// for the common name.
private enum SignalFormat {
    static func band(_ band: String, technology: BandTechnology, isPCC: Bool) -> String {
        guard !band.isEmpty else { return "--" }
        let pccSuffix = isPCC ? " · PCC" : ""
        guard let spec = technology.spec(for: band) else { return "B\(band)\(pccSuffix)" }
        return "B\(band) (\(spec.commonName), \(spec.duplexMode.rawValue))\(pccSuffix)"
    }

    static func carrierBand(_ band: String, technology: BandTechnology) -> String {
        guard let spec = technology.spec(for: band) else { return "B\(band)" }
        return "B\(band) (\(spec.commonName))"
    }

    static func bandwidth(_ bandwidth: String, band: String, technology: BandTechnology) -> String {
        if let value = bandwidthMHz(bandwidth), let spec = technology.spec(for: band) {
            return "\(value) / \(spec.maxBandwidthMHz) MHz max"
        }
        return bandwidth.isEmpty ? "--" : bandwidth
    }

    /// Strips the unit off a firmware bandwidth string ("20 MHz" -> 20).
    static func bandwidthMHz(_ raw: String) -> Int? {
        var slice = raw[...]
        while let first = slice.first, first.isLetter || first.isWhitespace { slice.removeFirst() }
        while let last = slice.last, last.isLetter || last.isWhitespace { slice.removeLast() }
        return Int(slice)
    }
}

// MARK: - Rows

private struct MetaRow: View {
    let label: String
    let value: String

    @ScaledMetric(relativeTo: .caption) private var labelWidth: CGFloat = 80

    var body: some View {
        HStack {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(width: labelWidth, alignment: .leading)
            Text(value.isEmpty ? "--" : value)
                .font(.caption.monospacedDigit())
        }
        .accessibilityElement(children: .combine)
    }
}

struct SignalMetricView: View {
    let label: String
    let value: Double?
    let unit: String
    let color: Color

    var body: some View {
        VStack(spacing: 2) {
            Text(label)
                .font(.caption2)
                .foregroundStyle(.secondary)
            Text(value.map { "\(Int($0))" } ?? "--")
                .font(.title3.monospacedDigit().bold())
                .foregroundStyle(color)
                .lineLimit(1)
                .minimumScaleFactor(0.6)
            Text(unit)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 4)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue(value.map { "\(Int($0)) \(unit)" } ?? "Unavailable")
    }
}
