package com.openu60.core.model

data class ConfigHeader(
    val magic: String = "",
    val payloadType: Int = 0,
    val signature: String = "",
    val payloadOffset: Int = 128,
) {
    val payloadTypeName: String
        get() = when (payloadType) {
            PAYLOAD_TYPE_ECB -> "AES-128-ECB"
            PAYLOAD_TYPE_CBC -> "AES-256-CBC"
            PAYLOAD_TYPE_PLAIN -> "Plain (unencrypted)"
            PAYLOAD_TYPE_CBC_NEW -> "AES-256-CBC (new)"
            else -> "Unknown ($payloadType)"
        }

    companion object {
        const val HEADER_MAGIC = "ZXHN"
        const val HEADER_SIZE = 128
        const val PAYLOAD_TYPE_OFFSET = 4
        const val SIGNATURE_OFFSET = 8
        const val SIGNATURE_MAX_LEN = 64
        const val PAYLOAD_OFFSET_FIELD = 72

        const val PAYLOAD_TYPE_ECB = 0
        const val PAYLOAD_TYPE_CBC = 1
        const val PAYLOAD_TYPE_PLAIN = 2
        const val PAYLOAD_TYPE_CBC_NEW = 3
    }
}

data class KnownKey(
    val description: String,
    val key: ByteArray,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is KnownKey) return false
        return description == other.description && key.contentEquals(other.key)
    }

    override fun hashCode(): Int = 31 * description.hashCode() + key.contentHashCode()

    companion object {
        val KNOWN_KEYS = listOf(
            KnownKey("ZTE default (MIIBIjANB...)", "MIIBIjANBgkqhk".toByteArray()),
            KnownKey("ZTE default 2", "Wj".toByteArray()),
            KnownKey("ZTE ZXHN H298N", "ZTE%FN\$GponNJ025".toByteArray()),
            KnownKey("ZTE ZXHN H108N V2.5", "GrWM2ans*f@7SSc&".toByteArray()),
            KnownKey("ZTE ZXHN H168N V3.5", byteArrayOf(
                'G'.code.toByte(), 'r'.code.toByte(), 'W'.code.toByte(), 'M'.code.toByte(),
                '3'.code.toByte(), 'm'.code.toByte(), 'n'.code.toByte(), '/'.code.toByte(),
                0x00, 'Y'.code.toByte(), '>'.code.toByte(), '*'.code.toByte(),
                'f'.code.toByte(), '2'.code.toByte(), 'g'.code.toByte(), 'U'.code.toByte(),
            )),
            KnownKey("ZTE ZXHN H298A", "m8@96&ah*ZTE%FN!".toByteArray()),
            KnownKey("ZTE ZXHN F670L", "ZTE%FN\$GponNJ025".toByteArray()),
            KnownKey("ZTE MF283+", "SDT&*Ssym0722!@#".toByteArray()),
            KnownKey("ZTE ZXHN F609", "'MMI@FP*Jhg&^%\$\$".toByteArray()),
            KnownKey("ZTE ZXHN F660", "ZTE%FN\$GponNJ025".toByteArray()),
            KnownKey("ZTE ZXHN H267A", "GrWM2ans*f@7SSc&".toByteArray()),
            KnownKey("ZTE generic key 1", "402c38de39bed665".toByteArray()),
            KnownKey("ZTE generic key 2", "8cc72b05705d5c46".toByteArray()),
            KnownKey("ZTE generic key 3", "SMGPOINTzteGpon!".toByteArray()),
        )

        fun keyFromSerial(serial: String): ByteArray {
            val md5 = java.security.MessageDigest.getInstance("MD5")
            return md5.digest(serial.toByteArray(Charsets.US_ASCII)).copyOf(16)
        }

        fun keyFromSignature(signature: ByteArray): ByteArray {
            val md5 = java.security.MessageDigest.getInstance("MD5")
            return md5.digest(signature).copyOf(16)
        }

        fun getAllKeys(serial: String? = null, signature: ByteArray? = null): List<KnownKey> {
            val candidates = mutableListOf<KnownKey>()
            if (signature?.isNotEmpty() == true) {
                candidates.add(KnownKey("derived from signature", keyFromSignature(signature)))
            }
            if (!serial.isNullOrBlank()) {
                candidates.add(KnownKey("derived from serial $serial", keyFromSerial(serial)))
            }
            candidates.addAll(KNOWN_KEYS)
            return candidates
        }
    }
}
