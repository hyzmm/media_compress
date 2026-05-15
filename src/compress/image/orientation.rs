/// Apply EXIF orientation to tightly-packed RGBA pixels.
/// Returns transformed `(pixels, width, height)`.
pub fn apply_exif_orientation_rgba(
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    orientation: u32,
) -> (Vec<u8>, u32, u32) {
    if orientation == 1 || width == 0 || height == 0 {
        return (pixels, width, height);
    }

    let (dst_w, dst_h) = if matches!(orientation, 5 | 6 | 7 | 8) {
        (height, width)
    } else {
        (width, height)
    };

    let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize) * 4];

    for dy in 0..dst_h {
        for dx in 0..dst_w {
            let (sx, sy) = match orientation {
                2 => (width - 1 - dx, dy),
                3 => (width - 1 - dx, height - 1 - dy),
                4 => (dx, height - 1 - dy),
                5 => (dy, dx),
                6 => (dy, height - 1 - dx),
                7 => (width - 1 - dy, height - 1 - dx),
                8 => (width - 1 - dy, dx),
                _ => (dx, dy),
            };

            let src_idx = ((sy * width + sx) * 4) as usize;
            let dst_idx = ((dy * dst_w + dx) * 4) as usize;
            out[dst_idx..dst_idx + 4].copy_from_slice(&pixels[src_idx..src_idx + 4]);
        }
    }

    (out, dst_w, dst_h)
}

#[cfg(test)]
mod tests {
    use super::apply_exif_orientation_rgba;

    #[test]
    fn orientation_6_swaps_dimensions() {
        // 2x1 RGBA: [A][B]
        let src = vec![
            1, 0, 0, 255, // A
            2, 0, 0, 255, // B
        ];
        let (_out, w, h) = apply_exif_orientation_rgba(src, 2, 1, 6);
        assert_eq!((w, h), (1, 2));
    }
}
