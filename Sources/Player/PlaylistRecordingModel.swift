import Foundation
import PlayerAudio
import PlayerControl
import SpotifyKit

/// Player owns the library and playback policy. The service only receives
/// capture observations; neither it nor the HAL driver needs Spotify knowledge.
@MainActor final class PlaylistRecordingModel: ObservableObject {
    @Published private(set) var active = false
    @Published private(set) var preparing = false
    @Published private(set) var detail = "Completed tracks are skipped when you resume a playlist."
    @Published private(set) var error = ""
    @Published private(set) var completeCount = 0
    @Published private(set) var unverifiedCount = 0
    @Published private(set) var skipped = 0
    @Published private(set) var plannedTracks = 0
    @Published private(set) var unavailableTracks = 0
    @Published private(set) var scanning = false
    var prepareSource: (() async throws -> Void)?
    @Published private(set) var sourceReady = false
    private let spotify: SpotifyPlaybackModel
    private let bridge: AudioBridgeModel
    let directory: URL
    private var policy = PlaylistRecovery()
    private var recordingPlaylistURI = ""
    private var recordingPlan: PlaylistRecordingPlan?
    private var entries: [[String:Any]] = []
    private var completeIDs: Set<String> = []
    private var latestTrack: RecordingTrack?
    private var operation: Task<Void,Never>?
    private var generation = 0
    private var lastSavedFiles = -1
    private var catalogTask: Task<Void, Error>?
    private var catalogDate: Date?
    private var catalogDirty = false
    private var suppressCapture = false
    private var manualRecording = false

    init(spotify: SpotifyPlaybackModel, bridge: AudioBridgeModel, directory: URL) {
        self.spotify = spotify; self.bridge = bridge; self.directory = directory
        Task { try? await refreshCatalog() }
    }
    var running: Bool { active || preparing }
    var status: [String:Any] {
        ["active":active,"preparing":preparing,"detail":detail,"error":error,
         "completeTracks":completeCount,"unverifiedFiles":unverifiedCount,"skipped":skipped,
         "plannedTracks":plannedTracks,"unavailableTracks":unavailableTracks,
         "directory":directory.path,"scanning":scanning,
         "catalogUpdatedAt":catalogDate.map { ISO8601DateFormatter().string(from:$0) } ?? ""]
    }
    func refreshCatalog() async throws {
        catalogDirty = true
        if let catalogTask { return try await catalogTask.value }
        scanning = true
        let folder = directory
        let task = Task { @MainActor in
            defer { scanning = false; catalogTask = nil }
            repeat {
                catalogDirty = false
                entries = try await Task.detached(priority:.utility) {
                    try RecordingCatalog.libraryEntries(directory:folder)
                }.value
                completeIDs = Set(entries.filter { $0["status"] as? String == "complete" }.compactMap { $0["trackID"] as? String })
                completeCount = completeIDs.count
                unverifiedCount = entries.filter { $0["status"] as? String == "unverified" }.count
                catalogDate = Date()
            } while catalogDirty
        }
        catalogTask = task
        try await task.value
    }
    /// Bounded pages keep raw sidecars inspectable through the existing app MCP.
    /// A refresh is asynchronous; the timestamp identifies the cached snapshot.
    func catalogPage(offset: Int, limit: Int) -> [String:Any] {
        var page: [[String:Any]] = []
        var bytes = 0
        for entry in entries.dropFirst(offset).prefix(limit) {
            let size = (try? JSONSerialization.data(withJSONObject:entry).count) ?? 0
            if bytes + size > 48_000 { break }
            page.append(entry); bytes += size
        }
        return ["ok":true,"schemaVersion":1,"entries":page,"total":entries.count,
                "offset":offset,"nextOffset":offset+page.count,"hasMore":offset+page.count < entries.count,
                "scanning":scanning,"updatedAt":catalogDate.map { ISO8601DateFormatter().string(from:$0) } ?? "",
                "completionPolicy":"observed-v1","boundaryToleranceSeconds":2]
    }
    func startManualRecording() {
        guard !running, operation == nil, !bridge.recordingEnabled, !bridge.busy else {
            error = "A recording is already active or preparing."; return
        }
        generation += 1; let current = generation
        preparing = true; error = ""; suppressCapture = true
        detail = "Connecting Spotify to Audio Bridge…"
        operation = Task {
            do {
                guard let prepareSource else { throw ControlError.message("Spotify's Audio Bridge route is unavailable.") }
                try await prepareSource()
                try Task.checkCancellation()
                guard current == generation else { return }
                try await bridge.startRecording(directory:directory,track:latestTrack)
                try Task.checkCancellation()
                guard current == generation else { return }
                operation = nil; preparing = false; manualRecording = true; suppressCapture = false
                detail = "Recording Spotify audio. Listening is optional."
                bridge.observe(latestTrack,capture:sourceReady)
            } catch {
                guard current == generation else { return }
                operation = nil; stop(reason:"Could not record: \(error.localizedDescription)",failed:true)
            }
        }
    }
    func start(playlist input: String) {
        guard !running, operation == nil, !bridge.recordingEnabled, !bridge.busy else {
            error = "Stop the current recording before resuming a playlist."; return
        }
        guard bridge.installed, spotify.canControl else {
            error = "Enable Audio Bridge and connect Spotify on this Mac first."; return
        }
        let uri: String
        do { uri = try SpotifyPlaylist.uri(input) }
        catch { self.error = error.localizedDescription; return }
        generation += 1; let current = generation
        preparing = true; skipped = 0; plannedTracks = 0; unavailableTracks = 0
        error = ""; detail = "Checking saved recordings…"
        suppressCapture = true
        bridge.observe(nil,capture:false)
        operation = Task {
            do {
                try await refreshCatalog()
                try Task.checkCancellation()
                detail = "Reading the playlist…"
                let items = try await spotify.playlistItems(uri)
                try Task.checkCancellation()
                let signatures = Set(entries.compactMap { entry -> RecordingTrackSignature? in
                    guard entry["status"] as? String == "complete",
                          let metadata = entry["metadata"] as? [String:Any],
                          let title = metadata["track"] as? String, let artist = metadata["artist"] as? String,
                          let album = metadata["album"] as? String,
                          let seconds = metadata["expectedDurationSeconds"] as? Double,
                          seconds.isFinite, (0...86400).contains(seconds) else { return nil }
                    return RecordingTrackSignature(title:title,artist:artist,album:album,durationMs:Int((seconds*1000).rounded()))
                })
                let plan = PlaylistRecordingPlan(items:items,completedTrackIDs:completeIDs,completedSignatures:signatures)
                recordingPlan = plan
                plannedTracks = plan.trackIDs.count
                skipped = plan.skipped
                unavailableTracks = plan.unavailable
                UserDefaults.standard.set(uri,forKey:"lastRecordingPlaylist")
                guard let first = plan.trackIDs.first, let firstPosition = plan.positions[first] else {
                    detail = plan.unavailable == 0 ? "All playlist tracks have complete recordings." :
                        "No playable tracks left to record. \(plan.unavailable) unavailable playlist items."
                    preparing = false; operation = nil
                    return
                }
                guard let prepareSource else { throw ControlError.message("Spotify's Audio Bridge route is unavailable.") }
                detail = "Connecting Spotify to Audio Bridge…"
                try await prepareSource()
                try Task.checkCancellation()
                try await bridge.perform("bridge_record_start",["directory":directory.path,"leaseSeconds":10,"playing":false])
                guard bridge.recordingEnabled else { throw ControlError.message("Audio Bridge did not arm the recorder.") }
                try Task.checkCancellation()
                detail = "Starting \(plan.trackIDs.count) unfinished tracks…"
                try await spotify.perform(.playlistAt(uri,position:firstPosition),name:"Resume playlist recording")
                // Spotify applies play asynchronously. A fresh HTTP response may
                // still describe the previous context; bootstrap waits boundedly.
                for _ in 0..<20 {
                    if startupConfirmed(uri, firstTrackID:first) { break }
                    try await Task.sleep(for:.milliseconds(500))
                    await spotify.refreshPlayback(force:true)
                    guard !spotify.stale else { throw ControlError.message("Spotify playback metadata became stale during startup.") }
                }
                guard startupConfirmed(uri, firstTrackID:first) else { throw ControlError.message("Spotify did not confirm the first unfinished track within 10 seconds (expected \(first), observed \(spotify.playback?.item?.id ?? "none")).") }
                try Task.checkCancellation()
                guard generation == current else { return }
                recordingPlaylistURI = uri
                policy.start(playlistURI:uri,deviceID:spotify.selectedDeviceID,now:ProcessInfo.processInfo.systemUptime,
                             orderedTrackIDs:plan.trackIDs,savedTrackIDs:plan.savedTrackIDs)
                preparing = false; active = true; operation = nil
                playbackChanged(latestTrack)
            } catch {
                guard generation == current else { return }
                operation = nil
                stop(reason:"Could not resume: \(error.localizedDescription)",failed:true)
            }
        }
    }
    private func startupConfirmed(_ uri: String, firstTrackID: String) -> Bool {
        guard !spotify.stale, spotify.isLocalPlayback, let playback = spotify.playback else { return false }
        return playback.context?.uri == uri &&
            playback.is_playing && plannedID(for:playback.item) == firstTrackID &&
            playback.currently_playing_type == "track" && playback.shuffle_state == false && playback.repeat_state == "off"
    }
    private func plannedID(for item: SpotifyItem?) -> String? {
        guard let item else { return nil }
        return recordingPlan?.plannedID(for:item,verifiedRecording:item.id.map(completeIDs.contains) == true)
    }

    func playbackChanged(_ track: RecordingTrack?) {
        let previousTrack = latestTrack
        latestTrack = track
        if preparing { bridge.observe(track,capture:false); return }
        guard active else { bridge.observe(track,capture:!suppressCapture && sourceReady); return }
        // A transport operation owns observations until its final refresh. This
        // prevents a new skip from being lost behind an in-flight command.
        guard operation == nil else { bridge.observe(track,capture:false); return }
        let item = spotify.playback?.item
        let plannedID = plannedID(for:item)
        var completed = completeIDs
        if let plannedID, let actualID = item?.id, completed.contains(actualID) {
            completed.insert(plannedID)
        }
        let action = policy.observe(.init(playback:spotify.playback,stale:spotify.stale,plannedTrackID:plannedID),
                                    completedTrackIDs:completed,now:ProcessInfo.processInfo.systemUptime)
        switch action {
        case .capture:
            bridge.observe(track)
            detail = "Recording missing track · \(skipped) skipped"
        case .wait:
            bridge.observe(track,capture:false)
            detail = "Waiting for Spotify to confirm the track…"
        case .skip, .restart, .jump:
            if case .jump = action, previousTrack?.id == track?.id { bridge.observe(nil,capture:false) }
            else { bridge.observe(track,capture:false) }
            let command: TransportCommand
            let name: String
            switch action {
            case .jump(let id):
                if let plannedID, let actualID = item?.id, plannedID != actualID, completeIDs.contains(actualID) {
                    skipped += 1
                }
                guard let position = recordingPlan?.positions[id] else {
                    stop(reason:"The next unfinished track has no playlist position.",failed:true); return
                }
                command = .playlistAt(recordingPlaylistURI,position:position)
                name = "Jump to next unfinished track"
            case .skip:
                skipped += 1
                command = .next
                name = "Skip saved track"
            default:
                command = .seek(0)
                name = "Restart missing track"
            }
            detail = name + "…"
            let current = generation
            operation = Task {
                do {
                    // Enqueue the boundary before advancing Spotify; the recorder
                    // processes observations serially and omits skipped WAVs.
                    try await bridge.pushObservation()
                    try Task.checkCancellation()
                    try await spotify.perform(command,name:name)
                    guard generation == current else { return }
                    operation = nil
                    playbackChanged(latestTrack)
                } catch {
                    guard generation == current else { return }
                    operation = nil
                    stop(reason:"Recovery stopped: \(error.localizedDescription)",failed:true)
                }
            }
        case .finish:
            stop(reason:"Reached the end of the planned tracks.", endOfQueue:true)
        case .stop(let reason): stop(reason:reason)
        }
    }
    func sourceChanged(ready: Bool) {
        sourceReady = ready
        if (active || manualRecording) && !ready { stop(reason:"Spotify's dedicated Audio Bridge route stopped.",failed:true) }
    }
    func bridgeChanged() {
        if let saved = bridge.recording["savedFiles"] as? Int, saved != lastSavedFiles {
            lastSavedFiles = saved
            Task { do { try await refreshCatalog() } catch { self.error = error.localizedDescription } }
        }
        if (active || manualRecording) && bridge.status["defaultOutput"] as? String == AudioBridgeEndpoint.uid {
            stop(reason:"Mac system audio was sent to Audio Bridge. Choose another system output before recording Spotify-only WAVs.",failed:true)
            return
        }
        if (active || manualRecording) && (!bridge.recordingEnabled || !bridge.capturing || !sourceReady) {
            stop(reason:"Recording or the Audio Bridge route stopped.",failed:true)
        }
    }
    func stop(reason: String = "Recording stopped. Resume the playlist to retry unfinished tracks.", failed: Bool = false, endOfQueue: Bool = false) {
        generation += 1
        _ = policy.cancel(reason:reason)
        active = false; manualRecording = false; preparing = true; detail = reason
        if failed { error = reason }
        let previous = operation
        let endingTrackID = endOfQueue ? spotify.playback?.item?.id : nil
        previous?.cancel()
        suppressCapture = true
        bridge.observe(nil,capture:false)
        operation = Task {
            await previous?.value
            do {
                if endOfQueue {
                    // Let the recorder close on the observed boundary before stopping.
                    try? await bridge.pushObservation()
                }
                try await bridge.perform("bridge_record_stop")
                for _ in 0..<100 {
                    if !bridge.recordingEnabled && bridge.recording["finishing"] as? Bool != true { break }
                    try await Task.sleep(for:.milliseconds(100))
                    await bridge.refresh()
                }
                guard !bridge.recordingEnabled, bridge.recording["finishing"] as? Bool != true else {
                    throw ControlError.message("Recording stop was not confirmed. Capture remains suppressed; try Stop recording again.")
                }
            } catch { self.error = error.localizedDescription }
            if let endingTrackID, spotify.isLocalPlayback, spotify.playback?.is_playing == true,
               spotify.playback?.item?.id == endingTrackID {
                do { try await spotify.perform(.pause,name:"End recording queue") }
                catch { self.error = error.localizedDescription }
            }
            operation = nil; preparing = false
            try? await refreshCatalog()
        }
    }
}
