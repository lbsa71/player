import Foundation
import CoreAudio
import RealtimeAudio

public struct RouteStats {
    public var callbacks: UInt64 = 0
    public var frames: UInt64 = 0
    public var nonzeroFrames: UInt64 = 0
    public var invalidBuffers: UInt64 = 0
    public var peak: Float = 0
}

// Owned exclusively by AudioRouteController's serial queue. The callback sees only C state.
final class ProcessTapSession {
    private var tap: AudioObjectID = 0
    private var aggregate: AudioObjectID = 0
    private var ioProc: AudioDeviceIOProcID?
    private var renderer: OpaquePointer?
    private var started = false
    private(set) var formatDescription = ""
    let output: OutputDevice
    let sources: [AudioSource]
    var recordingRenderer: OpaquePointer? { started ? renderer : nil }

    init(output: OutputDevice, sources: [AudioSource]) {
        self.output = output
        self.sources = sources
    }

    func start(probe: Bool, gain: Float, progress: (String) -> Void = { _ in }) throws {
        guard !sources.isEmpty else { throw AudioFailure("Open Spotify and play a track on this Mac first.") }
        let endpoint = output.uid == AudioBridgeEndpoint.uid
        guard output.supportsPrototype || (endpoint && output.inputChannels == 2 && output.outputChannels == 2) else {
            throw AudioFailure("This prototype requires a stereo output-only device. Use the Mac headphone jack or an output-only DAC; duplex interfaces and multichannel devices await validation.")
        }
        do {
            progress("Creating a private Spotify-only process tap.")
            let description = CATapDescription(stereoMixdownOfProcesses: sources.map(\.objectID))
            description.name = "Player · Spotify only"
            description.isPrivate = true
            description.muteBehavior = endpoint ? .mutedWhenTapped : (probe ? .unmuted : .mutedWhenTapped)
            try checked(AudioHardwareCreateProcessTap(description, &tap), "Create Spotify process tap")
            let tapFormat = try HAL.value(tap, kAudioTapPropertyFormat, initial: AudioStreamBasicDescription())
            let tapUID = try HAL.string(tap, kAudioTapPropertyUID)
            progress("Tap format: \(Int(tapFormat.mSampleRate)) Hz, \(tapFormat.mChannelsPerFrame) channels. Creating the private output aggregate.")
            let composition: [String: Any] = [
                kAudioAggregateDeviceNameKey: "Player private route",
                kAudioAggregateDeviceUIDKey: "se.lbsa71.player.route.\(UUID().uuidString)",
                kAudioAggregateDeviceIsPrivateKey: true,
                kAudioAggregateDeviceIsStackedKey: false,
                kAudioAggregateDeviceMainSubDeviceKey: output.uid,
                kAudioAggregateDeviceSubDeviceListKey: [[kAudioSubDeviceUIDKey: output.uid]],
                kAudioAggregateDeviceTapListKey: [[
                    kAudioSubTapUIDKey: tapUID,
                    kAudioSubTapDriftCompensationKey: true,
                    kAudioSubTapDriftCompensationQualityKey: kAudioAggregateDriftCompensationHighQuality
                ]]
            ]
            try checked(AudioHardwareCreateAggregateDevice(composition as CFDictionary, &aggregate), "Create private aggregate")
            // A newly created aggregate can retain a default rate different from its clock device.
            // Set only our private aggregate to the already selected hardware rate.
            var rate = output.sampleRate
            var rateAddress = HAL.address(kAudioDevicePropertyNominalSampleRate)
            try checked(AudioObjectSetPropertyData(aggregate, &rateAddress, 0, nil,
                UInt32(MemoryLayout<Double>.size), &rate), "Negotiate aggregate sample rate")
            let inputs = try HAL.formats(aggregate, scope: kAudioObjectPropertyScopeInput)
            let outputs = try HAL.formats(aggregate, scope: kAudioObjectPropertyScopeOutput)
            var selection: TapInputSelection?
            if endpoint {
                let nativeInputs = try HAL.ids(output.objectID,kAudioDevicePropertyStreams,scope:kAudioObjectPropertyScopeInput)
                let inputStreams = try HAL.ids(aggregate,kAudioDevicePropertyStreams,scope:kAudioObjectPropertyScopeInput)
                let selected = try Self.tapInputSelection(
                    endpointInputTerminals:nativeInputs.map { try HAL.value($0,kAudioStreamPropertyTerminalType,initial:UInt32(0)) },
                    aggregateTerminals:inputStreams.map { try HAL.value($0,kAudioStreamPropertyTerminalType,initial:UInt32(0)) },
                    startingChannels:inputStreams.map { try HAL.value($0,kAudioStreamPropertyStartingChannel,initial:UInt32(0)) },
                    formats:inputs,bufferChannels:inputBufferChannels())
                selection = selected
                try Self.validateFormats([inputs[selected.streamIndex]], outputs, expectedRate: output.sampleRate)
            } else { try Self.validateFormats(inputs, outputs, expectedRate: output.sampleRate) }
            formatDescription = "Tap \(Int(tapFormat.mSampleRate)) Hz → output \(Int(output.sampleRate)) Hz · Float32 stereo · hardware clock / tap drift compensation"
            progress(formatDescription)
            guard let storage = player_renderer_create(output.sampleRate, endpoint || !probe) else {
                throw AudioFailure("Could not allocate the renderer.")
            }
            renderer = storage
            player_renderer_set_gain(storage, endpoint ? 1 : gain)
            if let selection {
                guard player_renderer_select_input_buffer(storage,selection.bufferIndex,UInt32(inputs.count)) else {
                    throw AudioFailure("Cannot select the proven tap input buffer.")
                }
            }
            progress("Creating the audio callback. macOS may wait here for you to allow Player system audio recording.")
            try checked(AudioDeviceCreateIOProcID(aggregate, player_audio_callback, UnsafeMutableRawPointer(storage), &ioProc), "Create audio callback")
            if let selection {
                try selectInputStream(UInt32(selection.streamIndex),count:UInt32(inputs.count))
                formatDescription = "Spotify tap → Audio Bridge · \(Int(output.sampleRate)) Hz Float32 stereo · independent of the Mac’s default output"
            }
            progress("Starting the audio device. Waiting for captured samples.")
            try checked(AudioDeviceStart(aggregate, ioProc), "Start audio capture (check system audio recording permission)")
            started = true
        } catch {
            let cleanup = stop()
            if !cleanup.isEmpty { throw AudioFailure("\(error.localizedDescription) Cleanup: \(cleanup.joined(separator: "; "))") }
            throw error
        }
    }

    struct TapInputSelection: Equatable {
        let streamIndex: Int
        let bufferIndex: UInt32
    }
    /// The endpoint's public driver contract exposes one Line input. The other
    /// aggregate input is the single Unknown-terminal process tap. Stream IDs
    /// are not semantic identities: HAL reuses them when composition changes.
    /// StartingChannel maps that stream to its actual IOProc buffer independently
    /// of the enumeration index used by IOProcStreamUsage.
    static func tapInputSelection(endpointInputTerminals: [UInt32], aggregateTerminals: [UInt32],
                                  startingChannels: [UInt32], formats: [AudioStreamBasicDescription],
                                  bufferChannels: [UInt32]) throws -> TapInputSelection {
        guard endpointInputTerminals == [kAudioStreamTerminalTypeLine],
              aggregateTerminals.count == 2,
              aggregateTerminals.filter({ $0 == kAudioStreamTerminalTypeLine }).count == 1,
              aggregateTerminals.filter({ $0 == kAudioStreamTerminalTypeUnknown }).count == 1,
              let tapIndex = aggregateTerminals.firstIndex(of:kAudioStreamTerminalTypeUnknown),
              startingChannels.count == 2, Set(startingChannels) == [1,3],
              formats.count == 2, bufferChannels == [2,2],
              formats.allSatisfy({ $0.mChannelsPerFrame == 2 && $0.mFormatFlags & kAudioFormatFlagIsNonInterleaved == 0 }) else {
            throw AudioFailure("Cannot distinguish the Spotify tap from Audio Bridge loopback; refusing an ambiguous input topology.")
        }
        return TapInputSelection(streamIndex:tapIndex,bufferIndex:(startingChannels[tapIndex]-1)/2)
    }
    private func inputBufferChannels() throws -> [UInt32] {
        var address = HAL.address(kAudioDevicePropertyStreamConfiguration,kAudioObjectPropertyScopeInput)
        var size: UInt32 = 0
        try checked(AudioObjectGetPropertyDataSize(aggregate,&address,0,nil,&size),"Read aggregate input layout size")
        guard size >= MemoryLayout<AudioBufferList>.size else { throw AudioFailure("Aggregate input layout is empty.") }
        let storage = UnsafeMutableRawPointer.allocate(byteCount:Int(size),alignment:MemoryLayout<AudioBufferList>.alignment)
        defer { storage.deallocate() }
        try checked(AudioObjectGetPropertyData(aggregate,&address,0,nil,&size,storage),"Read aggregate input layout")
        let list = UnsafeMutableAudioBufferListPointer(storage.assumingMemoryBound(to:AudioBufferList.self))
        return list.map(\.mNumberChannels)
    }
    private func selectInputStream(_ selected: UInt32, count: UInt32) throws {
        guard let ioProc, selected < count else { throw AudioFailure("Invalid input stream selection.") }
        let size = MemoryLayout<AudioHardwareIOProcStreamUsage>.size + max(0,Int(count)-1)*MemoryLayout<UInt32>.size
        let storage = UnsafeMutableRawPointer.allocate(byteCount:size,alignment:MemoryLayout<AudioHardwareIOProcStreamUsage>.alignment)
        defer { storage.deallocate() }
        storage.initializeMemory(as:UInt8.self,repeating:0,count:size)
        let usage = storage.assumingMemoryBound(to:AudioHardwareIOProcStreamUsage.self)
        usage.pointee.mIOProc = unsafeBitCast(ioProc,to:UnsafeMutableRawPointer.self)
        usage.pointee.mNumberStreams = count
        let offset = MemoryLayout<AudioHardwareIOProcStreamUsage>.offset(of:\.mStreamIsOn)!
        let enabled = storage.advanced(by:offset).assumingMemoryBound(to:UInt32.self)
        enabled[Int(selected)] = 1
        var address = HAL.address(kAudioDevicePropertyIOProcStreamUsage,kAudioObjectPropertyScopeInput)
        try checked(AudioObjectSetPropertyData(aggregate,&address,0,nil,UInt32(size),storage),"Disable endpoint loopback in Spotify route")
        var returnedSize = UInt32(size)
        try checked(AudioObjectGetPropertyData(aggregate,&address,0,nil,&returnedSize,storage),"Verify tap-only input stream usage")
        guard usage.pointee.mNumberStreams == count,
              (0..<Int(count)).allSatisfy({ (enabled[$0] != 0) == ($0 == Int(selected)) }) else {
            throw AudioFailure("HAL did not confirm that only the Spotify tap input is enabled.")
        }
    }

    static func validateFormats(_ inputs: [AudioStreamBasicDescription], _ outputs: [AudioStreamBasicDescription], expectedRate: Double) throws {
        for formats in [inputs, outputs] {
            guard formats.reduce(0, { $0 + Int($1.mChannelsPerFrame) }) == 2 else {
                throw AudioFailure("The aggregate did not expose exactly two input and two output channels. Routing stopped before playback.")
            }
            for f in formats {
                let noninterleaved = f.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0
                let stride = noninterleaved ? 4 : 4 * f.mChannelsPerFrame
                guard f.mFormatID == kAudioFormatLinearPCM,
                      f.mFormatFlags & kAudioFormatFlagIsFloat != 0,
                      f.mFormatFlags & kAudioFormatFlagIsBigEndian == 0,
                      f.mBitsPerChannel == 32, f.mBytesPerFrame == stride,
                      f.mFramesPerPacket == 1, f.mBytesPerPacket == stride,
                      abs(f.mSampleRate - expectedRate) < 0.1 else {
                    throw AudioFailure("Unsupported negotiated audio format: \(f.mSampleRate) Hz (expected \(expectedRate)), \(f.mChannelsPerFrame) channels, \(f.mBitsPerChannel) bits, flags \(f.mFormatFlags), bytes/frame \(f.mBytesPerFrame), frames/packet \(f.mFramesPerPacket), bytes/packet \(f.mBytesPerPacket). This build requires native Float32 stereo.")
                }
            }
        }
    }

    func setGain(_ gain: Float) { if let renderer { player_renderer_set_gain(renderer, gain) } }
    func stats() -> RouteStats {
        guard let renderer else { return RouteStats() }
        let s = player_renderer_stats(renderer)
        return RouteStats(callbacks: s.callbacks, frames: s.frames, nonzeroFrames: s.nonzeroFrames,
                          invalidBuffers: s.invalidBuffers, peak: s.peak)
    }

    @discardableResult func stop() -> [String] {
        var errors: [String] = []
        if started {
            let status = AudioDeviceStop(aggregate, ioProc)
            if status != noErr { errors.append("Stop I/O: \(status)") }
            started = false
        }
        var callbackReleased = true
        if let ioProc {
            let status = AudioDeviceDestroyIOProcID(aggregate, ioProc)
            callbackReleased = status == noErr
            if !callbackReleased { errors.append("Release callback: \(status)") }
            self.ioProc = nil
        }
        if aggregate != 0 {
            let status = AudioHardwareDestroyAggregateDevice(aggregate)
            if status != noErr { errors.append("Destroy aggregate: \(status)") }
            callbackReleased = callbackReleased || status == noErr
            aggregate = 0
        }
        if tap != 0 {
            let status = AudioHardwareDestroyProcessTap(tap)
            if status != noErr { errors.append("Destroy tap: \(status)") }
            tap = 0
        }
        // If HAL failed to release both owners, retain storage to avoid a callback use-after-free.
        if callbackReleased, let renderer { player_renderer_destroy(renderer) }
        renderer = nil
        return errors
    }
    deinit { stop() }
}
