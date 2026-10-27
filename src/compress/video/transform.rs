//! Pure math for mapping a video track's `preferredTransform` into an
//! `AVMutableVideoComposition` render box.
//!
//! Kept free of FFI so the orientation cases can be unit-tested on any host.

/// CoreGraphics affine transform layout (row-vector convention):
/// `x' = a*x + c*y + tx`, `y' = b*x + d*y + ty`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CGAffineTransform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

impl CGAffineTransform {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.tx,
            self.b * x + self.d * y + self.ty,
        )
    }
}

/// Bounding box `(min_x, min_y, max_x, max_y)` of the transformed source rect.
fn bounds(width: f64, height: f64, transform: CGAffineTransform) -> (f64, f64, f64, f64) {
    let corners = [
        transform.apply(0.0, 0.0),
        transform.apply(width, 0.0),
        transform.apply(0.0, height),
        transform.apply(width, height),
    ];

    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for (x, y) in corners {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }

    (min_x, min_y, max_x, max_y)
}

/// Display dimensions after applying `transform` to the natural frame.
///
/// Uses the transformed bounding box, so 90/180/270 rotations and mirrored
/// (front camera) transforms all produce the visible size.
pub fn display_size(width: f64, height: f64, transform: CGAffineTransform) -> (f64, f64) {
    if width <= 0.0 || height <= 0.0 {
        return (width.max(0.0), height.max(0.0));
    }

    let (min_x, min_y, max_x, max_y) = bounds(width, height, transform);

    ((max_x - min_x).max(1.0), (max_y - min_y).max(1.0))
}

/// [`display_size`] rounded for feeding `compute_video_target_dimensions`.
pub fn display_size_u32(width: u32, height: u32, transform: CGAffineTransform) -> (u32, u32) {
    let (display_w, display_h) = display_size(width as f64, height as f64, transform);

    (
        display_w.round().max(1.0) as u32,
        display_h.round().max(1.0) as u32,
    )
}

/// Transform to apply to the composition layer: rotate/mirror the natural frame
/// upright, move its bounding box to the origin, then scale it to the target
/// render size.
///
/// The composition does not apply `preferredTransform` on its own, and an
/// un-normalized transform can have a negative origin that gets clipped by the
/// render box, hence the explicit translation.
pub fn layer_transform(
    width: f64,
    height: f64,
    preferred: CGAffineTransform,
    target_width: u32,
    target_height: u32,
) -> CGAffineTransform {
    if width <= 0.0 || height <= 0.0 {
        return CGAffineTransform::IDENTITY;
    }

    let (min_x, min_y, max_x, max_y) = bounds(width, height, preferred);
    let display_w = (max_x - min_x).max(1.0);
    let display_h = (max_y - min_y).max(1.0);
    let scale_x = target_width as f64 / display_w;
    let scale_y = target_height as f64 / display_h;

    CGAffineTransform {
        a: preferred.a * scale_x,
        b: preferred.b * scale_y,
        c: preferred.c * scale_x,
        d: preferred.d * scale_y,
        tx: (preferred.tx - min_x) * scale_x,
        ty: (preferred.ty - min_y) * scale_y,
    }
}

#[cfg(test)]
mod tests {
    use super::{display_size_u32, layer_transform, CGAffineTransform};

    const NATURAL_W: u32 = 1920;
    const NATURAL_H: u32 = 1080;

    fn rotation_90() -> CGAffineTransform {
        CGAffineTransform {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            tx: NATURAL_H as f64,
            ty: 0.0,
        }
    }

    fn rotation_180() -> CGAffineTransform {
        CGAffineTransform {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            tx: NATURAL_W as f64,
            ty: NATURAL_H as f64,
        }
    }

    fn rotation_270() -> CGAffineTransform {
        CGAffineTransform {
            a: 0.0,
            b: -1.0,
            c: 1.0,
            d: 0.0,
            tx: 0.0,
            ty: NATURAL_W as f64,
        }
    }

    fn mirrored() -> CGAffineTransform {
        CGAffineTransform {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: NATURAL_W as f64,
            ty: 0.0,
        }
    }

    fn assert_corners_fill_target(preferred: CGAffineTransform, target_w: u32, target_h: u32) {
        let transform = layer_transform(
            NATURAL_W as f64,
            NATURAL_H as f64,
            preferred,
            target_w,
            target_h,
        );
        let corners = [
            transform.apply(0.0, 0.0),
            transform.apply(NATURAL_W as f64, 0.0),
            transform.apply(0.0, NATURAL_H as f64),
            transform.apply(NATURAL_W as f64, NATURAL_H as f64),
        ];

        for (x, y) in corners {
            assert!(
                x >= -0.5 && x <= target_w as f64 + 0.5,
                "x {x} outside 0..={target_w}"
            );
            assert!(
                y >= -0.5 && y <= target_h as f64 + 0.5,
                "y {y} outside 0..={target_h}"
            );
        }

        let min_x = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
        let max_x = corners
            .iter()
            .map(|c| c.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_y = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
        let max_y = corners
            .iter()
            .map(|c| c.1)
            .fold(f64::NEG_INFINITY, f64::max);

        assert!((min_x - 0.0).abs() <= 0.5, "left edge not flush: {min_x}");
        assert!(
            (max_x - target_w as f64).abs() <= 0.5,
            "right edge not flush: {max_x}"
        );
        assert!((min_y - 0.0).abs() <= 0.5, "top edge not flush: {min_y}");
        assert!(
            (max_y - target_h as f64).abs() <= 0.5,
            "bottom edge not flush: {max_y}"
        );
    }

    #[test]
    fn identity_keeps_natural_size() {
        assert_eq!(
            display_size_u32(NATURAL_W, NATURAL_H, CGAffineTransform::IDENTITY),
            (NATURAL_W, NATURAL_H)
        );
    }

    #[test]
    fn portrait_rotation_swaps_dimensions() {
        assert_eq!(
            display_size_u32(NATURAL_W, NATURAL_H, rotation_90()),
            (NATURAL_H, NATURAL_W)
        );
        assert_eq!(
            display_size_u32(NATURAL_W, NATURAL_H, rotation_270()),
            (NATURAL_H, NATURAL_W)
        );
    }

    #[test]
    fn rotation_180_keeps_dimensions() {
        assert_eq!(
            display_size_u32(NATURAL_W, NATURAL_H, rotation_180()),
            (NATURAL_W, NATURAL_H)
        );
    }

    #[test]
    fn mirror_keeps_dimensions() {
        assert_eq!(
            display_size_u32(NATURAL_W, NATURAL_H, mirrored()),
            (NATURAL_W, NATURAL_H)
        );
    }

    #[test]
    fn layer_transform_fills_portrait_target() {
        assert_corners_fill_target(rotation_90(), 404, 720);
    }

    #[test]
    fn layer_transform_fills_landscape_target() {
        assert_corners_fill_target(CGAffineTransform::IDENTITY, 1280, 720);
        assert_corners_fill_target(rotation_180(), 1280, 720);
        assert_corners_fill_target(mirrored(), 1280, 720);
    }

    #[test]
    fn layer_transform_fills_rotated_target() {
        assert_corners_fill_target(rotation_270(), 404, 720);
    }
}
