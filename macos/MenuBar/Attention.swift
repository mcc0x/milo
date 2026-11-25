import AppKit
import SwiftUI

// The optional "Needs you" section: Rex panes whose program reports that it waits for the
// person, failed or finished. The bundled Milo decides what is listed; this file starts that
// watch only while the section is on, shows its rows and asks it to show a pane in Rex.

enum AttentionShow: String, CaseIterable {
    case off, menu, dot

    var title: String {
        switch self {
        case .off: "Off"
        case .menu: "In this menu"
        case .dot: "In this menu, with a menu bar dot"
        }
    }
}

enum AttentionState: String, CaseIterable {
    case waiting, errors, done

    var title: String {
        switch self {
        case .waiting: "Waiting for input"
        case .errors: "Errors"
        case .done: "Finished work"
        }
    }
}

struct AttentionChoice: Equatable {
    var show = AttentionShow.off
    var states: Set<AttentionState> = [.waiting, .errors]

    init() {}

    /// What a usage frame says is saved. Anything this build cannot read is off, never a watch.
    init(show: String?, states: String?) {
        let listed = states?.split(separator: ",").map { AttentionState(rawValue: String($0)) } ?? []
        guard let show = show.flatMap(AttentionShow.init(rawValue:)), !listed.isEmpty, !listed.contains(nil) else { return }
        self.show = show
        self.states = Set(listed.compactMap { $0 })
    }

    /// The list `milo menubar attention states` takes for these states with one of them switched.
    func statesArgument(setting state: AttentionState, to included: Bool) -> String? {
        var next = states
        if included { next.insert(state) } else { next.remove(state) }
        guard !next.isEmpty else { return nil }
        return AttentionState.allCases.filter(next.contains).map(\.rawValue).joined(separator: ",")
    }
}

struct AttentionEntry: Codable, Equatable, Identifiable {
    let host: String
    let block: String
    let name: String
    let state: String
    let since: Int
    var tab: String? = nil
    var app: String? = nil
    var asks: String? = nil

    var id: String { block }
    /// A wait ends when the program is answered, so only a failure or finished work can be marked seen.
    var canMarkSeen: Bool { state != "waiting" }

    var title: String { tab.map { "\(name) › \($0)" } ?? name }

    var report: String {
        switch (state, asks) {
        case ("waiting", "permission"): "Waiting for permission"
        case ("waiting", "question"): "Waiting for an answer"
        case ("waiting", "auth"): "Waiting for sign-in"
        case ("waiting", _): "Waiting for input"
        case ("error", _): "Reported an error"
        default: "Finished"
        }
    }

    var detail: String { app.map { "\(report) · \($0)" } ?? report }

    var symbol: String {
        switch state {
        case "waiting": "hand.raised"
        case "error": "exclamationmark.triangle"
        default: "checkmark.circle"
        }
    }

    private func minutes(at now: Date) -> Int { max(0, (Int(now.timeIntervalSince1970) - since) / 60) }

    func age(at now: Date) -> String {
        switch minutes(at: now) {
        case ..<1: "now"
        case let m where m < 60: "\(m)m"
        case let m where m < 1_440: "\(m / 60)h"
        case let m: "\(m / 1_440)d"
        }
    }

    func spokenAge(at now: Date) -> String {
        func unit(_ count: Int, _ name: String) -> String { "for \(count) \(name)\(count == 1 ? "" : "s")" }
        switch minutes(at: now) {
        case ..<1: return "just now"
        case let m where m < 60: return unit(m, "minute")
        case let m where m < 1_440: return unit(m / 60, "hour")
        case let m: return unit(m / 1_440, "day")
        }
    }
}

struct AttentionFrame: Codable, Equatable {
    let version: Int
    let rex: String
    let dot: Bool
    let entries: [AttentionEntry]
    let more: Int

    var count: Int { entries.count + more }
}

struct AttentionNote: Equatable {
    let block: String
    let text: String
}

@MainActor
final class AttentionWatch: ObservableObject {
    @Published private(set) var frame: AttentionFrame?
    /// What the last attempt to show a pane said, when it did not end with the pane on screen.
    @Published private(set) var note: AttentionNote?
    @Published private(set) var jumping: String?
    var changed: (() -> Void)?
    /// Brings forward the Rex app that showed the pane.
    var activate: (pid_t) -> Void = { NSRunningApplication(processIdentifier: $0)?.activate(options: [.activateAllWindows]) }
    private var child: Process?
    private var input: FileHandle?
    private var make: (() -> Process?)?
    private var retry: DispatchWorkItem?
    private let reader = DispatchQueue(label: "net.usemilo.attention.frames", qos: .utility)
    private static let retryDelay: TimeInterval = 5

    /// Whether the menu bar item carries the dot.
    var needsYou: Bool { frame.map { $0.dot && $0.count > 0 } ?? false }
    var count: Int { frame?.count ?? 0 }
    var running: Bool { child != nil }

    /// Off ends the watch: no process is left, and nothing asks Rex again until it is turned on.
    func setEnabled(_ enabled: Bool, process make: @escaping () -> Process?) {
        guard enabled else { stop(); return }
        self.make = make
        if child == nil && retry == nil { start() }
    }

    private func start() {
        retry = nil
        guard let make, let process = make() else { return }
        let incoming = Pipe(), outgoing = Pipe()
        process.standardInput = incoming; process.standardOutput = outgoing; process.standardError = FileHandle.nullDevice
        do { try process.run() } catch { ended(); return }
        child = process; input = incoming.fileHandleForWriting
        let output = outgoing.fileHandleForReading
        reader.async { [weak self] in
            defer { try? output.close() }
            var pending = Data()
            let decoder = JSONDecoder()
            read: while true {
                let bytes = output.availableData
                if bytes.isEmpty { break }
                pending.append(bytes)
                while let newline = pending.firstIndex(of: 10) {
                    // Rows are shown whole or not at all: a frame this build cannot read ends the watch.
                    guard newline - pending.startIndex <= 65536,
                          let frame = try? decoder.decode(AttentionFrame.self, from: pending[..<newline]),
                          frame.version == 1 else { break read }
                    pending.removeSubrange(...newline)
                    DispatchQueue.main.async { [weak self] in
                        guard let self, self.child === process else { return }
                        self.receive(frame)
                    }
                }
                guard pending.count <= 65536 else { break }
            }
            DispatchQueue.main.async { [weak self] in
                guard let self, self.child === process else { return }
                self.ended()
            }
        }
    }

    func receive(_ next: AttentionFrame) {
        guard frame != next else { return }
        frame = next
        if let note, !next.entries.contains(where: { $0.block == note.block }) { self.note = nil }
        changed?()
    }

    /// The watch ended by itself. Its rows are no longer current, so none is kept; try again shortly.
    private func ended() {
        close()
        guard make != nil else { return }
        let retry = DispatchWorkItem { [weak self] in self?.start() }
        self.retry = retry
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.retryDelay, execute: retry)
    }

    private func close() {
        try? input?.close(); input = nil
        if let process = child, process.isRunning { process.terminate() }
        child = nil
        if frame != nil || note != nil {
            frame = nil; note = nil
            changed?()
        }
    }

    func stop() {
        retry?.cancel(); retry = nil
        make = nil
        close()
    }

    /// The saved choice changed: the watch reads it again, and ends itself if it is now off.
    func reread() { send(Data([0])) }

    func markSeen(_ entry: AttentionEntry) {
        guard entry.canMarkSeen else { return }
        send(Data("seen \(entry.block)\n".utf8))
    }

    func clearNote() { if note != nil { note = nil } }

    private func send(_ data: Data) {
        guard let input else { return }
        do { try input.write(contentsOf: data) } catch { ended() }
    }

    /// Runs `milo menubar attention jump` for the row. Only that row is marked seen, and only once Rex shows its pane.
    func jump(to entry: AttentionEntry, using process: Process?) {
        guard jumping == nil else { return }
        guard let process else { note = AttentionNote(block: entry.block, text: "Milo couldn’t ask Rex to show that pane."); return }
        let output = Pipe()
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        process.terminationHandler = { [weak self] _ in
            let data = (try? output.fileHandleForReading.readToEnd()) ?? Data()
            DispatchQueue.main.async { self?.jumped(entry, said: String(decoding: data.prefix(4096), as: UTF8.self)) }
        }
        jumping = entry.block
        note = nil
        do { try process.run() } catch { jumped(entry, said: "") }
    }

    private func jumped(_ entry: AttentionEntry, said: String) {
        jumping = nil
        let lines = said.split(separator: "\n").map(String.init)
        let words = lines.first?.split(separator: " ") ?? []
        if words.count == 2, words[0] == "shown", let pid = pid_t(words[1]) {
            markSeen(entry)
            activate(pid)
        } else if lines.count == 2, ["focused", "unavailable"].contains(lines[0]) {
            note = AttentionNote(block: entry.block, text: lines[1])
        } else {
            note = AttentionNote(block: entry.block, text: "Milo couldn’t ask Rex to show that pane.")
        }
    }
}

struct AttentionSection: View {
    @ObservedObject var watch: AttentionWatch
    let now: Date
    let jump: (AttentionEntry) -> Void
    private static let rows = 5

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Needs you")
                .font(.system(size: 13, weight: .semibold))
                .accessibilityAddTraits(.isHeader)
            if let frame = watch.frame {
                if frame.entries.isEmpty {
                    caption(Self.status(frame.rex))
                } else {
                    ForEach(frame.entries.prefix(Self.rows)) { entry in
                        AttentionRow(entry: entry, now: now, busy: watch.jumping != nil,
                                     jump: { jump(entry) }, markSeen: { watch.markSeen(entry) })
                    }
                    let hidden = frame.count - min(frame.entries.count, Self.rows)
                    if hidden > 0 { caption("\(hidden) more in Rex") }
                }
            } else {
                caption("Asking Rex…")
            }
            if let note = watch.note {
                Text(note.text)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityLabel("Couldn’t show the pane. \(note.text)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.bottom, 12)
    }

    static func status(_ rex: String) -> String {
        switch rex {
        case "connected": "No pane needs you."
        case "missing": "Rex wasn’t found on this Mac, so no pane is watched."
        case "remote": "The Rex server in use runs on another computer. Its panes aren’t listed here."
        default: "Rex isn’t running on this Mac. No pane is counted until it is."
        }
    }

    private func caption(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 12))
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

// The command-line toolchain builds without SwiftUI's macro plugin, so the wrapper is named as a type.
private typealias RowState<Value> = SwiftUI.State<Value>

private struct AttentionRow: View {
    let entry: AttentionEntry
    let now: Date
    let busy: Bool
    let jump: () -> Void
    let markSeen: () -> Void
    @RowState private var hovered = false

    var body: some View {
        HStack(spacing: 4) {
            Button(action: jump) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: entry.symbol)
                        .font(.system(size: 12))
                        .foregroundStyle(entry.state == "done" ? Color.secondary : Color.orange)
                        .frame(width: 16)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(entry.title)
                            .font(.system(size: 12, weight: .medium))
                            .lineLimit(1).truncationMode(.middle)
                        Text(entry.detail)
                            .font(.system(size: 11))
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    Text(entry.age(at: now))
                        .font(.system(size: 11).monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                .padding(.vertical, 4).padding(.horizontal, 6)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .background(RoundedRectangle(cornerRadius: 5).fill(Color.primary.opacity(hovered ? 0.08 : 0)))
            .onHover { hovered = $0 }
            .disabled(busy)
            .help("Show this pane in Rex")
            .accessibilityLabel("\(entry.title). \(entry.detail), \(entry.spokenAge(at: now)).")
            .accessibilityHint("Shows this pane in Rex")
            if entry.canMarkSeen {
                Button(action: markSeen) {
                    Image(systemName: "eye").font(.system(size: 11)).frame(width: 20, height: 20)
                }
                .buttonStyle(.borderless)
                .help("Mark seen")
                .accessibilityLabel("Mark \(entry.title) seen")
            } else {
                // Keeps the ages of every row in one column.
                Color.clear.frame(width: 20, height: 20).accessibilityHidden(true)
            }
        }
        .padding(.horizontal, -6)
    }
}
