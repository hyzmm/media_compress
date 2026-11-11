use std::path::Path;

use crate::error::Error;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;

#[cfg(target_os = "android")]
mod android;

#[cfg(target_os = "windows")]
mod windows;

// Pure-Rust transform helpers used by the Apple path; compiled on the Apple
// platforms and during `cargo test` so the orientation math runs on the host.
#[cfg(any(target_os = "macos", target_os = "ios", test))]
mod transform;

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

/// Default compression quality (x264-style CRF).
pub(crate) const DEFAULT_CRF: f32 = 28.0;

/// Minimum / maximum x264-style CRF.
pub(crate) const MIN_CRF: f32 = 0.0;
pub(crate) const MAX_CRF: f32 = 51.0;

#[derive(Debug, Clone, Copy)]
pub struct VideoCompressOptions {
    /// x264-style CRF, 0–51. Lower keeps more quality, higher compresses more.
    pub crf: f32,
    /// Reserved for future downscaling. Ignored by the compression path for
    /// now: output keeps the source dimensions.
    pub max_width: Option<u32>,
    /// Reserved for future downscaling. Ignored by the compression path for
    /// now: output keeps the source dimensions.
    pub max_height: Option<u32>,
}

impl VideoCompressOptions {
    pub fn new(crf: f32) -> Self {
        Self {
            crf,
            max_width: None,
            max_height: None,
        }
    }
}

impl Default for VideoCompressOptions {
    fn default() -> Self {
        Self::new(DEFAULT_CRF)
    }
}

/// H.264 encode target shared by the platform backends.
pub(crate) struct H264Target {
    /// Normalized x264-style CRF.
    pub(crate) crf: f32,
}

impl H264Target {
    /// CRF compression target. `max_width`/`max_height` are deliberately not
    /// applied yet: the output keeps the source dimensions.
    pub(crate) fn compress(options: VideoCompressOptions) -> Self {
        Self {
            crf: normalized_crf(options.crf),
        }
    }

    /// CRF-equivalent average bitrate for encoders that only take a bitrate.
    /// Calibrated against x264 on natural video: CRF 24 ≈ 0.085 bit per pixel
    /// per frame, roughly halving every 5 CRF steps.
    #[cfg_attr(
        not(any(target_os = "android", target_os = "windows")),
        allow(dead_code)
    )]
    pub(crate) fn crf_fallback_bitrate_bps(width: u32, height: u32, fps: u32, crf: f32) -> u32 {
        let pixels_per_second = (width.max(2) as u64) * (height.max(2) as u64) * fps.max(1) as u64;
        let bpp = 0.085 * 2f64.powf((24.0 - normalized_crf(crf) as f64) / 5.0);
        (pixels_per_second as f64 * bpp).clamp(100_000.0, 100_000_000.0) as u32
    }
}

/// Clamp a user-supplied CRF to the x264 range; non-finite values fall back to
/// the default.
fn normalized_crf(crf: f32) -> f32 {
    if crf.is_finite() {
        crf.clamp(MIN_CRF, MAX_CRF)
    } else {
        DEFAULT_CRF
    }
}

/// Map x264 CRF to a normalized quality in `0.0..=1.0` (1.0 = best).
pub(crate) fn crf_to_quality_unit(crf: f32) -> f32 {
    1.0 - normalized_crf(crf) / MAX_CRF
}

/// Map x264 CRF to the `1..=100` quality scale used by MediaCodec CQ.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn crf_to_quality_100(crf: f32) -> i32 {
    (crf_to_quality_unit(crf) * 100.0).round().clamp(1.0, 100.0) as i32
}

/// Fit the source dimensions to H.264: round down to even values, never
/// resize. H.264 requires even dimensions.
pub(crate) fn compute_video_target_dimensions(src_w: u32, src_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (src_w, src_h);
    }

    // Round down to even (>= 2) for H.264.
    ((src_w.max(2) / 2) * 2, (src_h.max(2) / 2) * 2)
}

/// Compress a video file directly from `input` to `output`.
///
/// The input and output files never go through memory: the platform backend
/// reads and writes them itself.
///
/// Returns `Ok(())` once `output` has been written. The re-encode is kept even
/// when it ends up larger than `input` — the caller should use `output` as the
/// result. `output` is only removed when encoding fails.
pub fn compress_video_file(
    input: &Path,
    output: &Path,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "windows"
    )))]
    {
        let _ = (input, output, options);
        return Err(Error::PlatformNotSupported(
            "Video compression is currently only supported on macOS/iOS/Android/Windows".into(),
        ));
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "windows"
    ))]
    {
        let header = read_header(input)?;
        VideoFormat::detect(&header).ok_or_else(|| {
            Error::UnsupportedFormat("Cannot detect video format from magic bytes".into())
        })?;

        compress_video_file_platform(input, output, options).inspect_err(|_| {
            let _ = std::fs::remove_file(output);
        })
    }
}

/// Read just enough of the file to run [`VideoFormat::detect`] (MPEG-TS needs
/// byte 188).
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "android",
    target_os = "windows"
))]
fn read_header(path: &Path) -> Result<Vec<u8>, Error> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|e| Error::DecodeError(format!("failed to open input video: {e}")))?;
    let mut header = vec![0u8; 189];
    let len = file
        .read(&mut header)
        .map_err(|e| Error::DecodeError(format!("failed to read input video header: {e}")))?;
    header.truncate(len);

    Ok(header)
}

/// Compress a video to H.264 in an MP4 container.
///
/// # Arguments
/// * `input`   — raw bytes of the source video
/// * `options` — `crf` quality target (x264-style 0–51). `max_width` /
///   `max_height` are reserved and not applied yet; output keeps the source
///   dimensions. Audio is re-encoded to AAC when present.
///
/// The re-encode is returned even when it is larger than `input`; callers that
/// need a size guarantee should decide after comparing lengths themselves.
///
/// # Platform support
/// Implemented for macOS / iOS (AVFoundation), Android (MediaCodec) and
/// Windows (Media Foundation). Other platforms return
/// `Error::PlatformNotSupported`.
pub fn compress_video(input: &[u8], options: VideoCompressOptions) -> Result<Vec<u8>, Error> {
    VideoFormat::detect(input).ok_or_else(|| {
        Error::UnsupportedFormat("Cannot detect video format from magic bytes".into())
    })?;

    compress_video_platform(input, options)
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
fn compress_video_platform(
    _input: &[u8],
    _options: VideoCompressOptions,
) -> Result<Vec<u8>, Error> {
    Err(Error::PlatformNotSupported(
        "Video compression is currently only supported on macOS/iOS/Android/Windows".into(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn compress_video_file_platform(
    input: &Path,
    output: &Path,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    apple::compress_file(input, output, options)
}

#[cfg(target_os = "android")]
fn compress_video_file_platform(
    input: &Path,
    output: &Path,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    android::compress_file(input, output, options)
}

#[cfg(target_os = "windows")]
fn compress_video_file_platform(
    input: &Path,
    output: &Path,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    windows::compress_file(input, output, options)
}

#[cfg(test)]
mod tests {
    use super::{
        compute_video_target_dimensions, crf_to_quality_100, crf_to_quality_unit, H264Target,
        VideoCompressOptions, DEFAULT_CRF,
    };

    #[test]
    fn keeps_source_dimensions() {
        assert_eq!(compute_video_target_dimensions(3840, 2160), (3840, 2160));
        assert_eq!(compute_video_target_dimensions(800, 600), (800, 600));
    }

    #[test]
    fn rounds_down_to_even_dimensions() {
        assert_eq!(compute_video_target_dimensions(1921, 1081), (1920, 1080));
    }

    #[test]
    fn compression_ignores_size_limits_and_keeps_source_dimensions() {
        let target = H264Target::compress(VideoCompressOptions {
            crf: 28.0,
            max_width: Some(640),
            max_height: Some(360),
        });

        assert_eq!(target.crf, 28.0);
        assert_eq!(compute_video_target_dimensions(3840, 2160), (3840, 2160));
    }

    #[test]
    fn crf_is_clamped_and_non_finite_uses_default() {
        assert_eq!(
            H264Target::compress(VideoCompressOptions::new(80.0)).crf,
            51.0
        );
        assert_eq!(
            H264Target::compress(VideoCompressOptions::new(-1.0)).crf,
            0.0
        );
        assert_eq!(
            H264Target::compress(VideoCompressOptions::new(f32::NAN)).crf,
            DEFAULT_CRF
        );
    }

    #[test]
    fn crf_maps_to_platform_quality_scales() {
        assert_eq!(crf_to_quality_unit(0.0), 1.0);
        assert!((crf_to_quality_unit(25.5) - 0.5).abs() < 1e-6);
        assert_eq!(crf_to_quality_unit(51.0), 0.0);

        assert_eq!(crf_to_quality_100(0.0), 100);
        assert_eq!(crf_to_quality_100(25.5), 50);
        assert_eq!(crf_to_quality_100(51.0), 1);
    }

    #[test]
    fn crf_fallback_bitrate_shrinks_as_crf_rises() {
        let at_24 = H264Target::crf_fallback_bitrate_bps(1920, 1080, 30, 24.0);
        let at_28 = H264Target::crf_fallback_bitrate_bps(1920, 1080, 30, 28.0);
        let at_35 = H264Target::crf_fallback_bitrate_bps(1920, 1080, 30, 35.0);

        assert!(at_24 > at_28 && at_28 > at_35);
        assert_eq!(H264Target::crf_fallback_bitrate_bps(2, 2, 1, 51.0), 100_000);

        let target = H264Target::compress(VideoCompressOptions::new(28.0));
        assert_eq!(
            H264Target::crf_fallback_bitrate_bps(1920, 1080, 30, target.crf),
            at_28
        );
    }
}
