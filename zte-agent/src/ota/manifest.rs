//! Signed release-channel envelopes and strict release-manifest validation.

use base64::Engine as _;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::pkcs8::DecodePublicKey as _;
use semver::Version;
use serde::{Deserialize, Serialize};

pub const MAX_ENVELOPE_BYTES: usize = 96 * 1024;
pub const MAX_BINARY_BYTES: u64 = 32 * 1024 * 1024;
pub const KEY_ID: &str = "ota-v1";
pub const RELEASE_TARGET: &str = "aarch64-unknown-linux-musl";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema: u8,
    pub channel: Channel,
    pub version: String,
    pub tag: String,
    pub commit: String,
    pub target: String,
    pub size: u64,
    pub sha256: String,
    pub binary_url: String,
    pub notes_url: String,
    pub published_at: String,
    pub minimum_updater_version: String,
}

impl ReleaseManifest {
    pub fn semver(&self) -> Result<Version, ManifestError> {
        Version::parse(&self.version).map_err(|_| ManifestError::InvalidVersion)
    }

    pub fn minimum_updater_semver(&self) -> Result<Version, ManifestError> {
        Version::parse(&self.minimum_updater_version)
            .map_err(|_| ManifestError::InvalidMinimumVersion)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema != 1 {
            return Err(ManifestError::UnsupportedManifestSchema);
        }
        let version = self.semver()?;
        if self.tag != format!("v{}", self.version) {
            return Err(ManifestError::TagMismatch);
        }
        let beta = version.pre.as_str().split('.').next() == Some("beta");
        match self.channel {
            Channel::Stable if !version.pre.is_empty() => {
                return Err(ManifestError::ChannelMismatch)
            }
            Channel::Beta if !beta => return Err(ManifestError::ChannelMismatch),
            _ => {}
        }
        if self.target != RELEASE_TARGET {
            return Err(ManifestError::WrongTarget);
        }
        if !(1024..=MAX_BINARY_BYTES).contains(&self.size) {
            return Err(ManifestError::InvalidSize);
        }
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(ManifestError::InvalidDigest);
        }
        if !(7..=64).contains(&self.commit.len())
            || !self.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ManifestError::InvalidCommit);
        }
        if self.published_at.is_empty() || self.published_at.len() > 64 {
            return Err(ManifestError::InvalidPublishedAt);
        }
        self.minimum_updater_semver()?;
        validate_release_url(&self.binary_url, &self.tag, true)?;
        validate_release_url(&self.notes_url, &self.tag, false)?;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedEnvelope {
    schema: u8,
    key_id: String,
    payload: String,
    signature: String,
}

pub struct ManifestVerifier {
    key: Option<VerifyingKey>,
}

impl ManifestVerifier {
    pub fn from_pem(pem: Option<&str>) -> Self {
        Self {
            key: pem.and_then(|value| VerifyingKey::from_public_key_pem(value).ok()),
        }
    }

    #[cfg(test)]
    pub fn from_key(key: VerifyingKey) -> Self {
        Self { key: Some(key) }
    }

    pub fn configured(&self) -> bool {
        self.key.is_some()
    }

    pub fn verify(&self, bytes: &[u8]) -> Result<ReleaseManifest, ManifestError> {
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(ManifestError::EnvelopeTooLarge);
        }
        let envelope: SignedEnvelope =
            serde_json::from_slice(bytes).map_err(|_| ManifestError::InvalidEnvelope)?;
        if envelope.schema != 1 {
            return Err(ManifestError::UnsupportedEnvelopeSchema);
        }
        if envelope.key_id != KEY_ID {
            return Err(ManifestError::UnknownKey);
        }
        let key = self
            .key
            .as_ref()
            .ok_or(ManifestError::SigningKeyUnavailable)?;
        let payload = base64::engine::general_purpose::STANDARD
            .decode(envelope.payload)
            .map_err(|_| ManifestError::InvalidEnvelope)?;
        if payload.len() > MAX_ENVELOPE_BYTES {
            return Err(ManifestError::EnvelopeTooLarge);
        }
        let signature = base64::engine::general_purpose::STANDARD
            .decode(envelope.signature)
            .ok()
            .and_then(|bytes| Signature::from_der(&bytes).ok())
            .ok_or(ManifestError::InvalidSignature)?;
        key.verify(&payload, &signature)
            .map_err(|_| ManifestError::InvalidSignature)?;
        let manifest: ReleaseManifest =
            serde_json::from_slice(&payload).map_err(|_| ManifestError::InvalidManifest)?;
        manifest.validate()?;
        Ok(manifest)
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum ManifestError {
    #[error("release signing key is not configured")]
    SigningKeyUnavailable,
    #[error("signed release envelope is too large")]
    EnvelopeTooLarge,
    #[error("invalid signed release envelope")]
    InvalidEnvelope,
    #[error("unsupported signed release envelope schema")]
    UnsupportedEnvelopeSchema,
    #[error("release was signed by an unknown key")]
    UnknownKey,
    #[error("release signature is invalid")]
    InvalidSignature,
    #[error("invalid release manifest")]
    InvalidManifest,
    #[error("unsupported release manifest schema")]
    UnsupportedManifestSchema,
    #[error("invalid release version")]
    InvalidVersion,
    #[error("invalid minimum updater version")]
    InvalidMinimumVersion,
    #[error("release tag does not match its version")]
    TagMismatch,
    #[error("release channel does not match its version")]
    ChannelMismatch,
    #[error("release is for a different target")]
    WrongTarget,
    #[error("release binary size is outside the accepted range")]
    InvalidSize,
    #[error("release digest is invalid")]
    InvalidDigest,
    #[error("release commit is invalid")]
    InvalidCommit,
    #[error("release publication time is invalid")]
    InvalidPublishedAt,
    #[error("release URL is not an allowed immutable GitHub URL")]
    InvalidUrl,
}

fn validate_release_url(value: &str, tag: &str, binary: bool) -> Result<(), ManifestError> {
    let url = reqwest::Url::parse(value).map_err(|_| ManifestError::InvalidUrl)?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ManifestError::InvalidUrl);
    }
    let expected = if binary {
        format!("/jesther-ai/open-u60-pro/releases/download/{tag}/zte-agent")
    } else {
        format!("/jesther-ai/open-u60-pro/releases/tag/{tag}")
    };
    if url.path() != expected {
        return Err(ManifestError::InvalidUrl);
    }
    Ok(())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use p256::ecdsa::signature::Signer as _;
    use p256::ecdsa::{Signature, SigningKey};

    use super::*;

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes((&[7u8; 32]).into()).unwrap()
    }

    fn manifest(channel: Channel, version: &str) -> ReleaseManifest {
        let tag = format!("v{version}");
        ReleaseManifest {
            schema: 1,
            channel,
            version: version.into(),
            tag: tag.clone(),
            commit: "1234567890ab".into(),
            target: RELEASE_TARGET.into(),
            size: 7_000_000,
            sha256: "a".repeat(64),
            binary_url: format!(
                "https://github.com/jesther-ai/open-u60-pro/releases/download/{tag}/zte-agent"
            ),
            notes_url: format!("https://github.com/jesther-ai/open-u60-pro/releases/tag/{tag}"),
            published_at: "2026-11-02T00:00:00Z".into(),
            minimum_updater_version: "2.4.0-beta.1".into(),
        }
    }

    fn envelope(manifest: &ReleaseManifest, key: &SigningKey) -> Vec<u8> {
        let payload = serde_json::to_vec(manifest).unwrap();
        let signature: Signature = key.sign(&payload);
        serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "key_id": KEY_ID,
            "payload": base64::engine::general_purpose::STANDARD.encode(payload),
            "signature": base64::engine::general_purpose::STANDARD.encode(signature.to_der()),
        }))
        .unwrap()
    }

    #[test]
    fn verifier_accepts_a_signed_stable_release() {
        let key = signing_key();
        let want = manifest(Channel::Stable, "2.4.1");
        let got = ManifestVerifier::from_key(*key.verifying_key())
            .verify(&envelope(&want, &key))
            .unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn verifier_rejects_a_modified_payload() {
        let key = signing_key();
        let mut bytes = envelope(&manifest(Channel::Stable, "2.4.1"), &key);
        let at = bytes.iter().position(|byte| *byte == b'1').unwrap();
        bytes[at] = b'2';
        assert!(ManifestVerifier::from_key(*key.verifying_key())
            .verify(&bytes)
            .is_err());
    }

    #[test]
    fn stable_channel_rejects_a_beta_version() {
        let error = manifest(Channel::Stable, "2.5.0-beta.1")
            .validate()
            .unwrap_err();
        assert_eq!(error, ManifestError::ChannelMismatch);
    }

    #[test]
    fn beta_channel_requires_the_beta_identifier() {
        let error = manifest(Channel::Beta, "2.5.0-rc.1")
            .validate()
            .unwrap_err();
        assert_eq!(error, ManifestError::ChannelMismatch);
    }

    #[test]
    fn binary_url_must_be_the_versioned_repository_asset() {
        let mut value = manifest(Channel::Stable, "2.4.1");
        value.binary_url = "https://example.test/zte-agent".into();
        assert_eq!(value.validate().unwrap_err(), ManifestError::InvalidUrl);
    }
}
