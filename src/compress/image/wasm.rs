use crate::compress::image::compute_target_dimensions;
use crate::compress::image::CompressOptions;
use crate::compress::image::ImageFormat;
use crate::error::Error;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, ImageBitmap, OffscreenCanvas};
#[cfg(not(feature = "web-zenwebp"))]
use web_sys::{ImageBitmapRenderingContext, ImageEncodeOptions};

/// Fallback GIF frame delay when the decoder reports no duration.
#[cfg(feature = "web-zenwebp")]
const DEFAULT_DELAY_MS: i32 = 100;

/// Decode raw image bytes with the browser-native `createImageBitmap`
/// (JPEG, PNG, GIF, BMP, WebP, AVIF — varies by browser). For animated
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

/// Default Web/WASM encoder: draw onto `OffscreenCanvas` and re-encode to WebP.
/// Only the first frame of an animated input is kept (the canvas API cannot
/// encode animations).
#[cfg(not(feature = "web-zenwebp"))]
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

/// `web-zenwebp` encoder: decode with `createImageBitmap`, pull the RGBA pixels
/// back through a 2d canvas, and encode with the pure-Rust zenwebp crate.
/// Only the first frame of an animated input is kept.
#[cfg(feature = "web-zenwebp")]
async fn zenwebp_compress(
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
        .get_context("2d")?
        .expect("2d context not available")
        .dyn_into::<web_sys::OffscreenCanvasRenderingContext2d>()?;
    ctx.draw_image_with_image_bitmap_and_dw_and_dh(
        &bitmap,
        0.0,
        0.0,
        target_w as f64,
        target_h as f64,
    )?;

    let image_data = ctx.get_image_data(0.0, 0.0, target_w as f64, target_h as f64)?;
    let pixels = image_data.data();

    let config = zenwebp::LossyConfig::new()
        .with_quality(quality)
        .with_method(4);
    let webp = zenwebp::EncodeRequest::lossy(
        &config,
        pixels.as_slice(),
        zenwebp::PixelLayout::Rgba8,
        target_w,
        target_h,
    )
    .encode()
    .map_err(|e| JsValue::from_str(&format!("zenwebp encode failed: {e}")))?;

    Ok(Uint8Array::from(webp.as_slice()))
}

/// Minimal hand-written bindings for the WebCodecs `ImageDecoder` API.
///
/// web-sys gates its `ImageDecoder` bindings behind `web_sys_unstable_apis`,
/// which would require a global `RUSTFLAGS` change for every build; these
/// bindings only cover what the animated-GIF path needs. `tracks`/`decode`
/// results (ImageTrackList, ImageDecodeResult) are plain JS objects/dicts and
/// are accessed through `js_sys::Reflect` instead of typed bindings.
#[cfg(feature = "web-zenwebp")]
mod webcodecs {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = "ImageDecoder")]
        pub type ImageDecoder;

        #[wasm_bindgen(constructor, catch)]
        pub fn new(init: &js_sys::Object) -> Result<ImageDecoder, JsValue>;

        #[wasm_bindgen(method, getter)]
        pub fn tracks(this: &ImageDecoder) -> JsValue;

        #[wasm_bindgen(method, catch)]
        pub fn decode(
            this: &ImageDecoder,
            options: &js_sys::Object,
        ) -> Result<js_sys::Promise, JsValue>;

        #[wasm_bindgen(method)]
        pub fn close(this: &ImageDecoder);
    }
}

/// Encode an animated GIF to animated WebP: the browser's WebCodecs
/// `ImageDecoder` extracts every frame (and its duration), then zenwebp
/// encodes the animation. Frame delays are preserved as-is — no merging,
/// no clamping; the last frame keeps its own delay via `finalize`.
///
/// Returns `Err` when the animated path is unavailable (no `ImageDecoder`,
/// single frame, or any decode error); the caller falls back to the
/// poster-frame path.
#[cfg(feature = "web-zenwebp")]
async fn encode_gif_animated_zenwebp(
    input: &[u8],
    quality: f32,
    min_width: Option<u32>,
    min_height: Option<u32>,
) -> Result<Vec<u8>, JsValue> {
    let global = js_sys::global();
    let ctor = js_sys::Reflect::get(&global, &JsValue::from_str("ImageDecoder"))?;
    if ctor.is_undefined() || ctor.is_null() {
        return Err(JsValue::from_str("ImageDecoder is not available"));
    }

    // new ImageDecoder({ data: <gif bytes>, type: "image/gif" })
    let init = js_sys::Object::new();
    js_sys::Reflect::set(
        init.as_ref(),
        &JsValue::from_str("data"),
        Uint8Array::from(input).as_ref(),
    )?;
    js_sys::Reflect::set(
        init.as_ref(),
        &JsValue::from_str("type"),
        &JsValue::from_str("image/gif"),
    )?;
    let decoder = webcodecs::ImageDecoder::new(&init)?;

    // Await `decoder.tracks.ready`, then read the frame count.
    let tracks = js_sys::Reflect::get(decoder.as_ref(), &JsValue::from_str("tracks"))?;
    let ready = js_sys::Reflect::get(&tracks, &JsValue::from_str("ready"))?;
    let ready: js_sys::Promise = ready
        .dyn_into()
        .map_err(|_| JsValue::from_str("ImageTrackList.ready is not a promise"))?;
    JsFuture::from(ready).await?;

    let track = js_sys::Reflect::get(&tracks, &JsValue::from_str("selectedTrack"))?;
    if track.is_null() || track.is_undefined() {
        return Err(JsValue::from_str("no selected image track"));
    }
    let frame_count = js_sys::Reflect::get(&track, &JsValue::from_str("frameCount"))?
        .as_f64()
        .unwrap_or(0.0) as u32;
    if frame_count <= 1 {
        return Err(JsValue::from_str("not a multi-frame image"));
    }

    let mut canvas: Option<(web_sys::OffscreenCanvasRenderingContext2d, u32, u32)> = None;
    let mut frames: Vec<(Vec<u8>, i32)> = Vec::with_capacity(frame_count as usize);

    for i in 0..frame_count {
        let options = js_sys::Object::new();
        js_sys::Reflect::set(
            options.as_ref(),
            &JsValue::from_str("frameIndex"),
            &JsValue::from_f64(i as f64),
        )?;
        let result = JsFuture::from(decoder.decode(&options)?).await?;
        let image = js_sys::Reflect::get(&result, &JsValue::from_str("image"))?
            .dyn_into::<web_sys::VideoFrame>()
            .map_err(|_| JsValue::from_str("decoded frame is not a VideoFrame"))?;

        if canvas.is_none() {
            let (target_w, target_h) = compute_target_dimensions(
                image.coded_width(),
                image.coded_height(),
                min_width,
                min_height,
            );
            let offscreen = OffscreenCanvas::new(target_w, target_h)?;
            let ctx = offscreen
                .get_context("2d")?
                .ok_or_else(|| JsValue::from_str("2d context not available"))?
                .dyn_into::<web_sys::OffscreenCanvasRenderingContext2d>()?;
            canvas = Some((ctx, target_w, target_h));
        }
        let (ctx, target_w, target_h) = canvas
            .as_ref()
            .ok_or_else(|| JsValue::from_str("canvas not initialized"))?;

        // Close the VideoFrame on every path (draw/getImageData may fail).
        let drawn = ctx.draw_image_with_video_frame_and_dw_and_dh(
            &image,
            0.0,
            0.0,
            *target_w as f64,
            *target_h as f64,
        );
        let pixels = drawn.and_then(|_| {
            ctx.get_image_data(0.0, 0.0, *target_w as f64, *target_h as f64)
                .map(|data| data.data().as_slice().to_vec())
        });
        let duration_ms = image
            .duration()
            .map(|micros| (micros / 1000.0).round() as i32)
            .unwrap_or(DEFAULT_DELAY_MS);
        image.close();

        frames.push((pixels?, duration_ms));
    }
    decoder.close();

    let (_, target_w, target_h) = canvas.ok_or_else(|| JsValue::from_str("canvas not initialized"))?;

    // Encode with zenwebp; options mirror the native encoder (libwebp
    // defaults: loop forever, no size minimization).
    let mut encoder = zenwebp::mux::AnimationEncoder::new(
        target_w,
        target_h,
        zenwebp::mux::AnimationConfig {
            minimize_size: false,
            ..Default::default()
        },
    )
    .map_err(|e| JsValue::from_str(&format!("zenwebp animation init failed: {e}")))?;

    let frame_config = zenwebp::EncoderConfig::new_lossy()
        .with_quality(quality)
        .with_method(4);

    let mut ts: u32 = 0;
    let mut last_delay: u32 = 0;
    for (pixels, delay) in &frames {
        encoder
            .add_frame(pixels, zenwebp::PixelLayout::Rgba8, ts, &frame_config)
            .map_err(|e| JsValue::from_str(&format!("zenwebp add_frame failed: {e}")))?;
        let delay = (*delay).max(0) as u32;
        ts = ts.saturating_add(delay);
        last_delay = delay;
    }

    encoder
        .finalize(last_delay)
        .map_err(|e| JsValue::from_str(&format!("zenwebp finalize failed: {e}")))
}

/// Compress raw image bytes to lossy WebP on the WASM/Web platform.
///
/// Decoding is browser-native via `createImageBitmap` (supports JPEG, PNG,
/// GIF, BMP, WebP, AVIF — varies by browser). Encoding goes through
/// `OffscreenCanvas.convertToBlob` by default, or through the pure-Rust
/// `zenwebp` encoder when built with `--features web-zenwebp`.
///
/// Animated GIFs become animated WebP with their original per-frame delays
/// when built with `--features web-zenwebp` and the browser provides the
/// WebCodecs `ImageDecoder` API; otherwise (default canvas build, or no
/// ImageDecoder) they are compressed to a static WebP poster frame. Other
/// animated formats (animated WebP, APNG) always use the poster frame.
///
/// # Arguments
/// * `input`   — raw bytes of the source image
/// * `quality` — WebP lossy quality, 0–100
/// * `min_width` / `min_height` — optional lower bound for output dimensions.
///   The output keeps the original aspect ratio and never upscales.
///
/// # JS usage
/// ```js
/// import init, { compress_image_js } from './media_compress.js';
/// await init();
/// const webpBytes = await compress_image_js(imageBytes, 75, 1280, 720);
/// ```
#[wasm_bindgen]
pub async fn compress_image_js(
    input: &[u8],
    quality: f32,
    min_width: Option<u32>,
    min_height: Option<u32>,
) -> Result<Uint8Array, JsValue> {
    let detected = ImageFormat::detect(input);
    let may_fallback = detected
        .as_ref()
        .map_or(false, |fmt| fmt.should_use_original_if_larger());

    let js_bytes = Uint8Array::from(input);

    #[cfg(feature = "web-zenwebp")]
    let compressed = if matches!(detected, Some(ImageFormat::Gif)) {
        match encode_gif_animated_zenwebp(input, quality, min_width, min_height).await {
            Ok(bytes) => Uint8Array::from(bytes.as_slice()),
            Err(_) => zenwebp_compress(input, quality, min_width, min_height).await?,
        }
    } else {
        zenwebp_compress(input, quality, min_width, min_height).await?
    };

    #[cfg(not(feature = "web-zenwebp"))]
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
