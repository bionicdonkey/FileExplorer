// swift-tools-version:5.10
import PackageDescription

let package = Package(
    name: "ExplorerSpecRunner",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "ExplorerSpecRunner", targets: ["ExplorerSpecRunner"]),
        .executable(name: "spec-runner", targets: ["spec-runner"]),
    ],
    dependencies: [
        .package(path: "../ExplorerDomain"),
        .package(path: "../ExplorerAdapters"),
    ],
    targets: [
        .target(name: "ExplorerSpecRunner", dependencies: ["ExplorerDomain", "ExplorerAdapters"]),
        .executableTarget(name: "spec-runner", dependencies: ["ExplorerSpecRunner"]),
        .testTarget(name: "ExplorerSpecRunnerTests", dependencies: ["ExplorerSpecRunner"]),
    ]
)
