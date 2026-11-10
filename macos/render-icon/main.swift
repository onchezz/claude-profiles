// Writes the app icon's PNGs into an .iconset folder for iconutil.
// Usage: render-icon <out.iconset>

import AppKit

let out = URL(fileURLWithPath: CommandLine.arguments[1])
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
for base in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = scale == 1 ? "icon_\(base)x\(base).png" : "icon_\(base)x\(base)@2x.png"
        let png = ProfilesIcon.appIcon(px: base * scale).representation(using: .png, properties: [:])!
        try png.write(to: out.appendingPathComponent(name))
    }
}
