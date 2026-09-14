import Foundation

/// A cancellation-correct repeating driver for the app's polling screens.
///
/// Replaces the hand-rolled `pollTask = Task { while !Task.isCancelled { … } }` pattern that
/// was duplicated across the view models with three different lifetime conventions. Two
/// properties matter:
///
/// 1. **No retain cycle.** The internal `Task` captures only the tick closure, never the loop,
///    so an owner that drops its `PollingLoop` deallocates it immediately and `deinit` cancels
///    the task. The tick closure itself **must capture its owner weakly** — see `start`.
/// 2. **Backoff.** A tick that reports `.failure` grows the delay geometrically up to
///    `maxDelay`, so an unreachable router costs a request every 30 seconds instead of a
///    request storm every 2.
@MainActor
final class PollingLoop {

    /// What one tick reports back, which decides the next delay.
    enum Outcome {
        /// The tick reached the agent. Resets backoff to the base interval.
        case success
        /// The tick failed. Grows the delay geometrically, capped at `maxDelay`.
        case failure
    }

    enum Schedule {
        case afterCompletion
        /// Keep request start times on cadence; skip overruns without catch-up bursts.
        case fixedInterval
    }

    private var task: Task<Void, Never>?
    private let maxDelay: Duration
    private let maxDoublings: Int

    /// - Parameters:
    ///   - maxDelay: ceiling for the backoff delay after repeated failures.
    ///   - maxDoublings: how many times the base interval may double before hitting `maxDelay`.
    init(maxDelay: Duration = .seconds(30), maxDoublings: Int = 4) {
        self.maxDelay = maxDelay
        self.maxDoublings = maxDoublings
    }

    var isRunning: Bool { task != nil }

    /// Starts (restarting if already running) a loop that runs `tick` immediately and then once
    /// per `interval`, backing off geometrically while `tick` reports `.failure`.
    /// By default the interval follows completion; `.fixedInterval` includes time spent working.
    ///
    /// - Important: `tick` must capture its owner **weakly**. Capturing `self` strongly keeps the
    ///   owner alive for as long as the loop runs, which is exactly the leak this type exists to
    ///   prevent.
    func start(interval: Duration, schedule: Schedule = .afterCompletion,
               tick: @escaping @MainActor () async -> Outcome) {
        stop()
        let ceiling = maxDelay
        let doublings = maxDoublings
        task = Task {
            let clock = ContinuousClock()
            var consecutiveFailures = 0
            while !Task.isCancelled {
                let started = clock.now
                let outcome = await tick()
                if Task.isCancelled { break }

                switch outcome {
                case .success:
                    consecutiveFailures = 0
                case .failure:
                    consecutiveFailures = min(consecutiveFailures + 1, doublings)
                }

                let delay: Duration
                if consecutiveFailures == 0 {
                    delay = interval
                } else {
                    delay = min(ceiling, interval * (1 << consecutiveFailures))
                }

                do {
                    let now = clock.now
                    let target = started.advanced(by: delay)
                    if schedule == .fixedInterval && consecutiveFailures == 0 && target > now {
                        try await clock.sleep(until: target)
                    } else {
                        // Backoff is measured after failure. An overrun skips missed ticks.
                        try await Task.sleep(for: delay)
                    }
                } catch {
                    break // cancelled while sleeping
                }
            }
        }
    }

    /// Cancels the loop. Safe to call when not running, and safe to call repeatedly.
    func stop() {
        task?.cancel()
        task = nil
    }

    deinit {
        task?.cancel()
    }
}
