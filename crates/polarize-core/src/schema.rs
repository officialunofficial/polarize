//! MCP tool request/response schemas for `polarize`'s four tools:
//! `screenshot`, `describe`, `tap`, and `keyboard`.
//!
//! `apps/polarize` deserializes an incoming MCP tool call into the
//! matching `*Request` type, drives `polarize-core`/`polarize-macos`
//! from it, and serializes the matching `*Response` type back out. Every
//! type here round-trips through `serde_json` — see the tests.
//!
//! ## PINV-9: screenshots travel as base64, not a file path
//!
//! [`ScreenshotResponse`] carries the PNG as a base64 string in the tool
//! response body rather than writing it to a temp file and returning a
//! path. `polarize` is a stdio MCP server: its client is often a
//! separate, possibly sandboxed process with no shared filesystem
//! namespace guarantee, and MCP's own image content type already expects
//! base64-encoded bytes inline. A file path would need a second
//! out-of-band contract (where the file lives, who deletes it, what
//! happens if the client reads it before/after the server exits); base64
//! in the response has none of that and keeps every tool response
//! self-contained.
//!
//! ## Why these types also derive `JsonSchema`
//!
//! `apps/polarize`'s `rmcp`-based server hands each `*Request` type to
//! `rmcp`'s `#[tool]` macro as its `Parameters<T>` input, and each
//! `*Response` type back out as `Json<T>` structured output; both need
//! `schemars::JsonSchema` to generate the tool's advertised input/output
//! schema. Deriving it here, next to `Serialize`/`Deserialize`, keeps the
//! wire contract in one place instead of duplicating these types as
//! schema-only wrappers in `apps/polarize`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ax::AxNode;

/// Identifies a running app by bundle id, by name, or both. At least one
/// should be set by callers; `polarize-macos` tries `bundle_id` first
/// when both are present.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct AppIdentifier {
    pub bundle_id: Option<String>,
    pub app_name: Option<String>,
}

/// What a `screenshot` call should capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum ScreenshotTarget {
    /// The whole screen. `display_id` selects a specific display on a
    /// multi-monitor setup; `None` means the main display.
    Screen { display_id: Option<u32> },
    /// The frontmost (or only) window of a named app.
    App { app: AppIdentifier },
    /// A specific window of a named app, matched by title.
    Window {
        app: AppIdentifier,
        window_title: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScreenshotRequest {
    pub target: ScreenshotTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScreenshotResponse {
    /// Standard-alphabet, padded base64 encoding of the captured PNG.
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
}

impl ScreenshotResponse {
    /// Builds a response from raw PNG bytes, base64-encoding them.
    pub fn from_png_bytes(png_bytes: &[u8], width: u32, height: u32) -> Self {
        Self {
            png_base64: BASE64.encode(png_bytes),
            width,
            height,
        }
    }

    /// Decodes [`Self::png_base64`] back to raw PNG bytes.
    pub fn decode_png_bytes(&self) -> Result<Vec<u8>, base64::DecodeError> {
        BASE64.decode(&self.png_base64)
    }
}

/// `describe` inspects the frontmost app when `app` is `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DescribeRequest {
    pub app: Option<AppIdentifier>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DescribeResponse {
    pub app_name: String,
    /// The resolved app's bundle id, when it publishes one. A caller
    /// naming an app in a follow-up call should prefer this over
    /// `app_name`: two processes can share one localized name.
    #[serde(default)]
    pub bundle_id: Option<String>,
    pub root: AxNode,
    /// [`crate::ax::format_tree`]'s indented text rendering of `root` — a
    /// ready-to-read tree, so a caller does not need to walk `root` itself
    /// just to see the app's structure. See PINV-3.
    pub formatted: String,
}

/// A synthetic mouse click at a normalized `[0.0, 1.0]` fraction point —
/// the same coordinate contract as argent's `gesture-tap`. `target`
/// scopes the fraction to a screen or a window; `None` means the main
/// screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TapRequest {
    pub x: f64,
    pub y: f64,
    pub target: Option<ScreenshotTarget>,
    /// `1` for a single click, `2` for a double-click. Defaults to `1`
    /// when omitted.
    pub click_count: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TapResponse {
    pub tapped: bool,
    /// The pixel point the fraction resolved to, for debugging.
    pub pixel_x: f64,
    pub pixel_y: f64,
    /// Which native path actually posted this click. See PINV-47.
    pub post_path: PostPath,
}

/// Which native path posted one `tap`'s click. See PINV-47 in
/// `docs/INVARIANTS.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostPath {
    /// Posted straight into the target app's own event queue, through
    /// `SLEventPostToPid`. The shared cursor did not move.
    Pid,
    /// Posted through the global `CGEvent` stream. This was `tap`'s
    /// only path before PINV-47. The shared cursor still moves.
    Global,
}

/// A named, non-printable key `keyboard` can press, independent of
/// keyboard layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NamedKey {
    Return,
    Tab,
    Escape,
    Delete,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Space,
}

/// A modifier key held during a [`NamedKey`] press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    Command,
    Shift,
    Option,
    Control,
}

/// `keyboard` either types a literal string or presses one named key
/// with optional modifiers; it cannot do both in one call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum KeyboardRequest {
    Type {
        text: String,
        target: Option<AppIdentifier>,
    },
    KeyPress {
        key: NamedKey,
        #[serde(default)]
        modifiers: Vec<Modifier>,
        target: Option<AppIdentifier>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KeyboardResponse {
    pub sent: bool,
    /// How `target` got activated before this call posted its keys.
    /// See PINV-48.
    pub activation_path: ActivationPath,
}

/// How a `keyboard` request's `target` got activated. See PINV-48 in
/// `docs/INVARIANTS.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivationPath {
    /// `target` was `None`. Neither activation method ran.
    None,
    /// Activated without raising the window or switching the current
    /// Space.
    RaiseFree,
    /// Activated through `NSRunningApplication`, as `keyboard` always
    /// did before PINV-48. The window raises. The current Space can
    /// switch.
    Raised,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::NormalizedFrame;

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let json = serde_json::to_string(value).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    #[test]
    fn app_identifier_round_trips() {
        let value = AppIdentifier {
            bundle_id: Some("com.apple.TextEdit".to_string()),
            app_name: None,
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn screenshot_request_screen_round_trips() {
        let value = ScreenshotRequest {
            target: ScreenshotTarget::Screen {
                display_id: Some(1),
            },
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn screenshot_request_window_round_trips() {
        let value = ScreenshotRequest {
            target: ScreenshotTarget::Window {
                app: AppIdentifier {
                    bundle_id: None,
                    app_name: Some("Notes".to_string()),
                },
                window_title: "Untitled".to_string(),
            },
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn screenshot_response_round_trips() {
        let value = ScreenshotResponse {
            png_base64: "aGVsbG8=".to_string(),
            width: 1920,
            height: 1080,
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn screenshot_response_encodes_and_decodes_png_bytes() {
        let bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x00, 0x01, 0x02, 0x03];
        let response = ScreenshotResponse::from_png_bytes(&bytes, 100, 200);
        assert_eq!(response.decode_png_bytes().unwrap(), bytes);
    }

    #[test]
    fn describe_request_round_trips_with_and_without_app() {
        let with_app = DescribeRequest {
            app: Some(AppIdentifier {
                bundle_id: Some("com.apple.Finder".to_string()),
                app_name: None,
            }),
        };
        assert_eq!(round_trip(&with_app), with_app);

        let without_app = DescribeRequest { app: None };
        assert_eq!(round_trip(&without_app), without_app);
    }

    #[test]
    fn describe_response_round_trips() {
        let value = DescribeResponse {
            app_name: "Finder".to_string(),
            bundle_id: Some("com.apple.finder".to_string()),
            root: AxNode {
                role: "AXWindow".to_string(),
                label: Some("Untitled".to_string()),
                frame: NormalizedFrame {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                children: vec![],
                ..AxNode::default()
            },
            formatted: "AXWindow \"Untitled\" (0.00,0.00,1.00,1.00)".to_string(),
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn tap_request_round_trips() {
        let value = TapRequest {
            x: 0.5,
            y: 0.25,
            target: Some(ScreenshotTarget::Screen { display_id: None }),
            click_count: Some(2),
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn tap_response_round_trips() {
        let value = TapResponse {
            tapped: true,
            pixel_x: 960.0,
            pixel_y: 270.0,
            post_path: PostPath::Pid,
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn tap_response_round_trips_the_global_post_path() {
        let value = TapResponse {
            tapped: true,
            pixel_x: 960.0,
            pixel_y: 270.0,
            post_path: PostPath::Global,
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn keyboard_request_type_variant_round_trips() {
        let value = KeyboardRequest::Type {
            text: "hello world".to_string(),
            target: None,
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn keyboard_request_key_press_variant_round_trips() {
        let value = KeyboardRequest::KeyPress {
            key: NamedKey::Return,
            modifiers: vec![Modifier::Command, Modifier::Shift],
            target: Some(AppIdentifier {
                bundle_id: None,
                app_name: Some("Safari".to_string()),
            }),
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn keyboard_request_key_press_defaults_modifiers_when_absent() {
        let json = r#"{"action":"key_press","key":"escape","target":null}"#;
        let value: KeyboardRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            value,
            KeyboardRequest::KeyPress {
                key: NamedKey::Escape,
                modifiers: vec![],
                target: None
            }
        );
    }

    #[test]
    fn keyboard_response_round_trips() {
        let value = KeyboardResponse {
            sent: true,
            activation_path: ActivationPath::RaiseFree,
        };
        assert_eq!(round_trip(&value), value);
    }

    #[test]
    fn keyboard_response_round_trips_every_activation_path() {
        for activation_path in [
            ActivationPath::None,
            ActivationPath::RaiseFree,
            ActivationPath::Raised,
        ] {
            let value = KeyboardResponse {
                sent: true,
                activation_path,
            };
            assert_eq!(round_trip(&value), value);
        }
    }
}
