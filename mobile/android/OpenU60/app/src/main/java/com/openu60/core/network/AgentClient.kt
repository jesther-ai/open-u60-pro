package com.openu60.core.network

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import java.net.SocketTimeoutException
import java.util.concurrent.TimeUnit
import javax.inject.Inject
import javax.inject.Singleton

@Singleton
class AgentClient @Inject constructor() {

    companion object {
        private val JSON_MEDIA_TYPE = "application/json".toMediaType()
        @PublishedApi
        internal val json = Json { ignoreUnknownKeys = true }
    }

    private val httpClient = OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(15, TimeUnit.SECONDS)
        .writeTimeout(10, TimeUnit.SECONDS)
        .build()

    private val pingClient = OkHttpClient.Builder()
        .connectTimeout(3, TimeUnit.SECONDS)
        .readTimeout(3, TimeUnit.SECONDS)
        .build()

    var baseURL: String = "http://192.168.0.1:9090"
    var token: String? = null

    // MARK: - Typed REST methods

    suspend inline fun <reified T> get(path: String): T {
        val data = request("GET", path, null)
        return decodeResponse(data)
    }

    suspend inline fun <reified T> post(path: String, body: String? = null): T {
        val data = request("POST", path, body)
        return decodeResponse(data)
    }

    suspend inline fun <reified T> put(path: String, body: String? = null): T {
        val data = request("PUT", path, body)
        return decodeResponse(data)
    }

    // MARK: - Raw JSON methods

    suspend fun getJSON(path: String): Map<String, Any?> {
        val data = request("GET", path, null)
        return unwrapResponse(data)
    }

    suspend fun getJSONArray(path: String): List<Map<String, Any?>> {
        val data = request("GET", path, null)
        val element = Json.parseToJsonElement(data)
        val obj = element.jsonObject
        val ok = obj["ok"]?.jsonPrimitive?.booleanOrNull ?: false
        if (!ok) throw AgentError.ServerError(obj["error"]?.jsonPrimitive?.contentOrNull ?: "Unknown error")
        val dataElement = obj["data"]
        if (dataElement is JsonArray) {
            return dataElement.map { it.jsonObject.toMap() }
        }
        return emptyList()
    }

    suspend fun postJSON(path: String, body: Map<String, Any?> = emptyMap()): Map<String, Any?> {
        val bodyStr = mapToJsonString(body)
        val data = request("POST", path, bodyStr)
        return unwrapResponse(data)
    }

    suspend fun putJSON(path: String, body: Map<String, Any?> = emptyMap()): Map<String, Any?> {
        val bodyStr = mapToJsonString(body)
        val data = request("PUT", path, bodyStr)
        return unwrapResponse(data)
    }

    suspend fun deleteJSON(path: String, body: Map<String, Any?> = emptyMap()): Map<String, Any?> {
        val bodyStr = mapToJsonString(body)
        val data = request("DELETE", path, bodyStr)
        return unwrapResponse(data)
    }

    // MARK: - Auth

    suspend fun login(password: String): String {
        val bodyStr = """{"password":"${password.replace("\"", "\\\"")}"}"""
        val data = request("POST", "/api/auth/login", bodyStr, authenticated = false)
        val element = Json.parseToJsonElement(data)
        val obj = element.jsonObject
        val ok = obj["ok"]?.jsonPrimitive?.booleanOrNull ?: false
        val dataObj = obj["data"]?.jsonObject
        val receivedToken = dataObj?.get("token")?.jsonPrimitive?.contentOrNull

        if (!ok || receivedToken.isNullOrEmpty()) {
            throw AgentError.Unauthorized()
        }
        token = receivedToken
        return receivedToken
    }

    suspend fun ping(): Boolean = withContext(Dispatchers.IO) {
        try {
            val request = Request.Builder()
                .url(baseURL)
                .head()
                .build()
            val response = pingClient.newCall(request).execute()
            response.code > 0
        } catch (_: Exception) {
            false
        }
    }

    // MARK: - Internal

    suspend fun request(
        method: String,
        path: String,
        body: String?,
        authenticated: Boolean = true,
    ): String = withContext(Dispatchers.IO) {
        val url = baseURL + path

        val requestBody = body?.toRequestBody(JSON_MEDIA_TYPE)
        val builder = Request.Builder().url(url)

        when (method) {
            "GET" -> builder.get()
            "POST" -> builder.post(requestBody ?: "".toRequestBody(JSON_MEDIA_TYPE))
            "PUT" -> builder.put(requestBody ?: "".toRequestBody(JSON_MEDIA_TYPE))
            "DELETE" -> builder.delete(requestBody)
            "HEAD" -> builder.head()
        }

        if (body != null) {
            builder.header("Content-Type", "application/json")
        }

        if (authenticated) {
            token?.let { builder.header("Authorization", "Bearer $it") }
        }

        try {
            val response = httpClient.newCall(builder.build()).execute()
            val responseBody = response.body?.string() ?: ""

            when (response.code) {
                in 200..299 -> responseBody
                401 -> throw AgentError.Unauthorized()
                else -> throw AgentError.ServerError(responseBody.ifEmpty { "HTTP ${response.code}" })
            }
        } catch (e: AgentError) {
            throw e
        } catch (e: SocketTimeoutException) {
            throw AgentError.Timeout()
        } catch (e: java.net.ConnectException) {
            throw AgentError.ServerUnreachable()
        } catch (e: Exception) {
            throw AgentError.NetworkError(e.message ?: "Unknown", e)
        }
    }

    @PublishedApi
    internal inline fun <reified T> decodeResponse(data: String): T {
        try {
            val element = Json.parseToJsonElement(data)
            val obj = element.jsonObject
            val ok = obj["ok"]?.jsonPrimitive?.booleanOrNull ?: false
            if (!ok) {
                throw AgentError.ServerError(obj["error"]?.jsonPrimitive?.contentOrNull ?: "Unknown error")
            }
            val dataElement = obj["data"] ?: throw AgentError.DecodingError("Response ok but data is null")
            return json.decodeFromJsonElement(dataElement)
        } catch (e: AgentError) {
            throw e
        } catch (e: Exception) {
            throw AgentError.DecodingError(e.message ?: "Unknown decoding error")
        }
    }

    private fun unwrapResponse(data: String): Map<String, Any?> {
        val element = Json.parseToJsonElement(data)
        val obj = element.jsonObject
        val ok = obj["ok"]?.jsonPrimitive?.booleanOrNull ?: false
        if (!ok) throw AgentError.ServerError(obj["error"]?.jsonPrimitive?.contentOrNull ?: "Unknown error")
        val dataElement = obj["data"]
        return when (dataElement) {
            is JsonObject -> dataElement.toMap()
            else -> emptyMap()
        }
    }

    private fun mapToJsonString(map: Map<String, Any?>): String {
        return buildJsonObject {
            for ((key, value) in map) {
                putAny(key, value)
            }
        }.toString()
    }
}

// MARK: - JSON helpers

fun JsonObject.toMap(): Map<String, Any?> {
    return entries.associate { (key, value) ->
        key to value.toAny()
    }
}

fun JsonElement.toAny(): Any? = when (this) {
    is JsonNull -> null
    is JsonPrimitive -> {
        booleanOrNull ?: intOrNull ?: longOrNull ?: doubleOrNull ?: contentOrNull
    }
    is JsonObject -> toMap()
    is JsonArray -> map { it.toAny() }
}

@Suppress("UNCHECKED_CAST")
private fun JsonObjectBuilder.putAny(key: String, value: Any?) {
    when (value) {
        null -> put(key, JsonNull)
        is String -> put(key, value)
        is Int -> put(key, value)
        is Long -> put(key, value)
        is Double -> put(key, value)
        is Boolean -> put(key, value)
        is Map<*, *> -> put(key, buildJsonObject {
            for ((k, v) in value) {
                putAny(k as String, v)
            }
        })
        is List<*> -> put(key, buildJsonArray {
            for (item in value) {
                addAny(item)
            }
        })
        else -> put(key, value.toString())
    }
}

@Suppress("UNCHECKED_CAST")
private fun JsonArrayBuilder.addAny(value: Any?) {
    when (value) {
        null -> add(JsonNull)
        is String -> add(value)
        is Int -> add(value)
        is Long -> add(value)
        is Double -> add(value)
        is Boolean -> add(value)
        is Map<*, *> -> add(buildJsonObject {
            for ((k, v) in value) {
                putAny(k as String, v)
            }
        })
        is List<*> -> add(buildJsonArray {
            for (item in value) {
                addAny(item)
            }
        })
        else -> add(value.toString())
    }
}
