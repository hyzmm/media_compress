use std::ffi::{c_char, c_void, CStr, CString};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::yuv;
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
    fn AMediaExtractor_readSampleData(ex: *mut AMediaExtractor, buffer: *mut u8, capacity: usize) -> isize;
    fn AMediaExtractor_getSampleTime(ex: *mut AMediaExtractor) -> i64;
    fn AMediaExtractor_getSampleFlags(ex: *mut AMediaExtractor) -> u32;
    fn AMediaExtractor_getSampleTrackIndex(ex: *mut AMediaExtractor) -> i32;
    fn AMediaExtractor_advance(ex: *mut AMediaExtractor) -> u8;

    // AMediaCodec
    fn AMediaCodec_createDecoderByType(mime: *const c_char) -> *mut AMediaCodec;
    fn AMediaCodec_createEncoderByType(mime: *const c_char) -> *mut AMediaCodec;
    fn AMediaCodec_delete(codec: *mut AMediaCodec) -> i32;
    fn AMediaCodec_configure(
        codec: *mut AMediaCodec,
        format: *const AMediaFormat,
        surface: *mut c_void,
        crypto: *mut c_void,
        flags: u32,
    ) -> i32;
    fn AMediaCodec_start(codec: *mut AMediaCodec) -> i32;
    fn AMediaCodec_stop(codec: *mut AMediaCodec) -> i32;
    fn AMediaCodec_dequeueInputBuffer(codec: *mut AMediaCodec, timeout_us: i64) -> isize;
    fn AMediaCodec_getInputBuffer(codec: *mut AMediaCodec, idx: usize, out_size: *mut usize) -> *mut u8;
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
    fn AMediaCodec_getOutputBuffer(codec: *mut AMediaCodec, idx: usize, out_size: *mut usize) -> *mut u8;
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
const AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG: u32 = 2;
const AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM: u32 = 4;
const AMEDIACODEC_CONFIGURE_FLAG_ENCODE: u32 = 1;
const AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED: isize = -2;
const AMEDIAMUXER_OUTPUT_FORMAT_MPEG_4: i32 = 0;
const DEQUEUE_TIMEOUT_US: i64 = 10_000;

const DEFAULT_FRAME_RATE: i32 = 30;
const AUDIO_SAMPLE_RATE: i32 = 44100;
const AUDIO_CHANNELS: i32 = 2;
const AUDIO_BITRATE: i32 = 128_000;
const AUDIO_AAC_LC: i32 = 2;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

unsafe fn format_get_int32(fmt: *mut AMediaFormat, key: &'static CStr, default: i32) -> i32 {
    let mut out: i32 = 0;
    if AMediaFormat_getInt32(fmt, key.as_ptr(), &mut out) != 0 {
        out
    } else {
        default
    }
}

unsafe fn format_get_string(fmt: *mut AMediaFormat, key: &'static CStr) -> Option<String> {
    let mut out: *const c_char = std::ptr::null();
    if AMediaFormat_getString(fmt, key.as_ptr(), &mut out) != 0 && !out.is_null() {
        Some(CStr::from_ptr(out).to_string_lossy().into_owned())
    } else {
        None
    }
}

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

// ---------------------------------------------------------------------------
// Transcode implementation
// ---------------------------------------------------------------------------

fn transcode(
    in_path: &std::path::Path,
    out_path: &std::path::Path,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    let in_file = std::fs::File::open(in_path)
        .map_err(|e| Error::DecodeError(format!("failed to open temp input: {e}")))?;
    let in_fd = in_file.as_raw_fd();
    let in_len = std::fs::metadata(in_path)
        .map_err(|e| Error::DecodeError(format!("failed to stat temp input: {e}")))?
        .len() as i64;

    let out_file = std::fs::File::create(out_path)
        .map_err(|e| Error::EncodeError(format!("failed to create temp output: {e}")))?;
    // AMediaMuxer takes ownership of the fd (closes it on delete).
    let out_fd = out_file.into_raw_fd();

    unsafe { transcode_impl(in_fd, in_len, out_fd, options) }
}

unsafe fn transcode_impl(
    in_fd: i32,
    in_len: i64,
    out_fd: i32,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    // ── Extractor ────────────────────────────────────────────────────────
    let extractor = AMediaExtractor_new();
    if extractor.is_null() {
        return Err(Error::DecodeError("AMediaExtractor_new failed".into()));
    }
    let ret = AMediaExtractor_setDataSourceFd(extractor, in_fd, 0, in_len);
    if ret != AMEDIA_OK {
        AMediaExtractor_delete(extractor);
        return Err(Error::DecodeError(format!("setDataSourceFd failed: {ret}")));
    }

    let track_count = AMediaExtractor_getTrackCount(extractor);

    let mut video_track: isize = -1;
    let mut audio_track: isize = -1;
    let mut video_mime: Option<String> = None;
    let mut audio_mime: Option<String> = None;
    let mut src_w: usize = 0;
    let mut src_h: usize = 0;
    let mut frame_rate: i32 = DEFAULT_FRAME_RATE;

    for i in 0..track_count {
        let fmt = AMediaExtractor_getTrackFormat(extractor, i);
        if fmt.is_null() {
            continue;
        }
        let Some(mime) = format_get_string(fmt, c"mime") else {
            AMediaFormat_delete(fmt);
            continue;
        };
        if mime.starts_with("video/") && video_track < 0 {
            video_track = i as isize;
            video_mime = Some(mime);
            src_w = format_get_int32(fmt, c"width", 0) as usize;
            src_h = format_get_int32(fmt, c"height", 0) as usize;
            let fps = format_get_int32(fmt, c"frame-rate", 0);
            if fps > 0 {
                frame_rate = fps;
            }
        } else if mime.starts_with("audio/") && audio_track < 0 {
            audio_track = i as isize;
            audio_mime = Some(mime);
        }
        AMediaFormat_delete(fmt);
    }

    let Some(video_mime) = video_mime else {
        AMediaExtractor_delete(extractor);
        return Err(Error::DecodeError("no video track found".into()));
    };
    if src_w == 0 || src_h == 0 {
        AMediaExtractor_delete(extractor);
        return Err(Error::DecodeError("video track has zero dimensions".into()));
    }
    let (target_w, target_h) = compute_video_target_dimensions(
        src_w as u32,
        src_h as u32,
        options.max_width,
        options.max_height,
    );
    let target_w = target_w as usize;
    let target_h = target_h as usize;

    let has_audio = audio_track >= 0;
    AMediaExtractor_selectTrack(extractor, video_track as usize);
    if has_audio {
        AMediaExtractor_selectTrack(extractor, audio_track as usize);
    }

    // ── Video decoder ────────────────────────────────────────────────────
    let video_mime_c = CString::new(video_mime.as_str())
        .map_err(|e| Error::DecodeError(format!("invalid mime: {e}")))?;
    let video_decoder = AMediaCodec_createDecoderByType(video_mime_c.as_ptr());
    if video_decoder.is_null() {
        AMediaExtractor_delete(extractor);
        return Err(Error::DecodeError("createDecoderByType (video) failed".into()));
    }
    {
        let fmt = AMediaExtractor_getTrackFormat(extractor, video_track as usize);
        let ret = AMediaCodec_configure(
            video_decoder,
            fmt,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        AMediaFormat_delete(fmt);
        if ret != AMEDIA_OK {
            AMediaCodec_delete(video_decoder);
            AMediaExtractor_delete(extractor);
            return Err(Error::DecodeError(format!("video decoder configure failed: {ret}")));
        }
    }
    if AMediaCodec_start(video_decoder) != AMEDIA_OK {
        AMediaCodec_delete(video_decoder);
        AMediaExtractor_delete(extractor);
        return Err(Error::DecodeError("video decoder start failed".into()));
    }

    // ── Video encoder ────────────────────────────────────────────────────
    let video_encoder = AMediaCodec_createEncoderByType(c"video/avc".as_ptr());
    if video_encoder.is_null() {
        AMediaCodec_delete(video_decoder);
        AMediaExtractor_delete(extractor);
        return Err(Error::EncodeError("createEncoderByType (video/avc) failed".into()));
    }
    {
        let fmt = AMediaFormat_new();
        AMediaFormat_setString(fmt, c"mime".as_ptr(), c"video/avc".as_ptr());
        AMediaFormat_setInt32(fmt, c"width".as_ptr(), target_w as i32);
        AMediaFormat_setInt32(fmt, c"height".as_ptr(), target_h as i32);
        AMediaFormat_setInt32(fmt, c"bitrate".as_ptr(), (options.bitrate_kbps * 1000) as i32);
        AMediaFormat_setInt32(fmt, c"frame-rate".as_ptr(), frame_rate);
        AMediaFormat_setInt32(fmt, c"i-frame-interval".as_ptr(), 1);
        AMediaFormat_setInt32(fmt, c"color-format".as_ptr(), yuv::COLOR_FORMAT_YUV420_PLANAR);
        let ret = AMediaCodec_configure(
            video_encoder,
            fmt,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            AMEDIACODEC_CONFIGURE_FLAG_ENCODE,
        );
        AMediaFormat_delete(fmt);
        if ret != AMEDIA_OK {
            AMediaCodec_delete(video_encoder);
            AMediaCodec_delete(video_decoder);
            AMediaExtractor_delete(extractor);
            return Err(Error::EncodeError(format!("video encoder configure failed: {ret}")));
        }
    }
    if AMediaCodec_start(video_encoder) != AMEDIA_OK {
        AMediaCodec_delete(video_encoder);
        AMediaCodec_delete(video_decoder);
        AMediaExtractor_delete(extractor);
        return Err(Error::EncodeError("video encoder start failed".into()));
    }

    // ── Audio decoder / encoder ──────────────────────────────────────────
    let audio_decoder = if has_audio {
        let mime = audio_mime.as_ref().unwrap();
        let mime_c = CString::new(mime.as_str())
            .map_err(|e| Error::DecodeError(format!("invalid mime: {e}")))?;
        let d = AMediaCodec_createDecoderByType(mime_c.as_ptr());
        if d.is_null() {
            return Err(Error::DecodeError("createDecoderByType (audio) failed".into()));
        }
        d
    } else {
        std::ptr::null_mut()
    };
    let audio_encoder = if has_audio {
        let e = AMediaCodec_createEncoderByType(c"audio/mp4a-latm".as_ptr());
        if e.is_null() {
            return Err(Error::EncodeError("createEncoderByType (audio/mp4a-latm) failed".into()));
        }
        e
    } else {
        std::ptr::null_mut()
    };

    if has_audio {
        let fmt = AMediaExtractor_getTrackFormat(extractor, audio_track as usize);
        let ret = AMediaCodec_configure(
            audio_decoder,
            fmt,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        AMediaFormat_delete(fmt);
        if ret != AMEDIA_OK {
            AMediaCodec_delete(audio_decoder);
            AMediaCodec_delete(audio_encoder);
            AMediaCodec_delete(video_encoder);
            AMediaCodec_delete(video_decoder);
            AMediaExtractor_delete(extractor);
            return Err(Error::DecodeError(format!("audio decoder configure failed: {ret}")));
        }
        if AMediaCodec_start(audio_decoder) != AMEDIA_OK {
            return Err(Error::DecodeError("audio decoder start failed".into()));
        }

        let afmt = AMediaFormat_new();
        AMediaFormat_setString(afmt, c"mime".as_ptr(), c"audio/mp4a-latm".as_ptr());
        AMediaFormat_setInt32(afmt, c"sample-rate".as_ptr(), AUDIO_SAMPLE_RATE);
        AMediaFormat_setInt32(afmt, c"channel-count".as_ptr(), AUDIO_CHANNELS);
        AMediaFormat_setInt32(afmt, c"bitrate".as_ptr(), AUDIO_BITRATE);
        AMediaFormat_setInt32(afmt, c"aac-profile".as_ptr(), AUDIO_AAC_LC);
        let ret = AMediaCodec_configure(
            audio_encoder,
            afmt,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            AMEDIACODEC_CONFIGURE_FLAG_ENCODE,
        );
        AMediaFormat_delete(afmt);
        if ret != AMEDIA_OK {
            return Err(Error::EncodeError(format!("audio encoder configure failed: {ret}")));
        }
        if AMediaCodec_start(audio_encoder) != AMEDIA_OK {
            return Err(Error::EncodeError("audio encoder start failed".into()));
        }
    }

    // ── Muxer ────────────────────────────────────────────────────────────
    let muxer = AMediaMuxer_new(out_fd, AMEDIAMUXER_OUTPUT_FORMAT_MPEG_4);
    if muxer.is_null() {
        return Err(Error::EncodeError("AMediaMuxer_new failed".into()));
    }

    // ── Pump loop ────────────────────────────────────────────────────────
    let mut video_in_eos = false;
    let mut audio_in_eos = false;
    let mut video_out_eos = false;
    let mut audio_out_eos = false;
    let mut video_track_added = false;
    let mut audio_track_added = false;
    let mut muxer_started = false;
    let mut video_track_index: isize = -1;
    let mut audio_track_index: isize = -1;

    let mut dec_stride: usize = src_w;
    let mut dec_slice_height: usize = src_h;
    let mut dec_color_format: i32 = yuv::COLOR_FORMAT_YUV420_FLEXIBLE;

    let mut sample_buf: Vec<u8> = vec![0u8; 4 * 1024 * 1024];
    let mut video_pending: Vec<(Vec<u8>, i64, u32)> = Vec::new();
    let mut audio_pending: Vec<(Vec<u8>, i64, u32)> = Vec::new();
    let mut pending_video_frame: Option<(Vec<u8>, i64)> = None;
    let mut pending_audio_frame: Option<(Vec<u8>, i64)> = None;

    let mut video_dec_info = AMediaCodecBufferInfo {
        offset: 0,
        size: 0,
        presentation_time_us: 0,
        flags: 0,
    };
    let mut video_enc_info = AMediaCodecBufferInfo {
        offset: 0,
        size: 0,
        presentation_time_us: 0,
        flags: 0,
    };
    let mut audio_dec_info = AMediaCodecBufferInfo {
        offset: 0,
        size: 0,
        presentation_time_us: 0,
        flags: 0,
    };
    let mut audio_enc_info = AMediaCodecBufferInfo {
        offset: 0,
        size: 0,
        presentation_time_us: 0,
        flags: 0,
    };

    loop {
        if video_out_eos && audio_out_eos {
            break;
        }

        // ── Extractor → decoders (route samples by track index) ─────────
        if !video_in_eos || (has_audio && !audio_in_eos) {
            let size = AMediaExtractor_readSampleData(extractor, sample_buf.as_mut_ptr(), sample_buf.len());
            if size < 0 {
                // All selected tracks exhausted.
                if !video_in_eos {
                    let idx = AMediaCodec_dequeueInputBuffer(video_decoder, DEQUEUE_TIMEOUT_US);
                    if idx >= 0 {
                        AMediaCodec_queueInputBuffer(
                            video_decoder,
                            idx as usize,
                            0,
                            0,
                            0,
                            AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
                        );
                        video_in_eos = true;
                    }
                }
                if has_audio && !audio_in_eos {
                    let idx = AMediaCodec_dequeueInputBuffer(audio_decoder, DEQUEUE_TIMEOUT_US);
                    if idx >= 0 {
                        AMediaCodec_queueInputBuffer(
                            audio_decoder,
                            idx as usize,
                            0,
                            0,
                            0,
                            AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
                        );
                        audio_in_eos = true;
                    }
                }
            } else {
                let track = AMediaExtractor_getSampleTrackIndex(extractor) as isize;
                let pts = AMediaExtractor_getSampleTime(extractor);
                let flags = AMediaExtractor_getSampleFlags(extractor);
                let mut consumed = false;

                if track == video_track {
                    let idx = AMediaCodec_dequeueInputBuffer(video_decoder, DEQUEUE_TIMEOUT_US);
                    if idx >= 0 {
                        let mut buf_size: usize = 0;
                        let buf = AMediaCodec_getInputBuffer(video_decoder, idx as usize, &mut buf_size);
                        if !buf.is_null() && (size as usize) <= buf_size {
                            std::ptr::copy_nonoverlapping(sample_buf.as_ptr(), buf, size as usize);
                            AMediaCodec_queueInputBuffer(
                                video_decoder,
                                idx as usize,
                                0,
                                size as usize,
                                pts as u64,
                                flags,
                            );
                            consumed = true;
                        }
                    }
                } else if has_audio && track == audio_track {
                    let idx = AMediaCodec_dequeueInputBuffer(audio_decoder, DEQUEUE_TIMEOUT_US);
                    if idx >= 0 {
                        let mut buf_size: usize = 0;
                        let buf = AMediaCodec_getInputBuffer(audio_decoder, idx as usize, &mut buf_size);
                        if !buf.is_null() && (size as usize) <= buf_size {
                            std::ptr::copy_nonoverlapping(sample_buf.as_ptr(), buf, size as usize);
                            AMediaCodec_queueInputBuffer(
                                audio_decoder,
                                idx as usize,
                                0,
                                size as usize,
                                pts as u64,
                                flags,
                            );
                            consumed = true;
                        }
                    }
                }

                if consumed {
                    AMediaExtractor_advance(extractor);
                }
            }
        }

        // ── Video: decoder → scale → encoder ────────────────────────────
        if pending_video_frame.is_none() {
            let idx = AMediaCodec_dequeueOutputBuffer(video_decoder, &mut video_dec_info, DEQUEUE_TIMEOUT_US);
            if idx == AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED {
                let fmt = AMediaCodec_getOutputFormat(video_decoder);
                if !fmt.is_null() {
                    dec_stride = format_get_int32(fmt, c"stride", src_w as i32) as usize;
                    dec_slice_height =
                        format_get_int32(fmt, c"slice-height", src_h as i32) as usize;
                    dec_color_format = format_get_int32(fmt, c"color-format", yuv::COLOR_FORMAT_YUV420_FLEXIBLE);
                    AMediaFormat_delete(fmt);
                }
            } else if idx >= 0 {
                let mut buf_size: usize = 0;
                let buf = AMediaCodec_getOutputBuffer(video_decoder, idx as usize, &mut buf_size);
                if !buf.is_null() {
                    let info = &video_dec_info;
                    let raw = std::slice::from_raw_parts(
                        buf.add(info.offset as usize),
                        info.size as usize,
                    );
                    let i420 = yuv::to_i420(
                        raw,
                        src_w,
                        src_h,
                        dec_stride,
                        dec_slice_height,
                        dec_color_format,
                    );
                    let scaled = if target_w != src_w || target_h != src_h {
                        yuv::scale_i420(&i420, src_w, src_h, target_w, target_h)
                    } else {
                        i420
                    };
                    pending_video_frame = Some((scaled, info.presentation_time_us));
                }
                AMediaCodec_releaseOutputBuffer(video_decoder, idx as usize, 0);
            }
        }

        if let Some((frame, pts)) = pending_video_frame.take() {
            let idx = AMediaCodec_dequeueInputBuffer(video_encoder, DEQUEUE_TIMEOUT_US);
            if idx >= 0 {
                let mut buf_size: usize = 0;
                let buf = AMediaCodec_getInputBuffer(video_encoder, idx as usize, &mut buf_size);
                if buf.is_null() || frame.len() > buf_size {
                    pending_video_frame = Some((frame, pts));
                } else {
                    std::ptr::copy_nonoverlapping(frame.as_ptr(), buf, frame.len());
                    AMediaCodec_queueInputBuffer(
                        video_encoder,
                        idx as usize,
                        0,
                        frame.len(),
                        pts as u64,
                        0,
                    );
                }
            } else {
                pending_video_frame = Some((frame, pts));
            }
        }

        // ── Video: encoder → muxer ──────────────────────────────────────
        {
            let idx = AMediaCodec_dequeueOutputBuffer(video_encoder, &mut video_enc_info, DEQUEUE_TIMEOUT_US);
            if idx == AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED {
                let fmt = AMediaCodec_getOutputFormat(video_encoder);
                if !fmt.is_null() {
                    video_track_index = AMediaMuxer_addTrack(muxer, fmt);
                    AMediaFormat_delete(fmt);
                    video_track_added = video_track_index >= 0;
                }
            } else if idx >= 0 {
                let info = &video_enc_info;
                if info.flags & AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG == 0 {
                    let mut buf_size: usize = 0;
                    let buf = AMediaCodec_getOutputBuffer(video_encoder, idx as usize, &mut buf_size);
                    if !buf.is_null() {
                        if muxer_started {
                            AMediaMuxer_writeSampleData(
                                muxer,
                                video_track_index as usize,
                                buf.add(info.offset as usize),
                                info,
                            );
                        } else {
                            let data = std::slice::from_raw_parts(
                                buf.add(info.offset as usize),
                                info.size as usize,
                            )
                            .to_vec();
                            video_pending.push((data, info.presentation_time_us, info.flags));
                        }
                    }
                }
                AMediaCodec_releaseOutputBuffer(video_encoder, idx as usize, 0);
                if info.flags & AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0 {
                    video_out_eos = true;
                }
            }
        }

        // ── Audio: decoder → encoder → muxer ────────────────────────────
        if has_audio {
            if pending_audio_frame.is_none() {
                let idx = AMediaCodec_dequeueOutputBuffer(audio_decoder, &mut audio_dec_info, DEQUEUE_TIMEOUT_US);
                if idx >= 0 {
                    let info = &audio_dec_info;
                    let mut buf_size: usize = 0;
                    let buf = AMediaCodec_getOutputBuffer(audio_decoder, idx as usize, &mut buf_size);
                    if !buf.is_null() {
                        let pcm = std::slice::from_raw_parts(
                            buf.add(info.offset as usize),
                            info.size as usize,
                        )
                        .to_vec();
                        pending_audio_frame = Some((pcm, info.presentation_time_us));
                    }
                    AMediaCodec_releaseOutputBuffer(audio_decoder, idx as usize, 0);
                }
            }

            if let Some((pcm, pts)) = pending_audio_frame.take() {
                let idx = AMediaCodec_dequeueInputBuffer(audio_encoder, DEQUEUE_TIMEOUT_US);
                if idx >= 0 {
                    let mut buf_size: usize = 0;
                    let buf = AMediaCodec_getInputBuffer(audio_encoder, idx as usize, &mut buf_size);
                    if buf.is_null() || pcm.len() > buf_size {
                        pending_audio_frame = Some((pcm, pts));
                    } else {
                        std::ptr::copy_nonoverlapping(pcm.as_ptr(), buf, pcm.len());
                        AMediaCodec_queueInputBuffer(
                            audio_encoder,
                            idx as usize,
                            0,
                            pcm.len(),
                            pts as u64,
                            0,
                        );
                    }
                } else {
                    pending_audio_frame = Some((pcm, pts));
                }
            }

            {
                let idx = AMediaCodec_dequeueOutputBuffer(audio_encoder, &mut audio_enc_info, DEQUEUE_TIMEOUT_US);
                if idx == AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED {
                    let fmt = AMediaCodec_getOutputFormat(audio_encoder);
                    if !fmt.is_null() {
                        audio_track_index = AMediaMuxer_addTrack(muxer, fmt);
                        AMediaFormat_delete(fmt);
                        audio_track_added = audio_track_index >= 0;
                    }
                } else if idx >= 0 {
                    let info = &audio_enc_info;
                    if info.flags & AMEDIACODEC_BUFFER_FLAG_CODEC_CONFIG == 0 {
                        let mut buf_size: usize = 0;
                        let buf = AMediaCodec_getOutputBuffer(audio_encoder, idx as usize, &mut buf_size);
                        if !buf.is_null() {
                            if muxer_started {
                                AMediaMuxer_writeSampleData(
                                    muxer,
                                    audio_track_index as usize,
                                    buf.add(info.offset as usize),
                                    info,
                                );
                            } else {
                                let data = std::slice::from_raw_parts(
                                    buf.add(info.offset as usize),
                                    info.size as usize,
                                )
                                .to_vec();
                                audio_pending.push((data, info.presentation_time_us, info.flags));
                            }
                        }
                    }
                    AMediaCodec_releaseOutputBuffer(audio_encoder, idx as usize, 0);
                    if info.flags & AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0 {
                        audio_out_eos = true;
                    }
                }
            }
        }

        // ── Start muxer once both tracks are added ──────────────────────
        if !muxer_started && video_track_added && (audio_track_added || !has_audio) {
            AMediaMuxer_start(muxer);
            muxer_started = true;
            for (data, pts, flags) in video_pending.drain(..) {
                let info = AMediaCodecBufferInfo {
                    offset: 0,
                    size: data.len() as i32,
                    presentation_time_us: pts,
                    flags,
                };
                AMediaMuxer_writeSampleData(muxer, video_track_index as usize, data.as_ptr(), &info);
            }
            for (data, pts, flags) in audio_pending.drain(..) {
                let info = AMediaCodecBufferInfo {
                    offset: 0,
                    size: data.len() as i32,
                    presentation_time_us: pts,
                    flags,
                };
                AMediaMuxer_writeSampleData(muxer, audio_track_index as usize, data.as_ptr(), &info);
            }
        }
    }

    // ── Finish ──────────────────────────────────────────────────────────
    AMediaMuxer_stop(muxer);
    AMediaMuxer_delete(muxer);

    AMediaCodec_stop(video_decoder);
    AMediaCodec_delete(video_decoder);
    AMediaCodec_stop(video_encoder);
    AMediaCodec_delete(video_encoder);
    if has_audio {
        AMediaCodec_stop(audio_decoder);
        AMediaCodec_delete(audio_decoder);
        AMediaCodec_stop(audio_encoder);
        AMediaCodec_delete(audio_encoder);
    }
    AMediaExtractor_delete(extractor);

    Ok(())
}
