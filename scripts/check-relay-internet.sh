#!/bin/sh
# Run on U60 over USB: adb shell sh /tmp/check-relay-internet.sh
# Every HTTP/ICMP probe binds to the relay, so cellular cannot hide a failure.
set -u
iface=u60sta0
gateway=$(ip -4 route show table 60631 | awk '$1 == "default" && $2 == "via" { print $3; exit }')
[ -n "$gateway" ] || { echo 'FAIL: no relay default gateway'; exit 1; }
failed=0
echo '=== relay IPv4 route ==='
ip -4 route get 223.5.5.5
echo '=== upstream gateway ==='
ping -I "$iface" -c 5 -W 2 "$gateway" || failed=1
echo '=== public IPv4 ==='
ping -I "$iface" -c 5 -W 2 223.5.5.5 || failed=1
echo '=== upstream DNS ==='
nslookup www.baidu.com "$gateway" || failed=1
echo '=== IPv4 HTTPS ==='
code=$(curl -4 --noproxy '*' --interface "$iface" --connect-timeout 5 --max-time 15 \
    -sS -o /dev/null -w '%{http_code}' https://www.baidu.com) || failed=1
echo "HTTPS IPv4 status=$code"
[ "$code" = 200 ] || failed=1
if ip -6 addr show dev "$iface" scope global | grep -q 'inet6'; then
    echo '=== IPv6 HTTPS ==='
    code=$(curl -6 --noproxy '*' --interface "$iface" --connect-timeout 5 --max-time 15 \
        -sS -o /dev/null -w '%{http_code}' https://www.baidu.com) || failed=1
    echo "HTTPS IPv6 status=$code"
    [ "$code" = 200 ] || failed=1
else
    echo 'IPv6 not configured; IPv4-only test'
fi
[ "$failed" = 0 ] && echo 'PASS: relay Internet probes completed'
exit "$failed"
