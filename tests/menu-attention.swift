import Foundation

// Checks the menu's Needs you section against stand-ins for the bundled Milo: `/bin/sh`
// scripts that print the frames and jump reports Milo would. No Rex is asked.

@main
@MainActor
private struct AttentionTests {
    private static func entry(_ block: String, _ state: String, asks: String? = nil, tab: String? = nil) -> AttentionEntry {
        AttentionEntry(host: "h", block: block, name: "api", state: state, since: 1_000, tab: tab, app: "codex", asks: asks)
    }

    private static func json(_ entries: [AttentionEntry], rex: String = "connected", dot: Bool = true) -> String {
        let frame = AttentionFrame(version: 1, rex: rex, dot: dot, entries: entries, more: 0)
        return String(decoding: try! JSONEncoder().encode(frame), as: UTF8.self)
    }

    private static func shell(_ script: String) -> Process {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = ["-c", script]
        return process
    }

    /// Waits up to fifteen seconds, for a shared runner that starts a process slowly.
    private static func until(_ what: String, _ check: () -> Bool) async {
        for _ in 0..<1500 where !check() { try? await Task.sleep(nanoseconds: 10_000_000) }
        precondition(check(), what)
    }

    static func main() async {
        choices()
        wording()
        await watching()
        await jumping()
    }

    private static func choices() {
        precondition(AttentionChoice(show: nil, states: nil).show == .off, "a frame from an older reader is off")
        let saved = AttentionChoice(show: "dot", states: "waiting,done")
        precondition(saved.show == .dot && saved.states == [.waiting, .done])
        for unknown in [("always", "waiting"), ("dot", ""), ("dot", "waiting,bell"), ("menu", nil)] {
            precondition(AttentionChoice(show: unknown.0, states: unknown.1) == AttentionChoice(), "\(unknown) must not start a watch")
        }
        precondition(saved.statesArgument(setting: .errors, to: true) == "waiting,errors,done")
        precondition(saved.statesArgument(setting: .waiting, to: false) == "done")
        precondition(AttentionChoice(show: "menu", states: "errors").statesArgument(setting: .errors, to: false) == nil,
                     "the last state cannot be switched off")
    }

    private static func wording() {
        let now = Date(timeIntervalSince1970: 1_000)
        let waiting = entry("b", "waiting", asks: "permission", tab: "Tests")
        precondition(waiting.title == "api › Tests" && waiting.detail == "Waiting for permission · codex")
        precondition(!waiting.canMarkSeen && entry("b", "done").canMarkSeen && entry("b", "error").canMarkSeen)
        precondition(entry("b", "waiting", asks: "question").report == "Waiting for an answer")
        precondition(entry("b", "waiting").report == "Waiting for input" && entry("b", "error").report == "Reported an error")
        precondition(entry("b", "done").title == "api" && entry("b", "done").report == "Finished")
        let ages: [(TimeInterval, String, String)] = [
            (-30, "now", "just now"), (59, "now", "just now"), (60, "1m", "for 1 minute"),
            (3_599, "59m", "for 59 minutes"), (7_200, "2h", "for 2 hours"), (200_000, "2d", "for 2 days"),
        ]
        for (later, short, spoken) in ages {
            let at = now.addingTimeInterval(later)
            precondition(waiting.age(at: at) == short && waiting.spokenAge(at: at) == spoken, "\(later)")
        }
        for (rex, said) in [("waiting", "isn’t running"), ("missing", "wasn’t found"), ("remote", "another computer"),
                            ("connected", "No pane needs you")] {
            precondition(AttentionSection.status(rex).contains(said), rex)
        }
    }

    private static func watching() async {
        let watch = AttentionWatch()
        var started = 0, drawn = 0
        watch.changed = { drawn += 1 }
        let two = json([entry("block:1", "waiting"), entry("block:2", "done")])
        let one = json([entry("block:1", "waiting")])
        // Prints both rows, then the frame without the pane it was told is seen, then ends at a NUL.
        let script = "printf '%s\\n' '\(two)'; read -r seen; [ \"$seen\" = 'seen block:2' ] && printf '%s\\n' '\(one)'; "
            + "while [ \"$(dd bs=1 count=1 2>/dev/null | od -An -tu1 | tr -d ' ')\" != 0 ]; do :; done"
        let make = { started += 1; return Optional(shell(script)) }

        watch.setEnabled(false, process: make)
        precondition(started == 0 && !watch.running && !watch.needsYou, "off starts nothing")
        watch.setEnabled(true, process: make)
        watch.setEnabled(true, process: make)
        precondition(started == 1 && watch.running, "a running watch is not started twice")
        await until("both rows") { watch.frame?.entries.count == 2 }
        precondition(watch.needsYou && watch.count == 2 && drawn == 1)

        watch.markSeen(entry("block:1", "waiting"))
        try? await Task.sleep(nanoseconds: 100_000_000)
        precondition(watch.frame?.entries.count == 2, "a wait cannot be marked seen")
        watch.markSeen(entry("block:2", "done"))
        await until("the seen row leaves") { watch.frame?.entries.map(\.block) == ["block:1"] }

        // Menu-only frames list rows without asking for the dot.
        watch.receive(AttentionFrame(version: 1, rex: "connected", dot: false, entries: [entry("block:1", "waiting")], more: 0))
        precondition(!watch.needsYou && watch.count == 1)
        watch.receive(AttentionFrame(version: 1, rex: "waiting", dot: true, entries: [], more: 0))
        precondition(!watch.needsYou, "a lost Rex shows no dot")

        // The saved choice turned off: the watch ends by itself and the menu keeps none of its rows.
        watch.reread()
        await until("the watch ended") { !watch.running && watch.frame == nil }
        watch.stop()
        try? await Task.sleep(nanoseconds: 100_000_000)
        precondition(started == 1 && !watch.running, "a stopped watch is not started again")

        // A frame this build cannot read ends the watch without showing part of it.
        let odd = AttentionWatch()
        odd.setEnabled(true) { shell("printf '%s\\n' '{\"version\":2,\"rex\":\"connected\"}'; sleep 5") }
        try? await Task.sleep(nanoseconds: 300_000_000)
        precondition(odd.frame == nil)
        odd.stop()
    }

    private static func jumping() async {
        let watch = AttentionWatch()
        var activated: [pid_t] = []
        watch.activate = { activated.append($0) }
        let log = FileManager.default.temporaryDirectory.appendingPathComponent("milo-attention-\(getpid())")
        defer { try? FileManager.default.removeItem(at: log) }
        let done = entry("block:2", "done")
        watch.setEnabled(true) { shell("printf '%s\\n' '\(json([done]))'; cat > '\(log.path)'") }
        await until("the row") { watch.frame != nil }

        watch.jump(to: done, using: shell("printf 'focused\\nOpen the session in Rex.\\n'"))
        precondition(watch.jumping == "block:2")
        await until("the fallback is explained") { watch.note?.text == "Open the session in Rex." }
        watch.jump(to: done, using: shell("printf 'unavailable\\nThat pane has closed.\\n'"))
        await until("the refusal is explained") { watch.note?.text == "That pane has closed." }
        watch.jump(to: done, using: shell("exit 3"))
        await until("a failed command is explained") { watch.note?.text.contains("couldn’t ask Rex") == true }
        precondition(activated.isEmpty && watch.jumping == nil)

        watch.jump(to: done, using: shell("printf 'shown 4242\\n'"))
        await until("Rex is brought forward") { activated == [4242] }
        precondition(watch.note == nil)
        watch.stop()
        await until("the watch's input closed") { FileManager.default.fileExists(atPath: log.path) }
        // Only a pane Rex showed is marked seen, and only that pane.
        await until("one row marked seen") { (try? String(contentsOf: log, encoding: .utf8)) == "seen block:2\n" }
    }
}
