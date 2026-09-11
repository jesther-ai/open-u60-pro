import Foundation

enum AgentError: LocalizedError {
    case unauthorized
    case serverError(String)
    case networkError(Error)
    case decodingError(String)
    case serverUnreachable
    case timeout
    /// The request was cancelled (task cancellation, tab switch, view dismissal).
    /// Callers should treat this as "no result" and leave existing state untouched —
    /// never as a failure worth surfacing, retrying, or re-authenticating against.
    case cancelled

    var errorDescription: String? {
        switch self {
        case .unauthorized:
            return "Not authenticated. Please log in."
        case .serverError(let message):
            return "Server error: \(message)"
        case .networkError(let error):
            return "Network error: \(error.localizedDescription)"
        case .decodingError(let detail):
            return "Failed to decode response: \(detail)"
        case .serverUnreachable:
            return "Cannot reach the agent"
        case .timeout:
            return "Request timed out"
        case .cancelled:
            return "Request cancelled"
        }
    }
}

extension Error {
    /// True when this error represents cancellation rather than a real failure.
    ///
    /// Recognises `CancellationError`, `AgentError.cancelled`, `URLError.cancelled`,
    /// and an `AgentError.networkError` that wraps either of the latter two — so call
    /// sites never have to unwrap the chain themselves.
    var isCancellation: Bool {
        if self is CancellationError { return true }
        if let urlError = self as? URLError, urlError.code == .cancelled { return true }
        guard let agentError = self as? AgentError else { return false }
        switch agentError {
        case .cancelled:
            return true
        case .networkError(let inner):
            return inner.isCancellation
        default:
            return false
        }
    }

    /// True when the agent rejected the request for lack of a valid session.
    var isUnauthorized: Bool {
        guard let agentError = self as? AgentError else { return false }
        if case .unauthorized = agentError { return true }
        return false
    }
}
