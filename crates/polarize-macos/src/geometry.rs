//! Pure geometry glue between real AX/window pixel rectangles and
//! `polarize-core`'s `[0.0, 1.0]`-normalized [`NormalizedFrame`].
//!
//! [`crate::coords::pixel_to_fraction`](polarize_core::coords::pixel_to_fraction)
//! (already unit-tested in `polarize-core`, PINV-1) deliberately *rejects*
//! out-of-range input rather than clamping it. That is the right contract
//! for a `tap` request, where an out-of-range fraction usually means the
//! caller mixed up pixels and fractions. It is the *wrong* contract for
//! `describe`: a real multi-monitor AX tree legitimately contains elements
//! whose position/size fall partly or fully outside the primary screen
//! (an element on a secondary display, or a window dragged half off-screen),
//! and `describe` should still describe them instead of dropping the whole
//! tree. [`safe_normalize_frame`] is the pure clamping wrapper that makes
//! that call — extracted from [`crate::accessibility`] specifically so this
//! one behavioral decision has real test coverage despite living inside a
//! crate whose native calls do not.

use polarize_core::ax::NormalizedFrame;
use polarize_core::coords::{Fraction, PixelPoint, PixelSize};

/// # PINV-8: an AX frame is clamped into `[0.0, 1.0]`, never dropped
///
/// - Always: [`safe_normalize_frame`] converts a pixel position/size pair
///   to a [`NormalizedFrame`] whose `x`, `y`, `width`, and `height` are all
///   clamped into `0.0..=1.0`, even when the input pixel rectangle falls
///   partly or fully outside `screen_size`.
/// - Because: unlike a `tap` fraction (PINV-1, which must error loudly on
///   bad input to catch a caller's coordinate-space mistake), a `describe`
///   response is built from real AX geometry the caller never supplied —
///   an off-screen element is a legitimate, common case (multi-monitor
///   setups, partially dragged-off windows), not a caller error, so it must
///   degrade to a best-effort frame instead of vanishing from the tree or
///   propagating an error that would blank out an entire subtree.
/// - If violated: either `describe` panics/errors on the first off-screen
///   element it meets (multi-monitor setups become unusable), or it passes
///   an out-of-range frame through to callers who trusted the `[0,1]`
///   contract every other normalized frame in the response honors.
pub fn safe_normalize_frame(
    position: PixelPoint,
    size: PixelSize,
    screen_size: PixelSize,
) -> NormalizedFrame {
    let (sw, sh) = (screen_size.width.max(1.0), screen_size.height.max(1.0));
    let x = (position.x / sw).clamp(0.0, 1.0);
    let y = (position.y / sh).clamp(0.0, 1.0);
    let width = (size.width / sw).clamp(0.0, 1.0);
    let height = (size.height / sh).clamp(0.0, 1.0);
    NormalizedFrame {
        x,
        y,
        width,
        height,
    }
}

/// Normalizes a fully in-range pixel rectangle the same way
/// [`polarize_core::coords::fraction_to_pixel`]'s round trip would, for
/// callers that already know the rectangle is on-screen and want the exact
/// (non-clamped) fraction. Falls back to [`safe_normalize_frame`] if either
/// corner is out of range, rather than erroring.
pub fn normalize_frame_best_effort(
    position: PixelPoint,
    size: PixelSize,
    screen_size: PixelSize,
) -> NormalizedFrame {
    let top_left = polarize_core::coords::pixel_to_fraction(position, screen_size);
    let bottom_right = polarize_core::coords::pixel_to_fraction(
        PixelPoint {
            x: position.x + size.width,
            y: position.y + size.height,
        },
        screen_size,
    );
    match (top_left, bottom_right) {
        (Ok(Fraction { x, y }), Ok(Fraction { x: x2, y: y2 })) => NormalizedFrame {
            x,
            y,
            width: (x2 - x).max(0.0),
            height: (y2 - y).max(0.0),
        },
        _ => safe_normalize_frame(position, size, screen_size),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: PixelSize = PixelSize {
        width: 2000.0,
        height: 1000.0,
    };

    #[test]
    fn on_screen_rect_normalizes_exactly() {
        let frame = safe_normalize_frame(
            PixelPoint { x: 500.0, y: 250.0 },
            PixelSize {
                width: 200.0,
                height: 100.0,
            },
            SCREEN,
        );
        assert_eq!(
            frame,
            NormalizedFrame {
                x: 0.25,
                y: 0.25,
                width: 0.1,
                height: 0.1,
            }
        );
    }

    #[test]
    fn negative_position_clamps_to_zero() {
        let frame = safe_normalize_frame(
            PixelPoint {
                x: -500.0,
                y: -100.0,
            },
            PixelSize {
                width: 100.0,
                height: 100.0,
            },
            SCREEN,
        );
        assert_eq!(frame.x, 0.0);
        assert_eq!(frame.y, 0.0);
    }

    #[test]
    fn position_beyond_screen_clamps_to_one() {
        let frame = safe_normalize_frame(
            PixelPoint {
                x: 5000.0,
                y: 3000.0,
            },
            PixelSize {
                width: 100.0,
                height: 100.0,
            },
            SCREEN,
        );
        assert_eq!(frame.x, 1.0);
        assert_eq!(frame.y, 1.0);
    }

    #[test]
    fn oversized_dimension_clamps_to_one() {
        let frame = safe_normalize_frame(
            PixelPoint { x: 0.0, y: 0.0 },
            PixelSize {
                width: 10_000.0,
                height: 10_000.0,
            },
            SCREEN,
        );
        assert_eq!(frame.width, 1.0);
        assert_eq!(frame.height, 1.0);
    }

    #[test]
    fn zero_or_negative_screen_size_does_not_divide_by_zero() {
        let frame = safe_normalize_frame(
            PixelPoint { x: 10.0, y: 10.0 },
            PixelSize {
                width: 10.0,
                height: 10.0,
            },
            PixelSize {
                width: 0.0,
                height: 0.0,
            },
        );
        assert!(frame.x.is_finite());
        assert!(frame.y.is_finite());
        assert!(frame.width.is_finite());
        assert!(frame.height.is_finite());
    }

    #[test]
    fn best_effort_matches_exact_conversion_when_on_screen() {
        let frame = normalize_frame_best_effort(
            PixelPoint { x: 500.0, y: 250.0 },
            PixelSize {
                width: 200.0,
                height: 100.0,
            },
            SCREEN,
        );
        // Width/height go through a subtraction of two independently
        // divided fractions, so compare with a tolerance rather than
        // exact equality (this is a float-precision artifact of the
        // computation, not a behavioral question).
        assert_eq!(frame.x, 0.25);
        assert_eq!(frame.y, 0.25);
        assert!((frame.width - 0.1).abs() < 1e-9);
        assert!((frame.height - 0.1).abs() < 1e-9);
    }

    #[test]
    fn best_effort_falls_back_to_clamped_when_off_screen() {
        let frame = normalize_frame_best_effort(
            PixelPoint { x: -100.0, y: 0.0 },
            PixelSize {
                width: 100.0,
                height: 100.0,
            },
            SCREEN,
        );
        assert_eq!(frame.x, 0.0);
    }
}
