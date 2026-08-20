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
pub mod coords;
pub mod error;
pub mod orchestrate;
pub mod permission;
pub mod schema;
pub mod selector;
pub mod traits;
