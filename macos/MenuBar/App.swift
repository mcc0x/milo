import AppKit
import ServiceManagement
import SwiftUI

@MainActor
private final class Model: ObservableObject {
    @Published var snapshot: UsageSnapshot?
    @Published var failure: String?
    @Published var loginEnabled = SMAppService.mainApp.status == .enabled
    @Published var mode = UserDefaults.standard.string(forKey: "badgeMode") ?? "auto" {
        didSet { UserDefaults.standard.set(mode, forKey: "badgeMode"); changed?() }
    }
    var changed: (() -> Void)?
    private var child: Process?
    private var input: FileHandle?
    private let reader = DispatchQueue(label: "net.usemilo.usage.frames", qos: .utility)
    private static let pathKeys = ["CODEX_HOME", "CLAUDE_CONFIG_DIR", "PI_CODING_AGENT_DIR", "OPENCODE_DB", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_DATA_HOME"]

    private var settings: Process?
    private var settingsURL: URL?

    private func bundledProcess(_ arguments: [String]) -> Process? {
        guard let tool = Bundle.main.url(forResource: "milo", withExtension: nil, subdirectory: "Tools") else { return nil }
        var environment = ["HOME": NSHomeDirectory(), "PATH": "/usr/bin:/bin"]
        // Launch Services need not inherit a shell environment. The local builder records
        // only provider/config paths, never the shell's tokens or other variables.
        if let url = Bundle.main.url(forResource: "ProfilePaths", withExtension: "plist"),
           let data = try? Data(contentsOf: url),
           let profile = try? PropertyListDecoder().decode([String:String].self, from: data) {
            for key in Self.pathKeys { environment[key] = profile[key] }
        }
        let process = Process()
        process.executableURL = tool; process.arguments = arguments
        process.environment = environment
        process.currentDirectoryURL = FileManager.default.homeDirectoryForCurrentUser
        return process
    }

    // A menu-launched editor belongs to the browser: allow it to finish a removal
    // and keep unsaved drafts when the menu app quits. It has its own idle timeout.
    private var independentSettings = false
    init() {
        if let path = ProcessInfo.processInfo.environment["MILO_MENU_INSTALLER"],
           path.hasPrefix("/"), FileManager.default.isExecutableFile(atPath: path),
           Bundle.main.bundleURL.standardizedFileURL.path == FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Applications/Milo Usage.app").standardizedFileURL.path {
            UserDefaults.standard.set(path, forKey: "installerPath")
        }
    }

    func openSettings() {
        if settings?.isRunning == true {
            if let settingsURL { openBrowser(settingsURL) }
            return
        }
        settingsURL = nil
        guard let process = bundledProcess(["web", "--port", "0", "--no-open"]) else { settingsError(); return }
        let defaults = UserDefaults.standard
        independentSettings = false
        if let path = ProcessInfo.processInfo.environment["MILO_MENU_INSTALLER"] ?? defaults.string(forKey: "installerPath"), path.hasPrefix("/"),
           FileManager.default.isExecutableFile(atPath: path) {
            process.executableURL = URL(fileURLWithPath: path)
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
        DispatchQueue.global(qos: .userInitiated).async {
            let pipe = output.fileHandleForReading
            defer { try? pipe.close() }
            var data = Data()
            while data.count < 512 && !data.contains(10) {
                let bytes = pipe.availableData
                if bytes.isEmpty { break }
                data.append(bytes)
            }
            let link = String(decoding: data.prefix(while: { $0 != 10 }), as: UTF8.self)
            DispatchQueue.main.async { [weak self] in
                guard let self, self.settings === process, process.isRunning else { return }
                guard let url = URL(string: link), url.scheme == "http", url.host == "127.0.0.1",
                      url.port != nil, url.fragment?.isEmpty == false else {
                    self.stopSettings(); self.settingsError(); return
                }
                self.settingsURL = url
                self.openBrowser(url)
            }
        }
    }

    private func openBrowser(_ url: URL) {
        if !NSWorkspace.shared.open(url) { settingsError() }
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
                            self.snapshot = frame; self.failure = nil; self.changed?()
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
    func refresh() {
        guard let input else { start(); return }
        do { try input.write(contentsOf: Data([10])) } catch { stop(); start() }
    }
    func stop() {
        try? input?.close(); input = nil
        if let process = child, process.isRunning { process.terminate() }
        child = nil
    }
    func toggleLogin() {
        do {
            switch SMAppService.mainApp.status {
            case .enabled: try SMAppService.mainApp.unregister()
            case .requiresApproval: SMAppService.openSystemSettingsLoginItems()
            default:
                try SMAppService.mainApp.register()
                if SMAppService.mainApp.status == .requiresApproval { SMAppService.openSystemSettingsLoginItems() }
            }
        } catch {
            let alert = NSAlert(); alert.messageText = "Couldn’t change Launch at Login"; alert.informativeText = error.localizedDescription; alert.runModal()
        }
        loginEnabled = SMAppService.mainApp.status == .enabled
    }
}

private struct Content: View {
    @ObservedObject var model: Model
    var body: some View {
        UsagePanel(snapshot: model.snapshot, error: model.failure, badgeMode: $model.mode,
                   refresh: model.refresh, openSettings: model.openSettings, quit: { NSApp.terminate(nil) },
                   loginEnabled: model.loginEnabled, toggleLogin: model.toggleLogin).fixedSize()
    }
}

@MainActor
private final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = Model()
    private let popover = NSPopover()
    private var status: NSStatusItem!

    func applicationDidFinishLaunching(_ notification: Notification) {
        status = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        status.autosaveName = "Milo Usage"
        if let button = status.button {
            button.image = MiloMark.awake
            button.target = self; button.action = #selector(toggle); button.imagePosition = .imageLeading
            button.imageScaling = .scaleProportionallyDown
            button.font = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .medium)
        }
        popover.behavior = .transient
        let host = NSHostingController(rootView: Content(model: model)); host.sizingOptions = [.preferredContentSize]; popover.contentViewController = host
        model.changed = { [weak self] in self?.updateBadge() }
        updateBadge(); model.start()
        NSWorkspace.shared.notificationCenter.addObserver(self, selector: #selector(woke), name: NSWorkspace.didWakeNotification, object: nil)
        if CommandLine.arguments.contains("--show") { show() }
        if CommandLine.arguments.contains("--settings") { model.openSettings() }
    }
    @objc private func woke() {
        // A badge that was fresh before sleep must be rechecked before showing it again.
        model.snapshot = nil
        updateBadge()
        model.refresh()
    }
    private func updateBadge() {
        guard let button = status?.button else { return }
        let reading = badgeReading(model.snapshot, mode: model.mode)
        let label = reading.map { "\($0.provider.name), \($0.window.label): \($0.window.remaining)% left" } ?? "Milo Usage"
        button.title = reading.map { " \($0.window.remaining)%" } ?? ""
        status.length = reading == nil ? NSStatusItem.variableLength : 67
        button.toolTip = label; button.setAccessibilityLabel(label)
    }
    @objc private func toggle() { if popover.isShown { popover.performClose(nil) } else { show() } }
    private func show() {
        guard let button = status?.button else { return }
        model.loginEnabled = SMAppService.mainApp.status == .enabled
        updateBadge(); popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        popover.contentViewController?.view.window?.makeKey()
    }
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool { show(); return false }
    func applicationWillTerminate(_ notification: Notification) { NSWorkspace.shared.notificationCenter.removeObserver(self); model.quit() }
}

@main
private struct MiloMenuApp {
    @MainActor static func main() {
        if CommandLine.arguments.count == 3,
           ["--stop-installed", "--remove-installed"].contains(CommandLine.arguments[1]) {
            let target = URL(fileURLWithPath: CommandLine.arguments[2]).standardizedFileURL
            do {
                if CommandLine.arguments[1] == "--remove-installed" {
                    guard Bundle.main.bundleURL.standardizedFileURL.path == target.path else { exit(1) }
                    let service = SMAppService.mainApp
                    if service.status == .enabled || service.status == .requiresApproval {
                        try service.unregister()
                    }
                }
                let running = NSRunningApplication.runningApplications(withBundleIdentifier: Bundle.main.bundleIdentifier!)
                    .filter { $0.processIdentifier != ProcessInfo.processInfo.processIdentifier && $0.bundleURL?.standardizedFileURL.path == target.path }
                for app in running { _ = app.terminate() }
                let deadline = Date().addingTimeInterval(5)
                while running.contains(where: { !$0.isTerminated }) && Date() < deadline {
                    RunLoop.current.run(until: Date().addingTimeInterval(0.05))
                }
                exit(running.allSatisfy(\.isTerminated) ? 0 : 1)
            } catch {
                FileHandle.standardError.write(Data("Could not unregister login item: \(error.localizedDescription)\n".utf8))
                exit(1)
            }
        }
        signal(SIGPIPE, SIG_IGN)
        let app = NSApplication.shared; app.setActivationPolicy(.accessory)
        let delegate = AppDelegate(); app.delegate = delegate
        withExtendedLifetime(delegate) { app.run() }
    }
}
