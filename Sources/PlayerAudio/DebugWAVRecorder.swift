import Foundation
import Darwin
import RealtimeAudio

public struct RecordingTrack: Sendable {
    public let id: String
    public let album: String
    public let artist: String
    public let name: String
    public let positionSeconds: Double
    public init(id: String, album: String, artist: String, name: String, positionSeconds: Double) {
        self.id = id; self.album = album; self.artist = artist; self.name = name
        self.positionSeconds = positionSeconds
    }
}

public struct RecordingSnapshot: Sendable {
    public var enabled = false
    public var finishing = false
    public var file = ""
    public var savedFiles = 0
    public var frames: UInt64 = 0
    public var detail = "WAV recording is off."
    public var error = ""
    public init() {}
}

// IEEE Float32 WAV preserves the rendered samples exactly. All file I/O is on
// the recorder worker, never HAL's callback or the main actor.
final class FloatWAVFile {
    let temporaryURL: URL
    let directory: URL
    let base: String
    let track: RecordingTrack
    let rate: UInt32
    let captureDescription: String
    let startedAt = Date()
    private var handle: FileHandle?
    private(set) var frames: UInt64 = 0
    private var checkpointFrames: UInt64 = 0
    private(set) var lastURL: URL

    static func filenameBase(_ track: RecordingTrack) -> String {
        func clean(_ value: String) -> String {
            let forbidden = CharacterSet.controlCharacters.union(CharacterSet(charactersIn: "/\\:"))
            let value = value.components(separatedBy: forbidden).joined(separator: "_")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            return value.isEmpty ? "Unknown" : value
        }
        var result = [track.album, track.artist, track.name].map(clean).joined(separator: " - ")
        while result.utf8.count > 200 { result.removeLast() }
        return result
    }

    init(directory: URL, track: RecordingTrack, sampleRate: Double, captureDescription: String = "IEEE Float32, post-Player-gain") throws {
        guard sampleRate.isFinite, (8000...384000).contains(sampleRate), sampleRate.rounded() == sampleRate else {
            throw AudioFailure("WAV recording requires an integer sample rate between 8 and 384 kHz.")
        }
        self.directory = directory; self.track = track; rate = UInt32(sampleRate)
        self.captureDescription = captureDescription
        base = Self.filenameBase(track)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let available = try FileManager.default.attributesOfFileSystem(forPath: directory.path)[.systemFreeSize] as? NSNumber
        guard (available?.uint64Value ?? 0) >= 32 * 1024 * 1024 else { throw AudioFailure("Less than 32 MB free in the WAV output volume.") }
        var candidate: URL!
        var descriptor: Int32 = -1
        for suffix in 0..<10000 {
            let stem = base + (suffix == 0 ? "" : " (\(suffix + 1))")
            if FileManager.default.fileExists(atPath: directory.appendingPathComponent(stem + ".wav").path) { continue }
            candidate = directory.appendingPathComponent(stem + ".partial.wav")
            descriptor = Darwin.open(candidate.path, O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW, 0o600)
            if descriptor >= 0 { break }
            if errno != EEXIST { throw AudioFailure("Cannot create WAV file (POSIX \(errno)).") }
        }
        guard descriptor >= 0 else { throw AudioFailure("Too many recordings with the same filename.") }
        temporaryURL = candidate; lastURL = candidate
        handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
        do { try handle?.write(contentsOf: Self.header(rate: rate, frames: 0)) }
        catch { try? handle?.close(); handle = nil; throw error }
    }

    static func header(rate: UInt32, frames: UInt64) -> Data {
        var data = Data()
        func tag(_ value: String) { data.append(contentsOf: value.utf8) }
        func u16(_ value: UInt16) { var v = value.littleEndian; withUnsafeBytes(of: &v) { data.append(contentsOf: $0) } }
        func u32(_ value: UInt32) { var v = value.littleEndian; withUnsafeBytes(of: &v) { data.append(contentsOf: $0) } }
        // 58 bytes: WAVEFORMATEX (IEEE float) + fact chunk + data header.
        tag("RIFF"); u32(UInt32(frames * 8) + 50); tag("WAVE")
        tag("fmt "); u32(18); u16(3); u16(2); u32(rate); u32(rate * 8); u16(8); u16(32); u16(0)
        tag("fact"); u32(4); u32(UInt32(frames))
        tag("data"); u32(UInt32(frames * 8))
        return data
    }
    func append(_ samples: UnsafePointer<Float>, count: Int) throws {
        guard let handle else { throw AudioFailure("The WAV file is closed.") }
        guard (frames + UInt64(count)) * 8 <= UInt64(UInt32.max) - 50 else {
            throw AudioFailure("WAV reached its 4 GB format limit. Recording stopped; start a new segment.")
        }
        // Supported macOS architectures are little endian, matching IEEE Float32 WAV.
        try handle.write(contentsOf: Data(bytes: samples, count: count * 8))
        frames += UInt64(count)
        if frames - checkpointFrames >= UInt64(rate) {
            try checkpoint(); checkpointFrames = frames
        }
    }
    private func checkpoint() throws {
        guard let handle else { return }
        try handle.seek(toOffset: 0)
        try handle.write(contentsOf: Self.header(rate: rate, frames: frames))
        try handle.seek(toOffset: 58 + frames * 8)
    }
    @discardableResult func finish(reason: String, complete: Bool) throws -> URL {
        guard let handle else { return lastURL }
        defer { try? handle.close(); self.handle = nil }
        // If a write failed partway through, keep only fully accounted frames.
        try handle.truncate(atOffset: 58 + frames * 8)
        try checkpoint(); try handle.synchronize(); try handle.close(); self.handle = nil
        if complete {
            for suffix in 0..<10000 {
                let stem = base + (suffix == 0 ? "" : " (\(suffix + 1))")
                let target = directory.appendingPathComponent(stem + ".wav")
                // link() atomically refuses existing destinations, including symlinks.
                if Darwin.link(temporaryURL.path, target.path) == 0 {
                    lastURL = target
                    _ = Darwin.unlink(temporaryURL.path)
                    break
                }
                if errno != EEXIST { throw AudioFailure("Cannot finalize WAV filename (POSIX \(errno)); the partial file is retained.") }
            }
            guard lastURL != temporaryURL else { throw AudioFailure("Cannot assign a unique WAV filename; the partial file is retained.") }
        }
        let manifest: [String: Any] = [
            "trackID": track.id, "album": track.album, "artist": track.artist, "track": track.name,
            "sampleRate": rate, "channels": 2, "encoding": captureDescription,
            "frames": frames, "durationSeconds": Double(frames) / Double(rate),
            "observedStartPositionSeconds": track.positionSeconds,
            "startedAt": ISO8601DateFormatter().string(from: startedAt),
            "finishedAt": ISO8601DateFormatter().string(from: Date()), "reason": reason,
            "segmentFinalized": complete,
            "boundaryMethod": "Spotify playback observation; approximate, not sample-accurate track boundaries",
            "file": lastURL.lastPathComponent
        ]
        let metadataURL = URL(fileURLWithPath: lastURL.path + ".json")
        let metadata = try JSONSerialization.data(withJSONObject: manifest, options: [.prettyPrinted, .sortedKeys])
        try metadata.write(to: metadataURL, options: .withoutOverwriting)
        return lastURL
    }
    deinit { try? handle?.close() }
}

final class DebugWAVRecorder {
    private let renderer: OpaquePointer
    private let directory: URL
    private let sampleRate: Double
    private let captureDescription: String
    private let queue = DispatchQueue(label: "se.lbsa71.player.wav-writer", qos: .utility)
    private let changed: (RecordingSnapshot) -> Void
    private var timer: DispatchSourceTimer?
    private var file: FloatWAVFile?
    private var state = RecordingSnapshot()
    private let scratch = UnsafeMutablePointer<Float>.allocate(capacity: 8192 * 2)
    private var lastReport = Date.distantPast
    private var stopping = false
    private var lastPosition: Double?

    init(renderer: OpaquePointer, directory: URL, sampleRate: Double,
         captureDescription: String = "IEEE Float32, post-Player-gain", changed: @escaping (RecordingSnapshot) -> Void) {
        self.renderer = renderer; self.directory = directory; self.sampleRate = sampleRate; self.changed = changed
        self.captureDescription = captureDescription
        player_renderer_retain(renderer)
    }
    func start(track: RecordingTrack?) throws -> RecordingSnapshot {
        try queue.sync {
            if let track {
                file = try FloatWAVFile(directory: directory, track: track, sampleRate: sampleRate, captureDescription: captureDescription)
                guard player_record_start(renderer) else { throw AudioFailure("The renderer is already recording or is a probe.") }
                state.file = file!.temporaryURL.path
            }
            state.enabled = true
            lastPosition = track?.positionSeconds
            state.detail = track == nil ? "WAV recording armed; waiting for track metadata." : "Recording audio. Track boundaries follow supplied metadata."
            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(deadline: .now() + .milliseconds(50), repeating: .milliseconds(50))
            timer.setEventHandler { [weak self] in self?.tick() }
            self.timer = timer; timer.resume()
            return state
        }
    }
    func observe(_ track: RecordingTrack?) {
        queue.async { [self] in
            guard state.enabled, !stopping else { return }
            do {
                if let track, track.id == file?.track.id {
                    let restarted = track.positionSeconds + 2 < (lastPosition ?? track.positionSeconds)
                    lastPosition = track.positionSeconds
                    if !restarted { return }
                }
                if track == nil { player_record_stop(renderer) }
                try drain(through: player_record_written(renderer))
                guard player_record_dropped(renderer) == 0 else { throw AudioFailure("Recording buffer overflow or audio-format fault; partial WAV retained.") }
                if let file {
                    state.file = try file.finish(reason: track == nil ? "Paused or metadata unavailable" : "Observed track change", complete: true).path
                    state.savedFiles += 1; self.file = nil
                }
                if let track {
                    file = try FloatWAVFile(directory: directory, track: track, sampleRate: sampleRate, captureDescription: captureDescription)
                    state.file = file!.temporaryURL.path
                    // For a rotation the ring stays enabled, preserving all samples across file I/O.
                    if !player_record_start(renderer) { /* already active across rotation */ }
                    state.detail = "Recording rendered audio. Track boundaries are approximate."
                } else { state.detail = "WAV recording armed; waiting for a playing track on this Mac." }
                lastPosition = track?.positionSeconds
                state.frames = file?.frames ?? 0
                changed(state)
            } catch { fail(error) }
        }
    }
    func stop(reason: String) {
        queue.async { [self] in
            guard !stopping else { return }; stopping = true
            player_record_stop(renderer); timer?.cancel(); timer = nil
            do {
                try drain(through: player_record_written(renderer))
                if player_record_dropped(renderer) > 0 { throw AudioFailure("Recording buffer overflow or audio-format fault; partial WAV retained.") }
                if let file {
                    state.file = try file.finish(reason: reason, complete: true).path
                    state.frames = file.frames; state.savedFiles += 1; self.file = nil
                }
                state.enabled = false; state.finishing = false; state.detail = "WAV recording stopped. \(reason)"
                changed(state)
            } catch { fail(error) }
        }
    }
    func waitUntilStopped() { queue.sync {} }
    private func drain(through boundary: UInt64) throws {
        guard let file else { return }
        while true {
            let count = player_record_read(renderer, scratch, 8192, boundary)
            if count == 0 { break }
            try file.append(scratch, count: Int(count))
        }
        state.frames = file.frames
    }
    private func tick() {
        guard state.enabled else { return }
        do {
            try drain(through: player_record_written(renderer))
            guard player_record_dropped(renderer) == 0 else { throw AudioFailure("Recording buffer overflow or audio-format fault; partial WAV retained.") }
            if Date().timeIntervalSince(lastReport) >= 1 { changed(state); lastReport = Date() }
        } catch { fail(error) }
    }
    private func fail(_ error: Error) {
        player_record_stop(renderer); timer?.cancel(); timer = nil
        if let file {
            _ = try? file.finish(reason: error.localizedDescription, complete: false)
            state.file = file.lastURL.path
        }
        file = nil; state.enabled = false; state.finishing = false
        state.error = error.localizedDescription; state.detail = "WAV recording failed; audio playback is independent."
        changed(state)
    }
    deinit { timer?.cancel(); scratch.deallocate(); player_renderer_destroy(renderer) }
}
