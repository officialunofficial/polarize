//! The macOS side of the two notification-banner tools.
//!
//! There is very little of it, and that is the point. macOS draws every
//! banner from one ordinary process, `com.apple.notificationcenterui`,
//! and that process publishes an ordinary accessibility tree. So
//! [`MacNotificationCenter`] only joins three parts that already exist:
//! [`crate::accessibility::MacAccessibilityInspector`] reads the tree,
//! [`crate::action::MacActionPerformer`] presses the close control, and
//! `polarize_core::notifications` decides everything in between.
//!
//! Both of those parts already run the Accessibility permission check
//! and the login-session preflight (PINV-10, PINV-23), so this module
//! adds neither. Adding a second preflight here would only report the
//! same failure twice.
//!
//! ## What this module does add
//!
//! One thing: a clear error when the notification centre process is not
//! running. `resolve_running_app` would otherwise report
//! `app not found: com.apple.notificationcenterui`, which reads like a
//! caller's typo rather than a fact about the Mac.
//!
//! ## What is not verified
//!
//! Nothing here has run against a real notification. See PINV-35's
//! enforcement entry, and the crate-level "what is and is not verified"
//! note.

use polarize_core::error::PolarizeError;
use polarize_core::notifications::{
    DescribeNotificationsRequest, DescribeNotificationsResponse, DismissNotificationRequest,
    DismissNotificationResponse, NOTIFICATION_CENTER_BUNDLE_ID, ThreadSleeper,
    perform_describe_notifications, perform_dismiss_notification,
};

use crate::accessibility::MacAccessibilityInspector;
use crate::action::MacActionPerformer;

/// The two notification tools, over the real macOS accessibility APIs.
#[derive(Debug, Default)]
pub struct MacNotificationCenter {
    inspector: MacAccessibilityInspector,
    performer: MacActionPerformer,
    sleeper: ThreadSleeper,
}

impl MacNotificationCenter {
    /// Reads every notification banner on screen.
    pub fn describe(
        &self,
        request: &DescribeNotificationsRequest,
    ) -> Result<DescribeNotificationsResponse, PolarizeError> {
        perform_describe_notifications(&self.inspector, request).map_err(explain_missing_process)
    }

    /// Closes one banner, then reports whether it went away.
    pub fn dismiss(
        &self,
        request: &DismissNotificationRequest,
    ) -> Result<DismissNotificationResponse, PolarizeError> {
        perform_dismiss_notification(&self.inspector, &self.performer, &self.sleeper, request)
            .map_err(explain_missing_process)
    }
}

/// Turns "app not found" into a fact about this Mac.
///
/// The notification centre runs on every healthy macOS session. It
/// missing means the session is unusual — a `launchctl` unload, or a
/// session that has no user interface at all.
fn explain_missing_process(error: PolarizeError) -> PolarizeError {
    match &error {
        PolarizeError::AppNotFound(name) if name == NOTIFICATION_CENTER_BUNDLE_ID => {
            PolarizeError::Platform(format!(
                "the notification centre process ({NOTIFICATION_CENTER_BUNDLE_ID}) is not \
                 running, so macOS can show no banner at all"
            ))
        }
        _ => error,
    }
}
