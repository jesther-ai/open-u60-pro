import Foundation

struct SchedulerJob: Identifiable {
    let id: Int
    var name: String
    var enabled: Bool
    var scheduleType: String
    var scheduleTime: String?
    var scheduleDays: [Int]
    var scheduleAt: Int?
    var actionMethod: String
    var actionPath: String
    var restoreTime: String?
    var lastRun: Int?
    var lastStatus: Int?
    var lastError: String?
    var lastRestore: Int?
    var createdAt: Int

    /// Display-only, so it follows the device locale — unlike the "HH:mm" wire format the
    /// agent parses. Static because `scheduleSummary` is evaluated once per row per render.
    private static let onceDateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter
    }()

    static func parse(_ dict: [String: Any]) -> SchedulerJob? {
        guard let id = dict["id"] as? Int,
              let name = dict["name"] as? String,
              let enabled = dict["enabled"] as? Bool,
              let schedule = dict["schedule"] as? [String: Any],
              let scheduleType = schedule["type"] as? String,
              let action = dict["action"] as? [String: Any],
              let method = action["method"] as? String,
              let path = action["path"] as? String else { return nil }

        let restore = dict["restore"] as? [String: Any]

        return SchedulerJob(
            id: id,
            name: name,
            enabled: enabled,
            scheduleType: scheduleType,
            scheduleTime: schedule["time"] as? String,
            scheduleDays: (schedule["days"] as? [Int]) ?? [],
            scheduleAt: schedule["at"] as? Int,
            actionMethod: method,
            actionPath: path,
            restoreTime: restore?["time"] as? String,
            lastRun: dict["last_run"] as? Int,
            lastStatus: dict["last_status"] as? Int,
            lastError: dict["last_error"] as? String,
            lastRestore: dict["last_restore"] as? Int,
            createdAt: (dict["created_at"] as? Int) ?? 0
        )
    }

    var scheduleSummary: String {
        switch scheduleType {
        case "recurring":
            let time = scheduleTime ?? "??:??"
            let dayNames = ["Mon","Tue","Wed","Thu","Fri","Sat","Sun"]
            let dayStr: String
            if scheduleDays.count == 7 {
                dayStr = "Every day"
            } else if scheduleDays == [0,1,2,3,4] {
                dayStr = "Mon\u{2013}Fri"
            } else if scheduleDays == [5,6] {
                dayStr = "Weekends"
            } else {
                dayStr = scheduleDays.compactMap { dayNames.indices.contains($0) ? dayNames[$0] : nil }.joined(separator: ", ")
            }
            if let rt = restoreTime {
                return "\(dayStr) at \(time) \u{2192} \(rt)"
            }
            return "\(dayStr) at \(time)"
        case "once":
            if let at = scheduleAt {
                let date = Date(timeIntervalSince1970: TimeInterval(at))
                return "Once: \(Self.onceDateFormatter.string(from: date))"
            }
            return "One-time"
        default:
            return scheduleType
        }
    }

    var actionSummary: String {
        ActionTemplate.allCases.first { $0.method == actionMethod && $0.path == actionPath }?.label
            ?? "\(actionMethod) \(actionPath)"
    }
}

enum ActionTemplate: String, CaseIterable, Identifiable {
    case airplaneOn
    case mobileDataOff
    case guestWifiOff
    case reboot
    case powerSaveOn

    var id: String { rawValue }

    var label: String {
        switch self {
        case .airplaneOn: return "Airplane Mode ON"
        case .mobileDataOff: return "Mobile Data OFF"
        case .guestWifiOff: return "Guest WiFi OFF"
        case .reboot: return "Reboot"
        case .powerSaveOn: return "Power Save ON"
        }
    }

    var method: String {
        switch self {
        case .airplaneOn: return "POST"
        case .mobileDataOff, .guestWifiOff, .powerSaveOn: return "PUT"
        case .reboot: return "POST"
        }
    }

    var path: String {
        switch self {
        case .airplaneOn: return "/api/modem/airplane"
        case .mobileDataOff: return "/api/modem/data"
        case .guestWifiOff: return "/api/wifi/guest"
        case .reboot: return "/api/device/reboot"
        case .powerSaveOn: return "/api/device/power-save"
        }
    }

    var actionBody: [String: Any]? {
        switch self {
        case .airplaneOn: return ["operate_mode": "LPM"]
        case .mobileDataOff: return ["cid": 1, "enable": 0, "connect_status": "disconnected"]
        case .guestWifiOff: return ["guest_disabled_2g": "1", "guest_disabled_5g": "1"]
        case .reboot: return nil
        case .powerSaveOn: return ["deviceInfoList": ["power_saver_mode": "1"]]
        }
    }

    var restoreBody: [String: Any]? {
        switch self {
        case .airplaneOn: return ["operate_mode": "ONLINE"]
        case .mobileDataOff: return ["cid": 1, "enable": 1]
        case .guestWifiOff: return ["guest_disabled_2g": "0", "guest_disabled_5g": "0"]
        case .reboot: return nil
        case .powerSaveOn: return ["deviceInfoList": ["power_saver_mode": "0"]]
        }
    }

    var supportsRestore: Bool { self != .reboot }

    static func from(method: String, path: String) -> ActionTemplate? {
        allCases.first { $0.method == method && $0.path == path }
    }

    var systemImage: String {
        switch self {
        case .airplaneOn: return "airplane"
        case .mobileDataOff: return "antenna.radiowaves.left.and.right.slash"
        case .guestWifiOff: return "wifi.slash"
        case .reboot: return "arrow.counterclockwise"
        case .powerSaveOn: return "leaf.fill"
        }
    }
}
