import Foundation
import AppKit
import SpotifyKit

@MainActor final class SpotifyPlaybackModel: ObservableObject {
    @Published var clientID = UserDefaults.standard.string(forKey: "spotifyClientID") ?? ""
    @Published var connected = false
    @Published var signingIn = false
    @Published var devices: [SpotifyDevice] = []
    @Published var selectedDeviceID = UserDefaults.standard.string(forKey: "spotifyDeviceID") ?? ""
    @Published var playback: SpotifyPlayback?
    @Published var observedAt: Date?
    @Published var stale = true
    @Published var error = ""
    @Published var commandPending = false
    @Published var refreshingDevices = false
    @Published var lastCommand = ""
    @Published var events: [String] = []
    var onRemotePlayback: (() -> Void)?
    var onPlaybackChanged: (() -> Void)?
    var recordingEnabled = false {
        didSet { if oldValue != recordingEnabled && connected { beginPolling() } }
    }
    private var auth: SpotifyAuth!
    private var client: SpotifyClient!
    private var pollTask: Task<Void, Never>?
    private var loginTask: Task<Void, Never>?
    private var reading = false
    private var retryAfter = Date.distantPast
    private var generation = 0

    init() { configure() }
    var selectedDevice: SpotifyDevice? { devices.first { $0.id == selectedDeviceID } }
    var isLocalPlayback: Bool { !selectedDeviceID.isEmpty && playback?.device?.id == selectedDeviceID }
    var canControl: Bool { connected && selectedDevice != nil && !commandPending && !selectedDeviceID.isEmpty }
    var playbackDescription: String {
        if !connected { return "Spotify is not connected" }
        if stale { return "Playback status is stale" }
        guard let playback else { return "No active Spotify playback" }
        if !isLocalPlayback { return "Playing on \(playback.device?.name ?? "another device")" }
        return playback.is_playing ? "Playing on this Mac" : "Paused on this Mac"
    }
    func progress(at date: Date) -> Double {
        guard let playback else { return 0 }
        let base = Double(playback.progress_ms ?? 0) / 1000
        let elapsed = playback.is_playing && !stale ? max(0, date.timeIntervalSince(observedAt ?? date)) : 0
        return min(Double(playback.item?.duration_ms ?? 0) / 1000, base + elapsed)
    }
    func configure() {
        generation += 1
        let current = generation
        pollTask?.cancel(); loginTask?.cancel()
        connected = false; devices = []; playback = nil; stale = true; observedAt = nil
        clientID = clientID.trimmingCharacters(in: .whitespacesAndNewlines)
        UserDefaults.standard.set(clientID, forKey: "spotifyClientID")
        auth = SpotifyAuth(clientID: clientID)
        client = SpotifyClient(auth: auth)
        guard !clientID.isEmpty else { return }
        Task {
            do {
                let saved = try await auth.hasSavedLogin()
                guard current == generation else { return }
                if saved { connected = true; await refreshDevices(); beginPolling() }
            } catch { self.error = error.localizedDescription }
        }
    }
    func signIn() {
        guard !signingIn else { return }
        if clientID != UserDefaults.standard.string(forKey: "spotifyClientID") { configure() }
        signingIn = true; error = ""
        let current = generation
        loginTask = Task {
            defer { signingIn = false }
            do {
                try await auth.signIn { url in Task { @MainActor in NSWorkspace.shared.open(url) } }
                guard current == generation else { return }
                connected = true
                event("Spotify connected; refresh credential saved to Keychain")
                await refreshDevices()
                beginPolling()
            } catch {
                guard current == generation else { return }
                handle(error)
            }
        }
    }
    func cancelSignIn() { loginTask?.cancel(); Task { await auth.cancelLogin() } }
    func signOut() {
        generation += 1
        pollTask?.cancel(); loginTask?.cancel()
        connected = false; playback = nil; devices = []; stale = true
        onPlaybackChanged?()
        Task {
            do { try await auth.signOut(); event("Spotify disconnected; saved refresh credential removed") }
            catch { self.error = error.localizedDescription }
        }
    }
    func selectDevice(_ id: String) {
        selectedDeviceID = id
        UserDefaults.standard.set(id, forKey: "spotifyDeviceID")
        event("Local Spotify device explicitly selected")
        if playback?.device?.id != id { onRemotePlayback?() }
        onPlaybackChanged?()
    }
    func refreshDevices() async {
        guard connected, !refreshingDevices else { return }
        refreshingDevices = true
        defer { refreshingDevices = false }
        do {
            devices = try await client.devices().filter { $0.id != nil && !$0.is_restricted }
            if !selectedDeviceID.isEmpty && !devices.contains(where: { $0.id == selectedDeviceID }) {
                selectedDeviceID = ""
                UserDefaults.standard.removeObject(forKey: "spotifyDeviceID")
                onRemotePlayback?()
            }
            event("Spotify device list refreshed")
        } catch { handle(error) }
    }
    func refreshPlayback(force: Bool = false) async {
        guard connected, !reading, force || !commandPending else { return }
        reading = true
        let current = generation
        defer { reading = false }
        do {
            let state = try await client.playback()
            guard current == generation else { return }
            playback = state; observedAt = Date(); stale = false; error = ""
            onPlaybackChanged?()
            if let deviceID = state?.device?.id, !selectedDeviceID.isEmpty, deviceID != selectedDeviceID { onRemotePlayback?() }
        } catch {
            guard current == generation else { return }
            stale = true; handle(error)
            onPlaybackChanged?()
        }
    }
    func send(_ command: TransportCommand, name: String) {
        guard canControl else { error = "Connect Spotify and identify its device on this Mac first."; return }
        commandPending = true
        lastCommand = "\(name) requested"
        let id = selectedDeviceID
        Task {
            defer { commandPending = false }
            do {
                try await client.command(command, deviceID: id)
                lastCommand = "\(name) accepted; checking Spotify"
                event(lastCommand)
                try await Task.sleep(for: .milliseconds(350))
                await refreshPlayback(force: true)
            } catch {
                lastCommand = "\(name) not confirmed"
                stale = true; handle(error)
                if case SpotifyFailure.noDevice = error { await refreshDevices() }
            }
        }
    }
    func pauseForOutputLoss() async -> Bool {
        guard connected, !selectedDeviceID.isEmpty else { return false }
        do {
            try await client.command(.pause, deviceID: selectedDeviceID)
            event("Best-effort pause accepted after audio output loss")
            return true
        } catch { handle(error); return false }
    }
    private func beginPolling() {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, self.connected else { return }
                if Date() >= self.retryAfter { await self.refreshPlayback() }
                let interval: Double = self.recordingEnabled ? 1 : (self.playback?.is_playing == true && NSApp.isActive ? 5 : 15)
                let delay = max(interval, self.retryAfter.timeIntervalSinceNow)
                do { try await Task.sleep(for: .seconds(delay)) } catch { return }
            }
        }
    }
    private func handle(_ error: Error) {
        if error is CancellationError { return }
        self.error = error.localizedDescription
        event(self.error)
        switch error {
        case SpotifyFailure.rateLimited(let date): retryAfter = date
        case SpotifyFailure.denied: retryAfter = Date().addingTimeInterval(300)
        case SpotifyFailure.signedOut: connected = false; pollTask?.cancel()
        default: break
        }
    }
    private func event(_ text: String) {
        events.append("\(ISO8601DateFormatter().string(from: Date()))  \(text)")
        if events.count > 80 { events.removeFirst(events.count - 80) }
    }
    var status: [String: Any] {
        ["connected": connected, "signingIn": signingIn, "stale": stale,
         "error": error, "deviceID": selectedDeviceID, "isLocalPlayback": isLocalPlayback,
         "isPlaying": playback?.is_playing ?? false, "title": playback?.item?.name ?? "",
         "artist": playback?.item?.subtitle ?? "", "positionSeconds": progress(at: Date()),
         "durationSeconds": Double(playback?.item?.duration_ms ?? 0) / 1000,
         "commandPending": commandPending, "lastCommand": lastCommand,
         "refreshingDevices": refreshingDevices,
         "contextURI": playback?.context?.uri ?? "",
         "devices": devices.map { ["id": $0.id ?? "", "name": $0.name, "type": $0.type, "active": $0.is_active] as [String: Any] }]
    }
}
