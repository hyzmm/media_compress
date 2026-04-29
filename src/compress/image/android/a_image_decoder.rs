use std::ffi::c_void;

use super::webp_encode;
use crate::error::Error;

// ---------------------------------------------------------------------------
// Opaque structs (never constructed in Rust — accessed via pointer only)
// ---------------------------------------------------------------------------

#[repr(C)]
struct AImageDecoder {
    _private: [u8; 0],
}

#[repr(C)]
struct AImageDecoderHeaderInfo {
    _private: [u8; 0],
}

#[repr(C)]
struct AImageDecoderFrameInfo {
    _private: [u8; 0],
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ANDROID_BITMAP_FORMAT_RGBA_8888: i32 = 1;
const ANDROID_IMAGE_DECODER_SUCCESS: i32 = 0;
const ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT: i32 = -6;
const DEFAULT_DELAY_MS: i32 = 100;

// ---------------------------------------------------------------------------
// FFI — android/imagedecoder.h  (NDK API level 28+)
// ---------------------------------------------------------------------------

extern "C" {
    fn AImageDecoder_createFromBuffer(
        buffer: *const c_void,
        length: usize,
        out_decoder: *mut *mut AImageDecoder,
    ) -> i32;

    fn AImageDecoder_delete(decoder: *mut AImageDecoder);

    fn AImageDecoder_getHeaderInfo(decoder: *const AImageDecoder)
        -> *const AImageDecoderHeaderInfo;

    fn AImageDecoderHeaderInfo_getWidth(info: *const AImageDecoderHeaderInfo) -> i32;
    fn AImageDecoderHeaderInfo_getHeight(info: *const AImageDecoderHeaderInfo) -> i32;

    fn AImageDecoder_setAndroidBitmapFormat(decoder: *mut AImageDecoder, format: i32) -> i32;

    fn AImageDecoder_getMinimumStride(decoder: *mut AImageDecoder) -> usize;

    fn AImageDecoder_decodeImage(
        decoder: *mut AImageDecoder,
        pixels: *mut c_void,
        stride: usize,
        size: usize,
    ) -> i32;

    fn AImageDecoder_isAnimated(decoder: *mut AImageDecoder) -> i32;

    fn AImageDecoder_advanceFrame(decoder: *mut AImageDecoder) -> i32;

    fn AImageDecoderFrameInfo_create(decoder: *mut AImageDecoder) -> *mut AImageDecoderFrameInfo;

    /// Returns frame duration in nanoseconds.
    fn AImageDecoderFrameInfo_getDuration(info: *const AImageDecoderFrameInfo) -> i64;

    fn AImageDecoderFrameInfo_delete(info: *mut AImageDecoderFrameInfo);
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn compress(input: &[u8], quality: f32) -> Result<Vec<u8>, Error> {
    unsafe {
        // ── Create decoder ─────────────────────────────────────────────────
        let mut dec: *mut AImageDecoder = std::ptr::null_mut();
        let ret =
            AImageDecoder_createFromBuffer(input.as_ptr() as *const c_void, input.len(), &mut dec);
        if ret != ANDROID_IMAGE_DECODER_SUCCESS || dec.is_null() {
            if ret == ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT {
                return Err(Error::PlatformNotSupported(
                    "format not supported by AImageDecoder on this device".into(),
                ));
            }
            return Err(Error::DecodeError(format!(
                "AImageDecoder_createFromBuffer failed: {}",
                ret
            )));
        }

        // ── Get dimensions ─────────────────────────────────────────────────
        let info = AImageDecoder_getHeaderInfo(dec);
        if info.is_null() {
            AImageDecoder_delete(dec);
            return Err(Error::DecodeError(
                "AImageDecoder_getHeaderInfo returned null".into(),
            ));
        }
        let w = AImageDecoderHeaderInfo_getWidth(info) as u32;
        let h = AImageDecoderHeaderInfo_getHeight(info) as u32;

        if w == 0 || h == 0 {
            AImageDecoder_delete(dec);
            return Err(Error::DecodeError("Image has zero dimensions".into()));
        }

        // ── Force RGBA_8888 output ─────────────────────────────────────────
        let ret = AImageDecoder_setAndroidBitmapFormat(dec, ANDROID_BITMAP_FORMAT_RGBA_8888);
        if ret != ANDROID_IMAGE_DECODER_SUCCESS {
            AImageDecoder_delete(dec);
            return Err(Error::DecodeError(format!(
                "AImageDecoder_setAndroidBitmapFormat failed: {}",
                ret
            )));
        }

        let stride = AImageDecoder_getMinimumStride(dec);
        let buf_size = stride * h as usize;
        let mut buf = vec![0u8; buf_size];

        // ── Animated or static? ────────────────────────────────────────────
        let animated = AImageDecoder_isAnimated(dec) != 0;

        let result = if !animated {
            // ── Static ──────────────────────────────────────────────────────
            let ret =
                AImageDecoder_decodeImage(dec, buf.as_mut_ptr() as *mut c_void, stride, buf_size);
            if ret != ANDROID_IMAGE_DECODER_SUCCESS {
                AImageDecoder_delete(dec);
                if ret == ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT {
                    return Err(Error::PlatformNotSupported(
                        "format not supported by AImageDecoder on this device".into(),
                    ));
                }
                return Err(Error::DecodeError(format!(
                    "AImageDecoder_decodeImage failed: {}",
                    ret
                )));
            }
            // stride may be wider than w*4; slice to exact RGBA rows
            let rgba = compact_rgba(&buf, w, h, stride);
            webp_encode::encode_static(&rgba, w, h, quality)
        } else {
            // ── Animated ────────────────────────────────────────────────────
            // Collect all frame pixel data first so that every Vec<u8> lives
            // long enough for AnimEncoder (which borrows each slice).
            let mut frame_data: Vec<(Vec<u8>, i32)> = Vec::new();
            let mut is_first = true;

            loop {
                let ret = AImageDecoder_decodeImage(
                    dec,
                    buf.as_mut_ptr() as *mut c_void,
                    stride,
                    buf_size,
                );
                if ret != ANDROID_IMAGE_DECODER_SUCCESS {
                    if is_first {
                        AImageDecoder_delete(dec);
                        if ret == ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT {
                            return Err(Error::PlatformNotSupported(
                                "format not supported by AImageDecoder on this device".into(),
                            ));
                        }
                        return Err(Error::DecodeError(format!(
                            "AImageDecoder_decodeImage failed on frame 0: {}",
                            ret
                        )));
                    }
                    break;
                }
                is_first = false;

                let rgba = compact_rgba(&buf, w, h, stride);

                // Read frame duration before advancing
                let fi = AImageDecoderFrameInfo_create(dec);
                let delay_ms = if fi.is_null() {
                    DEFAULT_DELAY_MS
                } else {
                    let ns = AImageDecoderFrameInfo_getDuration(fi);
                    AImageDecoderFrameInfo_delete(fi);
                    ((ns / 1_000_000) as i32).max(10)
                };

                frame_data.push((rgba, delay_ms));

                let adv = AImageDecoder_advanceFrame(dec);
                if adv != ANDROID_IMAGE_DECODER_SUCCESS {
                    break;
                }
            }

            webp_encode::encode_animated(&frame_data, w, h, quality)
        };

        AImageDecoder_delete(dec);
        result
    }
}

/// When `stride > w * 4`, compact rows so that `webp::Encoder` receives
/// tightly-packed RGBA without padding bytes.
fn compact_rgba(buf: &[u8], w: u32, h: u32, stride: usize) -> Vec<u8> {
    let row_bytes = w as usize * 4;
    if stride == row_bytes {
        return buf[..row_bytes * h as usize].to_vec();
    }
    let mut out = Vec::with_capacity(row_bytes * h as usize);
    for row in 0..h as usize {
        let start = row * stride;
        out.extend_from_slice(&buf[start..start + row_bytes]);
    }
    out
}
