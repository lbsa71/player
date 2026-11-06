import Foundation
import Network

// An ephemeral HTTP listener bound only to 127.0.0.1. No auth codes are logged or stored.
final class LoopbackLogin: @unchecked Sendable {
    private let queue = DispatchQueue(label: "se.lbsa71.player.oauth-callback")
    private let state: String
    private var listener: NWListener?
    private var continuation: CheckedContinuation<String, Error>?
    private var connections: [UUID: NWConnection] = [:]
    private var cancelled = false

    init(state: String) { self.state = state }

    func code(openBrowser: @escaping @Sendable () -> Void) async throws -> String {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                queue.async { [self] in
                    self.continuation = continuation
                    guard !cancelled else { finish(.failure(SpotifyFailure.cancelled)); return }
                    do {
                        let parameters = NWParameters.tcp
                        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: 43871)
                        parameters.allowLocalEndpointReuse = false
                        let listener = try NWListener(using: parameters)
                        self.listener = listener
                        listener.stateUpdateHandler = { [weak self] status in
                            guard let self else { return }
                            switch status {
                            case .ready: openBrowser()
                            case .failed: self.finish(.failure(SpotifyFailure.setup("Cannot open the sign-in callback on 127.0.0.1:43871. Another app may be using that port.")))
                            default: break
                            }
                        }
                        listener.newConnectionHandler = { [weak self] connection in self?.accept(connection) }
                        listener.start(queue: queue)
                        queue.asyncAfter(deadline: .now() + 180) { [weak self] in self?.finish(.failure(SpotifyFailure.cancelled)) }
                    } catch { finish(.failure(SpotifyFailure.setup("Could not create the local Spotify sign-in listener."))) }
                }
            }
        } onCancel: { self.cancel() }
    }
    func cancel() { queue.async { [self] in cancelled = true; finish(.failure(SpotifyFailure.cancelled)) } }

    private func accept(_ connection: NWConnection) {
        guard continuation != nil, connections.count < 4 else { connection.cancel(); return }
        let id = UUID()
        connections[id] = connection
        connection.start(queue: queue)
        queue.asyncAfter(deadline: .now() + 5) { [weak self] in
            self?.connections.removeValue(forKey: id)?.cancel()
        }
        receive(connection, id: id, data: Data())
    }
    private func receive(_ connection: NWConnection, id: UUID, data previous: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 4096) { [weak self] bytes, _, complete, error in
            guard let self, self.connections[id] != nil else { return }
            var data = previous
            if let bytes { data.append(bytes) }
            guard data.count <= 16384, error == nil else { self.drop(id); return }
            if let request = String(data: data, encoding: .utf8), request.contains("\r\n\r\n") {
                do {
                    guard let code = try Self.parseCallback(request, state: self.state) else {
                        self.respond(connection, id: id, status: "404 Not Found", text: "Not found.", result: nil)
                        return
                    }
                    self.respond(connection, id: id, status: "200 OK", text: "Spotify sign-in received. Return to Player to finish connecting.", result: .success(code))
                } catch {
                    self.respond(connection, id: id, status: "400 Bad Request", text: "Sign-in could not be completed. Return to Player and try again.", result: .failure(error))
                }
            } else if complete { self.drop(id) }
            else { self.receive(connection, id: id, data: data) }
        }
    }
    static func parseCallback(_ request: String, state: String) throws -> String? {
        let lines = request.components(separatedBy: "\r\n")
        let parts = (lines.first ?? "").split(separator: " ")
        guard parts.count == 3, parts[0] == "GET", parts[2] == "HTTP/1.1", parts[1].hasPrefix("/") else {
            throw SpotifyFailure.invalidCallback
        }
        let hosts = lines.filter { $0.lowercased().hasPrefix("host:") }
        guard hosts.count == 1,
              hosts[0].dropFirst(5).trimmingCharacters(in: .whitespaces) == "127.0.0.1:43871",
              let url = URLComponents(string: "http://127.0.0.1:43871\(parts[1])") else { throw SpotifyFailure.invalidCallback }
        guard url.path == "/callback" else { return nil }
        let items = url.queryItems ?? []
        let states = items.filter { $0.name == "state" }
        guard states.count == 1, states[0].value == state else { throw SpotifyFailure.invalidCallback }
        if items.contains(where: { $0.name == "error" }) { throw SpotifyFailure.cancelled }
        let codes = items.filter { $0.name == "code" }
        guard codes.count == 1, let code = codes[0].value, !code.isEmpty else { throw SpotifyFailure.invalidCallback }
        return code
    }
    private func respond(_ connection: NWConnection, id: UUID, status: String, text: String, result: Result<String, Error>?) {
        let body = Data(text.utf8)
        var data = Data("HTTP/1.1 \(status)\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: \(body.count)\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n".utf8)
        data.append(body)
        connection.send(content: data, completion: .contentProcessed { [weak self] _ in
            self?.drop(id)
            if let result { self?.finish(result) }
        })
    }
    private func drop(_ id: UUID) { connections.removeValue(forKey: id)?.cancel() }
    private func finish(_ result: Result<String, Error>) {
        listener?.cancel(); listener = nil
        connections.values.forEach { $0.cancel() }; connections.removeAll()
        let completion = continuation; continuation = nil
        completion?.resume(with: result)
    }
}
