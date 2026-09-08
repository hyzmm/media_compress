//! BGRA32 scaling for the Windows Media Foundation path: bilinear resize of
//! tightly-packed 32bpp BGRA (the layout of `MFVideoFormat_RGB32`). Pure Rust,
//! no FFI, so it can be unit-tested on the host.

/// Bilinear-scale a tightly-packed BGRA32 buffer to `dst_w` x `dst_h`.
///
/// `src` must contain exactly `src_w * src_h * 4` bytes. Each of the four
/// channels is interpolated independently with the same weights.
pub fn scale_bgra(src: &[u8], src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> Vec<u8> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    if src.len() < src_w * src_h * 4 {
        return Vec::new();
    }

    let mut out = vec![0u8; dst_w * dst_h * 4];
    for dy in 0..dst_h {
        let sy = ((dy as f64 + 0.5) * src_h as f64 / dst_h as f64 - 0.5).clamp(0.0, (src_h - 1) as f64);
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(src_h - 1);
        let wy = sy - y0 as f64;
        for dx in 0..dst_w {
            let sx = ((dx as f64 + 0.5) * src_w as f64 / dst_w as f64 - 0.5).clamp(0.0, (src_w - 1) as f64);
            let x0 = sx.floor() as usize;
            let x1 = (x0 + 1).min(src_w - 1);
            let wx = sx - x0 as f64;

            let src_base = |x: usize, y: usize| (y * src_w + x) * 4;
            for c in 0..4 {
                let p00 = src[src_base(x0, y0) + c] as f64;
                let p01 = src[src_base(x1, y0) + c] as f64;
                let p10 = src[src_base(x0, y1) + c] as f64;
                let p11 = src[src_base(x1, y1) + c] as f64;
                let top = p00 + (p01 - p00) * wx;
                let bottom = p10 + (p11 - p10) * wx;
                out[(dy * dst_w + dx) * 4 + c] =
                    (top + (bottom - top) * wy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_size_is_identity() {
        let src: Vec<u8> = (0..64u8).collect(); // 4x4 BGRA
        let out = scale_bgra(&src, 4, 4, 4, 4);
        assert_eq!(out, src);
    }

    #[test]
    fn halves_dimensions() {
        let src = vec![0u8; 4 * 4 * 4];
        let out = scale_bgra(&src, 4, 4, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 4);
    }

    #[test]
    fn uniform_color_preserved() {
        // Solid mid-grey BGRA (B=100, G=110, R=120, A=255).
        let px = [100u8, 110, 120, 255];
        let mut src = Vec::new();
        for _ in 0..(8 * 8) {
            src.extend_from_slice(&px);
        }
        let out = scale_bgra(&src, 8, 8, 3, 5);
        assert_eq!(out.len(), 3 * 5 * 4);
        for chunk in out.chunks_exact(4) {
            assert_eq!(chunk, px);
        }
    }

    #[test]
    fn invalid_input_returns_empty() {
        assert!(scale_bgra(&[], 4, 4, 2, 2).is_empty());
        assert!(scale_bgra(&[0u8; 8], 4, 4, 2, 2).is_empty()); // too short
        assert!(scale_bgra(&[0u8; 64], 0, 4, 2, 2).is_empty());
    }
}
