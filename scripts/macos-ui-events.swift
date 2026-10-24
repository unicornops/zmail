// macos-ui-events.swift - Posts mouse and keyboard events, for driving the
// app in CI's macOS screenshots (scripts/capture-macos-web-screenshots.sh).
//
// Usage (screen points, top-left origin, as in the screenshots):
//   macos-ui-events click X Y
//   macos-ui-events scroll X Y PIXELS      (positive scrolls the content down)
//   macos-ui-events drag X1 Y1 X2 Y2
//   macos-ui-events key-copy               (Command-C)
//
// Build: swiftc -O scripts/macos-ui-events.swift -o macos-ui-events

import CoreGraphics
import Foundation

func post(_ event: CGEvent?) {
    event?.post(tap: .cghidEventTap)
    usleep(30_000)
}

func mouse(_ type: CGEventType, _ point: CGPoint) {
    post(CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: point, mouseButton: .left))
}

func number(_ index: Int) -> Double {
    let arguments = CommandLine.arguments
    guard index < arguments.count, let value = Double(arguments[index]) else {
        FileHandle.standardError.write("missing or bad number argument \(index)\n".data(using: .utf8)!)
        exit(2)
    }
    return value
}

let command = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : ""
switch command {
case "click":
    let point = CGPoint(x: number(2), y: number(3))
    mouse(.mouseMoved, point)
    mouse(.leftMouseDown, point)
    mouse(.leftMouseUp, point)
case "scroll":
    let point = CGPoint(x: number(2), y: number(3))
    mouse(.mouseMoved, point)
    var remaining = Int32(number(4))
    while remaining != 0 {
        let step = max(-40, min(40, remaining))
        post(CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: -step, wheel2: 0, wheel3: 0))
        remaining -= step
    }
case "drag":
    let start = CGPoint(x: number(2), y: number(3))
    let end = CGPoint(x: number(4), y: number(5))
    mouse(.mouseMoved, start)
    mouse(.leftMouseDown, start)
    for step in 1...20 {
        let t = Double(step) / 20
        mouse(.leftMouseDragged, CGPoint(x: start.x + (end.x - start.x) * t, y: start.y + (end.y - start.y) * t))
    }
    mouse(.leftMouseUp, end)
case "key-copy":
    let c: CGKeyCode = 8
    let down = CGEvent(keyboardEventSource: nil, virtualKey: c, keyDown: true)
    down?.flags = .maskCommand
    post(down)
    let up = CGEvent(keyboardEventSource: nil, virtualKey: c, keyDown: false)
    up?.flags = .maskCommand
    post(up)
default:
    FileHandle.standardError.write("usage: macos-ui-events click|scroll|drag|key-copy ...\n".data(using: .utf8)!)
    exit(2)
}
