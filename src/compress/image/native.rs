use crate::error::Error;
use super::ImageFormat;

pub fn decode_native(input: &[u8], _format: &ImageFormat, quality: f32) -> Result<Vec<u8>, Error> {
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    {
        apple::decode_with_imageio(input, quality)
    }

    #[cfg(not(any(target_os = "ios", target_os = "macos")))]
    {
        let _ = (input, quality);
        Err(Error::PlatformNotSupported(format!(
            "{:?} decoding is only supported on iOS/macOS",
            _format
        )))
    }
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
mod apple {
    use crate::error::Error;

    // ImageIO and CoreFoundation types (opaque pointers)
    enum CGImageSourceOpaque {}
    enum CGImageOpaque {}
    enum CFDataOpaque {}
    enum CFDictionaryOpaque {}

    type CGImageSourceRef = *mut CGImageSourceOpaque;
    type CGImageRef = *mut CGImageOpaque;
    type CFDataRef = *const CFDataOpaque;
    type CFDictionaryRef = *const CFDictionaryOpaque;
    type CGFloat = f64;

    #[link(name = "ImageIO", kind = "framework")]
    extern "C" {
        fn CGImageSourceCreateWithData(
            data: CFDataRef,
            options: CFDictionaryRef,
        ) -> CGImageSourceRef;
        fn CGImageSourceCreateImageAtIndex(
            source: CGImageSourceRef,
            index: usize,
            options: CFDictionaryRef,
        ) -> CGImageRef;
        fn CGImageGetWidth(image: CGImageRef) -> usize;
        fn CGImageGetHeight(image: CGImageRef) -> usize;
        fn CGImageRelease(image: CGImageRef);
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDataCreate(
            allocator: *const std::ffi::c_void,
            bytes: *const u8,
            length: isize,
        ) -> CFDataRef;
    }

    // CoreGraphics bitmap context for pixel extraction
    enum CGBitmapContextOpaque {}
    type CGContextRef = *mut CGBitmapContextOpaque;
    type CGColorSpaceRef = *mut std::ffi::c_void;
    type CGBitmapInfo = u32;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
        fn CGColorSpaceRelease(cs: CGColorSpaceRef);
        fn CGBitmapContextCreate(
            data: *mut u8,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            color_space: CGColorSpaceRef,
            bitmap_info: CGBitmapInfo,
        ) -> CGContextRef;
        fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, image: CGImageRef);
        fn CGContextRelease(ctx: CGContextRef);
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: CGFloat,
        y: CGFloat,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: CGFloat,
        height: CGFloat,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    // kCGImageAlphaPremultipliedLast = 1
    const BITMAP_INFO_RGBA: CGBitmapInfo = 1;

    pub fn decode_with_imageio(input: &[u8], quality: f32) -> Result<Vec<u8>, Error> {
        unsafe {
            let cf_data = CFDataCreate(
                std::ptr::null(),
                input.as_ptr(),
                input.len() as isize,
            );
            if cf_data.is_null() {
                return Err(Error::NativeError("CFDataCreate failed".into()));
            }

            let source = CGImageSourceCreateWithData(cf_data, std::ptr::null());
            CFRelease(cf_data as *const _);
            if source.is_null() {
                return Err(Error::NativeError("CGImageSourceCreateWithData failed".into()));
            }

            let cg_image = CGImageSourceCreateImageAtIndex(source, 0, std::ptr::null());
            CFRelease(source as *const _);
            if cg_image.is_null() {
                return Err(Error::NativeError("CGImageSourceCreateImageAtIndex failed".into()));
            }

            let width = CGImageGetWidth(cg_image);
            let height = CGImageGetHeight(cg_image);

            let bytes_per_row = width * 4;
            let mut pixel_data: Vec<u8> = vec![0u8; height * bytes_per_row];

            let cs = CGColorSpaceCreateDeviceRGB();
            let ctx = CGBitmapContextCreate(
                pixel_data.as_mut_ptr(),
                width,
                height,
                8,
                bytes_per_row,
                cs,
                BITMAP_INFO_RGBA,
            );
            CGColorSpaceRelease(cs);

            if ctx.is_null() {
                CGImageRelease(cg_image);
                return Err(Error::NativeError("CGBitmapContextCreate failed".into()));
            }

            let rect = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: width as CGFloat,
                    height: height as CGFloat,
                },
            };
            CGContextDrawImage(ctx, rect, cg_image);
            CGContextRelease(ctx);
            CGImageRelease(cg_image);

            let encoder = webp::Encoder::from_rgba(&pixel_data, width as u32, height as u32);
            let encoded = encoder.encode(quality);
            Ok(encoded.to_vec())
        }
    }
}
