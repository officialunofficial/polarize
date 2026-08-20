//! Real macOS framework bindings for `polarize`.
//!
//! This crate implements the traits `polarize-core` defines using
//! ScreenCaptureKit (screen capture), `AXUIElement` via objc2-accessibility
//! (accessibility-tree inspection), `CGEvent` via objc2-core-graphics
//! (synthetic mouse/keyboard input), and objc2-app-kit (window/app
//! enumeration).
//!
//! macOS-only: it builds solely on `target_os = "macos"`, and its real
//! native-API behavior is **not** covered by automated tests anywhere in
//! CI. No CI runner can grant Screen Recording or Accessibility TCC
//! permission, or verify pixel/AX content, headlessly. Verifying this
//! crate's behavior requires a real macOS session with those permissions
//! granted. See the "Testing harness" section of `docs/INVARIANTS.md`.
//!
//! ## What is, and is not, verified
//!
//! Every native call in this crate (`AXUIElementCopyAttributeValue`,
//! `CGEventPost`, `SCScreenshotManager::capture_image`, `NSWorkspace`
//! enumeration, …) has been checked for exactly one thing in this
//! environment: that it **compiles and links** (`cargo build -p
//! polarize-macos`, `cargo clippy -p polarize-macos -- -D warnings`). None
//! of it has been exercised against a real accessibility/screen-recording
//! session — this sandbox has no display and no way to grant Screen
//! Recording or Accessibility TCC permission. A human on a real macOS
//! session with both permissions granted needs to confirm: a `screenshot`
//! call returns real pixels, a `describe` call returns a real AX tree
//! shaped like the target app's actual UI, and a `tap`/`keyboard` call
//! visibly lands on screen. See the "Testing harness" section of
//! `docs/INVARIANTS.md`.
//!
//! Only the pure sub-logic factored out of the native calls — app-identity
//! matching ([`app_lookup`]), modifier/keycode/click-sequence mapping
//! ([`keymap`]), and pixel-rect→normalized-frame clamping ([`geometry`]) —
//! is covered by real `cargo test` runs, since none of it touches a real
//! window server.
#![cfg(target_os = "macos")]

pub mod accessibility;
pub mod action;
pub mod app_lookup;
pub mod applescript;
mod ax_ffi;
pub mod capture;
pub mod clipboard;
mod content;
pub mod geometry;
pub mod hit_test;
pub mod input;
pub mod keymap;
pub mod observer;
pub mod permission_bootstrap;
pub mod session;
pub mod set_value;
pub mod window;
