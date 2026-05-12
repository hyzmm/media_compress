/// Resize tightly-packed RGBA pixels using bilinear interpolation.
///
/// The function name is kept for API stability inside this crate.
pub fn resize_rgba_nearest(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    if dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }

    let src_len_expected = src_w as usize * src_h as usize * 4;
    if src.len() != src_len_expected {
        return src.to_vec();
    }

    if src_w == dst_w && src_h == dst_h {
        return src.to_vec();
    }

    let mut out = vec![0u8; dst_w as usize * dst_h as usize * 4];

    let src_wf = src_w as f32;
    let src_hf = src_h as f32;
    let dst_wf = dst_w as f32;
    let dst_hf = dst_h as f32;

    for y in 0..dst_h {
        let fy = ((y as f32 + 0.5) * src_hf / dst_hf - 0.5).clamp(0.0, src_hf - 1.0);
        let y0 = fy.floor() as u32;
        let y1 = (y0 + 1).min(src_h - 1);
        let wy = fy - y0 as f32;

        for x in 0..dst_w {
            let fx = ((x as f32 + 0.5) * src_wf / dst_wf - 0.5).clamp(0.0, src_wf - 1.0);
            let x0 = fx.floor() as u32;
            let x1 = (x0 + 1).min(src_w - 1);
            let wx = fx - x0 as f32;

            let idx00 = ((y0 * src_w + x0) * 4) as usize;
            let idx01 = ((y0 * src_w + x1) * 4) as usize;
            let idx10 = ((y1 * src_w + x0) * 4) as usize;
            let idx11 = ((y1 * src_w + x1) * 4) as usize;

            let out_idx = ((y * dst_w + x) * 4) as usize;
            for c in 0..4 {
                let p00 = src[idx00 + c] as f32;
                let p01 = src[idx01 + c] as f32;
                let p10 = src[idx10 + c] as f32;
                let p11 = src[idx11 + c] as f32;

                let top = p00 + (p01 - p00) * wx;
                let bottom = p10 + (p11 - p10) * wx;
                let val = top + (bottom - top) * wy;
                out[out_idx + c] = val.round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    out
}
