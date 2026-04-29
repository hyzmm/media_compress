use jni::objects::{JByteArray, JObject, JValue};
use jni::sys::{jint, jobject, JNIEnv as JNIEnvRaw};
use jni::JNIEnv;
use jni::JavaVM;

use crate::compress::image::ImageFormat;
use crate::error::Error;

const ANDROID_BITMAP_RESULT_SUCCESS: i32 = 0;
const ANDROID_BITMAP_FORMAT_RGBA_8888: i32 = 1;

#[repr(C)]
struct AndroidBitmapInfo {
    width: u32,
    height: u32,
    stride: u32,
    format: i32,
    flags: u32,
}

#[link(name = "jnigraphics")]
extern "C" {
    fn AndroidBitmap_getInfo(
        env: *mut JNIEnvRaw,
        bitmap: jobject,
        info: *mut AndroidBitmapInfo,
    ) -> i32;
    fn AndroidBitmap_lockPixels(
        env: *mut JNIEnvRaw,
        bitmap: jobject,
        addr_ptr: *mut *mut core::ffi::c_void,
    ) -> i32;
    fn AndroidBitmap_unlockPixels(env: *mut JNIEnvRaw, bitmap: jobject) -> i32;
}
/// 通过 JNI 获取 android.os.Build.VERSION.SDK_INT
pub fn get_sdk_int(env: &jni::JNIEnv) -> Result<i32, crate::error::Error> {
    let version_class = env
        .find_class("android/os/Build$VERSION")
        .map_err(|e| crate::error::Error::NativeError(format!("find Build.VERSION failed: {e}")))?;
    let sdk_int_field = env
        .get_static_field(version_class, "SDK_INT", "I")
        .map_err(|e| crate::error::Error::NativeError(format!("get SDK_INT failed: {e}")))?;
    sdk_int_field
        .i()
        .map_err(|e| crate::error::Error::NativeError(format!("SDK_INT not int: {e}")))
}

pub fn compress(input: &[u8], quality: f32) -> Result<Vec<u8>, Error> {
    let (rgba, w, h) = jni_bitmap_factory::decode_to_rgba(input)?;
    webp_encode::encode_static(&rgba, w, h, quality)
}

fn decode_to_rgba(input: &[u8]) -> Result<(Vec<u8>, u32, u32), Error> {
    let fmt = ImageFormat::detect(input);
    let ctx = match try_android_context() {
        Some(c) => c,
        None => {
            return unsupported_for_format(
                fmt,
                "android context was not initialized (non-Activity runtime)",
            )
        }
    };
    let vm_ptr = ctx.vm();
    if vm_ptr.is_null() {
        return unsupported_for_format(fmt, "Java VM is unavailable in this runtime");
    }

    let vm = unsafe { JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) }
        .map_err(|e| Error::NativeError(format!("JavaVM::from_raw failed: {e}")))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| Error::NativeError(format!("attach_current_thread failed: {e}")))?;

    decode_with_bitmap_factory(&mut env, input, fmt)
}

fn try_android_context() -> Option<ndk_context::AndroidContext> {
    // ndk_context::android_context() panics when not initialized (e.g. native
    // test runners). Probe it without emitting panic noise to stderr.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = std::panic::catch_unwind(ndk_context::android_context).ok();
    std::panic::set_hook(prev);
    out
}

fn decode_with_bitmap_factory(
    env: &mut JNIEnv,
    input: &[u8],
    fmt: Option<ImageFormat>,
) -> Result<(Vec<u8>, u32, u32), Error> {
    let byte_array = env
        .byte_array_from_slice(input)
        .map_err(|e| Error::DecodeError(format!("byte_array_from_slice failed: {e}")))?;

    let bitmap = call_decode_byte_array(env, &byte_array, input.len())?;
    if bitmap.is_null() {
        return unsupported_for_format(fmt, "BitmapFactory.decodeByteArray returned null");
    }

    let bitmap = ensure_argb_8888(env, &bitmap)?;
    if bitmap.is_null() {
        return Err(Error::DecodeError("Bitmap.copy returned null".into()));
    }

    extract_rgba_pixels(env, &bitmap)
}

fn call_decode_byte_array<'a>(
    env: &mut JNIEnv<'a>,
    input: &JByteArray<'a>,
    input_len: usize,
) -> Result<JObject<'a>, Error> {
    let bitmap_factory = env
        .find_class("android/graphics/BitmapFactory")
        .map_err(|e| Error::NativeError(format!("find BitmapFactory failed: {e}")))?;

    env.call_static_method(
        bitmap_factory,
        "decodeByteArray",
        "([BII)Landroid/graphics/Bitmap;",
        &[
            JValue::Object(input.as_ref()),
            JValue::Int(0),
            JValue::Int(input_len as jint),
        ],
    )
    .and_then(|v| v.l())
    .map_err(|e| Error::DecodeError(format!("BitmapFactory.decodeByteArray failed: {e}")))
}

fn ensure_argb_8888<'a>(env: &mut JNIEnv<'a>, bitmap: &JObject<'a>) -> Result<JObject<'a>, Error> {
    let config_class = env
        .find_class("android/graphics/Bitmap$Config")
        .map_err(|e| Error::NativeError(format!("find Bitmap$Config failed: {e}")))?;

    let argb_8888 = env
        .get_static_field(
            config_class,
            "ARGB_8888",
            "Landroid/graphics/Bitmap$Config;",
        )
        .and_then(|v| v.l())
        .map_err(|e| Error::NativeError(format!("get ARGB_8888 failed: {e}")))?;

    env.call_method(
        bitmap,
        "copy",
        "(Landroid/graphics/Bitmap$Config;Z)Landroid/graphics/Bitmap;",
        &[JValue::Object(&argb_8888), JValue::Bool(0)],
    )
    .and_then(|v| v.l())
    .map_err(|e| Error::DecodeError(format!("Bitmap.copy failed: {e}")))
}

fn extract_rgba_pixels(env: &mut JNIEnv, bitmap: &JObject) -> Result<(Vec<u8>, u32, u32), Error> {
    let mut info = AndroidBitmapInfo {
        width: 0,
        height: 0,
        stride: 0,
        format: 0,
        flags: 0,
    };

    let env_raw = env.get_raw();
    let bitmap_raw = bitmap.as_raw();

    let info_ret = unsafe { AndroidBitmap_getInfo(env_raw, bitmap_raw, &mut info) };
    if info_ret != ANDROID_BITMAP_RESULT_SUCCESS {
        return Err(Error::DecodeError(format!(
            "AndroidBitmap_getInfo failed: {info_ret}"
        )));
    }
    if info.format != ANDROID_BITMAP_FORMAT_RGBA_8888 {
        return Err(Error::DecodeError(format!(
            "unsupported Android bitmap format: {}",
            info.format
        )));
    }

    let mut pixels_ptr: *mut core::ffi::c_void = core::ptr::null_mut();
    let lock_ret = unsafe { AndroidBitmap_lockPixels(env_raw, bitmap_raw, &mut pixels_ptr) };
    if lock_ret != ANDROID_BITMAP_RESULT_SUCCESS || pixels_ptr.is_null() {
        return Err(Error::DecodeError(format!(
            "AndroidBitmap_lockPixels failed: {lock_ret}"
        )));
    }

    let row_bytes = info.width as usize * 4;
    let stride = info.stride as usize;
    let mut out = Vec::with_capacity(row_bytes * info.height as usize);

    unsafe {
        let all =
            core::slice::from_raw_parts(pixels_ptr as *const u8, stride * info.height as usize);
        for row in 0..info.height as usize {
            let start = row * stride;
            out.extend_from_slice(&all[start..start + row_bytes]);
        }
        let _ = AndroidBitmap_unlockPixels(env_raw, bitmap_raw);
    }

    Ok((out, info.width, info.height))
}

fn unsupported_for_format(
    fmt: Option<ImageFormat>,
    reason: &str,
) -> Result<(Vec<u8>, u32, u32), Error> {
    let msg = match fmt {
        Some(ImageFormat::Heic) => {
            "HEIC is not supported by Android JNI fallback decoder".to_string()
        }
        Some(ImageFormat::Tiff) => {
            "TIFF is not supported by Android JNI fallback decoder".to_string()
        }
        Some(_) | None => format!("Android JNI fallback decoder unavailable: {reason}"),
    };
    Err(Error::PlatformNotSupported(msg))
}
