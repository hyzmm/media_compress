use crate::error::Error;
use image::DynamicImage;
use webp::Encoder;

pub fn compress_static(input: &[u8], quality: f32) -> Result<Vec<u8>, Error> {
    let img = image::load_from_memory(input)
        .map_err(|e| Error::DecodeError(e.to_string()))?;
    encode_to_webp(&img, quality)
}

pub fn encode_to_webp(img: &DynamicImage, quality: f32) -> Result<Vec<u8>, Error> {
    let encoder = Encoder::from_image(img)
        .map_err(|e| Error::EncodeError(e.to_string()))?;
    let encoded = encoder.encode(quality);
    Ok(encoded.to_vec())
}
