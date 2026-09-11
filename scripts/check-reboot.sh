#!/bin/bash
# Check if U60 Pro has rebooted successfully

HOST="192.168.0.1"
PORT="2222"
TIMEOUT=120
INTERVAL=5
ELAPSED=0

echo "Waiting for U60 Pro to come back online..."

# Wait for ping
while [ $ELAPSED -lt $TIMEOUT ]; do
    if ping -c 1 -W 2 "$HOST" &>/dev/null; then
        echo "[${ELAPSED}s] Ping OK — device is reachable"
        break
    fi
    echo "[${ELAPSED}s] No ping response..."
    sleep $INTERVAL
    ELAPSED=$((ELAPSED + INTERVAL))
done

if [ $ELAPSED -ge $TIMEOUT ]; then
    echo "FAIL: Device did not respond to ping within ${TIMEOUT}s"
    exit 1
fi

# Wait for SSH
while [ $ELAPSED -lt $TIMEOUT ]; do
    if ssh -p "$PORT" -o ConnectTimeout=3 -o StrictHostKeyChecking=no root@"$HOST" "echo ok" &>/dev/null; then
        echo "[${ELAPSED}s] SSH OK"
        break
    fi
    echo "[${ELAPSED}s] SSH not ready..."
    sleep $INTERVAL
    ELAPSED=$((ELAPSED + INTERVAL))
done

if [ $ELAPSED -ge $TIMEOUT ]; then
    echo "FAIL: SSH did not come up within ${TIMEOUT}s"
    exit 1
fi

# Grab uptime and key info
echo ""
echo "=== U60 Pro Reboot Successful ==="
ssh -p "$PORT" -o ConnectTimeout=5 root@"$HOST" "
echo \"Uptime: \$(cat /proc/uptime | awk '{print \$1}')s\"
echo \"Load:   \$(cat /proc/loadavg)\"
echo \"Memory: \$(free | grep Mem | awk '{printf \"%dMB used / %dMB total\", \$3/1024, \$2/1024}')\"
echo \"Agent:  \$(pidof zte-agent >/dev/null && echo 'running' || echo 'NOT running')\"
echo \"Tailscale: \$(pidof tailscaled >/dev/null && echo 'running' || echo 'not running')\"
"
