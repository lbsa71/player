import Foundation
import PlayerAudio
import PlayerControl

@MainActor final class AudioBridgeModel: ObservableObject {
    @Published var status: [String:Any] = [:]
    @Published var error = ""
    @Published var pending = false
    @Published var monitorGain = 0.5
    var onRecordingChange: (() -> Void)?
    private var pollTask: Task<Void,Never>?
    private var latestTrack: RecordingTrack?
    private var hasObservedPlayback = false
    private var captureTrack = true
    private var observationTask: Task<Void,Error>?
    private var observationGeneration = 0
    init() {
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.refresh()
                try? await Task.sleep(for:.seconds(1))
            }
        }
    }
    var capturing: Bool { status["capturing"] as? Bool == true }
    var installed: Bool { status["installed"] as? Bool == true }
    var monitoring: Bool { status["monitoring"] as? Bool == true }
    var busy: Bool { pending || status["busy"] as? Bool == true }
    var recording: [String:Any] { status["recording"] as? [String:Any] ?? [:] }
    var recordingEnabled: Bool { recording["enabled"] as? Bool == true }
    private func request(_ method: String, _ arguments: [String:Any] = [:]) async throws -> [String:Any] {
        try await Task.detached { try BridgeControl.request(method,arguments:arguments) }.value
    }
    private func metadata(_ track: RecordingTrack) -> [String:Any] {
        var result: [String:Any] = ["trackID":track.id,"album":track.album,"artist":track.artist,"track":track.name,
                                   "positionSeconds":track.positionSeconds,"playing":true]
        if let duration = track.expectedDurationSeconds { result["expectedDurationSeconds"] = duration }
        return result
    }
    func observe(_ track: RecordingTrack?, capture: Bool = true) {
        latestTrack = track; captureTrack = capture; hasObservedPlayback = true
    }
    func pushObservation() async throws {
        let previous = observationTask
        observationGeneration += 1; let current = observationGeneration
        let task = Task { @MainActor in
            _ = try? await previous?.value
            // Snapshot after earlier submissions finish. An old heartbeat must
            // never re-enable capture after a newer suppression boundary.
            var arguments = latestTrack.map(metadata) ?? ["playing":false]
            arguments["capture"] = captureTrack
            let response = try await request("bridge_record_track", arguments)
            guard response["ok"] as? Bool == true else {
                throw ControlError.message(response["error"] as? String ?? "Could not update recording metadata.")
            }
        }
        observationTask = task
        defer { if current == observationGeneration { observationTask = nil } }
        try await task.value
    }
    func perform(_ method: String, _ arguments: [String:Any] = [:]) async throws {
        for _ in 0..<100 {
            if !pending { break }
            try await Task.sleep(for:.milliseconds(100))
        }
        guard !pending else { throw ControlError.message("Audio Bridge is handling another command.") }
        pending = true
        defer { pending = false }
        let response = try await request(method, arguments)
        guard response["ok"] as? Bool == true else {
            throw ControlError.message(response["error"] as? String ?? "Audio Bridge rejected the request.")
        }
        for _ in 0..<100 {
            let state = try await request("bridge_status")
            status = state
            if state["busy"] as? Bool != true {
                if let message = state["controlError"] as? String, !message.isEmpty { throw ControlError.message(message) }
                onRecordingChange?()
                return
            }
            try await Task.sleep(for:.milliseconds(100))
        }
        throw ControlError.message("Audio Bridge did not finish the command in time.")
    }
    func refresh() async {
        do {
            let response = try await request("bridge_status")
            status = response
            monitorGain = response["monitorGain"] as? Double ?? monitorGain
            error = response["controlError"] as? String ?? ""
            if error.isEmpty { error = response["error"] as? String ?? "" }
            onRecordingChange?()
            if recordingEnabled && hasObservedPlayback && response["usesTrackMetadata"] as? Bool == true {
                try await pushObservation()
            }
        } catch { self.error = error.localizedDescription }
    }
    func command(_ method: String, _ arguments: [String:Any] = [:]) {
        guard !pending else { return }
        pending = true
        Task {
            defer { pending = false }
            do {
                let response = try await request(method,arguments)
                guard response["ok"] as? Bool == true else {
                    error = response["error"] as? String ?? "Audio Bridge rejected the request."; return
                }
                await refresh()
            } catch { self.error = error.localizedDescription }
        }
    }
    func startRecording(directory: URL, track: RecordingTrack?) async throws {
        latestTrack = track; captureTrack = true; hasObservedPlayback = true
        var args: [String:Any] = ["directory":directory.path,"leaseSeconds":10,"playing":track != nil]
        if let track { args.merge(metadata(track)) { _,new in new } }
        try await perform("bridge_record_start",args)
    }
    func record(directory: URL, track: RecordingTrack?) {
        latestTrack = track; captureTrack = true
        var args: [String:Any] = ["directory":directory.path,"leaseSeconds":10,"playing":track != nil]
        if let track { args.merge(metadata(track)) { _,new in new } }
        command("bridge_record_start",args)
    }
    func connect(outputUID: String) {
        guard !pending else { return }
        pending = true
        Task {
            defer { pending = false }
            do {
                for method in ["bridge_monitor","bridge_use_default"] {
                    let reply = try await request(method,method == "bridge_monitor" ? ["outputUID":outputUID] : [:])
                    guard reply["ok"] as? Bool == true else { throw ControlError.message(reply["error"] as? String ?? "Could not enable audio.") }
                    for _ in 0..<100 {
                        try await Task.sleep(for:.milliseconds(100))
                        let state = try await request("bridge_status")
                        if state["busy"] as? Bool != true {
                            if let error = state["controlError"] as? String, !error.isEmpty { throw ControlError.message(error) }
                            break
                        }
                    }
                }
                await refresh()
            } catch { self.error = error.localizedDescription }
        }
    }
    func leaveEndpointMode() {
        pending = true
        Task {
            defer { pending = false }
            do {
                for method in ["bridge_restore_default","bridge_monitor_stop"] {
                    let reply = try await request(method)
                    guard reply["ok"] as? Bool == true else { error = reply["error"] as? String ?? "Could not leave endpoint mode."; return }
                }
                await refresh()
            } catch { self.error = error.localizedDescription }
        }
    }
}
