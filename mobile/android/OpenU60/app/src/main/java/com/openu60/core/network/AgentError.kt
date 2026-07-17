package com.openu60.core.network

sealed class AgentError(override val message: String, override val cause: Throwable? = null) : Exception(message, cause) {
    class Unauthorized : AgentError("Not authenticated. Please log in.")

    /**
     * A non-2xx response whose HTTP status the caller has to branch on. Carries the agent's
     * own `error` string unprefixed — those are already human-readable and get shown verbatim.
     */
    class ApiError(val status: Int, val serverMessage: String) : AgentError(serverMessage)
    class ServerError(msg: String) : AgentError("Server error: $msg")
    class NetworkError(msg: String, cause: Throwable? = null) : AgentError("Network error: $msg", cause)
    class DecodingError(msg: String) : AgentError("Failed to decode response: $msg")
    class ServerUnreachable : AgentError("Cannot reach the agent")
    class Timeout : AgentError("Request timed out")
}
