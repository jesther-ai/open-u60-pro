import SwiftUI

@Observable
@MainActor
final class ClientsViewModel {
    var devices: [ConnectedDevice] = []
    var isLoading: Bool = false
    var error: String?

    private let client: AgentClient
    private let authManager: AuthManager

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    func refresh() async {
        isLoading = true
        error = nil

        do {
            let data = try await client.getJSON("/api/network/clients")
            let hostsData = data["hosts"] as? [String: Any] ?? [:]
            var deviceList = DeviceParser.parseHostHints(hostsData)
            if let leases = data["dhcp_leases"] as? [[String: Any]] {
                DeviceParser.enrichWithDHCP(devices: &deviceList, leases: leases)
            }
            devices = deviceList
        } catch {
            if !error.isCancellation {
                self.error = error.localizedDescription
            }
        }

        isLoading = false
    }
}
