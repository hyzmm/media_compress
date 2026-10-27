use std::ffi::{c_char, c_void, CStr, CString};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::yuv::{self, CropRect};
use super::{compute_video_target_dimensions, VideoCompressOptions};
use crate::error::Error;

// ---------------------------------------------------------------------------
// Opaque NDK media types (libmediandk.so, API 21+)
// ---------------------------------------------------------------------------

#[repr(C)]
struct AMediaCodec {
    _private: [u8; 0],
}
#[repr(C)]
struct AMediaFormat {
    _private: [u8; 0],
}
#[repr(C)]
struct AMediaExtractor {
    _private: [u8; 0],
}
#[repr(C)]
struct AMediaMuxer {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AMediaCodecBufferInfo {
    offset: i32,
    size: i32,
    presentation_time_us: i64,
    flags: u32,
}

// ---------------------------------------------------------------------------
// FFI — NdkMedia (libmediandk.so)
// ---------------------------------------------------------------------------

#[link(name = "mediandk")]
extern "C" {
    // AMediaFormat
    fn AMediaFormat_new() -> *mut AMediaFormat;
    fn AMediaFormat_delete(fmt: *mut AMediaFormat) -> i32;
    fn AMediaFormat_getInt32(fmt: *mut AMediaFormat, name: *const c_char, out: *mut i32) -> u8;
    fn AMediaFormat_getFloat(fmt: *mut AMediaFormat, name: *const c_char, out: *mut f32) -> u8;
    fn AMediaFormat_getString(
        fmt: *mut AMediaFormat,
        name: *const c_char,
        out: *mut *const c_char,
    ) -> u8;
    fn AMediaFormat_setInt32(fmt: *mut AMediaFormat, name: *const c_char, value: i32);
    fn AMediaFormat_setString(fmt: *mut AMediaFormat, name: *const c_char, value: *const c_char);

    // AMediaExtractor
    fn AMediaExtractor_new() -> *mut AMediaExtractor;
    fn AMediaExtractor_delete(ex: *mut AMediaExtractor) -> i32;
    fn AMediaExtractor_setDataSourceFd(
        ex: *mut AMediaExtractor,
        fd: i32,
        offset: i64,
        length: i64,
    ) -> i32;
    fn AMediaExtractor_getTrackCount(ex: *mut AMediaExtractor) -> usize;
    fn AMediaExtractor_getTrackFormat(ex: *mut AMediaExtractor, idx: usize) -> *mut AMediaFormat;
    fn AMediaExtractor_selectTrack(ex: *mut AMediaExtractor, idx: usize) -> i32;
    fn AMediaExtractor_readSampleData(
        ex: *mut AMediaExtractor,
        buffer: *mut u8,
        capacity: usize,
    ) -> isize;
    fn AMediaExtractor_getSampleTime(ex: *mut AMediaExtractor) -> i64;
    fn AMediaExtractor_getSampleTrackIndex(ex: *mut AMediaExtractor) -> i32;
    fn AMediaExtractor_advance(ex: *mut AMediaExtractor) -> u8;

    // AMediaCodec
    fn AMediaCodec_createDecoderByType(mime: *const c_char) -> *mut AMediaCodec;
    fn AMediaCodec_createEncoderByType(mime: *const c_char) -> *mut AMediaCodec;
    fn AMediaCodec_createCodecByName(name: *const c_char) -> *mut AMediaCodec;
    fn AMediaCodec_delete(codec: *mut AMediaCodec) -> i32;
    fn AMediaCodec_configure(
        codec: *mut AMediaCodec,
        format: *const AMediaFormat,
        surface: *mut c_void,
        crypto: *mut c_void,
        flags: u32,
    ) -> i32;
    fn AMediaCodec_start(codec: *mut AMediaCodec) -> i32;
    fn AMediaCodec_dequeueInputBuffer(codec: *mut AMediaCodec, timeout_us: i64) -> isize;
    fn AMediaCodec_getInputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        out_size: *mut usize,
    ) -> *mut u8;
    fn AMediaCodec_queueInputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        offset: i32,
        size: usize,
        time: u64,
        flags: u32,
    ) -> i32;
    fn AMediaCodec_dequeueOutputBuffer(
        codec: *mut AMediaCodec,
        info: *mut AMediaCodecBufferInfo,
        timeout_us: i64,
    ) -> isize;
    fn AMediaCodec_getOutputBuffer(
        codec: *mut AMediaCodec,
        idx: usize,
        out_size: *mut usize,
    ) -> *mut u8;
    fn AMediaCodec_getOutputFormat(codec: *mut AMediaCodec) -> *mut AMediaFormat;
    fn AMediaCodec_releaseOutputBuffer(codec: *mut AMediaCodec, idx: usize, render: u8) -> i32;

    // AMediaMuxer
    fn AMediaMuxer_new(fd: i32, format: i32) -> *mut AMediaMuxer;
    fn AMediaMuxer_delete(muxer: *mut AMediaMuxer) -> i32;
    fn AMediaMuxer_addTrack(muxer: *mut AMediaMuxer, format: *const AMediaFormat) -> isize;
    fn AMediaMuxer_start(muxer: *mut AMediaMuxer) -> i32;
    fn AMediaMuxer_writeSampleData(
        muxer: *mut AMediaMuxer,
        track_idx: usize,
        data: *const u8,
        info: *const AMediaCodecBufferInfo,
    ) -> i32;
    fn AMediaMuxer_stop(muxer: *mut AMediaMuxer) -> i32;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const AMEDIA_OK: i32 = 0;
const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;
const BUFFER_FLAG_END_OF_STREAM: u32 = 4;
const CONFIGURE_FLAG_ENCODE: u32 = 1;
const INFO_TRY_AGAIN_LATER: isize = -1;
const INFO_OUTPUT_FORMAT_CHANGED: isize = -2;
const INFO_OUTPUT_BUFFERS_CHANGED: isize = -3;
const MUXER_OUTPUT_FORMAT_MPEG_4: i32 = 0;
const DEQUEUE_TIMEOUT_US: i64 = 10_000;

const DEFAULT_FRAME_RATE: i32 = 30;
const DEFAULT_SAMPLE_RATE: i32 = 44_100;
const DEFAULT_CHANNELS: i32 = 2;
const AUDIO_BITRATE: i32 = 128_000;
const AAC_LC_PROFILE: i32 = 2;

/// Encoder input layouts we can produce, in preference order.
const VIDEO_ENCODER_COLOR_FORMATS: [i32; 3] = [
    yuv::COLOR_FORMAT_YUV420_PLANAR,
    yuv::COLOR_FORMAT_YUV420_SEMI_PLANAR,
    yuv::COLOR_FORMAT_YUV420_FLEXIBLE,
];

/// Abort when no codec makes progress for this long instead of spinning forever.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// RAII guards
// ---------------------------------------------------------------------------

struct Codec(*mut AMediaCodec);

impl Drop for Codec {
    fn drop(&mut self) {
        unsafe { AMediaCodec_delete(self.0) };
    }
}

struct Format(*mut AMediaFormat);

impl Format {
    fn new() -> Result<Self, Error> {
        let ptr = unsafe { AMediaFormat_new() };
        if ptr.is_null() {
            Err(Error::EncodeError("AMediaFormat_new failed".into()))
        } else {
            Ok(Self(ptr))
        }
    }
}

impl Drop for Format {
    fn drop(&mut self) {
        unsafe { AMediaFormat_delete(self.0) };
    }
}

struct Extractor(*mut AMediaExtractor);

impl Drop for Extractor {
    fn drop(&mut self) {
        unsafe { AMediaExtractor_delete(self.0) };
    }
}

struct Muxer {
    ptr: *mut AMediaMuxer,
    started: bool,
}

impl Muxer {
    fn new(fd: i32) -> Result<Self, Error> {
        let ptr = unsafe { AMediaMuxer_new(fd, MUXER_OUTPUT_FORMAT_MPEG_4) };
        if ptr.is_null() {
            Err(Error::EncodeError("AMediaMuxer_new failed".into()))
        } else {
            Ok(Self {
                ptr,
                started: false,
            })
        }
    }

    fn start(&mut self) -> Result<(), Error> {
        let ret = unsafe { AMediaMuxer_start(self.ptr) };
        if ret != AMEDIA_OK {
            return Err(Error::EncodeError(format!(
                "AMediaMuxer_start failed: {ret}"
            )));
        }
        self.started = true;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Error> {
        if !self.started {
            return Err(Error::EncodeError(
                "AMediaMuxer_stop called before start".into(),
            ));
        }
        let ret = unsafe { AMediaMuxer_stop(self.ptr) };
        self.started = false;
        if ret != AMEDIA_OK {
            return Err(Error::EncodeError(format!(
                "AMediaMuxer_stop failed: {ret}"
            )));
        }
        Ok(())
    }
}

impl Drop for Muxer {
    fn drop(&mut self) {
        unsafe {
            if self.started {
                AMediaMuxer_stop(self.ptr);
            }
            AMediaMuxer_delete(self.ptr);
        }
    }
}

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

unsafe fn format_i32(fmt: *mut AMediaFormat, key: &'static CStr) -> Option<i32> {
    let mut out: i32 = 0;
    (AMediaFormat_getInt32(fmt, key.as_ptr(), &mut out) != 0).then_some(out)
}

unsafe fn format_f32(fmt: *mut AMediaFormat, key: &'static CStr) -> Option<f32> {
    let mut out: f32 = 0.0;
    (AMediaFormat_getFloat(fmt, key.as_ptr(), &mut out) != 0).then_some(out)
}

unsafe fn format_string(fmt: *mut AMediaFormat, key: &'static CStr) -> Option<String> {
    let mut out: *const c_char = std::ptr::null();
    (AMediaFormat_getString(fmt, key.as_ptr(), &mut out) != 0 && !out.is_null())
        .then(|| CStr::from_ptr(out).to_string_lossy().into_owned())
}

/// Visible region of a codec frame; falls back to the full size when the
/// format has no (valid) crop keys.
unsafe fn crop_rect(fmt: *mut AMediaFormat, width: usize, height: usize) -> CropRect {
    let full = CropRect {
        left: 0,
        top: 0,
        width,
        height,
    };
    if width == 0 || height == 0 {
        return full;
    }

    let left = format_i32(fmt, c"crop-left").unwrap_or(0).max(0) as usize;
    let top = format_i32(fmt, c"crop-top").unwrap_or(0).max(0) as usize;
    let right = format_i32(fmt, c"crop-right")
        .map(|v| v.max(0) as usize)
        .unwrap_or(width - 1);
    let bottom = format_i32(fmt, c"crop-bottom")
        .map(|v| v.max(0) as usize)
        .unwrap_or(height - 1);

    if right < left || bottom < top || right >= width || bottom >= height {
        return full;
    }

    CropRect {
        left,
        top,
        width: right - left + 1,
        height: bottom - top + 1,
    }
    .even()
}

fn normalize_rotation(degrees: i32) -> u32 {
    match degrees.rem_euclid(360) {
        90 => 90,
        180 => 180,
        270 => 270,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Track discovery
// ---------------------------------------------------------------------------

struct VideoTrack {
    index: usize,
    mime: String,
    crop: CropRect,
    rotation: u32,
    frame_rate: i32,
    max_input_size: usize,
}

struct AudioTrack {
    index: usize,
    mime: String,
    sample_rate: i32,
    channel_count: i32,
    max_input_size: usize,
}

unsafe fn scan_tracks(
    extractor: *mut AMediaExtractor,
) -> Result<(VideoTrack, Option<AudioTrack>), Error> {
    let track_count = AMediaExtractor_getTrackCount(extractor);
    let mut video = None;
    let mut audio = None;

    for index in 0..track_count {
        let fmt = AMediaExtractor_getTrackFormat(extractor, index);
        if fmt.is_null() {
            continue;
        }
        let fmt = Format(fmt);

        let Some(mime) = format_string(fmt.0, c"mime") else {
            continue;
        };
        let max_input_size = format_i32(fmt.0, c"max-input-size").unwrap_or(0).max(0) as usize;

        if video.is_none() && mime.starts_with("video/") {
            let width = format_i32(fmt.0, c"width").unwrap_or(0).max(0) as usize;
            let height = format_i32(fmt.0, c"height").unwrap_or(0).max(0) as usize;
            if width == 0 || height == 0 {
                continue;
            }

            let frame_rate = format_f32(fmt.0, c"frame-rate")
                .or_else(|| format_i32(fmt.0, c"frame-rate").map(|v| v as f32))
                .filter(|fps| fps.is_finite() && *fps >= 1.0)
                .map(|fps| (fps.round() as i32).clamp(1, 120))
                .unwrap_or(DEFAULT_FRAME_RATE);

            video = Some(VideoTrack {
                index,
                mime,
                crop: crop_rect(fmt.0, width, height),
                rotation: normalize_rotation(format_i32(fmt.0, c"rotation-degrees").unwrap_or(0)),
                frame_rate,
                max_input_size,
            });
        } else if audio.is_none() && mime.starts_with("audio/") {
            audio = Some(AudioTrack {
                index,
                mime,
                sample_rate: format_i32(fmt.0, c"sample-rate")
                    .unwrap_or(DEFAULT_SAMPLE_RATE)
                    .max(1),
                channel_count: format_i32(fmt.0, c"channel-count")
                    .unwrap_or(DEFAULT_CHANNELS)
                    .max(1),
                max_input_size,
            });
        }
    }

    let video = video.ok_or_else(|| Error::DecodeError("no video track found".into()))?;

    Ok((video, audio))
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn compress(input: &[u8], options: VideoCompressOptions) -> Result<Vec<u8>, Error> {
    let in_path = unique_temp_path("mp4");
    let out_path = unique_temp_path("mp4");

    std::fs::write(&in_path, input)
        .map_err(|e| Error::EncodeError(format!("failed to write temp input: {e}")))?;

    let result = transcode(&in_path, &out_path, options);

    let _ = std::fs::remove_file(&in_path);

    match result {
        Ok(()) => {
            let out = std::fs::read(&out_path)
                .map_err(|e| Error::EncodeError(format!("failed to read temp output: {e}")))?;
            let _ = std::fs::remove_file(&out_path);
            Ok(out)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&out_path);
            Err(e)
        }
    }
}

/// Transcode a video file directly to `out_path`.
///
/// Neither file is buffered in memory; MediaExtractor/MediaCodec stream
/// samples through file descriptors.
pub fn compress_file(
    input: &std::path::Path,
    output: &std::path::Path,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    transcode(input, output, options)
}

// ---------------------------------------------------------------------------
// Transcode
// ---------------------------------------------------------------------------

fn unique_temp_path(ext: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "media_compress_{}_{}.{}",
        std::process::id(),
        nanos,
        ext
    ))
}

fn transcode(
    in_path: &std::path::Path,
    out_path: &std::path::Path,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    let in_file = std::fs::File::open(in_path)
        .map_err(|e| Error::DecodeError(format!("failed to open input: {e}")))?;
    let in_fd = in_file.as_raw_fd();
    let in_len = std::fs::metadata(in_path)
        .map_err(|e| Error::DecodeError(format!("failed to stat input: {e}")))?
        .len() as i64;

    let out_file = std::fs::File::create(out_path)
        .map_err(|e| Error::EncodeError(format!("failed to create output: {e}")))?;
    let out_fd = out_file.into_raw_fd();

    let result = unsafe { transcode_fd(in_fd, in_len, out_fd, options) };

    // The muxer is deleted by the time transcode_fd returns, so our descriptor
    // can be released regardless of whether the muxer duplicated it.
    drop(unsafe { std::fs::File::from_raw_fd(out_fd) });
    drop(in_file);

    result
}

#[derive(Clone, Copy)]
struct VideoLayout {
    crop: CropRect,
    stride: usize,
    slice_height: usize,
    color_format: i32,
}

impl VideoLayout {
    fn from_track(track: &VideoTrack) -> Self {
        let crop = track.crop.even();
        Self {
            crop,
            stride: crop.width,
            slice_height: crop.height,
            color_format: yuv::COLOR_FORMAT_YUV420_PLANAR,
        }
    }

    unsafe fn from_codec(fmt: *mut AMediaFormat, fallback: Self) -> Option<Self> {
        let width = format_i32(fmt, c"width").unwrap_or(0).max(0) as usize;
        let height = format_i32(fmt, c"height").unwrap_or(0).max(0) as usize;
        if width == 0 || height == 0 {
            return None;
        }

        let crop = crop_rect(fmt, width, height);
        if crop.width == 0 || crop.height == 0 {
            return None;
        }

        let stride = format_i32(fmt, c"stride")
            .map(|v| v.max(0) as usize)
            .filter(|v| *v >= width)
            .unwrap_or(width);
        let slice_height = format_i32(fmt, c"slice-height")
            .map(|v| v.max(0) as usize)
            .filter(|v| *v >= height)
            .unwrap_or(height);

        Some(Self {
            crop,
            stride,
            slice_height,
            color_format: format_i32(fmt, c"color-format").unwrap_or(fallback.color_format),
        })
    }
}

/// A demuxed sample waiting for a decoder input buffer.
#[derive(Clone, Copy)]
struct Sample {
    track: usize,
    pts: i64,
    size: usize,
}

#[derive(Clone)]
struct PendingSample {
    data: Vec<u8>,
    pts: i64,
    flags: u32,
}

unsafe fn transcode_fd(
    in_fd: i32,
    in_len: i64,
    out_fd: i32,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    let extractor = {
        let ptr = AMediaExtractor_new();
        if ptr.is_null() {
            return Err(Error::DecodeError("AMediaExtractor_new failed".into()));
        }
        Extractor(ptr)
    };
    let ret = AMediaExtractor_setDataSourceFd(extractor.0, in_fd, 0, in_len);
    if ret != AMEDIA_OK {
        return Err(Error::DecodeError(format!(
            "AMediaExtractor_setDataSourceFd failed: {ret}"
        )));
    }

    let (video_track, audio_track) = scan_tracks(extractor.0)?;
    let has_audio = audio_track.is_some();

    let (display_w, display_h) = match video_track.rotation {
        90 | 270 => (video_track.crop.height, video_track.crop.width),
        _ => (video_track.crop.width, video_track.crop.height),
    };
    let (target_w, target_h) = compute_video_target_dimensions(
        display_w as u32,
        display_h as u32,
        options.max_width,
        options.max_height,
    );
    let (target_w, target_h) = (target_w as usize, target_h as usize);

    AMediaExtractor_selectTrack(extractor.0, video_track.index);
    if let Some(track) = &audio_track {
        AMediaExtractor_selectTrack(extractor.0, track.index);
    }

    let video_decoder = create_decoder(&video_track.mime)?;
    configure_from_track(&video_decoder, extractor.0, video_track.index)?;
    start_codec(&video_decoder, "video decoder")?;

    let (video_encoder, encoder_color_format) = create_video_encoder(
        target_w,
        target_h,
        video_track.frame_rate,
        options.bitrate_kbps,
    )?;

    let audio_decoder = match &audio_track {
        Some(track) => {
            let decoder = create_decoder(&track.mime)?;
            configure_from_track(&decoder, extractor.0, track.index)?;
            start_codec(&decoder, "audio decoder")?;
            Some(decoder)
        }
        None => None,
    };

    let mut muxer = Muxer::new(out_fd)?;
    let mut muxer_started = false;

    let mut layout = VideoLayout::from_track(&video_track);
    let sample_capacity = video_track
        .max_input_size
        .max(audio_track.as_ref().map_or(0, |t| t.max_input_size))
        .max(1 << 20);
    let mut sample_buf = vec![0u8; sample_capacity];
    let mut pending_input: Option<Sample> = None;
    let mut extractor_eos = false;

    let mut video_input_eos = false;
    let mut video_decoder_eos = false;
    let mut video_encoder_input_eos = false;
    let mut video_output_eos = false;
    let mut video_track_index: isize = -1;
    let mut pending_video_frame: Option<(Vec<u8>, i64)> = None;
    let mut pending_video_samples: Vec<PendingSample> = Vec::new();

    let mut audio_input_eos = !has_audio;
    let mut audio_decoder_eos = !has_audio;
    let mut audio_encoder_input_eos = !has_audio;
    let mut audio_output_eos = !has_audio;
    let mut audio_track_index: isize = -1;
    let mut audio_encoder: Option<Codec> = None;
    let mut audio_rate = audio_track
        .as_ref()
        .map_or(DEFAULT_SAMPLE_RATE, |t| t.sample_rate);
    let mut audio_channels = audio_track
        .as_ref()
        .map_or(DEFAULT_CHANNELS, |t| t.channel_count);
    let mut pending_audio_pcm: Option<(Vec<u8>, i64)> = None;
    let mut pending_audio_samples: Vec<PendingSample> = Vec::new();

    let mut last_progress = Instant::now();

    loop {
        if video_output_eos && audio_output_eos {
            break;
        }
        let mut progressed = false;

        // ── Extractor → decoder input ────────────────────────────────────
        if pending_input.is_none() && !extractor_eos {
            pending_input = read_sample(
                extractor.0,
                &mut sample_buf,
                video_track.index,
                audio_track.as_ref().map(|t| t.index),
            )?;
            if pending_input.is_none() {
                extractor_eos = true;
            }
        }

        if let Some(sample) = pending_input.take() {
            let decoder = if sample.track == video_track.index {
                Some(&video_decoder)
            } else {
                audio_decoder.as_ref()
            };
            match decoder {
                Some(decoder) => match dequeue_input(decoder)? {
                    Some(idx) => {
                        let (ptr, capacity) = input_buffer(decoder, idx).ok_or_else(|| {
                            Error::EncodeError("decoder input buffer unavailable".into())
                        })?;
                        if sample.size > capacity {
                            return Err(Error::DecodeError(format!(
                                "sample of {} bytes exceeds decoder input buffer of {capacity}",
                                sample.size
                            )));
                        }
                        std::ptr::copy_nonoverlapping(sample_buf.as_ptr(), ptr, sample.size);
                        queue_input(decoder, idx, sample.size, sample.pts, 0)?;
                        AMediaExtractor_advance(extractor.0);
                        progressed = true;
                    }
                    None => pending_input = Some(sample),
                },
                None => {
                    return Err(Error::DecodeError(
                        "demuxed sample has no matching decoder".into(),
                    ));
                }
            }
        }

        if extractor_eos && pending_input.is_none() {
            if !video_input_eos && queue_eos(&video_decoder)? {
                video_input_eos = true;
                progressed = true;
            }
            if let Some(decoder) = &audio_decoder {
                if !audio_input_eos && queue_eos(decoder)? {
                    audio_input_eos = true;
                    progressed = true;
                }
            }
        }

        // ── Video: decoder → rotate/scale → encoder ──────────────────────
        if !video_decoder_eos && pending_video_frame.is_none() {
            let mut info = empty_info();
            match dequeue_output(&video_decoder, &mut info)? {
                OutputBuffer::FormatChanged => {
                    let fmt = AMediaCodec_getOutputFormat(video_decoder.0);
                    if !fmt.is_null() {
                        let fmt = Format(fmt);
                        if let Some(updated) = VideoLayout::from_codec(fmt.0, layout) {
                            layout = updated;
                        }
                    }
                    progressed = true;
                }
                OutputBuffer::Buffer(idx) => {
                    if info.size > 0 {
                        let (ptr, capacity) =
                            output_buffer(&video_decoder, idx).ok_or_else(|| {
                                Error::DecodeError("video decoder output buffer unavailable".into())
                            })?;
                        let raw = output_slice(ptr, capacity, &info)?;
                        let frame = prepare_frame(
                            raw,
                            &layout,
                            video_track.rotation,
                            target_w,
                            target_h,
                            encoder_color_format,
                        )?;
                        pending_video_frame = Some((frame, info.presentation_time_us));
                    }
                    AMediaCodec_releaseOutputBuffer(video_decoder.0, idx, 0);
                    progressed = true;
                    if info.flags & BUFFER_FLAG_END_OF_STREAM != 0 {
                        video_decoder_eos = true;
                    }
                }
                OutputBuffer::TryAgain | OutputBuffer::BuffersChanged => {}
            }
        }

        if let Some((frame, pts)) = pending_video_frame.take() {
            if let Some(idx) = dequeue_input(&video_encoder)? {
                let (ptr, capacity) = input_buffer(&video_encoder, idx).ok_or_else(|| {
                    Error::EncodeError("video encoder input buffer unavailable".into())
                })?;
                if frame.len() > capacity {
                    return Err(Error::EncodeError(format!(
                        "frame of {} bytes exceeds encoder input buffer of {capacity}",
                        frame.len()
                    )));
                }
                std::ptr::copy_nonoverlapping(frame.as_ptr(), ptr, frame.len());
                queue_input(&video_encoder, idx, frame.len(), pts, 0)?;
                progressed = true;
            } else {
                pending_video_frame = Some((frame, pts));
            }
        }

        if video_decoder_eos && pending_video_frame.is_none() && !video_encoder_input_eos {
            if queue_eos(&video_encoder)? {
                video_encoder_input_eos = true;
                progressed = true;
            }
        }

        // ── Video: encoder → muxer ───────────────────────────────────────
        if !video_output_eos {
            let mut info = empty_info();
            match dequeue_output(&video_encoder, &mut info)? {
                OutputBuffer::FormatChanged => {
                    let fmt = AMediaCodec_getOutputFormat(video_encoder.0);
                    if !fmt.is_null() {
                        let fmt = Format(fmt);
                        video_track_index = AMediaMuxer_addTrack(muxer.ptr, fmt.0);
                        if video_track_index < 0 {
                            return Err(Error::EncodeError(
                                "AMediaMuxer_addTrack (video) failed".into(),
                            ));
                        }
                        progressed = true;
                    }
                }
                OutputBuffer::Buffer(idx) => {
                    if info.flags & BUFFER_FLAG_CODEC_CONFIG == 0 && info.size > 0 {
                        let (ptr, capacity) =
                            output_buffer(&video_encoder, idx).ok_or_else(|| {
                                Error::EncodeError("video encoder output buffer unavailable".into())
                            })?;
                        let data = output_slice(ptr, capacity, &info)?;
                        if muxer_started {
                            write_sample(muxer.ptr, video_track_index as usize, data, &info)?;
                        } else {
                            pending_video_samples.push(PendingSample {
                                data: data.to_vec(),
                                pts: info.presentation_time_us,
                                flags: info.flags,
                            });
                        }
                    }
                    AMediaCodec_releaseOutputBuffer(video_encoder.0, idx, 0);
                    progressed = true;
                    if info.flags & BUFFER_FLAG_END_OF_STREAM != 0 {
                        video_output_eos = true;
                    }
                }
                OutputBuffer::TryAgain | OutputBuffer::BuffersChanged => {}
            }
        }

        // ── Audio: decoder → encoder ─────────────────────────────────────
        if let Some(decoder) = &audio_decoder {
            if !audio_decoder_eos && pending_audio_pcm.is_none() {
                let mut info = empty_info();
                match dequeue_output(decoder, &mut info)? {
                    OutputBuffer::FormatChanged => {
                        let fmt = AMediaCodec_getOutputFormat(decoder.0);
                        if !fmt.is_null() {
                            let fmt = Format(fmt);
                            audio_rate = format_i32(fmt.0, c"sample-rate")
                                .unwrap_or(audio_rate)
                                .max(1);
                            audio_channels = format_i32(fmt.0, c"channel-count")
                                .unwrap_or(audio_channels)
                                .max(1);
                        }
                        if audio_encoder.is_none() {
                            audio_encoder = Some(create_audio_encoder(audio_rate, audio_channels)?);
                        }
                        progressed = true;
                    }
                    OutputBuffer::Buffer(idx) => {
                        if info.size > 0 {
                            let (ptr, capacity) = output_buffer(decoder, idx).ok_or_else(|| {
                                Error::DecodeError("audio decoder output buffer unavailable".into())
                            })?;
                            pending_audio_pcm = Some((
                                output_slice(ptr, capacity, &info)?.to_vec(),
                                info.presentation_time_us,
                            ));
                        }
                        AMediaCodec_releaseOutputBuffer(decoder.0, idx, 0);
                        progressed = true;
                        if info.flags & BUFFER_FLAG_END_OF_STREAM != 0 {
                            audio_decoder_eos = true;
                        }
                    }
                    OutputBuffer::TryAgain | OutputBuffer::BuffersChanged => {}
                }
            }

            if audio_encoder.is_none() && (pending_audio_pcm.is_some() || audio_decoder_eos) {
                audio_encoder = Some(create_audio_encoder(audio_rate, audio_channels)?);
                progressed = true;
            }
        }

        if let (Some(encoder), Some((pcm, pts))) =
            (audio_encoder.as_ref(), pending_audio_pcm.take())
        {
            if let Some(idx) = dequeue_input(encoder)? {
                let (ptr, capacity) = input_buffer(encoder, idx).ok_or_else(|| {
                    Error::EncodeError("audio encoder input buffer unavailable".into())
                })?;
                let bytes_per_frame = (audio_channels as usize) * 2;
                let mut take = pcm.len().min(capacity);
                take -= take % bytes_per_frame;
                if take == 0 {
                    return Err(Error::EncodeError(
                        "audio encoder input buffer cannot fit one PCM frame".into(),
                    ));
                }
                std::ptr::copy_nonoverlapping(pcm.as_ptr(), ptr, take);
                queue_input(encoder, idx, take, pts, 0)?;
                if take < pcm.len() {
                    let frames = (take / bytes_per_frame) as i64;
                    let next_pts = pts + frames * 1_000_000 / audio_rate as i64;
                    pending_audio_pcm = Some((pcm[take..].to_vec(), next_pts));
                }
                progressed = true;
            } else {
                pending_audio_pcm = Some((pcm, pts));
            }
        }

        if let Some(encoder) = &audio_encoder {
            if audio_decoder_eos && pending_audio_pcm.is_none() && !audio_encoder_input_eos {
                if queue_eos(encoder)? {
                    audio_encoder_input_eos = true;
                    progressed = true;
                }
            }
        }

        // ── Audio: encoder → muxer ───────────────────────────────────────
        if let Some(encoder) = &audio_encoder {
            if !audio_output_eos {
                let mut info = empty_info();
                match dequeue_output(encoder, &mut info)? {
                    OutputBuffer::FormatChanged => {
                        let fmt = AMediaCodec_getOutputFormat(encoder.0);
                        if !fmt.is_null() {
                            let fmt = Format(fmt);
                            audio_track_index = AMediaMuxer_addTrack(muxer.ptr, fmt.0);
                            if audio_track_index < 0 {
                                return Err(Error::EncodeError(
                                    "AMediaMuxer_addTrack (audio) failed".into(),
                                ));
                            }
                            progressed = true;
                        }
                    }
                    OutputBuffer::Buffer(idx) => {
                        if info.flags & BUFFER_FLAG_CODEC_CONFIG == 0 && info.size > 0 {
                            let (ptr, capacity) = output_buffer(encoder, idx).ok_or_else(|| {
                                Error::EncodeError("audio encoder output buffer unavailable".into())
                            })?;
                            let data = output_slice(ptr, capacity, &info)?;
                            if muxer_started {
                                write_sample(muxer.ptr, audio_track_index as usize, data, &info)?;
                            } else {
                                pending_audio_samples.push(PendingSample {
                                    data: data.to_vec(),
                                    pts: info.presentation_time_us,
                                    flags: info.flags,
                                });
                            }
                        }
                        AMediaCodec_releaseOutputBuffer(encoder.0, idx, 0);
                        progressed = true;
                        if info.flags & BUFFER_FLAG_END_OF_STREAM != 0 {
                            audio_output_eos = true;
                        }
                    }
                    OutputBuffer::TryAgain | OutputBuffer::BuffersChanged => {}
                }
            }
        }

        // ── Muxer start (needs every track format first) ─────────────────
        if !muxer_started && video_track_index >= 0 && (audio_track_index >= 0 || !has_audio) {
            muxer.start()?;
            muxer_started = true;
            for sample in pending_video_samples.drain(..) {
                write_pending_sample(muxer.ptr, video_track_index as usize, &sample)?;
            }
            for sample in pending_audio_samples.drain(..) {
                write_pending_sample(muxer.ptr, audio_track_index as usize, &sample)?;
            }
            progressed = true;
        }

        if progressed {
            last_progress = Instant::now();
        } else if last_progress.elapsed() > STALL_TIMEOUT {
            return Err(Error::EncodeError(
                "video transcode stalled: no codec made progress".into(),
            ));
        }
    }

    muxer.finish()
}

// ---------------------------------------------------------------------------
// Codec helpers
// ---------------------------------------------------------------------------

fn empty_info() -> AMediaCodecBufferInfo {
    AMediaCodecBufferInfo {
        offset: 0,
        size: 0,
        presentation_time_us: 0,
        flags: 0,
    }
}

fn create_decoder(mime: &str) -> Result<Codec, Error> {
    let mime_c =
        CString::new(mime).map_err(|e| Error::DecodeError(format!("invalid mime type: {e}")))?;
    let ptr = unsafe { AMediaCodec_createDecoderByType(mime_c.as_ptr()) };
    if ptr.is_null() {
        return Err(Error::DecodeError(format!("no decoder for {mime}")));
    }
    Ok(Codec(ptr))
}

fn configure_from_track(
    codec: &Codec,
    extractor: *mut AMediaExtractor,
    track_index: usize,
) -> Result<(), Error> {
    unsafe {
        let fmt_ptr = AMediaExtractor_getTrackFormat(extractor, track_index);
        if fmt_ptr.is_null() {
            return Err(Error::DecodeError("track format unavailable".into()));
        }
        let fmt = Format(fmt_ptr);
        let ret = AMediaCodec_configure(
            codec.0,
            fmt.0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        if ret != AMEDIA_OK {
            return Err(Error::DecodeError(format!(
                "decoder configure failed: {ret}"
            )));
        }
    }
    Ok(())
}

fn start_codec(codec: &Codec, what: &str) -> Result<(), Error> {
    let ret = unsafe { AMediaCodec_start(codec.0) };
    if ret != AMEDIA_OK {
        return Err(Error::DecodeError(format!("{what} start failed: {ret}")));
    }
    Ok(())
}

fn create_video_encoder(
    target_w: usize,
    target_h: usize,
    frame_rate: i32,
    bitrate_kbps: u32,
) -> Result<(Codec, i32), Error> {
    let bitrate = bitrate_kbps
        .saturating_mul(1000)
        .clamp(100_000, i32::MAX as u32) as i32;
    let candidates: [Option<&CStr>; 3] = [
        None,
        Some(c"c2.android.avc.encoder"),
        Some(c"OMX.google.h264.encoder"),
    ];
    let mut last_error = String::from("no encoder candidates");

    for candidate in candidates {
        for color_format in VIDEO_ENCODER_COLOR_FORMATS {
            let ptr = unsafe {
                match candidate {
                    Some(name) => AMediaCodec_createCodecByName(name.as_ptr()),
                    None => AMediaCodec_createEncoderByType(c"video/avc".as_ptr()),
                }
            };
            if ptr.is_null() {
                last_error = format!("codec {candidate:?} unavailable");
                continue;
            }
            let codec = Codec(ptr);

            let Ok(fmt) = Format::new() else {
                continue;
            };
            unsafe {
                AMediaFormat_setString(fmt.0, c"mime".as_ptr(), c"video/avc".as_ptr());
                AMediaFormat_setInt32(fmt.0, c"width".as_ptr(), target_w as i32);
                AMediaFormat_setInt32(fmt.0, c"height".as_ptr(), target_h as i32);
                AMediaFormat_setInt32(fmt.0, c"bitrate".as_ptr(), bitrate);
                AMediaFormat_setInt32(fmt.0, c"frame-rate".as_ptr(), frame_rate);
                AMediaFormat_setInt32(fmt.0, c"i-frame-interval".as_ptr(), 1);
                AMediaFormat_setInt32(fmt.0, c"color-format".as_ptr(), color_format);
            }
            let ret = unsafe {
                AMediaCodec_configure(
                    codec.0,
                    fmt.0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    CONFIGURE_FLAG_ENCODE,
                )
            };
            drop(fmt);
            if ret != AMEDIA_OK {
                last_error = format!("color-format {color_format} configure failed: {ret}");
                continue;
            }

            let start = unsafe { AMediaCodec_start(codec.0) };
            if start != AMEDIA_OK {
                last_error = format!("color-format {color_format} start failed: {start}");
                continue;
            }

            return Ok((codec, color_format));
        }
    }

    Err(Error::EncodeError(format!(
        "no usable H.264 encoder ({last_error})"
    )))
}

fn create_audio_encoder(sample_rate: i32, channels: i32) -> Result<Codec, Error> {
    let candidates: [Option<&CStr>; 3] = [
        None,
        Some(c"c2.android.aac.encoder"),
        Some(c"OMX.google.aac.encoder"),
    ];
    let mut last_error = String::from("no encoder candidates");

    for candidate in candidates {
        let ptr = unsafe {
            match candidate {
                Some(name) => AMediaCodec_createCodecByName(name.as_ptr()),
                None => AMediaCodec_createEncoderByType(c"audio/mp4a-latm".as_ptr()),
            }
        };
        if ptr.is_null() {
            last_error = format!("codec {candidate:?} unavailable");
            continue;
        }
        let codec = Codec(ptr);

        let Ok(fmt) = Format::new() else {
            continue;
        };
        unsafe {
            AMediaFormat_setString(fmt.0, c"mime".as_ptr(), c"audio/mp4a-latm".as_ptr());
            AMediaFormat_setInt32(fmt.0, c"sample-rate".as_ptr(), sample_rate);
            AMediaFormat_setInt32(fmt.0, c"channel-count".as_ptr(), channels);
            AMediaFormat_setInt32(fmt.0, c"bitrate".as_ptr(), AUDIO_BITRATE);
            AMediaFormat_setInt32(fmt.0, c"aac-profile".as_ptr(), AAC_LC_PROFILE);
        }
        let ret = unsafe {
            AMediaCodec_configure(
                codec.0,
                fmt.0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                CONFIGURE_FLAG_ENCODE,
            )
        };
        drop(fmt);
        if ret != AMEDIA_OK {
            last_error = format!("configure failed: {ret}");
            continue;
        }

        let start = unsafe { AMediaCodec_start(codec.0) };
        if start != AMEDIA_OK {
            last_error = format!("start failed: {start}");
            continue;
        }

        return Ok(codec);
    }

    Err(Error::EncodeError(format!(
        "no usable AAC encoder ({last_error})"
    )))
}

// ---------------------------------------------------------------------------
// Buffer helpers
// ---------------------------------------------------------------------------

enum OutputBuffer {
    TryAgain,
    FormatChanged,
    BuffersChanged,
    Buffer(usize),
}

unsafe fn dequeue_input(codec: &Codec) -> Result<Option<usize>, Error> {
    match AMediaCodec_dequeueInputBuffer(codec.0, DEQUEUE_TIMEOUT_US) {
        INFO_TRY_AGAIN_LATER => Ok(None),
        idx if idx >= 0 => Ok(Some(idx as usize)),
        err => Err(Error::NativeError(format!(
            "AMediaCodec_dequeueInputBuffer failed: {err}"
        ))),
    }
}

unsafe fn dequeue_output(
    codec: &Codec,
    info: &mut AMediaCodecBufferInfo,
) -> Result<OutputBuffer, Error> {
    match AMediaCodec_dequeueOutputBuffer(codec.0, info, DEQUEUE_TIMEOUT_US) {
        INFO_TRY_AGAIN_LATER => Ok(OutputBuffer::TryAgain),
        INFO_OUTPUT_FORMAT_CHANGED => Ok(OutputBuffer::FormatChanged),
        INFO_OUTPUT_BUFFERS_CHANGED => Ok(OutputBuffer::BuffersChanged),
        idx if idx >= 0 => Ok(OutputBuffer::Buffer(idx as usize)),
        err => Err(Error::NativeError(format!(
            "AMediaCodec_dequeueOutputBuffer failed: {err}"
        ))),
    }
}

unsafe fn input_buffer(codec: &Codec, idx: usize) -> Option<(*mut u8, usize)> {
    let mut size: usize = 0;
    let ptr = AMediaCodec_getInputBuffer(codec.0, idx, &mut size);
    (!ptr.is_null() && size > 0).then_some((ptr, size))
}

unsafe fn output_buffer(codec: &Codec, idx: usize) -> Option<(*const u8, usize)> {
    let mut size: usize = 0;
    let ptr = AMediaCodec_getOutputBuffer(codec.0, idx, &mut size);
    (!ptr.is_null() && size > 0).then_some((ptr, size))
}

unsafe fn output_slice<'a>(
    ptr: *const u8,
    capacity: usize,
    info: &AMediaCodecBufferInfo,
) -> Result<&'a [u8], Error> {
    let offset = info.offset.max(0) as usize;
    let size = info.size.max(0) as usize;
    if offset + size > capacity {
        return Err(Error::DecodeError(format!(
            "codec buffer info out of range: offset {offset} + size {size} > {capacity}"
        )));
    }
    Ok(std::slice::from_raw_parts(ptr.add(offset), size))
}

unsafe fn queue_input(
    codec: &Codec,
    idx: usize,
    size: usize,
    pts: i64,
    flags: u32,
) -> Result<(), Error> {
    let ret = AMediaCodec_queueInputBuffer(codec.0, idx, 0, size, pts.max(0) as u64, flags);
    if ret != AMEDIA_OK {
        return Err(Error::EncodeError(format!(
            "AMediaCodec_queueInputBuffer failed: {ret}"
        )));
    }
    Ok(())
}

/// Queues an empty end-of-stream input buffer. Returns false when the codec has
/// no input buffer free yet, so the caller can retry on the next iteration.
unsafe fn queue_eos(codec: &Codec) -> Result<bool, Error> {
    let Some(idx) = dequeue_input(codec)? else {
        return Ok(false);
    };
    queue_input(codec, idx, 0, 0, BUFFER_FLAG_END_OF_STREAM)?;
    Ok(true)
}

unsafe fn read_sample(
    extractor: *mut AMediaExtractor,
    buf: &mut [u8],
    video_track: usize,
    audio_track: Option<usize>,
) -> Result<Option<Sample>, Error> {
    loop {
        let size = AMediaExtractor_readSampleData(extractor, buf.as_mut_ptr(), buf.len());
        if size < 0 {
            return Ok(None);
        }

        let track = AMediaExtractor_getSampleTrackIndex(extractor);
        if track < 0 {
            return Ok(None);
        }

        let track = track as usize;
        if track == video_track || Some(track) == audio_track {
            let size = size as usize;
            if size > buf.len() {
                return Err(Error::DecodeError(format!(
                    "sample of {size} bytes exceeds input buffer of {}",
                    buf.len()
                )));
            }
            return Ok(Some(Sample {
                track,
                pts: AMediaExtractor_getSampleTime(extractor),
                size,
            }));
        }

        AMediaExtractor_advance(extractor);
    }
}

unsafe fn write_sample(
    muxer: *mut AMediaMuxer,
    track_index: usize,
    data: &[u8],
    info: &AMediaCodecBufferInfo,
) -> Result<(), Error> {
    let info = AMediaCodecBufferInfo {
        offset: 0,
        size: data.len() as i32,
        presentation_time_us: info.presentation_time_us,
        flags: info.flags,
    };
    let ret = AMediaMuxer_writeSampleData(muxer, track_index, data.as_ptr(), &info);
    if ret != AMEDIA_OK {
        return Err(Error::EncodeError(format!(
            "AMediaMuxer_writeSampleData failed: {ret}"
        )));
    }
    Ok(())
}

unsafe fn write_pending_sample(
    muxer: *mut AMediaMuxer,
    track_index: usize,
    sample: &PendingSample,
) -> Result<(), Error> {
    let info = AMediaCodecBufferInfo {
        offset: 0,
        size: sample.data.len() as i32,
        presentation_time_us: sample.pts,
        flags: sample.flags,
    };
    let ret = AMediaMuxer_writeSampleData(muxer, track_index, sample.data.as_ptr(), &info);
    if ret != AMEDIA_OK {
        return Err(Error::EncodeError(format!(
            "AMediaMuxer_writeSampleData failed: {ret}"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Frame preparation
// ---------------------------------------------------------------------------

fn prepare_frame(
    raw: &[u8],
    layout: &VideoLayout,
    rotation: u32,
    target_w: usize,
    target_h: usize,
    encoder_color_format: i32,
) -> Result<Vec<u8>, Error> {
    let crop = layout.crop.even();
    let i420 = yuv::to_i420(
        raw,
        layout.stride,
        layout.slice_height,
        layout.color_format,
        crop,
    );
    if i420.is_empty() {
        return Err(Error::DecodeError(format!(
            "decoder output does not fit layout {:?} stride {} slice-height {} ({} bytes)",
            crop,
            layout.stride,
            layout.slice_height,
            raw.len()
        )));
    }

    let (frame, width, height) = if rotation == 0 {
        (i420, crop.width, crop.height)
    } else {
        yuv::rotate_i420_clockwise(&i420, crop.width, crop.height, rotation)
    };
    if frame.is_empty() {
        return Err(Error::DecodeError("frame rotation failed".into()));
    }

    let frame = if width != target_w || height != target_h {
        let scaled = yuv::scale_i420(&frame, width, height, target_w, target_h);
        if scaled.is_empty() {
            return Err(Error::EncodeError("frame scaling failed".into()));
        }
        scaled
    } else {
        frame
    };

    if encoder_color_format == yuv::COLOR_FORMAT_YUV420_SEMI_PLANAR {
        let nv12 = yuv::i420_to_nv12(&frame, target_w, target_h);
        if nv12.is_empty() {
            return Err(Error::EncodeError("I420 to NV12 conversion failed".into()));
        }
        return Ok(nv12);
    }

    Ok(frame)
}
