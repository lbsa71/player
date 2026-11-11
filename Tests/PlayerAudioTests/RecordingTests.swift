import XCTest
import CoreAudio
import AVFAudio
import RealtimeAudio
@testable import PlayerAudio

final class RecordingTests: XCTestCase {
    private final class Produced: @unchecked Sendable { var values: [Float] = [] }
    private func folder() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        addTeardownBlock { try FileManager.default.removeItem(at: url) }
        return url
    }
    private func track(_ name: String, position: Double = 0, duration: Double? = nil) -> RecordingTrack {
        RecordingTrack(id: name, album: "Album", artist: "Artist", name: name, positionSeconds: position,
                       expectedDurationSeconds: duration)
    }
    private func manifest(_ wav: URL) throws -> [String: Any] {
        let data = try Data(contentsOf: URL(fileURLWithPath: wav.path + ".json"))
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }
    private func appendSecond(to file: FloatWAVFile) throws {
        let samples = [Float](repeating: 0.25, count: 8000 * 2)
        try samples.withUnsafeBufferPointer { try file.append($0.baseAddress!, count: 8000) }
    }
    private func render(_ renderer: OpaquePointer, count: Int = 512, value: Float = 0.75) -> [Float] {
        var input = (0..<count * 2).map { $0 % 2 == 0 ? value : -value }
        var output = [Float](repeating: 0, count: count * 2)
        input.withUnsafeMutableBytes { source in
            output.withUnsafeMutableBytes { destination in
                var i = AudioBufferList(mNumberBuffers: 1, mBuffers: AudioBuffer(mNumberChannels: 2, mDataByteSize: UInt32(source.count), mData: source.baseAddress))
                var o = AudioBufferList(mNumberBuffers: 1, mBuffers: AudioBuffer(mNumberChannels: 2, mDataByteSize: UInt32(destination.count), mData: destination.baseAddress))
                player_renderer_process(renderer, &i, &o)
            }
        }
        return output
    }
    func testRecordingCopiesPostGainSamplesExactlyAndRingWraps() throws {
        let renderer = try XCTUnwrap(player_renderer_create(8000, true))
        defer { player_renderer_destroy(renderer) }
        XCTAssertTrue(player_record_start(renderer))
        var actual = [Float](repeating: 0, count: 1024)
        for block in 0..<100 {
            player_renderer_set_gain(renderer, block < 50 ? 0.37 : 0)
            let expected = render(renderer)
            let count = actual.withUnsafeMutableBufferPointer { player_record_read(renderer, $0.baseAddress!, 512, UInt64.max) }
            XCTAssertEqual(count, 512)
            XCTAssertEqual(actual.map(\.bitPattern), expected.map(\.bitPattern))
        }
        player_record_stop(renderer)
        _ = render(renderer)
        XCTAssertEqual(player_record_written(renderer), 51200)
        XCTAssertEqual(player_record_dropped(renderer), 0)
    }
    func testOverflowStopsRecordingWithoutOverwritingUnreadPrefix() throws {
        let renderer = try XCTUnwrap(player_renderer_create(8000, true))
        defer { player_renderer_destroy(renderer) }
        XCTAssertTrue(player_record_start(renderer))
        let first = render(renderer)
        for _ in 0..<100 { _ = render(renderer, value: 0.25) }
        XCTAssertGreaterThan(player_record_dropped(renderer), 0)
        var actual = [Float](repeating: 0, count: 1024)
        _ = actual.withUnsafeMutableBufferPointer { player_record_read(renderer, $0.baseAddress!, 512, UInt64.max) }
        XCTAssertEqual(actual, first)
    }
    func testConcurrentProducerConsumerPreserveSampleOrder() throws {
        let renderer = try XCTUnwrap(player_renderer_create(44100, true))
        defer { player_renderer_destroy(renderer) }
        XCTAssertTrue(player_record_start(renderer))
        let produced = Produced()
        let group = DispatchGroup()
        group.enter()
        DispatchQueue.global().async {
            for block in 0..<1000 {
                produced.values.append(contentsOf: self.render(renderer, value: Float(block % 97) / 100))
            }
            group.leave()
        }
        var received: [Float] = []
        var scratch = [Float](repeating: 0, count: 8192 * 2)
        while true {
            let count = scratch.withUnsafeMutableBufferPointer {
                player_record_read(renderer, $0.baseAddress!, 8192, UInt64.max)
            }
            received.append(contentsOf: scratch.prefix(Int(count) * 2))
            if group.wait(timeout: .now()) == .success,
               UInt64(received.count / 2) == player_record_written(renderer) { break }
        }
        XCTAssertFalse(received.isEmpty)
        XCTAssertEqual(received, Array(produced.values.prefix(received.count)))
    }
    func testWAVHeaderAndSamplesReadableByCoreAudio() throws {
        for rate in [44100.0, 48000.0] {
            let file = try FloatWAVFile(directory: folder(), track: track("Test"), sampleRate: rate)
            let samples: [Float] = [0, -0.0, 0.12345679, -0.9876543, 1.25, -1.25]
            try samples.withUnsafeBufferPointer { try file.append($0.baseAddress!, count: 3) }
            let url = try file.finish(reason: "Test", complete: true)
            let wav = try AVAudioFile(forReading: url)
            XCTAssertEqual(wav.length, 3)
            XCTAssertEqual(wav.fileFormat.sampleRate, rate)
            XCTAssertEqual(wav.fileFormat.commonFormat, .pcmFormatFloat32)
            let buffer = try XCTUnwrap(AVAudioPCMBuffer(pcmFormat: wav.processingFormat, frameCapacity: 3))
            try wav.read(into: buffer)
            for frame in 0..<3 {
                XCTAssertEqual(buffer.floatChannelData![0][frame].bitPattern, samples[frame * 2].bitPattern)
                XCTAssertEqual(buffer.floatChannelData![1][frame].bitPattern, samples[frame * 2 + 1].bitPattern)
            }
            XCTAssertTrue(FileManager.default.fileExists(atPath: url.path + ".json"))
        }
    }
    func testNamesAreSafeAndExistingFilesAreNeverOverwritten() throws {
        let directory = try folder()
        let metadata = RecordingTrack(id: "x", album: "../Album", artist: "A:B", name: "Song/Live", positionSeconds: 5)
        var urls: [URL] = []
        for _ in 0..<2 {
            let file = try FloatWAVFile(directory: directory, track: metadata, sampleRate: 44100)
            urls.append(try file.finish(reason: "Test", complete: true))
        }
        XCTAssertNotEqual(urls[0], urls[1])
        XCTAssertEqual(urls[0].deletingLastPathComponent().path, directory.path)
        XCTAssertEqual(urls[1].lastPathComponent, ".._Album - A_B - Song_Live (2).wav")
        let huge = RecordingTrack(id: "x", album: String(repeating: "å", count: 300), artist: "B", name: "C", positionSeconds: 0)
        XCTAssertLessThanOrEqual(FloatWAVFile.filenameBase(huge).utf8.count, 200)
    }
    func testRotationPreservesEveryFrameAndRetainsStorageAfterHALRelease() throws {
        let directory = try folder()
        let renderer = try XCTUnwrap(player_renderer_create(44100, true))
        let recorder = DebugWAVRecorder(renderer: renderer, directory: directory, sampleRate: 44100) { _ in }
        _ = try recorder.start(track: track("First"))
        let first = render(renderer)
        recorder.observe(track("Second")); recorder.waitUntilStopped()
        let second = render(renderer)
        // Simulate releasing HAL's ownership before the writer finishes.
        player_renderer_destroy(renderer)
        recorder.stop(reason: "Test route release"); recorder.waitUntilStopped()
        for (name, expected) in [("First", first), ("Second", second)] {
            let data = try Data(contentsOf: directory.appendingPathComponent("Album - Artist - \(name).wav"))
            let bytes = expected.withUnsafeBytes { Data($0) }
            XCTAssertEqual(data.dropFirst(58), bytes)
        }
    }
    func testPauseAndRestartCreateSeparateSegments() throws {
        let directory = try folder()
        let renderer = try XCTUnwrap(player_renderer_create(44100, true))
        defer { player_renderer_destroy(renderer) }
        let recorder = DebugWAVRecorder(renderer: renderer, directory: directory, sampleRate: 44100) { _ in }
        _ = try recorder.start(track: track("Track", position: 20))
        _ = render(renderer)
        recorder.observe(nil); recorder.waitUntilStopped()
        _ = render(renderer) // paused samples must not enter a later segment
        recorder.observe(track("Track", position: 20)); recorder.waitUntilStopped()
        _ = render(renderer)
        recorder.observe(track("Track", position: 0)); recorder.waitUntilStopped()
        _ = render(renderer)
        recorder.stop(reason: "Test"); recorder.waitUntilStopped()
        let wavs = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil).filter { $0.pathExtension == "wav" }
        XCTAssertEqual(wavs.count, 3)
        for url in wavs { XCTAssertEqual(try AVAudioFile(forReading: url).length, 512) }
    }
    func testWAVSizeLimitFailsWithoutReadingBeyondInput() throws {
        let file = try FloatWAVFile(directory: folder(), track: track("Limit"), sampleRate: 44100)
        var value: Float = 0
        XCTAssertThrowsError(try file.append(&value, count: Int(UInt32.max) / 8))
        let url = try file.finish(reason: "Size limit", complete: false)
        XCTAssertTrue(url.lastPathComponent.hasSuffix(".partial.wav"))
        XCTAssertEqual(try AVAudioFile(forReading: url).length, 0)
    }
    func testArmingWithoutMetadataCreatesOnlyTheSubsequentTrack() throws {
        let directory = try folder()
        let renderer = try XCTUnwrap(player_renderer_create(44100, true))
        defer { player_renderer_destroy(renderer) }
        let recorder = DebugWAVRecorder(renderer:renderer,directory:directory,sampleRate:44100) { _ in }
        let state = try recorder.start(track:nil)
        XCTAssertTrue(state.enabled); XCTAssertTrue(state.file.isEmpty)
        _ = render(renderer)
        recorder.observe(track("Started")); recorder.waitUntilStopped()
        let expected = render(renderer)
        recorder.stop(reason:"Test"); recorder.waitUntilStopped()
        let wav = try Data(contentsOf:directory.appendingPathComponent("Album - Artist - Started.wav"))
        XCTAssertEqual(wav.dropFirst(58),expected.withUnsafeBytes { Data($0) })
    }
    func testRecorderPersistsObservedCoverageOnNaturalTrackChange() throws {
        let directory = try folder()
        let renderer = try XCTUnwrap(player_renderer_create(8000, true))
        defer { player_renderer_destroy(renderer) }
        let recorder = DebugWAVRecorder(renderer: renderer, directory: directory, sampleRate: 8000) { _ in }
        _ = try recorder.start(track: track("First", duration: 3))
        for second in 1...3 {
            _ = render(renderer, count: 8000)
            recorder.observe(track("First", position: Double(second), duration: 3))
            recorder.waitUntilStopped()
        }
        recorder.observe(track("Second", duration: 5)); recorder.waitUntilStopped()
        recorder.stop(reason: "Test"); recorder.waitUntilStopped()
        let metadata = try manifest(directory.appendingPathComponent("Album - Artist - First.wav"))
        XCTAssertEqual(metadata["trackID"] as? String, "First")
        XCTAssertNil(metadata["assetID"])
        XCTAssertEqual(metadata["recordingSchemaVersion"] as? Int, 2)
        XCTAssertEqual(metadata["completionPolicy"] as? String, "observed-v1")
        XCTAssertEqual(metadata["expectedDurationSeconds"] as? Double, 3)
        XCTAssertEqual(metadata["observedStartPositionSeconds"] as? Double, 0)
        XCTAssertEqual(metadata["observedEndPositionSeconds"] as? Double, 3)
        XCTAssertEqual(metadata["durationSeconds"] as? Double, 3)
        XCTAssertEqual(metadata["coverageContinuous"] as? Bool, true)
    }
    func testSkippedTrackFinalizesPriorCaptureAndResumesWithoutSkippedAudio() throws {
        let directory = try folder()
        let renderer = try XCTUnwrap(player_renderer_create(8000, true))
        defer { player_renderer_destroy(renderer) }
        let recorder = DebugWAVRecorder(renderer: renderer, directory: directory, sampleRate: 8000) { _ in }
        _ = try recorder.start(track: track("First", duration: 1))
        _ = render(renderer, count: 8000)
        recorder.observe(track("First", position: 1, duration: 1)); recorder.waitUntilStopped()
        recorder.observe(track("Skipped", duration: 10), capture: false); recorder.waitUntilStopped()
        let first = try manifest(directory.appendingPathComponent("Album - Artist - First.wav"))
        XCTAssertEqual(first["reason"] as? String, "Observed track change")
        XCTAssertEqual(first["coverageContinuous"] as? Bool, true)
        let writtenBeforeSkip = player_record_written(renderer)
        _ = render(renderer, count: 8000, value: 0.1)
        recorder.observe(track("Skipped", position: 1, duration: 10), capture: false); recorder.waitUntilStopped()
        XCTAssertEqual(player_record_written(renderer), writtenBeforeSkip)
        recorder.observe(track("Resumed", duration: 1)); recorder.waitUntilStopped()
        let resumed = render(renderer, count: 8000, value: 0.5)
        recorder.stop(reason: "Test"); recorder.waitUntilStopped()
        let wavs = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "wav" }.map(\.lastPathComponent).sorted()
        XCTAssertEqual(wavs, ["Album - Artist - First.wav", "Album - Artist - Resumed.wav"])
        let resumedFile = try Data(contentsOf: directory.appendingPathComponent("Album - Artist - Resumed.wav"))
        XCTAssertEqual(resumedFile.dropFirst(58), resumed.withUnsafeBytes { Data($0) })
    }
    func testSuspendingCurrentTrackCannotClaimCompletionAndCanResumeSameTrack() throws {
        let directory = try folder()
        let renderer = try XCTUnwrap(player_renderer_create(8000, true))
        defer { player_renderer_destroy(renderer) }
        let recorder = DebugWAVRecorder(renderer: renderer, directory: directory, sampleRate: 8000) { _ in }
        _ = try recorder.start(track: track("Track", duration: 1))
        _ = render(renderer, count: 8000)
        recorder.observe(track("Track", position: 1, duration: 1), capture: false); recorder.waitUntilStopped()
        let suspended = try manifest(directory.appendingPathComponent("Album - Artist - Track.wav"))
        XCTAssertEqual(suspended["reason"] as? String, "Capture suspended")
        XCTAssertEqual(suspended["coverageContinuous"] as? Bool, false)
        _ = render(renderer, count: 8000, value: 0.1)
        recorder.observe(track("Track", duration: 1)); recorder.waitUntilStopped()
        let resumed = render(renderer, count: 8000, value: 0.5)
        recorder.stop(reason: "Test"); recorder.waitUntilStopped()
        let resumedFile = try Data(contentsOf: directory.appendingPathComponent("Album - Artist - Track (2).wav"))
        XCTAssertEqual(resumedFile.dropFirst(58), resumed.withUnsafeBytes { Data($0) })
    }
    func testCoverageRejectsSeekJumpsAndDurationChanges() throws {
        let cases: [(String, Double, Double)] = [
            ("Forward seek", 8, 10),
            ("Backward seek", 0.5, 10),
            ("Duration changed", 2, 30)
        ]
        for (name, position, duration) in cases {
            let file = try FloatWAVFile(directory: folder(), track: track(name, duration: 10), sampleRate: 8000)
            try appendSecond(to: file)
            file.observe(positionSeconds: 1, expectedDurationSeconds: 10)
            try appendSecond(to: file)
            file.observe(positionSeconds: position, expectedDurationSeconds: duration)
            let metadata = try manifest(file.finish(reason: "Observed track change", complete: true))
            XCTAssertEqual(metadata["coverageContinuous"] as? Bool, false, name)
            XCTAssertEqual(metadata["observedEndPositionSeconds"] as? Double, position, name)
        }
    }
    func testCoverageKeepsActualLastObservationAndOmitsUnknownDuration() throws {
        let file = try FloatWAVFile(directory: folder(), track: track("Unobserved"), sampleRate: 8000)
        try appendSecond(to: file)
        let metadata = try manifest(file.finish(reason: "Observed track change", complete: true))
        XCTAssertEqual(metadata["observedEndPositionSeconds"] as? Double, 0)
        XCTAssertEqual(metadata["durationSeconds"] as? Double, 1)
        XCTAssertNil(metadata["expectedDurationSeconds"])
    }
    func testStoppedPausedAndFailedSegmentsCannotClaimContinuousCompletion() throws {
        for (reason, complete) in [("Stopped by user", true), ("Paused or metadata unavailable", true),
                                   ("Observed track change", false)] {
            let file = try FloatWAVFile(directory: folder(), track: track("Track", duration: 1), sampleRate: 8000)
            try appendSecond(to: file)
            file.observe(positionSeconds: 1, expectedDurationSeconds: 1)
            let metadata = try manifest(file.finish(reason: reason, complete: complete))
            XCTAssertEqual(metadata["coverageContinuous"] as? Bool, false, reason)
        }
    }
    func testInvalidInitialAndLaterPlaybackMetadataCannotClaimCoverage() throws {
        for position in [-1.0, .infinity, .nan] {
            XCTAssertThrowsError(try FloatWAVFile(directory: folder(), track: track("Invalid", position: position), sampleRate: 8000))
        }
        let file = try FloatWAVFile(directory: folder(), track: track("Invalid observation", duration: 1), sampleRate: 8000)
        try appendSecond(to: file)
        file.observe(positionSeconds: .nan, expectedDurationSeconds: 1)
        let metadata = try manifest(file.finish(reason: "Observed track change", complete: true))
        XCTAssertEqual(metadata["coverageContinuous"] as? Bool, false)
    }
}
