import AppKit

// An 18-point cutout: the taller far ear, cheek and whiskers survive menu-bar scale.
enum MiloMark {
    static let awake = draw(blinking: false)
    static let blink = draw(blinking: true)

    private static func draw(blinking: Bool) -> NSImage {
        let image = NSImage(size: NSSize(width: 18, height: 18), flipped: true) { _ in
            let face = NSBezierPath()
            face.move(to: NSPoint(x: 2, y: 8))
            face.line(to: NSPoint(x: 3.5, y: 1.6)); face.line(to: NSPoint(x: 8, y: 5))
            face.line(to: NSPoint(x: 11, y: 4.7)); face.line(to: NSPoint(x: 14.5, y: 1.4))
            face.line(to: NSPoint(x: 16, y: 7.4))
            face.curve(to: NSPoint(x: 16, y: 13), controlPoint1: NSPoint(x: 18.1, y: 10), controlPoint2: NSPoint(x: 17, y: 12))
            face.curve(to: NSPoint(x: 3, y: 14), controlPoint1: NSPoint(x: 13, y: 17.7), controlPoint2: NSPoint(x: 6, y: 17.5))
            face.curve(to: NSPoint(x: 2, y: 8), controlPoint1: NSPoint(x: 1, y: 12.6), controlPoint2: NSPoint(x: 1, y: 10))
            face.close(); NSColor.black.setFill(); face.fill()
            guard let cg = NSGraphicsContext.current?.cgContext else { return false }
            cg.setBlendMode(.clear)
            for x in [8.0, 13.0] {
                cg.fillEllipse(in: CGRect(x: x, y: blinking ? 9 : 8, width: 2, height: blinking ? 1 : 2.8))
            }
            cg.setLineCap(.round); cg.setLineWidth(1)
            for y in [10.5, 12.5] {
                cg.move(to: CGPoint(x: 2.5, y: y)); cg.addLine(to: CGPoint(x: 5.2, y: y-0.7)); cg.strokePath()
            }
            return true
        }
        image.isTemplate = true
        return image
    }
}
