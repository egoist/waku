import Foundation

// The released C ABI is Cua's native SDK boundary. One runtime belongs to
// each Waku helper connection; capture and input never cross into a Cua daemon.
final class CuaDriver: @unchecked Sendable {
    private var handle: OpaquePointer?
    private let lock = NSLock()
    private var operation: OpaquePointer?
    private var disconnected = false

    init(cursorEnabled: Bool = false) throws {
        var version = CuaDriverAbiVersion()
        version.struct_size = UInt32(MemoryLayout<CuaDriverAbiVersion>.size)
        guard cua_driver_abi_version_v1(&version) == 0,
              cua_driver_abi_is_compatible_v1(UInt16(CUA_DRIVER_ABI_MAJOR), UInt16(CUA_DRIVER_ABI_MINOR)) else {
            throw CuaError("This Cua Driver library is incompatible with Waku.")
        }
        var error = CuaDriverBuffer()
        defer { cua_driver_buffer_free_v1(&error) }
        let status = waku_cua_driver_create_v1(cursorEnabled, nil, 0, &handle, &error)
        guard status == 0 else { throw nativeError(status, error) }
    }

    deinit { cua_driver_destroy_v1(&handle) }

    func listTools() throws -> [String: Any] {
        var result = CuaDriverBuffer()
        var error = CuaDriverBuffer()
        defer {
            cua_driver_buffer_free_v1(&result)
            cua_driver_buffer_free_v1(&error)
        }
        let status = cua_driver_list_tools_json_v1(handle, &result, &error)
        guard status == 0 else { throw nativeError(status, error) }
        return try object(bufferData(result))
    }

    func call(_ name: String, arguments: [String: Any]) async throws -> [String: Any] {
        let name = Array(name.utf8)
        let arguments = try JSONSerialization.data(withJSONObject: arguments)
        let result = try await perform { callback, context, operation, error in
            name.withUnsafeBufferPointer { name in
                arguments.withUnsafeBytes { arguments in
                    cua_driver_invoke_v1(
                        self.handle, name.baseAddress, name.count,
                        arguments.baseAddress?.assumingMemoryBound(to: UInt8.self), arguments.count,
                        callback, context, operation, error
                    )
                }
            }
        }
        return try object(result)
    }

    // Socket EOF / cancellation must interrupt admitted work as well as stop
    // subsequent calls. Never retry an action whose completion is unknown.
    func disconnect() {
        lock.lock()
        defer { lock.unlock() }
        disconnected = true
        if let operation { cua_driver_operation_cancel_v1(operation) }
    }

    func shutdown() async throws {
        _ = try await perform(shutdown: true) { callback, context, operation, error in
            cua_driver_shutdown_v1(self.handle, callback, context, operation, error)
        }
    }

    private func perform(
        shutdown: Bool = false,
        _ start: (CuaDriverCompletionV1?, UnsafeMutableRawPointer?, UnsafeMutablePointer<OpaquePointer?>,
                  UnsafeMutablePointer<CuaDriverBuffer>) -> Int32
    ) async throws -> Data {
        defer { releaseOperation() }
        return try await withCheckedThrowingContinuation { continuation in
            lock.lock()
            defer { lock.unlock() }
            guard shutdown || !disconnected else {
                continuation.resume(throwing: CuaError("Computer Use connection closed; the action was not started."))
                return
            }
            let context = Unmanaged.passRetained(CuaCompletion(continuation)).toOpaque()
            var error = CuaDriverBuffer()
            defer { cua_driver_buffer_free_v1(&error) }
            let status = start(cuaCompletion, context, &operation, &error)
            if status != 0 {
                Unmanaged<CuaCompletion>.fromOpaque(context).takeRetainedValue()
                    .continuation.resume(throwing: nativeError(status, error))
            }
        }
    }

    private func releaseOperation() {
        lock.lock()
        defer { lock.unlock() }
        cua_driver_operation_release_v1(&operation)
    }
}

struct CuaError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

private final class CuaCompletion {
    let continuation: CheckedContinuation<Data, Error>
    init(_ continuation: CheckedContinuation<Data, Error>) { self.continuation = continuation }
}

private let cuaCompletion: CuaDriverCompletionV1 = { context, status, result, error in
    guard let context else { return }
    let completion = Unmanaged<CuaCompletion>.fromOpaque(context).takeRetainedValue()
    var result = result
    var error = error
    defer {
        cua_driver_buffer_free_v1(&result)
        cua_driver_buffer_free_v1(&error)
    }
    if status == 0 {
        completion.continuation.resume(returning: bufferData(result))
    } else {
        completion.continuation.resume(throwing: nativeError(status, error))
    }
}

private func bufferData(_ buffer: CuaDriverBuffer) -> Data {
    guard let data = buffer.data, buffer.len > 0 else { return Data() }
    return Data(bytes: data, count: buffer.len)
}

private func nativeError(_ status: Int32, _ buffer: CuaDriverBuffer) -> CuaError {
    let detail = String(decoding: bufferData(buffer), as: UTF8.self)
    return CuaError(detail.isEmpty ? "Cua Driver failed (status \(status))." : detail)
}

private func object(_ data: Data) throws -> [String: Any] {
    guard let value = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw CuaError("Cua Driver returned an invalid result.")
    }
    return value
}
