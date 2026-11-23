// Rasterizes real `rex capture --format=html --trim` terminal cells without a GUI client.
// Usage: swift tests/rex_lab/capture_png.swift CAPTURE.html OUTPUT.png
import AppKit
import CoreText

private struct Paint {
    var ink = rgb(0xd8dee9)
    var paper: CGColor?
    var faint = false
    var bold = false
    var italic = false
    var underline = false
}

private func rgb(_ hex: UInt32) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 255) / 255, green: CGFloat((hex >> 8) & 255) / 255,
            blue: CGFloat(hex & 255) / 255, alpha: 1)
}

private let background = rgb(0x171b23)
private let ansi: [UInt32] = [
    0x171b23, 0xd06377, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xd8dee9,
    0x303948, 0xe37489, 0xb4d39b, 0xffdc98, 0x94b2cf, 0xc8a2c1, 0x9ad3e0, 0xeceff4,
]

private func color(_ value: String) -> CGColor? {
    if value.hasPrefix("#"), let hex = UInt32(value.dropFirst(), radix: 16) { return rgb(hex) }
    let prefix = "var(--vt-palette-"
    guard value.hasPrefix(prefix), value.hasSuffix(")"),
          let index = Int(value.dropFirst(prefix.count).dropLast()), (0..<256).contains(index) else { return nil }
    if index < ansi.count { return rgb(ansi[index]) }
    if index >= 232 { return rgb(UInt32(8 + 10 * (index - 232)) * 0x010101) }
    let steps: [UInt32] = [0, 95, 135, 175, 215, 255]
    let i = index - 16
    return rgb(steps[i / 36] << 16 | steps[i / 6 % 6] << 8 | steps[i % 6])
}

private final class Terminal: NSObject, XMLParserDelegate {
    private var stack = [Paint()]
    var lines: [[(Character, Paint)]] = [[]]

    func parser(_ parser: XMLParser, didStartElement name: String, namespaceURI: String?,
                qualifiedName: String?, attributes: [String: String]) {
        var paint = stack.last!
        for rule in (attributes["style"] ?? "").split(separator: ";") {
            let parts = rule.split(separator: ":", maxSplits: 1).map { $0.trimmingCharacters(in: .whitespaces) }
            guard parts.count == 2 else { continue }
            switch parts[0] {
            case "color": paint.ink = color(parts[1]) ?? paint.ink
            case "background-color": paint.paper = color(parts[1]) ?? paint.paper
            case "opacity": paint.faint = parts[1] == "0.5"
            case "font-weight": paint.bold = parts[1] == "bold"
            case "font-style": paint.italic = parts[1] == "italic"
            case "text-decoration": paint.underline = parts[1].contains("underline")
            default: break
            }
        }
        stack.append(paint)
    }

    func parser(_ parser: XMLParser, didEndElement name: String, namespaceURI: String?, qualifiedName: String?) {
        stack.removeLast()
    }

    func parser(_ parser: XMLParser, foundCharacters text: String) {
        let paint = stack.last!
        for ch in text {
            if ch == "\n" { lines.append([]) } else { lines[lines.count - 1].append((ch, paint)) }
        }
    }
}

private func render(_ source: URL, to png: URL) throws {
    let terminal = Terminal()
    let parser = XMLParser(data: try Data(contentsOf: source))
    parser.delegate = terminal
    guard parser.parse() else { throw parser.parserError! }
    if terminal.lines.last?.isEmpty == true { terminal.lines.removeLast() }

    let cellWidth = 10, cellHeight = 20, margin = 16, scale = 2
    let width = (terminal.lines.map(\.count).max() ?? 1) * cellWidth + 2 * margin
    let height = terminal.lines.count * cellHeight + 2 * margin
    let bitmap = CGContext(data: nil, width: width * scale, height: height * scale, bitsPerComponent: 8,
                           bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                           bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    bitmap.scaleBy(x: CGFloat(scale), y: CGFloat(scale))
    bitmap.setFillColor(background)
    bitmap.fill(CGRect(x: 0, y: 0, width: width, height: height))

    let base = NSFont(name: "JetBrainsMonoNFM-Regular", size: 16) ?? NSFont(name: "Menlo", size: 16)!
    let fonts = (0..<4).map { index -> NSFont in
        var traits: NSFontTraitMask = []
        if index & 1 != 0 { traits.insert(.boldFontMask) }
        if index & 2 != 0 { traits.insert(.italicFontMask) }
        return NSFontManager.shared.convert(base, toHaveTrait: traits)
    }
    for (row, cells) in terminal.lines.enumerated() {
        let y = height - margin - (row + 1) * cellHeight
        for (column, cell) in cells.enumerated() {
            let (ch, paint) = cell
            let x = margin + column * cellWidth
            let rect = CGRect(x: x, y: y, width: cellWidth, height: cellHeight)
            bitmap.setAlpha(paint.faint ? 0.5 : 1)
            if let paper = paint.paper {
                bitmap.setFillColor(paper)
                bitmap.fill(rect)
            }
            if ch == "▀" || ch == "▄" || ch == "█" {
                bitmap.setFillColor(paint.ink)
                let half = cellHeight / 2
                if ch == "█" { bitmap.fill(rect) }
                else { bitmap.fill(CGRect(x: x, y: y + (ch == "▀" ? half : 0), width: cellWidth, height: half)) }
            } else if ch != " " {
                let font = fonts[(paint.bold ? 1 : 0) + (paint.italic ? 2 : 0)]
                let text = NSAttributedString(string: String(ch), attributes: [
                    .font: font, .foregroundColor: NSColor(cgColor: paint.ink)!,
                ])
                bitmap.textPosition = CGPoint(x: x, y: y + 4)
                CTLineDraw(CTLineCreateWithAttributedString(text), bitmap)
            }
            if paint.underline {
                bitmap.setFillColor(paint.ink)
                bitmap.fill(CGRect(x: x, y: y + 2, width: cellWidth, height: 1))
            }
        }
    }
    let output = NSBitmapImageRep(cgImage: bitmap.makeImage()!).representation(using: .png, properties: [:])!
    try output.write(to: png)
}

let args = CommandLine.arguments
if args.count != 3 {
    FileHandle.standardError.write(Data("Usage: swift capture_png.swift CAPTURE.html OUTPUT.png\n".utf8))
    exit(64)
}
do {
    try render(URL(fileURLWithPath: args[1]), to: URL(fileURLWithPath: args[2]))
} catch {
    FileHandle.standardError.write(Data("\(error)\n".utf8))
    exit(1)
}
