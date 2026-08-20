//! Platform-agnostic logic for `polarize`.
//!
//! This crate holds everything that does not need a real macOS session to
//! run or to test: coordinate normalization, the accessibility-tree data
//! model, MCP tool request/response schemas, the traits `polarize-macos`
//! implements, error types, and permission state. It has zero macOS-only
//! dependencies and is fully covered by `cargo test`; see the "Testing
//! harness" section of `docs/INVARIANTS.md`.

pub mod action;
pub mod ax;
/// The pure half of a batched AX attribute read. See PINV-41.
pub mod ax_batch;
pub mod clipboard;
pub mod coords;
pub mod error;
/// OCR fallback for apps with a sparse accessibility tree. See
/// PINV-37 and PINV-38.
pub mod find_text;
pub mod hit_test;
pub mod notifications;
pub mod orchestrate;
pub mod permission;
pub mod process;
pub mod schema;
pub mod script;
pub mod selector;
pub mod session;
pub mod set_value;
pub mod traits;
pub mod wait;
pub mod window_control;
pub mod workspace;
pub mod workspace_events;
