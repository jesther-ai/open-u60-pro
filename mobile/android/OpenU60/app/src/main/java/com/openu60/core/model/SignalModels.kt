package com.openu60.core.model

data class LTECarrier(
    val label: String = "",
    val pci: String = "",
    val band: String = "",
    val earfcn: String = "",
    val bandwidth: String = "",
    val rsrp: Double? = null,
    val rsrq: Double? = null,
    val sinr: Double? = null,
    val rssi: Double? = null,
) {
    val id: String get() = "$label-$band-$pci-$earfcn"
}

data class NRSignal(
    val rsrp: Double? = null,
    val rsrq: Double? = null,
    val sinr: Double? = null,
    val rssi: Double? = null,
    val band: String = "",
    val pci: String = "",
    val cellID: String = "",
    val channel: String = "",
    val bandwidth: String = "",
    val carrierAggregation: String = "",
    val sccCarriers: List<LTECarrier> = emptyList(),
) {
    val isConnected: Boolean get() = rsrp != null
    val hasSignal: Boolean get() = isConnected || sccCarriers.any { it.rsrp != null }

    companion object {
        val empty = NRSignal()
    }
}

data class LTESignal(
    val rsrp: Double? = null,
    val rsrq: Double? = null,
    val sinr: Double? = null,
    val rssi: Double? = null,
    val pci: String = "",
    val band: String = "",
    val earfcn: String = "",
    val bandwidth: String = "",
    val cellID: String = "",
    val carrierAggregation: String = "",
    val caState: String = "",
    val sccCarriers: List<LTECarrier> = emptyList(),
) {
    val isConnected: Boolean get() = rsrp != null
    val hasSignal: Boolean get() = isConnected || sccCarriers.any { it.rsrp != null }

    companion object {
        val empty = LTESignal()
    }
}

data class WCDMASignal(
    val rscp: Double? = null,
    val ecio: Double? = null,
) {
    val isConnected: Boolean get() = rscp != null

    companion object {
        val empty = WCDMASignal()
    }
}

data class OperatorInfo(
    val provider: String = "",
    val networkType: String = "",
    val signalBar: Int = 0,
    val roaming: Boolean = false,
) {
    enum class NetworkMode { SA, NSA, LTE, LEGACY, UNKNOWN }

    val networkMode: NetworkMode
        get() {
            val raw = networkType.uppercase()
            return when {
                raw == "SA" || raw.contains("5G SA") || raw.contains("NR SA") || raw.contains("NR-SA") -> NetworkMode.SA
                raw.contains("NSA") || raw == "ENDC" || raw == "EN-DC" -> NetworkMode.NSA
                raw.contains("LTE") || raw == "4G" || raw == "4G+" -> NetworkMode.LTE
                raw.contains("WCDMA") || raw.contains("UMTS") || raw.contains("GSM")
                    || raw.contains("2G") || raw.contains("3G") -> NetworkMode.LEGACY
                else -> NetworkMode.UNKNOWN
            }
        }

    fun displayNetworkType(nrConnected: Boolean, lteSignal: LTESignal = LTESignal.empty): String {
        if (nrConnected && (networkMode == NetworkMode.LTE || networkMode == NetworkMode.UNKNOWN)) {
            return "5G NSA"
        }
        if (!nrConnected && (networkMode == NetworkMode.SA || networkMode == NetworkMode.NSA)) {
            return if (lteSignal.isConnected) {
                if (lteSignal.sccCarriers.isEmpty()) "4G" else "4G+"
            } else "4G"
        }
        return when (networkMode) {
            NetworkMode.SA -> "5G SA"
            NetworkMode.NSA -> "5G NSA"
            NetworkMode.LTE -> {
                val raw = networkType.uppercase()
                if (raw.contains("CA") || raw == "4G+" || raw.contains("LTE-A") || raw.contains("LTE+")) "4G+" else "4G"
            }
            NetworkMode.LEGACY -> networkType
            NetworkMode.UNKNOWN -> networkType
        }
    }

    fun showNR(nr: NRSignal): Boolean = nr.hasSignal

    fun showLTE(lte: LTESignal): Boolean {
        if (networkMode == NetworkMode.SA) return false
        val raw = networkType.uppercase()
        val hasData = lte.hasSignal
        val actHintsLTE = raw.contains("NSA") || raw.contains("LTE") || raw.contains("E-UTRAN")
            || raw.contains("ENDC") || raw.contains("EN-DC") || raw == "4G" || raw == "4G+"
        val actHintsNR = raw.contains("SA") || raw.contains("NR") || raw.contains("5G")
            || raw.contains("ENDC") || raw.contains("EN-DC")
        return hasData && (actHintsLTE || raw.isEmpty() || actHintsNR)
    }

    fun show3G(nr: NRSignal, lte: LTESignal, wcdma: WCDMASignal): Boolean {
        return !showNR(nr) && !showLTE(lte) && (wcdma.rscp != null || wcdma.ecio != null)
    }

    companion object {
        val empty = OperatorInfo()
    }
}

data class SignalSnapshot(
    val id: Int,
    val timestamp: Long,
    val nrRSRP: Double?,
    val lteRSRP: Double?,
) {
    companion object {
        private var nextID = 0
        fun create(nrRSRP: Double?, lteRSRP: Double?): SignalSnapshot {
            return SignalSnapshot(
                id = nextID++,
                timestamp = System.currentTimeMillis(),
                nrRSRP = nrRSRP,
                lteRSRP = lteRSRP,
            )
        }
    }
}

// MARK: - Parser

object SignalParser {
    fun parseNetInfo(data: Map<String, Any?>): SignalResult {
        var nr = NRSignal.empty
        var lte = LTESignal.empty
        var wcdma = WCDMASignal.empty
        var op = OperatorInfo.empty

        nr = nr.copy(
            rsrp = parseSignalDouble(data["nr5g_rsrp"]),
            rsrq = parseSignalDouble(data["nr5g_rsrq"]),
            sinr = parseSignalDouble(data["nr5g_snr"]),
            rssi = parseSignalDouble(data["nr5g_rssi"]),
            band = stringVal(data["nr5g_action_band"]),
            pci = stringVal(data["nr5g_pci"]),
            cellID = stringVal(data["nr5g_cell_id"]),
            channel = stringVal(data["nr5g_action_channel"]),
            bandwidth = stringVal(data["nr5g_bandwidth"]),
            carrierAggregation = stringVal(data["nrca"]),
        )

        // Parse nrca: "PCI,Band,Index,EARFCN,BW;..."
        val nrcaStr = stringVal(data["nrca"])
        val nrCarriers = parseCAString(nrcaStr)
        val nrcasigStr = stringVal(data["nrcasig"])
        val nrSccSigs = parseCASigString(nrcasigStr)

        // Match NR PCC by PCI+channel
        val nrPccPci = nr.pci
        val nrPccChannel = nr.channel
        val nrSccEntries = mutableListOf<CarrierEntry>()
        var nrPccFound = false
        var updatedNrBw = nr.bandwidth
        for (c in nrCarriers) {
            if (!nrPccFound && c.pci == nrPccPci && c.earfcn == nrPccChannel && nrPccPci.isNotEmpty()) {
                if (c.bandwidth.isNotEmpty()) updatedNrBw = c.bandwidth
                nrPccFound = true
            } else {
                nrSccEntries.add(c)
            }
        }
        nr = nr.copy(bandwidth = updatedNrBw)

        val nrSccCarriers = nrSccEntries.mapIndexed { i, sc ->
            val sig = nrSccSigs.getOrNull(i)
            LTECarrier(
                label = "5G SCC$i", pci = sc.pci, band = sc.band,
                earfcn = sc.earfcn, bandwidth = sc.bandwidth,
                rsrp = sig?.rsrp, rsrq = sig?.rsrq, sinr = sig?.sinr, rssi = sig?.rssi,
            )
        }
        nr = nr.copy(sccCarriers = nrSccCarriers)

        // LTE
        val pccPci = stringVal(data["lte_pci"])
        val pccEarfcn = stringVal(data["wan_active_channel"])
        lte = lte.copy(
            rsrp = parseSignalDouble(data["lte_rsrp"]),
            rsrq = parseSignalDouble(data["lte_rsrq"]),
            sinr = parseSignalDouble(data["lte_snr"]),
            rssi = parseSignalDouble(data["lte_rssi"]),
            pci = pccPci,
            earfcn = pccEarfcn,
            band = stringVal(data["wan_active_band"]),
            cellID = stringVal(data["cell_id"]),
            caState = stringVal(data["lteca_state"]),
        )

        val ltecaStr = stringVal(data["lteca"])
        lte = lte.copy(carrierAggregation = ltecaStr)
        val carriers = parseCAString(ltecaStr)
        val ltecasigStr = stringVal(data["ltecasig"])
        val sccSigs = parseCASigString(ltecasigStr)

        val sccEntries = mutableListOf<CarrierEntry>()
        var pccFound = false
        var updatedLteBw = lte.bandwidth
        for (c in carriers) {
            if (!pccFound && c.pci == pccPci && c.earfcn == pccEarfcn && pccPci.isNotEmpty()) {
                if (c.bandwidth.isNotEmpty()) updatedLteBw = c.bandwidth
                pccFound = true
            } else {
                sccEntries.add(c)
            }
        }
        lte = lte.copy(bandwidth = updatedLteBw)

        val sccCarriers = sccEntries.mapIndexed { i, sc ->
            val sig = sccSigs.getOrNull(i)
            LTECarrier(
                label = "SCC$i", pci = sc.pci, band = sc.band,
                earfcn = sc.earfcn, bandwidth = sc.bandwidth,
                rsrp = sig?.rsrp, rsrq = sig?.rsrq, sinr = sig?.sinr, rssi = sig?.rssi,
            )
        }
        lte = lte.copy(sccCarriers = sccCarriers)

        wcdma = wcdma.copy(
            rscp = parseSignalDouble(data["rscp"]),
            ecio = parseSignalDouble(data["ecio"]),
        )

        op = op.copy(
            provider = stringVal(data["network_provider"]),
            networkType = stringVal(data["network_type"]),
            signalBar = stringVal(data["signalbar"]).toIntOrNull() ?: 0,
            roaming = stringVal(data["simcard_roam"]) == "1",
        )

        return SignalResult(nr, lte, wcdma, op)
    }

    private data class CarrierEntry(val pci: String, val band: String, val earfcn: String, val bandwidth: String)
    private data class SigEntry(val rsrp: Double?, val rsrq: Double?, val sinr: Double?, val rssi: Double?)

    private fun parseCAString(str: String): List<CarrierEntry> {
        if (str.isBlank()) return emptyList()
        return str.trimEnd(';').split(";").mapNotNull { entry ->
            val parts = entry.split(",")
            if (parts.size >= 5) CarrierEntry(parts[0], parts[1], parts[3], parts[4])
            else null
        }
    }

    private fun parseCASigString(str: String): List<SigEntry> {
        if (str.isBlank()) return emptyList()
        return str.trimEnd(';').split(";").mapNotNull { entry ->
            val parts = entry.split(",")
            if (parts.size >= 4) SigEntry(
                parts[0].trim().toDoubleOrNull(),
                parts[1].trim().toDoubleOrNull(),
                parts[2].trim().toDoubleOrNull(),
                parts[3].trim().toDoubleOrNull(),
            ) else null
        }
    }

    private fun parseSignalDouble(value: Any?): Double? {
        val result = when (value) {
            is Double -> value
            is Int -> value.toDouble()
            is String -> {
                val trimmed = value.trim()
                if (trimmed.isEmpty() || trimmed == "--" || trimmed == "N/A") null
                else trimmed.toDoubleOrNull()
            }
            else -> null
        }
        return result?.takeIf { it in -9000.0..9000.0 }?.takeIf { it != 0.0 }
    }

    private fun stringVal(value: Any?): String = when (value) {
        is String -> value
        is Int -> value.toString()
        is Double -> value.toString()
        else -> ""
    }
}

data class SignalResult(
    val nr: NRSignal,
    val lte: LTESignal,
    val wcdma: WCDMASignal,
    val operatorInfo: OperatorInfo,
)
