// swift-tools-version: 6.0
//
// `PolarizeSetupHelper` is a nested AppKit app inside `Polarize.app`
// (see `justfile`'s `bundle-app` recipe). Today it is a skeleton: one
// plain window, no permission logic. PLZ-3 fills it in later as a
// guided-permission helper — see PINV-56 through PINV-65 in
// `docs/INVARIANTS.md`.
//
// `.macOS(.v14)` matches the `macos-26` CI runner and the deployment
// target this repo otherwise assumes. `justfile`'s `build-helper`
// recipe builds this package with `swift build --arch`, one
// architecture at a time — never a universal binary. See PINV-54's
// one-bundle-per-target-triple rule.
import PackageDescription

let package = Package(
    name: "PolarizeSetupHelper",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "PolarizeSetupHelper"
        )
    ]
)
