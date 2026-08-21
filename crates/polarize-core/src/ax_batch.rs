//! The pure half of a batched AX attribute read.
//!
//! `polarize-macos` reads one element's attributes with a single
//! `AXUIElementCopyMultipleAttributeValues` call. That call answers with
//! an array that lines up, slot by slot, with the attribute names the
//! caller passed. This module turns that positional array into the
//! fields of an [`AxNode`], and it does so with no macOS types, so
//! `cargo test` covers it for real.
//!
//! ## Why this is a module of its own
//!
//! The batch call marks a slot it could not read. It does not drop the
//! slot. It writes a placeholder value there instead. Reading that
//! placeholder as a real value would write a wrong string, a wrong
//! frame, or a wrong `enabled` flag into the tree, which is what
//! PINV-16 forbids. The mapping from slots to fields is therefore the
//! one place a batching bug can live, so it lives here, where a test
//! can reach it. See PINV-41 in `docs/INVARIANTS.md`.

use crate::ax::{AxNode, NormalizedFrame};
use crate::coords::{PixelPoint, PixelSize};

/// The attribute names one batch call asks for, in the order the result
/// array answers them.
///
/// The index constants below name the same positions. The two must stay
/// in step, and a test asserts that they do.
///
/// The batch asks for all three label candidates every time. The
/// one-at-a-time path stopped at the first one that answered. The label
/// is the same either way, because the order below is the order of
/// preference. Only the cost differs: an element with a title and a
/// very large `AXValue` now copies that value as well.
pub const BATCHED_ATTRIBUTES: [&str; 11] = [
    "AXRole",
    "AXTitle",
    "AXDescription",
    "AXValue",
    "AXPosition",
    "AXSize",
    "AXEnabled",
    "AXSubrole",
    "AXRoleDescription",
    "AXIdentifier",
    "AXHelp",
];

const ROLE: usize = 0;
const TITLE: usize = 1;
const DESCRIPTION: usize = 2;
const VALUE: usize = 3;
const POSITION: usize = 4;
const SIZE: usize = 5;
const ENABLED: usize = 6;
const SUBROLE: usize = 7;
const ROLE_DESCRIPTION: usize = 8;
const IDENTIFIER: usize = 9;
const HELP: usize = 10;

/// One slot of a batch result, already converted to plain data.
///
/// `polarize-macos` builds these. It maps a Core Foundation string to
/// [`Self::Text`], a Core Foundation boolean to [`Self::Flag`], and the
/// two geometric `AXValue` boxes to [`Self::Point`] and [`Self::Size`].
/// Everything else — an unreadable slot, the batch call's own error
/// placeholder, a null, or a value of a type no attribute here uses —
/// becomes [`Self::Unread`].
#[derive(Debug, Clone, PartialEq, Default)]
pub enum AxAttributeSlot {
    /// The slot holds no value this reader can use.
    #[default]
    Unread,
    Text(String),
    Flag(bool),
    Point(PixelPoint),
    Size(PixelSize),
}

impl AxAttributeSlot {
    /// The slot's text, or `None` when it holds anything else.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }

    /// The slot's boolean, or `None` when it holds anything else.
    pub fn flag(&self) -> Option<bool> {
        match self {
            Self::Flag(value) => Some(*value),
            _ => None,
        }
    }

    /// The slot's point, or `None` when it holds anything else.
    pub fn point(&self) -> Option<PixelPoint> {
        match self {
            Self::Point(value) => Some(*value),
            _ => None,
        }
    }

    /// The slot's size, or `None` when it holds anything else.
    pub fn size(&self) -> Option<PixelSize> {
        match self {
            Self::Size(value) => Some(*value),
            _ => None,
        }
    }
}

/// The attributes of one element, after the slots are read.
///
/// Every field carries the same default the one-at-a-time read path
/// produces, so a batched walk and a fallback walk build the same
/// [`AxNode`].
#[derive(Debug, Clone, PartialEq)]
pub struct AxAttributes {
    pub role: String,
    pub label: Option<String>,
    pub position: PixelPoint,
    pub size: PixelSize,
    pub enabled: bool,
    pub subrole: Option<String>,
    pub role_description: Option<String>,
    pub identifier: Option<String>,
    pub help: Option<String>,
}

impl Default for AxAttributes {
    fn default() -> Self {
        Self {
            role: "AXUnknown".to_string(),
            label: None,
            position: PixelPoint { x: 0.0, y: 0.0 },
            size: PixelSize {
                width: 0.0,
                height: 0.0,
            },
            enabled: true,
            subrole: None,
            role_description: None,
            identifier: None,
            help: None,
        }
    }
}

impl AxAttributes {
    /// Reads a batch result whose length matches [`BATCHED_ATTRIBUTES`].
    ///
    /// A result of any other length is not positionally trustworthy, so
    /// this returns `None` and the caller reads the attributes one at a
    /// time instead. See PINV-41.
    pub fn from_batch(slots: &[AxAttributeSlot]) -> Option<Self> {
        (slots.len() == BATCHED_ATTRIBUTES.len()).then(|| Self::from_slots(slots))
    }

    /// Maps a positional slot array to attributes.
    ///
    /// A missing slot, a slot the batch call could not read, and a slot
    /// of an unexpected type all degrade to the field's default. Extra
    /// slots past the last known attribute are ignored.
    pub fn from_slots(slots: &[AxAttributeSlot]) -> Self {
        let defaults = Self::default();
        Self {
            // An unreadable role becomes "AXUnknown" (PINV-12). An
            // empty one stays empty, because the one-at-a-time path
            // does not filter it either.
            role: text_at(slots, ROLE)
                .map(ToString::to_string)
                .unwrap_or(defaults.role),
            label: [TITLE, DESCRIPTION, VALUE]
                .into_iter()
                .find_map(|index| non_empty_at(slots, index)),
            position: point_at(slots, POSITION).unwrap_or(defaults.position),
            size: size_at(slots, SIZE).unwrap_or(defaults.size),
            // A missing AXEnabled means "this element publishes no
            // enabled state", never "this element is disabled". See
            // PINV-16.
            enabled: flag_at(slots, ENABLED).unwrap_or(defaults.enabled),
            subrole: non_empty_at(slots, SUBROLE),
            role_description: non_empty_at(slots, ROLE_DESCRIPTION),
            identifier: non_empty_at(slots, IDENTIFIER),
            help: non_empty_at(slots, HELP),
        }
    }

    /// Joins these attributes with the parts a batch call cannot
    /// answer: the normalized frame, the settable-`AXFocused` check,
    /// the action list, and the children.
    ///
    /// `interactive` is not read at all. It is exactly "this element
    /// publishes at least one action".
    pub fn into_node(
        self,
        frame: NormalizedFrame,
        focusable: bool,
        actions: Vec<String>,
        children: Vec<AxNode>,
    ) -> AxNode {
        AxNode {
            role: self.role,
            label: self.label,
            frame,
            focusable,
            interactive: !actions.is_empty(),
            enabled: self.enabled,
            subrole: self.subrole,
            role_description: self.role_description,
            identifier: self.identifier,
            help: self.help,
            actions,
            children,
        }
    }
}

/// The text in one slot, or `None` when the slot is missing, unread, or
/// of another type.
fn text_at(slots: &[AxAttributeSlot], index: usize) -> Option<&str> {
    slots.get(index).and_then(AxAttributeSlot::text)
}

/// The text in one slot, with an empty string read as absent.
fn non_empty_at(slots: &[AxAttributeSlot], index: usize) -> Option<String> {
    text_at(slots, index)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn flag_at(slots: &[AxAttributeSlot], index: usize) -> Option<bool> {
    slots.get(index).and_then(AxAttributeSlot::flag)
}

fn point_at(slots: &[AxAttributeSlot], index: usize) -> Option<PixelPoint> {
    slots.get(index).and_then(AxAttributeSlot::point)
}

fn size_at(slots: &[AxAttributeSlot], index: usize) -> Option<PixelSize> {
    slots.get(index).and_then(AxAttributeSlot::size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> AxAttributeSlot {
        AxAttributeSlot::Text(value.to_string())
    }

    /// A batch result in which every slot reads.
    fn full_slots() -> Vec<AxAttributeSlot> {
        vec![
            text("AXButton"),
            text("Save"),
            text("save the document"),
            text("saved"),
            AxAttributeSlot::Point(PixelPoint { x: 10.0, y: 20.0 }),
            AxAttributeSlot::Size(PixelSize {
                width: 30.0,
                height: 40.0,
            }),
            AxAttributeSlot::Flag(false),
            text("AXCloseButton"),
            text("button"),
            text("save-button"),
            text("Save the document"),
        ]
    }

    fn full_attributes() -> AxAttributes {
        AxAttributes {
            role: "AXButton".to_string(),
            label: Some("Save".to_string()),
            position: PixelPoint { x: 10.0, y: 20.0 },
            size: PixelSize {
                width: 30.0,
                height: 40.0,
            },
            enabled: false,
            subrole: Some("AXCloseButton".to_string()),
            role_description: Some("button".to_string()),
            identifier: Some("save-button".to_string()),
            help: Some("Save the document".to_string()),
        }
    }

    #[test]
    fn the_attribute_names_match_their_index_constants() {
        assert_eq!(BATCHED_ATTRIBUTES[ROLE], "AXRole");
        assert_eq!(BATCHED_ATTRIBUTES[TITLE], "AXTitle");
        assert_eq!(BATCHED_ATTRIBUTES[DESCRIPTION], "AXDescription");
        assert_eq!(BATCHED_ATTRIBUTES[VALUE], "AXValue");
        assert_eq!(BATCHED_ATTRIBUTES[POSITION], "AXPosition");
        assert_eq!(BATCHED_ATTRIBUTES[SIZE], "AXSize");
        assert_eq!(BATCHED_ATTRIBUTES[ENABLED], "AXEnabled");
        assert_eq!(BATCHED_ATTRIBUTES[SUBROLE], "AXSubrole");
        assert_eq!(BATCHED_ATTRIBUTES[ROLE_DESCRIPTION], "AXRoleDescription");
        assert_eq!(BATCHED_ATTRIBUTES[IDENTIFIER], "AXIdentifier");
        assert_eq!(BATCHED_ATTRIBUTES[HELP], "AXHelp");
    }

    #[test]
    fn the_attribute_names_hold_no_duplicate() {
        let mut seen = BATCHED_ATTRIBUTES.to_vec();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "a duplicate name wastes a slot");
    }

    #[test]
    fn a_full_result_set_fills_every_field() {
        assert_eq!(AxAttributes::from_slots(&full_slots()), full_attributes());
    }

    #[test]
    fn an_empty_result_array_yields_every_default() {
        assert_eq!(AxAttributes::from_slots(&[]), AxAttributes::default());
    }

    #[test]
    fn every_slot_unread_matches_the_empty_result_array() {
        let unread = vec![AxAttributeSlot::Unread; BATCHED_ATTRIBUTES.len()];
        assert_eq!(
            AxAttributes::from_slots(&unread),
            AxAttributes::from_slots(&[])
        );
    }

    #[test]
    fn a_short_result_array_defaults_the_missing_tail() {
        let short = &full_slots()[..2];
        let read = AxAttributes::from_slots(short);
        assert_eq!(read.role, "AXButton");
        assert_eq!(read.label.as_deref(), Some("Save"));
        assert_eq!(read.position, PixelPoint { x: 0.0, y: 0.0 });
        assert!(read.enabled, "a missing AXEnabled slot means enabled");
        assert_eq!(read.subrole, None);
        assert_eq!(read.help, None);
    }

    #[test]
    fn an_over_long_result_array_ignores_the_extra_slots() {
        let mut long = full_slots();
        long.push(text("an attribute nobody asked for"));
        long.push(AxAttributeSlot::Flag(true));
        assert_eq!(AxAttributes::from_slots(&long), full_attributes());
    }

    #[test]
    fn an_unread_slot_in_every_position_degrades_only_that_field() {
        for index in 0..BATCHED_ATTRIBUTES.len() {
            let mut slots = full_slots();
            slots[index] = AxAttributeSlot::Unread;
            let read = AxAttributes::from_slots(&slots);
            let full = full_attributes();
            let name = BATCHED_ATTRIBUTES[index];

            match index {
                ROLE => assert_eq!(read.role, "AXUnknown", "{name}"),
                TITLE => assert_eq!(
                    read.label.as_deref(),
                    Some("save the document"),
                    "an unread {name} falls through to AXDescription"
                ),
                DESCRIPTION | VALUE => {
                    assert_eq!(read.label.as_deref(), Some("Save"), "{name}")
                }
                POSITION => assert_eq!(read.position, PixelPoint { x: 0.0, y: 0.0 }, "{name}"),
                SIZE => assert_eq!(
                    read.size,
                    PixelSize {
                        width: 0.0,
                        height: 0.0
                    },
                    "{name}"
                ),
                ENABLED => assert!(read.enabled, "an unread {name} must not read as disabled"),
                SUBROLE => assert_eq!(read.subrole, None, "{name}"),
                ROLE_DESCRIPTION => assert_eq!(read.role_description, None, "{name}"),
                IDENTIFIER => assert_eq!(read.identifier, None, "{name}"),
                HELP => assert_eq!(read.help, None, "{name}"),
                _ => unreachable!("{name} has no rule"),
            }

            // Every other field still reads its real value.
            if !matches!(index, ROLE) {
                assert_eq!(read.role, full.role, "{name} disturbed AXRole");
            }
            if !matches!(index, POSITION) {
                assert_eq!(read.position, full.position, "{name} disturbed AXPosition");
            }
            if !matches!(index, SIZE) {
                assert_eq!(read.size, full.size, "{name} disturbed AXSize");
            }
            if !matches!(index, ENABLED) {
                assert_eq!(read.enabled, full.enabled, "{name} disturbed AXEnabled");
            }
            if !matches!(index, SUBROLE) {
                assert_eq!(read.subrole, full.subrole, "{name} disturbed AXSubrole");
            }
            if !matches!(index, IDENTIFIER) {
                assert_eq!(
                    read.identifier, full.identifier,
                    "{name} disturbed AXIdentifier"
                );
            }
            if !matches!(index, HELP) {
                assert_eq!(read.help, full.help, "{name} disturbed AXHelp");
            }
        }
    }

    #[test]
    fn a_wrong_type_in_every_position_degrades_like_an_unread_slot() {
        // A value of a type the attribute never uses. A string
        // attribute gets a flag, and every other attribute gets a
        // string.
        for index in 0..BATCHED_ATTRIBUTES.len() {
            let mut wrong = full_slots();
            wrong[index] = match index {
                POSITION | SIZE | ENABLED => text("not what this attribute holds"),
                _ => AxAttributeSlot::Flag(true),
            };
            let mut unread = full_slots();
            unread[index] = AxAttributeSlot::Unread;
            assert_eq!(
                AxAttributes::from_slots(&wrong),
                AxAttributes::from_slots(&unread),
                "a wrong type in {} must read like an unread slot",
                BATCHED_ATTRIBUTES[index]
            );
        }
    }

    #[test]
    fn a_point_in_the_size_slot_does_not_become_a_size() {
        let mut slots = full_slots();
        slots[SIZE] = AxAttributeSlot::Point(PixelPoint { x: 9.0, y: 9.0 });
        let read = AxAttributes::from_slots(&slots);
        assert_eq!(
            read.size,
            PixelSize {
                width: 0.0,
                height: 0.0
            }
        );
        assert_eq!(read.position, PixelPoint { x: 10.0, y: 20.0 });
    }

    #[test]
    fn the_label_prefers_the_title_then_the_description_then_the_value() {
        let mut slots = full_slots();
        assert_eq!(
            AxAttributes::from_slots(&slots).label.as_deref(),
            Some("Save")
        );

        slots[TITLE] = text("");
        assert_eq!(
            AxAttributes::from_slots(&slots).label.as_deref(),
            Some("save the document")
        );

        slots[DESCRIPTION] = AxAttributeSlot::Unread;
        assert_eq!(
            AxAttributes::from_slots(&slots).label.as_deref(),
            Some("saved")
        );

        slots[VALUE] = text("");
        assert_eq!(AxAttributes::from_slots(&slots).label, None);
    }

    #[test]
    fn an_empty_string_attribute_reads_as_absent() {
        let mut slots = full_slots();
        for index in [SUBROLE, ROLE_DESCRIPTION, IDENTIFIER, HELP] {
            slots[index] = text("");
        }
        let read = AxAttributes::from_slots(&slots);
        assert_eq!(read.subrole, None);
        assert_eq!(read.role_description, None);
        assert_eq!(read.identifier, None);
        assert_eq!(read.help, None);
    }

    #[test]
    fn an_empty_role_string_is_kept_exactly_as_read() {
        // The one-at-a-time path does not filter an empty AXRole, so
        // the batched path must not either.
        let mut slots = full_slots();
        slots[ROLE] = text("");
        assert_eq!(AxAttributes::from_slots(&slots).role, "");
    }

    #[test]
    fn a_false_enabled_slot_reads_as_disabled() {
        let mut slots = full_slots();
        slots[ENABLED] = AxAttributeSlot::Flag(false);
        assert!(!AxAttributes::from_slots(&slots).enabled);

        slots[ENABLED] = AxAttributeSlot::Flag(true);
        assert!(AxAttributes::from_slots(&slots).enabled);
    }

    #[test]
    fn from_batch_accepts_an_aligned_result_array() {
        assert_eq!(
            AxAttributes::from_batch(&full_slots()),
            Some(full_attributes())
        );
    }

    #[test]
    fn from_batch_refuses_a_result_array_of_the_wrong_length() {
        assert_eq!(AxAttributes::from_batch(&[]), None);
        assert_eq!(AxAttributes::from_batch(&full_slots()[..3]), None);
        let mut long = full_slots();
        long.push(AxAttributeSlot::Unread);
        assert_eq!(AxAttributes::from_batch(&long), None);
    }

    #[test]
    fn into_node_copies_every_attribute_into_the_node() {
        let frame = NormalizedFrame {
            x: 0.1,
            y: 0.2,
            width: 0.3,
            height: 0.4,
        };
        let child = AxAttributes::default().into_node(
            NormalizedFrame::default(),
            false,
            Vec::new(),
            Vec::new(),
        );
        let node = full_attributes().into_node(
            frame,
            true,
            vec!["AXPress".to_string()],
            vec![child.clone()],
        );

        assert_eq!(node.role, "AXButton");
        assert_eq!(node.label.as_deref(), Some("Save"));
        assert_eq!(node.frame, frame);
        assert!(node.focusable);
        assert!(node.interactive);
        assert!(!node.enabled);
        assert_eq!(node.subrole.as_deref(), Some("AXCloseButton"));
        assert_eq!(node.role_description.as_deref(), Some("button"));
        assert_eq!(node.identifier.as_deref(), Some("save-button"));
        assert_eq!(node.help.as_deref(), Some("Save the document"));
        assert_eq!(node.actions, vec!["AXPress".to_string()]);
        assert_eq!(node.children, vec![child]);
    }

    #[test]
    fn into_node_reads_interactive_straight_from_the_action_list() {
        let quiet =
            full_attributes().into_node(NormalizedFrame::default(), false, Vec::new(), Vec::new());
        assert!(!quiet.interactive, "no action means not interactive");

        let loud = full_attributes().into_node(
            NormalizedFrame::default(),
            false,
            vec!["AXShowMenu".to_string()],
            Vec::new(),
        );
        assert!(loud.interactive, "one action means interactive");
    }
}
