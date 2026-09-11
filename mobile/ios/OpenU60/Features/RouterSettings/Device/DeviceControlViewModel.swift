import SwiftUI

@Observable
@MainActor
final class DeviceControlViewModel {
    var isLoading: Bool = false
    var message: String?
    var messageIsError: Bool = false
    var showRebootConfirm: Bool = false
    var showFactoryResetConfirm: Bool = false
    var chargeLimitEnabled: Bool = false
    var chargeLimit: Int = 100
    var hysteresis: Int = 5
    var powerSaveEnabled: Bool = false
    var fastBootEnabled: Bool = false
    var autoSleepEnabled: Bool = false
    var autoSleepTimeout: String = "5"
    private var chargeControlLoaded: Bool = false
    private var powerSaveLoaded: Bool = false
    private var fastBootLoaded: Bool = false
    private var autoSleepLoaded: Bool = false
    private let client: AgentClient
    private let authManager: AuthManager

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    func refresh() async {
        do {
            let resp = try await client.getJSON("/api/device/charge-control")
            let data = resp["data"] as? [String: Any] ?? resp
            if let enabled = data["charge_limit_enabled"] as? Bool {
                chargeLimitEnabled = enabled
            }
            if let limit = data["charge_limit"] as? Int {
                chargeLimit = limit
            }
            if let hyst = data["hysteresis"] as? Int {
                hysteresis = hyst
            }
            chargeControlLoaded = true
        } catch {
            showMessage("Failed to load charge control status", isError: true)
        }

        do {
            let psData = try await client.postJSON("/api/device/power-save", body: ["deviceInfoList": ["power_saver_mode"]])
            let psMode = psData["power_saver_mode"] as? String ?? ""
            if !psMode.isEmpty {
                let newPowerSave = (psMode == "1")
                if newPowerSave != powerSaveEnabled { powerSaveEnabled = newPowerSave }
            }
            powerSaveLoaded = true
        } catch {
            showMessage("Failed to load power-save settings", isError: true)
        }

        do {
            let fbData = try await client.getJSON("/api/device/fast-boot")
            let data = fbData["data"] as? [String: Any] ?? fbData
            let fbMode = data["fast_boot"] as? String ?? ""
            if !fbMode.isEmpty {
                let newFastBoot = (fbMode == "1")
                if newFastBoot != fastBootEnabled { fastBootEnabled = newFastBoot }
            }
            fastBootLoaded = true
        } catch {
            showMessage("Failed to load fast boot settings", isError: true)
        }

        do {
            let asData = try await client.getJSON("/api/device/auto-sleep")
            let data = asData["data"] as? [String: Any] ?? asData
            if let enabled = data["enabled"] as? Bool {
                autoSleepEnabled = enabled
            }
            if let timeout = data["timeout"] as? String, !timeout.isEmpty {
                autoSleepTimeout = timeout
            }
            autoSleepLoaded = true
        } catch {
            showMessage("Failed to load auto-sleep settings", isError: true)
        }
    }

    func setChargeLimit(enabled: Bool, limit: Int, hysteresis: Int? = nil) async {
        guard chargeControlLoaded else { return }
        let prevEnabled = chargeLimitEnabled
        let prevLimit = chargeLimit
        let prevHysteresis = self.hysteresis
        do {
            var body: [String: Any] = [
                "charge_limit_enabled": enabled,
                "charge_limit": limit,
            ]
            if let hyst = hysteresis {
                body["hysteresis"] = hyst
            }
            let resp = try await client.putJSON("/api/device/charge-control", body: body)
            let data = (resp["data"] as? [String: Any]) ?? resp
            if let newEnabled = data["charge_limit_enabled"] as? Bool {
                chargeLimitEnabled = newEnabled
            }
            if let newLimit = data["charge_limit"] as? Int {
                chargeLimit = newLimit
            }
            if let newHyst = data["hysteresis"] as? Int {
                self.hysteresis = newHyst
            }
        } catch {
            chargeLimitEnabled = prevEnabled
            chargeLimit = prevLimit
            self.hysteresis = prevHysteresis
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
    }

    func setPowerSave(enabled: Bool) async {
        guard powerSaveLoaded else { return }
        do {
            let _ = try await client.putJSON("/api/device/power-save", body: ["deviceInfoList": ["power_saver_mode": enabled ? "1" : "0"]])
        } catch {
            powerSaveEnabled = !enabled
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
    }

    func setFastBoot(enabled: Bool) async {
        guard fastBootLoaded else { return }
        do {
            let _ = try await client.putJSON("/api/device/fast-boot", body: ["fast_boot": enabled ? "1" : "0"])
        } catch {
            fastBootEnabled = !enabled
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
    }

    func setAutoSleep(enabled: Bool, timeout: String? = nil) async {
        guard autoSleepLoaded else { return }
        let prevEnabled = autoSleepEnabled
        let prevTimeout = autoSleepTimeout
        do {
            var body: [String: Any] = ["enabled": enabled]
            if let t = timeout { body["timeout"] = t }
            let resp = try await client.putJSON("/api/device/auto-sleep", body: body)
            let data = resp["data"] as? [String: Any] ?? resp
            if let newEnabled = data["enabled"] as? Bool { autoSleepEnabled = newEnabled }
            if let newTimeout = data["timeout"] as? String { autoSleepTimeout = newTimeout }
        } catch {
            autoSleepEnabled = prevEnabled
            autoSleepTimeout = prevTimeout
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
    }

    func reboot() async {
        isLoading = true

        do {
            let _ = try await client.postJSON("/api/device/reboot")
            showMessage("Router is rebooting...", isError: false)
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }

        isLoading = false
    }

    func factoryReset() async {
        isLoading = true

        do {
            let _ = try await client.postJSON("/api/device/factory-reset")
            showMessage("Factory reset initiated...", isError: false)
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }

        isLoading = false
    }

    private func showMessage(_ text: String, isError: Bool) {
        message = text
        messageIsError = isError
    }
}
