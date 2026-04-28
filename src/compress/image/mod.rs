use crate::error::Error;

// Shared WebP encoding helpers (not needed on WASM which has no webp dependency)
#[cfg(not(target_arch = "wasm32"))]
mod webp_encode;

// Platform-specific decoder modules
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;

#[cfg(target_os = "android")]
mod android;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

// ---------------------------------------------------------------------------
// Supported input image formats (used for format detection and error messages)
// ---------------------------------------------------------------------------

/// Supported input image formats.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Webp,
    /// HEIC/HEIF — supported on iOS/macOS via ImageIO
    Heic,
    /// TIFF — supported on iOS/macOS/Windows via native APIs
    Tiff,
}

impl ImageFormat {
    /// Attempt to detect the image format from magic bytes.
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
            // HEIC/HEIF: ISO Base Media File Format ftyp box
            d if d.len() >= 12 && &d[4..8] == b"ftyp" => {
                let brand = &d[8..12];
                if matches!(
                    brand,
                    b"heic" | b"heix" | b"heim" | b"heis" | b"hevm" | b"hevs" | b"mif1" | b"msf1"
                ) {
                    Some(Self::Heic)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compress an image to lossy WebP.
///
/// # Arguments
/// * `input`   — raw bytes of the source image (any supported format)
/// * `format`  — optional format hint; if `None`, auto-detection is attempted
/// * `quality` — WebP lossy quality, 0–100 (e.g. 80 is a good default)
///
/// # Platform support
/// | Platform | Formats                                   |
/// |----------|-------------------------------------------|
/// | macOS / iOS | JPEG, PNG, GIF, BMP, HEIC, TIFF, WebP |
/// | Android (API 28+) | JPEG, PNG, GIF, BMP, WebP       |
/// | Windows  | JPEG, PNG, GIF, BMP, TIFF, WebP           |
/// | Web/WASM | not supported (decode on the JS side)     |
pub fn compress_image(
    input: &[u8],
    format: Option<ImageFormat>,
    quality: f32,
) -> Result<Vec<u8>, Error> {
    // Validate or detect format (used only for error messages if the platform
    // rejects the input; the native API handles actual format detection).
    let _fmt = match format {
        Some(f) => f,
        None => ImageFormat::detect(input).ok_or_else(|| {
            Error::UnsupportedFormat("Cannot detect image format from magic bytes".into())
        })?,
    };

    // Route to the platform implementation. Each platform's `compress()`
    // function receives the raw bytes and lets the OS API detect the format.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    return apple::compress(input, quality);

    #[cfg(target_os = "android")]
    return android::compress(input, quality);

    #[cfg(target_os = "windows")]
    return windows::compress(input, quality);

    #[cfg(target_arch = "wasm32")]
    return wasm::compress(input, quality);

    // Fallback for unsupported platforms (e.g. Linux desktop)
    #[allow(unreachable_code)]
    Err(Error::PlatformNotSupported(
        "Image compression is not supported on this platform".into(),
    ))
}
