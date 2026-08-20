//! The `clipboard_read` and `clipboard_write` tools, over the general
//! pasteboard.
//!
//! A caller uses the clipboard as a fast, reliable channel into an app.
//! It writes text, then presses Command+V. That beats typing a long
//! string one synthetic key at a time. It reads the clipboard to check
//! what a "Copy" command produced.
//!
//! ## Why a read needs care, and a write does not
//!
//! macOS 26 protects pasteboard contents. A read that no user paste
//! gesture preceded can prompt the user, and it can return nothing at
//! all. `NSPasteboard` reports that refusal the same way it reports an
//! empty pasteboard: it hands back no string. The two facts are not the
//! same. "The clipboard holds no text" is an answer. "macOS refused the
//! read" is a permission problem the user must fix.
//!
//! [`classify_read`] separates them, and PINV-34 states the rule. The
//! pasteboard publishes its type list without giving up the contents.
//! So a declared type with no value means macOS withheld the value.
//!
//! A write has none of this. macOS never refuses one.
//!
//! ## One content type, named in the request
//!
//! [`ClipboardContentType`] carries one variant today: plain text. The
//! request still names it. A later type is then an added variant, and no
//! request changes meaning. `polarize` never coerces one type into
//! another: a caller that asks for a type the pasteboard does not hold
//! reads `text: None`, not a converted value.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::PolarizeError;
use crate::permission::{PermissionError, PermissionKind, PermissionState};
use crate::traits::ClipboardAccess;

/// Which pasteboard content type a clipboard tool reads or writes.
///
/// `PlainText` maps to `NSPasteboardTypeString`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardContentType {
    #[default]
    PlainText,
}

/// What the pasteboard reported for one content type, before
/// [`classify_read`] reads a meaning into it.
///
/// `polarize-macos` fills this in from two `NSPasteboard` calls that
/// cannot run in CI. The decision that follows is pure.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawClipboardRead {
    /// `true` when the pasteboard lists the requested type.
    pub declared: bool,
    /// The value the pasteboard handed over, if it handed over one.
    pub value: Option<String>,
}

/// A `clipboard_read` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ClipboardReadRequest {
    /// Which content type to read. Defaults to plain text.
    #[serde(default)]
    pub content_type: ClipboardContentType,
}

/// The result of a `clipboard_read` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClipboardReadResponse {
    /// The content type the tool read.
    pub content_type: ClipboardContentType,
    /// The pasteboard contents. `None` means the pasteboard holds
    /// nothing of that type. A refused read is an error instead, never
    /// `None` and never `Some("")` (PINV-34).
    #[serde(default)]
    pub text: Option<String>,
}

/// A `clipboard_write` tool call.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ClipboardWriteRequest {
    /// Which content type to write. Defaults to plain text.
    #[serde(default)]
    pub content_type: ClipboardContentType,
    /// The text to put on the pasteboard. It replaces the whole
    /// contents.
    pub text: String,
}

/// The result of a `clipboard_write` tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClipboardWriteResponse {
    /// Always `true`. A failed write returns an error instead.
    pub written: bool,
    /// The content type the tool wrote.
    pub content_type: ClipboardContentType,
    /// How many characters the tool wrote, so a caller can confirm the
    /// length it meant to send.
    pub characters: usize,
}

/// # PINV-34: a refused clipboard read is a permission error, not empty text
///
/// - Always: [`classify_read`] reports `Ok(None)` only when the
///   pasteboard does not list the requested type. It reports
///   `Err(PermissionError::NotGranted)` with
///   [`PermissionKind::Clipboard`] when the pasteboard lists the type
///   and still hands over no value. It reports `Ok(Some(text))` whenever
///   a value arrives, and an empty string is such a value.
/// - Because: macOS 26 can withhold pasteboard contents from a read that
///   no user paste gesture preceded. `NSPasteboard` signals that refusal
///   by handing back no string, which is exactly what an empty
///   pasteboard does. A caller that reads a refusal as "the clipboard is
///   empty" then copies again, reads nothing again, and never learns
///   that only the user can repair the state.
/// - If violated: a `clipboard_read` call answers "" forever on a Mac
///   whose clipboard holds real text, and no error names the cause.
///
/// The state reported is [`PermissionState::NotDetermined`], never
/// `Denied`. The pasteboard gives no evidence that the user made an
/// explicit choice. This matches PINV-11.
pub fn classify_read(raw: &RawClipboardRead) -> Result<Option<String>, PermissionError> {
    match (raw.declared, &raw.value) {
        // A value arrived. The pasteboard held it, whatever its type
        // list said.
        (_, Some(text)) => Ok(Some(text.clone())),
        // The type is on offer, and the value is not. macOS withheld it.
        (true, None) => Err(PermissionError::NotGranted {
            kind: PermissionKind::Clipboard,
            state: PermissionState::NotDetermined,
        }),
        // The pasteboard holds nothing of this type.
        (false, None) => Ok(None),
    }
}

/// Reads the pasteboard through [`ClipboardAccess`], then applies
/// [`classify_read`] (PINV-34).
pub fn perform_clipboard_read<C>(
    clipboard: &C,
    request: &ClipboardReadRequest,
) -> Result<ClipboardReadResponse, PolarizeError>
where
    C: ClipboardAccess,
{
    let raw = clipboard.read_clipboard(request.content_type)?;
    let text = classify_read(&raw)?;
    Ok(ClipboardReadResponse {
        content_type: request.content_type,
        text,
    })
}

/// Writes `request.text` to the pasteboard through [`ClipboardAccess`].
pub fn perform_clipboard_write<C>(
    clipboard: &C,
    request: &ClipboardWriteRequest,
) -> Result<ClipboardWriteResponse, PolarizeError>
where
    C: ClipboardAccess,
{
    clipboard.write_clipboard(request.content_type, &request.text)?;
    Ok(ClipboardWriteResponse {
        written: true,
        content_type: request.content_type,
        characters: request.text.chars().count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ---- fake --------------------------------------------------------

    #[derive(Default)]
    struct FakeClipboard {
        read: RawClipboardRead,
        read_calls: RefCell<Vec<ClipboardContentType>>,
        writes: RefCell<Vec<(ClipboardContentType, String)>>,
        write_fails: bool,
    }

    impl FakeClipboard {
        fn holding(text: &str) -> Self {
            Self {
                read: RawClipboardRead {
                    declared: true,
                    value: Some(text.to_string()),
                },
                ..Self::default()
            }
        }

        fn refusing() -> Self {
            Self {
                read: RawClipboardRead {
                    declared: true,
                    value: None,
                },
                ..Self::default()
            }
        }
    }

    impl ClipboardAccess for FakeClipboard {
        fn read_clipboard(
            &self,
            content_type: ClipboardContentType,
        ) -> Result<RawClipboardRead, PolarizeError> {
            self.read_calls.borrow_mut().push(content_type);
            Ok(self.read.clone())
        }

        fn write_clipboard(
            &self,
            content_type: ClipboardContentType,
            text: &str,
        ) -> Result<(), PolarizeError> {
            if self.write_fails {
                return Err(PolarizeError::Platform(
                    "NSPasteboard setString:forType: reported false".to_string(),
                ));
            }
            self.writes
                .borrow_mut()
                .push((content_type, text.to_string()));
            Ok(())
        }
    }

    // ---- classify_read (PINV-34) -------------------------------------

    #[test]
    fn a_declared_type_with_a_value_reads_as_text() {
        let raw = RawClipboardRead {
            declared: true,
            value: Some("hello".to_string()),
        };
        assert_eq!(classify_read(&raw).unwrap(), Some("hello".to_string()));
    }

    #[test]
    fn an_undeclared_type_reads_as_no_text() {
        let raw = RawClipboardRead {
            declared: false,
            value: None,
        };
        assert_eq!(classify_read(&raw).unwrap(), None);
    }

    /// PINV-34. This is the whole point of the classification.
    #[test]
    fn a_declared_type_with_no_value_reads_as_a_refusal() {
        let raw = RawClipboardRead {
            declared: true,
            value: None,
        };
        let err = classify_read(&raw).unwrap_err();
        assert_eq!(
            err,
            PermissionError::NotGranted {
                kind: PermissionKind::Clipboard,
                state: PermissionState::NotDetermined,
            }
        );
        assert_eq!(
            err.to_string(),
            "Clipboard permission is NotDetermined, not granted"
        );
    }

    #[test]
    fn a_refusal_never_reports_denied() {
        let raw = RawClipboardRead {
            declared: true,
            value: None,
        };
        let PermissionError::NotGranted { state, .. } = classify_read(&raw).unwrap_err();
        assert_eq!(state, PermissionState::NotDetermined);
    }

    #[test]
    fn an_empty_string_is_text_not_an_absent_value() {
        let raw = RawClipboardRead {
            declared: true,
            value: Some(String::new()),
        };
        assert_eq!(classify_read(&raw).unwrap(), Some(String::new()));
    }

    #[test]
    fn a_value_arrives_even_when_the_type_list_did_not_name_it() {
        let raw = RawClipboardRead {
            declared: false,
            value: Some("hello".to_string()),
        };
        assert_eq!(classify_read(&raw).unwrap(), Some("hello".to_string()));
    }

    // ---- read orchestration ------------------------------------------

    #[test]
    fn read_reports_the_pasteboard_text() {
        let clipboard = FakeClipboard::holding("hello");
        let response =
            perform_clipboard_read(&clipboard, &ClipboardReadRequest::default()).unwrap();
        assert_eq!(response.text.as_deref(), Some("hello"));
        assert_eq!(response.content_type, ClipboardContentType::PlainText);
    }

    #[test]
    fn read_asks_the_platform_for_the_requested_type() {
        let clipboard = FakeClipboard::holding("hello");
        perform_clipboard_read(
            &clipboard,
            &ClipboardReadRequest {
                content_type: ClipboardContentType::PlainText,
            },
        )
        .unwrap();
        assert_eq!(
            clipboard.read_calls.borrow().as_slice(),
            [ClipboardContentType::PlainText]
        );
    }

    #[test]
    fn read_reports_an_empty_clipboard_as_no_text() {
        let clipboard = FakeClipboard::default();
        let response =
            perform_clipboard_read(&clipboard, &ClipboardReadRequest::default()).unwrap();
        assert_eq!(response.text, None);
    }

    /// PINV-34, through the tool the caller actually calls.
    #[test]
    fn read_reports_a_refusal_as_a_permission_error() {
        let clipboard = FakeClipboard::refusing();
        let err = perform_clipboard_read(&clipboard, &ClipboardReadRequest::default()).unwrap_err();
        assert!(matches!(err, PolarizeError::Permission(_)));
        assert!(err.to_string().contains("Clipboard"));
    }

    // ---- write orchestration -----------------------------------------

    #[test]
    fn write_sends_the_text_and_the_type_to_the_platform() {
        let clipboard = FakeClipboard::default();
        let response = perform_clipboard_write(
            &clipboard,
            &ClipboardWriteRequest {
                content_type: ClipboardContentType::PlainText,
                text: "hello".to_string(),
            },
        )
        .unwrap();
        assert!(response.written);
        assert_eq!(response.characters, 5);
        assert_eq!(
            clipboard.writes.borrow().as_slice(),
            [(ClipboardContentType::PlainText, "hello".to_string())]
        );
    }

    #[test]
    fn write_counts_characters_not_bytes() {
        let clipboard = FakeClipboard::default();
        let response = perform_clipboard_write(
            &clipboard,
            &ClipboardWriteRequest {
                content_type: ClipboardContentType::PlainText,
                text: "héllo".to_string(),
            },
        )
        .unwrap();
        assert_eq!(response.characters, 5);
    }

    #[test]
    fn a_failed_write_reports_the_platform_error() {
        let clipboard = FakeClipboard {
            write_fails: true,
            ..FakeClipboard::default()
        };
        let err = perform_clipboard_write(
            &clipboard,
            &ClipboardWriteRequest {
                content_type: ClipboardContentType::PlainText,
                text: "hello".to_string(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, PolarizeError::Platform(_)));
    }

    // ---- wire contract -----------------------------------------------

    #[test]
    fn a_read_request_defaults_to_plain_text() {
        let request: ClipboardReadRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(request.content_type, ClipboardContentType::PlainText);
    }

    #[test]
    fn a_write_request_round_trips() {
        let request = ClipboardWriteRequest {
            content_type: ClipboardContentType::PlainText,
            text: "hello".to_string(),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("plain_text"));
        assert_eq!(
            serde_json::from_str::<ClipboardWriteRequest>(&json).unwrap(),
            request
        );
    }

    /// No silent coercion: a type `polarize` does not support fails to
    /// deserialize instead of falling back to plain text.
    #[test]
    fn an_unsupported_content_type_is_rejected() {
        let result = serde_json::from_str::<ClipboardReadRequest>(r#"{"content_type":"html"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn a_read_response_round_trips_with_no_text() {
        let response = ClipboardReadResponse {
            content_type: ClipboardContentType::PlainText,
            text: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("null"));
        assert_eq!(
            serde_json::from_str::<ClipboardReadResponse>(&json).unwrap(),
            response
        );
    }
}
