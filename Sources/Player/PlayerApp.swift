import SwiftUI
import AppKit
import PlayerAudio
import PlayerControl
import SpotifyKit

@MainActor final class PlayerViewModel: ObservableObject {
    @Published var outputs: [OutputDevice] = []
    @Published var selectedUID = UserDefaults.standard.string(forKey: "outputDeviceUID") ?? ""
    @Published var sourceSummary = ""
    @Published var discoveryError = ""
    @Published var route = RouteSnapshot()
    @Published var gain: Double = 0.5 {
        didSet { controller.setGain(Float(gain)) }
    }
    let spotify = SpotifyPlaybackModel()
    let bridge = AudioBridgeModel()
    let playlistRecording: PlaylistRecordingModel
    @Published var bridgeRecordingActive = false
    let audioPath = "bridge"
    let controller = AudioRouteController()
    let controlServer = LocalControlServer()
    private var observers: [NSObjectProtocol] = []

    init() {
        let folder = UserDefaults.standard.string(forKey:"debugOutputDirectory") ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Documents/Audio").path
        playlistRecording = PlaylistRecordingModel(spotify:spotify,bridge:bridge,directory:URL(fileURLWithPath:folder,isDirectory:true))
        playlistRecording.prepareSource = { [weak self] in
            guard let self else { throw ControlError.message("Player is shutting down.") }
            try await self.prepareEndpointRoute()
        }
        controller.onChange = { [weak self] state in
            self?.route = state
            guard let self else { return }
            self.spotify.recordingEnabled = state.recording.enabled || self.bridge.recordingEnabled
            self.playlistRecording.sourceChanged(ready:state.phase == .running && state.outputUID == AudioBridgeEndpoint.uid)
        }
        bridge.onRecordingChange = { [weak self] in
            guard let self else { return }
            self.spotify.recordingEnabled = self.route.recording.enabled || self.bridge.recordingEnabled
            self.bridgeRecordingActive = self.bridge.recordingEnabled
            self.playlistRecording.bridgeChanged()
        }
        spotify.onPlaybackChanged = { [weak self] in
            guard let self else { return }
            self.controller.observeRecordingTrack(self.recordingTrack)
            self.playlistRecording.playbackChanged(self.recordingTrack)
        }
        spotify.onManualCommand = { [weak self] in
            guard let self, self.playlistRecording.running else { return }
            self.playlistRecording.stop(reason:"Automatic recovery stopped after a manual playback command.")
        }
        spotify.onRemotePlayback = { [weak self] in
            guard let self, self.route.isBusy, self.route.outputUID != AudioBridgeEndpoint.uid else { return }
            self.controller.stop(reason: "Playback is on another Spotify device. Local routing stopped.")
        }
        controller.onOutputLoss = { [weak self] completion in
            Task { @MainActor in completion(await self?.spotify.pauseForOutputLoss() ?? false) }
        }
        refresh()
        let center = NSWorkspace.shared.notificationCenter
        observers.append(center.addObserver(forName: NSWorkspace.willSleepNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.controller.stop(reason: "Stopped for sleep. After wake, refresh devices and restart routing.")
            }
        })
        observers.append(center.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.refresh() }
        })
        do {
            try controlServer.start { [weak self] request, reply in
                DispatchQueue.main.async {
                    guard let self else { reply(["ok": false, "error": "Player is shutting down."]); return }
                    reply(self.handleControl(request))
                }
            }
        } catch { discoveryError = "Local control unavailable: \(error.localizedDescription)" }
    }

    private func handleControl(_ request: [String: Any]) -> [String: Any] {
        guard let method = request["method"] as? String,
              let arguments = request["arguments"] as? [String: Any] else {
            return ["ok": false, "error": "Expected method and arguments object."]
        }
        switch method {
        case "player_status":
            return ["ok": true, "phase": route.phase.rawValue, "busy": route.isBusy,
                    "detail": route.detail, "format": route.format,
                    "selectedOutputUID": selectedUID, "gain": gain, "sourceOutputUID":route.outputUID, "sourceReady":endpointReady,
                    "sourceSummary": sourceSummary, "discoveryError": discoveryError,
                    "stats": ["callbacks": route.stats.callbacks, "frames": route.stats.frames,
                              "nonzeroFrames": route.stats.nonzeroFrames, "invalidBuffers": route.stats.invalidBuffers,
                              "peak": route.stats.peak],
                    "spotify": spotify.status, "audioPath":audioPath, "bridge":bridge.status,
                    "playlistRecovery":playlistRecording.status,
                    "recording": ["enabled": route.recording.enabled, "finishing": route.recording.finishing,
                                  "file": route.recording.file, "savedFiles": route.recording.savedFiles,
                                  "frames": route.recording.frames, "detail": route.recording.detail,
                                  "error": route.recording.error, "directory": debugOutputDirectory.path]]
        case "player_devices":
            refresh()
            guard discoveryError.isEmpty else { return ["ok": false, "error": discoveryError] }
            return ["ok": true, "devices": outputs.map { output -> [String: Any] in
                ["uid": output.uid, "name": output.name, "sampleRate": output.sampleRate,
                 "inputChannels": output.inputChannels, "outputChannels": output.outputChannels,
                 "supported": output.supportsPrototype, "bluetooth": output.isBluetooth]
            }]
        case "player_logs": return ["ok": true, "events": route.events, "spotifyEvents": spotify.events, "latestError": discoveryError,
                                     "routeDetail": route.detail]
        case "player_start":
            if audioPath == "bridge" {
                enableEndpointRoute()
                return ["ok":true,"accepted":true,"message":"Spotify-only routing to Audio Bridge requested. Inspect sourceReady and phase; the Mac's default output is unchanged."]
            }
            guard !route.isBusy else { return ["ok": false, "error": "A route is already active or preparing. Stop it first."] }
            refresh()
            guard let uid = arguments["outputUID"] as? String,
                  let device = outputs.first(where: { $0.uid == uid }), device.supportsPrototype else {
                return ["ok": false, "error": "Choose a supported outputUID from player_devices."]
            }
            selectedUID = uid
            start()
            return ["ok": true, "accepted": true, "message": "Start requested. Poll player_status; acceptance does not imply audio is routing."]
        case "player_stop":
            if playlistRecording.running || bridge.recordingEnabled { playlistRecording.stop() }
            if audioPath == "bridge" { controller.stop(); bridge.command("bridge_stop") } else { controller.stop() }
            return ["ok": true, "accepted": true, "message": "Stop requested. Poll player_status for teardown completion."]
        case "player_volume":
            guard let value = arguments["gain"] as? Double, value.isFinite, (0...1).contains(value) else {
                return ["ok": false, "error": "gain must be a finite number between 0 and 1."]
            }
            gain = value
            if audioPath == "bridge" { bridge.command("bridge_gain",["gain":value]) }
            return ["ok": true, "gain": gain]
        case "player_recordings":
            let offset = max(0,arguments["offset"] as? Int ?? 0)
            let limit = max(1,min(20,arguments["limit"] as? Int ?? 20))
            if arguments["refresh"] as? Bool == true { Task { try? await playlistRecording.refreshCatalog() } }
            return playlistRecording.catalogPage(offset:offset,limit:limit)
        case "player_record_resume":
            guard !playlistRecording.running, !bridge.recordingEnabled, !bridge.busy, spotify.canControl,
                  bridge.installed else {
                return ["ok":false,"error":"Enable Audio Bridge, connect Spotify on this Mac, and stop any current recording first."]
            }
            let input = arguments["playlist"] as? String ?? spotify.playback?.context?.uri ?? UserDefaults.standard.string(forKey:"lastRecordingPlaylist") ?? ""
            guard let uri = try? SpotifyPlaylist.uri(input) else { return ["ok":false,"error":"Provide a Spotify playlist link or play a playlist first."] }
            playlistRecording.start(playlist:uri)
            return ["ok":true,"accepted":true,"message":"Recovery requested. Inspect player_status.playlistRecovery."]
        case "player_record_start":
            guard !playlistRecording.running else { return ["ok":false,"error":"Playlist recovery is already active."] }
            if audioPath == "bridge" {
                playlistRecording.playbackChanged(recordingTrack)
                playlistRecording.startManualRecording()
                return ["ok":true,"accepted":true,"message":"Spotify-only endpoint WAV recording requested. Inspect sourceReady and bridge.recording."]
            }
            guard route.phase == .running, recordingTrack != nil,
                  !route.recording.enabled, !route.recording.finishing else {
                return ["ok": false, "error": "Start headphone routing and play a known track on this Mac before recording; wait for any previous WAV to finalize."]
            }
            startRecording()
            return ["ok": true, "accepted": true, "directory": debugOutputDirectory.path,
                    "message": "Recording requested. Poll player_status.recording for file creation or errors."]
        case "player_record_stop":
            if playlistRecording.running { playlistRecording.stop(); return ["ok":true,"accepted":true] }
            if audioPath == "bridge" { bridge.command("bridge_record_stop") } else { controller.stopRecording() }
            return ["ok": true, "accepted": true, "message": "Finalization requested. Poll until recording.enabled and recording.finishing are false."]
        case "player_login":
            spotify.signIn()
            return ["ok": true, "accepted": true, "message": "Sign-in opened in the system browser. The user must approve Spotify access there."]
        case "player_spotify_devices":
            Task { await spotify.refreshDevices() }
            return ["ok": true, "accepted": true, "spotify": spotify.status]
        case "player_select_spotify_device":
            guard let id = arguments["deviceID"] as? String, spotify.devices.contains(where: { $0.id == id }) else {
                return ["ok": false, "error": "Select a current Spotify device ID and confirm it represents this Mac."]
            }
            spotify.selectDevice(id)
            return ["ok": true, "spotify": spotify.status]
        case "player_transport":
            guard spotify.canControl, let action = arguments["action"] as? String else {
                return ["ok": false, "error": "Connect Spotify, select this Mac's Spotify device, and wait for any pending command."]
            }
            let command: TransportCommand
            switch action {
            case "play": command = .play
            case "pause": command = .pause
            case "next": command = .next
            case "previous": command = .previous
            case "transfer": command = .transfer
            case "playlist":
                guard let input = arguments["playlist"] as? String, let uri = try? SpotifyPlaylist.uri(input) else {
                    return ["ok": false, "error": "Provide a valid Spotify playlist URL or URI in playlist."]
                }
                command = .playlist(uri)
            case "seek":
                guard let position = arguments["positionMs"] as? Int, position >= 0,
                      position <= (spotify.playback?.item?.duration_ms ?? 0) else {
                    return ["ok": false, "error": "positionMs must be within the observed track duration."]
                }
                command = .seek(position)
            default: return ["ok": false, "error": "Unknown transport action."]
            }
            spotify.send(command, name: action)
            return ["ok": true, "accepted": true, "message": "Command queued. Poll Spotify status to reconcile the result."]
        case "player_quit":
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { NSApp.terminate(nil) }
            return ["ok": true, "accepted": true]
        default: return ["ok": false, "error": "Unknown control method."]
        }
    }
    func refresh() {
        do {
            outputs = try OutputDevice.discover()
            if selectedUID.isEmpty { selectedUID = (try? OutputDevice.defaultOutputUID()) ?? "" }
            let sources = try SourceResolver.spotify()
            sourceSummary = sources.isEmpty ? "No Spotify audio processes found" : "\(sources.count) Spotify audio process\(sources.count == 1 ? "" : "es") · \(sources.filter(\.isPlaying).count) producing output"
            discoveryError = ""
        } catch { discoveryError = error.localizedDescription }
    }
    var listeningOutputUID: String? {
        let current = bridge.status["defaultOutput"] as? String
        if let current, outputs.contains(where:{ $0.uid == current }) { return current }
        return outputs.contains(where:{ $0.uid == selectedUID }) ? selectedUID : nil
    }
    var endpointReady: Bool { route.phase == .running && route.outputUID == AudioBridgeEndpoint.uid }
    func prepareEndpointRoute() async throws {
        try await bridge.perform("bridge_start")
        guard bridge.status["defaultOutput"] as? String != AudioBridgeEndpoint.uid else {
            throw ControlError.message("Choose speakers or headphones as the Mac's output. Player routes Spotify to Audio Bridge separately, keeping other apps out of its WAVs.")
        }
        if endpointReady { return }
        guard !route.isBusy else { throw ControlError.message("Wait for the current audio route to finish.") }
        controller.start(outputUID:AudioBridgeEndpoint.uid)
        // Core Audio setup and TCC stay on the existing lifecycle queue; the UI
        // observes progress and remains responsive while this request waits.
        do {
            for attempt in 0..<200 {
                try Task.checkCancellation()
                if endpointReady { return }
                if attempt > 0 && route.phase == .failed { throw ControlError.message(route.detail) }
                try await Task.sleep(for:.milliseconds(100))
            }
            throw ControlError.message("Spotify routing is not ready. Check Player's system audio recording permission.")
        } catch { controller.stop(reason:"Spotify endpoint setup cancelled or failed."); throw error }
    }
    func enableEndpointRoute() {
        Task {
            do { try await prepareEndpointRoute() }
            catch { discoveryError = error.localizedDescription }
        }
    }
    func start() {
        if spotify.connected && !spotify.isLocalPlayback {
            discoveryError = "Identify Spotify on this Mac and transfer playback here before routing."
            return
        }
        UserDefaults.standard.set(selectedUID, forKey: "outputDeviceUID")
        controller.start(outputUID: selectedUID)
    }
    var debugOutputDirectory: URL {
        if let path = UserDefaults.standard.string(forKey: "debugOutputDirectory") {
            return URL(fileURLWithPath: path, isDirectory: true)
        }
        return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Documents/Audio", isDirectory: true)
    }
    var recordingTrack: RecordingTrack? {
        guard spotify.connected, !spotify.stale, spotify.isLocalPlayback,
              let playback = spotify.playback, playback.is_playing,
              let item = playback.item, let id = item.id,
              playback.currently_playing_type == "track" else { return nil }
        return RecordingTrack(id: id, album: item.album?.name ?? "Unknown album", artist: item.subtitle,
                              name: item.name, positionSeconds: spotify.progress(at: Date()),
                              expectedDurationSeconds:Double(item.duration_ms)/1000)
    }
    func startRecording() {
        guard let track = recordingTrack else { return }
        controller.startRecording(directory: debugOutputDirectory, track: track)
    }
    func exportDiagnostics() {
        let panel = NSSavePanel()
        if let directory = UserDefaults.standard.string(forKey: "debugOutputDirectory") {
            let url = URL(fileURLWithPath: directory, isDirectory: true)
            do {
                try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
                panel.directoryURL = url
            } catch {
                discoveryError = "Cannot open debug output folder: \(error.localizedDescription)"
                return
            }
        }
        panel.nameFieldStringValue = "Player-diagnostics.txt"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let spotifyVersion = Bundle(url: URL(fileURLWithPath: "/Applications/Spotify.app"))?.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "unknown"
        let report = """
        Player 0.3 — local Spotify player
        OS: \(ProcessInfo.processInfo.operatingSystemVersionString)
        Architecture: \(architecture)
        Spotify: \(spotifyVersion)
        Output: \(outputs.first(where: { $0.uid == selectedUID })?.name ?? "unavailable")
        State: \(route.phase.rawValue)
        Detail: \(route.detail)
        Format: \(route.format)
        Callbacks: \(route.stats.callbacks)
        Frames: \(route.stats.frames)
        Nonzero frames: \(route.stats.nonzeroFrames)
        Invalid/underrun buffers: \(route.stats.invalidBuffers)
        Peak: \(route.stats.peak)
        \(route.events.joined(separator: "\n"))
        \(spotify.events.joined(separator: "\n"))
        """
        do { try report.write(to: url, atomically: true, encoding: .utf8) }
        catch { discoveryError = "Export failed: \(error.localizedDescription)" }
    }
}

private var architecture: String {
    #if arch(arm64)
    "Apple silicon (arm64)"
    #else
    "Intel (unvalidated)"
    #endif
}

@MainActor final class AppDelegate: NSObject, NSApplicationDelegate {
    var model: PlayerViewModel?
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
    func applicationWillTerminate(_ notification: Notification) {
        model?.controlServer.stop()
        model?.controller.shutdown()
    }
}

@main struct PlayerApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = PlayerViewModel()

    init() {
        if CommandLine.arguments.contains("--inspect") {
            do {
                print("Player feasibility environment · \(ProcessInfo.processInfo.operatingSystemVersionString) · \(architecture)")
                print("DEFAULT OUTPUT \(try OutputDevice.defaultOutputUID())")
                for output in try OutputDevice.discover() {
                    print("OUTPUT \(output.name) | uid=\(output.uid) | \(Int(output.sampleRate)) Hz | in=\(output.inputChannels) out=\(output.outputChannels) | supported=\(output.supportsPrototype)")
                }
                for source in try SourceResolver.spotify() {
                    print("SPOTIFY object=\(source.objectID) pid=\(source.pid) bundle=\(source.bundleID) playing=\(source.isPlaying)")
                }
                exit(0)
            } catch { print(error.localizedDescription); exit(1) }
        }
    }

    var body: some Scene {
        WindowGroup("Player") {
            PlayerWindow(model: model, spotify: model.spotify)
                .onAppear { delegate.model = model }
        }
        .windowResizability(.contentSize)
        .commands {
            CommandGroup(replacing: .newItem) {}
            CommandMenu("Recording") {
                Button("Stop recording") { model.playlistRecording.stop() }.keyboardShortcut(".", modifiers: .command)
                Button("Open recording folder") { NSWorkspace.shared.open(model.debugOutputDirectory) }
            }
        }
    }
}

