#!/bin/bash
set -e

PASSWORD="${1:?Usage: ./deploy.sh <agent-password>}"
if [[ "$PASSWORD" == *$'\n'* || "$PASSWORD" == *$'\r'* ]]; then
    echo "Agent password cannot contain a line break." >&2
    exit 1
fi
DEVICE="192.168.0.1"
SSH_PORT=2222
TARGET=aarch64-unknown-linux-musl
BINARY=target/$TARGET/release/zte-agent
OTA_ROOT=/data/open-u60/update
REMOTE_SLOT=$OTA_ROOT/slot-a/zte-agent
REMOTE_BIN=/data/zte-agent
SUPERVISOR=/data/local/tmp/zte-agent-supervisor.sh
SUPERVISOR_LOCAL=scripts/router/zte-agent-supervisor.sh
STARTUP_SCRIPT=/data/local/tmp/start_zte_agent.sh
SSH="ssh -p $SSH_PORT -o StrictHostKeyChecking=no root@$DEVICE"

# 1. Build
echo "Building..."
cargo build --release --target $TARGET -p zte-agent

# 2. Size comparison
LOCAL_SIZE=$(wc -c < "$BINARY")
REMOTE_SIZE=$($SSH "wc -c < $REMOTE_SLOT 2>/dev/null" 2>/dev/null || echo 0)
DIFF=$((LOCAL_SIZE - REMOTE_SIZE))
if [ "$REMOTE_SIZE" -eq 0 ]; then
    echo "Binary size: $LOCAL_SIZE bytes (first deploy)"
else
    echo "Binary size: $REMOTE_SIZE -> $LOCAL_SIZE bytes ($( [ $DIFF -ge 0 ] && echo '+' )${DIFF} bytes)"
fi

# 3. Deploy binary
echo "Deploying binary..."
$SSH "killall zte-agent 2>/dev/null; sleep 1"
$SSH "mkdir -p $OTA_ROOT/slot-a $OTA_ROOT/slot-b $OTA_ROOT/last-stable && chmod 700 $OTA_ROOT $OTA_ROOT/slot-a $OTA_ROOT/slot-b $OTA_ROOT/last-stable"
cat "$BINARY" | $SSH "cat > $REMOTE_SLOT && chmod 755 $REMOTE_SLOT"
cat "$SUPERVISOR_LOCAL" | $SSH "cat > $SUPERVISOR && chmod 755 $SUPERVISOR"
$SSH "printf 'a\n' > $OTA_ROOT/active; chmod 600 $OTA_ROOT/active; rm -f $OTA_ROOT/pending $OTA_ROOT/state.json; ln -sfn $REMOTE_SLOT $REMOTE_BIN.next; mv -f $REMOTE_BIN.next $REMOTE_BIN"

# 4. Update password in startup script
echo "Updating password..."
SAFE_PASSWORD=$(printf '%s' "$PASSWORD" | sed "s/'/'\\\\''/g")
$SSH "cat > $STARTUP_SCRIPT" <<BOOT
#!/bin/sh
export ZTE_AGENT_PASSWORD='${SAFE_PASSWORD}'
export ZTE_OTA_ROOT='$OTA_ROOT'
export ZTE_OTA_SUPERVISOR='$SUPERVISOR'
export ZTE_OTA_LEGACY_LINK='$REMOTE_BIN'
nohup $SUPERVISOR >/tmp/zte-agent.log 2>&1 &
BOOT
$SSH "chmod 700 $STARTUP_SCRIPT"

# 5. Restart
echo "Restarting agent..."
$SSH "sh $STARTUP_SCRIPT"

# 6. Verify
echo "Verifying..."
sleep 2
TOKEN=$(curl -sf http://$DEVICE:9090/api/auth/login -H 'Content-Type: application/json' -d "{\"password\":\"$PASSWORD\"}" | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["token"])')
curl -sf http://$DEVICE:9090/api/device -H "Authorization: Bearer $TOKEN" > /dev/null
echo "Deploy successful!"
