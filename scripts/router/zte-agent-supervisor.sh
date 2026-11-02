#!/bin/sh
# Minimal boot supervisor for OpenU60 A/B agent updates. Keep this POSIX and
# independent from the agent so a bad candidate can never replace recovery.
set -u

OTA_ROOT="${ZTE_OTA_ROOT:-/data/open-u60/update}"
ACTIVE="$OTA_ROOT/active"
PENDING="$OTA_ROOT/pending"
LEGACY="${ZTE_OTA_LEGACY_LINK:-/data/zte-agent}"
LAST_STABLE="$OTA_ROOT/last-stable/zte-agent"
LAST_STABLE_META="$OTA_ROOT/last-stable/release.json"
BLOCKED="$OTA_ROOT/blocked.json"

slot_binary() {
    printf '%s/slot-%s/zte-agent' "$OTA_ROOT" "$1"
}

slot_meta() {
    printf '%s/slot-%s/release.json' "$OTA_ROOT" "$1"
}

valid_slot() {
    [ "$1" = a ] || [ "$1" = b ]
}

runnable_binary() {
    binary=$1
    meta=$2
    [ -x "$binary" ] || return 1

    if [ -f "$meta" ]; then
        version=$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$meta" | sed -n '1p')
        expected_size=$(sed -n 's/^[[:space:]]*"size"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$meta" | sed -n '1p')
        expected_sha=$(sed -n 's/^[[:space:]]*"sha256"[[:space:]]*:[[:space:]]*"\([0-9a-f][0-9a-f]*\)".*/\1/p' "$meta" | sed -n '1p')
        [ -n "$version" ] && [ -n "$expected_size" ] && [ -n "$expected_sha" ] || return 1
        if [ -f "$BLOCKED" ] && grep -Fq "\"$version\"" "$BLOCKED"; then
            return 1
        fi
        actual_size=$(wc -c < "$binary" | tr -d '[:space:]')
        [ "$actual_size" = "$expected_size" ] || return 1
        actual_sha=$(sha256sum "$binary" 2>/dev/null | sed -n 's/[[:space:]].*//p')
        [ "$actual_sha" = "$expected_sha" ] || return 1
        "$binary" ota-probe "$version" >/dev/null 2>&1
        return $?
    fi

    bootstrap_sha="${binary%/*}/bootstrap.sha256"
    if [ -f "$bootstrap_sha" ]; then
        expected_sha=$(sed -n '1{s/[[:space:]].*//;p;}' "$bootstrap_sha")
        bootstrap_version=$(sed -n '2p' "$bootstrap_sha")
        actual_sha=$(sha256sum "$binary" 2>/dev/null | sed -n 's/[[:space:]].*//p')
        [ -n "$expected_sha" ] && [ -n "$bootstrap_version" ] && [ "$actual_sha" = "$expected_sha" ] || return 1
        "$binary" ota-probe "$bootstrap_version" >/dev/null 2>&1
        return $?
    fi

    # A manually bootstrapped legacy slot has no signed release metadata, so
    # keep the executable-bit compatibility fallback used before OTA.
    return 0
}

runnable_slot() {
    runnable_binary "$(slot_binary "$1")" "$(slot_meta "$1")"
}

runnable_stable() {
    [ -f "$LAST_STABLE_META" ] && runnable_binary "$LAST_STABLE" "$LAST_STABLE_META"
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
excluded=
if [ -f "$PENDING" ]; then
    candidate=$(sed -n '1p' "$PENDING" 2>/dev/null || true)
    previous=$(sed -n '2p' "$PENDING" 2>/dev/null || true)
    if valid_slot "$candidate"; then
        excluded=$candidate
    fi
    if valid_slot "$previous" && runnable_slot "$previous"; then
        atomic_active "$previous"
    fi
    rm -f "$PENDING"
fi

active=$(sed -n '1p' "$ACTIVE" 2>/dev/null || true)
if ! valid_slot "$active" || ! runnable_slot "$active"; then
    if [ "$excluded" != a ] && runnable_slot a; then active=a
    elif [ "$excluded" != b ] && runnable_slot b; then active=b
    elif runnable_stable; then active=stable
    else
        echo "[ota-supervisor] no runnable agent slot" >&2
        exit 127
    fi
fi

if valid_slot "$active"; then
    atomic_active "$active"
    binary=$(slot_binary "$active")
else
    # The last Stable copy is a read-only rescue path. Keep the invalid slot
    # out of the active marker so the agent refuses another install until an
    # operator repairs A/B storage.
    atomic_active rescue
    binary=$LAST_STABLE
fi

ln -sfn "$binary" "$LEGACY.next"
mv -f "$LEGACY.next" "$LEGACY"
exec "$binary"
