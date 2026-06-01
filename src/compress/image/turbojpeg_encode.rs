use crate::error::Error;

use turbojpeg::{Compressor, Image, PixelFormat, Subsamp};

pub(super) fn encode_rgba_to_jpeg(
    rgba: &[u8],
    w: u32,
    h: u32,
    quality: f32,
) -> Result<Vec<u8>, Error> {
    if w == 0 || h == 0 {
        return Err(Error::EncodeError("invalid image dimensions".into()));
    }

    let expected = w as usize * h as usize * 4;
    if rgba.len() != expected {
        return Err(Error::EncodeError(format!(
            "invalid RGBA length: got {}, expected {}",
            rgba.len(),
            expected
        )));
    }

    let q = quality.round().clamp(1.0, 100.0) as i32;
    let image = Image {
        pixels: rgba,
        width: w as usize,
        pitch: (w as usize) * 4,
        height: h as usize,
        format: PixelFormat::RGBA,
    };

    let mut compressor =
        Compressor::new().map_err(|e| Error::EncodeError(format!("TurboJPEG init failed: {e}")))?;
    compressor
        .set_quality(q)
        .map_err(|e| Error::EncodeError(format!("TurboJPEG set_quality failed: {e}")))?;
    compressor
        .set_subsamp(Subsamp::Sub2x2)
        .map_err(|e| Error::EncodeError(format!("TurboJPEG set_subsamp failed: {e}")))?;
    compressor
        .set_optimize(true)
        .map_err(|e| Error::EncodeError(format!("TurboJPEG set_optimize failed: {e}")))?;
    compressor
        .compress_to_vec(image)
        .map_err(|e| Error::EncodeError(format!("TurboJPEG encode failed: {e}")))
}
