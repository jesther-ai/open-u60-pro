import Foundation

// Compile with Core/Models/DeviceModels.swift and Core/Models/RouterSettingsModels.swift.
@main
struct BatteryStatusTests {
    static func main() {
        var battery = DeviceParser.parseBattery([
            "battery_capacity": 100, "battery_temperature": 41,
            "battery_time_to_full": 0, "battery_time_to_empty": 1166
        ])
        precondition(battery.capacityText == "100%")
        DeviceParser.parseCharger([
            "charger_connect": 1, "charge_status": 4
        ], into: &battery, chargeControl: ["charging_stopped": true])
        precondition(battery.charging == "stopped")
        precondition(battery.capacityText == "100%")

        // The complete percentage stays correct across both sides of a digit-width change.
        for capacity in [99, 100, 99, 9, 10, 0, 100] {
            battery.capacity = capacity
            precondition(battery.capacityText == "\(capacity)%")
        }
        for invalid in [-1, 101, 199] {
            battery.capacity = invalid
            precondition(battery.capacityPercent == nil)
            precondition(battery.capacityText == "—")
        }
        print("Battery percentage and charge-stopped regression checks passed")
    }
}
