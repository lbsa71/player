import XCTest
import CoreAudio
import RealtimeAudio
@testable import PlayerAudio

final class ProcessTapEndpointTests: XCTestCase {
    private func stereo(_ channels: UInt32 = 2) -> AudioStreamBasicDescription {
        AudioStreamBasicDescription(mSampleRate:44100,mFormatID:kAudioFormatLinearPCM,
            mFormatFlags:kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket:channels*4,mFramesPerPacket:1,mBytesPerFrame:channels*4,
            mChannelsPerFrame:channels,mBitsPerChannel:32,mReserved:0)
    }
    func testTapSelectionUsesSemanticTerminalAndLogicalChannelInsteadOfReusedID() throws {
        // Actual HAL repro: an original tap ID was reused for the Line input.
        // Current semantic topology selects the new Unknown tap at channels3–4.
        let selected = try ProcessTapSession.tapInputSelection(endpointInputTerminals:[kAudioStreamTerminalTypeLine],
            aggregateTerminals:[kAudioStreamTerminalTypeLine,kAudioStreamTerminalTypeUnknown],
            startingChannels:[1,3],formats:[stereo(),stereo()],bufferChannels:[2,2])
        XCTAssertEqual(selected,.init(streamIndex:1,bufferIndex:1))
        // Enumeration can differ from logical buffer order. Preserve both indices.
        let reversed = try ProcessTapSession.tapInputSelection(endpointInputTerminals:[kAudioStreamTerminalTypeLine],
            aggregateTerminals:[kAudioStreamTerminalTypeUnknown,kAudioStreamTerminalTypeLine],
            startingChannels:[3,1],formats:[stereo(),stereo()],bufferChannels:[2,2])
        XCTAssertEqual(reversed,.init(streamIndex:0,bufferIndex:1))
    }
    func testAmbiguousOrChangedTapTopologyIsRejected() {
        func rejects(_ native: [UInt32] = [kAudioStreamTerminalTypeLine],
                     _ terms: [UInt32] = [kAudioStreamTerminalTypeLine,kAudioStreamTerminalTypeUnknown],
                     _ starts: [UInt32] = [1,3], _ formats: [AudioStreamBasicDescription]? = nil,
                     _ buffers: [UInt32] = [2,2]) {
            XCTAssertThrowsError(try ProcessTapSession.tapInputSelection(endpointInputTerminals:native,
                aggregateTerminals:terms,startingChannels:starts,formats:formats ?? [stereo(),stereo()],bufferChannels:buffers))
        }
        rejects([kAudioStreamTerminalTypeUnknown])
        rejects([kAudioStreamTerminalTypeLine],[0,0])
        rejects([kAudioStreamTerminalTypeLine],[kAudioStreamTerminalTypeLine,kAudioStreamTerminalTypeMicrophone])
        rejects([kAudioStreamTerminalTypeLine],[kAudioStreamTerminalTypeLine,0],[1,1])
        rejects([kAudioStreamTerminalTypeLine],[kAudioStreamTerminalTypeLine,0],[1,2])
        rejects([kAudioStreamTerminalTypeLine],[kAudioStreamTerminalTypeLine,0],[1,3],[stereo(),stereo(4)])
        rejects([kAudioStreamTerminalTypeLine],[kAudioStreamTerminalTypeLine,0],[1,3],nil,[1,1,2])
        var planar = stereo(); planar.mFormatFlags |= kAudioFormatFlagIsNonInterleaved
        rejects([kAudioStreamTerminalTypeLine],[kAudioStreamTerminalTypeLine,0],[1,3],[stereo(),planar])
    }
    func testOnlyProvenTapBufferReachesOutputAndNullLoopbackIsAllowed() throws {
        for selected: UInt32 in [0,1] {
            let renderer = try XCTUnwrap(player_renderer_create(44100,true))
            defer { player_renderer_destroy(renderer) }
            XCTAssertTrue(player_renderer_select_input_buffer(renderer,selected,2))
            player_renderer_set_gain(renderer,1)
            let input = AudioBufferList.allocate(maximumBuffers:2)
            let output = AudioBufferList.allocate(maximumBuffers:1)
            let tap = UnsafeMutablePointer<Float>.allocate(capacity:1024)
            let loopback = UnsafeMutablePointer<Float>.allocate(capacity:1024)
            let destination = UnsafeMutablePointer<Float>.allocate(capacity:1024)
            defer { input.unsafeMutablePointer.deallocate(); output.unsafeMutablePointer.deallocate(); tap.deallocate(); loopback.deallocate(); destination.deallocate() }
            for f in 0..<512 { tap[f*2] = 0.25; tap[f*2+1] = -0.5 }
            loopback.initialize(repeating:99,count:1024)
            input.unsafeMutablePointer.pointee.mNumberBuffers = 2
            input[Int(selected)] = AudioBuffer(mNumberChannels:2,mDataByteSize:4096,mData:tap)
            input[1-Int(selected)] = AudioBuffer(mNumberChannels:2,mDataByteSize:4096,mData:loopback)
            output.unsafeMutablePointer.pointee.mNumberBuffers = 1
            output[0] = AudioBuffer(mNumberChannels:2,mDataByteSize:4096,mData:destination)
            player_renderer_process(renderer,input.unsafePointer,output.unsafeMutablePointer)
            XCTAssertEqual(destination[1022],0.25,accuracy:0.00001)
            XCTAssertEqual(destination[1023],-0.5,accuracy:0.00001)
            // HAL disables the duplex endpoint's input only for this IOProc.
            input[1-Int(selected)].mData = nil
            player_renderer_process(renderer,input.unsafePointer,output.unsafeMutablePointer)
            XCTAssertEqual(destination[0],0.25)
            XCTAssertEqual(destination[1],-0.5)
            XCTAssertEqual(player_renderer_stats(renderer).invalidBuffers,0)
            XCTAssertFalse(player_renderer_select_input_buffer(renderer,1-selected,2))
            // A changed buffer topology cannot accidentally select loopback.
            input.unsafeMutablePointer.pointee.mNumberBuffers = 1
            player_renderer_process(renderer,input.unsafePointer,output.unsafeMutablePointer)
            XCTAssertTrue((0..<1024).allSatisfy { destination[$0] == 0 })
            XCTAssertEqual(player_renderer_stats(renderer).invalidBuffers,1)
        }
    }
    func testInvalidBufferSelectionIsRejectedBeforeIO() throws {
        let renderer = try XCTUnwrap(player_renderer_create(44100,true))
        defer { player_renderer_destroy(renderer) }
        XCTAssertFalse(player_renderer_select_input_buffer(renderer,0,0))
        XCTAssertFalse(player_renderer_select_input_buffer(renderer,2,2))
    }
}
