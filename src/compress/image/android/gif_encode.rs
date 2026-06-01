use crate::error::Error;

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
            let mut frame_pixels = pixels.clone();
            let mut frame = gif::Frame::from_rgba_speed(w as u16, h as u16, &mut frame_pixels, 10);
            frame.delay = ((*delay_ms).max(10) / 10) as u16;
            encoder
                .write_frame(&frame)
                .map_err(|e| Error::EncodeError(format!("gif write frame failed: {e}")))?;
        }
    }
    Ok(out)
}