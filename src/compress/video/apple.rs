use std::ffi::{c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::transform::{display_size_u32, layer_transform, CGAffineTransform};
use super::{
    compute_video_target_dimensions, crf_to_quality_unit, H264Target, VideoCompressOptions,
    VideoFormat,
};
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
struct CMTimeMapping {
    source: CMTimeRange,
    target: CMTimeRange,
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
const CF_NUMBER_FLOAT32_TYPE: i32 = 5;
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
    msg_id_index(index: usize) -> Id;
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
// As with `objc_msgSend` above, the symbol is declared exactly once and each
// helper transmutes it to the exact signature it needs.
#[cfg(target_arch = "x86_64")]
#[link(name = "objc", kind = "dylib")]
extern "C" {
    #[link_name = "objc_msgSend_stret"]
    fn objc_msg_send_stret_raw();
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn msg_stret_cmtime(ret: *mut CMTime, receiver: Id, sel: Sel) {
    let send: unsafe extern "C" fn(*mut CMTime, Id, Sel) =
        std::mem::transmute(objc_msg_send_stret_raw as unsafe extern "C" fn());
    send(ret, receiver, sel);
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn msg_stret_transform(ret: *mut CGAffineTransform, receiver: Id, sel: Sel) {
    let send: unsafe extern "C" fn(*mut CGAffineTransform, Id, Sel) =
        std::mem::transmute(objc_msg_send_stret_raw as unsafe extern "C" fn());
    send(ret, receiver, sel);
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn msg_stret_mapping(ret: *mut CMTimeMapping, receiver: Id, sel: Sel) {
    let send: unsafe extern "C" fn(*mut CMTimeMapping, Id, Sel) =
        std::mem::transmute(objc_msg_send_stret_raw as unsafe extern "C" fn());
    send(ret, receiver, sel);
}

#[cfg(target_arch = "aarch64")]
define_msg_send! {
    msg_cmtime() -> CMTime;
    msg_transform() -> CGAffineTransform;
    msg_mapping() -> CMTimeMapping;
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
    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: i32,
        value_ptr: *const c_void,
    ) -> CFNumberRef;

    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryKeyCallBacks;
}

// ---------------------------------------------------------------------------
// FFI — CoreMedia time arithmetic
// ---------------------------------------------------------------------------

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMTimeAdd(lhs: CMTime, rhs: CMTime) -> CMTime;
    fn CMTimeSubtract(lhs: CMTime, rhs: CMTime) -> CMTime;
    fn CMTimeCompare(lhs: CMTime, rhs: CMTime) -> i32;
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
    static AVVideoProfileLevelKey: CFStringRef;
    static AVVideoProfileLevelH264MainAutoLevel: CFStringRef;
    static AVFormatIDKey: CFStringRef;
    static AVSampleRateKey: CFStringRef;
    static AVNumberOfChannelsKey: CFStringRef;
    static AVEncoderBitRateKey: CFStringRef;
    static AVFileTypeMPEG4: CFStringRef;
    static AVVideoCodecTypeH264: CFStringRef;
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    static kVTCompressionPropertyKey_Quality: CFStringRef;
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

unsafe fn cmtime_property(object: Id, selector: Sel) -> CMTime {
    #[cfg(target_arch = "x86_64")]
    {
        let mut t = CMTime {
            value: 0,
            timescale: 0,
            flags: 0,
            epoch: 0,
        };
        msg_stret_cmtime(&mut t, object, selector);
        t
    }
    #[cfg(target_arch = "aarch64")]
    {
        msg_cmtime(object, selector)
    }
}

fn cmtime_is_valid(t: CMTime) -> bool {
    t.flags & K_CMTIME_FLAGS_VALID != 0 && t.timescale > 0
}

fn cmtime_compare(lhs: CMTime, rhs: CMTime) -> i32 {
    unsafe { CMTimeCompare(lhs, rhs) }
}

/// Earliest non-negative start time among the supplied track ranges.
///
/// `AVAssetWriter.startSessionAtSourceTime` rebases the output timeline to
/// this value. Using the earliest track start avoids emitting a long frozen
/// frame or silence before the real first sample, while still preserving the
/// relative offset when one track starts later than another.
fn earliest_track_start(ranges: &[CMTimeRange]) -> CMTime {
    let zero = cmtime_numeric_zero();
    let mut earliest: Option<CMTime> = None;
    for range in ranges {
        if !cmtime_is_valid(range.start) {
            continue;
        }
        if earliest.is_none_or(|current| cmtime_compare(range.start, current) < 0) {
            earliest = Some(range.start);
        }
    }
    match earliest {
        Some(start) if cmtime_compare(start, zero) >= 0 => start,
        _ => zero,
    }
}

unsafe fn cmtime_mapping_property(object: Id, selector: Sel) -> CMTimeMapping {
    #[cfg(target_arch = "x86_64")]
    {
        let mut mapping = CMTimeMapping {
            source: CMTimeRange {
                start: cmtime_numeric_zero(),
                duration: cmtime_numeric_zero(),
            },
            target: CMTimeRange {
                start: cmtime_numeric_zero(),
                duration: cmtime_numeric_zero(),
            },
        };
        msg_stret_mapping(&mut mapping, object, selector);
        mapping
    }
    #[cfg(target_arch = "aarch64")]
    {
        msg_mapping(object, selector)
    }
}

/// Time range covered by the track's non-empty segments.
///
/// `AVAssetTrack.timeRange` starts at zero and stretches across leading empty
/// edits, which makes a late-starting video look like it begins at zero. The
/// segments' `timeMapping.target` values identify where real media actually
/// starts and ends.
unsafe fn track_media_range(track: Id) -> Option<CMTimeRange> {
    let segments = msg_id(track, sel(c"segments"));
    if segments.is_null() {
        return None;
    }

    let count = msg_usize(segments, sel(c"count"));
    let mut start: Option<CMTime> = None;
    let mut end: Option<CMTime> = None;

    for index in 0..count {
        let segment = msg_id_index(segments, sel(c"objectAtIndex:"), index);
        if segment.is_null() || msg_bool(segment, sel(c"isEmpty")) != 0 {
            continue;
        }

        let mapping = cmtime_mapping_property(segment, sel(c"timeMapping"));
        let target = mapping.target;
        if !cmtime_is_valid(target.start)
            || !cmtime_is_valid(target.duration)
            || target.duration.value <= 0
        {
            continue;
        }

        let segment_end = CMTimeAdd(target.start, target.duration);
        if !cmtime_is_valid(segment_end) {
            continue;
        }

        if start.is_none_or(|current| cmtime_compare(target.start, current) < 0) {
            start = Some(target.start);
        }
        if end.is_none_or(|current| cmtime_compare(segment_end, current) > 0) {
            end = Some(segment_end);
        }
    }

    let (start, end) = (start?, end?);
    let duration = CMTimeSubtract(end, start);
    if !cmtime_is_valid(duration) || cmtime_compare(duration, cmtime_numeric_zero()) <= 0 {
        return None;
    }
    Some(CMTimeRange { start, duration })
}

/// Restrict a reader to a track's real media range.
///
/// Without this, a reader outputs the track's leading empty edit and
/// `AVAssetReaderVideoCompositionOutput` holds the first real frame across
/// the whole gap. A per-track reader can simply start at the first sample;
/// the writer then preserves the offset relative to the other track.
unsafe fn set_reader_time_range(reader: Id, range: Option<CMTimeRange>) {
    let Some(range) = usable_reader_range(range) else {
        return;
    };
    msg_set_timerange(reader, sel(c"setTimeRange:"), range);
}

fn usable_reader_range(range: Option<CMTimeRange>) -> Option<CMTimeRange> {
    let range = range?;
    let zero = cmtime_numeric_zero();
    if !cmtime_is_valid(range.start)
        || !cmtime_is_valid(range.duration)
        || cmtime_compare(range.start, zero) < 0
        || cmtime_compare(range.duration, zero) <= 0
    {
        return None;
    }
    Some(range)
}

fn usable_frame_duration(t: CMTime) -> bool {
    if t.flags & K_CMTIME_FLAGS_VALID == 0 || t.value <= 0 || t.timescale <= 0 {
        return false;
    }
    let fps = t.timescale as f64 / t.value as f64;
    (1.0..=240.0).contains(&fps)
}

unsafe fn asset_duration(asset: Id) -> CMTime {
    cmtime_property(asset, sel(c"duration"))
}

/// The track's display orientation/scale relative to its encoded pixels.
unsafe fn track_preferred_transform(track: Id) -> CGAffineTransform {
    #[cfg(target_arch = "x86_64")]
    {
        let mut transform = CGAffineTransform::IDENTITY;
        msg_stret_transform(&mut transform, track, sel(c"preferredTransform"));
        transform
    }
    #[cfg(target_arch = "aarch64")]
    {
        msg_transform(track, sel(c"preferredTransform"))
    }
}

unsafe fn cf_number_i32(v: i32) -> CFNumberRef {
    CFNumberCreate(
        std::ptr::null(),
        CF_NUMBER_SINT32_TYPE,
        &v as *const i32 as *const c_void,
    )
}

unsafe fn cf_number_f32(v: f32) -> CFNumberRef {
    CFNumberCreate(
        std::ptr::null(),
        CF_NUMBER_FLOAT32_TYPE,
        &v as *const f32 as *const c_void,
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

    let result = unsafe { transcode(&in_path, &out_path, H264Target::compress(options)) };

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
/// Neither file is buffered in memory; AVFoundation streams samples.
pub fn compress_file(
    input: &Path,
    output: &Path,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    unsafe { transcode(input, output, H264Target::compress(options)) }
}

// ---------------------------------------------------------------------------
// Transcode implementation
// ---------------------------------------------------------------------------

unsafe fn transcode(in_path: &Path, out_path: &Path, target: H264Target) -> Result<(), Error> {
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

        // The composition does not apply the track's `preferredTransform`; keep
        // it here and fold it into the layer instruction below.
        let preferred = track_preferred_transform(video_track);
        let (display_w, display_h) = display_size_u32(src_w, src_h, preferred);
        let (target_w, target_h) = compute_video_target_dimensions(display_w, display_h);

        // ── Optional audio track ─────────────────────────────────────────
        let audio_tracks = msg_id1(asset, sel(c"tracksWithMediaType:"), AVMediaTypeAudio as Id);
        let has_audio = !audio_tracks.is_null() && msg_usize(audio_tracks, sel(c"count")) > 0;
        let audio_track = if has_audio {
            msg_id(audio_tracks, sel(c"firstObject"))
        } else {
            std::ptr::null_mut()
        };

        // Leading empty edits are common in recordings and exports. Use the
        // actual media ranges so the composition does not hold the first frame
        // across the empty edit, and start the writer session at the earliest
        // real sample.
        let video_media_range = track_media_range(video_track);
        let audio_media_range = has_audio.then(|| track_media_range(audio_track)).flatten();
        let mut track_starts = Vec::with_capacity(2);
        if let Some(range) = video_media_range {
            track_starts.push(range);
        }
        if let Some(range) = audio_media_range {
            track_starts.push(range);
        }
        let session_start = earliest_track_start(&track_starts);
        let asset_end = asset_duration(asset);
        let composition_range = video_media_range.unwrap_or_else(|| CMTimeRange {
            start: cmtime_numeric_zero(),
            duration: asset_end,
        });

        // ── Video composition (scaling) ──────────────────────────────────
        let comp = msg_id(
            class(c"AVMutableVideoComposition"),
            sel(c"videoComposition"),
        );
        if comp.is_null() {
            return Err(Error::EncodeError(
                "AVMutableVideoComposition creation failed".into(),
            ));
        }

        // Keep the track's exact frame duration. `nominalFrameRate` is a float,
        // and rounding it to an integer turns 23.976 / 29.97 / 59.94 sources
        // into 24 / 30 / 60 fps. The composition would then render at the
        // wrong cadence, making the video stream drift away from the audio.
        let min_frame_duration = cmtime_property(video_track, sel(c"minFrameDuration"));
        let frame_duration = if usable_frame_duration(min_frame_duration) {
            min_frame_duration
        } else {
            let fps = msg_float(video_track, sel(c"nominalFrameRate"));
            CMTime {
                value: 1,
                timescale: if fps.is_finite() && fps > 1.0 {
                    fps.round() as i32
                } else {
                    30
                },
                flags: K_CMTIME_FLAGS_VALID,
                epoch: 0,
            }
        };
        msg_set_cmtime(comp, sel(c"setFrameDuration:"), frame_duration);
        msg_set_size(
            comp,
            sel(c"setRenderSize:"),
            CGSize {
                width: target_w as f64,
                height: target_h as f64,
            },
        );

        // Single instruction covering the track's real media range, with a
        // scale transform.
        let instruction = msg_id(
            class(c"AVMutableVideoCompositionInstruction"),
            sel(c"videoCompositionInstruction"),
        );
        if instruction.is_null() {
            return Err(Error::EncodeError(
                "videoCompositionInstruction creation failed".into(),
            ));
        }
        msg_set_timerange(instruction, sel(c"setTimeRange:"), composition_range);

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
        let transform = layer_transform(src_w as f64, src_h as f64, preferred, target_w, target_h);
        msg_set_transform(
            layer,
            sel(c"setTransform:atTime:"),
            transform,
            cmtime_numeric_zero(),
        );

        let layer_array = msg_id1(class(c"NSArray"), sel(c"arrayWithObject:"), layer);
        msg_void1(instruction, sel(c"setLayerInstructions:"), layer_array);

        let instructions = msg_id1(class(c"NSArray"), sel(c"arrayWithObject:"), instruction);
        msg_void1(comp, sel(c"setInstructions:"), instructions);

        // ── Video reader ─────────────────────────────────────────────────
        // Reading video and audio through one reader forces the video
        // composition to span the union of both tracks. Empty edits then make
        // AVAssetReader hold the first real frame (or emit a sample before the
        // writer session) across the gap. A dedicated reader per track lets
        // each one start at its own first real sample; the writer keeps the
        // relative offset because the session starts at the earliest track.
        let video_reader = msg_id_err(
            class(c"AVAssetReader"),
            sel(c"assetReaderWithAsset:error:"),
            asset,
            std::ptr::null_mut(),
        );
        if video_reader.is_null() {
            return Err(Error::DecodeError(
                "AVAssetReader (video) creation failed".into(),
            ));
        }
        set_reader_time_range(video_reader, video_media_range);

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
        msg_void1(video_reader, sel(c"addOutput:"), video_output);

        let audio_reader = if has_audio {
            let reader = msg_id_err(
                class(c"AVAssetReader"),
                sel(c"assetReaderWithAsset:error:"),
                asset,
                std::ptr::null_mut(),
            );
            if reader.is_null() {
                return Err(Error::DecodeError(
                    "AVAssetReader (audio) creation failed".into(),
                ));
            }
            set_reader_time_range(reader, audio_media_range);
            reader
        } else {
            std::ptr::null_mut()
        };

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
            msg_void1(audio_reader, sel(c"addOutput:"), out);
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

        let w_num = cf_number_i32(target_w as i32);
        let h_num = cf_number_i32(target_h as i32);
        let quality_num = cf_number_f32(crf_to_quality_unit(target.crf));
        let compression = cf_dictionary(
            &[kVTCompressionPropertyKey_Quality, AVVideoProfileLevelKey],
            &[quality_num, AVVideoProfileLevelH264MainAutoLevel],
        );
        CFRelease(quality_num);
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
        if msg_bool(video_reader, sel(c"startReading")) == 0 {
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::DecodeError(
                "AVAssetReader (video) startReading failed".into(),
            ));
        }
        if has_audio && msg_bool(audio_reader, sel(c"startReading")) == 0 {
            msg_void(video_reader, sel(c"cancelReading"));
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::DecodeError(
                "AVAssetReader (audio) startReading failed".into(),
            ));
        }
        if msg_bool(writer, sel(c"startWriting")) == 0 {
            if has_audio {
                msg_void(audio_reader, sel(c"cancelReading"));
            }
            msg_void(video_reader, sel(c"cancelReading"));
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::EncodeError(
                "AVAssetWriter startWriting failed".into(),
            ));
        }
        msg_set_cmtime(writer, sel(c"startSessionAtSourceTime:"), session_start);

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
        if msg_isize(video_reader, sel(c"status")) == AV_ASSET_READER_STATUS_FAILED
            || (has_audio
                && msg_isize(audio_reader, sel(c"status")) == AV_ASSET_READER_STATUS_FAILED)
        {
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::DecodeError(
                "AVAssetReader failed during reading".into(),
            ));
        }
        if msg_isize(writer, sel(c"status")) == AV_ASSET_WRITER_STATUS_FAILED {
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::EncodeError(
                "AVAssetWriter failed during writing".into(),
            ));
        }
        if msg_bool(writer, sel(c"finishWriting")) == 0 {
            msg_void(writer, sel(c"cancelWriting"));
            return Err(Error::EncodeError(
                "AVAssetWriter finishWriting failed".into(),
            ));
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

#[cfg(test)]
mod tests {
    use super::{
        earliest_track_start, usable_frame_duration, usable_reader_range, CMTime, CMTimeRange,
        K_CMTIME_FLAGS_VALID,
    };

    fn cmtime(value: i64, timescale: i32, flags: u32) -> CMTime {
        CMTime {
            value,
            timescale,
            flags,
            epoch: 0,
        }
    }

    fn time_range(start: CMTime, duration: CMTime) -> CMTimeRange {
        CMTimeRange { start, duration }
    }

    fn valid(value: i64, timescale: i32) -> CMTime {
        cmtime(value, timescale, K_CMTIME_FLAGS_VALID)
    }

    #[test]
    fn accepts_exact_ntsc_frame_durations() {
        for (value, timescale) in [
            (1001, 24_000),
            (1001, 30_000),
            (1001, 60_000),
            (1, 25),
            (1, 30),
        ] {
            assert!(
                usable_frame_duration(cmtime(value, timescale, K_CMTIME_FLAGS_VALID)),
                "{timescale}/{value} should be a usable frame duration"
            );
        }
    }

    #[test]
    fn rejects_invalid_or_implausible_frame_durations() {
        assert!(!usable_frame_duration(cmtime(1, 30, 0)));
        assert!(!usable_frame_duration(cmtime(0, 30, K_CMTIME_FLAGS_VALID)));
        assert!(!usable_frame_duration(cmtime(1, 0, K_CMTIME_FLAGS_VALID)));
        assert!(!usable_frame_duration(cmtime(-1, 30, K_CMTIME_FLAGS_VALID)));
        // A VFR track's minimum can be far above any sensible render rate.
        assert!(!usable_frame_duration(cmtime(
            1,
            1000,
            K_CMTIME_FLAGS_VALID
        )));
    }

    #[test]
    fn session_starts_at_earliest_real_track_start() {
        let ranges = [
            time_range(valid(5, 1), valid(2, 1)),
            time_range(valid(499, 100), valid(2, 1)),
        ];
        let start = earliest_track_start(&ranges);
        assert_eq!(start.value, 499);
        assert_eq!(start.timescale, 100);
    }

    #[test]
    fn session_start_ignores_invalid_ranges() {
        let ranges = [
            time_range(cmtime(1, 30, 0), valid(2, 1)),
            time_range(cmtime(0, 30, 0), valid(2, 1)),
        ];
        let start = earliest_track_start(&ranges);
        assert_eq!(start.value, 0);
        assert_eq!(start.timescale, 1);
    }

    #[test]
    fn session_start_falls_back_to_zero_for_negative_or_empty_ranges() {
        let negative = [time_range(valid(-1, 1), valid(2, 1))];
        assert_eq!(earliest_track_start(&negative).value, 0);
        assert_eq!(earliest_track_start(&[]).value, 0);
    }

    #[test]
    fn reader_range_rejects_invalid_bounds() {
        assert!(usable_reader_range(None).is_none());
        assert!(usable_reader_range(Some(time_range(valid(-1, 1), valid(2, 1)))).is_none());
        assert!(usable_reader_range(Some(time_range(valid(0, 1), valid(0, 1)))).is_none());
        assert!(usable_reader_range(Some(time_range(valid(0, 1), cmtime(2, 1, 0)))).is_none());
        assert!(usable_reader_range(Some(time_range(cmtime(0, 1, 0), valid(2, 1)))).is_none());
    }

    #[test]
    fn reader_range_keeps_real_track_bounds() {
        let range = usable_reader_range(Some(time_range(valid(5, 1), valid(2, 1))))
            .expect("valid range should be accepted");
        assert_eq!(range.start.value, 5);
        assert_eq!(range.duration.value, 2);
    }
}
