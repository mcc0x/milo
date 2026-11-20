import Foundation
import Darwin

/// Launch Services receives a short-lived file URL, never the reusable editor key.
final class BrowserHandoff {
    let file: URL

    init?(_ editor: URL) {
        guard let port = editor.port, (1...65535).contains(port), let token = editor.fragment,
              token.utf8.count == 64, token.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              editor.absoluteString == "http://127.0.0.1:\(port)/#\(token)" else { return nil }
        var path = Array("/tmp/milo-web-XXXXXXXX.html".utf8CString)
        let fd = mkstemps(&path, 5)
        guard fd >= 0 else { return nil }
        let url = URL(fileURLWithPath: String(cString: path))
        let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        do {
            let html = "<!doctype html><meta charset=utf-8><meta name=referrer content=no-referrer><meta http-equiv=refresh content=\"0;url=\(editor.absoluteString)\"><title>Opening Milo</title>"
            try handle.write(contentsOf: Data(html.utf8))
            try handle.close()
        } catch {
            try? FileManager.default.removeItem(at: url)
            return nil
        }
        file = url
    }

    deinit { try? FileManager.default.removeItem(at: file) }
}
