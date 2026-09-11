import SwiftUI

struct BatteryCardView: View, Equatable {
    let battery: BatteryStatus

    var body: some View {
        CardView {
            VStack(spacing: 8) {
                Image(systemName: batteryIcon(battery.capacity))
                    .font(.title2)
                    .foregroundStyle(Color.batteryColor(battery.capacity))
                // A battery percentage must render as one exact value, not independent reels.
                Text(battery.capacityText)
                    .font(.title3.weight(.bold).monospacedDigit())
                    .transaction { $0.animation = nil }
                batteryStatusLine
                if battery.temperature > 0 {
                    AnimatedNumber(value: battery.temperature, decimalPlaces: 0,
                                   font: .caption, textColor: .secondary, suffix: "\u{00B0}C")
                }
                Text("Battery")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity)
        }
    }

    @ViewBuilder
    private var batteryStatusLine: some View {
        HStack(spacing: 4) {
            batteryStatusText
            if let ma = battery.currentMA {
                Text("\u{00B7}").font(.caption).foregroundStyle(.secondary)
                if let mv = battery.voltageMV {
                    let watts = Double(mv) * Double(abs(ma)) / 1_000_000.0
                    AnimatedNumber(value: watts, decimalPlaces: 1,
                                   font: .caption.monospacedDigit(),
                                   textColor: batteryStatusColor,
                                   suffix: "W")
                } else {
                    AnimatedNumber(value: ma,
                                   font: .caption.monospacedDigit(),
                                   textColor: batteryStatusColor,
                                   prefix: ma >= 0 ? "+" : nil,
                                   suffix: "mA")
                }
            }
        }
        .lineLimit(1)
        .minimumScaleFactor(0.8)
    }

    @ViewBuilder
    private var batteryStatusText: some View {
        switch battery.charging {
        case "stopped":
            Text("Charge Stopped")
                .font(.caption)
                .foregroundStyle(.orange)
        case "charging":
            if battery.capacityPercent == 100 {
                Text("Full")
                    .font(.caption)
                    .foregroundStyle(.green)
            } else if battery.currentMA != nil {
                Text("Charging")
                    .font(.caption)
                    .foregroundStyle(.green)
            } else if battery.timeToFull > 0 {
                Text("Charging \u{00B7} \(formatETA(battery.timeToFull))")
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.green)
            } else {
                Text("Charging")
                    .font(.caption)
                    .foregroundStyle(.green)
            }
        default:
            if battery.timeToEmpty > 0 {
                if battery.currentMA != nil {
                    Text(formatETA(battery.timeToEmpty))
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(Color.batteryColor(battery.capacity))
                } else {
                    Text("\(formatETA(battery.timeToEmpty)) left")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(Color.batteryColor(battery.capacity))
                }
            } else {
                Text("Discharging")
                    .font(.caption)
                    .foregroundStyle(Color.batteryColor(battery.capacity))
            }
        }
    }

    private var batteryStatusColor: Color {
        switch battery.charging {
        case "stopped": return .orange
        case "charging": return .green
        default: return Color.batteryColor(battery.capacity)
        }
    }

    private func formatETA(_ minutes: Int) -> String {
        if minutes >= 1440 {
            let d = minutes / 1440, h = (minutes % 1440) / 60, m = minutes % 60
            return "\(d)d \(h)h \(m)m"
        }
        return minutes >= 60 ? "\(minutes / 60)h \(minutes % 60)m" : "\(minutes)m"
    }

    private func batteryIcon(_ percent: Int) -> String {
        guard (0...100).contains(percent) else { return "battery.0" }
        if percent >= 75 { return "battery.100" }
        if percent >= 50 { return "battery.75" }
        if percent >= 25 { return "battery.50" }
        return "battery.25"
    }
}

struct BatteryDetailSheet: View {
    let battery: BatteryStatus

    var body: some View {
        NavigationStack {
            List {
                row("Capacity", icon: batteryIcon(battery.capacity), value: battery.capacityText)
                row("Status", icon: "bolt.fill", value: statusLabel)
                if let mv = battery.voltageMV {
                    row("Voltage", icon: "bolt.circle", value: String(format: "%.3f V", Double(mv) / 1000.0))
                }
                if let ma = battery.currentMA {
                    row("Current", icon: "arrow.left.arrow.right", value: "\(ma > 0 ? "+" : "")\(ma) mA")
                }
                if let mv = battery.voltageMV, let ma = battery.currentMA {
                    let watts = Double(mv) * Double(abs(ma)) / 1_000_000.0
                    row("Power", icon: "flame", value: String(format: "%.1f W", watts))
                }
                row("Temperature", icon: "thermometer.medium", value: String(format: "%.1f \u{00B0}C", battery.temperature))
                row("Time to Full", icon: "battery.100.bolt", value: battery.charging == "charging" && battery.timeToFull > 0 ? formatETA(battery.timeToFull) : "—")
                row("Time to Empty", icon: "battery.25", value: battery.charging == "discharging" && battery.timeToEmpty > 0 ? formatETA(battery.timeToEmpty) : "—")
            }
            .navigationTitle("Battery Details")
            .navigationBarTitleDisplayMode(.inline)
        }
    }

    private func row(_ label: String, icon: String, value: String) -> some View {
        HStack {
            Label(label, systemImage: icon)
            Spacer()
            Text(value)
                .foregroundStyle(.secondary)
                .monospacedDigit()
        }
    }

    private var statusLabel: String {
        switch battery.charging {
        case "stopped": return "Charge Stopped"
        case "charging": return battery.capacityPercent == 100 ? "Full" : "Charging"
        default: return "Discharging"
        }
    }

    private func formatETA(_ minutes: Int) -> String {
        if minutes >= 1440 {
            let d = minutes / 1440, h = (minutes % 1440) / 60, m = minutes % 60
            return "\(d)d \(h)h \(m)m"
        }
        return minutes >= 60 ? "\(minutes / 60)h \(minutes % 60)m" : "\(minutes)m"
    }

    private func batteryIcon(_ percent: Int) -> String {
        guard (0...100).contains(percent) else { return "battery.0" }
        if percent >= 75 { return "battery.100" }
        if percent >= 50 { return "battery.75" }
        if percent >= 25 { return "battery.50" }
        return "battery.25"
    }
}
