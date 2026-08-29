// swift-tools-version: 6.0
//
// `PolarizeSetupHelper` is a nested AppKit app inside `Polarize.app`
// (see `justfile`'s `bundle-app` recipe). `SetupHelperCore` holds the
// helper's pure permission-pane logic — argv parsing, deep-link pane
// mapping, and fallback selection — with no AppKit import, so
// `SetupHelperCoreTests` can run in plain `swift test`. See PINV-56
// through PINV-65 in `docs/INVARIANTS.md`.
//
// `.macOS(.v14)` matches the `macos-26` CI runner and the deployment
// target this repo otherwise assumes. `justfile`'s `build-helper`
// recipe builds this package with `swift build --arch`, one
// architecture at a time — never a universal binary. See PINV-54's
// one-bundle-per-target-triple rule. The `PolarizeSetupHelper`
// executable target's name must stay exactly this: `justfile`'s
// `bundle-app` recipe copies a binary of this name out of
// `swift build --show-bin-path`.
import PackageDescription

let package = Package(
    name: "PolarizeSetupHelper",
    platforms: [.macOS(.v14)],
    targets: [
        .target(
            name: "SetupHelperCore"
        ),
        .executableTarget(
            name: "PolarizeSetupHelper",
            dependencies: ["SetupHelperCore"]
        ),
        .testTarget(
            name: "SetupHelperCoreTests",
            dependencies: ["SetupHelperCore"]
        ),
    ]
)
