//! GIF compression: decode with `zengif`, re-encode as GIF with the
//! quantette quantizer (MIT/Apache-2.0). Shared by every platform.

use std::io::Cursor;

use zengif::{Decoder, EncodeRequest, EncoderConfig, FrameInput, Limits, Quantizer, Unstoppable};

use super::{compute_target_dimensions, resize, CompressOptions};
use crate::error::Error;

/// Decode all frames of a GIF and re-encode it as a GIF.
///
/// Per-frame delays and the loop count are preserved as-is (no merging, no
/// clamping, no re-timing). Frames are downscaled when `min_width` /
/// `min_height` demand it. `quality` is forwarded to zengif, but the
/// quantette backend ignores it (zengif 0.7.3).
pub(crate) fn compress_gif(input: &[u8], options: CompressOptions) -> Result<Vec<u8>, Error> {
    let limits = Limits::default();
    let mut dec = Decoder::new(Cursor::new(input), limits.clone(), &Unstoppable)
        .map_err(|e| Error::DecodeError(format!("zengif: {e}")))?;
    let src_w = dec.metadata().width as u32;
    let src_h = dec.metadata().height as u32;

    let (target_w, target_h) =
        compute_target_dimensions(src_w, src_h, options.min_width, options.min_height);
    let downscale = (target_w, target_h) != (src_w, src_h);

    let mut frames: Vec<(u16, Vec<u8>)> = Vec::new();
    while let Some(f) = dec
        .next_frame()
        .map_err(|e| Error::DecodeError(format!("zengif: {e}")))?
    {
        let pixels = if downscale {
            resize::resize_rgba_nearest(f.as_bytes(), src_w, src_h, target_w, target_h)
        } else {
            f.as_bytes().to_vec()
        };
        frames.push((f.delay, pixels));
    }
    if frames.is_empty() {
        return Err(Error::DecodeError("zengif: no frames".into()));
    }
    // The NETSCAPE loop extension is parsed while frames are read, so the
    // repeat value is only final after the decode loop.
    let repeat = dec.metadata().repeat;

    let config = EncoderConfig::new()
        .quality(options.quality.round().clamp(1.0, 100.0) as u8)
        .quantizer(Quantizer::quantette())
        .repeat(repeat)
        // Round-trip guidance: the decoded content is already dithered.
        .dithering(0.0)
        // zengif 0.7.3's transparency-delta path corrupts frame deltas on some
        // GIFs (reproduced in examples/gif_zengif_bench.rs); keep it off.
        .use_transparency(false);

    let mut enc = EncodeRequest::new(&config, target_w as u16, target_h as u16)
        .limits(&limits)
        .stop(&Unstoppable)
        .build()
        .map_err(|e| Error::EncodeError(format!("zengif: {e}")))?;
    for (delay, pixels) in &frames {
        enc.add_frame(FrameInput::from_bytes(
            target_w as u16,
            target_h as u16,
            *delay,
            pixels,
        ))
        .map_err(|e| Error::EncodeError(format!("zengif: {e}")))?;
    }
    enc.finish()
        .map_err(|e| Error::EncodeError(format!("zengif: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_frames(data: &[u8]) -> Vec<(u16, u16, u16, Vec<u8>)> {
        let mut dec = Decoder::new(Cursor::new(data), Limits::default(), &Unstoppable).unwrap();
        let mut frames = Vec::new();
        while let Some(f) = dec.next_frame().unwrap() {
            frames.push((f.width, f.height, f.delay, f.as_bytes().to_vec()));
        }
        frames
    }

    #[test]
    fn reencode_preserves_frame_count_and_delays() {
        let input = std::fs::read("test_images/13424514615469820.gif").unwrap();
        let src = decode_frames(&input);

        let out = compress_gif(&input, CompressOptions::new(75.0)).unwrap();
        assert!(out.starts_with(b"GIF8"), "output is not a GIF");

        let re = decode_frames(&out);
        assert_eq!(re.len(), src.len());
        assert_eq!(
            re.iter().map(|f| f.2).collect::<Vec<_>>(),
            src.iter().map(|f| f.2).collect::<Vec<_>>(),
            "frame delays must be preserved"
        );
    }

    #[test]
    fn min_dimensions_downscale_frames() {
        let input = std::fs::read("test_images/test_image.gif").unwrap();
        let src = decode_frames(&input);
        assert_eq!((src[0].0, src[0].1), (480, 270));

        let mut options = CompressOptions::new(75.0);
        options.min_width = Some(240);
        let out = compress_gif(&input, options).unwrap();

        let re = decode_frames(&out);
        assert_eq!((re[0].0, re[0].1), (240, 135), "GIF frames should downscale");
        assert_eq!(re.len(), src.len());
    }
}
