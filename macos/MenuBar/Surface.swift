import AppKit
import ServiceManagement
import SwiftUI

struct UsageSnapshot: Codable, Equatable {
    let version: Int
    let providers: [UsageProvider]
}

struct UsageProvider: Codable, Equatable {
    let id: String
    let name: String
    let windows: [UsageWindow]
    let detail: String?
}

struct UsageWindow: Codable, Equatable {
    let label: String
    let remaining: Int
    let fresh: Bool
    let resetAt: Int?

    fileprivate func resetHasElapsed(at now: Date) -> Bool {
        guard let resetAt else { return false }
        return Double(resetAt) <= now.timeIntervalSince1970
    }
}

func badgeReading(_ snapshot: UsageSnapshot?, mode: String) -> (provider: UsageProvider, window: UsageWindow)? {
    guard let snapshot, mode == "auto" || mode == "codex" || mode == "claude" else { return nil }
    let now = Date()
    var lowest: (provider: UsageProvider, window: UsageWindow)?
    for provider in snapshot.providers where mode == "auto" || provider.id == mode {
        for window in provider.windows where window.fresh && !window.resetHasElapsed(at: now) {
            if let lowest, lowest.window.remaining <= window.remaining { continue }
            lowest = (provider, window)
        }
    }
    return lowest
}

private typealias PanelState<Value> = SwiftUI.State<Value>

struct UsagePanel: View {
    let snapshot: UsageSnapshot?
    let error: String?
    @Binding var badgeMode: String
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
            usageHeading.padding(.bottom, 8)
            readings
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
            TimelineView(.periodic(from: .now, by: 30)) { context in
                let active = badgeReading(snapshot, mode: badgeMode)
                VStack(spacing: 12) {
                    ForEach(snapshot.providers, id: \.id) { provider in
                        section(provider, at: context.date, marked: active?.provider.id == provider.id ? active?.window : nil)
                    }
                }
                .padding(.bottom, 14)
                if active == nil && badgeMode != "icon" {
                    Text("No fresh limit for the menu bar.")
                        .font(.caption).foregroundStyle(.secondary).padding(.bottom, 10)
                }
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

    private func section(_ provider: UsageProvider, at now: Date, marked: UsageWindow?) -> some View {
        HStack(alignment: .top, spacing: 10) {
            UsageGlyph(id: provider.id)
                .frame(width: 21, height: 21)
            VStack(alignment: .leading, spacing: 8) {
                Text(provider.name)
                    .font(.system(size: 14, weight: .semibold))
                    .frame(minHeight: 21, alignment: .leading)
                    .accessibilityAddTraits(.isHeader)
                ForEach(provider.windows.indices, id: \.self) { index in
                    UsageLimitRow(window: provider.windows[index], now: now, marked: marked == provider.windows[index], blink: blink)
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
                Label("Settings…", systemImage: "slider.horizontal.3")
            }
            .buttonStyle(.borderless)
            .help("Open Milo web settings")
            .keyboardShortcut(",", modifiers: .command)
            Spacer()
            Menu {
                Picker("Menu bar display", selection: $badgeMode) {
                    Text("Automatic").tag("auto")
                    Text("Codex").tag("codex")
                    Text("Claude").tag("claude")
                    Text("Icon only").tag("icon")
                }
                .pickerStyle(.menu)
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

private struct UsageLimitRow: View {
    let window: UsageWindow
    let now: Date
    let marked: Bool
    let blink: Bool

    private func note(elapsed: Bool) -> String {
        if elapsed { return "Reset elapsed. Waiting for a new report." }
        if !window.fresh { return "Older reading" }
        guard let resetAt = window.resetAt else { return "Reset time unavailable" }
        let reset = Date(timeIntervalSince1970: Double(resetAt))
        if Calendar.current.isDate(reset, inSameDayAs: now) {
            return "Resets at \(reset.formatted(.dateTime.hour().minute()))"
        }
        return "Resets \(reset.formatted(.dateTime.month(.abbreviated).day().hour().minute()))"
    }


    var body: some View {
        let elapsed = window.resetHasElapsed(at: now)
        let note = note(elapsed: elapsed)
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
            if !elapsed {
                Text("\(window.remaining)%")
                    .font(.system(size: 15, weight: .semibold).monospacedDigit())
                    .foregroundStyle(!window.fresh ? Color.secondary : window.remaining <= 10 ? Color.orange : Color.primary)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(window.label)
        .accessibilityValue(elapsed ? note : "\(window.remaining) percent remaining. \(note)\(marked ? ". Shown in the menu bar." : "")")
    }
}

private struct UsageGlyph: View {
    let id: String

    private static let codex = load("codex")
    private static let claude = load("claude")

    private static func load(_ name: String) -> NSImage? {
        guard let url = Bundle.main.url(forResource: name, withExtension: "png") else { return nil }
        return NSImage(contentsOf: url)
    }

    private var image: NSImage? {
        switch id {
        case "codex": return Self.codex
        case "claude": return Self.claude
        default: return nil
        }
    }

    var body: some View {
        Group {
            if let image {
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
