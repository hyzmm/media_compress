//! YUV420 helpers for the Android MediaCodec path: color-format conversion
//! (I420 / NV12 / NV21) and CPU scaling. Pure Rust, no JNI, so it can be
//! unit-tested on the host.

/// `COLOR_FormatYUV420Planar` (I420) — MediaCodecInfo.CodecCapabilities.
pub const COLOR_FORMAT_YUV420_PLANAR: i32 = 19;
/// `COLOR_FormatYUV420SemiPlanar` (NV12).
pub const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;
/// `COLOR_FormatYUV420PackedSemiPlanar` (NV21).
pub const COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR: i32 = 39;
/// `COLOR_FormatYUV420Flexible` — the actual layout is device dependent
/// (usually NV12); treated as NV12 here.
pub const COLOR_FORMAT_YUV420_FLEXIBLE: i32 = 0x7F42_0888;

/// Convert a MediaCodec decoder output buffer into tightly-packed planar I420.
///
/// `stride` / `slice_height` come from the codec output format (KEY_STRIDE /
/// KEY_SLICE_HEIGHT) and may exceed `width` / `height` due to alignment.
/// `width`/`height` are assumed to be even.
pub fn to_i420(
    src: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    slice_height: usize,
    color_format: i32,
) -> Vec<u8> {
    if width == 0 || height == 0 || stride == 0 || slice_height == 0 {
        return Vec::new();
    }
    match color_format {
        COLOR_FORMAT_YUV420_PLANAR => planar_to_i420(src, width, height, stride, slice_height),
        // FLEXIBLE is ambiguous; NV12 is the most common actual layout.
        COLOR_FORMAT_YUV420_FLEXIBLE | COLOR_FORMAT_YUV420_SEMI_PLANAR => {
            semi_planar_to_i420(src, width, height, stride, slice_height, false)
        }
        COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR => {
            semi_planar_to_i420(src, width, height, stride, slice_height, true)
        }
        _ => planar_to_i420(src, width, height, stride, slice_height),
    }
}

fn planar_to_i420(src: &[u8], w: usize, h: usize, stride: usize, slice_height: usize) -> Vec<u8> {
    let cw = w / 2;
    let ch = h / 2;
    let mut out = Vec::with_capacity(w * h + 2 * cw * ch);

    for row in 0..h {
        let start = row * stride;
        if start + w > src.len() {
            return out;
        }
        out.extend_from_slice(&src[start..start + w]);
    }

    let c_stride = stride / 2;
    let c_slice_height = slice_height / 2;
    let u_plane = stride * slice_height;
    let v_plane = u_plane + c_stride * c_slice_height;

    for plane in [u_plane, v_plane] {
        for row in 0..ch {
            let start = plane + row * c_stride;
            if start + cw > src.len() {
                return out;
            }
            out.extend_from_slice(&src[start..start + cw]);
        }
    }
    out
}

fn semi_planar_to_i420(
    src: &[u8],
    w: usize,
    h: usize,
    stride: usize,
    slice_height: usize,
    swap_uv: bool,
) -> Vec<u8> {
    let cw = w / 2;
    let ch = h / 2;
    let mut out = Vec::with_capacity(w * h + 2 * cw * ch);

    for row in 0..h {
        let start = row * stride;
        if start + w > src.len() {
            return out;
        }
        out.extend_from_slice(&src[start..start + w]);
    }

    // Interleaved UV plane; each row covers `w` luma pixels (2*cw bytes).
    let uv_plane = stride * slice_height;
    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        let start = uv_plane + row * stride;
        if start + 2 * cw > src.len() {
            return out;
        }
        for i in 0..cw {
            let a = src[start + i * 2];
            let b = src[start + i * 2 + 1];
            if swap_uv {
                u.push(b);
                v.push(a);
            } else {
                u.push(a);
                v.push(b);
            }
        }
    }
    out.extend_from_slice(&u);
    out.extend_from_slice(&v);
    out
}

/// Scale tightly-packed planar I420 to `dst_w` x `dst_h` (both even).
pub fn scale_i420(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
) -> Vec<u8> {
    let src_y = src_w * src_h;
    let src_u = (src_w / 2) * (src_h / 2);
    if src.len() < src_y + 2 * src_u {
        return Vec::new();
    }

    let y = scale_plane(&src[..src_y], src_w, src_h, dst_w, dst_h);
    let u = scale_plane(
        &src[src_y..src_y + src_u],
        src_w / 2,
        src_h / 2,
        dst_w / 2,
        dst_h / 2,
    );
    let v = scale_plane(
        &src[src_y + src_u..],
        src_w / 2,
        src_h / 2,
        dst_w / 2,
        dst_h / 2,
    );

    let mut out = Vec::with_capacity(dst_w * dst_h + 2 * (dst_w / 2) * (dst_h / 2));
    out.extend_from_slice(&y);
    out.extend_from_slice(&u);
    out.extend_from_slice(&v);
    out
}

/// Bilinear scale of a single grayscale plane.
fn scale_plane(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; dw * dh];
    for dy in 0..dh {
        let sy = ((dy as f64 + 0.5) * sh as f64 / dh as f64 - 0.5).clamp(0.0, (sh - 1) as f64);
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let wy = sy - y0 as f64;
        for dx in 0..dw {
            let sx = ((dx as f64 + 0.5) * sw as f64 / dw as f64 - 0.5).clamp(0.0, (sw - 1) as f64);
            let x0 = sx.floor() as usize;
            let x1 = (x0 + 1).min(sw - 1);
            let wx = sx - x0 as f64;

            let p00 = src[y0 * sw + x0] as f64;
            let p01 = src[y0 * sw + x1] as f64;
            let p10 = src[y1 * sw + x0] as f64;
            let p11 = src[y1 * sw + x1] as f64;
            let top = p00 + (p01 - p00) * wx;
            let bottom = p10 + (p11 - p10) * wx;
            out[dy * dw + dx] = (top + (bottom - top) * wy).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i420_planes(w: usize, h: usize) -> (usize, usize) {
        (w * h, (w / 2) * (h / 2))
    }

    #[test]
    fn nv12_to_i420_converts() {
        // 2x2 NV12: Y = [0,1,2,3], UV interleaved = [4,5] (U=4, V=5).
        let src = [0u8, 1, 2, 3, 4, 5];
        let out = to_i420(&src, 2, 2, 2, 2, COLOR_FORMAT_YUV420_SEMI_PLANAR);
        assert_eq!(out, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn nv21_to_i420_swaps_uv() {
        // 2x2 NV21: Y = [0,1,2,3], VU interleaved = [4,5] (V=4, U=5).
        let src = [0u8, 1, 2, 3, 4, 5];
        let out = to_i420(&src, 2, 2, 2, 2, COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR);
        // I420 planar: Y, then U=5, then V=4.
        assert_eq!(out, vec![0, 1, 2, 3, 5, 4]);
    }

    #[test]
    fn planar_roundtrip_preserves_layout() {
        let (ys, cs) = i420_planes(4, 4);
        let mut src = vec![0u8; ys + 2 * cs];
        for (i, b) in src.iter_mut().enumerate() {
            *b = i as u8;
        }
        let out = to_i420(&src, 4, 4, 4, 4, COLOR_FORMAT_YUV420_PLANAR);
        assert_eq!(out, src);
    }

    #[test]
    fn scale_i420_halves_dimensions() {
        // 4x4 source, scale to 2x2.
        let (ys, cs) = i420_planes(4, 4);
        let src = vec![128u8; ys + 2 * cs];
        let out = scale_i420(&src, 4, 4, 2, 2);
        assert_eq!(out.len(), 2 * 2 + 2 * 1 * 1);
    }

    #[test]
    fn scale_i420_same_size_is_copy() {
        let (ys, cs) = i420_planes(4, 4);
        let mut src = vec![0u8; ys + 2 * cs];
        for (i, b) in src.iter_mut().enumerate() {
            *b = i as u8;
        }
        let out = scale_i420(&src, 4, 4, 4, 4);
        assert_eq!(out, src);
    }
}
