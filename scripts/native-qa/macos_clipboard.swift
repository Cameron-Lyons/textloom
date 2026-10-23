import AppKit
import ApplicationServices
import Foundation
import Darwin

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

struct ControlAXError: Error, CustomStringConvertible {
    let code: AXError
    let operation: String
    var description: String { "\(operation): AX error \(code.rawValue) (\(code))" }
}

// The catch covers discovery ONLY. Even invalidUIElement/cannotComplete from
// AXPress is unreplayable: a failed acknowledgement need not mean no action.
// Injecting discovery, time, and press also lets the hosted helper test this
// exact retry boundary without touching a desktop or sleeping.
func discoverAndPress<T>(deadline: TimeInterval, now: () -> TimeInterval,
                         pause: (TimeInterval) -> Void, begin: (Int) throws -> Void,
                         failed: (Error?) throws -> Void, discover: () throws -> T?,
                         press: (T) throws -> Void) throws {
    var lastError: Error = ProbeError.failed("Target control count 0 after 30 attempts")
    for attempt in 1...30 {
        try require(now() < deadline, "AX lookup exceeded six-second deadline")
        try begin(attempt)
        let candidate: T?
        do {
            candidate = try discover()
        } catch {
            try failed(error)
            guard let failure = error as? ControlAXError, failure.code == .invalidUIElement else { throw error }
            lastError = error
            if attempt < 30 { pause(min(0.1, max(0, deadline - now()))) }
            continue
        }
        if let candidate = candidate {
            try require(now() < deadline, "AX lookup exceeded six-second deadline")
            try press(candidate)
            return
        }
        lastError = ProbeError.failed("Target control count 0 after 30 attempts")
        try failed(nil)
        if attempt < 30 { pause(min(0.1, max(0, deadline - now()))) }
    }
    throw lastError
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
    var attempts: [[String: Any]] = []

    init(artifact: String, observation: [String: Any]) {
        self.artifact = URL(fileURLWithPath: artifact)
        self.observation = observation
    }

    func snapshot() -> [String: Any] {
        var result = observation
        result["nodes"] = nodes
        result["attribute_errors"] = errors
        result["elapsed_seconds"] = 6 - (deadline - ProcessInfo.processInfo.systemUptime)
        return result
    }

    func save() throws {
        var result = snapshot()
        result["failed_attempts"] = attempts
        let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys, .prettyPrinted])
        try data.write(to: artifact, options: .atomic)
    }

    func beginAttempt(_ attempt: Int) throws {
        nodes = []
        errors = []
        for key in ["error", "windows", "window_count", "match_count", "current_node", "retryable"] {
            observation.removeValue(forKey: key)
        }
        observation["attempt"] = attempt
        observation["status"] = "discovering"
        observation["complete"] = false
        try save()
    }

    func failedAttempt(_ error: Error?) throws {
        observation["status"] = error == nil ? "not-found" : "discovery-failed"
        if let error = error { observation["error"] = String(describing: error) }
        observation["retryable"] = error == nil || (error as? ControlAXError)?.code == .invalidUIElement
        attempts.append(snapshot())
        try save()
    }

    func checkTime() throws {
        try require(ProcessInfo.processInfo.systemUptime < deadline, "AX lookup exceeded six-second deadline")
    }

    func checked(_ error: AXError, _ operation: String, optional: Bool = false) throws -> Bool {
        if error == .success { return true }
        if errors.count < 128 {
            errors.append(["operation": operation, "code": Int(error.rawValue),
                           "error": String(describing: error)])
        }
        if optional && (error == .attributeUnsupported || error == .noValue) { return false }
        throw ControlAXError(code: error, operation: operation)
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
        // The system-wide element applies the timeout to ALL queried elements,
        // unlike setting it on just the app. A single stalled IPC cannot consume
        // the driver's ten-second subprocess budget.
        _ = try checked(AXUIElementSetMessagingTimeout(AXUIElementCreateSystemWide(), 0.25), "set AX timeout")
        try discoverAndPress(
            deadline: deadline, now: { ProcessInfo.processInfo.systemUptime },
            pause: { Thread.sleep(forTimeInterval: $0) }, begin: beginAttempt, failed: failedAttempt,
            discover: { () throws -> AXUIElement? in
            // Activation can replace AccessKit's placeholder context. Discard
            // the ENTIRE failed scan and reacquire application/windows/children.
            let app = AXUIElementCreateApplication(pid)
            var windows: [AXUIElement] = []
            var windowInventory: [[String: Any]] = []
            for window in try children(app, attribute: "AXWindows") {
                windowInventory.append(["index": windowInventory.count])
                observation["windows"] = windowInventory
                let title = try string(window, "AXTitle") ?? ""
                windowInventory[windowInventory.count - 1]["title"] = String(title.prefix(200))
                observation["windows"] = windowInventory
                let uri = try string(window, "AXDocument") ?? ""
                windowInventory[windowInventory.count - 1]["document"] = String(uri.prefix(512))
                observation["windows"] = windowInventory
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
                    observation["current_node"] = ["index": current, "parent": parent, "depth": depth]
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
                    return control
                }
            }
            return nil
        }, press: { control in
            observation["status"] = "pressing"
            observation["press_attempted"] = true
            try save()
            try checkTime()
            _ = try checked(AXUIElementPerformAction(control, "AXPress" as CFString), "AXPress")
            observation["pressed"] = true
            observation["status"] = "pressed"
            try save()
            try emit(["pressed": true, "backend": "application-services"])
        })
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
        "backend": "application-services", "trusted": trusted, "pressed": false,
        "press_attempted": false, "status": "starting"
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

// Runs the production retry controller with fake candidate IDs and a virtual
// clock. This touches neither AX nor the pasteboard and takes no real sleeps.
// A partial scan contains a matching candidate BEFORE its later stale node;
// the only permissible action uses the candidate from a subsequent full scan.
func controlRetrySelfTest() throws {
    var passed: [String] = []
    func scenario(_ name: String, stale: Int = 0, empty: Int = 0,
                  discoveryError: AXError? = nil, semanticError: Bool = false,
                  actionError: AXError? = nil, afterActionError: Bool = false,
                  duration: TimeInterval = 0, expectedAttempts: Int,
                  expectedPresses: Int, expectError: Bool) throws {
        let artifact = FileManager.default.temporaryDirectory.appendingPathComponent("textloom-ax-retry-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: artifact) }
        let lookup = ControlLookup(artifact: artifact.path, observation: ["pressed": false])
        var time: TimeInterval = 0
        var discoveries = 0
        var presses: [Int] = []
        var failure: Error?
        do {
            try discoverAndPress(deadline: 6, now: { time }, pause: { time += $0 }, begin: {
                discoveries = $0
                try lookup.beginAttempt($0)
            }, failed: lookup.failedAttempt, discover: { () throws -> Int? in
                time += duration
                if discoveries <= empty {
                    lookup.observation["match_count"] = 0
                    lookup.observation["complete"] = true
                    return nil
                }
                lookup.nodes = [["role": "AXButton", "title": "Clear formatting", "candidate": discoveries]]
                lookup.observation["match_count"] = 1
                lookup.observation["current_node"] = ["index": 2, "parent": 0, "depth": 1]
                if let code = discoveryError { _ = try lookup.checked(code, "AXRole") }
                if semanticError { throw ProbeError.failed("Ambiguous target AX controls") }
                if discoveries <= stale + empty { _ = try lookup.checked(.invalidUIElement, "AXRole") }
                lookup.observation["complete"] = true
                return discoveries
            }, press: { candidate in
                presses.append(candidate)
                if let code = actionError { _ = try lookup.checked(code, "AXPress") }
                lookup.observation["pressed"] = true
                try lookup.save()
                if afterActionError { throw ProbeError.failed("Recording the successful action failed") }
            })
        } catch {
            failure = error
        }
        try require(discoveries == expectedAttempts, "\(name): wrong discovery count \(discoveries)")
        try require(presses.count == expectedPresses, "\(name): wrong press count \(presses.count)")
        try require((failure != nil) == expectError, "\(name): wrong success/failure outcome")
        if expectedPresses == 1 {
            try require(presses == [expectedAttempts], "\(name): pressed the stale partial match")
        }
        if let expectedCode = actionError ?? discoveryError {
            try require((failure as? ControlAXError)?.code == expectedCode, "\(name): lost the terminal AX error")
        }
        try require(lookup.attempts.count == expectedAttempts - expectedPresses,
                    "\(name): failed discovery history was lost")
        for (index, attempt) in lookup.attempts.enumerated() {
            try require(attempt["attempt"] as? Int == index + 1, "\(name): wrong attempt identity")
            let nodes = attempt["nodes"] as? [[String: Any]]
            if index >= empty {
                try require(nodes?.first?["candidate"] as? Int == index + 1, "\(name): prior inventory overwritten")
            }
            try require(attempt["complete"] as? Bool == (index < empty), "\(name): wrong scan completion status")
            if index >= empty && (stale > 0 || discoveryError != nil) {
                let errors = attempt["attribute_errors"] as? [[String: Any]]
                try require(errors?.last?["code"] as? Int == Int((discoveryError ?? .invalidUIElement).rawValue),
                            "\(name): failed AX code was lost")
            }
        }
        // Exercise the actual diagnostic serializer, including independent
        // snapshots (without recursively embedding all preceding attempts).
        try lookup.save()
        let saved = try JSONSerialization.jsonObject(with: Data(contentsOf: artifact)) as? [String: Any]
        let history = saved?["failed_attempts"] as? [[String: Any]]
        try require(history?.count == lookup.attempts.count, "\(name): failed attempts not persisted")
        try require(history?.allSatisfy { $0["failed_attempts"] == nil } == true, "\(name): recursive attempt history")
        passed.append(name)
    }
    try scenario("stale partial match is discarded", stale: 1, expectedAttempts: 2, expectedPresses: 1, expectError: false)
    try scenario("empty and stale scans recover", stale: 1, empty: 1, expectedAttempts: 3, expectedPresses: 1, expectError: false)
    try scenario("permanent stale tree stops at 30", stale: 100, expectedAttempts: 30, expectedPresses: 0, expectError: true)
    try scenario("fixed deadline bounds stale retries", stale: 100, duration: 2.1, expectedAttempts: 3, expectedPresses: 0, expectError: true)
    try scenario("messaging failure is terminal", discoveryError: .cannotComplete, expectedAttempts: 1, expectedPresses: 0, expectError: true)
    try scenario("ambiguous match is terminal", semanticError: true, expectedAttempts: 1, expectedPresses: 0, expectError: true)
    try scenario("invalid-element press is not replayed", actionError: .invalidUIElement, expectedAttempts: 1, expectedPresses: 1, expectError: true)
    try scenario("uncertain press is not replayed", actionError: .cannotComplete, expectedAttempts: 1, expectedPresses: 1, expectError: true)
    try scenario("post-press failure is not replayed", afterActionError: true, expectedAttempts: 1, expectedPresses: 1, expectError: true)
    let observation: [String: Any] = ["ax_retry_self_test": true, "checks": passed]
    let data = try JSONSerialization.data(withJSONObject: observation, options: [.sortedKeys])
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

// NSPasteboard has no read lock. Read every representation between generation
// checks, and never publish a partial/mixed-generation snapshot as evidence.
func nativeOfferSnapshot(_ expected: Int, generation: () -> Int,
                         readTypes: () -> [String],
                         readData: (NSPasteboard.PasteboardType) -> Data?) throws -> ([String: Any], [(String, Data)]) {
    let before = generation()
    try require(before == expected, "Native source changed before capture")
    let advertised = readTypes()
    try require(advertised.count <= 32 && advertised.allSatisfy { $0.utf8.count <= 256 },
                "Native source type inventory exceeds its cap")
    let representations: [(String, NSPasteboard.PasteboardType)] = [
        ("html", .html), ("envelope", customType), ("plain", .string)
    ]
    var captured: [(String, Data)] = []
    var sizes: [String: Int] = [:]
    for (name, type) in representations {
        try require(advertised.contains(type.rawValue), "Native source does not advertise \(name)")
        guard let data = readData(type) else { throw ProbeError.failed("Native source \(name) read failed") }
        try require(!data.isEmpty && data.count <= 64 * 1024, "Native source \(name) exceeds its capture bounds")
        captured.append((name, data))
        sizes[name] = data.count
    }
    let after = generation()
    try require(after == before, "Native source changed during capture")
    return (["native_source_snapshot": true, "change_count": before,
             "types": advertised, "representation_bytes": sizes], captured)
}

func captureNativeOffer(_ arguments: [String]) throws {
    try require(arguments.count == 3, "capture-native-offer requires artifact directory, source label, and generation")
    let label = arguments[1]
    try require(["quote-native-reused-source", "quote-native-fresh-source"].contains(label),
                "Unexpected native source capture label")
    guard let expected = Int(arguments[2]) else { throw ProbeError.failed("Invalid native source generation") }
    let pasteboard = NSPasteboard.general
    let (snapshot, captured) = try nativeOfferSnapshot(expected,
        generation: { pasteboard.changeCount }, readTypes: types,
        readData: { pasteboard.data(forType: $0) })
    var observation = snapshot
    observation["label"] = label
    let prefix = URL(fileURLWithPath: arguments[0], isDirectory: true)
        .appendingPathComponent("macos-fixture-\(label)")
    for (name, data) in captured {
        try data.write(to: prefix.appendingPathExtension(name), options: .withoutOverwriting)
    }
    // The manifest is written last and carries the captured generation. Generic
    // emit() would replace it with an unrelated post-write pasteboard read.
    let manifest = try JSONSerialization.data(withJSONObject: observation, options: [.sortedKeys])
    try manifest.write(to: prefix.appendingPathExtension("json"), options: .withoutOverwriting)
    print(String(decoding: manifest, as: UTF8.self))
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

func prepareImport(_ arguments: [String]) throws {
    try require(arguments.count == 1, "prepare-import requires a closed private RTF path")
    let paragraph = NSMutableParagraphStyle()
    try writeRTF(NSAttributedString(string: "paste target", attributes: [
        .font: NSFont.systemFont(ofSize: 14), .paragraphStyle: paragraph
    ]), arguments[0])
    // Do not touch the clipboard: the next paste must use the original offer.
    try emit(["neutral_import_prepared": true])
}

func visibleIdentity(_ expected: [String: Any], _ observed: [String: Any]) throws {
    try require(observed["pid"] as? Int == expected["pid"] as? Int, "Visible capture wrong PID")
    try require(observed["bundle"] as? String == "com.apple.TextEdit", "Visible capture is not TextEdit")
    guard let window = expected["window"] as? String, let title = observed["window"] as? String,
          let document = observed["document"] as? String, !document.isEmpty else {
        throw ProbeError.failed("Visible capture missing window/document identity")
    }
    try require(title == window || title == window + ".rtf", "Visible capture wrong window")
    try require(document == expected["document"] as? String || document == expected["resolved_document"] as? String,
                "Visible capture wrong document")
    try require(observed["foreground_verified"] as? Bool == true && observed["text_area_in_window"] as? Bool == true,
                "Visible capture lost focused text area/window")
    guard let text = expected["text"] as? String, let actual = observed["text"] as? String else {
        throw ProbeError.failed("Visible capture missing exact text")
    }
    // HTML import may append one paragraph terminator, as in the RTF checks.
    try require(Data(actual.utf8) == Data(text.utf8) || Data(actual.utf8) == Data((text + "\n").utf8),
                "Visible capture changed fixture text")
    try require(observed["change_count"] as? Int == expected["generation"] as? Int,
                "Visible capture happened after copy or source replacement")
}

func visibleRect(_ lookup: ControlLookup, _ element: AXUIElement) throws -> CGRect {
    guard let position = try lookup.read(element, "AXPosition", optional: false),
          let size = try lookup.read(element, "AXSize", optional: false),
          CFGetTypeID(position) == AXValueGetTypeID(), CFGetTypeID(size) == AXValueGetTypeID() else {
        throw ProbeError.failed("Visible capture missing AX bounds")
    }
    var point = CGPoint.zero
    var dimensions = CGSize.zero
    try require(AXValueGetValue(position as! AXValue, .cgPoint, &point)
                && AXValueGetValue(size as! AXValue, .cgSize, &dimensions), "Invalid AX bounds type")
    try require(point.x.isFinite && point.y.isFinite && dimensions.width.isFinite && dimensions.height.isFinite
                && dimensions.width > 0 && dimensions.height > 0, "Invalid AX bounds")
    return CGRect(origin: point, size: dimensions)
}

func rectFields(_ rect: CGRect) -> [String: Double] {
    ["x": Double(rect.minX), "y": Double(rect.minY), "width": Double(rect.width), "height": Double(rect.height)]
}

// Only the already-focused owned document is read. No activation, traversal,
// desktop capture, or focus fallback is performed by this helper.
func textEditVisible(_ arguments: [String]) throws {
    let environment = ProcessInfo.processInfo.environment
    try require(environment["GITHUB_ACTIONS"] == "true" && environment["RUNNER_OS"] == "macOS",
                "Visible comparison requires an isolated hosted macOS Actions runner")
    try require(arguments.count == 1 && arguments[0].utf8.count <= 16384, "Visible comparison requires bounded JSON")
    guard let expected = try JSONSerialization.jsonObject(with: Data(arguments[0].utf8)) as? [String: Any],
          let pid = expected["pid"] as? Int, pid > 0 && pid <= Int(Int32.max),
          let label = expected["label"] as? String,
          ["quote-native-fresh", "quote-visible-plain", "quote-visible-indented"].contains(label),
          let stage = expected["stage"] as? String, ["neutral", "pasted"].contains(stage),
          let directory = expected["directory"] as? String,
          let neutralFile = expected["neutral_file"] as? String,
          let expectedText = expected["text"] as? String, expectedText.utf8.count <= 128 else {
        throw ProbeError.failed("Invalid visible comparison specification")
    }
    try require(AXIsProcessTrusted(), "Visible comparison AX helper is untrusted")
    let prefix = URL(fileURLWithPath: directory, isDirectory: true).appendingPathComponent("macos-fixture-\(label)")
    let artifact = prefix.appendingPathExtension("visible-\(stage).json")
    let lookup = ControlLookup(artifact: artifact.path, observation: [:])
    _ = try lookup.checked(AXUIElementSetMessagingTimeout(AXUIElementCreateSystemWide(), 0.25), "set visible AX timeout")
    let app = AXUIElementCreateApplication(pid_t(pid))
    let window = try lookup.element(lookup.read(app, "AXFocusedWindow", optional: false))
    let textArea = try lookup.element(lookup.read(app, "AXFocusedUIElement", optional: false))
    func identity() throws -> [String: Any] {
        try lookup.foreground(app, window: window, pid: pid_t(pid))
        for element in [app, window, textArea] {
            var owner: pid_t = 0
            _ = try lookup.checked(AXUIElementGetPid(element, &owner), "visible element PID")
            try require(owner == pid_t(pid), "Visible AX element belongs to wrong PID")
        }
        let focused = try lookup.element(lookup.read(app, "AXFocusedUIElement", optional: false))
        let ownerWindow = try lookup.element(lookup.read(textArea, "AXWindow", optional: false))
        try require(CFEqual(focused, textArea) && CFEqual(ownerWindow, window), "Visible text area/window changed")
        try require(try lookup.string(textArea, "AXRole", optional: false) == "AXTextArea", "Visible focus is not a text area")
        let observed: [String: Any] = [
            "pid": pid, "bundle": NSRunningApplication(processIdentifier: pid_t(pid))?.bundleIdentifier ?? "",
            "window": try lookup.string(window, "AXTitle", optional: false) ?? "",
            "document": try lookup.string(window, "AXDocument", optional: false) ?? "",
            "text": try lookup.string(textArea, "AXValue", optional: false) ?? "",
            "foreground_verified": true, "text_area_in_window": true,
            "change_count": NSPasteboard.general.changeCount
        ]
        try visibleIdentity(expected, observed)
        return observed
    }
    var observation = try identity()
    let windowRect = try visibleRect(lookup, window)
    let areaRect: CGRect?
    do {
        areaRect = try visibleRect(lookup, textArea)
        observation["text_area_bounds_available"] = true
    } catch {
        if let failure = error as? ControlAXError,
           failure.code == .invalidUIElement || failure.code == .cannotComplete { throw error }
        areaRect = nil
        observation["text_area_bounds_available"] = false
        observation["text_area_bounds_error"] = String(describing: error)
    }
    observation["window_bounds"] = rectFields(windowRect)
    if let areaRect = areaRect { observation["text_area_bounds"] = rectFields(areaRect) }
    observation["label"] = label
    observation["stage"] = stage
    observation["source_manifest"] = label == "quote-native-fresh"
        ? "macos-fixture-quote-native-fresh-source.json" : "macos-fixture-\(label).source.json"
    observation["neutral_fixture"] = prefix.appendingPathExtension("neutral.rtf").lastPathComponent
    observation["timestamp_utc"] = ISO8601DateFormatter().string(from: Date())
    observation["macos"] = ProcessInfo.processInfo.operatingSystemVersionString
    let bundle = NSRunningApplication(processIdentifier: pid_t(pid))?.bundleURL.flatMap { Bundle(url: $0) }
    observation["textedit_version"] = bundle?.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "unavailable"
    observation["textedit_build"] = bundle?.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "unavailable"
    observation["scope"] = "two observed paragraph starts; no witnessed wrapping or automatic visual pass"
    var geometry: [[String: Any]] = []
    for offset in stage == "neutral" ? [0] : [0, 6] {
        try lookup.checkTime()
        var range = CFRange(location: offset, length: 1)
        guard let parameter = AXValueCreate(.cfRange, &range) else { throw ProbeError.failed("Could not create visible range") }
        var result: CFTypeRef?
        let error = AXUIElementCopyParameterizedAttributeValue(textArea, "AXBoundsForRange" as CFString, parameter, &result)
        var item: [String: Any] = ["utf16_location": offset, "utf16_length": 1]
        if error == .success, let result = result, CFGetTypeID(result) == AXValueGetTypeID() {
            var bounds = CGRect.zero
            if AXValueGetValue(result as! AXValue, .cgRect, &bounds), !bounds.isEmpty, !bounds.isInfinite, !bounds.isNull {
                item["screen_bounds"] = rectFields(bounds)
                if let areaRect = areaRect {
                    item["available"] = true
                    item["text_area_relative_bounds"] = rectFields(bounds.offsetBy(dx: -areaRect.minX, dy: -areaRect.minY))
                } else { item["available"] = false; item["error"] = "text-area origin unavailable for normalization" }
            } else { item["available"] = false; item["error"] = "invalid or empty range bounds" }
        } else {
            if error == .invalidUIElement || error == .cannotComplete { throw ControlAXError(code: error, operation: "visible range bounds") }
            item["available"] = false
            item["error"] = "AXBoundsForRange: \(error.rawValue)"
        }
        geometry.append(item)
    }
    observation["character_bounds"] = geometry
    if stage == "neutral" {
        let url = URL(fileURLWithPath: neutralFile)
        let data = try Data(contentsOf: url)
        try require(data.count <= 65536, "Neutral fixture exceeds cap")
        let value = try NSAttributedString(data: data, options: [.documentType: NSAttributedString.DocumentType.rtf], documentAttributes: nil)
        try require(value.string == "paste target", "Wrong neutral fixture payload")
        var offset = 0
        while offset < value.length {
            var range = NSRange(location: 0, length: 0)
            let attributes = value.attributes(at: offset, effectiveRange: &range)
            guard let style = attributes[.paragraphStyle] as? NSParagraphStyle else { throw ProbeError.failed("Neutral fixture lacks paragraph style") }
            try require(style.headIndent == 0 && style.firstLineHeadIndent == 0 && style.tailIndent == 0 && style.textBlocks.isEmpty && style.textLists.isEmpty,
                        "Neutral destination retained paragraph formatting")
            try require(NSMaxRange(range) > offset, "Invalid neutral attributes")
            offset = NSMaxRange(range)
        }
        observation["neutral_attribute_runs"] = try featureAttributes(value)
        try data.write(to: prefix.appendingPathExtension("neutral.rtf"), options: .withoutOverwriting)
    } else {
        // AX and CG use screen coordinates. Require a unique same-PID, normal
        // window with the verified AX frame; never capture a display rectangle.
        guard let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]], windows.count <= 512 else {
            throw ProbeError.failed("Visible window inventory unavailable or exceeds cap")
        }
        let matches = windows.filter { entry in
            guard entry[kCGWindowOwnerPID as String] as? Int == pid,
                  entry[kCGWindowLayer as String] as? Int == 0,
                  let fields = entry[kCGWindowBounds as String] as? [String: Any],
                  let bounds = CGRect(dictionaryRepresentation: fields as CFDictionary) else { return false }
            return abs(bounds.minX - windowRect.minX) < 1 && abs(bounds.minY - windowRect.minY) < 1
                && abs(bounds.width - windowRect.width) < 1 && abs(bounds.height - windowRect.height) < 1
        }
        try require(matches.count == 1, "Visible screenshot window identity is ambiguous")
        guard let windowID = matches[0][kCGWindowNumber as String] as? UInt32 else { throw ProbeError.failed("Visible window has no ID") }
        try require(windowRect.width <= 4096 && windowRect.height <= 4096, "Visible window exceeds screenshot cap")
        observation["cg_window_id"] = windowID
        _ = try identity()
        let temporary = prefix.appendingPathExtension(UUID().uuidString + ".png")
        defer { try? FileManager.default.removeItem(at: temporary) }
        let stderrURL = prefix.appendingPathExtension("screenshot.stderr.log")
        try Data().write(to: stderrURL, options: .withoutOverwriting)
        let stderr = try FileHandle(forWritingTo: stderrURL)
        defer { try? stderr.close() }
        let capture = Process()
        capture.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        capture.arguments = ["-x", "-o", "-l", String(windowID), "-t", "png", temporary.path]
        capture.standardError = stderr
        capture.standardOutput = FileHandle.nullDevice
        try capture.run()
        let deadline = ProcessInfo.processInfo.systemUptime + 2
        while capture.isRunning && ProcessInfo.processInfo.systemUptime < deadline { Thread.sleep(forTimeInterval: 0.02) }
        if capture.isRunning {
            capture.terminate()
            let stop = ProcessInfo.processInfo.systemUptime + 0.25
            while capture.isRunning && ProcessInfo.processInfo.systemUptime < stop { Thread.sleep(forTimeInterval: 0.01) }
            if capture.isRunning { _ = Darwin.kill(capture.processIdentifier, SIGKILL) }
            throw ProbeError.failed("Owned-window screenshot timed out")
        }
        try require(capture.terminationStatus == 0, "Owned-window screenshot failed; see scoped stderr")
        let attributes = try FileManager.default.attributesOfItem(atPath: temporary.path)
        try require(((attributes[.size] as? NSNumber)?.intValue ?? Int.max) <= 8 * 1024 * 1024, "Screenshot exceeds byte cap")
        let png = try Data(contentsOf: temporary)
        guard let bitmap = NSBitmapImageRep(data: png), bitmap.pixelsWide > 0 && bitmap.pixelsHigh > 0,
              bitmap.pixelsWide <= 8192 && bitmap.pixelsHigh <= 8192 else { throw ProbeError.failed("Invalid owned-window screenshot") }
        observation["screenshot"] = prefix.appendingPathExtension("visible.png").lastPathComponent
        observation["screenshot_pixels"] = [bitmap.pixelsWide, bitmap.pixelsHigh]
        // Recheck exact identity, clipboard and viewport after capture before
        // publishing any image as fixture evidence.
        _ = try identity()
        try require(try visibleRect(lookup, window) == windowRect, "Visible window moved during capture")
        if let areaRect = areaRect {
            try require(try visibleRect(lookup, textArea) == areaRect, "Visible text area moved during capture")
        }
        guard let current = CGWindowListCopyWindowInfo(.optionIncludingWindow, windowID) as? [[String: Any]],
              current.count == 1,
              current[0][kCGWindowOwnerPID as String] as? Int == pid,
              current[0][kCGWindowNumber as String] as? UInt32 == windowID else {
            throw ProbeError.failed("Screenshot window identity changed during capture")
        }
        try png.write(to: prefix.appendingPathExtension("visible.png"), options: .withoutOverwriting)
    }
    _ = try identity()
    observation["completed_utc"] = ISO8601DateFormatter().string(from: Date())
    observation["visible_capture_verified"] = true
    let result = try JSONSerialization.data(withJSONObject: observation, options: [.sortedKeys])
    try result.write(to: artifact, options: .withoutOverwriting)
    print(String(decoding: result, as: UTF8.self))
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

func featureSpecification(_ argument: String) throws -> ([String: Any], [[String: Any]]) {
    guard let specification = try JSONSerialization.jsonObject(with: Data(argument.utf8)) as? [String: Any],
          let paragraphs = specification["paragraphs"] as? [[String: Any]],
          !paragraphs.isEmpty, paragraphs.count <= 8 else {
        throw ProbeError.failed("Missing feature specification")
    }
    try require(paragraphs.allSatisfy { ($0["text"] as? String).map { !$0.isEmpty } == true },
                "Feature paragraphs require nonempty text")
    return (specification, paragraphs)
}

func textBlockAttributes(_ block: NSTextBlock) -> [String: Any] {
    var layers: [String: Any] = [:]
    let layerKinds: [(String, NSTextBlock.Layer)] = [("margin", .margin), ("padding", .padding), ("border", .border)]
    let edges: [(String, NSRectEdge)] = [("min_x", .minX), ("max_x", .maxX), ("min_y", .minY), ("max_y", .maxY)]
    for (name, layer) in layerKinds {
        var widths: [String: Any] = [:]
        for (edgeName, edge) in edges {
            let width: [String: Any] = ["width": block.width(for: layer, edge: edge),
                                       "value_type": block.widthValueType(for: layer, edge: edge).rawValue]
            widths[edgeName] = width
        }
        layers[name] = widths
    }
    return ["class": String(describing: type(of: block)), "description": String(describing: block), "layers": layers]
}

// Keep all attribute keys and text-block descriptions, including representations
// the strict indentation oracle does not yet understand. This is evidence, not
// an alternative path to passing a quote assertion.
func featureAttributes(_ attributed: NSAttributedString) throws -> [[String: Any]] {
    try require(attributed.length <= 8192, "Feature attribute diagnostic exceeds its cap")
    var observations: [[String: Any]] = []
    var offset = 0
    while offset < attributed.length {
        try require(observations.count < 64, "Too many feature attribute runs")
        var range = NSRange(location: 0, length: 0)
        let attributes = attributed.attributes(at: offset, effectiveRange: &range)
        try require(NSMaxRange(range) > offset, "Invalid feature attribute range")
        var descriptions: [String: Any] = [:]
        for (key, value) in attributes {
            descriptions[key.rawValue] = ["class": String(describing: type(of: value)),
                                          "value": String(String(describing: value).prefix(2048))]
        }
        var observation: [String: Any] = [
            "location": range.location, "length": range.length,
            "text": (attributed.string as NSString).substring(with: range),
            "attributes": descriptions
        ]
        if let paragraph = attributes[.paragraphStyle] as? NSParagraphStyle {
            try require(paragraph.textBlocks.count <= 8 && paragraph.textLists.count <= 8,
                        "Too many feature text blocks or lists")
            let paragraphObservation: [String: Any] = [
                "head_indent": paragraph.headIndent,
                "first_line_head_indent": paragraph.firstLineHeadIndent,
                "tail_indent": paragraph.tailIndent,
                "text_lists": paragraph.textLists.map { String(describing: $0) },
                "text_blocks": paragraph.textBlocks.map { textBlockAttributes($0) }
            ]
            observation["paragraph"] = paragraphObservation
        }
        observations.append(observation)
        offset = NSMaxRange(range)
    }
    return observations
}

func validateFeatures(_ attributed: NSAttributedString, _ specification: [String: Any],
                      _ paragraphs: [[String: Any]]) throws -> [[String: Any]] {
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
    return observations
}

// Semantic mismatches are explicit false observations so the driver can retain
// them and collect the remaining independent checks. Transport, parsing, stale
// envelopes, and malformed specifications still fail the command immediately.
func textEditFeatures(_ arguments: [String]) throws {
    try require(arguments.count == 1, "textedit-features requires a JSON specification")
    let (specification, paragraphs) = try featureSpecification(arguments[0])
    guard let data = try boundedData(.rtf) else { throw ProbeError.failed("Missing TextEdit RTF") }
    try require(try boundedData(customType) == nil, "TextEdit feature copy retained a stale native envelope")
    let attributed = try NSAttributedString(
        data: data, options: [.documentType: NSAttributedString.DocumentType.rtf], documentAttributes: nil
    )
    var result: [String: Any] = [
        "checked_specification": specification, "types": types(),
        "attribute_runs": try featureAttributes(attributed)
    ]
    do {
        result["paragraphs"] = try validateFeatures(attributed, specification, paragraphs)
        result["textedit_rtf_features_exact"] = true
    } catch {
        result["textedit_rtf_features_exact"] = false
        result["error"] = String(describing: error)
    }
    try emit(result)
}

func featureSelfTest() throws {
    let paragraphs: [[String: Any]] = [["text": "quote", "quote": true]]
    let specification: [String: Any] = ["paragraphs": paragraphs]
    var checks: [String] = []
    for (head, first, expected) in [(36.0, 36.0, true), (0, 36, false), (36, 0, false), (0, 0, false)] {
        let style = NSMutableParagraphStyle()
        style.headIndent = CGFloat(head)
        style.firstLineHeadIndent = CGFloat(first)
        let value = NSAttributedString(string: "quote\n", attributes: [.paragraphStyle: style])
        var matched = false
        do { _ = try validateFeatures(value, specification, paragraphs); matched = true } catch {}
        try require(matched == expected, "Feature self-test changed the strict quote oracle")
        _ = try featureAttributes(value)
        checks.append("head=\(head), first=\(first), accepted=\(expected)")
    }
    let style = NSMutableParagraphStyle()
    style.textBlocks = [NSTextBlock()]
    let value = NSAttributedString(string: "quote\n", attributes: [.paragraphStyle: style])
    var matched = false
    do { _ = try validateFeatures(value, specification, paragraphs); matched = true } catch {}
    try require(!matched, "A text block alone must not bypass the quote assertion")
    _ = try featureAttributes(value)
    checks.append("text-block presence alone is not a pass")
    for (name, before, after, missing, oversized, expected) in [
        ("stable source bytes", 7, 7, false, false, true),
        ("source changed before capture", 8, 8, false, false, false),
        ("source changed during capture", 7, 8, false, false, false),
        ("advertised source read failed", 7, 7, true, false, false),
        ("oversized source rejected", 7, 7, false, true, false)
    ] {
        var reads = 0
        var accepted = false
        do {
            let (snapshot, captured) = try nativeOfferSnapshot(7, generation: {
                reads += 1
                return reads == 1 ? before : after
            }, readTypes: { [NSPasteboard.PasteboardType.html.rawValue, customType.rawValue, NSPasteboard.PasteboardType.string.rawValue] },
            readData: { _ in missing ? nil : (oversized ? Data(repeating: 0, count: 65537) : Data([0, 255, 13, 10])) })
            try require(snapshot["change_count"] as? Int == 7 && captured.allSatisfy { $0.1 == Data([0, 255, 13, 10]) },
                        "Source capture altered raw bytes or generation")
            accepted = true
        } catch {}
        try require(accepted == expected, "Native source self-test failed: \(name)")
        checks.append(name)
    }
    let visibleExpected: [String: Any] = ["pid": 42, "window": "fixture", "document": "file:///fixture.rtf",
        "resolved_document": "file:///private/fixture.rtf", "generation": 7, "text": "quote\ncontinued"]
    let visibleObserved: [String: Any] = ["pid": 42, "bundle": "com.apple.TextEdit", "window": "fixture.rtf",
        "document": "file:///fixture.rtf", "change_count": 7, "text": "quote\ncontinued\n",
        "foreground_verified": true, "text_area_in_window": true]
    try visibleIdentity(visibleExpected, visibleObserved)
    checks.append("visible identity and terminal paragraph LF accepted")
    for (key, value) in [("pid", 99 as Any), ("window", "other"), ("document", "file:///other.rtf"),
                          ("text", "wrong"), ("change_count", 8), ("foreground_verified", false),
                          ("text_area_in_window", false), ("bundle", "other.application")] {
        var altered = visibleObserved
        altered[key] = value
        var accepted = false
        do { try visibleIdentity(visibleExpected, altered); accepted = true } catch {}
        try require(!accepted, "Visible self-test accepted changed \(key)")
        checks.append("visible capture rejects changed \(key)")
    }
    try emit(["feature_self_test": true, "checks": checks])
}

// Parser-only controls are never a native interoperability pass. The same
// bounded HTML file is also offered to TextEdit by the native comparison.
func inspectHTML(_ arguments: [String]) throws {
    try require(arguments.count == 2, "inspect-html requires a fixture HTML path and specification")
    let (specification, paragraphs) = try featureSpecification(arguments[1])
    let data = try Data(contentsOf: URL(fileURLWithPath: arguments[0]))
    try require(data.count <= 8192, "HTML comparison fixture exceeds its cap")
    var result: [String: Any] = ["source": "appkit-html-parser", "checked_specification": specification]
    let attributed: NSAttributedString
    do {
        attributed = try NSAttributedString(data: data, options: [
            .documentType: NSAttributedString.DocumentType.html,
            .characterEncoding: String.Encoding.utf8.rawValue
        ], documentAttributes: nil)
    } catch {
        result["appkit_html_features_exact"] = false
        result["error"] = String(describing: error)
        try emit(result)
        return
    }
    result["attribute_runs"] = try featureAttributes(attributed)
    result["text"] = attributed.string
    try writeRTF(attributed, arguments[0] + ".appkit.rtf")
    do {
        result["paragraphs"] = try validateFeatures(attributed, specification, paragraphs)
        result["appkit_html_features_exact"] = true
    } catch {
        result["appkit_html_features_exact"] = false
        result["error"] = String(describing: error)
    }
    try emit(result)
}

func offerHTML(_ arguments: [String]) throws {
    let environment = ProcessInfo.processInfo.environment
    try require(environment["GITHUB_ACTIONS"] == "true" && environment["RUNNER_OS"] == "macOS",
                "HTML comparison offers require an isolated hosted macOS Actions runner")
    try require(arguments.count == 2, "offer-html requires a fixture HTML path and plain text")
    let data = try Data(contentsOf: URL(fileURLWithPath: arguments[0]))
    try require(data.count <= 8192 && arguments[1].utf8.count <= 8192, "HTML comparison fixture exceeds its cap")
    let pasteboard = NSPasteboard.general
    pasteboard.clearContents()
    try require(pasteboard.setData(data, forType: .html), "Could not offer comparison HTML")
    try require(pasteboard.setString(arguments[1], forType: .string), "Could not offer comparison plain text")
    try emit(["controlled_html_offer": true, "types": types()])
}

func offerVisibleControl(_ arguments: [String]) throws {
    let environment = ProcessInfo.processInfo.environment
    try require(environment["GITHUB_ACTIONS"] == "true" && environment["RUNNER_OS"] == "macOS",
                "Visible controls require an isolated hosted macOS Actions runner")
    try require(arguments.count == 2, "Visible control requires directory and label")
    let label = arguments[1]
    try require(["quote-visible-plain", "quote-visible-indented"].contains(label), "Unknown visible control")
    let text = "quote\ncontinued"
    let prefix = URL(fileURLWithPath: arguments[0], isDirectory: true).appendingPathComponent("macos-fixture-\(label)")
    let type: NSPasteboard.PasteboardType
    let data: Data
    var observation: [String: Any] = ["label": label, "source": "diagnostic-control", "plain_utf8_exact": true]
    if label == "quote-visible-plain" {
        type = .html
        data = Data("<!doctype html><html><head><meta charset=\"utf-8\"></head><body><div style=\"white-space:pre-wrap\"><p>quote</p><p>continued</p></div></body></html>".utf8)
    } else {
        type = .rtf
        let paragraph = NSMutableParagraphStyle()
        paragraph.headIndent = 36
        paragraph.firstLineHeadIndent = 36
        paragraph.paragraphSpacing = 12
        guard let font = NSFont(name: "Times-Roman", size: 12) else { throw ProbeError.failed("Positive control font unavailable") }
        let value = NSAttributedString(string: text, attributes: [.font: font, .paragraphStyle: paragraph])
        data = try value.data(from: NSRange(location: 0, length: value.length), documentAttributes: [.documentType: NSAttributedString.DocumentType.rtf])
        observation["source_attribute_runs"] = try featureAttributes(value)
    }
    let name = type == .html ? "html" : "rtf"
    try require(data.count <= 65536, "Visible control representation exceeds cap")
    let pasteboard = NSPasteboard.general
    pasteboard.clearContents()
    try require(pasteboard.setData(data, forType: type) && pasteboard.setData(Data(text.utf8), forType: .string), "Visible control publication failed")
    let generation = pasteboard.changeCount
    let offeredTypes = types()
    try require(try boundedData(type) == data && boundedData(.string) == Data(text.utf8)
                && boundedData(customType) == nil, "Visible control offer readback differs")
    try require(pasteboard.changeCount == generation, "Visible control changed during source capture")
    observation["change_count"] = generation
    observation["types"] = offeredTypes
    observation["representation_bytes"] = [name: data.count, "plain": text.utf8.count]
    try data.write(to: prefix.appendingPathExtension("source.\(name)"), options: .withoutOverwriting)
    try Data(text.utf8).write(to: prefix.appendingPathExtension("source.plain"), options: .withoutOverwriting)
    let manifest = try JSONSerialization.data(withJSONObject: observation, options: [.sortedKeys])
    try manifest.write(to: prefix.appendingPathExtension("source.json"), options: .withoutOverwriting)
    print(String(decoding: manifest, as: UTF8.self))
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
    let labels = ["native-offer", "textedit-before-paste", "textedit-after-paste", "textedit-after-copy", "feature-import", "failure",
                  "feature-colors", "feature-soft-break", "feature-numbering", "quote-native-reused", "quote-native-fresh",
                  "quote-bare", "quote-block-margin", "quote-paragraph-margin", "quote-visible-plain", "quote-visible-indented"]
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
            observation["rtf_attribute_runs"] = try featureAttributes(parsed)
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
    case "ax-retry-self-test": try controlRetrySelfTest()
    case "ax-press": try pressControl(Array(arguments.dropFirst()))
    case "prepare": try prepare(Array(arguments.dropFirst()))
    case "prepare-import": try prepareImport(Array(arguments.dropFirst()))
    case "native-fixture": try nativeFixture()
    case "native-html": try nativeHTML()
    case "capture-native-offer": try captureNativeOffer(Array(arguments.dropFirst()))
    case "textedit-features": try textEditFeatures(Array(arguments.dropFirst()))
    case "feature-self-test": try featureSelfTest()
    case "inspect-html": try inspectHTML(Array(arguments.dropFirst()))
    case "offer-html": try offerHTML(Array(arguments.dropFirst()))
    case "offer-visible-control": try offerVisibleControl(Array(arguments.dropFirst()))
    case "textedit-visible": try textEditVisible(Array(arguments.dropFirst()))
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
