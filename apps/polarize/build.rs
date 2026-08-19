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
}
