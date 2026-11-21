// Draws the app icon (the tray icon's two screens on a blue rounded square, `tray.rs` `icon`)
// and writes assets/Aun.icns and assets/aun.ico. Run from the repository root on a Mac:
//   swift scripts/icon.swift
// The outputs are committed, so CI (Windows included) doesn't need to run this.
import AppKit

// `pad`: the share of each side left empty. macOS icons sit inside a margin; Windows icons fill.
func png(_ n: Int, pad: Double) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: n, pixelsHigh: n, bitsPerSample: 8, samplesPerPixel: 4,
                               hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let side = Double(n) * (1 - 2 * pad), o = Double(n) * pad, f = side / 32
    // tray.rs uses y down; flip so the same numbers place the same shapes.
    func rect(_ x0: Double, _ y0: Double, _ x1: Double, _ y1: Double) -> NSRect {
        NSRect(x: o + x0 * f, y: o + side - y1 * f, width: (x1 - x0) * f, height: (y1 - y0) * f)
    }
    NSColor(srgbRed: 37 / 255, green: 99 / 255, blue: 235 / 255, alpha: 1).setFill()
    NSBezierPath(roundedRect: rect(0, 0, 32, 32), xRadius: 6 * f, yRadius: 6 * f).fill()
    NSColor.white.setFill()
    for (a, b) in [(rect(4, 7, 20, 19), rect(6, 9, 18, 17)), (rect(12, 13, 28, 25), rect(14, 15, 26, 23))] {
        let p = NSBezierPath(rect: a)
        p.append(NSBezierPath(rect: b))
        p.windingRule = .evenOdd
        p.fill()
    }
    NSGraphicsContext.current = nil
    return rep.representation(using: .png, properties: [:])!
}

let fm = FileManager.default
try? fm.createDirectory(atPath: "assets", withIntermediateDirectories: true)

// macOS: an iconset through iconutil. 100 of 1024 px margin, as in Apple's icon template.
let set = NSTemporaryDirectory() + "Aun.iconset"
try? fm.removeItem(atPath: set)
try! fm.createDirectory(atPath: set, withIntermediateDirectories: true)
for s in [16, 32, 128, 256, 512] {
    try! png(s, pad: 100 / 1024).write(to: URL(fileURLWithPath: "\(set)/icon_\(s)x\(s).png"))
    try! png(s * 2, pad: 100 / 1024).write(to: URL(fileURLWithPath: "\(set)/icon_\(s)x\(s)@2x.png"))
}
let p = Process()
p.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
p.arguments = ["-c", "icns", set, "-o", "assets/Aun.icns"]
try! p.run()
p.waitUntilExit()
precondition(p.terminationStatus == 0, "iconutil failed")

// Windows: an ICO whose entries are PNGs (read by Windows Vista and later).
let sizes = [16, 20, 24, 32, 40, 48, 64, 256]
let images = sizes.map { png($0, pad: 0) }
var ico = Data()
func le16(_ v: Int) { ico.append(contentsOf: [UInt8(v & 0xff), UInt8(v >> 8 & 0xff)]) }
func le32(_ v: Int) { le16(v & 0xffff); le16(v >> 16) }
le16(0); le16(1); le16(sizes.count)
var offset = 6 + 16 * sizes.count
for (s, d) in zip(sizes, images) {
    ico.append(contentsOf: [UInt8(s & 0xff), UInt8(s & 0xff), 0, 0]) // 256 is stored as 0
    le16(1); le16(32); le32(d.count); le32(offset)
    offset += d.count
}
images.forEach { ico.append($0) }
try! ico.write(to: URL(fileURLWithPath: "assets/aun.ico"))
print("wrote assets/Aun.icns and assets/aun.ico")
