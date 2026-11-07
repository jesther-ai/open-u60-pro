#!/bin/sh
set -eu
test -f /.dockerenv
test -r /agent-bin
test -r /agent-tests
test ! -e /ow
mkdir /ow
tar xzf /cache/openwrt-rootfs.tar.gz -C /ow
cp -a /cache/ow-extra/. /ow/
ln -sf /usr/libexec/ip-full /ow/sbin/ip
cp /agent-bin /ow/qa-agent
cp /agent-tests /ow/qa-tests

# Keep OpenWrt's real client/UCI/services and use Debian's netfilter userspace
# against the container kernel. No router or host networking is mounted.
mkdir -p /ow/usr/lib/aarch64-linux-gnu
mount --bind /usr/lib/aarch64-linux-gnu /ow/usr/lib/aarch64-linux-gnu
ln -sf /usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1 /ow/lib/ld-linux-aarch64.so.1
cp /usr/sbin/xtables-nft-multi /ow/usr/sbin/
ln -sf xtables-nft-multi /ow/usr/sbin/ip6tables
ln -sf xtables-nft-multi /ow/usr/sbin/iptables
ln -sf xtables-nft-multi /ow/usr/sbin/iptables-restore
ln -sf xtables-nft-multi /ow/usr/sbin/ip6tables-restore
case "${U60_OPENWRT_QA_MODE:-lifecycle}" in
    lifecycle) exec python3 /qa/openwrt_lifecycle.py ;;
    runtime-ra) exec python3 /qa/ra_runtime.py ;;
    *) echo "Unknown isolated OpenWrt QA mode" >&2; exit 1 ;;
esac
