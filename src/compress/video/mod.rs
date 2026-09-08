use crate::error::Error;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;

#[cfg(target_os = "android")]
mod android;

#[cfg(target_os = "windows")]
mod windows;

// Pure-Rust YUV helpers used by the Android path; compiled on Android and
// during `cargo test` (so its unit tests run on the host).
#[cfg(any(target_os = "android", test))]
mod yuv;

// Pure-Rust BGRA32 scaler used by the Windows path; compiled on Windows and
// during `cargo test` (so its unit tests run on the host).
#[cfg(any(target_os = "windows", test))]
mod rgb;

// ---------------------------------------------------------------------------
// Supported input video container formats
// ---------------------------------------------------------------------------

/// Supported input video container formats.
#[derive(Debug, Clone, PartialEq)]
pub enum VideoFormat {
    /// ISO Base Media File Format (mp4 / m4v / 3gp / …)
    Mp4,
    /// QuickTime movie
    QuickTime,
    /// WebM / Matroska (EBML)
    Webm,
    /// AVI (RIFF)
    Avi,
    /// MPEG-TS
    MpegTs,
}

impl VideoFormat {
    /// Attempt to detect the video container from magic bytes.
    pub fn detect(data: &[u8]) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }

        // ISO Base Media File Format (mp4 / mov / m4v / 3gp / …): "ftyp" box
        if &data[4..8] == b"ftyp" {
            let brand = &data[8..12];
            return match brand {
                b"qt  " => Some(Self::QuickTime),
                _ => Some(Self::Mp4),
            };
        }

        // WebM / Matroska (EBML magic)
        if data.starts_with(b"\x1a\x45\xdf\xa3") {
            return Some(Self::Webm);
        }

        // AVI (RIFF + "AVI ")
        if data.starts_with(b"RIFF") && &data[8..12] == b"AVI " {
            return Some(Self::Avi);
        }

        // MPEG-TS: 0x47 sync byte every 188 bytes
        if data[0] == 0x47 && data.len() >= 188 && data[188] == 0x47 {
            return Some(Self::MpegTs);
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct VideoCompressOptions {
    /// Target average video bitrate, in kilobits per second.
    pub bitrate_kbps: u32,
    /// Optional upper bound for output width. Shrinks only, preserving aspect ratio.
    pub max_width: Option<u32>,
    /// Optional upper bound for output height. Shrinks only, preserving aspect ratio.
    pub max_height: Option<u32>,
}

impl VideoCompressOptions {
    pub fn new(bitrate_kbps: u32) -> Self {
        Self {
            bitrate_kbps,
            max_width: None,
            max_height: None,
        }
    }
}

impl Default for VideoCompressOptions {
    fn default() -> Self {
        Self::new(2000)
    }
}

/// Compute target dimensions that fit within `max_width`/`max_height`,
/// preserving aspect ratio and never upscaling. Width/height are rounded down
/// to even values (H.264 requires even dimensions).
pub(crate) fn compute_video_target_dimensions(
    src_w: u32,
    src_h: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (src_w, src_h);
    }

    let (w, h) = if max_width.is_none() && max_height.is_none() {
        (src_w, src_h)
    } else {
        let scale_w = max_width.map_or(1.0, |m| m as f64 / src_w as f64);
        let scale_h = max_height.map_or(1.0, |m| m as f64 / src_h as f64);
        // Fit within the box: use the smaller scale factor, clamp to never upscale.
        let scale = scale_w.min(scale_h).clamp(0.0, 1.0);
        let w = (src_w as f64 * scale).round() as u32;
        let h = (src_h as f64 * scale).round() as u32;
        (w.max(1), h.max(1))
    };

    // Round down to even (>= 2) for H.264.
    ((w.max(2) / 2) * 2, (h.max(2) / 2) * 2)
}

/// Compress a video to H.264 in an MP4 container.
///
/// # Arguments
/// * `input`   — raw bytes of the source video
/// * `options` — `bitrate_kbps` target average video bitrate, plus optional
///   `max_width` / `max_height` upper bounds. Output preserves aspect ratio,
///   never upscales, and keeps audio re-encoded to AAC when present.
///
/// # Platform support
/// Implemented for macOS / iOS (AVFoundation), Android (MediaCodec) and
/// Windows (Media Foundation). Other platforms return
/// `Error::PlatformNotSupported`.
pub fn compress_video(input: &[u8], options: VideoCompressOptions) -> Result<Vec<u8>, Error> {
    VideoFormat::detect(input).ok_or_else(|| {
        Error::UnsupportedFormat("Cannot detect video format from magic bytes".into())
    })?;

    let compressed = compress_video_platform(input, options)?;

    if compressed.len() > input.len() {
        return Ok(input.to_vec());
    }
    Ok(compressed)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn compress_video_platform(input: &[u8], options: VideoCompressOptions) -> Result<Vec<u8>, Error> {
    apple::compress(input, options)
}

#[cfg(target_os = "android")]
fn compress_video_platform(input: &[u8], options: VideoCompressOptions) -> Result<Vec<u8>, Error> {
    android::compress(input, options)
}

#[cfg(target_os = "windows")]
fn compress_video_platform(input: &[u8], options: VideoCompressOptions) -> Result<Vec<u8>, Error> {
    windows::compress(input, options)
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "android",
    target_os = "windows"
)))]
fn compress_video_platform(_input: &[u8], _options: VideoCompressOptions) -> Result<Vec<u8>, Error> {
    Err(Error::PlatformNotSupported(
        "Video compression is currently only supported on macOS/iOS/Android/Windows".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::compute_video_target_dimensions;

    #[test]
    fn no_max_constraints_keep_original_dimensions() {
        assert_eq!(
            compute_video_target_dimensions(3840, 2160, None, None),
            (3840, 2160)
        );
    }

    #[test]
    fn only_shrink_never_upscale() {
        assert_eq!(
            compute_video_target_dimensions(800, 600, Some(1200), Some(1000)),
            (800, 600)
        );
    }

    #[test]
    fn fits_within_max_box_preserving_aspect_ratio() {
        assert_eq!(
            compute_video_target_dimensions(4000, 3000, Some(1280), Some(720)),
            (960, 720)
        );
    }

    #[test]
    fn single_max_dimension_still_preserves_aspect_ratio() {
        assert_eq!(
            compute_video_target_dimensions(4000, 2000, Some(1280), None),
            (1280, 640)
        );
    }

    #[test]
    fn rounds_down_to_even_dimensions() {
        assert_eq!(
            compute_video_target_dimensions(1921, 1081, None, None),
            (1920, 1080)
        );
    }
}
