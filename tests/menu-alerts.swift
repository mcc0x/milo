import Foundation

@MainActor
private final class FakeDelivery {
    enum Failure: Error { case system }

    var permission = UsageAlertPermission.notDetermined
    var requestResult: Result<Bool, Failure> = .success(true)
    var requestCount = 0
    var checkCount = 0
    var submitted: [UsageAlertNotice] = []
    var submissionResult = UsageAlertDelivery.Submission.accepted
    var now: TimeInterval = 100

    var pauseCheck = false
    var pauseSubmission = false
    private var paused: CheckedContinuation<Void, Never>?
    private var waiting: CheckedContinuation<Void, Never>?

    func waitForPause() async {
        if paused == nil { await withCheckedContinuation { waiting = $0 } }
    }

    func resume() {
        paused?.resume()
        paused = nil
    }

    private func pause() async {
        await withCheckedContinuation { continuation in
            paused = continuation
            waiting?.resume()
            waiting = nil
        }
    }

    var client: UsageAlertDelivery {
        UsageAlertDelivery(permission: {
            self.checkCount += 1
            if self.pauseCheck {
                self.pauseCheck = false
                await self.pause()
            }
            return self.permission
        }, requestPermission: {
            self.requestCount += 1
            let granted = try self.requestResult.get()
            self.permission = granted ? .available : .blocked
            return granted
        }, submit: {
            self.submitted.append($0)
            if self.pauseSubmission {
                self.pauseSubmission = false
                await self.pause()
            }
            return self.submissionResult
        })
    }

    func model(_ defaults: UserDefaults) -> UsageAlerts {
        UsageAlerts(defaults: defaults, delivery: client, clock: { self.now })
    }
}

@main
@MainActor
private struct AlertTests {
    private static func window(_ remaining: Int, label: String = "5h", reset: Int? = 200,
                               fresh: Bool = true, stale: Int? = 150) -> UsageWindow {
        UsageWindow(label: label, remaining: remaining, fresh: fresh, resetAt: reset, staleAt: stale)
    }

    private static func provider(_ id: String, _ windows: [UsageWindow]) -> UsageProvider {
        UsageProvider(id: id, name: id.capitalized, windows: windows, detail: nil)
    }

    private static func frame(_ windows: UsageWindow...) -> UsageSnapshot {
        UsageSnapshot(version: 1, providers: windows.isEmpty ? [] : [provider("codex", windows)])
    }

    private static func isolated(_ test: @MainActor (UserDefaults, String) async -> Void) async {
        let suite = "net.usemilo.alert-tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        await test(defaults, suite)
    }

    static func main() async {
        await isolated(crossings)
        await isolated(ineligibleReadings)
        await isolated(permission)
        await isolated(inFlight)
        await isolated(failure)
    }

    private static func crossings(_ defaults: UserDefaults, _ suite: String) async {
        let system = FakeDelivery()
        let alerts = system.model(defaults)
        precondition(alerts.threshold == .off)
        precondition(alerts.refreshPermission() == nil)
        precondition(alerts.observe(frame(window(31))) == nil)
        precondition(alerts.observe(frame(window(10))) == nil)
        precondition(system.checkCount == 0 && system.requestCount == 0)

        await alerts.select(.twenty)?.value
        precondition(system.requestCount == 1 && alerts.issue == nil)
        precondition(alerts.observe(frame(window(19))) == nil, "a first low sample is only a baseline")
        precondition(alerts.observe(frame(window(19))) == nil, "re-reading a snapshot is not a crossing")
        precondition(alerts.observe(frame(window(21))) == nil)
        await alerts.observe(frame(window(20)))?.value
        precondition(system.submitted == [.init(providerID: "codex", providerName: "Codex", windowLabel: "5h",
                                                remaining: 20, resetAt: 200)])

        _ = alerts.observe(frame())
        precondition(alerts.observe(frame(window(12))) == nil, "unhiding a low reading is not a crossing")
        _ = alerts.observe(frame(window(39)))
        precondition(alerts.observe(frame(window(12))) == nil, "recrossing the same reset does not repeat")
        await alerts.select(.thirty)?.value
        precondition(alerts.observe(frame(window(12))) == nil, "changing threshold does not send")
        _ = alerts.observe(frame(window(39)))
        precondition(alerts.observe(frame(window(28))) == nil, "changing threshold preserves deduplication")
        precondition(system.requestCount == 1)

        let reopened = system.model(UserDefaults(suiteName: suite)!)
        precondition(reopened.threshold == .thirty)
        await reopened.refreshPermission()?.value
        _ = reopened.observe(frame(window(40)))
        precondition(reopened.observe(frame(window(29))) == nil, "relaunch preserves recorded resets")
        let snapshots = [35, 29].map { value in
            UsageSnapshot(version: 1, providers: [
                provider("codex", [window(value, reset: 201), window(value, label: "week", reset: 220),
                                   window(value, label: "model week", reset: 220)]),
                provider("claude", [window(value, reset: 201)]),
            ])
        }
        _ = reopened.observe(snapshots[0])
        await reopened.observe(snapshots[1])?.value
        precondition(system.submitted.map(\.providerID) == ["codex", "codex", "codex", "claude"])
        precondition(system.submitted.map(\.resetAt) == [200, 201, 220, 201], "distinct resets and providers are independent")
    }

    private static func ineligibleReadings(_ defaults: UserDefaults, _: String) async {
        let system = FakeDelivery()
        system.permission = .available
        let alerts = system.model(defaults)
        await alerts.select(.twenty)?.value
        for low in [window(10, reset: nil), window(10, fresh: false), window(10, stale: 100),
                    window(10, stale: nil), window(10, reset: 100), window(10, label: "week")] {
            _ = alerts.observe(frame(window(30)))
            precondition(alerts.observe(frame(low)) == nil)
            precondition(alerts.observe(frame(window(10))) == nil, "a missing or unreliable reading breaks the baseline")
        }
        _ = alerts.observe(frame(window(30, stale: 105)))
        system.now = 105
        precondition(alerts.observe(frame(window(10))) == nil, "cannot cross from a baseline that became stale")
        _ = alerts.observe(frame(window(30)))
        precondition(alerts.observe(frame(window(10, reset: 201))) == nil, "a new reset starts a baseline")
        _ = alerts.observe(frame(window(30)))
        alerts.clearBaseline()
        precondition(alerts.observe(frame(window(10))) == nil, "reader restart does not synthesize a crossing")
        precondition(system.submitted.isEmpty)
    }

    private static func permission(_ defaults: UserDefaults, _: String) async {
        let system = FakeDelivery()
        system.requestResult = .success(false)
        let alerts = system.model(defaults)
        system.pauseCheck = true
        let choice = alerts.select(.ten)
        precondition(choice != nil)
        await system.waitForPause()
        _ = alerts.select(.off)
        system.resume()
        await choice?.value
        precondition(system.requestCount == 0 && alerts.issue == nil, "turning off before a check completes must not prompt")
        await alerts.select(.ten)?.value
        precondition(system.requestCount == 1 && alerts.issue?.contains("System Settings") == true)
        precondition(alerts.observe(frame(window(31))) == nil)
        await alerts.observe(frame(window(7)))?.value
        await alerts.refreshPermission()?.value
        precondition(system.requestCount == 1 && system.submitted.isEmpty)

        system.permission = .notDetermined
        let reopened = system.model(defaults)
        await reopened.refreshPermission()?.value
        precondition(system.requestCount == 1, "a saved threshold must not request permission at launch")
        precondition(reopened.issue?.contains("permission is required") == true)
        await reopened.select(.twenty)?.value
        precondition(system.requestCount == 1, "editing an enabled threshold must not request permission")
        _ = reopened.select(.off)
        system.requestResult = .failure(.system)
        await reopened.select(.twenty)?.value
        precondition(system.requestCount == 2 && reopened.issue?.contains("couldn’t confirm") == true)
        system.permission = .available
        _ = reopened.observe(frame(window(32)))
        await reopened.observe(frame(window(19)))?.value
        precondition(reopened.issue == nil && system.requestCount == 2 && system.submitted.count == 1,
                     "a real crossing honors changes in System Settings without prompting")
    }

    private static func inFlight(_ defaults: UserDefaults, _: String) async {
        let system = FakeDelivery()
        system.permission = .available
        let alerts = system.model(defaults)
        await alerts.select(.twenty)?.value

        _ = alerts.observe(frame(window(30)))
        system.pauseCheck = true
        let whenHidden = alerts.observe(frame(window(10)))
        precondition(whenHidden != nil)
        await system.waitForPause()
        _ = alerts.observe(frame())
        _ = alerts.observe(frame(window(10)))
        system.resume()
        await whenHidden?.value
        precondition(system.submitted.isEmpty, "a hidden and restored reading invalidates a queued crossing")

        _ = alerts.observe(frame(window(30)))
        system.pauseCheck = true
        let whenStale = alerts.observe(frame(window(10)))
        precondition(whenStale != nil)
        await system.waitForPause()
        system.now = 150
        system.resume()
        await whenStale?.value
        precondition(system.submitted.isEmpty, "a crossing can expire while permission is checked")

        _ = alerts.observe(frame(window(31, stale: 180)))
        system.pauseCheck = true
        let whenChanged = alerts.observe(frame(window(10, stale: 180)))
        precondition(whenChanged != nil)
        await system.waitForPause()
        let changed = alerts.select(.thirty)
        _ = alerts.observe(frame(window(10, stale: 180)))
        system.resume()
        await changed?.value
        await whenChanged?.value
        precondition(system.submitted.isEmpty, "a threshold change invalidates a queued crossing")

        _ = alerts.observe(frame(window(31, stale: 180)))
        system.pauseCheck = true
        let whenRefreshed = alerts.observe(frame(window(10, stale: 180)))
        precondition(whenRefreshed != nil)
        await system.waitForPause()
        precondition(alerts.observe(frame(window(10, stale: 180))) == nil)
        system.resume()
        await whenRefreshed?.value
        precondition(system.submitted.count == 1, "a duplicate refresh neither loses nor repeats a real crossing")
    }

    private static func failure(_ defaults: UserDefaults, _: String) async {
        let system = FakeDelivery()
        system.permission = .available
        system.submissionResult = .rejected
        let alerts = system.model(defaults)
        await alerts.select(.ten)?.value
        _ = alerts.observe(frame(window(11), window(11, label: "week")))
        await alerts.observe(frame(window(9), window(9, label: "week")))?.value
        precondition(alerts.issue?.contains("couldn’t send") == true && system.submitted.count == 1)
        await alerts.refreshPermission()?.value
        precondition(alerts.issue?.contains("couldn’t send") == true, "permission is not evidence of delivery")
        precondition(alerts.observe(frame(window(9))) == nil)
        system.submissionResult = .accepted
        _ = alerts.observe(frame(window(11)))
        await alerts.observe(frame(window(9)))?.value
        precondition(alerts.issue == nil && system.submitted.count == 2, "a later real crossing can succeed after rejection")
        _ = alerts.observe(frame(window(11)))
        precondition(alerts.observe(frame(window(9))) == nil, "the successful attempt remains deduplicated")

        system.submissionResult = .unconfirmed
        _ = alerts.observe(frame(window(11, reset: 201)))
        await alerts.observe(frame(window(9, reset: 201)))?.value
        precondition(alerts.issue?.contains("couldn’t confirm") == true && system.submitted.count == 3)
        _ = alerts.observe(frame(window(11, reset: 201)))
        precondition(alerts.observe(frame(window(9, reset: 201))) == nil, "an uncertain submission must remain deduplicated")

        system.submissionResult = .rejected
        system.pauseSubmission = true
        _ = alerts.observe(frame(window(11, reset: 202)))
        let pending = alerts.observe(frame(window(9, reset: 202)))
        precondition(pending != nil)
        await system.waitForPause()
        _ = alerts.select(.off)
        await alerts.select(.ten)?.value
        system.resume()
        await pending?.value
        precondition(alerts.issue == nil, "an obsolete submission cannot set the current alert status")
    }
}
