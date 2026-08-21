//! Rebakes the Swift Concurrency runtime rpath into the `polarize`
//! binary.
//!
//! The `screencapturekit` crate's own build script emits
//! `cargo:rustc-link-arg=-Wl,-rpath,...` for the Swift runtime, but
//! plain `rustc-link-arg` only applies to targets built by the
//! *emitting* package — see
//! <https://doc.rust-lang.org/cargo/reference/build-scripts.html#rustc-link-arg>.
//! `apps/polarize` only depends on `screencapturekit` through
//! `polarize-macos`; it never builds `screencapturekit`'s own targets.
//! So that rpath never reaches this binary, which then fails to launch
//! with `dyld: Library not loaded: @rpath/libswift_Concurrency.dylib`.
//! Re-emitting the same rpath flags here, for the package that owns
//! the `polarize` bin target, fixes that.

// Shared with `crates/polarize-macos/build.rs` — see that file's own
// doc comment for why both need the same lookup, and why it lives in
// one `include!`d file instead of a separate build-dependency crate.
include!("../../build-support/swift_concurrency_rpath.rs");

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    println!("cargo:rustc-link-arg-bins=-Wl,-rpath,/usr/lib/swift");
    for path in xcode_swift_lib_paths() {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{path}");
    }

    embed_info_plist();
}

/// Embeds a real `Info.plist` into the `polarize` binary's
/// `__TEXT,__info_plist` section.
///
/// macOS's Automation TCC grant is not attributed to whichever process
/// literally calls the Apple Event API. It climbs to the nearest
/// ancestor process that carries a `CFBundleIdentifier` — its
/// "responsible process." A bare Mach-O binary with no embedded
/// `Info.plist` has no bundle identity of its own to climb to, so TCC
/// always attributes its Apple Event sends to whatever bundled app
/// happens to own its process tree (the terminal or MCP client that
/// launched it) instead of to `polarize` itself. That grant can vanish
/// or differ across launch contexts, and `polarize`'s own
/// `AEDeterminePermissionToAutomateTarget` query — which does ask
/// about *this* process's identity — then reports `NotDetermined`
/// even when a real send just silently succeeded under someone else's
/// grant. Embedding `CFBundleIdentifier` here gives `polarize` its own
/// stable identity to hold that grant against, matching
/// `justfile`'s `--identifier` (code-signing identity and TCC's
/// bundle identity are two different things; both are needed).
///
/// This section covers the bare-binary artifact — `cargo build`'s own
/// output, and what `dist` ships in a release. `just bundle-app`
/// assembles a real `Polarize.app` directory around the same signed
/// binary; that bundle's own `Contents/Info.plist` is the identity
/// macOS actually checks when the bundle is present. See PINV-52.
///
/// Both plists render from the same template
/// (`apps/polarize/bundle/Info.plist.in`), so they cannot drift apart.
/// This function only substitutes `__VERSION__`; `just bundle-app`
/// does the same substitution with `sed` for the bundle copy.
fn embed_info_plist() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is always set for a build script");
    let template_path = std::path::Path::new(&manifest_dir).join("bundle/Info.plist.in");
    println!("cargo:rerun-if-changed={}", template_path.display());
    let template = std::fs::read_to_string(&template_path)
        .expect("reading apps/polarize/bundle/Info.plist.in");

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let plist = template.replace("__VERSION__", &version);

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is always set for a build script");
    let plist_path = std::path::Path::new(&out_dir).join("Info.plist");
    std::fs::write(&plist_path, plist).expect("writing the generated Info.plist to OUT_DIR");

    let plist_path = plist_path.to_str().expect("OUT_DIR is valid UTF-8");
    for arg in [
        "-Xlinker",
        "-sectcreate",
        "-Xlinker",
        "__TEXT",
        "-Xlinker",
        "__info_plist",
        "-Xlinker",
        plist_path,
    ] {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}
