mod gif;
mod native;
mod static_img;

use crate::error::Error;

/// Supported input image formats.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Webp,
    /// HEIC/HEIF — requires native API (iOS/macOS only)
    Heic,
    /// TIFF — requires native API (iOS/macOS only)
    Tiff,
}

impl ImageFormat {
    /// Attempt to detect format from magic bytes.
    pub fn detect(data: &[u8]) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }
        match data {
            d if d.starts_with(b"\xff\xd8\xff") => Some(Self::Jpeg),
            d if d.starts_with(b"\x89PNG\r\n\x1a\n") => Some(Self::Png),
            d if d.starts_with(b"GIF87a") || d.starts_with(b"GIF89a") => Some(Self::Gif),
            d if d.starts_with(b"BM") => Some(Self::Bmp),
            d if d.len() >= 12 && &d[8..12] == b"WEBP" => Some(Self::Webp),
            d if d.starts_with(b"II\x2a\x00") || d.starts_with(b"MM\x00\x2a") => Some(Self::Tiff),
            // HEIC: ftyp box with heic/heix/mif1 brand
            d if d.len() >= 12 && (&d[4..8] == b"ftyp") => {
                let brand = &d[8..12];
                if brand == b"heic" || brand == b"heix" || brand == b"mif1" || brand == b"msf1" {
                    Some(Self::Heic)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Compress `input` image bytes to WebP.
///
/// - `format`: hint for the input format. If `None`, auto-detection is attempted.
/// - `quality`: WebP quality 0–100 (lossy). Use 100 for lossless-like, 80 for good balance.
///
/// Returns the compressed WebP bytes.
pub fn compress_image(
    input: &[u8],
    format: Option<ImageFormat>,
    quality: f32,
) -> Result<Vec<u8>, Error> {
    let fmt = match format {
        Some(f) => f,
        None => ImageFormat::detect(input)
            .ok_or_else(|| Error::UnsupportedFormat("Cannot detect image format".into()))?,
    };

    match fmt {
        ImageFormat::Gif => gif::compress_gif_to_webp(input, quality),
        ImageFormat::Heic | ImageFormat::Tiff => native::decode_native(input, &fmt, quality),
        _ => static_img::compress_static(input, quality),
    }
}
