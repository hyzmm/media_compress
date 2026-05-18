use webp::{AnimEncoder, AnimFrame, Encoder, WebPConfig};

use crate::error::Error;

const MIN_FRAME_DELAY_MS: i32 = 100;

/// Merge frames so that every output frame has at least `MIN_FRAME_DELAY_MS` of delay.
/// When multiple input frames fall within one window, only the last one is kept
/// and its delay becomes the accumulated total, preserving the original total duration.
///
/// This minimum delay requirement works around a Flutter bug where animated WebP frames
/// with very short delays are rendered incorrectly:
/// https://github.com/flutter/flutter/issues/29130
pub fn merge_frames_min_delay(frames: Vec<(Vec<u8>, i32)>) -> Vec<(Vec<u8>, i32)> {
    if frames.is_empty() {
        return frames;
    }

    // For short animations (≤ 3 frames), clamp each frame individually instead
    // of merging, so no frames are dropped (total duration may increase).
    if frames.len() <= 3 {
        return frames
            .into_iter()
            .map(|(pixels, delay)| (pixels, delay.max(MIN_FRAME_DELAY_MS)))
            .collect();
    }

    let mut result: Vec<(Vec<u8>, i32)> = Vec::new();
    let mut accumulated: i32 = 0;
    let mut pending: Option<Vec<u8>> = None;

    for (pixels, delay) in frames {
        accumulated += delay.max(0);
        pending = Some(pixels);

        if accumulated >= MIN_FRAME_DELAY_MS {
            result.push((pending.take().unwrap(), accumulated));
            accumulated = 0;
        }
    }

    // Flush any remaining frames whose accumulated delay didn't reach the threshold
    if let Some(pixels) = pending {
        if let Some(last) = result.last_mut() {
            // Merge remaining delay into the last output frame
            last.1 += accumulated;
        } else {
            // Every frame was below threshold; output the last one
            result.push((pixels, accumulated.max(MIN_FRAME_DELAY_MS)));
        }
    }

    result
}

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
