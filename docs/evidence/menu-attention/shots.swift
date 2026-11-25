// Appended to macos/MenuBar/App.swift by walkthrough.py, in place of the app's entry point, so the
// real model, status item and popover run with the real bundled Milo. It takes one command a line
// on standard input and answers each with one line, so the walkthrough can drive the menu and
// save pictures of the app's own views. It installs nothing and reads no screen but its own.
//
// The Needs you choices are made in the real More options menu, with arrow keys and Return put in
// the app's own event queue. A row has no key of its own, so `jump` and `seen` call the action the
// row's button is wired to: the pointer path from a click on a row to that action is not exercised.
// `indicator` calls the action of the indicator picker in the same way.

private extension AppDelegate {
    var panel: NSView? { popover.contentViewController?.view }

    /// The popover as the system composes it. It is the app's own window, so no Screen Recording
    /// permission is asked for and nothing of another app is read.
    func picture(of window: NSWindow) -> Data? {
        guard window.isVisible, let number = CGWindowID(exactly: window.windowNumber),
              let shot = CGWindowListCreateImage(.null, .optionIncludingWindow, number, [.boundsIgnoreFraming, .bestResolution]),
              shot.width > 0, shot.height > 0 else { return nil }
        return NSBitmapImageRep(cgImage: shot).representation(using: .png, properties: [:])
    }

    /// The menu bar item's contents. The system draws the item in a process of its own, so this app has
    /// no window of it to capture: the image and title the app gave the item are drawn here, at twice
    /// their size, on a strip of the bar's light or dark appearance. It is not a picture of the menu bar.
    func statusPicture() -> Data? {
        guard let button = status?.button, let mark = button.image else { return nil }
        let dark = button.effectiveAppearance.bestMatch(from: [.aqua, .darkAqua, .vibrantLight, .vibrantDark])
            .map { [.darkAqua, .vibrantDark].contains($0) } ?? false
        let ink = dark ? NSColor.white : NSColor.black
        let font = button.font ?? NSFont.menuBarFont(ofSize: 0)
        let title = NSAttributedString(string: button.title, attributes: [.font: font, .foregroundColor: ink])
        let size = NSSize(width: 10 + mark.size.width + ceil(title.size().width) + 10, height: 24)
        let tinted = NSImage(size: mark.size, flipped: false) { rect in
            mark.draw(in: rect)
            ink.set()
            rect.fill(using: .sourceAtop)
            return true
        }
        guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width) * 2, pixelsHigh: Int(size.height) * 2,
                                         bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                         colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
              let context = NSGraphicsContext(bitmapImageRep: rep) else { return nil }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        context.cgContext.scaleBy(x: 2, y: 2)
        NSColor(white: dark ? 0.16 : 0.93, alpha: 1).setFill()
        NSRect(origin: .zero, size: size).fill()
        tinted.draw(in: NSRect(x: 10, y: (size.height - mark.size.height) / 2, width: mark.size.width, height: mark.size.height))
        title.draw(at: NSPoint(x: 10 + mark.size.width, y: (size.height - title.size().height) / 2))
        NSGraphicsContext.restoreGraphicsState()
        return rep.representation(using: .png, properties: [:])
    }

    func save(_ data: Data?, to path: String) -> String {
        guard let data else { return "fail nothing to draw" }
        do { try data.write(to: URL(fileURLWithPath: path)); return "ok" } catch { return "fail \(error.localizedDescription)" }
    }

    /// Every view under `view`, depth first.
    func views(under view: NSView) -> [NSView] {
        [view] + view.subviews.flatMap { views(under: $0) }
    }

    /// The app's open menus as the system composes them, side by side as they are on screen.
    /// A menu is a window of this app that AppKit does not list. The system's window list gives the
    /// app its own windows' numbers and places; only those are asked for.
    func menusPicture() -> Data? {
        let listed = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[CFString: Any]] ?? []
        let menus = listed.compactMap { window -> (CGRect, CGImage)? in
            guard (window[kCGWindowOwnerPID] as? pid_t) == getpid(), let id = window[kCGWindowNumber] as? CGWindowID,
                  Int(id) != panel?.window?.windowNumber,
                  let place = (window[kCGWindowBounds] as? NSDictionary).flatMap({ CGRect(dictionaryRepresentation: $0) }),
                  let shot = CGWindowListCreateImage(.null, .optionIncludingWindow, id, [.boundsIgnoreFraming, .bestResolution]),
                  shot.width > 1 else { return nil }
            return (place, shot)
        }
        guard let first = menus.first else { return nil }
        let all = menus.dropFirst().reduce(first.0) { $0.union($1.0) }
        let image = NSImage(size: all.size, flipped: true) { _ in
            for (place, shot) in menus {
                NSImage(cgImage: shot, size: place.size).draw(in: place.offsetBy(dx: -all.minX, dy: -all.minY), from: .zero,
                                                             operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
            }
            return true
        }
        return image.tiffRepresentation.flatMap(NSBitmapImageRep.init(data:))?.representation(using: .png, properties: [:])
    }

    /// Opens More options as a click on it does and works the real menu with keys put in the app's
    /// own event queue: down to Needs you, right into it, then `down` items further. It saves a
    /// picture of the open menus, and then either chooses the item with Return or closes the menu.
    func options(picture path: String, down: Int, choose: Bool) -> String {
        guard let panel, let button = views(under: panel).compactMap({ $0 as? NSPopUpButton }).first else {
            return "fail no options button"
        }
        var answer = "fail the menu did not open"
        func key(_ code: UInt16) {
            for type in [NSEvent.EventType.keyDown, .keyUp] {
                if let event = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                                                windowNumber: 0, context: nil, characters: "", charactersIgnoringModifiers: "",
                                                isARepeat: false, keyCode: code) {
                    NSApp.postEvent(event, atStart: false)
                }
            }
        }
        let (downArrow, rightArrow, returnKey): (UInt16, UInt16, UInt16) = (125, 124, 36)
        // Needs you follows Menu bar indicator, Quota display (only in quota mode) and Low-quota alerts.
        let before = model.indicator == .quota ? 3 : 2
        let steps: [(TimeInterval, () -> Void)] = [
            (0.6, { for _ in 0...before { key(downArrow) } }),
            (1.0, { key(rightArrow) }),
            (1.3, { for _ in 0..<down { key(downArrow) } }),
            (1.9, { [weak self] in answer = self?.save(self?.menusPicture(), to: path) ?? answer }),
            (2.1, { if choose { key(returnKey) } else { button.menu?.cancelTracking() } }),
        ]
        for (delay, step) in steps {
            let timer = Timer(timeInterval: delay, repeats: false) { _ in step() }
            RunLoop.main.add(timer, forMode: .common)
        }
        // Returns once the menu has closed.
        button.performClick(nil)
        return answer
    }

    func run(_ line: String) -> String {
        let words = line.split(separator: " ", maxSplits: 1).map(String.init)
        let argument = words.count == 2 ? words[1] : ""
        switch words.first ?? "" {
        case "show":
            // Held open by the walkthrough: another app taking focus must not close it mid-picture.
            popover.delegate = nil
            show()
            return "ok"
        case "close": popover.close(); return "ok"
        case "panel": return save(panel?.window.flatMap { picture(of: $0) }, to: argument)
        case "status": return save(statusPicture(), to: argument)
        case "options":
            // options choose|look DOWN FILE
            let parts = argument.split(separator: " ", maxSplits: 2).map(String.init)
            guard parts.count == 3, let down = Int(parts[1]), ["choose", "look"].contains(parts[0]) else { return "fail options choose|look DOWN FILE" }
            return options(picture: parts[2], down: down, choose: parts[0] == "choose")
        case "state":
            let frame = model.attention.frame
            let rows = frame?.entries.map { "\($0.name):\($0.state)" }.joined(separator: ",") ?? "-"
            // How long each row says its pane has been in its state, at the time the panel last drew.
            let ages = frame?.entries.map { $0.age(at: model.badgeUpdatedAt) }.joined(separator: ",") ?? "-"
            return "show=\(model.attentionChoice.show.rawValue) watching=\(model.attention.running) "
                + "rex=\(frame?.rex ?? "-") rows=\(rows) ages=\(ages) dot=\(model.attention.needsYou) "
                // A command can arrive before the app has finished launching and made its status item.
                + "title=\(status?.button?.title ?? "") tip=\(status?.button?.toolTip ?? "") note=\(model.attention.note?.text ?? "-")"
        case "indicator":
            guard let choice = MenuIndicator(rawValue: argument) else { return "fail unknown indicator" }
            model.selectIndicator(choice); return "ok"
        case "jump", "seen":
            guard let entry = model.attention.frame?.entries.first(where: { $0.name == argument }) else {
                return "fail no row for \(argument)"
            }
            if words[0] == "jump" { model.jump(to: entry) } else { model.attention.markSeen(entry) }
            return "ok"
        default: return "fail unknown command"
        }
    }
}

@main
private struct MenuShots {
    @MainActor static func main() {
        setvbuf(stdout, nil, _IOLBF, 0)
        signal(SIGPIPE, SIG_IGN)
        let app = NSApplication.shared; app.setActivationPolicy(.accessory)
        let delegate = AppDelegate(); app.delegate = delegate
        Thread.detachNewThread {
            // Answer in order: wait for the main thread before reading the next command.
            while let line = readLine() { DispatchQueue.main.sync { print(delegate.run(line)) } }
            DispatchQueue.main.async { NSApp.terminate(nil) }
        }
        withExtendedLifetime(delegate) { app.run() }
    }
}
