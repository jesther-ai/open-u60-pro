//! Parameter encryption required by newer MU5250 firmware builds.
//!
//! Firmware `CN_ZTE_MU5250V1.0.0B27` (build dated 2025-12-25) rejects
//! `zte_libwms_send_sms` when `number` and `message_body` are sent as
//! plaintext — ubus answers with status 2 (`Invalid argument`). The stock web
//! UI encrypts those fields with AES-256-GCM using a key the client generates
//! and hands to the device RSA-encrypted (see `f()` in `js/service_rpc.js`):
//!
//! 1. `zwrt_web.web_crt_get` -> RSA public key
//! 2. client generates 32 random bytes and sends them **as a 64-character hex
//!    string** (not raw bytes), RSA PKCS#1 v1.5 encrypted, to
//!    `zwrt_web.web_http_enstr_set` as `web_enstr`
//! 3. sensitive fields are then passed as
//!    `base64( IV(12) || GCM tag(16) || ciphertext )`
//!
//! The key turned out to be **global rather than tied to a web session**:
//! after the handshake, calls issued through the `ubus` CLI outside any
//! session succeed. So the handshake only has to run once and the key can be
//! cached in memory.
//!
//! Any later web UI login overwrites the key on the device, which is why
//! callers should drop the cached key and retry once on `Invalid argument`.

use std::io::Read;
use std::sync::Mutex;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const WEB_HOST: &str = "http://127.0.0.1";
const ANON_SESSION: &str = "00000000000000000000000000000000";
const HTTP_TIMEOUT_SECS: u64 = 10;

pub struct WebCrypto {
    /// AES-256 key as a 64-character hex string — exactly the form that gets
    /// handed to `web_http_enstr_set`.
    key_hex: Mutex<Option<String>>,
}

static GLOBAL: std::sync::OnceLock<WebCrypto> = std::sync::OnceLock::new();

/// Shared instance. The device-side key is global, so there is no point in
/// keeping more than one. Used by both `/api/sms/send` and SMS forwarding.
pub fn global() -> &'static WebCrypto {
    GLOBAL.get_or_init(WebCrypto::new)
}

/// Encrypt a field. On an `Invalid argument` response from ubus, drop the key
/// via [`WebCrypto::invalidate`] and call again.
pub fn encrypt_field(value: &str) -> Result<String, String> {
    global().encrypt_field(value)
}

/// Decrypt a field if it looks encrypted, otherwise return it unchanged.
///
/// Once a key exists on the device, `zte_libwms_get_sms_data` returns `number`
/// and `content` encrypted with it, using the same
/// `base64( IV || tag || ciphertext )` framing as outgoing parameters. Firmware
/// that has never seen a handshake returns plaintext, and so does an agent
/// without `ZTE_ROUTER_PASSWORD` — hence the passthrough on any failure.
pub fn maybe_decrypt(value: &str) -> String {
    global().maybe_decrypt(value)
}

impl WebCrypto {
    pub fn new() -> Self {
        Self {
            key_hex: Mutex::new(None),
        }
    }

    /// Drop the cached key so the next call performs a fresh handshake.
    pub fn invalidate(&self) {
        *self.key_hex.lock().unwrap() = None;
    }

    pub fn encrypt_field(&self, value: &str) -> Result<String, String> {
        let key_hex = self.ensure_key()?;
        encrypt_with(&key_hex, value)
    }

    /// Best-effort decryption — anything that is not a well-formed ciphertext
    /// under the current key comes back untouched.
    pub fn maybe_decrypt(&self, value: &str) -> String {
        if !looks_encrypted(value) {
            return value.to_string();
        }
        let Ok(key_hex) = self.ensure_key() else {
            return value.to_string();
        };
        decrypt_with(&key_hex, value).unwrap_or_else(|_| value.to_string())
    }

    fn ensure_key(&self) -> Result<String, String> {
        if let Some(k) = self.key_hex.lock().unwrap().clone() {
            return Ok(k);
        }
        let k = self.handshake()?;
        *self.key_hex.lock().unwrap() = Some(k.clone());
        Ok(k)
    }

    fn handshake(&self) -> Result<String, String> {
        let password = std::env::var("ZTE_ROUTER_PASSWORD").map_err(|_| {
            "ZTE_ROUTER_PASSWORD is not set — the web UI password is required to encrypt SMS parameters"
                .to_string()
        })?;

        let session = web_login(&password)?;
        let crt = web_call(&session, "zwrt_web", "web_crt_get", json!({}))?
            .get("result")
            .and_then(|v| v.as_str())
            .ok_or("web_crt_get returned no certificate")?
            .to_string();

        let key_hex = random_hex_32()?;
        let enc = rsa_encrypt_pkcs1(&crt, key_hex.as_bytes())?;
        web_call(
            &session,
            "zwrt_web",
            "web_http_enstr_set",
            json!({ "web_enstr": enc }),
        )?;

        Ok(key_hex)
    }
}

fn encrypt_with(key_hex: &str, value: &str) -> Result<String, String> {
    let key = hex_decode(key_hex)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("AES key: {e}"))?;

    let mut iv = [0u8; 12];
    fill_random(&mut iv)?;
    let nonce = Nonce::from_slice(&iv);

    let ct = cipher
        .encrypt(
            nonce,
            Payload {
                msg: value.as_bytes(),
                aad: b"",
            },
        )
        .map_err(|e| format!("AES-GCM: {e}"))?;

    // `encrypt` returns ciphertext || tag; the firmware expects
    // IV || tag || ciphertext.
    let split = ct.len().checked_sub(16).ok_or("ciphertext without tag")?;
    let (body, tag) = ct.split_at(split);

    let mut out = Vec::with_capacity(12 + 16 + body.len());
    out.extend_from_slice(&iv);
    out.extend_from_slice(tag);
    out.extend_from_slice(body);
    Ok(base64_encode(&out))
}

/// Cheap pre-filter so plaintext fields never trigger a handshake.
///
/// UCS-2 hex (what unencrypted `number`/`content` look like) is all hex digits
/// with a length divisible by four, so it is excluded explicitly — otherwise a
/// long hex string could decode as valid base64 and waste a decrypt attempt.
fn looks_encrypted(value: &str) -> bool {
    let v = value.trim();
    if v.len() < 40 || v.len() % 4 != 0 {
        return false;
    }
    if v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    v.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
}

fn decrypt_with(key_hex: &str, b64: &str) -> Result<String, String> {
    let raw = base64_decode(b64.trim())?;
    if raw.len() < 12 + 16 {
        return Err("ciphertext too short".into());
    }
    let key = hex_decode(key_hex)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("AES key: {e}"))?;

    let (iv, rest) = raw.split_at(12);
    let (tag, body) = rest.split_at(16);

    // aes-gcm wants ciphertext || tag, the wire format is IV || tag || ciphertext.
    let mut buf = Vec::with_capacity(body.len() + tag.len());
    buf.extend_from_slice(body);
    buf.extend_from_slice(tag);

    let plain = cipher
        .decrypt(
            Nonce::from_slice(iv),
            Payload {
                msg: &buf,
                aad: b"",
            },
        )
        .map_err(|e| format!("AES-GCM decrypt: {e}"))?;

    String::from_utf8(plain).map_err(|e| format!("utf8: {e}"))
}

// --- web ubus ---

fn web_login(password: &str) -> Result<String, String> {
    let salt = web_call(ANON_SESSION, "zwrt_web", "web_login_info", json!({}))?
        .get("zte_web_sault")
        .and_then(|v| v.as_str())
        .ok_or("web_login_info returned no salt")?
        .to_string();

    let pw_hash = sha256_upper(password.as_bytes());
    let login_hash = sha256_upper(format!("{pw_hash}{salt}").as_bytes());

    web_call(
        ANON_SESSION,
        "zwrt_web",
        "web_login",
        json!({ "password": login_hash }),
    )?
    .get("ubus_rpc_session")
    .and_then(|v| v.as_str())
    .map(|s| s.to_string())
    .ok_or_else(|| "web UI login failed (wrong ZTE_ROUTER_PASSWORD?)".to_string())
}

/// Call the web ubus endpoint. `Referer`/`Origin` are mandatory — without them
/// the firmware answers HTTP 400.
fn web_call(session: &str, object: &str, method: &str, params: Value) -> Result<Value, String> {
    let body = json!([{
        "jsonrpc": "2.0",
        "id": 1,
        "method": "call",
        "params": [session, object, method, params],
    }]);

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS)))
        .build()
        .into();

    let mut resp = agent
        .post(&format!("{WEB_HOST}/ubus/"))
        .header("Content-Type", "application/json")
        .header("Referer", &format!("{WEB_HOST}/"))
        .header("Origin", WEB_HOST)
        .send(body.to_string().as_bytes())
        .map_err(|e| format!("web ubus {object}.{method}: {e}"))?;

    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("reading response: {e}"))?;

    let parsed: Value =
        serde_json::from_str(&text).map_err(|e| format!("web ubus JSON: {e} ({text:.120})"))?;

    // Response shape: [{"result":[status, payload]}] — status 0 means OK.
    let result = parsed
        .get(0)
        .and_then(|v| v.get("result"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("unexpected response: {text:.120}"))?;

    match result.first().and_then(|v| v.as_u64()) {
        Some(0) => Ok(result.get(1).cloned().unwrap_or(Value::Null)),
        Some(code) => Err(format!("web ubus {object}.{method}: status {code}")),
        None => Err(format!("web ubus {object}.{method}: missing status")),
    }
}

// --- crypto helpers ---

fn rsa_encrypt_pkcs1(pem: &str, data: &[u8]) -> Result<String, String> {
    // The firmware returns the PEM on a single line; RsaPublicKey needs the
    // standard 64-character line wrapping.
    let b64: String = pem
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .replace("-----BEGINPUBLICKEY-----", "")
        .replace("-----ENDPUBLICKEY-----", "");

    let mut normalized = String::from("-----BEGIN PUBLIC KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        normalized.push_str(std::str::from_utf8(chunk).map_err(|e| format!("PEM: {e}"))?);
        normalized.push('\n');
    }
    normalized.push_str("-----END PUBLIC KEY-----\n");

    let key = RsaPublicKey::from_public_key_pem(&normalized)
        .map_err(|e| format!("RSA public key: {e}"))?;

    let mut rng = UrandomRng;
    let enc = key
        .encrypt(&mut rng, Pkcs1v15Encrypt, data)
        .map_err(|e| format!("RSA encrypt: {e}"))?;
    Ok(base64_encode(&enc))
}

fn sha256_upper(data: &[u8]) -> String {
    format!("{:X}", Sha256::digest(data))
}

fn random_hex_32() -> Result<String, String> {
    let mut buf = [0u8; 32];
    fill_random(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn fill_random(buf: &mut [u8]) -> Result<(), String> {
    let mut f = std::fs::File::open("/dev/urandom").map_err(|e| format!("urandom: {e}"))?;
    f.read_exact(buf).map_err(|e| format!("urandom read: {e}"))
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("hex string must have even length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("hex: {e}")))
        .collect()
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for b in s.bytes() {
        if b == b'=' {
            break;
        }
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'\n' | b'\r' => continue,
            _ => return Err("invalid base64".into()),
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// RNG backed by `/dev/urandom`, so the agent doesn't need the `rand` crate
/// as a direct dependency.
struct UrandomRng;

impl rsa::rand_core::RngCore for UrandomRng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        let _ = fill_random(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        let _ = fill_random(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let _ = fill_random(dest);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rsa::rand_core::Error> {
        fill_random(dest).map_err(|_| rsa::rand_core::Error::new("urandom failed"))
    }
}

impl rsa::rand_core::CryptoRng for UrandomRng {}
