#!/usr/bin/env python3
"""Apply Rust-exported rules and test traffic in a disposable Docker network."""

import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time

DESTINATION = "2001:db8:ffff::1"
UPSTREAM_ULA = "fd99::53"
PORT = 45678


def run(*args, check=True):
    result = subprocess.run(args, capture_output=True, text=True, timeout=15)
    if check and result.returncode:
        raise RuntimeError(f"{args}: {result.stderr.strip()}")
    return result


def ip(*args):
    return run("ip", *args).stdout.strip()


def ns(name, *args, check=True):
    return run("ip", "netns", "exec", name, *args, check=check)


def command(command):
    if command["program"] == "@u60-relay-ipv6-dad":
        for _ in range(40):
            info = json.loads(ip("-j", "-6", "addr", "show", "dev", "u60sta0"))[0]["addr_info"]
            ready = {a["local"] for a in info if not a.get("tentative") and not a.get("dadfailed")}
            if set(command["args"]) <= ready:
                return
            time.sleep(0.1)
        raise RuntimeError("Upstream IPv6 DAD did not finish")
    run(command["program"], *command["args"])


def apply(steps, applied):
    for step in steps:
        applied.append(step)
        command(step["apply"])


def undo(applied):
    while applied:
        command(applied[-1]["undo"])
        applied.pop()


def echo_server():
    def serve(kind, address):
        sock = socket.socket(socket.AF_INET6, kind)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind((address, PORT))
        if kind == socket.SOCK_STREAM:
            sock.listen()
        while True:
            if kind == socket.SOCK_STREAM:
                client, peer = sock.accept()
                with client:
                    client.recv(1024)
                    client.sendall(peer[0].encode())
            else:
                _, peer = sock.recvfrom(1024)
                sock.sendto(peer[0].encode(), peer)

    for kind, address in [(socket.SOCK_STREAM, "::"), (socket.SOCK_DGRAM, DESTINATION), (socket.SOCK_DGRAM, UPSTREAM_ULA)]:
        threading.Thread(target=serve, args=(kind, address), daemon=True).start()
    while True:
        time.sleep(30)


def dns_server(addresses):
    def serve(kind, address):
        sock = socket.socket(socket.AF_INET6, kind)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind((address, 53))
        if kind == socket.SOCK_STREAM:
            sock.listen()
        while True:
            if kind == socket.SOCK_STREAM:
                client, _ = sock.accept()
                with client:
                    client.recv(1024)
                    client.sendall(b"dns-echo")
            else:
                _, peer = sock.recvfrom(1024)
                sock.sendto(b"dns-echo", peer)
    for address in addresses:
        for kind in [socket.SOCK_STREAM, socket.SOCK_DGRAM]:
            threading.Thread(target=serve, args=(kind, address), daemon=True).start()
    while True:
        time.sleep(30)


def echo_client(kind, destination, source=None, port=PORT):
    with socket.socket(socket.AF_INET6, int(kind)) as sock:
        sock.settimeout(3)
        if source:
            sock.bind((source, 0))
        sock.connect((destination, int(port)))
        sock.send(b"test")
        print(sock.recv(1024).decode())


def acceptance(plan_path):
    # Never allow this script to change a host or an existing container network.
    if os.geteuid() != 0 or not Path("/.dockerenv").exists():
        raise RuntimeError("Use a disposable root Docker container, not the host/router")
    if {link["ifname"] for link in json.loads(ip("-j", "link"))} != {"lo"}:
        raise RuntimeError("Container must start with --network none and only loopback")
    with open(plan_path, encoding="utf-8") as file:
        plan = json.load(file)
    for name in ["lan", "wan", "cell"]:
        ip("netns", "add", name)
    for iface, peer, name in [("br-lan", "lan0", "lan"), ("u60sta0", "wan0", "wan"), ("rmnet_data0", "cell0", "cell")]:
        ip("link", "add", iface, "type", "veth", "peer", "name", peer)
        ip("link", "set", peer, "netns", name)
        ip("link", "set", iface, "up")
        ns(name, "ip", "link", "set", "lo", "up")
        ns(name, "ip", "link", "set", peer, "up")
    ip("link", "set", "lo", "up")
    run("sysctl", "-w", "net.ipv6.conf.all.forwarding=1")
    ip("-6", "addr", "add", "2001:db8:2::1/64", "dev", "br-lan")
    ip("-6", "addr", "add", "2001:db8:9::1/64", "dev", "rmnet_data0")
    ip("-6", "route", "add", "default", "via", "fe80::999", "dev", "rmnet_data0")
    ip("-4", "addr", "add", "192.168.0.1/24", "dev", "br-lan")
    for name, iface, address in [
        ("lan", "lan0", "fd12:3456:789a::123/64"),
        ("lan", "lan0", "2001:db8:2::123/64"),
        ("wan", "wan0", "2001:db8:1::53/64"),
        ("wan", "wan0", "fe80::1/64"),
        ("wan", "lo", f"{DESTINATION}/128"),
        ("wan", "lo", f"{UPSTREAM_ULA}/128"),
    ]:
        ns(name, "ip", "-6", "addr", "add", address, "dev", iface)
    ns("lan", "ip", "-6", "route", "add", "default", "via", "2001:db8:2::1", "dev", "lan0")
    for tool in ["iptables", "ip6tables"]:
        run(tool, "-t", "nat", "-N", "redirect_chain")
        run(tool, "-N", "U60_RELAY_IN")
        run(tool, "-A", "U60_RELAY_IN", "-j", "DROP")
        run(tool, "-I", "INPUT", "-i", "u60sta0", "-j", "U60_RELAY_IN")
    before = ip("-6", "route", "show", "table", "main")
    applied, network, mihomo = [], [], []
    server = None
    dns = []
    try:
        apply(plan["ipv4"], applied)
        result = run("python3", __file__, "client", "1", UPSTREAM_ULA, check=False)
        assert result.returncode != 0, "IPv4-only relay leaked ULA IPv6 to SIM"
        apply(plan["setup"], applied)
        apply(plan["network"], network)
        time.sleep(2)
        for route in before.splitlines():
            assert route in ip("-6", "route", "show", "table", "main"), route
        server = subprocess.Popen(["ip", "netns", "exec", "wan", "python3", __file__, "server"])
        time.sleep(0.4)
        for kind in ["1", "2"]:
            for source in ["fd12:3456:789a::123", "2001:db8:2::123"]:
                seen = ns("lan", "python3", __file__, "client", kind, DESTINATION, source).stdout.strip()
                assert seen == "2001:db8:1::2", (kind, source, seen)
            for destination in [DESTINATION, UPSTREAM_ULA]:
                seen = run("python3", __file__, "client", kind, destination).stdout.strip()
                assert seen == "2001:db8:1::2", (kind, destination, seen)
        print("PASS LAN ULA/retained GUA NAT66 and router TCP/UDP, including upstream ULA")
        # Use the link-local default gateway a real RA client keeps.
        ll = next(a["local"] for i in json.loads(ip("-j", "-6", "addr", "show", "dev", "br-lan")) for a in i["addr_info"] if a.get("scope") == "link")
        ns("lan", "ip", "-6", "route", "replace", "default", "via", ll, "dev", "lan0")
        assert ns("lan", "python3", __file__, "client", "2", DESTINATION, "2001:db8:2::123").stdout.strip() == "2001:db8:1::2"
        print("PASS retained GUA over link-local default gateway before vendor change", flush=True)
        ip("-6", "addr", "del", "2001:db8:2::1/64", "dev", "br-lan")
        try:
            # Exercise the same IPv6 addr/route lifetime changes as same_path.
            for step in plan["network"]:
                args = step["apply"]["args"].copy()
                if step["apply"]["program"] == "ip" and args[:3] in [["-6", "route", "add"], ["-6", "addr", "add"]]:
                    if "expires" in args or "valid_lft" in args:
                        args[2] = "change"
                        run("ip", *args)
            route = ip("-6", "route", "get", "2001:db8:2::123")
            print("Return route after vendor removes LAN GUA:", route, flush=True)
            ula = ns("lan", "python3", __file__, "client", "2", DESTINATION, "fd12:3456:789a::123").stdout.strip()
            print("ULA still succeeds:", ula, flush=True)
            results = []
            for kind in ["1", "2"]:
                result = ns("lan", "python3", __file__, "client", kind, DESTINATION, "2001:db8:2::123", check=False)
                results.append(result.returncode == 0)
                print("Retained GUA", "TCP" if kind == "1" else "UDP", "success:", result.returncode == 0, flush=True)
            assert "dev br-lan" in route and all(results), "client-retained GUA needs a bounded LAN return path after vendor removal"
        finally:
            ip("-6", "addr", "add", "2001:db8:2::1/64", "dev", "br-lan")
            time.sleep(2)

        dns.append(subprocess.Popen(["ip", "netns", "exec", "wan", "python3", __file__, "dns-server", "2001:db8:1::53", UPSTREAM_ULA]))
        dns.append(subprocess.Popen(["python3", __file__, "dns-server", "2001:db8:2::1", "fd12:3456:789a::1"]))
        time.sleep(0.4)
        for group in ["network", "network_ula_dns"]:
            if group != "network":
                undo(network)
                apply(plan[group], network)
            for kind in ["1", "2"]:
                # No server owns fd88::53. Success proves DNAT to the current RDNSS.
                seen = run("python3", __file__, "client", kind, "fd88::53", "", "53").stdout.strip()
                assert seen == "dns-echo", (group, kind, seen)
                seen = ns("lan", "python3", __file__, "client", kind, "fd88::53", "", "53").stdout.strip()
                assert seen == "dns-echo", (group, "LAN redirect", kind, seen)
                seen = ns("lan", "python3", __file__, "client", kind, "fd12:3456:789a::1", "", "53").stdout.strip()
                assert seen == "dns-echo", (group, "local management", kind, seen)
        print("PASS TCP/UDP old cellular ULA DNS to current global/ULA RDNSS; LAN management preserved")
        # Exercise the real route/address change syntax used by lifetime renewal.
        for step in plan["network"]:
            args = step["apply"]["args"].copy()
            if step["apply"]["program"] == "ip" and args[:3] in [["-6", "route", "add"], ["-6", "addr", "add"]]:
                if "expires" in args or "valid_lft" in args:
                    args[2] = "change"
                    run("ip", *args)
        # Model only the core-owned TUN/routes; real_core.py exercises the
        # actual core, including crash cleanup of its IPv6 oif rule.
        ip("link", "add", "u60mh0", "type", "dummy")
        ip("link", "set", "u60mh0", "up")
        ip("-6", "route", "add", "default", "dev", "u60mh0", "table", "60630")
        apply(plan["mihomo"], mihomo)
        assert "dev u60mh0" in ip("-6", "route", "get", DESTINATION, "mark", "0x600")
        print("PASS finite-lifetime renewal and Mihomo marked-route priority")
    finally:
        for process in dns:
            process.kill()
            process.wait()
        if server:
            server.kill()
            server.wait()
        undo(mihomo)
        if run("ip", "link", "show", "u60mh0", check=False).returncode == 0:
            ip("link", "del", "u60mh0")
        undo(network)
        undo(applied)
    assert ip("-6", "route", "show", "table", "main") == before
    assert "18640:" not in ip("-6", "rule", "show")
    for table in ["filter", "nat"]:
        assert "U60_R6" not in run("ip6tables", "-t", table, "-S").stdout
    print("PASS reverse rollback; cellular main routes unchanged and owned IPv6 rules removed")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "server":
        echo_server()
    elif len(sys.argv) > 1 and sys.argv[1] == "dns-server":
        dns_server(sys.argv[2:])
    elif len(sys.argv) > 1 and sys.argv[1] == "client":
        echo_client(*sys.argv[2:])
    else:
        acceptance(sys.argv[1])
