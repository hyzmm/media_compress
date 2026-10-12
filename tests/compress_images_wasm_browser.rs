//! Browser-only WASM integration tests — run with:
//!   wasm-pack test --headless --chrome --test compress_images_wasm_browser
//!
//! These tests exercise `compress_image_js`:
//! - Default canvas build: every input (GIF included, first frame only) is
//!   encoded as static WebP via `createImageBitmap` + `OffscreenCanvas`.
//! - `web-zenwebp` build: GIFs are encoded as animated WebP (frames and
//!   delays extracted with the browser's WebCodecs `ImageDecoder`, encoded
//!   with zenwebp); other inputs are static WebP.

#![cfg(target_arch = "wasm32")]

use media_compress::compress_image_js;
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

static JPEG_BYTES: &[u8] = include_bytes!("../test_images/portrait_2.jpg");
static PNG_BYTES: &[u8] = include_bytes!("../test_images/test_image.png");
static GIF_BYTES: &[u8] = include_bytes!("../test_images/test_image.gif");
static BMP_BYTES: &[u8] = include_bytes!("../test_images/test_image.bmp");

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[8..12] == b"WEBP"
}

fn is_gif(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

/// Walk the RIFF chunks of a WebP file looking for the "ANIM" chunk
/// (present only in animated WebP). Only used by the web-zenwebp build.
#[cfg(feature = "web-zenwebp")]
fn has_anim_chunk(bytes: &[u8]) -> bool {
    if !is_webp(bytes) {
        return false;
    }
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"ANIM" {
            return true;
        }
        pos += 8 + size + (size & 1);
    }
    false
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
async fn compress_gif_to_webp() -> Result<(), JsValue> {
    let bytes = compress_and_log("GIF  q=75", GIF_BYTES, 75.0).await?;
    assert!(is_webp(&bytes), "output is not WebP");

    // The web-zenwebp build emits an animated WebP when the browser provides
    // the WebCodecs ImageDecoder API (Chrome); the default canvas build
    // always emits a static poster frame.
    #[cfg(feature = "web-zenwebp")]
    assert!(
        has_anim_chunk(&bytes),
        "expected animated WebP (ANIM chunk) for GIF input in the web-zenwebp build"
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
