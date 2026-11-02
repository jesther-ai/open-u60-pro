#!/bin/sh
# Minimal boot supervisor for OpenU60 A/B agent updates. Keep this POSIX and
# independent from the agent so a bad candidate can never replace recovery.
set -u

OTA_ROOT="${ZTE_OTA_ROOT:-/data/open-u60/update}"
ACTIVE="$OTA_ROOT/active"
PENDING="$OTA_ROOT/pending"
LEGACY="${ZTE_OTA_LEGACY_LINK:-/data/zte-agent}"

slot_binary() {
    printf '%s/slot-%s/zte-agent' "$OTA_ROOT" "$1"
}

valid_slot() {
    [ "$1" = a ] || [ "$1" = b ]
}

atomic_active() {
    printf '%s\n' "$1" > "$ACTIVE.tmp"
    chmod 600 "$ACTIVE.tmp"
    mv -f "$ACTIVE.tmp" "$ACTIVE"
}

mkdir -p "$OTA_ROOT/slot-a" "$OTA_ROOT/slot-b" "$OTA_ROOT/last-stable"
chmod 700 "$OTA_ROOT" "$OTA_ROOT/slot-a" "$OTA_ROOT/slot-b" "$OTA_ROOT/last-stable"

# A pending marker exists from before the candidate health trial. If power was
# lost or both processes died, select the recorded previous slot first.
if [ -f "$PENDING" ]; then
    previous=$(sed -n '2p' "$PENDING" 2>/dev/null || true)
    if valid_slot "$previous" && [ -x "$(slot_binary "$previous")" ]; then
        atomic_active "$previous"
    fi
    rm -f "$PENDING"
fi

active=$(sed -n '1p' "$ACTIVE" 2>/dev/null || true)
if ! valid_slot "$active" || [ ! -x "$(slot_binary "$active")" ]; then
    if [ -x "$(slot_binary a)" ]; then active=a
    elif [ -x "$(slot_binary b)" ]; then active=b
    else
        echo "[ota-supervisor] no executable agent slot" >&2
        exit 127
    fi
    atomic_active "$active"
fi

binary=$(slot_binary "$active")
ln -sfn "$binary" "$LEGACY.next"
mv -f "$LEGACY.next" "$LEGACY"
exec "$binary"
