import AppKit
import CoreGraphics
import CoreMedia
import Darwin
import Foundation
import ApplicationServices
import ScreenCaptureKit
import Security

// This process owns macOS privacy access and keeps its Apple Development
// code-signing identity while the surrounding debug app is rebuilt.
private let maximumCaptureWidth = 1440
private let maximumCaptureHeight = 1200
private let helperDisplayName =
    (Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
    ?? "Waku Computer Use"

struct Permissions: Codable {
    let screenRecording: Bool
    let accessibility: Bool
}

struct Target: Codable {
    let windowId: UInt32
    let bundleId: String
    let teamId: String?
    let appName: String
    let windowTitle: String
    let width: UInt32
    let height: UInt32
}

struct Action: Decodable {
    let type: String
    let x: Double?
    let y: Double?
    let toX: Double?
    let toY: Double?
    let deltaX: Double?
    let deltaY: Double?
    let text: String?
    let key: String?
    let modifiers: [String]?
    let mouseButton: String?
    let durationMs: UInt64?
}

struct Request: Decodable {
    let operation: String
    let target: Target?
    let actions: [Action]?
}

private final class VirtualCursorStore {
    static let shared = VirtualCursorStore()

    private var positions: [CGWindowID: CGPoint] = [:]
    private let lock = NSLock()

    func position(for windowID: CGWindowID, size: CGSize) -> CGPoint {
        lock.lock()
        defer { lock.unlock() }
        if let position = positions[windowID] {
            return position
        }
        let fallback = CGPoint(x: size.width * 0.5, y: size.height * 0.5)
        positions[windowID] = fallback
        return fallback
    }

    func move(for windowID: CGWindowID, to position: CGPoint, size: CGSize) {
        let clamped = CGPoint(
            x: min(max(position.x, 0), max(size.width, 1)),
            y: min(max(position.y, 0), max(size.height, 1))
        )
        lock.lock()
        positions[windowID] = clamped
        lock.unlock()
    }
}

private final class ComputerUseStatusItem {
    static let shared = ComputerUseStatusItem()

    private var item: NSStatusItem?
    private var apps: [TrackedApp] = []

    func track(bundleID: String, name: String) {
        DispatchQueue.main.async {
            self.createItemIfNeeded()
            if let index = self.apps.firstIndex(where: { $0.bundleID == bundleID }) {
                self.apps[index].name = name
            } else {
                self.apps.append(TrackedApp(bundleID: bundleID, name: name))
            }
            self.updateMenu()
        }
    }

    private func createItemIfNeeded() {
        guard item == nil else { return }
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.length = 80
        item.button?.toolTip = "Waku Computer Use"
        self.item = item
    }

    func stop() {
        let remove = {
            if let item = self.item {
                NSStatusBar.system.removeStatusItem(item)
                self.item = nil
            }
        }
        if Thread.isMainThread {
            remove()
        } else {
            DispatchQueue.main.sync(execute: remove)
        }
    }

    private func updateMenu() {
        guard let item else { return }
        guard !apps.isEmpty else {
            NSStatusBar.system.removeStatusItem(item)
            self.item = nil
            return
        }
        item.button?.image = self.statusImage()
        let menu = NSMenu()
        let title = NSMenuItem(title: "Waku Computer Use", action: nil, keyEquivalent: "")
        title.isEnabled = false
        menu.addItem(title)
        menu.addItem(.separator())
        for app in apps {
            let entry = NSMenuItem(title: app.name, action: nil, keyEquivalent: "")
            entry.isEnabled = false
            menu.addItem(entry)
        }
        item.menu = menu
    }

    private func statusImage() -> NSImage {
        let image = NSImage(size: NSSize(width: 80, height: 22))
        image.lockFocus()
        let bounds = NSRect(x: 0.5, y: 0.5, width: 79, height: 21)
        NSColor.white.withAlphaComponent(0.58).setFill()
        NSBezierPath(roundedRect: bounds, xRadius: 10.5, yRadius: 10.5).fill()

        let visibleApps = Array(apps.prefix(4))
        for (index, app) in visibleApps.enumerated() {
            let iconRect = NSRect(
                x: 12 + CGFloat(index) * 8,
                y: 1,
                width: 20,
                height: 20
            )
            NSGraphicsContext.current?.saveGraphicsState()
            let clip = NSBezierPath(roundedRect: iconRect, xRadius: 5, yRadius: 5)
            clip.addClip()
            if let icon = app.icon {
                icon.draw(in: iconRect, from: .zero, operation: .sourceOver, fraction: 1)
            } else {
                NSColor.windowBackgroundColor.setFill()
                iconRect.fill()
            }
            NSGraphicsContext.current?.restoreGraphicsState()
        }

        let cursor = NSBezierPath()
        cursor.move(to: NSPoint(x: 52, y: 20))
        cursor.line(to: NSPoint(x: 72, y: 16))
        cursor.line(to: NSPoint(x: 64, y: 14))
        cursor.line(to: NSPoint(x: 69, y: 4))
        cursor.line(to: NSPoint(x: 63, y: 1))
        cursor.line(to: NSPoint(x: 58, y: 12))
        cursor.line(to: NSPoint(x: 52, y: 7))
        cursor.close()
        NSColor.white.setFill()
        NSColor.gray.withAlphaComponent(0.55).setStroke()
        cursor.lineWidth = 1.3
        cursor.fill()
        cursor.stroke()
        image.unlockFocus()
        return image
    }
}

private struct TrackedApp {
    let bundleID: String
    var name: String
    var icon: NSImage? {
        guard let application = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).first,
              let bundleURL = application.bundleURL else {
            return nil
        }
        return NSWorkspace.shared.icon(forFile: bundleURL.path)
    }
}

private final class StatusAgentProcess {
    static let shared = StatusAgentProcess()

    private var process: Process?
    private var input: FileHandle?
    private let lock = NSLock()

    func start() {
        lock.lock()
        defer { lock.unlock() }
        guard process == nil else { return }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: CommandLine.arguments[0])
        process.arguments = ["status-agent"]
        let pipe = Pipe()
        process.standardInput = pipe
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            self.process = process
            self.input = pipe.fileHandleForWriting
        } catch {
            self.process = nil
            self.input = nil
        }
    }

    func track(bundleID: String, name: String) {
        start()
        lock.lock()
        defer { lock.unlock() }
        guard let input else { return }
        let message: [String: String] = ["bundleID": bundleID, "name": name]
        guard let data = try? JSONSerialization.data(withJSONObject: message) else { return }
        var payload = data
        payload.append(10)
        try? input.write(contentsOf: payload)
    }

    func stop() {
        lock.lock()
        let process = self.process
        let input = self.input
        self.process = nil
        self.input = nil
        lock.unlock()
        try? input?.close()
        if let process, process.isRunning {
            process.terminate()
        }
    }
}

struct Response: Encodable {
    let success: Bool
    let error: String?
    let permissions: Permissions?
    let targets: [Target]?
    let target: Target?
    let imageUrl: String?
    let summary: String?

    static func failure(_ error: Error) -> Response {
        Response(
            success: false,
            error: error.localizedDescription,
            permissions: nil,
            targets: nil,
            target: nil,
            imageUrl: nil,
            summary: nil
        )
    }
}

enum HelperError: LocalizedError {
    case invalidRequest(String)
    case ipc(String)
    case missingPermission(String)
    case unauthorizedClient(String)
    case targetUnavailable
    case targetIdentityChanged
    case targetBlocked
    case unsupportedAction(String)
    case eventCreationFailed
    case captureFailed

    var errorDescription: String? {
        switch self {
        case .invalidRequest(let message): message
        case .ipc(let message): "Computer Use connection failed: \(message)"
        case .missingPermission(let permission): "\(helperDisplayName) needs \(permission) access. Open Waku Settings > Computer Use to grant it."
        case .unauthorizedClient(let reason): "Computer Use rejected a request that did not come from a trusted Waku app: \(reason)"
        case .targetUnavailable: "The app has no available window. Retry get_app_state, or call list_apps() to confirm the app identifier."
        case .targetIdentityChanged: "The selected app window changed. Call get_app_state again before interacting."
        case .targetBlocked: "Waku does not allow computer control of that app."
        case .unsupportedAction(let action): "Unsupported computer-use action: \(action)"
        case .eventCreationFailed: "macOS could not create an input event."
        case .captureFailed: "macOS could not capture the selected app window."
        }
    }
}

@main
struct WakuComputerUse {
    static func main() async {
        if CommandLine.arguments.contains("status-agent") {
            serveStatusAgent()
            return
        }

        if CommandLine.arguments.contains("mcp") {
            do {
                if CommandLine.arguments.contains("mcp-child") {
                    let (channel, _) = try connectedChannel(at: socketPathArgument() ?? "")
                    defer { StatusAgentProcess.shared.stop() }
                    try await serveMCP(input: channel, output: channel)
                    await CaptureSessions.shared.stopAll()
                    channel.closeFile()
                } else {
                    try await serveMCPBridge()
                }
            } catch {
                FileHandle.standardError.write(Data("\(helperDisplayName): \(error.localizedDescription)\n".utf8))
                exit(1)
            }
            return
        }

        if !CommandLine.arguments.contains("request-child"),
           (CommandLine.arguments.contains("status") || CommandLine.arguments.contains("request-permissions")) {
            do {
                try await serveRequestBridge(input: .standardInput, output: .standardOutput)
            } catch {
                try? write(.failure(error), to: .standardOutput)
                exit(1)
            }
            return
        }

        if CommandLine.arguments.contains("request-child") {
            do {
                if CommandLine.arguments.contains("request-permissions") {
                    NSApplication.shared.setActivationPolicy(.accessory)
                    NSApplication.shared.activate(ignoringOtherApps: true)
                }
                let (channel, _) = try connectedChannel(at: socketPathArgument() ?? "")
                try await serveSession(input: channel, output: channel)
                channel.closeFile()
            } catch {
                FileHandle.standardError.write(Data("\(helperDisplayName): \(error.localizedDescription)\n".utf8))
                exit(1)
            }
            return
        }

        if let socketPath = socketPathArgument() {
            do {
                let (channel, peerPID) = try connectedChannel(at: socketPath)
                try await serveSession(
                    input: channel,
                    output: channel,
                    authorizationFailure: authorizationFailure(pid: peerPID)
                )
                await CaptureSessions.shared.stopAll()
                channel.closeFile()
            } catch {
                FileHandle.standardError.write(Data("\(helperDisplayName): \(error.localizedDescription)\n".utf8))
                exit(1)
            }
            return
        }

        do {
            try await serveOneShot(input: .standardInput, output: .standardOutput)
        } catch {
            FileHandle.standardError.write(Data("\(helperDisplayName): \(error.localizedDescription)\n".utf8))
            exit(1)
        }
    }

    static func serveOneShot(
        input: FileHandle,
        output: FileHandle,
        authorizationFailure: String? = nil
    ) async throws {
        let response: Response
        do {
            let data = input.readDataToEndOfFile()
            let request = try JSONDecoder().decode(Request.self, from: data)
            if let authorizationFailure {
                response = .failure(HelperError.unauthorizedClient(authorizationFailure))
            } else {
                response = try await handle(request)
            }
        } catch {
            response = .failure(error)
        }

        try write(response, to: output)
    }

    static func serveStatusAgent() {
        let run = {
            NSApplication.shared.setActivationPolicy(.accessory)
            DispatchQueue.global(qos: .utility).async {
                while let app = readLine(from: .standardInput) {
                    DispatchQueue.main.async {
                        guard let data = app.data(using: .utf8),
                              let message = try? JSONSerialization.jsonObject(with: data) as? [String: String],
                              let bundleID = message["bundleID"],
                              let name = message["name"] else {
                            return
                        }
                        ComputerUseStatusItem.shared.track(bundleID: bundleID, name: name)
                    }
                }
                DispatchQueue.main.async {
                    NSApplication.shared.terminate(nil)
                }
            }
            NSApplication.shared.run()
        }
        if Thread.isMainThread {
            run()
        } else {
            DispatchQueue.main.sync(execute: run)
        }
    }

    static func serveSession(
        input: FileHandle,
        output: FileHandle,
        authorizationFailure: String? = nil
    ) async throws {
        while let payload = try readFrame(from: input) {
            let response: Response
            do {
                let request = try JSONDecoder().decode(Request.self, from: payload)
                if let authorizationFailure {
                    response = .failure(HelperError.unauthorizedClient(authorizationFailure))
                } else {
                    response = try await handle(request)
                }
            } catch {
                response = .failure(error)
            }
            try writeFrame(response, to: output)
        }
    }

    static func handle(_ request: Request) async throws -> Response {
        switch request.operation {
        case "status":
            return Response(
                success: true,
                error: nil,
                permissions: currentPermissions(),
                targets: nil,
                target: nil,
                imageUrl: nil,
                summary: nil
            )
        case "requestPermissions":
            _ = CGRequestScreenCaptureAccess()
            // Exercise the same ScreenCaptureKit path used for window state.
            // On macOS this is what registers the app as a screen-content
            // capture client; Accessibility and Screen Recording are separate
            // TCC services and one does not register the other.
            _ = try? await availableContent()
            let prompt = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
            _ = AXIsProcessTrustedWithOptions(prompt)
            return Response(
                success: true,
                error: nil,
                permissions: currentPermissions(),
                targets: nil,
                target: nil,
                imageUrl: nil,
                summary: "macOS permission prompts requested."
            )
        case "listTargets":
            guard CGPreflightScreenCaptureAccess() else {
                throw HelperError.missingPermission("Screen Recording")
            }
            let content = try await availableContent()
            let windows = availableWindows(in: content)
            return Response(
                success: true,
                error: nil,
                permissions: currentPermissions(),
                targets: windows.map { target(for: $0, includeTitle: false) },
                target: nil,
                imageUrl: nil,
                summary: nil
            )
        case "use":
            guard let requested = request.target else {
                throw HelperError.invalidRequest("use requires a target")
            }
            let actions = request.actions ?? []
            guard actions.count <= 16 else {
                throw HelperError.invalidRequest("A computer-use call may contain at most 16 actions")
            }
            guard CGPreflightScreenCaptureAccess() else {
                throw HelperError.missingPermission("Screen Recording")
            }
            guard actions.isEmpty || AXIsProcessTrusted() else {
                throw HelperError.missingPermission("Accessibility")
            }
            let content = try await availableContent()
            guard let window = availableWindows(in: content).first(where: { $0.windowID == requested.windowId }) else {
                throw HelperError.targetUnavailable
            }
            let current = target(for: window)
            guard !isBlocked(bundleId: current.bundleId) else {
                throw HelperError.targetBlocked
            }
            guard current.bundleId == requested.bundleId,
                  requested.teamId == nil || current.teamId == requested.teamId else {
                throw HelperError.targetIdentityChanged
            }
            guard let display = captureDisplay(for: window, in: content.displays) else {
                throw HelperError.targetUnavailable
            }
            let filter = SCContentFilter(display: display, including: [window])
            let sourceRect = captureSourceRect(for: window, on: display)
            try await CaptureSessions.shared.start(
                for: window,
                filter: filter,
                sourceRect: sourceRect
            )
            if !actions.isEmpty {
                try perform(actions, in: window, coordinateSpace: requested)
            }
            StatusAgentProcess.shared.track(bundleID: current.bundleId, name: current.appName)
            let capture = try await capture(
                window,
                filter: filter,
                sourceRect: sourceRect,
                cursor: VirtualCursorStore.shared.position(
                    for: window.windowID,
                    size: CGSize(width: CGFloat(requested.width), height: CGFloat(requested.height))
                ),
                coordinateSpace: requested
            )
            return Response(
                success: true,
                error: nil,
                permissions: currentPermissions(),
                targets: nil,
                target: Target(
                    windowId: current.windowId,
                    bundleId: current.bundleId,
                    teamId: current.teamId,
                    appName: current.appName,
                    windowTitle: current.windowTitle,
                    width: UInt32(capture.width),
                    height: UInt32(capture.height)
                ),
                imageUrl: capture.dataUrl,
                summary: actions.isEmpty ? "Captured \(current.appName)." : "Completed \(actions.count) action\(actions.count == 1 ? "" : "s") in \(current.appName)."
            )
        default:
            throw HelperError.invalidRequest("Unknown operation: \(request.operation)")
        }
    }

    static func serveMCP(input: FileHandle, output: FileHandle) async throws {
        while let line = readLine(from: input) {
            guard let data = line.data(using: .utf8),
                  let message = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let method = message["method"] as? String else {
                continue
            }
            let id = message["id"]
            if method == "notifications/initialized" || method == "notifications/cancelled" {
                continue
            }

            do {
                let result: [String: Any]
                switch method {
                case "initialize":
                    result = [
                        "protocolVersion": "2025-06-18",
                        "capabilities": ["tools": ["listChanged": false]],
                        "serverInfo": ["name": "Waku Computer Use", "version": "1.0.0"],
                    ]
                case "tools/list":
                    result = ["tools": mcpTools()]
                case "tools/call":
                    guard let params = message["params"] as? [String: Any],
                          let name = params["name"] as? String else {
                        throw HelperError.invalidRequest("tools/call requires a tool name")
                    }
                    let arguments = params["arguments"] as? [String: Any] ?? [:]
                    result = try await callMCPTool(name: name, arguments: arguments)
                default:
                    throw HelperError.invalidRequest("Unsupported MCP method: \(method)")
                }
                if let id {
                    try writeMCP(["jsonrpc": "2.0", "id": id, "result": result], to: output)
                }
            } catch {
                if let id {
                    try writeMCP(
                        [
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": ["code": -32603, "message": error.localizedDescription],
                        ],
                        to: output
                    )
                }
            }
        }
    }

    static func serveMCPBridge() async throws {
        let listener = try UnixListener()
        defer { listener.close() }
        let launcher = try launchSelfThroughLaunchServices(
            arguments: ["mcp", "mcp-child", "--socket", listener.path]
        )
        defer {
            if launcher.isRunning { launcher.terminate() }
        }
        let channel = try listener.accept()
        defer { channel.closeFile() }

        while let line = readLine(from: .standardInput) {
            guard let payload = line.data(using: .utf8) else { continue }
            try channel.write(contentsOf: payload)
            try channel.write(contentsOf: Data([10]))
            guard let message = try? JSONSerialization.jsonObject(with: payload) as? [String: Any],
                  message["id"] != nil else {
                continue
            }
            guard let response = readLine(from: channel) else {
                throw HelperError.ipc("the Launch Services helper closed the MCP connection")
            }
            try writeLine(response, to: .standardOutput)
        }
    }

    static func serveRequestBridge(input: FileHandle, output: FileHandle) async throws {
        let listener = try UnixListener()
        defer { listener.close() }
        let childMode = CommandLine.arguments.contains("request-permissions")
        let arguments = [
            "request-child",
            childMode ? "request-permissions" : "status",
            "--socket",
            listener.path,
        ]
        let launcher = try launchSelfThroughLaunchServices(
            arguments: arguments,
            background: !childMode
        )
        defer {
            if launcher.isRunning { launcher.terminate() }
        }
        let channel = try listener.accept()
        defer { channel.closeFile() }

        let request = input.readDataToEndOfFile()
        try writeFrame(request, to: channel)
        guard let response = try readFrame(from: channel) else {
            throw HelperError.ipc("the Launch Services helper closed the permission connection")
        }
        try output.write(contentsOf: response)
    }

    private static func callMCPTool(name: String, arguments: [String: Any]) async throws -> [String: Any] {
        switch name {
        case "list_apps":
            let apps = listTargetableApps()
            return mcpResult(text: try jsonString(apps), structured: ["apps": apps])

        case "get_app_state":
            guard CGPreflightScreenCaptureAccess() else {
                throw HelperError.missingPermission("Screen Recording")
            }
            guard AXIsProcessTrusted() else {
                throw HelperError.missingPermission("Accessibility")
            }
            let app = try requiredString(arguments, "app")
            let resolved = try await resolveAppWindow(app)
            let state = try await captureAppState(
                resolved,
                disableDiff: (arguments["disableDiff"] as? Bool) ?? false
            )
            let structured: [String: Any] = [
                "app": app,
                "text": state.text,
                "screenshot": state.capture.dataUrl,
            ]
            return mcpResult(
                text: state.text,
                imageURL: state.capture.dataUrl,
                structured: structured
            )

        case "click", "drag", "press_key", "type_text":
            guard AXIsProcessTrusted() else {
                throw HelperError.missingPermission("Accessibility")
            }
            let app = try requiredString(arguments, "app")
            let resolved = try await resolveAppWindow(app)
            if name == "click", let rawIndex = arguments["element_index"] {
                let index = try elementIndex(rawIndex)
                guard AccessibilityRegistry.shared.belongs(
                    bundleID: resolved.target.bundleId,
                    processID: resolved.processID,
                    windowID: resolved.target.windowId
                ),
                      let element = AccessibilityRegistry.shared.element(for: index) else {
                    throw HelperError.invalidRequest("Element (index) is stale. Call get_app_state again.")
                }
                try performAccessibilityAction(element, name: kAXPressAction)
                RecentComputerActionStore.shared.record(resolved.target.bundleId)
                return mcpResult(text: "")
            }
            switch name {
            case "click":
                try performCoordinateClick(arguments, in: resolved)
            case "drag":
                try perform(
                    [Action(
                        type: "drag",
                        x: number(arguments, "from_x"),
                        y: number(arguments, "from_y"),
                        toX: number(arguments, "to_x"),
                        toY: number(arguments, "to_y"),
                        deltaX: nil,
                        deltaY: nil,
                        text: nil,
                        key: nil,
                        modifiers: nil,
                        mouseButton: nil,
                        durationMs: nil
                    )],
                    in: resolved.window,
                    coordinateSpace: resolved.target
                )
            case "press_key":
                let keyParts = try keyAndModifiers(try requiredString(arguments, "key"))
                try pressKey(keyParts.key, modifiers: keyParts.modifiers, processID: resolved.processID)
            default:
                try typeText(try requiredString(arguments, "text"), processID: resolved.processID)
            }
            RecentComputerActionStore.shared.record(resolved.target.bundleId)
            return mcpResult(text: "")

        case "perform_secondary_action", "set_value", "select_text", "scroll":
            guard AXIsProcessTrusted() else {
                throw HelperError.missingPermission("Accessibility")
            }
            let app = try requiredString(arguments, "app")
            let resolved = try await resolveAppWindow(app)
            let index = try elementIndex(arguments["element_index"] as Any)
            guard AccessibilityRegistry.shared.belongs(
                bundleID: resolved.target.bundleId,
                processID: resolved.processID,
                windowID: resolved.target.windowId
            ) else {
                throw HelperError.invalidRequest("Element (index) is stale. Call get_app_state again.")
            }
            guard let element = AccessibilityRegistry.shared.element(for: index) else {
                throw HelperError.invalidRequest("Element \(index) is stale. Call get_app_state again.")
            }
            switch name {
            case "perform_secondary_action":
                try performAccessibilityAction(
                    element,
                    matching: try requiredString(arguments, "action")
                )
            case "set_value":
                let value = try requiredString(arguments, "value")
                guard AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, value as CFTypeRef) == .success else {
                    throw HelperError.invalidRequest("Element \(index) does not accept a value")
                }
            case "select_text":
                try selectText(arguments, in: element)
            default:
                try scrollAccessibility(arguments, element: element, processID: resolved.processID)
            }
            RecentComputerActionStore.shared.record(resolved.target.bundleId)
            return mcpResult(text: "")

        default:
            throw HelperError.invalidRequest("Unknown MCP tool: \(name)")
        }
    }
}

private func socketPathArgument() -> String? {
    guard let index = CommandLine.arguments.firstIndex(of: "--socket"),
          CommandLine.arguments.indices.contains(index + 1) else {
        return nil
    }
    return CommandLine.arguments[index + 1]
}

private final class AccessibilityRegistry {
    static let shared = AccessibilityRegistry()

    private var elements: [String: AXUIElement] = [:]
    private var bundleID: String?
    private var processID: pid_t?
    private var windowID: CGWindowID?
    private let lock = NSLock()

    func reset(bundleID: String, processID: pid_t, windowID: CGWindowID) {
        lock.lock()
        elements.removeAll(keepingCapacity: true)
        self.bundleID = bundleID
        self.processID = processID
        self.windowID = windowID
        lock.unlock()
    }

    func add(_ element: AXUIElement) -> String {
        lock.lock()
        defer { lock.unlock() }
        let index = String(elements.count)
        elements[index] = element
        return index
    }

    func element(for index: String) -> AXUIElement? {
        lock.lock()
        defer { lock.unlock() }
        return elements[index]
    }

    func belongs(bundleID: String, processID: pid_t, windowID: CGWindowID) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return self.bundleID == bundleID
            && self.processID == processID
            && self.windowID == windowID
    }
}

private func mcpTools() -> [[String: Any]] {
    let app = ["type": "string", "description": "App name, full app path, or unambiguous bundle identifier"] as [String: Any]
    let element = ["type": "integer", "description": "Element index from get_app_state"] as [String: Any]
    let coordinate = ["type": "number"] as [String: Any]
    return [
        [
            "name": "list_apps",
            "description": "List the visible apps currently running on this Mac.",
            "inputSchema": ["type": "object", "additionalProperties": false, "properties": [:]],
            "annotations": ["readOnlyHint": true, "idempotentHint": true, "openWorldHint": false, "destructiveHint": false],
        ],
        [
            "name": "get_app_state",
            "description": "Get an app's current screenshot and accessibility tree. Call this before interacting with the app.",
            "inputSchema": ["type": "object", "additionalProperties": false, "required": ["app"], "properties": ["app": app, "disableDiff": ["type": "boolean"]]],
            "annotations": ["readOnlyHint": true, "idempotentHint": true, "openWorldHint": false, "destructiveHint": false],
        ],
        [
            "name": "click",
            "description": "Click an app at screenshot pixel coordinates.",
            "inputSchema": ["type": "object", "additionalProperties": false, "required": ["app"], "properties": ["app": app, "element_index": element, "x": coordinate, "y": coordinate, "click_count": ["type": "integer", "minimum": 1], "mouse_button": ["type": "string", "enum": ["left", "right", "middle", "l", "r", "m"]]]],
            "annotations": ["readOnlyHint": false, "idempotentHint": false, "openWorldHint": false, "destructiveHint": false],
        ],
        [
            "name": "drag",
            "description": "Drag between screenshot pixel coordinates.",
            "inputSchema": ["type": "object", "additionalProperties": false, "required": ["app", "from_x", "from_y", "to_x", "to_y"], "properties": ["app": app, "from_x": coordinate, "from_y": coordinate, "to_x": coordinate, "to_y": coordinate]],
            "annotations": ["readOnlyHint": false, "idempotentHint": false, "openWorldHint": false, "destructiveHint": false],
        ],
        [
            "name": "press_key",
            "description": "Press a key or key combination in the app.",
            "inputSchema": ["type": "object", "additionalProperties": false, "required": ["app", "key"], "properties": ["app": app, "key": ["type": "string"]]],
            "annotations": ["readOnlyHint": false, "idempotentHint": false, "openWorldHint": false, "destructiveHint": false],
        ],
        [
            "name": "type_text",
            "description": "Type literal text into the app.",
            "inputSchema": ["type": "object", "additionalProperties": false, "required": ["app", "text"], "properties": ["app": app, "text": ["type": "string"]]],
            "annotations": ["readOnlyHint": false, "idempotentHint": false, "openWorldHint": false, "destructiveHint": false],
        ],
        [
            "name": "perform_secondary_action",
            "description": "Invoke a secondary accessibility action exposed by an element.",
            "inputSchema": ["type": "object", "additionalProperties": false, "required": ["app", "element_index", "action"], "properties": ["app": app, "element_index": element, "action": ["type": "string"]]],
            "annotations": ["readOnlyHint": false, "idempotentHint": false, "openWorldHint": false, "destructiveHint": false],
        ],
        [
            "name": "set_value",
            "description": "Set the value of a settable accessibility element.",
            "inputSchema": ["type": "object", "additionalProperties": false, "required": ["app", "element_index", "value"], "properties": ["app": app, "element_index": element, "value": ["type": "string"]]],
            "annotations": ["readOnlyHint": false, "idempotentHint": false, "openWorldHint": false, "destructiveHint": false],
        ],
        [
            "name": "select_text",
            "description": "Select matching text in an accessibility element.",
            "inputSchema": ["type": "object", "additionalProperties": false, "required": ["app", "element_index", "text"], "properties": ["app": app, "element_index": element, "text": ["type": "string"], "prefix": ["type": "string"], "suffix": ["type": "string"], "selection_type": ["type": "string", "enum": ["text", "cursor_before", "cursor_after"]]]],
            "annotations": ["readOnlyHint": false, "idempotentHint": false, "openWorldHint": false, "destructiveHint": false],
        ],
        [
            "name": "scroll",
            "description": "Scroll an accessibility element in a direction.",
            "inputSchema": ["type": "object", "additionalProperties": false, "required": ["app", "element_index", "direction"], "properties": ["app": app, "element_index": element, "direction": ["type": "string", "enum": ["up", "down", "left", "right", "u", "d", "l", "r"]], "pages": ["type": "number", "exclusiveMinimum": 0]]],
            "annotations": ["readOnlyHint": false, "idempotentHint": false, "openWorldHint": false, "destructiveHint": false],
        ],
    ]
}

private func readLine(from input: FileHandle) -> String? {
    var data = Data()
    while true {
        guard let byte = try? input.read(upToCount: 1), !byte.isEmpty else {
            return data.isEmpty ? nil : String(data: data, encoding: .utf8)
        }
        if byte[0] == 10 {
            return String(data: data, encoding: .utf8)
        }
        data.append(byte[0])
    }
}

private func writeLine(_ line: String, to output: FileHandle) throws {
    try output.write(contentsOf: Data(line.utf8))
    try output.write(contentsOf: Data([10]))
}

private func writeMCP(_ object: [String: Any], to output: FileHandle) throws {
    let data = try JSONSerialization.data(withJSONObject: object, options: [.withoutEscapingSlashes])
    try output.write(contentsOf: data)
    try output.write(contentsOf: Data([10]))
}

private func mcpResult(text: String, imageURL: String? = nil, structured: [String: Any]? = nil) -> [String: Any] {
    var content: [[String: Any]] = [["type": "text", "text": text]]
    if let imageURL,
       let comma = imageURL.firstIndex(of: ","),
       let data = Data(base64Encoded: String(imageURL[imageURL.index(after: comma)...])) {
        content.append(["type": "image", "data": data.base64EncodedString(), "mimeType": "image/png"])
    }
    var result: [String: Any] = ["content": content, "isError": false]
    if let structured { result["structuredContent"] = structured }
    return result
}

private func requiredString(_ arguments: [String: Any], _ name: String) throws -> String {
    guard let value = arguments[name] as? String,
          !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
        throw HelperError.invalidRequest("\(name) is required")
    }
    return value
}

private func elementIndex(_ value: Any) throws -> String {
    if let value = value as? String, !value.isEmpty { return value }
    if let value = value as? NSNumber { return value.stringValue }
    throw HelperError.invalidRequest("Missing element_index")
}

private func number(_ arguments: [String: Any], _ name: String) -> Double? {
    if let value = arguments[name] as? Double { return value }
    if let value = arguments[name] as? NSNumber { return value.doubleValue }
    return nil
}

private func keyAndModifiers(_ value: String) throws -> (key: String, modifiers: [String]) {
    let parts = value.split(separator: "+", omittingEmptySubsequences: false)
        .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
    guard let key = parts.last, !key.isEmpty else {
        throw HelperError.invalidRequest("Key is empty")
    }
    let modifiers = parts.dropLast().map { part -> String in
        switch part.lowercased() {
        case "cmd", "command", "command_l", "command_r", "super", "super_l", "super_r", "meta":
            return "command"
        case "ctrl", "control", "control_l", "control_r":
            return "control"
        case "alt", "alt_l", "alt_r", "option", "option_l", "option_r":
            return "option"
        case "shift", "shift_l", "shift_r":
            return "shift"
        default: return part
        }
    }
    return (key, modifiers)
}

private func jsonString(_ value: Any) throws -> String {
    let data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    return String(decoding: data, as: UTF8.self)
}

private func jsonObject<T: Encodable>(_ value: T) throws -> [String: Any] {
    let data = try JSONEncoder().encode(value)
    guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw HelperError.invalidRequest("Could not encode response")
    }
    return object
}

private func write(_ response: Response, to output: FileHandle) throws {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.withoutEscapingSlashes]
    try output.write(contentsOf: encoder.encode(response))
}

private func readFrame(from input: FileHandle) throws -> Data? {
    guard let header = try readExactly(4, from: input) else {
        return nil
    }
    let bytes = [UInt8](header)
    let length =
        Int(bytes[0]) << 24
        | Int(bytes[1]) << 16
        | Int(bytes[2]) << 8
        | Int(bytes[3])
    guard length <= 24 * 1024 * 1024 else {
        throw HelperError.ipc("request is too large")
    }
    return try readExactly(length, from: input)
}

private func readExactly(_ count: Int, from input: FileHandle) throws -> Data? {
    var data = Data()
    while data.count < count {
        let chunk = try input.read(upToCount: count - data.count) ?? Data()
        if chunk.isEmpty {
            if data.isEmpty {
                return nil
            }
            throw HelperError.ipc("the Waku connection closed mid-message")
        }
        data.append(chunk)
    }
    return data
}

private func writeFrame(_ response: Response, to output: FileHandle) throws {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.withoutEscapingSlashes]
    let payload = try encoder.encode(response)
    try writeFrame(payload, to: output)
}

private func writeFrame(_ payload: Data, to output: FileHandle) throws {
    guard let length = UInt32(exactly: payload.count) else {
        throw HelperError.ipc("response is too large")
    }
    var bigEndianLength = length.bigEndian
    try withUnsafeBytes(of: &bigEndianLength) { header in
        try output.write(contentsOf: header)
    }
    try output.write(contentsOf: payload)
}

private final class UnixListener {
    let path: String
    private var descriptor: Int32

    init() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("waku-computer-use", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        path = directory.appendingPathComponent(UUID().uuidString).path
        descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else {
            throw HelperError.ipc(String(cString: strerror(errno)))
        }

        do {
            var address = sockaddr_un()
            let pathBytes = Array(path.utf8CString)
            guard pathBytes.count <= MemoryLayout.size(ofValue: address.sun_path) else {
                throw HelperError.ipc("socket path is too long")
            }
            address.sun_family = sa_family_t(AF_UNIX)
            address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
            path.withCString { source in
                withUnsafeMutablePointer(to: &address.sun_path) { pointer in
                    pointer.withMemoryRebound(to: CChar.self, capacity: pathBytes.count) { destination in
                        _ = strlcpy(destination, source, pathBytes.count)
                    }
                }
            }
            Darwin.unlink(path)
            let bound = withUnsafePointer(to: &address) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { socketAddress in
                    Darwin.bind(descriptor, socketAddress, socklen_t(MemoryLayout<sockaddr_un>.size))
                }
            }
            guard bound == 0 else {
                throw HelperError.ipc(String(cString: strerror(errno)))
            }
            guard Darwin.listen(descriptor, 1) == 0 else {
                throw HelperError.ipc(String(cString: strerror(errno)))
            }
        } catch {
            close()
            throw error
        }
    }

    func accept() throws -> FileHandle {
        let connection = Darwin.accept(descriptor, nil, nil)
        guard connection >= 0 else {
            throw HelperError.ipc(String(cString: strerror(errno)))
        }
        return FileHandle(fileDescriptor: connection, closeOnDealloc: true)
    }

    func close() {
        if descriptor >= 0 {
            Darwin.close(descriptor)
            descriptor = -1
        }
        Darwin.unlink(path)
    }
}

private func launchSelfThroughLaunchServices(arguments: [String], background: Bool = true) throws -> Process {
    let launcher = Process()
    launcher.executableURL = URL(fileURLWithPath: "/usr/bin/open")
    launcher.arguments = ["-n", "-W"] + (background ? ["-g"] : []) + [Bundle.main.bundleURL.path, "--args"] + arguments
    launcher.standardInput = FileHandle.nullDevice
    launcher.standardOutput = FileHandle.nullDevice
    launcher.standardError = FileHandle.nullDevice
    try launcher.run()
    return launcher
}

private func connectedChannel(at path: String) throws -> (FileHandle, pid_t) {
    let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
    guard descriptor >= 0 else {
        throw HelperError.ipc(String(cString: strerror(errno)))
    }

    do {
        var address = sockaddr_un()
        let pathBytes = Array(path.utf8CString)
        guard pathBytes.count <= MemoryLayout.size(ofValue: address.sun_path) else {
            throw HelperError.ipc("socket path is too long")
        }
        address.sun_family = sa_family_t(AF_UNIX)
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        path.withCString { source in
            withUnsafeMutablePointer(to: &address.sun_path) { pointer in
                pointer.withMemoryRebound(to: CChar.self, capacity: pathBytes.count) { destination in
                    _ = strlcpy(destination, source, pathBytes.count)
                }
            }
        }
        let result = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { socketAddress in
                Darwin.connect(descriptor, socketAddress, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard result == 0 else {
            throw HelperError.ipc(String(cString: strerror(errno)))
        }

        var peerPID: pid_t = 0
        var peerPIDSize = socklen_t(MemoryLayout.size(ofValue: peerPID))
        guard getsockopt(descriptor, SOL_LOCAL, LOCAL_PEERPID, &peerPID, &peerPIDSize) == 0 else {
            throw HelperError.ipc("could not identify the Waku process")
        }
        return (FileHandle(fileDescriptor: descriptor, closeOnDealloc: true), peerPID)
    } catch {
        Darwin.close(descriptor)
        throw error
    }
}

private func authorizationFailure(pid: pid_t) -> String? {
    guard let information = signingInformation(pid: pid) else {
        return "the peer has no valid code signature"
    }
    guard let identifier = information[kSecCodeInfoIdentifier as String] as? String else {
        return "the peer has no signing identifier"
    }
    guard let helperIdentifier = Bundle.main.bundleIdentifier,
          helperIdentifier.hasSuffix(".computer-use") else {
        return "the helper bundle identifier is invalid"
    }
    let expectedIdentifier = String(helperIdentifier.dropLast(".computer-use".count))
    guard identifier == expectedIdentifier else {
        return "expected \(expectedIdentifier), got \(identifier)"
    }
    if let helperTeam = signingTeamId(pid: getpid()) {
        let peerTeam = information[kSecCodeInfoTeamIdentifier as String] as? String
        guard peerTeam == helperTeam else {
            return "expected signing team \(helperTeam), got \(peerTeam ?? "none")"
        }
    }
    return nil
}

private func currentPermissions() -> Permissions {
    Permissions(
        screenRecording: CGPreflightScreenCaptureAccess(),
        accessibility: AXIsProcessTrusted()
    )
}

private func availableContent() async throws -> SCShareableContent {
    try await SCShareableContent.excludingDesktopWindows(
        true,
        onScreenWindowsOnly: false
    )
}

private func availableWindows(in content: SCShareableContent) -> [SCWindow] {
    return content.windows
        .filter { window in
            guard let app = window.owningApplication,
                  !app.bundleIdentifier.isEmpty,
                  window.frame.width >= 80,
                  window.frame.height >= 60,
                  !isBlocked(bundleId: app.bundleIdentifier) else {
                return false
            }
            return true
        }
        .sorted { lhs, rhs in
            let leftApp = lhs.owningApplication?.applicationName ?? ""
            let rightApp = rhs.owningApplication?.applicationName ?? ""
            if leftApp == rightApp {
                return (lhs.title ?? "") < (rhs.title ?? "")
            }
            return leftApp < rightApp
        }
}

private struct ResolvedAppWindow {
    let content: SCShareableContent
    let window: SCWindow
    let target: Target
    let processID: pid_t
}

private final class AccessibilityDiffStore {
    static let shared = AccessibilityDiffStore()

    private var trees: [String: String] = [:]
    private let lock = NSLock()

    func render(_ tree: String, for app: String, disableDiff: Bool) -> String {
        lock.lock()
        let previous = trees.updateValue(tree, forKey: app)
        lock.unlock()
        guard !disableDiff, let previous else {
            return tree
        }
        guard previous != tree else {
            return "<accessibility_diff>\nNo accessibility changes.\n</accessibility_diff>"
        }

        let oldLines = indexedAccessibilityLines(previous)
        let newLines = indexedAccessibilityLines(tree)
        let indexes = Set(oldLines.keys).union(newLines.keys).sorted()
        var changes: [String] = []
        for index in indexes {
            switch (oldLines[index], newLines[index]) {
            case let (old?, new?) where old != new:
                changes.append("- \(old)")
                changes.append("+ \(new)")
            case let (old?, nil):
                changes.append("- \(old)")
            case let (nil, new?):
                changes.append("+ \(new)")
            default:
                break
            }
        }
        return "<accessibility_diff>\n\(changes.joined(separator: "\n"))\n</accessibility_diff>"
    }
}

private final class RecentComputerActionStore {
    static let shared = RecentComputerActionStore()

    private var dates: [String: Date] = [:]
    private let lock = NSLock()

    func record(_ bundleID: String) {
        lock.lock()
        dates[bundleID] = Date()
        lock.unlock()
    }

    func remainingDelay(for bundleID: String) -> TimeInterval {
        lock.lock()
        let date = dates[bundleID]
        lock.unlock()
        guard let date else { return 0 }
        return max(0, 1 - Date().timeIntervalSince(date))
    }
}

private func indexedAccessibilityLines(_ tree: String) -> [Int: String] {
    var result: [Int: String] = [:]
    for line in tree.split(separator: "\n", omittingEmptySubsequences: false) {
        let value = String(line)
        guard let open = value.firstIndex(of: "["),
              let close = value[open...].firstIndex(of: "]"),
              let index = Int(value[value.index(after: open)..<close]) else {
            continue
        }
        result[index] = value
    }
    return result
}

private func normalizedAppIdentifier(_ identifier: String) throws -> String {
    let normalized = identifier.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    guard !normalized.isEmpty else {
        throw HelperError.invalidRequest("app is required")
    }
    return normalized
}

private func matchingWindows(
    for identifier: String,
    in content: SCShareableContent
) throws -> [SCWindow] {
    let normalized = try normalizedAppIdentifier(identifier)
    return availableWindows(in: content).filter { window in
        guard let app = window.owningApplication else { return false }
        let running = NSRunningApplication(processIdentifier: app.processID)
        let path = running?.bundleURL?.path.lowercased()
        let processName = running?.executableURL?.deletingPathExtension().lastPathComponent.lowercased()
        return app.applicationName.lowercased() == normalized
            || app.bundleIdentifier.lowercased() == normalized
            || path == normalized
            || processName == normalized
    }
}

private func chooseWindow(_ windows: [SCWindow], identifier: String) throws -> SCWindow? {
    guard !windows.isEmpty else { return nil }
    let normalized = try normalizedAppIdentifier(identifier)
    let exactIdentities = Set(windows.compactMap { window -> String? in
        guard let app = window.owningApplication else { return nil }
        let running = NSRunningApplication(processIdentifier: app.processID)
        let path = running?.bundleURL?.path.lowercased()
        if app.bundleIdentifier.lowercased() == normalized || path == normalized {
            return app.bundleIdentifier
        }
        return nil
    })
    let bundleIDs = Set(windows.compactMap { $0.owningApplication?.bundleIdentifier })
    if exactIdentities.isEmpty, bundleIDs.count > 1 {
        let choices = bundleIDs.sorted().joined(separator: ", ")
        throw HelperError.invalidRequest(
            "App '\(identifier)' is ambiguous. Retry with a bundle identifier from list_apps(): \(choices)"
        )
    }

    let frontmostPID = NSWorkspace.shared.frontmostApplication?.processIdentifier
    return windows.max { lhs, rhs in
        func rank(_ window: SCWindow) -> (Int, Int, CGFloat) {
            let frontmost = window.owningApplication?.processID == frontmostPID ? 1 : 0
            let onScreen = window.isOnScreen ? 1 : 0
            return (frontmost, onScreen, window.frame.width * window.frame.height)
        }
        return rank(lhs) < rank(rhs)
    }
}

private func applicationURL(for identifier: String) -> URL? {
    let trimmed = identifier.trimmingCharacters(in: .whitespacesAndNewlines)
    if trimmed.contains("/"), FileManager.default.fileExists(atPath: trimmed) {
        return URL(fileURLWithPath: trimmed)
    }
    if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: trimmed) {
        return url
    }
    let normalized = trimmed.lowercased()
    return installedApplicationURLs().first { url in
        let bundle = Bundle(url: url)
        let displayName = (bundle?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
            ?? (bundle?.object(forInfoDictionaryKey: "CFBundleName") as? String)
            ?? url.deletingPathExtension().lastPathComponent
        return displayName.lowercased() == normalized
            || url.deletingPathExtension().lastPathComponent.lowercased() == normalized
    }
}

private func installedApplicationURLs() -> [URL] {
    let roots = [
        URL(fileURLWithPath: "/Applications", isDirectory: true),
        URL(fileURLWithPath: "/System/Applications", isDirectory: true),
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Applications", isDirectory: true),
    ]
    var urls = Set<URL>()
    for root in roots where FileManager.default.fileExists(atPath: root.path) {
        guard let enumerator = FileManager.default.enumerator(
            at: root,
            includingPropertiesForKeys: [.isApplicationKey],
            options: [.skipsHiddenFiles, .skipsPackageDescendants]
        ) else { continue }
        for case let url as URL in enumerator where url.pathExtension.lowercased() == "app" {
            urls.insert(url.resolvingSymlinksInPath())
            enumerator.skipDescendants()
        }
    }
    for app in NSWorkspace.shared.runningApplications {
        if let url = app.bundleURL {
            urls.insert(url.resolvingSymlinksInPath())
        }
    }
    return urls.sorted { $0.path.localizedCaseInsensitiveCompare($1.path) == .orderedAscending }
}

private func listTargetableApps() -> [[String: Any]] {
    let running = NSWorkspace.shared.runningApplications.filter { !$0.isTerminated }
    let runningBundleIDs = Set(running.compactMap(\.bundleIdentifier))
    var apps: [String: (name: String, running: Bool)] = [:]

    for url in installedApplicationURLs() {
        guard let bundle = Bundle(url: url),
              let bundleID = bundle.bundleIdentifier,
              !isBlocked(bundleId: bundleID) else {
            continue
        }
        let name = (bundle.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
            ?? (bundle.object(forInfoDictionaryKey: "CFBundleName") as? String)
            ?? url.deletingPathExtension().lastPathComponent
        apps[bundleID] = (name, runningBundleIDs.contains(bundleID))
    }
    for app in running {
        guard let bundleID = app.bundleIdentifier,
              !isBlocked(bundleId: bundleID),
              app.activationPolicy != .prohibited else {
            continue
        }
        apps[bundleID] = (app.localizedName ?? apps[bundleID]?.name ?? bundleID, true)
    }

    return apps.map { bundleID, app in
        [
            "id": bundleID,
            "displayName": app.name,
            "isRunning": app.running,
        ]
    }.sorted {
        ($0["displayName"] as? String ?? "").localizedCaseInsensitiveCompare(
            $1["displayName"] as? String ?? ""
        ) == .orderedAscending
    }
}

private func launchApplication(for identifier: String) async throws {
    guard let url = applicationURL(for: identifier),
          let bundleID = Bundle(url: url)?.bundleIdentifier,
          !isBlocked(bundleId: bundleID) else {
        throw HelperError.invalidRequest(
            "App '\(identifier)' was not found. Call list_apps() to inspect targetable apps."
        )
    }
    let configuration = NSWorkspace.OpenConfiguration()
    configuration.activates = false
    try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
        NSWorkspace.shared.openApplication(at: url, configuration: configuration) { _, error in
            if let error {
                continuation.resume(throwing: error)
            } else {
                continuation.resume(returning: ())
            }
        }
    }
}

private func resolveAppWindow(_ identifier: String, launchIfNeeded: Bool = true) async throws -> ResolvedAppWindow {
    func resolve() async throws -> ResolvedAppWindow? {
        let content = try await availableContent()
        let windows = try matchingWindows(for: identifier, in: content)
        guard let window = try chooseWindow(windows, identifier: identifier),
              let processID = window.owningApplication?.processID else {
            return nil
        }
        return ResolvedAppWindow(
            content: content,
            window: window,
            target: target(for: window),
            processID: processID
        )
    }

    if let resolved = try await resolve() {
        return resolved
    }
    guard launchIfNeeded else {
        throw HelperError.invalidRequest(
            "App '\(identifier)' has no targetable window. Call list_apps() to confirm the app identifier."
        )
    }
    try await launchApplication(for: identifier)
    for _ in 0..<50 {
        if let resolved = try await resolve() {
            return resolved
        }
        try await Task.sleep(nanoseconds: 100_000_000)
    }
    throw HelperError.invalidRequest(
        "App '\(identifier)' launched but did not open a targetable window."
    )
}

private func pointInWindow(
    x: Double?,
    y: Double?,
    resolved: ResolvedAppWindow
) throws -> CGPoint {
    guard let x, let y, x.isFinite, y.isFinite else {
        throw HelperError.invalidRequest("Pointer actions require finite x and y coordinates")
    }
    let width = Double(max(resolved.target.width, 1))
    let height = Double(max(resolved.target.height, 1))
    let localX = min(max(x, 0), width)
    let localY = min(max(y, 0), height)
    return CGPoint(
        x: resolved.window.frame.minX + localX / width * resolved.window.frame.width,
        y: resolved.window.frame.minY + localY / height * resolved.window.frame.height
    )
}

private func performCoordinateClick(
    _ arguments: [String: Any],
    in resolved: ResolvedAppWindow
) throws {
    let rawCount = number(arguments, "click_count") ?? 1
    guard rawCount.isFinite,
          rawCount.rounded() == rawCount,
          rawCount >= 1,
          rawCount <= 10 else {
        throw HelperError.invalidRequest("click_count must be an integer from 1 through 10")
    }
    let x = number(arguments, "x")
    let y = number(arguments, "y")
    updateVirtualCursor(x, y, window: resolved.window, coordinateSpace: resolved.target)
    try click(
        at: pointInWindow(x: x, y: y, resolved: resolved),
        count: Int64(rawCount),
        button: mouseButton(arguments["mouse_button"] as? String),
        processID: resolved.processID
    )
}

private func targetForApp(_ identifier: String) async throws -> Target {
    let content = try await availableContent()
    let normalized = identifier.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    let windows = availableWindows(in: content)
    let matching = windows.filter { window in
        guard let app = window.owningApplication else { return false }
        let name = app.applicationName.lowercased()
        let bundle = app.bundleIdentifier.lowercased()
        let path = NSRunningApplication(processIdentifier: app.processID)?.bundleURL?.path.lowercased()
        return name == normalized || bundle == normalized || path == normalized
    }
    if let frontmost = NSWorkspace.shared.frontmostApplication,
       let window = matching.first(where: { $0.owningApplication?.processID == frontmost.processIdentifier }) {
        return target(for: window)
    }
    guard let window = matching.first else {
        throw HelperError.targetUnavailable
    }
    return target(for: window)
}

private func processID(for target: Target) async throws -> pid_t {
    let content = try await availableContent()
    guard let window = availableWindows(in: content).first(where: { $0.windowID == target.windowId }),
          let processID = window.owningApplication?.processID else {
        throw HelperError.targetUnavailable
    }
    return processID
}

private func accessibilityRoot(for processID: pid_t) -> AXUIElement {
    AXUIElementCreateApplication(processID)
}

private func accessibilityAttribute(_ element: AXUIElement, _ name: String) -> AnyObject? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else {
        return nil
    }
    return value
}

private func accessibilityString(_ element: AXUIElement, _ name: String) -> String? {
    if let value = accessibilityAttribute(element, name) as? String {
        return value
    }
    if let value = accessibilityAttribute(element, name) as? NSNumber {
        return value.stringValue
    }
    return nil
}

private func accessibilityChildren(_ element: AXUIElement) -> [AXUIElement] {
    let attributes = [
        kAXChildrenAttribute,
        "AXChildrenInNavigationOrder",
        kAXVisibleChildrenAttribute,
        kAXContentsAttribute,
    ]
    var seen = Set<CFHashCode>()
    var children: [AXUIElement] = []
    for attribute in attributes {
        for child in (accessibilityAttribute(element, attribute) as? [AXUIElement]) ?? [] {
            if seen.insert(CFHash(child)).inserted {
                children.append(child)
            }
        }
    }
    return children
}

private func enableEnhancedAccessibility(_ application: AXUIElement) {
    _ = AXUIElementSetAttributeValue(
        application,
        "AXEnhancedUserInterface" as CFString,
        kCFBooleanTrue
    )
}

private func accessibilityFrameCenter(_ element: AXUIElement) -> CGPoint? {
    guard let positionAttribute = accessibilityAttribute(element, kAXPositionAttribute),
          let sizeAttribute = accessibilityAttribute(element, kAXSizeAttribute),
          CFGetTypeID(positionAttribute) == AXValueGetTypeID(),
          CFGetTypeID(sizeAttribute) == AXValueGetTypeID() else {
        return nil
    }
    let positionValue = unsafeBitCast(positionAttribute, to: AXValue.self)
    let sizeValue = unsafeBitCast(sizeAttribute, to: AXValue.self)
    var position = CGPoint.zero
    var size = CGSize.zero
    guard AXValueGetValue(positionValue, .cgPoint, &position),
          AXValueGetValue(sizeValue, .cgSize, &size) else {
        return nil
    }
    return CGPoint(x: position.x + size.width / 2, y: position.y + size.height / 2)
}

private func accessibilityText(for target: Target) async throws -> String {
    let content = try await availableContent()
    guard let window = availableWindows(in: content).first(where: { $0.windowID == target.windowId }),
          let processID = window.owningApplication?.processID else {
        throw HelperError.targetUnavailable
    }
    AccessibilityRegistry.shared.reset(
        bundleID: target.bundleId,
        processID: processID,
        windowID: target.windowId
    )
    let root = accessibilityRoot(for: processID)
    enableEnhancedAccessibility(root)
    let windows = accessibilityChildren(root)
    let axWindow = windows.first ?? root
    var lines: [String] = []
    appendAccessibilityTree(axWindow, depth: 0, lines: &lines)
    return lines.joined(separator: "\n")
}

private func accessibilityText(for resolved: ResolvedAppWindow) -> String {
    AccessibilityRegistry.shared.reset(
        bundleID: resolved.target.bundleId,
        processID: resolved.processID,
        windowID: resolved.target.windowId
    )
    let root = accessibilityRoot(for: resolved.processID)
    enableEnhancedAccessibility(root)
    let windows = accessibilityChildren(root)
    let axWindow = windows.first { element in
        (accessibilityAttribute(element, "AXWindowNumber") as? NSNumber)?.uint32Value
            == resolved.target.windowId
    } ?? windows.first ?? root
    var lines: [String] = []
    appendAccessibilityTree(axWindow, depth: 0, lines: &lines)
    return lines.joined(separator: "\n")
}

private func captureAppState(
    _ initialResolved: ResolvedAppWindow,
    disableDiff: Bool
) async throws -> (text: String, capture: Capture) {
    var resolved = initialResolved
    let remainingDelay = RecentComputerActionStore.shared.remainingDelay(for: resolved.target.bundleId)
    if remainingDelay > 0 {
        try await Task.sleep(nanoseconds: UInt64(remainingDelay * 1_000_000_000))
        resolved = try await resolveAppWindow(resolved.target.bundleId, launchIfNeeded: false)
    }
    guard let display = captureDisplay(for: resolved.window, in: resolved.content.displays) else {
        throw HelperError.captureFailed
    }
    let filter = SCContentFilter(display: display, including: [resolved.window])
    let sourceRect = captureSourceRect(for: resolved.window, on: display)
    try await CaptureSessions.shared.start(
        for: resolved.window,
        filter: filter,
        sourceRect: sourceRect
    )
    StatusAgentProcess.shared.track(
        bundleID: resolved.target.bundleId,
        name: resolved.target.appName
    )
    let cursor = VirtualCursorStore.shared.position(
        for: resolved.window.windowID,
        size: CGSize(
            width: CGFloat(resolved.target.width),
            height: CGFloat(resolved.target.height)
        )
    )
    let capture = try await capture(
        resolved.window,
        filter: filter,
        sourceRect: sourceRect,
        cursor: cursor,
        coordinateSpace: resolved.target
    )
    let fullTree = accessibilityText(for: resolved)
    let text = AccessibilityDiffStore.shared.render(
        fullTree,
        for: resolved.target.bundleId,
        disableDiff: disableDiff
    )
    return (text, capture)
}

private func appendAccessibilityTree(
    _ element: AXUIElement,
    depth: Int,
    lines: inout [String]
) {
    guard depth <= 8, lines.count < 500 else { return }
    let index = AccessibilityRegistry.shared.add(element)
    let role = accessibilityString(element, kAXRoleAttribute) ?? "AXUIElement"
    let title = accessibilityString(element, kAXTitleAttribute)
        ?? accessibilityString(element, kAXDescriptionAttribute)
    let value = accessibilityString(element, kAXValueAttribute)
    let enabled = (accessibilityAttribute(element, kAXEnabledAttribute) as? NSNumber)?.boolValue
    let actions = (try? accessibilityActions(element)) ?? []
    var detail = "[\(index)] \(role)"
    if let title, !title.isEmpty { detail += " \"\(title)\"" }
    if let value, !value.isEmpty, value != title { detail += " value=\"\(value)\"" }
    if enabled == false { detail += " disabled" }
    if !actions.isEmpty { detail += " actions=\(actions.joined(separator: ","))" }
    lines.append(String(repeating: "  ", count: depth) + detail)
    for child in accessibilityChildren(element) {
        appendAccessibilityTree(child, depth: depth + 1, lines: &lines)
    }
}

private func accessibilityActions(_ element: AXUIElement) throws -> [String] {
    var names: CFArray?
    guard AXUIElementCopyActionNames(element, &names) == .success,
          let names = names as? [String] else {
        return []
    }
    return names
}

private func performAccessibilityAction(_ element: AXUIElement, name: String) throws {
    guard AXUIElementPerformAction(element, name as CFString) == .success else {
        throw HelperError.invalidRequest("The element does not expose the \(name) action")
    }
}

private func performAccessibilityAction(_ element: AXUIElement, matching requested: String) throws {
    let actions = try accessibilityActions(element)
    let normalized = normalizeAccessibilityAction(requested)
    guard let action = actions.first(where: { normalizeAccessibilityAction($0) == normalized }) else {
        throw HelperError.invalidRequest(
            "The element does not expose the \(requested) action"
        )
    }
    try performAccessibilityAction(element, name: action)
}

private func normalizeAccessibilityAction(_ value: String) -> String {
    var normalized = value.lowercased().filter(\.isLetter)
    if normalized.hasPrefix("ax") {
        normalized.removeFirst(2)
    }
    return normalized
}

private func selectText(_ arguments: [String: Any], in element: AXUIElement) throws {
    let text = try requiredString(arguments, "text")
    guard let value = accessibilityString(element, kAXValueAttribute) else {
        throw HelperError.invalidRequest("Text was not found in the element")
    }
    let prefix = arguments["prefix"] as? String
    let suffix = arguments["suffix"] as? String
    let source = value as NSString
    let needle = text as NSString
    var candidates: [NSRange] = []
    var search = NSRange(location: 0, length: source.length)
    while search.length > 0 {
        let match = source.range(of: needle as String, options: [], range: search)
        if match.location == NSNotFound { break }
        let prefixMatches = prefix.map { candidate in
            let length = (candidate as NSString).length
            guard match.location >= length else { return false }
            return source.substring(with: NSRange(location: match.location - length, length: length)) == candidate
        } ?? true
        let suffixMatches = suffix.map { candidate in
            let length = (candidate as NSString).length
            let location = match.location + match.length
            guard location + length <= source.length else { return false }
            return source.substring(with: NSRange(location: location, length: length)) == candidate
        } ?? true
        if prefixMatches && suffixMatches {
            candidates.append(match)
        }
        let next = match.location + max(match.length, 1)
        search = NSRange(location: next, length: source.length - next)
    }
    guard candidates.count == 1, var match = candidates.first else {
        let reason = candidates.isEmpty ? "was not found" : "is ambiguous"
        throw HelperError.invalidRequest("Text \(reason) in the element")
    }
    switch (arguments["selection_type"] as? String) ?? "text" {
    case "text":
        break
    case "cursor_before":
        match.length = 0
    case "cursor_after":
        match.location += match.length
        match.length = 0
    default:
        throw HelperError.invalidRequest(
            "selection_type must be text, cursor_before, or cursor_after"
        )
    }
    var selection = CFRange(location: match.location, length: match.length)
    guard let rangeValue = AXValueCreate(.cfRange, &selection),
          AXUIElementSetAttributeValue(element, kAXSelectedTextRangeAttribute as CFString, rangeValue) == .success else {
        throw HelperError.invalidRequest("The element does not support text selection")
    }
}

private func scrollAccessibility(
    _ arguments: [String: Any],
    element: AXUIElement,
    processID: pid_t
) throws {
    let direction = try requiredString(arguments, "direction").lowercased()
    let pages = number(arguments, "pages") ?? 1
    guard pages.isFinite, pages > 0 else {
        throw HelperError.invalidRequest("pages must be a finite number greater than zero")
    }
    let distance = pages * 700
    let delta: (x: Double, y: Double)
    switch direction {
    case "up", "u": delta = (0, distance)
    case "down", "d": delta = (0, -distance)
    case "left", "l": delta = (distance, 0)
    case "right", "r": delta = (-distance, 0)
    default: throw HelperError.invalidRequest("Unsupported scroll direction: \(direction)")
    }
    try scroll(
        deltaX: delta.x,
        deltaY: delta.y,
        at: accessibilityFrameCenter(element),
        processID: processID
    )
}

private func captureDisplay(for window: SCWindow, in displays: [SCDisplay]) -> SCDisplay? {
    var selected: SCDisplay?
    var selectedArea: CGFloat = 0
    for display in displays {
        let intersection = display.frame.intersection(window.frame)
        guard !intersection.isNull, !intersection.isEmpty else {
            continue
        }
        let area: CGFloat = intersection.width * intersection.height
        if area > selectedArea {
            selected = display
            selectedArea = area
        }
    }
    return selected
}

private func captureSourceRect(for window: SCWindow, on display: SCDisplay) -> CGRect {
    let intersection = window.frame.intersection(display.frame)
    return CGRect(
        x: intersection.minX - display.frame.minX,
        y: intersection.minY - display.frame.minY,
        width: intersection.width,
        height: intersection.height
    )
}

private func target(for window: SCWindow, includeTitle: Bool = true) -> Target {
    let app = window.owningApplication!
    let size = captureSize(for: window.frame)
    return Target(
        windowId: window.windowID,
        bundleId: app.bundleIdentifier,
        teamId: signingTeamId(pid: app.processID),
        appName: app.applicationName,
        windowTitle: includeTitle ? (window.title ?? "Untitled window") : "Window",
        width: UInt32(size.width),
        height: UInt32(size.height)
    )
}

private func signingTeamId(pid: pid_t) -> String? {
    signingInformation(pid: pid)?[kSecCodeInfoTeamIdentifier as String] as? String
}

private func signingInformation(pid: pid_t) -> [String: Any]? {
    var code: SecCode?
    let attributes = [kSecGuestAttributePid as String: NSNumber(value: pid)] as CFDictionary
    guard SecCodeCopyGuestWithAttributes(nil, attributes, SecCSFlags(rawValue: 0), &code) == errSecSuccess,
          let code,
          SecCodeCheckValidity(code, SecCSFlags(rawValue: 0), nil) == errSecSuccess else {
        return nil
    }
    var staticCode: SecStaticCode?
    guard SecCodeCopyStaticCode(code, SecCSFlags(rawValue: 0), &staticCode) == errSecSuccess,
          let staticCode else {
        return nil
    }
    var information: CFDictionary?
    guard SecCodeCopySigningInformation(
        staticCode,
        SecCSFlags(rawValue: UInt32(kSecCSSigningInformation)),
        &information
    ) == errSecSuccess,
    let dictionary = information as? [String: Any] else {
        return nil
    }
    return dictionary
}

private func isBlocked(bundleId: String) -> Bool {
    let bundle = bundleId.lowercased()
    if [
        "codes.waku",
        "com.openai.codex",
        "com.openai.sky.",
        "com.1password.",
        "2bua8c4s2c.com.1password.",
        "com.bitwarden.",
        "com.lastpass.",
    ].contains(where: bundle.hasPrefix) {
        return true
    }
    return [
        "com.apple.loginwindow",
        "com.apple.securityagent",
        "com.apple.systempreferences",
        "com.apple.systemsettings",
        "com.openai.chat",
        "com.apple.terminal",
        "com.apple.keychainaccess",
        "com.googlecode.iterm2",
        "com.mitchellh.ghostty",
        "org.alacritty",
    ].contains(bundle)
}

private actor CaptureSessions {
    static let shared = CaptureSessions()

    private var sessions: [CGWindowID: WindowCaptureSession] = [:]

    func start(
        for window: SCWindow,
        filter: SCContentFilter,
        sourceRect: CGRect
    ) async throws {
        guard sessions[window.windowID] == nil else {
            return
        }
        let staleSessions = sessions.filter { $0.key != window.windowID }.map(\.value)
        sessions = sessions.filter { $0.key == window.windowID }
        for session in staleSessions {
            await session.stop()
        }
        let session = try WindowCaptureSession(
            window: window,
            filter: filter,
            sourceRect: sourceRect
        )
        do {
            try await session.start()
            sessions[window.windowID] = session
        } catch {
            await session.stop()
            throw error
        }
    }

    func stopAll() async {
        let active = sessions.values
        sessions.removeAll()
        for session in active {
            await session.stop()
        }
    }
}

private final class WindowCaptureOutput: NSObject, SCStreamOutput, SCStreamDelegate {
    func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {}

    func stream(_ stream: SCStream, didStopWithError error: Error) {}
}

private final class WindowCaptureSession {
    private let output = WindowCaptureOutput()
    private let outputQueue: DispatchQueue
    private let stream: SCStream
    private var capturing = false

    init(window: SCWindow, filter: SCContentFilter, sourceRect: CGRect) throws {
        let configuration = SCStreamConfiguration()
        // The stream exists to let macOS own and display the window-sharing
        // state. Actual tool screenshots still use SCScreenshotManager.
        let size = captureSize(for: sourceRect)
        configuration.sourceRect = sourceRect
        configuration.width = size.width
        configuration.height = size.height
        configuration.queueDepth = 1
        configuration.minimumFrameInterval = CMTime(value: 1, timescale: 1)
        configuration.showsCursor = false
        configuration.capturesAudio = false
        outputQueue = DispatchQueue(label: "codes.waku.computer-use.capture.\(window.windowID)")
        stream = SCStream(filter: filter, configuration: configuration, delegate: output)
        try stream.addStreamOutput(output, type: .screen, sampleHandlerQueue: outputQueue)
    }

    func start() async throws {
        try await stream.startCapture()
        capturing = true
    }

    func stop() async {
        guard capturing else {
            return
        }
        try? await stream.stopCapture()
        capturing = false
    }
}

private struct Capture {
    let dataUrl: String
    let width: Int
    let height: Int
}

private func captureSize(for frame: CGRect) -> (width: Int, height: Int) {
    let scale = min(
        1,
        min(Double(maximumCaptureWidth) / max(frame.width, 1), Double(maximumCaptureHeight) / max(frame.height, 1))
    )
    return (
        max(1, Int((frame.width * scale).rounded())),
        max(1, Int((frame.height * scale).rounded()))
    )
}

private func capture(
    _ window: SCWindow,
    filter: SCContentFilter,
    sourceRect: CGRect,
    cursor: CGPoint? = nil,
    coordinateSpace: Target? = nil
) async throws -> Capture {
    let size = captureSize(for: sourceRect)
    let image: CGImage
    if #available(macOS 14.0, *) {
        let configuration = SCStreamConfiguration()
        configuration.sourceRect = sourceRect
        configuration.width = size.width
        configuration.height = size.height
        configuration.scalesToFit = true
        configuration.showsCursor = false
        configuration.capturesAudio = false
        image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration)
    } else {
        guard let fallback = CGWindowListCreateImage(
            .null,
            .optionIncludingWindow,
            window.windowID,
            [.boundsIgnoreFraming, .bestResolution]
        ) else {
            throw HelperError.captureFailed
        }
        image = fallback
    }
    let drawnImage = drawVirtualCursor(on: image, cursor: cursor, coordinateSpace: coordinateSpace)
    let representation = NSBitmapImageRep(cgImage: drawnImage)
    guard let png = representation.representation(using: .png, properties: [:]) else {
        throw HelperError.captureFailed
    }
    return Capture(
        dataUrl: "data:image/png;base64," + png.base64EncodedString(),
        width: drawnImage.width,
        height: drawnImage.height
    )
}

private func drawVirtualCursor(on image: CGImage, cursor: CGPoint?, coordinateSpace: Target?) -> CGImage {
    guard let cursor, let coordinateSpace,
          coordinateSpace.width > 0, coordinateSpace.height > 0,
          let context = CGContext(
              data: nil,
              width: image.width,
              height: image.height,
              bitsPerComponent: 8,
              bytesPerRow: image.width * 4,
              space: CGColorSpaceCreateDeviceRGB(),
              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
          ) else {
        return image
    }
    context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
    let x = cursor.x / CGFloat(coordinateSpace.width) * CGFloat(image.width)
    let y = cursor.y / CGFloat(coordinateSpace.height) * CGFloat(image.height)
    context.saveGState()
    context.translateBy(x: x, y: CGFloat(image.height) - y)
    context.setShadow(offset: CGSize(width: 1, height: -1), blur: 2, color: CGColor(gray: 0, alpha: 0.7))
    let arrow = CGMutablePath()
    arrow.move(to: CGPoint(x: 0, y: 0))
    arrow.addLine(to: CGPoint(x: 0, y: -24))
    arrow.addLine(to: CGPoint(x: 7, y: -17))
    arrow.addLine(to: CGPoint(x: 12, y: -29))
    arrow.addLine(to: CGPoint(x: 17, y: -27))
    arrow.addLine(to: CGPoint(x: 12, y: -15))
    arrow.addLine(to: CGPoint(x: 22, y: -15))
    arrow.closeSubpath()
    context.setFillColor(CGColor(gray: 1, alpha: 1))
    context.setStrokeColor(CGColor(gray: 0, alpha: 1))
    context.setLineWidth(2)
    context.addPath(arrow)
    context.drawPath(using: .fillStroke)
    context.restoreGState()
    return context.makeImage() ?? image
}

private func perform(_ actions: [Action], in window: SCWindow, coordinateSpace: Target) throws {
    guard let processID = window.owningApplication?.processID else {
        throw HelperError.targetUnavailable
    }
    let frame = window.frame
    func point(_ x: Double?, _ y: Double?) throws -> CGPoint {
        guard let x, let y, x.isFinite, y.isFinite else {
            throw HelperError.invalidRequest("Pointer actions require finite x and y coordinates")
        }
        let localX = min(max(x, 0), Double(max(coordinateSpace.width, 1)))
        let localY = min(max(y, 0), Double(max(coordinateSpace.height, 1)))
        return CGPoint(
            x: frame.minX + localX / Double(max(coordinateSpace.width, 1)) * frame.width,
            y: frame.minY + localY / Double(max(coordinateSpace.height, 1)) * frame.height
        )
    }

    for action in actions {
        switch action.type {
        case "click":
            updateVirtualCursor(action.x, action.y, window: window, coordinateSpace: coordinateSpace)
            try click(
                at: point(action.x, action.y),
                count: 1,
                button: mouseButton(action.mouseButton),
                processID: processID
            )
        case "double_click":
            updateVirtualCursor(action.x, action.y, window: window, coordinateSpace: coordinateSpace)
            try click(
                at: point(action.x, action.y),
                count: 2,
                button: mouseButton(action.mouseButton),
                processID: processID
            )
        case "move":
            updateVirtualCursor(action.x, action.y, window: window, coordinateSpace: coordinateSpace)
            try post(
                mouseEvent(type: .mouseMoved, at: point(action.x, action.y), button: .left),
                to: processID
            )
        case "drag":
            updateVirtualCursor(action.toX, action.toY, window: window, coordinateSpace: coordinateSpace)
            try drag(
                from: point(action.x, action.y),
                to: point(action.toX, action.toY),
                processID: processID
            )
        case "scroll":
            try scroll(deltaX: action.deltaX ?? 0, deltaY: action.deltaY ?? 0, processID: processID)
        case "type":
            guard let text = action.text, text.utf8.count <= 10_000 else {
                throw HelperError.invalidRequest("Typed text is missing or too long")
            }
            try typeText(text, processID: processID)
        case "keypress":
            guard let key = action.key else {
                throw HelperError.invalidRequest("keypress requires a key")
            }
            try pressKey(key, modifiers: action.modifiers ?? [], processID: processID)
        case "wait":
            usleep(useconds_t(min(action.durationMs ?? 0, 2_000) * 1_000))
        default:
            throw HelperError.unsupportedAction(action.type)
        }
        usleep(90_000)
    }
}

private func updateVirtualCursor(
    _ x: Double?,
    _ y: Double?,
    window: SCWindow,
    coordinateSpace: Target
) {
    guard let x, let y, x.isFinite, y.isFinite else { return }
    VirtualCursorStore.shared.move(
        for: window.windowID,
        to: CGPoint(x: x, y: y),
        size: CGSize(width: CGFloat(coordinateSpace.width), height: CGFloat(coordinateSpace.height))
    )
}

private func mouseEvent(type: CGEventType, at point: CGPoint, button: CGMouseButton) throws -> CGEvent {
    guard let event = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: point, mouseButton: button) else {
        throw HelperError.eventCreationFailed
    }
    return event
}

private func post(_ event: CGEvent, to processID: pid_t) {
    event.postToPid(processID)
}

private func mouseButton(_ value: String?) -> CGMouseButton {
    switch value?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
    case "r", "right", "secondary": return .right
    case "m", "middle", "center": return .center
    default: return .left
    }
}

private func click(at point: CGPoint, count: Int64, button: CGMouseButton, processID: pid_t) throws {
    let downType: CGEventType
    let upType: CGEventType
    switch button {
    case .right:
        downType = .rightMouseDown
        upType = .rightMouseUp
    case .center:
        downType = .otherMouseDown
        upType = .otherMouseUp
    default:
        downType = .leftMouseDown
        upType = .leftMouseUp
    }
    for index in 1...count {
        let down = try mouseEvent(type: downType, at: point, button: button)
        let up = try mouseEvent(type: upType, at: point, button: button)
        down.setIntegerValueField(.mouseEventClickState, value: index)
        up.setIntegerValueField(.mouseEventClickState, value: index)
        post(down, to: processID)
        usleep(35_000)
        post(up, to: processID)
        usleep(70_000)
    }
}

private func drag(from start: CGPoint, to end: CGPoint, processID: pid_t) throws {
    let down = try mouseEvent(type: .leftMouseDown, at: start, button: .left)
    post(down, to: processID)
    for step in 1...12 {
        let progress = Double(step) / 12
        let point = CGPoint(
            x: start.x + (end.x - start.x) * progress,
            y: start.y + (end.y - start.y) * progress
        )
        try post(mouseEvent(type: .leftMouseDragged, at: point, button: .left), to: processID)
        usleep(12_000)
    }
    try post(mouseEvent(type: .leftMouseUp, at: end, button: .left), to: processID)
}

private func scroll(
    deltaX: Double,
    deltaY: Double,
    at location: CGPoint? = nil,
    processID: pid_t
) throws {
    guard deltaX.isFinite, deltaY.isFinite,
          let event = CGEvent(
            scrollWheelEvent2Source: nil,
            units: .pixel,
            wheelCount: 2,
            wheel1: Int32(deltaY.rounded()),
            wheel2: Int32(deltaX.rounded()),
            wheel3: 0
          ) else {
        throw HelperError.eventCreationFailed
    }
    if let location {
        event.location = location
    }
    post(event, to: processID)
}

private func typeText(_ text: String, processID: pid_t) throws {
    for chunk in Array(text.utf16).chunked(maxCount: 32) {
        guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
              let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false) else {
            throw HelperError.eventCreationFailed
        }
        down.keyboardSetUnicodeString(stringLength: chunk.count, unicodeString: chunk)
        up.keyboardSetUnicodeString(stringLength: chunk.count, unicodeString: chunk)
        post(down, to: processID)
        post(up, to: processID)
        usleep(8_000)
    }
}

private func pressKey(_ key: String, modifiers: [String], processID: pid_t) throws {
    guard let keyCode = virtualKeyCode(for: key) else {
        throw HelperError.invalidRequest("Unsupported key: \(key)")
    }
    let flags = eventFlags(modifiers)
    guard let down = CGEvent(keyboardEventSource: nil, virtualKey: keyCode, keyDown: true),
          let up = CGEvent(keyboardEventSource: nil, virtualKey: keyCode, keyDown: false) else {
        throw HelperError.eventCreationFailed
    }
    down.flags = flags
    up.flags = flags
    post(down, to: processID)
    usleep(35_000)
    post(up, to: processID)
}

private func eventFlags(_ modifiers: [String]) -> CGEventFlags {
    var flags: CGEventFlags = []
    for modifier in modifiers {
        switch modifier.lowercased() {
        case "command": flags.insert(.maskCommand)
        case "control": flags.insert(.maskControl)
        case "option": flags.insert(.maskAlternate)
        case "shift": flags.insert(.maskShift)
        default: break
        }
    }
    return flags
}

private func virtualKeyCode(for key: String) -> CGKeyCode? {
    let codes: [String: CGKeyCode] = [
        "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7,
        "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14, "r": 15,
        "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22,
        "5": 23, "=": 24, "9": 25, "7": 26, "-": 27, "8": 28, "0": 29,
        "]": 30, "o": 31, "u": 32, "[": 33, "i": 34, "p": 35, "return": 36,
        "enter": 36, "l": 37, "j": 38, "'": 39, "k": 40, ";": 41, "\\": 42,
        ",": 43, "/": 44, "n": 45, "m": 46, ".": 47, "tab": 48, "space": 49,
        "`": 50, "backspace": 51, "delete": 51, "escape": 53, "command": 55,
        "shift": 56, "capslock": 57, "option": 58, "control": 59, "f17": 64,
        "volumeup": 72, "volumedown": 73, "mute": 74, "f18": 79, "f19": 80,
        "f20": 90, "f5": 96, "f6": 97, "f7": 98, "f3": 99, "f8": 100,
        "f9": 101, "f11": 103, "f13": 105, "f16": 106, "f14": 107, "f10": 109,
        "f12": 111, "f15": 113, "home": 115, "pageup": 116, "forwarddelete": 117,
        "f4": 118, "end": 119, "f2": 120, "pagedown": 121, "f1": 122,
        "left": 123, "right": 124, "down": 125, "up": 126,
    ]
    let normalized = key.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    let aliases: [String: String] = [
        "backspace": "delete",
        "esc": "escape",
        "page_down": "pagedown",
        "page_up": "pageup",
        "period": ".",
        "greater": ".",
        "kp_0": "0",
        "numpad_0": "0",
        "spacebar": "space",
    ]
    return codes[aliases[normalized] ?? normalized]
}

private extension Array where Element == UInt16 {
    func chunked(maxCount: Int) -> [[UInt16]] {
        guard !isEmpty else { return [] }
        return stride(from: 0, to: count, by: maxCount).map { start in
            Array(self[start..<Swift.min(start + maxCount, count)])
        }
    }
}
