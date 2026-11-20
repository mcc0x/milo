import AppKit
import SwiftUI

struct UsageSnapshot: Codable {
    let version: Int
    let providers: [UsageProvider]
}

struct UsageProvider: Codable {
    let id: String
    let name: String
    let windows: [UsageWindow]
    let detail: String?
}

struct UsageWindow: Codable {
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

struct UsagePanel: View {
    let snapshot: UsageSnapshot?
    let error: String?
    @Binding var badgeMode: String
    let refresh: () -> Void
    let openSettings: () -> Void
    let quit: () -> Void
    let loginEnabled: Bool
    let toggleLogin: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            header
                .padding(.top, 17)
                .padding(.bottom, 14)
            Divider()
            readings
            Divider()
            footer
                .padding(.vertical, 10)
        }
        .padding(.horizontal, 17)
        .frame(width: 348)
        .fixedSize(horizontal: false, vertical: true)
    }

    private var header: some View {
        HStack(spacing: 10) {
            UsageGlyph(id: "milo")
                .frame(width: 34, height: 34)
            VStack(alignment: .leading, spacing: 2) {
                Text("Usage")
                    .font(.system(size: 19, weight: .semibold))
                    .accessibilityAddTraits(.isHeader)
                Text("Limits left and activity on this Mac")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 4)
            Button(action: refresh) {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 12, weight: .semibold))
                    .padding(.horizontal, 3)
            }
            .controlSize(.small)
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
                VStack(spacing: 0) {
                    ForEach(snapshot.providers, id: \.id) { provider in
                        if provider.id != snapshot.providers.first?.id { Divider() }
                        section(provider, at: context.date)
                            .padding(.vertical, 12)
                    }
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

    private func section(_ provider: UsageProvider, at now: Date) -> some View {
        HStack(alignment: .top, spacing: 10) {
            UsageGlyph(id: provider.id)
                .frame(width: 21, height: 21)
            VStack(alignment: .leading, spacing: 8) {
                Text(provider.name)
                    .font(.system(size: 14, weight: .semibold))
                    .frame(minHeight: 21, alignment: .leading)
                    .accessibilityAddTraits(.isHeader)
                ForEach(provider.windows.indices, id: \.self) { index in
                    UsageLimitRow(window: provider.windows[index], now: now)
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
            .help("Open Milo web settings")
            .keyboardShortcut(",", modifiers: .command)
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
                    get: { loginEnabled },
                    set: { if $0 != loginEnabled { toggleLogin() } }
                ))
            } label: {
                Label("Options", systemImage: "gearshape")
            }
            .fixedSize()
            Spacer()
            Button("Quit Milo", action: quit)
                .keyboardShortcut("q", modifiers: .command)
        }
        .controlSize(.small)
    }
}

private struct UsageLimitRow: View {
    let window: UsageWindow
    let now: Date

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

    private var meterColor: Color {
        guard window.fresh else { return Color(nsColor: .tertiaryLabelColor) }
        switch window.remaining {
        case ...10: return Color(nsColor: .systemRed)
        case ...30: return Color(nsColor: .systemOrange)
        default: return Color(nsColor: .systemBlue)
        }
    }

    var body: some View {
        let elapsed = window.resetHasElapsed(at: now)
        let note = note(elapsed: elapsed)
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text(window.label)
                    .font(.system(size: 12, weight: .medium))
                Spacer(minLength: 0)
                if !elapsed {
                    Text("\(window.remaining)%")
                        .font(.system(size: 15, weight: .semibold).monospacedDigit())
                        .foregroundStyle(window.fresh ? Color.primary : Color.secondary)
                }
            }
            if !elapsed {
                GeometryReader { geometry in
                    Capsule().fill(Color(nsColor: .separatorColor))
                        .overlay(alignment: .leading) {
                            Capsule().fill(meterColor)
                                .frame(width: geometry.size.width * CGFloat(min(100, max(0, window.remaining))) / 100)
                        }
                }
                .frame(height: 4)
            }
            Text(note)
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(window.label)
        .accessibilityValue(elapsed ? note : "\(window.remaining) percent remaining. \(note)")
    }
}

private struct UsageGlyph: View {
    let id: String

    private static let milo = load("milo")
    private static let codex = load("codex")
    private static let claude = load("claude")

    private static func load(_ name: String) -> NSImage? {
        guard let url = Bundle.main.url(forResource: name, withExtension: "png") else { return nil }
        return NSImage(contentsOf: url)
    }

    private var image: NSImage? {
        switch id {
        case "milo": return Self.milo
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
                Image(systemName: id == "milo" ? "gauge" : "terminal")
                    .font(.system(size: id == "milo" ? 22 : 15))
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityHidden(true)
    }
}
