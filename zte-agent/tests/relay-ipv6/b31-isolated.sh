#!/bin/sh
# Run only via USB ADB on the intended B31. All links/routes live in four
# temporary namespaces. Chroot /tmp and /data isolate Rust recovery/config files.
set -eu
QA=/tmp/u60-network-qa
ROOT=$QA/root
R=u60qa_router
L=u60qa_lan
W=u60qa_wan
C=u60qa_cell
test -f "$QA/agent-tests"
test -f "$QA/device-peer"
test -f "$QA/mihomo"
test ! -e "$ROOT"
for n in "$R" "$L" "$W" "$C"; do test ! -e "/var/run/netns/$n"; done
mkdir -p "$ROOT" "$QA/state"
cleanup() {
    for n in "$R" "$L" "$W" "$C"; do
        for p in $(ip netns pids "$n" 2>/dev/null || true); do kill "$p" 2>/dev/null || true; done
    done
    sleep 1
    for n in "$R" "$L" "$W" "$C"; do
        for p in $(ip netns pids "$n" 2>/dev/null || true); do kill -9 "$p" 2>/dev/null || true; done
        ip netns delete "$n" 2>/dev/null || true
    done
    for d in qa-state qa-core dev/net/tun dev/null proc lib usr sbin bin; do umount "$ROOT/$d" 2>/dev/null || true; done
}
trap 'code=$?; trap - EXIT HUP INT TERM; cleanup; exit "$code"' EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM
for d in bin sbin usr lib proc; do mkdir -p "$ROOT/$d"; mount --bind "/$d" "$ROOT/$d"; done
mkdir -p "$ROOT/tmp" "$ROOT/data" "$ROOT/etc/config" "$ROOT/qa-state" "$ROOT/qa-core" "$ROOT/dev"
mkdir -p "$ROOT/var/run"
# /tmp is nodev on B31; bind only the two required character devices.
touch "$ROOT/dev/null"
mount --bind /dev/null "$ROOT/dev/null"
mkdir -p "$ROOT/dev/net"
touch "$ROOT/dev/net/tun"
mount --bind /dev/net/tun "$ROOT/dev/net/tun"
chroot "$ROOT" /bin/sh -c 'ip -Version </dev/null >/dev/null'
touch "$ROOT/u60-ipv6-qa-disposable"
mount --bind "$QA/state" "$ROOT/qa-state"
mkdir "$QA/core"
cp "$QA/mihomo" "$QA/core/mihomo"
chmod 700 "$QA/core/mihomo"
mount --bind "$QA/core" "$ROOT/qa-core"
cp "$QA/agent-tests" "$ROOT/agent-tests"
cp "$QA/device-peer" "$ROOT/device-peer"
cp "$QA/plan.json" "$ROOT/plan.json"
for n in "$R" "$L" "$W" "$C"; do ip netns add "$n"; ip -n "$n" link set lo up; done
for pair in "br-lan lan0 $L" "u60sta0 wan0 $W" "rmnet_data0 cell0 $C"; do
    set -- $pair
    if [ "$1" = br-lan ]; then
        ip -n "$R" link add br-lan type bridge
        ip -n "$R" link add lanport type veth peer name "$2"
        ip -n "$R" link set lanport master br-lan
        ip -n "$R" link set lanport up
    else
        ip -n "$R" link add "$1" type veth peer name "$2"
    fi
    ip -n "$R" link set "$2" netns "$3"
    ip -n "$R" link set "$1" up
    ip -n "$3" link set "$2" up
done
ip netns exec "$R" sysctl -w net.ipv4.ip_forward=1 net.ipv6.conf.all.forwarding=1 >/dev/null
ip -n "$R" -4 addr add 192.168.0.1/24 dev br-lan
ip -n "$R" -4 addr add 10.9.0.2/24 dev rmnet_data0
ip -n "$R" -6 addr add 2001:db8:2::1/64 dev br-lan
ip -n "$R" -6 addr add 2001:db8:9::2/64 dev rmnet_data0
ip -n "$R" -4 route add default via 10.9.0.1 dev rmnet_data0
ip -n "$R" -6 route add default via fe80::9 dev rmnet_data0
ip -n "$L" -4 addr add 192.168.0.123/24 dev lan0
ip -n "$L" -6 addr add 2001:db8:2::123/64 dev lan0
ip -n "$L" -6 addr add fd12:3456:789a::123/64 dev lan0
ip -n "$L" -4 route add default via 192.168.0.1 dev lan0
ip -n "$L" -6 route add default via 2001:db8:2::1 dev lan0
ip -n "$W" -4 addr add 10.0.0.1/24 dev wan0
ip -n "$W" -6 addr add fe80::1/64 dev wan0
ip -n "$W" -6 addr add 2001:db8:1::53/64 dev wan0
ip -n "$C" -4 addr add 10.9.0.1/24 dev cell0
ip -n "$C" -6 addr add fe80::9/64 dev cell0
ip -n "$C" -6 addr add 2001:db8:9::1/64 dev cell0
for n in "$W" "$C"; do
    ip -n "$n" -4 addr add 203.0.113.1/32 dev lo
    ip -n "$n" -6 addr add 2001:db8:ffff::1/128 dev lo
    ip netns exec "$n" "$QA/device-peer" server > "$QA/server-$n.log" 2>&1 &
done
for tool in iptables ip6tables; do
    ip netns exec "$R" "$tool" -t nat -N redirect_chain
    ip netns exec "$R" "$tool" -P INPUT DROP
    ip netns exec "$R" "$tool" -P FORWARD DROP
    ip netns exec "$R" "$tool" -A INPUT -i lo -j ACCEPT
    ip netns exec "$R" "$tool" -A INPUT -i br-lan -j ACCEPT
    ip netns exec "$R" "$tool" -A INPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
    if [ "$tool" = ip6tables ]; then ip netns exec "$R" "$tool" -A INPUT -p ipv6-icmp -j ACCEPT; fi
    ip netns exec "$R" "$tool" -N U60_RELAY_IN
    if [ "$tool" = iptables ]; then ip netns exec "$R" "$tool" -A U60_RELAY_IN -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT; fi
    ip netns exec "$R" "$tool" -A U60_RELAY_IN -j DROP
    ip netns exec "$R" "$tool" -I INPUT -i u60sta0 -j U60_RELAY_IN
done
sleep 3
ip netns exec "$L" "$QA/device-peer" persistent "$QA/state" > "$QA/persistent.log" 2>&1 &
ip netns exec "$R" chroot "$ROOT" /bin/sh -c 'U60_CORE_ISOLATED_QA=1 U60_CORE_QA_ROOT=/qa-core /agent-tests --ignored --exact mihomo::tests::isolated_real_core_uplink_transitions --nocapture' > "$QA/core-test.log" 2>&1 &
TEST=$!
wait_for() {
    count=0
    while [ ! -f "$QA/state/$1" ]; do
        kill -0 "$TEST" 2>/dev/null || { cat "$QA/core-test.log"; return 1; }
        count=$((count+1)); test "$count" -lt 120 || { cat "$QA/core-test.log"; return 1; }
        sleep 1
    done
}
check() {
    stage=$1; v4=$2; v6=$3
    wait_for "$stage"; wait_for "udp-$stage"
    test "$(cat "$QA/state/udp-$stage")" = "$v4"
    for proto in tcp udp; do
        seen=$(ip netns exec "$L" "$QA/device-peer" client "$proto" 203.0.113.1)
        test "$seen" = "$v4" || { echo "FAIL $stage $proto IPv4 $seen"; return 1; }
        if [ "$v6" != none ]; then
            seen=$(ip netns exec "$L" "$QA/device-peer" client "$proto" 2001:db8:ffff::1)
            test "$seen" = "$v6" || { echo "FAIL $stage $proto IPv6 $seen"; return 1; }
        else
            seen=$(ip netns exec "$L" "$QA/device-peer" client "$proto" 2001:db8:ffff::1 2>/dev/null || true)
            test "$seen" != 2001:db8:9::2
            test "$seen" != 2001:db8:1::2
        fi
    done
    echo "PASS B31 isolated real-core $stage TCP/UDP v4=$v4 v6=$v6 persistent UDP"
}
plan() { ip netns exec "$R" chroot "$ROOT" /device-peer plan /plan.json "$1" "$2"; }
check cell-ready 10.9.0.2 2001:db8:9::2
plan ipv4 apply
touch "$QA/state/wifi"
check wifi-ready 10.0.0.2 none
plan setup apply
plan network apply
touch "$QA/state/v6"
check v6-ready 10.0.0.2 2001:db8:1::2
plan network undo; plan setup undo; plan ipv4 undo
touch "$QA/state/cell"
check cell-again 10.9.0.2 2001:db8:9::2
plan ipv4 apply; plan setup apply; plan network apply
touch "$QA/state/wifi2"
check wifi2-ready 10.0.0.2 2001:db8:1::2
ip -n "$R" -4 route del default via 10.9.0.1 dev rmnet_data0
ip -n "$R" -6 route del default via fe80::9 dev rmnet_data0
plan network undo; plan setup undo; plan ipv4 undo
touch "$QA/state/offline"
wait_for offline-ready
if ip -n "$R" -4 route get 203.0.113.1 >/dev/null 2>&1; then exit 1; fi
if ip -n "$R" -6 route get 2001:db8:ffff::1 >/dev/null 2>&1; then exit 1; fi
echo 'PASS B31 isolated core remains enabled offline; no cellular route created'
plan ipv4 apply; plan setup apply; plan network apply
touch "$QA/state/wifi3"
check wifi3-ready 10.0.0.2 2001:db8:1::2
ip -n "$R" -4 route add default via 10.9.0.1 dev rmnet_data0
ip -n "$R" -6 route add default via fe80::9 dev rmnet_data0
plan network undo; plan setup undo; plan ipv4 undo
touch "$QA/state/stalled"
check stalled-ready 10.9.0.2 2001:db8:9::2
touch "$QA/state/finish"
wait "$TEST"
ip -n "$R" -4 route get 203.0.113.1 | grep -q 'dev rmnet_data0'
ip -n "$R" -6 route get 2001:db8:ffff::1 | grep -q 'dev rmnet_data0'
cat "$QA/core-test.log"
echo 'PASS B31 kernel/core isolated dual-stack transitions and rollback; physical radio/SIM routes unchanged'
