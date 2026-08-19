//! Rebakes the Swift Concurrency runtime rpath for every downstream
//! binary and test target.
//!
//! The `screencapturekit` crate's own build script emits
//! `cargo:rustc-link-arg=-Wl,-rpath,...` for the Swift runtime. Cargo
//! only applies plain `rustc-link-arg` to targets built by the
//! *emitting* package itself — see
//! <https://doc.rust-lang.org/cargo/reference/build-scripts.html#rustc-link-arg>.
//! `polarize-macos` and `apps/polarize` only depend on
//! `screencapturekit`; they do not build any of its own targets. So
//! that rpath never reaches `polarize`'s final binary, and it fails to
//! launch with `dyld: Library not loaded: @rpath/libswift_Concurrency.dylib`.
//!
//! The plain `rustc-link-arg` instruction re-bakes the rpath into this
//! crate's own unit-test binary (the `-tests`-suffixed variant needs a
//! `tests/` integration-test directory to count as "a test target",
//! which this crate does not have). `apps/polarize`'s own `build.rs`
//! re-emits the same flags, via `rustc-link-arg-bins`, for the
//! `polarize` binary.

// Shared with `apps/polarize/build.rs` — see that file's own doc
// comment for why both need the same lookup, and why it lives in one
// `include!`d file instead of a separate build-dependency crate.
include!("../../build-support/swift_concurrency_rpath.rs");

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    for path in xcode_swift_lib_paths() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{path}");
    }
}
