//! Pixel helpers for the Windows Media Foundation path: bilinear resizing of
//! tightly-packed 32bpp BGRA (the layout of `MFVideoFormat_RGB32`) and NV12 to
//! BGRA conversion. Pure Rust, no FFI, so it can be unit-tested on the host.

/// YCbCr matrix used for NV12 -> RGB conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
}

/// Convert an NV12 frame (full-size Y plane, then interleaved UV at half
/// height) to tightly-packed BGRA32.
///
/// `stride` is the byte stride of both planes and may exceed the width.
/// `full_range` selects 0-255 input levels instead of the 16-235 video range.
/// Returns an empty buffer when the input is too small.
pub fn nv12_to_bgra(
    src: &[u8],
    w: usize,
    h: usize,
    stride: usize,
    matrix: YuvMatrix,
    full_range: bool,
) -> Vec<u8> {
    if w == 0 || h == 0 || stride < w {
        return Vec::new();
    }
    let y_len = stride * h;
    let uv_rows = h.div_ceil(2);
    if src.len() < y_len + stride * uv_rows {
        return Vec::new();
    }

    // Limited-range coefficients in 8.8 fixed point: 1.164 -> 298, plus the
    // per-matrix chroma weights.
    let (rv, gu, gv, bu) = match (matrix, full_range) {
        (YuvMatrix::Bt601, false) => (409i32, 100i32, 208i32, 516i32),
        (YuvMatrix::Bt601, true) => (359, 88, 183, 454),
        (YuvMatrix::Bt709, false) => (459, 55, 136, 541),
        (YuvMatrix::Bt709, true) => (403, 48, 120, 475),
    };
    let luma_scale = if full_range { 256 } else { 298 };
    let luma_offset = if full_range { 0 } else { 16 };

    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let y_row = &src[y * stride..y * stride + w];
        let uv_start = y_len + (y / 2) * stride;
        let uv_row = &src[uv_start..uv_start + w];
        let dst = &mut out[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let c = (y_row[x] as i32 - luma_offset).max(0);
            let d = uv_row[(x / 2) * 2] as i32 - 128;
            let e = uv_row[(x / 2) * 2 + 1] as i32 - 128;
            let luma = luma_scale * c;
            let r = ((luma + rv * e + 128) >> 8).clamp(0, 255) as u8;
            let g = ((luma - gu * d - gv * e + 128) >> 8).clamp(0, 255) as u8;
            let b = ((luma + bu * d + 128) >> 8).clamp(0, 255) as u8;
            dst[x * 4] = b;
            dst[x * 4 + 1] = g;
            dst[x * 4 + 2] = r;
            dst[x * 4 + 3] = 255;
        }
    }
    out
}

/// Copy a stride-padded BGRA image into a tightly packed buffer.
///
/// Returns an empty buffer when the rows do not fit in `src`.
pub fn pack_bgra_rows(src: &[u8], w: usize, h: usize, stride: usize) -> Vec<u8> {
    if w == 0 || h == 0 || stride < w * 4 || src.len() < stride * h {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        out.extend_from_slice(&src[y * stride..y * stride + w * 4]);
    }
    out
}

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
        let sy =
            ((dy as f64 + 0.5) * src_h as f64 / dst_h as f64 - 0.5).clamp(0.0, (src_h - 1) as f64);
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(src_h - 1);
        let wy = sy - y0 as f64;
        for dx in 0..dst_w {
            let sx = ((dx as f64 + 0.5) * src_w as f64 / dst_w as f64 - 0.5)
                .clamp(0.0, (src_w - 1) as f64);
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

    #[test]
    fn nv12_limited_range_extremes_map_to_black_and_white() {
        // 2x2 NV12: Y = 16 (black level), neutral chroma.
        let src = [16u8, 16, 16, 16, 128, 128];
        let out = nv12_to_bgra(&src, 2, 2, 2, YuvMatrix::Bt709, false);
        assert_eq!(out, vec![0, 0, 0, 255].repeat(4));

        let src = [235u8, 235, 235, 235, 128, 128];
        let out = nv12_to_bgra(&src, 2, 2, 2, YuvMatrix::Bt709, false);
        assert_eq!(out, vec![255, 255, 255, 255].repeat(4));
    }

    #[test]
    fn nv12_bt709_red_is_recovered() {
        // Limited-range BT.709 red: Y=63, Cb=102, Cr=240.
        let src = [63u8, 63, 63, 63, 102, 240];
        let out = nv12_to_bgra(&src, 2, 2, 2, YuvMatrix::Bt709, false);
        assert_eq!(&out[..4], &[0, 1, 255, 255]);
    }

    #[test]
    fn nv12_honours_row_stride() {
        // 2x2 NV12 with stride 4: rows are padded with garbage that must be
        // skipped. Y rows at 0 and 4, UV row at 8.
        let src = [
            100u8, 100, 0xAA, 0xAA, // Y row 0
            100, 100, 0xAA, 0xAA, // Y row 1
            128, 128, 0xAA, 0xAA, // UV row
        ];
        let out = nv12_to_bgra(&src, 2, 2, 4, YuvMatrix::Bt601, false);
        assert_eq!(out.len(), 16);
        // Y=100 with neutral chroma -> 1.164 * (100 - 16) = 97.8 -> 98.
        for px in out.chunks_exact(4) {
            assert_eq!(px, [98, 98, 98, 255]);
        }
    }

    #[test]
    fn nv12_too_small_returns_empty() {
        assert!(nv12_to_bgra(&[0u8; 5], 2, 2, 2, YuvMatrix::Bt709, false).is_empty());
        assert!(nv12_to_bgra(&[0u8; 6], 2, 2, 1, YuvMatrix::Bt709, false).is_empty());
    }

    #[test]
    fn pack_bgra_rows_drops_padding() {
        let src = [
            1u8, 2, 3, 4, 5, 6, 7, 8, // row 0, 2 pixels
            0xAA, 0xAA, // padding
            9, 10, 11, 12, 13, 14, 15, 16, // row 1
            0xAA, 0xAA,
        ];
        let out = pack_bgra_rows(&src, 2, 2, 10);
        assert_eq!(
            out,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert!(pack_bgra_rows(&src, 2, 2, 6).is_empty());
    }
}
