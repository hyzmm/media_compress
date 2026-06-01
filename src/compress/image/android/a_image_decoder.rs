use std::ffi::{c_char, c_void};
use std::sync::OnceLock;

use crate::compress::image::resize;
use crate::compress::image::{compute_target_dimensions, CompressOptions, ImageFormat};
use crate::error::Error;

// ─────────────────────────────────────────────────────────────────────────
// Opaque structs (never constructed in Rust — accessed via pointer only)
// ─────────────────────────────────────────────────────────────────────────

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
// Dynamic loading — AImageDecoder lives in libjnigraphics.so (API 30+).
// Using dlopen/dlsym so the binary can still load on API < 30 devices
// (where the symbols are absent); those devices fall back to JNI BitmapFactory.
//
// Animation helpers (AImageDecoder_isAnimated etc.) require API 31+, so they
// are loaded as Option and treated as absent when missing.
// ---------------------------------------------------------------------------

const RTLD_NOW: i32 = 2;

extern "C" {
    fn dlopen(filename: *const c_char, flag: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

// ---------------------------------------------------------------------------
// Function pointer types
// ---------------------------------------------------------------------------

type CreateFromBufferFn =
    unsafe extern "C" fn(*const c_void, usize, *mut *mut AImageDecoder) -> i32;
type DeleteFn = unsafe extern "C" fn(*mut AImageDecoder);
type GetHeaderInfoFn = unsafe extern "C" fn(*const AImageDecoder) -> *const AImageDecoderHeaderInfo;
type HeaderGetWidthFn = unsafe extern "C" fn(*const AImageDecoderHeaderInfo) -> i32;
type HeaderGetHeightFn = unsafe extern "C" fn(*const AImageDecoderHeaderInfo) -> i32;
type SetBitmapFormatFn = unsafe extern "C" fn(*mut AImageDecoder, i32) -> i32;
type GetMinimumStrideFn = unsafe extern "C" fn(*mut AImageDecoder) -> usize;
type DecodeImageFn = unsafe extern "C" fn(*mut AImageDecoder, *mut c_void, usize, usize) -> i32;
type IsAnimatedFn = unsafe extern "C" fn(*mut AImageDecoder) -> i32;
type AdvanceFrameFn = unsafe extern "C" fn(*mut AImageDecoder) -> i32;
type FrameInfoCreateFn = unsafe extern "C" fn(*mut AImageDecoder) -> *mut AImageDecoderFrameInfo;
type FrameInfoGetDurationFn = unsafe extern "C" fn(*const AImageDecoderFrameInfo) -> i64;
type FrameInfoDeleteFn = unsafe extern "C" fn(*mut AImageDecoderFrameInfo);

#[allow(non_snake_case)]
struct Api {
    // Core decode functions — present on API 30+
    AImageDecoder_createFromBuffer: CreateFromBufferFn,
    AImageDecoder_delete: DeleteFn,
    AImageDecoder_getHeaderInfo: GetHeaderInfoFn,
    AImageDecoderHeaderInfo_getWidth: HeaderGetWidthFn,
    AImageDecoderHeaderInfo_getHeight: HeaderGetHeightFn,
    AImageDecoder_setAndroidBitmapFormat: SetBitmapFormatFn,
    AImageDecoder_getMinimumStride: GetMinimumStrideFn,
    AImageDecoder_decodeImage: DecodeImageFn,
    // Animation helpers — present on API 31+ only
    AImageDecoder_isAnimated: Option<IsAnimatedFn>,
    AImageDecoder_advanceFrame: Option<AdvanceFrameFn>,
    AImageDecoderFrameInfo_create: Option<FrameInfoCreateFn>,
    AImageDecoderFrameInfo_getDuration: Option<FrameInfoGetDurationFn>,
    AImageDecoderFrameInfo_delete: Option<FrameInfoDeleteFn>,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(try_load).as_ref()
}

fn try_load() -> Option<Api> {
    // AImageDecoder is in libjnigraphics.so on API 30+.
    // dlopen returns null on API < 30 (symbol absent) → api() returns None
    // and the caller falls back to JNI BitmapFactory.
    unsafe {
        let handle = dlopen(c"libjnigraphics.so".as_ptr(), RTLD_NOW);
        if handle.is_null() {
            return None;
        }
        try_load_from(handle)
    }
}

fn try_load_from(handle: *mut c_void) -> Option<Api> {
    unsafe {
        macro_rules! load {
            ($name:expr) => {{
                let ptr = dlsym(handle, concat!($name, "\0").as_ptr() as *const c_char);
                if ptr.is_null() {
                    return None;
                }
                std::mem::transmute::<*mut c_void, _>(ptr)
            }};
        }
        macro_rules! load_opt {
            ($name:expr) => {{
                let ptr = dlsym(handle, concat!($name, "\0").as_ptr() as *const c_char);
                if ptr.is_null() {
                    None
                } else {
                    Some(std::mem::transmute::<*mut c_void, _>(ptr))
                }
            }};
        }

        Some(Api {
            AImageDecoder_createFromBuffer: load!("AImageDecoder_createFromBuffer"),
            AImageDecoder_delete: load!("AImageDecoder_delete"),
            AImageDecoder_getHeaderInfo: load!("AImageDecoder_getHeaderInfo"),
            AImageDecoderHeaderInfo_getWidth: load!("AImageDecoderHeaderInfo_getWidth"),
            AImageDecoderHeaderInfo_getHeight: load!("AImageDecoderHeaderInfo_getHeight"),
            AImageDecoder_setAndroidBitmapFormat: load!("AImageDecoder_setAndroidBitmapFormat"),
            AImageDecoder_getMinimumStride: load!("AImageDecoder_getMinimumStride"),
            AImageDecoder_decodeImage: load!("AImageDecoder_decodeImage"),
            AImageDecoder_isAnimated: load_opt!("AImageDecoder_isAnimated"),
            AImageDecoder_advanceFrame: load_opt!("AImageDecoder_advanceFrame"),
            AImageDecoderFrameInfo_create: load_opt!("AImageDecoderFrameInfo_create"),
            AImageDecoderFrameInfo_getDuration: load_opt!("AImageDecoderFrameInfo_getDuration"),
            AImageDecoderFrameInfo_delete: load_opt!("AImageDecoderFrameInfo_delete"),
        })
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn compress(input: &[u8], options: CompressOptions) -> Result<Vec<u8>, Error> {
    if matches!(ImageFormat::detect(input), Some(ImageFormat::Gif)) {
        return super::gif_codec::transcode_gif(input, options);
    }

    let api = match api() {
        Some(a) => a,
        None => {
            return Err(Error::PlatformNotSupported(
                "AImageDecoder not available on this device (requires API 30+)".into(),
            ));
        }
    };

    unsafe {
        // ── Create decoder ─────────────────────────────────────────────────
        let mut dec: *mut AImageDecoder = std::ptr::null_mut();
        let ret = (api.AImageDecoder_createFromBuffer)(
            input.as_ptr() as *const c_void,
            input.len(),
            &mut dec,
        );
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
        let info = (api.AImageDecoder_getHeaderInfo)(dec);
        if info.is_null() {
            (api.AImageDecoder_delete)(dec);
            return Err(Error::DecodeError(
                "AImageDecoder_getHeaderInfo returned null".into(),
            ));
        }
        let w = (api.AImageDecoderHeaderInfo_getWidth)(info) as u32;
        let h = (api.AImageDecoderHeaderInfo_getHeight)(info) as u32;

        if w == 0 || h == 0 {
            (api.AImageDecoder_delete)(dec);
            return Err(Error::DecodeError("Image has zero dimensions".into()));
        }

        let (target_w, target_h) =
            compute_target_dimensions(w, h, options.min_width, options.min_height);

        // ── Force RGBA_8888 output ─────────────────────────────────────────
        let ret = (api.AImageDecoder_setAndroidBitmapFormat)(dec, ANDROID_BITMAP_FORMAT_RGBA_8888);
        if ret != ANDROID_IMAGE_DECODER_SUCCESS {
            (api.AImageDecoder_delete)(dec);
            return Err(Error::DecodeError(format!(
                "AImageDecoder_setAndroidBitmapFormat failed: {}",
                ret
            )));
        }

        let stride = (api.AImageDecoder_getMinimumStride)(dec);
        let buf_size = stride * h as usize;
        let mut buf = vec![0u8; buf_size];

        // ── Animated or static? ────────────────────────────────────────────
        // AImageDecoder_isAnimated requires API 31+.
        let animated = match api.AImageDecoder_isAnimated {
            Some(is_anim) => is_anim(dec) != 0,
            None => false,
        };

        let result = if !animated {
            // ── Static ──────────────────────────────────────────────────────
            let ret = (api.AImageDecoder_decodeImage)(
                dec,
                buf.as_mut_ptr() as *mut c_void,
                stride,
                buf_size,
            );
            if ret != ANDROID_IMAGE_DECODER_SUCCESS {
                (api.AImageDecoder_delete)(dec);
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
            let (target_w, target_h) =
                compute_target_dimensions(w, h, options.min_width, options.min_height);
            let resized = resize::resize_rgba_nearest(&rgba, w, h, target_w, target_h);
            super::encode_rgba_to_jpeg_turbo(&resized, target_w, target_h, options.quality)
        } else {
            // For non-GIF animations, export a JPEG poster frame.
            let ret = (api.AImageDecoder_decodeImage)(
                dec,
                buf.as_mut_ptr() as *mut c_void,
                stride,
                buf_size,
            );
            if ret != ANDROID_IMAGE_DECODER_SUCCESS {
                (api.AImageDecoder_delete)(dec);
                if ret == ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT {
                    return Err(Error::PlatformNotSupported(
                        "format not supported by AImageDecoder on this device".into(),
                    ));
                }
                return Err(Error::DecodeError(format!(
                    "AImageDecoder_decodeImage failed on poster frame: {}",
                    ret
                )));
            }

            let rgba = compact_rgba(&buf, w, h, stride);
            let resized = resize::resize_rgba_nearest(&rgba, w, h, target_w, target_h);
            super::encode_rgba_to_jpeg_turbo(&resized, target_w, target_h, options.quality)
        };

        (api.AImageDecoder_delete)(dec);
        result
    }
}

/// When `stride > w * 4`, compact rows so that encoders receive
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
