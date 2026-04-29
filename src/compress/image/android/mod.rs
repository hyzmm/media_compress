mod a_image_decoder;
mod jni_bitmap_factory;

mod a_image_decoder;
mod jni_bitmap_factory;

use crate::error::Error;
use jni::JNIEnv;

/// 需要传入 JNIEnv，自动判断 SDK 版本，>=30 用 a_image_decoder，否则用 jni_bitmap_factory。
pub fn compress(env: &JNIEnv, input: &[u8], quality: f32) -> Result<Vec<u8>, Error> {
    let sdk_int = crate::compress::image::android::jni_bitmap_factory::get_sdk_int(env)?;
    if sdk_int >= 30 {
        a_image_decoder::compress(input, quality)
    } else {
        jni_bitmap_factory::compress(input, quality)
    }
}
