//! Resolve the login verifier once, before any listener or background service.
//! B31 stores the admin password as SHA-256; no plaintext copy is needed.

use std::path::Path;

use sha2::{Digest, Sha256};

use super::{parse_hex, AuthState};

/// In the launcher's run directory (0700, tmpfs): only root reads it.
pub const LOCAL_TOKEN: &str = "/tmp/zte-agent-run/admin-token";

/// A new local token for scripts run as root on the router, replacing the
/// previous process's file (`AuthState::issue_local`). Never logged.
pub fn write_local_token(auth: &AuthState, path: &Path) -> Result<(), String> {
    let token = auth.issue_local()?;
    crate::net_tools::write_private(path, token.expose().as_bytes())
}

#[derive(Debug, PartialEq, Eq)]
pub enum PasswordSource {
    Explicit,
    FactoryAdmin,
}

pub fn configure(auth: &AuthState) -> Result<PasswordSource, &'static str> {
    let explicit = match std::env::var("ZTE_AGENT_PASSWORD") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("ZTE_AGENT_PASSWORD must be valid UTF-8"),
    };
    configure_with(auth, explicit.as_deref(), || {
        crate::net_tools::Cmd::new("uci", &["-q", "show", "zwrt_web"])
            .run()
            .map_err(|_| {
                "Cannot read the factory admin password; set ZTE_AGENT_PASSWORD explicitly"
            })
    })
}

fn configure_with(
    auth: &AuthState,
    explicit: Option<&str>,
    firmware: impl FnOnce() -> Result<String, &'static str>,
) -> Result<PasswordSource, &'static str> {
    let (hash, source) = if let Some(password) = explicit {
        if password.is_empty() {
            return Err("ZTE_AGENT_PASSWORD is empty; unset it for the factory password or set a nonempty override");
        }
        (
            Sha256::digest(password.as_bytes()).into(),
            PasswordSource::Explicit,
        )
    } else {
        (
            factory_verifier(&firmware()?)?,
            PasswordSource::FactoryAdmin,
        )
    };
    auth.password
        .set(hash)
        .map_err(|_| "Authentication is already initialized")?;
    Ok(source)
}

fn factory_verifier(text: &str) -> Result<[u8; 32], &'static str> {
    const INVALID: &str = "Missing, ambiguous or unsupported factory admin password; set ZTE_AGENT_PASSWORD explicitly";
    let fields = crate::ubus::uci_show_map(text);
    let mut admins = fields.iter().filter_map(|(key, value)| {
        let section = key.strip_prefix("zwrt_web.")?.strip_suffix(".username")?;
        (value == "admin" && !section.is_empty() && !section.contains('.')).then_some(section)
    });
    let section = admins.next().ok_or(INVALID)?;
    if admins.next().is_some()
        || fields
            .get(&format!("zwrt_web.{section}.password_type"))
            .map(String::as_str)
            != Some("1")
    {
        return Err(INVALID);
    }
    let encoded = fields
        .get(&format!("zwrt_web.{section}.password"))
        .ok_or(INVALID)?;
    let hash = parse_hex::<32>(&encoded.to_ascii_lowercase()).ok_or(INVALID)?;
    if hash == <[u8; 32]>::from(Sha256::digest(b"")) {
        return Err(INVALID);
    }
    // This is already SHA-256(password), not a plaintext password to hash again.
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Kind, LoginError, PeerKey};

    const FACTORY: &str = "factory-test-passphrase";

    fn firmware() -> String {
        format!("zwrt_web.user0.username='root'\nzwrt_web.user7.username='admin'\nzwrt_web.user7.password_type='1'\nzwrt_web.user7.password='{:X}'\n", Sha256::digest(FACTORY.as_bytes()))
    }

    #[test]
    fn default_factory_password_authenticates_both_session_types_but_never_its_hash() {
        let auth = AuthState::new();
        assert_eq!(
            configure_with(&auth, None, || Ok(firmware())),
            Ok(PasswordSource::FactoryAdmin)
        );
        for kind in [Kind::Browser, Kind::Bearer] {
            assert!(auth.login(FACTORY, kind, PeerKey::UNKNOWN, &[]).is_ok());
        }
        assert!(auth.check_password(FACTORY, PeerKey::UNKNOWN).is_ok());
        for hash in [
            format!("{:X}", Sha256::digest(FACTORY.as_bytes())),
            format!("{:x}", Sha256::digest(FACTORY.as_bytes())),
        ] {
            assert!(matches!(
                auth.login(&hash, Kind::Bearer, PeerKey::UNKNOWN, &[]),
                Err(LoginError::BadPassword)
            ));
        }
    }

    #[test]
    fn factory_verifier_accepts_both_hex_cases_and_does_not_depend_on_section_index() {
        let upper = format!("{:X}", Sha256::digest(FACTORY.as_bytes()));
        let lower = upper.to_ascii_lowercase();
        for text in [
            firmware().replace(&upper, &lower),
            firmware().replace("user7", "another_admin"),
        ] {
            let auth = AuthState::new();
            assert_eq!(
                configure_with(&auth, None, || Ok(text)),
                Ok(PasswordSource::FactoryAdmin)
            );
            assert!(auth.check_password(FACTORY, PeerKey::UNKNOWN).is_ok());
        }
    }

    #[test]
    fn initialized_authentication_cannot_be_replaced_silently() {
        for explicit in [Some("replacement"), None] {
            let auth = AuthState::new();
            configure_with(&auth, Some("original"), || panic!("must not read firmware")).unwrap();
            assert!(configure_with(&auth, explicit, || Ok(firmware())).is_err());
            assert!(auth.check_password("original", PeerKey::UNKNOWN).is_ok());
            assert!(matches!(
                auth.check_password(FACTORY, PeerKey::UNKNOWN),
                Err(LoginError::BadPassword)
            ));
        }
    }

    #[test]
    fn local_token_file_is_private_and_replaced_each_start() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("u60-local-token-{}", std::process::id()));
        let path = dir.join("admin-token");
        let first = AuthState::new();
        write_local_token(&first, &path).unwrap();
        let old = std::fs::read_to_string(&path).unwrap();
        assert!(first.authenticate_local(&old).is_some());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(write_local_token(&first, &path).is_err(), "one per process");
        let next = AuthState::new();
        write_local_token(&next, &path).unwrap();
        let new = std::fs::read_to_string(&path).unwrap();
        assert!(next.authenticate_local(&new).is_some());
        assert!(next.authenticate_local(&old).is_none(), "a restart retires the old token");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn explicit_password_overrides_factory_without_reading_it() {
        let auth = AuthState::new();
        assert_eq!(
            configure_with(&auth, Some("custom password"), || panic!(
                "must not read firmware"
            )),
            Ok(PasswordSource::Explicit)
        );
        assert!(auth
            .check_password("custom password", PeerKey::UNKNOWN)
            .is_ok());
        assert!(matches!(
            auth.check_password(FACTORY, PeerKey::UNKNOWN),
            Err(LoginError::BadPassword)
        ));
    }

    #[test]
    fn missing_or_unsupported_factory_password_never_initializes_open_authentication() {
        for text in [
            String::new(),
            firmware().replace("username='admin'", "username='operator'"),
            firmware().replace("password_type='1'", "password_type='2'"),
            firmware().replace("password_type='1'", "password_type=''"),
            firmware().replace("zwrt_web.user7.password_type='1'\n", ""),
            firmware().replace("user7", "user7.nested"),
            "zwrt_web.user1.username='admin'\nzwrt_web.user1.password_type='1'\nzwrt_web.user1.password='plaintext'\n".into(),
            "zwrt_web.user1.username='admin'\nzwrt_web.user1.password_type='1'\n".into(),
            format!("zwrt_web.user1.username='admin'\nzwrt_web.user1.password_type='1'\nzwrt_web.user1.password='{}'\n", "g".repeat(64)),
            format!("{}zwrt_web.user2.username='admin'\n", firmware()),
            format!("zwrt_web.user1.username='admin'\nzwrt_web.user1.password_type='1'\nzwrt_web.user1.password='{:x}'\n", Sha256::digest(b"")),
        ] {
            let auth = AuthState::new();
            assert!(configure_with(&auth, None, || Ok(text)).is_err());
            assert!(!auth.has_password());
        }
        let auth = AuthState::new();
        assert!(configure_with(&auth, Some(""), || panic!(
            "empty override must not fall back"
        ))
        .is_err());
        assert!(configure_with(&auth, None, || Err("unavailable")).is_err());
        assert!(!auth.has_password());
    }
}
