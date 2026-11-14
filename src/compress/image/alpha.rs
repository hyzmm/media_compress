//! Alpha handling for platform decoders that return premultiplied RGBA.
//!
//! `webp::Encoder::from_rgba` expects straight (non-premultiplied) alpha.
//! CoreGraphics and Android's `Bitmap`/`AImageDecoder` return premultiplied
//! pixels, so their RGB values must be divided by alpha before encoding.

/// Convert tightly-packed premultiplied RGBA pixels to straight alpha in place.
///
/// Fully transparent pixels become `(0, 0, 0, 0)`. Channels that round above
/// 255 after division are clamped; this can only happen for slightly invalid
/// premultiplied input.
pub(crate) fn unpremultiply_rgba(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = pixel[3] as u32;
        if alpha == 0 {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
        } else if alpha < 255 {
            for channel in &mut pixel[..3] {
                let value = (*channel as u32 * 255 + alpha / 2) / alpha;
                *channel = value.min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::unpremultiply_rgba;

    #[test]
    fn opaque_pixels_are_unchanged() {
        let mut pixels = [12, 34, 56, 255, 200, 100, 50, 255];
        unpremultiply_rgba(&mut pixels);
        assert_eq!(pixels, [12, 34, 56, 255, 200, 100, 50, 255]);
    }

    #[test]
    fn half_alpha_is_divided_out() {
        let mut pixels = [128, 64, 32, 128];
        unpremultiply_rgba(&mut pixels);
        assert_eq!(pixels, [255, 128, 64, 128]);
    }

    #[test]
    fn transparent_pixels_become_black() {
        let mut pixels = [9, 8, 7, 0];
        unpremultiply_rgba(&mut pixels);
        assert_eq!(pixels, [0, 0, 0, 0]);
    }

    #[test]
    fn image_without_alpha_is_not_darkened() {
        // Fully opaque white must stay white, regardless of RGBA layout.
        let mut pixels = vec![255u8; 4 * 32];
        unpremultiply_rgba(&mut pixels);
        assert!(pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel == &[255, 255, 255, 255]));
    }
}
