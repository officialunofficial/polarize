//! Resolves whether `polarize`'s own code signature can see the
//! desktop host app's shared App Group container. See PINV-52's
//! follow-up note in `docs/INVARIANTS.md`: the
//! `com.apple.security.application-groups` entitlement in
//! `apps/polarize/polarize.entitlements` was added ahead of any code
//! that reads it, and `polarize`'s own `codesign`-only signing story
//! has no equivalent to the desktop host app's Xcode-managed signing
//! validation. This module makes that gap observable at startup
//! instead of leaving it silently unverified.

use objc2_foundation::{NSFileManager, NSString};

/// The App Group both `polarize` and the desktop host app declare in
/// their own entitlements files. Not re-derived from either
/// entitlements file at build time — a mismatch here, against either
/// file, is exactly the kind of drift [`container_summary`] exists to
/// surface.
const SHARED_APP_GROUP: &str = "N8R89M8725.fun.uno.polarize";

/// Asks `NSFileManager` for the shared App Group's container
/// directory. `None` means this process's own code signature cannot
/// see it — either it is not signed under a certificate this App
/// Group's provisioning covers, or the group is not registered the
/// way this code expects. `Some` means the container is real and
/// this process can read and write inside it.
pub fn container_url() -> Option<String> {
    let group = NSString::from_str(SHARED_APP_GROUP);
    let manager = NSFileManager::defaultManager();
    let url = manager.containerURLForSecurityApplicationGroupIdentifier(&group)?;
    url.path().map(|path| path.to_string())
}

/// One line summarizing whether this process can see the shared App
/// Group container, for `apps/polarize`'s startup log — mirrors
/// [`crate::self_responsibility::responsibility_summary`]'s role for
/// PINV-52's other still-open question.
pub fn container_summary() -> String {
    match container_url() {
        Some(path) => format!("resolved: {path}"),
        None => "unresolved (no App Group container visible to this process)".to_string(),
    }
}
