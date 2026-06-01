use crate::error::Error;

fn clamp_quality(quality: f32) -> u8 {
    quality.clamp(0.0, 100.0).round() as u8
}

fn delay_to_centiseconds(delay_ms: i32) -> u16 {
    (delay_ms.max(10) / 10) as u16
}

fn rgba_to_lq_pixels(pixels: &[u8], w: u32, h: u32) -> Result<Vec<imagequant::RGBA>, Error> {
    let expected = (w as usize) * (h as usize) * 4;
    if pixels.len() != expected {
        return Err(Error::EncodeError(format!(
            "invalid RGBA length for GIF frame: got {}, expected {}",
            pixels.len(),
            expected
        )));
    }

    Ok(pixels
        .chunks_exact(4)
        .map(|px| imagequant::RGBA::new(px[0], px[1], px[2], px[3]))
        .collect())
}

fn new_imagequant_attributes(quality: f32) -> Result<imagequant::Attributes, Error> {
    let q = clamp_quality(quality);
    // ImageOptim's GIF quality scale is more aggressive than a linear 0-100
    // imagequant ceiling; map user quality to an equivalent effective range.
    let effective_q = ((q as f32) * 0.62).round().clamp(1.0, 100.0) as u8;
    let mut attr = imagequant::new();
    // ImageOptim-style strategy: slower search and quality ceiling based on
    // requested quality, while allowing lossy quantization for smaller output.
    attr.set_speed(1)
        .map_err(|e| Error::EncodeError(format!("imagequant set_speed failed: {e}")))?;
    let max_colors = if effective_q >= 90 {
        256
    } else if effective_q >= 80 {
        192
    } else if effective_q >= 70 {
        160
    } else if effective_q >= 60 {
        128
    } else {
        96
    };
    attr.set_max_colors(max_colors)
        .map_err(|e| Error::EncodeError(format!("imagequant set_max_colors failed: {e}")))?;
    attr.set_quality(0, effective_q)
        .map_err(|e| Error::EncodeError(format!("imagequant set_quality failed: {e}")))?;
    Ok(attr)
}

fn palette_to_gif_bytes(palette: &[imagequant::RGBA]) -> (Vec<u8>, Option<u8>) {
    let mut gif_palette = Vec::with_capacity(palette.len() * 3);
    let mut transparent = None;
    for (idx, color) in palette.iter().enumerate() {
        gif_palette.extend_from_slice(&[color.r, color.g, color.b]);
        if transparent.is_none() && color.a < 128 {
            transparent = Some(idx as u8);
        }
    }

    // Reserve one explicit transparent index for delta-frame encoding.
    if transparent.is_none() && palette.len() < 256 {
        transparent = Some(palette.len() as u8);
        gif_palette.extend_from_slice(&[0, 0, 0]);
    }

    (gif_palette, transparent)
}

fn quantize_global_palette(
    attr: &imagequant::Attributes,
    rgba_frames: &[Vec<imagequant::RGBA>],
    w: u32,
    h: u32,
) -> Result<imagequant::QuantizationResult, Error> {
    let mut hist = imagequant::Histogram::new(attr);
    for frame in rgba_frames {
        let mut image = attr
            .new_image_borrowed(frame, w as usize, h as usize, 0.0)
            .map_err(|e| Error::EncodeError(format!("imagequant new_image failed: {e}")))?;
        hist.add_image(attr, &mut image)
            .map_err(|e| Error::EncodeError(format!("imagequant histogram add failed: {e}")))?;
    }

    let mut quantized = hist
        .quantize(attr)
        .map_err(|e| Error::EncodeError(format!("imagequant histogram quantize failed: {e}")))?;

    // Dithering usually improves visual quality but increases entropy and size.
    quantized
        .set_dithering_level(0.0)
        .map_err(|e| Error::EncodeError(format!("imagequant set_dithering_level failed: {e}")))?;
    Ok(quantized)
}

fn diff_bounds(prev: &[u8], curr: &[u8], w: usize, h: usize) -> Option<(u16, u16, u16, u16)> {
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut changed = false;

    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let idx = row + x;
            if prev[idx] != curr[idx] {
                changed = true;
                if x < min_x {
                    min_x = x;
                }
                if y < min_y {
                    min_y = y;
                }
                if x > max_x {
                    max_x = x;
                }
                if y > max_y {
                    max_y = y;
                }
            }
        }
    }

    if !changed {
        return None;
    }

    Some((
        min_x as u16,
        min_y as u16,
        (max_x - min_x + 1) as u16,
        (max_y - min_y + 1) as u16,
    ))
}

fn non_transparent_bounds(
    indexed: &[u8],
    w: usize,
    h: usize,
    transparent_idx: u8,
) -> Option<(u16, u16, u16, u16)> {
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut changed = false;

    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let idx = row + x;
            if indexed[idx] != transparent_idx {
                changed = true;
                if x < min_x {
                    min_x = x;
                }
                if y < min_y {
                    min_y = y;
                }
                if x > max_x {
                    max_x = x;
                }
                if y > max_y {
                    max_y = y;
                }
            }
        }
    }

    if !changed {
        return None;
    }

    Some((
        min_x as u16,
        min_y as u16,
        (max_x - min_x + 1) as u16,
        (max_y - min_y + 1) as u16,
    ))
}

fn crop_indexed_region(
    indexed: &[u8],
    full_w: usize,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height);
    for row in 0..height {
        let start = (top + row) * full_w + left;
        out.extend_from_slice(&indexed[start..start + width]);
    }
    out
}

pub(super) fn encode_gif(
    frames: &[(Vec<u8>, i32)],
    w: u32,
    h: u32,
    quality: f32,
) -> Result<Vec<u8>, Error> {
    if frames.is_empty() {
        return Err(Error::EncodeError(
            "cannot encode GIF with zero frames".into(),
        ));
    }
    if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
        return Err(Error::EncodeError(format!(
            "invalid GIF dimensions: {}x{}",
            w, h
        )));
    }

    let mut rgba_frames = Vec::with_capacity(frames.len());
    for (pixels, _) in frames {
        rgba_frames.push(rgba_to_lq_pixels(pixels, w, h)?);
    }

    let attr = new_imagequant_attributes(quality)?;
    let mut quantized = quantize_global_palette(&attr, &rgba_frames, w, h)?;
    let (global_palette, transparent) = palette_to_gif_bytes(quantized.palette());

    let mut indexed_frames = Vec::with_capacity(rgba_frames.len());
    for rgba in &rgba_frames {
        let mut image = attr
            .new_image_borrowed(rgba, w as usize, h as usize, 0.0)
            .map_err(|e| Error::EncodeError(format!("imagequant new_image failed: {e}")))?;
        let (_, indexed) = quantized
            .remapped(&mut image)
            .map_err(|e| Error::EncodeError(format!("imagequant remap failed: {e}")))?;
        indexed_frames.push(indexed);
    }

    let mut out_frames = Vec::with_capacity(indexed_frames.len());
    let full_w = w as usize;
    let full_h = h as usize;

    for idx in 0..indexed_frames.len() {
        let delay = delay_to_centiseconds(frames[idx].1);

        if idx == 0 {
            let mut frame = gif::Frame::from_indexed_pixels(
                w as u16,
                h as u16,
                indexed_frames[idx].clone(),
                transparent,
            );
            frame.delay = delay;
            frame.dispose = gif::DisposalMethod::Keep;
            out_frames.push(frame);
            continue;
        }

        if let Some((left, top, rect_w, rect_h, cropped)) =
            if let Some(transparent_idx) = transparent {
                let mut delta = indexed_frames[idx].clone();
                for (px, prev) in delta.iter_mut().zip(indexed_frames[idx - 1].iter()) {
                    if *px == *prev {
                        *px = transparent_idx;
                    }
                }

                non_transparent_bounds(&delta, full_w, full_h, transparent_idx).map(
                    |(left, top, rect_w, rect_h)| {
                        (
                            left,
                            top,
                            rect_w,
                            rect_h,
                            crop_indexed_region(
                                &delta,
                                full_w,
                                left as usize,
                                top as usize,
                                rect_w as usize,
                                rect_h as usize,
                            ),
                        )
                    },
                )
            } else {
                diff_bounds(
                    &indexed_frames[idx - 1],
                    &indexed_frames[idx],
                    full_w,
                    full_h,
                )
                .map(|(left, top, rect_w, rect_h)| {
                    (
                        left,
                        top,
                        rect_w,
                        rect_h,
                        crop_indexed_region(
                            &indexed_frames[idx],
                            full_w,
                            left as usize,
                            top as usize,
                            rect_w as usize,
                            rect_h as usize,
                        ),
                    )
                })
            }
        {
            let mut frame = gif::Frame::from_indexed_pixels(rect_w, rect_h, cropped, transparent);
            frame.left = left;
            frame.top = top;
            frame.delay = delay;
            frame.dispose = gif::DisposalMethod::Keep;
            out_frames.push(frame);
        } else if let Some(last) = out_frames.last_mut() {
            // Consecutive identical frames are merged by extending delay.
            last.delay = last.delay.saturating_add(delay);
        }
    }

    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, w as u16, h as u16, &global_palette)
            .map_err(|e| Error::EncodeError(format!("gif encoder init failed: {e}")))?;
        encoder
            .set_repeat(gif::Repeat::Infinite)
            .map_err(|e| Error::EncodeError(format!("gif set repeat failed: {e}")))?;

        for frame in &out_frames {
            encoder
                .write_frame(frame)
                .map_err(|e| Error::EncodeError(format!("gif write frame failed: {e}")))?;
        }
    }

    Ok(out)
}
