use webp::{AnimEncoder, AnimFrame, Encoder, WebPConfig};

use crate::error::Error;

/// Encode a single static RGBA frame to lossy WebP.
pub fn encode_static(pixels: &[u8], w: u32, h: u32, quality: f32) -> Result<Vec<u8>, Error> {
    let mut config =
        WebPConfig::new().map_err(|_| Error::EncodeError("WebPConfig::new() failed".into()))?;
    config.lossless = 0;
    config.quality = quality;
    config.thread_level = 1;
    config.method = 4;

    Encoder::from_rgba(pixels, w, h)
        .encode_advanced(&config)
        .map(|m| m.to_vec())
        .map_err(|e| Error::EncodeError(format!("WebPEncode failed: {:?}", e)))
}

/// Encode a sequence of RGBA frames to an animated WebP.
///
/// `frames` is a slice of `(rgba_pixels, delay_ms)` pairs.
/// All frames must share the same `w` × `h` dimensions.
pub fn encode_animated(
    frames: &[(Vec<u8>, i32)],
    w: u32,
    h: u32,
    quality: f32,
) -> Result<Vec<u8>, Error> {
    let mut config =
        WebPConfig::new().map_err(|_| Error::EncodeError("WebPConfig::new() failed".into()))?;
    config.lossless = 0;
    config.quality = quality;
    config.thread_level = 1;
    config.method = 4;

    let mut encoder = AnimEncoder::new(w, h, &config);
    let mut ts: i32 = 0;
    for (pixels, delay) in frames {
        encoder.add_frame(AnimFrame::from_rgba(pixels, w, h, ts));
        ts += delay;
    }

    encoder
        .try_encode()
        .map(|data| data.to_vec())
        .map_err(|e| Error::EncodeError(format!("AnimEncoder::try_encode failed: {:?}", e)))
}
