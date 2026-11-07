//! Exercise the production entry point: bad credentials must exit before the
//! listener or background services, not merely leave an empty AuthState.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture(std::path::PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn invalid_credentials_exit_before_serving_or_starting_services() {
    let dir = std::env::temp_dir().join(format!("u60-auth-startup-{}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let fixture = Fixture(dir);
    for (name, script) in [
        (
            "uci",
            "#!/bin/sh\nprintf '%s' \"$TEST_FACTORY_CONFIG\"\nexit \"$TEST_UCI_EXIT\"\n",
        ),
        ("ubus", "#!/bin/sh\n: > \"$TEST_SERVICE_MARKER\"\nexit 1\n"),
    ] {
        let path = fixture.0.join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let marker = fixture.0.join("service-started");
    let invalid_utf8 = std::ffi::OsString::from_vec(vec![0xff]);
    for (name, config, uci_exit, explicit) in [
        ("unreadable", "not a verifier", "1", None),
        ("no admin", "zwrt_web.user0.username='root'\n", "0", None),
        ("bad hash", "zwrt_web.user1.username='admin'\nzwrt_web.user1.password_type='1'\nzwrt_web.user1.password='not-a-sha256-verifier'\n", "0", None),
        ("duplicate admin", "zwrt_web.user1.username='admin'\nzwrt_web.user2.username='admin'\n", "0", None),
        ("empty override", "", "0", Some(std::ffi::OsStr::new(""))),
        ("invalid UTF-8 override", "", "0", Some(invalid_utf8.as_os_str())),
    ] {
        let reserve = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserve.local_addr().unwrap();
        drop(reserve);
        let mut command = Command::new(env!("CARGO_BIN_EXE_zte-agent"));
        command.env_clear()
            .env("PATH", &fixture.0)
            .env("ZTE_AGENT_BIND", address.to_string())
            .env("TEST_FACTORY_CONFIG", config)
            .env("TEST_UCI_EXIT", uci_exit)
            .env("TEST_SERVICE_MARKER", &marker)
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(value) = explicit {
            command.env("ZTE_AGENT_PASSWORD", value);
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut served = false;
        let mut timed_out = false;
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            served |= TcpStream::connect_timeout(&address, Duration::from_millis(20)).is_ok();
            if served || Instant::now() >= deadline {
                timed_out = !served;
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(!timed_out, "{name}: did not exit promptly");
        assert!(!served, "{name}: opened an unauthenticated listener");
        assert_eq!(output.status.code(), Some(1), "{name}");
        assert!(output.stdout.is_empty(), "{name}: unexpected stdout");
        let log = String::from_utf8(output.stderr).unwrap();
        assert!(log.contains("[auth]"), "{name}: no auth failure diagnostic");
        assert!(!log.contains("not-a-sha256-verifier"), "{name}: leaked verifier");
        assert!(!marker.exists(), "{name}: started background services");
    }
}
