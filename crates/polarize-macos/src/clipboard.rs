//! [`ClipboardAccess`] over `NSPasteboard` (objc2-app-kit).
//!
//! Real native calls throughout; see the crate-level "what is and is not
//! verified" note. No clipboard call has run against a real pasteboard
//! in this environment.
//!
//! ## Why a read makes two calls
//!
//! `stringForType:` alone cannot tell an empty pasteboard from a
//! refused read. Both hand back `nil`. macOS 26 refuses a programmatic
//! read that no user paste gesture preceded, so the refusal is a real
//! case, not a rare one.
//!
//! `availableTypeFromArray:` reads only the pasteboard's type list. That
//! list stays readable while the contents do not. So a type on the list
//! plus a `nil` string means macOS withheld the contents.
//! [`polarize_core::clipboard::classify_read`] applies that rule, and
//! real unit tests cover it (PINV-34).
//!
//! ## Why there is no session preflight
//!
//! PINV-23 makes every tool that captures pixels, reads the
//! accessibility tree, or posts input call
//! [`crate::session::ensure_session_usable`] first. A clipboard call
//! does none of those three. The pasteboard belongs to the login
//! session, not to the display. It works while the screen is locked,
//! and it works while another user holds the console. A preflight there
//! would invent a failure a caller would otherwise never meet. This is
//! the same exclusion PINV-23 already grants the two AppleScript tools.
//!
//! There is no TCC preflight either. `polarize` cannot ask macOS about
//! pasteboard access before it reads. The read itself is the only test,
//! and PINV-34 classifies its result.

use objc2_app_kit::{NSPasteboard, NSPasteboardType, NSPasteboardTypeString};
use objc2_foundation::{NSArray, NSString};
use polarize_core::clipboard::{ClipboardContentType, RawClipboardRead};
use polarize_core::error::PolarizeError;
use polarize_core::traits::ClipboardAccess;

/// `ClipboardAccess` implementation over `NSPasteboard.general`.
#[derive(Debug, Default)]
pub struct MacClipboard;

impl ClipboardAccess for MacClipboard {
    fn read_clipboard(
        &self,
        content_type: ClipboardContentType,
    ) -> Result<RawClipboardRead, PolarizeError> {
        let pasteboard = NSPasteboard::generalPasteboard();
        let wanted = pasteboard_type(content_type);
        let offered = NSArray::from_slice(&[wanted]);
        // Reads the type list only. This call does not touch the
        // contents, so macOS does not withhold its answer.
        let declared = pasteboard.availableTypeFromArray(&offered).is_some();
        let value = pasteboard
            .stringForType(wanted)
            .map(|text| text.to_string());
        Ok(RawClipboardRead { declared, value })
    }

    fn write_clipboard(
        &self,
        content_type: ClipboardContentType,
        text: &str,
    ) -> Result<(), PolarizeError> {
        let pasteboard = NSPasteboard::generalPasteboard();
        // `clearContents` is required before a write. It also declares
        // this process the pasteboard owner.
        pasteboard.clearContents();
        let value = NSString::from_str(text);
        if pasteboard.setString_forType(&value, pasteboard_type(content_type)) {
            return Ok(());
        }
        Err(PolarizeError::Platform(
            "NSPasteboard setString:forType: reported a failed write".to_string(),
        ))
    }
}

/// The `NSPasteboardType` one [`ClipboardContentType`] names.
///
/// The mapping is explicit, and it never converts one type into
/// another. See [`polarize_core::clipboard`].
fn pasteboard_type(content_type: ClipboardContentType) -> &'static NSPasteboardType {
    match content_type {
        ClipboardContentType::PlainText => unsafe { NSPasteboardTypeString },
    }
}
