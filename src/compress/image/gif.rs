use crate::error::Error;
use image::AnimationDecoder;
use image::codecs::gif::GifDecoder;
use std::io::Cursor;
use webp::{AnimEncoder, AnimFrame, WebPConfig};

pub fn compress_gif_to_webp(input: &[u8], quality: f32) -> Result<Vec<u8>, Error> {
    let cursor = Cursor::new(input);
    let decoder = GifDecoder::new(cursor)
        .map_err(|e| Error::DecodeError(e.to_string()))?;

    let frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|e| Error::DecodeError(e.to_string()))?;

    if frames.is_empty() {
        return Err(Error::DecodeError("GIF has no frames".into()));
    }

    let first = &frames[0];
    let (width, height) = first.buffer().dimensions();

    let mut config = WebPConfig::new()
        .map_err(|_| Error::EncodeError("WebPConfig init failed".into()))?;
    config.lossless = 0;
    config.quality = quality;

    let mut encoder = AnimEncoder::new(width, height, &config);

    let mut timestamp_ms: i32 = 0;
    for frame in &frames {
        let (numer, denom) = frame.delay().numer_denom_ms();
        let delay_ms = if denom == 0 { 100 } else { numer / denom } as i32;

        let rgba = frame.buffer();
        let anim_frame = AnimFrame::from_rgba(rgba.as_raw(), width, height, timestamp_ms);
        encoder.add_frame(anim_frame);

        timestamp_ms += delay_ms;
    }

    let webp_data = encoder
        .try_encode()
        .map_err(|e| Error::EncodeError(format!("Failed to encode animated WebP: {:?}", e)))?;

    Ok(webp_data.to_vec())
}
