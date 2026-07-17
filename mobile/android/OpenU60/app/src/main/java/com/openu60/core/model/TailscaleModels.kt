package com.openu60.core.model

import java.time.Instant
import java.time.OffsetDateTime

// MARK: - Tailscale

/** A release resolved from pkgs.tailscale.com. The agent verifies the download against [sha256]. */
data class TailscaleRelease(
    val version: String,
    val sha256: String,
)

data class TailscaleLastUpdate(
    val version: String = "",
    val ok: Boolean = false,
    val error: String? = null,
    val finishedAt: Long = 0L,
)

data class TailscaleStatus(
    val installed: Boolean = false,
    val enabled: Boolean = false,
    val enrolled: Boolean = false,
    val daemonRunning: Boolean = false,
    val state: String = "unknown",
    val backendState: String? = null,
    val health: List<String> = emptyList(),
    val tailscaleIps: List<String> = emptyList(),
    val dnsName: String? = null,
    val relay: String? = null,
    val direct: Boolean = false,
    val keyExpiry: String? = null,
    val expired: Boolean = false,
    val clientVersion: String? = null,
    val loginUrl: String? = null,
    val wakelockHeld: Boolean = false,
    val updating: Boolean = false,
    val lastUpdate: TailscaleLastUpdate? = null,
    val hostname: String = "u60-pro",
    val advertiseRoutes: List<String> = emptyList(),
    val tailscaleSsh: Boolean = false,
    val holdWakelock: Boolean = true,
    val pinnedVersion: String = "",
    val error: String? = null,
) {
    companion object {
        val empty = TailscaleStatus()
    }

    val displayDnsName: String? get() = dnsName?.takeIf { it.isNotEmpty() }?.removeSuffix(".")

    val primaryIp: String? get() = tailscaleIps.firstOrNull()

    /** Relayed is expected behind CGNAT-to-CGNAT, so this reads neutrally rather than as a fault. */
    val pathDescription: String get() = when {
        direct -> "Direct"
        !relay.isNullOrEmpty() -> "Via relay ($relay)"
        else -> "Via relay"
    }

    /** hold_wakelock is intent; wakelock_held is whether the router actually granted it. */
    val wakelockFailing: Boolean get() = enabled && holdWakelock && !wakelockHeld

    /** A version swap that did not take effect. */
    val versionMismatch: Boolean get() =
        pinnedVersion.isNotEmpty() && !clientVersion.isNullOrEmpty() && pinnedVersion != clientVersion

    /**
     * Parsed key_expiry. Null when expiry is disabled — which is the GOOD outcome and must
     * render as "Never", not as unknown — and also when the string won't parse, so callers
     * that distinguish the two must test [keyExpiry] itself.
     */
    val keyExpiryAt: Instant? get() = keyExpiry?.let {
        runCatching { OffsetDateTime.parse(it).toInstant() }.getOrNull()
    }
}

object TailscaleParser {

    /**
     * REQUIRED — see AgentClient.toAny(): it coerces a JSON *string* through
     * booleanOrNull ?: intOrNull ?: longOrNull ?: doubleOrNull before falling back to
     * content, and none of those check isString. The string "12345" therefore arrives as
     * Int and "1.98" as Double, so `as? String` yields null for an all-digit hostname
     * (which the agent accepts) or a two-component version. DeviceParser has no asString.
     *
     * Public because the same hazard applies to every Tailscale payload, not just /status.
     */
    fun asString(value: Any?): String? = when (value) {
        null -> null
        is String -> value
        else -> value.toString()
    }

    private fun asStringList(value: Any?): List<String> =
        (value as? List<*>)?.mapNotNull { asString(it) } ?: emptyList()

    fun parse(data: Map<String, Any?>): TailscaleStatus = TailscaleStatus(
        installed = DeviceParser.asBool(data["installed"]),
        enabled = DeviceParser.asBool(data["enabled"]),
        enrolled = DeviceParser.asBool(data["enrolled"]),
        daemonRunning = DeviceParser.asBool(data["daemon_running"]),
        state = asString(data["state"]) ?: "unknown",
        backendState = asString(data["backend_state"]),
        health = asStringList(data["health"]),
        // tailscale_ips can be NULL, not just [], when unenrolled — this collapses both.
        tailscaleIps = asStringList(data["tailscale_ips"]),
        dnsName = asString(data["dns_name"]),
        relay = asString(data["relay"]),
        direct = DeviceParser.asBool(data["direct"]),
        keyExpiry = asString(data["key_expiry"]),
        expired = DeviceParser.asBool(data["expired"]),
        clientVersion = asString(data["client_version"]),
        loginUrl = asString(data["login_url"]),
        wakelockHeld = DeviceParser.asBool(data["wakelock_held"]),
        updating = DeviceParser.asBool(data["updating"]),
        lastUpdate = parseLastUpdate(data["last_update"] as? Map<*, *>),
        hostname = asString(data["hostname"]) ?: "u60-pro",
        advertiseRoutes = asStringList(data["advertise_routes"]),
        tailscaleSsh = DeviceParser.asBool(data["tailscale_ssh"]),
        // The agent's serde default is TRUE — an absent key must not become false.
        holdWakelock = if (data.containsKey("hold_wakelock")) DeviceParser.asBool(data["hold_wakelock"]) else true,
        pinnedVersion = asString(data["pinned_version"]) ?: "",
        // The only field whose key is absent unless state == "starting".
        error = asString(data["error"]),
    )

    private fun parseLastUpdate(d: Map<*, *>?): TailscaleLastUpdate? {
        if (d == null) return null
        return TailscaleLastUpdate(
            version = asString(d["version"]) ?: "",
            ok = DeviceParser.asBool(d["ok"]),
            error = asString(d["error"]),
            finishedAt = DeviceParser.asLong(d["finished_at"]) ?: 0L,
        )
    }
}

/**
 * Mirrors the agent's own validation so a bad value never becomes a 400 the user has to
 * decode. Every rule below is deliberately ASCII-only: Kotlin's Char.isDigit() and
 * isLetterOrDigit() are Unicode-aware and would accept input the Rust side rejects.
 */
object TailscaleValidator {

    fun hostname(value: String): String? {
        val v = value.trim()
        return if (v.isNotEmpty() && v.length <= 63 && v.all { it.isAsciiAlnum() || it == '-' }) null
        else "Use 1-63 letters, numbers or hyphens."
    }

    /** Comma-separated field to the array the agent wants. Empty means "clear all routes". */
    fun parseRoutes(value: String): List<String> =
        value.split(',').map { it.trim() }.filter { it.isNotEmpty() }

    fun routes(value: String): String? =
        if (parseRoutes(value).all { validCidr(it) }) null
        else "Only IPv4 routes like 192.168.0.0/24 are supported."

    fun version(value: String): String? =
        if (value.isNotEmpty() && value.all { it.isAsciiDigit() || it == '.' }) null
        else "Use digits and dots only, like 1.98.9."

    fun sha256(value: String): String? =
        if (value.length == 64 && value.all { it.isAsciiHex() }) null
        else "Must be exactly 64 hexadecimal characters."

    /** IPv4-only, and host bits are not checked — exactly matching the agent's valid_cidr. */
    private fun validCidr(value: String): Boolean {
        val parts = value.split('/', limit = 2)
        if (parts.size != 2) return false
        val octets = parts[0].split('.')
        val addrOk = octets.size == 4 && octets.all { o -> o.toIntOrNull()?.let { it in 0..255 } == true }
        val prefixOk = parts[1].toIntOrNull()?.let { it in 0..32 } == true
        return addrOk && prefixOk
    }

    private fun Char.isAsciiAlnum(): Boolean = this in '0'..'9' || this in 'a'..'z' || this in 'A'..'Z'
    private fun Char.isAsciiDigit(): Boolean = this in '0'..'9'
    private fun Char.isAsciiHex(): Boolean = this in '0'..'9' || this in 'a'..'f' || this in 'A'..'F'
}
