import Foundation

struct BatteryStatus: Equatable {
    var capacity: Int = 0
    var temperature: Double = 0.0
    var charging: String = ""
    var chargeStatus: Int = 0
    var timeToFull: Int = -1      // minutes, -1 = unknown
    var timeToEmpty: Int = -1     // minutes, -1 = unknown
    var currentMA: Int?            // milliamps; negative = discharging, positive = charging
    var voltageMV: Int?            // millivolts from agent

    /// Preserve invalid telemetry as unknown instead of displaying or clamping it to full.
    var capacityPercent: Int? { (0...100).contains(capacity) ? capacity : nil }
    var capacityText: String { capacityPercent.map { "\($0)%" } ?? "—" }

    static let empty = BatteryStatus()
}

struct ThermalStatus: Equatable {
    var cpuTemp: Double = 0.0

    static let empty = ThermalStatus()
}

struct TrafficStats: Equatable {
    var rxBytes: UInt64 = 0
    var txBytes: UInt64 = 0
    var timestamp: Date = Date()
    var source: String = ""
    /// Pre-computed rates from the router (bytes/sec), if available
    var precomputedRxRate: Double?
    var precomputedTxRate: Double?
    /// Server-computed speeds from zte-agent (bytes/sec), highest priority
    var serverRxSpeed: Double?
    var serverTxSpeed: Double?

    static let empty = TrafficStats()

    static func == (lhs: TrafficStats, rhs: TrafficStats) -> Bool {
        lhs.rxBytes == rhs.rxBytes && lhs.txBytes == rhs.txBytes
            && lhs.source == rhs.source
            && lhs.precomputedRxRate == rhs.precomputedRxRate
            && lhs.precomputedTxRate == rhs.precomputedTxRate
            && lhs.serverRxSpeed == rhs.serverRxSpeed
            && lhs.serverTxSpeed == rhs.serverTxSpeed
    }
}

struct TrafficSpeed: Equatable {
    var downloadBytesPerSec: Double = 0.0
    var uploadBytesPerSec: Double = 0.0

    static let zero = TrafficSpeed()
}

struct ConnectedDevice: Identifiable, Equatable {
    let id: String // MAC address
    var name: String
    var ipAddress: String
    var ip6Addresses: [String]
    var macAddress: String
    var dhcpHostname: String

    var displayName: String {
        if !dhcpHostname.isEmpty { return dhcpHostname }
        if !name.isEmpty { return name }
        return macAddress
    }
}

struct DeviceIdentity: Equatable {
    var imei: String = ""
    var simICCID: String = ""
    var simIMSI: String = ""
    var msisdn: String = ""
    var wanIPv4: String = ""
    var wanIPv6: [String] = []
    var lanIP: String = ""
    var spn: String = ""
    var mcc: String = ""
    var mnc: String = ""
    var simStatus: String = ""

    static let empty = DeviceIdentity()
}

struct WifiStatus: Equatable {
    var wifiOn: Bool = false
    var ssid2g: String = ""
    var ssid5g: String = ""
    var channel2g: String = ""
    var channel5g: String = ""
    var radio2gDisabled: Bool = false
    var radio5gDisabled: Bool = false
    var encryption2g: String = ""
    var encryption5g: String = ""
    var hidden2g: Bool = false
    var hidden5g: Bool = false
    var txPower2g: String = ""
    var txPower5g: String = ""
    var bandwidth2g: String = ""
    var bandwidth5g: String = ""
    var clientsTotal: Int = 0
    var wifi6: Bool = false
    var guestEnabled: Bool = false
    var guestSsid: String = ""

    static let empty = WifiStatus()
}

struct CpuStatSample: Equatable {
    var idle: UInt64
    var total: UInt64
}

struct SystemInfo: Equatable {
    var cpuUsagePercent: Double = 0.0
    var cpuUsageIsEstimate: Bool = true
    var cpuCores: Int = 1
    var uptime: Int = 0
    var memTotal: UInt64 = 0
    var memFree: UInt64 = 0

    static let empty = SystemInfo()
}

struct USBStatus: Equatable {
    var mode: String = ""
    var typecCC: String = "no_cc"
    var dataConnected: Bool = false
    var powerbankActive: Bool = false
    var cableAttached: Bool { typecCC != "no_cc" }

    static let empty = USBStatus()
}

// MARK: - Parsers

enum DeviceParser {
    static func parseBattery(_ data: [String: Any]) -> BatteryStatus {
        BatteryStatus(
            capacity: asInt(data["battery_capacity"]) ?? 0,
            temperature: asDouble(data["battery_temperature"]) ?? 0.0,
            charging: "",
            timeToFull: asInt(data["battery_time_to_full"]) ?? -1,
            timeToEmpty: asInt(data["battery_time_to_empty"]) ?? -1
        )
    }

    static func parseCharger(_ data: [String: Any], into battery: inout BatteryStatus, chargeControl: [String: Any]? = nil) {
        battery.chargeStatus = asInt(data["charge_status"]) ?? 0
        let chargerConnected = asInt(data["charger_connect"]) == 1
        let chargingStopped = chargeControl?["charging_stopped"] as? Bool ?? false
        if chargerConnected && chargingStopped {
            battery.charging = "stopped"
        } else if battery.chargeStatus == 1 {
            battery.charging = "charging"
        } else {
            battery.charging = "discharging"
        }
    }

    static func parseThermal(_ data: [String: Any]) -> ThermalStatus {
        ThermalStatus(cpuTemp: asDouble(data["cpuss_temp"]) ?? 0.0)
    }

    static func parseTraffic(_ data: [String: Any]) -> TrafficStats {
        let stats = data["statistics"] as? [String: Any] ?? [:]
        return TrafficStats(
            rxBytes: asUInt64(stats["rx_bytes"]) ?? 0,
            txBytes: asUInt64(stats["tx_bytes"]) ?? 0,
            timestamp: Date()
        )
    }

    static func parseWwandstTraffic(_ data: [String: Any]) -> TrafficStats? {
        guard let rx = asUInt64(data["real_rx_bytes"]),
              let tx = asUInt64(data["real_tx_bytes"]) else { return nil }
        var stats = TrafficStats(
            rxBytes: rx,
            txBytes: tx,
            timestamp: Date(),
            source: "wwandst"
        )
        // Zero is a valid idle rate, even when the modem publishes a delayed byte total.
        if let rxRate = asDouble(data["real_rx_speed"]),
           let txRate = asDouble(data["real_tx_speed"]),
           rxRate.isFinite, txRate.isFinite, rxRate >= 0, txRate >= 0 {
            stats.precomputedRxRate = rxRate
            stats.precomputedTxRate = txRate
        }
        return stats
    }

    static func computeSpeed(previous: TrafficStats, current: TrafficStats) -> TrafficSpeed {
        // Priority 1: server-computed speeds from zte-agent (precise Instant timing)
        if let rxSpeed = current.serverRxSpeed,
           let txSpeed = current.serverTxSpeed {
            return TrafficSpeed(
                downloadBytesPerSec: rxSpeed,
                uploadBytesPerSec: txSpeed
            )
        }

        // Priority 2: pre-computed rates from ZTE daemon
        if let rxRate = current.precomputedRxRate,
           let txRate = current.precomputedTxRate {
            return TrafficSpeed(
                downloadBytesPerSec: rxRate,
                uploadBytesPerSec: txRate
            )
        }

        // Priority 3: client-side delta
        // Skip delta computation when source changes to avoid invalid spikes
        if !previous.source.isEmpty && !current.source.isEmpty && previous.source != current.source {
            return .zero
        }

        let elapsed = current.timestamp.timeIntervalSince(previous.timestamp)
        guard elapsed > 0 else { return .zero }

        let rxDelta = current.rxBytes > previous.rxBytes ? current.rxBytes - previous.rxBytes : 0
        let txDelta = current.txBytes > previous.txBytes ? current.txBytes - previous.txBytes : 0

        return TrafficSpeed(
            downloadBytesPerSec: Double(rxDelta) / elapsed,
            uploadBytesPerSec: Double(txDelta) / elapsed
        )
    }

    static func parseHostHints(_ data: [String: Any]) -> [ConnectedDevice] {
        var devices: [ConnectedDevice] = []
        devices.reserveCapacity(data.count)
        for (mac, value) in data {
            guard let info = value as? [String: Any] else { continue }
            let name = info["name"] as? String ?? ""
            let ipAddrs = info["ipaddrs"] as? [String] ?? []
            let ip6Addrs = info["ip6addrs"] as? [String] ?? []
            let ip = ipAddrs.first ?? ""
            devices.append(ConnectedDevice(
                id: mac,
                name: name,
                ipAddress: ip,
                ip6Addresses: ip6Addrs,
                macAddress: mac,
                dhcpHostname: ""
            ))
        }

        // `localizedStandardCompare` is an ICU call per comparison. For dotted-quad IPv4 it orders
        // identically to comparing the packed 32-bit value (its numeric chunk comparison is exactly
        // an octet-wise numeric compare), so take that path when every host has one and fall back
        // to the locale-aware comparator otherwise.
        let keyed = devices.compactMap { device -> (UInt32, ConnectedDevice)? in
            guard let key = ipv4SortKey(device.ipAddress) else { return nil }
            return (key, device)
        }
        if keyed.count == devices.count {
            return keyed.sorted { $0.0 < $1.0 }.map { $0.1 }
        }
        return devices.sorted { $0.ipAddress.localizedStandardCompare($1.ipAddress) == .orderedAscending }
    }

    /// Packs a dotted-quad IPv4 string into a sortable integer; nil for anything else.
    private static func ipv4SortKey(_ ip: String) -> UInt32? {
        var key: UInt32 = 0
        var octets = 0
        for part in ip.split(separator: ".", omittingEmptySubsequences: false) {
            guard octets < 4,
                  !part.isEmpty, part.count <= 3,
                  part.allSatisfy({ $0.isASCII && $0.isNumber }),
                  let octet = UInt32(part), octet <= 255 else { return nil }
            key = key << 8 | octet
            octets += 1
        }
        return octets == 4 ? key : nil
    }

    static func enrichWithDHCP(devices: inout [ConnectedDevice], leases: [[String: Any]]) {
        let leaseMap = Dictionary(
            leases.compactMap { lease -> (String, String)? in
                guard let mac = lease["macaddr"] as? String,
                      let hostname = lease["hostname"] as? String else { return nil }
                return (mac.uppercased(), hostname)
            },
            uniquingKeysWith: { _, latest in latest }
        )
        for i in devices.indices {
            if let hostname = leaseMap[devices[i].macAddress.uppercased()] {
                devices[i].dhcpHostname = hostname
            }
        }
    }

    static func parseIdentity(
        simInfo: [String: Any],
        imeiData: [String: Any],
        wanStatus: [String: Any],
        wan6Status: [String: Any],
        lanStatus: [String: Any]
    ) -> DeviceIdentity {
        var identity = DeviceIdentity()
        identity.simICCID = simInfo["sim_iccid"] as? String ?? ""
        identity.simIMSI = simInfo["sim_imsi"] as? String ?? ""
        identity.msisdn = simInfo["msisdn"] as? String ?? ""
        identity.imei = imeiData["imei"] as? String ?? ""
        identity.mcc = simInfo["mdm_mcc"] as? String ?? ""
        identity.mnc = simInfo["mdm_mnc"] as? String ?? ""
        identity.simStatus = simInfo["sim_states"] as? String ?? ""
        if let spnHex = simInfo["spn_name_data"] as? String {
            identity.spn = decodeSpn(spnHex)
        }

        if let ipv4Arr = wanStatus["ipv4-address"] as? [[String: Any]],
           let first = ipv4Arr.first {
            identity.wanIPv4 = first["address"] as? String ?? ""
        }

        if let ipv6Arr = wan6Status["ipv6-address"] as? [[String: Any]] {
            identity.wanIPv6 = ipv6Arr.compactMap { entry -> String? in
                guard let addr = entry["address"] as? String,
                      !addr.hasPrefix("fe80") else { return nil }
                return addr
            }
        }

        if let lanArr = lanStatus["ipv4-address"] as? [[String: Any]],
           let first = lanArr.first {
            identity.lanIP = first["address"] as? String ?? ""
        }

        return identity
    }

    // MARK: - SPN Decoder

    static func decodeSpn(_ hex: String) -> String {
        let trimmed = hex.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty, trimmed.count % 4 == 0 else { return "" }
        var u16s: [UInt16] = []
        var i = trimmed.startIndex
        while i < trimmed.endIndex {
            let next = trimmed.index(i, offsetBy: 4)
            guard let val = UInt16(trimmed[i..<next], radix: 16), val != 0 else {
                i = next; continue
            }
            u16s.append(val)
            i = next
        }
        return String(utf16CodeUnits: u16s, count: u16s.count)
    }

    // MARK: - USB Parser

    static func parseUSBStatus(_ usbData: [String: Any], chargerData: [String: Any]?) -> USBStatus {
        USBStatus(
            mode: usbData["mode"] as? String ?? "",
            typecCC: usbData["typec_cc"] as? String ?? "no_cc",
            dataConnected: asInt(usbData["connect"]) == 1,
            powerbankActive: asInt(chargerData?["otg_powerbank_state"]) == 1
        )
    }

    // MARK: - WiFi Parser

    static func parseWifiStatus(_ data: [String: Any]) -> WifiStatus {
        WifiStatus(
            wifiOn: (data["wifi_onoff"] as? String) == "1",
            ssid2g: data["main2g_ssid"] as? String ?? "",
            ssid5g: data["main5g_ssid"] as? String ?? "",
            radio2gDisabled: (data["radio2_disabled"] as? String) == "1",
            radio5gDisabled: (data["radio5_disabled"] as? String) == "1"
        )
    }

    static func parseWifiChannels(_ data: [String: Any], into status: inout WifiStatus) {
        status.channel2g = data["radio2"] as? String ?? ""
        status.channel5g = data["radio5"] as? String ?? ""
    }

    static func parseWifiInterfaces(_ data: [String: Any], into status: inout WifiStatus) {
        guard let ifaces = data["ifaces"] as? [[String: Any]] else { return }
        for iface in ifaces {
            let section = iface["section_name"] as? String ?? ""
            let encryption = iface["encryption"] as? String ?? ""
            let hidden = (iface["hidden"] as? String) == "1"
            if section == "main_2g" {
                status.encryption2g = formatEncryption(encryption)
                status.hidden2g = hidden
            } else if section == "main_5g" {
                status.encryption5g = formatEncryption(encryption)
                status.hidden5g = hidden
            }
        }
    }

    static func parseWifiTxPower(_ data: [String: Any], band: String, into status: inout WifiStatus) {
        let percent = data["txpowerpercent"] as? String ?? ""
        let htmode = data["htmode"] as? String ?? ""
        let options = band == "2g" ? WiFiConfig.bandwidthOptions2g : WiFiConfig.bandwidthOptions5g
        let normalizedBw = options.contains(htmode) ? htmode : (htmode.isEmpty ? "" : "auto")
        if band == "2g" {
            status.txPower2g = percent
            status.bandwidth2g = normalizedBw
        } else if band == "5g" {
            status.txPower5g = percent
            status.bandwidth5g = normalizedBw
        }
    }

    static func parseWifiClients(_ data: [String: Any], into status: inout WifiStatus) {
        status.clientsTotal = asInt(data["assoc_num"]) ?? 0
    }

    static func parseWifi6(_ data: [String: Any], into status: inout WifiStatus) {
        status.wifi6 = (data["wifi6_switch"] as? String) == "1"
    }

    static func formatEncryption(_ raw: String) -> String {
        switch raw.lowercased() {
        case "psk2": return "WPA2"
        case "psk2+ccmp": return "WPA2"
        case "sae": return "WPA3"
        case "sae-mixed", "sae+psk2": return "WPA2/3"
        case "psk-mixed", "psk+psk2": return "WPA/2"
        case "psk": return "WPA"
        case "none", "": return "Open"
        default: return raw.uppercased()
        }
    }

    // MARK: - System Parser

    static func parseSystemInfo(_ data: [String: Any], cpuCores: Int = 1) -> SystemInfo {
        var info = SystemInfo()
        info.cpuCores = cpuCores
        if let load = data["load"] as? [Any], !load.isEmpty {
            let load1 = asDouble(load[0]) ?? 0.0
            let loadAvg = load1 / 65536.0
            info.cpuUsagePercent = min(loadAvg / Double(max(cpuCores, 1)) * 100.0, 100.0)
            info.cpuUsageIsEstimate = true
        }
        if let uptime = asInt(data["uptime"]) {
            info.uptime = uptime
        }
        if let total = asUInt64(data["memory_total"]) ?? asUInt64((data["memory"] as? [String: Any])?["total"]) {
            info.memTotal = total
        }
        if let free = asUInt64(data["memory_free"]) ?? asUInt64((data["memory"] as? [String: Any])?["free"]) {
            info.memFree = free
        }
        return info
    }

    // MARK: - CPU /proc/stat Parser

    static func parseBatteryCurrent(_ text: String) -> Int? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let microamps = Int(trimmed) else { return nil }
        return microamps / 1000
    }

    static func parseProcStat(_ text: String) -> CpuStatSample? {
        for line in text.split(separator: "\n") {
            guard line.hasPrefix("cpu ") else { continue }
            let fields = line.split(separator: " ", omittingEmptySubsequences: true)
            // fields[0] = "cpu", fields[1..] = user, nice, system, idle, iowait, ...
            guard fields.count >= 5 else { return nil }
            let values = fields.dropFirst().compactMap { UInt64($0) }
            guard values.count >= 4 else { return nil }
            var idle = values[3]
            if values.count > 4 {
                idle += values[4] // iowait
            }
            let total = values.reduce(0, +)
            return CpuStatSample(idle: idle, total: total)
        }
        return nil
    }

    static func parseCpuCoreCount(_ text: String) -> Int {
        var count = 0
        for line in text.split(separator: "\n") {
            if line.hasPrefix("cpu") && !line.hasPrefix("cpu ") {
                count += 1
            }
        }
        return max(count, 1)
    }

    static func computeCpuUsage(previous: CpuStatSample, current: CpuStatSample) -> Double? {
        let dTotal = current.total &- previous.total
        let dIdle = current.idle &- previous.idle
        guard dTotal > 0 else { return nil }
        let usage = (1.0 - Double(dIdle) / Double(dTotal)) * 100.0
        return (usage * 10.0).rounded() / 10.0
    }

    // MARK: - WAN Parser

    static func parseWanIPv4(_ data: [String: Any]) -> String {
        if let ipv4Arr = data["ipv4-address"] as? [[String: Any]],
           let first = ipv4Arr.first,
           let addr = first["address"] as? String {
            return addr
        }
        return ""
    }

    static func parseWanIPv6(_ data: [String: Any]) -> String {
        if let ipv6Arr = data["ipv6-address"] as? [[String: Any]] {
            for entry in ipv6Arr {
                if let addr = entry["address"] as? String, !addr.hasPrefix("fe80") {
                    return addr
                }
            }
        }
        if let ipv6Prefix = data["ipv6-prefix-assignment"] as? [[String: Any]] {
            for entry in ipv6Prefix {
                if let addr = entry["address"] as? String, !addr.hasPrefix("fe80") {
                    return addr
                }
            }
        }
        return ""
    }

    // MARK: - Formatted Value (for AnimatedNumber)

    struct FormattedValue: Equatable {
        let number: Double
        let unit: String
        let decimalPlaces: Int
    }

    private static func adaptiveDecimals(_ value: Double) -> Int {
        let abs = Swift.abs(value)
        if abs < 10 { return 2 }
        if abs < 100 { return 1 }
        return 0
    }

    private static func roundTo(_ v: Double, decimals: Int) -> Double {
        let factor = pow(10.0, Double(decimals))
        return (v * factor).rounded() / factor
    }

    static func speedComponents(_ bytesPerSec: Double) -> FormattedValue {
        let bits = bytesPerSec * 8.0
        let gb = 1_000_000_000.0
        let mb = 1_000_000.0
        let kb = 1_000.0
        let (raw, unit): (Double, String)
        if bits >= gb { (raw, unit) = (bits / gb, " Gb/s") }
        else if bits >= mb { (raw, unit) = (bits / mb, " Mb/s") }
        else if bits >= kb { (raw, unit) = (bits / kb, " Kb/s") }
        else { (raw, unit) = (bits, " b/s") }
        return FormattedValue(number: roundTo(raw, decimals: 1), unit: unit, decimalPlaces: 1)
    }

    static func bytesComponents(_ bytes: UInt64) -> FormattedValue {
        let b = Double(bytes)
        let tb = 1024.0 * 1024.0 * 1024.0 * 1024.0
        let gb = 1024.0 * 1024.0 * 1024.0
        let mb = 1024.0 * 1024.0
        let kb = 1024.0
        let (raw, unit): (Double, String)
        if b >= tb { (raw, unit) = (b / tb, " TB") }
        else if b >= gb { (raw, unit) = (b / gb, " GB") }
        else if b >= mb { (raw, unit) = (b / mb, " MB") }
        else if b >= kb { (raw, unit) = (b / kb, " KB") }
        else { (raw, unit) = (b, " B") }
        let dp = adaptiveDecimals(raw)
        return FormattedValue(number: roundTo(raw, decimals: dp), unit: unit, decimalPlaces: dp)
    }

    // MARK: - Formatting Helpers

    static func formatSpeed(_ bytesPerSec: Double) -> String {
        let c = speedComponents(bytesPerSec)
        return String(format: "%.\(c.decimalPlaces)f\(c.unit.trimmingCharacters(in: .whitespaces))", c.number)
    }

    static func formatBytes(_ bytes: UInt64) -> String {
        let c = bytesComponents(bytes)
        return String(format: "%.\(c.decimalPlaces)f\(c.unit.trimmingCharacters(in: .whitespaces))", c.number)
    }

    // MARK: - Helpers

    private static func asInt(_ val: Any?) -> Int? {
        if let i = val as? Int { return i }
        if let s = val as? String { return Int(s) }
        if let d = val as? Double { return Int(d) }
        return nil
    }

    private static func asDouble(_ val: Any?) -> Double? {
        if let d = val as? Double { return d }
        if let i = val as? Int { return Double(i) }
        if let s = val as? String { return Double(s) }
        return nil
    }

    private static func asUInt64(_ val: Any?) -> UInt64? {
        if let i = val as? Int { return UInt64(i) }
        if let i = val as? UInt64 { return i }
        if let d = val as? Double { return UInt64(d) }
        if let s = val as? String { return UInt64(s) }
        return nil
    }
}

// MARK: - Process Monitor

struct ProcessInfo: Codable, Identifiable {
    let pid: Int
    let name: String
    let cpuPct: Double
    let rssKb: Int
    let state: String
    let isBloat: Bool
    var id: Int { pid }

    enum CodingKeys: String, CodingKey {
        case pid, name, state
        case cpuPct = "cpu_pct"
        case rssKb = "rss_kb"
        case isBloat = "is_bloat"
    }
}

struct ProcessListResponse: Codable {
    let processes: [ProcessInfo]
    let totalCount: Int
    let bloatCount: Int
    let bloatCpuPct: Double
    let bloatRssKb: Int

    enum CodingKeys: String, CodingKey {
        case processes
        case totalCount = "total_count"
        case bloatCount = "bloat_count"
        case bloatCpuPct = "bloat_cpu_pct"
        case bloatRssKb = "bloat_rss_kb"
    }
}

struct KillBloatResponse: Codable {
    let killed: [KilledProcess]
    let skipped: [KilledProcess]
    let freedRssKb: Int

    enum CodingKeys: String, CodingKey {
        case killed, skipped
        case freedRssKb = "freed_rss_kb"
    }
}

struct KilledProcess: Codable {
    let pid: Int
    let name: String
}
