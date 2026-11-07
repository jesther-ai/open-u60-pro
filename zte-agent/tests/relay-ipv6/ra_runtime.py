#!/usr/bin/env python3
"""Assert real downstream RAs from Rust + unmodified netifd/odhcpd, offline."""

import json
import os
from pathlib import Path
import signal
import subprocess
import time

from netns import ip, ns, run

ROOT = Path("/ow")


def ow(*args):
    return run("chroot", str(ROOT), *args).stdout.strip()


def ubus(method, value=None):
    raw = ow("ubus", "call", "network.interface.lan", method, json.dumps(value or {}))
    return json.loads(raw) if raw else None


def agent(action, mtu=1480):
    subprocess.run([
        "chroot", str(ROOT), "/qa-tests", "--ignored", "--exact",
        "wifi_relay::ipv6::tests::isolated_openwrt_ra_packet_driver", "--nocapture",
    ], env={**os.environ, "U60_IPV6_ISOLATED_QA": "1", "U60_IPV6_RA_QA_ACTION": action,
            "U60_IPV6_RA_QA_MTU": str(mtu)},
        check=True, timeout=20)


def advertisement():
    return json.loads(ns("client", "python3", "-c", RA_CLIENT).stdout)


def check_relay(mtu=1480):
    packet = advertisement()
    assert packet["lifetime"] == 90, packet
    assert packet["flags"] == 0, packet
    assert packet["mtu"] == mtu, packet
    assert packet["prefixes"] == ["fd12:3456:789a::/64"], packet
    assert packet["dns"] == [], "Never advertise stale cellular DNS during relay"


def main():
    assert Path("/.dockerenv").exists() and os.geteuid() == 0
    assert {link["ifname"] for link in json.loads(ip("-j", "link"))} == {"lo"}
    for directory in ["proc", "dev"]:
        run("mount", "--bind", "/" + directory, str(ROOT / directory))
    run("mount", "-t", "sysfs", "sysfs", str(ROOT / "sys"))
    for directory in ["tmp/lock", "tmp/run", "var/lock", "tmp/resolv.conf.d"]:
        (ROOT / directory).mkdir(parents=True, exist_ok=True)
    (ROOT / "u60-ipv6-qa-disposable").touch()
    ip("link", "set", "lo", "up")
    ip("netns", "add", "client")
    ip("link", "add", "lan0", "type", "veth", "peer", "name", "client0")
    ip("link", "set", "client0", "netns", "client")
    ip("link", "set", "lan0", "up")
    ns("client", "ip", "link", "set", "lo", "up")
    ns("client", "ip", "link", "set", "client0", "up")
    run("sysctl", "-w", "net.ipv6.conf.all.forwarding=1")
    (ROOT / "etc/config/network").write_text("""
config device
 option name 'br-lan'
 option type 'bridge'
 list ports 'lan0'
config interface 'lan'
 option device 'br-lan'
 option proto 'static'
 option ipaddr '192.0.2.1'
 option netmask '255.255.255.0'
 list ip6addr '2001:db8:2::1/64'
 list ip6addr 'fd12:3456:789a::1/64'
""")
    (ROOT / "etc/config/dhcp").write_text("""
config dhcp 'lan'
 option interface 'lan'
 option ra 'server'
 option dhcpv6 'server'
 option ndp 'disabled'
 option ra_default '2'
 option ra_lifetime '1200'
 option ra_mininterval '3'
 option ra_maxinterval '4'
 option ra_mtu '1500'
 list dns '2001:db8:9::53'
 list ra_flags 'other-config'
""")
    service = ROOT / "etc/init.d/odhcpd"
    service.write_text("#!/bin/ash\nkill -HUP $(cat /tmp/qa-odhcpd.pid)\n")
    service.chmod(0o755)
    processes = []
    try:
        for binary in [["/sbin/ubusd"], ["/sbin/netifd"], ["/usr/sbin/odhcpd"]]:
            process = subprocess.Popen(["chroot", str(ROOT), *binary],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            processes.append(process)
            time.sleep(0.5)
        daemon = processes[-1]
        (ROOT / "tmp/qa-odhcpd.pid").write_text(str(daemon.pid))
        time.sleep(2)
        assert advertisement()["dns"] == ["2001:db8:9::53"]
        before = (ROOT / "etc/config/dhcp").read_bytes()
        ubus("set_data", {"qa_preserve": {"nested": "vendor"}})
        agent("apply")
        check_relay()
        assert (ROOT / "etc/config/dhcp").read_bytes() == before
        assert ow("uci", "-q", "changes", "dhcp") == ""
        ow("uci", "set", "dhcp.lan.ra_mtu=1450")
        ow("uci", "delete", "dhcp.lan.dns")
        ow("uci", "add_list", "dhcp.lan.dns=2001:db8:8::54")
        ow("uci", "commit", "dhcp")
        os.kill(daemon.pid, signal.SIGHUP)
        time.sleep(0.3)
        check_relay()
        current = (ROOT / "etc/config/dhcp").read_bytes()
        agent("maintain")
        # New process: actual journal-driven same-boot crash cleanup.
        agent("release")
        restored = advertisement()
        assert restored["dns"] == ["2001:db8:8::54"], restored
        assert restored["mtu"] == 1450 and restored["lifetime"] == 1200, restored
        assert ubus("status")["data"]["qa_preserve"] == {"nested": "vendor"}
        assert (ROOT / "etc/config/dhcp").read_bytes() == current
        agent("release")
        print("PASS actual RA: add, vendor commit/reload, current DNS/MTU, crash cleanup, unrelated data")

        agent("apply", mtu=1400)
        check_relay(mtu=1400)
        agent("release")
        ip("link", "set", "br-lan", "mtu", "1360")
        agent("apply", mtu=1480)
        check_relay(mtu=1360)
        agent("release")
        ip("link", "set", "br-lan", "mtu", "1500")
        assert (ROOT / "etc/config/dhcp").read_bytes() == current
        print("PASS actual RA MTU: upstream changes, LAN cap, no persistent DHCP write")

        agent("apply")
        check_relay()
        agent("previous-boot")
        # Simulate reboot by replacing netifd, without cycling LAN or the VM.
        processes[1].kill()
        processes[1].wait()
        processes[1] = subprocess.Popen(["chroot", str(ROOT), "/sbin/netifd"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(2)
        agent("release")
        packet = advertisement()
        assert packet["dns"] == ["2001:db8:8::54"] and packet["mtu"] == 1450, packet
        assert (ROOT / "etc/config/dhcp").read_bytes() == current
        assert not (ROOT / "data/zte-agent-relay/ra-runtime.json").exists()
        print("PASS previous boot: runtime data disappears; fresh vendor IPv6 untouched")
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.kill()
            process.wait()


RA_CLIENT = r"""
import socket, struct, json, time
s = socket.socket(socket.AF_INET6, socket.SOCK_RAW, socket.IPPROTO_ICMPV6)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, b'client0\0')
s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_MULTICAST_HOPS, 255)
s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_UNICAST_HOPS, 255)
s.settimeout(0.5)
s.bind(('::', 0, 0, socket.if_nametoindex('client0')))
end = time.monotonic() + 8
while time.monotonic() < end:
    s.sendto(struct.pack('!BBHI',133,0,0,0), ('ff02::2',0,0,socket.if_nametoindex('client0')))
    try: packet, peer = s.recvfrom(4096)
    except TimeoutError: continue
    if packet[0] != 134: continue
    result = {'lifetime':struct.unpack('!H',packet[6:8])[0], 'flags':packet[5], 'dns':[], 'prefixes':[]}
    offset = 16
    while offset + 2 <= len(packet):
        kind, units = packet[offset:offset+2]; size = units * 8
        if not size: raise Exception('Invalid RA option')
        part = packet[offset:offset+size]
        if kind == 25:
            result['dns'] += [socket.inet_ntop(socket.AF_INET6,part[x:x+16]) for x in range(8,size,16)]
        if kind == 3:
            result['prefixes'].append(socket.inet_ntop(socket.AF_INET6,part[16:32])+'/'+str(part[2]))
        if kind == 5: result['mtu'] = struct.unpack('!I',part[4:8])[0]
        offset += size
    print(json.dumps(result)); break
else: raise Exception('No downstream RA')
"""


if __name__ == "__main__":
    main()
