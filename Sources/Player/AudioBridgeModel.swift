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
        ["trackID":track.id,"album":track.album,"artist":track.artist,"track":track.name,
         "positionSeconds":track.positionSeconds,"playing":true]
    }
    func observe(_ track: RecordingTrack?) { latestTrack = track; hasObservedPlayback = true }
    func refresh() async {
        do {
            let response = try await request("bridge_status")
            status = response
            monitorGain = response["monitorGain"] as? Double ?? monitorGain
            error = response["controlError"] as? String ?? ""
            if error.isEmpty { error = response["error"] as? String ?? "" }
            onRecordingChange?()
            if recordingEnabled && hasObservedPlayback && response["usesTrackMetadata"] as? Bool == true {
                _ = try await request("bridge_record_track",latestTrack.map(metadata) ?? ["playing":false])
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
    func record(directory: URL, track: RecordingTrack?) {
        latestTrack = track
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
