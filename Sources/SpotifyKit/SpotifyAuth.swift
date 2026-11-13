import Foundation
import CryptoKit
import Security

public enum PKCE {
    public static func randomString() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw SpotifyFailure.setup("Could not generate secure sign-in randomness.")
        }
        return base64url(Data(bytes))
    }
    public static func challenge(_ verifier: String) -> String { base64url(Data(SHA256.hash(data: Data(verifier.utf8)))) }
    static func base64url(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
    static func form(_ values: [String: String]) -> Data {
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~")
        return Data(values.sorted { $0.key < $1.key }.map {
            "\($0.key.addingPercentEncoding(withAllowedCharacters: allowed)!)=\($0.value.addingPercentEncoding(withAllowedCharacters: allowed)!)"
        }.joined(separator: "&").utf8)
    }
}

public protocol RefreshTokenStore: Sendable {
    func read(clientID: String) throws -> String?
    func write(_ token: String, clientID: String) throws
    func remove(clientID: String) throws
}
public struct KeychainTokenStore: RefreshTokenStore {
    public init() {}
    private func query(_ clientID: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: "se.lbsa71.player.spotify.refresh",
         kSecAttrAccount as String: clientID]
    }
    public func read(clientID: String) throws -> String? {
        var q = query(clientID)
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(q as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data, let token = String(data: data, encoding: .utf8) else {
            throw SpotifyFailure.setup("Keychain access failed (\(status)). Unlock your login Keychain and try again.")
        }
        return token
    }
    public func write(_ token: String, clientID: String) throws {
        let q = query(clientID)
        let update = [kSecValueData as String: Data(token.utf8)]
        var status = SecItemUpdate(q as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var item = q.merging(update) { _, value in value }
            item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            status = SecItemAdd(item as CFDictionary, nil)
        }
        guard status == errSecSuccess else { throw SpotifyFailure.setup("Could not save the Spotify refresh token to Keychain (\(status)).") }
    }
    public func remove(clientID: String) throws {
        let status = SecItemDelete(query(clientID) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw SpotifyFailure.setup("Could not remove Spotify credentials from Keychain (\(status)).")
        }
    }
}

struct TokenResponse: Decodable {
    let access_token: String
    let refresh_token: String?
    let expires_in: Double
    let scope: String?
}
struct AccessCredential { let token: String; let expires: Date }

public actor SpotifyAuth {
    public static let redirectURI = "http://127.0.0.1:43871/callback"
    public static let scopes = "user-read-playback-state user-modify-playback-state"
    public let clientID: String
    private let http: any HTTPTransport
    private let store: any RefreshTokenStore
    private var credential: AccessCredential?
    private var refreshTask: Task<AccessCredential, Error>?
    private var login: LoopbackLogin?
    private var generation = 0
    private var retryAfter = Date.distantPast

    public init(clientID: String, http: any HTTPTransport = SpotifyHTTP(), store: any RefreshTokenStore = KeychainTokenStore()) {
        self.clientID = clientID; self.http = http; self.store = store
    }
    public func hasSavedLogin() throws -> Bool { try store.read(clientID: clientID) != nil }
    public func signIn(openBrowser: @escaping @Sendable (URL) -> Void) async throws {
        guard login == nil else { throw SpotifyFailure.setup("A Spotify sign-in is already in progress.") }
        guard clientID.count == 32, clientID.allSatisfy(\.isHexDigit) else {
            throw SpotifyFailure.setup("Enter the 32-character client ID from your Spotify developer app.")
        }
        let verifier = try PKCE.randomString(), state = try PKCE.randomString()
        let listener = LoopbackLogin(state: state)
        login = listener
        let attempt = generation
        defer { login = nil }
        var components = URLComponents(string: "https://accounts.spotify.com/authorize")!
        components.queryItems = [
            URLQueryItem(name: "client_id", value: clientID), URLQueryItem(name: "response_type", value: "code"),
            URLQueryItem(name: "redirect_uri", value: Self.redirectURI), URLQueryItem(name: "scope", value: Self.scopes + " playlist-read-private"),
            URLQueryItem(name: "state", value: state), URLQueryItem(name: "code_challenge_method", value: "S256"),
            URLQueryItem(name: "code_challenge", value: PKCE.challenge(verifier))
        ]
        let url = components.url!
        let code = try await listener.code { openBrowser(url) }
        let result = try await exchange(["grant_type": "authorization_code", "code": code,
            "redirect_uri": Self.redirectURI, "client_id": clientID, "code_verifier": verifier])
        guard attempt == generation else { throw SpotifyFailure.cancelled }
        guard let refresh = result.refresh_token else { throw SpotifyFailure.invalidResponse }
        try store.write(refresh, clientID: clientID)
        credential = AccessCredential(token: result.access_token, expires: Date().addingTimeInterval(result.expires_in))
    }
    public func cancelLogin() { login?.cancel() }
    public func signOut() throws {
        generation += 1
        login?.cancel(); refreshTask?.cancel(); refreshTask = nil; credential = nil
        try store.remove(clientID: clientID)
    }
    public func accessToken(rejectedToken: String? = nil) async throws -> String {
        if let credential, credential.expires.timeIntervalSinceNow > 60, credential.token != rejectedToken { return credential.token }
        if let refreshTask { return try await refreshTask.value.token }
        guard Date() >= retryAfter else { throw SpotifyFailure.rateLimited(retryAfter) }
        guard let refresh = try store.read(clientID: clientID) else { throw SpotifyFailure.signedOut }
        let attempt = generation
        let task = Task { () throws -> AccessCredential in
            let result = try await self.exchange(["grant_type": "refresh_token", "refresh_token": refresh, "client_id": self.clientID])
            return try self.acceptRefresh(result, generation: attempt)
        }
        refreshTask = task
        defer { refreshTask = nil }
        return try await task.value.token
    }
    private func acceptRefresh(_ result: TokenResponse, generation attempt: Int) throws -> AccessCredential {
        guard attempt == generation else { throw SpotifyFailure.cancelled }
        if let refresh = result.refresh_token { try store.write(refresh, clientID: clientID) }
        let value = AccessCredential(token: result.access_token, expires: Date().addingTimeInterval(result.expires_in))
        credential = value
        return value
    }
    private func exchange(_ values: [String: String]) async throws -> TokenResponse {
        guard Date() >= retryAfter else { throw SpotifyFailure.rateLimited(retryAfter) }
        var request = URLRequest(url: URL(string: "https://accounts.spotify.com/api/token")!)
        request.httpMethod = "POST"
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.httpBody = PKCE.form(values)
        let (data, response) = try await http.send(request)
        if response.statusCode == 429 {
            retryAfter = SpotifyClient.retryDate(response)
            throw SpotifyFailure.rateLimited(retryAfter)
        }
        if response.statusCode == 400 || response.statusCode == 401 {
            if values["grant_type"] == "refresh_token",
               (try? JSONSerialization.jsonObject(with: data) as? [String: Any])?["error"] as? String == "invalid_grant" {
                credential = nil
                try store.remove(clientID: clientID)
            }
            throw SpotifyFailure.signedOut
        }
        guard response.statusCode == 200 else { throw SpotifyFailure.http(response.statusCode) }
        guard let token = try? JSONDecoder().decode(TokenResponse.self, from: data),
              !token.access_token.isEmpty, token.expires_in > 0 else { throw SpotifyFailure.invalidResponse }
        if let scope = token.scope {
            let granted = Set(scope.split(separator: " "))
            guard Set(Self.scopes.split(separator: " ")).isSubset(of: granted) else { throw SpotifyFailure.denied }
        }
        return token
    }
}
