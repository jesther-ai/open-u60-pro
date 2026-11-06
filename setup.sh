#!/bin/bash
set -e
cd "$(cd "$(dirname "$0")" && pwd)"

# ── Colors ───────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()  { echo -e "${CYAN}[*]${NC} $1"; }
ok()    { echo -e "${GREEN}[+]${NC} $1"; }
warn()  { echo -e "${YELLOW}[!]${NC} $1"; }
fail()  { echo -e "${RED}[-]${NC} $1" >&2; exit 1; }

# Existing interactive/positional use remains supported. The local B31 mode
# needs no plaintext password when USB ADB is already authorized.
BUILD_CHOICE=""
FACTORY_PASSWORD=false
FORCE_ADB=false
SKIP_SSH=false
while [ $# -gt 0 ]; do
    case "$1" in
        --build) BUILD_CHOICE=2; shift ;;
        --download) BUILD_CHOICE=1; shift ;;
        --factory-password) FACTORY_PASSWORD=true; shift ;;
        --adb) FORCE_ADB=true; shift ;;
        --no-ssh) SKIP_SSH=true; shift ;;
        --serial)
            [ $# -ge 2 ] && [ -n "$2" ] || fail "--serial needs an ADB serial."
            export ANDROID_SERIAL="$2"
            FORCE_ADB=true
            shift 2 ;;
        --help|-h)
            echo "Usage: ./setup.sh [--build|--download] [--adb] [--serial SERIAL] [--no-ssh]"
            echo "                  [--factory-password | router-password agent-password]"
            echo "Local B31: ./setup.sh --build --adb --factory-password --no-ssh"
            echo "Factory-password mode requires a locally built agent and authorized ADB or SSH."
            exit 0 ;;
        --*) fail "Unknown option: $1" ;;
        *) break ;;
    esac
done

# ── Password input ──────────────────────────────────────────────────
if [ "$FACTORY_PASSWORD" = true ]; then
    [ $# -eq 0 ] || fail "Do not pass passwords with --factory-password."
    [ "$BUILD_CHOICE" != 1 ] || fail "Factory-password mode requires --build, not an older release download."
    BUILD_CHOICE=2
    ROUTER_PASSWORD=""
    AGENT_PASSWORD=""
else
if [ $# -ge 2 ]; then
    [ $# -eq 2 ] || fail "Expected exactly two passwords."
    ROUTER_PASSWORD="$1"
    AGENT_PASSWORD="$2"
elif [ $# -eq 0 ]; then
    echo -e "${CYAN}Router admin password:${NC} "
    read -rs ROUTER_PASSWORD; echo
    echo -e "${CYAN}Agent API password:${NC} "
    read -rs AGENT_PASSWORD; echo
    [ -z "$ROUTER_PASSWORD" ] && fail "Router password cannot be empty."
    [ -z "$AGENT_PASSWORD" ] && fail "Agent password cannot be empty."
else
    echo "Usage: ./setup.sh [router-password agent-password]"
    echo "       ./setup.sh    (interactive — prompts for passwords)"
    exit 1
fi
[ -n "$AGENT_PASSWORD" ] || fail "Agent password cannot be empty; use --factory-password instead."
fi

if [[ "$AGENT_PASSWORD" == *$'\n'* || "$AGENT_PASSWORD" == *$'\r'* ]]; then
    fail "Agent password cannot contain a line break."
fi

GATEWAY=192.168.0.1
AGENT_PORT=9090
SSH_PORT=2222
TARGET=aarch64-unknown-linux-musl
BINARY=target/$TARGET/release/zte-agent
OTA_ROOT=/data/open-u60/update
REMOTE_SLOT=$OTA_ROOT/slot-a/zte-agent
REMOTE_BIN=/data/zte-agent
SUPERVISOR_LOCAL=scripts/router/zte-agent-supervisor.sh
STARTUP_SCRIPT=/data/local/tmp/start_zte_agent.sh
DOWNLOAD_URL="${ZTE_AGENT_DOWNLOAD_URL:-https://github.com/jesther-ai/open-u60-pro/releases/latest/download/zte-agent}"
WORKDIR=$(mktemp -d)
chmod 700 "$WORKDIR"
umask 077
FORWARD_PORT=""
cleanup() {
    [ -z "$FORWARD_PORT" ] || adb forward --remove "tcp:$FORWARD_PORT" >/dev/null 2>&1 || true
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

# ── Binary source menu ──────────────────────────────────────────────
if [ -z "$BUILD_CHOICE" ]; then
echo ""
echo -e "${CYAN}How would you like to get the zte-agent binary?${NC}"
echo "  1) Download pre-built from GitHub (recommended — no dev tools needed)"
echo "  2) Build from source (requires Rust toolchain)"
echo ""
echo -n "Choice [1]: "
read -r BUILD_CHOICE
BUILD_CHOICE="${BUILD_CHOICE:-1}"
fi

if [ "$BUILD_CHOICE" != "1" ] && [ "$BUILD_CHOICE" != "2" ]; then
    fail "Invalid choice. Enter 1 or 2."
fi

# ── Step 0: Prerequisites ───────────────────────────────────────────
info "Checking prerequisites..."

OS="$(uname -s)"
HAS_BREW=false
HAS_APT=false
command -v brew >/dev/null 2>&1 && HAS_BREW=true
command -v apt-get >/dev/null 2>&1 && HAS_APT=true

# Cross-compiler tool varies by platform
if [ "$OS" = "Darwin" ]; then
    CROSS_CC="aarch64-linux-musl-gcc"
else
    CROSS_CC="aarch64-linux-gnu-gcc"
fi

# Dependency table: cmd | brew_pkg | apt_pkg | custom_install
# Build path needs all deps; download path only needs curl, python3, adb
if [ "$BUILD_CHOICE" = "1" ]; then
    DEPS=(
        "curl|curl|curl|"
        "python3|python3|python3|"
        "adb|android-platform-tools|android-tools-adb|"
    )
else
    DEPS=(
        "curl|curl|curl|"
        "python3|python3|python3|"
        "adb|android-platform-tools|android-tools-adb|"
        "cargo|||curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"
        "$CROSS_CC|filosottile/musl-cross/musl-cross|gcc-aarch64-linux-gnu musl-tools|"
    )
fi

MISSING_CMDS=()
MISSING_INSTALLS=()
ALL_OK=true

# Returns 0 if every missing tool has a resolved install command
all_have_installers() {
    for i in "${!MISSING_CMDS[@]}"; do
        [ -z "${MISSING_INSTALLS[$i]}" ] && return 1
    done
    return 0
}

for entry in "${DEPS[@]}"; do
    IFS='|' read -r cmd brew_pkg apt_pkg custom <<< "$entry"
    if command -v "$cmd" >/dev/null 2>&1; then
        echo -e "  ${GREEN}✓${NC} $cmd"
    else
        echo -e "  ${RED}✗${NC} $cmd"
        ALL_OK=false
        MISSING_CMDS+=("$cmd")

        # Determine install command
        install_cmd=""
        if [ -n "$custom" ]; then
            install_cmd="$custom"
        elif [ "$OS" = "Darwin" ] && [ "$HAS_BREW" = true ] && [ -n "$brew_pkg" ]; then
            install_cmd="brew install $brew_pkg"
        elif [ "$OS" != "Darwin" ] && [ "$HAS_APT" = true ] && [ -n "$apt_pkg" ]; then
            install_cmd="sudo apt-get install -y $apt_pkg"
        fi
        MISSING_INSTALLS+=("$install_cmd")
    fi
done

# For download path, ADB is only needed if SSH isn't set up — defer check
# (we'll check SSH reachability later and skip ADB requirement if SSH works)

if [ "$ALL_OK" = false ]; then
    # On macOS without brew, offer to install it first
    if [ "$OS" = "Darwin" ] && [ "$HAS_BREW" = false ]; then
        echo ""
        warn "Homebrew not found. It's needed to install dependencies on macOS."
        echo -e "${CYAN}Install Homebrew now? (y/N)${NC}"
        read -r INSTALL_BREW
        if [ "$INSTALL_BREW" = "y" ] || [ "$INSTALL_BREW" = "Y" ]; then
            /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
            # Source brew shellenv for current session
            if [ -x /opt/homebrew/bin/brew ]; then
                eval "$(/opt/homebrew/bin/brew shellenv)"
            elif [ -x /usr/local/bin/brew ]; then
                eval "$(/usr/local/bin/brew shellenv)"
            fi
            HAS_BREW=true
            # Re-evaluate install commands now that brew is available
            for i in "${!MISSING_CMDS[@]}"; do
                if [ -z "${MISSING_INSTALLS[$i]}" ]; then
                    for entry in "${DEPS[@]}"; do
                        IFS='|' read -r cmd brew_pkg _ _ <<< "$entry"
                        if [ "$cmd" = "${MISSING_CMDS[$i]}" ] && [ -n "$brew_pkg" ]; then
                            MISSING_INSTALLS[$i]="brew install $brew_pkg"
                            break
                        fi
                    done
                fi
            done
        else
            fail "Homebrew is required on macOS. Install it from https://brew.sh"
        fi
    fi

    if ! all_have_installers; then
        echo ""
        echo "Cannot auto-install all dependencies. Please install manually:"
        for i in "${!MISSING_CMDS[@]}"; do
            if [ -z "${MISSING_INSTALLS[$i]}" ]; then
                echo "  ${MISSING_CMDS[$i]} — no auto-install available for this platform"
            fi
        done
        exit 1
    fi

    echo ""
    echo "The following will be installed:"
    for i in "${!MISSING_CMDS[@]}"; do
        echo "  ${MISSING_CMDS[$i]}  →  ${MISSING_INSTALLS[$i]}"
    done
    echo ""
    echo -e "${CYAN}Install all missing dependencies now? (y/N)${NC}"
    read -r DO_INSTALL
    if [ "$DO_INSTALL" != "y" ] && [ "$DO_INSTALL" != "Y" ]; then
        fail "Missing dependencies: ${MISSING_CMDS[*]}. Install them and re-run."
    fi

    for i in "${!MISSING_CMDS[@]}"; do
        info "Installing ${MISSING_CMDS[$i]}..."
        if ! eval "${MISSING_INSTALLS[$i]}"; then
            fail "Failed to install ${MISSING_CMDS[$i]}."
        fi
        # Source cargo env if we just installed rustup
        if [ "${MISSING_CMDS[$i]}" = "cargo" ] && [ -f "$HOME/.cargo/env" ]; then
            # shellcheck disable=SC1091
            source "$HOME/.cargo/env"
        fi
        if command -v "${MISSING_CMDS[$i]}" >/dev/null 2>&1; then
            ok "${MISSING_CMDS[$i]} installed."
        else
            fail "Failed to install ${MISSING_CMDS[$i]}."
        fi
    done
fi

# Ensure Rust cross-compilation target is installed (build path only)
if [ "$BUILD_CHOICE" = "2" ]; then
    if ! rustup target list --installed 2>/dev/null | grep -q aarch64-unknown-linux-musl; then
        info "Adding Rust cross-compilation target..."
        rustup target add aarch64-unknown-linux-musl
    fi
fi
ok "All prerequisites found."

# ── Helper: SHA-256 ──────────────────────────────────────────────────
sha256() {
    printf '%s' "$1" | python3 -c 'import hashlib,sys; print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())'
}

sha256_file() {
    python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$1"
}

upper() {
    echo "$1" | tr '[:lower:]' '[:upper:]'
}

# ── Helper: ubus JSON-RPC call ───────────────────────────────────────
ubus_call() {
    local session="$1" object="$2" method="$3" params="$4"
    local ts
    ts=$(date +%s)
    curl -sf "http://$GATEWAY/ubus/?t=$ts" \
        -H 'Content-Type: application/json' \
        -d "[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"call\",\"params\":[\"$session\",\"$object\",\"$method\",$params]}]"
}

# ── Helper: timeout (macOS-compatible) ───────────────────────────────
# macOS doesn't have GNU timeout; use a portable fallback
wait_with_timeout() {
    local secs="$1"; shift
    if command -v timeout >/dev/null 2>&1; then
        timeout "$secs" "$@"
    else
        # Portable fallback: run in background with a timer
        "$@" &
        local pid=$!
        local i=0
        while [ "$i" -lt "$secs" ]; do
            if ! kill -0 "$pid" 2>/dev/null; then
                wait "$pid"
                return $?
            fi
            sleep 1
            i=$((i + 1))
        done
        kill "$pid" 2>/dev/null
        return 124
    fi
}

# ── Transport detection ──────────────────────────────────────────────
SSH_CMD="ssh -p $SSH_PORT -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=3 root@$GATEWAY"
USE_SSH=false

if [ "$FORCE_ADB" != true ] && $SSH_CMD "echo ok" >/dev/null 2>&1; then
    USE_SSH=true
    ok "SSH reachable — using wireless deploy."
else
    info "Using authorized USB ADB (SSH unavailable or --adb selected)."

    # ── Steps 1-2: Enable ADB + connect ──────────────────────────────────
    if adb get-state 2>/dev/null | grep -qx device; then
        ok "ADB already connected, skipping web auth."
    else
        [ "$FACTORY_PASSWORD" != true ] || fail "Authorize USB ADB first, or connect SSH before using --factory-password."
        info "Authenticating with router web interface..."

        ANON_SESSION="00000000000000000000000000000000"

        # Get salt (with safe JSON extraction)
        # || true prevents set -e from killing the script before we can show a helpful error
        SALT_RESP=$(ubus_call "$ANON_SESSION" "zwrt_web" "web_login_info" '{}') || true
        SALT=$(echo "$SALT_RESP" | python3 -c '
import sys, json
try:
    print(json.load(sys.stdin)[0]["result"][1]["zte_web_sault"])
except Exception:
    pass
' 2>/dev/null)

        if [ -z "$SALT" ]; then
            fail "Failed to extract salt. Is the router reachable at $GATEWAY?"
        fi

        # Hash password
        PASS_HASH=$(upper "$(sha256 "$ROUTER_PASSWORD")")
        LOGIN_HASH=$(upper "$(sha256 "${PASS_HASH}${SALT}")")

        # Login (with safe JSON extraction)
        LOGIN_RESP=$(ubus_call "$ANON_SESSION" "zwrt_web" "web_login" "{\"password\":\"$LOGIN_HASH\"}") || true
        SESSION=$(echo "$LOGIN_RESP" | python3 -c '
import sys, json
try:
    print(json.load(sys.stdin)[0]["result"][1]["ubus_rpc_session"])
except Exception:
    pass
' 2>/dev/null)

        if [ -z "$SESSION" ] || [ "$SESSION" = "null" ]; then
            fail "Login failed. Check your router password."
        fi
        ok "Logged in to router."

        # Set USB mode to debug
        info "Enabling ADB (USB debug mode)..."
        if ! ubus_call "$SESSION" "zwrt_bsp.usb" "set" '{"mode":"debug"}' >/dev/null; then
            fail "Failed to enable USB debug mode. Session may have expired."
        fi
        ok "USB debug mode enabled."

        # Wait for ADB device
        info "Waiting for ADB device (plug USB cable if not connected)..."
        if ! wait_with_timeout 30 adb wait-for-device 2>/dev/null; then
            fail "ADB device not found after 30s. Check USB connection."
        fi
        ok "ADB device connected."
    fi
    # Pin this device for every command, including forward cleanup.
    ANDROID_SERIAL=$(adb get-serialno)
    [ -n "$ANDROID_SERIAL" ] && [ "$ANDROID_SERIAL" != unknown ] || fail "Cannot identify one ADB device. Use --serial."
    export ANDROID_SERIAL
fi

# ── Helper: remote command / push ────────────────────────────────────
rcmd() {
    if [ "$USE_SSH" = true ]; then
        $SSH_CMD "$@"
    else
        # B31's old adbd reports host exit 0 even when its shell command fails.
        # Carry the real status separately; keep raw command output private.
        local output status marker="__U60_RC_$$_${RANDOM}__"
        output=$(adb shell "(
$*
)
status=\$?
printf '\\n${marker}%s\\n' \"\$status\"" | tr -d '\r') || return 1
        status="${output##*$'\n'$marker}"
        case "$status" in ''|*[!0-9]*) echo "Missing remote command status" >&2; return 1 ;; esac
        printf '%s' "${output%$'\n'$marker*}"
        return "$status"
    fi
}

rpush() {
    local src="$1" dst="$2"
    if [ "$USE_SSH" = true ]; then
        cat "$src" | $SSH_CMD "umask 077; cat > '$dst' && chmod 700 '$dst'"
    else
        adb push "$src" "$dst" && rcmd "chmod 700 '$dst'"
    fi
}

# ── Step 3: Get zte-agent binary ─────────────────────────────────────
if [ "$BUILD_CHOICE" = "1" ]; then
    info "Downloading zte-agent from GitHub releases..."
    mkdir -p "$(dirname "$BINARY")"
    if ! curl -sfL "$DOWNLOAD_URL" -o "$BINARY"; then
        fail "Download failed. Check your internet connection or try: $DOWNLOAD_URL"
    fi
    # Verify download
    FILE_SIZE=$(wc -c < "$BINARY" | tr -d ' ')
    if [ "$FILE_SIZE" -lt 1000 ]; then
        rm -f "$BINARY"
        fail "Downloaded file is too small ($FILE_SIZE bytes) — likely not a valid binary. Check the release page."
    fi
    chmod +x "$BINARY"
    ok "Downloaded zte-agent ($FILE_SIZE bytes)."
else
    info "Building zte-agent (this may take a few minutes on first run)..."
    cargo build --release --target "$TARGET" -p zte-agent
    ok "Build complete."
fi

# Stage and verify before stopping the current panel.
info "Checking device state and staging the local installation..."
rcmd 'test -d /data && command -v flock >/dev/null && command -v sha256sum >/dev/null'
rcmd 'test ! -e /etc/init.d/zte-agent' || fail "An init-managed agent exists; use its service manager instead."
# Relay and Mihomo may stay on: the new agent rolls back what the old one
# left and reconnects them (relay networks connected with Remember on).
if ! rcmd 'test ! -e /sys/class/net/u60sta0 && test ! -e /sys/class/net/u60mh0'; then
    info "Relay or Mihomo is on; the new agent turns them back on after a short interruption."
fi
STAGE="/data/zte-agent-install-$(date +%Y%m%d-%H%M%S)-$$"
rcmd "umask 077; mkdir '$STAGE'; mkdir -p /data/local/tmp"
ok "Installation backup directory: $STAGE"

LOCAL_SHA=$(sha256_file "$BINARY")
rpush "$BINARY" "$STAGE/agent.new"
REMOTE_SHA=$(rcmd "sha256sum '$STAGE/agent.new'" | awk '{print $1}')
[ "$LOCAL_SHA" = "$REMOTE_SHA" ] || fail "Staged binary checksum mismatch; current agent is untouched."

# The A/B update supervisor and its first slot contents are staged and checked
# like the agent: nothing on the router changes before install.sh runs.
rpush "$SUPERVISOR_LOCAL" "$STAGE/supervisor.new"
REMOTE_VERSION=$(rcmd "'$STAGE/agent.new' --version" | awk 'NR == 1 {print $2}')
if [[ ! "$REMOTE_VERSION" =~ ^[0-9A-Za-z][0-9A-Za-z.+-]{0,63}$ ]]; then
    fail "The staged agent reported an invalid build version; current agent is untouched."
fi
rcmd "'$STAGE/agent.new' ota-probe '$REMOTE_VERSION' >/dev/null 2>&1" \
    || fail "The staged agent failed its on-device compatibility probe; current agent is untouched."

if [ "$FACTORY_PASSWORD" = true ]; then
    python3 scripts/install-agent.py launcher "$WORKDIR/start.sh"
else
    printf '%s' "$AGENT_PASSWORD" | python3 scripts/install-agent.py launcher "$WORKDIR/start.sh" override
fi
rpush "$WORKDIR/start.sh" "$STAGE/start.sh"
rcmd "set -e; sh -n '$STAGE/start.sh'; cp -p /etc/rc.local '$STAGE/rc.local.before'; chmod 600 '$STAGE/rc.local.before'"
# rcmd intentionally adds no newline; preserve the boot file byte for byte.
rcmd "cat '$STAGE/rc.local.before'" >"$WORKDIR/rc.local.before"
RC_SHA=$(sha256_file "$WORKDIR/rc.local.before")
[ "$RC_SHA" = "$(rcmd "sha256sum '$STAGE/rc.local.before'" | awk '{print $1}')" ] \
    || fail "Could not preserve rc.local exactly; current agent is untouched."
python3 scripts/install-agent.py boot "$WORKDIR/rc.local.new" "$WORKDIR/rc.local.before"
rpush "$WORKDIR/rc.local.new" "$STAGE/rc.local.new"
rcmd "sh -n '$STAGE/rc.local.new'; [ \"\$(sha256sum /etc/rc.local | awk '{print \$1}')\" = '$RC_SHA' ]" \
    || fail "rc.local changed during staging; retry without overwriting it."
# The install and boot launcher share exact process-identity checks and the
# same lock. Stop only verified mains, never relay callbacks/CLI helpers.
python3 scripts/install-agent.py install "$WORKDIR/install.sh"
rpush "$WORKDIR/install.sh" "$STAGE/install.sh"
rcmd "sh '$STAGE/install.sh' '$STAGE' '$RC_SHA' '$LOCAL_SHA' '$REMOTE_VERSION'"
ok "Installed $REMOTE_SLOT (A/B slots and update supervisor, linked at $REMOTE_BIN); verifying before enabling the boot hook."

if [ "$USE_SSH" = true ]; then
    VERIFY_URL="http://$GATEWAY:$AGENT_PORT"
else
    FORWARD_PORT=$(adb forward tcp:0 tcp:$AGENT_PORT | tr -d '\r\n')
    case "$FORWARD_PORT" in ''|*[!0-9]*) fail "Cannot allocate verification port." ;; esac
    VERIFY_URL="http://127.0.0.1:$FORWARD_PORT"
fi
READY=false
for attempt in $(seq 1 30); do
    if curl --noproxy '*' -fsSL --max-time 2 "$VERIFY_URL/console/" >"$WORKDIR/console.html" 2>/dev/null \
        && grep -q 'OpenU60' "$WORKDIR/console.html"; then
        READY=true
        break
    fi
    sleep 1
done
[ "$READY" = true ] || fail "Console not ready; boot hook is unchanged. Backup: $STAGE"
[ "$(curl --noproxy '*' -sS --max-time 5 -o /dev/null -w '%{http_code}' "$VERIFY_URL/api/device")" = 401 ] \
    || fail "Unauthenticated API was not rejected; boot hook is unchanged."
rcmd "p=\$(cat /tmp/zte-agent-run/pid); [ \"\$(readlink /proc/\$p/exe)\" = '$REMOTE_SLOT' ]" \
    || fail "The responding agent is not the process the supervisor started from OTA slot A."
if [ "$FACTORY_PASSWORD" = true ]; then
    rcmd "grep -q '^\[auth\] using factory admin password' /tmp/zte-agent.log" \
        || fail "The installed agent did not load the factory admin verifier."
    ok "Panel ready; factory admin verifier loaded and unauthenticated API rejected."
else
    printf '%s' "$AGENT_PASSWORD" | python3 -c 'import json,sys; json.dump({"password":sys.stdin.read()},sys.stdout)' >"$WORKDIR/login.json"
    chmod 600 "$WORKDIR/login.json"
    curl --noproxy '*' -fsS --max-time 10 "$VERIFY_URL/api/auth/login" -H 'Content-Type: application/json' \
        --data-binary "@$WORKDIR/login.json" >"$WORKDIR/login-response.json"
    python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert d.get("ok") and d.get("data",{}).get("token")' "$WORKDIR/login-response.json"
    ok "Agent authenticated with the explicit password."
fi

rcmd "set -e
[ \"\$(sha256sum /etc/rc.local | awk '{print \$1}')\" = '$RC_SHA' ]
cp -p /etc/rc.local /etc/rc.local.zte-agent-new
cat '$STAGE/rc.local.new' >/etc/rc.local.zte-agent-new
sh -n /etc/rc.local.zte-agent-new
mv /etc/rc.local.zte-agent-new /etc/rc.local
sync"
ok "Boot hook enabled in rc.local; unrelated boot commands are preserved."
# Keep only rollback inputs, not upload payloads or install helpers.
rcmd "rm -f '$STAGE/agent.new' '$STAGE/agent.ready' '$STAGE/supervisor.new' '$STAGE/supervisor.ready' '$STAGE/start.sh' '$STAGE/start.ready' '$STAGE/rc.local.new' '$STAGE/install.sh' '$STAGE/backups.ready'"

# ── Step 9: Optional SSH setup ──────────────────────────────────────
if [ "$SKIP_SSH" = true ]; then
    ok "SSH setup skipped."
elif [ "$USE_SSH" = true ]; then
    ok "SSH already configured, skipping SSH setup."
else
echo ""
echo -e "${CYAN}Set up SSH for wireless deploys? (y/N)${NC}"
read -r SETUP_SSH

if [ "$SETUP_SSH" = "y" ] || [ "$SETUP_SSH" = "Y" ]; then
    SSH_KEY="$HOME/.ssh/id_ed25519"
    SSH_PUB="$SSH_KEY.pub"

    # Generate SSH key if needed
    if [ ! -f "$SSH_KEY" ]; then
        info "Generating SSH key..."
        ssh-keygen -t ed25519 -f "$SSH_KEY" -N ""
        ok "SSH key generated at $SSH_KEY."
    else
        ok "SSH key already exists at $SSH_KEY."
    fi

    # Install dropbear on device
    info "Setting up dropbear SSH server..."

    # Check if dropbear is already present
    if adb shell "test -x /usr/sbin/dropbear" 2>/dev/null; then
        ok "Dropbear already installed."
    else
        info "Downloading dropbear for aarch64..."
        DROPBEAR_URL="https://downloads.openwrt.org/releases/23.05.4/targets/armsr/armv8/packages/dropbear_2022.83-1_aarch64_generic.ipk"
        TMPDIR=$(mktemp -d)
        curl -sfL "$DROPBEAR_URL" -o "$TMPDIR/dropbear.ipk" || fail "Failed to download dropbear."
        adb push "$TMPDIR/dropbear.ipk" /tmp/dropbear.ipk
        adb shell "opkg install /tmp/dropbear.ipk 2>/dev/null || true"
        adb shell "rm -f /tmp/dropbear.ipk"
        rm -rf "$TMPDIR"
        ok "Dropbear installed."
    fi

    # Set up authorized_keys
    info "Configuring SSH keys..."
    adb shell "mkdir -p /etc/dropbear && chmod 700 /etc/dropbear"
    PUBKEY=$(cat "$SSH_PUB")
    if adb shell "grep -qF '$PUBKEY' /etc/dropbear/authorized_keys 2>/dev/null"; then
        ok "SSH key already authorized."
    else
        adb shell "echo '$PUBKEY' >> /etc/dropbear/authorized_keys"
        ok "SSH key added to authorized_keys."
    fi
    adb shell "chmod 600 /etc/dropbear/authorized_keys"

    # Create dropbear startup script
    DROPBEAR_STARTUP=/data/local/tmp/start_dropbear.sh
    if adb shell "test -x $DROPBEAR_STARTUP" 2>/dev/null; then
        ok "Dropbear startup script already exists."
    else
        adb shell "cat > $DROPBEAR_STARTUP" <<'DBBOOT'
#!/bin/sh
/usr/sbin/dropbear -p 2222 -R
DBBOOT
        adb shell "chmod +x $DROPBEAR_STARTUP"
        ok "Dropbear startup script created."
    fi

    # Add to rc.local if not already there
    DB_RC_LINE="sh $DROPBEAR_STARTUP"
    if adb shell "grep -qF '$DB_RC_LINE' /etc/rc.local 2>/dev/null"; then
        ok "Dropbear rc.local entry already configured."
    else
        adb shell "grep -q '^exit 0' /etc/rc.local \
            && sed -i '/^exit 0/i $DB_RC_LINE' /etc/rc.local \
            || echo '$DB_RC_LINE' >> /etc/rc.local"
        ok "Added dropbear to /etc/rc.local."
    fi

    # Start dropbear now
    if adb shell "pidof dropbear" >/dev/null 2>&1; then
        ok "Dropbear already running on port $SSH_PORT."
    else
        info "Starting dropbear..."
        adb shell "sh $DROPBEAR_STARTUP"
        ok "Dropbear started on port $SSH_PORT."
    fi

    # Verify SSH
    info "Verifying SSH connection..."
    if ssh -p "$SSH_PORT" -o StrictHostKeyChecking=no -o ConnectTimeout=5 "root@$GATEWAY" "echo ok" >/dev/null 2>&1; then
        ok "SSH connection verified."
    else
        warn "SSH connection could not be verified. You may need to reboot the router."
    fi

    echo ""
    ok "SSH is configured. You can now use ./deploy.sh for wireless deploys:"
    echo "    ssh -p $SSH_PORT root@$GATEWAY"
fi
fi

# ── Done ─────────────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}Setup complete!${NC}"
echo ""
echo "  Agent API:  http://$GATEWAY:$AGENT_PORT"
echo "  Console:    http://$GATEWAY:$AGENT_PORT/console/"
if [ "$FACTORY_PASSWORD" = true ]; then
    echo "  Login:      official router admin password (restart agent after changing it)"
    echo "  Update:     ./setup.sh --build --adb --factory-password --no-ssh"
else
    echo "  Login:      your explicit agent password (not displayed)"
fi
echo "  Auto-start: rc.local -> $STARTUP_SCRIPT"
echo "  Backup:     $STAGE"
echo ""
echo "  Point the iOS/Android companion app at http://$GATEWAY:$AGENT_PORT"
