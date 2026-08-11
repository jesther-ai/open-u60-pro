#!/bin/bash
set -e

# ── Colors ───────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()  { echo -e "${CYAN}[*]${NC} $1"; }
ok()    { echo -e "${GREEN}[+]${NC} $1"; }
warn()  { echo -e "${YELLOW}[!]${NC} $1"; }
fail()  { echo -e "${RED}[-]${NC} $1" >&2; exit 1; }

# ── Password input ──────────────────────────────────────────────────
if [ $# -ge 2 ]; then
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

GATEWAY=192.168.0.1
AGENT_PORT=9090
SSH_PORT=2222
TARGET=aarch64-unknown-linux-musl
BINARY=target/$TARGET/release/zte-agent
REMOTE_BIN=/data/zte-agent
STARTUP_SCRIPT=/data/local/tmp/start_zte_agent.sh
BINARY_CHANGED=false
DOWNLOAD_URL="https://github.com/jesther-ai/open-u60-pro/releases/latest/download/zte-agent"

# ── Binary source menu ──────────────────────────────────────────────
echo ""
echo -e "${CYAN}How would you like to get the zte-agent binary?${NC}"
echo "  1) Download pre-built from GitHub (recommended — no dev tools needed)"
echo "  2) Build from source (requires Rust toolchain)"
echo ""
echo -n "Choice [1]: "
read -r BUILD_CHOICE
BUILD_CHOICE="${BUILD_CHOICE:-1}"

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
    echo -n "$1" | shasum -a 256 2>/dev/null | awk '{print $1}' \
        || echo -n "$1" | sha256sum | awk '{print $1}'
}

sha256_file() {
    shasum -a 256 "$1" 2>/dev/null | awk '{print $1}' \
        || sha256sum "$1" | awk '{print $1}'
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
SSH_CMD="ssh -p $SSH_PORT -o StrictHostKeyChecking=no -o ConnectTimeout=3 root@$GATEWAY"
USE_SSH=false

if $SSH_CMD "echo ok" >/dev/null 2>&1; then
    USE_SSH=true
    ok "SSH reachable — using wireless deploy."
else
    warn "SSH not reachable — falling back to ADB."

    # ── Steps 1-2: Enable ADB + connect ──────────────────────────────────
    if adb devices 2>/dev/null | grep -qw device; then
        ok "ADB already connected, skipping web auth."
    else
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
        ok "Logged in to router (session: ${SESSION:0:8}...)."

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
fi

# ── Helper: remote command / push ────────────────────────────────────
rcmd() {
    if [ "$USE_SSH" = true ]; then
        $SSH_CMD "$@"
    else
        adb shell "$@"
    fi
}

rpush() {
    local src="$1" dst="$2"
    if [ "$USE_SSH" = true ]; then
        cat "$src" | $SSH_CMD "cat > $dst && chmod +x $dst"
    else
        adb push "$src" "$dst" && adb shell "chmod +x $dst"
    fi
}

# Remote test that works over both transports.
#
# `adb shell` does not propagate the remote exit code (it reports the exit
# status of adb itself), so `if rcmd "grep -q ..."` was always true and the
# script happily reported files as present when they were not. Echo a marker
# and inspect the output instead.
rtest() {
    local out
    out=$(rcmd "if $1; then echo __RTEST_YES__; else echo __RTEST_NO__; fi" 2>/dev/null)
    case "$out" in
        *__RTEST_YES__*) return 0 ;;
        *) return 1 ;;
    esac
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

# ── Step 4: Push binary ─────────────────────────────────────────────
info "Checking zte-agent binary..."
LOCAL_SHA=$(sha256_file "$BINARY")
REMOTE_SHA=$(rcmd "sha256sum $REMOTE_BIN 2>/dev/null" | awk '{print $1}')
if [ "$LOCAL_SHA" = "$REMOTE_SHA" ]; then
    ok "Binary unchanged, skipping push."
else
    info "Stopping running agent before push..."
    rcmd "killall zte-agent 2>/dev/null; sleep 1"
    info "Pushing zte-agent to device..."
    rpush "$BINARY" "$REMOTE_BIN"
    BINARY_CHANGED=true
    ok "Binary deployed to $REMOTE_BIN."
fi

# ── Step 5: Create startup script ───────────────────────────────────
# Escape single quotes for safe embedding in sh single-quoted string
SAFE_PASSWORD=$(printf '%s' "$AGENT_PASSWORD" | sed "s/'/'\\\\''/g")

if rtest "grep -qF '${SAFE_PASSWORD}' $STARTUP_SCRIPT 2>/dev/null"; then
    ok "Startup script already up to date."
else
    info "Creating startup script..."
    cat > /tmp/start_zte_agent.sh <<BOOT
#!/bin/sh
export ZTE_AGENT_PASSWORD='${SAFE_PASSWORD}'
nohup /data/zte-agent >/tmp/zte-agent.log 2>&1 &
BOOT
    rpush /tmp/start_zte_agent.sh "$STARTUP_SCRIPT"
    rm /tmp/start_zte_agent.sh
    ok "Startup script created at $STARTUP_SCRIPT."
fi

# ── Step 6: Update rc.local for boot persistence ────────────────────
info "Configuring auto-start on boot..."
RC_LINE="sh $STARTUP_SCRIPT"
if rtest "grep -qF '$RC_LINE' /etc/rc.local 2>/dev/null"; then
    ok "rc.local already configured."
else
    rcmd "grep -q '^exit 0' /etc/rc.local \
        && sed -i '/^exit 0/i $RC_LINE' /etc/rc.local \
        || echo '$RC_LINE' >> /etc/rc.local"
    ok "Added zte-agent to /etc/rc.local."
fi

# ── Step 7: Start agent ─────────────────────────────────────────────
info "Checking agent status..."
AGENT_RUNNING=false
curl -sf "http://$GATEWAY:$AGENT_PORT/api/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"password\":\"$AGENT_PASSWORD\"}" >/dev/null 2>&1 && AGENT_RUNNING=true

if [ "$BINARY_CHANGED" = true ] || [ "$AGENT_RUNNING" = false ]; then
    info "Starting zte-agent..."
    rcmd "killall zte-agent 2>/dev/null; true"
    sleep 1
    rcmd "sh $STARTUP_SCRIPT"
    ok "Agent (re)started."
else
    ok "Agent already running with current binary, skipping restart."
fi

# ── Step 8: Verify ──────────────────────────────────────────────────
info "Verifying agent is running..."
sleep 2

if [ "$USE_SSH" = true ]; then
    VERIFY_URL="http://$GATEWAY:$AGENT_PORT/api/auth/login"
else
    adb forward tcp:19090 tcp:$AGENT_PORT
    VERIFY_URL="http://127.0.0.1:19090/api/auth/login"
fi

TOKEN=$(curl -sf "$VERIFY_URL" \
    -H 'Content-Type: application/json' \
    -d "{\"password\":\"$AGENT_PASSWORD\"}" \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["token"])')

if [ "$USE_SSH" != true ]; then
    adb forward --remove tcp:19090 2>/dev/null || true
fi

if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
    fail "Agent started but login verification failed."
fi
ok "Agent is running and authenticated."

# ── Step 9: Optional SSH setup ──────────────────────────────────────
if [ "$USE_SSH" = true ]; then
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
    if rtest "test -x /usr/sbin/dropbear"; then
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
    if rtest "grep -qF '$PUBKEY' /etc/dropbear/authorized_keys 2>/dev/null"; then
        ok "SSH key already authorized."
    else
        adb shell "echo '$PUBKEY' >> /etc/dropbear/authorized_keys"
        ok "SSH key added to authorized_keys."
    fi
    adb shell "chmod 600 /etc/dropbear/authorized_keys"

    # Create dropbear startup script
    DROPBEAR_STARTUP=/data/local/tmp/start_dropbear.sh
    if rtest "test -x $DROPBEAR_STARTUP"; then
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
    if rtest "grep -qF '$DB_RC_LINE' /etc/rc.local 2>/dev/null"; then
        ok "Dropbear rc.local entry already configured."
    else
        adb shell "grep -q '^exit 0' /etc/rc.local \
            && sed -i '/^exit 0/i $DB_RC_LINE' /etc/rc.local \
            || echo '$DB_RC_LINE' >> /etc/rc.local"
        ok "Added dropbear to /etc/rc.local."
    fi

    # Start dropbear now
    if rtest "pidof dropbear >/dev/null 2>&1"; then
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
echo "  Password:   $AGENT_PASSWORD"
echo "  Deploy:     ./deploy.sh $AGENT_PASSWORD"
echo ""
echo "  Point the iOS/Android companion app at http://$GATEWAY:$AGENT_PORT"
