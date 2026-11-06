import Foundation
import CoreAudio

public enum RoutePhase: String {
    case idle = "Ordinary Spotify playback"
    case preparing = "Checking captured audio"
    case starting = "Starting muted route"
    case running = "Routing active"
    case stopping = "Stopping"
    case failed = "Routing stopped"
}

public struct RouteSnapshot {
    public init() {}
    public var phase: RoutePhase = .idle
    public var detail = "Start Spotify manually, select wired headphones, then test routing."
    public var stats = RouteStats()
    public var format = ""
    public var events: [String] = []
    public var recording = RecordingSnapshot()
    public var isBusy: Bool { [.preparing, .starting, .running, .stopping].contains(phase) }
}

public final class AudioRouteController {
    private let queue = DispatchQueue(label: "se.lbsa71.player.audio-lifecycle")
    private var session: ProcessTapSession?
    private var timer: DispatchSourceTimer?
    private var listeners: [(AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertyListenerBlock)] = []
    private var snapshot = RouteSnapshot()
    private var gain: Float = 0.5
    private var began = Date()
    private var lastAdvance = Date()
    private var lastSignal = Date()
    private var previousCallbacks: UInt64 = 0
    private var previousNonzero: UInt64 = 0
    private var tickCount = 0
    private var routeID = UUID()
    private var recorder: DebugWAVRecorder?
    private var recordingID = UUID()
    public var onChange: ((RouteSnapshot) -> Void)?
    public var onOutputLoss: ((@escaping (Bool) -> Void) -> Void)?

    public init() {}
    public func start(outputUID: String) {
        queue.async { [self] in
            guard session == nil else { return }
            routeID = UUID()
            do {
                guard let output = try OutputDevice.discover().first(where: { $0.uid == outputUID }) else {
                    throw AudioFailure("The selected output is no longer available. Reconnect it and select it again.")
                }
                let sources = try SourceResolver.spotify()
                guard sources.contains(where: \.isPlaying) else {
                    throw AudioFailure("No local Spotify audio is playing. Play a track in the Spotify desktop app on this Mac, then try again.")
                }
                event("Preparing \(output.name), \(Int(output.sampleRate)) Hz; \(sources.count) Spotify process objects")
                snapshot.stats = RouteStats()
                let probe = ProcessTapSession(output: output, sources: sources)
                session = probe
                snapshot.phase = .preparing
                snapshot.detail = "Allow system audio recording if macOS asks. Checking for Spotify samples; ordinary playback remains on."
                publish()
                try probe.start(probe: true, gain: gain, progress: preparationProgress)
                snapshot.format = probe.formatDescription
                began = Date(); lastAdvance = began; lastSignal = began
                previousCallbacks = 0; previousNonzero = 0; tickCount = 0
                try addListeners(output: output)
                let timer = DispatchSource.makeTimerSource(queue: queue)
                timer.schedule(deadline: .now() + 0.25, repeating: 0.25)
                timer.setEventHandler { [weak self] in self?.tick() }
                self.timer = timer
                timer.resume()
                publish()
            } catch { fail(error.localizedDescription) }
        }
    }

    public func stop(reason: String = "Stopped. Ordinary Spotify output restored.") {
        queue.async { [self] in finish(reason: reason, failed: false) }
    }
    public func shutdown() {
        queue.sync {
            let finishingRecorder = recorder
            finish(reason: "App quit; route released.", failed: false)
            // Audio has already been released. Let the disk worker finalize WAV headers.
            finishingRecorder?.waitUntilStopped()
        }
    }
    public func startRecording(directory: URL, track: RecordingTrack) {
        queue.async { [self] in
            guard snapshot.phase == .running, let session, let renderer = session.recordingRenderer,
                  !snapshot.recording.enabled, !snapshot.recording.finishing else { return }
            recordingID = UUID(); let id = recordingID
            let writer = DebugWAVRecorder(renderer: renderer, directory: directory,
                                          sampleRate: session.output.sampleRate) { [weak self] state in
                guard let self else { return }
                self.queue.async {
                    guard self.recordingID == id else { return }
                    if self.snapshot.recording.finishing && state.enabled { return }
                    self.snapshot.recording = state
                    if !state.enabled && !state.finishing { self.recorder = nil }
                    self.publish()
                }
            }
            do {
                snapshot.recording = try writer.start(track: track)
                recorder = writer
                event("WAV recording started: Float32 stereo, post-renderer gain, approximate track boundaries")
            } catch {
                snapshot.recording = RecordingSnapshot()
                snapshot.recording.error = error.localizedDescription
            }
            publish()
        }
    }
    public func observeRecordingTrack(_ track: RecordingTrack?) {
        queue.async { [self] in recorder?.observe(track) }
    }
    public func stopRecording() {
        queue.async { [self] in stopRecording(reason: "Stopped by user."); publish() }
    }
    private func stopRecording(reason: String) {
        guard let recorder, !snapshot.recording.finishing else { return }
        snapshot.recording.enabled = false; snapshot.recording.finishing = true
        snapshot.recording.detail = "Finalizing WAV files…"
        recorder.stop(reason: reason)
    }
    public func setGain(_ value: Float) {
        queue.async { [self] in gain = value; session?.setGain(value) }
    }

    private func tick() {
        guard let session else { return }
        let stats = session.stats()
        snapshot.stats = stats
        if stats.callbacks > previousCallbacks { lastAdvance = Date() }
        if stats.nonzeroFrames > previousNonzero { lastSignal = Date() }
        previousCallbacks = stats.callbacks; previousNonzero = stats.nonzeroFrames
        if stats.invalidBuffers > 0 {
            fail("The audio buffer layout changed or buffers were unavailable. Normal Spotify output may resume.")
            return
        }
        if snapshot.phase == .preparing {
            if stats.nonzeroFrames > 512 {
                event("Probe received nonzero Spotify samples; rebuilding with conditional source muting")
                let errors = session.stop()
                guard errors.isEmpty else { fail(errors.joined(separator: "; ")); return }
                let route = ProcessTapSession(output: session.output, sources: session.sources)
                self.session = route
                do {
                    snapshot.phase = .starting
                    snapshot.detail = "Captured signal verified. Waiting for the muted route to advance."
                    try route.start(probe: false, gain: gain, progress: preparationProgress)
                    snapshot.format = route.formatDescription
                    began = Date(); lastAdvance = began; lastSignal = began
                    previousCallbacks = 0; previousNonzero = 0
                } catch { fail(error.localizedDescription); return }
            } else if Date().timeIntervalSince(began) > 20 {
                fail("No nonzero Spotify samples received. Check System Settings → Privacy & Security → Screen & System Audio Recording, local Spotify playback, and the selected source. Silence alone does not identify the cause.")
                return
            }
        } else if snapshot.phase == .starting {
            if stats.callbacks > 8 && stats.nonzeroFrames > 512 {
                snapshot.phase = .running
                snapshot.detail = "Captured samples are advancing. Verify one audible Spotify path and that other apps are excluded."
                event("Routing active; callbacks and nonzero samples confirmed")
            } else if Date().timeIntervalSince(began) > 8 {
                fail("The muted route did not produce healthy audio. Normal Spotify output may resume.")
                return
            }
        } else if snapshot.phase == .running {
            if Date().timeIntervalSince(lastAdvance) > 3 {
                fail("Audio callbacks stopped. Normal Spotify output may resume.")
                return
            }
            snapshot.detail = Date().timeIntervalSince(lastSignal) > 5
                ? "No recent signal. Spotify may be paused or playing elsewhere; capture is not evidence of local playback."
                : "Spotify signal detected. Local volume affects only the routed output."
        }
        tickCount += 1
        if tickCount % 8 == 0 { revalidate() }
        publish()
    }

    private func addListeners(output: OutputDevice) throws {
        let properties: [(AudioObjectID, AudioObjectPropertySelector)] = [
            (HAL.system, kAudioHardwarePropertyDevices),
            (HAL.system, kAudioHardwarePropertyProcessObjectList),
            (output.objectID, kAudioDevicePropertyDeviceIsAlive),
            (output.objectID, kAudioDevicePropertyNominalSampleRate),
            (output.objectID, kAudioDevicePropertyStreamConfiguration)
        ]
        for (id, selector) in properties {
            var address = HAL.address(selector)
            if selector == kAudioDevicePropertyStreamConfiguration { address.mScope = kAudioObjectPropertyScopeOutput }
            let block: AudioObjectPropertyListenerBlock = { [weak self] _, _ in self?.revalidate() }
            try checked(AudioObjectAddPropertyListenerBlock(id, &address, queue, block), "Watch audio device")
            listeners.append((id, address, block))
        }
    }

    private func revalidate() {
        guard let session, snapshot.phase != .stopping else { return }
        do {
            guard let current = try OutputDevice.discover().first(where: { $0.uid == session.output.uid }),
                  current == session.output else {
                outputLost()
                return
            }
            let sources = try SourceResolver.spotify()
            guard sources.map(\.objectID) == session.sources.map(\.objectID),
                  sources.map(\.pid) == session.sources.map(\.pid) else {
                fail("Spotify's audio processes changed. Start routing again to capture the new process set.")
                return
            }
        } catch { fail(error.localizedDescription) }
    }

    private func fail(_ message: String) { finish(reason: message, failed: true) }
    private func outputLost() {
        timer?.cancel(); timer = nil
        session?.setGain(0)
        snapshot.phase = .stopping
        snapshot.detail = "Output lost or reconfigured. Requesting Spotify pause before releasing the route."
        event(snapshot.detail); publish()
        let id = routeID
        let completion: (Bool) -> Void = { [weak self] accepted in
            guard let self else { return }
            self.queue.async {
                guard self.routeID == id, self.session != nil else { return }
                self.finish(reason: accepted
                    ? "Output changed. Spotify accepted a pause request and the route was released. Reconnect and restart routing."
                    : "Output changed. Spotify pause could not be confirmed; ordinary playback may resume on another output. Reconnect and restart routing.", failed: true)
            }
        }
        DispatchQueue.main.async { [weak self] in
            if let handler = self?.onOutputLoss { handler(completion) } else { completion(false) }
        }
        // Never retain a failed muted route indefinitely if the network is unavailable.
        queue.asyncAfter(deadline: .now() + 2) { completion(false) }
    }
    private func preparationProgress(_ message: String) {
        snapshot.detail = message
        event(message)
        publish()
    }
    private func finish(reason: String, failed: Bool) {
        stopRecording(reason: reason)
        timer?.cancel(); timer = nil
        for (id, original, block) in listeners {
            var address = original
            AudioObjectRemovePropertyListenerBlock(id, &address, queue, block)
        }
        listeners.removeAll()
        snapshot.phase = .stopping
        if let session { snapshot.stats = session.stats() }
        let errors = session?.stop() ?? []
        session = nil
        routeID = UUID()
        snapshot.phase = failed || !errors.isEmpty ? .failed : .idle
        snapshot.detail = reason + (errors.isEmpty ? "" : " Cleanup errors: " + errors.joined(separator: "; "))
        event(snapshot.detail)
        publish()
    }
    private func event(_ message: String) {
        snapshot.events.append("\(ISO8601DateFormatter().string(from: Date()))  \(message)")
        if snapshot.events.count > 80 { snapshot.events.removeFirst(snapshot.events.count - 80) }
    }
    private func publish() {
        let value = snapshot
        DispatchQueue.main.async { [weak self] in self?.onChange?(value) }
    }
}
