import Foundation

// MARK: - Network Mode

struct NetworkModeConfig: Equatable {
    var netSelect: String       // WL_AND_5G, LTE_AND_5G, Only_5G, Only_LTE, WCDMA_AND_LTE, Only_WCDMA

    static let empty = NetworkModeConfig(netSelect: "WL_AND_5G")

    static let netSelectOptions: [(label: String, value: String)] = [
        ("Auto", "WL_AND_5G"),
        ("5G NSA (LTE + NR)", "LTE_AND_5G"),
        ("5G SA Only", "Only_5G"),
        ("4G Only", "Only_LTE"),
        ("4G + 3G", "WCDMA_AND_LTE"),
        ("3G Only", "Only_WCDMA"),
    ]
}

enum NetworkModeParser {
    static func parse(_ data: [String: Any]) -> NetworkModeConfig {
        NetworkModeConfig(
            netSelect: data["net_select"] as? String ?? ""
        )
    }
}

// MARK: - Cell Lock

struct CellLockStatus: Equatable {
    var nrPCI: String
    var nrEARFCN: String
    var nrBand: String
    var ltePCI: String
    var lteEARFCN: String
    var locked: Bool

    static let empty = CellLockStatus(
        nrPCI: "", nrEARFCN: "", nrBand: "",
        ltePCI: "", lteEARFCN: "", locked: false
    )
}

struct NeighborCell: Equatable, Identifiable {
    let id = UUID()
    var pci: String
    var earfcn: String
    var band: String
    var rsrp: String
    var type: String    // "NR" or "LTE"
}

enum CellLockParser {
    static func parse(_ data: [String: Any]) -> CellLockStatus {
        CellLockStatus(
            nrPCI: data["nr_pci"] as? String ?? data["nr5g_pci"] as? String ?? "",
            nrEARFCN: data["nr_earfcn"] as? String ?? data["nr5g_earfcn"] as? String ?? "",
            nrBand: data["nr_band"] as? String ?? data["nr5g_band"] as? String ?? "",
            ltePCI: data["lte_pci"] as? String ?? "",
            lteEARFCN: data["lte_earfcn"] as? String ?? "",
            locked: asBool(data["cell_lock_status"])
        )
    }

    static func parseNeighbors(_ data: [String: Any], type: String) -> [NeighborCell] {
        guard let cells = data["cell_list"] as? [[String: Any]] else { return [] }
        return cells.map { cell in
            NeighborCell(
                pci: cell["pci"] as? String ?? "",
                earfcn: cell["earfcn"] as? String ?? "",
                band: cell["band"] as? String ?? "",
                rsrp: cell["rsrp"] as? String ?? "",
                type: type
            )
        }
    }

    private static func asBool(_ value: Any?) -> Bool {
        if let str = value as? String {
            return str == "1" || str.lowercased() == "true" || str.lowercased() == "on"
        }
        if let num = value as? Int { return num != 0 }
        if let b = value as? Bool { return b }
        return false
    }
}

// MARK: - STC (Smart Tower Connect)

struct STCConfig: Equatable {
    var lteCollectTimer: String
    var nrsaCollectTimer: String
    var lteWhitelistMax: String
    var nrsaWhitelistMax: String
    var enabled: Bool

    static let empty = STCConfig(
        lteCollectTimer: "", nrsaCollectTimer: "",
        lteWhitelistMax: "", nrsaWhitelistMax: "",
        enabled: false
    )
}

enum STCParser {
    static func parseParams(_ data: [String: Any]) -> STCConfig {
        STCConfig(
            lteCollectTimer: data["lte_collect_timer"] as? String ?? "",
            nrsaCollectTimer: data["nrsa_collect_timer"] as? String ?? "",
            lteWhitelistMax: data["lte_whitelist_max"] as? String ?? "",
            nrsaWhitelistMax: data["nrsa_whitelist_max"] as? String ?? "",
            enabled: asBool(data["stc_enable"])
        )
    }

    static func parseStatus(_ data: [String: Any], into config: STCConfig) -> STCConfig {
        var updated = config
        updated.enabled = asBool(data["stc_enable"]) || asBool(data["status"])
        return updated
    }

    private static func asBool(_ value: Any?) -> Bool {
        if let str = value as? String {
            return str == "1" || str.lowercased() == "true" || str.lowercased() == "on"
        }
        if let num = value as? Int { return num != 0 }
        if let b = value as? Bool { return b }
        return false
    }
}

// MARK: - Signal Detect

struct SignalDetectStatus: Equatable {
    var progress: Int
    var running: Bool
    var results: [SignalQualityResult]

    static let empty = SignalDetectStatus(progress: 0, running: false, results: [])
}

struct SignalQualityResult: Equatable, Identifiable {
    let id = UUID()
    var band: String
    var earfcn: String
    var pci: String
    var rsrp: String
    var rsrq: String
    var sinr: String
    var type: String
}

enum SignalDetectParser {
    static func parseProgress(_ data: [String: Any]) -> SignalDetectStatus {
        SignalDetectStatus(
            progress: asInt(data["progress"]) ?? 0,
            running: asBool(data["running"]),
            results: []
        )
    }

    static func parseResults(_ data: [String: Any]) -> [SignalQualityResult] {
        guard let records = data["record_list"] as? [[String: Any]] else { return [] }
        return records.map { record in
            SignalQualityResult(
                band: record["band"] as? String ?? "",
                earfcn: record["earfcn"] as? String ?? "",
                pci: record["pci"] as? String ?? "",
                rsrp: record["rsrp"] as? String ?? "",
                rsrq: record["rsrq"] as? String ?? "",
                sinr: record["sinr"] as? String ?? "",
                type: record["type"] as? String ?? ""
            )
        }
    }

    private static func asBool(_ value: Any?) -> Bool {
        if let str = value as? String {
            return str == "1" || str.lowercased() == "true" || str.lowercased() == "on"
        }
        if let num = value as? Int { return num != 0 }
        if let b = value as? Bool { return b }
        return false
    }

    private static func asInt(_ val: Any?) -> Int? {
        if let i = val as? Int { return i }
        if let s = val as? String { return Int(s) }
        if let d = val as? Double { return Int(d) }
        return nil
    }
}

// MARK: - Mobile Network

struct MobileNetworkConfig: Equatable {
    var connectMode: Int        // 0=manual, 1=auto
    var roamEnable: Int         // 0=off, 1=on
    var dataEnabled: Int        // 0=off, 1=on
    var connectStatus: String   // "ipv4_ipv6_connected", "disconnected", etc.
    var netSelectMode: String   // "auto_select" / "manual_select"
    var operators: [NetworkOperator]
    var scanStatus: String      // "", "scanning", "done"

    var isAutoConnect: Bool { connectMode == 1 }
    var isRoamingEnabled: Bool { roamEnable == 1 }
    var isDataEnabled: Bool { dataEnabled == 1 }
    var isConnected: Bool { connectStatus.contains("connected") }
    var isAutoNetSelect: Bool { netSelectMode == "auto_select" }

    static let empty = MobileNetworkConfig(
        connectMode: 1, roamEnable: 0, dataEnabled: 0, connectStatus: "",
        netSelectMode: "auto_select", operators: [], scanStatus: ""
    )
}

struct NetworkOperator: Equatable, Identifiable {
    let id = UUID()
    var name: String
    var mccMnc: String
    var rat: String
    var status: String  // "available", "current", "forbidden"
}

enum MobileNetworkParser {
    static func parseWWAN(_ data: [String: Any]) -> (connectMode: Int, roamEnable: Int, dataEnabled: Int, connectStatus: String) {
        let connectMode = asInt(data["connect_mode"]) ?? 1
        let roamEnable = asInt(data["roam_enable"]) ?? 0
        let dataEnabled = asInt(data["enable"]) ?? 1
        let connectStatus = data["connect_status"] as? String ?? ""
        return (connectMode, roamEnable, dataEnabled, connectStatus)
    }

    static func parseNetInfo(_ data: [String: Any]) -> String {
        data["net_select_mode"] as? String ?? "auto_select"
    }

    static func parseScanStatus(_ data: [String: Any]) -> String {
        data["m_netselect_status"] as? String ?? ""
    }

    static func parseScanResults(_ data: [String: Any]) -> [NetworkOperator] {
        guard let list = data["m_netselect_contents"] as? [[String: Any]] else { return [] }
        return list.map { item in
            NetworkOperator(
                name: item["name"] as? String ?? item["operator_name"] as? String ?? "",
                mccMnc: item["mcc_mnc"] as? String ?? item["plmn"] as? String ?? "",
                rat: item["rat"] as? String ?? "",
                status: item["status"] as? String ?? "available"
            )
        }
    }

    static func parseRegisterResult(_ data: [String: Any]) -> String {
        data["m_netselect_result"] as? String ?? ""
    }

    private static func asInt(_ val: Any?) -> Int? {
        if let i = val as? Int { return i }
        if let s = val as? String { return Int(s) }
        if let d = val as? Double { return Int(d) }
        return nil
    }
}

// MARK: - APN

struct APNConfig: Equatable {
    var mode: String            // "0" = auto, "1" = manual
    var profiles: [APNProfile]
    var autoProfiles: [APNProfile]

    var isManual: Bool { mode == "1" }

    static let empty = APNConfig(mode: "", profiles: [], autoProfiles: [])
}

struct APNProfile: Equatable, Identifiable {
    let id: String           // maps to profileId ("manu1", "manu2", ...)
    var name: String         // maps to profilename
    var apn: String          // maps to wanapn
    var pdpType: Int         // 1=IPv4, 2=IPv6, 3=IPv4v6
    var authMode: Int        // 0=none, 1=PAP, 2=CHAP
    var username: String
    var password: String
    var active: Bool         // maps to isEnable

    static let empty = APNProfile(
        id: "", name: "", apn: "", pdpType: 3,
        authMode: 0, username: "", password: "", active: false
    )

    static let pdpTypeOptions: [(label: String, value: Int)] = [
        ("IPv4", 1), ("IPv6", 2), ("IPv4v6", 3)
    ]
    static let authModeOptions: [(label: String, value: Int)] = [
        ("None", 0), ("PAP", 1), ("CHAP", 2)
    ]

    var pdpTypeLabel: String {
        Self.pdpTypeOptions.first { $0.value == pdpType }?.label ?? "IPv4v6"
    }
    var authModeLabel: String {
        Self.authModeOptions.first { $0.value == authMode }?.label ?? "None"
    }
}

enum APNParser {
    static func parseMode(_ data: [String: Any]) -> String {
        if let i = data["apn_mode"] as? Int { return "\(i)" }
        return data["apn_mode"] as? String ?? "0"
    }

    static func parseProfiles(_ data: [String: Any]) -> [APNProfile] {
        guard let list = data["apnListArray"] as? [[String: Any]] else { return [] }
        return list.compactMap { item in
            let id = item["profileId"] as? String ?? ""
            guard !id.isEmpty else { return nil }
            let name = item["profilename"] as? String ?? ""
            let apn = item["wanapn"] as? String ?? ""
            // Filter out empty pre-allocated slots
            guard !name.isEmpty || !apn.isEmpty else { return nil }
            return APNProfile(
                id: id,
                name: name,
                apn: apn,
                pdpType: asInt(item["pdpType"]) ?? 3,
                authMode: asInt(item["pppAuthMode"]) ?? 0,
                username: item["username"] as? String ?? "",
                password: item["password"] as? String ?? "",
                active: asBool(item["isEnable"])
            )
        }
    }

    private static func asInt(_ val: Any?) -> Int? {
        if let i = val as? Int { return i }
        if let s = val as? String { return Int(s) }
        if let d = val as? Double { return Int(d) }
        return nil
    }

    private static func asBool(_ value: Any?) -> Bool {
        if let str = value as? String {
            return str == "1" || str.lowercased() == "true" || str.lowercased() == "on"
        }
        if let num = value as? Int { return num != 0 }
        if let b = value as? Bool { return b }
        return false
    }
}

// MARK: - WiFi

struct WiFiConfig: Equatable {
    var ssid2g: String
    var ssid5g: String
    var key2g: String
    var key5g: String
    var channel2g: String
    var channel5g: String
    var txpower2g: String
    var txpower5g: String
    var encryption2g: String
    var encryption5g: String
    var wifiOnOff: Bool
    var hidden2g: Bool
    var hidden5g: Bool
    var radio2gDisabled: Bool
    var radio5gDisabled: Bool
    var wifi7Enabled: Bool
    var bandwidth2g: String
    var bandwidth5g: String

    static let empty = WiFiConfig(
        ssid2g: "", ssid5g: "", key2g: "", key5g: "",
        channel2g: "auto", channel5g: "auto",
        txpower2g: "100", txpower5g: "100",
        encryption2g: "psk2+ccmp", encryption5g: "psk2+ccmp",
        wifiOnOff: true, hidden2g: false, hidden5g: false,
        radio2gDisabled: false, radio5gDisabled: false,
        wifi7Enabled: false, bandwidth2g: "auto", bandwidth5g: "auto"
    )

    static let channelOptions2g = ["auto", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"]
    static let channelOptions5g = ["auto", "36", "40", "44", "48", "52", "56", "60", "64", "100", "104", "108", "112", "116", "120", "124", "128", "132", "136", "140", "149", "153", "157", "161", "165"]
    static let txpowerOptions = ["10", "20", "30", "40", "50", "60", "70", "80", "90", "100"]
    static let encryptionOptions = ["none", "psk+tkip", "psk+ccmp", "psk2+ccmp", "psk-mixed+ccmp", "sae", "sae-mixed"]
    static let bandwidthOptions2g = ["auto", "EHT20", "EHT40"]
    static let bandwidthOptions5g = ["auto", "EHT20", "EHT40", "EHT80", "EHT160"]

    /// Returns 5GHz channels compatible with the given bandwidth
    static func channels5g(for bandwidth: String) -> [String] {
        switch bandwidth {
        case "EHT160":
            // UNII-1+2 (36-64) and UNII-2C (100-128) only
            return ["auto", "36", "40", "44", "48", "52", "56", "60", "64",
                    "100", "104", "108", "112", "116", "120", "124", "128"]
        case "EHT80":
            // Exclude 132-140 (no full 80MHz block without ch144) and 165
            return ["auto", "36", "40", "44", "48", "52", "56", "60", "64",
                    "100", "104", "108", "112", "116", "120", "124", "128",
                    "149", "153", "157", "161"]
        case "EHT40":
            // Exclude 165 (no 40MHz pair)
            return ["auto", "36", "40", "44", "48", "52", "56", "60", "64",
                    "100", "104", "108", "112", "116", "120", "124", "128",
                    "132", "136", "140", "149", "153", "157", "161"]
        default: // auto, EHT20
            return channelOptions5g
        }
    }

    /// Returns 5GHz bandwidths compatible with the given channel
    static func bandwidths5g(for channel: String) -> [String] {
        guard channel != "auto" else { return bandwidthOptions5g }
        guard let ch = Int(channel) else { return bandwidthOptions5g }
        if ch == 165 {
            return ["auto", "EHT20"]
        } else if ch >= 149 {
            return ["auto", "EHT20", "EHT40", "EHT80"]
        } else if ch >= 132 {
            return ["auto", "EHT20", "EHT40"]
        } else {
            return bandwidthOptions5g
        }
    }

    /// Returns max bandwidth available for a given 5GHz channel
    static func maxBandwidth5g(for channel: String) -> String {
        guard channel != "auto" else { return "EHT160" }
        guard let ch = Int(channel) else { return "EHT160" }
        if ch == 165 { return "EHT20" }
        if ch >= 149 { return "EHT80" }
        if ch >= 132 { return "EHT40" }
        return "EHT160"
    }
}

enum WiFiParser {
    static func parse(_ data: [String: Any]) -> WiFiConfig {
        let isCompanion = data["htmode_2g"] != nil
        return WiFiConfig(
            ssid2g: data["ssid_2g"] as? String ?? "",
            ssid5g: data["ssid_5g"] as? String ?? "",
            key2g: data["key_2g"] as? String ?? "",
            key5g: data["key_5g"] as? String ?? "",
            channel2g: normalizeChannel(data["channel_2g"] as? String),
            channel5g: normalizeChannel(data["channel_5g"] as? String),
            txpower2g: data["txpower_2g"] as? String ?? "100",
            txpower5g: data["txpower_5g"] as? String ?? "100",
            encryption2g: data["encryption_2g"] as? String ?? "psk2+ccmp",
            encryption5g: data["encryption_5g"] as? String ?? "psk2+ccmp",
            wifiOnOff: asBool(data["wifi_onoff"]),
            hidden2g: asBool(data["hidden_2g"]),
            hidden5g: asBool(data["hidden_5g"]),
            radio2gDisabled: asBool(data["radio2_disabled"]),
            radio5gDisabled: asBool(data["radio5_disabled"]),
            wifi7Enabled: isCompanion ? asBool(data["wifi6_switch"]) : false,
            bandwidth2g: isCompanion ? normalizeBandwidth(data["htmode_2g"] as? String, is5g: false) : "auto",
            bandwidth5g: isCompanion ? normalizeBandwidth(data["htmode_5g"] as? String, is5g: true) : "auto"
        )
    }

    static func parseWifi7(_ data: [String: Any]) -> Bool {
        asBool(data["wifi6_switch"])
    }

    static func parseBandwidth(_ data: [String: Any]) -> String {
        normalizeBandwidth(data["htmode"] as? String)
    }

    private static func normalizeBandwidth(_ raw: String?, is5g: Bool = true) -> String {
        guard let raw, !raw.isEmpty else { return "auto" }
        let options = is5g ? WiFiConfig.bandwidthOptions5g : WiFiConfig.bandwidthOptions2g
        return options.contains(raw) ? raw : "auto"
    }

    private static func normalizeChannel(_ raw: String?) -> String {
        guard let raw, !raw.isEmpty, raw != "0" else { return "auto" }
        return raw
    }

    static func asBool(_ value: Any?) -> Bool {
        if let str = value as? String {
            return str == "1" || str.lowercased() == "true" || str.lowercased() == "on"
        }
        if let num = value as? Int { return num != 0 }
        if let b = value as? Bool { return b }
        return false
    }
}

// MARK: - Guest WiFi

struct GuestWiFiConfig: Equatable {
    var enabled2g: Bool
    var enabled5g: Bool
    var ssid: String
    var key: String
    var encryption: String
    var hidden: Bool
    var isolate: Bool
    var activeTime: Int       // minutes (0 = no limit)
    var remainingSeconds: Int // runtime countdown (-1 = not active)

    static let empty = GuestWiFiConfig(
        enabled2g: false, enabled5g: false, ssid: "", key: "", encryption: "psk2+ccmp",
        hidden: false, isolate: true, activeTime: 0, remainingSeconds: -1
    )

    static let activeTimeOptions: [(label: String, minutes: Int)] = [
        ("No Limit", 0),
        ("30 min", 30),
        ("1 hour", 60),
        ("2 hours", 120),
        ("4 hours", 240),
        ("8 hours", 480),
        ("12 hours", 720),
        ("24 hours", 1440)
    ]
}

enum GuestWiFiParser {
    static func parse(_ data: [String: Any]) -> GuestWiFiConfig {
        GuestWiFiConfig(
            enabled2g: !asBool(data["disabled_2g"]),
            enabled5g: !asBool(data["disabled_5g"]),
            ssid: data["ssid"] as? String ?? "",
            key: data["key"] as? String ?? "",
            encryption: data["encryption"] as? String ?? "psk2+ccmp",
            hidden: asBool(data["hidden"]),
            isolate: asBool(data["isolate"]),
            activeTime: asInt(data["guest_active_time"]) ?? 0,
            remainingSeconds: asInt(data["remaining_seconds"]) ?? -1
        )
    }

    private static func asBool(_ value: Any?) -> Bool {
        if let str = value as? String {
            return str == "1" || str.lowercased() == "true" || str.lowercased() == "on"
        }
        if let num = value as? Int { return num != 0 }
        if let b = value as? Bool { return b }
        return false
    }

    private static func asInt(_ val: Any?) -> Int? {
        if let i = val as? Int { return i }
        if let s = val as? String { return Int(s) }
        if let d = val as? Double { return Int(d) }
        return nil
    }
}

// MARK: - LAN/DHCP

struct LANConfig: Equatable {
    var lanIP: String
    var netmask: String
    var dhcpEnabled: Bool
    var dhcpStart: String
    var dhcpEnd: String
    var dhcpLeaseTime: String

    static let empty = LANConfig(
        lanIP: "", netmask: "", dhcpEnabled: false,
        dhcpStart: "", dhcpEnd: "", dhcpLeaseTime: ""
    )
}

enum LANParser {
    static func parse(_ data: [String: Any]) -> LANConfig {
        LANConfig(
            lanIP: data["lan_ipaddr"] as? String ?? "",
            netmask: data["lan_netmask"] as? String ?? "",
            dhcpEnabled: asBool(data["dhcp_enable"]),
            dhcpStart: data["dhcp_start"] as? String ?? "",
            dhcpEnd: data["dhcp_end"] as? String ?? "",
            dhcpLeaseTime: data["dhcp_lease_time"] as? String ?? ""
        )
    }

    private static func asBool(_ value: Any?) -> Bool {
        if let str = value as? String {
            return str == "1" || str.lowercased() == "true" || str.lowercased() == "on"
        }
        if let num = value as? Int { return num != 0 }
        if let b = value as? Bool { return b }
        return false
    }
}

// MARK: - QoS

struct QoSConfig: Equatable {
    var enabled: Bool

    static let empty = QoSConfig(enabled: false)
}

enum QoSParser {
    static func parse(_ data: [String: Any]) -> QoSConfig {
        QoSConfig(enabled: asBool(data["qos_switch"]))
    }

    private static func asBool(_ value: Any?) -> Bool {
        if let str = value as? String {
            return str == "1" || str.lowercased() == "true" || str.lowercased() == "on"
        }
        if let num = value as? Int { return num != 0 }
        if let b = value as? Bool { return b }
        return false
    }
}

// MARK: - VPN Passthrough

struct VPNPassthroughConfig: Equatable {
    var l2tp: Bool
    var pptp: Bool
    var ipsec: Bool

    static let empty = VPNPassthroughConfig(l2tp: false, pptp: false, ipsec: false)
}

enum VPNPassthroughParser {
    static func parse(_ data: [String: Any]) -> VPNPassthroughConfig {
        VPNPassthroughConfig(
            l2tp: asBool(data["l2tp_passthrough"]),
            pptp: asBool(data["pptp_passthrough"]),
            ipsec: asBool(data["ipsec_passthrough"])
        )
    }

    private static func asBool(_ value: Any?) -> Bool {
        if let str = value as? String {
            return str == "1" || str.lowercased() == "true" || str.lowercased() == "on"
        }
        if let num = value as? Int { return num != 0 }
        if let b = value as? Bool { return b }
        return false
    }
}

// MARK: - Scheduled Reboot

struct ScheduleRebootConfig: Equatable {
    var enabled: Bool
    var time: String        // "HH:MM"
    var days: String        // comma-separated day numbers

    static let empty = ScheduleRebootConfig(enabled: false, time: "03:00", days: "")

    static let dayOptions: [(label: String, value: String)] = [
        ("Mon", "1"), ("Tue", "2"), ("Wed", "3"), ("Thu", "4"),
        ("Fri", "5"), ("Sat", "6"), ("Sun", "0")
    ]
}

enum ScheduleRebootParser {
    static func parse(_ data: [String: Any]) -> ScheduleRebootConfig {
        ScheduleRebootConfig(
            enabled: asBool(data["auto_reboot_enable"]),
            time: data["auto_reboot_time"] as? String ?? "03:00",
            days: data["auto_reboot_days"] as? String ?? ""
        )
    }

    private static func asBool(_ value: Any?) -> Bool {
        if let str = value as? String {
            return str == "1" || str.lowercased() == "true" || str.lowercased() == "on"
        }
        if let num = value as? Int { return num != 0 }
        if let b = value as? Bool { return b }
        return false
    }
}
