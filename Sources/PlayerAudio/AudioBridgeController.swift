import Foundation
import CoreAudio
import AVFAudio
import RealtimeAudio

func makeBridgeSourceNode(forwarder: OpaquePointer, format: AVAudioFormat) -> AVAudioSourceNode {
    AVAudioSourceNode(format:format) { isSilence,_,_,buffers in
        bridge_monitor_render(forwarder,buffers)
        // This is an output parameter. Leaving it untouched can make the engine
        // discard nonzero samples after an initially silent input period.
        isSilence.pointee = false
        return noErr
    }
}

public enum AudioBridgeEndpoint {
    public static let uid = "se.lbsa71.AudioBridge.endpoint.v1"
    public static func find() throws -> AudioObjectID? {
        try HAL.ids(HAL.system,kAudioHardwarePropertyDevices).first {
            (try? HAL.string($0,kAudioDevicePropertyDeviceUID)) == uid
        }
    }
}

private final class EndpointInput {
    let device: AudioObjectID
    let rate: Double
    let renderer: OpaquePointer
    let forwarder: OpaquePointer
    private var callback: AudioDeviceIOProcID?
    private var started = false
    private var released = false
    init() throws {
        guard let device = try AudioBridgeEndpoint.find() else { throw AudioFailure("Install AudioBridge.driver before starting the service.") }
        self.device = device
        rate = try HAL.value(device,kAudioDevicePropertyNominalSampleRate,initial: Double(0))
        try ProcessTapSession.validateFormats(HAL.formats(device,scope:kAudioObjectPropertyScopeInput),
                                              HAL.formats(device,scope:kAudioObjectPropertyScopeOutput),expectedRate:rate)
        guard let renderer = player_renderer_create(rate,true) else { throw AudioFailure("Cannot allocate endpoint capture buffers.") }
        self.renderer = renderer
        guard let forwarder = bridge_forwarder_create(renderer) else {
            player_renderer_destroy(renderer); throw AudioFailure("Cannot allocate monitoring buffers.")
        }
        self.forwarder = forwarder
        do {
            try checked(AudioDeviceCreateIOProcID(device,bridge_capture_callback,UnsafeMutableRawPointer(forwarder),&callback),"Create Audio Bridge input callback")
            try checked(AudioDeviceStart(device,callback),"Start Audio Bridge input")
            started = true
        } catch { _ = stop(); throw error }
    }
    func stop() -> [String] {
        guard !released else { return [] }; released = true
        var errors: [String] = []
        if started {
            let status = AudioDeviceStop(device,callback)
            if status != noErr { errors.append("Stop endpoint input: \(status)") }
        }
        var safe = callback == nil
        if let callback {
            let status = AudioDeviceDestroyIOProcID(device,callback)
            safe = status == noErr
            if !safe { errors.append("Release endpoint callback: \(status); retaining callback storage for safety") }
        }
        if safe { bridge_forwarder_destroy(forwarder); player_renderer_destroy(renderer) }
        return errors
    }
    deinit { _ = stop() }
}

// Single authority for the user-space endpoint session. Input capture owns its
// clock; monitoring can stop/reconfigure independently. No Spotify dependency.
@MainActor public final class AudioBridgeController {
    private var input: EndpointInput?
    private var engine: AVAudioEngine?
    private var sourceNode: AVAudioSourceNode?
    private var monitorConfigurationObserver: NSObjectProtocol?
    private var monitorDevice: OutputDevice?
    private var recorder: DebugWAVRecorder?
    private var recordingID = UUID()
    private var timer: Timer?
    private var recording = RecordingSnapshot()
    private var error = ""
    private var monitorError = ""
    private var events: [String] = []
    private var lastCallbacks: UInt64 = 0
    private var lastAdvance = Date()
    private var metadataLeaseUntil: Date?
    private var gain: Float = 0.5

    public init() {}
    private func event(_ message: String) {
        events.append("\(ISO8601DateFormatter().string(from: Date()))  \(message)")
        if events.count > 80 { events.removeFirst(events.count-80) }
    }
    public func start() throws {
        guard input == nil else { return }
        input = try EndpointInput(); error = ""; lastAdvance = Date(); lastCallbacks = 0
        event("Endpoint capture active at \(Int(input!.rate)) Hz; physical monitoring is independent")
        timer = Timer.scheduledTimer(withTimeInterval: 1,repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick() }
        }
    }
    public func stop() {
        stopRecording(reason: "Endpoint capture stopped.")
        stopMonitoring()
        timer?.invalidate(); timer = nil
        let errors = input?.stop() ?? []; input = nil
        if !errors.isEmpty { error = errors.joined(separator: "; ") }
        event("Endpoint capture stopped")
    }
    public func shutdown() {
        let finishing = recorder
        stop()
        finishing?.waitUntilStopped()
    }
    public func setGain(_ value: Float) throws {
        guard value.isFinite, (0...1).contains(value) else { throw AudioFailure("Monitoring gain must be between 0 and 1.") }
        gain = value
        if let input { bridge_monitor_gain(input.forwarder,gain) }
    }
    public func monitor(outputUID: String) throws {
        try start()
        guard let input else { return }
        guard outputUID != AudioBridgeEndpoint.uid,
              let output = try OutputDevice.discover().first(where: { $0.uid == outputUID }), output.outputChannels == 2 else {
            throw AudioFailure("Choose a physical stereo output; monitoring to Audio Bridge would create feedback.")
        }
        stopMonitoring()
        let engine = AVAudioEngine()
        guard let unit = engine.outputNode.audioUnit else { throw AudioFailure("Cannot create physical output audio unit.") }
        var device = output.objectID
        try checked(AudioUnitSetProperty(unit,kAudioOutputUnitProperty_CurrentDevice,kAudioUnitScope_Global,0,
                                         &device,UInt32(MemoryLayout<AudioObjectID>.size)),"Select monitoring output")
        guard let format = AVAudioFormat(standardFormatWithSampleRate:input.rate,channels:2) else { throw AudioFailure("Unsupported input format for monitoring.") }
        let pointer = input.forwarder
        let node = makeBridgeSourceNode(forwarder:pointer,format:format)
        engine.attach(node); engine.connect(node,to:engine.mainMixerNode,format:format)
        bridge_monitor_gain(pointer,gain); bridge_monitor_enable(pointer,true)
        do { try engine.start() }
        catch { engine.stop(); bridge_monitor_enable(pointer,false); throw error }
        self.engine = engine; sourceNode = node; monitorDevice = output; monitorError = ""
        monitorConfigurationObserver = NotificationCenter.default.addObserver(forName:.AVAudioEngineConfigurationChange,object:engine,queue:.main) { [weak self,weak engine] _ in
            MainActor.assumeIsolated {
                guard let self, let engine, self.engine === engine else { return }
                self.verifyMonitoringDestination()
            }
        }
        verifyMonitoringDestination()
        guard self.engine != nil else { throw AudioFailure(monitorError) }
        event("Monitoring on \(output.name); WAV capture remains on the endpoint clock")
    }
    public func stopMonitoring() {
        if let observer = monitorConfigurationObserver { NotificationCenter.default.removeObserver(observer) }
        monitorConfigurationObserver = nil
        engine?.stop(); engine = nil; sourceNode = nil; monitorDevice = nil
        if let input { bridge_monitor_enable(input.forwarder,false) }
    }
    public func startRecording(directory: URL, track: RecordingTrack?, leaseSeconds: Double? = nil) throws {
        try start()
        guard let input, !recording.enabled, !recording.finishing else { throw AudioFailure("A recording is already active or finalizing.") }
        recordingID = UUID(); let id = recordingID
        let writer = DebugWAVRecorder(renderer:input.renderer,directory:directory,sampleRate:input.rate,
                                      captureDescription:"IEEE Float32; Audio Bridge input before monitoring gain or rate conversion") { [weak self] state in
            Task { @MainActor in
                guard let self, self.recordingID == id else { return }
                if self.recording.finishing && state.enabled { return }
                self.recording = state
                if !state.enabled && !state.finishing { self.recorder = nil }
            }
        }
        recording = try writer.start(track:track); recorder = writer
        metadataLeaseUntil = leaseSeconds.map { Date().addingTimeInterval(max(2,min(60,$0))) }
        event("WAV recording started at the virtual endpoint, before monitoring")
    }
    public func observeTrack(_ track: RecordingTrack?, leaseSeconds: Double = 10) {
        guard recording.enabled else { return }
        metadataLeaseUntil = Date().addingTimeInterval(max(2,min(60,leaseSeconds)))
        recorder?.observe(track)
    }
    public func stopRecording(reason: String = "Stopped by user.") {
        guard let recorder, !recording.finishing else { return }
        recording.enabled = false; recording.finishing = true
        metadataLeaseUntil = nil
        recorder.stop(reason:reason)
    }
    // Explicit opt-in: only regular default output changes, never default input
    // or the system-alert device. Remember the previous UID for restoration.
    public func useAsDefaultOutput() throws {
        try start()
        guard let input else { return }
        let previous = try OutputDevice.defaultOutputUID()
        if previous == AudioBridgeEndpoint.uid { return }
        // AVAudioEngine follows default-device changes even after CurrentDevice
        // is set. Destroy its old graph before changing the default, then create
        // a graph pinned to the physical output under the new default.
        let listeningUID = monitorDevice?.uid
        stopMonitoring()
        if previous != AudioBridgeEndpoint.uid { UserDefaults.standard.set(previous,forKey:"previousOutputUID") }
        var device = input.device
        var address = HAL.address(kAudioHardwarePropertyDefaultOutputDevice)
        try checked(AudioObjectSetPropertyData(HAL.system,&address,0,nil,UInt32(MemoryLayout.size(ofValue:device)),&device),"Use Audio Bridge as default output")
        event("Audio Bridge selected as the default output by explicit request")
        if let listeningUID { try monitor(outputUID:listeningUID) }
    }
    public func restoreDefaultOutput() throws {
        guard try OutputDevice.defaultOutputUID() == AudioBridgeEndpoint.uid else { return }
        guard let previous = UserDefaults.standard.string(forKey:"previousOutputUID"),
              let output = try OutputDevice.discover().first(where: { $0.uid == previous }) else {
            throw AudioFailure("The previous output is unavailable. Choose a physical output in macOS Sound settings.")
        }
        var device = output.objectID; var address = HAL.address(kAudioHardwarePropertyDefaultOutputDevice)
        stopMonitoring()
        try checked(AudioObjectSetPropertyData(HAL.system,&address,0,nil,UInt32(MemoryLayout.size(ofValue:device)),&device),"Restore previous output")
        event("Previous default output restored")
    }
    private func actualMonitoringDevice() -> AudioObjectID {
        guard let unit = engine?.outputNode.audioUnit else { return 0 }
        var device: AudioObjectID = 0
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        guard AudioUnitGetProperty(unit,kAudioOutputUnitProperty_CurrentDevice,kAudioUnitScope_Global,0,&device,&size) == noErr else { return 0 }
        return device
    }
    private func verifyMonitoringDestination() {
        guard let engine, let output = monitorDevice else { return }
        guard engine.isRunning, actualMonitoringDevice() == output.objectID else {
            stopMonitoring()
            monitorError = "The listening device configuration changed. Monitoring stopped to prevent fallback or feedback; WAV capture continues. Choose Listen to restart."
            event(monitorError)
            return
        }
    }
    private func tick() {
        guard let input else { return }
        verifyMonitoringDestination()
        let stats = player_renderer_stats(input.renderer)
        if stats.callbacks != lastCallbacks { lastAdvance = Date(); lastCallbacks = stats.callbacks }
        if let lease = metadataLeaseUntil, Date() > lease {
            stopRecording(reason:"Track metadata lease expired; stopped to avoid mislabeled files.")
        }
        do {
            guard try AudioBridgeEndpoint.find() == input.device,
                  abs(try HAL.value(input.device,kAudioDevicePropertyNominalSampleRate,initial:Double(0))-input.rate)<0.1,
                  try HAL.formats(input.device,scope:kAudioObjectPropertyScopeInput).reduce(0,{$0+Int($1.mChannelsPerFrame)}) == 2 else {
                error = "Endpoint format changed. Restart capture using a supported stereo format."
                stop(); return
            }
            if Date().timeIntervalSince(lastAdvance)>5 {
                error = "Endpoint callbacks stopped. Restart capture."
                stop(); return
            }
            if let output = monitorDevice, !(try OutputDevice.discover().contains(output)) {
                stopMonitoring(); monitorError = "Monitoring output disconnected or changed. Input capture and WAV recording continue."
                event(monitorError)
            }
        } catch { self.error = error.localizedDescription; stop() }
    }
    public var status: [String: Any] {
        let stats = input.map { player_renderer_stats($0.renderer) }
        let monitor = input.map { bridge_monitor_stats($0.forwarder) }
        let actualDevice = actualMonitoringDevice()
        return ["ok":true,"endpointUID":AudioBridgeEndpoint.uid,"installed":(try? AudioBridgeEndpoint.find()) != nil,
                "capturing":input != nil,"sampleRate":input?.rate ?? 0,"error":error,
                "defaultOutput":(try? OutputDevice.defaultOutputUID()) ?? "",
                "monitoring":engine != nil,"monitorOutputUID":monitorDevice?.uid ?? "",
                "monitorEngineRunning":engine?.isRunning ?? false,
                "monitorActualOutputUID":(try? HAL.string(actualDevice,kAudioDevicePropertyDeviceUID)) ?? "",
                "monitorSignalFrames":monitor?.signalFrames ?? 0,"monitorPeak":monitor?.peak ?? 0,
                "monitorError":monitorError,"monitorGain":gain,
                "monitorUnderruns":monitor?.underruns ?? 0,"monitorOverflows":monitor?.overflows ?? 0,
                "monitorQueuedFrames":monitor?.queuedFrames ?? 0,"monitorRatio":monitor?.ratio ?? 1,
                "callbacks":stats?.callbacks ?? 0,"frames":stats?.frames ?? 0,
                "nonzeroFrames":stats?.nonzeroFrames ?? 0,"invalidBuffers":stats?.invalidBuffers ?? 0,
                "recording":["enabled":recording.enabled,"finishing":recording.finishing,"file":recording.file,
                             "frames":recording.frames,"savedFiles":recording.savedFiles,"error":recording.error,"detail":recording.detail],
                "events":events]
    }
}
