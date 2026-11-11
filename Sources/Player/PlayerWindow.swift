import SwiftUI
import AppKit
import SpotifyKit

private enum PlayerPalette {
    static let canvas = Color(red: 0.945, green: 0.957, blue: 0.969)
    static let ink = Color(red: 0.09, green: 0.16, blue: 0.25)
    static let accent = Color(red: 0.14, green: 0.43, blue: 0.77)
}

struct PlayerWindow: View {
    @ObservedObject var model: PlayerViewModel
    @ObservedObject var spotify: SpotifyPlaybackModel
    @State private var showSettings = false
    @State private var seeking = false
    @State private var seekValue = 0.0
    @AppStorage("lastRecordingPlaylist") private var playlistInput = ""

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Label("Player", systemImage: "waveform.path").font(.title2.weight(.semibold))
                Spacer()
                Circle().fill(spotify.connected ? Color.green : Color.gray).frame(width: 7, height: 7)
                Text(spotify.connected ? "Spotify connected" : "Spotify not connected").font(.callout)
                Button { showSettings = true } label: { Image(systemName: "gearshape") }
                    .help("Spotify connection and local device")
            }.padding(.horizontal, 28).padding(.vertical, 20)
            Divider()
            VStack(alignment: .leading, spacing: 22) {
                HStack(alignment: .center, spacing: 24) {
                    AsyncImage(url: spotify.playback?.item?.artworkURL) { image in
                        image.resizable().scaledToFill()
                    } placeholder: {
                        ZStack {
                            Color.white.opacity(0.8)
                            Image(systemName: "music.note").font(.system(size: 46, weight: .light)).foregroundStyle(.secondary)
                        }
                    }.frame(width: 160, height: 160).clipShape(RoundedRectangle(cornerRadius: 8))
                    VStack(alignment: .leading, spacing: 10) {
                        Text(spotify.playback?.item?.name ?? "Ready when you are")
                            .font(.system(size: 27, weight: .semibold)).lineLimit(3)
                            .help(spotify.playback?.item?.name ?? "")
                        Text(spotify.playback?.item?.subtitle ?? "Connect Spotify to see what’s playing.")
                            .font(.title3).foregroundStyle(.secondary).lineLimit(2)
                        Label(spotify.playbackDescription, systemImage: spotify.stale ? "clock.badge.exclamationmark" : "desktopcomputer")
                            .font(.caption).foregroundStyle(.secondary)
                        if let link = spotify.playback?.item?.external_urls?["spotify"], let url = URL(string: link) {
                            Link("Open in Spotify", destination: url).font(.caption)
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                }
                TimelineView(.periodic(from: .now, by: 1)) { timeline in
                    let duration = max(1, Double(spotify.playback?.item?.duration_ms ?? 0) / 1000)
                    VStack(spacing: 4) {
                        Slider(value: Binding(get: { seeking ? seekValue : spotify.progress(at: timeline.date) },
                                              set: { seekValue = $0 }), in: 0...duration) { editing in
                            seeking = editing
                            if !editing { spotify.send(.seek(Int(seekValue * 1000)), name: "Seek") }
                        }.disabled(!spotify.canControl || !spotify.isLocalPlayback || spotify.stale || spotify.playback?.item == nil)
                            .accessibilityLabel("Track position")
                        HStack {
                            Text(Self.time(seeking ? seekValue : spotify.progress(at: timeline.date)))
                            Spacer()
                            Text(Self.time(Double(spotify.playback?.item?.duration_ms ?? 0) / 1000))
                        }.font(.caption).monospacedDigit().foregroundStyle(.secondary)
                    }
                }
                HStack(spacing: 28) {
                    Spacer()
                    transport("Previous track", symbol: "backward.end.fill") { spotify.send(.previous, name: "Previous") }
                    Button {
                        let pause = spotify.isLocalPlayback && spotify.playback?.is_playing == true
                        spotify.send(pause ? .pause : .play, name: pause ? "Pause" : "Play")
                    } label: {
                        Image(systemName: spotify.isLocalPlayback && spotify.playback?.is_playing == true ? "pause.fill" : "play.fill")
                            .font(.system(size: 22)).frame(width: 58, height: 48)
                    }.buttonStyle(.borderedProminent).controlSize(.large)
                        .disabled(!spotify.canControl)
                        .accessibilityLabel(spotify.isLocalPlayback && spotify.playback?.is_playing == true ? "Pause" : "Play")
                    transport("Next track", symbol: "forward.end.fill") { spotify.send(.next, name: "Next") }
                    Spacer()
                }
                if !spotify.connected {
                    HStack {
                        Text("Use Spotify desktop for playback. Connect to control it here.").font(.callout).foregroundStyle(.secondary)
                        Spacer()
                        Button("Connect Spotify") { showSettings = true }
                    }
                } else if spotify.selectedDeviceID.isEmpty {
                    Button("Identify Spotify on this Mac…") { showSettings = true }.buttonStyle(.borderedProminent)
                } else if !spotify.isLocalPlayback {
                    Button("Transfer playback to \(spotify.selectedDevice?.name ?? "this Mac")") {
                        spotify.send(.transfer, name: "Transfer")
                    }.disabled(!spotify.canControl)
                }
                HStack {
                    TextField("Spotify playlist link", text: $playlistInput).textFieldStyle(.roundedBorder)
                    Button("Play playlist") { spotify.send(.playlist(playlistInput), name: "Playlist") }
                        .disabled(!spotify.canControl || playlistInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        .help("Start at the first track with shuffle and repeat off")
                }
                if spotify.commandPending { Text(spotify.lastCommand).font(.caption).foregroundStyle(.secondary) }
                if !spotify.error.isEmpty {
                    Text(spotify.error).font(.callout).foregroundStyle(.red).textSelection(.enabled)
                }
            }.padding(28)
            Divider()
            AudioBridgePanel(model:model,bridge:model.bridge,recovery:model.playlistRecording)
        }
        .frame(width: 720).background(PlayerPalette.canvas).foregroundStyle(PlayerPalette.ink)
        .tint(PlayerPalette.accent).preferredColorScheme(.light)
        .sheet(isPresented: $showSettings) { SpotifySettings(spotify: spotify) }
    }
    private func transport(_ label: String, symbol: String, action: @escaping () -> Void) -> some View {
        Button(action: action) { Image(systemName: symbol).font(.title3).frame(width: 28, height: 32) }
            .buttonStyle(.borderless).disabled(!spotify.canControl).accessibilityLabel(label).help(label)
    }
    private static func time(_ seconds: Double) -> String {
        let value = max(0, Int(seconds))
        return "\(value / 60):\(String(format: "%02d", value % 60))"
    }
}

private struct AudioBridgePanel: View {
    @ObservedObject var model: PlayerViewModel
    @ObservedObject var bridge: AudioBridgeModel
    @ObservedObject var recovery: PlaylistRecordingModel
    private var ready: Bool {
        bridge.capturing && model.endpointReady
    }
    private var outputName: String {
        let uid = bridge.status["monitorActualOutputUID"] as? String ?? ""
        return model.outputs.first(where: { $0.uid == uid })?.name ?? "Listening off"
    }
    var body: some View {
        VStack(alignment:.leading,spacing:16) {
            HStack(alignment:.top) {
                VStack(alignment:.leading,spacing:5) {
                    Label(bridge.recordingEnabled ? "Recording playlist" : "Playlist recording",
                          systemImage:bridge.recordingEnabled ? "record.circle.fill" : "waveform")
                        .font(.headline).foregroundStyle(bridge.recordingEnabled ? Color.red : PlayerPalette.ink)
                    Text("Spotify → Audio Bridge").font(.callout)
                    Text("One WAV per track · Float32 WAV")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button(bridge.recordingEnabled || recovery.running ? "Stop recording" : "Resume playlist recording") {
                    if bridge.recordingEnabled || recovery.running { recovery.stop() }
                    else { recovery.start(playlist:model.spotify.playback?.context?.uri ?? UserDefaults.standard.string(forKey:"lastRecordingPlaylist") ?? "") }
                }.buttonStyle(.borderedProminent)
                    .disabled(!recovery.running && !bridge.recordingEnabled && (bridge.busy || !bridge.installed || bridge.recording["finishing"] as? Bool == true))
            }
            Text(recovery.detail).font(.caption).foregroundStyle(.secondary)
            if recovery.completeCount > 0 || recovery.unverifiedCount > 0 {
                Text("\(recovery.completeCount) tracks complete · \(recovery.unverifiedCount) older files unverified")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if !ready {
                HStack {
                    Text(bridge.installed ? "Connect Spotify to Audio Bridge to record. Other apps keep your chosen system output." : "Audio Bridge needs to be installed.")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Enable audio") { model.enableEndpointRoute() }
                        .disabled(bridge.busy || !bridge.installed)
                }
            }
            if let file = bridge.recording["file"] as? String, !file.isEmpty {
                Text(URL(fileURLWithPath:file).lastPathComponent)
                    .font(.caption).lineLimit(1).truncationMode(.middle).help(file)
            }
            HStack {
                Text(model.debugOutputDirectory.path).font(.caption).foregroundStyle(.secondary)
                    .textSelection(.enabled)
                Spacer()
                Button("Open folder") { NSWorkspace.shared.open(model.debugOutputDirectory) }
                    .buttonStyle(.link).font(.caption)
            }
            Divider()
            HStack {
                Label(outputName,systemImage:"headphones").font(.callout)
                Spacer()
                if bridge.monitoring {
                    Slider(value:$bridge.monitorGain,in:0...1) { editing in
                        if !editing { bridge.command("bridge_gain",["gain":bridge.monitorGain]) }
                    }.frame(width:160).accessibilityLabel("Listening volume")
                    Text("\(Int(bridge.monitorGain*100))%").font(.caption).monospacedDigit()
                    Button("Stop listening") { bridge.command("bridge_monitor_stop") }
                } else {
                    Button("Listen") {
                        if let uid = model.listeningOutputUID { bridge.command("bridge_monitor",["outputUID":uid]) }
                    }
                        .disabled(bridge.busy || model.listeningOutputUID == nil)
                }
            }
            Text("Other apps use your Mac’s output. Listening here is optional and does not affect WAVs.")
                .font(.caption).foregroundStyle(.secondary)
            ForEach([model.discoveryError,recovery.error,bridge.error,bridge.recording["error"] as? String ?? ""].filter { !$0.isEmpty },id:\.self) {
                Text($0).font(.caption).foregroundStyle(.red).fixedSize(horizontal:false,vertical:true)
            }
        }.padding(24).background(.white.opacity(0.55))
    }
}

private struct SpotifySettings: View {
    @ObservedObject var spotify: SpotifyPlaybackModel
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack { Text("Spotify connection").font(.title2.weight(.semibold)); Spacer(); Button("Done") { dismiss() } }
            Text("Spotify desktop plays the music. Player controls it through your Spotify account.")
                .font(.callout).foregroundStyle(.secondary)
            TextField("Spotify developer client ID", text: $spotify.clientID)
                .textFieldStyle(.roundedBorder).disabled(spotify.connected || spotify.signingIn)
            Text("Registered callback: \(SpotifyAuth.redirectURI)").font(.caption).textSelection(.enabled)
            HStack {
                if spotify.connected {
                    Label("Connected", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                    Spacer()
                    Button("Disconnect") { spotify.signOut() }
                } else if spotify.signingIn {
                    ProgressView().controlSize(.small)
                    Text("Complete sign-in in your browser…")
                    Spacer()
                    Button("Cancel") { spotify.cancelSignIn() }
                } else {
                    Button("Connect Spotify") { spotify.signIn() }.buttonStyle(.borderedProminent)
                        .disabled(spotify.clientID.isEmpty)
                }
            }
            Divider()
            Text("Spotify on this Mac").font(.headline)
            Text("Choose the Spotify device that represents this computer. Confirm its identity yourself; the device name alone doesn’t establish its location.")
                .font(.callout).foregroundStyle(.secondary)
            HStack {
                Picker("Device", selection: Binding(get: { spotify.selectedDeviceID }, set: { spotify.selectDevice($0) })) {
                    Text("Choose this Mac’s Spotify device…").tag("")
                    ForEach(spotify.devices) { device in
                        Text("\(device.name) (\(device.type))\(device.is_active ? " · active" : "")").tag(device.id ?? "")
                    }
                }.disabled(!spotify.connected || spotify.commandPending)
                Button("Refresh") { Task { await spotify.refreshDevices() } }.disabled(!spotify.connected || spotify.refreshingDevices)
            }
            if spotify.selectedDevice != nil {
                Button("Transfer playback here") { spotify.send(.transfer, name: "Transfer") }.disabled(!spotify.canControl)
            }
            if !spotify.error.isEmpty { Text(spotify.error).foregroundStyle(.red).font(.callout).textSelection(.enabled) }
            Text("Refresh credentials are stored in your macOS Keychain. No client secret is used.").font(.caption).foregroundStyle(.secondary)
        }.padding(28).frame(width: 590)
    }
}

