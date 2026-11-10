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

/// Size limits for [`convert_video_file`].
#[derive(Debug, Clone, Copy, Default)]
pub struct VideoConvertOptions {
    /// Optional upper bound for output width. Shrinks only, preserving aspect ratio.
    pub max_width: Option<u32>,
    /// Optional upper bound for output height. Shrinks only, preserving aspect ratio.
    pub max_height: Option<u32>,
}

impl VideoConvertOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_size(max_width: u32, max_height: u32) -> Self {
        Self {
            max_width: Some(max_width),
            max_height: Some(max_height),
        }
    }
}

/// Video codecs the platform probes recognise on the source track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
}

/// Outcome of [`convert_video_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoConvertOutcome {
    /// The video track already uses H.264 within the size limits; no output was
    /// written. Always carries [`VideoCodec::H264`].
    AlreadyCompliant(VideoCodec),
    /// The source was transcoded to H.264/MP4 at the output path.
    Converted,
}

/// How the H.264 encoder picks its rate/quality target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RateControl {
    /// x264-style CRF: quality-driven, the bitrate is a result.
    Crf(f32),
    /// Format conversion: keep the source bitrate (size-capped when downscaled).
    SourceBitrate,
}

/// H.264 encode target shared by the platform backends.
pub(crate) struct H264Target {
    pub(crate) max_width: Option<u32>,
    pub(crate) max_height: Option<u32>,
    pub(crate) rate_control: RateControl,
}

impl H264Target {
    /// CRF compression target. `max_width`/`max_height` are deliberately not
    /// applied yet: the output keeps the source dimensions.
    pub(crate) fn compress(options: VideoCompressOptions) -> Self {
        Self {
            max_width: None,
            max_height: None,
            rate_control: RateControl::Crf(normalized_crf(options.crf)),
        }
    }

    /// Format-conversion target: source resolution and source bitrate, with the
    /// optional size limit from [`VideoConvertOptions`].
    pub(crate) fn convert(options: VideoConvertOptions) -> Self {
        Self {
            max_width: options.max_width,
            max_height: options.max_height,
            rate_control: RateControl::SourceBitrate,
        }
    }

    /// CRF when this target is quality-driven.
    pub(crate) fn crf(&self) -> Option<f32> {
        match self.rate_control {
            RateControl::Crf(crf) => Some(crf),
            RateControl::SourceBitrate => None,
        }
    }

    /// Bitrate for a conversion re-encode, or a CRF-derived fallback for
    /// platform encoders that only accept an average bitrate.
    ///
    /// Conversions keep the source bitrate when the frame size is unchanged.
    /// When the frame is downscaled the source bitrate belongs to a larger
    /// picture, so it is capped at what the target resolution needs (~0.1 bit
    /// per pixel per frame) instead of ballooning the smaller output.
    pub(crate) fn resolve_bitrate_bps(
        &self,
        source_bitrate_bps: Option<u32>,
        source_size: (u32, u32),
        target_size: (u32, u32),
        fps: u32,
    ) -> u32 {
        if let RateControl::Crf(crf) = self.rate_control {
            return Self::crf_fallback_bitrate_bps(target_size.0, target_size.1, fps, crf);
        }

        let estimated = Self::fallback_bitrate_bps(target_size.0, target_size.1, fps);
        match source_bitrate_bps.filter(|value| *value > 0) {
            Some(source) if target_size == source_size => source,
            Some(source) => source.min(estimated),
            None => estimated,
        }
    }

    /// Target bitrate when the source track does not report one: ~0.1 bit per
    /// pixel per frame, clamped to a range that keeps small clips from looking
    /// starved and huge frames from exploding.
    pub(crate) fn fallback_bitrate_bps(width: u32, height: u32, fps: u32) -> u32 {
        let pixels_per_second = (width.max(2) as u64) * (height.max(2) as u64) * fps.max(1) as u64;
        (pixels_per_second as f64 * 0.1).clamp(1_000_000.0, 100_000_000.0) as u32
    }

    /// CRF-equivalent average bitrate for encoders that only take a bitrate.
    /// Calibrated against x264 on natural video: CRF 24 ≈ 0.085 bit per pixel
    /// per frame, roughly halving every 5 CRF steps.
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

/// Whether `width x height` already fits the optional limits, so a compliant
/// codec needs no re-encode.
pub(crate) fn within_size_limits(width: u32, height: u32, options: VideoConvertOptions) -> bool {
    options.max_width.map_or(true, |max| width <= max)
        && options.max_height.map_or(true, |max| height <= max)
}

/// The probed codec when the source can be kept as-is, `None` when it must be
/// transcoded to H.264.
///
/// Only H.264 within the size limits is kept. HEVC is deliberately not
/// accepted — every output of [`convert_video_file`] is H.264/MP4, so an HEVC
/// source is re-encoded like any other codec. The container is not part of the
/// decision: an H.264 track in a MOV/MP4 within the limits is left untouched.
pub(crate) fn compliant_codec(
    codec: Option<VideoCodec>,
    width: u32,
    height: u32,
    options: VideoConvertOptions,
) -> Option<VideoCodec> {
    match codec {
        Some(VideoCodec::H264) if within_size_limits(width, height, options) => {
            Some(VideoCodec::H264)
        }
        _ => None,
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

/// Compress a video file directly from `input` to `output`.
///
/// The input and output files never go through memory: the platform backend
/// reads and writes them itself.
///
/// Returns `Ok(true)` when `output` contains a smaller re-encode. Returns
/// `Ok(false)` when the re-encode was not smaller, in which case `output` has
/// been removed and the caller should keep the original file.
pub fn compress_video_file(
    input: &Path,
    output: &Path,
    options: VideoCompressOptions,
) -> Result<bool, Error> {
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

        let input_len = std::fs::metadata(input)
            .map_err(|e| Error::DecodeError(format!("failed to stat input video: {e}")))?
            .len();

        match compress_video_file_platform(input, output, options) {
            Ok(()) => {
                let output_len = std::fs::metadata(output)
                    .map_err(|e| Error::EncodeError(format!("failed to stat output video: {e}")))?
                    .len();
                if output_len >= input_len {
                    let _ = std::fs::remove_file(output);
                    Ok(false)
                } else {
                    Ok(true)
                }
            }
            Err(e) => {
                let _ = std::fs::remove_file(output);
                Err(e)
            }
        }
    }
}

/// Convert a video file to H.264/MP4 unless its video track is already H.264
/// within the size limits.
///
/// HEVC is transcoded like any other codec: H.264 is the only encoding kept
/// as-is.
///
/// This is a codec normalization step, not a size-driven compression: the
/// output keeps the source resolution, capped by
/// [`VideoConvertOptions::max_width`] / [`max_height`], and targets the source
/// video bitrate (falling back to a high-quality estimate when the container
/// reports none). A downscaled output caps the bitrate at what the target
/// resolution needs.
///
/// Returns [`VideoConvertOutcome::AlreadyCompliant`] when no conversion is
/// needed; in that case `output` is not created and the caller should keep the
/// original file. On failure a partially written `output` is removed.
///
/// The input and output files never go through memory: the platform backend
/// reads and writes them itself.
pub fn convert_video_file(
    input: &Path,
    output: &Path,
    options: VideoConvertOptions,
) -> Result<VideoConvertOutcome, Error> {
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "windows"
    )))]
    {
        let _ = (input, output, options);
        return Err(Error::PlatformNotSupported(
            "Video conversion is currently only supported on macOS/iOS/Android/Windows".into(),
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

        match convert_video_file_platform(input, output, options) {
            Ok(outcome) => Ok(outcome),
            Err(e) => {
                let _ = std::fs::remove_file(output);
                Err(e)
            }
        }
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

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn convert_video_file_platform(
    input: &Path,
    output: &Path,
    options: VideoConvertOptions,
) -> Result<VideoConvertOutcome, Error> {
    apple::convert_file(input, output, options)
}

#[cfg(target_os = "android")]
fn convert_video_file_platform(
    input: &Path,
    output: &Path,
    options: VideoConvertOptions,
) -> Result<VideoConvertOutcome, Error> {
    android::convert_file(input, output, options)
}

#[cfg(target_os = "windows")]
fn convert_video_file_platform(
    input: &Path,
    output: &Path,
    options: VideoConvertOptions,
) -> Result<VideoConvertOutcome, Error> {
    windows::convert_file(input, output, options)
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "android",
    target_os = "windows"
)))]
fn convert_video_file_platform(
    _input: &Path,
    _output: &Path,
    _options: VideoConvertOptions,
) -> Result<VideoConvertOutcome, Error> {
    Err(Error::PlatformNotSupported(
        "Video conversion is currently only supported on macOS/iOS/Android/Windows".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        compliant_codec, compute_video_target_dimensions, crf_to_quality_100, crf_to_quality_unit,
        within_size_limits, H264Target, VideoCodec, VideoCompressOptions, VideoConvertOptions,
        DEFAULT_CRF,
    };

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

    #[test]
    fn fallback_bitrate_scales_with_pixels_per_second() {
        // 1080p30 ≈ 0.1 bit/pixel/frame
        assert_eq!(H264Target::fallback_bitrate_bps(1920, 1080, 30), 6_220_800);
    }

    #[test]
    fn fallback_bitrate_is_clamped_at_both_ends() {
        assert_eq!(H264Target::fallback_bitrate_bps(2, 2, 1), 1_000_000);
        assert_eq!(
            H264Target::fallback_bitrate_bps(7680, 4320, 60),
            100_000_000
        );
    }

    #[test]
    fn size_limits_decide_whether_compliant_input_is_kept() {
        let limited = VideoConvertOptions::with_max_size(1920, 1080);

        assert!(within_size_limits(1920, 1080, limited));
        assert!(within_size_limits(1280, 720, limited));
        assert!(!within_size_limits(3840, 2160, limited));
        // 竖屏 1080x1920 超出高度上限
        assert!(!within_size_limits(1080, 1920, limited));
        // 未设置上限时任何尺寸都算合规
        assert!(within_size_limits(7680, 4320, VideoConvertOptions::new()));
    }

    #[test]
    fn only_h264_is_kept_and_hevc_is_transcoded() {
        let unlimited = VideoConvertOptions::new();

        assert_eq!(
            compliant_codec(Some(VideoCodec::H264), 1920, 1080, unlimited),
            Some(VideoCodec::H264)
        );
        // HEVC 不再免转码，无论尺寸
        assert_eq!(
            compliant_codec(Some(VideoCodec::Hevc), 1920, 1080, unlimited),
            None
        );
        assert_eq!(
            compliant_codec(Some(VideoCodec::Hevc), 640, 360, unlimited),
            None
        );
        // 探测不出编码时同样走转码
        assert_eq!(compliant_codec(None, 640, 360, unlimited), None);
    }

    #[test]
    fn h264_over_the_size_limit_is_transcoded() {
        let limited = VideoConvertOptions::with_max_size(1920, 1080);

        assert_eq!(
            compliant_codec(Some(VideoCodec::H264), 3840, 2160, limited),
            None
        );
        assert_eq!(
            compliant_codec(Some(VideoCodec::H264), 1920, 1080, limited),
            Some(VideoCodec::H264)
        );
    }

    #[test]
    fn conversion_bitrate_keeps_source_when_size_is_unchanged() {
        let target = H264Target::convert(VideoConvertOptions::new());

        assert_eq!(
            target.resolve_bitrate_bps(Some(8_000_000), (1920, 1080), (1920, 1080), 30),
            8_000_000
        );
        // 源未报告码率时退回分辨率估算
        assert_eq!(
            target.resolve_bitrate_bps(None, (1920, 1080), (1920, 1080), 30),
            6_220_800
        );
    }

    #[test]
    fn conversion_bitrate_is_capped_after_downscale() {
        let target = H264Target::convert(VideoConvertOptions::with_max_size(1920, 1080));

        // 4K 40 Mbps 缩到 1080p30 后按目标分辨率估算封顶
        assert_eq!(
            target.resolve_bitrate_bps(Some(40_000_000), (3840, 2160), (1920, 1080), 30),
            6_220_800
        );
    }

    #[test]
    fn compression_ignores_size_limits_and_keeps_source_dimensions() {
        let target = H264Target::compress(VideoCompressOptions {
            crf: 28.0,
            max_width: Some(640),
            max_height: Some(360),
        });

        assert_eq!(target.max_width, None);
        assert_eq!(target.max_height, None);
        assert_eq!(target.crf(), Some(28.0));
    }

    #[test]
    fn crf_is_clamped_and_non_finite_uses_default() {
        assert_eq!(
            H264Target::compress(VideoCompressOptions::new(80.0)).crf(),
            Some(51.0)
        );
        assert_eq!(
            H264Target::compress(VideoCompressOptions::new(-1.0)).crf(),
            Some(0.0)
        );
        assert_eq!(
            H264Target::compress(VideoCompressOptions::new(f32::NAN)).crf(),
            Some(DEFAULT_CRF)
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
            target.resolve_bitrate_bps(Some(50_000_000), (1920, 1080), (1920, 1080), 30),
            at_28
        );
    }
}
