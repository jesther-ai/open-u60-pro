import Foundation

/// REST client for the zte-agent HTTP API.
@Observable
@MainActor
final class AgentClient {
    var baseURL: String
    var token: String?

    private let session: URLSession
    private let slowSession: URLSession

    init(baseURL: String = "http://192.168.0.1:9090") {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 10
        config.timeoutIntervalForResource = 15
        self.session = URLSession(configuration: config)

        // For endpoints that legitimately block for minutes (tailscale setup /
        // logout / config / disable). A separate session is mandatory:
        // timeoutIntervalForResource is a session-level total-load cap that
        // URLRequest.timeoutInterval cannot override, and raising it on the
        // shared session would let a slow response hang every other screen.
        let slowConfig = URLSessionConfiguration.default
        slowConfig.timeoutIntervalForRequest = 240
        slowConfig.timeoutIntervalForResource = 300
        self.slowSession = URLSession(configuration: slowConfig)

        self.baseURL = baseURL
    }

    // MARK: - Typed REST methods

    func get<T: Decodable>(_ path: String) async throws -> T {
        let data = try await request(method: "GET", path: path, body: nil)
        return try decodeResponse(data)
    }

    func post<T: Decodable>(_ path: String, body: (any Encodable)? = nil) async throws -> T {
        let bodyData = try body.map { try JSONEncoder().encode(AnyEncodable($0)) }
        let data = try await request(method: "POST", path: path, body: bodyData)
        return try decodeResponse(data)
    }

    func put<T: Decodable>(_ path: String, body: (any Encodable)? = nil) async throws -> T {
        let bodyData = try body.map { try JSONEncoder().encode(AnyEncodable($0)) }
        let data = try await request(method: "PUT", path: path, body: bodyData)
        return try decodeResponse(data)
    }

    // MARK: - Raw JSON

    /// GET a path and return the `data` field as a raw dictionary.
    /// Useful for endpoints where the response format matches `[String: Any]` parsers.
    func getJSON(_ path: String) async throws -> [String: Any] {
        let data = try await request(method: "GET", path: path, body: nil)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw AgentError.decodingError("Expected JSON object")
        }
        guard let ok = json["ok"] as? Bool, ok else {
            throw AgentError.serverError(json["error"] as? String ?? "Unknown error")
        }
        return json["data"] as? [String: Any] ?? [:]
    }

    /// GET a path and return the `data` field as an array of dictionaries.
    func getJSONArray(_ path: String) async throws -> [[String: Any]] {
        let data = try await request(method: "GET", path: path, body: nil)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw AgentError.decodingError("Expected JSON object")
        }
        guard let ok = json["ok"] as? Bool, ok else {
            throw AgentError.serverError(json["error"] as? String ?? "Unknown error")
        }
        return json["data"] as? [[String: Any]] ?? []
    }

    /// POST with a raw dict body and return the `data` field as a raw dictionary.
    func postJSON(_ path: String, body: [String: Any] = [:]) async throws -> [String: Any] {
        let bodyData = try JSONSerialization.data(withJSONObject: body)
        let data = try await request(method: "POST", path: path, body: bodyData)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw AgentError.decodingError("Expected JSON object")
        }
        guard let ok = json["ok"] as? Bool, ok else {
            throw AgentError.serverError(json["error"] as? String ?? "Unknown error")
        }
        return json["data"] as? [String: Any] ?? [:]
    }

    /// PUT with a raw dict body and return the `data` field as a raw dictionary.
    func putJSON(_ path: String, body: [String: Any] = [:]) async throws -> [String: Any] {
        let bodyData = try JSONSerialization.data(withJSONObject: body)
        let data = try await request(method: "PUT", path: path, body: bodyData)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw AgentError.decodingError("Expected JSON object")
        }
        guard let ok = json["ok"] as? Bool, ok else {
            throw AgentError.serverError(json["error"] as? String ?? "Unknown error")
        }
        return json["data"] as? [String: Any] ?? [:]
    }

    /// POST with a raw dict body on the long-timeout session. For endpoints that
    /// legitimately block for minutes.
    func postJSONSlow(_ path: String, body: [String: Any] = [:]) async throws -> [String: Any] {
        let bodyData = try JSONSerialization.data(withJSONObject: body)
        let data = try await request(method: "POST", path: path, body: bodyData, slow: true)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw AgentError.decodingError("Expected JSON object")
        }
        guard let ok = json["ok"] as? Bool, ok else {
            throw AgentError.serverError(json["error"] as? String ?? "Unknown error")
        }
        return json["data"] as? [String: Any] ?? [:]
    }

    /// PUT with a raw dict body on the long-timeout session.
    func putJSONSlow(_ path: String, body: [String: Any] = [:]) async throws -> [String: Any] {
        let bodyData = try JSONSerialization.data(withJSONObject: body)
        let data = try await request(method: "PUT", path: path, body: bodyData, slow: true)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw AgentError.decodingError("Expected JSON object")
        }
        guard let ok = json["ok"] as? Bool, ok else {
            throw AgentError.serverError(json["error"] as? String ?? "Unknown error")
        }
        return json["data"] as? [String: Any] ?? [:]
    }

    /// DELETE with a raw dict body and return the `data` field as a raw dictionary.
    func deleteJSON(_ path: String, body: [String: Any] = [:]) async throws -> [String: Any] {
        let bodyData = try JSONSerialization.data(withJSONObject: body)
        let data = try await request(method: "DELETE", path: path, body: bodyData)
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw AgentError.decodingError("Expected JSON object")
        }
        guard let ok = json["ok"] as? Bool, ok else {
            throw AgentError.serverError(json["error"] as? String ?? "Unknown error")
        }
        return json["data"] as? [String: Any] ?? [:]
    }

    // MARK: - Auth

    /// Login with plaintext password. Returns the token string.
    @discardableResult
    func login(password: String) async throws -> String {
        let payload = ["password": password]
        let bodyData = try JSONEncoder().encode(payload)
        let data = try await request(method: "POST", path: "/api/auth/login", body: bodyData, authenticated: false)

        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw AgentError.decodingError("Expected JSON object from login")
        }

        guard let ok = json["ok"] as? Bool, ok,
              let dataDict = json["data"] as? [String: Any],
              let receivedToken = dataDict["token"] as? String, !receivedToken.isEmpty else {
            throw AgentError.unauthorized
        }

        token = receivedToken
        return receivedToken
    }

    /// Check if the agent is reachable.
    func ping() async -> Bool {
        guard let url = URL(string: baseURL) else { return false }
        var req = URLRequest(url: url)
        req.httpMethod = "HEAD"
        req.timeoutInterval = 3
        do {
            let (_, response) = try await session.data(for: req)
            return (response as? HTTPURLResponse)?.statusCode != nil
        } catch {
            return false
        }
    }

    // MARK: - Internal

    private func request(method: String, path: String, body: Data?, authenticated: Bool = true, slow: Bool = false) async throws -> Data {
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

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await (slow ? slowSession : session).data(for: req)
        } catch let error as URLError where error.code == .timedOut {
            throw AgentError.timeout
        } catch let error as URLError where error.code == .cannotConnectToHost || error.code == .notConnectedToInternet {
            throw AgentError.serverUnreachable
        } catch {
            throw AgentError.networkError(error)
        }

        guard let httpResponse = response as? HTTPURLResponse else {
            throw AgentError.serverUnreachable
        }

        switch httpResponse.statusCode {
        case 200...299:
            return data
        case 401:
            throw AgentError.unauthorized
        default:
            // Lift the envelope's error so callers get a clean message AND the
            // status code. Falls back to the raw body.
            if let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let message = json["error"] as? String, !message.isEmpty {
                throw AgentError.apiError(status: httpResponse.statusCode, message: message)
            }
            let raw = String(data: data, encoding: .utf8) ?? "HTTP \(httpResponse.statusCode)"
            throw AgentError.apiError(status: httpResponse.statusCode, message: raw)
        }
    }

    private func decodeResponse<T: Decodable>(_ data: Data) throws -> T {
        do {
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
}

// MARK: - Response wrapper

struct AgentResponse<T: Decodable>: Decodable {
    let ok: Bool
    let data: T?
    let error: String?
}

// MARK: - Type-erased Encodable wrapper

private struct AnyEncodable: Encodable {
    private let _encode: (Encoder) throws -> Void

    init(_ value: any Encodable) {
        _encode = { encoder in try value.encode(to: encoder) }
    }

    func encode(to encoder: Encoder) throws {
        try _encode(encoder)
    }
}
