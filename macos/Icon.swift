// The Claude Profiles mark: a Claude-style spark with a small "people" badge
// in its lower right corner. One drawing serves the menu bar (a monochrome
// template image) and the app icon (white on Claude's terracotta), so the two
// always match.

import AppKit

enum ProfilesIcon {
    static let terracotta = NSColor(srgbRed: 0.851, green: 0.467, blue: 0.341, alpha: 1)

    /// The spark: rays of uneven length from one centre, like Claude's mark.
    static func drawSpark(in rect: NSRect, color: NSColor) {
        let c = NSPoint(x: rect.midX, y: rect.midY)
        let r = min(rect.width, rect.height) / 2
        let lengths: [CGFloat] = [1.0, 0.78, 0.92, 0.72, 0.98, 0.8, 0.9, 0.74, 0.96, 0.82, 0.88]
        color.setStroke()
        for (i, len) in lengths.enumerated() {
            let a = CGFloat(i) / CGFloat(lengths.count) * 2 * .pi + .pi / 2
            let path = NSBezierPath()
            path.lineWidth = r * 0.17
            path.lineCapStyle = .round
            path.move(to: c)
            path.line(to: NSPoint(x: c.x + cos(a) * r * len * 0.9, y: c.y + sin(a) * r * len * 0.9))
            path.stroke()
        }
    }

    /// The people symbol in one colour.
    static func people(_ color: NSColor, size: CGFloat) -> NSImage? {
        let config = NSImage.SymbolConfiguration(pointSize: size, weight: .bold)
        guard let symbol = NSImage(systemSymbolName: "person.2.fill", accessibilityDescription: nil)?
            .withSymbolConfiguration(config) else { return nil }
        let out = NSImage(size: symbol.size)
        out.lockFocus()
        symbol.draw(at: .zero, from: .zero, operation: .sourceOver, fraction: 1)
        color.set()
        NSRect(origin: .zero, size: symbol.size).fill(using: .sourceAtop)
        out.unlockFocus()
        return out
    }

    /// Spark plus badge in `rect`. The badge sits in a cut-out so it reads at
    /// menu bar size.
    static func drawMark(in rect: NSRect, color: NSColor) {
        let s = min(rect.width, rect.height)
        drawSpark(in: NSRect(x: rect.minX, y: rect.minY + s * 0.12, width: s * 0.88, height: s * 0.88), color: color)
        let badge = NSRect(x: rect.maxX - s * 0.56, y: rect.minY, width: s * 0.56, height: s * 0.46)
        NSGraphicsContext.current?.compositingOperation = .clear
        NSBezierPath(roundedRect: badge.insetBy(dx: -s * 0.03, dy: -s * 0.03), xRadius: s * 0.12, yRadius: s * 0.12).fill()
        NSGraphicsContext.current?.compositingOperation = .sourceOver
        if let p = people(color, size: s * 0.36) {
            let fit = min(badge.width / p.size.width, badge.height / p.size.height)
            let size = NSSize(width: p.size.width * fit, height: p.size.height * fit)
            p.draw(in: NSRect(x: badge.midX - size.width / 2, y: badge.midY - size.height / 2,
                              width: size.width, height: size.height))
        }
    }

    /// The menu bar image: black on clear, marked as a template so macOS
    /// tints it for light and dark menu bars.
    static func menuBar() -> NSImage {
        let img = NSImage(size: NSSize(width: 20, height: 18), flipped: false) { r in
            drawMark(in: r.insetBy(dx: 1, dy: 0), color: .black)
            return true
        }
        img.isTemplate = true
        return img
    }

    /// The app icon at `px` pixels square, in the macOS icon grid: a rounded
    /// square inset from the canvas edge.
    static func appIcon(px: Int) -> NSBitmapImageRep {
        let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                                   samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                   colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        let s = CGFloat(px)
        let tile = NSRect(x: s * 0.1, y: s * 0.1, width: s * 0.8, height: s * 0.8)
        terracotta.setFill()
        NSBezierPath(roundedRect: tile, xRadius: s * 0.18, yRadius: s * 0.18).fill()
        // Draw the mark on its own layer so its cut-out clears only the mark,
        // not the tile behind it.
        let mark = NSImage(size: NSSize(width: s, height: s), flipped: false) { _ in
            drawMark(in: tile.insetBy(dx: s * 0.13, dy: s * 0.13), color: .white)
            return true
        }
        mark.draw(in: NSRect(x: 0, y: 0, width: s, height: s))
        NSGraphicsContext.restoreGraphicsState()
        return rep
    }
}
