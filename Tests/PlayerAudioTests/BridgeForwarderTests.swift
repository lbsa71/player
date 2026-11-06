import XCTest
import CoreAudio
import RealtimeAudio

final class BridgeForwarderTests: XCTestCase {
    func testEndpointRecordingIsExactWithMonitoringOffMutedAndReset() throws {
        let renderer = try XCTUnwrap(player_renderer_create(44100,true))
        let forwarder = try XCTUnwrap(bridge_forwarder_create(renderer))
        defer { bridge_forwarder_destroy(forwarder); player_renderer_destroy(renderer) }
        XCTAssertTrue(player_record_start(renderer))
        var input: [Float] = (0..<1024).map { Float($0-512)/2048 }
        var output = [Float](repeating:0,count:1024)
        var recorded = [Float](repeating:0,count:1024)
        var timestamp = AudioTimeStamp()
        for block in 0..<100 {
            if block == 20 { bridge_monitor_enable(forwarder,true) }
            if block == 50 { bridge_monitor_gain(forwarder,0) }
            if block == 70 { bridge_monitor_enable(forwarder,false) }
            input.withUnsafeMutableBytes { source in
                output.withUnsafeMutableBytes { target in
                    var incoming = AudioBufferList(mNumberBuffers:1,mBuffers:AudioBuffer(mNumberChannels:2,mDataByteSize:UInt32(source.count),mData:source.baseAddress))
                    var outgoing = AudioBufferList(mNumberBuffers:1,mBuffers:AudioBuffer(mNumberChannels:2,mDataByteSize:UInt32(target.count),mData:target.baseAddress))
                    XCTAssertEqual(bridge_capture_callback(1,&timestamp,&incoming,&timestamp,&outgoing,&timestamp,UnsafeMutableRawPointer(forwarder)),noErr)
                    // The input-only client must not inject any signal into the endpoint mix.
                    XCTAssertTrue(target.bindMemory(to:Float.self).allSatisfy { $0 == 0 })
                    bridge_monitor_render(forwarder,&outgoing)
                }
            }
            let count = recorded.withUnsafeMutableBufferPointer { player_record_read(renderer,$0.baseAddress!,512,UInt64.max) }
            XCTAssertEqual(count,512)
            XCTAssertEqual(recorded.map(\.bitPattern),input.map(\.bitPattern))
        }
        XCTAssertEqual(player_renderer_stats(renderer).invalidBuffers,0)
        XCTAssertEqual(player_record_dropped(renderer),0)
    }
    func testMonitoringResetDoesNotReplayQueuedAudio() throws {
        let renderer = try XCTUnwrap(player_renderer_create(44100,true))
        let forwarder = try XCTUnwrap(bridge_forwarder_create(renderer))
        defer { bridge_forwarder_destroy(forwarder); player_renderer_destroy(renderer) }
        bridge_monitor_enable(forwarder,true)
        var input = [Float](repeating:0.5,count:4096)
        var output = [Float](repeating:0,count:4096)
        var time = AudioTimeStamp()
        input.withUnsafeMutableBytes { i in
            output.withUnsafeMutableBytes { o in
                var source = AudioBufferList(mNumberBuffers:1,mBuffers:AudioBuffer(mNumberChannels:2,mDataByteSize:UInt32(i.count),mData:i.baseAddress))
                var target = AudioBufferList(mNumberBuffers:1,mBuffers:AudioBuffer(mNumberChannels:2,mDataByteSize:UInt32(o.count),mData:o.baseAddress))
                _ = bridge_capture_callback(1,&time,&source,&time,&target,&time,UnsafeMutableRawPointer(forwarder))
                bridge_monitor_enable(forwarder,false)
                bridge_monitor_enable(forwarder,true)
                bridge_monitor_render(forwarder,&target)
            }
        }
        XCTAssertTrue(output.allSatisfy { $0 == 0 })
    }
}
