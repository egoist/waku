import AppKit
import ApplicationServices
import Darwin
import Foundation
import ScreenCaptureKit

// A thin, signed macOS host. Cua owns app discovery, accessibility, capture,
// input delivery, sessions, and native authorization. Waku owns TCC onboarding
// and the private connection to its JavaScript REPL.
private let helperDisplayName =
    (Bundle.main.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
    ?? "Waku Computer Use"

@main
struct WakuComputerUse {
    @MainActor
    static func main() {
        if CommandLine.arguments.contains("mcp-child") {
            do { try runNativeHost() }
            catch {
                FileHandle.standardError.write(Data("\(helperDisplayName): \(error.localizedDescription)\n".utf8))
                exit(1)
            }
            return
        }
        Task.detached {
            await runCommand()
            exit(0)
        }
        dispatchMain()
    }

    private static func runCommand() async {
        do {
            if CommandLine.arguments.contains("list-tools") {
                let driver = try CuaDriver()
                do {
                    let catalog = try driver.listTools()
                    try FileHandle.standardOutput.write(contentsOf: JSONSerialization.data(withJSONObject: catalog) + Data([10]))
                    try await driver.shutdown()
                } catch {
                    try? await driver.shutdown()
                    throw error
                }
            } else if CommandLine.arguments.contains("mcp") {
                try serveBridge(mcp: true)
            } else if CommandLine.arguments.contains("request-child") {
                let channel = try connectToBridge()
                defer { channel.closeFile() }
                guard try readFrame(from: channel) != nil else { return }
                let response = await permissions(prompt: CommandLine.arguments.contains("request-permissions"))
                try writeFrame(JSONSerialization.data(withJSONObject: response), to: channel)
            } else if CommandLine.arguments.contains("status") || CommandLine.arguments.contains("request-permissions") {
                try serveBridge(mcp: false)
            } else {
                throw CuaError("Expected mcp, status, or request-permissions.")
            }
        } catch {
            FileHandle.standardError.write(Data("\(helperDisplayName): \(error.localizedDescription)\n".utf8))
            exit(1)
        }
    }

    @MainActor
    private static func runNativeHost() throws {
        let channel = try connectToBridge()
        let driver = try CuaDriver(cursorEnabled: true)
        Task.detached {
            do {
                try await serveMCP(channel, driver: driver)
                channel.closeFile()
                exit(0)
            } catch {
                FileHandle.standardError.write(Data("\(helperDisplayName): \(error.localizedDescription)\n".utf8))
                exit(1)
            }
        }
        // Cua's own overlay must own the OS main thread; requests and native
        // actions run on workers. The overlay is a non-activating window.
        waku_cua_driver_run_cursor_v1()
        dispatchMain()
    }

    private static func connectToBridge() throws -> FileHandle {
        let (channel, peerPID) = try connectedChannel(at: commandLineArgument("--socket") ?? "")
        guard commandLineArgument("--bridge-pid") == String(peerPID) else {
            channel.closeFile()
            throw CuaError("The Computer Use bridge identity changed.")
        }
        return channel
    }

    private static func serveBridge(mcp: Bool) throws {
        let listener = try UnixListener()
        defer { listener.close() }
        let prompt = CommandLine.arguments.contains("request-permissions")
        var arguments = [
            mcp ? "mcp-child" : "request-child",
            "--socket", listener.path, "--bridge-pid", String(getpid()),
        ]
        if prompt { arguments.append("request-permissions") }
        if let directory = ProcessInfo.processInfo.environment["WAKU_COMPUTER_USE_PROCESS_DIRECTORY"], !directory.isEmpty {
            arguments.append(contentsOf: ["--process-directory", directory])
        }
        let launcher = try launchSelfThroughLaunchServices(arguments: arguments, background: !prompt)
        defer { if launcher.isRunning { launcher.terminate() } }
        let channel = try listener.accept()
        defer { channel.closeFile() }
        if !mcp {
            try writeFrame(FileHandle.standardInput.readDataToEndOfFile(), to: channel)
            guard let response = try readFrame(from: channel) else { throw CuaError("Permission helper disconnected.") }
            try FileHandle.standardOutput.write(contentsOf: response)
            return
        }
        let input = LineReader(.standardInput)
        let output = LineReader(channel)
        while let line = try input.next() {
            try channel.write(contentsOf: line + Data([10]))
            let message = try JSONSerialization.jsonObject(with: line) as? [String: Any]
            if message?["id"] != nil {
                guard let response = try output.next() else { throw CuaError("Cua Driver connection closed.") }
                try FileHandle.standardOutput.write(contentsOf: response + Data([10]))
            }
        }
    }

    private static func serveMCP(_ channel: FileHandle, driver: CuaDriver) async throws {
        let registration = registerBridge()
        defer { if let registration { try? FileManager.default.removeItem(at: registration) } }
        // Read independently of native work so closing the REPL or Stop can
        // cancel an in-flight operation immediately, without admitting a retry.
        let requests = AsyncThrowingStream<Data, Error> { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    let reader = LineReader(channel)
                    while let line = try reader.next() { continuation.yield(line) }
                    continuation.finish()
                } catch { continuation.finish(throwing: error) }
                driver.disconnect()
            }
        }
        do {
            for try await line in requests {
                guard let request = try JSONSerialization.jsonObject(with: line) as? [String: Any],
                      let id = request["id"], let method = request["method"] as? String else { continue }
                let response: [String: Any]
                do {
                    let result: [String: Any]
                    switch method {
                    case "initialize":
                        result = ["protocolVersion": "2025-06-18", "capabilities": ["tools": [:]],
                                  "serverInfo": ["name": "Waku Cua Driver", "version": "0.28.0"]]
                    case "tools/list": result = try driver.listTools()
                    case "tools/call":
                        guard let params = request["params"] as? [String: Any], let name = params["name"] as? String else {
                            throw CuaError("tools/call requires a name.")
                        }
                        result = try await driver.call(name, arguments: params["arguments"] as? [String: Any] ?? [:])
                        if name == "get_window_state" { publishPreview(result) }
                    case "ping": result = [:]
                    default: throw CuaError("Unsupported MCP method: \(method)")
                    }
                    response = ["jsonrpc": "2.0", "id": id, "result": result]
                } catch {
                    response = ["jsonrpc": "2.0", "id": id, "error": ["code": -32603, "message": error.localizedDescription]]
                }
                try channel.write(contentsOf: JSONSerialization.data(withJSONObject: response) + Data([10]))
            }
        } catch {
            driver.disconnect()
            try? await driver.shutdown()
            throw error
        }
        try await driver.shutdown()
    }

    @MainActor
    private static func permissions(prompt: Bool) async -> [String: Any] {
        if prompt {
            NSApplication.shared.setActivationPolicy(.accessory)
            _ = CGRequestScreenCaptureAccess()
            // Register the responsible helper with ScreenCaptureKit as well.
            _ = try? await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
            _ = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
        }
        return ["success": true, "permissions": ["screenRecording": CGPreflightScreenCaptureAccess(), "accessibility": AXIsProcessTrusted()]]
    }
}

// Preview the exact SDK observation without a second capture, image resize,
// or accessibility walk that could invalidate the agent's snapshot tokens.
private func publishPreview(_ result: [String: Any]) {
    guard result["isError"] as? Bool != true,
          let state = result["structuredContent"] as? [String: Any],
          state["screenshot_frame_valid"] as? Bool != false,
          let windowID = state["window_id"] as? UInt64,
          let width = state["screenshot_width"] as? UInt32,
          let height = state["screenshot_height"] as? UInt32,
          let content = result["content"] as? [[String: Any]],
          let image = content.first(where: { $0["type"] as? String == "image" && $0["mimeType"] as? String == "image/png" }),
          let data = image["data"] as? String,
          let directory = commandLineArgument("--process-directory") else { return }
    let pid = (state["pid"] as? NSNumber)?.int32Value ?? 0
    let bundleID = NSRunningApplication(processIdentifier: pid)?.bundleIdentifier ?? ""
    let update: [String: Any] = [
        "target": ["windowId": windowID, "bundleId": bundleID, "appName": state["app_name"] as? String ?? "App",
                   "windowTitle": state["window_title"] as? String ?? "", "width": width, "height": height],
        "imageUrl": "data:image/png;base64,\(data)",
    ]
    let destination = URL(fileURLWithPath: directory).appendingPathComponent("preview-\(windowID).json")
    if let payload = try? JSONSerialization.data(withJSONObject: update) { try? payload.write(to: destination, options: .atomic) }
}

private func registerBridge() -> URL? {
    guard let directory = commandLineArgument("--process-directory"), let pid = commandLineArgument("--bridge-pid") else { return nil }
    let path = URL(fileURLWithPath: directory).appendingPathComponent(pid)
    return FileManager.default.createFile(atPath: path.path, contents: Data()) ? path : nil
}

private func commandLineArgument(_ name: String) -> String? {
    guard let index = CommandLine.arguments.firstIndex(of: name), CommandLine.arguments.indices.contains(index + 1) else { return nil }
    return CommandLine.arguments[index + 1]
}

// Buffered reads matter: a window image is often several megabytes.
private final class LineReader {
    private let input: FileHandle
    private var buffer = Data()
    init(_ input: FileHandle) { self.input = input }
    func next() throws -> Data? {
        while true {
            if let newline = buffer.firstIndex(of: 10) {
                let line = Data(buffer[..<newline])
                buffer.removeSubrange(...newline)
                return line
            }
            guard buffer.count <= 32 * 1024 * 1024 else { throw CuaError("Computer Use message is too large.") }
            // read(upToCount:) can wait to fill a pipe buffer; POSIX read returns
            // as soon as the peer's complete short JSON-RPC request arrives.
            var bytes = [UInt8](repeating: 0, count: 65536)
            let count = Darwin.read(input.fileDescriptor, &bytes, bytes.count)
            if count < 0 {
                if errno == EINTR { continue }
                throw CuaError(String(cString: strerror(errno)))
            }
            if count == 0 { return nil }
            buffer.append(contentsOf: bytes.prefix(count))
        }
    }
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
        throw CuaError("request is too large")
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
            throw CuaError("the Waku connection closed mid-message")
        }
        data.append(chunk)
    }
    return data
}

private func writeFrame(_ payload: Data, to output: FileHandle) throws {
    guard let length = UInt32(exactly: payload.count) else {
        throw CuaError("response is too large")
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
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        path = directory.appendingPathComponent(UUID().uuidString).path
        descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else {
            throw CuaError(String(cString: strerror(errno)))
        }

        do {
            var address = sockaddr_un()
            let pathBytes = Array(path.utf8CString)
            guard pathBytes.count <= MemoryLayout.size(ofValue: address.sun_path) else {
                throw CuaError("socket path is too long")
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
                throw CuaError(String(cString: strerror(errno)))
            }
            guard Darwin.chmod(path, 0o600) == 0, Darwin.listen(descriptor, 1) == 0 else {
                throw CuaError(String(cString: strerror(errno)))
            }
        } catch {
            close()
            throw error
        }
    }

    func accept() throws -> FileHandle {
        var readiness = pollfd(fd: descriptor, events: Int16(POLLIN), revents: 0)
        guard Darwin.poll(&readiness, 1, 15_000) > 0 else {
            throw CuaError("The Computer Use helper did not connect within 15 seconds.")
        }
        let connection = Darwin.accept(descriptor, nil, nil)
        guard connection >= 0 else {
            throw CuaError(String(cString: strerror(errno)))
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
        throw CuaError(
            "could not resolve the Computer Use app bundle from \(executable.path)"
        )
    }
    return application
}

private func connectedChannel(at path: String) throws -> (FileHandle, pid_t) {
    let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
    guard descriptor >= 0 else {
        throw CuaError(String(cString: strerror(errno)))
    }

    do {
        var address = sockaddr_un()
        let pathBytes = Array(path.utf8CString)
        guard pathBytes.count <= MemoryLayout.size(ofValue: address.sun_path) else {
            throw CuaError("socket path is too long")
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
            throw CuaError(String(cString: strerror(errno)))
        }

        var peerPID: pid_t = 0
        var peerPIDSize = socklen_t(MemoryLayout.size(ofValue: peerPID))
        guard getsockopt(descriptor, SOL_LOCAL, LOCAL_PEERPID, &peerPID, &peerPIDSize) == 0 else {
            throw CuaError("could not identify the Waku process")
        }
        return (FileHandle(fileDescriptor: descriptor, closeOnDealloc: true), peerPID)
    } catch {
        Darwin.close(descriptor)
        throw error
    }
}
