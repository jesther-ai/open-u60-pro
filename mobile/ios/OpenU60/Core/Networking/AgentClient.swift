import Foundation

/// REST client for the zte-agent HTTP API.
///
/// `baseURL` and `token` stay main-actor state because Settings observes them, but the transport
/// and the JSON parse run off the main actor: one dashboard tick decodes sixteen payloads, the
/// largest of which has a hundred-plus keys.
@Observable
@MainActor
final class AgentClient {
    var baseURL: String
    var token: String?

    /// Invoked when a request comes back 401. Returning `true` means a fresh session was
    /// established, and the request is replayed exactly once. `AuthManager` installs this so
    /// call sites never have to implement session recovery themselves.
    @ObservationIgnored
    var onUnauthorized: (@MainActor () async -> Bool)?

    private let session: URLSession

    init(baseURL: String = "http://192.168.0.1:9090") {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 10
        config.timeoutIntervalForResource = 15
        // Every response is live device state polled on a timer: it must not be served from a
        // cache, nor written to the shared on-disk one. The interface is deliberately left to the
        // routing table — the configured host is whatever the user typed, and pinning the session
        // to Wi-Fi would fail every request as "Cannot reach the agent" with no way to relax it.
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.urlCache = nil
        self.session = URLSession(configuration: config)
        self.baseURL = baseURL
    }

    // MARK: - Typed REST methods

    func get<T: Decodable>(_ path: String) async throws -> T {
        try await send(method: "GET", path: path, body: nil) { try AgentClient.decodeEnvelope($0) }
    }

    func post<T: Decodable>(_ path: String, body: (any Encodable)? = nil) async throws -> T {
        let bodyData = try encodeBody(body)
        return try await send(method: "POST", path: path, body: bodyData) { try AgentClient.decodeEnvelope($0) }
    }

    func put<T: Decodable>(_ path: String, body: (any Encodable)? = nil) async throws -> T {
        let bodyData = try encodeBody(body)
        return try await send(method: "PUT", path: path, body: bodyData) { try AgentClient.decodeEnvelope($0) }
    }

    // MARK: - Raw JSON

    /// GET a path and return the `data` field as a raw dictionary.
    /// Useful for endpoints where the response format matches `[String: Any]` parsers.
    ///
    /// Pass `retryOnUnauthorized: false` when the host on the other end is not known to be the
    /// agent — a discovery probe, say. A 401 then surfaces as `AgentError.unauthorized` without
    /// waking the session-recovery hook, which would otherwise offer the router password to
    /// whatever happens to be listening on that address.
    func getJSON(_ path: String, retryOnUnauthorized: Bool = true) async throws -> [String: Any] {
        let payload = try await envelopeData(
            method: "GET",
            path: path,
            body: nil,
            retryOnUnauthorized: retryOnUnauthorized
        )
        return payload as? [String: Any] ?? [:]
    }

    /// GET a path and return the `data` field as an array of dictionaries.
    func getJSONArray(_ path: String) async throws -> [[String: Any]] {
        let payload = try await envelopeData(method: "GET", path: path, body: nil)
        return payload as? [[String: Any]] ?? []
    }

    /// POST with a raw dict body and return the `data` field as a raw dictionary.
    func postJSON(_ path: String, body: [String: Any] = [:]) async throws -> [String: Any] {
        let bodyData = try JSONSerialization.data(withJSONObject: body)
        let payload = try await envelopeData(method: "POST", path: path, body: bodyData)
        return payload as? [String: Any] ?? [:]
    }

    /// PUT with a raw dict body and return the `data` field as a raw dictionary.
    func putJSON(_ path: String, body: [String: Any] = [:]) async throws -> [String: Any] {
        let bodyData = try JSONSerialization.data(withJSONObject: body)
        let payload = try await envelopeData(method: "PUT", path: path, body: bodyData)
        return payload as? [String: Any] ?? [:]
    }

    /// DELETE with a raw dict body and return the `data` field as a raw dictionary.
    func deleteJSON(_ path: String, body: [String: Any] = [:]) async throws -> [String: Any] {
        let bodyData = try JSONSerialization.data(withJSONObject: body)
        let payload = try await envelopeData(method: "DELETE", path: path, body: bodyData)
        return payload as? [String: Any] ?? [:]
    }

    // MARK: - Auth

    /// Login with plaintext password. Returns the token string.
    @discardableResult
    func login(password: String) async throws -> String {
        let bodyData = try JSONEncoder().encode(["password": password])
        // `authenticated: false` also keeps this request out of the 401 retry hook, which would
        // otherwise be able to call back into login and recurse.
        let receivedToken = try await send(
            method: "POST",
            path: "/api/auth/login",
            body: bodyData,
            authenticated: false
        ) { data -> String in
            guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                throw AgentError.decodingError("Expected JSON object from login")
            }
            guard let ok = json["ok"] as? Bool, ok,
                  let dataDict = json["data"] as? [String: Any],
                  let issued = dataDict["token"] as? String, !issued.isEmpty else {
                throw AgentError.unauthorized
            }
            return issued
        }

        token = receivedToken
        return receivedToken
    }

    /// Check if the agent is reachable. Any HTTP response counts as reachable.
    func ping() async -> Bool {
        guard let url = URL(string: baseURL) else { return false }
        var req = URLRequest(url: url)
        req.httpMethod = "HEAD"
        req.timeoutInterval = 3
        do {
            let (_, response) = try await session.data(for: req)
            return response is HTTPURLResponse
        } catch {
            return false
        }
    }

    // MARK: - Internal

    /// Runs a request whose response is the `{ ok, data, error }` envelope and hands back the raw
    /// `data` field, leaving the shape cast to the caller.
    private func envelopeData(
        method: String,
        path: String,
        body: Data?,
        retryOnUnauthorized: Bool = true
    ) async throws -> Any? {
        try await send(
            method: method,
            path: path,
            body: body,
            retryOnUnauthorized: retryOnUnauthorized
        ) { data -> Any? in
            guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                throw AgentError.decodingError("Expected JSON object")
            }
            guard let ok = json["ok"] as? Bool, ok else {
                throw AgentError.serverError(json["error"] as? String ?? "Unknown error")
            }
            return json["data"]
        }
    }

    /// Builds the request on the main actor, runs and parses it off the main actor, and gives the
    /// 401 hook one chance to refresh the session before the failure escapes.
    private func send<T>(
        method: String,
        path: String,
        body: Data?,
        authenticated: Bool = true,
        retryOnUnauthorized: Bool = true,
        parse: @escaping @Sendable (Data) throws -> T
    ) async throws -> T {
        do {
            let request = try makeRequest(method: method, path: path, body: body, authenticated: authenticated)
            return try await AgentClient.perform(request, on: session, parse: parse).value
        } catch let error as AgentError where error.isUnauthorized {
            guard retryOnUnauthorized, authenticated, let onUnauthorized, await onUnauthorized() else { throw error }
            // Rebuilt so it picks up the token the hook just installed. Errors from here escape:
            // a request is retried at most once.
            let retry = try makeRequest(method: method, path: path, body: body, authenticated: true)
            return try await AgentClient.perform(retry, on: session, parse: parse).value
        }
    }

    private func makeRequest(method: String, path: String, body: Data?, authenticated: Bool) throws -> URLRequest {
        guard let url = URL(string: baseURL + path) else {
            throw AgentError.serverUnreachable
        }

        var req = URLRequest(url: url)
        req.httpMethod = method
        if body != nil {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        req.httpBody = body

        if authenticated, let token {
            req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        return req
    }

    /// The off-actor half of a request: transport plus parse, neither of which needs the main
    /// actor and both of which are the expensive part of a poll tick.
    private nonisolated static func perform<T>(
        _ request: URLRequest,
        on session: URLSession,
        parse: @escaping @Sendable (Data) throws -> T
    ) async throws -> UnsafeTransfer<T> {
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch let error as URLError {
            throw translate(error)
        } catch {
            throw AgentError.networkError(error)
        }

        guard let httpResponse = response as? HTTPURLResponse else {
            throw AgentError.serverUnreachable
        }

        switch httpResponse.statusCode {
        case 200...299:
            return UnsafeTransfer(value: try parse(data))
        case 401:
            throw AgentError.unauthorized
        default:
            let message = String(data: data, encoding: .utf8) ?? "HTTP \(httpResponse.statusCode)"
            throw AgentError.serverError(message)
        }
    }

    private nonisolated static func translate(_ error: URLError) -> AgentError {
        switch error.code {
        case .timedOut:
            return .timeout
        case .cancelled:
            return .cancelled
        case .cannotConnectToHost, .notConnectedToInternet, .networkConnectionLost, .cannotFindHost:
            return .serverUnreachable
        default:
            return .networkError(error)
        }
    }

    private nonisolated static func decodeEnvelope<T: Decodable>(_ data: Data) throws -> T {
        do {
            // A fresh decoder per call: parsing now happens concurrently off the main actor, and
            // JSONDecoder is a class that cannot be shared across those tasks.
            let wrapper = try JSONDecoder().decode(AgentResponse<T>.self, from: data)
            guard wrapper.ok else {
                throw AgentError.serverError(wrapper.error ?? "Unknown error")
            }
            guard let result = wrapper.data else {
                throw AgentError.decodingError("Response ok but data is null")
            }
            return result
        } catch let error as AgentError {
            throw error
        } catch {
            throw AgentError.decodingError(error.localizedDescription)
        }
    }

    private func encodeBody(_ body: (any Encodable)?) throws -> Data? {
        try body.map { try JSONEncoder().encode($0) }
    }
}

// MARK: - Response wrapper

struct AgentResponse<T: Decodable>: Decodable {
    let ok: Bool
    let data: T?
    let error: String?
}

// MARK: - Actor transfer

/// Carries a parsed payload from the off-actor parse step back to the main actor.
///
/// The values involved (`[String: Any]`, `Any?`, arbitrary decoded models) cannot conform to
/// `Sendable`, and Swift 5 has no `sending` parameter modifier. The transfer is nonetheless safe:
/// each value is created inside the parse closure, is uniquely owned by the box, and is unwrapped
/// exactly once by the caller — the producing task keeps no reference, so there is nothing shared
/// to race on.
private struct UnsafeTransfer<T>: @unchecked Sendable {
    let value: T
}
