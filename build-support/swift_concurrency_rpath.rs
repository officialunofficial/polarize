// Shared by `crates/polarize-macos/build.rs` and `apps/polarize/build.rs`
// via `include!`, not a crate of its own — see either of those files
// for why they both need this. `include!` keeps this shape in one
// place without a separate build-dependency crate for two call sites.

/// The Xcode toolchain's Swift-5.5-back-deployment library paths, if
/// `xcode-select -p` resolves. Empty when it does not — a build.rs
/// including this always still emits the plain `/usr/lib/swift` rpath
/// on its own; this only adds the Xcode-toolchain ones on top.
fn xcode_swift_lib_paths() -> Vec<String> {
    let Ok(output) = std::process::Command::new("xcode-select")
        .arg("-p")
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let xcode_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let toolchain = format!("{xcode_path}/Toolchains/XcodeDefault.xctoolchain/usr/lib");
    ["swift-5.5/macosx", "swift/macosx"]
        .into_iter()
        .map(|suffix| format!("{toolchain}/{suffix}"))
        .collect()
}
