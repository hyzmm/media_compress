//! zengif trial bench: GIF handling today vs zengif.
//!
//! Run: cargo run --release --example gif_zengif_bench [quality]   (default 75)
//!
//! For each test GIF this measures three things:
//!  [1] production baseline: media_compress::compress_image (platform decode +
//!      libwebp animated WebP) — size / PSNR / time
//!  [2] zengif decode + the same libwebp animated encode — isolates the
//!      decoder swap (output size should not change, time shows decode speed)
//!  [3] zengif GIF -> GIF re-encode with several quantizer configs
//!      (size vs the original GIF, PSNR vs the source frames, time)
//!
//! All PSNR values are RGB-only against zengif-decoded source frames
//! (zengif's decoder was cross-checked pixel-exact against ffmpeg).

use std::io::Cursor;
use std::time::Instant;

use libwebp_sys as sys;
use webp::{AnimDecoder, WebPConfig};
use zengif::{
    Decoder, EncodeRequest, EncoderConfig, FrameInput, Limits, Quantizer, Repeat, Unstoppable,
};

const DEFAULT_QUALITY: u8 = 75;

/// (label, path) — the GIF test images in the repo.
const INPUTS: &[(&str, &str)] = &[
    ("test_image.gif", "test_images/test_image.gif"),
    (
        "13424514615469820.gif",
        "test_images/13424514615469820.gif",
    ),
];

struct Decoded {
    w: u16,
    h: u16,
    frames: Vec<(u16, Vec<u8>)>, // (delay_cs, rgba)
}

fn main() {
    let quality: u8 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(DEFAULT_QUALITY);
    println!("zengif trial — GIF compression, q={quality}");
    println!("PSNR: RGB, vs zengif-decoded source frames (verified against ffmpeg)\n");

    for (label, path) in INPUTS {
        run_input(label, path, quality);
    }
}

fn run_input(label: &str, path: &str, quality: u8) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            println!("skip {label}: {e}\n");
            return;
        }
    };
    let src_len = data.len();

    let (src, dec_ms) = bench(5, || zengif_decode(&data));
    let has_alpha = src
        .frames
        .iter()
        .any(|(_, p)| p.chunks_exact(4).any(|px| px[3] != 255));
    let delays_cs: Vec<u16> = src.frames.iter().map(|f| f.0).collect();

    println!(
        "== {label}: {}x{}, {} frames, src {} ==",
        src.w,
        src.h,
        src.frames.len(),
        fmt_bytes(src_len)
    );
    println!(
        "   delays(cs) {:?}{}   alpha: {}   zengif decode {}",
        &delays_cs[..delays_cs.len().min(12)],
        if delays_cs.len() > 12 { "..." } else { "" },
        has_alpha,
        fmt_ms(dec_ms)
    );

    // ---- [1] production baseline -----------------------------------------
    let opts = media_compress::CompressOptions::new(quality as f32);
    let (prod_out, prod_ms) =
        bench(3, || media_compress::compress_image(&data, opts).expect("compress_image failed"));
    let is_webp = prod_out.len() >= 12 && &prod_out[0..4] == b"RIFF" && &prod_out[8..12] == b"WEBP";
    if is_webp {
        println!(
            "   [1] production GIF->WebP     {:>10}  {:>7} of src   {:>8}  PSNR {}  hash {:08x}",
            fmt_bytes(prod_out.len()),
            pct(prod_out.len(), src_len),
            fmt_ms(prod_ms),
            psnr_webp_vs_source(&prod_out, &src),
            fnv1a(&prod_out)
        );
    } else {
        println!(
            "   [1] production GIF->WebP     fell back to original bytes ({}), {}",
            fmt_bytes(prod_out.len()),
            fmt_ms(prod_ms)
        );
    }

    // ---- [2] zengif decode + libwebp animated WebP -----------------------
    let frames_ms: Vec<(Vec<u8>, i32)> = src
        .frames
        .iter()
        .map(|(d, p)| (p.clone(), *d as i32 * 10))
        .collect();
    let (swap_out, swap_ms) = bench(3, || {
        libwebp_encode_animated(&frames_ms, src.w as u32, src.h as u32, quality as f32)
            .expect("libwebp encode failed")
    });
    println!(
        "   [2] zengif dec + libwebp     {:>10}  {:>7} of src   {:>8}  PSNR {}  hash {:08x}",
        fmt_bytes(swap_out.len()),
        pct(swap_out.len(), src_len),
        fmt_ms(swap_ms),
        psnr_webp_vs_source(&swap_out, &src),
        fnv1a(&swap_out)
    );

    // ---- [3] zengif GIF -> GIF -------------------------------------------
    let out_dir = std::path::Path::new("target/gif_zengif_bench");
    std::fs::create_dir_all(out_dir).ok();

    for (cfg_label, cfg) in gif_configs(quality) {
        let (gif_out, ms) = bench(3, || zengif_reencode(&src, &cfg).expect("zengif encode failed"));
        let (dec2, _) = bench(1, || zengif_decode(&gif_out));
        let (psnr, alpha_diff) = psnr_frames(&src.frames, &dec2.frames);
        let delays2: Vec<u16> = dec2.frames.iter().map(|f| f.0).collect();
        let file = out_dir.join(format!(
            "{}_{}_q{}.gif",
            label.trim_end_matches(".gif"),
            cfg_label.replace(['(', ')', '+'], "_"),
            quality
        ));
        std::fs::write(&file, &gif_out).ok();
        println!(
            "   [3] {:22} {:>10}  {:>7} of src   {:>8}  PSNR {:>9}  frames {}/{}  delays_eq {}  alpha_diff {}",
            cfg_label,
            fmt_bytes(gif_out.len()),
            pct(gif_out.len(), src_len),
            fmt_ms(ms),
            psnr,
            dec2.frames.len(),
            src.frames.len(),
            delays2 == delays_cs,
            alpha_diff,
        );
    }
    println!();
}

/// GIF->GIF encode configs, all at dithering 0 (content is already dithered).
fn gif_configs(quality: u8) -> Vec<(&'static str, EncoderConfig)> {
    let base = |q: Quantizer| {
        EncoderConfig::new()
            .quality(quality)
            .quantizer(q)
            .repeat(Repeat::Infinite)
            .dithering(0.0)
    };
    vec![
        // Library default: shared palette + transparency delta. NOTE: this
        // combination corrupts frame deltas on 13424514615469820.gif.
        (
            "default(shared+trans)",
            EncoderConfig::new()
                .quality(quality)
                .quantizer(Quantizer::auto())
                .repeat(Repeat::Infinite)
                .dithering(0.0),
        ),
        (
            "zenquant shared",
            base(Quantizer::zenquant()).use_transparency(false),
        ),
        (
            "quantette shared",
            base(Quantizer::quantette()).use_transparency(false),
        ),
        // quantette is the only knob it actually honors (it ignores `quality`);
        // 0.5 is zengif's default dithering for fresh content.
        (
            "quantette dither.5",
            base(Quantizer::quantette())
                .dithering(0.5)
                .use_transparency(false),
        ),
        // GPL-3 backend — trial only, cannot ship without a license. Its
        // quality knob works (0-100), so it shows a real quality ladder.
        (
            "imagequant shared",
            base(Quantizer::imagequant()).use_transparency(false),
        ),
        (
            "quantizr shared",
            base(Quantizer::quantizr()).use_transparency(false),
        ),
    ]
}

fn zengif_decode(data: &[u8]) -> Decoded {
    let limits = Limits::default();
    let mut dec = Decoder::new(Cursor::new(data), limits, &Unstoppable).expect("zengif decode");
    let w = dec.metadata().width;
    let h = dec.metadata().height;
    let mut frames = Vec::new();
    while let Some(f) = dec.next_frame().expect("zengif frame") {
        frames.push((f.delay, f.as_bytes().to_vec()));
    }
    Decoded { w, h, frames }
}

fn zengif_reencode(src: &Decoded, config: &EncoderConfig) -> Result<Vec<u8>, String> {
    let limits = Limits::default();
    let mut enc = EncodeRequest::new(config, src.w, src.h)
        .limits(&limits)
        .stop(&Unstoppable)
        .build()
        .map_err(|e| e.to_string())?;
    for (delay, pixels) in &src.frames {
        enc.add_frame(FrameInput::from_bytes(src.w, src.h, *delay, pixels))
            .map_err(|e| e.to_string())?;
    }
    enc.finish().map_err(|e| e.to_string())
}

/// libwebp animated WebP encoding, matching src/compress/image/webp_encode.rs
/// (method 4, thread_level 1, exact last-frame delay via the NULL-sentinel add).
fn libwebp_encode_animated(
    frames: &[(Vec<u8>, i32)],
    w: u32,
    h: u32,
    quality: f32,
) -> Result<Vec<u8>, String> {
    let stride = w as i32 * 4;
    unsafe {
        let mut options = std::mem::MaybeUninit::<sys::WebPAnimEncoderOptions>::uninit();
        if sys::WebPAnimEncoderOptionsInitInternal(
            options.as_mut_ptr(),
            sys::WEBP_MUX_ABI_VERSION as i32,
        ) == 0
        {
            return Err("WebPAnimEncoderOptionsInit failed".into());
        }
        let options = options.assume_init();

        let mut config = WebPConfig::new().map_err(|_| "WebPConfig::new failed".to_string())?;
        config.lossless = 0;
        config.quality = quality;
        config.thread_level = 1;
        config.method = 4;

        let enc = sys::WebPAnimEncoderNewInternal(
            w as i32,
            h as i32,
            &options,
            sys::WEBP_MUX_ABI_VERSION as i32,
        );
        if enc.is_null() {
            return Err("WebPAnimEncoderNew failed".into());
        }
        let guard = AnimEncoderGuard(enc);

        let mut ts: i32 = 0;
        for (pixels, delay) in frames {
            let mut pic =
                sys::WebPPicture::new().map_err(|_| "WebPPictureInit failed".to_string())?;
            pic.use_argb = 1;
            pic.width = w as i32;
            pic.height = h as i32;
            if sys::WebPPictureImportRGBA(&mut pic, pixels.as_ptr(), stride) == 0 {
                sys::WebPPictureFree(&mut pic);
                return Err("WebPPictureImportRGBA failed".into());
            }
            let ok = sys::WebPAnimEncoderAdd(enc, &mut pic, ts, &config);
            sys::WebPPictureFree(&mut pic);
            if ok == 0 {
                return Err("WebPAnimEncoderAdd failed".into());
            }
            ts = ts.saturating_add((*delay).max(0));
        }
        if sys::WebPAnimEncoderAdd(enc, std::ptr::null_mut(), ts, std::ptr::null()) == 0 {
            return Err("WebPAnimEncoderAdd(end) failed".into());
        }

        let mut d = std::mem::MaybeUninit::<sys::WebPData>::uninit();
        if sys::WebPAnimEncoderAssemble(enc, d.as_mut_ptr()) == 0 {
            return Err("WebPAnimEncoderAssemble failed".into());
        }
        let mut d = d.assume_init();
        let out = std::slice::from_raw_parts(d.bytes, d.size).to_vec();
        sys::WebPDataClear(&mut d);
        drop(guard);
        Ok(out)
    }
}

struct AnimEncoderGuard(*mut sys::WebPAnimEncoder);
impl Drop for AnimEncoderGuard {
    fn drop(&mut self) {
        unsafe { sys::WebPAnimEncoderDelete(self.0) };
    }
}

/// Decode an animated WebP with libwebp and PSNR its frames against the source.
fn psnr_webp_vs_source(webp_bytes: &[u8], src: &Decoded) -> String {
    let dec = match AnimDecoder::new(webp_bytes).decode() {
        Ok(d) => d,
        Err(e) => return format!("decode failed: {e}"),
    };
    let mut frames = Vec::new();
    for i in 0..dec.len() {
        let Some(f) = dec.get_frame(i) else { continue };
        frames.push((0u16, f.get_image().to_vec()));
    }
    let (psnr, alpha_diff) = psnr_frames(&src.frames, &frames);
    if alpha_diff > 0 {
        format!("{psnr} (alpha_diff {alpha_diff})")
    } else {
        psnr
    }
}

/// PSNR over RGB, plus the count of pixels whose alpha differs.
fn psnr_frames(a: &[(u16, Vec<u8>)], b: &[(u16, Vec<u8>)]) -> (String, usize) {
    if a.len() != b.len() {
        return (format!("frame count {}/{}", a.len(), b.len()), 0);
    }
    let mut sse = 0f64;
    let mut n = 0usize;
    let mut alpha_diff = 0usize;
    for (fa, fb) in a.iter().zip(b.iter()) {
        if fa.1.len() != fb.1.len() {
            return ("pixel size mismatch".into(), 0);
        }
        for (pa, pb) in fa.1.chunks_exact(4).zip(fb.1.chunks_exact(4)) {
            for c in 0..3 {
                let d = pa[c] as f64 - pb[c] as f64;
                sse += d * d;
            }
            if pa[3] != pb[3] {
                alpha_diff += 1;
            }
            n += 3;
        }
    }
    if n == 0 {
        return ("-".into(), alpha_diff);
    }
    let mse = sse / n as f64;
    if mse == 0.0 {
        ("inf".into(), alpha_diff)
    } else {
        (
            format!("{:.2} dB", 10.0 * (255.0f64 * 255.0 / mse).log10()),
            alpha_diff,
        )
    }
}

/// Median-of-`iters` timing (warmup excluded); returns the last output.
fn bench<T, F: FnMut() -> T>(iters: usize, mut f: F) -> (T, f64) {
    let _ = f(); // warmup
    let mut times = Vec::with_capacity(iters);
    let mut out = None;
    for _ in 0..iters {
        let t = Instant::now();
        out = Some(f());
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (out.unwrap(), times[times.len() / 2])
}

fn pct(part: usize, whole: usize) -> String {
    format!("{:.1}%", part as f64 / whole as f64 * 100.0)
}

/// FNV-1a — byte-identity check between the production and swapped pipelines.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn fmt_bytes(n: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let n = n as f64;
    if n >= MIB {
        format!("{:.2} MiB", n / MIB)
    } else {
        format!("{:.1} KiB", n / KIB)
    }
}

fn fmt_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        format!("{ms:.1} ms")
    }
}
