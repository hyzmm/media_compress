use crate::compress::image::ImageFormat;
use crate::error::Error;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

// ---------------------------------------------------------------------------
// Inline JS helper
//
// Uses the browser's `createImageBitmap` + `OffscreenCanvas.convertToBlob`
// to decode any browser-supported image format and re-encode it as WebP.
// Both APIs are asynchronous, so the function returns a Promise.
// ---------------------------------------------------------------------------
#[wasm_bindgen(inline_js = "
export function _mc_to_webp(bytes, quality) {
    var blob = new Blob([bytes]);
    return createImageBitmap(blob)
        .then(function(bitmap) {
            var canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
            var ctx = canvas.getContext('2d');
            ctx.drawImage(bitmap, 0, 0);
            return canvas.convertToBlob({ type: 'image/webp', quality: quality / 100.0 })
                .then(function(b) {
                    if (b.type === 'image/webp') {
                        return b;
                    }
                    // Browser does not support WebP encoding (e.g. Safari) — fall back to JPEG.
                    return canvas.convertToBlob({ type: 'image/jpeg', quality: quality / 100.0 });
                });
        })
        .then(function(b) { return b.arrayBuffer(); })
        .then(function(buf) { return new Uint8Array(buf); });
}
")]
extern "C" {
    #[wasm_bindgen(catch)]
    fn _mc_to_webp(bytes: &Uint8Array, quality: f32) -> Result<js_sys::Promise, JsValue>;
}

/// Compress raw image bytes to lossy WebP using the browser's Canvas API.
///
/// This is the async, browser-native alternative to `compress_image` on the
/// WASM/Web platform. Decoding is handled by `createImageBitmap` (supports
/// JPEG, PNG, GIF, BMP, WebP, AVIF — varies by browser), and encoding is
/// done via `OffscreenCanvas.convertToBlob`.
///
/// # Arguments
/// * `input`   — raw bytes of the source image
/// * `quality` — WebP lossy quality, 0–100
///
/// # JS usage
/// ```js
/// import init, { compress_image_js } from './media_compress.js';
/// await init();
/// const webpBytes = await compress_image_js(imageBytes, 75);
/// ```
#[wasm_bindgen]
pub async fn compress_image_js(input: &[u8], quality: f32) -> Result<Uint8Array, JsValue> {
    let may_fallback =
        ImageFormat::detect(input).map_or(false, |fmt| fmt.should_use_original_if_larger());

    let js_bytes = Uint8Array::from(input);
    let promise = _mc_to_webp(&js_bytes, quality)?;
    let result = JsFuture::from(promise).await?;
    let compressed = Uint8Array::new(&result);

    if may_fallback && compressed.length() as usize > input.len() {
        return Ok(js_bytes);
    }
    Ok(compressed)
}

/// Synchronous path — always returns `PlatformNotSupported`.
/// Call [`compress_image_js`] (async) on the Web platform instead.
pub fn compress(_input: &[u8], _quality: f32) -> Result<Vec<u8>, Error> {
    Err(Error::PlatformNotSupported(
        "Use compress_image_js() (async) on the Web platform".into(),
    ))
}
