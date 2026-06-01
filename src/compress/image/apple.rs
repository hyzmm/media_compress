use std::ffi::c_void;

use super::encode;
use super::orientation::apply_exif_orientation_rgba;
use super::{compute_target_dimensions, resize, CompressOptions};
use crate::error::Error;

// ---------------------------------------------------------------------------
// Opaque pointer type aliases
// ---------------------------------------------------------------------------

type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFDataRef = *const c_void;
type CFMutableDataRef = *mut c_void;
type CFDictionaryRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CGImageRef = *mut c_void;
type CGContextRef = *mut c_void;
type CGColorSpaceRef = *mut c_void;
type CGImageSourceRef = *mut c_void;
type CGImageDestinationRef = *mut c_void;

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

/// CFNumberType: kCFNumberSInt32Type = 3
const CF_NUMBER_SINT32_TYPE: i32 = 3;
/// CFNumberType: kCFNumberFloat32Type = 5
const CF_NUMBER_FLOAT32_TYPE: i32 = 5;

// ---------------------------------------------------------------------------
// FFI — CoreFoundation
// ---------------------------------------------------------------------------

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataCreate(allocator: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
    fn CFDataCreateMutable(allocator: CFAllocatorRef, capacity: CFIndex) -> CFMutableDataRef;
    fn CFDataGetLength(the_data: CFDataRef) -> CFIndex;
    fn CFDataGetBytePtr(the_data: CFDataRef) -> *const u8;

    fn CFRelease(cf: CFTypeRef);

    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: CFTypeRef) -> *const c_void;
    fn CFDictionaryCreateMutable(
        allocator: CFAllocatorRef,
        capacity: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFMutableDictionaryRef;
    fn CFDictionarySetValue(
        the_dict: CFMutableDictionaryRef,
        key: *const c_void,
        value: *const c_void,
    );
    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: i32,
        value_ptr: *const c_void,
    ) -> CFNumberRef;

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

    fn CGImageDestinationCreateWithData(
        data: CFMutableDataRef,
        ty: CFStringRef,
        count: usize,
        options: CFDictionaryRef,
    ) -> CGImageDestinationRef;
    fn CGImageDestinationAddImage(
        idst: CGImageDestinationRef,
        image: CGImageRef,
        properties: CFDictionaryRef,
    );
    fn CGImageDestinationSetProperties(idst: CGImageDestinationRef, properties: CFDictionaryRef);
    fn CGImageDestinationFinalize(idst: CGImageDestinationRef) -> u8;

    static kCGImagePropertyOrientation: CFStringRef;
    static kCGImagePropertyGIFDictionary: CFStringRef;
    static kCGImagePropertyGIFDelayTime: CFStringRef;
    static kCGImagePropertyGIFLoopCount: CFStringRef;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    static kUTTypeGIF: CFStringRef;
}

#[cfg(target_os = "ios")]
#[link(name = "MobileCoreServices", kind = "framework")]
extern "C" {
    static kUTTypeGIF: CFStringRef;
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

    fn CGBitmapContextCreateImage(ctx: CGContextRef) -> CGImageRef;

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
    // Swap B and R in-place to produce RGBA.
    for chunk in pixels.chunks_exact_mut(4) {
        chunk.swap(0, 2); // BGRA → RGBA
    }

    let orientation = get_frame_orientation(src, index);
    Ok(apply_exif_orientation_rgba(
        pixels,
        w as u32,
        h as u32,
        orientation,
    ))
}

unsafe fn get_frame_orientation(src: CGImageSourceRef, index: usize) -> u32 {
    let props = CGImageSourceCopyPropertiesAtIndex(src, index, std::ptr::null());
    if props.is_null() {
        return 1;
    }

    let value = CFDictionaryGetValue(props, kCGImagePropertyOrientation as CFTypeRef);
    if value.is_null() {
        CFRelease(props as CFTypeRef);
        return 1;
    }

    let mut orientation: i32 = 1;
    let ok = CFNumberGetValue(
        value as CFNumberRef,
        CF_NUMBER_SINT32_TYPE,
        &mut orientation as *mut i32 as *mut c_void,
    );

    CFRelease(props as CFTypeRef);

    if ok == 0 {
        1
    } else {
        (orientation as u32).clamp(1, 8)
    }
}

unsafe fn make_cgimage_from_rgba(pixels: &[u8], w: u32, h: u32) -> Result<CGImageRef, Error> {
    let mut bgra = pixels.to_vec();
    for chunk in bgra.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    let cs = CGColorSpaceCreateDeviceRGB();
    let bytes_per_row = w as usize * 4;
    let ctx = CGBitmapContextCreate(
        bgra.as_mut_ptr() as *mut c_void,
        w as usize,
        h as usize,
        8,
        bytes_per_row,
        cs,
        BITMAP_INFO,
    );
    if ctx.is_null() {
        CGColorSpaceRelease(cs);
        return Err(Error::EncodeError(
            "CGBitmapContextCreate failed while encoding JPEG".into(),
        ));
    }

    let image = CGBitmapContextCreateImage(ctx);
    CGContextRelease(ctx);
    CGColorSpaceRelease(cs);

    if image.is_null() {
        return Err(Error::EncodeError(
            "CGBitmapContextCreateImage failed while encoding JPEG".into(),
        ));
    }

    Ok(image)
}

unsafe fn get_gif_frame_delay_ms(src: CGImageSourceRef, index: usize) -> i32 {
    let props = CGImageSourceCopyPropertiesAtIndex(src, index, std::ptr::null());
    if props.is_null() {
        return 100;
    }

    let gif_props = CFDictionaryGetValue(props, kCGImagePropertyGIFDictionary as CFTypeRef);
    if gif_props.is_null() {
        CFRelease(props as CFTypeRef);
        return 100;
    }

    let delay_ref = CFDictionaryGetValue(
        gif_props as CFDictionaryRef,
        kCGImagePropertyGIFDelayTime as CFTypeRef,
    );
    if delay_ref.is_null() {
        CFRelease(props as CFTypeRef);
        return 100;
    }

    let mut delay: f32 = 0.1;
    let ok = CFNumberGetValue(
        delay_ref as CFNumberRef,
        CF_NUMBER_FLOAT32_TYPE,
        &mut delay as *mut f32 as *mut c_void,
    );

    CFRelease(props as CFTypeRef);

    if ok == 0 {
        100
    } else {
        ((delay.max(0.01) * 1000.0).round() as i32).max(10)
    }
}

unsafe fn encode_animated_gif(frames: &[(Vec<u8>, i32)], w: u32, h: u32) -> Result<Vec<u8>, Error> {
    if frames.is_empty() {
        return Err(Error::EncodeError(
            "cannot encode GIF with zero frames".into(),
        ));
    }

    let out_data = CFDataCreateMutable(std::ptr::null(), 0);
    if out_data.is_null() {
        return Err(Error::EncodeError(
            "CFDataCreateMutable returned null".into(),
        ));
    }

    let dest =
        CGImageDestinationCreateWithData(out_data, kUTTypeGIF, frames.len(), std::ptr::null());
    if dest.is_null() {
        CFRelease(out_data as CFTypeRef);
        return Err(Error::EncodeError(
            "CGImageDestinationCreateWithData failed for GIF".into(),
        ));
    }

    let loop_count: i32 = 0;
    let loop_count_ref = CFNumberCreate(
        std::ptr::null(),
        CF_NUMBER_SINT32_TYPE,
        &loop_count as *const i32 as *const c_void,
    );
    if loop_count_ref.is_null() {
        CFRelease(dest as CFTypeRef);
        CFRelease(out_data as CFTypeRef);
        return Err(Error::EncodeError(
            "CFNumberCreate failed for GIF loop count".into(),
        ));
    }

    let gif_dict =
        CFDictionaryCreateMutable(std::ptr::null(), 1, std::ptr::null(), std::ptr::null());
    if gif_dict.is_null() {
        CFRelease(loop_count_ref as CFTypeRef);
        CFRelease(dest as CFTypeRef);
        CFRelease(out_data as CFTypeRef);
        return Err(Error::EncodeError(
            "CFDictionaryCreateMutable failed for GIF container properties".into(),
        ));
    }
    CFDictionarySetValue(
        gif_dict,
        kCGImagePropertyGIFLoopCount as *const c_void,
        loop_count_ref as *const c_void,
    );

    let container_props =
        CFDictionaryCreateMutable(std::ptr::null(), 1, std::ptr::null(), std::ptr::null());
    if container_props.is_null() {
        CFRelease(gif_dict as CFTypeRef);
        CFRelease(loop_count_ref as CFTypeRef);
        CFRelease(dest as CFTypeRef);
        CFRelease(out_data as CFTypeRef);
        return Err(Error::EncodeError(
            "CFDictionaryCreateMutable failed for top-level GIF properties".into(),
        ));
    }
    CFDictionarySetValue(
        container_props,
        kCGImagePropertyGIFDictionary as *const c_void,
        gif_dict as *const c_void,
    );
    CGImageDestinationSetProperties(dest, container_props as CFDictionaryRef);

    for (pixels, delay_ms) in frames {
        let delay = (*delay_ms).max(10) as f32 / 1000.0;
        let delay_ref = CFNumberCreate(
            std::ptr::null(),
            CF_NUMBER_FLOAT32_TYPE,
            &delay as *const f32 as *const c_void,
        );
        if delay_ref.is_null() {
            CFRelease(container_props as CFTypeRef);
            CFRelease(gif_dict as CFTypeRef);
            CFRelease(loop_count_ref as CFTypeRef);
            CFRelease(dest as CFTypeRef);
            CFRelease(out_data as CFTypeRef);
            return Err(Error::EncodeError(
                "CFNumberCreate failed for GIF delay".into(),
            ));
        }

        let frame_gif_dict =
            CFDictionaryCreateMutable(std::ptr::null(), 1, std::ptr::null(), std::ptr::null());
        if frame_gif_dict.is_null() {
            CFRelease(delay_ref as CFTypeRef);
            CFRelease(container_props as CFTypeRef);
            CFRelease(gif_dict as CFTypeRef);
            CFRelease(loop_count_ref as CFTypeRef);
            CFRelease(dest as CFTypeRef);
            CFRelease(out_data as CFTypeRef);
            return Err(Error::EncodeError(
                "CFDictionaryCreateMutable failed for GIF frame properties".into(),
            ));
        }
        CFDictionarySetValue(
            frame_gif_dict,
            kCGImagePropertyGIFDelayTime as *const c_void,
            delay_ref as *const c_void,
        );

        let frame_props =
            CFDictionaryCreateMutable(std::ptr::null(), 1, std::ptr::null(), std::ptr::null());
        if frame_props.is_null() {
            CFRelease(frame_gif_dict as CFTypeRef);
            CFRelease(delay_ref as CFTypeRef);
            CFRelease(container_props as CFTypeRef);
            CFRelease(gif_dict as CFTypeRef);
            CFRelease(loop_count_ref as CFTypeRef);
            CFRelease(dest as CFTypeRef);
            CFRelease(out_data as CFTypeRef);
            return Err(Error::EncodeError(
                "CFDictionaryCreateMutable failed for GIF image properties".into(),
            ));
        }
        CFDictionarySetValue(
            frame_props,
            kCGImagePropertyGIFDictionary as *const c_void,
            frame_gif_dict as *const c_void,
        );

        let image = make_cgimage_from_rgba(pixels, w, h)?;
        CGImageDestinationAddImage(dest, image, frame_props as CFDictionaryRef);
        CGImageRelease(image);
        CFRelease(frame_props as CFTypeRef);
        CFRelease(frame_gif_dict as CFTypeRef);
        CFRelease(delay_ref as CFTypeRef);
    }

    CFRelease(container_props as CFTypeRef);
    CFRelease(gif_dict as CFTypeRef);
    CFRelease(loop_count_ref as CFTypeRef);

    if CGImageDestinationFinalize(dest) == 0 {
        CFRelease(dest as CFTypeRef);
        CFRelease(out_data as CFTypeRef);
        return Err(Error::EncodeError(
            "CGImageDestinationFinalize failed".into(),
        ));
    }

    let len = CFDataGetLength(out_data as CFDataRef);
    let ptr = CFDataGetBytePtr(out_data as CFDataRef);
    let bytes = if len <= 0 || ptr.is_null() {
        Vec::new()
    } else {
        std::slice::from_raw_parts(ptr, len as usize).to_vec()
    };

    CFRelease(dest as CFTypeRef);
    CFRelease(out_data as CFTypeRef);
    Ok(bytes)
}

unsafe fn transcode_gif(
    src: CGImageSourceRef,
    frame_count: usize,
    options: CompressOptions,
) -> Result<Vec<u8>, Error> {
    let (first_pixels, src_w, src_h) = decode_frame(src, 0)?;
    let (target_w, target_h) =
        compute_target_dimensions(src_w, src_h, options.min_width, options.min_height);

    let mut frames = Vec::with_capacity(frame_count);
    frames.push((
        resize::resize_rgba_nearest(&first_pixels, src_w, src_h, target_w, target_h),
        get_gif_frame_delay_ms(src, 0),
    ));

    for index in 1..frame_count {
        let (pixels, w, h) = decode_frame(src, index)?;
        frames.push((
            resize::resize_rgba_nearest(&pixels, w, h, target_w, target_h),
            get_gif_frame_delay_ms(src, index),
        ));
    }

    encode_animated_gif(&encode::merge_frames_min_delay(frames), target_w, target_h)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn compress(input: &[u8], options: CompressOptions) -> Result<Vec<u8>, Error> {
    unsafe {
        let detected = crate::compress::image::ImageFormat::detect(input);
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

        let result = if matches!(detected, Some(crate::compress::image::ImageFormat::Gif)) {
            transcode_gif(src, count, options)
        } else if count == 1 {
            // ── Static image ──────────────────────────────────────────────
            let (pixels, w, h) = decode_frame(src, 0)?;
            let (target_w, target_h) =
                compute_target_dimensions(w, h, options.min_width, options.min_height);
            let resized = resize::resize_rgba_nearest(&pixels, w, h, target_w, target_h);
            super::turbojpeg_encode::encode_rgba_to_jpeg(
                &resized,
                target_w,
                target_h,
                options.quality,
            )
        } else {
            // ── Animated non-GIF image ─────────────────────────────────────
            // Export a JPEG poster frame.
            let (first_pixels, w, h) = decode_frame(src, 0)?;
            let (target_w, target_h) =
                compute_target_dimensions(w, h, options.min_width, options.min_height);
            let first_resized =
                resize::resize_rgba_nearest(&first_pixels, w, h, target_w, target_h);
            super::turbojpeg_encode::encode_rgba_to_jpeg(
                &first_resized,
                target_w,
                target_h,
                options.quality,
            )
        };

        CFRelease(src as CFTypeRef);
        CFRelease(data_ref as CFTypeRef);

        result
    }
}
