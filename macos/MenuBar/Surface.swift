import AppKit
import ServiceManagement
import SwiftUI

struct UsageSnapshot: Codable, Equatable {
    let version: Int
    let providers: [UsageProvider]
    var indicator: String? = nil
    var indicatorAck: Bool? = nil
    var attention: String? = nil
    var attentionStates: String? = nil

    func orderedProviders(pinning badge: BadgeReading?) -> some RandomAccessCollection<UsageProvider> {
        pinnedIndices(providers.indices, first: badge?.providerIndex).lazy.map { providers[$0] }
    }

    func matchingBadge(_ badge: BadgeReading?) -> BadgeReading? {
        guard let badge, providers.indices.contains(badge.providerIndex) else { return nil }
        let provider = providers[badge.providerIndex]
        guard provider.id == badge.providerID, provider.windows.indices.contains(badge.windowIndex),
              provider.windows[badge.windowIndex] == badge.window else { return nil }
        return badge
    }
}

struct UsageProvider: Codable, Equatable {
    let id: String
    let name: String
    let windows: [UsageWindow]
    let detail: String?

    func orderedWindowIndices(pinning index: Int?) -> some RandomAccessCollection<Int> {
        pinnedIndices(windows.indices, first: index)
    }
}

struct UsageWindow: Codable, Equatable {
    let label: String
    let remaining: Int
    let fresh: Bool
    let resetAt: Int?
    let staleAt: Int?

    fileprivate enum State { case current, historical, elapsed }

    fileprivate func state(at now: Date) -> State {
        let seconds = now.timeIntervalSince1970
        if let resetAt, Double(resetAt) <= seconds { return .elapsed }
        if !fresh || staleAt.map({ Double($0) <= seconds }) == true { return .historical }
        return .current
    }
}

struct BadgeReading: Equatable {
    let providerIndex: Int
    let windowIndex: Int
    let providerID: String
    let providerName: String
    let window: UsageWindow

    func label(at now: Date) -> String {
        let summary = "\(providerName), \(limitName): \(window.remaining)% left."
        guard let resetAt = window.resetAt else { return "\(summary) Reset time unavailable." }
        let minutes = Int((Double(resetAt) - now.timeIntervalSince1970) / 60)
        return "\(summary) Resets in \(Self.countdown(minutes))."
    }

    private var limitName: String {
        let label = window.label
        switch label {
        case "week": return "weekly limit"
        case "day": return "daily limit"
        default: break
        }
        if label.hasSuffix(" week") { return "\(label.dropLast(5)) weekly limit" }
        if let amount = UInt(label.dropLast()) {
            switch label.last {
            case "d": return "\(amount)-day limit"
            case "h": return "\(amount)-hour limit"
            case "m": return "\(amount)-minute limit"
            default: break
            }
        }
        return "\(label) limit"
    }

    private static func countdown(_ minutes: Int) -> String {
        func unit(_ count: Int, _ name: String) -> String { "\(count) \(name)\(count == 1 ? "" : "s")" }
        func pair(_ count: Int, _ name: String, _ remainder: Int, _ smaller: String) -> String {
            let main = unit(count, name)
            return remainder == 0 ? main : "\(main), \(unit(remainder, smaller))"
        }
        switch minutes {
        case ..<1: return "less than a minute"
        case ..<60: return unit(minutes, "minute")
        case ..<1_440: return pair(minutes / 60, "hour", minutes % 60, "minute")
        default: return pair(minutes / 1_440, "day", minutes / 60 % 24, "hour")
        }
    }
}

func badgeReading(_ snapshot: UsageSnapshot?, mode: String, now: Date) -> BadgeReading? {
    guard let snapshot, mode == "auto" || mode == "codex" || mode == "claude" else { return nil }
    var lowest: BadgeReading?
    for (providerIndex, provider) in snapshot.providers.enumerated() where mode == "auto" || provider.id == mode {
        for (windowIndex, window) in provider.windows.enumerated() where window.state(at: now) == .current {
            if let lowest, lowest.window.remaining <= window.remaining { continue }
            lowest = BadgeReading(providerIndex: providerIndex, windowIndex: windowIndex,
                                  providerID: provider.id, providerName: provider.name, window: window)
        }
    }
    return lowest
}

private func pinnedIndices(_ indices: Range<Int>, first: Int?) -> some RandomAccessCollection<Int> {
    let first = first.flatMap { indices.contains($0) ? $0 : nil }
    return indices.lazy.map { index in
        guard let first, index <= first else { return index }
        return index == 0 ? first : index - 1
    }
}

private typealias PanelState<Value> = SwiftUI.State<Value>

struct UsagePanel: View {
    let snapshot: UsageSnapshot?
    let error: String?
    let badge: BadgeReading?
    let now: Date
    @Binding var badgeMode: String
    @Binding var indicator: MenuIndicator
    let savingIndicator: Bool
    let monitor: ResourceMonitor
    @Binding var alertThreshold: UsageAlertThreshold
    let alertIssue: String?
    let attention: AttentionWatch
    let attentionChoice: AttentionChoice
    let savingAttention: Bool
    let setAttentionShow: (AttentionShow) -> Void
    let setAttentionState: (AttentionState, Bool) -> Void
    let jump: (AttentionEntry) -> Void
    let refresh: () -> Void
    let openSettings: () -> Void
    let quit: () -> Void
    let loginStatus: SMAppService.Status
    let setLoginEnabled: (Bool) -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @PanelState private var turn = 0
    @PanelState private var awaitingRefresh = false
    @PanelState private var blink = false

    private static let brand = Bundle.main.url(forResource: "milo-lockup", withExtension: "webp").flatMap(NSImage.init(contentsOf:))
    private static let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                if let brand = Self.brand {
                    Image(nsImage: brand).resizable().scaledToFit().frame(width: 86, height: 32)
                        .accessibilityLabel("Milo")
                } else {
                    Text("milo").font(.headline)
                }
                Spacer()
                if let version = Self.version {
                    Text(verbatim: "v\(version)")
                        .font(.system(size: 11).monospacedDigit())
                        .foregroundStyle(.secondary)
                        .accessibilityLabel("Milo version \(version)")
                }
            }
            .padding(.vertical, 10)
            if attentionChoice.show != .off {
                AttentionSection(watch: attention, now: now, jump: jump)
                Divider().padding(.bottom, 10)
            }
            if indicator != .quota {
                ResourceSection(monitor: monitor)
                Divider().padding(.bottom, 10)
            }
            usageHeading.padding(.bottom, 8)
            readings
            if let alertIssue {
                Text(alertIssue)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.bottom, 10)
            }
            Divider()
            footer
                .padding(.vertical, 10)
        }
        .padding(.horizontal, 17)
        .frame(width: 312)
        .fixedSize(horizontal: false, vertical: true)
        .onChange(of: snapshot) { _ in
            guard awaitingRefresh else { return }
            awaitingRefresh = false
            if !reduceMotion { blink = true }
        }
        .task(id: blink) {
            guard blink else { return }
            try? await Task.sleep(nanoseconds: 140_000_000)
            blink = false
        }
        .task(id: turn) {
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            if !Task.isCancelled { awaitingRefresh = false }
        }
    }

    private var usageHeading: some View {
        HStack(spacing: 10) {
            Text("Usage")
                .font(.system(size: 13, weight: .semibold))
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 4)
            Button {
                awaitingRefresh = true
                turn += 1
                refresh()
            } label: {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 12, weight: .semibold))
                    .padding(.horizontal, 3)
                    .rotationEffect(.degrees(reduceMotion ? 0 : Double(turn) * 360))
                    .animation(reduceMotion ? nil : .easeOut(duration: 0.32), value: turn)
            }
            .buttonStyle(.borderless)
            .accessibilityLabel("Refresh usage")
            .help("Refresh usage (⌘R)")
            .keyboardShortcut("r", modifiers: .command)
        }
    }

    @ViewBuilder private var readings: some View {
        if let snapshot, !snapshot.providers.isEmpty {
            if let error {
                message("Couldn’t refresh usage", detail: error, symbol: "exclamationmark.circle")
                Divider()
            }
            let badge = snapshot.matchingBadge(self.badge)
            VStack(spacing: 12) {
                ForEach(snapshot.orderedProviders(pinning: badge), id: \.id) { provider in
                    section(provider, at: now, marked: badge?.providerID == provider.id ? badge?.windowIndex : nil)
                }
            }
            .padding(.bottom, 14)
            if badge == nil && indicator == .quota && badgeMode != "icon" {
                Text("No fresh limit for the menu bar.")
                    .font(.caption).foregroundStyle(.secondary).padding(.bottom, 10)
            }
        } else if let error {
            message("Couldn’t read usage", detail: error, symbol: "exclamationmark.circle")
        } else if snapshot != nil {
            message("No usage yet", detail: "Readings appear here after your coding tools save usage on this Mac.", symbol: "gauge")
        } else {
            HStack(spacing: 10) {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityHidden(true)
                Text("Reading local usage…")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
            }
            .padding(.vertical, 21)
        }
    }

    private func section(_ provider: UsageProvider, at now: Date, marked: Int?) -> some View {
        HStack(alignment: .top, spacing: 10) {
            UsageGlyph(id: provider.id)
                .frame(width: 21, height: 21)
            VStack(alignment: .leading, spacing: 8) {
                Text(provider.name)
                    .font(.system(size: 14, weight: .semibold))
                    .frame(minHeight: 21, alignment: .leading)
                    .accessibilityAddTraits(.isHeader)
                ForEach(provider.orderedWindowIndices(pinning: marked), id: \.self) { index in
                    UsageLimitRow(window: provider.windows[index], now: now, marked: marked == index, blink: blink)
                }
                if let detail = provider.detail ?? (provider.windows.isEmpty ? "No reading yet" : nil) {
                    Text(detail)
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func message(_ title: String, detail: String, symbol: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 16))
                .foregroundStyle(.secondary)
                .frame(width: 22, height: 19)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                Text(detail)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
        }
        .padding(.vertical, 17)
    }

    private var footer: some View {
        HStack {
            Button(action: openSettings) {
                Label("Browser settings…", systemImage: "slider.horizontal.3")
            }
            .buttonStyle(.borderless)
            .help("Open Milo settings in your browser")
            .keyboardShortcut(",", modifiers: .command)
            Spacer()
            Menu {
                Picker("Menu bar indicator", selection: $indicator) {
                    ForEach(MenuIndicator.allCases, id: \.self) { choice in
                        Text(choice.title).tag(choice)
                    }
                }
                .pickerStyle(.menu)
                .disabled(savingIndicator)
                if indicator == .quota {
                    Picker("Quota display", selection: $badgeMode) {
                        Text("Automatic").tag("auto")
                        Text("Codex").tag("codex")
                        Text("Claude").tag("claude")
                        Text("Icon only").tag("icon")
                    }
                    .pickerStyle(.menu)
                }
                Divider()
                Picker("Low-quota alerts", selection: $alertThreshold) {
                    ForEach(UsageAlertThreshold.allCases, id: \.self) { threshold in
                        Text(threshold.title).tag(threshold)
                    }
                }
                .pickerStyle(.menu)
                Divider()
                Menu("Needs you") {
                    Picker("Show Rex panes that need you", selection: Binding(get: { attentionChoice.show }, set: setAttentionShow)) {
                        ForEach(AttentionShow.allCases, id: \.self) { show in
                            Text(show.title).tag(show)
                        }
                    }
                    .pickerStyle(.inline)
                    Section("Include") {
                        ForEach(AttentionState.allCases, id: \.self) { state in
                            let included = attentionChoice.states.contains(state)
                            Toggle(state.title, isOn: Binding(get: { included }, set: { setAttentionState(state, $0) }))
                                // At least one state stays chosen; turn the section off to list none.
                                .disabled(included && attentionChoice.states.count == 1)
                        }
                    }
                }
                .disabled(savingAttention)
                Divider()
                Toggle("Launch at Login", isOn: Binding(
                    get: { loginStatus == .enabled || loginStatus == .requiresApproval },
                    set: setLoginEnabled
                ))
                if loginStatus == .requiresApproval {
                    Button("Approve Launch at Login…", action: SMAppService.openSystemSettingsLoginItems)
                }
                Divider()
                Button("Quit Milo", action: quit)
                    .keyboardShortcut("q", modifiers: .command)
            } label: {
                Image(systemName: "ellipsis").frame(width: 22, height: 18)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .accessibilityLabel("More options")
            .help("More options")
            .fixedSize()
        }
        .controlSize(.small)
    }
}

private struct ResourceSection: View {
    @ObservedObject var monitor: ResourceMonitor

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text("This Mac")
                .font(.system(size: 13, weight: .semibold))
                .accessibilityAddTraits(.isHeader)
            row("CPU busy · all cores", monitor.reading.cpuPercent.map { "\($0)%" })
            row("Memory used", monitor.reading.memory?.amounts)
        }
        .padding(.bottom, 12)
    }

    private func row(_ label: String, _ value: String?) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label).foregroundStyle(.secondary)
            Spacer()
            Text(value ?? "—").fontWeight(.semibold).monospacedDigit()
        }
        .font(.system(size: 12))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue(value ?? "Not available yet")
    }
}

private struct UsageLimitRow: View {
    let window: UsageWindow
    let now: Date
    let marked: Bool
    let blink: Bool

    private func note(for state: UsageWindow.State) -> String {
        if state == .elapsed { return "Reset elapsed. Waiting for a new report." }
        if state == .historical { return "Last reported, not current" }
        guard let resetAt = window.resetAt else { return "Reset time unavailable" }
        let reset = Date(timeIntervalSince1970: Double(resetAt))
        if Calendar.current.isDate(reset, inSameDayAs: now) {
            return "Resets at \(reset.formatted(.dateTime.hour().minute()))"
        }
        return "Resets \(reset.formatted(.dateTime.month(.abbreviated).day().hour().minute()))"
    }

    var body: some View {
        let state = window.state(at: now)
        let note = note(for: state)
        let value = switch state {
        case .current: "\(window.remaining) percent left. \(note)\(marked ? ". Shown in the menu bar." : "")"
        case .historical: "\(window.remaining) percent when last reported. Not current."
        case .elapsed: note
        }
        HStack(alignment: .firstTextBaseline, spacing: 5) {
            VStack(alignment: .leading, spacing: 2) {
                Text(window.label)
                    .font(.system(size: 12, weight: .medium))
                Text(note)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 4)
            if marked {
                Image(nsImage: blink ? MiloMark.blink : MiloMark.awake)
                    .resizable().frame(width: 14, height: 14)
                    .foregroundStyle(.secondary)
                    .help("Shown in the menu bar")
            }
            if state != .elapsed {
                Text(state == .current ? "\(window.remaining)% left" : "\(window.remaining)%")
                    .font(.system(size: 15, weight: .semibold).monospacedDigit())
                    .foregroundStyle(state == .historical ? Color.secondary : window.remaining <= 10 ? Color.orange : Color.primary)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(window.label)
        .accessibilityValue(value)
    }
}

private struct UsageGlyph: View {
    let id: String

    private static let images: [String: NSImage] = {
        var images: [String: NSImage] = [:]
        for name in ["codex", "claude", "deepseek", "devin", "openrouter", "copilot", "opencode"] {
            guard let url = Bundle.main.url(forResource: name, withExtension: "png"),
                  let image = NSImage(contentsOf: url) else { continue }
            image.isTemplate = ["openrouter", "copilot", "opencode"].contains(name)
            images[name] = image
        }
        return images
    }()

    var body: some View {
        Group {
            if let image = Self.images[id] {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
            } else {
                Image(systemName: "terminal")
                    .font(.system(size: 15))
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityHidden(true)
    }
}
