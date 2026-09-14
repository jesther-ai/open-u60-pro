import SwiftUI

@Observable
@MainActor
final class FirewallSettingsViewModel {
    var config: FirewallConfig = .empty
    var portForwardRules: [PortForwardRule] = []
    var filterRules: [FilterRule] = []
    var upnpEnabled: Bool = false
    var isLoading: Bool = false
    var message: String?
    var messageIsError: Bool = false
    var showAddPortForward: Bool = false

    // DMZ edit fields
    var editDmzEnabled: Bool = false
    var editDmzIP: String = ""

    private let client: AgentClient
    private let authManager: AuthManager

    /// `FirewallParser` falls back to the array index when the router omits a rule id, so a local
    /// list that has drifted out of sync makes a delete act on whatever rule now sits at that
    /// index. Set whenever a mutation lands but the follow-up list read fails.
    private var portForwardListIsStale: Bool = false

    init(client: AgentClient, authManager: AuthManager) {
        self.client = client
        self.authManager = authManager
    }

    func refresh() async {
        isLoading = true
        message = nil

        do {
            let data = try await client.getJSON("/api/router/firewall")
            config = FirewallParser.parseConfig(data)
            editDmzEnabled = config.dmzEnabled
            editDmzIP = config.dmzHost

            async let pfResult = fetchPortForwardRules()
            async let filterResult = fetchFilterRules()
            async let upnpResult = fetchUPnP()

            let rules = await pfResult
            portForwardRules = rules ?? []
            portForwardListIsStale = rules == nil
            filterRules = await filterResult
            upnpEnabled = await upnpResult
        } catch {
            if !error.isCancellation {
                showMessage("Failed to load firewall: \(error.localizedDescription)", isError: true)
            }
        }

        isLoading = false
    }

    /// Returns `nil` when the list could not be fetched, so callers can tell "no rules" apart
    /// from "we don't know".
    private func fetchPortForwardRules() async -> [PortForwardRule]? {
        do {
            let data = try await client.getJSON("/api/router/firewall/port-forward")
            return FirewallParser.parsePortForwardRules(data)
        } catch {
            return nil
        }
    }

    /// Replaces the local rules with the router's so every id is re-derived after a mutation.
    /// Returns `false` when the list could not be read, leaving the local ids marked stale.
    private func reloadPortForwardRules() async -> Bool {
        guard let refreshed = await fetchPortForwardRules() else {
            portForwardListIsStale = true
            return false
        }
        portForwardRules = refreshed
        portForwardListIsStale = false
        return true
    }

    private func fetchFilterRules() async -> [FilterRule] {
        do {
            let data = try await client.getJSON("/api/router/firewall/filter-rules")
            return FirewallParser.parseFilterRules(data)
        } catch {
            return []
        }
    }

    private func fetchUPnP() async -> Bool {
        do {
            let data = try await client.getJSON("/api/router/firewall/upnp")
            if let str = data["upnp_switch"] as? String {
                return str == "1"
            }
            return false
        } catch {
            return false
        }
    }

    func toggleFirewall(enabled: Bool) async {
        isLoading = true
        do {
            let _ = try await client.putJSON("/api/router/firewall/switch", body: ["firewall_switch": enabled ? "1" : "0"])
            showMessage("Firewall \(enabled ? "enabled" : "disabled")", isError: false)
            config.enabled = enabled
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
        isLoading = false
    }

    func setLevel(_ level: String) async {
        isLoading = true
        do {
            let _ = try await client.putJSON("/api/router/firewall/level", body: ["firewall_level": level])
            showMessage("Firewall level set to \(level)", isError: false)
            config.level = level
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
        isLoading = false
    }

    func toggleNAT(enabled: Bool) async {
        isLoading = true
        do {
            let _ = try await client.putJSON("/api/router/firewall/nat", body: ["nat_switch": enabled ? "1" : "0"])
            showMessage("NAT \(enabled ? "enabled" : "disabled")", isError: false)
            config.nat = enabled
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
        isLoading = false
    }

    func toggleUPnP(enabled: Bool) async {
        isLoading = true
        do {
            let _ = try await client.putJSON("/api/router/firewall/upnp", body: ["upnp_switch": enabled ? "1" : "0"])
            showMessage("UPnP \(enabled ? "enabled" : "disabled")", isError: false)
            upnpEnabled = enabled
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
        isLoading = false
    }

    func applyDMZ() async {
        isLoading = true
        do {
            let _ = try await client.putJSON("/api/router/firewall/dmz", body: [
                "dmz_enabled": editDmzEnabled ? "1" : "0",
                "dmz_ip": editDmzIP
            ])
            showMessage("DMZ settings updated", isError: false)
            config.dmzEnabled = editDmzEnabled
            config.dmzHost = editDmzIP
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
        isLoading = false
    }

    func togglePortForward(enabled: Bool) async {
        isLoading = true
        do {
            let _ = try await client.putJSON("/api/router/firewall/port-forward/switch", body: ["port_forward_switch": enabled ? "1" : "0"])
            showMessage("Port forwarding \(enabled ? "enabled" : "disabled")", isError: false)
            config.portForwardEnabled = enabled
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
        isLoading = false
    }

    func addPortForward(name: String, protocol_: String, wanPort: String, lanIP: String, lanPort: String) async {
        isLoading = true
        do {
            let _ = try await client.postJSON("/api/router/firewall/port-forward", body: [
                "action": "add",
                "name": name,
                "protocol": protocol_,
                "wan_port": wanPort,
                "lan_ip": lanIP,
                "lan_port": lanPort,
                "enabled": "1"
            ])
            showAddPortForward = false
            // Re-read the list so the new row carries the router's own id. A client-side id would
            // make a later delete a no-op on the router and silently leave the WAN port open.
            if await reloadPortForwardRules() {
                showMessage("Port forward rule added", isError: false)
            } else {
                // The router accepted the rule, so show it rather than let it vanish. Its id is a
                // placeholder the router would not recognise, which is why `deletePortForward`
                // re-reads the list before acting on a stale one.
                portForwardRules.append(PortForwardRule(
                    id: "pending-\(UUID().uuidString)",
                    name: name,
                    protocol_: protocol_,
                    wanPort: wanPort,
                    lanIP: lanIP,
                    lanPort: lanPort,
                    enabled: true
                ))
                showMessage("Rule added, but the list could not be reloaded. Pull to refresh.", isError: true)
            }
        } catch {
            showMessage("Failed: \(error.localizedDescription)", isError: true)
        }
        isLoading = false
    }

    func deletePortForward(_ rule: PortForwardRule) async {
        isLoading = true

        // Sending an id from a list we know is out of sync would delete whichever rule now sits at
        // that index. Reload first and let the user re-pick from the authoritative list.
        if portForwardListIsStale {
            if await reloadPortForwardRules() {
                showMessage("The rule list was out of date and has been reloaded. Try again.", isError: true)
            } else {
                showMessage("The rule list is out of date and could not be reloaded. Pull to refresh.", isError: true)
            }
            isLoading = false
            return
        }

        do {
            let _ = try await client.postJSON("/api/router/firewall/port-forward", body: [
                "action": "delete",
                "id": rule.id
            ])
            // Deleting shifts the remaining rules up, so ids derived from the array index no longer
            // match. Re-read for the same reason the add path does.
            if await reloadPortForwardRules() {
                showMessage("Port forward rule deleted", isError: false)
            } else {
                portForwardRules.removeAll { $0.id == rule.id }
                showMessage("Rule deleted, but the list could not be reloaded. Pull to refresh.", isError: true)
            }
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
