import Foundation

// MARK: - Status

struct TailscaleLastUpdate: Equatable {
    var version: String
    var ok: Bool
    var error: String?
    var finishedAt: Int
}

struct TailscaleStatus: Equatable {
    var installed: Bool
    var enabled: Bool
    var enrolled: Bool
    var daemonRunning: Bool
    var state: String
    var backendState: String?
    var health: [String]
    var tailscaleIPs: [String]
    var dnsName: String?
    var relay: String?
    var direct: Bool
    var keyExpiry: String?
    var expired: Bool
    var clientVersion: String?
    var loginURL: String?
    var wakelockHeld: Bool
    var updating: Bool
    var lastUpdate: TailscaleLastUpdate?
    var hostname: String
    var advertiseRoutes: [String]
    var tailscaleSSH: Bool
    var holdWakelock: Bool
    var pinnedVersion: String
    /// The key is absent from the payload except when `state == "starting"`.
    var error: String?

    static let empty = TailscaleStatus(
        installed: false, enabled: false, enrolled: false, daemonRunning: false,
        state: "unknown", backendState: nil, health: [], tailscaleIPs: [],
        dnsName: nil, relay: nil, direct: false, keyExpiry: nil, expired: false,
        clientVersion: nil, loginURL: nil, wakelockHeld: false, updating: false,
        lastUpdate: nil, hostname: "u60-pro", advertiseRoutes: [],
        tailscaleSSH: false, holdWakelock: true, pinnedVersion: "", error: nil
    )

    /// dns_name without the trailing dot.
    var displayDNSName: String? {
        guard let dnsName, !dnsName.isEmpty else { return nil }
        return dnsName.hasSuffix(".") ? String(dnsName.dropLast()) : dnsName
    }

    /// First tailnet address (the one users copy).
    var primaryIP: String? { tailscaleIPs.first }

    /// Relayed is EXPECTED behind CGNAT-to-CGNAT — render neutrally.
    var pathDescription: String {
        if direct { return "Direct" }
        if let relay, !relay.isEmpty { return "Via relay (\(relay))" }
        return "Via relay"
    }

    /// hold_wakelock is intent; wakelock_held is reality.
    var wakelockFailing: Bool { enabled && holdWakelock && !wakelockHeld }

    /// A version swap that did not take effect.
    var versionMismatch: Bool {
        guard !pinnedVersion.isEmpty, let clientVersion, !clientVersion.isEmpty else { return false }
        return pinnedVersion != clientVersion
    }

    /// key_expiry is RFC3339. A null key_expiry means expiry is disabled, which
    /// is the good outcome — it has no date and must never render as "unknown".
    var keyExpiryDate: Date? {
        guard let keyExpiry, !keyExpiry.isEmpty else { return nil }
        return RFC3339.fractional.date(from: keyExpiry) ?? RFC3339.plain.date(from: keyExpiry)
    }

    var displayKeyExpiry: String {
        guard let keyExpiry, !keyExpiry.isEmpty else { return "Never" }
        guard let date = keyExpiryDate else { return keyExpiry }
        return date.formatted(date: .abbreviated, time: .shortened)
    }

    /// Null unless the key expires within 14 days — an expiry already in the past
    /// belongs to the "Node key expired" card, and a null one means "never".
    /// The count is a seconds-based ceiling because Android computes it that way:
    /// the two platforms must name the same N in "expires in N days".
    var daysUntilKeyExpiry: Int? {
        guard let date = keyExpiryDate else { return nil }
        let seconds = date.timeIntervalSinceNow
        guard seconds >= 1, seconds < 14 * 86_400 else { return nil }
        return (Int(seconds) + 86_399) / 86_400
    }
}

private enum RFC3339 {
    static let fractional: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()
    static let plain = ISO8601DateFormatter()
}

// MARK: - Parsing

enum TailscaleParser {
    /// Total: never throws, never returns nil, defaults every field.
    static func parse(_ data: [String: Any]) -> TailscaleStatus {
        TailscaleStatus(
            installed: data["installed"] as? Bool ?? false,
            enabled: data["enabled"] as? Bool ?? false,
            enrolled: data["enrolled"] as? Bool ?? false,
            daemonRunning: data["daemon_running"] as? Bool ?? false,
            state: data["state"] as? String ?? "unknown",
            backendState: data["backend_state"] as? String,
            health: data["health"] as? [String] ?? [],
            // tailscale_ips can be NULL (not just []) when unenrolled.
            tailscaleIPs: data["tailscale_ips"] as? [String] ?? [],
            dnsName: data["dns_name"] as? String,
            relay: data["relay"] as? String,
            direct: data["direct"] as? Bool ?? false,
            keyExpiry: data["key_expiry"] as? String,
            expired: data["expired"] as? Bool ?? false,
            clientVersion: data["client_version"] as? String,
            loginURL: data["login_url"] as? String,
            wakelockHeld: data["wakelock_held"] as? Bool ?? false,
            updating: data["updating"] as? Bool ?? false,
            lastUpdate: parseLastUpdate(data["last_update"] as? [String: Any]),
            hostname: data["hostname"] as? String ?? "u60-pro",
            advertiseRoutes: data["advertise_routes"] as? [String] ?? [],
            tailscaleSSH: data["tailscale_ssh"] as? Bool ?? false,
            holdWakelock: data["hold_wakelock"] as? Bool ?? true,
            pinnedVersion: data["pinned_version"] as? String ?? "",
            error: data["error"] as? String
        )
    }

    static func parseLastUpdate(_ data: [String: Any]?) -> TailscaleLastUpdate? {
        guard let data else { return nil }
        return TailscaleLastUpdate(
            version: data["version"] as? String ?? "",
            ok: data["ok"] as? Bool ?? false,
            error: data["error"] as? String,
            finishedAt: data["finished_at"] as? Int ?? 0
        )
    }
}

// MARK: - Validation

/// Mirrors the agent's own rules so a bad field is caught before the round trip.
/// The server stays authoritative — these only pre-empt.
enum TailscaleValidator {
    /// Mirrors `TsConfig::apply_patch`, config.rs:57-62.
    static func hostname(_ raw: String) -> String? {
        let value = raw.trimmingCharacters(in: .whitespaces)
        guard !value.isEmpty, value.count <= 63,
              value.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-") }) else {
            return "Use 1-63 letters, numbers or hyphens."
        }
        return nil
    }

    /// Mirrors `valid_cidr`, config.rs:83-93 — IPv4 only, even though the
    /// tailnet itself supports IPv6.
    static func routes(_ raw: String) -> String? {
        for entry in parseRoutes(raw) where !isValidCIDR(entry) {
            return "Only IPv4 routes like 192.168.0.0/24 are supported."
        }
        return nil
    }

    /// Comma-separated field ⇄ array. An empty field yields `[]`, which clears
    /// the advertised routes — a real capability, not an error.
    static func parseRoutes(_ raw: String) -> [String] {
        raw.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    /// Mirrors mod.rs:1116.
    static func version(_ raw: String) -> String? {
        let value = raw.trimmingCharacters(in: .whitespaces)
        guard !value.isEmpty, value.allSatisfy({ $0.isASCII && ($0.isNumber || $0 == ".") }) else {
            return "Use digits and dots only, like 1.98.9."
        }
        return nil
    }

    /// Mirrors mod.rs:1120. The agent lowercases server-side, so accept either case.
    static func sha256(_ raw: String) -> String? {
        let value = raw.trimmingCharacters(in: .whitespaces)
        guard value.count == 64, value.allSatisfy({ $0.isASCII && $0.isHexDigit }) else {
            return "Must be exactly 64 hexadecimal characters."
        }
        return nil
    }

    /// `valid_cidr` does not require host bits to be zero, so `192.168.0.5/24`
    /// is accepted server-side. Do not be stricter than the server.
    private static func isValidCIDR(_ entry: String) -> Bool {
        let parts = entry.split(separator: "/", maxSplits: 1, omittingEmptySubsequences: false)
        guard parts.count == 2, let prefix = UInt8(parts[1]), prefix <= 32 else { return false }
        let octets = parts[0].split(separator: ".", omittingEmptySubsequences: false)
        return octets.count == 4 && octets.allSatisfy { UInt8($0) != nil }
    }
}

// MARK: - Release lookup

enum TailscaleReleaseError: LocalizedError {
    case fetchFailed
    case noARM64
    case badSHA

    var errorDescription: String? {
        switch self {
        case .fetchFailed:
            return "Couldn't reach pkgs.tailscale.com to look up the latest release. Check this phone's internet connection, or enter a version and checksum manually under Advanced."
        case .noARM64:
            return "The latest Tailscale release doesn't publish an arm64 build, which this router needs. Enter a version and checksum manually under Advanced."
        case .badSHA:
            return "pkgs.tailscale.com returned a checksum this app couldn't read. Enter a version and checksum manually under Advanced."
        }
    }
}

/// Resolves the current stable release straight from pkgs.tailscale.com.
///
/// Deliberately not routed through `AgentClient`: that client is pinned to the
/// router's base URL and attaches the agent bearer token, which must never
/// leave the LAN. A failure here is the phone's internet, never the agent.
enum TailscaleReleaseFetcher {
    /// The device is aarch64 and the agent's download URL hardcodes arm64
    /// (mod.rs:1175). The two MUST agree or the sha256 can never match.
    private static let arch = "arm64"
    private static let indexURL = "https://pkgs.tailscale.com/stable/?mode=json"

    static func resolveLatest() async throws -> (version: String, sha256: String) {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 20
        config.timeoutIntervalForResource = 20
        let session = URLSession(configuration: config)
        defer { session.invalidateAndCancel() }

        let indexData = try await fetch(indexURL, on: session, failure: .fetchFailed)
        guard let index = try? JSONSerialization.jsonObject(with: indexData) as? [String: Any] else {
            throw TailscaleReleaseError.fetchFailed
        }
        guard let tarballs = index["Tarballs"] as? [String: Any],
              let tarball = tarballs[arch] as? String, !tarball.isEmpty else {
            throw TailscaleReleaseError.noARM64
        }
        // A version the agent would reject is a lookup failure, not a 400 to
        // discover server-side.
        guard let version = index["TarballsVersion"] as? String,
              TailscaleValidator.version(version) == nil else {
            throw TailscaleReleaseError.fetchFailed
        }

        let shaURL = "https://pkgs.tailscale.com/stable/tailscale_\(version)_\(arch).tgz.sha256"
        // A sidecar that never arrives is a reachability failure, not a checksum
        // we couldn't read — .badSHA is only for a body we did receive.
        let shaData = try await fetch(shaURL, on: session, failure: .fetchFailed)
        // Today the body is a bare hash, but `<hash>  <filename>` is the common
        // sidecar shape and must not break us.
        guard let body = String(data: shaData, encoding: .utf8),
              let first = body.split(whereSeparator: { $0.isWhitespace }).first else {
            throw TailscaleReleaseError.badSHA
        }
        let sha256 = first.lowercased()
        guard TailscaleValidator.sha256(sha256) == nil else { throw TailscaleReleaseError.badSHA }
        return (version, sha256)
    }

    private static func fetch(
        _ urlString: String,
        on session: URLSession,
        failure: TailscaleReleaseError
    ) async throws -> Data {
        guard let url = URL(string: urlString), url.scheme == "https" else { throw failure }
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(from: url)
        } catch {
            throw failure
        }
        guard let http = response as? HTTPURLResponse, (200...299).contains(http.statusCode) else {
            throw failure
        }
        return data
    }
}
