import XCTest
import CoreAudio
import RealtimeAudio
@testable import PlayerAudio

private final class Buffers {
    let list: UnsafeMutableAudioBufferListPointer
    let storage: [UnsafeMutablePointer<Float>]
    init(planar: Bool, frames: Int, left: Float = 0, right: Float = 0) {
        list = AudioBufferList.allocate(maximumBuffers: planar ? 2 : 1)
        list.unsafeMutablePointer.pointee.mNumberBuffers = planar ? 2 : 1
        if planar {
            storage = (0..<2).map { _ in .allocate(capacity: max(frames, 1)) }
            for c in 0..<2 {
                storage[c].initialize(repeating: c == 0 ? left : right, count: frames)
                list[c] = AudioBuffer(mNumberChannels: 1, mDataByteSize: UInt32(frames * 4), mData: storage[c])
            }
        } else {
            let data = UnsafeMutablePointer<Float>.allocate(capacity: max(frames * 2, 1))
            storage = [data]
            for frame in 0..<frames { data[frame * 2] = left; data[frame * 2 + 1] = right }
            list[0] = AudioBuffer(mNumberChannels: 2, mDataByteSize: UInt32(frames * 8), mData: data)
        }
    }
    deinit { storage.forEach { $0.deallocate() }; list.unsafeMutablePointer.deallocate() }
}

final class RendererTests: XCTestCase {
    func testStereoLayoutsAndSampleRatesPreserveChannelOrder() throws {
        for rate in [44100.0, 48000.0] {
            for inputPlanar in [false, true] {
                for outputPlanar in [false, true] {
                    let renderer = try XCTUnwrap(player_renderer_create(rate, true))
                    defer { player_renderer_destroy(renderer) }
                    let source = Buffers(planar: inputPlanar, frames: 512, left: 0.8, right: -0.4)
                    let output = Buffers(planar: outputPlanar, frames: 512)
                    player_renderer_set_gain(renderer, 0.5)
                    player_renderer_process(renderer, source.list.unsafePointer, output.list.unsafeMutablePointer)
                    let left = output.storage[0][outputPlanar ? 511 : 1022]
                    let right = output.storage[outputPlanar ? 1 : 0][outputPlanar ? 511 : 1023]
                    XCTAssertEqual(left, 0.4, accuracy: 0.00001)
                    XCTAssertEqual(right, -0.2, accuracy: 0.00001)
                    let stats = player_renderer_stats(renderer)
                    XCTAssertEqual(stats.frames, 512)
                    XCTAssertEqual(stats.nonzeroFrames, 512)
                    XCTAssertEqual(stats.invalidBuffers, 0)
                }
            }
        }
    }
    func testProbeMeasuresInputWithoutDuplicatePlayback() throws {
        let renderer = try XCTUnwrap(player_renderer_create(48000, false))
        defer { player_renderer_destroy(renderer) }
        let source = Buffers(planar: false, frames: 512, left: 0.5, right: 0.25)
        let output = Buffers(planar: false, frames: 512, left: 99, right: 99)
        player_renderer_process(renderer, source.list.unsafePointer, output.list.unsafeMutablePointer)
        XCTAssertTrue((0..<1024).allSatisfy { output.storage[0][$0] == 0 })
        XCTAssertEqual(player_renderer_stats(renderer).nonzeroFrames, 512)
    }
    func testMismatchedBuffersRenderSilenceAndCountFault() throws {
        let renderer = try XCTUnwrap(player_renderer_create(44100, true))
        defer { player_renderer_destroy(renderer) }
        let source = Buffers(planar: false, frames: 64, left: 0.5, right: 0.5)
        let output = Buffers(planar: false, frames: 128, left: 99, right: 99)
        player_renderer_process(renderer, source.list.unsafePointer, output.list.unsafeMutablePointer)
        XCTAssertTrue((0..<256).allSatisfy { output.storage[0][$0] == 0 })
        XCTAssertEqual(player_renderer_stats(renderer).invalidBuffers, 1)
        XCTAssertEqual(player_renderer_stats(renderer).frames, 0)
    }
    func testNonfiniteSamplesAndGainCannotReachOutput() throws {
        let renderer = try XCTUnwrap(player_renderer_create(48000, true))
        defer { player_renderer_destroy(renderer) }
        let source = Buffers(planar: false, frames: 512, left: .nan, right: .infinity)
        let output = Buffers(planar: false, frames: 512)
        player_renderer_set_gain(renderer, .nan)
        player_renderer_process(renderer, source.list.unsafePointer, output.list.unsafeMutablePointer)
        XCTAssertTrue((0..<1024).allSatisfy { output.storage[0][$0] == 0 })
        XCTAssertEqual(player_renderer_stats(renderer).nonzeroFrames, 0)
    }
    func testGainRampsToZeroAndDoesNotReplayOldAudio() throws {
        let renderer = try XCTUnwrap(player_renderer_create(48000, true))
        defer { player_renderer_destroy(renderer) }
        let source = Buffers(planar: false, frames: 512, left: 1, right: 1)
        let silence = Buffers(planar: false, frames: 512)
        let output = Buffers(planar: false, frames: 512)
        player_renderer_set_gain(renderer, 1)
        player_renderer_process(renderer, source.list.unsafePointer, output.list.unsafeMutablePointer)
        XCTAssertGreaterThan(output.storage[0][0], 0)
        XCTAssertLessThan(output.storage[0][0], 0.01)
        player_renderer_set_gain(renderer, 0)
        player_renderer_process(renderer, source.list.unsafePointer, output.list.unsafeMutablePointer)
        XCTAssertEqual(output.storage[0][1023], 0)
        player_renderer_set_gain(renderer, 1)
        player_renderer_process(renderer, silence.list.unsafePointer, output.list.unsafeMutablePointer)
        XCTAssertTrue((0..<1024).allSatisfy { output.storage[0][$0] == 0 })
    }
    func testFormatNegotiationRejectsUnexpectedRateAndIntegerPCM() throws {
        var format = AudioStreamBasicDescription(mSampleRate: 44100, mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 8, mFramesPerPacket: 1, mBytesPerFrame: 8, mChannelsPerFrame: 2,
            mBitsPerChannel: 32, mReserved: 0)
        XCTAssertNoThrow(try ProcessTapSession.validateFormats([format], [format], expectedRate: 44100))
        XCTAssertThrowsError(try ProcessTapSession.validateFormats([format], [format], expectedRate: 48000))
        format.mFormatFlags = kAudioFormatFlagIsSignedInteger
        XCTAssertThrowsError(try ProcessTapSession.validateFormats([format], [format], expectedRate: 44100))
    }
    func testPartialStartAndRepeatedCleanupAreSafe() throws {
        let output = OutputDevice(objectID: 0, uid: "test", name: "Test", sampleRate: 48000,
                                  inputChannels: 0, outputChannels: 2, transport: 0)
        let session = ProcessTapSession(output: output, sources: [])
        XCTAssertThrowsError(try session.start(probe: true, gain: 0.5))
        XCTAssertTrue(session.stop().isEmpty)
        XCTAssertTrue(session.stop().isEmpty)
    }
}
