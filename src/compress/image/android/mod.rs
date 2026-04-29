mod a_image_decoder;
mod jni_bitmap_factory;

use crate::error::Error;

/// Compress an image. Tries AImageDecoder first (API 28+), falls back to
/// JNI BitmapFactory on failure.
pub fn compress(input: &[u8], quality: f32) -> Result<Vec<u8>, Error> {
    a_image_decoder::compress(input, quality)
        .or_else(|_| jni_bitmap_factory::compress(input, quality))
}
