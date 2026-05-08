// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "MDK",
    platforms: [.iOS(.v15)],
    products: [.library(name: "MDKBindings", targets: ["MDKBindings"])],
    targets: [
        .binaryTarget(name: "mdk_uniffiLib", path: "Binary/mdk_uniffi.xcframework"),
        .target(name: "mdk_uniffiFFI", dependencies: ["mdk_uniffiLib"], path: "Sources/mdk_uniffiFFI", publicHeadersPath: "include"),
        .target(name: "MDKBindings", dependencies: ["mdk_uniffiFFI"], path: "Sources/MDKBindings", linkerSettings: [.linkedLibrary("sqlite3"), .linkedLibrary("c++")])
    ]
)
