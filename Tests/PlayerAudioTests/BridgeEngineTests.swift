import XCTest
import AVFAudio
import CoreAudio
import RealtimeAudio
@testable import PlayerAudio

final class BridgeEngineTests: XCTestCase {
    func testEngineRendersSignalAfterInitialSilence() throws {
        let renderer = try XCTUnwrap(player_renderer_create(44100,true))
        let forwarder = try XCTUnwrap(bridge_forwarder_create(renderer))
        defer { bridge_forwarder_destroy(forwarder); player_renderer_destroy(renderer) }
        let engine = AVAudioEngine()
        let format = try XCTUnwrap(AVAudioFormat(standardFormatWithSampleRate:44100,channels:2))
        let node = makeBridgeSourceNode(forwarder:forwarder,format:format)
        engine.attach(node); engine.connect(node,to:engine.mainMixerNode,format:format)
        try engine.enableManualRenderingMode(.offline,format:format,maximumFrameCount:512)
        bridge_monitor_enable(forwarder,true)
        try engine.start()
        defer { engine.stop(); bridge_monitor_enable(forwarder,false) }
        let output = try XCTUnwrap(AVAudioPCMBuffer(pcmFormat:format,frameCapacity:512))
        XCTAssertEqual(try engine.renderOffline(512,to:output),.success)
        var source = [Float](repeating:0.25,count:8192)
        var timestamp = AudioTimeStamp()
        source.withUnsafeMutableBytes { bytes in
            var input = AudioBufferList(mNumberBuffers:1,mBuffers:AudioBuffer(mNumberChannels:2,mDataByteSize:UInt32(bytes.count),mData:bytes.baseAddress))
            var empty = AudioBufferList(mNumberBuffers:0,mBuffers:AudioBuffer())
            _ = bridge_capture_callback(1,&timestamp,&input,&timestamp,&empty,&timestamp,UnsafeMutableRawPointer(forwarder))
        }
        XCTAssertEqual(try engine.renderOffline(512,to:output),.success)
        let samples = UnsafeBufferPointer(start:output.floatChannelData![0],count:Int(output.frameLength))
        XCTAssertGreaterThan(samples.map { abs($0) }.max() ?? 0,0.1)
        XCTAssertGreaterThan(bridge_monitor_stats(forwarder).signalFrames,0)
    }
}
