mod a_image_decoder;
mod jni_bitmap_factory;

use crate::compress::image::CompressOptions;
use crate::error::Error;

use jni::objects::JValue;
use jni::JavaVM;

/// Compress an image. Tries AImageDecoder first (API 28+), falls back to
/// JNI BitmapFactory on failure.
pub fn compress(input: &[u8], options: CompressOptions) -> Result<Vec<u8>, Error> {
    a_image_decoder::compress(input, options)
        .or_else(|_| jni_bitmap_factory::compress(input, options))
}

/// JNI-based orientation retrieval for jni_bitmap_factory.
pub(super) fn orientation_from_metadata_jni(input: &[u8]) -> u32 {
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
