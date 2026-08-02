import AppKit
import CoreGraphics
import CoreMedia
import Darwin
import Foundation
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
    let durationMs: UInt64?
}

struct Request: Decodable {
    let operation: String
    let target: Target?
    let actions: [Action]?
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
        case .targetUnavailable: "The selected app window is no longer available. Call list_targets again."
        case .targetIdentityChanged: "The selected window now belongs to a different signed app. Call list_targets again."
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
            let capture = try await capture(window, filter: filter, sourceRect: sourceRect)
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
}

private func socketPathArgument() -> String? {
    guard let index = CommandLine.arguments.firstIndex(of: "--socket"),
          CommandLine.arguments.indices.contains(index + 1) else {
        return nil
    }
    return CommandLine.arguments[index + 1]
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
    guard let length = UInt32(exactly: payload.count) else {
        throw HelperError.ipc("response is too large")
    }
    var bigEndianLength = length.bigEndian
    try withUnsafeBytes(of: &bigEndianLength) { header in
        try output.write(contentsOf: header)
    }
    try output.write(contentsOf: payload)
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
    try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
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
    return bundle.hasPrefix("codes.waku") || [
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
        "com.1password.1password",
        "com.1password.1password7",
        "com.bitwarden.desktop",
        "com.lastpass.lastpass",
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
    sourceRect: CGRect
) async throws -> Capture {
    let size = captureSize(for: sourceRect)
    let image: CGImage
    if #available(macOS 14.0, *) {
        let configuration = SCStreamConfiguration()
        configuration.sourceRect = sourceRect
        configuration.width = size.width
        configuration.height = size.height
        configuration.scalesToFit = true
        configuration.showsCursor = true
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
    let representation = NSBitmapImageRep(cgImage: image)
    guard let png = representation.representation(using: .png, properties: [:]) else {
        throw HelperError.captureFailed
    }
    return Capture(
        dataUrl: "data:image/png;base64," + png.base64EncodedString(),
        width: image.width,
        height: image.height
    )
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
            try click(at: point(action.x, action.y), count: 1, processID: processID)
        case "double_click":
            try click(at: point(action.x, action.y), count: 2, processID: processID)
        case "move":
            try post(
                mouseEvent(type: .mouseMoved, at: point(action.x, action.y), button: .left),
                to: processID
            )
        case "drag":
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

private func mouseEvent(type: CGEventType, at point: CGPoint, button: CGMouseButton) throws -> CGEvent {
    guard let event = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: point, mouseButton: button) else {
        throw HelperError.eventCreationFailed
    }
    return event
}

private func post(_ event: CGEvent, to processID: pid_t) {
    event.postToPid(processID)
}

private func click(at point: CGPoint, count: Int64, processID: pid_t) throws {
    for index in 1...count {
        let down = try mouseEvent(type: .leftMouseDown, at: point, button: .left)
        let up = try mouseEvent(type: .leftMouseUp, at: point, button: .left)
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

private func scroll(deltaX: Double, deltaY: Double, processID: pid_t) throws {
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
    return codes[key.lowercased()]
}

private extension Array where Element == UInt16 {
    func chunked(maxCount: Int) -> [[UInt16]] {
        guard !isEmpty else { return [] }
        return stride(from: 0, to: count, by: maxCount).map { start in
            Array(self[start..<Swift.min(start + maxCount, count)])
        }
    }
}
