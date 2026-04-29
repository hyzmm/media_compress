use std::ffi::{c_char, c_void};
use std::sync::OnceLock;

use crate::compress::image::webp_encode;
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
// Dynamic loading — avoids static symbol references so the binary can load
// on API < 28 devices. Symbols are resolved at call time via dlsym.
//
// On API 28-33, AImageDecoder symbols are in libmediandk.so.
// On API 34+, they're in libandroid.so (always loaded → RTLD_DEFAULT works).
// We try RTLD_DEFAULT first, then fall back to dlopen("libmediandk.so").
// ---------------------------------------------------------------------------

const RTLD_DEFAULT: *mut c_void = 0 as *mut c_void;
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
type GetHeaderInfoFn =
    unsafe extern "C" fn(*const AImageDecoder) -> *const AImageDecoderHeaderInfo;
type HeaderGetWidthFn = unsafe extern "C" fn(*const AImageDecoderHeaderInfo) -> i32;
type HeaderGetHeightFn = unsafe extern "C" fn(*const AImageDecoderHeaderInfo) -> i32;
type SetBitmapFormatFn = unsafe extern "C" fn(*mut AImageDecoder, i32) -> i32;
type GetMinimumStrideFn = unsafe extern "C" fn(*mut AImageDecoder) -> usize;
type DecodeImageFn =
    unsafe extern "C" fn(*mut AImageDecoder, *mut c_void, usize, usize) -> i32;
type IsAnimatedFn = unsafe extern "C" fn(*mut AImageDecoder) -> i32;
type AdvanceFrameFn = unsafe extern "C" fn(*mut AImageDecoder) -> i32;
type FrameInfoCreateFn =
    unsafe extern "C" fn(*mut AImageDecoder) -> *mut AImageDecoderFrameInfo;
type FrameInfoGetDurationFn = unsafe extern "C" fn(*const AImageDecoderFrameInfo) -> i64;
type FrameInfoDeleteFn = unsafe extern "C" fn(*mut AImageDecoderFrameInfo);

#[allow(non_snake_case)]
struct Api {
    AImageDecoder_createFromBuffer: CreateFromBufferFn,
    AImageDecoder_delete: DeleteFn,
    AImageDecoder_getHeaderInfo: GetHeaderInfoFn,
    AImageDecoderHeaderInfo_getWidth: HeaderGetWidthFn,
    AImageDecoderHeaderInfo_getHeight: HeaderGetHeightFn,
    AImageDecoder_setAndroidBitmapFormat: SetBitmapFormatFn,
    AImageDecoder_getMinimumStride: GetMinimumStrideFn,
    AImageDecoder_decodeImage: DecodeImageFn,
    AImageDecoder_isAnimated: IsAnimatedFn,
    AImageDecoder_advanceFrame: AdvanceFrameFn,
    AImageDecoderFrameInfo_create: FrameInfoCreateFn,
    AImageDecoderFrameInfo_getDuration: FrameInfoGetDurationFn,
    AImageDecoderFrameInfo_delete: FrameInfoDeleteFn,
}

unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(try_load).as_ref()
}

fn try_load() -> Option<Api> {
    unsafe {
        // Try RTLD_DEFAULT first (covers libandroid.so on API 34+).
        let handle = try_load_from(RTLD_DEFAULT);
        if handle.is_some() {
            return handle;
        }
        // Fall back to dlopen("libmediandk.so") for API 28-33.
        let mediandk = dlopen(c"libmediandk.so".as_ptr(), RTLD_NOW);
        if mediandk.is_null() {
            return None;
        }
        try_load_from(mediandk)
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

        Some(Api {
            AImageDecoder_createFromBuffer: load!("AImageDecoder_createFromBuffer"),
            AImageDecoder_delete: load!("AImageDecoder_delete"),
            AImageDecoder_getHeaderInfo: load!("AImageDecoder_getHeaderInfo"),
            AImageDecoderHeaderInfo_getWidth: load!("AImageDecoderHeaderInfo_getWidth"),
            AImageDecoderHeaderInfo_getHeight: load!("AImageDecoderHeaderInfo_getHeight"),
            AImageDecoder_setAndroidBitmapFormat: load!(
                "AImageDecoder_setAndroidBitmapFormat"
            ),
            AImageDecoder_getMinimumStride: load!("AImageDecoder_getMinimumStride"),
            AImageDecoder_decodeImage: load!("AImageDecoder_decodeImage"),
            AImageDecoder_isAnimated: load!("AImageDecoder_isAnimated"),
            AImageDecoder_advanceFrame: load!("AImageDecoder_advanceFrame"),
            AImageDecoderFrameInfo_create: load!("AImageDecoderFrameInfo_create"),
            AImageDecoderFrameInfo_getDuration: load!("AImageDecoderFrameInfo_getDuration"),
            AImageDecoderFrameInfo_delete: load!("AImageDecoderFrameInfo_delete"),
        })
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn compress(input: &[u8], quality: f32) -> Result<Vec<u8>, Error> {
    let api = match api() {
        Some(a) => a,
        None => {
            return Err(Error::PlatformNotSupported(
                "AImageDecoder not available on this device (requires API 28+)".into(),
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

        // ── Force RGBA_8888 output ─────────────────────────────────────────
        let ret =
            (api.AImageDecoder_setAndroidBitmapFormat)(dec, ANDROID_BITMAP_FORMAT_RGBA_8888);
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
        let animated = (api.AImageDecoder_isAnimated)(dec) != 0;

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
            webp_encode::encode_static(&rgba, w, h, quality)
        } else {
            // ── Animated ────────────────────────────────────────────────────
            let mut frame_data: Vec<(Vec<u8>, i32)> = Vec::new();
            let mut is_first = true;

            loop {
                let ret = (api.AImageDecoder_decodeImage)(
                    dec,
                    buf.as_mut_ptr() as *mut c_void,
                    stride,
                    buf_size,
                );
                if ret != ANDROID_IMAGE_DECODER_SUCCESS {
                    if is_first {
                        (api.AImageDecoder_delete)(dec);
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
                let fi = (api.AImageDecoderFrameInfo_create)(dec);
                let delay_ms = if fi.is_null() {
                    DEFAULT_DELAY_MS
                } else {
                    let ns = (api.AImageDecoderFrameInfo_getDuration)(fi);
                    (api.AImageDecoderFrameInfo_delete)(fi);
                    ((ns / 1_000_000) as i32).max(10)
                };

                frame_data.push((rgba, delay_ms));

                let adv = (api.AImageDecoder_advanceFrame)(dec);
                if adv != ANDROID_IMAGE_DECODER_SUCCESS {
                    break;
                }
            }

            webp_encode::encode_animated(&frame_data, w, h, quality)
        };

        (api.AImageDecoder_delete)(dec);
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
