use crate::error::Error;

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

fn quantize_rgba_frame(
    pixels: &[u8],
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, Vec<u8>, Option<u8>), Error> {
    let lq_pixels = rgba_to_lq_pixels(pixels, w, h)?;

    let mut attr = imagequant::new();
    attr.set_speed(5)
        .map_err(|e| Error::EncodeError(format!("imagequant set_speed failed: {e}")))?;

    let mut image = attr
        .new_image_borrowed(&lq_pixels, w as usize, h as usize, 0.0)
        .map_err(|e| Error::EncodeError(format!("imagequant new_image failed: {e}")))?;

    let mut quantized = attr
        .quantize(&mut image)
        .map_err(|e| Error::EncodeError(format!("imagequant quantize failed: {e}")))?;

    quantized
        .set_dithering_level(1.0)
        .map_err(|e| Error::EncodeError(format!("imagequant set_dithering_level failed: {e}")))?;

    let (palette, indexed_pixels) = quantized
        .remapped(&mut image)
        .map_err(|e| Error::EncodeError(format!("imagequant remap failed: {e}")))?;

    let mut gif_palette = Vec::with_capacity(palette.len() * 3);
    let mut transparent = None;
    for (idx, color) in palette.iter().enumerate() {
        gif_palette.extend_from_slice(&[color.r, color.g, color.b]);
        if transparent.is_none() && color.a < 128 {
            transparent = Some(idx as u8);
        }
    }

    Ok((indexed_pixels, gif_palette, transparent))
}

/// Encode RGBA frames to animated GIF.
pub fn encode_gif(frames: &[(Vec<u8>, i32)], w: u32, h: u32) -> Result<Vec<u8>, Error> {
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

    let mut out = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, w as u16, h as u16, &[])
            .map_err(|e| Error::EncodeError(format!("gif encoder init failed: {e}")))?;
        encoder
            .set_repeat(gif::Repeat::Infinite)
            .map_err(|e| Error::EncodeError(format!("gif set repeat failed: {e}")))?;

        for (pixels, delay_ms) in frames {
            let (indexed, palette, transparent) = quantize_rgba_frame(pixels, w, h)?;
            let mut frame =
                gif::Frame::from_palette_pixels(w as u16, h as u16, indexed, palette, transparent);
            frame.delay = ((*delay_ms).max(10) / 10) as u16;
            encoder
                .write_frame(&frame)
                .map_err(|e| Error::EncodeError(format!("gif write frame failed: {e}")))?;
        }
    }
    Ok(out)
}
