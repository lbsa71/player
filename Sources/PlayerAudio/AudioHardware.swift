import Foundation
import CoreAudio
import AppKit

public struct AudioFailure: LocalizedError {
    public let message: String
    public var errorDescription: String? { message }
    public init(_ message: String) { self.message = message }
}

func checked(_ status: OSStatus, _ operation: String) throws {
    guard status == noErr else { throw AudioFailure("\(operation) failed (OSStatus \(status)).") }
}

enum HAL {
    static let system = AudioObjectID(kAudioObjectSystemObject)
    static func address(_ selector: AudioObjectPropertySelector,
                        _ scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress(mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain)
    }
    static func value<T>(_ id: AudioObjectID, _ selector: AudioObjectPropertySelector,
                         initial: T, scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) throws -> T {
        var result = initial
        var address = address(selector, scope)
        var size = UInt32(MemoryLayout<T>.size)
        try checked(withUnsafeMutablePointer(to: &result) {
            AudioObjectGetPropertyData(id, &address, 0, nil, &size, $0)
        }, "Read audio property \(selector)")
        return result
    }
    static func string(_ id: AudioObjectID, _ selector: AudioObjectPropertySelector) throws -> String {
        let result: CFString = try value(id, selector, initial: "" as CFString)
        return result as String
    }
    static func ids(_ id: AudioObjectID, _ selector: AudioObjectPropertySelector,
                    scope: AudioObjectPropertyScope = kAudioObjectPropertyScopeGlobal) throws -> [AudioObjectID] {
        var address = address(selector, scope)
        var size: UInt32 = 0
        try checked(AudioObjectGetPropertyDataSize(id, &address, 0, nil, &size), "Read audio list size")
        guard size > 0 else { return [] }
        var result = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
        try checked(result.withUnsafeMutableBytes {
            AudioObjectGetPropertyData(id, &address, 0, nil, &size, $0.baseAddress!)
        }, "Read audio list")
        return Array(result.prefix(Int(size) / MemoryLayout<AudioObjectID>.size))
    }
    static func formats(_ id: AudioObjectID, scope: AudioObjectPropertyScope) throws -> [AudioStreamBasicDescription] {
        try ids(id, kAudioDevicePropertyStreams, scope: scope).map {
            try value($0, kAudioStreamPropertyVirtualFormat, initial: AudioStreamBasicDescription())
        }
    }
}

public struct OutputDevice: Identifiable, Equatable {
    public let objectID: AudioObjectID
    public let uid: String
    public let name: String
    public let sampleRate: Double
    public let inputChannels: Int
    public let outputChannels: Int
    public let transport: UInt32
    public var id: String { uid }
    public var isBluetooth: Bool {
        transport == kAudioDeviceTransportTypeBluetooth || transport == kAudioDeviceTransportTypeBluetoothLE
    }
    public var supportsPrototype: Bool { inputChannels == 0 && outputChannels == 2 }

    public static func defaultOutputUID() throws -> String {
        let id = try HAL.value(HAL.system, kAudioHardwarePropertyDefaultOutputDevice, initial: AudioObjectID(0))
        return try HAL.string(id, kAudioDevicePropertyDeviceUID)
    }

    public static func discover() throws -> [OutputDevice] {
        try HAL.ids(HAL.system, kAudioHardwarePropertyDevices).compactMap { id in
            let outputs = try HAL.formats(id, scope: kAudioObjectPropertyScopeOutput)
            let channels = outputs.reduce(0) { $0 + Int($1.mChannelsPerFrame) }
            guard channels > 0 else { return nil }
            let transport = try HAL.value(id, kAudioDevicePropertyTransportType, initial: UInt32(0))
            guard transport != kAudioDeviceTransportTypeAggregate,
                  transport != kAudioDeviceTransportTypeVirtual else { return nil }
            return OutputDevice(objectID: id,
                uid: try HAL.string(id, kAudioDevicePropertyDeviceUID),
                name: try HAL.string(id, kAudioObjectPropertyName),
                sampleRate: try HAL.value(id, kAudioDevicePropertyNominalSampleRate, initial: Double(0)),
                inputChannels: try HAL.formats(id, scope: kAudioObjectPropertyScopeInput).reduce(0) { $0 + Int($1.mChannelsPerFrame) },
                outputChannels: channels, transport: transport)
        }.sorted { $0.name < $1.name }
    }
}

public struct AudioSource: Equatable {
    public let objectID: AudioObjectID
    public let pid: pid_t
    public let bundleID: String
    public let isPlaying: Bool
}

public enum SourceResolver {
    public static func spotify() throws -> [AudioSource] {
        try HAL.ids(HAL.system, kAudioHardwarePropertyProcessObjectList).compactMap { id in
            let pid = try HAL.value(id, kAudioProcessPropertyPID, initial: pid_t(0))
            guard pid != getpid() else { return nil }
            let bundle = try HAL.string(id, kAudioProcessPropertyBundleID)
            guard bundle == "com.spotify.client" || bundle.hasPrefix("com.spotify.client.") else { return nil }
            // Also require an executable inside Spotify.app, avoiding a name-only PID match.
            var path = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
            guard proc_pidpath(pid, &path, UInt32(path.count)) > 0 else { return nil }
            let executable = String(cString: path)
            guard executable.contains("/Spotify.app/Contents/") else { return nil }
            return AudioSource(objectID: id, pid: pid, bundleID: bundle,
                isPlaying: try HAL.value(id, kAudioProcessPropertyIsRunningOutput, initial: UInt32(0)) != 0)
        }.sorted { $0.objectID < $1.objectID }
    }
}
