use std::ffi::c_void;

use super::webp_encode;
use crate::error::Error;

// ---------------------------------------------------------------------------
// Opaque pointer type aliases
// ---------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFDataRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CGImageRef = *mut c_void;
type CGContextRef = *mut c_void;
type CGColorSpaceRef = *mut c_void;
type CGImageSourceRef = *mut c_void;

type CFIndex = isize;

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// kCGBitmapByteOrder32Little (0x2000) | kCGImageAlphaPremultipliedFirst (0x2)
/// Native BGRA format on Apple Silicon (ARM little-endian). Using the non-native
/// big-endian RGBA (0x4001) forces CoreGraphics into a slow software fallback.
const BITMAP_INFO: u32 = 0x2002;

/// CFNumberType: kCFNumberFloat64Type = 13
const CF_NUMBER_FLOAT64_TYPE: i32 = 13;

const DEFAULT_DELAY_MS: i32 = 100;

// ---------------------------------------------------------------------------
// FFI — CoreFoundation
// ---------------------------------------------------------------------------

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataCreate(allocator: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;

    fn CFRelease(cf: CFTypeRef);

    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: CFTypeRef) -> *const c_void;

    fn CFNumberGetValue(number: CFNumberRef, the_type: i32, value_ptr: *mut c_void) -> u8;
}

// ---------------------------------------------------------------------------
// FFI — ImageIO
// ---------------------------------------------------------------------------

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageSourceCreateWithData(data: CFDataRef, options: CFDictionaryRef) -> CGImageSourceRef;

    fn CGImageSourceGetCount(isrc: CGImageSourceRef) -> usize;

    fn CGImageSourceCreateImageAtIndex(
        isrc: CGImageSourceRef,
        index: usize,
        options: CFDictionaryRef,
    ) -> CGImageRef;

    fn CGImageSourceCopyPropertiesAtIndex(
        isrc: CGImageSourceRef,
        index: usize,
        options: CFDictionaryRef,
    ) -> CFDictionaryRef;

    static kCGImagePropertyGIFDictionary: CFStringRef;
    static kCGImagePropertyGIFDelayTime: CFStringRef;
}

// ---------------------------------------------------------------------------
// FFI — CoreGraphics
// ---------------------------------------------------------------------------

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageGetWidth(image: CGImageRef) -> usize;
    fn CGImageGetHeight(image: CGImageRef) -> usize;
    fn CGImageRelease(image: CGImageRef);

    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(space: CGColorSpaceRef);

    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;

    fn CGContextRelease(ctx: CGContextRef);
    fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, image: CGImageRef);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decode one frame from an image source into RGBA pixels.
/// Returns `(pixels, width, height)`.
unsafe fn decode_frame(src: CGImageSourceRef, index: usize) -> Result<(Vec<u8>, u32, u32), Error> {
    let img = CGImageSourceCreateImageAtIndex(src, index, std::ptr::null());
    if img.is_null() {
        return Err(Error::DecodeError(format!(
            "CGImageSourceCreateImageAtIndex returned null for frame {}",
            index
        )));
    }

    let w = CGImageGetWidth(img);
    let h = CGImageGetHeight(img);

    if w == 0 || h == 0 {
        CGImageRelease(img);
        return Err(Error::DecodeError("Image has zero dimensions".into()));
    }

    let cs = CGColorSpaceCreateDeviceRGB();
    let bytes_per_row = w * 4;
    let buf_size = bytes_per_row * h;
    let mut pixels = vec![0u8; buf_size];

    let ctx = CGBitmapContextCreate(
        pixels.as_mut_ptr() as *mut c_void,
        w,
        h,
        8,
        bytes_per_row,
        cs,
        BITMAP_INFO,
    );

    if ctx.is_null() {
        CGColorSpaceRelease(cs);
        CGImageRelease(img);
        return Err(Error::DecodeError(
            "CGBitmapContextCreate returned null".into(),
        ));
    }

    let rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: w as f64,
            height: h as f64,
        },
    };
    // CGContextDrawImage writes directly into the `pixels` buffer we supplied
    // to CGBitmapContextCreate, so no extra copy is needed.
    CGContextDrawImage(ctx, rect, img);

    CGContextRelease(ctx);
    CGColorSpaceRelease(cs);
    CGImageRelease(img);

    // CoreGraphics fills the buffer as BGRA (native ARM little-endian format).
    // Swap B and R in-place to produce RGBA for libwebp.
    for chunk in pixels.chunks_exact_mut(4) {
        chunk.swap(0, 2); // BGRA → RGBA
    }

    Ok((pixels, w as u32, h as u32))
}

/// Read the GIF frame delay for frame at `index` from the image source.
/// Returns milliseconds; falls back to `DEFAULT_DELAY_MS` on any failure.
unsafe fn get_gif_delay_ms(src: CGImageSourceRef, index: usize) -> i32 {
    let props = CGImageSourceCopyPropertiesAtIndex(src, index, std::ptr::null());
    if props.is_null() {
        return DEFAULT_DELAY_MS;
    }

    let gif_dict =
        CFDictionaryGetValue(props, kCGImagePropertyGIFDictionary as CFTypeRef) as CFDictionaryRef;
    if gif_dict.is_null() {
        CFRelease(props as CFTypeRef);
        return DEFAULT_DELAY_MS;
    }

    let delay_val = CFDictionaryGetValue(gif_dict, kCGImagePropertyGIFDelayTime as CFTypeRef);
    if delay_val.is_null() {
        CFRelease(props as CFTypeRef);
        return DEFAULT_DELAY_MS;
    }

    let mut secs: f64 = 0.0;
    CFNumberGetValue(
        delay_val as CFNumberRef,
        CF_NUMBER_FLOAT64_TYPE,
        &mut secs as *mut f64 as *mut c_void,
    );

    CFRelease(props as CFTypeRef);

    let ms = (secs * 1000.0) as i32;
    ms.max(10)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn compress(input: &[u8], quality: f32) -> Result<Vec<u8>, Error> {
    unsafe {
        // Wrap raw bytes in a CFData — no copy, no allocator (null = default).
        let data_ref = CFDataCreate(std::ptr::null(), input.as_ptr(), input.len() as CFIndex);
        if data_ref.is_null() {
            return Err(Error::DecodeError("CFDataCreate returned null".into()));
        }

        let src = CGImageSourceCreateWithData(data_ref, std::ptr::null());
        if src.is_null() {
            CFRelease(data_ref as CFTypeRef);
            return Err(Error::DecodeError(
                "CGImageSourceCreateWithData returned null".into(),
            ));
        }

        let count = CGImageSourceGetCount(src);
        if count == 0 {
            CFRelease(src as CFTypeRef);
            CFRelease(data_ref as CFTypeRef);
            return Err(Error::DecodeError("Image source has no frames".into()));
        }

        let result = if count == 1 {
            // ── Static image ──────────────────────────────────────────────
            let (pixels, w, h) = decode_frame(src, 0)?;
            webp_encode::encode_static(&pixels, w, h, quality)
        } else {
            // ── Animated image (GIF, APNG, …) ─────────────────────────────
            // Get dimensions from first frame.
            let (first_pixels, w, h) = decode_frame(src, 0)?;

            // Collect all frame pixel data first so that every Vec<u8> lives
            // long enough for AnimEncoder (which borrows each slice).
            let mut frame_data: Vec<(Vec<u8>, i32)> = Vec::with_capacity(count);

            let delay0 = get_gif_delay_ms(src, 0);
            frame_data.push((first_pixels, delay0));

            for i in 1..count {
                let (pixels, _, _) = decode_frame(src, i)?;
                let delay = get_gif_delay_ms(src, i);
                frame_data.push((pixels, delay));
            }

            webp_encode::encode_animated(&frame_data, w, h, quality)
        };

        CFRelease(src as CFTypeRef);
        CFRelease(data_ref as CFTypeRef);

        result
    }
}
