import Foundation

struct NRSignal: Equatable {
    var rsrp: Double?
    var rsrq: Double?
    var sinr: Double?
    var rssi: Double?
    var band: String = ""
    var pci: String = ""
    var cellID: String = ""
    var channel: String = ""
    var bandwidth: String = ""
    var carrierAggregation: String = ""
    var sccCarriers: [LTECarrier] = []

    static let empty = NRSignal()

    var isConnected: Bool { rsrp != nil }

    var hasSignal: Bool {
        isConnected || sccCarriers.contains(where: { $0.rsrp != nil })
    }
}

struct LTECarrier: Equatable, Identifiable {
    var id: String { "\(label)-\(band)-\(pci)-\(earfcn)" }
    var label: String = ""
    var pci: String = ""
    var band: String = ""
    var earfcn: String = ""
    var bandwidth: String = ""
    var rsrp: Double?
    var rsrq: Double?
    var sinr: Double?
    var rssi: Double?
}

struct LTESignal: Equatable {
    var rsrp: Double?
    var rsrq: Double?
    var sinr: Double?
    var rssi: Double?
    var pci: String = ""
    var band: String = ""
    var earfcn: String = ""
    var bandwidth: String = ""
    var cellID: String = ""
    var carrierAggregation: String = ""
    var caState: String = ""
    var sccCarriers: [LTECarrier] = []

    static let empty = LTESignal()

    var isConnected: Bool { rsrp != nil }

    var hasSignal: Bool {
        isConnected || sccCarriers.contains(where: { $0.rsrp != nil })
    }
}

struct WCDMASignal: Equatable {
    var rscp: Double?
    var ecio: Double?

    static let empty = WCDMASignal()

    var isConnected: Bool { rscp != nil }
}

struct OperatorInfo: Equatable {
    var provider: String = ""
    var networkType: String = ""
    var signalBar: Int = 0
    var roaming: Bool = false

    static let empty = OperatorInfo()

    enum NetworkMode: Equatable {
        case sa, nsa, lte, legacy, unknown
    }

    var networkMode: NetworkMode {
        let raw = networkType.uppercased()
        if raw == "SA" || raw.contains("5G SA") || raw.contains("NR SA") || raw.contains("NR-SA") { return .sa }
        if raw.contains("NSA") || raw == "ENDC" || raw == "EN-DC" { return .nsa }
        if raw.contains("LTE") || raw == "4G" || raw == "4G+" { return .lte }
        if raw.contains("WCDMA") || raw.contains("UMTS") || raw.contains("GSM")
            || raw.contains("2G") || raw.contains("3G") { return .legacy }
        return .unknown
    }

    func displayNetworkType(nrConnected: Bool, lteSignal: LTESignal = .empty) -> String {
        // Firmware says LTE but NR is actually connected → 5G NSA
        if nrConnected && (networkMode == .lte || networkMode == .unknown) {
            return "5G NSA"
        }
        // Firmware still says SA/NSA but NR has dropped → fall back to 4G
        if !nrConnected && (networkMode == .sa || networkMode == .nsa) {
            if lteSignal.isConnected {
                return lteSignal.sccCarriers.isEmpty ? "4G" : "4G+"
            }
            return "4G"
        }
        switch networkMode {
        case .sa: return "5G SA"
        case .nsa: return "5G NSA"
        case .lte:
            let raw = networkType.uppercased()
            return (raw.contains("CA") || raw == "4G+" || raw.contains("LTE-A") || raw.contains("LTE+"))
                ? "4G+" : "4G"
        case .legacy: return networkType
        case .unknown: return networkType
        }
    }

    func showNR(nr: NRSignal) -> Bool {
        nr.hasSignal
    }

    func showLTE(lte: LTESignal) -> Bool {
        if networkMode == .sa { return false }
        let raw = networkType.uppercased()
        let hasData = lte.hasSignal
        let actHintsLTE = raw.contains("NSA") || raw.contains("LTE") || raw.contains("E-UTRAN")
            || raw.contains("ENDC") || raw.contains("EN-DC") || raw == "4G" || raw == "4G+"
        let actHintsNR = raw.contains("SA") || raw.contains("NR") || raw.contains("5G")
            || raw.contains("ENDC") || raw.contains("EN-DC")
        return hasData && (actHintsLTE || raw.isEmpty || actHintsNR)
    }

    func show3G(nr: NRSignal, lte: LTESignal, wcdma: WCDMASignal) -> Bool {
        !showNR(nr: nr) && !showLTE(lte: lte) && (wcdma.rscp != nil || wcdma.ecio != nil)
    }
}

struct SignalSnapshot: Identifiable, Equatable {
    let id = UUID()
    let timestamp: Date
    let nrRSRP: Double?
    let lteRSRP: Double?
    let wcdmaRSCP: Double?

    init(timestamp: Date, nrRSRP: Double?, lteRSRP: Double?, wcdmaRSCP: Double? = nil) {
        self.timestamp = timestamp
        self.nrRSRP = nrRSRP
        self.lteRSRP = lteRSRP
        self.wcdmaRSCP = wcdmaRSCP
    }
}

/// Parser that extracts signal data from the agent nwinfo_get_netinfo response.
enum SignalParser {

    /// One entry of an `lteca` / `nrca` list: "PCI,Band,Index,EARFCN,BW".
    private typealias CarrierEntry = (pci: String, band: String, earfcn: String, bandwidth: String)

    /// One entry of an `ltecasig` / `nrcasig` list: "RSRP,RSRQ,SINR,RSSI,...".
    private typealias CarrierSignal = (rsrp: Double?, rsrq: Double?, sinr: Double?, rssi: Double?)

    static func parseNetInfo(_ data: [String: Any]) -> (NRSignal, LTESignal, WCDMASignal, OperatorInfo) {
        var nr = NRSignal()
        var lte = LTESignal()
        var wcdma = WCDMASignal()
        var op = OperatorInfo()

        let nrcaStr = stringVal(data["nrca"])

        nr.rsrp = parseDouble(data["nr5g_rsrp"]).flatMap { $0 == 0 ? nil : $0 }
        nr.rsrq = parseDouble(data["nr5g_rsrq"])
        nr.sinr = parseDouble(data["nr5g_snr"])
        nr.rssi = parseDouble(data["nr5g_rssi"])
        nr.band = stringVal(data["nr5g_action_band"])
        nr.pci = stringVal(data["nr5g_pci"])
        nr.cellID = stringVal(data["nr5g_cell_id"])
        nr.channel = stringVal(data["nr5g_action_channel"])
        nr.bandwidth = stringVal(data["nr5g_bandwidth"])
        nr.carrierAggregation = nrcaStr

        // nrca/nrcasig use the same wire format as lteca/ltecasig.
        let nrSplit = splitPCC(parseCarriers(nrcaStr), pci: nr.pci, channel: nr.channel)
        if let pccBandwidth = nrSplit.pccBandwidth { nr.bandwidth = pccBandwidth }
        nr.sccCarriers = makeCarriers(
            nrSplit.sccs,
            signals: parseCarrierSignals(stringVal(data["nrcasig"])),
            labelPrefix: "5G SCC"
        )

        let pccPci = stringVal(data["lte_pci"])
        let pccEarfcn = stringVal(data["wan_active_channel"])
        lte.rsrp = parseDouble(data["lte_rsrp"]).flatMap { $0 == 0 ? nil : $0 }
        lte.rsrq = parseDouble(data["lte_rsrq"])
        lte.sinr = parseDouble(data["lte_snr"])
        lte.rssi = parseDouble(data["lte_rssi"])
        lte.pci = pccPci
        lte.earfcn = pccEarfcn
        lte.band = stringVal(data["wan_active_band"])
        lte.cellID = stringVal(data["cell_id"])
        lte.caState = stringVal(data["lteca_state"])

        let ltecaStr = stringVal(data["lteca"])
        lte.carrierAggregation = ltecaStr

        let lteSplit = splitPCC(parseCarriers(ltecaStr), pci: pccPci, channel: pccEarfcn)
        if let pccBandwidth = lteSplit.pccBandwidth { lte.bandwidth = pccBandwidth }
        lte.sccCarriers = makeCarriers(
            lteSplit.sccs,
            signals: parseCarrierSignals(stringVal(data["ltecasig"])),
            labelPrefix: "SCC"
        )

        wcdma.rscp = parseDouble(data["rscp"]).flatMap { $0 == 0 ? nil : $0 }
        wcdma.ecio = parseDouble(data["ecio"])

        op.provider = stringVal(data["network_provider"])
        op.networkType = stringVal(data["network_type"])
        op.signalBar = Int(stringVal(data["signalbar"])) ?? 0
        op.roaming = stringVal(data["simcard_roam"]) == "1"

        return (nr, lte, wcdma, op)
    }

    // MARK: - Carrier lists

    /// `split` already omits empty subsequences, so leading/trailing/repeated separators need
    /// no pre-trimming.
    private static func parseCarriers(_ raw: String) -> [CarrierEntry] {
        var entries: [CarrierEntry] = []
        for entry in raw.split(separator: ";") {
            let parts = entry.split(separator: ",")
            guard parts.count >= 5 else { continue }
            entries.append((
                pci: String(parts[0]),
                band: String(parts[1]),
                earfcn: String(parts[3]),
                bandwidth: String(parts[4])
            ))
        }
        return entries
    }

    private static func parseCarrierSignals(_ raw: String) -> [CarrierSignal] {
        var signals: [CarrierSignal] = []
        for entry in raw.split(separator: ";") {
            let parts = entry.split(separator: ",")
            guard parts.count >= 4 else { continue }
            signals.append((
                rsrp: parseField(parts[0]),
                rsrq: parseField(parts[1]),
                sinr: parseField(parts[2]),
                rssi: parseField(parts[3])
            ))
        }
        return signals
    }

    /// Separates the primary carrier — matched on PCI plus channel — from the secondaries.
    /// - Returns: the PCC's own bandwidth when the firmware reported one, and the rest.
    private static func splitPCC(_ carriers: [CarrierEntry], pci: String, channel: String)
        -> (pccBandwidth: String?, sccs: [CarrierEntry]) {
        var pccBandwidth: String?
        var sccs: [CarrierEntry] = []
        var pccFound = false
        for carrier in carriers {
            if !pccFound && !pci.isEmpty && carrier.pci == pci && carrier.earfcn == channel {
                if !carrier.bandwidth.isEmpty { pccBandwidth = carrier.bandwidth }
                pccFound = true
            } else {
                sccs.append(carrier)
            }
        }
        return (pccBandwidth, sccs)
    }

    private static func makeCarriers(_ entries: [CarrierEntry], signals: [CarrierSignal], labelPrefix: String) -> [LTECarrier] {
        entries.enumerated().map { index, entry in
            var carrier = LTECarrier(
                label: "\(labelPrefix)\(index)",
                pci: entry.pci,
                band: entry.band,
                earfcn: entry.earfcn,
                bandwidth: entry.bandwidth
            )
            if index < signals.count {
                carrier.rsrp = signals[index].rsrp
                carrier.rsrq = signals[index].rsrq
                carrier.sinr = signals[index].sinr
                carrier.rssi = signals[index].rssi
            }
            return carrier
        }
    }

    // MARK: - Scalars

    /// Parses one CSV field in place, without materialising a `String` for it.
    private static func parseField(_ field: Substring) -> Double? {
        var slice = field
        while let first = slice.first, first.isWhitespace { slice.removeFirst() }
        while let last = slice.last, last.isWhitespace { slice.removeLast() }
        return Double(slice)
    }

    private static func parseDouble(_ value: Any?) -> Double? {
        var result: Double?
        if let d = value as? Double { result = d }
        else if let i = value as? Int { result = Double(i) }
        else if let s = value as? String {
            let trimmed = s.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty || trimmed == "--" || trimmed == "N/A" { return nil }
            result = Double(trimmed)
        }
        if let r = result, r > 9000 || r < -9000 { return nil }
        return result
    }

    private static func stringVal(_ value: Any?) -> String {
        if let s = value as? String { return s }
        if let i = value as? Int { return String(i) }
        if let d = value as? Double { return String(d) }
        return ""
    }
}
