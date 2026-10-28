use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::rgb;
use super::{
    compute_video_target_dimensions, within_size_limits, H264Target, VideoCodec,
    VideoCompressOptions, VideoConvertOptions, VideoConvertOutcome, VideoFormat,
};
use crate::error::Error;

// ---------------------------------------------------------------------------
// Windows-only scalar types
// ---------------------------------------------------------------------------

type HRESULT = i32;

const S_OK: HRESULT = 0;

/// MF_E_TOPO_CODEC_NOT_FOUND: no suitable transform found to encode/decode.
const MF_E_TOPO_CODEC_NOT_FOUND: HRESULT = 0xC00D5212u32 as i32;

const MF_VERSION: u32 = 0x0002_0070;
const MFSTARTUP_FULL: u32 = 0;

/// MF_SOURCE_READER_FIRST_VIDEO_STREAM / _FIRST_AUDIO_STREAM.
const MF_SOURCE_READER_FIRST_VIDEO_STREAM: u32 = 0xFFFF_FFFC;
const MF_SOURCE_READER_FIRST_AUDIO_STREAM: u32 = 0xFFFF_FFFD;

const MF_SOURCE_READERF_ENDOFSTREAM: u32 = 0x1;

/// MFVideoInterlace_Progressive.
const MF_VIDEO_INTERLACE_PROGRESSIVE: u32 = 2;

const AUDIO_SAMPLE_RATE: u32 = 44100;
const AUDIO_CHANNELS: u32 = 2;
const AUDIO_BITS_PER_SAMPLE: u32 = 16;
const AUDIO_BLOCK_ALIGNMENT: u32 = 4; // channels * bits_per_sample / 8
const AUDIO_BITRATE_BPS: u32 = 128_000;
const DEFAULT_FRAME_RATE: u32 = 30;

// 100-nanosecond units per second.
const HNS_PER_SECOND: i64 = 10_000_000;

// ---------------------------------------------------------------------------
// GUID
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct GUID {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// Subtype / major-type GUIDs follow the FOURCC pattern
/// `XXXXXXXX-0000-0010-8000-00AA00389B71`.
const fn fourcc_guid(dword: u32) -> GUID {
    GUID {
        data1: dword,
        data2: 0,
        data3: 0x10,
        data4: [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
    }
}

/// MFMediaType_Video ("vids").
const MF_MEDIA_TYPE_VIDEO: GUID = fourcc_guid(0x7364_6976);
/// MFMediaType_Audio ("auds").
const MF_MEDIA_TYPE_AUDIO: GUID = fourcc_guid(0x7364_7561);
/// MFVideoFormat_H264 ('H264').
const MF_VIDEO_FORMAT_H264: GUID = fourcc_guid(0x3436_3248);
/// MFVideoFormat_HEVC ('HEVC').
const MF_VIDEO_FORMAT_HEVC: GUID = fourcc_guid(0x4356_4548);
/// MFVideoFormat_RGB32 (D3DFMT_X8R8G8B8 = 22).
const MF_VIDEO_FORMAT_RGB32: GUID = fourcc_guid(0x0000_0016);
/// MFAudioFormat_AAC (WAVE_FORMAT_MPEG_HEAAC = 0x1610).
const MF_AUDIO_FORMAT_AAC: GUID = fourcc_guid(0x0000_1610);
/// MFAudioFormat_PCM (WAVE_FORMAT_PCM = 1).
const MF_AUDIO_FORMAT_PCM: GUID = fourcc_guid(0x0000_0001);

// Media type attribute GUIDs (defined in mfapi.h).
const MF_MT_MAJOR_TYPE: GUID = GUID {
    data1: 0x48eba18e,
    data2: 0xf8c9,
    data3: 0x4687,
    data4: [0xbf, 0x11, 0x0a, 0x74, 0xc9, 0xf9, 0x6a, 0x8f],
};
const MF_MT_SUBTYPE: GUID = GUID {
    data1: 0xf7e34c9a,
    data2: 0x42e8,
    data3: 0x4714,
    data4: [0xb7, 0x4b, 0xcb, 0x29, 0xd7, 0x2c, 0x35, 0xe5],
};
const MF_MT_FRAME_SIZE: GUID = GUID {
    data1: 0x1652c33d,
    data2: 0xd6b2,
    data3: 0x4012,
    data4: [0xb8, 0x34, 0x72, 0x03, 0x08, 0x49, 0xa3, 0x7d],
};
const MF_MT_FRAME_RATE: GUID = GUID {
    data1: 0xc459a2e8,
    data2: 0x3d2c,
    data3: 0x4e44,
    data4: [0xb1, 0x32, 0xfe, 0xe5, 0x15, 0x6c, 0x7b, 0xb0],
};
const MF_MT_PIXEL_ASPECT_RATIO: GUID = GUID {
    data1: 0xc6376a1e,
    data2: 0x8d0a,
    data3: 0x4027,
    data4: [0xbe, 0x45, 0x6d, 0x9a, 0x0a, 0xd3, 0x9b, 0xb6],
};
const MF_MT_INTERLACE_MODE: GUID = GUID {
    data1: 0xe2724bb8,
    data2: 0xe676,
    data3: 0x4806,
    data4: [0xb4, 0xb2, 0xa8, 0xd6, 0xef, 0xb4, 0x4c, 0xcd],
};
const MF_MT_AVG_BITRATE: GUID = GUID {
    data1: 0x20332624,
    data2: 0xfb0d,
    data3: 0x4d9e,
    data4: [0xbd, 0x0d, 0xcb, 0xf6, 0x78, 0x6c, 0x10, 0x2e],
};
const MF_MT_DEFAULT_STRIDE: GUID = GUID {
    data1: 0x644b4e48,
    data2: 0x1e02,
    data3: 0x4516,
    data4: [0xb0, 0xeb, 0xc0, 0x1c, 0xa9, 0xd4, 0x9a, 0xc6],
};
const MF_MT_AUDIO_NUM_CHANNELS: GUID = GUID {
    data1: 0x37e48bf5,
    data2: 0x645e,
    data3: 0x4c5b,
    data4: [0x89, 0xde, 0xad, 0xa9, 0xe2, 0x9b, 0x69, 0x6a],
};
const MF_MT_AUDIO_SAMPLES_PER_SECOND: GUID = GUID {
    data1: 0x5faeeae7,
    data2: 0x0290,
    data3: 0x4c31,
    data4: [0x9e, 0x8a, 0xc5, 0x34, 0xf6, 0x8d, 0x9d, 0xba],
};
const MF_MT_AUDIO_BLOCK_ALIGNMENT: GUID = GUID {
    data1: 0x322de230,
    data2: 0x9eeb,
    data3: 0x43bd,
    data4: [0xab, 0x7a, 0xff, 0x41, 0x22, 0x51, 0x54, 0x1d],
};
const MF_MT_AUDIO_AVG_BYTES_PER_SECOND: GUID = GUID {
    data1: 0x1aab75c8,
    data2: 0xcfef,
    data3: 0x451c,
    data4: [0xab, 0x95, 0xac, 0x03, 0x4b, 0x8e, 0x17, 0x31],
};
const MF_MT_AUDIO_BITS_PER_SAMPLE: GUID = GUID {
    data1: 0xf2deb57f,
    data2: 0x40fa,
    data3: 0x4764,
    data4: [0xaa, 0x33, 0xed, 0x4f, 0x2d, 0x1f, 0xf6, 0x69],
};

// ---------------------------------------------------------------------------
// COM vtable helpers
// ---------------------------------------------------------------------------

#[repr(C)]
struct IUnknown {
    vtable: *const *const c_void,
}

macro_rules! com_call {
    ($iface:expr, $slot:expr, $fn_ty:ty $(, $arg:expr)*) => {{
        let this = $iface as *mut IUnknown;
        let vtbl = (*this).vtable;
        let fn_ptr = *vtbl.add($slot);
        let f: $fn_ty = std::mem::transmute(fn_ptr);
        f(this as *mut c_void $(, $arg)*)
    }};
}

// IUnknown slot 2 = Release.
unsafe fn release_com(p: *mut c_void) {
    if !p.is_null() {
        com_call!(p, 2, unsafe extern "system" fn(*mut c_void) -> u32);
    }
}

// ---------------------------------------------------------------------------
// FFI — Media Foundation free functions
// ---------------------------------------------------------------------------

#[link(name = "mfplat")]
extern "system" {
    fn MFStartup(version: u32, flags: u32) -> HRESULT;
    fn MFShutdown() -> HRESULT;
    fn MFCreateMediaType(pp_mt: *mut *mut c_void) -> HRESULT;
    fn MFCreateMemoryBuffer(max_length: u32, pp_buffer: *mut *mut c_void) -> HRESULT;
    fn MFCreateSample(pp_sample: *mut *mut c_void) -> HRESULT;
}

#[link(name = "mfreadwrite")]
extern "system" {
    fn MFCreateSourceReaderFromURL(
        url: *const u16,
        attrs: *mut c_void,
        pp_reader: *mut *mut c_void,
    ) -> HRESULT;
    fn MFCreateSinkWriterFromURL(
        url: *const u16,
        byte_stream: *mut c_void,
        attrs: *mut c_void,
        pp_writer: *mut *mut c_void,
    ) -> HRESULT;
}

// ---------------------------------------------------------------------------
// Attribute helpers (IMFMediaType inherits IMFAttributes)
// ---------------------------------------------------------------------------

// IMFAttributes vtable slots: GetUINT32=7, GetUINT64=8, SetUINT32=20,
// SetUINT64=21, SetGUID=23.
unsafe fn attr_set_guid(p: *mut c_void, key: &GUID, value: &GUID) -> HRESULT {
    com_call!(
        p,
        23,
        unsafe extern "system" fn(*mut c_void, *const GUID, *const GUID) -> HRESULT,
        key,
        value
    )
}

unsafe fn attr_set_u32(p: *mut c_void, key: &GUID, value: u32) -> HRESULT {
    com_call!(
        p,
        20,
        unsafe extern "system" fn(*mut c_void, *const GUID, u32) -> HRESULT,
        key,
        value
    )
}

unsafe fn attr_set_u64(p: *mut c_void, key: &GUID, value: u64) -> HRESULT {
    com_call!(
        p,
        21,
        unsafe extern "system" fn(*mut c_void, *const GUID, u64) -> HRESULT,
        key,
        value
    )
}

unsafe fn attr_get_u64(p: *mut c_void, key: &GUID) -> Result<u64, HRESULT> {
    let mut v: u64 = 0;
    let hr = com_call!(
        p,
        8,
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut u64) -> HRESULT,
        key,
        &mut v
    );
    if hr == S_OK {
        Ok(v)
    } else {
        Err(hr)
    }
}

unsafe fn attr_get_u32(p: *mut c_void, key: &GUID) -> Result<u32, HRESULT> {
    let mut v: u32 = 0;
    let hr = com_call!(
        p,
        7,
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut u32) -> HRESULT,
        key,
        &mut v
    );
    if hr == S_OK {
        Ok(v)
    } else {
        Err(hr)
    }
}

unsafe fn attr_get_guid(p: *mut c_void, key: &GUID) -> Result<GUID, HRESULT> {
    let mut v = GUID {
        data1: 0,
        data2: 0,
        data3: 0,
        data4: [0; 8],
    };
    let hr = com_call!(
        p,
        10,
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut GUID) -> HRESULT,
        key,
        &mut v
    );
    if hr == S_OK {
        Ok(v)
    } else {
        Err(hr)
    }
}

// MF_MT_FRAME_SIZE / MF_MT_FRAME_RATE / MF_MT_PIXEL_ASPECT_RATIO are packed
// UINT64 values (width / numerator in the low 32 bits, height / denominator in
// the high 32 bits). The MFSetAttributeSize / MFGetAttributeSize / ... helpers
// are inline wrappers over SetUINT64 / GetUINT64 in mfapi.h, NOT exported from
// mfplat.dll, so reimplement the packing here.
unsafe fn attr_set_size(p: *mut c_void, key: &GUID, w: u32, h: u32) -> HRESULT {
    attr_set_u64(p, key, (w as u64) | ((h as u64) << 32))
}

unsafe fn attr_set_ratio(p: *mut c_void, key: &GUID, num: u32, den: u32) -> HRESULT {
    attr_set_u64(p, key, (num as u64) | ((den as u64) << 32))
}

unsafe fn attr_get_size(p: *mut c_void, key: &GUID) -> Result<(u32, u32), HRESULT> {
    let v = attr_get_u64(p, key)?;
    Ok((v as u32, (v >> 32) as u32))
}

unsafe fn attr_get_ratio(p: *mut c_void, key: &GUID) -> Result<(u32, u32), HRESULT> {
    let v = attr_get_u64(p, key)?;
    Ok((v as u32, (v >> 32) as u32))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn hres_ok(hr: HRESULT) -> bool {
    hr == S_OK
}

fn err_decode(context: &str, hr: HRESULT) -> Error {
    if hr == MF_E_TOPO_CODEC_NOT_FOUND {
        Error::PlatformNotSupported(format!("{context}: codec not found"))
    } else {
        Error::DecodeError(format!("{context}: 0x{:08x}", hr as u32))
    }
}

fn err_encode(context: &str, hr: HRESULT) -> Error {
    if hr == MF_E_TOPO_CODEC_NOT_FOUND {
        Error::PlatformNotSupported(format!("{context}: codec not found"))
    } else {
        Error::EncodeError(format!("{context}: 0x{:08x}", hr as u32))
    }
}

/// Null-terminated UTF-16 path (LPCWSTR) for the MF URL APIs.
fn wide_path(path: &Path) -> Vec<u16> {
    let mut v: Vec<u16> = path.as_os_str().encode_wide().collect();
    v.push(0);
    v
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

fn guid_eq(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

/// Map a native video subtype to the codecs the converter accepts as-is.
fn video_codec(subtype: &GUID) -> Option<VideoCodec> {
    if guid_eq(subtype, &MF_VIDEO_FORMAT_H264) {
        Some(VideoCodec::H264)
    } else if guid_eq(subtype, &MF_VIDEO_FORMAT_HEVC) {
        Some(VideoCodec::Hevc)
    } else {
        None
    }
}

fn detected_extension(input: &[u8]) -> &'static str {
    match VideoFormat::detect(input) {
        Some(VideoFormat::QuickTime) => "mov",
        Some(VideoFormat::Webm) => "webm",
        Some(VideoFormat::Avi) => "avi",
        _ => "mp4",
    }
}

unsafe fn new_media_type() -> Result<*mut c_void, Error> {
    let mut mt: *mut c_void = std::ptr::null_mut();
    let hr = MFCreateMediaType(&mut mt);
    if !hres_ok(hr) || mt.is_null() {
        return Err(err_encode("MFCreateMediaType", hr));
    }
    Ok(mt)
}

/// Media type for decoding video as RGB32 at the source resolution.
unsafe fn source_rgb32_type(src_w: u32, src_h: u32) -> Result<*mut c_void, Error> {
    let mt = new_media_type()?;
    let r = (|| {
        if !hres_ok(attr_set_guid(mt, &MF_MT_MAJOR_TYPE, &MF_MEDIA_TYPE_VIDEO)) {
            return Err(err_decode("set MF_MT_MAJOR_TYPE", -1));
        }
        if !hres_ok(attr_set_guid(mt, &MF_MT_SUBTYPE, &MF_VIDEO_FORMAT_RGB32)) {
            return Err(err_decode("set MF_MT_SUBTYPE (RGB32)", -1));
        }
        if !hres_ok(attr_set_size(mt, &MF_MT_FRAME_SIZE, src_w, src_h)) {
            return Err(err_decode("set MF_MT_FRAME_SIZE", -1));
        }
        Ok(())
    })();
    match r {
        Ok(()) => Ok(mt),
        Err(e) => {
            release_com(mt);
            Err(e)
        }
    }
}

/// Media type for decoding audio as 16-bit stereo PCM at 44.1 kHz.
unsafe fn source_pcm_type() -> Result<*mut c_void, Error> {
    let mt = new_media_type()?;
    let r = (|| {
        if !hres_ok(attr_set_guid(mt, &MF_MT_MAJOR_TYPE, &MF_MEDIA_TYPE_AUDIO)) {
            return Err(err_decode("set MF_MT_MAJOR_TYPE (audio)", -1));
        }
        if !hres_ok(attr_set_guid(mt, &MF_MT_SUBTYPE, &MF_AUDIO_FORMAT_PCM)) {
            return Err(err_decode("set MF_MT_SUBTYPE (PCM)", -1));
        }
        if !hres_ok(attr_set_u32(mt, &MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)) {
            return Err(err_decode("set MF_MT_AUDIO_NUM_CHANNELS", -1));
        }
        if !hres_ok(attr_set_u32(
            mt,
            &MF_MT_AUDIO_SAMPLES_PER_SECOND,
            AUDIO_SAMPLE_RATE,
        )) {
            return Err(err_decode("set MF_MT_AUDIO_SAMPLES_PER_SECOND", -1));
        }
        if !hres_ok(attr_set_u32(mt, &MF_MT_AUDIO_BITS_PER_SAMPLE, AUDIO_BITS_PER_SAMPLE)) {
            return Err(err_decode("set MF_MT_AUDIO_BITS_PER_SAMPLE", -1));
        }
        if !hres_ok(attr_set_u32(mt, &MF_MT_AUDIO_BLOCK_ALIGNMENT, AUDIO_BLOCK_ALIGNMENT)) {
            return Err(err_decode("set MF_MT_AUDIO_BLOCK_ALIGNMENT", -1));
        }
        Ok(())
    })();
    match r {
        Ok(()) => Ok(mt),
        Err(e) => {
            release_com(mt);
            Err(e)
        }
    }
}

/// Sink writer output type: H.264 at target dimensions and bitrate.
unsafe fn h264_output_type(
    w: u32,
    h: u32,
    bitrate_bps: u32,
    fps: u32,
) -> Result<*mut c_void, Error> {
    let mt = new_media_type()?;
    let r = (|| {
        if !hres_ok(attr_set_guid(mt, &MF_MT_MAJOR_TYPE, &MF_MEDIA_TYPE_VIDEO)) {
            return Err(err_encode("set MF_MT_MAJOR_TYPE", -1));
        }
        if !hres_ok(attr_set_guid(mt, &MF_MT_SUBTYPE, &MF_VIDEO_FORMAT_H264)) {
            return Err(err_encode("set MF_MT_SUBTYPE (H264)", -1));
        }
        if !hres_ok(attr_set_size(mt, &MF_MT_FRAME_SIZE, w, h)) {
            return Err(err_encode("set MF_MT_FRAME_SIZE", -1));
        }
        if !hres_ok(attr_set_u32(mt, &MF_MT_AVG_BITRATE, bitrate_bps)) {
            return Err(err_encode("set MF_MT_AVG_BITRATE", -1));
        }
        if !hres_ok(attr_set_ratio(mt, &MF_MT_FRAME_RATE, fps, 1)) {
            return Err(err_encode("set MF_MT_FRAME_RATE", -1));
        }
        if !hres_ok(attr_set_ratio(mt, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)) {
            return Err(err_encode("set MF_MT_PIXEL_ASPECT_RATIO", -1));
        }
        if !hres_ok(attr_set_u32(
            mt,
            &MF_MT_INTERLACE_MODE,
            MF_VIDEO_INTERLACE_PROGRESSIVE,
        )) {
            return Err(err_encode("set MF_MT_INTERLACE_MODE", -1));
        }
        Ok(())
    })();
    match r {
        Ok(()) => Ok(mt),
        Err(e) => {
            release_com(mt);
            Err(e)
        }
    }
}

/// Sink writer input type: RGB32 at target dimensions.
unsafe fn rgb32_input_type(w: u32, h: u32, fps: u32) -> Result<*mut c_void, Error> {
    let mt = new_media_type()?;
    let r = (|| {
        if !hres_ok(attr_set_guid(mt, &MF_MT_MAJOR_TYPE, &MF_MEDIA_TYPE_VIDEO)) {
            return Err(err_encode("set MF_MT_MAJOR_TYPE", -1));
        }
        if !hres_ok(attr_set_guid(mt, &MF_MT_SUBTYPE, &MF_VIDEO_FORMAT_RGB32)) {
            return Err(err_encode("set MF_MT_SUBTYPE (RGB32)", -1));
        }
        if !hres_ok(attr_set_size(mt, &MF_MT_FRAME_SIZE, w, h)) {
            return Err(err_encode("set MF_MT_FRAME_SIZE", -1));
        }
        if !hres_ok(attr_set_ratio(mt, &MF_MT_FRAME_RATE, fps, 1)) {
            return Err(err_encode("set MF_MT_FRAME_RATE", -1));
        }
        if !hres_ok(attr_set_ratio(mt, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1)) {
            return Err(err_encode("set MF_MT_PIXEL_ASPECT_RATIO", -1));
        }
        if !hres_ok(attr_set_u32(mt, &MF_MT_DEFAULT_STRIDE, w * 4)) {
            return Err(err_encode("set MF_MT_DEFAULT_STRIDE", -1));
        }
        if !hres_ok(attr_set_u32(
            mt,
            &MF_MT_INTERLACE_MODE,
            MF_VIDEO_INTERLACE_PROGRESSIVE,
        )) {
            return Err(err_encode("set MF_MT_INTERLACE_MODE", -1));
        }
        Ok(())
    })();
    match r {
        Ok(()) => Ok(mt),
        Err(e) => {
            release_com(mt);
            Err(e)
        }
    }
}

/// Sink writer output type: AAC stereo at 44.1 kHz / 128 kbps.
unsafe fn aac_output_type() -> Result<*mut c_void, Error> {
    let mt = new_media_type()?;
    let r = (|| {
        if !hres_ok(attr_set_guid(mt, &MF_MT_MAJOR_TYPE, &MF_MEDIA_TYPE_AUDIO)) {
            return Err(err_encode("set MF_MT_MAJOR_TYPE (audio)", -1));
        }
        if !hres_ok(attr_set_guid(mt, &MF_MT_SUBTYPE, &MF_AUDIO_FORMAT_AAC)) {
            return Err(err_encode("set MF_MT_SUBTYPE (AAC)", -1));
        }
        if !hres_ok(attr_set_u32(mt, &MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)) {
            return Err(err_encode("set MF_MT_AUDIO_NUM_CHANNELS", -1));
        }
        if !hres_ok(attr_set_u32(
            mt,
            &MF_MT_AUDIO_SAMPLES_PER_SECOND,
            AUDIO_SAMPLE_RATE,
        )) {
            return Err(err_encode("set MF_MT_AUDIO_SAMPLES_PER_SECOND", -1));
        }
        if !hres_ok(attr_set_u32(
            mt,
            &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            AUDIO_BITRATE_BPS / 8,
        )) {
            return Err(err_encode("set MF_MT_AUDIO_AVG_BYTES_PER_SECOND", -1));
        }
        Ok(())
    })();
    match r {
        Ok(()) => Ok(mt),
        Err(e) => {
            release_com(mt);
            Err(e)
        }
    }
}

/// Sink writer input type: PCM stereo at 44.1 kHz.
unsafe fn pcm_input_type() -> Result<*mut c_void, Error> {
    let mt = new_media_type()?;
    let r = (|| {
        if !hres_ok(attr_set_guid(mt, &MF_MT_MAJOR_TYPE, &MF_MEDIA_TYPE_AUDIO)) {
            return Err(err_encode("set MF_MT_MAJOR_TYPE (audio)", -1));
        }
        if !hres_ok(attr_set_guid(mt, &MF_MT_SUBTYPE, &MF_AUDIO_FORMAT_PCM)) {
            return Err(err_encode("set MF_MT_SUBTYPE (PCM)", -1));
        }
        if !hres_ok(attr_set_u32(mt, &MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)) {
            return Err(err_encode("set MF_MT_AUDIO_NUM_CHANNELS", -1));
        }
        if !hres_ok(attr_set_u32(
            mt,
            &MF_MT_AUDIO_SAMPLES_PER_SECOND,
            AUDIO_SAMPLE_RATE,
        )) {
            return Err(err_encode("set MF_MT_AUDIO_SAMPLES_PER_SECOND", -1));
        }
        if !hres_ok(attr_set_u32(mt, &MF_MT_AUDIO_BITS_PER_SAMPLE, AUDIO_BITS_PER_SAMPLE)) {
            return Err(err_encode("set MF_MT_AUDIO_BITS_PER_SAMPLE", -1));
        }
        if !hres_ok(attr_set_u32(mt, &MF_MT_AUDIO_BLOCK_ALIGNMENT, AUDIO_BLOCK_ALIGNMENT)) {
            return Err(err_encode("set MF_MT_AUDIO_BLOCK_ALIGNMENT", -1));
        }
        if !hres_ok(attr_set_u32(
            mt,
            &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            AUDIO_SAMPLE_RATE * AUDIO_BLOCK_ALIGNMENT,
        )) {
            return Err(err_encode("set MF_MT_AUDIO_AVG_BYTES_PER_SECOND", -1));
        }
        Ok(())
    })();
    match r {
        Ok(()) => Ok(mt),
        Err(e) => {
            release_com(mt);
            Err(e)
        }
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

    let result = transcode(&in_path, &out_path, H264Target::compress(options));

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
/// Neither file is buffered in memory; Media Foundation streams samples.
pub fn compress_file(
    input: &Path,
    output: &Path,
    options: VideoCompressOptions,
) -> Result<(), Error> {
    transcode(input, output, H264Target::compress(options))
}

/// Convert `input` to H.264/MP4 unless its video track is already H.264/HEVC
/// within the size limits.
pub fn convert_file(
    input: &Path,
    output: &Path,
    options: VideoConvertOptions,
) -> Result<VideoConvertOutcome, Error> {
    let probe = probe_video(input)?;

    if let Some(codec) = probe.codec {
        if within_size_limits(probe.width, probe.height, options) {
            return Ok(VideoConvertOutcome::AlreadyCompliant(codec));
        }
    }

    transcode(input, output, H264Target::convert(options))?;
    Ok(VideoConvertOutcome::Converted)
}

/// Codec and display size of the first video stream.
struct VideoProbe {
    codec: Option<VideoCodec>,
    width: u32,
    height: u32,
}

fn probe_video(path: &Path) -> Result<VideoProbe, Error> {
    let url = wide_path(path);

    unsafe {
        let hr = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        if !hres_ok(hr) {
            return Err(err_encode("MFStartup", hr));
        }

        let result = probe_video_impl(&url);

        MFShutdown();
        result
    }
}

unsafe fn probe_video_impl(url: &[u16]) -> Result<VideoProbe, Error> {
    let mut reader: *mut c_void = std::ptr::null_mut();
    let hr = MFCreateSourceReaderFromURL(url.as_ptr(), std::ptr::null_mut(), &mut reader);
    if !hres_ok(hr) || reader.is_null() {
        return Err(err_decode("MFCreateSourceReaderFromURL", hr));
    }

    let mut native: *mut c_void = std::ptr::null_mut();
    let hr = com_call!(
        reader,
        5,
        unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT,
        MF_SOURCE_READER_FIRST_VIDEO_STREAM,
        0u32,
        &mut native
    );
    if !hres_ok(hr) || native.is_null() {
        release_com(reader);
        return Err(err_decode("GetNativeMediaType (video)", hr));
    }

    let size = attr_get_size(native, &MF_MT_FRAME_SIZE);
    let codec = attr_get_guid(native, &MF_MT_SUBTYPE).map(|subtype| video_codec(&subtype));
    release_com(native);
    release_com(reader);

    let (width, height) = size.map_err(|hr| err_decode("GetUINT64 (MF_MT_FRAME_SIZE)", hr))?;
    if width == 0 || height == 0 {
        return Err(err_decode("video stream has zero dimensions", -1));
    }

    Ok(VideoProbe {
        codec: codec.map_err(|hr| err_decode("GetGUID (MF_MT_SUBTYPE)", hr))?,
        width,
        height,
    })
}

// ---------------------------------------------------------------------------
// Transcode implementation
// ---------------------------------------------------------------------------

fn transcode(in_path: &Path, out_path: &Path, target: H264Target) -> Result<(), Error> {
    let in_url = wide_path(in_path);
    let out_url = wide_path(out_path);

    unsafe {
        let hr = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        if !hres_ok(hr) {
            return Err(err_encode("MFStartup", hr));
        }

        let result = transcode_impl(&in_url, &out_url, target);

        MFShutdown();
        result
    }
}

unsafe fn transcode_impl(in_url: &[u16], out_url: &[u16], target: H264Target) -> Result<(), Error> {
    // ── Source reader ────────────────────────────────────────────────────
    let mut reader: *mut c_void = std::ptr::null_mut();
    let hr = MFCreateSourceReaderFromURL(in_url.as_ptr(), std::ptr::null_mut(), &mut reader);
    if !hres_ok(hr) || reader.is_null() {
        return Err(err_decode("MFCreateSourceReaderFromURL", hr));
    }

    // Probe the first video stream and read native dimensions / frame rate.
    let mut native: *mut c_void = std::ptr::null_mut();
    let hr = com_call!(
        reader,
        5,
        unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT,
        MF_SOURCE_READER_FIRST_VIDEO_STREAM,
        0u32,
        &mut native
    );
    if !hres_ok(hr) || native.is_null() {
        release_com(reader);
        return Err(err_decode("GetNativeMediaType (video)", hr));
    }

    let (src_w, src_h) = match attr_get_size(native, &MF_MT_FRAME_SIZE) {
        Ok(v) => v,
        Err(_) => {
            release_com(native);
            release_com(reader);
            return Err(err_decode("MFGetAttributeSize", -1));
        }
    };

    let (fps_num, fps_den) = match attr_get_ratio(native, &MF_MT_FRAME_RATE) {
        Ok((n, d)) if n > 0 && d > 0 => (n, d),
        _ => (DEFAULT_FRAME_RATE, 1),
    };
    let source_bitrate = attr_get_u32(native, &MF_MT_AVG_BITRATE)
        .ok()
        .filter(|value| *value > 0);
    release_com(native);

    if src_w == 0 || src_h == 0 {
        release_com(reader);
        return Err(err_decode("video stream has zero dimensions", -1));
    }

    let (target_w, target_h) =
        compute_video_target_dimensions(src_w, src_h, target.max_width, target.max_height);
    let fps = (fps_num as u64 + fps_den as u64 - 1) / fps_den as u64; // ceil
    let fps = fps.clamp(1, u32::MAX as u64) as u32;
    let bitrate_bps = target.resolve_bitrate_bps(
        source_bitrate,
        (src_w, src_h),
        (target_w, target_h),
        fps,
    );

    // Probe for audio.
    let mut audio_native: *mut c_void = std::ptr::null_mut();
    let audio_hr = com_call!(
        reader,
        5,
        unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT,
        MF_SOURCE_READER_FIRST_AUDIO_STREAM,
        0u32,
        &mut audio_native
    );
    let has_audio = hres_ok(audio_hr) && !audio_native.is_null();
    if has_audio {
        release_com(audio_native);
    }

    // Select first video / audio stream only.
    com_call!(
        reader,
        4,
        unsafe extern "system" fn(*mut c_void, u32, i32) -> HRESULT,
        MF_SOURCE_READER_FIRST_VIDEO_STREAM,
        1i32
    );
    if has_audio {
        com_call!(
            reader,
            4,
            unsafe extern "system" fn(*mut c_void, u32, i32) -> HRESULT,
            MF_SOURCE_READER_FIRST_AUDIO_STREAM,
            1i32
        );
    }

    // ── Configure source output formats ──────────────────────────────────
    let video_src_type = source_rgb32_type(src_w, src_h)?;
    let hr = com_call!(
        reader,
        7,
        unsafe extern "system" fn(*mut c_void, u32, *mut u32, *mut c_void) -> HRESULT,
        MF_SOURCE_READER_FIRST_VIDEO_STREAM,
        std::ptr::null_mut::<u32>(),
        video_src_type
    );
    release_com(video_src_type);
    if !hres_ok(hr) {
        release_com(reader);
        return Err(err_decode("SetCurrentMediaType (RGB32)", hr));
    }

    if has_audio {
        let audio_src_type = source_pcm_type()?;
        let hr = com_call!(
            reader,
            7,
            unsafe extern "system" fn(*mut c_void, u32, *mut u32, *mut c_void) -> HRESULT,
            MF_SOURCE_READER_FIRST_AUDIO_STREAM,
            std::ptr::null_mut::<u32>(),
            audio_src_type
        );
        release_com(audio_src_type);
        if !hres_ok(hr) {
            release_com(reader);
            return Err(err_decode("SetCurrentMediaType (PCM)", hr));
        }
    }

    // ── Sink writer ──────────────────────────────────────────────────────
    let mut writer: *mut c_void = std::ptr::null_mut();
    let hr = MFCreateSinkWriterFromURL(
        out_url.as_ptr(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut writer,
    );
    if !hres_ok(hr) || writer.is_null() {
        release_com(reader);
        return Err(err_encode("MFCreateSinkWriterFromURL", hr));
    }

    let video_output_type = h264_output_type(target_w, target_h, bitrate_bps, fps)?;
    let mut video_stream: u32 = 0;
    let hr = com_call!(
        writer,
        3,
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut u32) -> HRESULT,
        video_output_type,
        &mut video_stream
    );
    release_com(video_output_type);
    if !hres_ok(hr) {
        release_com(writer);
        release_com(reader);
        return Err(err_encode("AddStream (video)", hr));
    }

    let video_input_type = rgb32_input_type(target_w, target_h, fps)?;
    let hr = com_call!(
        writer,
        4,
        unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> HRESULT,
        video_stream,
        video_input_type,
        std::ptr::null_mut::<c_void>()
    );
    release_com(video_input_type);
    if !hres_ok(hr) {
        release_com(writer);
        release_com(reader);
        return Err(err_encode("SetInputMediaType (video)", hr));
    }

    let mut audio_stream: u32 = 0;
    if has_audio {
        let audio_output_type = aac_output_type()?;
        let hr = com_call!(
            writer,
            3,
            unsafe extern "system" fn(*mut c_void, *mut c_void, *mut u32) -> HRESULT,
            audio_output_type,
            &mut audio_stream
        );
        release_com(audio_output_type);
        if !hres_ok(hr) {
            release_com(writer);
            release_com(reader);
            return Err(err_encode("AddStream (audio)", hr));
        }

        let audio_input_type = pcm_input_type()?;
        let hr = com_call!(
            writer,
            4,
            unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> HRESULT,
            audio_stream,
            audio_input_type,
            std::ptr::null_mut::<c_void>()
        );
        release_com(audio_input_type);
        if !hres_ok(hr) {
            release_com(writer);
            release_com(reader);
            return Err(err_encode("SetInputMediaType (audio)", hr));
        }
    }

    let hr = com_call!(writer, 6, unsafe extern "system" fn(*mut c_void) -> HRESULT);
    if !hres_ok(hr) {
        release_com(writer);
        release_com(reader);
        return Err(err_encode("BeginWriting", hr));
    }

    // ── Pump samples (timestamp-interleaved) ─────────────────────────────
    let mut video_pending: Option<(*mut c_void, i64)> = None;
    let mut audio_pending: Option<(*mut c_void, i64)> = None;
    let mut video_done = false;
    let mut audio_done = !has_audio;

    let pump = (|| -> Result<(), Error> {
        loop {
            if video_done && audio_done {
                break;
            }

            if !video_done && video_pending.is_none() {
                match read_sample(reader, MF_SOURCE_READER_FIRST_VIDEO_STREAM)? {
                    Some((s, ts)) => video_pending = Some((s, ts)),
                    None => video_done = true,
                }
            }
            if !audio_done && audio_pending.is_none() {
                match read_sample(reader, MF_SOURCE_READER_FIRST_AUDIO_STREAM)? {
                    Some((s, ts)) => audio_pending = Some((s, ts)),
                    None => audio_done = true,
                }
            }

            let write_video = match (&video_pending, &audio_pending) {
                (Some((_, vt)), Some((_, at))) => *vt <= *at,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => continue,
            };

            if write_video {
                let (sample, ts) = video_pending.take().unwrap();
                let r = write_video_sample(
                    writer,
                    video_stream,
                    sample,
                    ts,
                    src_w as usize,
                    src_h as usize,
                    target_w as usize,
                    target_h as usize,
                    fps as i64,
                );
                release_com(sample);
                r?;
            } else {
                let (sample, _ts) = audio_pending.take().unwrap();
                let hr = com_call!(
                    writer,
                    7,
                    unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> HRESULT,
                    audio_stream,
                    sample
                );
                release_com(sample);
                if !hres_ok(hr) {
                    return Err(err_encode("WriteSample (audio)", hr));
                }
            }
        }
        Ok(())
    })();

    if let Err(e) = pump {
        release_com(writer);
        release_com(reader);
        return Err(e);
    }

    // ── Finalize ─────────────────────────────────────────────────────────
    let hr = com_call!(writer, 12, unsafe extern "system" fn(*mut c_void) -> HRESULT);
    release_com(writer);
    release_com(reader);
    if !hres_ok(hr) {
        return Err(err_encode("Finalize", hr));
    }

    Ok(())
}

/// Read the next sample for `stream`. Returns `None` at end of stream.
unsafe fn read_sample(
    reader: *mut c_void,
    stream: u32,
) -> Result<Option<(*mut c_void, i64)>, Error> {
    let mut sample: *mut c_void = std::ptr::null_mut();
    let mut actual_index: u32 = 0;
    let mut flags: u32 = 0;
    let mut ts: i64 = 0;
    let hr = com_call!(
        reader,
        9,
        unsafe extern "system" fn(
            *mut c_void,
            u32,
            u32,
            *mut u32,
            *mut u32,
            *mut i64,
            *mut *mut c_void,
        ) -> HRESULT,
        stream,
        0u32,
        &mut actual_index,
        &mut flags,
        &mut ts,
        &mut sample
    );
    if !hres_ok(hr) {
        return Err(err_decode("ReadSample", hr));
    }
    if sample.is_null() || (flags & MF_SOURCE_READERF_ENDOFSTREAM) != 0 {
        if !sample.is_null() {
            release_com(sample);
        }
        return Ok(None);
    }
    let _ = (actual_index, flags);
    Ok(Some((sample, ts)))
}

/// Convert one decoded RGB32 frame to the target size and write it.
unsafe fn write_video_sample(
    writer: *mut c_void,
    stream: u32,
    sample: *mut c_void,
    ts: i64,
    src_w: usize,
    src_h: usize,
    target_w: usize,
    target_h: usize,
    fps: i64,
) -> Result<(), Error> {
    // IMFSample slots: GetSampleTime=34, SetSampleTime=35, GetSampleDuration=36,
    // SetSampleDuration=37, GetBufferByIndex=39, ConvertToContiguousBuffer=40,
    // AddBuffer=41.
    let mut duration: i64 = 0;
    com_call!(
        sample,
        36,
        unsafe extern "system" fn(*mut c_void, *mut i64) -> HRESULT,
        &mut duration
    );
    if duration <= 0 {
        duration = HNS_PER_SECOND / fps.max(1);
    }

    let mut buffer: *mut c_void = std::ptr::null_mut();
    let hr = com_call!(
        sample,
        39,
        unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
        0u32,
        &mut buffer
    );
    if !hres_ok(hr) || buffer.is_null() {
        if !buffer.is_null() {
            release_com(buffer);
        }
        return Err(err_decode("GetBufferByIndex", hr));
    }

    let mut contiguous: *mut c_void = std::ptr::null_mut();
    let hr = com_call!(
        sample,
        40,
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
        &mut contiguous
    );
    release_com(buffer);
    if !hres_ok(hr) || contiguous.is_null() {
        if !contiguous.is_null() {
            release_com(contiguous);
        }
        return Err(err_decode("ConvertToContiguousBuffer", hr));
    }

    // IMFMediaBuffer: Lock=3, Unlock=4, SetCurrentLength=6.
    let mut data: *mut u8 = std::ptr::null_mut();
    let mut max_len: u32 = 0;
    let mut cur_len: u32 = 0;
    let hr = com_call!(
        contiguous,
        3,
        unsafe extern "system" fn(*mut c_void, *mut *mut u8, *mut u32, *mut u32) -> HRESULT,
        &mut data,
        &mut max_len,
        &mut cur_len
    );
    if !hres_ok(hr) || data.is_null() {
        release_com(contiguous);
        return Err(err_decode("IMFMediaBuffer::Lock", hr));
    }
    let _ = max_len;

    let frame_len = src_w * src_h * 4;
    let src_slice = std::slice::from_raw_parts(data, cur_len.min(frame_len as u32) as usize);

    let scaled = if target_w != src_w || target_h != src_h {
        rgb::scale_bgra(src_slice, src_w, src_h, target_w, target_h)
    } else {
        src_slice.to_vec()
    };

    com_call!(contiguous, 4, unsafe extern "system" fn(*mut c_void) -> HRESULT);
    release_com(contiguous);

    if scaled.is_empty() {
        return Err(err_decode("frame scale produced empty buffer", -1));
    }

    // Build a new sample backed by a contiguous memory buffer.
    let mut out_buffer: *mut c_void = std::ptr::null_mut();
    let hr = MFCreateMemoryBuffer(scaled.len() as u32, &mut out_buffer);
    if !hres_ok(hr) || out_buffer.is_null() {
        if !out_buffer.is_null() {
            release_com(out_buffer);
        }
        return Err(err_encode("MFCreateMemoryBuffer", hr));
    }

    let mut out_data: *mut u8 = std::ptr::null_mut();
    let mut out_max: u32 = 0;
    let mut out_cur: u32 = 0;
    let hr = com_call!(
        out_buffer,
        3,
        unsafe extern "system" fn(*mut c_void, *mut *mut u8, *mut u32, *mut u32) -> HRESULT,
        &mut out_data,
        &mut out_max,
        &mut out_cur
    );
    if !hres_ok(hr) || out_data.is_null() {
        release_com(out_buffer);
        return Err(err_encode("IMFMediaBuffer::Lock (out)", hr));
    }
    let _ = (out_max, out_cur);
    std::ptr::copy_nonoverlapping(scaled.as_ptr(), out_data, scaled.len());
    com_call!(out_buffer, 4, unsafe extern "system" fn(*mut c_void) -> HRESULT);
    com_call!(
        out_buffer,
        6,
        unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
        scaled.len() as u32
    );

    let mut out_sample: *mut c_void = std::ptr::null_mut();
    let hr = MFCreateSample(&mut out_sample);
    if !hres_ok(hr) || out_sample.is_null() {
        release_com(out_buffer);
        if !out_sample.is_null() {
            release_com(out_sample);
        }
        return Err(err_encode("MFCreateSample", hr));
    }

    let hr = com_call!(
        out_sample,
        41,
        unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
        out_buffer
    );
    release_com(out_buffer);
    if !hres_ok(hr) {
        release_com(out_sample);
        return Err(err_encode("IMFSample::AddBuffer", hr));
    }

    com_call!(
        out_sample,
        35,
        unsafe extern "system" fn(*mut c_void, i64) -> HRESULT,
        ts
    );
    com_call!(
        out_sample,
        37,
        unsafe extern "system" fn(*mut c_void, i64) -> HRESULT,
        duration
    );

    let hr = com_call!(
        writer,
        7,
        unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> HRESULT,
        stream,
        out_sample
    );
    release_com(out_sample);
    if !hres_ok(hr) {
        return Err(err_encode("WriteSample (video)", hr));
    }

    Ok(())
}
