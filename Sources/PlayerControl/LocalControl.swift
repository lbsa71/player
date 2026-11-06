import Foundation
import Darwin

public enum ControlError: LocalizedError {
    case message(String)
    public var errorDescription: String? {
        switch self { case .message(let text): return text }
    }
}

public enum LocalControl {
    public static var directory: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Player", isDirectory: true)
    }
    public static var path: String { directory.appendingPathComponent("control.sock").path }
    private static let maxBytes = 65_536

    static func socketAddress(path: String = path) throws -> sockaddr_un {
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let bytes = path.utf8CString
        guard bytes.count <= MemoryLayout.size(ofValue: address.sun_path) else {
            throw ControlError.message("Local control socket path is too long.")
        }
        withUnsafeMutableBytes(of: &address.sun_path) { buffer in
            bytes.withUnsafeBytes { buffer.copyBytes(from: $0) }
        }
        return address
    }
    static func withAddress<T>(_ address: inout sockaddr_un, _ body: (UnsafePointer<sockaddr>, socklen_t) -> T) -> T {
        withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { body($0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
    }
    static func prepare(_ fd: Int32) {
        // Darwin accept() inherits O_NONBLOCK from the listening descriptor.
        // Requests run off the UI thread and use bounded blocking I/O.
        let flags = fcntl(fd, F_GETFL)
        if flags >= 0 { _ = fcntl(fd, F_SETFL, flags & ~O_NONBLOCK) }
        var enabled: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &enabled, socklen_t(MemoryLayout<Int32>.size))
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    }
    static func sameUser(_ fd: Int32) -> Bool {
        var uid: uid_t = 0
        var gid: gid_t = 0
        return getpeereid(fd, &uid, &gid) == 0 && uid == getuid()
    }
    static func readMessage(_ fd: Int32) throws -> [String: Any] {
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 4096)
        while data.count <= maxBytes {
            let count = recv(fd, &buffer, buffer.count, 0)
            guard count > 0 else { throw ControlError.message("Local control connection closed or timed out.") }
            if let end = buffer.prefix(count).firstIndex(of: 10) {
                data.append(contentsOf: buffer[..<end])
                guard data.count <= maxBytes,
                      let result = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                    throw ControlError.message("Invalid control request.")
                }
                return result
            }
            data.append(contentsOf: buffer.prefix(count))
        }
        throw ControlError.message("Control request exceeds 64 KiB.")
    }
    static func writeMessage(_ value: [String: Any], to fd: Int32) throws {
        var data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        data.append(10)
        guard data.count <= maxBytes else { throw ControlError.message("Control response exceeds 64 KiB.") }
        try data.withUnsafeBytes { bytes in
            var sent = 0
            while sent < bytes.count {
                let count = send(fd, bytes.baseAddress!.advanced(by: sent), bytes.count - sent, 0)
                guard count > 0 else { throw ControlError.message("Could not write local control response.") }
                sent += count
            }
        }
    }
    public static func request(_ method: String, arguments: [String: Any] = [:], directory: URL = directory) throws -> [String: Any] {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw ControlError.message("Could not create local socket.") }
        defer { close(fd) }
        prepare(fd)
        var address = try socketAddress(path: directory.appendingPathComponent("control.sock").path)
        guard withAddress(&address, { connect(fd, $0, $1) }) == 0 else {
            throw ControlError.message("The local audio service is not running at \(directory.path).")
        }
        guard sameUser(fd) else { throw ControlError.message("Local control peer belongs to another user.") }
        try writeMessage(["method": method, "arguments": arguments], to: fd)
        return try readMessage(fd)
    }
}

// Only lifecycle/control messages enter this server; PCM never crosses its boundary.
public final class LocalControlServer {
    private var source: DispatchSourceRead?
    private let acceptQueue = DispatchQueue(label: "se.lbsa71.player.control.accept")
    private let requestQueue = DispatchQueue(label: "se.lbsa71.player.control.requests")
    private var lockFD: Int32 = -1
    private var ownsPath = false
    private let directory: URL
    private var path: String { directory.appendingPathComponent("control.sock").path }
    public init(directory: URL = LocalControl.directory) { self.directory = directory }

    public func start(handler: @escaping ([String: Any], @escaping ([String: Any]) -> Void) -> Void) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
        guard chmod(directory.path, 0o700) == 0 else { throw ControlError.message("Could not secure control directory.") }
        let lockPath = directory.appendingPathComponent("control.lock").path
        lockFD = open(lockPath, O_CREAT | O_RDWR | O_NOFOLLOW, 0o600)
        guard lockFD >= 0, flock(lockFD, LOCK_EX | LOCK_NB) == 0 else {
            if lockFD >= 0 { close(lockFD); lockFD = -1 }
            throw ControlError.message("Another Player instance owns the local control socket.")
        }
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { stop(); throw ControlError.message("Could not create control listener.") }
        do {
            var address = try LocalControl.socketAddress(path: path)
            unlink(path)
            guard LocalControl.withAddress(&address, { bind(fd, $0, $1) }) == 0 else {
                throw ControlError.message("Could not bind local control socket.")
            }
            ownsPath = true
            guard chmod(path, 0o600) == 0, listen(fd, 4) == 0 else {
                throw ControlError.message("Could not secure/start local control listener.")
            }
            guard fcntl(fd, F_SETFL, O_NONBLOCK) == 0 else {
                throw ControlError.message("Could not make control listener nonblocking.")
            }
            let source = DispatchSource.makeReadSource(fileDescriptor: fd, queue: acceptQueue)
            source.setEventHandler { [requestQueue] in
                // One accepted connection at a time. Suspend accepting until it finishes,
                // keeping a stalled client bounded by the socket timeout and listen backlog.
                let client = accept(fd, nil, nil)
                guard client >= 0 else { return }
                source.suspend()
                requestQueue.async {
                    defer { close(client); source.resume() }
                    LocalControl.prepare(client)
                    guard LocalControl.sameUser(client) else { return }
                    do {
                        let request = try LocalControl.readMessage(client)
                        let completed = DispatchSemaphore(value: 0)
                        let reply = ReplyBox()
                        handler(request) { response in reply.set(response); completed.signal() }
                        guard completed.wait(timeout: .now() + 5) == .success else {
                            throw ControlError.message("Player control handler timed out.")
                        }
                        try LocalControl.writeMessage(reply.get(), to: client)
                    } catch {
                        try? LocalControl.writeMessage(["ok": false, "error": error.localizedDescription], to: client)
                    }
                }
            }
            source.setCancelHandler { close(fd) }
            self.source = source
            source.resume()
        } catch { close(fd); stop(); throw error }
    }
    public func stop() {
        source?.cancel(); source = nil
        if ownsPath { unlink(path); ownsPath = false }
        if lockFD >= 0 { flock(lockFD, LOCK_UN); close(lockFD); lockFD = -1 }
    }
    deinit { stop() }
}

private final class ReplyBox {
    private let lock = NSLock()
    private var value: [String: Any] = [:]
    func set(_ value: [String: Any]) { lock.lock(); self.value = value; lock.unlock() }
    func get() -> [String: Any] { lock.lock(); defer { lock.unlock() }; return value }
}
