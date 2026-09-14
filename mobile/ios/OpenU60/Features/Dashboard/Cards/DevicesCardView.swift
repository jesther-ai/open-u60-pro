import SwiftUI

struct DevicesCardView: View {
    let connectedDevices: [ConnectedDevice]
    @Binding var showAllDevices: Bool

    var body: some View {
        CardView {
            VStack(alignment: .leading, spacing: 8) {
                Button {
                    withAnimation { showAllDevices.toggle() }
                } label: {
                    HStack {
                        Label("Connected Devices", systemImage: "laptopcomputer.and.iphone")
                            .font(.headline)
                        Spacer()
                        AnimatedNumber(value: connectedDevices.count,
                                       font: .title3.weight(.bold), textColor: .primary)
                        Image(systemName: showAllDevices ? "chevron.up" : "chevron.down")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .contentTransition(.symbolEffect(.replace))
                    }
                }
                .buttonStyle(.plain)

                if showAllDevices {
                    Divider()
                    let lastID = connectedDevices.last?.id
                    ForEach(connectedDevices) { device in
                        DeviceRow(device: device)
                        if device.id != lastID {
                            Divider()
                        }
                    }
                }
            }
        }
    }
}

/// Holds the already-resolved strings for one row so the row body is a pure layout of `String`s:
/// picking the global IPv6 address and resolving `displayName` happen once per device, in `init`,
/// and `Equatable` lets SwiftUI skip the row entirely when nothing about that device moved.
private struct DeviceRow: View, Equatable {
    let name: String
    let macAddress: String
    let ipAddress: String
    let globalIPv6: String?

    init(device: ConnectedDevice) {
        name = device.displayName
        macAddress = device.macAddress
        ipAddress = device.ipAddress
        globalIPv6 = device.ip6Addresses.first(where: { !$0.hasPrefix("fe80") })
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(name)
                .font(.subheadline.bold())
            HStack {
                Text(macAddress)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
                Spacer()
                if !ipAddress.isEmpty {
                    Text(ipAddress)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }
            }
            if let globalIPv6 {
                Text(globalIPv6)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
        .padding(.vertical, 2)
    }
}
