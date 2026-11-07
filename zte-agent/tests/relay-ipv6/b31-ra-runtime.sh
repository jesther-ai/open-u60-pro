#!/bin/sh
# Explicit USB QA only. Firmware netifd/odhcpd, private config/data, no real LAN.
set -eu
QA=/tmp/u60-ra-runtime-qa
ROOT=$QA/root
R=u60ra_router
C=u60ra_client
test -x "$QA/agent-tests"
test ! -e "$ROOT"
test ! -e "/var/run/netns/$R"
test ! -e "/var/run/netns/$C"
mkdir -p "$ROOT" "$QA/results"
cleanup() {
    for n in "$R" "$C"; do
        for p in $(ip netns pids "$n" 2>/dev/null || true); do kill "$p" 2>/dev/null || true; done
    done
    sleep 1
    for n in "$R" "$C"; do
        for p in $(ip netns pids "$n" 2>/dev/null || true); do kill -9 "$p" 2>/dev/null || true; done
        ip netns delete "$n" 2>/dev/null || true
    done
    for d in dev/urandom dev/null proc lib usr sbin bin; do umount "$ROOT/$d" 2>/dev/null || true; done
}
trap 'code=$?; trap - EXIT HUP INT TERM; cleanup; exit "$code"' EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM
for d in bin sbin usr lib proc; do mkdir -p "$ROOT/$d"; mount --bind "/$d" "$ROOT/$d"; done
mkdir -p "$ROOT/tmp/run/ubus" "$ROOT/tmp/lock" "$ROOT/tmp/resolv.conf.d" "$ROOT/data" "$ROOT/dev" "$ROOT/sys" "$ROOT/etc/config" "$ROOT/etc/init.d"
ln -s /tmp "$ROOT/var"
for d in null urandom; do touch "$ROOT/dev/$d"; mount --bind "/dev/$d" "$ROOT/dev/$d"; done
cp /etc/passwd /etc/group "$ROOT/etc/"
cp "$QA/agent-tests" "$ROOT/qa-tests"
touch "$ROOT/u60-ipv6-qa-disposable"
cat > "$ROOT/etc/config/network" <<'CONFIG'
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
CONFIG
cat > "$ROOT/etc/config/dhcp" <<'CONFIG'
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
CONFIG
cat > "$ROOT/etc/init.d/odhcpd" <<'SCRIPT'
#!/bin/sh
kill -HUP "$(cat /tmp/qa-odhcpd.pid)"
SCRIPT
chmod 700 "$ROOT/etc/init.d/odhcpd"
for n in "$R" "$C"; do ip netns add "$n"; ip -n "$n" link set lo up; done
ip -n "$R" link add lan0 type veth peer name client0
ip -n "$R" link set client0 netns "$C"
ip -n "$R" link set lan0 up
ip -n "$C" link set client0 up
cat > "$QA/run-private.sh" <<'SCRIPT'
#!/bin/sh
set -eu
QA=/tmp/u60-ra-runtime-qa
ROOT=$QA/root
mount -t sysfs sysfs "$ROOT/sys"
sysctl -w net.ipv6.conf.all.forwarding=1 >/dev/null
chroot "$ROOT" /sbin/ubusd -s /var/run/ubus/ubus.sock >"$QA/ubusd.log" 2>&1 &
ubus_pid=$!
sleep 1
kill -0 "$ubus_pid"
test -S "$ROOT/var/run/ubus/ubus.sock"
chroot "$ROOT" ubus list >/dev/null
chroot "$ROOT" /sbin/netifd -h /bin/true -S -l 4 >"$QA/netifd.log" 2>&1 &
sleep 1
chroot "$ROOT" /usr/sbin/odhcpd >"$QA/odhcpd.log" 2>&1 &
echo $! >"$ROOT/tmp/qa-odhcpd.pid"
sleep 3
agent() {
    U60_IPV6_ISOLATED_QA=1 U60_IPV6_RA_QA_ACTION="$1" chroot "$ROOT" /qa-tests --ignored --exact wifi_relay::ipv6::tests::isolated_openwrt_ra_packet_driver --nocapture
}
capture() {
    ip netns exec u60ra_client tcpdump -i client0 -c 1 -s 0 -U -w "$QA/results/$1.pcap" 'icmp6 and ip6[40] == 134' >"$QA/capture.log" 2>&1 &
    capture=$!
    sleep 0.3
    kill -HUP "$(cat "$ROOT/tmp/qa-odhcpd.pid")"
    count=0
    while kill -0 "$capture" 2>/dev/null; do
        count=$((count+1))
        if [ "$count" -gt 10 ]; then kill "$capture"; wait "$capture" || true; echo "RA capture timeout" >&2; exit 1; fi
        sleep 1
    done
    wait "$capture"
    test -s "$QA/results/$1.pcap"
}
capture baseline
before=$(sha256sum "$ROOT/etc/config/dhcp")
agent apply
capture relay
test "$(sha256sum "$ROOT/etc/config/dhcp")" = "$before"
test -z "$(chroot "$ROOT" uci -q changes dhcp)"
chroot "$ROOT" uci set dhcp.lan.ra_mtu=1450
chroot "$ROOT" uci delete dhcp.lan.dns
chroot "$ROOT" uci add_list dhcp.lan.dns=2001:db8:8::54
chroot "$ROOT" uci commit dhcp
current=$(sha256sum "$ROOT/etc/config/dhcp")
capture vendor-reload
agent maintain
agent release
capture released
test "$(sha256sum "$ROOT/etc/config/dhcp")" = "$current"
test -z "$(chroot "$ROOT" uci -q changes dhcp)"
agent release
echo 'PASS B31 firmware runtime RA lifecycle; packet assertions run on computer'
SCRIPT
chmod 700 "$QA/run-private.sh"
# ip netns exec creates a private mount namespace too: the sysfs mount never
# replaces the physical device's /sys or survives this test namespace.
ip netns exec "$R" sh "$QA/run-private.sh"
