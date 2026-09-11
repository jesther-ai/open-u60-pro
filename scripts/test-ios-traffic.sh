#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
test_dir=$(mktemp -d "${TMPDIR:-/tmp}/u60-traffic-tests.XXXXXX")
trap 'rm -rf "$test_dir"' EXIT
models=mobile/ios/OpenU60/Core/Models
swiftc -module-cache-path "$test_dir/cache" \
    "$models/DeviceModels.swift" "$models/RouterSettingsModels.swift" \
    mobile/ios/Tests/TrafficSpeedTests.swift -o "$test_dir/parser-tests"
"$test_dir/parser-tests"
swiftc -module-cache-path "$test_dir/cache" \
    "$models/DeviceModels.swift" "$models/RouterSettingsModels.swift" "$models/SignalModels.swift" \
    mobile/ios/OpenU60/Core/Networking/AgentError.swift \
    mobile/ios/OpenU60/Core/Concurrency/PollingLoop.swift \
    mobile/ios/OpenU60/Features/Dashboard/DashboardViewModel.swift \
    mobile/ios/Tests/DashboardTrafficTests.swift -o "$test_dir/dashboard-tests"
"$test_dir/dashboard-tests"
swiftc -module-cache-path "$test_dir/cache" \
    "$models/DeviceModels.swift" "$models/RouterSettingsModels.swift" \
    mobile/ios/Tests/BatteryStatusTests.swift -o "$test_dir/battery-tests"
"$test_dir/battery-tests"
