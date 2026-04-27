use std::ffi::c_void;

use crate::error::Error;
use super::webp_encode;

// ---------------------------------------------------------------------------
// Windows-only types
// ---------------------------------------------------------------------------

type HRESULT = i32;
type BOOL = i32;

const S_OK: HRESULT = 0;
const COINIT_APARTMENTTHREADED: u32 = 0x2;
const CLSCTX_INPROC_SERVER: u32 = 0x1;

/// WICDecodeOptions: WICDecodeMetadataCacheOnLoad = 0
const WIC_DECODE_METADATA_CACHE_ON_LOAD: u32 = 0;
/// WICBitmapDitherType: WICBitmapDitherTypeNone = 0
const WIC_BITMAP_DITHER_TYPE_NONE: u32 = 0;
/// WICBitmapPaletteType: WICBitmapPaletteTypeCustom = 0
const WIC_BITMAP_PALETTE_TYPE_CUSTOM: u32 = 0;

const DEFAULT_DELAY_MS: i32 = 100;

// ---------------------------------------------------------------------------
// GUID / IID
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct GUID {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// {CACAF262-9370-4615-A13B-9F5539DA4C0A}
const CLSID_WIC_IMAGING_FACTORY: GUID = GUID {
    data1: 0xcacaf262,
    data2: 0x9370,
    data3: 0x4615,
    data4: [0xa1, 0x3b, 0x9f, 0x55, 0x39, 0xda, 0x4c, 0x0a],
};

/// {EC5EC8A9-C395-4314-9C77-54D7A935FF70}
const IID_IWIC_IMAGING_FACTORY: GUID = GUID {
    data1: 0xec5ec8a9,
    data2: 0xc395,
    data3: 0x4314,
    data4: [0x9c, 0x77, 0x54, 0xd7, 0xa9, 0x35, 0xff, 0x70],
};

/// GUID_WICPixelFormat32bppRGBA {F5C7AD2D-6A8D-43DD-A7A8-A29935261AE9}
const GUID_WIC_PIXEL_FORMAT_32BPP_RGBA: GUID = GUID {
    data1: 0xf5c7ad2d,
    data2: 0x6a8d,
    data3: 0x43dd,
    data4: [0xa7, 0xa8, 0xa2, 0x99, 0x35, 0x26, 0x1a, 0xe9],
};

// ---------------------------------------------------------------------------
// PROPVARIANT  (minimal — only vt + union needed for USHORT value)
// ---------------------------------------------------------------------------

#[repr(C)]
struct PROPVARIANT {
    vt: u16,
    reserved1: u16,
    reserved2: u16,
    reserved3: u16,
    // The union field that holds VT_UI2 (uint16).
    val: PropVariantVal,
}

#[repr(C)]
union PropVariantVal {
    ui_val: u16,   // VT_UI2
    i_val: i16,    // VT_I2
    raw: [u8; 16], // ensure enough space for any PROPVARIANT value
}

impl PROPVARIANT {
    fn zero() -> Self {
        PROPVARIANT {
            vt: 0,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            val: PropVariantVal { raw: [0u8; 16] },
        }
    }
}

const VT_UI2: u16 = 18;

// ---------------------------------------------------------------------------
// COM vtable helpers — IUnknown base
// ---------------------------------------------------------------------------

/// Every COM interface starts with a vtable pointer.
/// We represent each interface as a struct containing only the vtable pointer
/// so we can offset into it by slot index.
#[repr(C)]
struct IUnknown {
    vtable: *const *const c_void,
}

impl IUnknown {
    /// Call the method at `slot` (0-indexed) with the given raw argument list.
    /// Safety: caller must ensure correct slot, argument types and ABI.
    #[inline(always)]
    unsafe fn call_slot<R>(&self, slot: usize) -> *const c_void {
        *(*self.vtable).add(slot)
    }
}

// Convenience macro for calling COM vtable methods via raw function pointer casts.
// $iface : *mut IUnknown (or castable pointer)
// $slot  : vtable slot index
// $fn_ty : the fn(...) -> R type of that slot
// $($arg),* : additional arguments after `this`
macro_rules! com_call {
    ($iface:expr, $slot:expr, $fn_ty:ty $(, $arg:expr)*) => {{
        let this = $iface as *mut IUnknown;
        let vtbl = (*this).vtable;
        let fn_ptr = *vtbl.add($slot);
        let f: $fn_ty = std::mem::transmute(fn_ptr);
        f(this as *mut c_void $(, $arg)*)
    }};
}

// ---------------------------------------------------------------------------
// FFI — Ole32 / Shell
// ---------------------------------------------------------------------------

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, co_init: u32) -> HRESULT;
    fn CoUninitialize();
    fn CoCreateInstance(
        rclsid: *const GUID,
        punk_outer: *mut c_void,
        dw_cls_context: u32,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT;
}

/// SHCreateMemStream is in shlwapi.dll on older Windows, but reliably available
/// via the Shell COM approach. We use a manual IStream from memory via
/// CreateStreamOnHGlobal + write instead for maximum compatibility.
#[link(name = "ole32")]
extern "system" {
    fn CreateStreamOnHGlobal(
        h_global: *mut c_void,
        delete_on_release: BOOL,
        ppstm: *mut *mut c_void,
    ) -> HRESULT;
}

// ---------------------------------------------------------------------------
// WIC vtable slot indices (all 0-indexed, including the 3 IUnknown slots)
// IUnknown: 0=QueryInterface, 1=AddRef, 2=Release
// ---------------------------------------------------------------------------

// IWICImagingFactory (inherits IUnknown):
const WIC_FACTORY_CREATE_DECODER_FROM_STREAM: usize = 14;
const WIC_FACTORY_CREATE_FORMAT_CONVERTER: usize = 17;

// IWICBitmapDecoder (inherits IUnknown):
const WIC_DECODER_GET_FRAME_COUNT: usize = 7;
const WIC_DECODER_GET_FRAME: usize = 8;

// IWICBitmapFrameDecode (inherits IWICBitmapSource, IUnknown):
// IWICBitmapSource slots: 3=GetSize, 4=GetPixelFormat, 5=GetResolution, 6=CopyPalette, 7=CopyPixels
// IWICBitmapFrameDecode slots start at 8: 8=GetMetadataQueryReader, 9=GetColorContexts, 10=GetThumbnail
const WIC_BITMAP_SOURCE_GET_SIZE: usize = 3;
const WIC_BITMAP_SOURCE_COPY_PIXELS: usize = 7;
const WIC_FRAME_GET_METADATA_QUERY_READER: usize = 8;

// IWICFormatConverter (inherits IWICBitmapSource, IUnknown):
// After IWICBitmapSource (3..7), Initialize is at slot 8
const WIC_FORMAT_CONVERTER_INITIALIZE: usize = 8;

// IWICMetadataQueryReader (inherits IUnknown):
// 3=GetContainerFormat, 4=GetLocation, 5=GetMetadataByName, 6=GetEnumerator
const WIC_METADATA_GET_BY_NAME: usize = 5;

// IStream (inherits ISequentialStream, IUnknown):
// ISequentialStream: 3=Read, 4=Write
// IStream: 5=Seek, 6=SetSize, 7=CopyTo, 8=Commit, 9=Revert, 10=LockRegion, 11=UnlockRegion, 12=Stat, 13=Clone
const ISTREAM_WRITE: usize = 4;
const ISTREAM_SEEK: usize = 5;

// STREAM_SEEK_SET = 0
const STREAM_SEEK_SET: u32 = 0;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `data` into an in-memory IStream, rewind, return the IStream.
unsafe fn make_mem_stream(data: &[u8]) -> Result<*mut c_void, Error> {
    let mut stream: *mut c_void = std::ptr::null_mut();
    let hr = CreateStreamOnHGlobal(std::ptr::null_mut(), 1, &mut stream);
    if hr != S_OK || stream.is_null() {
        return Err(Error::DecodeError(format!(
            "CreateStreamOnHGlobal failed: 0x{:08x}",
            hr
        )));
    }

    // Write data into stream
    let mut written: u32 = 0;
    let hr: HRESULT = com_call!(
        stream,
        ISTREAM_WRITE,
        unsafe extern "system" fn(*mut c_void, *const c_void, u32, *mut u32) -> HRESULT,
        data.as_ptr() as *const c_void,
        data.len() as u32,
        &mut written
    );
    if hr != S_OK {
        com_call!(stream, 2, unsafe extern "system" fn(*mut c_void) -> u32); // Release
        return Err(Error::DecodeError(format!(
            "IStream::Write failed: 0x{:08x}",
            hr
        )));
    }

    // Seek back to beginning
    let zero_pos: i64 = 0;
    let hr: HRESULT = com_call!(
        stream,
        ISTREAM_SEEK,
        unsafe extern "system" fn(*mut c_void, i64, u32, *mut u64) -> HRESULT,
        zero_pos,
        STREAM_SEEK_SET,
        std::ptr::null_mut::<u64>()
    );
    if hr != S_OK {
        com_call!(stream, 2, unsafe extern "system" fn(*mut c_void) -> u32);
        return Err(Error::DecodeError(format!(
            "IStream::Seek failed: 0x{:08x}",
            hr
        )));
    }

    Ok(stream)
}

/// Read one frame's RGBA pixels via a WIC format converter.
unsafe fn decode_wic_frame(
    factory: *mut c_void,
    frame: *mut c_void,
) -> Result<(Vec<u8>, u32, u32), Error> {
    // Get frame dimensions
    let mut w: u32 = 0;
    let mut h: u32 = 0;
    let hr: HRESULT = com_call!(
        frame,
        WIC_BITMAP_SOURCE_GET_SIZE,
        unsafe extern "system" fn(*mut c_void, *mut u32, *mut u32) -> HRESULT,
        &mut w,
        &mut h
    );
    if hr != S_OK {
        return Err(Error::DecodeError(format!(
            "IWICBitmapFrameDecode::GetSize failed: 0x{:08x}",
            hr
        )));
    }

    // Create format converter
    let mut converter: *mut c_void = std::ptr::null_mut();
    let hr: HRESULT = com_call!(
        factory,
        WIC_FACTORY_CREATE_FORMAT_CONVERTER,
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
        &mut converter
    );
    if hr != S_OK || converter.is_null() {
        return Err(Error::DecodeError(format!(
            "IWICImagingFactory::CreateFormatConverter failed: 0x{:08x}",
            hr
        )));
    }

    // Initialize converter: source → 32bppRGBA, no dithering, no palette
    let hr: HRESULT = com_call!(
        converter,
        WIC_FORMAT_CONVERTER_INITIALIZE,
        unsafe extern "system" fn(*mut c_void, *mut c_void, *const GUID, u32, *mut c_void, f64, u32) -> HRESULT,
        frame,
        &GUID_WIC_PIXEL_FORMAT_32BPP_RGBA as *const GUID,
        WIC_BITMAP_DITHER_TYPE_NONE,
        std::ptr::null_mut::<c_void>(),
        0.0f64,
        WIC_BITMAP_PALETTE_TYPE_CUSTOM
    );
    if hr != S_OK {
        com_call!(converter, 2, unsafe extern "system" fn(*mut c_void) -> u32);
        return Err(Error::DecodeError(format!(
            "IWICFormatConverter::Initialize failed: 0x{:08x}",
            hr
        )));
    }

    // Copy pixels into buffer
    let stride = w as usize * 4;
    let buf_size = stride * h as usize;
    let mut pixels = vec![0u8; buf_size];

    let hr: HRESULT = com_call!(
        converter,
        WIC_BITMAP_SOURCE_COPY_PIXELS,
        unsafe extern "system" fn(*mut c_void, *const c_void, u32, u32, *mut u8) -> HRESULT,
        std::ptr::null::<c_void>(), // entire image
        stride as u32,
        buf_size as u32,
        pixels.as_mut_ptr()
    );

    com_call!(converter, 2, unsafe extern "system" fn(*mut c_void) -> u32); // Release converter

    if hr != S_OK {
        return Err(Error::DecodeError(format!(
            "IWICFormatConverter::CopyPixels failed: 0x{:08x}",
            hr
        )));
    }

    Ok((pixels, w, h))
}

/// Read GIF frame delay via metadata query "/grctlext/Delay" (unit: 1/100 s).
/// Returns milliseconds; falls back to `DEFAULT_DELAY_MS` on any failure.
unsafe fn get_gif_delay_ms(frame: *mut c_void) -> i32 {
    let mut mqr: *mut c_void = std::ptr::null_mut();
    let hr: HRESULT = com_call!(
        frame,
        WIC_FRAME_GET_METADATA_QUERY_READER,
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
        &mut mqr
    );
    if hr != S_OK || mqr.is_null() {
        return DEFAULT_DELAY_MS;
    }

    // Build a PROPVARIANT to receive the result
    let mut pv = PROPVARIANT::zero();

    // Path: "/grctlext/Delay" as a null-terminated wide string
    let path_wide: Vec<u16> = "/grctlext/Delay\0"
        .encode_utf16()
        .collect();

    let hr: HRESULT = com_call!(
        mqr,
        WIC_METADATA_GET_BY_NAME,
        unsafe extern "system" fn(*mut c_void, *const u16, *mut PROPVARIANT) -> HRESULT,
        path_wide.as_ptr(),
        &mut pv
    );

    com_call!(mqr, 2, unsafe extern "system" fn(*mut c_void) -> u32); // Release MQR

    if hr != S_OK || pv.vt != VT_UI2 {
        return DEFAULT_DELAY_MS;
    }

    // Delay is in units of 1/100 second → convert to ms
    let hundredths = pv.val.ui_val as i32;
    (hundredths * 10).max(10)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn compress(input: &[u8], quality: f32) -> Result<Vec<u8>, Error> {
    unsafe {
        // Initialize COM in apartment-threaded mode (idempotent for same thread)
        CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);

        // Create WIC factory
        let mut factory: *mut c_void = std::ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_WIC_IMAGING_FACTORY,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_IWIC_IMAGING_FACTORY,
            &mut factory,
        );
        if hr != S_OK || factory.is_null() {
            return Err(Error::DecodeError(format!(
                "CoCreateInstance(WICImagingFactory) failed: 0x{:08x}",
                hr
            )));
        }

        // Wrap input bytes in an in-memory IStream
        let stream = match make_mem_stream(input) {
            Ok(s) => s,
            Err(e) => {
                com_call!(factory, 2, unsafe extern "system" fn(*mut c_void) -> u32);
                return Err(e);
            }
        };

        // Create decoder from stream
        let mut decoder: *mut c_void = std::ptr::null_mut();
        let hr: HRESULT = com_call!(
            factory,
            WIC_FACTORY_CREATE_DECODER_FROM_STREAM,
            unsafe extern "system" fn(*mut c_void, *mut c_void, *const GUID, u32, *mut *mut c_void) -> HRESULT,
            stream,
            std::ptr::null::<GUID>(), // no preferred vendor
            WIC_DECODE_METADATA_CACHE_ON_LOAD,
            &mut decoder
        );
        com_call!(stream, 2, unsafe extern "system" fn(*mut c_void) -> u32); // Release stream

        if hr != S_OK || decoder.is_null() {
            com_call!(factory, 2, unsafe extern "system" fn(*mut c_void) -> u32);
            return Err(Error::DecodeError(format!(
                "IWICImagingFactory::CreateDecoderFromStream failed: 0x{:08x}",
                hr
            )));
        }

        // Get frame count
        let mut frame_count: u32 = 0;
        let hr: HRESULT = com_call!(
            decoder,
            WIC_DECODER_GET_FRAME_COUNT,
            unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
            &mut frame_count
        );
        if hr != S_OK || frame_count == 0 {
            com_call!(decoder, 2, unsafe extern "system" fn(*mut c_void) -> u32);
            com_call!(factory, 2, unsafe extern "system" fn(*mut c_void) -> u32);
            return Err(Error::DecodeError(
                "IWICBitmapDecoder::GetFrameCount returned 0".into(),
            ));
        }

        let result = if frame_count == 1 {
            // ── Static image ────────────────────────────────────────────────
            let mut frame: *mut c_void = std::ptr::null_mut();
            let hr: HRESULT = com_call!(
                decoder,
                WIC_DECODER_GET_FRAME,
                unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
                0u32,
                &mut frame
            );
            if hr != S_OK || frame.is_null() {
                com_call!(decoder, 2, unsafe extern "system" fn(*mut c_void) -> u32);
                com_call!(factory, 2, unsafe extern "system" fn(*mut c_void) -> u32);
                return Err(Error::DecodeError(format!(
                    "IWICBitmapDecoder::GetFrame(0) failed: 0x{:08x}",
                    hr
                )));
            }

            let r = decode_wic_frame(factory, frame);
            com_call!(frame, 2, unsafe extern "system" fn(*mut c_void) -> u32);
            let (pixels, w, h) = r?;

            webp_encode::encode_static(&pixels, w, h, quality)
        } else {
            // ── Animated (GIF, APNG, …) ─────────────────────────────────────
            // Get first frame to determine dimensions
            let mut frame0: *mut c_void = std::ptr::null_mut();
            let hr: HRESULT = com_call!(
                decoder,
                WIC_DECODER_GET_FRAME,
                unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
                0u32,
                &mut frame0
            );
            if hr != S_OK || frame0.is_null() {
                com_call!(decoder, 2, unsafe extern "system" fn(*mut c_void) -> u32);
                com_call!(factory, 2, unsafe extern "system" fn(*mut c_void) -> u32);
                return Err(Error::DecodeError(
                    "IWICBitmapDecoder::GetFrame(0) failed for animated image".into(),
                ));
            }

            let delay0 = get_gif_delay_ms(frame0);
            let r0 = decode_wic_frame(factory, frame0);
            com_call!(frame0, 2, unsafe extern "system" fn(*mut c_void) -> u32);
            let (pixels0, w, h) = r0?;

            // Collect all frame pixel data first so that every Vec<u8> lives
            // long enough for AnimEncoder (which borrows each slice).
            let mut frame_data: Vec<(Vec<u8>, i32)> = Vec::with_capacity(frame_count as usize);
            frame_data.push((pixels0, delay0));

            for i in 1..frame_count {
                let mut frame: *mut c_void = std::ptr::null_mut();
                let hr: HRESULT = com_call!(
                    decoder,
                    WIC_DECODER_GET_FRAME,
                    unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
                    i,
                    &mut frame
                );
                if hr != S_OK || frame.is_null() {
                    break; // best-effort: stop at first failure
                }

                let delay = get_gif_delay_ms(frame);
                let r = decode_wic_frame(factory, frame);
                com_call!(frame, 2, unsafe extern "system" fn(*mut c_void) -> u32);
                let (pixels, _, _) = r?;
                frame_data.push((pixels, delay));
            }

            webp_encode::encode_animated(&frame_data, w, h, quality)
        };

        com_call!(decoder, 2, unsafe extern "system" fn(*mut c_void) -> u32);
        com_call!(factory, 2, unsafe extern "system" fn(*mut c_void) -> u32);
        CoUninitialize();

        result
    }
}
