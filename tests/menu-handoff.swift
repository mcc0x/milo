import Foundation

@main struct HandoffTest {
    static func main() throws {
        let token = String(repeating: "a1", count: 32)
        let url = URL(string: "http://127.0.0.1:6110/#\(token)")!
        let path: String
        do {
            let handoff = BrowserHandoff(url)!
            path = handoff.file.path
            let attributes = try FileManager.default.attributesOfItem(atPath: path)
            precondition((attributes[.posixPermissions] as? NSNumber)?.intValue == 0o600)
            precondition(!path.contains(token))
            precondition(tryText(path).contains("url=\(url.absoluteString)\""))
            precondition(BrowserHandoff(URL(string: "https://example.com/#\(token)")!) == nil)
            precondition(BrowserHandoff(URL(string: "http://127.0.0.1:6110/?other#\(token)")!) == nil)
            withExtendedLifetime(handoff) {}
        }
        precondition(!FileManager.default.fileExists(atPath: path))
    }
    private static func tryText(_ path: String) -> String { (try? String(contentsOfFile: path, encoding: .utf8)) ?? "" }
}
