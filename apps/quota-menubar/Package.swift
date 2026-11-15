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
            dependencies: ["ProvenantKit", "ProvenantDesign"],
            path: "Sources/QuotaMenuBar"
        ),
        // Design tokens and the small components every console screen shares. SwiftUI only.
        .target(name: "ProvenantDesign", path: "Sources/ProvenantDesign"),
        .executableTarget(
            name: "ProvenantConsole",
            dependencies: ["ProvenantKit", "ProvenantDesign"],
            path: "Sources/ProvenantConsole"
        ),
        // Recorded CLI output and the one Replay both test targets use. Not a product.
        .target(name: "ProvenantTestSupport", dependencies: ["ProvenantKit"], path: "Tests/ProvenantTestSupport",
                exclude: [], resources: [.copy("Fixtures")]),
        .testTarget(name: "ProvenantKitTests", dependencies: ["ProvenantKit", "ProvenantTestSupport"]),
        .testTarget(name: "QuotaMenuBarTests", dependencies: ["QuotaMenuBar", "ProvenantKit"]),
        .testTarget(name: "ProvenantConsoleTests", dependencies: ["ProvenantConsole", "ProvenantKit", "ProvenantDesign", "ProvenantTestSupport"])
    ]
)
