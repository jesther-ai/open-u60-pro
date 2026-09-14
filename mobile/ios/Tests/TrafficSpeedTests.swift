import Foundation

// Run from the repository root: sh scripts/test-ios-traffic.sh
@main
struct TrafficSpeedTests {
    static func main() {
        let old = TrafficStats(rxBytes: 10, txBytes: 20,
                               timestamp: Date(timeIntervalSince1970: 0), source: "wwandst")
        let live = DeviceParser.parseWwandstTraffic([
            "real_rx_bytes": 349032201, "real_tx_bytes": 399696948,
            "real_rx_speed": 52_500_000, "real_tx_speed": 5_000_000
        ])!
        let speed = DeviceParser.computeSpeed(previous: old, current: live)
        precondition(speed.downloadBytesPerSec == 52_500_000)
        precondition(speed.uploadBytesPerSec == 5_000_000)
        precondition(DeviceParser.speedComponents(speed.downloadBytesPerSec).number == 420)
        precondition(DeviceParser.speedComponents(speed.uploadBytesPerSec).number == 40)

        // A delayed counter increase must not override an explicit idle sample.
        let idle = DeviceParser.parseWwandstTraffic([
            "real_rx_bytes": "349032301", "real_tx_bytes": "399697048",
            "real_rx_speed": "0", "real_tx_speed": "0"
        ])!
        precondition(DeviceParser.computeSpeed(previous: live, current: idle) == .zero)
        precondition(DeviceParser.parseWwandstTraffic(["real_rx_bytes": 1]) == nil)

        // Older firmware with counters only still supports deltas, without source-switch spikes.
        var counter = DeviceParser.parseWwandstTraffic([
            "real_rx_bytes": 2010, "real_tx_bytes": 1020
        ])!
        counter.timestamp = Date(timeIntervalSince1970: 2)
        let delta = DeviceParser.computeSpeed(previous: old, current: counter)
        precondition(delta.downloadBytesPerSec == 1000 && delta.uploadBytesPerSec == 500)
        counter.source = "agent"
        precondition(DeviceParser.computeSpeed(previous: old, current: counter) == .zero)
        counter.source = "wwandst"
        counter.rxBytes = 0
        counter.txBytes = 0
        precondition(DeviceParser.computeSpeed(previous: old, current: counter) == .zero)
        print("Traffic speed regression checks passed")
    }
}
