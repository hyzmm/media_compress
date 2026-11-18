use crate::error::Error;

// Shared WebP encoding helpers (not needed on WASM which has no webp dependency)
#[cfg(not(target_arch = "wasm32"))]
mod webp_encode;

#[cfg(not(target_arch = "wasm32"))]
mod orientation;

// Apple and Android decoders return premultiplied RGBA; WebP expects straight
// alpha. Windows WIC already provides straight alpha. Compiled under `test` so
// the conversion itself also runs on non-Apple/non-Android hosts.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "android",
    test
))]
mod alpha;

// Error/fallback policy for the Android decoders, compiled under `test` so its
// pure decision logic runs on the host.
#[cfg(any(target_os = "android", test))]
mod android_fallback;

// GIF decode + re-encode (zengif); used on every platform, native and wasm.
pub(crate) mod gif;

pub(crate) mod resize;

// Platform-specific decoder modules
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;

#[cfg(target_os = "android")]
pub mod android;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

// ---------------------------------------------------------------------------
// Supported input image formats (used for format detection and error messages)
// ---------------------------------------------------------------------------

/// Input image formats recognised from magic bytes.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Gif,
    Bmp,
    /// WebP — detected, but rejected as compression input: it already is the
    /// format this library outputs.
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
                    b"heic"
                        | b"heix"
                        | b"hevc"
                        | b"hevx"
                        | b"heim"
                        | b"heis"
                        | b"hevm"
                        | b"hevs"
                        | b"mif1"
                        | b"msf1"
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
    /// (JPEG, PNG, or GIF), meaning we should keep the original when the
    /// compressed output turns out to be larger.
    pub fn should_use_original_if_larger(&self) -> bool {
        matches!(self, Self::Jpeg | Self::Png | Self::Gif)
    }

    /// MIME type used when handing the source bytes to a browser API.
    ///
    /// `createImageBitmap` accepts a `Blob`, and some browsers only decode
    /// formats such as HEIC when the blob declares the matching type instead
    /// of relying on content sniffing.
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn mime_type(&self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Bmp => "image/bmp",
            Self::Webp => "image/webp",
            Self::Heic => "image/heic",
            Self::Tiff => "image/tiff",
        }
    }
}

/// Error text shared by the native and Web entry points when the input is
/// already WebP. Re-encoding it would only add another lossy generation.
pub(crate) const WEBP_INPUT_REJECTED: &str =
    "WebP input is not supported (already the output format)";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Quality used when the caller supplies a non-finite value.
pub(crate) const DEFAULT_QUALITY: f32 = 75.0;

/// Clamp a caller-supplied WebP quality to libwebp's `0..=100` range.
/// Non-finite values fall back to [`DEFAULT_QUALITY`] so they never reach the
/// C encoder or the browser canvas API.
pub(crate) fn normalize_quality(quality: f32) -> f32 {
    if quality.is_finite() {
        quality.clamp(0.0, 100.0)
    } else {
        DEFAULT_QUALITY
    }
}

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
        Self::new(DEFAULT_QUALITY)
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

    if min_width.is_none() && min_height.is_none() {
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
/// WebP input is rejected with [`Error::UnsupportedFormat`]: it already is the
/// output format, so re-encoding it would only add another lossy generation.
/// GIF input is decoded and re-encoded as GIF (zengif + the quantette
/// quantizer): per-frame delays and the loop count are preserved as-is.
/// Other animated formats (APNG) are reduced to their first frame.
///
/// # Arguments
/// * `input`   — raw bytes of the source image (any supported format)
/// * `options` — compression options:
///   - `quality`: lossy quality, 0–100 (drives the WebP encoder; the GIF
///     quantizer currently ignores it). Values outside the range are clamped;
///     non-finite values use the default 75.
///   - `min_width` / `min_height`: optional lower bound for output size.
///     Compression may downscale while preserving aspect ratio, but will not
///     upscale when the source image is already smaller.
///
/// # Platform support
/// | Platform | Formats                                   |
/// |----------|-------------------------------------------|
/// | macOS / iOS | JPEG, PNG, GIF (→ GIF), BMP, HEIC, TIFF |
/// | Android (API 24+) | JPEG, PNG, GIF (→ GIF), BMP, TIFF |
/// | Windows  | JPEG, PNG, GIF (→ GIF), BMP, TIFF |
/// | Web/WASM | not supported; use `compress_image_js` instead |
pub fn compress_image(input: &[u8], options: CompressOptions) -> Result<Vec<u8>, Error> {
    // Clamp before any backend sees it: libwebp only accepts 0–100, and
    // non-finite floats have no meaningful interpretation.
    let options = CompressOptions {
        quality: normalize_quality(options.quality),
        min_width: options.min_width,
        min_height: options.min_height,
    };

    // Validate or detect format (used only for error messages if the platform
    // rejects the input; the native API handles actual format detection).
    let fmt = ImageFormat::detect(input).ok_or_else(|| {
        Error::UnsupportedFormat("Cannot detect image format from magic bytes".into())
    })?;

    if fmt == ImageFormat::Webp {
        return Err(Error::UnsupportedFormat(WEBP_INPUT_REJECTED.into()));
    }

    // GIF is handled by the shared zengif codec on every platform; the
    // platform decoders below never see GIF input.
    if fmt == ImageFormat::Gif {
        let compressed = gif::compress_gif(input, options)?;
        return Ok(if compressed.len() > input.len() {
            input.to_vec()
        } else {
            compressed
        });
    }

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
    Ok(compressed)
}

#[cfg(test)]
mod tests {
    use super::{compute_target_dimensions, normalize_quality, ImageFormat, DEFAULT_QUALITY};

    fn ftyp_brand(brand: &[u8; 4]) -> Vec<u8> {
        let mut data = vec![0, 0, 0, 12, b'f', b't', b'y', b'p'];
        data.extend_from_slice(brand);
        data
    }

    #[test]
    fn detects_all_heif_major_brands() {
        for brand in [
            b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs", b"mif1",
            b"msf1",
        ] {
            assert_eq!(
                ImageFormat::detect(&ftyp_brand(brand)),
                Some(ImageFormat::Heic),
                "brand {} should be detected as HEIC",
                std::str::from_utf8(brand).unwrap()
            );
        }
    }

    #[test]
    fn mp4_brand_is_not_treated_as_heic_image() {
        assert_eq!(ImageFormat::detect(&ftyp_brand(b"mp42")), None);
        assert_eq!(ImageFormat::detect(&ftyp_brand(b"qt  ")), None);
    }

    #[test]
    fn detected_formats_map_to_browser_mime_types() {
        for (format, mime) in [
            (ImageFormat::Jpeg, "image/jpeg"),
            (ImageFormat::Png, "image/png"),
            (ImageFormat::Gif, "image/gif"),
            (ImageFormat::Bmp, "image/bmp"),
            (ImageFormat::Webp, "image/webp"),
            (ImageFormat::Heic, "image/heic"),
            (ImageFormat::Tiff, "image/tiff"),
        ] {
            assert_eq!(format.mime_type(), mime);
        }
    }

    #[test]
    fn quality_is_clamped_and_non_finite_falls_back() {
        assert_eq!(normalize_quality(75.0), 75.0);
        assert_eq!(normalize_quality(-20.0), 0.0);
        assert_eq!(normalize_quality(120.0), 100.0);
        assert_eq!(normalize_quality(f32::NAN), DEFAULT_QUALITY);
        assert_eq!(normalize_quality(f32::INFINITY), DEFAULT_QUALITY);
        assert_eq!(normalize_quality(f32::NEG_INFINITY), DEFAULT_QUALITY);
    }

    #[test]
    fn no_min_constraints_keep_original_dimensions() {
        assert_eq!(
            compute_target_dimensions(3456, 4608, None, None),
            (3456, 4608)
        );
    }

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
