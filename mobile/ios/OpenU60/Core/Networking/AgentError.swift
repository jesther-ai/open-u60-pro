import Foundation

enum AgentError: LocalizedError {
    case unauthorized
    /// A non-2xx, non-401 response. Carries the status so callers can branch on
    /// it — the agent uses distinct codes for conditions its strings do not
    /// reliably distinguish.
    case apiError(status: Int, message: String)
    case serverError(String)
    case networkError(Error)
    case decodingError(String)
    case serverUnreachable
    case timeout

    var errorDescription: String? {
        switch self {
        case .unauthorized:
            return "Not authenticated. Please log in."
        case .apiError(_, let message):
            return message
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
        }
    }
}
