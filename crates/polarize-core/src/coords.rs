//! Pure coordinate math: normalized `[0.0, 1.0]` fraction points converted
//! to and from pixel points on a target of a given size.
//!
//! This matches `argent`'s own `gesture-tap` contract: `x`/`y` are
//! fractions of the target's width/height in `0.0..=1.0`, not raw pixels.
//! Using the same mental model across both tools means an agent that
//! already knows argent's coordinate contract needs no new one here.

use crate::error::CoordAxis;

/// A point expressed as normalized fractions of a target's width/height.
///
/// `x` and `y` are expected to lie in `0.0..=1.0`; see [`fraction_to_pixel`]
/// for what happens when they do not.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Fraction {
    pub x: f64,
    pub y: f64,
}

/// A point expressed in pixels, in the target's own coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PixelPoint {
    pub x: f64,
    pub y: f64,
}

/// The pixel dimensions of a screenshot/tap target (a screen or a window).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PixelSize {
    pub width: f64,
    pub height: f64,
}

/// Errors produced by fraction/pixel conversion.
///
/// # PINV-1: coordinate conversion never silently clamps
///
/// - Always: [`fraction_to_pixel`] and [`pixel_to_fraction`] reject
///   out-of-range input with an error instead of clamping it into range.
/// - Because: a `tap` request built from a stale or miscomputed fraction
///   (e.g. `x: 1.4` from a caller that mixed up pixels and fractions)
///   must not silently land at the target's edge — that produces a click
///   on the wrong element that looks like a successful tap.
/// - If violated: a caller passing garbage coordinates gets a "successful"
///   tap at the wrong point instead of a clear error to fix the caller.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum CoordError {
    /// A fraction component was outside `0.0..=1.0`.
    #[error("fraction {axis} = {value} is out of range 0.0..=1.0")]
    FractionOutOfRange { axis: CoordAxis, value: f64 },
    /// A pixel component was outside `0.0..=dimension`.
    #[error("pixel {axis} = {value} is out of range 0.0..={dimension}")]
    PixelOutOfRange {
        axis: CoordAxis,
        value: f64,
        dimension: f64,
    },
    /// The target size had a non-positive width or height.
    #[error("target size must be positive, got width={width}, height={height}")]
    NonPositiveSize { width: f64, height: f64 },
}

fn check_size(size: PixelSize) -> Result<(), CoordError> {
    if size.width <= 0.0 || size.height <= 0.0 {
        return Err(CoordError::NonPositiveSize {
            width: size.width,
            height: size.height,
        });
    }
    Ok(())
}

/// Converts a normalized `[0.0, 1.0]` fraction point to a pixel point
/// within a target of the given size.
///
/// Fraction components outside `0.0..=1.0`, or a non-positive target size,
/// are rejected rather than clamped — see [`PINV-1`](CoordError).
pub fn fraction_to_pixel(fraction: Fraction, size: PixelSize) -> Result<PixelPoint, CoordError> {
    check_size(size)?;
    if !(0.0..=1.0).contains(&fraction.x) {
        return Err(CoordError::FractionOutOfRange {
            axis: CoordAxis::X,
            value: fraction.x,
        });
    }
    if !(0.0..=1.0).contains(&fraction.y) {
        return Err(CoordError::FractionOutOfRange {
            axis: CoordAxis::Y,
            value: fraction.y,
        });
    }
    Ok(PixelPoint {
        x: fraction.x * size.width,
        y: fraction.y * size.height,
    })
}

/// Converts a pixel point within a target of the given size back to a
/// normalized `[0.0, 1.0]` fraction point.
///
/// Pixel components outside `0.0..=size.{width,height}`, or a
/// non-positive target size, are rejected rather than clamped — see
/// [`PINV-1`](CoordError).
pub fn pixel_to_fraction(point: PixelPoint, size: PixelSize) -> Result<Fraction, CoordError> {
    check_size(size)?;
    if !(0.0..=size.width).contains(&point.x) {
        return Err(CoordError::PixelOutOfRange {
            axis: CoordAxis::X,
            value: point.x,
            dimension: size.width,
        });
    }
    if !(0.0..=size.height).contains(&point.y) {
        return Err(CoordError::PixelOutOfRange {
            axis: CoordAxis::Y,
            value: point.y,
            dimension: size.height,
        });
    }
    Ok(Fraction {
        x: point.x / size.width,
        y: point.y / size.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: PixelSize = PixelSize {
        width: 1920.0,
        height: 1080.0,
    };

    #[test]
    fn fraction_to_pixel_top_left_corner() {
        let p = fraction_to_pixel(Fraction { x: 0.0, y: 0.0 }, SIZE).unwrap();
        assert_eq!(p, PixelPoint { x: 0.0, y: 0.0 });
    }

    #[test]
    fn fraction_to_pixel_bottom_right_corner() {
        let p = fraction_to_pixel(Fraction { x: 1.0, y: 1.0 }, SIZE).unwrap();
        assert_eq!(
            p,
            PixelPoint {
                x: 1920.0,
                y: 1080.0
            }
        );
    }

    #[test]
    fn fraction_to_pixel_center() {
        let p = fraction_to_pixel(Fraction { x: 0.5, y: 0.5 }, SIZE).unwrap();
        assert_eq!(p, PixelPoint { x: 960.0, y: 540.0 });
    }

    #[test]
    fn fraction_to_pixel_rejects_negative_x() {
        let err = fraction_to_pixel(Fraction { x: -0.1, y: 0.5 }, SIZE).unwrap_err();
        assert_eq!(
            err,
            CoordError::FractionOutOfRange {
                axis: CoordAxis::X,
                value: -0.1
            }
        );
    }

    #[test]
    fn fraction_to_pixel_rejects_x_over_one() {
        let err = fraction_to_pixel(Fraction { x: 1.1, y: 0.5 }, SIZE).unwrap_err();
        assert_eq!(
            err,
            CoordError::FractionOutOfRange {
                axis: CoordAxis::X,
                value: 1.1
            }
        );
    }

    #[test]
    fn fraction_to_pixel_rejects_negative_y() {
        let err = fraction_to_pixel(Fraction { x: 0.5, y: -0.001 }, SIZE).unwrap_err();
        assert_eq!(
            err,
            CoordError::FractionOutOfRange {
                axis: CoordAxis::Y,
                value: -0.001
            }
        );
    }

    #[test]
    fn fraction_to_pixel_rejects_y_over_one() {
        let err = fraction_to_pixel(Fraction { x: 0.5, y: 2.0 }, SIZE).unwrap_err();
        assert_eq!(
            err,
            CoordError::FractionOutOfRange {
                axis: CoordAxis::Y,
                value: 2.0
            }
        );
    }

    #[test]
    fn fraction_to_pixel_rejects_non_positive_size() {
        let err = fraction_to_pixel(
            Fraction { x: 0.5, y: 0.5 },
            PixelSize {
                width: 0.0,
                height: 1080.0,
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            CoordError::NonPositiveSize {
                width: 0.0,
                height: 1080.0
            }
        );
    }

    #[test]
    fn pixel_to_fraction_top_left_corner() {
        let f = pixel_to_fraction(PixelPoint { x: 0.0, y: 0.0 }, SIZE).unwrap();
        assert_eq!(f, Fraction { x: 0.0, y: 0.0 });
    }

    #[test]
    fn pixel_to_fraction_bottom_right_corner() {
        let f = pixel_to_fraction(
            PixelPoint {
                x: 1920.0,
                y: 1080.0,
            },
            SIZE,
        )
        .unwrap();
        assert_eq!(f, Fraction { x: 1.0, y: 1.0 });
    }

    #[test]
    fn pixel_to_fraction_center() {
        let f = pixel_to_fraction(PixelPoint { x: 960.0, y: 540.0 }, SIZE).unwrap();
        assert_eq!(f, Fraction { x: 0.5, y: 0.5 });
    }

    #[test]
    fn pixel_to_fraction_rejects_out_of_range_x() {
        let err = pixel_to_fraction(PixelPoint { x: 2000.0, y: 0.0 }, SIZE).unwrap_err();
        assert_eq!(
            err,
            CoordError::PixelOutOfRange {
                axis: CoordAxis::X,
                value: 2000.0,
                dimension: 1920.0
            }
        );
    }

    #[test]
    fn pixel_to_fraction_rejects_negative_pixel() {
        let err = pixel_to_fraction(PixelPoint { x: -1.0, y: 0.0 }, SIZE).unwrap_err();
        assert_eq!(
            err,
            CoordError::PixelOutOfRange {
                axis: CoordAxis::X,
                value: -1.0,
                dimension: 1920.0
            }
        );
    }

    #[test]
    fn round_trip_is_stable_for_arbitrary_interior_point() {
        let original = Fraction { x: 0.3125, y: 0.7 };
        let pixel = fraction_to_pixel(original, SIZE).unwrap();
        let back = pixel_to_fraction(pixel, SIZE).unwrap();
        assert!((back.x - original.x).abs() < 1e-9);
        assert!((back.y - original.y).abs() < 1e-9);
    }
}
