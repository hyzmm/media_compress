use crate::compress::image::compute_target_dimensions;
use crate::compress::image::CompressOptions;
use crate::compress::image::ImageFormat;
use crate::error::Error;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, ImageBitmapRenderingContext, ImageEncodeOptions, OffscreenCanvas};

/// Decode raw image bytes with `createImageBitmap`, draw onto `OffscreenCanvas`,
/// and re-encode to JPEG (or GIF for GIF input when browser supports GIF encode).
async fn canvas_compress(
    input: &[u8],
    input_is_gif: bool,
    quality: f32,
    min_width: Option<u32>,
    min_height: Option<u32>,
) -> Result<Uint8Array, JsValue> {
    let bytes = Uint8Array::from(input);
    let array = js_sys::Array::of1(&bytes);

    let blob = Blob::new_with_u8_array_sequence(&array)?;
    let bitmap = {
        let window = web_sys::window().expect("no window");
        let promise = window.create_image_bitmap_with_blob(&blob)?;
        JsFuture::from(promise)
            .await?
            .dyn_into::<web_sys::ImageBitmap>()?
    };

    let (target_w, target_h) =
        compute_target_dimensions(bitmap.width(), bitmap.height(), min_width, min_height);

    let canvas = OffscreenCanvas::new(target_w, target_h)?;
    let ctx = canvas
        .get_context("bitmaprenderer")?
        .expect("bitmaprenderer context not available")
        .dyn_into::<ImageBitmapRenderingContext>()?;
    ctx.transfer_from_image_bitmap(&bitmap);

    let blob = if input_is_gif {
        let mut gif_opts = ImageEncodeOptions::new();
        gif_opts.type_("image/gif");
        let gif_result = JsFuture::from(canvas.convert_to_blob_with_options(&gif_opts)?).await?;
        let gif_blob = gif_result.dyn_into::<Blob>()?;
        if gif_blob.type_() == "image/gif" {
            gif_blob
        } else {
            Blob::new_with_u8_array_sequence(&js_sys::Array::of1(&Uint8Array::from(input)))?
        }
    } else {
        let mut jpeg_opts = ImageEncodeOptions::new();
        jpeg_opts.type_("image/jpeg");
        jpeg_opts.quality((quality as f64) / 100.0);
        let jpeg_result = JsFuture::from(canvas.convert_to_blob_with_options(&jpeg_opts)?).await?;
        jpeg_result.dyn_into::<Blob>()?
    };

    let array_buf = JsFuture::from(blob.array_buffer()).await?;
    Ok(Uint8Array::new(&array_buf))
}

/// Compress raw image bytes using the browser's Canvas API.
///
/// This is the async, browser-native alternative to `compress_image` on the
/// WASM/Web platform. Decoding is handled by `createImageBitmap` (supports
/// JPEG, PNG, GIF, BMP, WebP, AVIF — varies by browser), and encoding is
/// done via `OffscreenCanvas.convertToBlob`.
///
/// # Arguments
/// * `input`   — raw bytes of the source image
/// * `quality` — JPEG quality, 0–100
/// * `min_width` / `min_height` — optional lower bound for output dimensions.
///   The output keeps the original aspect ratio and never upscales.
///
/// # JS usage
/// ```js
/// import init, { compress_image_js } from './media_compress.js';
/// await init();
/// const bytes = await compress_image_js(imageBytes, 75, 1280, 720);
/// ```
#[wasm_bindgen]
pub async fn compress_image_js(
    input: &[u8],
    quality: f32,
    min_width: Option<u32>,
    min_height: Option<u32>,
) -> Result<Uint8Array, JsValue> {
    let detected = ImageFormat::detect(input);
    let input_is_gif = matches!(detected, Some(ImageFormat::Gif));
    if input_is_gif {
        return Err(JsValue::from_str("GIF compression is not supported"));
    }
    let may_fallback = detected.map_or(false, |fmt| fmt.should_use_original_if_larger());

    let js_bytes = Uint8Array::from(input);
    let compressed = canvas_compress(input, input_is_gif, quality, min_width, min_height).await?;

    if may_fallback && compressed.length() as usize > input.len() {
        return Ok(js_bytes);
    }
    Ok(compressed)
}

/// Synchronous path — always returns `PlatformNotSupported`.
/// Call [`compress_image_js`] (async) on the Web platform instead.
pub fn compress(_input: &[u8], _options: CompressOptions) -> Result<Vec<u8>, Error> {
    Err(Error::PlatformNotSupported(
        "Use compress_image_js() (async) on the Web platform".into(),
    ))
}
