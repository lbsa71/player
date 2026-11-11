import Foundation

public protocol AccessTokenProvider: Sendable {
    func accessToken(rejectedToken: String?) async throws -> String
}
extension SpotifyAuth: AccessTokenProvider {}

private actor RequestGate {
    private var occupied = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    func acquire() async {
        if !occupied { occupied = true; return }
        await withCheckedContinuation { waiters.append($0) }
    }
    func release() {
        if waiters.isEmpty { occupied = false }
        else { waiters.removeFirst().resume() }
    }
}

public actor SpotifyClient {
    private let auth: any AccessTokenProvider
    private let http: any HTTPTransport
    private let gate = RequestGate()
    private let modeConfirmationAttempts: Int
    private let modeConfirmationDelay: @Sendable () async throws -> Void
    private var retryAfter = Date.distantPast
    public init(auth: any AccessTokenProvider, http: any HTTPTransport = SpotifyHTTP()) {
        self.auth = auth; self.http = http
        modeConfirmationAttempts = 11
        modeConfirmationDelay = { try await Task.sleep(for:.milliseconds(500)) }
    }
    // Internal injection keeps asynchronous propagation tests deterministic and fast.
    init(auth: any AccessTokenProvider, http: any HTTPTransport,
         modeConfirmationAttempts: Int, modeConfirmationDelay: @escaping @Sendable () async throws -> Void) {
        self.auth = auth; self.http = http
        self.modeConfirmationAttempts = max(1,min(modeConfirmationAttempts,11))
        self.modeConfirmationDelay = modeConfirmationDelay
    }

    public func devices() async throws -> [SpotifyDevice] {
        guard let data = try await request(path: "devices"),
              let response = try? JSONDecoder().decode(DeviceResponse.self, from: data) else { throw SpotifyFailure.invalidResponse }
        return response.devices
    }
    public func playback() async throws -> SpotifyPlayback? {
        guard let data = try await request(path: "") else { return nil }
        guard let playback = try? JSONDecoder().decode(SpotifyPlayback.self, from: data) else { throw SpotifyFailure.invalidResponse }
        return playback
    }
    public func command(_ command: TransportCommand, deviceID: String) async throws {
        guard !deviceID.isEmpty else { throw SpotifyFailure.noDevice }
        var path: String
        var method = "PUT"
        var query = [URLQueryItem(name: "device_id", value: deviceID)]
        var body: Data?
        switch command {
        case .play: path = "play"
        case .pause: path = "pause"
        case .next: path = "next"; method = "POST"
        case .previous: path = "previous"; method = "POST"
        case .seek(let position):
            guard position >= 0 else { throw SpotifyFailure.setup("Seek position cannot be negative.") }
            path = "seek"; query.append(URLQueryItem(name: "position_ms", value: String(position)))
        case .transfer:
            path = ""; query = []
            body = try JSONSerialization.data(withJSONObject: ["device_ids": [deviceID], "play": false])
        case .shuffleOff:
            path = "shuffle"; query.append(URLQueryItem(name: "state", value: "false"))
        case .repeatOff:
            path = "repeat"; query.append(URLQueryItem(name: "state", value: "off"))
        case .playlist(let input):
            let uri = try SpotifyPlaylist.uri(input)
            try await self.command(.shuffleOff, deviceID: deviceID)
            try await self.command(.repeatOff, deviceID: deviceID)
            // Commands can be applied asynchronously by Spotify. Observe the modes
            // before starting; do not claim ordered playback from HTTP success alone.
            try await confirmOrderedPlaybackModes(deviceID:deviceID)
            path = "play"
            body = try JSONSerialization.data(withJSONObject: ["context_uri": uri, "offset": ["position": 0], "position_ms": 0])
        }
        _ = try await request(path: path, method: method, query: query, body: body)
    }

    private func confirmOrderedPlaybackModes(deviceID: String) async throws {
        let clock = ContinuousClock(), deadline = ContinuousClock.now.advanced(by:.seconds(5))
        for attempt in 0..<modeConfirmationAttempts {
            try Task.checkCancellation()
            if attempt > 0 {
                guard clock.now < deadline else { break }
                try await modeConfirmationDelay()
                try Task.checkCancellation()
                guard clock.now < deadline else { break }
            }
            // Only observations repeat. HTTP/auth/rate-limit failures propagate;
            // no second shuffle, repeat, or play command is issued here.
            let observed = try await playback()
            try Task.checkCancellation()
            if observed?.device?.id == deviceID, observed?.shuffle_state == false, observed?.repeat_state == "off" {
                return
            }
        }
        throw SpotifyFailure.setup("Shuffle/repeat changes were not confirmed on this Mac. Check Spotify, then try Play playlist again.")
    }

    private func request(path: String, method: String = "GET", query: [URLQueryItem] = [], body: Data? = nil) async throws -> Data? {
        await gate.acquire()
        defer { Task { await gate.release() } }
        try Task.checkCancellation()
        guard Date() >= retryAfter else { throw SpotifyFailure.rateLimited(retryAfter) }
        var url = URLComponents(string: "https://api.spotify.com/v1/me/player" + (path.isEmpty ? "" : "/\(path)"))!
        url.queryItems = query.isEmpty ? nil : query
        var token = try await auth.accessToken(rejectedToken: nil)
        for attempt in 0...1 {
            var request = URLRequest(url: url.url!)
            request.httpMethod = method; request.httpBody = body
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            if body != nil { request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
            // Deliberately no network-error retries, especially for next/previous.
            let (data, response) = try await http.send(request)
            if response.statusCode == 401, attempt == 0 {
                token = try await auth.accessToken(rejectedToken: token)
                continue
            }
            switch response.statusCode {
            case 200...299: return response.statusCode == 204 || data.isEmpty ? nil : data
            case 401: throw SpotifyFailure.signedOut
            case 403: throw SpotifyFailure.denied
            case 404: throw SpotifyFailure.noDevice
            case 429:
                retryAfter = Self.retryDate(response)
                throw SpotifyFailure.rateLimited(retryAfter)
            default: throw SpotifyFailure.http(response.statusCode)
            }
        }
        throw SpotifyFailure.signedOut
    }
    static func retryDate(_ response: HTTPURLResponse, now: Date = Date()) -> Date {
        let header = response.value(forHTTPHeaderField: "Retry-After") ?? ""
        if let seconds = Double(header), seconds.isFinite { return now.addingTimeInterval(max(1, seconds)) }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.dateFormat = "EEE',' dd MMM yyyy HH':'mm':'ss z"
        if let date = formatter.date(from: header), date > now { return date }
        return now.addingTimeInterval(60)
    }
}
