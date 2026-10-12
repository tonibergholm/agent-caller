// swift-tools-version: 6.0
// The voice bridge (voice spec §4.2, step-2 plan S1). Built into the app by
// build.rs through swift-rs; the C contract is VoiceBridgeC/voice_bridge.h.
import PackageDescription

// FoundationModels exists only on macOS 26. The probe (Task 1A) uses it, so
// it is weak-linked: autolinking is off, and the link names it with
// -weak_framework (plan S16). build.rs does the same for the app's link.
#if compiler(>=6.2)
let swiftSettings: [SwiftSetting] = [
    .swiftLanguageMode(.v5),
    .unsafeFlags(["-Xfrontend", "-disable-autolink-framework", "-Xfrontend", "FoundationModels"]),
]
let linkerSettings: [LinkerSetting] = [.unsafeFlags(["-Xlinker", "-weak_framework", "-Xlinker", "FoundationModels"])]
#else
let swiftSettings: [SwiftSetting] = [.swiftLanguageMode(.v5)]
let linkerSettings: [LinkerSetting] = []
#endif

let package = Package(
    name: "VoiceBridge",
    // The app's minimum macOS (plan S16). Voice itself needs macOS 26 and
    // checks at run time.
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "VoiceBridge", type: .static, targets: ["VoiceBridge"]),
    ],
    targets: [
        .target(name: "VoiceBridgeC", path: "Sources/VoiceBridgeC"),
        .target(
            name: "VoiceBridge",
            dependencies: ["VoiceBridgeC"],
            path: "Sources/VoiceBridge",
            swiftSettings: swiftSettings,
            linkerSettings: linkerSettings
        ),
        .testTarget(
            name: "VoiceBridgeTests",
            dependencies: ["VoiceBridge", "VoiceBridgeC"],
            path: "Tests/VoiceBridgeTests",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
