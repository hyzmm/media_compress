//! Browser-only WASM integration tests — run with:
//!   wasm-pack test --headless --chrome --test compress_images_wasm_browser
//!
//! These tests exercise `compress_image_js`, which delegates encoding to the
//! browser's `createImageBitmap` + `OffscreenCanvas` Canvas API.
//! On browsers that support WebP encoding (Chrome, Firefox) the output is WebP.
//! On browsers that do not (Safari), the JS helper falls back to JPEG.

#![cfg(target_arch = "wasm32")]

use media_compress::compress_image_js;
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

static JPEG_BYTES: &[u8] = include_bytes!("../test_images/test_image.jpg");
static PNG_BYTES: &[u8] = include_bytes!("../test_images/test_image.png");
static GIF_BYTES: &[u8] = include_bytes!("../test_images/test_image.gif");
static BMP_BYTES: &[u8] = include_bytes!("../test_images/test_image.bmp");

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\xff\xd8\xff")
}

fn detect_format(bytes: &[u8]) -> &'static str {
    if is_webp(bytes) {
        "WebP"
    } else if is_jpeg(bytes) {
        "JPEG (fallback)"
    } else {
        "unknown"
    }
}

/// Compress, log stats including detected output format, return bytes.
async fn compress_and_log(label: &str, input: &[u8], quality: f32) -> Result<Vec<u8>, JsValue> {
    let out = compress_image_js(input, quality).await?.to_vec();
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
async fn compress_jpeg_to_webp_or_jpeg() -> Result<(), JsValue> {
    let bytes = compress_and_log("JPEG q=75", JPEG_BYTES, 75.0).await?;
    assert!(
        is_webp(&bytes) || is_jpeg(&bytes),
        "output is neither WebP nor JPEG (first 12 bytes: {:?})",
        &bytes[..12.min(bytes.len())]
    );
    Ok(())
}

#[wasm_bindgen_test]
async fn compress_png_to_webp_or_jpeg() -> Result<(), JsValue> {
    let bytes = compress_and_log("PNG  q=75", PNG_BYTES, 75.0).await?;
    assert!(
        is_webp(&bytes) || is_jpeg(&bytes),
        "output is neither WebP nor JPEG"
    );
    Ok(())
}

#[wasm_bindgen_test]
async fn compress_gif_to_webp_or_jpeg() -> Result<(), JsValue> {
    let bytes = compress_and_log("GIF  q=75", GIF_BYTES, 75.0).await?;
    assert!(
        is_webp(&bytes) || is_jpeg(&bytes),
        "output is neither WebP nor JPEG"
    );
    Ok(())
}

#[wasm_bindgen_test]
async fn compress_bmp_to_webp_or_jpeg() -> Result<(), JsValue> {
    let bytes = compress_and_log("BMP  q=75", BMP_BYTES, 75.0).await?;
    assert!(
        is_webp(&bytes) || is_jpeg(&bytes),
        "output is neither WebP nor JPEG"
    );
    Ok(())
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
