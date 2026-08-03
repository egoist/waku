import AppKit
import CoreGraphics
import CoreImage
import CoreMedia
import CoreVideo
import Darwin
import Foundation
import ApplicationServices
import ScreenCaptureKit
import Security

// This process owns macOS privacy access and keeps its Apple Development
// code-signing identity while the surrounding debug app is rebuilt.
private let maximumCaptureWidth = 1440
private let maximumCaptureHeight = 1200
private let maximumPreviewWidth = 608
private let maximumPreviewHeight = 480
private let previewFramesPerSecond: Int32 = 15
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

private enum CursorAssets {
    static let overlaySize = NSSize(width: 32, height: 32)
    static let overlayHotspot = NSPoint(x: 4, y: 3)

    static let menuBar: NSImage? = {
        guard let url = Bundle.main.url(forResource: "menubar-cursor", withExtension: "png") else {
            return nil
        }
        return NSImage(contentsOf: url)
    }()

    static let overlayImage: NSImage? = {
        guard let url = Bundle.main.url(forResource: "overlay-cursor", withExtension: "svg") else {
            return nil
        }
        return NSImage(contentsOf: url)
    }()

    static let overlay: CGImage? = {
        guard let image = overlayImage else { return nil }
        var rect = NSRect(origin: .zero, size: overlaySize)
        return image.cgImage(forProposedRect: &rect, context: nil, hints: nil)
    }()
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

    @discardableResult
    func move(for windowID: CGWindowID, to position: CGPoint, size: CGSize) -> CGPoint {
        let clamped = CGPoint(
            x: min(max(position.x, 0), max(size.width, 1)),
            y: min(max(position.y, 0), max(size.height, 1))
        )
        lock.lock()
        positions[windowID] = clamped
        lock.unlock()
        return clamped
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
        item.length = 54
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
        let image = self.statusImage()
        item.length = image.size.width
        item.button?.image = image
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
        let visibleApps = Array(apps.prefix(4))
        let stackedAppWidth = CGFloat(max(visibleApps.count - 1, 0)) * 8
        let cursorX = 29 + stackedAppWidth
        let imageWidth = 54 + stackedAppWidth
        let image = NSImage(size: NSSize(width: imageWidth, height: 22))
        image.lockFocus()
        let bounds = NSRect(x: 0.5, y: 0.5, width: imageWidth - 1, height: 21)
        NSColor.white.withAlphaComponent(0.58).setFill()
        NSBezierPath(roundedRect: bounds, xRadius: 10.5, yRadius: 10.5).fill()

        for (index, app) in visibleApps.enumerated() {
            let iconRect = NSRect(
                x: 7 + CGFloat(index) * 8,
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

        CursorAssets.menuBar?.draw(
            in: NSRect(x: cursorX, y: 2, width: 19, height: 17),
            from: .zero,
            operation: .sourceOver,
            fraction: 1
        )
        image.unlockFocus()
        return image
    }
}

private final class SoftwareCursorView: NSView {
    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        CursorAssets.overlayImage?.draw(
            in: bounds,
            from: .zero,
            operation: .sourceOver,
            fraction: 1,
            respectFlipped: true,
            hints: nil
        )
    }
}

private final class SoftwareCursorOverlay {
    static let shared = SoftwareCursorOverlay()

    private struct State {
        let panel: NSPanel
        let processID: pid_t
        let windowLayer: Int
        let normalizedPoint: CGPoint
        let fallbackScreenPoint: CGPoint
    }

    private var states: [CGWindowID: State] = [:]
    private var refreshTimer: Timer?

    func show(
        windowID: CGWindowID,
        processID: pid_t,
        windowLayer: Int,
        normalizedPoint: CGPoint,
        fallbackScreenPoint: CGPoint
    ) {
        guard CursorAssets.overlayImage != nil else { return }
        DispatchQueue.main.async {
            let panel = self.states[windowID]?.panel ?? self.makePanel()
            self.states[windowID] = State(
                panel: panel,
                processID: processID,
                windowLayer: windowLayer,
                normalizedPoint: normalizedPoint,
                fallbackScreenPoint: fallbackScreenPoint
            )
            self.startRefreshTimerIfNeeded()
            self.refresh()
        }
    }

    private func startRefreshTimerIfNeeded() {
        guard refreshTimer == nil else { return }
        refreshTimer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
            self?.refresh()
        }
    }

    private func refresh() {
        for (windowID, state) in states {
            guard let screenPoint = screenPoint(for: windowID, state: state) else {
                state.panel.orderOut(nil)
                continue
            }
            let mainScreenMaxY = NSScreen.screens.first?.frame.maxY ?? 0
            state.panel.setFrameOrigin(NSPoint(
                x: screenPoint.x - CursorAssets.overlayHotspot.x,
                y: mainScreenMaxY - screenPoint.y
                    - (CursorAssets.overlaySize.height - CursorAssets.overlayHotspot.y)
            ))
            state.panel.level = NSWindow.Level(rawValue: state.windowLayer)
            // Keep the cursor immediately above its controlled window rather
            // than gating it on macOS's single global frontmost application.
            // A window on another display can therefore stay visible while
            // focus is elsewhere, and windows above the target in this same
            // z-order still cover the cursor naturally.
            state.panel.order(.above, relativeTo: Int(windowID))
        }
    }

    private func screenPoint(for windowID: CGWindowID, state: State) -> CGPoint? {
        guard let windows = CGWindowListCopyWindowInfo([.optionIncludingWindow], windowID)
                as? [[String: Any]],
              let window = windows.first(where: {
                  ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == windowID
              }) else {
            return state.fallbackScreenPoint
        }
        guard (window[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == state.processID,
              (window[kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue != false,
              let bounds = window[kCGWindowBounds as String] as? NSDictionary else {
            return nil
        }
        var frame = CGRect.zero
        guard CGRectMakeWithDictionaryRepresentation(bounds as CFDictionary, &frame) else {
            return nil
        }
        return CGPoint(
            x: frame.minX + state.normalizedPoint.x * frame.width,
            y: frame.minY + state.normalizedPoint.y * frame.height
        )
    }

    private func makePanel() -> NSPanel {
        let panel = NSPanel(
            contentRect: NSRect(origin: .zero, size: CursorAssets.overlaySize),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.hidesOnDeactivate = false
        panel.ignoresMouseEvents = true
        panel.collectionBehavior = [.fullScreenAuxiliary]
        panel.contentView = SoftwareCursorView(frame: NSRect(origin: .zero, size: CursorAssets.overlaySize))
        return panel
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
        send(["type": "track", "bundleID": bundleID, "name": name])
    }

    func showCursor(window: SCWindow, localPoint: CGPoint, coordinateSpace: Target) {
        let width = CGFloat(max(coordinateSpace.width, 1))
        let height = CGFloat(max(coordinateSpace.height, 1))
        let normalizedPoint = CGPoint(x: localPoint.x / width, y: localPoint.y / height)
        let screenPoint = CGPoint(
            x: window.frame.minX + normalizedPoint.x * window.frame.width,
            y: window.frame.minY + normalizedPoint.y * window.frame.height
        )
        guard let processID = window.owningApplication?.processID else { return }
        start()
        send([
            "type": "cursor",
            "windowID": Int(window.windowID),
            "processID": Int(processID),
            "windowLayer": window.windowLayer,
            "normalizedX": Double(normalizedPoint.x),
            "normalizedY": Double(normalizedPoint.y),
            "x": Double(screenPoint.x),
            "y": Double(screenPoint.y),
        ])
    }

    private func send(_ message: [String: Any]) {
        lock.lock()
        defer { lock.unlock() }
        guard let input else { return }
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

private final class BridgeProcessRegistration {
    static let shared = BridgeProcessRegistration()

    private var registration: URL?
    private let lock = NSLock()

    func activate() {
        lock.lock()
        defer { lock.unlock() }
        guard registration == nil,
              let directoryPath = commandLineArgument("--process-directory"),
              let bridgePID = commandLineArgument("--bridge-pid"),
              let bridgePIDValue = Int32(bridgePID),
              bridgePIDValue > 1 else {
            return
        }
        let directory = URL(fileURLWithPath: directoryPath, isDirectory: true).standardizedFileURL
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: directory.path, isDirectory: &isDirectory),
              isDirectory.boolValue else {
            return
        }
        let registration = directory.appendingPathComponent(bridgePID, isDirectory: false)
        guard FileManager.default.createFile(atPath: registration.path, contents: Data()) else {
            return
        }
        self.registration = registration
    }

    func remove() {
        lock.lock()
        let registration = self.registration
        self.registration = nil
        lock.unlock()
        if let registration {
            try? FileManager.default.removeItem(at: registration)
        }
    }
}

private struct ComputerUsePreviewUpdate: Encodable {
    let target: Target
    let imageUrl: String
}

private enum ComputerUsePreviewStore {
    static func publish(target: Target, capture: Capture) {
        guard let directoryPath = commandLineArgument("--process-directory") else { return }
        let directory = URL(fileURLWithPath: directoryPath, isDirectory: true)
        let destination = directory.appendingPathComponent(
            "preview-\(target.windowId).json",
            isDirectory: false
        )
        let update = ComputerUsePreviewUpdate(target: target, imageUrl: capture.dataUrl)
        guard let data = try? JSONEncoder().encode(update) else { return }
        try? data.write(to: destination, options: .atomic)
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
                    defer {
                        BridgeProcessRegistration.shared.remove()
                        StatusAgentProcess.shared.stop()
                    }
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
                while let line = readLine(from: .standardInput) {
                    guard let data = line.data(using: .utf8),
                          let message = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
                    else {
                        continue
                    }
                    DispatchQueue.main.async {
                        switch message["type"] as? String {
                        case "cursor":
                            guard let windowID = (message["windowID"] as? NSNumber)?.uint32Value,
                                  let processID = (message["processID"] as? NSNumber)?.int32Value,
                                  let windowLayer = (message["windowLayer"] as? NSNumber)?.intValue,
                                  let normalizedX = (message["normalizedX"] as? NSNumber)?.doubleValue,
                                  let normalizedY = (message["normalizedY"] as? NSNumber)?.doubleValue,
                                  let x = (message["x"] as? NSNumber)?.doubleValue,
                                  let y = (message["y"] as? NSNumber)?.doubleValue else {
                                return
                            }
                            SoftwareCursorOverlay.shared.show(
                                windowID: windowID,
                                processID: processID,
                                windowLayer: windowLayer,
                                normalizedPoint: CGPoint(x: normalizedX, y: normalizedY),
                                fallbackScreenPoint: CGPoint(x: x, y: y)
                            )
                        default:
                            guard let bundleID = message["bundleID"] as? String,
                                  let name = message["name"] as? String else {
                                return
                            }
                            ComputerUseStatusItem.shared.track(bundleID: bundleID, name: name)
                        }
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
        defer { TargetProcessInputRouting.deactivateCurrentRoute() }
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
            BridgeProcessRegistration.shared.activate()
            try await CaptureSessions.shared.start(
                for: window,
                filter: filter,
                sourceRect: sourceRect,
                target: current
            )
            if !actions.isEmpty {
                try perform(actions, in: window, coordinateSpace: requested)
            }
            StatusAgentProcess.shared.track(bundleID: current.bundleId, name: current.appName)
            let cursor = VirtualCursorStore.shared.position(
                for: window.windowID,
                size: CGSize(width: CGFloat(requested.width), height: CGFloat(requested.height))
            )
            let capture = try await capture(
                window,
                filter: filter,
                sourceRect: sourceRect,
                cursor: cursor,
                coordinateSpace: requested
            )
            let capturedTarget = Target(
                windowId: current.windowId,
                bundleId: current.bundleId,
                teamId: current.teamId,
                appName: current.appName,
                windowTitle: current.windowTitle,
                width: UInt32(capture.width),
                height: UInt32(capture.height)
            )
            ComputerUsePreviewStore.publish(target: capturedTarget, capture: capture)
            return Response(
                success: true,
                error: nil,
                permissions: currentPermissions(),
                targets: nil,
                target: capturedTarget,
                imageUrl: capture.dataUrl,
                summary: actions.isEmpty ? "Captured \(current.appName)." : "Completed \(actions.count) action\(actions.count == 1 ? "" : "s") in \(current.appName)."
            )
        default:
            throw HelperError.invalidRequest("Unknown operation: \(request.operation)")
        }
    }

    static func serveMCP(input: FileHandle, output: FileHandle) async throws {
        // The inactive-window route is intentionally shared by every action in
        // this MCP session. Tear it down once when the session ends, never once
        // per pen stroke; per-action activation/deactivation repaints Chromium
        // browser chrome and is visible as toolbar blinking.
        defer { TargetProcessInputRouting.deactivateCurrentRoute() }
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
        var childArguments = [
            "mcp",
            "mcp-child",
            "--socket",
            listener.path,
            "--bridge-pid",
            String(getpid()),
        ]
        if let processDirectory = ProcessInfo.processInfo.environment["WAKU_COMPUTER_USE_PROCESS_DIRECTORY"],
           !processDirectory.isEmpty {
            childArguments.append(contentsOf: ["--process-directory", processDirectory])
        }
        let launcher = try launchSelfThroughLaunchServices(
            arguments: childArguments
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
                let rawCount = number(arguments, "click_count") ?? 1
                guard rawCount.isFinite,
                      rawCount.rounded() == rawCount,
                      rawCount >= 1,
                      rawCount <= 10 else {
                    throw HelperError.invalidRequest(
                        "click_count must be an integer from 1 through 10"
                    )
                }
                let button = mouseButton(arguments["mouse_button"] as? String)
                if button == .left, rawCount == 1 {
                    try withInactiveElementRouting(
                        resolved: resolved,
                        element: element
                    ) {
                        try performAccessibilityAction(element, name: kAXPressAction)
                    }
                } else {
                    guard let location = accessibilityFrameCenter(element) else {
                        throw HelperError.invalidRequest(
                            "Element \(index) has no clickable position"
                        )
                    }
                    try click(
                        at: location,
                        localPoint: CGPoint(
                            x: location.x - resolved.window.frame.minX,
                            y: location.y - resolved.window.frame.minY
                        ),
                        count: Int64(rawCount),
                        button: button,
                        windowID: resolved.window.windowID,
                        processID: resolved.processID
                    )
                }
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
                let globalPoint = CGPoint(
                    x: resolved.window.frame.midX,
                    y: resolved.window.frame.midY
                )
                try withInactiveWindowRouting(
                    processID: resolved.processID,
                    windowID: resolved.window.windowID,
                    globalPoint: globalPoint,
                    localPoint: CGPoint(
                        x: resolved.window.frame.width / 2,
                        y: resolved.window.frame.height / 2
                    )
                ) {
                    try pressKey(
                        keyParts.key,
                        modifiers: keyParts.modifiers,
                        processID: resolved.processID
                    )
                }
            default:
                let globalPoint = CGPoint(
                    x: resolved.window.frame.midX,
                    y: resolved.window.frame.midY
                )
                try withInactiveWindowRouting(
                    processID: resolved.processID,
                    windowID: resolved.window.windowID,
                    globalPoint: globalPoint,
                    localPoint: CGPoint(
                        x: resolved.window.frame.width / 2,
                        y: resolved.window.frame.height / 2
                    )
                ) {
                    try typeText(
                        try requiredString(arguments, "text"),
                        processID: resolved.processID
                    )
                }
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
            try withInactiveElementRouting(
                resolved: resolved,
                element: element
            ) {
                switch name {
                case "perform_secondary_action":
                    try performAccessibilityAction(
                        element,
                        matching: try requiredString(arguments, "action")
                    )
                case "set_value":
                    let value = try requiredString(arguments, "value")
                    guard AXUIElementSetAttributeValue(
                        element,
                        kAXValueAttribute as CFString,
                        value as CFTypeRef
                    ) == .success else {
                        throw HelperError.invalidRequest(
                            "Element \(index) does not accept a value"
                        )
                    }
                case "select_text":
                    try selectText(arguments, in: element)
                default:
                    try scrollAccessibility(
                        arguments,
                        element: element,
                        processID: resolved.processID
                    )
                }
            }
            RecentComputerActionStore.shared.record(resolved.target.bundleId)
            return mcpResult(text: "")

        default:
            throw HelperError.invalidRequest("Unknown MCP tool: \(name)")
        }
    }
}

private func commandLineArgument(_ name: String) -> String? {
    guard let index = CommandLine.arguments.firstIndex(of: name),
          CommandLine.arguments.indices.contains(index + 1) else {
        return nil
    }
    return CommandLine.arguments[index + 1]
}

private func socketPathArgument() -> String? {
    commandLineArgument("--socket")
}

private final class AccessibilityRegistry {
    static let shared = AccessibilityRegistry()

    private var elements: [String: AXUIElement] = [:]
    private var orderedElements: [(String, AXUIElement)] = []
    private var bundleID: String?
    private var processID: pid_t?
    private var windowID: CGWindowID?
    private let lock = NSLock()

    func reset(bundleID: String, processID: pid_t, windowID: CGWindowID) {
        lock.lock()
        elements.removeAll(keepingCapacity: true)
        orderedElements.removeAll(keepingCapacity: true)
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
        orderedElements.append((index, element))
        return index
    }

    func index(for element: AXUIElement) -> String? {
        lock.lock()
        defer { lock.unlock() }
        return orderedElements.first(where: { CFEqual($0.1, element) })?.0
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
    let applicationBundle = try containingApplicationBundleURL()
    let launcher = Process()
    launcher.executableURL = URL(fileURLWithPath: "/usr/bin/open")
    launcher.arguments = ["-n", "-W"]
        + (background ? ["-g"] : [])
        + [applicationBundle.path, "--args"]
        + arguments
    launcher.standardInput = FileHandle.nullDevice
    launcher.standardOutput = FileHandle.nullDevice
    launcher.standardError = FileHandle.nullDevice
    try launcher.run()
    return launcher
}

private func containingApplicationBundleURL() throws -> URL {
    let executable = URL(fileURLWithPath: CommandLine.arguments[0])
        .standardizedFileURL
        .resolvingSymlinksInPath()
    let contents = executable
        .deletingLastPathComponent()
        .deletingLastPathComponent()
    let application = contents.deletingLastPathComponent()
    guard contents.lastPathComponent == "Contents",
          application.pathExtension.lowercased() == "app",
          FileManager.default.fileExists(atPath: application.path) else {
        throw HelperError.ipc(
            "could not resolve the Computer Use app bundle from \(executable.path)"
        )
    }
    return application
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
            let window = tree.split(separator: "\n", omittingEmptySubsequences: false)
                .first(where: { $0.hasPrefix("Window: ") })
                .map(String.init)
                ?? "the current window."
            let focus = tree.split(separator: "\n", omittingEmptySubsequences: false)
                .last(where: { $0.hasPrefix("The focused UI element is ") })
                .map(String.init)
            var result = "There has been no change in the accessibility tree for \(window)"
            if let focus {
                result += "\n\(focus)"
            }
            return result
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

private final class AppSpecificInstructionsStore {
    static let shared = AppSpecificInstructionsStore()

    private var injectedApps = Set<String>()
    private let lock = NSLock()

    func take(_ instructions: String, for bundleID: String) -> String? {
        lock.lock()
        let isFirstInjection = injectedApps.insert(bundleID.lowercased()).inserted
        lock.unlock()
        return isFirstInjection ? instructions : nil
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
        let trimmed = value.drop(while: { $0 == "\t" || $0 == " " })
        let digits = trimmed.prefix(while: \.isNumber)
        guard !digits.isEmpty, let index = Int(digits) else {
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
    let location = try pointInWindow(x: x, y: y, resolved: resolved)
    try click(
        at: location,
        localPoint: CGPoint(
            x: location.x - resolved.window.frame.minX,
            y: location.y - resolved.window.frame.minY
        ),
        count: Int64(rawCount),
        button: mouseButton(arguments["mouse_button"] as? String),
        windowID: resolved.window.windowID,
        processID: resolved.processID
    )
}

private func withInactiveElementRouting<T>(
    resolved: ResolvedAppWindow,
    element: AXUIElement,
    _ operation: () throws -> T
) throws -> T {
    let point = accessibilityFrameCenter(element) ?? CGPoint(
        x: resolved.window.frame.midX,
        y: resolved.window.frame.midY
    )
    return try withInactiveWindowRouting(
        processID: resolved.processID,
        windowID: resolved.window.windowID,
        globalPoint: point,
        localPoint: CGPoint(
            x: point.x - resolved.window.frame.minX,
            y: point.y - resolved.window.frame.minY
        ),
        operation
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

private func accessibilityElement(_ element: AXUIElement, _ name: String) -> AXUIElement? {
    guard let value = accessibilityAttribute(element, name),
          CFGetTypeID(value) == AXUIElementGetTypeID() else {
        return nil
    }
    return unsafeBitCast(value, to: AXUIElement.self)
}

private func accessibilityBoolean(_ element: AXUIElement, _ name: String) -> Bool? {
    (accessibilityAttribute(element, name) as? NSNumber)?.boolValue
}

private func accessibilityURLString(_ element: AXUIElement, _ name: String) -> String? {
    guard let value = accessibilityAttribute(element, name) else { return nil }
    if let url = value as? URL {
        return url.absoluteString
    }
    if let url = value as? NSURL {
        return url.absoluteString
    }
    return value as? String
}

private func accessibilityValueString(_ value: AnyObject?) -> String? {
    guard let value else { return nil }
    if let string = value as? String {
        return string
    }
    if CFGetTypeID(value) == CFBooleanGetTypeID(), let number = value as? NSNumber {
        return number.boolValue ? "on" : "off"
    }
    if let number = value as? NSNumber {
        return number.stringValue
    }
    if let attributed = value as? NSAttributedString {
        return attributed.string
    }
    return nil
}

private func accessibilityFrame(_ element: AXUIElement) -> CGRect? {
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
    return CGRect(origin: position, size: size)
}

private func accessibilityChildren(_ element: AXUIElement) -> [AXUIElement] {
    if accessibilityString(element, kAXRoleAttribute) == "AXLink" {
        return []
    }
    if ["AXCloseButton", "AXFullScreenButton", "AXMinimizeButton", "AXZoomButton"]
        .contains(accessibilityString(element, kAXSubroleAttribute)) {
        return []
    }
    var attributes = [
        kAXChildrenAttribute,
        "AXChildrenInNavigationOrder",
        kAXVisibleChildrenAttribute,
        kAXContentsAttribute,
        "AXRows",
        "AXColumns",
        "AXTabs",
    ]
    if accessibilityString(element, kAXRoleAttribute) == kAXWindowRole {
        attributes += [
            "AXToolbarButton",
            "AXCloseButton",
            "AXFullScreenButton",
            "AXZoomButton",
            "AXMinimizeButton",
            "AXDefaultButton",
            "AXCancelButton",
        ]
    }
    var seen = Set<CFHashCode>()
    var children: [AXUIElement] = []
    for attribute in attributes {
        if let child = accessibilityElement(element, attribute),
           seen.insert(CFHash(child)).inserted {
            children.append(child)
        }
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
    guard let frame = accessibilityFrame(element) else { return nil }
    return CGPoint(x: frame.midX, y: frame.midY)
}

private func accessibilityWindowCandidates(_ application: AXUIElement) -> [AXUIElement] {
    var candidates: [AXUIElement] = []
    var seen = Set<CFHashCode>()
    func append(_ candidate: AXUIElement?) {
        guard let candidate,
              accessibilityString(candidate, kAXRoleAttribute) == kAXWindowRole,
              seen.insert(CFHash(candidate)).inserted else {
            return
        }
        candidates.append(candidate)
    }
    for candidate in (accessibilityAttribute(application, kAXWindowsAttribute) as? [AXUIElement]) ?? [] {
        append(candidate)
    }
    append(accessibilityElement(application, kAXFocusedWindowAttribute))
    append(accessibilityElement(application, kAXMainWindowAttribute))
    for candidate in accessibilityChildren(application) {
        append(candidate)
    }
    return candidates
}

private func chooseAccessibilityWindow(
    application: AXUIElement,
    resolved: ResolvedAppWindow
) -> AXUIElement {
    let candidates = accessibilityWindowCandidates(application)
    guard !candidates.isEmpty else { return application }
    let targetFrame = resolved.window.frame
    let targetTitle = resolved.target.windowTitle
        .trimmingCharacters(in: .whitespacesAndNewlines)
        .lowercased()
    let focusedWindow = accessibilityElement(application, kAXFocusedWindowAttribute)
    let mainWindow = accessibilityElement(application, kAXMainWindowAttribute)

    func score(_ candidate: AXUIElement) -> Double {
        var score = 0.0
        for attribute in ["AXWindowNumber", "_AXWindowNumber"] {
            if (accessibilityAttribute(candidate, attribute) as? NSNumber)?.uint32Value
                == resolved.target.windowId {
                score += 1_000_000
            }
        }
        let title = (accessibilityString(candidate, kAXTitleAttribute) ?? "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
        if !targetTitle.isEmpty, title == targetTitle {
            score += 100_000
        } else if !targetTitle.isEmpty,
                  (title.contains(targetTitle) || targetTitle.contains(title)),
                  !title.isEmpty {
            score += 50_000
        }
        if let frame = accessibilityFrame(candidate), frame.width > 0, frame.height > 0 {
            let intersection = frame.intersection(targetFrame)
            if !intersection.isNull, !intersection.isEmpty {
                let intersectionArea = intersection.width * intersection.height
                let unionArea = frame.width * frame.height
                    + targetFrame.width * targetFrame.height
                    - intersectionArea
                score += 20_000 * Double(intersectionArea / max(unionArea, 1))
            }
            let widthRatio = min(frame.width, targetFrame.width) / max(frame.width, targetFrame.width, 1)
            let heightRatio = min(frame.height, targetFrame.height) / max(frame.height, targetFrame.height, 1)
            score += 5_000 * Double(widthRatio * heightRatio)
            if frame.width < 80 || frame.height < 60 {
                score -= 10_000
            }
        }
        if let focusedWindow, CFEqual(candidate, focusedWindow) { score += 200 }
        if let mainWindow, CFEqual(candidate, mainWindow) { score += 100 }
        return score
    }
    return candidates.max(by: { score($0) < score($1) }) ?? candidates[0]
}

private let browserComputerUseInstructions = """
<app_specific_instructions>
## Browser Computer Use

When navigating to a new website or starting a separate web task, prefer opening a new tab instead of reusing the current tab; reuse the current tab only when the user explicitly asks to continue there or when the current page is clearly the right place to continue the existing workflow.
</app_specific_instructions>
"""

private func isBrowser(bundleID: String) -> Bool {
    let bundleID = bundleID.lowercased()
    return [
        "com.apple.safari",
        "com.brave.browser",
        "com.google.chrome",
        "com.microsoft.edgemac",
        "company.thebrowser.browser",
        "net.imput.helium",
        "org.mozilla.firefox",
    ].contains(where: bundleID.hasPrefix)
}

private func accessibilityText(for resolved: ResolvedAppWindow) -> String {
    AccessibilityRegistry.shared.reset(
        bundleID: resolved.target.bundleId,
        processID: resolved.processID,
        windowID: resolved.target.windowId
    )
    let application = accessibilityRoot(for: resolved.processID)
    enableEnhancedAccessibility(application)
    let window = chooseAccessibilityWindow(application: application, resolved: resolved)
    var lines: [String] = []
    var ancestors = Set<CFHashCode>()
    appendAccessibilityTree(
        window,
        depth: 0,
        maximumDepth: 30,
        lines: &lines,
        ancestors: &ancestors
    )
    if let menuBar = accessibilityElement(application, kAXMenuBarAttribute) {
        ancestors.removeAll(keepingCapacity: true)
        appendAccessibilityTree(
            menuBar,
            depth: 0,
            maximumDepth: 1,
            lines: &lines,
            ancestors: &ancestors
        )
    }

    let focused = accessibilityElement(application, kAXFocusedUIElementAttribute)
    if let focused, AccessibilityRegistry.shared.index(for: focused) == nil {
        ancestors.removeAll(keepingCapacity: true)
        appendAccessibilityTree(
            focused,
            depth: 0,
            maximumDepth: 0,
            lines: &lines,
            ancestors: &ancestors
        )
    }

    var sections: [String] = []
    sections.append("Window: \"\(resolved.target.windowTitle)\", App: \(resolved.target.appName).")
    sections.append(lines.joined(separator: "\n"))
    if let focused,
       let focusedIndex = AccessibilityRegistry.shared.index(for: focused) {
        sections.append(
            "The focused UI element is \(accessibilityDetail(focused, index: focusedIndex))"
        )
    }
    return sections.joined(separator: "\n")
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
    BridgeProcessRegistration.shared.activate()
    try await CaptureSessions.shared.start(
        for: resolved.window,
        filter: filter,
        sourceRect: sourceRect,
        target: resolved.target
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
    ComputerUsePreviewStore.publish(target: resolved.target, capture: capture)
    let fullTree = accessibilityText(for: resolved)
    var text = AccessibilityDiffStore.shared.render(
        fullTree,
        for: resolved.target.bundleId,
        disableDiff: disableDiff
    )
    // App-specific guidance belongs to the conversation context, not to each
    // accessibility snapshot. A fresh full tree must not repeat it.
    if isBrowser(bundleID: resolved.target.bundleId),
       let instructions = AppSpecificInstructionsStore.shared.take(
           browserComputerUseInstructions,
           for: resolved.target.bundleId
       ) {
        text = "\(instructions)\n\(text)"
    }
    return (text, capture)
}

private func appendAccessibilityTree(
    _ element: AXUIElement,
    depth: Int,
    maximumDepth: Int,
    lines: inout [String],
    ancestors: inout Set<CFHashCode>
) {
    guard depth <= maximumDepth, lines.count < 5_000 else { return }
    let hash = CFHash(element)
    guard !ancestors.contains(hash) else { return }
    ancestors.insert(hash)
    defer { ancestors.remove(hash) }

    let children = accessibilityChildren(element).filter {
        !isEmptyAccessibilityContainerLeaf($0)
    }
    if shouldFlattenAccessibilityElement(element, children: children) {
        for child in children {
            appendAccessibilityTree(
                child,
                depth: depth,
                maximumDepth: maximumDepth,
                lines: &lines,
                ancestors: &ancestors
            )
        }
        return
    }

    let index = AccessibilityRegistry.shared.add(element)
    lines.append(String(repeating: "\t", count: depth) + accessibilityDetail(element, index: index))
    let parentRole = accessibilityString(element, kAXRoleAttribute)
    for child in children {
        if isRedundantLinkText(child, parent: element) { continue }
        if parentRole == kAXMenuBarRole, shouldHideMenuBarItem(child) { continue }
        appendAccessibilityTree(
            child,
            depth: depth + 1,
            maximumDepth: maximumDepth,
            lines: &lines,
            ancestors: &ancestors
        )
    }
}

private func shouldFlattenAccessibilityElement(
    _ element: AXUIElement,
    children: [AXUIElement]
) -> Bool {
    guard children.count == 1 else { return false }
    return isSemanticallyEmptyAccessibilityContainer(element)
}

private func isEmptyAccessibilityContainerLeaf(_ element: AXUIElement) -> Bool {
    isSemanticallyEmptyAccessibilityContainer(element)
        && accessibilityChildren(element).isEmpty
}

private func isSemanticallyEmptyAccessibilityContainer(_ element: AXUIElement) -> Bool {
    let role = accessibilityString(element, kAXRoleAttribute)
    guard role == kAXGroupRole || role == "AXGenericElement" else { return false }
    return renderedAccessibilityString(accessibilityString(element, kAXTitleAttribute)) == nil
        && renderedAccessibilityString(accessibilityString(element, kAXDescriptionAttribute)) == nil
        && renderedAccessibilityString(
            accessibilityValueString(accessibilityAttribute(element, kAXValueAttribute))
        ) == nil
        && renderedAccessibilityString(accessibilityURLString(element, kAXURLAttribute)) == nil
}

private func shouldHideMenuBarItem(_ element: AXUIElement) -> Bool {
    let title = renderedAccessibilityString(accessibilityString(element, kAXTitleAttribute))
    return title == "Apple" || title == "AppKit Debug"
}

private func isRedundantLinkText(_ element: AXUIElement, parent: AXUIElement) -> Bool {
    guard accessibilityString(parent, kAXRoleAttribute) == "AXLink",
          accessibilityString(element, kAXRoleAttribute) == kAXStaticTextRole else {
        return false
    }
    let childText = renderedAccessibilityString(accessibilityString(element, kAXTitleAttribute))
        ?? renderedAccessibilityString(
            accessibilityValueString(accessibilityAttribute(element, kAXValueAttribute))
        )
    let parentText = renderedAccessibilityString(accessibilityString(parent, kAXTitleAttribute))
        ?? renderedAccessibilityString(accessibilityString(parent, kAXDescriptionAttribute))
    return childText != nil && childText == parentText
}

private func accessibilityDetail(_ element: AXUIElement, index: String) -> String {
    let axRole = accessibilityString(element, kAXRoleAttribute) ?? "AXUIElement"
    let subrole = accessibilityString(element, kAXSubroleAttribute)
    let roleDescription = accessibilityString(element, "AXRoleDescription")
    let role = semanticAccessibilityRole(
        role: axRole,
        subrole: subrole,
        roleDescription: roleDescription
    )
    let rawValue = accessibilityAttribute(element, kAXValueAttribute)
    var value = accessibilityValueString(rawValue)
    var title = accessibilityString(element, kAXTitleAttribute)
    var description = accessibilityString(element, kAXDescriptionAttribute)

    if axRole == kAXStaticTextRole {
        if title?.isEmpty != false { title = value }
        value = nil
    } else if title?.isEmpty != false,
              [
                  kAXButtonRole,
                  kAXGroupRole,
                  kAXImageRole,
                  kAXMenuBarItemRole,
                  "AXPopUpButton",
              ].contains(axRole) {
        title = description
        description = nil
    }

    title = renderedAccessibilityString(title)
    description = renderedAccessibilityString(description)
    value = renderedAccessibilityString(value)
    let placeholder = renderedAccessibilityString(
        accessibilityString(element, kAXPlaceholderValueAttribute)
    )
    let help = renderedAccessibilityString(accessibilityString(element, kAXHelpAttribute))
    var url = renderedAccessibilityString(accessibilityURLString(element, kAXURLAttribute))
    if url == nil, axRole == kAXWindowRole {
        url = firstAccessibilityURL(in: element, remainingDepth: 4)
    }

    var traits: [String] = []
    if accessibilityBoolean(element, kAXEnabledAttribute) == false {
        traits.append("disabled")
    }
    if accessibilityBoolean(element, kAXSelectedAttribute) == true {
        traits.append("selected")
    }
    let settableRoles = ["AXComboBox", "AXTab", "AXTextArea", "AXTextField"]
    var settable = DarwinBoolean(false)
    if settableRoles.contains(axRole),
       rawValue != nil,
       AXUIElementIsAttributeSettable(element, kAXValueAttribute as CFString, &settable) == .success,
       settable.boolValue {
        traits.append("settable")
        if let type = accessibilityValueType(rawValue) {
            traits.append(type)
        }
    }

    var detail = index
    if !role.isEmpty { detail += " \(role)" }
    if !traits.isEmpty { detail += " (\(traits.joined(separator: ", ")))" }
    if let title, !title.isEmpty { detail += " \(title)" }
    if let description, !description.isEmpty, description != title {
        detail += ", Description: \(description)"
    }
    if let url, !url.isEmpty {
        detail += axRole == "AXLink" ? ", Value: \(url)" : ", URL: \(url)"
    } else if let value, !value.isEmpty, value != title, value != description {
        detail += ", Value: \(value)"
    }
    if let placeholder, !placeholder.isEmpty {
        detail += ", Placeholder: \(placeholder)"
    }
    if let help, !help.isEmpty {
        detail += ", Help: \(help)"
    }
    let secondaryActions: [String]
    if axRole == kAXMenuBarRole || axRole == kAXMenuBarItemRole {
        secondaryActions = []
    } else {
        secondaryActions = ((try? accessibilityActions(element)) ?? [])
            .filter {
                $0 != kAXPressAction
                    && $0 != "AXShowMenu"
                    && $0 != "AXScrollToVisible"
            }
            .map(accessibilityActionName)
    }
    if !secondaryActions.isEmpty {
        detail += ", Secondary Actions: \(secondaryActions.joined(separator: ", "))"
    }
    return detail
}

private func firstAccessibilityURL(in element: AXUIElement, remainingDepth: Int) -> String? {
    guard remainingDepth >= 0 else { return nil }
    if let url = renderedAccessibilityString(accessibilityURLString(element, kAXURLAttribute)) {
        return url
    }
    for child in accessibilityChildren(element) {
        if let url = firstAccessibilityURL(in: child, remainingDepth: remainingDepth - 1) {
            return url
        }
    }
    return nil
}

private func renderedAccessibilityString(_ value: String?) -> String? {
    guard var value else { return nil }
    value = value.replacingOccurrences(of: "\r", with: "")
        .trimmingCharacters(in: .whitespacesAndNewlines)
    guard !value.isEmpty else { return nil }
    let limit = 1_000
    if value.count > limit {
        value = String(value.prefix(limit)) + "…"
    }
    return value
}

private func accessibilityValueType(_ value: AnyObject?) -> String? {
    guard let value else { return nil }
    if CFGetTypeID(value) == CFBooleanGetTypeID() { return "boolean" }
    if value is String || value is NSAttributedString { return "string" }
    if value is NSNumber { return "number" }
    return nil
}

private func semanticAccessibilityRole(
    role: String,
    subrole: String?,
    roleDescription: String?
) -> String {
    if roleDescription?.lowercased() == "column header" {
        return "column header"
    }
    switch subrole {
    case "AXStandardWindow": return "standard window"
    case "AXDialog", "AXSystemDialog": return "dialog"
    case "AXCloseButton": return "close button"
    case "AXMinimizeButton": return "minimize button"
    case "AXZoomButton", "AXFullScreenButton": return "full screen button"
    case "AXSearchField": return "text field"
    case "AXSecureTextField": return "secure text field"
    case "AXToggle", "AXSwitch": return "toggle button"
    case "AXColumnHeader": return "column header"
    default: break
    }
    switch role {
    case "AXApplication": return "application"
    case "AXBrowser", "AXGenericElement", "AXGroup": return "container"
    case "AXButton": return "button"
    case "AXCheckBox": return "toggle button"
    case "AXCell": return "cell"
    case "AXColumn": return "column"
    case "AXColumnHeader": return "column header"
    case "AXComboBox": return "combo box"
    case "AXDisclosureTriangle": return "disclosure triangle"
    case "AXHeading": return "heading"
    case "AXImage": return "image"
    case "AXLink": return "link"
    case "AXList": return "content list"
    case "AXMenu": return "menu"
    case "AXMenuBar": return "menu bar"
    case "AXMenuBarItem": return ""
    case "AXMenuButton": return "menu button"
    case "AXMenuItem": return "menu item"
    case "AXOutline": return "outline"
    case "AXPopUpButton": return "pop up button"
    case "AXProgressIndicator": return "progress indicator"
    case "AXRadioButton": return "radio button"
    case "AXRadioGroup": return "radio group"
    case "AXRow": return "row"
    case "AXScrollArea": return "scroll area"
    case "AXSheet": return "sheet"
    case "AXSlider": return "slider"
    case "AXSplitGroup": return "split group"
    case "AXStaticText": return "text"
    case "AXTab": return "tab"
    case "AXTabGroup": return "tab group"
    case "AXTable": return "table"
    case "AXTextArea": return "text area"
    case "AXTextField": return "text field"
    case "AXToolbar": return "toolbar"
    case "AXWebArea": return "HTML content"
    case "AXWindow": return "window"
    default: return humanizeAccessibilityIdentifier(role)
    }
}

private func accessibilityActionName(_ action: String) -> String {
    switch action {
    case "AXRaise": return "Raise"
    case "AXShowMenu": return "Show Menu"
    case "AXIncrement": return "Increment"
    case "AXDecrement": return "Decrement"
    case "AXConfirm": return "Confirm"
    case "AXCancel": return "Cancel"
    case "AXZoomWindow": return "zoom the window"
    default:
        let name = humanizeAccessibilityIdentifier(action)
        return name.prefix(1).uppercased() + name.dropFirst()
    }
}

private func humanizeAccessibilityIdentifier(_ identifier: String) -> String {
    let source = identifier.hasPrefix("AX") ? String(identifier.dropFirst(2)) : identifier
    var result = ""
    for character in source {
        if character.isUppercase, !result.isEmpty, result.last != " " {
            result.append(" ")
        }
        if character == "_" || character == "-" {
            if result.last != " " { result.append(" ") }
        } else {
            result.append(character.lowercased())
        }
    }
    return result.trimmingCharacters(in: .whitespaces)
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
        // "sh.waku",
        // "com.openai.codex",
        // "com.openai.sky.",
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
        // "com.openai.chat",
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
        sourceRect: CGRect,
        target: Target
    ) async throws {
        if let session = sessions[window.windowID], session.sourceRect == sourceRect {
            session.update(target: target)
            return
        }

        let staleSessions = sessions.values
        sessions.removeAll()
        for session in staleSessions {
            await session.stop()
        }
        let session = try WindowCaptureSession(
            window: window,
            filter: filter,
            sourceRect: sourceRect,
            target: target
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
    private let context = CIContext(options: [.cacheIntermediates: false])
    private let targetLock = NSLock()
    private var target: Target

    init(target: Target) {
        self.target = target
    }

    func update(target: Target) {
        targetLock.lock()
        self.target = target
        targetLock.unlock()
    }

    private func currentTarget() -> Target {
        targetLock.lock()
        defer { targetLock.unlock() }
        return target
    }

    func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {
        guard outputType == .screen,
              sampleBuffer.isValid,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(
                  sampleBuffer,
                  createIfNecessary: false
              ) as? [[SCStreamFrameInfo: Any]],
              let attachment = attachments.first,
              let statusRawValue = attachment[.status] as? Int,
              SCFrameStatus(rawValue: statusRawValue) == .complete,
              let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer)
        else {
            return
        }

        autoreleasepool {
            let image = CIImage(cvImageBuffer: pixelBuffer)
            let bounds = CGRect(
                x: 0,
                y: 0,
                width: CVPixelBufferGetWidth(pixelBuffer),
                height: CVPixelBufferGetHeight(pixelBuffer)
            )
            guard let frame = context.createCGImage(image, from: bounds) else {
                return
            }
            let target = currentTarget()
            let cursor = VirtualCursorStore.shared.position(
                for: target.windowId,
                size: CGSize(width: CGFloat(target.width), height: CGFloat(target.height))
            )
            let preview = drawVirtualCursor(
                on: frame,
                cursor: cursor,
                coordinateSpace: target
            )
            let previewTarget = Target(
                windowId: target.windowId,
                bundleId: target.bundleId,
                teamId: target.teamId,
                appName: target.appName,
                windowTitle: target.windowTitle,
                width: UInt32(preview.width),
                height: UInt32(preview.height)
            )
            ComputerUsePreviewStore.publish(target: previewTarget, image: preview)
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {}
}

private final class WindowCaptureSession {
    let sourceRect: CGRect
    private let output: WindowCaptureOutput
    private let outputQueue: DispatchQueue
    private let stream: SCStream
    private var capturing = false

    init(
        window: SCWindow,
        filter: SCContentFilter,
        sourceRect: CGRect,
        target: Target
    ) throws {
        self.sourceRect = sourceRect
        output = WindowCaptureOutput(target: target)
        let configuration = SCStreamConfiguration()
        let size = previewCaptureSize(for: sourceRect)
        configuration.sourceRect = sourceRect
        configuration.width = size.width
        configuration.height = size.height
        configuration.scalesToFit = true
        configuration.pixelFormat = kCVPixelFormatType_32BGRA
        configuration.queueDepth = 1
        configuration.minimumFrameInterval = CMTime(
            value: 1,
            timescale: previewFramesPerSecond
        )
        configuration.showsCursor = false
        configuration.capturesAudio = false
        outputQueue = DispatchQueue(label: "sh.waku.computer-use.capture.\(window.windowID)")
        stream = SCStream(filter: filter, configuration: configuration, delegate: output)
        try stream.addStreamOutput(output, type: .screen, sampleHandlerQueue: outputQueue)
    }

    func update(target: Target) {
        output.update(target: target)
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

private extension ComputerUsePreviewStore {
    static func publish(target: Target, image: CGImage) {
        let representation = NSBitmapImageRep(cgImage: image)
        guard let png = representation.representation(using: .png, properties: [:]) else {
            return
        }
        publish(
            target: target,
            capture: Capture(
                dataUrl: "data:image/png;base64," + png.base64EncodedString(),
                width: image.width,
                height: image.height
            )
        )
    }
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

private func previewCaptureSize(for frame: CGRect) -> (width: Int, height: Int) {
    let scale = min(
        1,
        min(Double(maximumPreviewWidth) / max(frame.width, 1), Double(maximumPreviewHeight) / max(frame.height, 1))
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
          let overlayCursor = CursorAssets.overlay,
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
    context.interpolationQuality = .high
    context.setShadow(
        offset: CGSize(width: 0, height: -1),
        blur: 2,
        color: CGColor(gray: 0, alpha: 0.55)
    )
    context.draw(
        overlayCursor,
        in: CGRect(x: -4, y: 7 - 32, width: 32, height: 32)
    )
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
    func withWindowRouting(_ operation: () throws -> Void) throws {
        try withInactiveWindowRouting(
            processID: processID,
            windowID: window.windowID,
            globalPoint: CGPoint(x: frame.midX, y: frame.midY),
            localPoint: CGPoint(x: frame.width / 2, y: frame.height / 2),
            operation
        )
    }

    for action in actions {
        switch action.type {
        case "click":
            updateVirtualCursor(action.x, action.y, window: window, coordinateSpace: coordinateSpace)
            let location = try point(action.x, action.y)
            try click(
                at: location,
                localPoint: CGPoint(x: location.x - frame.minX, y: location.y - frame.minY),
                count: 1,
                button: mouseButton(action.mouseButton),
                windowID: window.windowID,
                processID: processID
            )
        case "double_click":
            updateVirtualCursor(action.x, action.y, window: window, coordinateSpace: coordinateSpace)
            let location = try point(action.x, action.y)
            try click(
                at: location,
                localPoint: CGPoint(x: location.x - frame.minX, y: location.y - frame.minY),
                count: 2,
                button: mouseButton(action.mouseButton),
                windowID: window.windowID,
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
            let start = try point(action.x, action.y)
            let end = try point(action.toX, action.toY)
            try drag(
                from: start,
                to: end,
                fromLocal: CGPoint(x: start.x - frame.minX, y: start.y - frame.minY),
                toLocal: CGPoint(x: end.x - frame.minX, y: end.y - frame.minY),
                windowID: window.windowID,
                processID: processID
            )
        case "scroll":
            try withWindowRouting {
                try scroll(
                    deltaX: action.deltaX ?? 0,
                    deltaY: action.deltaY ?? 0,
                    processID: processID
                )
            }
        case "type":
            guard let text = action.text, text.utf8.count <= 10_000 else {
                throw HelperError.invalidRequest("Typed text is missing or too long")
            }
            try withWindowRouting {
                try typeText(text, processID: processID)
            }
        case "keypress":
            guard let key = action.key else {
                throw HelperError.invalidRequest("keypress requires a key")
            }
            try withWindowRouting {
                try pressKey(
                    key,
                    modifiers: action.modifiers ?? [],
                    processID: processID
                )
            }
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
    let cursor = VirtualCursorStore.shared.move(
        for: window.windowID,
        to: CGPoint(x: x, y: y),
        size: CGSize(width: CGFloat(coordinateSpace.width), height: CGFloat(coordinateSpace.height))
    )
    StatusAgentProcess.shared.showCursor(
        window: window,
        localPoint: cursor,
        coordinateSpace: coordinateSpace
    )
}

private enum WindowServerEventBridge {
    typealias SetWindowLocation = @convention(c) (UnsafeMutableRawPointer?, Double, Double) -> Void

    private static let handle = dlopen(
        "/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",
        RTLD_LAZY | RTLD_GLOBAL
    )

    private static func resolve<T>(_ name: String, as type: T.Type) -> T? {
        guard let symbol = name.withCString({ dlsym(handle, $0) }) else {
            return nil
        }
        return unsafeBitCast(symbol, to: type)
    }

    private static let setWindowLocation = resolve(
        "CGEventSetWindowLocation",
        as: SetWindowLocation.self
    )

    static func setLocalLocation(_ localPoint: CGPoint, on event: CGEvent) {
        let pointer = Unmanaged.passUnretained(event).toOpaque()
        setWindowLocation?(pointer, localPoint.x, localPoint.y)
    }
}

private enum TargetProcessInputRouting {
    private static let processNotificationType: UInt = 21
    private static let appKitDefinedType: UInt = 13
    private static let keyFocusReturnedSubtype: UInt16 = 0x8000
    private static let inactiveWindowActivatedSubtype: UInt16 = 1
    private static let appDeactivatedSubtype: UInt16 = 2
    private static let inactiveWindowModifiers = NSEvent.ModifierFlags(
        rawValue: 0xC0000
    )
    private static let lock = NSLock()
    private static var currentRoute: Route?

    private final class Route {
        let processID: pid_t
        let windowID: CGWindowID
        let focusStealSuppression: FocusStealSuppression

        init(
            processID: pid_t,
            windowID: CGWindowID,
            focusStealSuppression: FocusStealSuppression
        ) {
            self.processID = processID
            self.windowID = windowID
            self.focusStealSuppression = focusStealSuppression
        }
    }

    static func ensureInactiveWindowRouting(
        processID: pid_t,
        windowID: CGWindowID,
        activationPoint: CGPoint?,
        activationLocalPoint: CGPoint?,
        synthesizeActivationClick: Bool
    ) throws {
        lock.lock()
        defer { lock.unlock() }

        let frontmostProcessID = NSWorkspace.shared.frontmostApplication?.processIdentifier

        // Real user focus always wins. If the controlled app is genuinely
        // frontmost, it needs no synthetic route. Do not post a synthetic
        // deactivation here; that would fight the user's focus change.
        if frontmostProcessID == processID {
            currentRoute = nil
            return
        }

        if let currentRoute,
           currentRoute.processID == processID {
            return
        }

        // Sky retains one SyntheticAppFocusEnforcer for the selected process.
        // Arm the preventer before delivering either activation-shaped event;
        // the packets are not safe when sent outside this retained guard.
        deactivateCurrentRouteLocked()
        guard let suppression = FocusStealSuppression(
            targetProcessID: processID
        ) else {
            throw HelperError.eventCreationFailed
        }
        let route = Route(
            processID: processID,
            windowID: windowID,
            focusStealSuppression: suppression
        )
        currentRoute = route

        do {
            // This is Sky's exact inactive-window activation packet: an
            // AppKit-defined subtype-1 event carrying the real window number
            // and the private 0xC0000 inactive-window mask.
            try post(
                type: appKitDefinedType,
                subtype: inactiveWindowActivatedSubtype,
                windowNumber: Int(windowID),
                modifierFlags: inactiveWindowModifiers,
                to: processID
            )

            // Sky's activation bundle can include a left down/up at the
            // activation point. A background secondary click needs this so
            // AppKit first establishes the row under the pointer as the menu
            // owner instead of reusing the previously active row.
            if synthesizeActivationClick,
               let activationPoint,
               let activationLocalPoint {
                let eventNumber = SyntheticMouseEventNumbers.next()
                let down = try windowTargetedMouseEvent(
                    type: .leftMouseDown,
                    at: activationPoint,
                    localPoint: activationLocalPoint,
                    button: .left,
                    windowID: windowID,
                    eventNumber: eventNumber,
                    clickCount: 1,
                    pressure: 1
                )
                let up = try windowTargetedMouseEvent(
                    type: .leftMouseUp,
                    at: activationPoint,
                    localPoint: activationLocalPoint,
                    button: .left,
                    windowID: windowID,
                    eventNumber: eventNumber,
                    clickCount: 1,
                    pressure: 0
                )
                down.timestamp = CGEventTimestamp(mach_absolute_time())
                down.postToPid(processID)
                up.timestamp = CGEventTimestamp(mach_absolute_time())
                up.postToPid(processID)
            }

            // Sky updates the target's believed state in this order:
            // application active first, then key focus returned.
            try post(
                type: processNotificationType,
                subtype: keyFocusReturnedSubtype,
                windowNumber: 0,
                modifierFlags: [],
                to: processID
            )
        } catch {
            currentRoute = nil
            suppression.stop()
            throw error
        }
    }

    static func deactivateCurrentRoute() {
        lock.lock()
        defer { lock.unlock() }
        deactivateCurrentRouteLocked()
    }

    private static func deactivateCurrentRouteLocked() {
        guard let route = currentRoute else { return }
        currentRoute = nil

        // Do not send a synthetic deactivation over a real user activation.
        guard NSWorkspace.shared.frontmostApplication?.processIdentifier
            != route.processID else {
            return
        }
        try? post(
            type: appKitDefinedType,
            subtype: appDeactivatedSubtype,
            windowNumber: 0,
            modifierFlags: [],
            to: route.processID
        )
        route.focusStealSuppression.stop()
    }

    private static func post(
        type: UInt,
        subtype: UInt16,
        windowNumber: Int,
        modifierFlags: NSEvent.ModifierFlags,
        to processID: pid_t
    ) throws {
        guard let eventType = NSEvent.EventType(rawValue: type),
              let nsEvent = NSEvent.otherEvent(
                with: eventType,
                location: .zero,
                modifierFlags: modifierFlags,
                timestamp: 0,
                windowNumber: windowNumber,
                context: nil,
                subtype: Int16(bitPattern: subtype),
                data1: 0,
                data2: 0
              ), let event = nsEvent.cgEvent else {
            throw HelperError.eventCreationFailed
        }
        event.timestamp = CGEventTimestamp(mach_absolute_time())
        event.postToPid(processID)
    }
}

private final class FocusStealSuppressionState {
    let targetProcessID: pid_t
    let ready = DispatchSemaphore(value: 0)
    let stopped = DispatchSemaphore(value: 0)
    var eventTap: CFMachPort?
    var runLoop: CFRunLoop?

    init(targetProcessID: pid_t) {
        self.targetProcessID = targetProcessID
    }
}

private let focusStealSuppressionCallback: CGEventTapCallBack = {
    _, type, event, userInfo in
    guard let userInfo else {
        return Unmanaged.passUnretained(event)
    }
    let state = Unmanaged<FocusStealSuppressionState>
        .fromOpaque(userInfo)
        .takeUnretainedValue()

    if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
        if let eventTap = state.eventTap {
            CGEvent.tapEnable(tap: eventTap, enable: true)
        }
        return Unmanaged.passUnretained(event)
    }

    guard let targetProcessField = CGEventField(rawValue: 40),
          let sourceProcessField = CGEventField(rawValue: 41),
          let subjectProcessField = CGEventField(rawValue: 73) else {
        return Unmanaged.passUnretained(event)
    }
    let targetProcessID = pid_t(event.getIntegerValueField(targetProcessField))
    let sourceProcessID = pid_t(event.getIntegerValueField(sourceProcessField))
    let subjectProcessID = pid_t(event.getIntegerValueField(subjectProcessField))

    if type.rawValue == 32 {
        // Sky installs a second annotated-session tap for this private focus
        // theft event type whenever the target believes it is active but is
        // not really frontmost. Reject target-originated/target-directed
        // promotion while leaving unrelated user input untouched.
        if sourceProcessID == state.targetProcessID
            || targetProcessID == state.targetProcessID
            || subjectProcessID == state.targetProcessID {
            return nil
        }
        return Unmanaged.passUnretained(event)
    }

    guard type.rawValue == 21,
          let subtypeField = CGEventField(rawValue: 64) else {
        return Unmanaged.passUnretained(event)
    }
    let subtype = UInt16(
        truncatingIfNeeded: event.getIntegerValueField(subtypeField)
    )

    // Permit only the helper's synthetic KeyFocusReturned notification. Drop
    // a target-originated NewFront/key-focus request before WindowServer can
    // reorder either app. The target still receives the logical AppKit events
    // posted directly to its PID.
    if sourceProcessID == getpid() && subtype == 0x8000 {
        return Unmanaged.passUnretained(event)
    }
    if sourceProcessID == state.targetProcessID
        || targetProcessID == state.targetProcessID
        || subjectProcessID == state.targetProcessID {
        return nil
    }
    return Unmanaged.passUnretained(event)
}

private final class FocusStealSuppression {
    private let state: FocusStealSuppressionState

    init?(targetProcessID: pid_t) {
        let state = FocusStealSuppressionState(targetProcessID: targetProcessID)
        self.state = state
        Thread {
            let mask = (CGEventMask(1) << 21) | (CGEventMask(1) << 32)
            guard let eventTap = CGEvent.tapCreate(
                tap: .cgAnnotatedSessionEventTap,
                place: .headInsertEventTap,
                options: .defaultTap,
                eventsOfInterest: mask,
                callback: focusStealSuppressionCallback,
                userInfo: Unmanaged.passUnretained(state).toOpaque()
            ) else {
                state.ready.signal()
                state.stopped.signal()
                return
            }
            let runLoop = CFRunLoopGetCurrent()
            let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, eventTap, 0)
            state.eventTap = eventTap
            state.runLoop = runLoop
            CFRunLoopAddSource(runLoop, source, .commonModes)
            CGEvent.tapEnable(tap: eventTap, enable: true)
            state.ready.signal()
            CFRunLoopRun()
            CGEvent.tapEnable(tap: eventTap, enable: false)
            CFRunLoopRemoveSource(runLoop, source, .commonModes)
            CFMachPortInvalidate(eventTap)
            state.stopped.signal()
        }.start()

        guard state.ready.wait(timeout: .now() + 1) == .success,
              state.eventTap != nil else {
            return nil
        }
    }

    func stop() {
        guard let runLoop = state.runLoop else { return }
        CFRunLoopStop(runLoop)
        _ = state.stopped.wait(timeout: .now() + 1)
        state.runLoop = nil
        state.eventTap = nil
    }

    deinit {
        stop()
    }
}

private enum SyntheticMouseEventNumbers {
    private static let lock = NSLock()
    private static var value: Int64 = 0

    static func next() -> Int64 {
        lock.lock()
        defer { lock.unlock() }
        let result = value
        value &+= 1
        return result
    }
}

private func windowTargetedMouseEvent(
    type: NSEvent.EventType,
    at globalPoint: CGPoint,
    localPoint: CGPoint,
    button: CGMouseButton,
    windowID: CGWindowID,
    eventNumber: Int64,
    clickCount: Int,
    pressure: Float = 1
) throws -> CGEvent {
    // Match Codex's inactive-window pointer construction. Creating an NSEvent
    // with the real target window number supplies AppKit metadata that a raw
    // HID-state CGEvent lacks. TargetProcessInputRouting separately brackets
    // this pointer stream with Codex's guarded background markers. Do not add
    // an unguarded or empty-flags window activation here: that makes browser
    // chrome blink and can bring the controlled app to the front.
    guard let nsEvent = NSEvent.mouseEvent(
        with: type,
        location: globalPoint,
        modifierFlags: [],
        timestamp: 0,
        windowNumber: Int(windowID),
        context: nil,
        eventNumber: Int(eventNumber),
        clickCount: clickCount,
        pressure: pressure
    ), let event = nsEvent.cgEvent else {
        throw HelperError.eventCreationFailed
    }

    event.flags = []
    event.location = globalPoint
    event.setIntegerValueField(CGEventField(rawValue: 3)!, value: Int64(button.rawValue))
    event.setIntegerValueField(CGEventField(rawValue: 7)!, value: 3)
    let windowValue = Int64(windowID)
    event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: windowValue)
    event.setIntegerValueField(
        .mouseEventWindowUnderMousePointerThatCanHandleThisEvent,
        value: windowValue
    )
    WindowServerEventBridge.setLocalLocation(localPoint, on: event)
    return event
}

private func mouseEvent(type: CGEventType, at point: CGPoint, button: CGMouseButton) throws -> CGEvent {
    guard let source = CGEventSource(stateID: .hidSystemState),
          let event = CGEvent(
              mouseEventSource: source,
              mouseType: type,
              mouseCursorPosition: point,
              mouseButton: button
          ) else {
        throw HelperError.eventCreationFailed
    }
    return event
}

private func post(_ event: CGEvent, to processID: pid_t) {
    // Computer Use must never activate the controlled app or post pointer
    // input to a global event tap. PID-targeted delivery is what lets Waku
    // operate another app without stealing the user's focus.
    event.timestamp = CGEventTimestamp(mach_absolute_time())
    event.postToPid(processID)
}

private func mouseButton(_ value: String?) -> CGMouseButton {
    switch value?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
    case "r", "right", "secondary": return .right
    case "m", "middle", "center": return .center
    default: return .left
    }
}

private func withInactiveWindowRouting<T>(
    processID: pid_t,
    windowID: CGWindowID,
    globalPoint: CGPoint,
    localPoint: CGPoint,
    synthesizeActivationClick: Bool = false,
    _ operation: () throws -> T
) throws -> T {
    // The retained route owns the focus guard. Per-action AX focus writes and
    // per-action tap teardown are observably different from Sky and can both
    // dismiss transient menus.
    try TargetProcessInputRouting.ensureInactiveWindowRouting(
        processID: processID,
        windowID: windowID,
        activationPoint: globalPoint,
        activationLocalPoint: localPoint,
        synthesizeActivationClick: synthesizeActivationClick
    )
    return try operation()
}

private func click(
    at point: CGPoint,
    localPoint: CGPoint,
    count: Int64,
    button: CGMouseButton,
    windowID: CGWindowID,
    processID: pid_t
) throws {
    let downType: NSEvent.EventType
    let upType: NSEvent.EventType
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

    try withInactiveWindowRouting(
        processID: processID,
        windowID: windowID,
        globalPoint: point,
        localPoint: localPoint,
        synthesizeActivationClick: button == .right
    ) {
        for index in 1...count {
            let eventNumber = SyntheticMouseEventNumbers.next()
            let down = try windowTargetedMouseEvent(
                type: downType,
                at: point,
                localPoint: localPoint,
                button: button,
                windowID: windowID,
                eventNumber: eventNumber,
                clickCount: Int(index),
                pressure: 1
            )
            let up = try windowTargetedMouseEvent(
                type: upType,
                at: point,
                localPoint: localPoint,
                button: button,
                windowID: windowID,
                eventNumber: eventNumber,
                clickCount: Int(index),
                pressure: 0
            )
            post(down, to: processID)
            usleep(35_000)
            post(up, to: processID)
            usleep(70_000)
        }
    }
}

private func drag(
    from start: CGPoint,
    to end: CGPoint,
    fromLocal: CGPoint,
    toLocal: CGPoint,
    windowID: CGWindowID,
    processID: pid_t
) throws {
    // A drag must remain PID-scoped. The only activation-shaped events allowed
    // are the guarded, specially flagged markers in TargetProcessInputRouting.
    // Never replace them with real app activation, empty-flags window markers,
    // or CGEvent.post(tap:): those change the controlled app's visible active
    // state. Restoring focus later is not equivalent because the browser has
    // already blinked or moved front.
    try withInactiveWindowRouting(
        processID: processID,
        windowID: windowID,
        globalPoint: start,
        localPoint: fromLocal
    ) {
        // Codex uses one event number for down/up and a second shared number
        // for every dragged sample. Preserve that AppKit gesture identity.
        let clickEventNumber = SyntheticMouseEventNumbers.next()
        let dragEventNumber = SyntheticMouseEventNumbers.next()
        let down = try windowTargetedMouseEvent(
            type: .leftMouseDown,
            at: start,
            localPoint: fromLocal,
            button: .left,
            windowID: windowID,
            eventNumber: clickEventNumber,
            clickCount: 1
        )
        post(down, to: processID)

        let initialDrag = try windowTargetedMouseEvent(
            type: .leftMouseDragged,
            at: start,
            localPoint: fromLocal,
            button: .left,
            windowID: windowID,
            eventNumber: dragEventNumber,
            clickCount: 0
        )
        post(initialDrag, to: processID)

        for step in 1...12 {
            let progress = Double(step) / 12
            let point = CGPoint(
                x: start.x + (end.x - start.x) * progress,
                y: start.y + (end.y - start.y) * progress
            )
            let localPoint = CGPoint(
                x: fromLocal.x + (toLocal.x - fromLocal.x) * progress,
                y: fromLocal.y + (toLocal.y - fromLocal.y) * progress
            )
            let event = try windowTargetedMouseEvent(
                type: .leftMouseDragged,
                at: point,
                localPoint: localPoint,
                button: .left,
                windowID: windowID,
                eventNumber: dragEventNumber,
                clickCount: 0
            )
            post(event, to: processID)
        }

        let up = try windowTargetedMouseEvent(
            type: .leftMouseUp,
            at: end,
            localPoint: toLocal,
            button: .left,
            windowID: windowID,
            eventNumber: clickEventNumber,
            clickCount: 1
        )
        post(up, to: processID)
    }
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
