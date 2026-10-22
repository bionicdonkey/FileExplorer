// swift-tools-version:5.10
import PackageDescription

// The domain layer imports only Foundation, so it builds and tests without Xcode.
let package = Package(
    name: "ExplorerDomain",
    platforms: [.macOS(.v14)],
    products: [.library(name: "ExplorerDomain", targets: ["ExplorerDomain"])],
    targets: [
        .target(name: "ExplorerDomain"),
        .testTarget(name: "ExplorerDomainTests", dependencies: ["ExplorerDomain"]),
    ]
)
