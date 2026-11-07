import hashlib
import importlib.util
import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


spec = importlib.util.spec_from_file_location("install", Path(__file__).with_name("install-agent.py"))
install = importlib.util.module_from_spec(spec)
spec.loader.exec_module(install)

# Files the installer used to keep as rollback inputs; it no longer makes backups.
BACKUPS = ("agent.before", "start.before.sh", "supervisor.before.sh", "backups.ready")


class LauncherHarness:
    """Run the generated shell against files, never real /proc or /data.

    A mock nohup creates its fake proc entry using its real child PID. Flock
    locks the actual inherited FD 9 using Python (macOS has no flock command).
    The mock leaves a stable file-only main entry after its simulated exec.
    """

    def __init__(self, root):
        self.root = Path(root)
        self.proc = self.root / "proc"
        self.run = self.root / "run"
        self.binary = self.root / "data" / "zte-agent"
        self.ota = self.root / "data" / "open-u60" / "update"
        self.slot = self.ota / "slot-a" / "zte-agent"
        self.supervisor = self.root / "data" / "local" / "tmp" / "zte-agent-supervisor.sh"
        self.bin = self.root / "bin"
        for path in (self.proc, self.run, self.binary.parent, self.bin):
            path.mkdir()
        self.binary.write_text("old installed executable\n")
        self.startup = self.binary.parent / "local" / "tmp" / "start_zte_agent.sh"
        self.startup.parent.mkdir(parents=True)
        self.startup.write_text("# old private launcher\n")
        self.temporary_binary = self.root / "tmp" / "u60-panel" / "agent"
        self.rc_local = self.root / "etc" / "rc.local"
        self.rc_local.parent.mkdir()
        self.rc_local.write_text("#!/bin/sh\necho keep-user-boot-command\nexit 0\n")
        self.stage = self.root / "stage"
        self.stage.mkdir(mode=0o700)
        self.expected_rc = hashlib.sha256(self.rc_local.read_bytes()).hexdigest()
        self.starts = self.root / "starts"
        self.calls = self.root / "calls"
        self.started = self.root / "started"
        self.helpers = self.root / "helpers.py"
        self.helpers.write_text(r'''import fcntl
import hashlib
import os
from pathlib import Path
import shutil
import sys
import time

root = Path(sys.argv[1])
mode = sys.argv[2]
args = sys.argv[3:]
proc = root / "proc"
binary = root / "data" / "zte-agent"
ota = root / "data" / "open-u60" / "update"
supervisor = root / "data" / "local" / "tmp" / "zte-agent-supervisor.sh"
slot = ota / "slot-a" / "zte-agent"

def record(message):
    with (root / "calls").open("a") as file:
        file.write(message + "\n")

def stat(pid, started):
    # Spaces and ')' inside comm must not shift the starttime field.
    fields = ["S"] + ["0"] * 18 + [str(started)] + ["0"] * 4
    (proc / str(pid) / "stat").write_text(f"{pid} (zte agent) worker) " + " ".join(fields) + "\n")

if mode == "flock":
    assert args in (["-x", "9"], ["-u", "9"])
    if args[0] == "-x":
        record("flock-wait")
        fcntl.flock(9, fcntl.LOCK_EX)
        record("flock-acquired")
    else:
        fcntl.flock(9, fcntl.LOCK_UN)
        record("flock-released")
elif mode == "sha256sum":
    for name in args:
        print(hashlib.sha256(Path(name).read_bytes()).hexdigest() + "  " + name)
elif mode == "kill":
    record("kill " + " ".join(args))
    signal, pid = args
    current = proc / pid
    if signal == "-0":
        sys.exit(0 if current.exists() else 1)
    assert signal == "-TERM"
    if not current.exists():
        sys.exit(1)
    if (current / "fail-term").exists():
        sys.exit(1)
    if (current / "ignore-term").exists():
        sys.exit(0)
    if (current / "reuse-on-term").exists():
        stat(pid, 999999)
        sys.exit(0)
    failed = (current / "exit-on-term").exists()
    shutil.rmtree(current)
    for child in proc.iterdir():
        status = child / "status"
        if status.exists() and status.read_text() == "PPid:\t" + pid + "\n":
            status.write_text("PPid:\t1\n")
            if (child / "reuse-on-reparent").exists():
                stat(child.name, 999999)
    sys.exit(1 if failed else 0)
elif mode == "readlink":
    path = Path(args[0])
    assert path.parent.parent == proc and path.name == "exe"
    marker = path.parent / "reuse-on-exe"
    if marker.exists():
        remaining = int(marker.read_text() or "1") - 1
        if remaining:
            marker.write_text(str(remaining))
        else:
            stat(path.parent.name, 999999)
            marker.unlink()
    try:
        print(os.readlink(path))
    except OSError:
        sys.exit(1)
elif mode == "nohup":
    assert args == [str(supervisor)]
    assert "U60_RELAY_DHCP" not in os.environ
    assert "U60_RELAY_DHCP6" not in os.environ
    assert os.environ["ZTE_OTA_ROOT"] == str(ota)
    assert os.environ["ZTE_OTA_SUPERVISOR"] == str(supervisor)
    assert os.environ["ZTE_OTA_LEGACY_LINK"] == str(binary)
    try:
        os.fstat(9)
    except OSError:
        pass
    else:
        raise AssertionError("Child inherited launcher lock")
    pid = os.getpid()
    current = proc / str(pid)
    current.mkdir()
    (current / "exe").symlink_to("/usr/bin/nohup")
    (current / "cmdline").write_bytes(b"nohup\0" + os.fsencode(supervisor) + b"\0")
    (current / "environ").write_bytes(b"ZTE_AGENT_PASSWORD=fake-child-secret\0")
    stat(pid, 10000 + pid)
    with (root / "starts").open("a") as file:
        file.write(str(pid) + "\n")
    record("nohup")
    deadline = time.monotonic() + 10
    # Wait until the launcher has observed nohup, to exercise its exec loop.
    while not (root / "ready").exists():
        if time.monotonic() > deadline:
            sys.exit(2)
        time.sleep(0.005)
    if (root / "wait-for-second").exists():
        while (root / "calls").read_text().count("flock-wait") < 2:
            if time.monotonic() > deadline:
                sys.exit(3)
            time.sleep(0.005)
    if (root / "reuse-child").exists():
        stat(pid, 999999)
    (current / "exe").unlink()
    suffix = " (deleted)" if (root / "delete-child").exists() else ""
    (current / "exe").symlink_to(str(slot) + suffix)
    (current / "cmdline").write_bytes(os.fsencode(slot) + b"\0")
    (root / "started").touch()
''')
        for command in ("flock", "readlink", "sha256sum"):
            wrapper = self.bin / command
            wrapper.write_text(
                "#!/bin/sh\nexec " + shlex.join([
                    sys.executable, "-I", str(self.helpers), str(self.root), command,
                ]) + ' "$@"\n'
            )
            wrapper.chmod(0o700)
        self.launcher = self.root / "start.sh"
        self.launcher.write_text(self.instrument(install.launcher()))
        self.env = {
            **os.environ, "HARNESS_ROOT": str(self.root), "HARNESS_PYTHON": sys.executable,
            "U60_RELAY_DHCP": "1", "U60_RELAY_DHCP6": "1",
        }

    def instrument(self, script):
        replacements = {
            "PATH=/usr/sbin:/usr/bin:/sbin:/bin":
                "PATH=" + shlex.quote(str(self.bin) + ":/usr/bin:/bin:/usr/sbin:/sbin"),
            "/tmp/zte-agent-run": str(self.run),
            "/tmp/zte-agent.log": str(self.root / "agent.log"),
            "/tmp/u60-panel/agent": str(self.temporary_binary),
            "/data/local/tmp/start_zte_agent.sh": str(self.startup),
            "/data/local/tmp/zte-agent-supervisor.sh": str(self.supervisor),
            "/data/open-u60/update": str(self.ota),
            "/data/zte-agent": str(self.binary),
            "/etc/rc.local": str(self.rc_local),
            "/proc/": str(self.proc) + "/",
        }
        for old, new in replacements.items():
            script = script.replace(old, new)
        prelude = """set -eu
kill() { "$HARNESS_PYTHON" -I "$HARNESS_ROOT/helpers.py" "$HARNESS_ROOT" kill "$@"; }
sleep() { : >"$HARNESS_ROOT/ready"; /bin/sleep 0.02; }
nohup() { exec "$HARNESS_PYTHON" -I "$HARNESS_ROOT/helpers.py" "$HARNESS_ROOT" nohup "$@"; }
"""
        # The former heredoc had no PATH reset; keep old and new renderings
        # equally isolated while exercising the actual generated shell body.
        prelude += "PATH=" + shlex.quote(str(self.bin) + ":/usr/bin:/bin:/usr/sbin:/sbin") + "\nexport PATH\n"
        # Synchronize only initial child observation; all process checks remain
        # the real generated logic, including its PID reuse/exec wait loop.
        script = script.replace("pid=$!\n", """pid=$!
attempt=0
while [ ! -f "__PROC__/$pid/stat" ]; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 500 ] || exit 90
    /bin/sleep 0.01
done
""".replace("__PROC__", str(self.proc)))
        device_paths = script.replace(str(self.root), "")
        for old, new in replacements.items():
            device_paths = device_paths.replace(new.replace(str(self.root), ""), "")
        assert "/proc/" not in device_paths
        assert "/data/" not in device_paths
        return prelude + script

    def prepare_install(self):
        self.stage.joinpath("agent.new").write_text("new installed executable\n")
        self.agent_sha = hashlib.sha256(self.stage.joinpath("agent.new").read_bytes()).hexdigest()
        self.stage.joinpath("supervisor.new").write_text("#!/bin/sh\n# new supervisor\n")
        self.stage.joinpath("start.sh").write_text(self.launcher.read_text())
        self.stage.joinpath("rc.local.new").write_text(install.boot_config(self.rc_local.read_text()))
        self.installer = self.stage / "install.sh"
        subprocess.run(
            [sys.executable, str(Path(__file__).with_name("install-agent.py")), "install", str(self.installer)],
            check=True, capture_output=True,
        )
        self.installer.write_text(self.instrument(self.installer.read_text()))
        return self.installer

    version = "9.9.9-test.1"

    def install_arguments(self, sha=None, version=None):
        return [
            str(self.stage), self.expected_rc,
            self.agent_sha if sha is None else sha,
            self.version if version is None else version,
        ]

    def run_install(self, sha=None, version=None):
        return subprocess.run(
            ["sh", str(self.installer), *self.install_arguments(sha, version)],
            env=self.env, text=True, capture_output=True, timeout=15,
        )

    def process(self, pid, *, args=None, environment=b"", exe=None, start=1234, state="S", ppid=1):
        path = self.proc / str(pid)
        path.mkdir()
        (path / "exe").symlink_to(str(self.binary) if exe is None else exe)
        if args is None:
            args = [str(self.binary)]
        (path / "cmdline").write_bytes(b"".join(os.fsencode(arg) + b"\0" for arg in args))
        (path / "environ").write_bytes(environment)
        (path / "status").write_text(f"PPid:\t{ppid}\n")
        fields = [state] + ["0"] * 18 + [str(start)] + ["0"] * 4
        (path / "stat").write_text(f"{pid} (zte agent) worker) " + " ".join(fields) + "\n")
        return path

    def run_launcher(self):
        return subprocess.run(
            ["sh", str(self.launcher)], env=self.env, text=True,
            capture_output=True, timeout=10,
        )

    def launched(self):
        return self.starts.read_text().splitlines() if self.starts.exists() else []


class InstallationTests(unittest.TestCase):
    def test_rc_local_preserves_other_commands_and_inserts_before_exit(self):
        old = "#!/bin/sh\n# user boot tasks\necho 'keep this'\nexit 0\n# tail\n"
        new = install.boot_config(old)
        self.assertEqual(new.replace(install.BOOT_LINE + "\n", ""), old)
        self.assertLess(new.index(install.BOOT_LINE), new.index("exit 0"))
        self.assertEqual(install.boot_config(new), new)

    def test_rc_local_moves_stale_hooks_and_deduplicates_only_our_line(self):
        old = "echo keep\n" + install.BOOT_LINE + "\nexit 0\n" + install.BOOT_LINE + "\n"
        self.assertEqual(install.boot_config(old), "echo keep\n" + install.BOOT_LINE + "\nexit 0\n")
        self.assertEqual(install.boot_config("echo keep"), "echo keep\n" + install.BOOT_LINE + "\n")
        self.assertEqual(install.boot_config(""), install.BOOT_LINE + "\n")

    def test_nonstandard_agent_hook_requires_manual_review(self):
        with self.assertRaises(ValueError):
            install.boot_config(install.BOOT_LINE + " --custom &\nexit 0\n")

    def test_factory_launcher_unsets_inherited_password_and_is_valid_shell(self):
        script = install.launcher()
        self.assertIn("unset ZTE_AGENT_PASSWORD\n", script)
        self.assertNotIn("export ZTE_AGENT_PASSWORD", script)
        self.assertIn("flock -x 9", script)
        self.assertIn("9>&- &", script)
        self.assertNotIn("killall", script)
        subprocess.run(["sh", "-n"], input=script, text=True, check=True)

    def test_explicit_password_is_data_not_shell_syntax(self):
        password = "literal ' quotes $(exit 99) ; new\nline & $HOME"
        script = install.launcher(password)
        prefix = script.split("export ZTE_AGENT_BIND=")[0]
        result = subprocess.run(
            ["sh"], input=prefix + "printf '%s' \"$ZTE_AGENT_PASSWORD\"\n",
            text=True, capture_output=True, check=True,
        )
        self.assertEqual(result.stdout, password)
        subprocess.run(["sh", "-n"], input=script, text=True, check=True)

    def test_callbacks_and_cli_helpers_do_not_suppress_main_start(self):
        for deleted in (False, True):
            with self.subTest(deleted=deleted), tempfile.TemporaryDirectory() as root:
                harness = LauncherHarness(root)
                executable = str(harness.binary) + (" (deleted)" if deleted else "")
                # Even a callback without arguments is not a main. Neither
                # callback mode nor its private environment may be printed.
                for pid, variable in ((11, "U60_RELAY_DHCP"), (12, "U60_RELAY_DHCP6")):
                    harness.process(pid, exe=executable, environment=(
                        "ZTE_AGENT_PASSWORD=fake-parent-secret\0" + variable + "=1\0"
                    ).encode())
                harness.process(13, exe=executable, args=[str(harness.binary), "bound"])
                for pid, mode in enumerate((
                    "esim-native", "qr-decode", "network-recover", "mihomo-script-child",
                ), start=14):
                    harness.process(pid, exe=executable, args=[str(harness.binary), mode])
                harness.run.joinpath("pid").write_text("11\n")  # Stale PID is not authority.
                result = harness.run_launcher()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout + result.stderr, "")
                self.assertEqual(len(harness.launched()), 1)
                self.assertEqual(harness.run.joinpath("pid").read_text().strip(), harness.launched()[0])
                self.assertIn("kill -0 ", harness.calls.read_text())

    def test_temporary_panel_does_not_suppress_official_launcher(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.process(77, exe=str(harness.temporary_binary))
            result = harness.run_launcher()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(harness.launched()), 1)
            self.assertNotIn("kill -TERM", harness.calls.read_text())

    def test_existing_main_is_preserved_with_or_without_old_pid_file(self):
        for deleted in (False, True):
            for old_pid in (None, "11\n", "77\n"):
                with self.subTest(deleted=deleted, old_pid=old_pid), tempfile.TemporaryDirectory() as root:
                    harness = LauncherHarness(root)
                    harness.process(11, environment=b"U60_RELAY_DHCP=1\0")
                    harness.process(77, exe=str(harness.binary) + (" (deleted)" if deleted else ""))
                    if old_pid is not None:
                        harness.run.joinpath("pid").write_text(old_pid)
                    result = harness.run_launcher()
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout + result.stderr, "")
                    self.assertEqual(harness.launched(), [])
                    self.assertEqual(harness.calls.read_text(), "flock-wait\nflock-acquired\n")
                    if old_pid is not None:
                        self.assertEqual(harness.run.joinpath("pid").read_text(), old_pid)
                    else:
                        self.assertFalse(harness.run.joinpath("pid").exists())

    def test_only_exact_environment_records_select_a_callback(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.process(77, environment=(
                b"U60_RELAY_DHCP=0\0U60_RELAY_DHCP6=10\0U60_RELAY_DHCP6_SESSION=1\0"
                b"ZTE_AGENT_PASSWORD=fake\nU60_RELAY_DHCP=1\0"
            ))
            result = harness.run_launcher()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout + result.stderr, "")
            self.assertEqual(harness.launched(), [])

    def test_unrelated_process_survives_and_does_not_suppress_start(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            process = harness.process(77, exe="/opt/another/zte-agent")
            before = {path.name: path.read_bytes() for path in process.iterdir() if not path.is_symlink()}
            harness.run.joinpath("pid").write_text("77\n")
            result = harness.run_launcher()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout + result.stderr, "")
            self.assertEqual(len(harness.launched()), 1)
            self.assertEqual(os.readlink(process / "exe"), "/opt/another/zte-agent")
            self.assertEqual(before, {
                path.name: path.read_bytes() for path in process.iterdir() if not path.is_symlink()
            })
            for line in harness.calls.read_text().splitlines():
                if line.startswith("kill "):
                    self.assertEqual(line, "kill -0 " + harness.launched()[0])

    def test_scan_rejects_pid_reuse_and_zombies(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            reused = harness.process(77)
            (reused / "reuse-on-exe").touch()
            harness.process(78, state="Z")
            result = harness.run_launcher()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(harness.launched()), 1)

    def test_real_unrelated_process_survives_launcher(self):
        # Only this test-owned sleep receives signals, in cleanup. The
        # generated launcher's kill builtin is intercepted by the harness.
        unrelated = subprocess.Popen(["/bin/sleep", "30"])
        try:
            with tempfile.TemporaryDirectory() as root:
                harness = LauncherHarness(root)
                harness.process(unrelated.pid, exe="/opt/another/zte-agent")
                harness.run.joinpath("pid").write_text(str(unrelated.pid) + "\n")
                result = harness.run_launcher()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(harness.launched()), 1)
                self.assertIsNone(unrelated.poll())
                self.assertNotIn("kill -0 " + str(unrelated.pid) + "\n", harness.calls.read_text())
        finally:
            unrelated.terminate()
            unrelated.wait(timeout=5)

    def test_missing_callback_environment_does_not_hide_another_main(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            process = harness.process(11)
            (process / "environ").unlink()
            harness.process(77)
            result = harness.run_launcher()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout + result.stderr, "")
            self.assertEqual(harness.launched(), [])

    def test_exec_wait_rejects_reused_child_pid(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.root.joinpath("reuse-child").touch()
            result = harness.run_launcher()
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout + result.stderr, "")
            self.assertEqual(len(harness.launched()), 1)
            self.assertFalse(harness.run.joinpath("pid").exists())

    def test_exec_wait_accepts_deleted_binary_after_update(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.root.joinpath("delete-child").touch()
            result = harness.run_launcher()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(harness.launched()), 1)

    def test_concurrent_launchers_start_exactly_one_main(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.root.joinpath("wait-for-second").touch()
            processes = []
            try:
                for _ in range(2):
                    processes.append(subprocess.Popen(
                        ["sh", str(harness.launcher)], env=harness.env,
                        text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    ))
                for process in processes:
                    stdout, stderr = process.communicate(timeout=10)
                    self.assertEqual(process.returncode, 0, stderr)
                    self.assertEqual(stdout + stderr, "")
                self.assertEqual(len(harness.launched()), 1)
                self.assertEqual(harness.calls.read_text().count("flock-acquired"), 2)
                self.assertEqual(harness.run.joinpath("pid").read_text().strip(), harness.launched()[0])
            finally:
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)

    def test_install_stops_only_main_and_its_verified_ubus_listener(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.prepare_install()
            harness.process(77, exe=str(harness.binary) + " (deleted)")
            harness.process(78, exe=str(harness.temporary_binary))
            harness.process(81, exe="/bin/ubus", args=["ubus", "listen", "network.interface"], ppid=77)
            survivors = [
                harness.process(11, environment=b"U60_RELAY_DHCP=1\0"),
                harness.process(12, environment=b"U60_RELAY_DHCP6=1\0"),
                harness.process(13, args=[str(harness.binary), "network-recover"]),
                harness.process(14, exe=str(harness.temporary_binary), environment=b"U60_RELAY_DHCP=1\0"),
                harness.process(82, exe="/bin/ubus", args=["ubus", "call", "system", "info"], ppid=77),
                harness.process(83, exe="/bin/ubus", args=["ubus", "listen"], ppid=999),
                harness.process(84, exe="/usr/bin/vendor-daemon"),
            ]
            before = harness.rc_local.read_bytes()
            result = harness.run_install()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout + result.stderr, "")
            self.assertEqual([
                line for line in harness.calls.read_text().splitlines() if line.startswith("kill -TERM")
            ], ["kill -TERM 77", "kill -TERM 81", "kill -TERM 78"])
            self.assertTrue(all(path.exists() for path in survivors))
            self.assertEqual(harness.binary.read_text(), "new installed executable\n")
            self.assertEqual([name for name in BACKUPS if harness.stage.joinpath(name).exists()], [])
            self.assertEqual(harness.rc_local.read_bytes(), before)
            self.assertEqual(len(harness.launched()), 1)

    def test_install_does_not_signal_reused_main_or_claim_replacement_is_stopped(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.prepare_install()
            main = harness.process(77)
            # First two exe reads identify the main. It is replaced before
            # stop's last identity check, retaining the same executable path.
            main.joinpath("reuse-on-exe").write_text("3")
            result = harness.run_install()
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("kill -TERM 77", harness.calls.read_text())
            self.assertEqual(harness.binary.read_text(), "old installed executable\n")
            self.assertEqual(harness.launched(), [])
            self.assertTrue(main.exists())

    def test_install_never_signals_reused_or_unverified_listener(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.prepare_install()
            harness.process(77)
            reused = harness.process(81, exe="/bin/ubus", args=["ubus", "listen"], ppid=77)
            reused.joinpath("reuse-on-reparent").touch()
            harness.process(82, exe="/bin/ubus", args=["ubus", "listen\nnot-listen"], ppid=77)
            result = harness.run_install()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual([
                line for line in harness.calls.read_text().splitlines() if line.startswith("kill -TERM")
            ], ["kill -TERM 77"])
            self.assertTrue(reused.exists())
            self.assertTrue(harness.proc.joinpath("82").exists())

    def test_install_handles_term_exit_race_but_fails_for_live_unstoppable_process(self):
        for marker, succeeds in (("exit-on-term", True), ("fail-term", False), ("ignore-term", False)):
            with self.subTest(marker=marker), tempfile.TemporaryDirectory() as root:
                harness = LauncherHarness(root)
                harness.prepare_install()
                harness.process(77).joinpath(marker).touch()
                result = harness.run_install()
                if succeeds:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(len(harness.launched()), 1)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(harness.launched(), [])
                    self.assertEqual(harness.binary.read_text(), "old installed executable\n")
                self.assertEqual(harness.calls.read_text().count("kill -TERM 77\n"), 1)

    def test_install_fails_before_stop_for_missing_payload_or_changed_boot_file(self):
        for failure in ("missing-payload", "changed-boot", "bad-launcher"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as root:
                harness = LauncherHarness(root)
                harness.prepare_install()
                harness.process(77)
                if failure == "missing-payload":
                    harness.stage.joinpath("agent.new").unlink()
                elif failure == "changed-boot":
                    harness.rc_local.write_text("echo user-edited\nexit 0\n")
                else:
                    harness.stage.joinpath("start.sh").write_text("if\n")
                before = harness.rc_local.read_bytes()
                result = harness.run_install()
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("kill -TERM", harness.calls.read_text())
                self.assertEqual(harness.binary.read_text(), "old installed executable\n")
                self.assertEqual(harness.rc_local.read_bytes(), before)

    def test_install_reports_launcher_failure_and_retry_keeps_no_backup(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.prepare_install()
            harness.stage.joinpath("start.sh").write_text("#!/bin/sh\nexit 19\n")
            result = harness.run_install()
            self.assertEqual(result.returncode, 19, result.stderr)
            self.assertEqual(harness.launched(), [])
            harness.stage.joinpath("start.sh").write_text(harness.launcher.read_text())
            result = harness.run_install()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(harness.launched()), 1)
            self.assertEqual([name for name in BACKUPS if harness.stage.joinpath(name).exists()], [])

    def test_install_and_concurrent_launcher_share_lock_without_duplicate_start(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.prepare_install()
            harness.root.joinpath("wait-for-second").touch()
            processes = []
            try:
                for command in (
                    ["sh", str(harness.installer), *harness.install_arguments()],
                    ["sh", str(harness.launcher)],
                ):
                    processes.append(subprocess.Popen(
                        command, env=harness.env, text=True,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    ))
                for process in processes:
                    stdout, stderr = process.communicate(timeout=15)
                    self.assertEqual(process.returncode, 0, stderr)
                    self.assertEqual(stdout + stderr, "")
                # If the standalone launcher wins the lock first, the install
                # replaces that one; never two live mains and no deadlock.
                self.assertEqual(len(list(harness.proc.iterdir())), 1)
                self.assertEqual(harness.binary.read_text(), "new installed executable\n")
                pid = harness.run.joinpath("pid").read_text().strip()
                self.assertTrue(harness.proc.joinpath(pid).exists())
            finally:
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)

    def test_install_rerun_is_noop_and_keeps_no_backup(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.prepare_install()
            for _ in range(2):
                result = harness.run_install()
                self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(harness.launched()), 1)
            self.assertEqual([name for name in BACKUPS if harness.stage.joinpath(name).exists()], [])
            self.assertEqual(harness.startup.stat().st_mode & 0o777, 0o700)
            self.assertEqual(harness.binary.stat().st_mode & 0o777, 0o700)
            self.assertNotIn("kill -TERM", harness.calls.read_text())

    def test_launcher_exports_the_ota_layout_and_starts_the_supervisor(self):
        script = install.launcher()
        self.assertIn("export ZTE_OTA_ROOT=/data/open-u60/update\n", script)
        self.assertIn("export ZTE_OTA_SUPERVISOR=/data/local/tmp/zte-agent-supervisor.sh\n", script)
        self.assertIn("export ZTE_OTA_LEGACY_LINK=/data/zte-agent\n", script)
        self.assertIn("nohup /data/local/tmp/zte-agent-supervisor.sh </dev/null", script)
        self.assertNotIn("nohup /data/zte-agent", script)

    def test_executables_in_ab_slots_and_the_rescue_copy_are_mains(self):
        for relative in ("slot-a", "slot-b", "last-stable"):
            for deleted in (False, True):
                with self.subTest(relative=relative, deleted=deleted), tempfile.TemporaryDirectory() as root:
                    harness = LauncherHarness(root)
                    exe = str(harness.ota / relative / "zte-agent") + (" (deleted)" if deleted else "")
                    harness.process(77, exe=exe, args=[exe.removesuffix(" (deleted)")])
                    result = harness.run_launcher()
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout + result.stderr, "")
                    self.assertEqual(harness.launched(), [])

    def test_other_slot_names_and_helpers_running_a_slot_are_not_mains(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.process(77, exe=str(harness.ota / "slot-c" / "zte-agent"))
            harness.process(78, exe=str(harness.ota / "slot-a" / "zte-agent.new"))
            harness.process(79, exe=str(harness.slot), args=[str(harness.slot), "ota-helper"])
            harness.process(80, exe=str(harness.slot), environment=b"U60_RELAY_DHCP6=1\0")
            result = harness.run_launcher()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(len(harness.launched()), 1)

    def test_install_stops_a_main_running_from_a_slot(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.prepare_install()
            harness.process(77, exe=str(harness.slot), args=[str(harness.slot)])
            result = harness.run_install()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("kill -TERM 77", harness.calls.read_text())
            self.assertEqual(len(harness.launched()), 1)

    def test_install_bootstraps_both_slots_the_supervisor_and_the_legacy_link(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.prepare_install()
            # A previous OTA left signed metadata, a trial marker and the
            # other slot active: all of it describes the old binary.
            for name in ("slot-a", "slot-b"):
                (harness.ota / name).mkdir(parents=True)
                (harness.ota / name / "zte-agent").write_text("old " + name + "\n")
                (harness.ota / name / "release.json").write_text("{}\n")
            (harness.ota / "slot-a" / "release.json.download").write_text("partial")
            (harness.ota / "pending").write_text("b\na\n")
            (harness.ota / "state.json").write_text("{}\n")
            (harness.ota / "active").write_text("b\n")
            (harness.ota / "blocked.json").write_text('{"blocked":["1.0.0"]}\n')
            result = harness.run_install()
            self.assertEqual(result.returncode, 0, result.stderr)
            for name in ("slot-a", "slot-b"):
                slot = harness.ota / name
                self.assertEqual(slot.joinpath("zte-agent").read_text(), "new installed executable\n")
                self.assertEqual(slot.joinpath("zte-agent").stat().st_mode & 0o777, 0o700)
                self.assertEqual(
                    slot.joinpath("bootstrap.sha256").read_text(), harness.agent_sha + "\n" + harness.version + "\n",
                )
                self.assertEqual(slot.joinpath("bootstrap.sha256").stat().st_mode & 0o777, 0o600)
                self.assertFalse(slot.joinpath("release.json").exists())
                self.assertFalse(slot.joinpath("zte-agent.new").exists())
            self.assertFalse(harness.ota.joinpath("slot-a", "release.json.download").exists())
            self.assertFalse(harness.ota.joinpath("pending").exists())
            self.assertFalse(harness.ota.joinpath("state.json").exists())
            self.assertEqual(harness.ota.joinpath("active").read_text(), "a\n")
            self.assertEqual(harness.ota.joinpath("active").stat().st_mode & 0o777, 0o600)
            self.assertTrue(harness.ota.joinpath("last-stable").is_dir())
            self.assertEqual(harness.ota.stat().st_mode & 0o777, 0o700)
            self.assertEqual(harness.ota.joinpath("blocked.json").read_text(), '{"blocked":["1.0.0"]}\n')
            self.assertTrue(harness.binary.is_symlink())
            self.assertEqual(os.readlink(harness.binary), str(harness.slot))
            self.assertFalse(harness.binary.parent.joinpath("zte-agent.next").exists())
            self.assertEqual(harness.supervisor.read_text(), "#!/bin/sh\n# new supervisor\n")
            self.assertEqual(harness.supervisor.stat().st_mode & 0o777, 0o700)
            self.assertEqual(harness.startup.read_text(), harness.launcher.read_text())
            self.assertEqual(len(harness.launched()), 1)

    def test_install_replaces_an_existing_supervisor_without_a_backup(self):
        with tempfile.TemporaryDirectory() as root:
            harness = LauncherHarness(root)
            harness.prepare_install()
            harness.supervisor.write_text("# previous supervisor\n")
            result = harness.run_install()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(harness.stage.joinpath("supervisor.before.sh").exists())
            self.assertEqual(harness.supervisor.read_text(), "#!/bin/sh\n# new supervisor\n")

    def test_install_refuses_a_wrong_digest_or_version_before_stopping_anything(self):
        cases = {
            "wrong digest": {"sha": "0" * 64},
            "short digest": {"sha": "abc"},
            "uppercase digest": {"sha": "A" * 64},
            "empty version": {"version": ""},
            "version with shell syntax": {"version": "1.0;reboot"},
        }
        for name, arguments in cases.items():
            with self.subTest(name), tempfile.TemporaryDirectory() as root:
                harness = LauncherHarness(root)
                harness.prepare_install()
                harness.process(77)
                result = harness.run_install(**arguments)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("kill -TERM", harness.calls.read_text())
                self.assertEqual(harness.binary.read_text(), "old installed executable\n")
                self.assertFalse(harness.binary.is_symlink())
                self.assertFalse(harness.ota.exists())
                self.assertEqual(harness.launched(), [])

    def test_install_refuses_a_missing_or_invalid_supervisor_before_stopping_anything(self):
        for failure in ("missing", "invalid"):
            with self.subTest(failure), tempfile.TemporaryDirectory() as root:
                harness = LauncherHarness(root)
                harness.prepare_install()
                harness.process(77)
                if failure == "missing":
                    harness.stage.joinpath("supervisor.new").unlink()
                else:
                    harness.stage.joinpath("supervisor.new").write_text("if\n")
                result = harness.run_install()
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("kill -TERM", harness.calls.read_text())
                self.assertFalse(harness.ota.exists())

    def test_setup_script_is_valid_bash_and_ships_the_supervisor(self):
        root = Path(__file__).resolve().parents[1]
        subprocess.run(["bash", "-n", str(root / "setup.sh")], check=True)
        self.assertTrue(root.joinpath("scripts", "router", "zte-agent-supervisor.sh").is_file())
        subprocess.run(["sh", "-n", str(root / "scripts" / "router" / "zte-agent-supervisor.sh")], check=True)
        self.assertNotIn("<<<<<<<", root.joinpath("setup.sh").read_text())

    def test_old_adbd_exit_zero_does_not_hide_remote_failure_or_trim_output(self):
        setup = Path(__file__).resolve().parents[1].joinpath("setup.sh").read_text()
        remote = setup[setup.index("rcmd() {"):setup.index("\nrpush() {")]
        stub = """USE_SSH=false
adb() { shift; /bin/sh -c "$1"; return 0; }
"""
        for command, status, output in [
            ("false", 1, ""),
            ("printf 'one\\n\\n'", 0, "one\n\n"),
            ("printf 'no newline'", 0, "no newline"),
            ("exit 17", 17, ""),
        ]:
            result = subprocess.run(
                ["bash", "-c", stub + remote + '\nrcmd "$1"', "test", command],
                text=True, capture_output=True,
            )
            self.assertEqual(result.returncode, status)
            self.assertEqual(result.stdout, output)

    def test_setup_removes_the_remote_staging_directory_and_keeps_no_backup(self):
        setup = Path(__file__).resolve().parents[1].joinpath("setup.sh").read_text()
        cleanup = setup[setup.index("cleanup() {"):setup.index("\ntrap cleanup EXIT")]
        stub = """rcmd() { printf '%s\\n' "$*" >>"$CALLS"; }
adb() { :; }
"""
        with tempfile.TemporaryDirectory() as root:
            calls = Path(root) / "calls"
            for stage, expected in [
                ("/data/zte-agent-install-1", "rm -rf '/data/zte-agent-install-1'\n"),
                ("", ""),
            ]:
                workdir = Path(root) / "work"
                workdir.mkdir()
                calls.write_text("")
                result = subprocess.run(
                    ["bash", "-c", stub + cleanup + "\ncleanup", "test"],
                    env={**os.environ, "CALLS": str(calls), "STAGE": stage,
                         "FORWARD_PORT": "", "WORKDIR": str(workdir)},
                    text=True, capture_output=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(calls.read_text(), expected)
                self.assertFalse(workdir.exists())
        self.assertNotIn("Backup", setup)
        for script in (setup, install.installer()):
            self.assertNotIn(".before", script)

    def test_factory_cli_does_not_accept_an_older_download_or_unknown_flags(self):
        setup = Path(__file__).resolve().parents[1] / "setup.sh"
        for args in [["--factory-password", "--download"], ["--serial"], ["--unknown"]]:
            result = subprocess.run(["bash", str(setup), *args], capture_output=True)
            self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
