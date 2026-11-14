//! Browser-only WASM integration tests — run with:
//!   wasm-pack test --headless --chrome --test compress_images_wasm_browser
//!
//! These tests exercise `compress_image_js`:
//! - GIF input is decoded and re-encoded as GIF by the shared zengif codec
//!   (per-frame delays preserved), identically to the native platforms.
//! - Every other input is encoded as static WebP via `createImageBitmap` +
//!   `OffscreenCanvas`.
//! - WebP input is rejected before any browser API is touched.

#![cfg(target_arch = "wasm32")]

use media_compress::compress_image_js;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

static JPEG_BYTES: &[u8] = include_bytes!("../test_images/portrait_2.jpg");
static PNG_BYTES: &[u8] = include_bytes!("../test_images/test_image.png");
static GIF_BYTES: &[u8] = include_bytes!("../test_images/test_image.gif");
static BMP_BYTES: &[u8] = include_bytes!("../test_images/test_image.bmp");
static WEBP_BYTES: &[u8] = include_bytes!("../test_images/test_image.webp");

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[8..12] == b"WEBP"
}

fn is_gif(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

fn detect_format(bytes: &[u8]) -> &'static str {
    if is_webp(bytes) {
        "WebP"
    } else if is_gif(bytes) {
        "GIF"
    } else {
        "unknown"
    }
}

/// Decode output bytes in the browser and return their intrinsic dimensions.
/// Used to verify that the canvas path actually applies min_width/min_height.
async fn decoded_dimensions(bytes: &[u8]) -> Result<(u32, u32), JsValue> {
    let array = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let blob = web_sys::Blob::new_with_u8_array_sequence(&array)?;
    let global = js_sys::global();
    let create_image_bitmap =
        js_sys::Reflect::get(&global, &JsValue::from_str("createImageBitmap"))?
            .dyn_into::<js_sys::Function>()
            .map_err(|_| JsValue::from_str("createImageBitmap is not available"))?;
    let promise = create_image_bitmap
        .call1(&global, &blob)?
        .dyn_into::<js_sys::Promise>()?;
    let bitmap = wasm_bindgen_futures::JsFuture::from(promise)
        .await?
        .dyn_into::<web_sys::ImageBitmap>()?;
    let dimensions = (bitmap.width(), bitmap.height());
    bitmap.close();
    Ok(dimensions)
}

/// Compress, log stats including detected output format, return bytes.
async fn compress_and_log(label: &str, input: &[u8], quality: f32) -> Result<Vec<u8>, JsValue> {
    let out = compress_image_js(input, quality, None, None).await?.to_vec();
    let ratio = out.len() as f64 / input.len() as f64 * 100.0;
    web_sys::console::log_1(
        &format!(
            "[compress_images_wasm_browser] {label}  {orig} → {compressed} bytes  ({ratio:.1}%)  [{fmt}]",
            orig = input.len(),
            compressed = out.len(),
            fmt = detect_format(&out),
        )
        .into(),
    );
    Ok(out)
}

#[wasm_bindgen_test]
async fn compress_jpeg_to_webp() -> Result<(), JsValue> {
    let bytes = compress_and_log("JPEG q=75", JPEG_BYTES, 75.0).await?;
    assert!(
        is_webp(&bytes),
        "output is not WebP (first 12 bytes: {:?})",
        &bytes[..12.min(bytes.len())]
    );
    Ok(())
}

#[wasm_bindgen_test]
async fn compress_png_to_webp() -> Result<(), JsValue> {
    let bytes = compress_and_log("PNG  q=75", PNG_BYTES, 75.0).await?;
    assert!(is_webp(&bytes), "output is not WebP");
    Ok(())
}

#[wasm_bindgen_test]
async fn min_dimensions_resize_webp_output() -> Result<(), JsValue> {
    let (src_w, src_h) = decoded_dimensions(PNG_BYTES).await?;
    assert!(
        src_w > 512 && src_h > 512,
        "fixture should be larger than 512x512, got {src_w}x{src_h}"
    );

    let out = compress_image_js(PNG_BYTES, 75.0, Some(512), Some(512))
        .await?
        .to_vec();
    assert!(
        is_webp(&out),
        "expected WebP output (first 12 bytes: {:?})",
        &out[..12.min(out.len())]
    );

    let (out_w, out_h) = decoded_dimensions(&out).await?;

    // Same formula as compute_target_dimensions: preserve aspect ratio,
    // shrink only, and keep the stricter of the two minimum constraints.
    let scale = (512.0 / src_w as f64)
        .max(512.0 / src_h as f64)
        .clamp(0.0, 1.0);
    let expected_w = ((src_w as f64 * scale).ceil() as u32).clamp(1, src_w);
    let expected_h = ((src_h as f64 * scale).ceil() as u32).clamp(1, src_h);

    assert_eq!(
        (out_w, out_h),
        (expected_w, expected_h),
        "min_width/min_height must resize the canvas output (source {src_w}x{src_h})"
    );
    assert!(
        out_w < src_w && out_h < src_h,
        "output {out_w}x{out_h} must be smaller than source {src_w}x{src_h}"
    );

    Ok(())
}

#[wasm_bindgen_test]
async fn compress_gif_to_gif() -> Result<(), JsValue> {
    let bytes = compress_and_log("GIF  q=75", GIF_BYTES, 75.0).await?;
    assert!(
        is_gif(&bytes),
        "expected GIF output for GIF input (first 12 bytes: {:?})",
        &bytes[..12.min(bytes.len())]
    );
    Ok(())
}

#[wasm_bindgen_test]
async fn compress_bmp_to_webp() -> Result<(), JsValue> {
    let bytes = compress_and_log("BMP  q=75", BMP_BYTES, 75.0).await?;
    assert!(is_webp(&bytes), "output is not WebP");
    Ok(())
}

#[wasm_bindgen_test]
async fn compress_webp_input_is_rejected() {
    let err = compress_image_js(WEBP_BYTES, 75.0, None, None)
        .await
        .expect_err("WebP input should be rejected");
    let msg = err.as_string().unwrap_or_default();
    assert!(msg.contains("WebP"), "unexpected error message: {msg}");
}

#[wasm_bindgen_test]
async fn lower_quality_produces_smaller_output() -> Result<(), JsValue> {
    let high = compress_and_log("JPEG q=90", JPEG_BYTES, 90.0).await?;
    let low = compress_and_log("JPEG q=10", JPEG_BYTES, 10.0).await?;
    assert!(
        low.len() < high.len(),
        "quality=10 ({} bytes) should be smaller than quality=90 ({} bytes)",
        low.len(),
        high.len()
    );
    Ok(())
}
