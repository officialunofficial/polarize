//! Pure geometry glue between real AX/window pixel rectangles and
//! `polarize-core`'s `[0.0, 1.0]`-normalized [`NormalizedFrame`].
//!
//! [`crate::coords::pixel_to_fraction`](polarize_core::coords::pixel_to_fraction)
//! rejects out-of-range input instead of clamping it (already tested in
//! `polarize-core`, PINV-1). That is the right contract for a `tap`
//! request: an out-of-range fraction there usually means the caller mixed
//! up pixels and fractions.
//!
//! It is the wrong contract for `describe`. A real multi-monitor AX tree
//! legitimately holds elements whose position or size falls partly or
//! fully outside the primary screen — an element on a secondary display,
//! or a window dragged half off-screen. `describe` should still describe
//! them, not drop the whole tree. [`safe_normalize_frame`] is the pure
//! clamping wrapper that makes that call. It is extracted from
//! [`crate::accessibility`] so this one behavioral decision gets real
//! test coverage, despite living inside a crate whose native calls do
//! not.

use polarize_core::ax::NormalizedFrame;
use polarize_core::coords::{PixelPoint, PixelSize};

/// # PINV-8: an AX frame is clamped into `[0.0, 1.0]`, never dropped
///
/// - Always: [`safe_normalize_frame`] converts a pixel position/size pair
///   to a [`NormalizedFrame`] whose `x`, `y`, `width`, and `height` are all
///   clamped into `0.0..=1.0`, even when the input pixel rectangle falls
///   partly or fully outside `screen_size`.
/// - Because: a `tap` fraction must error loudly on bad input, to catch a
///   caller's coordinate-space mistake (PINV-1). A `describe` response is
///   different: it is built from real AX geometry the caller never
///   supplied. An off-screen element is a legitimate, common case —
///   multi-monitor setups, or a window dragged half off-screen — not a
///   caller error. It must degrade to a best-effort frame, not vanish
///   from the tree or blank out an entire subtree with a propagated
///   error.
/// - If violated: `describe` either panics or errors on the first
///   off-screen element it meets, making multi-monitor setups unusable.
///   Or it passes an out-of-range frame through to callers who trusted
///   the `[0,1]` contract every other normalized frame in the response
///   honors.
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
}
