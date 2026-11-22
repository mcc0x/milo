import AppKit
import CryptoKit
import Darwin
import ServiceManagement
import SwiftUI

private struct MenuProfile: Codable {
    private static let pathKeys = ["CODEX_HOME", "CLAUDE_CONFIG_DIR", "PI_CODING_AGENT_DIR", "OPENCODE_DB", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_DATA_HOME"]
    // Milo resolves these against its working directory, so a relative one is captured in full.
    // It ignores a relative XDG directory, and an OpenCode database name (or :memory:) is
    // relative to OpenCode's data folder: those stay as set, to mean here what they meant there.
    private static let workingDirectoryKeys: Set = ["CODEX_HOME", "CLAUDE_CONFIG_DIR", "PI_CODING_AGENT_DIR"]

    struct Installer: Codable {
        let path: String
        let digest: Data

        private static func hash(_ path: String) -> Data? {
            guard MenuProfile.validPath(path, absolute: true),
                  let file = MenuProfile.openRegularFile(path) else { return nil }
            defer { try? file.close() }
            do {
                var hash = SHA256()
                while let data = try file.read(upToCount: 64 * 1024), !data.isEmpty { hash.update(data: data) }
                return Data(hash.finalize())
            } catch {
                return nil
            }
        }

        init?(path: String) {
            guard let digest = Self.hash(path) else { return nil }
            self.path = path
            self.digest = digest
        }

        var executable: URL? {
            guard FileManager.default.isExecutableFile(atPath: path),
                  Self.hash(path) == digest else { return nil }
            return URL(fileURLWithPath: path)
        }
    }

    let paths: [String: String]
    let installer: Installer

    static func write(installer path: String) throws {
        guard let installer = Installer(path: path) else { throw CocoaError(.fileReadUnknown) }
        let source = ProcessInfo.processInfo.environment
        let cwd = FileManager.default.currentDirectoryPath
        var paths: [String: String] = [:]
        for key in pathKeys {
            guard let value = source[key], validPath(value) else { continue }
            paths[key] = value.hasPrefix("/") || !workingDirectoryKeys.contains(key) ? value : cwd + "/" + value
        }
        let encoder = PropertyListEncoder()
        encoder.outputFormat = .xml
        try FileHandle.standardOutput.write(contentsOf: encoder.encode(Self(paths: paths, installer: installer)))
    }

    static func installed() -> Self? {
        let apps = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Applications", isDirectory: true)
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: apps.path),
              attributes[.type] as? FileAttributeType == .typeDirectory,
              (attributes[.ownerAccountID] as? NSNumber)?.uint32Value == getuid(),
              ((attributes[.posixPermissions] as? NSNumber)?.uint16Value ?? 0o022) & 0o022 == 0,
              Bundle.main.bundleURL.resolvingSymlinksInPath().path ==
                apps.appendingPathComponent("Milo.app", isDirectory: true).resolvingSymlinksInPath().path,
              let file = openRegularFile(apps.appendingPathComponent(".milo-menu-profile.plist").path, privateProfile: true)
        else { return nil }
        defer { try? file.close() }
        guard let data = try? file.readToEnd() else { return nil }
        return try? PropertyListDecoder().decode(Self.self, from: data)
    }

    func applyPaths(to environment: inout [String: String]) {
        for key in Self.pathKeys {
            if let path = paths[key], Self.validPath(path, absolute: Self.workingDirectoryKeys.contains(key)) {
                environment[key] = path
            }
        }
    }

    private static func validPath(_ path: String, absolute: Bool = false) -> Bool {
        !path.isEmpty && !path.utf8.contains(0) && (!absolute || path.hasPrefix("/"))
    }

    private static func openRegularFile(_ path: String, privateProfile: Bool = false) -> FileHandle? {
        let flags = O_RDONLY | O_NONBLOCK | O_CLOEXEC | (privateProfile ? O_NOFOLLOW : 0)
        let fd = open(path, flags)
        guard fd >= 0 else { return nil }
        var info = stat()
        guard fstat(fd, &info) == 0, info.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG),
              !privateProfile || (info.st_uid == getuid() && info.st_mode & 0o777 == 0o600)
        else {
            close(fd)
            return nil
        }
        return FileHandle(fileDescriptor: fd, closeOnDealloc: true)
    }
}

@MainActor
private final class Model: ObservableObject {
    @Published var snapshot: UsageSnapshot?
    @Published var failure: String?
    @Published private(set) var loginStatus = SMAppService.mainApp.status
    @Published var mode = UserDefaults.standard.string(forKey: "badgeMode") ?? "auto" {
        didSet { UserDefaults.standard.set(mode, forKey: "badgeMode"); changed?() }
    }
    @Published private(set) var badge: BadgeReading?
    private(set) var badgeUpdatedAt = Date.now
    var changed: (() -> Void)?
    private var child: Process?
    private var input: FileHandle?
    private let reader = DispatchQueue(label: "net.usemilo.usage.frames", qos: .utility)
    let alerts = UsageAlerts()

    private var settings: Process?
    private var settingsURL: URL?
    private let profile = MenuProfile.installed()

    private func bundledProcess(_ arguments: [String]) -> Process? {
        guard let tool = Bundle.main.url(forResource: "milo", withExtension: nil, subdirectory: "Tools") else { return nil }
        var environment = ["HOME": NSHomeDirectory(), "PATH": "/usr/bin:/bin"]
        profile?.applyPaths(to: &environment)
        let process = Process()
        process.executableURL = tool; process.arguments = arguments
        process.environment = environment
        process.currentDirectoryURL = FileManager.default.homeDirectoryForCurrentUser
        return process
    }

    private var handoffs: [URL: BrowserHandoff] = [:]
    // A menu-launched editor belongs to the browser: allow it to finish a removal
    // and keep unsaved drafts when the menu app quits. It has its own idle timeout.
    private var independentSettings = false

    func openSettings() {
        if settings?.isRunning == true {
            if let settingsURL { openBrowser(settingsURL) }
            return
        }
        settingsURL = nil
        guard let process = bundledProcess(["web", "--port", "0", "--no-open"]) else { settingsError(); return }
        independentSettings = false
        // A replaced installer path must not turn Settings into a launcher for another program.
        if let executable = profile?.installer.executable {
            process.executableURL = executable
            independentSettings = true
        }
        let output = Pipe()
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        process.terminationHandler = { [weak self] child in
            DispatchQueue.main.async {
                guard let self, self.settings === child else { return }
                self.settings = nil; self.settingsURL = nil
                self.refresh()
                if child.terminationStatus != 0 { self.settingsError() }
            }
        }
        do { try process.run() } catch { settingsError(); return }
        settings = process
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let pipe = output.fileHandleForReading
            defer { try? pipe.close() }
            var data = Data()
            while data.count < 512 && !data.contains(10) {
                let bytes = pipe.availableData
                if bytes.isEmpty { break }
                data.append(contentsOf: bytes.prefix(512 - data.count))
            }
            let link = data.firstIndex(of: 10).map { String(decoding: data[..<$0], as: UTF8.self) }
            DispatchQueue.main.async { [weak self] in
                guard let self, self.settings === process, process.isRunning else { return }
                guard let link, let url = URL(string: link), url.scheme == "http", url.host == "127.0.0.1",
                      url.port != nil, url.fragment?.isEmpty == false else {
                    self.stopSettings(); self.settingsError(); return
                }
                self.settingsURL = url
                self.openBrowser(url)
            }
        }
    }

    private func openBrowser(_ url: URL) {
        guard let handoff = BrowserHandoff(url), NSWorkspace.shared.open(handoff.file) else { settingsError(); return }
        let file = handoff.file
        handoffs[file] = handoff
        DispatchQueue.main.asyncAfter(deadline: .now() + 15) { [weak self] in self?.handoffs[file] = nil }
    }
    private func settingsError() {
        let alert = NSAlert()
        alert.messageText = "Couldn’t open Milo Settings"
        alert.informativeText = "Try again, or run milo web from your terminal."
        alert.runModal()
    }
    func stopSettings() {
        if let settings, settings.isRunning { settings.terminate() }
        settings = nil; settingsURL = nil
    }

    func quit() {
        handoffs.removeAll()
        if !independentSettings { stopSettings() }
        stop()
    }

    func start() {
        guard child == nil else { return }
        guard let process = bundledProcess(["usage", "--menu-stream"]) else {
            failed("The bundled Milo reader is missing. Rebuild the menu app."); return
        }
        let incoming = Pipe(), outgoing = Pipe()
        process.standardInput = incoming; process.standardOutput = outgoing; process.standardError = FileHandle.nullDevice
        do { try process.run() } catch { failed("Couldn’t start the local reader. Click refresh to retry."); return }
        child = process; input = incoming.fileHandleForWriting; failure = nil
        let output = outgoing.fileHandleForReading
        reader.async { [weak self] in
            defer { try? output.close() }
            var pending = Data()
            let decoder = JSONDecoder()
            do {
                while true {
                    let bytes = output.availableData
                    if bytes.isEmpty { break }
                    pending.append(bytes)
                    while let newline = pending.firstIndex(of: 10) {
                        guard newline - pending.startIndex <= 65536 else { throw FeedError.invalid }
                        let frame = try decoder.decode(UsageSnapshot.self, from: pending[..<newline]); guard frame.version == 1 else { throw FeedError.invalid }
                        pending.removeSubrange(...newline)
                        DispatchQueue.main.async { [weak self] in
                            guard let self, self.child === process else { return }
                            self.receive(frame)
                        }
                    }
                    guard pending.count <= 65536 else { throw FeedError.invalid }
                }
            } catch { /* Report below. Never display malformed data as a limit. */ }
            DispatchQueue.main.async { [weak self] in
                guard let self, self.child === process else { return }
                self.stop(); self.failed("Usage reader disconnected. Click refresh to reconnect.")
            }
        }
    }

    private enum FeedError: Error { case invalid }
    private func failed(_ message: String) { snapshot = nil; failure = message; changed?() }
    func receive(_ frame: UsageSnapshot) {
        // The reader repeats a settled frame about every 30 seconds. Publishing an equal one
        // would have SwiftUI work the whole panel out again, on screen or not.
        if snapshot != frame { snapshot = frame }
        if failure != nil { failure = nil }
        changed?()
        alerts.observe(frame)
    }
    func updateBadge(at now: Date, panelShown: Bool) {
        // The panel's rows read the time too, so one on screen is redrawn at every tick.
        if panelShown { objectWillChange.send() }
        badgeUpdatedAt = now
        let reading = badgeReading(snapshot, mode: mode, now: now)
        if badge != reading { badge = reading }
    }
    func refresh() {
        guard let input else { start(); return }
        do { try input.write(contentsOf: Data([10])) } catch { stop(); start() }
    }
    func stop() {
        alerts.clearBaseline()
        try? input?.close(); input = nil
        if let process = child, process.isRunning { process.terminate() }
        child = nil
    }
    func refreshLoginStatus() { loginStatus = SMAppService.mainApp.status }

    func setLoginEnabled(_ enabled: Bool) {
        let service = SMAppService.mainApp
        do {
            let status = service.status
            let registered = status == .enabled || status == .requiresApproval
            if enabled && !registered {
                try service.register()
                if service.status == .requiresApproval { SMAppService.openSystemSettingsLoginItems() }
            } else if !enabled && registered {
                try service.unregister()
            }
        } catch {
            let alert = NSAlert(); alert.messageText = "Couldn’t change Launch at Login"; alert.informativeText = error.localizedDescription; alert.runModal()
        }
        refreshLoginStatus()
    }
}

private struct Content: View {
    @ObservedObject var model: Model
    @ObservedObject var alerts: UsageAlerts
    var body: some View {
        UsagePanel(snapshot: model.snapshot, error: model.failure, badge: model.badge, now: model.badgeUpdatedAt, badgeMode: $model.mode,
                   alertThreshold: Binding(get: { alerts.threshold }, set: { alerts.select($0) }), alertIssue: alerts.issue,
                   refresh: model.refresh, openSettings: model.openSettings, quit: { NSApp.terminate(nil) },
                   loginStatus: model.loginStatus, setLoginEnabled: model.setLoginEnabled).fixedSize()
    }
}

@MainActor
private final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = Model()
    private let popover = NSPopover()
    private var status: NSStatusItem!

    func applicationDidFinishLaunching(_ notification: Notification) {
        model.alerts.start()
        status = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        status.autosaveName = "Milo Usage"
        if let button = status.button {
            button.image = MiloMark.awake
            button.target = self; button.action = #selector(toggle); button.imagePosition = .imageLeading
            button.imageScaling = .scaleProportionallyDown
            button.font = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .medium)
        }
        popover.behavior = .transient
        let host = NSHostingController(rootView: Content(model: model, alerts: model.alerts)); host.sizingOptions = [.preferredContentSize]; popover.contentViewController = host
        model.changed = { [weak self] in self?.updateBadge() }
        updateBadge(); model.start()
        NSWorkspace.shared.notificationCenter.addObserver(self, selector: #selector(woke), name: NSWorkspace.didWakeNotification, object: nil)
        if CommandLine.arguments.contains("--show") { show() }
        if CommandLine.arguments.contains("--settings") { model.openSettings() }
    }
    @objc private func woke() {
        // A badge that was fresh before sleep must be rechecked before showing it again.
        model.snapshot = nil
        model.alerts.clearBaseline()
        updateBadge()
        model.refresh()
    }
    // The popover can go on reporting itself shown after its window has left the screen.
    private var panelOnScreen: Bool {
        guard let host = popover.contentViewController, host.isViewLoaded else { return false }
        return host.view.window?.isVisible == true
    }
    private func updateBadge(opening: Bool = false) {
        guard let button = status?.button else { return }
        // The reader emits even unchanged frames, at most 30 seconds apart when settled.
        // Use those ticks for the countdown and share the chosen window with the popover.
        let now = Date.now
        model.updateBadge(at: now, panelShown: opening || panelOnScreen)
        let label = model.badge?.label(at: now) ?? "Milo"
        let title = model.badge.map { " \($0.window.remaining)%" } ?? ""
        if button.title != title {
            button.title = title
            status.length = model.badge == nil ? NSStatusItem.variableLength : 67
        }
        if button.toolTip != label {
            button.toolTip = label; button.setAccessibilityLabel(label)
        }
    }
    @objc private func toggle() { if popover.isShown { popover.performClose(nil) } else { show() } }
    private func show() {
        guard let button = status?.button else { return }
        model.refreshLoginStatus()
        model.alerts.refreshPermission()
        updateBadge(opening: true); popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        popover.contentViewController?.view.window?.makeKey()
    }
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool { show(); return false }
    func applicationWillTerminate(_ notification: Notification) { NSWorkspace.shared.notificationCenter.removeObserver(self); model.quit() }
}

@main
private struct MiloMenuApp {
    @MainActor static func main() {
        if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--install-profile" {
            do { try MenuProfile.write(installer: CommandLine.arguments[2]) } catch { exit(1) }
            exit(0)
        }
        if CommandLine.arguments.count == 3,
           ["--stop-installed", "--remove-installed"].contains(CommandLine.arguments[1]) {
            let target = URL(fileURLWithPath: CommandLine.arguments[2]).resolvingSymlinksInPath().path
            let removing = CommandLine.arguments[1] == "--remove-installed"
            guard let identifier = Bundle.main.bundleIdentifier,
                  !removing || Bundle.main.bundleURL.resolvingSymlinksInPath().path == target else { exit(1) }
            let running = NSRunningApplication.runningApplications(withBundleIdentifier: identifier)
                .filter { $0.processIdentifier != ProcessInfo.processInfo.processIdentifier && $0.bundleURL?.resolvingSymlinksInPath().path == target }
            for app in running { _ = app.terminate() }
            let deadline = DispatchTime.now() + .seconds(5)
            while running.contains(where: { !$0.isTerminated }) && DispatchTime.now() < deadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.05))
            }
            guard running.allSatisfy(\.isTerminated) else { exit(1) }
            if removing {
                let service = SMAppService.mainApp
                do {
                    if service.status == .enabled || service.status == .requiresApproval {
                        try service.unregister()
                    }
                } catch {
                    FileHandle.standardError.write(Data("Could not unregister login item: \(error.localizedDescription)\n".utf8))
                    exit(1)
                }
            }
            exit(0)
        }
        signal(SIGPIPE, SIG_IGN)
        let app = NSApplication.shared; app.setActivationPolicy(.accessory)
        let delegate = AppDelegate(); app.delegate = delegate
        withExtendedLifetime(delegate) { app.run() }
    }
}
