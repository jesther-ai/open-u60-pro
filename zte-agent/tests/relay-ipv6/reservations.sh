#!/bin/sh
# Disposable ARM64 OpenWrt container, --privileged --network none; no host net.
set -eu
test -f /.dockerenv
test "$(ip -o link show | wc -l)" -eq 1
touch /u60-dhcp-disposable
mkdir -p /tmp /var/etc /var/run /etc/config
touch /etc/config/network /etc/config/dhcp
ip link add br-lan type bridge
ip addr add 192.168.50.1/24 dev br-lan
ip link set br-lan up
ip netns add client
ip link add lanport type veth peer name eth0
ip link set lanport master br-lan
ip link set lanport up
ip link set eth0 netns client
ip netns exec client ip link set lo up
ip netns exec client ip link set eth0 address 02:00:00:00:60:02
ip netns exec client ip link set eth0 up
uci set network.lan=interface
uci set network.lan.ipaddr=192.168.50.1
uci set network.lan.netmask=255.255.255.0
cat > /var/etc/dnsmasq.conf.lan_dns <<'EOF'
port=0
interface=br-lan
bind-interfaces
read-ethers
dhcp-range=192.168.50.100,192.168.50.120,255.255.255.0,1h
dhcp-leasefile=/tmp/qa.leases
pid-file=/var/run/qa-dnsmasq.pid
dhcp-authoritative
EOF
dnsmasq -C /var/etc/dnsmasq.conf.lan_dns -k >/tmp/dnsmasq.log 2>&1 &
DNS=$!
trap 'code=$?; if [ "$code" != 0 ]; then cat /tmp/dnsmasq.log /etc/ethers /tmp/qa.leases; fi; kill "$DNS" 2>/dev/null || true; ip netns delete client 2>/dev/null || true' EXIT
sleep 1
U60_DHCP_QA=1 /agent-tests --ignored --exact dhcp_reservations::tests::isolated_dhcp_reservation_lifecycle --nocapture &
TEST=$!
for i in $(seq 1 15); do test -f /tmp/qa-ready && break; kill -0 "$TEST"; sleep 1; done
test -f /tmp/qa-ready
cat > /tmp/lease.sh <<'EOF'
#!/bin/sh
if [ "$1" = bound ] && [ "$ip" = 192.168.50.60 ]; then touch /tmp/qa-client-leased; fi
EOF
chmod +x /tmp/lease.sh
ip netns exec client udhcpc -f -n -q -i eth0 -s /tmp/lease.sh -t 5 -T 1
test -f /tmp/qa-client-leased
wait "$TEST"
kill -0 "$DNS"
echo 'PASS production reservation add / DHCP lease / conflict / remove; dnsmasq never restarted'
