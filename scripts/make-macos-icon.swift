#!/usr/bin/env swift
// make-macos-icon.swift — bake the macOS squircle + margin into an app icon.
//
// macOS does NOT mask app icons the way iOS does: the Dock, Command-Tab,
// Finder and Launchpad blit the .icns raster verbatim, alpha and all. The
// rounded shape and the surrounding margin have to be IN the artwork. A
// full-bleed square PNG therefore shows up as a hard square next to every
// other app — which is exactly what RabbitHole was doing.
//
// Apple's macOS Big Sur+ grid, at a 1024pt canvas: the icon body is 824pt
// (80.47%), centred, with a continuous ("squircle") corner radius of ~185pt.
//
// Usage: swift make-macos-icon.swift <source.png> <out-1024.png>
import AppKit
import QuartzCore

let args = CommandLine.arguments
guard args.count >= 3 else {
    FileHandle.standardError.write("usage: make-macos-icon.swift <src.png> <out.png>\n".data(using: .utf8)!)
    exit(1)
}
let srcPath = args[1], outPath = args[2]

let S: CGFloat = 1024        // full canvas
let body: CGFloat = 824      // icon body
let radius: CGFloat = 185.4  // continuous corner radius
let inset = (S - body) / 2   // transparent margin on every side

guard let src = NSImage(contentsOfFile: srcPath) else {
    FileHandle.standardError.write("cannot read \(srcPath)\n".data(using: .utf8)!)
    exit(1)
}

let root = CALayer()
root.frame = CGRect(x: 0, y: 0, width: S, height: S)
root.isGeometryFlipped = true

let shape = CALayer()
shape.frame = CGRect(x: inset, y: inset, width: body, height: body)
shape.cornerRadius = radius
shape.cornerCurve = .continuous
shape.masksToBounds = true
shape.contents = src
shape.contentsGravity = .resizeAspectFill
root.addSublayer(shape)

let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(S), pixelsHigh: Int(S),
                           bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                           colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
let ctx = NSGraphicsContext(bitmapImageRep: rep)!
root.render(in: ctx.cgContext)
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: outPath))
