use libwebp_sys as sys;
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

/// Encode a sequence of RGBA frames into an animated WebP.
///
/// `frames` is a slice of `(rgba_pixels, delay_ms)` pairs; every frame must
/// share the same `w` × `h` dimensions. Frame delays are preserved exactly,
/// including the last frame: the animation is finished with
/// `WebPAnimEncoderAdd(NULL, ts_total)`, which pins the last frame's duration
/// to its own delay (the `webp` crate's `AnimEncoder` wrapper cannot do this —
/// it ends with timestamp 0, making libwebp fall back to "last frame duration
/// = average of previous durations").
///
/// Encoder options use libwebp defaults (loop forever, no size minimization),
/// matching the historical implementation.
pub fn encode_animated(
    frames: &[(Vec<u8>, i32)],
    w: u32,
    h: u32,
    quality: f32,
) -> Result<Vec<u8>, Error> {
    if frames.is_empty() {
        return Err(Error::EncodeError("encode_animated: no frames".into()));
    }
    let stride = w as i32 * 4;
    let expected_len = w as usize * h as usize * 4;

    unsafe {
        let mut options = std::mem::MaybeUninit::<sys::WebPAnimEncoderOptions>::uninit();
        if sys::WebPAnimEncoderOptionsInitInternal(
            options.as_mut_ptr(),
            sys::WEBP_MUX_ABI_VERSION as i32,
        ) == 0
        {
            return Err(Error::EncodeError(
                "WebPAnimEncoderOptionsInit failed".into(),
            ));
        }
        let options = options.assume_init();

        let mut config =
            WebPConfig::new().map_err(|_| Error::EncodeError("WebPConfig::new() failed".into()))?;
        config.lossless = 0;
        config.quality = quality;
        config.thread_level = 1;
        config.method = 4;
        if sys::WebPValidateConfig(&config) == 0 {
            return Err(Error::EncodeError("invalid WebPConfig".into()));
        }

        let enc = sys::WebPAnimEncoderNewInternal(
            w as i32,
            h as i32,
            &options,
            sys::WEBP_MUX_ABI_VERSION as i32,
        );
        if enc.is_null() {
            return Err(Error::EncodeError("WebPAnimEncoderNew failed".into()));
        }
        let enc = AnimEncoderGuard(enc);

        let mut ts: i32 = 0;
        for (pixels, delay) in frames {
            if pixels.len() < expected_len {
                return Err(Error::EncodeError(format!(
                    "encode_animated: frame has {} bytes, expected {}",
                    pixels.len(),
                    expected_len
                )));
            }

            let mut pic = sys::WebPPicture::new()
                .map_err(|_| Error::EncodeError("WebPPictureInit failed".into()))?;
            pic.use_argb = 1;
            pic.width = w as i32;
            pic.height = h as i32;
            if sys::WebPPictureImportRGBA(&mut pic, pixels.as_ptr(), stride) == 0 {
                sys::WebPPictureFree(&mut pic);
                return Err(Error::EncodeError("WebPPictureImportRGBA failed".into()));
            }

            let ok = sys::WebPAnimEncoderAdd(enc.0, &mut pic, ts, &config);
            sys::WebPPictureFree(&mut pic);
            if ok == 0 {
                return Err(Error::EncodeError(format!(
                    "WebPAnimEncoderAdd failed: {}",
                    anim_error(enc.0)
                )));
            }
            ts = ts.saturating_add((*delay).max(0));
        }

        // End-of-animation marker: determines the last frame's duration.
        if sys::WebPAnimEncoderAdd(enc.0, std::ptr::null_mut(), ts, std::ptr::null()) == 0 {
            return Err(Error::EncodeError(format!(
                "WebPAnimEncoderAdd(end) failed: {}",
                anim_error(enc.0)
            )));
        }

        let mut data = std::mem::MaybeUninit::<sys::WebPData>::uninit();
        if sys::WebPAnimEncoderAssemble(enc.0, data.as_mut_ptr()) == 0 {
            return Err(Error::EncodeError(format!(
                "WebPAnimEncoderAssemble failed: {}",
                anim_error(enc.0)
            )));
        }
        let mut data = data.assume_init();
        let out = std::slice::from_raw_parts(data.bytes, data.size).to_vec();
        sys::WebPDataClear(&mut data);
        Ok(out)
    }
}

/// Frees the animation encoder on every code path.
struct AnimEncoderGuard(*mut sys::WebPAnimEncoder);

impl Drop for AnimEncoderGuard {
    fn drop(&mut self) {
        unsafe { sys::WebPAnimEncoderDelete(self.0) };
    }
}

unsafe fn anim_error(enc: *mut sys::WebPAnimEncoder) -> String {
    let ptr = sys::WebPAnimEncoderGetError(enc);
    if ptr.is_null() {
        "unknown error".into()
    } else {
        std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter().copied().cycle().take((w * h * 4) as usize).collect()
    }

    /// Durations (ms) of every `ANMF` chunk in order.
    fn anmf_durations(webp: &[u8]) -> Vec<u32> {
        let mut out = Vec::new();
        let mut pos = 12usize; // skip RIFF header + "WEBP"
        while pos + 8 <= webp.len() {
            let id = &webp[pos..pos + 4];
            let size = u32::from_le_bytes(webp[pos + 4..pos + 8].try_into().unwrap()) as usize;
            if id == b"ANMF" && size >= 16 {
                let d = &webp[pos + 8 + 12..pos + 8 + 15];
                out.push(d[0] as u32 | (d[1] as u32) << 8 | (d[2] as u32) << 16);
            }
            pos += 8 + size + (size & 1);
        }
        out
    }

    #[test]
    fn animated_encode_preserves_non_uniform_delays_including_last() {
        let frames = vec![
            (solid_frame(2, 2, [255, 0, 0, 255]), 100),
            (solid_frame(2, 2, [0, 0, 255, 255]), 300),
        ];
        let out = encode_animated(&frames, 2, 2, 75.0).expect("encode failed");

        assert!(webp::BitstreamFeatures::new(&out).unwrap().has_animation());

        let anim = webp::AnimDecoder::new(&out).decode().expect("decode failed");
        assert_eq!(anim.len(), 2);
        // libwebp reports each frame's cumulative end timestamp:
        // 100 (0+100) and 400 (100+300).
        let stamps: Vec<i32> = (0..anim.len())
            .map(|i| anim.get_frame(i).unwrap().get_time_ms())
            .collect();
        assert_eq!(stamps, vec![100, 400]);

        // Per-frame durations from the ANMF chunks: the last frame must keep
        // its own delay (300), not the average of previous ones (100).
        assert_eq!(anmf_durations(&out), vec![100, 300]);
    }

    #[test]
    fn animated_encode_uniform_delays() {
        let frames = vec![
            (solid_frame(3, 2, [10, 20, 30, 255]), 80),
            (solid_frame(3, 2, [40, 50, 60, 255]), 80),
            (solid_frame(3, 2, [70, 80, 90, 255]), 80),
        ];
        let out = encode_animated(&frames, 3, 2, 75.0).expect("encode failed");

        let anim = webp::AnimDecoder::new(&out).decode().expect("decode failed");
        assert_eq!(anim.len(), 3);
        assert_eq!(anmf_durations(&out), vec![80, 80, 80]);
        assert_eq!(anim.loop_count, 0); // loop forever
    }
}
