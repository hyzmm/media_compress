//! GIF compression: decode with `zengif`, re-encode as GIF with the
//! quantette quantizer (MIT/Apache-2.0). Shared by every platform.

use std::borrow::Cow;
use std::io::Cursor;

use zengif::{
    Decoder, FrameInput, Limits, QuantetteQuantizer, QuantizeConfig, QuantizedFrame,
    QuantizerTrait, Repeat, Rgba, Stop, Unstoppable,
};

use super::{compute_target_dimensions, resize, CompressOptions};
use crate::error::Error;

/// Pixels with alpha below this value are written with the frame's transparent
/// palette index. zengif uses the same threshold when it looks for
/// transparency.
const TRANSPARENT_ALPHA_THRESHOLD: u8 = 128;

/// Decode all frames of a GIF and re-encode it as a GIF.
///
/// Per-frame delays and the loop count are preserved as-is (no merging, no
/// clamping, no re-timing). Frames are downscaled when `min_width` /
/// `min_height` demand it. `quality` is forwarded to the quantizer, but the
/// quantette backend ignores it (zengif 0.7.3).
pub(crate) fn compress_gif(input: &[u8], options: CompressOptions) -> Result<Vec<u8>, Error> {
    let limits = Limits::default();
    let mut dec = Decoder::new(Cursor::new(input), limits, &Unstoppable)
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

    // The re-encoder writes each decoded (already composited) frame as a full
    // canvas with `DisposalMethod::Background`. Every frame therefore starts
    // from a transparent canvas instead of inheriting pixels that were erased
    // in the source animation. That makes the previous frame irrelevant as a
    // transparency mask, so palette building and quantization stay RGB-only.
    let mut config = QuantizeConfig::default();
    config.quality = options.quality.round().clamp(1.0, 100.0) as u8;
    config.dithering = 0.0;
    config.use_background = false;

    let frames: Vec<FrameInput> = frames
        .iter()
        .map(|(delay, pixels)| {
            FrameInput::from_bytes(target_w as u16, target_h as u16, *delay, pixels)
        })
        .collect();

    let mut quantizer = TransparentSafeQuantizer::new(QuantetteQuantizer::new());
    let frame_refs: Vec<&[Rgba]> = frames.iter().map(|frame| frame.pixels.as_slice()).collect();
    let palette = quantizer
        .build_shared_palette(
            &frame_refs,
            target_w as u16,
            target_h as u16,
            &config,
            &Unstoppable,
        )
        .map_err(|e| Error::EncodeError(format!("zengif: {e}")))?;

    let output = Vec::new();
    let mut encoder = gif::Encoder::new(output, target_w as u16, target_h as u16, &palette)
        .map_err(|e| Error::EncodeError(format!("gif: {e}")))?;

    if let Some(repeat) = gif_repeat(repeat) {
        encoder
            .write_extension(gif::ExtensionData::Repetitions(repeat))
            .map_err(|e| Error::EncodeError(format!("gif: {e}")))?;
    }

    for frame in &frames {
        let quantized = quantizer
            .quantize_frame_with_palette(&frame.pixels, frame.width, frame.height, None, &config)
            .map_err(|e| Error::EncodeError(format!("zengif: {e}")))?;
        let gif_frame = gif::Frame {
            left: 0,
            top: 0,
            width: frame.width,
            height: frame.height,
            delay: frame.delay,
            dispose: gif::DisposalMethod::Background,
            transparent: quantized.transparent_index,
            palette: None,
            buffer: Cow::Owned(quantized.pixels),
            ..Default::default()
        };
        encoder
            .write_frame(&gif_frame)
            .map_err(|e| Error::EncodeError(format!("gif: {e}")))?;
    }

    encoder
        .into_inner()
        .map_err(|e| Error::EncodeError(format!("gif: {e}")))
}

fn gif_repeat(repeat: Repeat) -> Option<gif::Repeat> {
    match repeat {
        Repeat::Once => None,
        Repeat::Infinite => Some(gif::Repeat::Infinite),
        Repeat::Count(count) => Some(gif::Repeat::Finite(count)),
        // `Repeat` is non-exhaustive; future variants default to play-once.
        _ => None,
    }
}

/// Wrapper around a zengif quantizer that makes its transparent palette index
/// safe for GIF output.
///
/// zengif's quantette backend only quantizes RGB and afterwards marks the
/// palette index of the first pixel with alpha < 128 as transparent. Because
/// transparent pixels are decoded as `Rgba { 0, 0, 0, 0 }`, their index can
/// collide with an opaque black pixel; marking that index transparent then
/// erases the opaque content. This wrapper leaves the palette itself alone and
/// only repairs the index assignment:
///
/// * the transparent index is never used by an opaque pixel;
/// * when no free palette entry exists, a new entry is appended (palette
///   sizes below 256), or the least-used entry is sacrificed and its opaque
///   pixels are remapped to their nearest remaining color (full palette);
/// * every transparent pixel uses that single index, instead of whichever
///   index its RGB happened to quantize to.
struct TransparentSafeQuantizer<Q> {
    inner: Q,
}

impl<Q> TransparentSafeQuantizer<Q> {
    fn new(inner: Q) -> Self {
        Self { inner }
    }
}

impl<Q: QuantizerTrait> QuantizerTrait for TransparentSafeQuantizer<Q> {
    fn quantize_frame(
        &mut self,
        pixels: &[Rgba],
        width: u16,
        height: u16,
        background: Option<&[Rgba]>,
        config: &QuantizeConfig,
    ) -> zengif::Result<QuantizedFrame> {
        let frame = self
            .inner
            .quantize_frame(pixels, width, height, background, config)?;
        Ok(repair_transparency(frame, pixels))
    }

    fn build_shared_palette(
        &mut self,
        frames: &[&[Rgba]],
        width: u16,
        height: u16,
        config: &QuantizeConfig,
        stop: &dyn Stop,
    ) -> zengif::Result<Vec<u8>> {
        let mut palette = self
            .inner
            .build_shared_palette(frames, width, height, config, stop)?;
        // `repair_transparency` may need one extra index for frames whose
        // opaque pixels occupy every palette entry that quantette returned.
        // Reserve that slot in the global color table so the transparent index
        // always refers to a real entry (a full 256-entry palette uses the
        // remapping fallback instead).
        if palette.len() < 256 * 3 {
            palette.extend_from_slice(&[0, 0, 0]);
        }
        Ok(palette)
    }

    fn quantize_frame_with_palette(
        &mut self,
        pixels: &[Rgba],
        width: u16,
        height: u16,
        background: Option<&[Rgba]>,
        config: &QuantizeConfig,
    ) -> zengif::Result<QuantizedFrame> {
        let frame = self
            .inner
            .quantize_frame_with_palette(pixels, width, height, background, config)?;
        Ok(repair_transparency(frame, pixels))
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

/// Choose a transparent index that no opaque pixel uses and point every
/// transparent pixel at it.
fn repair_transparency(mut frame: QuantizedFrame, source: &[Rgba]) -> QuantizedFrame {
    if frame.pixels.len() != source.len()
        || frame.palette.is_empty()
        || !frame.palette.len().is_multiple_of(3)
    {
        return frame;
    }
    if !source
        .iter()
        .any(|pixel| pixel.a < TRANSPARENT_ALPHA_THRESHOLD)
    {
        // Never mark an opaque color as transparent when the frame has no
        // transparent pixels at all.
        frame.transparent_index = None;
        return frame;
    }

    let entries = frame.palette.len() / 3;
    if entries > 256 {
        frame.transparent_index = None;
        return frame;
    }

    let mut opaque_uses = [0u32; 256];
    for (index, pixel) in frame.pixels.iter().zip(source) {
        if pixel.a >= TRANSPARENT_ALPHA_THRESHOLD {
            opaque_uses[*index as usize] += 1;
        }
    }

    let transparent_index = frame
        .transparent_index
        .filter(|index| (*index as usize) < entries && opaque_uses[*index as usize] == 0)
        .or_else(|| {
            (0..entries)
                .find(|index| opaque_uses[*index] == 0)
                .map(|index| index as u8)
        })
        .unwrap_or_else(|| {
            if entries < 256 {
                // Every existing entry is used by opaque pixels; append a
                // dedicated transparent entry.
                frame.palette.extend_from_slice(&[0, 0, 0]);
                entries as u8
            } else {
                // Full palette: sacrifice the least-used color and remap its
                // opaque pixels to the closest remaining entry.
                let victim = (0..entries)
                    .min_by_key(|index| opaque_uses[*index])
                    .unwrap_or(0) as u8;
                remap_opaque_pixels(&mut frame, source, victim);
                victim
            }
        });

    for (index, pixel) in frame.pixels.iter_mut().zip(source) {
        if pixel.a < TRANSPARENT_ALPHA_THRESHOLD {
            *index = transparent_index;
        }
    }
    frame.transparent_index = Some(transparent_index);
    frame
}

/// Move opaque pixels that use `victim` to the nearest other palette entry.
fn remap_opaque_pixels(frame: &mut QuantizedFrame, source: &[Rgba], victim: u8) {
    let palette = &frame.palette;
    let entries = palette.len() / 3;
    let pixels = &mut frame.pixels;

    for (index, pixel) in pixels.iter_mut().zip(source) {
        if *index != victim || pixel.a < TRANSPARENT_ALPHA_THRESHOLD {
            continue;
        }

        let mut best = victim;
        let mut best_distance = u32::MAX;
        for candidate in 0..entries {
            if candidate == victim as usize {
                continue;
            }
            let base = candidate * 3;
            let dr = i32::from(pixel.r) - i32::from(palette[base]);
            let dg = i32::from(pixel.g) - i32::from(palette[base + 1]);
            let db = i32::from(pixel.b) - i32::from(palette[base + 2]);
            let distance = (dr * dr + dg * dg + db * db) as u32;
            if distance < best_distance {
                best_distance = distance;
                best = candidate as u8;
            }
        }
        *index = best;
    }
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
        assert_eq!(
            (re[0].0, re[0].1),
            (240, 135),
            "GIF frames should downscale"
        );
        assert_eq!(re.len(), src.len());
    }

    #[test]
    fn reencode_keeps_transparency_without_leaving_previous_frames() {
        // Three full-canvas frames with a transparent background and an
        // opaque black square. The transparent pixels are decoded as
        // `Rgba { 0, 0, 0, 0 }`, so their RGB matches the opaque black square
        // and the old quantizer marked the square's palette index transparent.
        // `dispose = Background` also checks that erased pixels do not remain
        // visible through the transparent index chosen for later frames.
        let mut input = Vec::new();
        {
            let mut encoder = gif::Encoder::new(&mut input, 8, 8, &[0, 0, 0, 255, 0, 255]).unwrap();
            encoder
                .write_extension(gif::ExtensionData::Repetitions(gif::Repeat::Infinite))
                .unwrap();
            for left in [1u8, 3, 5] {
                let mut buffer = vec![1u8; 8 * 8];
                for y in 3..5 {
                    for x in left..left + 2 {
                        buffer[y as usize * 8 + x as usize] = 0;
                    }
                }
                let frame = gif::Frame {
                    width: 8,
                    height: 8,
                    delay: 10,
                    dispose: gif::DisposalMethod::Background,
                    transparent: Some(1),
                    buffer: Cow::Owned(buffer),
                    ..Default::default()
                };
                encoder.write_frame(&frame).unwrap();
            }
        }

        let out = compress_gif(&input, CompressOptions::new(75.0)).unwrap();
        let frames = decode_frames(&out);
        assert_eq!(frames.len(), 3);
        for (frame_index, (_, _, _, bytes)) in frames.iter().enumerate() {
            let opaque = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| pixel[3] >= 128)
                .count();
            assert_eq!(
                opaque, 4,
                "frame {frame_index} must keep only its own black square"
            );
        }
    }
}
