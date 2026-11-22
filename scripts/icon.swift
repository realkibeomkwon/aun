// Builds the app icon from assets/src/icon-source.png (the rounded square on transparency)
// and writes assets/Aun.icns and assets/aun.ico. Run from the repository root on a Mac:
//   swift scripts/icon.swift
// The outputs are committed, so CI (Windows included) doesn't need to run this.
import AppKit

let source = NSBitmapImageRep(data: try! Data(contentsOf: URL(fileURLWithPath: "assets/src/icon-source.png")))!
// Clear the source's own soft shadow (translucent, gray rather than teal); the template adds one.
for y in 0..<source.pixelsHigh {
    for x in 0..<source.pixelsWide {
        let c = source.colorAt(x: x, y: y)!
        if c.alphaComponent > 0, c.alphaComponent < 0.95, c.greenComponent - c.redComponent < 0.08 {
            source.setColor(NSColor(deviceRed: 0, green: 0, blue: 0, alpha: 0), atX: x, y: y)
        }
    }
}

// The rounded square's bounds in the source: its teal pixels.
var box = (x0: Int.max, y0: Int.max, x1: 0, y1: 0)
for y in 0..<source.pixelsHigh {
    for x in 0..<source.pixelsWide where source.colorAt(x: x, y: y)!.alphaComponent > 0.5
        && source.colorAt(x: x, y: y)!.redComponent < 0.3 {
        box = (min(box.x0, x), min(box.y0, y), max(box.x1, x), max(box.y1, y))
    }
}
// NSImage's origin is bottom-left; the bitmap's rows go down.
let crop = NSRect(x: box.x0, y: source.pixelsHigh - 1 - box.y1, width: box.x1 - box.x0 + 1, height: box.y1 - box.y0 + 1)
let image = NSImage(size: NSSize(width: source.pixelsWide, height: source.pixelsHigh))
image.addRepresentation(source)

// Apple's macOS icon template: on a 1024 canvas the body is 824 (100 margin each side) with a
// drop shadow below it. Windows uses the same picture, as asked.
func png(_ n: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: n, pixelsHigh: n, bitsPerSample: 8, samplesPerPixel: 4,
                               hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    NSGraphicsContext.current!.imageInterpolation = .high
    let f = Double(n) / 1024
    let shadow = NSShadow()
    shadow.shadowColor = NSColor(white: 0, alpha: 0.3)
    shadow.shadowOffset = NSSize(width: 0, height: -12 * f)
    shadow.shadowBlurRadius = 28 * f
    shadow.set()
    // A transparency layer, or the shadow is clipped to the image's rectangle.
    let cg = NSGraphicsContext.current!.cgContext
    cg.beginTransparencyLayer(auxiliaryInfo: nil)
    image.draw(in: NSRect(x: 100 * f, y: 100 * f, width: 824 * f, height: 824 * f), from: crop, operation: .sourceOver, fraction: 1)
    cg.endTransparencyLayer()
    NSGraphicsContext.current = nil
    return rep.representation(using: .png, properties: [:])!
}

let fm = FileManager.default
let set = NSTemporaryDirectory() + "Aun.iconset"
try? fm.removeItem(atPath: set)
try! fm.createDirectory(atPath: set, withIntermediateDirectories: true)
for s in [16, 32, 128, 256, 512] {
    try! png(s).write(to: URL(fileURLWithPath: "\(set)/icon_\(s)x\(s).png"))
    try! png(s * 2).write(to: URL(fileURLWithPath: "\(set)/icon_\(s)x\(s)@2x.png"))
}
let p = Process()
p.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
p.arguments = ["-c", "icns", set, "-o", "assets/Aun.icns"]
try! p.run()
p.waitUntilExit()
precondition(p.terminationStatus == 0, "iconutil failed")

// Windows: an ICO whose entries are PNGs (read by Windows Vista and later).
let sizes = [16, 24, 32, 48, 64, 128, 256]
let images = sizes.map { png($0) }
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

func bitmap(_ n: Int) -> NSBitmapImageRep {
    NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: n, pixelsHigh: n, bitsPerSample: 8, samplesPerPixel: 4,
                     hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: n * 4, bitsPerPixel: 32)!
}

// Windows tray: the square alone, filling 32 × 32 (no margin or shadow at that size), as raw
// straight RGBA so the app hands it to the tray without decoding anything.
let tray = bitmap(32)
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: tray)
NSGraphicsContext.current!.imageInterpolation = .high
image.draw(in: NSRect(x: 0, y: 0, width: 32, height: 32), from: crop, operation: .copy, fraction: 1)
NSGraphicsContext.current = nil
var rgba = [UInt8](UnsafeBufferPointer(start: tray.bitmapData!, count: 32 * 32 * 4))
for i in stride(from: 0, to: rgba.count, by: 4) where rgba[i + 3] > 0 && rgba[i + 3] < 255 {
    for c in 0..<3 { rgba[i + c] = UInt8(min(255, Int(rgba[i + c]) * 255 / Int(rgba[i + 3]))) } // the bitmap is premultiplied
}
try! Data(rgba).write(to: URL(fileURLWithPath: "assets/tray.rgba"))

// macOS menu bar: the two pointers alone as a template image (black, the shape in alpha), 36 px
// for 18 pt on a Retina display. A pixel's share of pointer: how much brighter than the teal it is.
let W = source.pixelsWide, H = source.pixelsHigh
let shape = bitmap(W)
var pbox = (x0: Int.max, y0: Int.max, x1: 0, y1: 0)
for y in 0..<H {
    for x in 0..<W {
        let c = source.colorAt(x: x, y: y)!
        guard c.alphaComponent > 0.9 else { continue }
        let lum = 0.3 * c.redComponent + 0.59 * c.greenComponent + 0.11 * c.blueComponent
        let share = max(0, min(1, (lum - 0.5) / 0.3))
        shape.setColor(NSColor(deviceRed: 0, green: 0, blue: 0, alpha: share), atX: x, y: y)
        if share > 0.5 { pbox = (min(pbox.x0, x), min(pbox.y0, y), max(pbox.x1, x), max(pbox.y1, y)) }
    }
}
let side = max(pbox.x1 - pbox.x0, pbox.y1 - pbox.y0) + 1
let pcrop = NSRect(x: (pbox.x0 + pbox.x1 - side) / 2, y: H - 1 - (pbox.y0 + pbox.y1 + side) / 2, width: side, height: side)
let shapes = NSImage(size: NSSize(width: W, height: H))
shapes.addRepresentation(shape)
let bar = bitmap(36)
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bar)
NSGraphicsContext.current!.imageInterpolation = .high
shapes.draw(in: NSRect(x: 2, y: 2, width: 32, height: 32), from: pcrop, operation: .sourceOver, fraction: 1)
NSGraphicsContext.current = nil
try! bar.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: "assets/menubar.png"))
print("wrote assets/Aun.icns, aun.ico, tray.rgba and menubar.png (body \(crop), pointers \(pcrop))")
