// swift-tools-version:5.10
import PackageDescription

let package = Package(
    name: "ExplorerAdapters",
    platforms: [.macOS(.v14)],
    products: [.library(name: "ExplorerAdapters", targets: ["ExplorerAdapters"])],
    dependencies: [.package(path: "../ExplorerDomain")],
    targets: [
        // The system libarchive, through a hand-written header: the SDK ships the library but
        // not its header. Only the functions the adapters call are declared.
        .target(name: "CArchive", linkerSettings: [.linkedLibrary("archive")]),
        .target(name: "ExplorerAdapters", dependencies: ["ExplorerDomain", "CArchive"]),
        .testTarget(name: "ExplorerAdaptersTests", dependencies: ["ExplorerAdapters"]),
    ]
)
