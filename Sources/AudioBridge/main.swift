import Foundation
import AppKit
import AVFoundation
import PlayerAudio
import PlayerControl

if CommandLine.arguments.count > 1 {
    guard CommandLine.arguments.count >= 3, CommandLine.arguments[1] == "--call" else {
        fputs("Usage: AudioBridge --call bridge_status '{JSON arguments}'\n",stderr); exit(2)
    }
    do {
        let args = try JSONSerialization.jsonObject(with:Data((CommandLine.arguments.count > 3 ? CommandLine.arguments[3] : "{}").utf8)) as? [String:Any] ?? [:]
        let result = try BridgeControl.request(CommandLine.arguments[2],arguments:args)
        let data = try JSONSerialization.data(withJSONObject:result,options:[.sortedKeys])
        print(String(decoding:data,as:UTF8.self)); exit(result["ok"] as? Bool == true ? 0 : 1)
    } catch { fputs("\(error.localizedDescription)\n",stderr); exit(1) }
}

@MainActor final class BridgeAgent: NSObject, NSApplicationDelegate {
    let controller = AudioBridgeController()
    let server = LocalControlServer(directory:BridgeControl.directory)
    private var signalSources: [DispatchSourceSignal] = []
    private var busy = false
    private var controlError = ""
    private var usesTrackMetadata = false
    private var operationGeneration = 0

    func applicationDidFinishLaunching(_ notification: Notification) {
        do {
            try server.start { [weak self] request,reply in
                DispatchQueue.main.async {
                    guard let self else { reply(["ok":false,"error":"Audio Bridge is stopping."]); return }
                    reply(self.handle(request))
                }
            }
        } catch { fputs("\(error.localizedDescription)\n",stderr); NSApp.terminate(nil); return }
        for sig in [SIGTERM,SIGINT] {
            signal(sig,SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal:sig,queue:.main)
            source.setEventHandler { NSApp.terminate(nil) }; source.resume(); signalSources.append(source)
        }
        if UserDefaults.standard.bool(forKey:"captureEnabled") {
            _ = enqueue {
                try self.controller.start()
                if UserDefaults.standard.bool(forKey:"monitorEnabled"),
                   let uid = UserDefaults.standard.string(forKey:"monitorOutputUID") {
                    try self.controller.monitor(outputUID:uid)
                }
            }
        }
    }
    func applicationWillTerminate(_ notification: Notification) { controller.shutdown(); server.stop() }
    private func enqueue(_ operation: @escaping @MainActor () throws -> Void) -> [String:Any] {
        guard !busy else { return ["ok":false,"error":"Wait for the current Audio Bridge operation to finish."] }
        busy = true; controlError = ""
        operationGeneration += 1; let generation = operationGeneration
        Task {
            defer { if generation == operationGeneration { busy = false } }
            let allowed: Bool
            switch AVCaptureDevice.authorizationStatus(for:.audio) {
            case .authorized: allowed = true
            case .notDetermined: allowed = await AVCaptureDevice.requestAccess(for:.audio)
            default: allowed = false
            }
            guard generation == operationGeneration else { return }
            guard allowed else {
                controlError = "Allow AudioBridge under Privacy & Security → Microphone. It reads only the virtual Audio Bridge device, not microphone hardware."
                return
            }
            do { try operation() } catch { controlError = error.localizedDescription }
        }
        return ["ok":true,"accepted":true,"message":"Operation queued. Poll bridge_status for busy=false and controlError."]
    }
    private func track(_ arguments: [String:Any]) -> RecordingTrack {
        let name = arguments["track"] as? String ?? ISO8601DateFormatter().string(from:Date())
        return RecordingTrack(id:arguments["trackID"] as? String ?? UUID().uuidString,
                              album:arguments["album"] as? String ?? "Audio Bridge",
                              artist:arguments["artist"] as? String ?? "Input",
                              name:name,positionSeconds:arguments["positionSeconds"] as? Double ?? 0)
    }
    private func handle(_ request: [String:Any]) -> [String:Any] {
        guard let method = request["method"] as? String, let arguments = request["arguments"] as? [String:Any] else {
            return ["ok":false,"error":"Expected method and arguments."]
        }
        do {
            switch method {
            case "bridge_status":
                var result = controller.status
                result["busy"] = busy; result["controlError"] = controlError
                result["usesTrackMetadata"] = usesTrackMetadata
                result["apiVersion"] = 1
                return result
            case "bridge_devices":
                return ["ok":true,"devices":try OutputDevice.discover().map {
                    ["uid":$0.uid,"name":$0.name,"sampleRate":$0.sampleRate,"channels":$0.outputChannels] as [String:Any]
                }]
            case "bridge_start":
                return enqueue { try self.controller.start(); UserDefaults.standard.set(true,forKey:"captureEnabled") }
            case "bridge_stop":
                operationGeneration += 1; busy = false
                try controller.restoreDefaultOutput()
                controller.stop(); UserDefaults.standard.set(false,forKey:"captureEnabled")
                UserDefaults.standard.set(false,forKey:"monitorEnabled")
            case "bridge_monitor":
                guard let uid = arguments["outputUID"] as? String else { throw ControlError.message("outputUID is required.") }
                return enqueue {
                    try self.controller.monitor(outputUID:uid)
                    UserDefaults.standard.set(uid,forKey:"monitorOutputUID")
                    UserDefaults.standard.set(true,forKey:"captureEnabled")
                    UserDefaults.standard.set(true,forKey:"monitorEnabled")
                }
            case "bridge_monitor_stop":
                controller.stopMonitoring(); UserDefaults.standard.set(false,forKey:"monitorEnabled")
            case "bridge_gain":
                guard let gain = arguments["gain"] as? Double else { throw ControlError.message("gain is required.") }
                try controller.setGain(Float(gain))
            case "bridge_record_start":
                let path = arguments["directory"] as? String ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Documents/Audio").path
                guard path.hasPrefix("/") else { throw ControlError.message("Recording directory must be absolute.") }
                let metadata = arguments["playing"] as? Bool == false ? nil : track(arguments)
                let lease = arguments["leaseSeconds"] as? Double
                return enqueue {
                    try self.controller.startRecording(directory:URL(fileURLWithPath:path,isDirectory:true),track:metadata,leaseSeconds:lease)
                    self.usesTrackMetadata = lease != nil
                    UserDefaults.standard.set(true,forKey:"captureEnabled")
                }
            case "bridge_record_track":
                controller.observeTrack(arguments["playing"] as? Bool == false ? nil : track(arguments),leaseSeconds:10)
            case "bridge_record_stop":
                operationGeneration += 1; busy = false
                controller.stopRecording(); usesTrackMetadata = false
            case "bridge_use_default": return enqueue { try self.controller.useAsDefaultOutput() }
            case "bridge_restore_default": try controller.restoreDefaultOutput()
            default: throw ControlError.message("Unknown Audio Bridge method.")
            }
            return ["ok":true,"accepted":true]
        } catch { return ["ok":false,"error":error.localizedDescription] }
    }
}

MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = BridgeAgent()
    app.delegate = delegate
    app.setActivationPolicy(.accessory)
    withExtendedLifetime(delegate) { app.run() }
}
