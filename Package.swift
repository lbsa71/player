// swift-tools-version: 5.10
import PackageDescription

let package = Package(
    name: "Player",
    platforms: [.macOS("14.4")],
    products: [
        .executable(name: "Player", targets: ["Player"]),
        .executable(name: "PlayerMCP", targets: ["PlayerMCP"]),
        .executable(name: "AudioBridge", targets: ["AudioBridge"])
    ],
    targets: [
        .target(name: "RealtimeAudio", publicHeadersPath: "include",
                linkerSettings: [.linkedFramework("CoreAudio")]),
        .target(name: "PlayerAudio", dependencies: ["RealtimeAudio"]),
        .target(name: "PlayerControl"),
        .target(name: "SpotifyKit"),
        .executableTarget(name: "Player", dependencies: ["PlayerAudio", "PlayerControl", "SpotifyKit"]),
        .executableTarget(name: "PlayerMCP", dependencies: ["PlayerControl"]),
        .executableTarget(name: "AudioBridge", dependencies: ["PlayerAudio", "PlayerControl"]),
        .testTarget(name: "PlayerAudioTests", dependencies: ["PlayerAudio", "RealtimeAudio"]),
        .testTarget(name: "SpotifyKitTests", dependencies: ["SpotifyKit"])
    ]
)
