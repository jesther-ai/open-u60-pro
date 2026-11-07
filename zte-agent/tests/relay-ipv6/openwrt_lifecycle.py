#!/usr/bin/env python3
"""Run the ignored Rust lifecycle test in a disposable official OpenWrt chroot."""

import json
import os
from pathlib import Path
import subprocess
import time

from netns import ip, ns, run

ROOT = Path("/ow")


def ow(*args):
    return run("chroot", str(ROOT), *args).stdout.strip()


def lifecycle():
    if os.geteuid() != 0 or not Path("/.dockerenv").exists():
        raise RuntimeError("Only run in a disposable Docker container")
    if {link["ifname"] for link in json.loads(ip("-j", "link"))} != {"lo"}:
        raise RuntimeError("Use --network none without host networking")
    for directory in ["proc", "dev"]:
        run("mount", "--bind", "/" + directory, str(ROOT / directory))
    run("mount", "-t", "sysfs", "sysfs", str(ROOT / "sys"))
    for directory in ["tmp/lock", "tmp/run", "var/lock", "tmp/resolv.conf.d"]:
        (ROOT / directory).mkdir(parents=True, exist_ok=True)
    (ROOT / "u60-ipv6-qa-disposable").touch()
    (ROOT / "tmp/resolv.conf").write_text("")
    # No procd in the chroot. Signal the real odhcpd for reload, and stub only
    # dnsmasq restart; the Rust DNS cleanup still executes actual UCI commands.
    for service, script in {
        "odhcpd": "#!/bin/ash\nkill -HUP $(cat /tmp/qa-odhcpd.pid)\n",
        "dnsmasq": "#!/bin/ash\nexit 0\n",
    }.items():
        path = ROOT / "etc/init.d" / service
        path.write_text(script)
        path.chmod(0o755)
    ip("link", "set", "lo", "up")
    for iface, address in [("br-lan", "2001:db8:2::1/64"), ("rmnet_data0", "2001:db8:9::1/64")]:
        ip("link", "add", iface, "type", "dummy")
        ip("link", "set", iface, "up")
        ip("-6", "addr", "add", address, "dev", iface)
    ip("-6", "route", "add", "default", "via", "fe80::999", "dev", "rmnet_data0")
    ip("netns", "add", "upstream")
    ip("link", "add", "u60sta0", "type", "veth", "peer", "name", "ap0")
    ip("link", "set", "ap0", "netns", "upstream")
    ip("link", "set", "u60sta0", "up")
    for args in [
        ("link", "set", "ap0", "up"), ("link", "set", "lo", "up"),
        ("-6", "addr", "add", "fe80::1/64", "dev", "ap0"),
        ("-6", "addr", "add", "2001:db8:1::53/64", "dev", "ap0"),
    ]:
        ns("upstream", "ip", *args)
    run("sysctl", "-w", "net.ipv6.conf.all.forwarding=1")
    run("sysctl", "-w", "net.ipv6.conf.u60sta0.accept_ra=0", "net.ipv6.conf.u60sta0.autoconf=0")
    ow("ip6tables", "-N", "U60_RELAY_IN")
    ow("ip6tables", "-A", "U60_RELAY_IN", "-j", "DROP")
    ow("ip6tables", "-I", "INPUT", "-i", "u60sta0", "-j", "U60_RELAY_IN")
    for key, value in [("ra", "server"), ("dhcpv6", "server"), ("ndp", "relay")]:
        ow("uci", "set", f"dhcp.lan.{key}={value}")
    ow("uci", "commit", "dhcp")
    # Real netifd provides runtime interface data to the single real odhcpd.
    # A dummy LAN is enough here; packet-level RA tests use a separate bridge.
    (ROOT / "etc/config/network").write_text(
        "config interface 'lan'\n option device 'br-lan'\n option proto 'none'\n"
    )
    time.sleep(2.5)
    processes = []
    try:
        ubus = subprocess.Popen(["chroot", str(ROOT), "/sbin/ubusd"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        processes.append(ubus)
        time.sleep(0.3)
        netifd = subprocess.Popen(["chroot", str(ROOT), "/sbin/netifd"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        processes.append(netifd)
        time.sleep(0.5)
        server = subprocess.Popen(["chroot", str(ROOT), "/usr/sbin/odhcpd"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        processes.append(server)
        (ROOT / "tmp/qa-odhcpd.pid").write_text(str(server.pid))
        sender = subprocess.Popen(["ip", "netns", "exec", "upstream", "python3", "-c", RA_SENDER])
        processes.append(sender)
        subprocess.run([
            "chroot", str(ROOT), "/qa-tests", "--ignored", "--exact",
            "wifi_relay::ipv6::tests::isolated_openwrt_runtime_ra_does_not_restore_old_ipv6", "--nocapture",
        ], env={**os.environ, "U60_IPV6_ISOLATED_QA": "1"}, check=True, timeout=60)
        # netifd's initial proto-none setup flushed the earlier dummy address.
        # Establish the vendor LAN prefix before testing renumber/removal.
        ip("-6", "addr", "replace", "2001:db8:2::1/64", "dev", "br-lan")
        time.sleep(2)
        subprocess.run([
            "chroot", str(ROOT), "/qa-tests", "--ignored", "--exact",
            "wifi_relay::ipv6::tests::isolated_openwrt_start_refresh_stop", "--nocapture",
        ], env={**os.environ, "U60_IPV6_ISOLATED_QA": "1", "U60_IPV6_QA_CALLBACK": "/qa-agent"}, check=True, timeout=120)
        subprocess.run([
            "chroot", str(ROOT), "/qa-tests", "--ignored", "--exact",
            "wifi_relay::portal::tests::isolated_openwrt_portal_lifecycle_preserves_configuration", "--nocapture",
        ], env={**os.environ, "U60_IPV6_ISOLATED_QA": "1"}, check=True, timeout=30)
        print("PASS complete Rust IPv6 lifecycle with real OpenWrt odhcp6c/UCI and Linux firewall/routes")
    finally:
        for process in reversed(processes):
            process.kill()
            process.wait()


RA_SENDER = r"""
import socket, struct, time
s = socket.socket(socket.AF_INET6, socket.SOCK_RAW, socket.IPPROTO_ICMPV6)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, b'ap0\0')
s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_MULTICAST_HOPS, 255)
s.bind(('fe80::1', 0, 0, socket.if_nametoindex('ap0')))
packet = struct.pack('!BBHBBHII', 134, 0, 0, 64, 0, 1800, 0, 0)
packet += struct.pack('!BBBBIII16s', 3, 4, 64, 0xc0, 900, 600, 0, socket.inet_pton(socket.AF_INET6, '2001:db8:1::'))
packet += struct.pack('!BBHI16s', 25, 3, 0, 900, socket.inet_pton(socket.AF_INET6, '2001:db8:1::53'))
packet += struct.pack('!BBHI', 5, 1, 0, 1500)
time.sleep(3)
while True:
    s.sendto(packet, ('ff02::1', 0, 0, socket.if_nametoindex('ap0')))
    time.sleep(0.6)
"""


if __name__ == "__main__":
    lifecycle()
