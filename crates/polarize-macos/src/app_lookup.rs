//! Pure app-matching logic: resolving a [`polarize_core::schema::AppIdentifier`]
//! against a list of candidate running apps.
//!
//! This is the one genuinely pure piece of "which app did the caller mean"
//! resolution — everything upstream of it (enumerating real running apps
//! via `NSWorkspace`) is real native-API behavior and lives in
//! [`crate::window`], untested here. This module is fully unit-tested.

use polarize_core::schema::AppIdentifier;

/// One running app as seen by a real enumeration API (`NSWorkspace`,
/// `SCShareableContent`, …), reduced to just the fields matching needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppCandidate<'a> {
    pub bundle_id: Option<&'a str>,
    pub name: Option<&'a str>,
}

/// # PINV-5: bundle id is tried before app name, but a mismatch falls through
///
/// - Always: [`find_matching_app_index`] tries an exact `bundle_id` match
///   first when `identifier.bundle_id` is set; only if that yields no
///   match does it fall back to a case-insensitive `app_name` match.
/// - Because: [`AppIdentifier`]'s own doc comment documents this
///   "bundle id tried first" contract; a caller that supplies a stale or
///   slightly-wrong bundle id (but a correct name) should still resolve
///   the app rather than fail outright.
/// - If violated: a caller who supplies both fields gets `AppNotFound`
///   whenever the bundle id is even slightly wrong, even though the name
///   alone would have resolved unambiguously.
pub fn find_matching_app_index(
    identifier: &AppIdentifier,
    candidates: &[AppCandidate<'_>],
) -> Option<usize> {
    if let Some(bundle_id) = identifier.bundle_id.as_deref()
        && let Some(idx) = candidates
            .iter()
            .position(|c| c.bundle_id == Some(bundle_id))
    {
        return Some(idx);
    }
    if let Some(name) = identifier.app_name.as_deref() {
        return candidates
            .iter()
            .position(|c| c.name.is_some_and(|n| n.eq_ignore_ascii_case(name)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> Vec<AppCandidate<'static>> {
        vec![
            AppCandidate {
                bundle_id: Some("com.apple.TextEdit"),
                name: Some("TextEdit"),
            },
            AppCandidate {
                bundle_id: Some("com.apple.Safari"),
                name: Some("Safari"),
            },
            AppCandidate {
                bundle_id: None,
                name: Some("Electron"),
            },
        ]
    }

    #[test]
    fn matches_by_exact_bundle_id() {
        let id = AppIdentifier {
            bundle_id: Some("com.apple.Safari".to_string()),
            app_name: None,
        };
        assert_eq!(find_matching_app_index(&id, &candidates()), Some(1));
    }

    #[test]
    fn matches_by_case_insensitive_name_when_bundle_id_absent() {
        let id = AppIdentifier {
            bundle_id: None,
            app_name: Some("textedit".to_string()),
        };
        assert_eq!(find_matching_app_index(&id, &candidates()), Some(0));
    }

    #[test]
    fn falls_back_to_name_when_bundle_id_does_not_match_any_candidate() {
        let id = AppIdentifier {
            bundle_id: Some("com.example.NoSuchApp".to_string()),
            app_name: Some("Electron".to_string()),
        };
        assert_eq!(find_matching_app_index(&id, &candidates()), Some(2));
    }

    #[test]
    fn bundle_id_match_wins_even_if_name_differs() {
        let id = AppIdentifier {
            bundle_id: Some("com.apple.TextEdit".to_string()),
            app_name: Some("Not TextEdit".to_string()),
        };
        assert_eq!(find_matching_app_index(&id, &candidates()), Some(0));
    }

    #[test]
    fn returns_none_when_identifier_is_empty() {
        let id = AppIdentifier::default();
        assert_eq!(find_matching_app_index(&id, &candidates()), None);
    }

    #[test]
    fn returns_none_when_nothing_matches() {
        let id = AppIdentifier {
            bundle_id: Some("com.example.Nope".to_string()),
            app_name: Some("Nope".to_string()),
        };
        assert_eq!(find_matching_app_index(&id, &candidates()), None);
    }

    #[test]
    fn returns_none_for_empty_candidate_list() {
        let id = AppIdentifier {
            bundle_id: Some("com.apple.Safari".to_string()),
            app_name: None,
        };
        assert_eq!(find_matching_app_index(&id, &[]), None);
    }
}
