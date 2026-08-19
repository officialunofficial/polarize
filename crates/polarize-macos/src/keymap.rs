//! Pure mappings from `polarize-core`'s platform-agnostic keyboard types to
//! the `CGEvent` values that describe them, plus the pure mouse-click event
//! sequence a multi-click `tap` needs.
//!
//! None of this touches a real window server: [`objc2_core_graphics::CGEventFlags`],
//! [`objc2_core_graphics::CGEventType`], and [`objc2_core_graphics::CGKeyCode`] are
//! plain data types (bitflags / newtype wrappers over integers), so building
//! and comparing them needs no display, no permission, and no live session —
//! that is what makes this module's logic genuinely unit-testable inside an
//! otherwise-untestable crate. See the "Testing harness" section of
//! `docs/INVARIANTS.md`.

use objc2_core_graphics::{CGEventFlags, CGEventType, CGKeyCode};
use polarize_core::schema::{Modifier, NamedKey};

/// # PINV-6: the modifier→flag mapping is a lossless, order-independent OR
///
/// - Always: [`modifiers_to_cgevent_flags`] ORs together exactly the
///   `CGEventFlags` mask bit for each [`Modifier`] present in the input,
///   regardless of input order or duplicates, and no others.
/// - Because: `CGEventSetFlags` takes a single bitmask; if the mapping
///   silently drops a requested modifier (e.g. because of a copy-paste bug
///   picking the wrong `CGEventFlags::Mask*` constant) a `keyboard` call
///   posts a keystroke with the wrong modifiers held, which is very easy to
///   miss in a manual smoke test since *a* key does get pressed.
/// - If violated: e.g. a caller asks for Command+Shift and the posted event
///   silently carries only Command, so a shortcut that depends on Shift
///   (rename vs. duplicate, etc.) fires the wrong action.
pub fn modifiers_to_cgevent_flags(modifiers: &[Modifier]) -> CGEventFlags {
    let mut flags = CGEventFlags(0);
    for modifier in modifiers {
        flags |= match modifier {
            Modifier::Command => CGEventFlags::MaskCommand,
            Modifier::Shift => CGEventFlags::MaskShift,
            Modifier::Option => CGEventFlags::MaskAlternate,
            Modifier::Control => CGEventFlags::MaskControl,
        };
    }
    flags
}

/// Maps a [`NamedKey`] to the macOS virtual keycode `CGEventCreateKeyboardEvent`
/// expects. These keycodes are physical-position codes fixed since classic
/// Mac OS (they identify a key by its position on an ANSI keyboard, not by
/// the character it currently produces), documented informally by Apple's
/// own `Carbon/HIToolbox` headers (`kVK_*` in `Events.h`) — not re-declared
/// here as `extern` constants because `objc2`'s umbrella does not bind that
/// header, so the well-known literal values are used directly.
pub fn named_key_to_keycode(key: NamedKey) -> CGKeyCode {
    match key {
        NamedKey::Return => 0x24,
        NamedKey::Tab => 0x30,
        NamedKey::Space => 0x31,
        NamedKey::Delete => 0x33,
        NamedKey::Escape => 0x35,
        NamedKey::ArrowLeft => 0x7B,
        NamedKey::ArrowRight => 0x7C,
        NamedKey::ArrowDown => 0x7D,
        NamedKey::ArrowUp => 0x7E,
    }
}

/// One step of a synthetic mouse click: which `CGEventType` to post, and
/// the `kCGMouseEventClickState` value (`CGEventField::MouseEventClickState`)
/// to set on it so the window server recognizes multi-clicks (a double
/// click needs two down/up pairs in quick succession, the second pair
/// carrying click state `2`, not two independent click state `1` clicks).
pub type ClickStep = (CGEventType, i64);

/// # PINV-7: an N-click tap posts N down/up pairs with an ascending click state
///
/// - Always: [`click_event_sequence`] returns exactly `2 * max(click_count, 1)`
///   steps: for each `state` in `1..=click_count.max(1)`, a `LeftMouseDown`
///   then a `LeftMouseUp`, both carrying click state `state`.
/// - Because: macOS's own double/triple-click recognition is driven by the
///   click-state field, not by event timing alone — posting two clicks both
///   at state `1` (instead of `1` then `2`) makes the window server treat
///   them as two independent single clicks, so a `tap` request with
///   `click_count: 2` would silently fail to trigger double-click behavior.
/// - If violated: a `tap` request that asks for a double-click instead
///   performs what the target application sees as two single clicks (e.g.
///   selecting a word fails, deselecting instead).
pub fn click_event_sequence(click_count: u8) -> Vec<ClickStep> {
    let clicks = click_count.max(1);
    let mut steps = Vec::with_capacity(2 * clicks as usize);
    for state in 1..=clicks {
        steps.push((CGEventType::LeftMouseDown, i64::from(state)));
        steps.push((CGEventType::LeftMouseUp, i64::from(state)));
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_modifiers_produces_empty_flags() {
        assert_eq!(modifiers_to_cgevent_flags(&[]), CGEventFlags(0));
    }

    #[test]
    fn single_modifier_maps_to_its_own_mask() {
        assert_eq!(
            modifiers_to_cgevent_flags(&[Modifier::Command]),
            CGEventFlags::MaskCommand
        );
        assert_eq!(
            modifiers_to_cgevent_flags(&[Modifier::Shift]),
            CGEventFlags::MaskShift
        );
        assert_eq!(
            modifiers_to_cgevent_flags(&[Modifier::Option]),
            CGEventFlags::MaskAlternate
        );
        assert_eq!(
            modifiers_to_cgevent_flags(&[Modifier::Control]),
            CGEventFlags::MaskControl
        );
    }

    #[test]
    fn multiple_modifiers_are_ored_together() {
        let flags = modifiers_to_cgevent_flags(&[Modifier::Command, Modifier::Shift]);
        assert_eq!(flags, CGEventFlags::MaskCommand | CGEventFlags::MaskShift);
    }

    #[test]
    fn modifier_order_does_not_affect_result() {
        let a = modifiers_to_cgevent_flags(&[Modifier::Shift, Modifier::Control]);
        let b = modifiers_to_cgevent_flags(&[Modifier::Control, Modifier::Shift]);
        assert_eq!(a, b);
    }

    #[test]
    fn duplicate_modifiers_do_not_double_set_bits() {
        let once = modifiers_to_cgevent_flags(&[Modifier::Command]);
        let twice = modifiers_to_cgevent_flags(&[Modifier::Command, Modifier::Command]);
        assert_eq!(once, twice);
    }

    #[test]
    fn named_key_mapping_has_no_collisions() {
        let keys = [
            NamedKey::Return,
            NamedKey::Tab,
            NamedKey::Escape,
            NamedKey::Delete,
            NamedKey::ArrowUp,
            NamedKey::ArrowDown,
            NamedKey::ArrowLeft,
            NamedKey::ArrowRight,
            NamedKey::Space,
        ];
        let mut codes: Vec<CGKeyCode> = keys.iter().copied().map(named_key_to_keycode).collect();
        let before = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(
            codes.len(),
            before,
            "two NamedKey variants mapped to the same keycode"
        );
    }

    #[test]
    fn known_keycodes_match_documented_virtual_key_values() {
        assert_eq!(named_key_to_keycode(NamedKey::Return), 0x24);
        assert_eq!(named_key_to_keycode(NamedKey::Escape), 0x35);
        assert_eq!(named_key_to_keycode(NamedKey::Space), 0x31);
    }

    #[test]
    fn click_sequence_for_single_click_is_one_down_up_pair_at_state_one() {
        let steps = click_event_sequence(1);
        assert_eq!(
            steps,
            vec![
                (CGEventType::LeftMouseDown, 1),
                (CGEventType::LeftMouseUp, 1),
            ]
        );
    }

    #[test]
    fn click_sequence_for_double_click_ascends_click_state() {
        let steps = click_event_sequence(2);
        assert_eq!(
            steps,
            vec![
                (CGEventType::LeftMouseDown, 1),
                (CGEventType::LeftMouseUp, 1),
                (CGEventType::LeftMouseDown, 2),
                (CGEventType::LeftMouseUp, 2),
            ]
        );
    }

    #[test]
    fn click_sequence_treats_zero_as_one_click() {
        assert_eq!(click_event_sequence(0), click_event_sequence(1));
    }
}
