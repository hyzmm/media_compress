use webp::{Encoder, WebPConfig};

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
