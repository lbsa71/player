import Foundation

public enum BridgeControl {
    public static var directory: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/AudioBridge",isDirectory:true)
    }
    public static func request(_ method: String, arguments: [String: Any] = [:]) throws -> [String: Any] {
        try LocalControl.request(method,arguments:arguments,directory:directory)
    }
}
