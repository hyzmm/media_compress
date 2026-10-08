use std::ffi::{c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{compute_video_target_dimensions, VideoCompressOptions, VideoFormat};
use crate::error::Error;

// ---------------------------------------------------------------------------
// Opaque pointer type aliases
// ---------------------------------------------------------------------------

type Id = *mut c_void;
type Sel = *const c_void;
type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFURLRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFNumberRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFIndex = isize;

// ---------------------------------------------------------------------------
// Structs (passed by value across the ObjC / CoreMedia ABI)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTimeRange {
    start: CMTime,
    duration: CMTime,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGAffineTransform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
}

#[repr(C)]
struct CFDictionaryKeyCallBacks {
    version: i64,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
    hash: *const c_void,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const CF_NUMBER_SINT32_TYPE: i32 = 3;
const K_CMTIME_FLAGS_VALID: u32 = 1;
const K_AUDIO_FORMAT_MPEG4_AAC: i32 = 0x6161_6320; // "aac "
const K_AUDIO_FORMAT_LINEAR_PCM: i32 = 0x6c70_636d; // "lpcm"
const AUDIO_SAMPLE_RATE: i32 = 44100;
const AUDIO_CHANNELS: i32 = 2;
const AUDIO_BITRATE: i32 = 128_000;

const AV_ASSET_READER_STATUS_FAILED: isize = 3;
const AV_ASSET_WRITER_STATUS_FAILED: isize = 3;
const AV_ASSET_WRITER_STATUS_COMPLETED: isize = 2;

// ---------------------------------------------------------------------------
// FFI — Objective-C runtime
// ---------------------------------------------------------------------------

// `objc_msgSend` must be declared exactly once. rustc merges every declaration
// of the same symbol into a single LLVM declaration and picks one signature for
// it - including its `sret` attribute. Declaring one symbol with many different
// signatures (as this file used to) therefore lowers every call site with that
// one signature: the receiver ends up in x8 instead of x0, the selector moves to
// x0, and the process crashes inside objc_msgSend. Each helper below instead
// transmutes the symbol address to the exact signature it needs, the same
// approach the `objc2` crate uses for `msg_send!`.
#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);

    #[link_name = "objc_msgSend"]
    fn objc_msg_send_raw();
}

macro_rules! define_msg_send {
    ($($name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
        $(
            #[inline(always)]
            unsafe fn $name(receiver: Id, sel: Sel $(, $arg: $ty)*) $(-> $ret)? {
                let send: unsafe extern "C" fn(Id, Sel $(, $ty)*) $(-> $ret)? =
                    std::mem::transmute(objc_msg_send_raw as unsafe extern "C" fn());
                send(receiver, sel $(, $arg)*)
            }
        )*
    };
}

define_msg_send! {
    msg_id() -> Id;
    msg_id1(a: Id) -> Id;
    msg_id2(a: Id, b: Id) -> Id;
    msg_id_err(a: Id, err: *mut *mut c_void) -> Id;
    msg_id2_err(a: Id, b: Id, err: *mut *mut c_void) -> Id;
    msg_void();
    msg_void1(a: Id);
    msg_bool() -> u8;
    msg_bool1(a: Id) -> u8;
    msg_isize() -> isize;
    msg_usize() -> usize;
    msg_size() -> CGSize;
    msg_set_size(size: CGSize);
    msg_set_cmtime(time: CMTime);
    msg_set_transform(transform: CGAffineTransform, time: CMTime);
    msg_float() -> f32;
    msg_set_timerange(range: CMTimeRange);
}

// Large-struct return differs by arch: x86_64 uses the explicit stret
// trampoline, aarch64 returns indirectly via x8 (plain objc_msgSend).
#[cfg(target_arch = "x86_64")]
#[link(name = "objc", kind = "dylib")]
extern "C" {
    #[link_name = "objc_msgSend_stret"]
    fn msg_stret_cmtime(ret: *mut CMTime, receiver: Id, sel: Sel);
}

#[cfg(target_arch = "aarch64")]
define_msg_send! {
    msg_cmtime() -> CMTime;
}

// ---------------------------------------------------------------------------
// FFI — CoreFoundation
// ---------------------------------------------------------------------------

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFAllocatorRef,
        buffer: *const u8,
        buf_len: CFIndex,
        is_directory: u8,
    ) -> CFURLRef;
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: CFIndex,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryKeyCallBacks,
    ) -> CFDictionaryRef;
    fn CFNumberCreate(allocator: CFAllocatorRef, the_type: i32, value_ptr: *const c_void) -> CFNumberRef;

    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryKeyCallBacks;
}

// ---------------------------------------------------------------------------
// FFI — CoreGraphics
// ---------------------------------------------------------------------------

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGAffineTransformMakeScale(sx: f64, sy: f64) -> CGAffineTransform;
}

// ---------------------------------------------------------------------------
// FFI — AVFoundation string constants (exported NSString* symbols)
// ---------------------------------------------------------------------------

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {
    static AVMediaTypeVideo: CFStringRef;
    static AVMediaTypeAudio: CFStringRef;
    static AVVideoCodecKey: CFStringRef;
    static AVVideoWidthKey: CFStringRef;
    static AVVideoHeightKey: CFStringRef;
    static AVVideoCompressionPropertiesKey: CFStringRef;
    static AVVideoAverageBitRateKey: CFStringRef;
    static AVVideoProfileLevelKey: CFStringRef;
    static AVVideoProfileLevelH264MainAutoLevel: CFStringRef;
    static AVFormatIDKey: CFStringRef;
    static AVSampleRateKey: CFStringRef;
    static AVNumberOfChannelsKey: CFStringRef;
    static AVEncoderBitRateKey: CFStringRef;
    static AVFileTypeMPEG4: CFStringRef;
    static AVVideoCodecTypeH264: CFStringRef;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct AutoreleasePool(*mut c_void);

impl AutoreleasePool {
    fn new() -> Self {
        AutoreleasePool(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}

unsafe fn class(name: &'static std::ffi::CStr) -> Id {
    objc_getClass(name.as_ptr())
}

unsafe fn sel(name: &'static std::ffi::CStr) -> Sel {
    sel_registerName(name.as_ptr())
}

/// A numeric zero CMTime (flags = Valid). Required by APIs like
/// `setTransform:atTime:` which throw when the time is not numeric.
fn cmtime_numeric_zero() -> CMTime {
    CMTime {
        value: 0,
        timescale: 1,
        flags: K_CMTIME_FLAGS_VALID,
        epoch: 0,
    }
}

unsafe fn asset_duration(asset: Id) -> CMTime {
    #[cfg(target_arch = "x86_64")]
    {
        let mut t = CMTime {
            value: 0,
            timescale: 0,
            flags: 0,
            epoch: 0,
        };
        msg_stret_cmtime(&mut t, asset, sel(c"duration"));
        t
    }
    #[cfg(target_arch = "aarch64")]
    {
        msg_cmtime(asset, sel(c"duration"))
    }
}

unsafe fn cf_number_i32(v: i32) -> CFNumberRef {
    CFNumberCreate(
        std::ptr::null(),
        CF_NUMBER_SINT32_TYPE,
        &v as *const i32 as *const c_void,
    )
}

unsafe fn cf_dictionary(keys: &[*const c_void], values: &[*const c_void]) -> CFDictionaryRef {
    CFDictionaryCreate(
        std::ptr::null(),
        keys.as_ptr(),
        values.as_ptr(),
        keys.len() as CFIndex,
        &kCFTypeDictionaryKeyCallBacks,
        &kCFTypeDictionaryValueCallBacks,
    )
}

unsafe fn file_url(path: &Path) -> Result<CFURLRef, Error> {
    let bytes = path.as_os_str().as_bytes();
    let url = CFURLCreateFromFileSystemRepresentation(
        std::ptr::null(),
        bytes.as_ptr(),
        bytes.len() as CFIndex,
        0,
    );
    if url.is_null() {
        Err(Error::EncodeError(
            "CFURLCreateFromFileSystemRepresentation failed".into(),
        ))
    } else {
        Ok(url)
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

fn detected_extension(input: &[u8]) -> &'static str {
    match VideoFormat::detect(input) {
        Some(VideoFormat::QuickTime) => "mov",
        Some(VideoFormat::Webm) => "webm",
        Some(VideoFormat::Avi) => "avi",
        _ => "mp4",
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn compress(input: &[u8], options: VideoCompressOptions) -> Result<Vec<u8>, Error> {
    let in_path = unique_temp_path(detected_extension(input));
    let out_path = unique_temp_path("mp4");

    std::fs::write(&in_path, input)
        .map_err(|e| Error::EncodeError(format!("failed to write temp input: {e}")))?;

    let result = unsafe { transcode(&in_path, &out_path, options) };

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

unsafe fn transcode(
    in_path: &Path,
    out_path: &Path,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    let _pool = AutoreleasePool::new();

    let in_url = file_url(in_path)?;
    let out_url = file_url(out_path)?;

    let result = (|| -> Result<(), Error> {
        // ── Source asset ─────────────────────────────────────────────────
        let asset = msg_id2(
            class(c"AVURLAsset"),
            sel(c"URLAssetWithURL:options:"),
            in_url as Id,
            std::ptr::null_mut(),
        );
        if asset.is_null() {
            return Err(Error::DecodeError("AVURLAsset creation failed".into()));
        }

        // ── Video track + natural size ───────────────────────────────────
        let video_tracks = msg_id1(asset, sel(c"tracksWithMediaType:"), AVMediaTypeVideo as Id);
        if video_tracks.is_null() || msg_usize(video_tracks, sel(c"count")) == 0 {
            return Err(Error::DecodeError("no video track found".into()));
        }
        let video_track = msg_id(video_tracks, sel(c"firstObject"));

        let src_size = msg_size(video_track, sel(c"naturalSize"));
        let src_w = src_size.width.max(0.0) as u32;
        let src_h = src_size.height.max(0.0) as u32;
        if src_w == 0 || src_h == 0 {
            return Err(Error::DecodeError("video track has zero dimensions".into()));
        }
        let (target_w, target_h) = compute_video_target_dimensions(
            src_w,
            src_h,
            options.max_width,
            options.max_height,
        );

        // ── Optional audio track ─────────────────────────────────────────
        let audio_tracks = msg_id1(asset, sel(c"tracksWithMediaType:"), AVMediaTypeAudio as Id);
        let has_audio = !audio_tracks.is_null() && msg_usize(audio_tracks, sel(c"count")) > 0;
        let audio_track = if has_audio {
            msg_id(audio_tracks, sel(c"firstObject"))
        } else {
            std::ptr::null_mut()
        };

        // ── Video composition (scaling) ──────────────────────────────────
        let comp = msg_id(class(c"AVMutableVideoComposition"), sel(c"videoComposition"));
        if comp.is_null() {
            return Err(Error::EncodeError(
                "AVMutableVideoComposition creation failed".into(),
            ));
        }

        // Frame duration from the track's nominal frame rate (default 30fps).
        let fps = msg_float(video_track, sel(c"nominalFrameRate"));
        let timescale = if fps.is_finite() && fps > 1.0 {
            fps.round() as i32
        } else {
            30
        };
        msg_set_cmtime(
            comp,
            sel(c"setFrameDuration:"),
            CMTime {
                value: 1,
                timescale,
                flags: K_CMTIME_FLAGS_VALID,
                epoch: 0,
            },
        );
        msg_set_size(
            comp,
            sel(c"setRenderSize:"),
            CGSize {
                width: target_w as f64,
                height: target_h as f64,
            },
        );

        // Single instruction covering the whole asset, with a scale transform.
        let instruction = msg_id(
            class(c"AVMutableVideoCompositionInstruction"),
            sel(c"videoCompositionInstruction"),
        );
        if instruction.is_null() {
            return Err(Error::EncodeError(
                "videoCompositionInstruction creation failed".into(),
            ));
        }
        let duration = asset_duration(asset);
        msg_set_timerange(
            instruction,
            sel(c"setTimeRange:"),
            CMTimeRange {
                start: cmtime_numeric_zero(),
                duration,
            },
        );

        let layer = msg_id1(
            class(c"AVMutableVideoCompositionLayerInstruction"),
            sel(c"videoCompositionLayerInstructionWithAssetTrack:"),
            video_track,
        );
        if layer.is_null() {
            return Err(Error::EncodeError(
                "videoCompositionLayerInstruction creation failed".into(),
            ));
        }
        let sx = target_w as f64 / src_w as f64;
        let sy = target_h as f64 / src_h as f64;
        let transform = CGAffineTransformMakeScale(sx, sy);
        msg_set_transform(layer, sel(c"setTransform:atTime:"), transform, cmtime_numeric_zero());

        let layer_array = msg_id1(class(c"NSArray"), sel(c"arrayWithObject:"), layer);
        msg_void1(instruction, sel(c"setLayerInstructions:"), layer_array);
        let inst_array = msg_id1(class(c"NSArray"), sel(c"arrayWithObject:"), instruction);
        msg_void1(comp, sel(c"setInstructions:"), inst_array);

        // ── Reader ───────────────────────────────────────────────────────
        let reader = msg_id_err(
            class(c"AVAssetReader"),
            sel(c"assetReaderWithAsset:error:"),
            asset,
            std::ptr::null_mut(),
        );
        if reader.is_null() {
            return Err(Error::DecodeError("AVAssetReader creation failed".into()));
        }

        let video_output = msg_id2(
            class(c"AVAssetReaderVideoCompositionOutput"),
            sel(c"assetReaderVideoCompositionOutputWithVideoTracks:videoSettings:"),
            video_tracks,
            std::ptr::null_mut(),
        );
        if video_output.is_null() {
            return Err(Error::DecodeError(
                "video reader output creation failed".into(),
            ));
        }
        msg_void1(video_output, sel(c"setVideoComposition:"), comp);
        msg_void1(reader, sel(c"addOutput:"), video_output);

        let audio_output = if has_audio {
            let lpcm_num = cf_number_i32(K_AUDIO_FORMAT_LINEAR_PCM);
            let sr_num = cf_number_i32(AUDIO_SAMPLE_RATE);
            let ch_num = cf_number_i32(AUDIO_CHANNELS);
            let lpcm_dict = cf_dictionary(
                &[AVFormatIDKey, AVSampleRateKey, AVNumberOfChannelsKey],
                &[lpcm_num, sr_num, ch_num],
            );
            let out = msg_id2(
                class(c"AVAssetReaderTrackOutput"),
                sel(c"assetReaderTrackOutputWithTrack:outputSettings:"),
                audio_track,
                lpcm_dict as Id,
            );
            CFRelease(lpcm_dict);
            CFRelease(lpcm_num);
            CFRelease(sr_num);
            CFRelease(ch_num);
            if out.is_null() {
                return Err(Error::DecodeError(
                    "audio reader output creation failed".into(),
                ));
            }
            msg_void1(reader, sel(c"addOutput:"), out);
            out
        } else {
            std::ptr::null_mut()
        };

        // ── Writer ───────────────────────────────────────────────────────
        let writer = msg_id2_err(
            class(c"AVAssetWriter"),
            sel(c"assetWriterWithURL:fileType:error:"),
            out_url as Id,
            AVFileTypeMPEG4 as Id,
            std::ptr::null_mut(),
        );
        if writer.is_null() {
            return Err(Error::EncodeError("AVAssetWriter creation failed".into()));
        }

        let bitrate_bps = (options.bitrate_kbps * 1000) as i32;
        let w_num = cf_number_i32(target_w as i32);
        let h_num = cf_number_i32(target_h as i32);
        let bitrate_num = cf_number_i32(bitrate_bps);
        let compression = cf_dictionary(
            &[AVVideoAverageBitRateKey, AVVideoProfileLevelKey],
            &[bitrate_num, AVVideoProfileLevelH264MainAutoLevel],
        );
        let video_settings = cf_dictionary(
            &[
                AVVideoCodecKey,
                AVVideoWidthKey,
                AVVideoHeightKey,
                AVVideoCompressionPropertiesKey,
            ],
            &[AVVideoCodecTypeH264, w_num, h_num, compression],
        );
        CFRelease(compression);
        CFRelease(bitrate_num);

        let video_input = msg_id2(
            class(c"AVAssetWriterInput"),
            sel(c"assetWriterInputWithMediaType:outputSettings:"),
            AVMediaTypeVideo as Id,
            video_settings as Id,
        );
        CFRelease(video_settings);
        CFRelease(w_num);
        CFRelease(h_num);
        if video_input.is_null() {
            return Err(Error::EncodeError(
                "video writer input creation failed".into(),
            ));
        }
        msg_void1(writer, sel(c"addInput:"), video_input);

        let audio_input = if has_audio {
            let aac_num = cf_number_i32(K_AUDIO_FORMAT_MPEG4_AAC);
            let sr_num = cf_number_i32(AUDIO_SAMPLE_RATE);
            let ch_num = cf_number_i32(AUDIO_CHANNELS);
            let abr_num = cf_number_i32(AUDIO_BITRATE);
            let audio_settings = cf_dictionary(
                &[
                    AVFormatIDKey,
                    AVSampleRateKey,
                    AVNumberOfChannelsKey,
                    AVEncoderBitRateKey,
                ],
                &[aac_num, sr_num, ch_num, abr_num],
            );
            let input = msg_id2(
                class(c"AVAssetWriterInput"),
                sel(c"assetWriterInputWithMediaType:outputSettings:"),
                AVMediaTypeAudio as Id,
                audio_settings as Id,
            );
            CFRelease(audio_settings);
            CFRelease(aac_num);
            CFRelease(sr_num);
            CFRelease(ch_num);
            CFRelease(abr_num);
            if input.is_null() {
                return Err(Error::EncodeError(
                    "audio writer input creation failed".into(),
                ));
            }
            msg_void1(writer, sel(c"addInput:"), input);
            input
        } else {
            std::ptr::null_mut()
        };

        // ── Start ────────────────────────────────────────────────────────
        if msg_bool(reader, sel(c"startReading")) == 0 {
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::DecodeError("AVAssetReader startReading failed".into()));
        }
        if msg_bool(writer, sel(c"startWriting")) == 0 {
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::EncodeError("AVAssetWriter startWriting failed".into()));
        }
        msg_set_cmtime(writer, sel(c"startSessionAtSourceTime:"), cmtime_numeric_zero());

        // ── Pump samples ─────────────────────────────────────────────────
        let mut video_done = false;
        let mut audio_done = !has_audio;

        loop {
            if video_done && audio_done {
                break;
            }

            let mut progressed = false;

            if !video_done && msg_bool(video_input, sel(c"isReadyForMoreMediaData")) != 0 {
                let sample = msg_id(video_output, sel(c"copyNextSampleBuffer"));
                if sample.is_null() {
                    msg_void(video_input, sel(c"markAsFinished"));
                    video_done = true;
                } else {
                    let ok = msg_bool1(video_input, sel(c"appendSampleBuffer:"), sample);
                    CFRelease(sample);
                    if ok == 0 {
                        msg_void(writer, sel(c"cancelWriting"));
                        return Err(Error::EncodeError(
                            "appendSampleBuffer (video) failed".into(),
                        ));
                    }
                    progressed = true;
                }
            }

            if !audio_done && msg_bool(audio_input, sel(c"isReadyForMoreMediaData")) != 0 {
                let sample = msg_id(audio_output, sel(c"copyNextSampleBuffer"));
                if sample.is_null() {
                    msg_void(audio_input, sel(c"markAsFinished"));
                    audio_done = true;
                } else {
                    let ok = msg_bool1(audio_input, sel(c"appendSampleBuffer:"), sample);
                    CFRelease(sample);
                    if ok == 0 {
                        msg_void(writer, sel(c"cancelWriting"));
                        return Err(Error::EncodeError(
                            "appendSampleBuffer (audio) failed".into(),
                        ));
                    }
                    progressed = true;
                }
            }

            if !progressed {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }

        // ── Finish ───────────────────────────────────────────────────────
        if msg_isize(reader, sel(c"status")) == AV_ASSET_READER_STATUS_FAILED {
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::DecodeError("AVAssetReader failed during reading".into()));
        }
        if msg_isize(writer, sel(c"status")) == AV_ASSET_WRITER_STATUS_FAILED {
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::EncodeError("AVAssetWriter failed during writing".into()));
        }
        if msg_bool(writer, sel(c"finishWriting")) == 0 {
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::EncodeError("AVAssetWriter finishWriting failed".into()));
        }
        if msg_isize(writer, sel(c"status")) != AV_ASSET_WRITER_STATUS_COMPLETED {
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::EncodeError("AVAssetWriter did not complete".into()));
        }

        Ok(())
    })();

    CFRelease(in_url);
    CFRelease(out_url);
    result
}
