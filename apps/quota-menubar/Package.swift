// swift-tools-version: 5.9
import PackageDescription

// ProvenantKit is the shared CLI client. The menu agent stays on macOS 14; the console checks for macOS 26 at launch.
let package = Package(
    name: "quota-menubar",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "quota-menubar", targets: ["QuotaMenuBar"]),
        .executable(name: "provenant-console", targets: ["ProvenantConsole"]),
        .library(name: "ProvenantKit", targets: ["ProvenantKit"])
    ],
    targets: [
        .target(name: "ProvenantKit", path: "Sources/ProvenantKit"),
        .executableTarget(
            name: "QuotaMenuBar",
            dependencies: ["ProvenantKit"],
            path: "Sources/QuotaMenuBar",
            resources: [.process("Resources")]
        ),
        .executableTarget(
            name: "ProvenantConsole",
            dependencies: ["ProvenantKit"],
            path: "Sources/ProvenantConsole"
        ),
        .testTarget(name: "ProvenantKitTests", dependencies: ["ProvenantKit"], resources: [.copy("Fixtures")]),
        .testTarget(name: "QuotaMenuBarTests", dependencies: ["QuotaMenuBar", "ProvenantKit"]),
        .testTarget(name: "ProvenantConsoleTests", dependencies: ["ProvenantConsole", "ProvenantKit"])
    ]
)
