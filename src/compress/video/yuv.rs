//! YUV420 helpers for the Android MediaCodec path: color-format conversion
//! (I420 / NV12 / NV21), cropping, rotation and CPU scaling. Pure Rust, no JNI,
//! so it can be unit-tested on the host.

/// `COLOR_FormatYUV420Planar` (I420) — MediaCodecInfo.CodecCapabilities.
pub const COLOR_FORMAT_YUV420_PLANAR: i32 = 19;
/// `COLOR_FormatYUV420SemiPlanar` (NV12).
pub const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;
/// `COLOR_FormatYUV420PackedSemiPlanar` (NV21).
pub const COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR: i32 = 39;
/// `COLOR_FormatYUV420Flexible` — the actual layout is device dependent.
///
/// The format alone does not tell us where the chroma planes live, so
/// [`to_i420`] deliberately refuses it. When a decoder reports flexible output
/// it also publishes an `image-data` descriptor; parse that with
/// [`parse_image_data`] and convert with [`to_i420_with_planes`] instead.
pub const COLOR_FORMAT_YUV420_FLEXIBLE: i32 = 0x7F42_0888;

/// Sizes of the `image-data` descriptor written by MediaCodec.
///
/// `MediaImage2` is 104 bytes; the older `MediaImage` (API 21-27) is 80 bytes.
/// Both are plain arrays of little-endian integers on Android.
const MEDIA_IMAGE_V1_SIZE: usize = 80;
const MEDIA_IMAGE_V2_SIZE: usize = 104;
const MEDIA_IMAGE_TYPE_YUV: u32 = 1;
const MEDIA_IMAGE_PLANE_SIZE: usize = 20;

/// Byte layout of one plane in a decoded YUV frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plane {
    /// Offset of the first pixel, relative to the start of the frame buffer.
    pub offset: usize,
    /// Distance in bytes between two horizontally adjacent pixels.
    pub col_inc: usize,
    /// Distance in bytes between two vertically adjacent pixels.
    pub row_inc: usize,
}

/// Exact Y/U/V plane addressing for an 8-bit YUV420 frame.
///
/// This mirrors the `image-data` buffer that MediaCodec exposes on the output
/// format. It is authoritative even for `COLOR_FormatYUV420Flexible` and
/// vendor-specific color formats whose plane layout cannot be derived from the
/// color-format integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneLayout {
    pub y: Plane,
    pub u: Plane,
    pub v: Plane,
}

/// Visible region inside a decoded frame, in luma pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRect {
    pub left: usize,
    pub top: usize,
    pub width: usize,
    pub height: usize,
}

impl CropRect {
    /// Rounds the visible region down to even dimensions, dropping the last
    /// row/column when the source is odd sized.
    ///
    /// Only MediaCodec crop rects can be odd sized; on the host this module is
    /// compiled just for its unit tests.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn even(self) -> Self {
        Self {
            left: self.left,
            top: self.top,
            width: self.width & !1,
            height: self.height & !1,
        }
    }
}

/// Convert a MediaCodec decoder output buffer into tightly-packed planar I420.
///
/// `stride` / `slice_height` come from the codec output format (KEY_STRIDE /
/// KEY_SLICE_HEIGHT) and may exceed the coded size due to alignment.
/// `crop` is the visible region inside that buffer; its width/height must be
/// even (use [`CropRect::even`]).
///
/// `COLOR_FORMAT_YUV420_FLEXIBLE` and vendor-specific formats return an empty
/// buffer because their plane layout is not implied by the color-format value.
/// Use [`to_i420_with_planes`] with the decoder's `image-data` descriptor.
pub fn to_i420(
    src: &[u8],
    stride: usize,
    slice_height: usize,
    color_format: i32,
    crop: CropRect,
) -> Vec<u8> {
    if crop.width == 0 || crop.height == 0 || stride == 0 || slice_height == 0 {
        return Vec::new();
    }
    match color_format {
        COLOR_FORMAT_YUV420_PLANAR => planar_to_i420(src, stride, slice_height, crop),
        COLOR_FORMAT_YUV420_SEMI_PLANAR => {
            semi_planar_to_i420(src, stride, slice_height, crop, false)
        }
        COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR => {
            semi_planar_to_i420(src, stride, slice_height, crop, true)
        }
        // FLEXIBLE / vendor formats (10-bit P010, ...) cannot be split into
        // planes safely without an explicit layout.
        _ => Vec::new(),
    }
}

/// Parse the `image-data` buffer that MediaCodec puts on decoder output
/// formats.
///
/// The buffer is a `MediaImage2` (API 28+) or legacy `MediaImage` (API 21-27)
/// descriptor. Only 8-bit, 3-plane YUV420 descriptors are accepted; everything
/// else (10-bit, RGBA, malformed/short buffers) returns `None`.
pub fn parse_image_data(data: &[u8]) -> Option<PlaneLayout> {
    let (header_len, allocated_bit_depth) = if data.len() >= MEDIA_IMAGE_V2_SIZE {
        (24, read_u32(data, 20)?)
    } else if data.len() >= MEDIA_IMAGE_V1_SIZE {
        (20, 8)
    } else {
        return None;
    };

    if read_u32(data, 0)? != MEDIA_IMAGE_TYPE_YUV
        || read_u32(data, 4)? != 3
        || read_u32(data, 16)? != 8
        || allocated_bit_depth != 8
    {
        return None;
    }

    let mut planes = [Plane {
        offset: 0,
        col_inc: 0,
        row_inc: 0,
    }; 3];
    for (index, plane) in planes.iter_mut().enumerate() {
        let base = header_len + index * MEDIA_IMAGE_PLANE_SIZE;
        let offset = read_u32(data, base)? as usize;
        let col_inc = read_i32(data, base + 4)?;
        let row_inc = read_i32(data, base + 8)?;
        let expected_sub = if index == 0 { 1 } else { 2 };
        if col_inc <= 0
            || row_inc <= 0
            || read_u32(data, base + 12)? != expected_sub
            || read_u32(data, base + 16)? != expected_sub
        {
            return None;
        }
        *plane = Plane {
            offset,
            col_inc: col_inc as usize,
            row_inc: row_inc as usize,
        };
    }

    // Luma is always one byte per pixel. Chroma is either planar (col_inc 1,
    // separate offsets) or interleaved (col_inc 2, U and V one byte apart).
    if planes[0].col_inc != 1 || planes[1].col_inc > 2 || planes[1].col_inc != planes[2].col_inc {
        return None;
    }
    if planes[1].col_inc == 2 {
        if planes[1].row_inc != planes[2].row_inc
            || planes[1].offset.abs_diff(planes[2].offset) != 1
        {
            return None;
        }
    } else if planes[1].offset == planes[2].offset {
        return None;
    }

    Some(PlaneLayout {
        y: planes[0],
        u: planes[1],
        v: planes[2],
    })
}

/// Convert a decoded frame into tightly-packed planar I420 using explicit
/// plane addressing.
///
/// This is the flexible-format path: the `image-data` descriptor tells us where
/// the visible Y/U/V samples start and how many bytes apart they are, so no
/// guess about NV12 vs. I420 (or a vendor layout) is needed.
pub fn to_i420_with_planes(src: &[u8], planes: &PlaneLayout, crop: CropRect) -> Vec<u8> {
    let (w, h) = (crop.width, crop.height);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let (cw, ch) = (w / 2, h / 2);
    let mut out = Vec::with_capacity(w * h + 2 * cw * ch);
    if copy_plane(src, planes.y, crop.left, crop.top, w, h, &mut out).is_none()
        || copy_plane(src, planes.u, crop.left / 2, crop.top / 2, cw, ch, &mut out).is_none()
        || copy_plane(src, planes.v, crop.left / 2, crop.top / 2, cw, ch, &mut out).is_none()
    {
        return Vec::new();
    }
    out
}

fn copy_plane(
    src: &[u8],
    plane: Plane,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    out: &mut Vec<u8>,
) -> Option<()> {
    if width == 0 || height == 0 || plane.col_inc == 0 {
        return None;
    }
    let row_len = (width - 1).checked_mul(plane.col_inc)?.checked_add(1)?;
    for row in 0..height {
        let start = plane
            .offset
            .checked_add(top.checked_add(row)?.checked_mul(plane.row_inc)?)?
            .checked_add(left.checked_mul(plane.col_inc)?)?;
        let line = src.get(start..start.checked_add(row_len)?)?;
        if plane.col_inc == 1 {
            out.extend_from_slice(line);
        } else {
            out.extend(line.iter().step_by(plane.col_inc));
        }
    }
    Some(())
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_ne_bytes(bytes.try_into().ok()?))
}

fn read_i32(data: &[u8], offset: usize) -> Option<i32> {
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(i32::from_ne_bytes(bytes.try_into().ok()?))
}

fn planar_to_i420(src: &[u8], stride: usize, slice_height: usize, crop: CropRect) -> Vec<u8> {
    let (w, h) = (crop.width, crop.height);
    let (cw, ch) = (w / 2, h / 2);
    let mut out = Vec::with_capacity(w * h + 2 * cw * ch);

    for row in 0..h {
        let start = (crop.top + row) * stride + crop.left;
        let Some(line) = src.get(start..start + w) else {
            return Vec::new();
        };
        out.extend_from_slice(line);
    }

    let c_stride = stride / 2;
    let c_slice_height = slice_height / 2;
    let c_plane_len = c_stride * c_slice_height;
    let u_plane = stride * slice_height;
    let c_left = crop.left / 2;
    let c_top = crop.top / 2;

    for plane in [u_plane, u_plane + c_plane_len] {
        for row in 0..ch {
            let start = plane + (c_top + row) * c_stride + c_left;
            let Some(line) = src.get(start..start + cw) else {
                return Vec::new();
            };
            out.extend_from_slice(line);
        }
    }

    out
}

fn semi_planar_to_i420(
    src: &[u8],
    stride: usize,
    slice_height: usize,
    crop: CropRect,
    swap_uv: bool,
) -> Vec<u8> {
    let (w, h) = (crop.width, crop.height);
    let (cw, ch) = (w / 2, h / 2);
    let mut out = Vec::with_capacity(w * h + 2 * cw * ch);

    for row in 0..h {
        let start = (crop.top + row) * stride + crop.left;
        let Some(line) = src.get(start..start + w) else {
            return Vec::new();
        };
        out.extend_from_slice(line);
    }

    let uv_plane = stride * slice_height;
    let c_left = crop.left / 2;
    let c_top = crop.top / 2;
    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        let start = uv_plane + (c_top + row) * stride + c_left * 2;
        let Some(line) = src.get(start..start + 2 * cw) else {
            return Vec::new();
        };
        for pair in line.as_chunks::<2>().0 {
            if swap_uv {
                u.push(pair[1]);
                v.push(pair[0]);
            } else {
                u.push(pair[0]);
                v.push(pair[1]);
            }
        }
    }
    out.extend_from_slice(&u);
    out.extend_from_slice(&v);

    out
}

/// Rotate a tightly-packed I420 frame clockwise by `degrees` (0/90/180/270).
///
/// Returns the new buffer and dimensions; 90/270 swap width and height.
pub fn rotate_i420_clockwise(
    src: &[u8],
    w: usize,
    h: usize,
    degrees: u32,
) -> (Vec<u8>, usize, usize) {
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || src.len() < w * h * 3 / 2 {
        return (Vec::new(), w, h);
    }

    let y_len = w * h;
    let c_len = (w / 2) * (h / 2);
    match degrees % 360 {
        90 | 270 => {
            let mut out = Vec::with_capacity(src.len());
            out.extend_from_slice(&rotate_plane_clockwise(&src[..y_len], w, h, degrees));
            for plane in [y_len, y_len + c_len] {
                out.extend_from_slice(&rotate_plane_clockwise(
                    &src[plane..plane + c_len],
                    w / 2,
                    h / 2,
                    degrees,
                ));
            }
            (out, h, w)
        }
        180 => {
            let mut out = Vec::with_capacity(src.len());
            out.extend_from_slice(&rotate_plane_clockwise(&src[..y_len], w, h, degrees));
            for plane in [y_len, y_len + c_len] {
                out.extend_from_slice(&rotate_plane_clockwise(
                    &src[plane..plane + c_len],
                    w / 2,
                    h / 2,
                    degrees,
                ));
            }
            (out, w, h)
        }
        _ => (src.to_vec(), w, h),
    }
}

fn rotate_plane_clockwise(src: &[u8], w: usize, h: usize, degrees: u32) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    match degrees % 360 {
        // dst is h wide and w tall: dst(x, y) = src(h - 1 - x, y)
        90 => {
            for y in 0..w {
                for x in 0..h {
                    out[y * h + x] = src[(h - 1 - x) * w + y];
                }
            }
        }
        // dst(x, y) = src(w - 1 - x, h - 1 - y)
        180 => {
            for y in 0..h {
                for x in 0..w {
                    out[y * w + x] = src[(h - 1 - y) * w + (w - 1 - x)];
                }
            }
        }
        // dst is h wide and w tall: dst(x, y) = src(x, w - 1 - y)
        270 => {
            for y in 0..w {
                for x in 0..h {
                    out[y * h + x] = src[x * w + (w - 1 - y)];
                }
            }
        }
        _ => out.copy_from_slice(src),
    }
    out
}

/// Interleave I420 chroma into NV12.
pub fn i420_to_nv12(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let y_len = w * h;
    let c_len = (w / 2) * (h / 2);
    if w == 0 || h == 0 || src.len() < y_len + 2 * c_len {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(y_len + 2 * c_len);
    out.extend_from_slice(&src[..y_len]);
    for i in 0..c_len {
        out.push(src[y_len + i]);
        out.push(src[y_len + c_len + i]);
    }
    out
}

/// Scale tightly-packed planar I420 to `dst_w` x `dst_h` (both even).
pub fn scale_i420(src: &[u8], src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> Vec<u8> {
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

    fn full(w: usize, h: usize) -> CropRect {
        CropRect {
            left: 0,
            top: 0,
            width: w,
            height: h,
        }
    }

    fn i420_planes(w: usize, h: usize) -> (usize, usize) {
        (w * h, (w / 2) * (h / 2))
    }

    #[test]
    fn nv12_to_i420_converts() {
        // 2x2 NV12: Y = [0,1,2,3], UV interleaved = [4,5] (U=4, V=5).
        let src = [0u8, 1, 2, 3, 4, 5];
        let out = to_i420(&src, 2, 2, COLOR_FORMAT_YUV420_SEMI_PLANAR, full(2, 2));
        assert_eq!(out, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn nv21_to_i420_swaps_uv() {
        // 2x2 NV21: Y = [0,1,2,3], VU interleaved = [4,5] (V=4, U=5).
        let src = [0u8, 1, 2, 3, 4, 5];
        let out = to_i420(
            &src,
            2,
            2,
            COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR,
            full(2, 2),
        );
        // I420 planar: Y, then U=5, then V=4.
        assert_eq!(out, vec![0, 1, 2, 3, 5, 4]);
    }

    #[test]
    fn unsupported_color_format_yields_empty() {
        let src = vec![0u8; 4 * 4 * 3 / 2];
        let out = to_i420(&src, 4, 4, 0x36, full(4, 4));
        assert!(out.is_empty());
    }

    #[test]
    fn flexible_color_format_is_not_guessed_as_nv12() {
        let src = vec![0u8; 4 * 4 * 3 / 2];
        let out = to_i420(&src, 4, 4, COLOR_FORMAT_YUV420_FLEXIBLE, full(4, 4));
        assert!(out.is_empty());
    }

    fn plane(offset: usize, col_inc: usize, row_inc: usize) -> Plane {
        Plane {
            offset,
            col_inc,
            row_inc,
        }
    }

    fn media_image_descriptor(
        size: usize,
        header_len: usize,
        planes: [Plane; 3],
        kind: u32,
        num_planes: u32,
        bit_depth: u32,
    ) -> Vec<u8> {
        let mut data = vec![0u8; size];
        data[0..4].copy_from_slice(&kind.to_ne_bytes());
        data[4..8].copy_from_slice(&num_planes.to_ne_bytes());
        data[8..12].copy_from_slice(&4u32.to_ne_bytes());
        data[12..16].copy_from_slice(&4u32.to_ne_bytes());
        data[16..20].copy_from_slice(&bit_depth.to_ne_bytes());
        if header_len == 24 {
            data[20..24].copy_from_slice(&bit_depth.to_ne_bytes());
        }
        for (index, plane) in planes.iter().enumerate() {
            let base = header_len + index * MEDIA_IMAGE_PLANE_SIZE;
            data[base..base + 4].copy_from_slice(&(plane.offset as u32).to_ne_bytes());
            data[base + 4..base + 8].copy_from_slice(&(plane.col_inc as i32).to_ne_bytes());
            data[base + 8..base + 12].copy_from_slice(&(plane.row_inc as i32).to_ne_bytes());
            let sub = if index == 0 { 1u32 } else { 2 };
            data[base + 12..base + 16].copy_from_slice(&sub.to_ne_bytes());
            data[base + 16..base + 20].copy_from_slice(&sub.to_ne_bytes());
        }
        data
    }

    fn media_image2(planes: [Plane; 3]) -> Vec<u8> {
        media_image_descriptor(MEDIA_IMAGE_V2_SIZE, 24, planes, MEDIA_IMAGE_TYPE_YUV, 3, 8)
    }

    fn media_image_v1(planes: [Plane; 3]) -> Vec<u8> {
        media_image_descriptor(MEDIA_IMAGE_V1_SIZE, 20, planes, MEDIA_IMAGE_TYPE_YUV, 3, 8)
    }

    #[test]
    fn image_data_resolves_interleaved_nv12() {
        let expected = PlaneLayout {
            y: plane(0, 1, 4),
            u: plane(16, 2, 4),
            v: plane(17, 2, 4),
        };
        let parsed = parse_image_data(&media_image2([expected.y, expected.u, expected.v]))
            .expect("valid NV12 descriptor");
        assert_eq!(parsed, expected);

        let mut src = vec![0u8; 24];
        for (index, byte) in src[..16].iter_mut().enumerate() {
            *byte = index as u8;
        }
        src[16..24].copy_from_slice(&[10, 20, 11, 21, 12, 22, 13, 23]);

        let out = to_i420_with_planes(&src, &parsed, full(4, 4));
        assert_eq!(
            out,
            vec![
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 10, 11, 12, 13, 20, 21, 22,
                23
            ]
        );
    }

    #[test]
    fn image_data_resolves_planar_padded_crop() {
        let stride = 8usize;
        let slice = 8usize;
        let c_plane = (stride / 2) * (slice / 2);
        let layout = media_image2([
            plane(0, 1, stride),
            plane(stride * slice, 1, stride / 2),
            plane(stride * slice + c_plane, 1, stride / 2),
        ]);
        let parsed = parse_image_data(&layout).expect("valid planar descriptor");

        let mut src = vec![0u8; stride * slice + 2 * c_plane];
        for row in 0..4 {
            for col in 0..4 {
                src[(2 + row) * stride + 2 + col] = (row * 4 + col + 1) as u8;
            }
        }
        for row in 0..2 {
            for col in 0..2 {
                let chroma = row * 2 + col;
                src[stride * slice + (1 + row) * (stride / 2) + 1 + col] = 100 + chroma as u8;
                src[stride * slice + c_plane + (1 + row) * (stride / 2) + 1 + col] =
                    200 + chroma as u8;
            }
        }

        let crop = CropRect {
            left: 2,
            top: 2,
            width: 4,
            height: 4,
        };
        let out = to_i420_with_planes(&src, &parsed, crop);
        assert_eq!(out.len(), 16 + 2 * 4);
        assert_eq!(&out[..16], &(1..=16).collect::<Vec<u8>>()[..]);
        assert_eq!(&out[16..], &[100, 101, 102, 103, 200, 201, 202, 203]);
    }

    #[test]
    fn image_data_supports_the_legacy_descriptor() {
        let expected = PlaneLayout {
            y: plane(0, 1, 4),
            u: plane(16, 1, 2),
            v: plane(20, 1, 2),
        };
        let parsed = parse_image_data(&media_image_v1([expected.y, expected.u, expected.v]))
            .expect("valid MediaImage descriptor");
        assert_eq!(parsed, expected);

        let mut src = vec![0u8; 24];
        for (index, byte) in src.iter_mut().enumerate() {
            *byte = index as u8;
        }
        assert_eq!(to_i420_with_planes(&src, &parsed, full(4, 4)), src);
    }

    #[test]
    fn image_data_rejects_unknown_or_inconsistent_layouts() {
        let valid = PlaneLayout {
            y: plane(0, 1, 4),
            u: plane(16, 1, 2),
            v: plane(20, 1, 2),
        };
        let encode = |planes: [Plane; 3], kind: u32, num: u32, depth: u32| {
            media_image_descriptor(104, 24, planes, kind, num, depth)
        };

        assert!(parse_image_data(&[]).is_none());
        assert!(parse_image_data(&[0u8; MEDIA_IMAGE_V1_SIZE - 1]).is_none());
        assert!(parse_image_data(&encode(
            [valid.y, valid.u, valid.v],
            MEDIA_IMAGE_TYPE_YUV + 1,
            3,
            8
        ))
        .is_none());
        assert!(parse_image_data(&encode(
            [valid.y, valid.u, valid.v],
            MEDIA_IMAGE_TYPE_YUV,
            2,
            8
        ))
        .is_none());
        assert!(parse_image_data(&encode(
            [valid.y, valid.u, valid.v],
            MEDIA_IMAGE_TYPE_YUV,
            3,
            10
        ))
        .is_none());
        assert!(parse_image_data(&encode(
            [plane(0, 2, 4), valid.u, valid.v],
            MEDIA_IMAGE_TYPE_YUV,
            3,
            8
        ))
        .is_none());
        assert!(parse_image_data(&encode(
            [valid.y, plane(16, 2, 4), plane(20, 2, 4)],
            MEDIA_IMAGE_TYPE_YUV,
            3,
            8
        ))
        .is_none());
        assert!(parse_image_data(&encode(
            [valid.y, valid.u, valid.u],
            MEDIA_IMAGE_TYPE_YUV,
            3,
            8
        ))
        .is_none());
    }

    #[test]
    fn image_data_conversion_rejects_a_truncated_frame() {
        let layout = PlaneLayout {
            y: plane(0, 1, 4),
            u: plane(16, 2, 4),
            v: plane(17, 2, 4),
        };
        assert!(to_i420_with_planes(&[0u8; 23], &layout, full(4, 4)).is_empty());
    }

    #[test]
    fn planar_roundtrip_preserves_layout() {
        let (ys, cs) = i420_planes(4, 4);
        let mut src = vec![0u8; ys + 2 * cs];
        for (i, b) in src.iter_mut().enumerate() {
            *b = i as u8;
        }
        let out = to_i420(&src, 4, 4, COLOR_FORMAT_YUV420_PLANAR, full(4, 4));
        assert_eq!(out, src);
    }

    #[test]
    fn planar_crop_extracts_visible_region() {
        // 6x6 I420 with stride 8 / slice height 8 and a 4x4 visible region at (2,2).
        let stride = 8usize;
        let slice = 8usize;
        let mut src = vec![0u8; stride * slice + 2 * (stride / 2) * (slice / 2)];
        for y in 0..4 {
            for x in 0..4 {
                src[(2 + y) * stride + 2 + x] = (y * 4 + x + 1) as u8;
            }
        }
        let y_len = stride * slice;
        let c_len = (stride / 2) * (slice / 2);
        // Chroma visible region (1,1)-(2,2) inside the chroma plane.
        for (row, values) in [[69usize, 70], [73, 74]].into_iter().enumerate() {
            for (i, pos) in values.into_iter().enumerate() {
                src[pos] = 100 + (row * 2 + i) as u8;
                src[y_len + c_len + (pos - y_len)] = 200 + (row * 2 + i) as u8;
            }
        }

        let crop = CropRect {
            left: 2,
            top: 2,
            width: 4,
            height: 4,
        };
        let out = to_i420(&src, stride, slice, COLOR_FORMAT_YUV420_PLANAR, crop);

        assert_eq!(out.len(), 4 * 4 + 2 * 2 * 2);
        assert_eq!(&out[..16], &(1..=16).collect::<Vec<u8>>()[..]);
        assert_eq!(&out[16..], &[100, 101, 102, 103, 200, 201, 202, 203]);
    }

    #[test]
    fn semi_planar_crop_extracts_visible_region() {
        // 4x4 NV12 with stride 6 / slice height 4, visible region starting at (2,2).
        let stride = 6usize;
        let slice = 4usize;
        let y_len = stride * slice;
        let mut src = vec![0u8; y_len + stride * (slice / 2)];
        for y in 0..2 {
            for x in 0..2 {
                src[(2 + y) * stride + 2 + x] = (y * 2 + x + 1) as u8;
            }
        }
        // UV visible region starts at (c_left * 2, c_top) = (2, 1) inside the UV plane.
        src[y_len + stride + 2] = 10;
        src[y_len + stride + 3] = 11;

        let crop = CropRect {
            left: 2,
            top: 2,
            width: 2,
            height: 2,
        };
        let out = to_i420(&src, stride, slice, COLOR_FORMAT_YUV420_SEMI_PLANAR, crop);

        assert_eq!(out, vec![1, 2, 3, 4, 10, 11]);
    }

    #[test]
    fn rotate_90_clockwise_maps_corners() {
        // 4x2 luma:
        // 1 2 3 4
        // 5 6 7 8
        let mut src = vec![0u8; 4 * 2 + 2 * 2];
        src[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);

        let (out, w, h) = rotate_i420_clockwise(&src, 4, 2, 90);

        assert_eq!((w, h), (2, 4));
        // 5 1
        // 6 2
        // 7 3
        // 8 4
        assert_eq!(&out[..8], &[5, 1, 6, 2, 7, 3, 8, 4]);
    }

    #[test]
    fn rotate_180_keeps_dimensions() {
        let mut src = vec![0u8; 4 * 2 + 2 * 2];
        src[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);

        let (out, w, h) = rotate_i420_clockwise(&src, 4, 2, 180);

        assert_eq!((w, h), (4, 2));
        assert_eq!(&out[..8], &[8, 7, 6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn rotate_270_clockwise_maps_corners() {
        let mut src = vec![0u8; 4 * 2 + 2 * 2];
        src[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);

        let (out, w, h) = rotate_i420_clockwise(&src, 4, 2, 270);

        assert_eq!((w, h), (2, 4));
        // 4 8
        // 3 7
        // 2 6
        // 1 5
        assert_eq!(&out[..8], &[4, 8, 3, 7, 2, 6, 1, 5]);
    }

    #[test]
    fn rotate_rotates_chroma_planes_too() {
        let (ys, cs) = i420_planes(4, 2);
        let mut src = vec![0u8; ys + 2 * cs];
        src[..ys].fill(120);
        src[ys..ys + cs].fill(50);
        src[ys + cs..].fill(200);

        let (out, _, _) = rotate_i420_clockwise(&src, 4, 2, 90);

        assert_eq!(&out[..ys], &[120u8; 8][..]);
        assert_eq!(&out[ys..ys + cs], &[50u8; 2][..]);
        assert_eq!(&out[ys + cs..], &[200u8; 2][..]);
    }

    #[test]
    fn i420_to_nv12_interleaves_uv() {
        let src = [1, 2, 3, 4, 10, 20];
        let out = i420_to_nv12(&src, 2, 2);
        assert_eq!(out, vec![1, 2, 3, 4, 10, 20]);
    }

    #[test]
    fn scale_i420_halves_dimensions() {
        let (ys, cs) = i420_planes(4, 4);
        let src = vec![128u8; ys + 2 * cs];
        let out = scale_i420(&src, 4, 4, 2, 2);
        let (out_ys, out_cs) = i420_planes(2, 2);
        assert_eq!(out.len(), out_ys + 2 * out_cs);
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
