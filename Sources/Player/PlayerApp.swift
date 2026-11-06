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
    @Published var bridgeRecordingActive = false
    let audioPath = "bridge"
    let controller = AudioRouteController()
    let controlServer = LocalControlServer()
    private var observers: [NSObjectProtocol] = []

    init() {
        controller.onChange = { [weak self] state in
            self?.route = state
            guard let self else { return }
            self.spotify.recordingEnabled = state.recording.enabled || self.bridge.recordingEnabled
        }
        bridge.onRecordingChange = { [weak self] in
            guard let self else { return }
            self.spotify.recordingEnabled = self.route.recording.enabled || self.bridge.recordingEnabled
            self.bridgeRecordingActive = self.bridge.recordingEnabled
        }
        spotify.onPlaybackChanged = { [weak self] in
            guard let self else { return }
            self.controller.observeRecordingTrack(self.recordingTrack)
            self.bridge.observe(self.recordingTrack)
        }
        spotify.onRemotePlayback = { [weak self] in
            guard let self, self.route.isBusy else { return }
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
                    "selectedOutputUID": selectedUID, "gain": gain,
                    "sourceSummary": sourceSummary, "discoveryError": discoveryError,
                    "stats": ["callbacks": route.stats.callbacks, "frames": route.stats.frames,
                              "nonzeroFrames": route.stats.nonzeroFrames, "invalidBuffers": route.stats.invalidBuffers,
                              "peak": route.stats.peak],
                    "spotify": spotify.status, "audioPath":audioPath, "bridge":bridge.status,
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
            if audioPath == "bridge" { return ["ok":false,"error":"Audio Bridge mode is selected. Use bridge_start and bridge_monitor; use bridge_use_default only when explicitly routing Mac audio to the endpoint."] }
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
            if audioPath == "bridge" { bridge.command("bridge_stop") } else { controller.stop() }
            return ["ok": true, "accepted": true, "message": "Stop requested. Poll player_status for teardown completion."]
        case "player_volume":
            guard let value = arguments["gain"] as? Double, value.isFinite, (0...1).contains(value) else {
                return ["ok": false, "error": "gain must be a finite number between 0 and 1."]
            }
            gain = value
            if audioPath == "bridge" { bridge.command("bridge_gain",["gain":value]) }
            return ["ok": true, "gain": gain]
        case "player_record_start":
            if audioPath == "bridge" {
                bridge.record(directory:debugOutputDirectory,track:recordingTrack)
                return ["ok":true,"accepted":true,"message":"Endpoint WAV recording requested. Inspect player_status.bridge.recording."]
            }
            guard route.phase == .running, recordingTrack != nil,
                  !route.recording.enabled, !route.recording.finishing else {
                return ["ok": false, "error": "Start headphone routing and play a known track on this Mac before recording; wait for any previous WAV to finalize."]
            }
            startRecording()
            return ["ok": true, "accepted": true, "directory": debugOutputDirectory.path,
                    "message": "Recording requested. Poll player_status.recording for file creation or errors."]
        case "player_record_stop":
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
                              name: item.name, positionSeconds: spotify.progress(at: Date()))
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
                Button("Stop recording") { model.bridge.command("bridge_record_stop") }.keyboardShortcut(".", modifiers: .command)
                Button("Open recording folder") { NSWorkspace.shared.open(model.debugOutputDirectory) }
            }
        }
    }
}

