// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "quota-menubar",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(
            name: "quota-menubar",
            targets: ["QuotaMenuBar"]
        )
    ],
    targets: [
        .executableTarget(
            name: "QuotaMenuBar",
            path: "Sources/QuotaMenuBar"
        ),
        .testTarget(name: "QuotaMenuBarTests", dependencies: ["QuotaMenuBar"])
    ]
)
