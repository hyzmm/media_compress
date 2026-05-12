use crate::error::Error;

// Shared WebP encoding helpers (not needed on WASM which has no webp dependency)
#[cfg(not(target_arch = "wasm32"))]
mod webp_encode;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod resize;

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

    /// Returns `true` if the format is one that may already be well-compressed
    /// (WebP, JPEG, or PNG), meaning we should keep the original when the
    /// compressed output turns out to be larger.
    pub fn should_use_original_if_larger(&self) -> bool {
        matches!(self, Self::Webp | Self::Jpeg | Self::Png)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct CompressOptions {
    pub quality: f32,
    pub min_width: Option<u32>,
    pub min_height: Option<u32>,
}

impl CompressOptions {
    pub fn new(quality: f32) -> Self {
        Self {
            quality,
            min_width: None,
            min_height: None,
        }
    }
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self::new(75.0)
    }
}

pub(crate) fn compute_target_dimensions(
    src_w: u32,
    src_h: u32,
    min_width: Option<u32>,
    min_height: Option<u32>,
) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (src_w, src_h);
    }

    let req_w = min_width.map_or(0.0, |w| w as f64 / src_w as f64);
    let req_h = min_height.map_or(0.0, |h| h as f64 / src_h as f64);

    // Keep aspect ratio, shrink only. Never upscale.
    let scale = req_w.max(req_h).clamp(0.0, 1.0);
    if scale >= 1.0 {
        return (src_w, src_h);
    }

    let dst_w = ((src_w as f64 * scale).ceil() as u32).clamp(1, src_w);
    let dst_h = ((src_h as f64 * scale).ceil() as u32).clamp(1, src_h);
    (dst_w, dst_h)
}

/// Compress an image to lossy WebP.
///
/// # Arguments
/// * `input`   — raw bytes of the source image (any supported format)
/// * `options` — compression options:
///   - `quality`: WebP lossy quality, 0–100
///   - `min_width` / `min_height`: optional lower bound for output size.
///     Compression may downscale while preserving aspect ratio, but will not
///     upscale when the source image is already smaller.
///
/// # Platform support
/// | Platform | Formats                                   |
/// |----------|-------------------------------------------|
/// | macOS / iOS | JPEG, PNG, GIF, BMP, HEIC, TIFF, WebP |
/// | Android (API 24+) | JPEG, PNG, GIF, BMP, WebP, TIFF |
/// | Windows  | JPEG, PNG, GIF, BMP, TIFF, WebP           |
/// | Web/WASM | not supported (decode on the JS side)     |
pub fn compress_image(input: &[u8], options: CompressOptions) -> Result<Vec<u8>, Error> {
    // Validate or detect format (used only for error messages if the platform
    // rejects the input; the native API handles actual format detection).
    let fmt = ImageFormat::detect(input).ok_or_else(|| {
        Error::UnsupportedFormat("Cannot detect image format from magic bytes".into())
    })?;

    // Whether the source format is one of the common lossy/lossless formats
    // that may already be well-compressed. If the output ends up larger than
    // the input we fall back to the original bytes.
    let may_fallback = fmt.should_use_original_if_larger();

    // Route to the platform implementation. Each platform's `compress()`
    // function receives the raw bytes and lets the OS API detect the format.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let compressed = apple::compress(input, options)?;

    #[cfg(target_os = "android")]
    let compressed = android::compress(input, options)?;

    #[cfg(target_os = "windows")]
    let compressed = windows::compress(input, options)?;

    #[cfg(target_arch = "wasm32")]
    let compressed = wasm::compress(input, options)?;

    if may_fallback && compressed.len() > input.len() {
        return Ok(input.to_vec());
    }
    return Ok(compressed);
}

#[cfg(test)]
mod tests {
    use super::compute_target_dimensions;

    #[test]
    fn only_shrink_never_upscale() {
        assert_eq!(
            compute_target_dimensions(800, 600, Some(1200), Some(1000)),
            (800, 600)
        );
    }

    #[test]
    fn min_width_keeps_aspect_ratio() {
        assert_eq!(
            compute_target_dimensions(4000, 2000, Some(1000), None),
            (1000, 500)
        );
    }

    #[test]
    fn min_height_keeps_aspect_ratio() {
        assert_eq!(
            compute_target_dimensions(3000, 2000, None, Some(800)),
            (1200, 800)
        );
    }

    #[test]
    fn both_mins_choose_stricter_scale() {
        assert_eq!(
            compute_target_dimensions(4000, 3000, Some(1000), Some(1200)),
            (1600, 1200)
        );
    }
}
