use std::io::Cursor;

use crate::compress::image::encode;
use crate::compress::image::resize;
use crate::compress::image::{compute_target_dimensions, CompressOptions};
use crate::error::Error;

const DEFAULT_DELAY_MS: i32 = 100;

pub fn transcode_gif(input: &[u8], options: CompressOptions) -> Result<Vec<u8>, Error> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);

    let mut reader = opts
        .read_info(Cursor::new(input))
        .map_err(|e| Error::DecodeError(format!("gif decode init failed: {e}")))?;

    let src_w = reader.width() as u32;
    let src_h = reader.height() as u32;
    if src_w == 0 || src_h == 0 {
        return Err(Error::DecodeError("GIF has zero dimensions".into()));
    }

    let (target_w, target_h) =
        compute_target_dimensions(src_w, src_h, options.min_width, options.min_height);

    let mut frames: Vec<(Vec<u8>, i32)> = Vec::new();
    while let Some(frame) = reader
        .read_next_frame()
        .map_err(|e| Error::DecodeError(format!("gif decode frame failed: {e}")))?
    {
        let delay_ms = if frame.delay == 0 {
            DEFAULT_DELAY_MS
        } else {
            (frame.delay as i32) * 10
        };

        let rgba = frame.buffer.to_vec();
        let resized = resize::resize_rgba_nearest(&rgba, src_w, src_h, target_w, target_h);
        frames.push((resized, delay_ms));
    }

    if frames.is_empty() {
        return Err(Error::DecodeError(
            "GIF contains no decodable frames".into(),
        ));
    }

    super::super::gif_imagequant_encode::encode_gif(
        &encode::merge_frames_min_delay(frames),
        target_w,
        target_h,
        options.quality,
    )
}
