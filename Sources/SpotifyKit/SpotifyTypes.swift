import Foundation

public enum SpotifyFailure: LocalizedError {
    case signedOut, denied, noDevice, invalidResponse, invalidCallback, cancelled
    case rateLimited(Date), http(Int), network(Int), setup(String)
    public var errorDescription: String? {
        switch self {
        case .signedOut: return "Connect Spotify to enable playback controls."
        case .denied: return "Spotify denied access (403). Check Premium, the app's allowed users, and Player API access."
        case .noDevice: return "Spotify device is unavailable. Refresh devices and select this Mac again."
        case .invalidResponse: return "Spotify returned an unexpected response. Playback has not been assumed to change."
        case .invalidCallback: return "The login callback did not match this sign-in attempt. Try connecting again."
        case .cancelled: return "Spotify sign-in was cancelled or expired."
        case .rateLimited(let until): return "Spotify requested a pause in API requests. Retry after \(until.formatted(date: .omitted, time: .standard))."
        case .http(let status): return "Spotify request failed (HTTP \(status))."
        case .network(let code): return "Spotify network request failed (\(code)). A transport command may have reached Spotify; check playback before repeating it."
        case .setup(let message): return message
        }
    }
}

public struct SpotifyDevice: Codable, Identifiable, Equatable, Sendable {
    public let id: String?
    public let name: String
    public let type: String
    public let is_active: Bool
    public let is_restricted: Bool
    public let volume_percent: Int?
}
public struct SpotifyImage: Codable, Sendable { public let url: String }
public struct SpotifyArtist: Codable, Sendable { public let name: String }
public struct SpotifyAlbum: Codable, Sendable { public let name: String?; public let images: [SpotifyImage]? }
public struct SpotifyShow: Codable, Sendable { public let name: String? }
public struct SpotifyItem: Codable, Sendable {
    public let id: String?
    public let name: String
    public let duration_ms: Int
    public let artists: [SpotifyArtist]?
    public let album: SpotifyAlbum?
    public let images: [SpotifyImage]?
    public let show: SpotifyShow?
    public let external_urls: [String: String]?
    public var subtitle: String { artists?.map(\.name).joined(separator: ", ") ?? show?.name ?? "Spotify" }
    public var artworkURL: URL? { (album?.images ?? images)?.first.flatMap { URL(string: $0.url) } }
}
public struct SpotifyPlayback: Codable, Sendable {
    public let device: SpotifyDevice?
    public let is_playing: Bool
    public let progress_ms: Int?
    public let item: SpotifyItem?
    public let currently_playing_type: String?
    public let context: SpotifyContext?
    public let shuffle_state: Bool?
    public let repeat_state: String?
}
public struct SpotifyContext: Codable, Sendable { public let uri: String? }
struct DeviceResponse: Decodable { let devices: [SpotifyDevice] }

public enum TransportCommand: Sendable {
    case play, pause, next, previous, seek(Int), transfer, playlist(String), shuffleOff, repeatOff
}

public enum SpotifyPlaylist {
    public static func uri(_ input: String) throws -> String {
        let text = input.trimmingCharacters(in: .whitespacesAndNewlines)
        var id: String?
        if text.hasPrefix("spotify:playlist:") { id = String(text.dropFirst("spotify:playlist:".count)) }
        else if let url = URLComponents(string: text), url.scheme == "https", url.host == "open.spotify.com",
                url.user == nil, url.password == nil, url.port == nil || url.port == 443 {
            let parts = url.path.split(separator: "/")
            if parts.count == 2, parts[0] == "playlist" { id = String(parts[1]) }
            else if parts.count == 3, parts[0].hasPrefix("intl-"), parts[1] == "playlist" { id = String(parts[2]) }
        }
        guard let id, id.utf8.count == 22, id.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) }) else {
            throw SpotifyFailure.setup("Paste a Spotify playlist URL or spotify:playlist: URI.")
        }
        return "spotify:playlist:" + id
    }
}

public protocol HTTPTransport: Sendable {
    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse)
}
public final class SpotifyHTTP: NSObject, HTTPTransport, URLSessionTaskDelegate, @unchecked Sendable {
    private lazy var session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 12
        configuration.timeoutIntervalForResource = 20
        configuration.urlCache = nil
        configuration.httpCookieStorage = nil
        return URLSession(configuration: configuration, delegate: self, delegateQueue: nil)
    }()
    public func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        do {
            let (data, response) = try await session.data(for: request)
            guard let response = response as? HTTPURLResponse else { throw SpotifyFailure.invalidResponse }
            return (data, response)
        } catch let error as URLError { throw SpotifyFailure.network(error.code.rawValue) }
    }
    // Never forward bearer credentials to a redirected host or URL.
    public func urlSession(_ session: URLSession, task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}
