use crate::compress::image::compute_target_dimensions;
use crate::compress::image::CompressOptions;
use crate::compress::image::ImageFormat;
use crate::compress::image::WEBP_INPUT_REJECTED;
use crate::error::Error;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, ImageBitmap, ImageBitmapRenderingContext, ImageEncodeOptions, OffscreenCanvas};

/// Decode raw image bytes with the browser-native `createImageBitmap`
/// (JPEG, PNG, GIF, BMP, AVIF — varies by browser). For animated
/// inputs this yields the first frame only.
///
/// Resolved off the global object rather than `window` so the same code also
/// runs inside a `Worker`, which has no `window` (both `Window` and
/// `WorkerGlobalScope` expose `createImageBitmap`).
async fn decode_to_bitmap(input: &[u8]) -> Result<ImageBitmap, JsValue> {
    let bytes = Uint8Array::from(input);
    let array = js_sys::Array::of1(&bytes);

    let blob = Blob::new_with_u8_array_sequence(&array)?;
    let global = js_sys::global();
    let create_image_bitmap = js_sys::Reflect::get(&global, &JsValue::from_str("createImageBitmap"))?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| JsValue::from_str("createImageBitmap is not available"))?;
    let promise = create_image_bitmap.call1(&global, &blob)?.dyn_into::<js_sys::Promise>()?;

    JsFuture::from(promise).await?.dyn_into::<ImageBitmap>()
}

/// Web/WASM encoder: draw onto `OffscreenCanvas` and re-encode to WebP.
/// Only the first frame of an animated input is kept (the canvas API cannot
/// encode animations). GIF input is handled before this path.
async fn canvas_compress(
    input: &[u8],
    quality: f32,
    min_width: Option<u32>,
    min_height: Option<u32>,
) -> Result<Uint8Array, JsValue> {
    let bitmap = decode_to_bitmap(input).await?;

    let (target_w, target_h) =
        compute_target_dimensions(bitmap.width(), bitmap.height(), min_width, min_height);

    let canvas = OffscreenCanvas::new(target_w, target_h)?;
    let ctx = canvas
        .get_context("bitmaprenderer")?
        .expect("bitmaprenderer context not available")
        .dyn_into::<ImageBitmapRenderingContext>()?;
    ctx.transfer_from_image_bitmap(&bitmap);

    let mut webp_opts = ImageEncodeOptions::new();
    webp_opts.type_("image/webp");
    webp_opts.quality((quality as f64) / 100.0);
    let webp_result = JsFuture::from(canvas.convert_to_blob_with_options(&webp_opts)?).await?;
    let blob = webp_result.dyn_into::<Blob>()?;

    let array_buf = JsFuture::from(blob.array_buffer()).await?;
    Ok(Uint8Array::new(&array_buf))
}

/// Compress raw image bytes on the WASM/Web platform.
///
/// WebP input is rejected — it already is the output format. GIF input is
/// decoded and re-encoded as GIF by the shared zengif codec (per-frame delays
/// and the loop count preserved, same as native). Every other format is
/// decoded browser-native via `createImageBitmap` and encoded to static WebP
/// through `OffscreenCanvas.convertToBlob`; animated non-GIF inputs keep
/// their first frame.
///
/// # Arguments
/// * `input`   — raw bytes of the source image
/// * `quality` — lossy quality, 0–100 (drives the WebP encoder; the GIF
///   quantizer currently ignores it)
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
    if detected == Some(ImageFormat::Webp) {
        return Err(JsValue::from_str(WEBP_INPUT_REJECTED));
    }
    let may_fallback = detected
        .as_ref()
        .map_or(false, |fmt| fmt.should_use_original_if_larger());

    let js_bytes = Uint8Array::from(input);

    // GIF is handled by the shared zengif codec (same path as native).
    if matches!(detected, Some(ImageFormat::Gif)) {
        let options = CompressOptions {
            quality,
            min_width,
            min_height,
        };
        let out = super::gif::compress_gif(input, options)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let bytes = Uint8Array::from(out.as_slice());
        if may_fallback && bytes.length() as usize > input.len() {
            return Ok(js_bytes);
        }
        return Ok(bytes);
    }

    let compressed = canvas_compress(input, quality, min_width, min_height).await?;

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
