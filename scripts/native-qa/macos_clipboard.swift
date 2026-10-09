// Original AppKit inspector for the isolated hosted macOS interaction probe.
// This helper never sends input; osascript owns all PID-checked native actions.
import AppKit
import Foundation

let fixture = "café 日本語👩🏽‍💻\nsecond"
let external = "external café 日本語👩🏽‍💻\nsecond"
let customType = NSPasteboard.PasteboardType("org.textloom.fragment")
let limit = 1024 * 1024

enum ProbeError: Error, CustomStringConvertible {
    case failed(String)
    var description: String {
        switch self { case .failed(let message): return message }
    }
}

func require(_ condition: Bool, _ message: String) throws {
    if !condition { throw ProbeError.failed(message) }
}

func emit(_ observation: [String: Any]) throws {
    var result = observation
    result["change_count"] = NSPasteboard.general.changeCount
    let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
    print(String(decoding: data, as: UTF8.self))
}

func boundedData(_ type: NSPasteboard.PasteboardType) throws -> Data? {
    guard let data = NSPasteboard.general.data(forType: type) else { return nil }
    try require(data.count <= limit, "Unexpectedly large clipboard representation")
    return data
}

func plain() throws -> String {
    guard let value = NSPasteboard.general.string(forType: .string) else {
        throw ProbeError.failed("Clipboard plain text is unavailable")
    }
    try require(value.utf8.count <= limit, "Unexpectedly large clipboard text")
    return value.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
}

func types() -> [String] {
    (NSPasteboard.general.types ?? []).map { $0.rawValue }.sorted()
}

func writeRTF(_ value: NSAttributedString, _ path: String) throws {
    let data = try value.data(
        from: NSRange(location: 0, length: value.length),
        documentAttributes: [.documentType: NSAttributedString.DocumentType.rtf]
    )
    try data.write(to: URL(fileURLWithPath: path), options: .atomic)
}

func prepare(_ arguments: [String]) throws {
    try require(arguments.count == 2, "prepare requires two private fixture paths")
    let baseFont = NSFont.systemFont(ofSize: 14)
    try writeRTF(NSAttributedString(string: "paste target", attributes: [.font: baseFont]), arguments[0])
    let boldFont = NSFontManager.shared.convert(baseFont, toHaveTrait: .boldFontMask)
    let attributed = NSAttributedString(string: external, attributes: [.font: boldFont])
    try writeRTF(attributed, arguments[1])
    try emit(["private_rtf_fixtures_prepared": true])
}

func nativeFixture() throws {
    try require(try plain() == fixture, "Native fixture plain text differs")
    guard let htmlData = try boundedData(.html), let html = String(data: htmlData, encoding: .utf8) else {
        throw ProbeError.failed("Native fixture HTML is unavailable or invalid UTF-8")
    }
    try require(html.contains("<strong>") && html.contains("<em>") && html.contains("<ol"),
                "Native fixture is missing its emphasis/list HTML")
    try require(html.contains("#2a64c8"), "Native fixture is missing its exact opaque CSS color")
    guard let rich = try boundedData(customType) else {
        throw ProbeError.failed("Native fixture envelope is unavailable")
    }
    try require(rich.count > 32 && Array(rich.prefix(8)) == [84, 76, 67, 76, 1, 0, 0, 0],
                "Native fixture envelope is malformed")
    try emit(["native_plain_exact": true, "native_html_present": true,
              "native_envelope_present": true, "types": types()])
}

func textEditImport() throws {
    guard let data = try boundedData(.rtf) else {
        throw ProbeError.failed("TextEdit did not publish an RTF representation after HTML paste")
    }
    let attributed = try NSAttributedString(
        data: data, options: [.documentType: NSAttributedString.DocumentType.rtf], documentAttributes: nil
    )
    let value = attributed.string as NSString
    let first = value.range(of: "café 日本語👩🏽‍💻")
    try require(first.location != NSNotFound, "TextEdit lost the Unicode first paragraph")
    let attributes = attributed.attributes(at: first.location, effectiveRange: nil)
    guard let font = attributes[.font] as? NSFont else {
        throw ProbeError.failed("TextEdit imported text has no font")
    }
    let traits = NSFontManager.shared.traits(of: font)
    try require(traits.contains(.boldFontMask), "TextEdit lost bold from native HTML")
    try require(traits.contains(.italicFontMask), "TextEdit lost italic from native HTML")
    guard let color = (attributes[.foregroundColor] as? NSColor)?.usingColorSpace(.sRGB) else {
        throw ProbeError.failed("TextEdit imported text has no sRGB foreground")
    }
    let rgba = [color.redComponent, color.greenComponent, color.blueComponent, color.alphaComponent]
        .map { Int(($0 * 255).rounded()) }
    try require(rgba == [42, 100, 200, 255], "TextEdit changed native HTML RGBA: \(rgba)")
    guard let paragraph = attributes[.paragraphStyle] as? NSParagraphStyle,
          let list = paragraph.textLists.last else {
        throw ProbeError.failed("TextEdit lost the ordered list from native HTML")
    }
    try require(list.markerFormat.rawValue.contains(NSTextList.MarkerFormat.decimal.rawValue),
                "TextEdit changed the ordered-list kind")
    // AppKit represents displayed list markers as literal tabs plus a marker in
    // RTF text. Accept that native representation only when every payload line
    // remains exact and each prefix matches its actual NSTextList marker.
    var lines = attributed.string.replacingOccurrences(of: "\r\n", with: "\n")
        .replacingOccurrences(of: "\r", with: "\n").components(separatedBy: "\n")
    if lines.last == "" { lines.removeLast() }
    let expected = fixture.components(separatedBy: "\n")
    try require(lines.count == expected.count, "TextEdit changed the paragraph count")
    for index in lines.indices {
        let marker = list.marker(forItemNumber: list.startingItemNumber + index)
        let allowed = [expected[index], "\t\(marker)\t\(expected[index])", "\(marker)\t\(expected[index])"]
        try require(allowed.contains(lines[index]), "TextEdit changed Unicode paragraph payload \(index)")
    }
    try emit(["textedit_html_import_unicode_exact": true, "bold": true, "italic": true,
              "ordered_list": true, "list_start": list.startingItemNumber,
              "rgba": rgba, "types": types()])
}

func externalFixture() throws {
    try require(try plain() == external, "TextEdit external fixture plain text differs")
    try require(try boundedData(customType) == nil, "TextEdit retained a stale Textloom envelope")
    guard let data = try boundedData(.rtf) else {
        throw ProbeError.failed("TextEdit external copy did not offer rich RTF")
    }
    let attributed = try NSAttributedString(
        data: data, options: [.documentType: NSAttributedString.DocumentType.rtf], documentAttributes: nil
    )
    try require(attributed.string == external, "TextEdit external RTF text differs")
    guard let font = attributed.attribute(.font, at: 0, effectiveRange: nil) as? NSFont else {
        throw ProbeError.failed("TextEdit external fixture has no font")
    }
    try require(NSFontManager.shared.traits(of: font).contains(.boldFontMask),
                "TextEdit external fixture is not rich text")
    try emit(["external_plain_exact": true, "external_rtf_bold": true,
              "external_textloom_envelope_absent": true, "types": types()])
}

func expectPlain(_ arguments: [String], fallback: Bool) throws {
    try require(arguments.count == 1, "plain assertion requires one expected value")
    try require(try plain() == arguments[0], "Native clipboard differs from the expected document state")
    if fallback {
        guard let data = try boundedData(.html), let html = String(data: data, encoding: .utf8) else {
            throw ProbeError.failed("Native copied fallback document has no HTML")
        }
        try require(!html.contains("<strong>") && !html.contains("<em>"),
                    "External rich formatting leaked into the plain fallback")
        try require(try boundedData(customType) != nil, "Native copy did not publish its own envelope")
    }
    try emit(["plain_exact": true, "plain_fallback_formatting": fallback, "types": types()])
}

do {
    let arguments = Array(CommandLine.arguments.dropFirst())
    try require(!arguments.isEmpty, "Missing inspector command")
    switch arguments[0] {
    case "prepare": try prepare(Array(arguments.dropFirst()))
    case "native-fixture": try nativeFixture()
    case "textedit-import": try textEditImport()
    case "external-fixture": try externalFixture()
    case "state": try emit(["types": types()])
    case "plain": try expectPlain(Array(arguments.dropFirst()), fallback: false)
    case "fallback": try expectPlain(Array(arguments.dropFirst()), fallback: true)
    default: throw ProbeError.failed("Unknown inspector command")
    }
} catch {
    fputs("macOS clipboard observation failed: \(error)\n", stderr)
    exit(1)
}
