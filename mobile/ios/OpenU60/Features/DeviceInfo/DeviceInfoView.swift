import SwiftUI

struct DeviceInfoView: View {
    var viewModel: DeviceInfoViewModel

    var body: some View {
        List {
            Section("Device") {
                infoRow("IMEI", viewModel.identity.imei)
            }

            Section("Network") {
                roamingRow
                signalBarsRow
            }

            Section("WAN") {
                infoRow("IPv4", viewModel.identity.wanIPv4)
                ForEach(viewModel.identity.wanIPv6, id: \.self) { addr in
                    infoRow("IPv6", addr)
                }
                if viewModel.identity.wanIPv6.isEmpty {
                    infoRow("IPv6", "")
                }
            }

            Section("LAN") {
                infoRow("Gateway", viewModel.identity.lanIP)
            }
        }
        .navigationTitle("Device Info")
        .refreshable { await viewModel.refresh() }
        .overlay {
            if viewModel.isLoading {
                ProgressView()
            }
        }
        .task { await viewModel.refresh() }
    }

    // MARK: - Rows

    private func infoRow(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label)
                .foregroundStyle(.secondary)
            Spacer()
            Text(value.isEmpty ? "--" : value)
                .font(.body.monospacedDigit())
                .textSelection(.enabled)
        }
    }

    private var roamingRow: some View {
        let roaming = viewModel.operatorInfo.roaming
        return HStack {
            Text("Roaming")
                .foregroundStyle(.secondary)
            Spacer()
            Text(roaming ? "Roaming" : "Home")
                .font(.body.monospacedDigit())
                .foregroundStyle(roaming ? .orange : .green)
        }
    }

    private var signalBarsRow: some View {
        let bars = viewModel.operatorInfo.signalBar
        let maxBars = 5
        return HStack {
            Text("Signal")
                .foregroundStyle(.secondary)
            Spacer()
            Image(systemName: "cellularbars")
                .foregroundStyle(bars > 0 ? signalColor(bars: bars, max: maxBars) : .secondary)
            Text("\(bars)/\(maxBars)")
                .font(.body.monospacedDigit())
                .foregroundStyle(.secondary)
                .padding(.leading, 4)
        }
    }

    // MARK: - Helpers

    private func signalColor(bars: Int, max: Int) -> Color {
        let ratio = Double(bars) / Double(max)
        if ratio >= 0.6 { return .green }
        if ratio >= 0.4 { return .yellow }
        return .red
    }
}
