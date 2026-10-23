import AppKit
import ApplicationServices
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

// A direct, PID-scoped AX client avoids System Events' expensive `entire
// contents` materialization. The caller raises its owned window first; this
// helper independently checks its identity and foreground status before press.
final class ControlLookup {
    let deadline = ProcessInfo.processInfo.systemUptime + 6
    let maxNodes = 512
    let artifact: URL
    var observation: [String: Any]
    var nodes: [[String: Any]] = []
    var errors: [[String: Any]] = []

    init(artifact: String, observation: [String: Any]) {
        self.artifact = URL(fileURLWithPath: artifact)
        self.observation = observation
    }

    func save() throws {
        observation["nodes"] = nodes
        observation["attribute_errors"] = errors
        observation["elapsed_seconds"] = 6 - (deadline - ProcessInfo.processInfo.systemUptime)
        let data = try JSONSerialization.data(withJSONObject: observation, options: [.sortedKeys, .prettyPrinted])
        try data.write(to: artifact, options: .atomic)
    }

    func checkTime() throws {
        try require(ProcessInfo.processInfo.systemUptime < deadline, "AX lookup exceeded six-second deadline")
    }

    func checked(_ error: AXError, _ operation: String, optional: Bool = false) throws -> Bool {
        if error == .success { return true }
        if errors.count < 128 {
            errors.append(["operation": operation, "code": error.rawValue,
                           "error": String(describing: error)])
        }
        if optional && (error == .attributeUnsupported || error == .noValue) { return false }
        throw ProbeError.failed("\(operation): AX error \(error.rawValue) (\(error))")
    }

    func read(_ element: AXUIElement, _ attribute: String, optional: Bool = true) throws -> CFTypeRef? {
        try checkTime()
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, attribute as CFString, &value)
        _ = try checked(error, attribute, optional: optional)
        return value
    }

    func string(_ element: AXUIElement, _ attribute: String, optional: Bool = true) throws -> String? {
        guard let value = try read(element, attribute, optional: optional) else { return nil }
        try require(CFGetTypeID(value) == CFStringGetTypeID(), "\(attribute) is not a string")
        return value as? String
    }

    func boolean(_ element: AXUIElement, _ attribute: String) throws -> Bool {
        guard let value = try read(element, attribute, optional: false),
              CFGetTypeID(value) == CFBooleanGetTypeID(), let result = value as? Bool else {
            throw ProbeError.failed("\(attribute) is not a Boolean")
        }
        return result
    }

    func element(_ value: CFTypeRef?) throws -> AXUIElement {
        guard let value = value, CFGetTypeID(value) == AXUIElementGetTypeID() else {
            throw ProbeError.failed("AX attribute is not an element")
        }
        return value as! AXUIElement
    }

    func children(_ element: AXUIElement, attribute: String = "AXChildren") throws -> [AXUIElement] {
        try checkTime()
        var values: CFArray?
        // Do not pre-query the count: AccessKit can publish a larger populated
        // tree before the copy. Request one beyond the cap so
        // an oversized result cannot masquerade as a complete unique scan.
        let error = AXUIElementCopyAttributeValues(element, attribute as CFString, 0, maxNodes + 1, &values)
        if error == .illegalArgument {
            // Some providers reject index zero for an empty array. Confirm the
            // empty leaf explicitly; all other illegal-argument errors fail.
            try checkTime()
            var count: CFIndex = 0
            _ = try checked(AXUIElementGetAttributeValueCount(element, attribute as CFString, &count), attribute + ".count")
            if count == 0 { return [] }
        }
        if !(try checked(error, attribute, optional: attribute == "AXChildren")) { return [] }
        guard let entries = values as? [AnyObject] else { throw ProbeError.failed("\(attribute) is not an array") }
        try require(entries.count <= maxNodes, "\(attribute) exceeds \(maxNodes)-child cap")
        return try entries.map { try self.element($0) }
    }

    func foreground(_ app: AXUIElement, window: AXUIElement, pid: pid_t) throws {
        try require(NSWorkspace.shared.frontmostApplication?.processIdentifier == pid,
                    "Target PID is not foreground")
        let focused = try element(read(app, "AXFocusedWindow", optional: false))
        try require(CFEqual(focused, window), "Wrong foreground AX window")
    }

    func press(pid: pid_t, windowName: String, command: String, label: String,
               document: String, resolvedDocument: String) throws {
        let app = AXUIElementCreateApplication(pid)
        // The system-wide element applies the timeout to ALL queried elements,
        // unlike setting it on just the app. A single stalled IPC cannot consume
        // the driver's ten-second subprocess budget.
        _ = try checked(AXUIElementSetMessagingTimeout(AXUIElementCreateSystemWide(), 0.25), "set AX timeout")
        for attempt in 1...30 {
            try checkTime()
            nodes = []
            errors = []
            observation["attempt"] = attempt
            observation["status"] = "discovering"
            observation["complete"] = false
            var windows: [AXUIElement] = []
            var windowInventory: [[String: Any]] = []
            for window in try children(app, attribute: "AXWindows") {
                let title = try string(window, "AXTitle") ?? ""
                let uri = try string(window, "AXDocument") ?? ""
                windowInventory.append(["title": String(title.prefix(200)), "document": String(uri.prefix(512))])
                if document.isEmpty ? title.contains(windowName) : (uri == document || uri == resolvedDocument) {
                    windows.append(window)
                }
            }
            observation["windows"] = windowInventory
            observation["window_count"] = windows.count
            try require(windows.count <= 1, "Ambiguous target AX windows: \(windows.count)")
            if let window = windows.first {
                try foreground(app, window: window, pid: pid)
                var queue: [(AXUIElement, Int, Int)] = [(window, 0, -1)]
                var visited: [AXUIElement] = []
                var matches: [AXUIElement] = []
                var index = 0
                while index < queue.count {
                    try checkTime()
                    let (node, depth, parent) = queue[index]
                    let current = index
                    index += 1
                    if visited.contains(where: { CFEqual($0, node) }) { continue }
                    visited.append(node)
                    try require(depth <= 32, "AX tree exceeds depth cap 32")
                    var nodePID: pid_t = 0
                    _ = try checked(AXUIElementGetPid(node, &nodePID), "AX element PID")
                    try require(nodePID == pid, "AX child belongs to a different PID")
                    let role = try string(node, "AXRole", optional: false) ?? ""
                    var entry: [String: Any] = ["index": current, "parent": parent, "depth": depth, "role": role]
                    nodes.append(entry)
                    let title = try string(node, "AXTitle")
                    entry["title"] = title.map { String($0.prefix(160)) } ?? ""
                    let wantedRole = command == "press-popup" ? role == "AXPopUpButton" : (role == "AXButton" || role == "AXCheckBox")
                    if role == "AXButton" || role == "AXCheckBox" || role == "AXPopUpButton" {
                        let value = try read(node, "AXValue")
                        entry["value"] = value.map { String(String(describing: $0).prefix(160)) } ?? ""
                        entry["enabled"] = try boolean(node, "AXEnabled")
                        let actualLabel = command == "press-popup" ? (value as? String) : title
                        if wantedRole && actualLabel == label { matches.append(node) }
                    }
                    nodes[nodes.count - 1] = entry
                    let descendants = try children(node)
                    try require(queue.count + descendants.count <= maxNodes, "AX tree exceeds \(maxNodes)-node/queue cap")
                    queue.append(contentsOf: descendants.map { ($0, depth + 1, current) })
                    // Preserve partial discovery even if the process subsequently
                    // stalls. The normal error path saves every collected node.
                    if nodes.count % 16 == 0 { try save() }
                }
                observation["complete"] = true
                observation["match_count"] = matches.count
                try save()
                try require(matches.count <= 1, "Ambiguous target AX controls: \(matches.count) for \(label)")
                if let control = matches.first {
                    try require(try boolean(control, "AXEnabled"), "Disabled control: \(label)")
                    try checkTime()
                    var actions: CFArray?
                    _ = try checked(AXUIElementCopyActionNames(control, &actions), "AX action names")
                    try require((actions as? [String])?.contains("AXPress") == true, "Control does not support AXPress: \(label)")
                    try foreground(app, window: window, pid: pid)
                    try checkTime()
                    observation["status"] = "pressing"
                    try save()
                    // Never retry an action: even cannotComplete can mean the
                    // target performed it but failed to acknowledge in time.
                    _ = try checked(AXUIElementPerformAction(control, "AXPress" as CFString), "AXPress")
                    observation["pressed"] = true
                    observation["status"] = "pressed"
                    try save()
                    try emit(["pressed": true, "backend": "application-services"])
                    return
                }
            }
            try save()
            // A first children query activates AccessKit asynchronously. Start
            // again at AXWindows, discarding any placeholder/old descendants.
            Thread.sleep(forTimeInterval: 0.1)
        }
        throw ProbeError.failed("Target control count 0 for \(command) named \(label)")
    }
}

func pressControl(_ arguments: [String]) throws {
    try require(arguments.count == 7, "ax-press requires PID, window, command, label, document URIs, and artifact")
    let environment = ProcessInfo.processInfo.environment
    try require(environment["GITHUB_ACTIONS"] == "true" && environment["RUNNER_OS"] == "macOS",
                "AX control probe requires an isolated hosted macOS Actions runner")
    guard let pid = pid_t(arguments[0]), pid > 0 else { throw ProbeError.failed("Invalid target PID") }
    try require(!arguments[1].isEmpty && !arguments[3].isEmpty, "Empty window or control label")
    try require(["press-button", "press-popup"].contains(arguments[2]), "Unknown AX control command")
    let trusted = AXIsProcessTrusted() // Never request a permission prompt on CI.
    let lookup = ControlLookup(artifact: arguments[6], observation: [
        "pid": pid, "window": arguments[1], "command": arguments[2], "label": arguments[3],
        "backend": "application-services", "trusted": trusted, "pressed": false, "status": "starting"
    ])
    try lookup.save()
    if !trusted {
        lookup.observation["status"] = "untrusted"
        try lookup.save()
        try emit(["pressed": false, "trusted": false, "fallback": "system-events"])
        exit(77) // The driver may use its already-authorized System Events path.
    }
    do {
        try lookup.press(pid: pid, windowName: arguments[1], command: arguments[2], label: arguments[3],
                         document: arguments[4], resolvedDocument: arguments[5])
    } catch {
        lookup.observation["status"] = "failed"
        lookup.observation["error"] = String(describing: error)
        try lookup.save()
        throw error
    }
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
    var lines = attributed.string.replacingOccurrences(of: "\r\n", with: "\n")
        .replacingOccurrences(of: "\r", with: "\n").components(separatedBy: "\n")
    if lines.last == "" { lines.removeLast() }
    let expected = fixture.components(separatedBy: "\n")
    try require(lines.count == expected.count, "TextEdit changed the paragraph count")
    var paragraphOffset = 0
    for index in lines.indices {
        let payload = (lines[index] as NSString).range(of: expected[index])
        try require(payload.location != NSNotFound, "TextEdit changed Unicode paragraph payload \(index)")
        let payloadRange = NSRange(location: paragraphOffset + payload.location, length: payload.length)
        let attributes = attributed.attributes(at: payloadRange.location, effectiveRange: nil)
        guard let paragraph = attributes[.paragraphStyle] as? NSParagraphStyle,
              let list = paragraph.textLists.last else {
            throw ProbeError.failed("TextEdit lost the ordered list in paragraph \(index)")
        }
        try require(list.markerFormat.rawValue.contains(NSTextList.MarkerFormat.decimal.rawValue),
                    "TextEdit changed the ordered-list kind in paragraph \(index)")
        try require(list.startingItemNumber == 4, "TextEdit reset the ordered-list start in paragraph \(index)")
        let marker = list.marker(forItemNumber: 4 + index)
        let allowed = [expected[index], "\t\(marker)\t\(expected[index])", "\(marker)\t\(expected[index])"]
        try require(allowed.contains(lines[index]), "TextEdit changed Unicode paragraph payload \(index)")
        var offset = payloadRange.location
        while offset < NSMaxRange(payloadRange) {
            var effective = NSRange(location: 0, length: 0)
            let attributes = attributed.attributes(at: offset, effectiveRange: &effective)
            guard let font = attributes[.font] as? NSFont else {
                throw ProbeError.failed("TextEdit imported payload has no font in paragraph \(index)")
            }
            let traits = NSFontManager.shared.traits(of: font)
            try require(traits.contains(.boldFontMask), "TextEdit lost bold in paragraph \(index)")
            try require(traits.contains(.italicFontMask), "TextEdit lost italic in paragraph \(index)")
            guard let color = (attributes[.foregroundColor] as? NSColor)?.usingColorSpace(.sRGB) else {
                throw ProbeError.failed("TextEdit payload has no sRGB foreground in paragraph \(index)")
            }
            let rgba = [color.redComponent, color.greenComponent, color.blueComponent, color.alphaComponent]
                .map { Int(($0 * 255).rounded()) }
            try require(rgba == [42, 100, 200, 255], "TextEdit changed native HTML RGBA in paragraph \(index): \(rgba)")
            try require(NSMaxRange(effective) > offset, "TextEdit returned an invalid attribute range")
            offset = min(NSMaxRange(effective), NSMaxRange(payloadRange))
        }
        paragraphOffset += (lines[index] as NSString).length + 1
    }
    try emit(["textedit_html_import_unicode_exact": true, "bold": true, "italic": true,
              "ordered_list": true, "list_start": 4, "paragraphs_checked": expected.count,
              "rgba": [42, 100, 200, 255], "types": types()])
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

func nativeHTML() throws {
    guard let data = try boundedData(.html), let html = String(data: data, encoding: .utf8),
          let rich = try boundedData(customType) else {
        throw ProbeError.failed("Native HTML or Textloom envelope is unavailable")
    }
    try require(rich.count > 32 && Array(rich.prefix(8)) == [84, 76, 67, 76, 1, 0, 0, 0],
                "Native feature envelope is malformed")
    try emit(["html": html, "native_envelope_present": true, "types": types()])
}

func rgba(_ color: Any?) -> [Int]? {
    guard let color = (color as? NSColor)?.usingColorSpace(.sRGB) else { return nil }
    return [color.redComponent, color.greenComponent, color.blueComponent, color.alphaComponent]
        .map { Int(($0 * 255).rounded()) }
}

// Inspect TextEdit's actual copied RTF, never an attributed string constructed
// directly from our HTML. List markers and one terminal paragraph LF may be
// added by TextEdit. All payload bytes, soft separators, and attributes must
// survive; typography and the particular installed font are not asserted.
func textEditFeatures(_ arguments: [String]) throws {
    try require(arguments.count == 1, "textedit-features requires a JSON specification")
    guard let specification = try JSONSerialization.jsonObject(with: Data(arguments[0].utf8)) as? [String: Any],
          let paragraphs = specification["paragraphs"] as? [[String: Any]],
          !paragraphs.isEmpty, paragraphs.count <= 8,
          let data = try boundedData(.rtf) else {
        throw ProbeError.failed("Missing feature specification or TextEdit RTF")
    }
    try require(try boundedData(customType) == nil, "TextEdit feature copy retained a stale native envelope")
    let attributed = try NSAttributedString(
        data: data, options: [.documentType: NSAttributedString.DocumentType.rtf], documentAttributes: nil
    )
    var lines = attributed.string.components(separatedBy: "\n")
    if lines.last == "" { lines.removeLast() }
    try require(lines.count == paragraphs.count, "TextEdit changed feature paragraph count: \(lines.count)")
    var offset = 0
    var observations: [[String: Any]] = []
    for index in paragraphs.indices {
        let expected = paragraphs[index]
        guard let text = expected["text"] as? String, !text.isEmpty else {
            throw ProbeError.failed("Feature paragraph requires nonempty text")
        }
        let line = lines[index] as NSString
        let found = line.range(of: text)
        try require(found.location != NSNotFound, "TextEdit changed feature payload \(index): \(lines[index])")
        let range = NSRange(location: offset + found.location, length: found.length)
        let attributes = attributed.attributes(at: range.location, effectiveRange: nil)
        let paragraph = attributes[.paragraphStyle] as? NSParagraphStyle
        var allowed = [text]
        var observation: [String: Any] = ["payload_utf8_exact": true, "soft_breaks": text.filter { $0 == "\u{2028}" }.count]
        if let start = expected["list_start"] as? Int, let number = expected["number"] as? Int {
            guard let list = paragraph?.textLists.last else {
                throw ProbeError.failed("TextEdit lost feature list \(index)")
            }
            try require(list.markerFormat.rawValue.contains(NSTextList.MarkerFormat.decimal.rawValue), "TextEdit changed feature list kind")
            try require(list.startingItemNumber == start, "TextEdit changed feature list start")
            let marker = list.marker(forItemNumber: number)
            allowed += ["\t\(marker)\t\(text)", "\(marker)\t\(text)"]
            observation["list_start"] = start
            observation["item_number"] = number
        } else {
            try require(paragraph?.textLists.isEmpty ?? true, "TextEdit added an unexpected list")
        }
        try require(allowed.contains { Array($0.utf8) == Array(lines[index].utf8) },
                    "TextEdit changed payload bytes, marker, or soft break in paragraph \(index)")
        if expected["quote"] as? Bool == true {
            guard let paragraph = paragraph else { throw ProbeError.failed("TextEdit lost quote paragraph style") }
            try require(paragraph.headIndent > 0 && paragraph.firstLineHeadIndent > 0,
                        "TextEdit lost blockquote indentation")
            observation["head_indent"] = paragraph.headIndent
            observation["first_line_head_indent"] = paragraph.firstLineHeadIndent
        }
        var cursor = range.location
        while cursor < NSMaxRange(range) {
            var effective = NSRange(location: 0, length: 0)
            let run = attributed.attributes(at: cursor, effectiveRange: &effective)
            for (name, key) in [("foreground", NSAttributedString.Key.foregroundColor), ("background", .backgroundColor)] {
                if let expectedColor = specification[name] as? [Int] {
                    try require(rgba(run[key]) == expectedColor, "TextEdit changed \(name) in feature paragraph \(index)")
                }
            }
            if let expectedLink = specification["link"] as? String {
                let actual = (run[.link] as? URL)?.absoluteString ?? (run[.link] as? String)
                try require(actual == expectedLink, "TextEdit changed link in feature paragraph \(index)")
            }
            try require(NSMaxRange(effective) > cursor, "TextEdit returned invalid feature attributes")
            cursor = min(NSMaxRange(effective), NSMaxRange(range))
        }
        observations.append(observation)
        offset += line.length + 1
    }
    try emit(["textedit_rtf_features_exact": true, "paragraphs": observations,
              "checked_specification": specification, "types": types()])
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

func diagnose(_ arguments: [String]) throws {
    try require(arguments.count == 2, "diagnose requires an artifact directory and fixture label")
    let labels = ["native-offer", "textedit-before-paste", "textedit-after-paste", "textedit-after-copy", "feature-import", "failure"]
    try require(labels.contains(arguments[1]), "Unexpected fixture diagnostic label")
    let directory = URL(fileURLWithPath: arguments[0], isDirectory: true)
    let label = arguments[1]
    var observation: [String: Any] = ["fixture_only": true, "types": types()]
    let diagnosticLimit = 64 * 1024
    if let value = NSPasteboard.general.string(forType: .string) {
        try require(value.utf8.count <= diagnosticLimit, "Fixture diagnostic text exceeds its cap")
        try value.write(to: directory.appendingPathComponent("macos-fixture-\(label).plain.txt"), atomically: true, encoding: .utf8)
    }
    for (name, type) in [("plain", NSPasteboard.PasteboardType.string), ("html", .html), ("rtf", .rtf)] {
        guard let data = try boundedData(type) else { continue }
        try require(data.count <= diagnosticLimit, "Fixture diagnostic representation exceeds its cap")
        try data.write(to: directory.appendingPathComponent("macos-fixture-\(label).\(name)"), options: .atomic)
        observation["\(name)_bytes"] = data.count
        if type == .rtf {
            let parsed = try NSAttributedString(
                data: data, options: [.documentType: NSAttributedString.DocumentType.rtf], documentAttributes: nil
            )
            try require(parsed.string.utf8.count <= diagnosticLimit, "Parsed fixture RTF exceeds its diagnostic cap")
            try parsed.string.write(to: directory.appendingPathComponent("macos-fixture-\(label).rtf.txt"), atomically: true, encoding: .utf8)
        }
        if type == .html && label == "native-offer" {
            for explicitUTF8 in [false, true] {
                let name = explicitUTF8 ? "explicit-utf8" : "default"
                var options: [NSAttributedString.DocumentReadingOptionKey: Any] = [.documentType: NSAttributedString.DocumentType.html]
                if explicitUTF8 { options[.characterEncoding] = String.Encoding.utf8.rawValue }
                do {
                    let parsed = try NSAttributedString(data: data, options: options, documentAttributes: nil)
                    try require(parsed.string.utf8.count <= diagnosticLimit, "Parsed fixture HTML exceeds its diagnostic cap")
                    try parsed.string.write(to: directory.appendingPathComponent("macos-fixture-native-offer.html-\(name).txt"), atomically: true, encoding: .utf8)
                    observation["html_\(name)_contains_exact_unicode"] = parsed.string.contains("café 日本語👩🏽‍💻")
                } catch {
                    observation["html_\(name)_error"] = String(describing: error)
                }
            }
        }
    }
    try emit(observation)
}

do {
    let arguments = Array(CommandLine.arguments.dropFirst())
    try require(!arguments.isEmpty, "Missing inspector command")
    switch arguments[0] {
    case "ax-press": try pressControl(Array(arguments.dropFirst()))
    case "prepare": try prepare(Array(arguments.dropFirst()))
    case "native-fixture": try nativeFixture()
    case "native-html": try nativeHTML()
    case "textedit-features": try textEditFeatures(Array(arguments.dropFirst()))
    case "textedit-import": try textEditImport()
    case "external-fixture": try externalFixture()
    case "state": try emit(["types": types()])
    case "plain": try expectPlain(Array(arguments.dropFirst()), fallback: false)
    case "fallback": try expectPlain(Array(arguments.dropFirst()), fallback: true)
    case "diagnose": try diagnose(Array(arguments.dropFirst()))
    default: throw ProbeError.failed("Unknown inspector command")
    }
} catch {
    fputs("macOS clipboard observation failed: \(error)\n", stderr)
    exit(1)
}
