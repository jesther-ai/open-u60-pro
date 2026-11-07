#!/usr/bin/env python3
"""Real managed Mihomo: cellular -> relay v4 -> dual stack -> fallback -> relay."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import time

from netns import apply, command, ip, ns, run, undo

V4 = "203.0.113.1"
V6 = "2001:db8:ffff::1"
PORT = 45679
STATE = Path("/qa-state")

SERVER = r"""
import socket, threading, time
def serve(family, kind, destination):
    s = socket.socket(family, kind)
    if family == socket.AF_INET6: s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((destination, 45679))
    if kind == socket.SOCK_STREAM:
        s.listen()
    while True:
        if kind == socket.SOCK_STREAM:
            c, p = s.accept()
            def stream(c=c, p=p):
                with c:
                    while c.recv(4096): c.sendall(p[0].encode())
            threading.Thread(target=stream, daemon=True).start()
        else:
            _, p = s.recvfrom(4096)
            s.sendto(p[0].encode(), p)
for family, destination in [(socket.AF_INET, '203.0.113.1'), (socket.AF_INET6, '2001:db8:ffff::1')]:
    for kind in [socket.SOCK_STREAM, socket.SOCK_DGRAM]:
        threading.Thread(target=serve, args=(family, kind, destination), daemon=True).start()
while True: time.sleep(30)
"""

CLIENT = r"""
import socket, sys
family = socket.AF_INET6 if ':' in sys.argv[2] else socket.AF_INET
s = socket.socket(family, int(sys.argv[1])); s.settimeout(4)
if len(sys.argv) > 3:
    s.setsockopt(socket.IPPROTO_IPV6 if family == socket.AF_INET6 else socket.IPPROTO_IP, 23 if family == socket.AF_INET6 else 10, 0)
s.connect((sys.argv[2], 45679)); s.send(b'x' * int(sys.argv[3]) if len(sys.argv) > 3 else b'test'); print(s.recv(4096).decode())
"""

LOCAL_SERVER = SERVER.replace("'203.0.113.1'", "'192.168.0.1'").replace("'2001:db8:ffff::1'", "'2001:db8:3::1'")

PERSISTENT = r"""
import socket, time
from pathlib import Path
state = Path('/qa-state')
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.settimeout(4)
s.connect(('203.0.113.1', 45679))
for stage in ['cell-ready', 'wifi-ready', 'v6-ready', 'cell-again', 'wifi2-ready', 'wifi3-ready', 'stalled-ready']:
    while not (state / stage).exists(): time.sleep(.05)
    s.send(b'persistent'); result = s.recv(4096).decode()
    (state / ('udp-' + stage)).write_text(result)
"""


def wait(name, process, timeout=65):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if (STATE / name).exists():
            return
        if process.poll() is not None:
            raise RuntimeError(f"Managed Rust/core test exited at {name}: {process.returncode}")
        time.sleep(0.05)
    raise RuntimeError(f"Timeout waiting for {name}")


def check(stage, source4, source6, process):
    wait(stage, process)
    wait("udp-" + stage, process)
    assert (STATE / ("udp-" + stage)).read_text() == source4, stage
    for kind in ["1", "2"]:
        seen = ns("lan", "python3", "-c", CLIENT, kind, V4).stdout.strip()
        assert seen == source4, (stage, kind, seen)
        if source6:
            seen = ns("lan", "python3", "-c", CLIENT, kind, V6).stdout.strip()
            assert seen == source6, (stage, kind, seen)
        else:
            result = ns("lan", "python3", "-c", CLIENT, kind, V6, check=False)
            assert result.stdout.strip() not in ["2001:db8:9::2", "2001:db8:1::2"], (stage, "unexpected IPv6 exit", result.stdout)
    for destination, source in [(V4, source4), (V6, source6)]:
        if source:
            for size in [1400, 2000]:
                seen = ns("lan", "python3", "-c", CLIENT, "2", destination, str(size)).stdout.strip()
                assert seen == source, (stage, "large/fragmented UDP", size, seen)
    print("PASS real core", stage, "TCP/UDP IPv4:", source4, "IPv6:", source6, "persistent LAN UDP reconnect", flush=True)


def acceptance():
    if os.geteuid() != 0 or not Path("/.dockerenv").exists():
        raise RuntimeError("Only use disposable Docker with --network none")
    if {i["ifname"] for i in json.loads(ip("-j", "link"))} != {"lo"}:
        raise RuntimeError("Only loopback may exist initially")
    plan = json.loads(Path("/plan.json").read_text())
    STATE.mkdir()
    Path("/u60-ipv6-qa-disposable").touch()
    root = Path("/qa-core")
    root.mkdir()
    shutil.copyfile("/core-bin", root / "mihomo")
    (root / "mihomo").chmod(0o700)
    for name in ["lan", "wan", "cell"]:
        ip("netns", "add", name)
    for tool in ["iptables", "ip6tables"]:
        run(tool, "-t", "nat", "-N", "redirect_chain")
    for iface, peer, name in [("br-lan", "lan0", "lan"), ("u60sta0", "wan0", "wan"), ("rmnet_data0", "cell0", "cell")]:
        if iface == "br-lan":
            ip("link", "add", iface, "type", "bridge")
            ip("link", "add", "lanport", "type", "veth", "peer", "name", peer)
            ip("link", "set", "lanport", "master", iface)
            ip("link", "set", "lanport", "up")
        else:
            ip("link", "add", iface, "type", "veth", "peer", "name", peer)
        ip("link", "set", peer, "netns", name)
        ip("link", "set", iface, "up")
        ns(name, "ip", "link", "set", "lo", "up")
        ns(name, "ip", "link", "set", peer, "up")
    ip("link", "set", "lo", "up")
    run("sysctl", "-w", "net.ipv4.ip_forward=1", "net.ipv6.conf.all.forwarding=1")
    # A veth merely named br-lan misses br_netfilter's early L3 pass. B31
    # enables both bridge hooks: TPROXY loses its socket before actual L3 input.
    bridge_hooks = [Path("/proc/sys/net/bridge/bridge-nf-call-" + family) for family in ["iptables", "ip6tables"]]
    assert all(path.read_text().strip() == "1" for path in bridge_hooks)
    for address, iface in [("192.168.0.1/24", "br-lan"), ("10.9.0.2/24", "rmnet_data0")]:
        ip("-4", "addr", "add", address, "dev", iface)
    for address, iface in [("2001:db8:2::1/64", "br-lan"), ("2001:db8:9::2/64", "rmnet_data0")]:
        ip("-6", "addr", "add", address, "dev", iface)
    ip("-4", "route", "add", "default", "via", "10.9.0.1", "dev", "rmnet_data0")
    ip("-6", "route", "add", "default", "via", "fe80::9", "dev", "rmnet_data0")
    for name, iface, address in [("lan", "lan0", "192.168.0.123/24"), ("wan", "wan0", "10.0.0.1/24"), ("cell", "cell0", "10.9.0.1/24"), ("wan", "lo", V4 + "/32"), ("cell", "lo", V4 + "/32")]:
        ns(name, "ip", "-4", "addr", "add", address, "dev", iface)
    for name, iface, address in [("lan", "lan0", "2001:db8:2::123/64"), ("lan", "lan0", "fd12:3456:789a::123/64"), ("wan", "wan0", "fe80::1/64"), ("wan", "wan0", "2001:db8:1::53/64"), ("cell", "cell0", "fe80::9/64"), ("cell", "cell0", "2001:db8:9::1/64"), ("wan", "lo", V6 + "/128"), ("cell", "lo", V6 + "/128")]:
        ns(name, "ip", "-6", "addr", "add", address, "dev", iface)
    ns("lan", "ip", "-4", "route", "add", "default", "via", "192.168.0.1")
    ns("lan", "ip", "-6", "route", "add", "default", "via", "2001:db8:2::1")
    for tool in ["iptables", "ip6tables"]:
        run(tool, "-P", "INPUT", "DROP")
        run(tool, "-P", "FORWARD", "DROP")
        run(tool, "-A", "INPUT", "-i", "lo", "-j", "ACCEPT")
        run(tool, "-A", "INPUT", "-i", "br-lan", "-j", "ACCEPT")
        run(tool, "-A", "INPUT", "-m", "conntrack", "--ctstate", "ESTABLISHED,RELATED", "-j", "ACCEPT")
        if tool == "ip6tables":
            run(tool, "-A", "INPUT", "-p", "ipv6-icmp", "-j", "ACCEPT")
        run(tool, "-N", "U60_RELAY_IN")
        if tool == "iptables":
            run(tool, "-A", "U60_RELAY_IN", "-m", "conntrack", "--ctstate", "ESTABLISHED,RELATED", "-j", "ACCEPT")
        run(tool, "-A", "U60_RELAY_IN", "-j", "DROP")
        run(tool, "-I", "INPUT", "-i", "u60sta0", "-j", "U60_RELAY_IN")
    time.sleep(2)
    processes, v4, v6, setup = [], [], [], []
    try:
        for name in ["wan", "cell"]:
            processes.append(subprocess.Popen(["ip", "netns", "exec", name, "python3", "-c", SERVER]))
        processes.append(subprocess.Popen(["ip", "netns", "exec", "lan", "python3", "-c", PERSISTENT]))
        test = subprocess.Popen(["/agent-tests", "--ignored", "--exact", "mihomo::tests::isolated_real_core_uplink_transitions", "--nocapture"], env={**os.environ, "U60_CORE_ISOLATED_QA": "1", "U60_CORE_QA_ROOT": str(root)})
        processes.append(test)
        check("cell-ready", "10.9.0.2", "2001:db8:9::2", test)
        ip("-6", "addr", "add", "2001:db8:3::1/128", "dev", "br-lan", "nodad")
        processes.append(subprocess.Popen(["python3", "-c", LOCAL_SERVER]))
        time.sleep(6)
        for kind in ["1", "2"]:
            seen = ns("lan", "python3", "-c", CLIENT, kind, "2001:db8:3::1").stdout.strip()
            assert seen == "2001:db8:2::123", ("new local GUA was intercepted", kind, seen)
        ip("-6", "addr", "del", "2001:db8:3::1/128", "dev", "br-lan")
        # The watchdog rebuilds interception by restarting the core. A fixed
        # sleep can inspect an empty/in-progress nft chain on a loaded VM.
        deadline = time.monotonic() + 25
        while True:
            rules = run("ip6tables", "-t", "mangle", "-S", "U60_MH", check=False)
            if rules.returncode == 0 and "2001:db8:3::1" not in rules.stdout:
                # The chain can also be empty during rollback. Require real
                # LAN TCP replies on both families, not just an absent rule.
                replies = [ns("lan", "python3", "-c", CLIENT, "1", destination, check=False)
                           for destination in [V4, V6]]
                if all(r.returncode == 0 and r.stdout.strip() == source for r, source in
                       zip(replies, ["10.9.0.2", "2001:db8:9::2"])):
                    break
            if test.poll() is not None or time.monotonic() >= deadline:
                raise AssertionError("local IPv6 bypass did not converge after address removal")
            time.sleep(0.2)
        check("cell-ready", "10.9.0.2", "2001:db8:9::2", test)
        print("PASS running core refreshes added/removed local GUA bypass for TCP/UDP", flush=True)
        apply(plan["ipv4"], v4)
        (STATE / "wifi").touch()
        check("wifi-ready", "10.0.0.2", None, test)
        apply(plan["setup"], setup)
        apply(plan["network"], v6)
        (STATE / "v6").touch()
        check("v6-ready", "10.0.0.2", "2001:db8:1::2", test)
        undo(v6)
        undo(setup)
        undo(v4)
        (STATE / "cell").touch()
        check("cell-again", "10.9.0.2", "2001:db8:9::2", test)
        apply(plan["ipv4"], v4)
        apply(plan["setup"], setup)
        apply(plan["network"], v6)
        (STATE / "wifi2").touch()
        check("wifi2-ready", "10.0.0.2", "2001:db8:1::2", test)
        # Model a user-disabled cellular uplink without touching a real modem.
        ip("-4", "route", "del", "default", "via", "10.9.0.1", "dev", "rmnet_data0")
        ip("-6", "route", "del", "default", "via", "fe80::9", "dev", "rmnet_data0")
        undo(v6)
        undo(setup)
        undo(v4)
        # Physical B31 removes all global/ULA addresses when cellular is off.
        # Leaving fixture GUA addresses here masks Mihomo's IPv6 startup probe.
        ip("-6", "addr", "del", "2001:db8:9::2/64", "dev", "rmnet_data0")
        ip("-6", "addr", "del", "2001:db8:2::1/64", "dev", "br-lan")
        (STATE / "offline").touch()
        wait("offline-ready", test)
        assert run("ip", "-4", "route", "get", V4, check=False).returncode != 0
        assert run("ip", "-6", "route", "get", V6, check=False).returncode != 0
        print("PASS core stays enabled offline; no cellular route created", flush=True)
        ip("-6", "addr", "add", "2001:db8:2::1/64", "dev", "br-lan", "nodad")
        apply(plan["ipv4"], v4)
        apply(plan["setup"], setup)
        apply(plan["network"], v6)
        (STATE / "wifi3").touch()
        check("wifi3-ready", "10.0.0.2", "2001:db8:1::2", test)
        ip("-4", "route", "add", "default", "via", "10.9.0.1", "dev", "rmnet_data0")
        ip("-6", "addr", "add", "2001:db8:9::2/64", "dev", "rmnet_data0", "nodad")
        ip("-6", "route", "add", "default", "via", "fe80::9", "dev", "rmnet_data0")
        undo(v6)
        undo(setup)
        undo(v4)
        (STATE / "stalled").touch()
        check("stalled-ready", "10.9.0.2", "2001:db8:9::2", test)
        (STATE / "finish").touch()
        assert test.wait(timeout=15) == 0
    finally:
        if not (STATE / "finish").exists() and (root / "check.yaml").exists():
            diagnostics = run(str(root / "mihomo"), "-t", "-d", str(root), "-f", str(root / "check.yaml"), check=False)
            print("Fixture core diagnostics:", diagnostics.stdout, diagnostics.stderr, flush=True)
        for process in reversed(processes):
            if process.poll() is None:
                process.kill()
            process.wait()
        undo(v6)
        undo(setup)
        undo(v4)
    assert "rmnet_data0" in ip("-4", "route", "get", V4)
    assert "rmnet_data0" in ip("-6", "route", "get", V6)
    assert all(path.read_text().strip() == "1" for path in bridge_hooks)
    assert not any(i["ifname"] == "u60mh0" for i in json.loads(ip("-j", "link")))
    for family in ["-4", "-6"]:
        assert "60630" not in ip(family, "rule", "show")
        assert "60630" not in ip(family, "route", "show", "table", "all")
    print("PASS real bridge, managed core lifecycle, dual-stack TCP/UDP, Wi-Fi NAT, fallback and clean rollback", flush=True)


if __name__ == "__main__":
    acceptance()
