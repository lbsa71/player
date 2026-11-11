import XCTest
@testable import SpotifyKit

private actor ScriptedHTTP: HTTPTransport {
    struct Reply: Sendable { var code: Int; var body = ""; var headers: [String: String] = [:] }
    var replies: [Reply]
    var requests: [URLRequest] = []
    var active = 0
    var maximumActive = 0
    var failNetwork = false
    init(_ replies: [Reply], failNetwork: Bool = false) { self.replies = replies; self.failNetwork = failNetwork }
    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        requests.append(request); active += 1; maximumActive = max(active, maximumActive)
        defer { active -= 1 }
        try await Task.sleep(for: .milliseconds(20))
        if failNetwork { throw SpotifyFailure.network(-1001) }
        guard !replies.isEmpty else { throw SpotifyFailure.invalidResponse }
        let reply = replies.removeFirst()
        return (Data(reply.body.utf8), HTTPURLResponse(url: request.url!, statusCode: reply.code, httpVersion: nil, headerFields: reply.headers)!)
    }
}
private actor TestTokens: AccessTokenProvider {
    var rejected: [String?] = []
    func accessToken(rejectedToken: String?) async throws -> String {
        rejected.append(rejectedToken)
        return rejectedToken == nil ? "old-token" : "new-token"
    }
}
private final class MemoryStore: RefreshTokenStore, @unchecked Sendable {
    private let lock = NSLock()
    private var token: String?
    init(_ token: String? = "refresh") { self.token = token }
    func read(clientID: String) throws -> String? { lock.lock(); defer { lock.unlock() }; return token }
    func write(_ token: String, clientID: String) throws { lock.lock(); defer { lock.unlock() }; self.token = token }
    func remove(clientID: String) throws { lock.lock(); defer { lock.unlock() }; token = nil }
}

final class SpotifyTests: XCTestCase {
    private func playbackModes(deviceID:String = "local", shuffle:Bool = false, repeatMode:String = "off") -> String {
        #"{"device":{"id":"\#(deviceID)","name":"Mac","type":"Computer","is_active":true,"is_restricted":false},"is_playing":false,"shuffle_state":\#(shuffle),"repeat_state":"\#(repeatMode)"}"#
    }
    private func pollingClient(_ http: ScriptedHTTP, attempts:Int = 3) -> SpotifyClient {
        SpotifyClient(auth:TestTokens(),http:http,modeConfirmationAttempts:attempts,
                      modeConfirmationDelay:{ try Task.checkCancellation() })
    }
    func testPlaylistLinksAreNormalizedAndRejectOtherDestinations() throws {
        let id = "37i9dQZF1DXcBWIGoYBM5M"
        XCTAssertEqual(try SpotifyPlaylist.uri("https://open.spotify.com/playlist/\(id)?si=test"), "spotify:playlist:\(id)")
        XCTAssertEqual(try SpotifyPlaylist.uri("spotify:playlist:\(id)"), "spotify:playlist:\(id)")
        XCTAssertThrowsError(try SpotifyPlaylist.uri("https://example.com/playlist/\(id)"))
        XCTAssertThrowsError(try SpotifyPlaylist.uri("spotify:track:\(id)"))
        XCTAssertThrowsError(try SpotifyPlaylist.uri("spotify:playlist:../../test"))
    }
    func testPlaylistStartsFromBeginningOnlyAfterModesAreObserved() async throws {
        let state = playbackModes()
        let http = ScriptedHTTP([.init(code: 204), .init(code: 204), .init(code: 200, body: state), .init(code: 204)])
        let client = SpotifyClient(auth: TestTokens(), http: http)
        try await client.command(.playlist("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"), deviceID: "local")
        let requests = await http.requests
        XCTAssertEqual(requests.map { $0.url!.lastPathComponent }, ["shuffle", "repeat", "player", "play"])
        let body = try XCTUnwrap(try JSONSerialization.jsonObject(with: requests[3].httpBody!) as? [String: Any])
        XCTAssertEqual(body["context_uri"] as? String, "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M")
        XCTAssertEqual((body["offset"] as? [String: Int])?["position"], 0)
        XCTAssertEqual(body["position_ms"] as? Int, 0)
    }
    func testPlaylistDoesNotStartWhenPlaybackModesAreUnconfirmed() async {
        let http = ScriptedHTTP([.init(code: 204), .init(code: 204), .init(code: 204),
                                 .init(code:200,body:playbackModes(shuffle:true)),
                                 .init(code:200,body:playbackModes(deviceID:"another-device"))])
        let client = pollingClient(http)
        do { try await client.command(.playlist("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"), deviceID: "local"); XCTFail("Expected unconfirmed mode error") }
        catch SpotifyFailure.setup {} catch { XCTFail("Unexpected error: \(error)") }
        let requests = await http.requests
        XCTAssertFalse(requests.contains { $0.url!.lastPathComponent == "play" })
        XCTAssertEqual(requests.map { $0.url!.lastPathComponent },["shuffle","repeat","player","player","player"])
    }
    func testPlaylistWaitsForEventualModesWithoutRepeatingControlCommands() async throws {
        let http = ScriptedHTTP([.init(code:204),.init(code:204),
                                .init(code:200,body:playbackModes(shuffle:true,repeatMode:"context")),
                                .init(code:200,body:playbackModes(repeatMode:"context")),
                                .init(code:200,body:playbackModes()),.init(code:204)])
        let client = pollingClient(http)
        try await client.command(.playlist("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"),deviceID:"local")
        let requests = await http.requests
        XCTAssertEqual(requests.map { $0.url!.lastPathComponent },["shuffle","repeat","player","player","player","play"])
        XCTAssertEqual(requests.filter { $0.httpMethod != "GET" }.count,3)
    }
    func testPlaylistConfirmationCancellationStopsBeforeAnotherObservationOrPlay() async {
        let http = ScriptedHTTP([.init(code:204),.init(code:204),.init(code:204)])
        let delayStarted = expectation(description:"Waiting for Spotify propagation")
        let client = SpotifyClient(auth:TestTokens(),http:http,modeConfirmationAttempts:3,modeConfirmationDelay:{
            delayStarted.fulfill()
            try await Task.sleep(for:.seconds(60))
        })
        let task = Task { try await client.command(.playlist("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"),deviceID:"local") }
        await fulfillment(of:[delayStarted],timeout:2)
        task.cancel()
        do { try await task.value; XCTFail("Expected cancellation") }
        catch is CancellationError {} catch { XCTFail("Unexpected error: \(error)") }
        let requests = await http.requests
        XCTAssertEqual(requests.map { $0.url!.lastPathComponent },["shuffle","repeat","player"])
    }
    func testPlaylistConfirmation403StopsWithoutRetryOrPlay() async {
        let http = ScriptedHTTP([.init(code:204),.init(code:204),.init(code:403)])
        let client = pollingClient(http)
        do { try await client.command(.playlist("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"),deviceID:"local"); XCTFail("Expected denial") }
        catch SpotifyFailure.denied {} catch { XCTFail("Unexpected error: \(error)") }
        let requests = await http.requests
        XCTAssertEqual(requests.map { $0.url!.lastPathComponent },["shuffle","repeat","player"])
    }
    func testPlaylistConfirmationRateLimitPreservesRequestGate() async {
        let http = ScriptedHTTP([.init(code:204),.init(code:204),.init(code:429,headers:["Retry-After":"120"])])
        let client = pollingClient(http)
        do { try await client.command(.playlist("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"),deviceID:"local"); XCTFail("Expected rate limit") }
        catch SpotifyFailure.rateLimited {} catch { XCTFail("Unexpected error: \(error)") }
        do { _ = try await client.playback(); XCTFail("Expected rate-limit gate") }
        catch SpotifyFailure.rateLimited {} catch { XCTFail("Unexpected error: \(error)") }
        let count = await http.requests.count
        XCTAssertEqual(count,3)
    }
    func testPlaylistConfirmation401RefreshesObservationOnly() async throws {
        let http = ScriptedHTTP([.init(code:204),.init(code:204),.init(code:401),
                                .init(code:200,body:playbackModes()),.init(code:204)])
        let client = pollingClient(http)
        try await client.command(.playlist("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"),deviceID:"local")
        let requests = await http.requests
        XCTAssertEqual(requests.map { $0.url!.lastPathComponent },["shuffle","repeat","player","player","play"])
        XCTAssertEqual(requests[3].value(forHTTPHeaderField:"Authorization"),"Bearer new-token")
    }
    func testPKCEAgainstRFC7636Vector() throws {
        XCTAssertEqual(PKCE.challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
                       "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
        let first = try PKCE.randomString(), second = try PKCE.randomString()
        XCTAssertEqual(first.count, 43); XCTAssertNotEqual(first, second)
        XCTAssertFalse(first.contains("="))
    }
    func testCallbackValidatesHostStateAndDuplicates() throws {
        func request(_ query: String, host: String = "127.0.0.1:43871") -> String {
            "GET /callback?\(query) HTTP/1.1\r\nHost: \(host)\r\n\r\n"
        }
        XCTAssertEqual(try LoopbackLogin.parseCallback(request("state=expected&code=abc%2B123"), state: "expected"), "abc+123")
        XCTAssertThrowsError(try LoopbackLogin.parseCallback(request("state=wrong&code=abc"), state: "expected"))
        XCTAssertThrowsError(try LoopbackLogin.parseCallback(request("state=expected&state=expected&code=abc"), state: "expected"))
        XCTAssertThrowsError(try LoopbackLogin.parseCallback(request("state=expected&code=abc", host: "example.com"), state: "expected"))
        XCTAssertThrowsError(try LoopbackLogin.parseCallback(request("state=expected&error=access_denied"), state: "expected"))
    }
    func testFormEncodingPreservesSpecialCharacters() {
        XCTAssertEqual(String(decoding: PKCE.form(["code": "a+b&c= d"]), as: UTF8.self), "code=a%2Bb%26c%3D%20d")
    }
    func test401RefreshesOnceAndRetriesRejectedRequest() async throws {
        let http = ScriptedHTTP([.init(code: 401), .init(code: 204)])
        let client = SpotifyClient(auth: TestTokens(), http: http)
        try await client.command(.next, deviceID: "local")
        let requests = await http.requests
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(requests[0].value(forHTTPHeaderField: "Authorization"), "Bearer old-token")
        XCTAssertEqual(requests[1].value(forHTTPHeaderField: "Authorization"), "Bearer new-token")
    }
    func testRepeated401StopsAfterSingleRefresh() async {
        let http = ScriptedHTTP([.init(code: 401), .init(code: 401), .init(code: 204)])
        let client = SpotifyClient(auth: TestTokens(), http: http)
        do { try await client.command(.play, deviceID: "local"); XCTFail("Expected failure") } catch {}
        let count = await http.requests.count
        XCTAssertEqual(count, 2)
    }
    func testRateLimitBlocksFollowingRequestsUntilRetryAfter() async {
        let http = ScriptedHTTP([.init(code: 429, headers: ["Retry-After": "120"])])
        let client = SpotifyClient(auth: TestTokens(), http: http)
        for _ in 0..<2 {
            do { _ = try await client.playback(); XCTFail("Expected 429") }
            catch SpotifyFailure.rateLimited(let date) { XCTAssertGreaterThan(date.timeIntervalSinceNow, 110) }
            catch { XCTFail("Unexpected error") }
        }
        let count = await http.requests.count
        XCTAssertEqual(count, 1)
    }
    func testAmbiguousNextTimeoutIsNeverRetried() async {
        let http = ScriptedHTTP([], failNetwork: true)
        let client = SpotifyClient(auth: TestTokens(), http: http)
        do { try await client.command(.next, deviceID: "local"); XCTFail("Expected timeout") } catch {}
        let count = await http.requests.count
        XCTAssertEqual(count, 1)
    }
    func testTransportRequestsDoNotOverlap() async throws {
        let http = ScriptedHTTP([.init(code: 204), .init(code: 204), .init(code: 204)])
        let client = SpotifyClient(auth: TestTokens(), http: http)
        async let one: Void = client.command(.play, deviceID: "local")
        async let two: Void = client.command(.pause, deviceID: "local")
        async let three: Void = client.command(.seek(1200), deviceID: "local")
        _ = try await (one, two, three)
        let maximum = await http.maximumActive
        XCTAssertEqual(maximum, 1)
    }
    func testRefreshCoalescesAndPreservesOldRefreshTokenWhenOmitted() async throws {
        let http = ScriptedHTTP([.init(code: 200, body: "{\"access_token\":\"access\",\"expires_in\":3600}")])
        let store = MemoryStore()
        let auth = SpotifyAuth(clientID: "test", http: http, store: store)
        async let first = auth.accessToken()
        async let second = auth.accessToken()
        let values = try await [first, second]
        XCTAssertEqual(values, ["access", "access"])
        let count = await http.requests.count
        XCTAssertEqual(count, 1)
        XCTAssertEqual(try store.read(clientID: "test"), "refresh")
    }
    func testInvalidGrantDiscardsRefreshToken() async {
        let http = ScriptedHTTP([.init(code: 400, body: "{\"error\":\"invalid_grant\"}")])
        let store = MemoryStore()
        let auth = SpotifyAuth(clientID: "test", http: http, store: store)
        do { _ = try await auth.accessToken(); XCTFail("Expected sign-in required") } catch {}
        XCTAssertNil(try store.read(clientID: "test"))
    }
    func testPlayback204MeansNoActivePlayback() async throws {
        let client = SpotifyClient(auth: TestTokens(), http: ScriptedHTTP([.init(code: 204)]))
        let state = try await client.playback()
        XCTAssertNil(state)
    }
    func test403IsAccessFailureNotSuccess() async {
        let client = SpotifyClient(auth: TestTokens(), http: ScriptedHTTP([.init(code: 403)]))
        do { try await client.command(.pause, deviceID: "local"); XCTFail("Expected 403") }
        catch SpotifyFailure.denied {} catch { XCTFail("Unexpected error") }
    }
}
