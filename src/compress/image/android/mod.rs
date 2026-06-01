mod a_image_decoder;
mod gif_codec;
mod gif_encode;
mod jni_bitmap_factory;

use crate::compress::image::CompressOptions;
use crate::error::Error;

use jni::objects::{JByteArray, JValue};
use jni::sys::jint;
use jni::JavaVM;

/// Compress an image. Uses AImageDecoder (API 30+) when available,
/// falls back to JNI BitmapFactory on older devices.
pub fn compress(input: &[u8], options: CompressOptions) -> Result<Vec<u8>, Error> {
    match a_image_decoder::compress(input, options) {
        Err(Error::PlatformNotSupported(_)) => jni_bitmap_factory::compress(input, options),
        other => other,
    }
}

/// JNI-based orientation retrieval via Android's `ExifInterface`.
pub fn orientation_from_metadata_jni(input: &[u8]) -> u32 {
    let vm_ptr = crate::android_runtime::java_vm_ptr()
        .or_else(|| try_android_context().map(|ctx| ctx.vm() as *mut jni::sys::JavaVM));

    let Some(vm_ptr) = vm_ptr else {
        return 1;
    };
    if vm_ptr.is_null() {
        return 1;
    }

    let vm = match unsafe { JavaVM::from_raw(vm_ptr) } {
        Ok(vm) => vm,
        Err(_) => return 1,
    };

    let mut env = match vm.attach_current_thread() {
        Ok(env) => env,
        Err(_) => return 1,
    };

    orientation_from_metadata_with_env(&mut env, input).unwrap_or(1)
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

fn orientation_from_metadata_with_env(env: &mut jni::JNIEnv, input: &[u8]) -> Option<u32> {
    let input_bytes = env.byte_array_from_slice(input).ok()?;

    let bais_class = env.find_class("java/io/ByteArrayInputStream").ok()?;
    let stream = env
        .new_object(bais_class, "([B)V", &[JValue::Object(input_bytes.as_ref())])
        .ok()?;

    let exif_class = env.find_class("android/media/ExifInterface").ok()?;
    let exif = env
        .new_object(
            &exif_class,
            "(Ljava/io/InputStream;)V",
            &[JValue::Object(&stream)],
        )
        .ok()?;

    let tag_orientation = env
        .get_static_field(&exif_class, "TAG_ORIENTATION", "Ljava/lang/String;")
        .ok()?
        .l()
        .ok()?;

    let default_orientation = env
        .get_static_field(&exif_class, "ORIENTATION_NORMAL", "I")
        .ok()?
        .i()
        .ok()?;

    let orientation = env
        .call_method(
            &exif,
            "getAttributeInt",
            "(Ljava/lang/String;I)I",
            &[
                JValue::Object(&tag_orientation),
                JValue::Int(default_orientation),
            ],
        )
        .ok()?
        .i()
        .ok()?;

    if (1..=8).contains(&orientation) {
        Some(orientation as u32)
    } else {
        Some(1)
    }
}

pub(super) fn encode_rgba_to_jpeg_jni(
    rgba: &[u8],
    w: u32,
    h: u32,
    quality: f32,
) -> Result<Vec<u8>, Error> {
    if w == 0 || h == 0 {
        return Err(Error::EncodeError("invalid image dimensions".into()));
    }
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return Err(Error::EncodeError(format!(
            "invalid RGBA buffer length: got {}, expected {}",
            rgba.len(),
            (w as usize) * (h as usize) * 4
        )));
    }

    let vm_ptr = crate::android_runtime::java_vm_ptr()
        .or_else(|| try_android_context().map(|ctx| ctx.vm() as *mut jni::sys::JavaVM))
        .ok_or_else(|| {
            Error::PlatformNotSupported("Java VM is unavailable in this runtime".into())
        })?;

    if vm_ptr.is_null() {
        return Err(Error::PlatformNotSupported(
            "Java VM is unavailable in this runtime".into(),
        ));
    }

    let vm = unsafe { JavaVM::from_raw(vm_ptr) }
        .map_err(|e| Error::NativeError(format!("JavaVM::from_raw failed: {e}")))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| Error::NativeError(format!("attach_current_thread failed: {e}")))?;

    encode_rgba_to_jpeg_with_env(&mut env, rgba, w, h, quality)
}

fn encode_rgba_to_jpeg_with_env(
    env: &mut jni::JNIEnv,
    rgba: &[u8],
    w: u32,
    h: u32,
    quality: f32,
) -> Result<Vec<u8>, Error> {
    let mut argb: Vec<jint> = Vec::with_capacity((w * h) as usize);
    for px in rgba.chunks_exact(4) {
        let r = px[0] as u32;
        let g = px[1] as u32;
        let b = px[2] as u32;
        let a = px[3] as u32;
        argb.push(((a << 24) | (r << 16) | (g << 8) | b) as jint);
    }

    let int_arr = env
        .new_int_array((w * h) as i32)
        .map_err(|e| Error::NativeError(format!("new_int_array failed: {e}")))?;
    env.set_int_array_region(&int_arr, 0, &argb)
        .map_err(|e| Error::NativeError(format!("set_int_array_region failed: {e}")))?;

    let bitmap_cls = env
        .find_class("android/graphics/Bitmap")
        .map_err(|e| Error::NativeError(format!("find Bitmap failed: {e}")))?;
    let config_cls = env
        .find_class("android/graphics/Bitmap$Config")
        .map_err(|e| Error::NativeError(format!("find Bitmap$Config failed: {e}")))?;
    let argb_8888 = env
        .get_static_field(config_cls, "ARGB_8888", "Landroid/graphics/Bitmap$Config;")
        .and_then(|v| v.l())
        .map_err(|e| Error::NativeError(format!("get ARGB_8888 failed: {e}")))?;

    let bitmap = env
        .call_static_method(
            bitmap_cls,
            "createBitmap",
            "([IIIIILandroid/graphics/Bitmap$Config;)Landroid/graphics/Bitmap;",
            &[
                JValue::Object(int_arr.as_ref()),
                JValue::Int(0),
                JValue::Int(w as i32),
                JValue::Int(w as i32),
                JValue::Int(h as i32),
                JValue::Object(&argb_8888),
            ],
        )
        .and_then(|v| v.l())
        .map_err(|e| Error::NativeError(format!("Bitmap.createBitmap failed: {e}")))?;

    let baos_cls = env
        .find_class("java/io/ByteArrayOutputStream")
        .map_err(|e| Error::NativeError(format!("find ByteArrayOutputStream failed: {e}")))?;
    let baos = env
        .new_object(baos_cls, "()V", &[])
        .map_err(|e| Error::NativeError(format!("new ByteArrayOutputStream failed: {e}")))?;

    let cf_cls = env
        .find_class("android/graphics/Bitmap$CompressFormat")
        .map_err(|e| Error::NativeError(format!("find CompressFormat failed: {e}")))?;
    let jpeg_fmt = env
        .get_static_field(cf_cls, "JPEG", "Landroid/graphics/Bitmap$CompressFormat;")
        .and_then(|v| v.l())
        .map_err(|e| Error::NativeError(format!("get JPEG compress format failed: {e}")))?;

    let q = quality.round().clamp(0.0, 100.0) as i32;
    let ok = env
        .call_method(
            &bitmap,
            "compress",
            "(Landroid/graphics/Bitmap$CompressFormat;ILjava/io/OutputStream;)Z",
            &[
                JValue::Object(&jpeg_fmt),
                JValue::Int(q),
                JValue::Object(&baos),
            ],
        )
        .and_then(|v| v.z())
        .map_err(|e| Error::NativeError(format!("Bitmap.compress failed: {e}")))?;
    if !ok {
        return Err(Error::EncodeError("Bitmap.compress returned false".into()));
    }

    let out_arr = env
        .call_method(&baos, "toByteArray", "()[B", &[])
        .and_then(|v| v.l())
        .map_err(|e| Error::NativeError(format!("toByteArray failed: {e}")))?;

    let out_arr = JByteArray::from(out_arr);
    env.convert_byte_array(out_arr)
        .map_err(|e| Error::NativeError(format!("convert_byte_array failed: {e}")))
}
