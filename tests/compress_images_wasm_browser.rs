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
