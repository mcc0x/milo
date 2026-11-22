// Appended to macos/MenuBar/App.swift by tests/menu-bench.sh, in place of the app's entry point,
// so the real model, status item and popover run against made-up frames. Nothing here reads
// usage, starts the bundled reader or touches the installed app.
import QuartzCore

private extension AppDelegate {
    var popoverShown: Bool { panelOnScreen }
    var panel: NSView? { popover.contentViewController?.view }
    // Under a sleeping display the closing animation never ends, and the popover does not open again.
    // A popover that closes at a click or another app's activation would end a row or a check early.
    func showPopover() { popover.animates = false; popover.behavior = .applicationDefined; show() }
    func closePopover() { popover.close() }
    var statusTitle: String? { status.button?.title }
}

@MainActor
private enum MenuBench {
    private static let start = Int(Date.now.timeIntervalSince1970)

    private static func window(_ label: String, _ remaining: Int, resetIn: Int?) -> UsageWindow {
        UsageWindow(label: label, remaining: remaining, fresh: true, resetAt: resetIn.map { start + $0 }, staleAt: start + 3_600)
    }

    // `lowest` is the reading the badge shows; `other` only appears in the popover.
    private static func frame(lowest: Int = 12, other: Int = 64, otherResetIn: Int = 2 * 3_600) -> UsageSnapshot {
        UsageSnapshot(version: 1, providers: [
            UsageProvider(id: "codex", name: "Codex", windows: [
                window("5h", other, resetIn: otherResetIn), window("week", 41, resetIn: 3 * 86_400),
            ], detail: nil),
            UsageProvider(id: "claude", name: "Claude Code", windows: [
                window("5h", 57, resetIn: 3 * 3_600), window("week", lowest, resetIn: 4 * 86_400),
                window("Opus week", 88, resetIn: 4 * 86_400),
            ], detail: nil),
            UsageProvider(id: "opencode", name: "OpenCode", windows: [], detail: "No limit reported"),
        ])
    }

    private static func cpu() -> UInt64 { clock_gettime_nsec_np(CLOCK_PROCESS_CPUTIME_ID) }
    private static func wall() -> UInt64 { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }
    private static func pause(_ milliseconds: UInt64) async { try? await Task.sleep(nanoseconds: milliseconds * 1_000_000) }

    private static func report(_ name: String, _ samples: [UInt64], cpu: UInt64?, note: String = "") {
        let sorted = samples.sorted()
        let median = Double(sorted[sorted.count / 2]) / 1_000
        let p95 = Double(sorted[min(sorted.count - 1, sorted.count * 95 / 100)]) / 1_000
        let whole = cpu.map { String(format: "%.1f", Double($0) / Double(samples.count) / 1_000) } ?? "-"
        print("\(name)\t\(samples.count)\t\(String(format: "%.1f", median))\t\(String(format: "%.1f", p95))\t\(whole)\t\(note)")
    }

    /// Runs `body` with the run loop idling in between, as frames arrive in the app.
    /// Reports the time the main thread was held, and the process CPU per iteration,
    /// which also counts rendering SwiftUI defers to the end of the run-loop turn.
    private static func paced(_ name: String, count: Int = 400, note: @escaping () -> String = { "" }, _ body: (Int) -> Void) async {
        await pause(150)
        var samples: [UInt64] = []
        samples.reserveCapacity(count)
        let before = cpu()
        for index in 0..<count {
            let began = wall()
            body(index)
            samples.append(wall() - began)
            await pause(3)
        }
        await pause(50)
        report(name, samples, cpu: cpu() - before, note: note())
    }

    private static func tight(_ name: String, count: Int = 20_000, _ body: (Int) -> Void) {
        var samples: [UInt64] = []
        samples.reserveCapacity(count)
        for index in 0..<count {
            let began = wall()
            body(index)
            samples.append(wall() - began)
        }
        report(name, samples, cpu: nil)
    }

    private static func frames(_ delegate: AppDelegate, _ state: String) async {
        let model = delegate.model
        // A click elsewhere closes the popover: say so when a row was not timed as named.
        let note = { delegate.popoverShown == (state == "open") ? "" : delegate.popoverShown ? "popover open" : "popover closed" }
        let same = frame(), badge = frame(lowest: 11), display = frame(other: 63)
        model.receive(same)
        await paced("idle/\(state)", note: note) { _ in }
        await paced("unchanged/\(state)", note: note) { _ in model.receive(same) }
        await paced("badge-changed/\(state)", note: note) { model.receive($0 % 2 == 0 ? badge : same) }
        await paced("display-changed/\(state)", note: note) { model.receive($0 % 2 == 0 ? display : same) }
    }

    static func run(_ delegate: AppDelegate) async {
        print("scenario\tn\tmain_median_us\tmain_p95_us\tcpu_us_per_iter\tnote")
        let model = delegate.model
        let same = frame()
        let encoded = try! JSONEncoder().encode(same)
        let decoder = JSONDecoder()
        tight("decode-frame") { _ in _ = try! decoder.decode(UsageSnapshot.self, from: encoded) }
        tight("compare-frame") { _ in precondition(same == frame()) }
        let now = Date.now
        tight("badge-reading") { _ in _ = badgeReading(same, mode: "auto", now: now)?.label(at: now) }
        let suite = "net.usemilo.usage.bench.alerts"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        let alerts = UsageAlerts(defaults: defaults, delivery: UsageAlertDelivery(
            permission: { .available }, requestPermission: { true }, submit: { _ in .accepted }))
        await alerts.select(.ten)?.value
        tight("alerts-observe") { _ in alerts.observe(same) }
        defaults.removePersistentDomain(forName: suite)

        await frames(delegate, "never-opened")

        var opens: [UInt64] = [], logins: [UInt64] = []
        var first: UInt64 = 0
        for index in 0..<12 {
            let began = wall()
            delegate.showPopover()
            CATransaction.flush()
            let took = wall() - began
            if index == 0 { first = took } else { opens.append(took) }
            await pause(450)
            delegate.closePopover()
            await pause(350)
        }
        report("open-popover/first", [first], cpu: nil)
        report("open-popover/again", opens, cpu: nil)
        for _ in 0..<50 {
            let began = wall()
            model.refreshLoginStatus()
            logins.append(wall() - began)
        }
        report("open-popover/login-status", logins, cpu: nil)

        await frames(delegate, "closed")
        delegate.showPopover()
        await pause(450)
        await frames(delegate, "open")
        delegate.closePopover()
        if let domain = Bundle.main.bundleIdentifier { UserDefaults.standard.removePersistentDomain(forName: domain) }
    }

    private static func picture(_ delegate: AppDelegate) -> Data? {
        guard let view = delegate.panel, let image = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return nil }
        view.cacheDisplay(in: view.bounds, to: image)
        return image.tiffRepresentation
    }

    /// Whether the panel looks different after three seconds of one repeated frame.
    private static func redrawn(_ delegate: AppDelegate, resetIn: Int, open: Bool) async -> Bool {
        let repeated = frame(otherResetIn: Int(Date.now.timeIntervalSince1970) - start + resetIn)
        delegate.model.receive(repeated)
        delegate.showPopover()
        await pause(600)
        let before = picture(delegate)
        if !open { delegate.closePopover() }
        for _ in 0..<6 {
            await pause(500)
            delegate.model.receive(repeated)
        }
        if !open { delegate.showPopover() }
        await pause(600)
        let after = picture(delegate)
        if !delegate.popoverShown { print("# the popover was closed from outside during this check") }
        delegate.closePopover()
        await pause(350)
        return before != nil && after != nil && before != after
    }

    static func checkIndicators(_ delegate: AppDelegate) {
        var saved = frame()
        delegate.model.mode = "codex"
        delegate.model.receive(saved)
        precondition(delegate.statusTitle == " 41%")
        saved.indicator = "cpu"
        delegate.model.receive(saved)
        precondition(delegate.statusTitle == " CPU —", "First sample cannot be a percentage")
        precondition(delegate.model.badge == nil, "A quota row must not claim the system badge")
        saved.indicator = "memory"
        delegate.model.receive(saved)
        precondition(delegate.statusTitle?.hasPrefix(" MEM ") == true)
        delegate.model.setSleeping(true)
        precondition(delegate.model.monitor.reading == SystemReading())
        delegate.model.setSleeping(false)
        precondition(delegate.model.monitor.reading.cpuPercent == nil)
        saved.indicator = "quota"
        delegate.model.receive(saved)
        precondition(delegate.statusTitle == " 41%" && delegate.model.monitor.reading == SystemReading())
        delegate.model.mode = "auto"
    }

    /// A row also changes when its reset passes, with no new frame to say so: the panel has to
    /// show that whether it was on screen at the time or is opened afterwards.
    static func followsClock(_ delegate: AppDelegate) async -> Bool {
        let steady = await redrawn(delegate, resetIn: 3_600, open: false)
        let open = await redrawn(delegate, resetIn: 2, open: true)
        let reopened = await redrawn(delegate, resetIn: 2, open: false)
        print("# a reset passing redraws the panel: open \(open), reopened \(reopened); the same picture otherwise: \(!steady)")
        return open && reopened && !steady
    }
}

@main
private struct MenuBenchApp {
    @MainActor static func main() {
        setvbuf(stdout, nil, _IOLBF, 0)
        // An app with nothing on screen is slowed down to save power, which would time the scheduler.
        let awake = ProcessInfo.processInfo.beginActivity(options: [.userInitiated, .latencyCritical], reason: "Timing the menu app")
        defer { ProcessInfo.processInfo.endActivity(awake) }
        let app = NSApplication.shared; app.setActivationPolicy(.accessory)
        let delegate = AppDelegate(); app.delegate = delegate
        Task { @MainActor in
            // The menu bar item needs a moment before a popover can open from it.
            if CommandLine.arguments.contains("--check") { try? await Task.sleep(nanoseconds: 1_000_000_000) } else { await MenuBench.run(delegate) }
            MenuBench.checkIndicators(delegate)
            exit(await MenuBench.followsClock(delegate) ? 0 : 1)
        }
        withExtendedLifetime(delegate) { app.run() }
    }
}
