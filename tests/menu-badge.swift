import Foundation

@main struct MenuBadgeTest {
    private static let timestamp = 1_750_000_000

    static func main() {
        let codex = provider("codex", "Codex", [
            window("5h", 45), window("week", 12), window("Old model week", 0, staleIn: 0)
        ])
        let claude = provider("claude", "Claude Code", [
            window("5h", 40), window("week", 8, resetIn: 130),
            window("Historical model week", 0, fresh: false), window("Elapsed model week", 0, resetIn: 0)
        ])
        let openCode = provider("opencode", "OpenCode", [])
        let frame = snapshot([codex, claude, openCode])
        let original = ["codex", "claude", "opencode"]
        let pinned = ["claude", "codex", "opencode"]

        let auto = badgeReading(frame, mode: "auto", now: date())!
        precondition(auto.providerIndex == 1 && auto.windowIndex == 1)
        precondition(auto.label(at: date()) == "Claude Code, weekly limit: 8% left. Resets in 2 minutes.")
        precondition(frame.matchingBadge(auto) == auto)
        precondition(frame.orderedProviders(pinning: auto).map(\.id) == pinned)
        precondition(Array(claude.orderedWindowIndices(pinning: auto.windowIndex)) == [1, 0, 2, 3])
        precondition(Array(codex.orderedWindowIndices(pinning: nil)) == [0, 1, 2])

        // The same frame (no new provider report) yields a new countdown and eventually a new selection.
        let oneMinute = badgeReading(frame, mode: "auto", now: date(70))!
        precondition(oneMinute == auto)
        precondition(oneMinute.label(at: date(70)) == "Claude Code, weekly limit: 8% left. Resets in 1 minute.")
        let lastMinute = badgeReading(frame, mode: "auto", now: date(129.5))!
        precondition(lastMinute == auto)
        precondition(lastMinute.label(at: date(129.5)) == "Claude Code, weekly limit: 8% left. Resets in less than a minute.")
        let elapsed = badgeReading(frame, mode: "auto", now: date(130))!
        precondition(elapsed.providerIndex == 0 && elapsed.windowIndex == 1)
        precondition(frame.orderedProviders(pinning: elapsed).map(\.id) == original)

        let pinnedCodex = badgeReading(frame, mode: "codex", now: date())!
        precondition(pinnedCodex == elapsed)
        precondition(pinnedCodex.label(at: date()) == "Codex, weekly limit: 12% left. Reset time unavailable.")
        precondition(frame.orderedProviders(pinning: pinnedCodex).map(\.id) == original)
        precondition(Array(codex.orderedWindowIndices(pinning: pinnedCodex.windowIndex)) == [1, 0, 2])
        let pinnedClaude = badgeReading(frame, mode: "claude", now: date(130))!
        precondition(pinnedClaude.providerIndex == 1 && pinnedClaude.windowIndex == 0)
        precondition(frame.orderedProviders(pinning: pinnedClaude).map(\.id) == pinned)
        precondition(Array(claude.orderedWindowIndices(pinning: pinnedClaude.windowIndex)) == Array(claude.windows.indices))

        let icon = badgeReading(frame, mode: "icon", now: date())
        let stale = badgeReading(frame, mode: "auto", now: date(600))
        precondition(icon == nil && stale == nil)
        precondition(frame.orderedProviders(pinning: icon).map(\.id) == original)
        precondition(frame.orderedProviders(pinning: stale).map(\.id) == original)
        precondition(badgeReading(snapshot([claude]), mode: "codex", now: date()) == nil)
        precondition(badgeReading(snapshot([]), mode: "auto", now: date()) == nil)
        precondition(badgeReading(nil, mode: "auto", now: date()) == nil)

        let shortened = snapshot([claude])
        precondition(shortened.matchingBadge(auto) == nil)
        precondition(shortened.orderedProviders(pinning: auto).map(\.id) == ["claude"])
        precondition(snapshot([codex, openCode, claude]).matchingBadge(auto) == nil)
        let shrunkClaude = provider("claude", "Claude Code", [claude.windows[0]])
        precondition(snapshot([codex, shrunkClaude, openCode]).matchingBadge(auto) == nil)
        precondition(Array(shrunkClaude.orderedWindowIndices(pinning: auto.windowIndex)) == [0])

        // Equal minimums select the first provider and first row in stream order.
        // Even identical readings pin only the selected row, not its equal sibling.
        let zero = window("5h", 0, resetIn: 90 * 60)
        let tied = provider("claude", "Claude Code", [window("week", 50), zero, zero])
        let tieFrame = snapshot([tied, provider("codex", "Codex", [zero])])
        let winner = badgeReading(tieFrame, mode: "auto", now: date())!
        precondition(winner.providerIndex == 0 && winner.windowIndex == 1)
        precondition(winner.label(at: date()) == "Claude Code, 5-hour limit: 0% left. Resets in 1 hour, 30 minutes.")
        precondition(tieFrame.orderedProviders(pinning: winner).map(\.id) == ["claude", "codex"])
        precondition(Array(tied.orderedWindowIndices(pinning: winner.windowIndex)) == [1, 0, 2])

        let days = window("Opus week", 99, resetIn: 49 * 60 * 60)
        let dayFrame = snapshot([provider("claude", "Claude Code", [days])])
        let dayReading = badgeReading(dayFrame, mode: "auto", now: date())!
        precondition(dayReading.label(at: date()) == "Claude Code, Opus weekly limit: 99% left. Resets in 2 days, 1 hour.")
    }

    private static func date(_ offset: TimeInterval = 0) -> Date {
        Date(timeIntervalSince1970: Double(timestamp) + offset)
    }

    private static func snapshot(_ providers: [UsageProvider]) -> UsageSnapshot {
        UsageSnapshot(version: 1, providers: providers)
    }

    private static func provider(_ id: String, _ name: String, _ windows: [UsageWindow]) -> UsageProvider {
        UsageProvider(id: id, name: name, windows: windows, detail: nil)
    }

    private static func window(_ label: String, _ remaining: Int, fresh: Bool = true, resetIn: Int? = nil, staleIn: Int = 600) -> UsageWindow {
        UsageWindow(label: label, remaining: remaining, fresh: fresh, resetAt: resetIn.map { timestamp + $0 }, staleAt: timestamp + staleIn)
    }
}
