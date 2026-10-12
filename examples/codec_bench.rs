//! libwebp vs zenwebp lossy WebP encode comparison: size / PSNR / encode time.
//!
//! Run: cargo run --release --example codec_bench
//!
//! Each input is decoded to RGBA once, then encoded with:
//!   - libwebp (`webp` crate = libwebp-sys C) at method 4, thread_level 0 and 1
//!     (thread_level 1 matches the production setting in webp_encode.rs)
//!   - zenwebp (pure Rust) at method 4; it has no thread knob (single-threaded)
//! All WebP outputs are decoded with libwebp for PSNR against the source, so
//! both encoders are judged by one common decoder.
//!
//! HEIC inputs are converted with `sips` (macOS) first; GIF is out of scope.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

const QUALITY: f32 = 75.0;
const METHOD: i32 = 4;
const SWEEP_QUALITIES: &[f32] = &[40.0, 60.0, 80.0, 95.0];

const INPUTS: &[(&str, &str)] = &[
    ("portrait_2.jpg", "test_images/portrait_2.jpg"),
    (
        "IMG_20260508_181840.jpg",
        "test_images/IMG_20260508_181840.jpg",
    ),
    ("test_image.heic", "test_images/test_image.heic"),
    ("test_image.webp", "test_images/test_image.webp"),
    ("test_image.png", "test_images/test_image.png"),
    ("test_image.tiff", "test_images/test_image.tiff"),
    ("test_image.bmp", "test_images/test_image.bmp"),
];

const SWEEP_SOURCE: &str = "test_images/IMG_20260508_181840.jpg";

fn main() {
    let tmp = Path::new("target/codec_bench_tmp");
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--probe") {
        for (label, path) in INPUTS {
            let Some(prepared) = prepare_input(path, tmp) else {
                println!("{label}: FAIL cannot prepare input");
                continue;
            };
            match load_rgba(&prepared) {
                Ok((w, h, _)) => println!("{label}: OK {w}x{h}"),
                Err(e) => println!("{label}: FAIL {e}"),
            }
        }
        return;
    }

    if let Some(pos) = args.iter().position(|a| a == "--dump-rgba") {
        for label in &args[pos + 1..] {
            dump_rgba(label, tmp);
        }
        return;
    }

    println!("libwebp vs zenwebp — lossy WebP, q={QUALITY}, method={METHOD}");
    println!("libwebp = webp crate (libwebp-sys, C); zenwebp = pure Rust, single-threaded");
    println!("PSNR: RGB, decoded by libwebp, against the decoded source\n");

    let mut size_ratios = Vec::new();
    let mut psnr_deltas = Vec::new();
    let mut time_ratios = Vec::new();

    for (label, path) in INPUTS {
        let Some(prepared) = prepare_input(path, tmp) else {
            println!("skip {label}: cannot prepare input\n");
            continue;
        };
        let src_file_len = std::fs::metadata(path).map(|m| m.len() as usize).ok();
        let (w, h, rgba) = match load_rgba(&prepared) {
            Ok(v) => v,
            // Some exotic TIFFs (e.g. RGBPalette photometric) are not supported
            // by the pure-Rust decoder; retry through sips like HEIC.
            Err(first_err) => match convert_to_png(&prepared, tmp).and_then(|p| load_rgba(&p).ok()) {
                Some(v) => v,
                None => {
                    println!("skip {label}: {first_err}\n");
                    continue;
                }
            },
        };

        let mpx = w as f64 * h as f64 / 1e6;
        let has_alpha = rgba.chunks_exact(4).any(|p| p[3] != 255);
        let iters = if w as u64 * h as u64 > 8_000_000 { 3 } else { 5 };

        println!(
            "{label}  {w}x{h} ({mpx:.1} MP)  src file {}  q={QUALITY}{}",
            src_file_len.map_or("-".to_string(), fmt_bytes),
            if has_alpha { "  [alpha]" } else { "" }
        );

        let (out_lw_st, t_lw_st) = bench(iters, || libwebp_encode(&rgba, w, h, QUALITY, 0));
        let (out_lw_mt, t_lw_mt) = bench(iters, || libwebp_encode(&rgba, w, h, QUALITY, 1));
        let (out_zw, t_zw) = bench(iters, || zenwebp_encode(&rgba, w, h, QUALITY));

        let p_lw_st = psnr_vs_source(&rgba, &out_lw_st, label);
        let p_lw_mt = psnr_vs_source(&rgba, &out_lw_mt, label);
        let p_zw = psnr_vs_source(&rgba, &out_zw, label);

        row("libwebp tl=0", out_lw_st.len(), src_file_len, p_lw_st, t_lw_st);
        row("libwebp tl=1", out_lw_mt.len(), src_file_len, p_lw_mt, t_lw_mt);
        row("zenwebp", out_zw.len(), src_file_len, p_zw, t_zw);
        println!("  (median of {iters} runs, warmup excluded)\n");

        size_ratios.push(out_zw.len() as f64 / out_lw_st.len() as f64);
        psnr_deltas.push(p_zw - p_lw_st);
        time_ratios.push(t_zw / t_lw_st);
    }

    if !size_ratios.is_empty() {
        println!(
            "====== summary at q={QUALITY}, {} images ======",
            size_ratios.len()
        );
        println!(
            "zenwebp vs libwebp(tl=0):  size {:+.1}%   PSNR {:+.2} dB   time {:.2}x",
            (mean(&size_ratios) - 1.0) * 100.0,
            mean(&psnr_deltas),
            mean(&time_ratios)
        );
        println!();
    }

    sweep(tmp);
}

fn sweep(tmp: &Path) {
    let Some(prepared) = prepare_input(SWEEP_SOURCE, tmp) else {
        return;
    };
    let Ok((w, h, rgba)) = load_rgba(&prepared) else {
        return;
    };
    let src_file_len = std::fs::metadata(SWEEP_SOURCE)
        .map(|m| m.len() as usize)
        .ok();

    println!("====== quality sweep: {SWEEP_SOURCE}  {w}x{h} ======");
    println!("  q     libwebp tl=0                        zenwebp                             delta");
    for &q in SWEEP_QUALITIES {
        let (out_lw, t_lw) = bench(3, || libwebp_encode(&rgba, w, h, q, 0));
        let (out_zw, t_zw) = bench(3, || zenwebp_encode(&rgba, w, h, q));
        let p_lw = psnr_vs_source(&rgba, &out_lw, "sweep libwebp");
        let p_zw = psnr_vs_source(&rgba, &out_zw, "sweep zenwebp");
        println!(
            "  {q:>3.0}   {:>9} {:>6}  {:>9}  {:>8}   {:>9} {:>6}  {:>9}  {:>8}   size {:+.1}%  psnr {:+.2} dB",
            fmt_bytes(out_lw.len()),
            src_file_len.map_or("-".into(), |n| pct(out_lw.len(), n)),
            fmt_psnr(p_lw),
            fmt_ms(t_lw),
            fmt_bytes(out_zw.len()),
            src_file_len.map_or("-".into(), |n| pct(out_zw.len(), n)),
            fmt_psnr(p_zw),
            fmt_ms(t_zw),
            (out_zw.len() as f64 / out_lw.len() as f64 - 1.0) * 100.0,
            p_zw - p_lw,
        );
    }
}

/// Write the decoded RGBA of an input (by label) to target/codec_bench_tmp/<label>.rgba
/// for external harnesses (e.g. the wasm speed bench run under Node).
fn dump_rgba(label: &str, tmp: &Path) {
    let Some((_, path)) = INPUTS.iter().find(|(l, _)| *l == label) else {
        println!("{label}: unknown image label");
        return;
    };
    let Some(prepared) = prepare_input(path, tmp) else {
        println!("{label}: cannot prepare input");
        return;
    };
    let (w, h, rgba) = match load_rgba(&prepared) {
        Ok(v) => v,
        Err(e) => match convert_to_png(&prepared, tmp).and_then(|p| load_rgba(&p).ok()) {
            Some(v) => v,
            None => {
                println!("{label}: {e}");
                return;
            }
        },
    };
    let out = tmp.join(format!("{label}.rgba"));
    std::fs::write(&out, &rgba).expect("write rgba");
    println!("{label}: {w}x{h} ({} bytes) -> {}", rgba.len(), out.display());
}

fn row(name: &str, out_len: usize, src_file_len: Option<usize>, psnr: f64, ms: f64) {
    println!(
        "  {name:<14} {:>9}  {:>7} of file  {:>10}  {:>10}",
        fmt_bytes(out_len),
        src_file_len.map_or("-".into(), |n| pct(out_len, n)),
        fmt_psnr(psnr),
        fmt_ms(ms),
    );
}

fn bench<F: FnMut() -> Vec<u8>>(iters: usize, mut f: F) -> (Vec<u8>, f64) {
    let _ = f(); // warmup
    let mut times = Vec::with_capacity(iters);
    let mut out = Vec::new();
    for _ in 0..iters {
        let t = Instant::now();
        out = f();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (out, times[times.len() / 2])
}

fn libwebp_encode(pixels: &[u8], w: u32, h: u32, quality: f32, thread_level: i32) -> Vec<u8> {
    let mut config = webp::WebPConfig::new().expect("WebPConfig::new");
    config.lossless = 0;
    config.quality = quality;
    config.method = METHOD;
    config.thread_level = thread_level;
    webp::Encoder::from_rgba(pixels, w, h)
        .encode_advanced(&config)
        .expect("libwebp encode failed")
        .to_vec()
}

fn zenwebp_encode(pixels: &[u8], w: u32, h: u32, quality: f32) -> Vec<u8> {
    let config = zenwebp::LossyConfig::new()
        .with_quality(quality)
        .with_method(METHOD as u8);
    zenwebp::EncodeRequest::lossy(&config, pixels, zenwebp::PixelLayout::Rgba8, w, h)
        .encode()
        .expect("zenwebp encode failed")
}

/// Decode a WebP output with libwebp and compute RGB PSNR against the source RGBA.
fn psnr_vs_source(reference_rgba: &[u8], webp_bytes: &[u8], label: &str) -> f64 {
    assert!(
        webp_bytes.len() >= 12 && &webp_bytes[0..4] == b"RIFF" && &webp_bytes[8..12] == b"WEBP",
        "{label}: output is not a WebP bitstream"
    );
    let img = webp::Decoder::new(webp_bytes)
        .decode()
        .unwrap_or_else(|| panic!("{label}: libwebp could not decode the output"));
    let dec: &[u8] = &img;
    let n = img.width() as usize * img.height() as usize;
    assert_eq!(n * 4, reference_rgba.len(), "{label}: decoded size mismatch");
    let step = if img.is_alpha() { 4 } else { 3 };

    let mut sse = 0f64;
    for i in 0..n {
        let r = &reference_rgba[i * 4..i * 4 + 3];
        let d = &dec[i * step..i * step + 3];
        for c in 0..3 {
            let diff = r[c] as f64 - d[c] as f64;
            sse += diff * diff;
        }
    }
    let mse = sse / (n * 3) as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

fn load_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let is_webp = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("webp"));
    if is_webp {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let img = webp::Decoder::new(&bytes)
            .decode()
            .ok_or_else(|| "libwebp decode returned None".to_string())?;
        let (w, h) = (img.width(), img.height());
        let src: &[u8] = &img;
        let rgba = if img.is_alpha() {
            src.to_vec()
        } else {
            rgb_to_rgba(src)
        };
        Ok((w, h, rgba))
    } else {
        let img = image::open(path).map_err(|e| format!("image::open: {e}"))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        Ok((w, h, rgba.into_raw()))
    }
}

fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for p in rgb.chunks_exact(3) {
        out.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    out
}

/// HEIC needs ImageIO — convert to PNG via `sips` (macOS) into the target dir.
fn prepare_input(path: &str, tmp: &Path) -> Option<PathBuf> {
    let p = Path::new(path);
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("heic")) {
        convert_to_png(p, tmp)
    } else {
        Some(p.to_path_buf())
    }
}

fn convert_to_png(p: &Path, tmp: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(tmp).ok()?;
    let out = tmp.join(format!(
        "{}.png",
        p.file_stem().unwrap_or_default().to_string_lossy()
    ));
    if !out.exists() {
        let status = Command::new("sips")
            .args(["-s", "format", "png"])
            .arg(p)
            .arg("--out")
            .arg(&out)
            .stdout(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
    }
    Some(out)
}

fn pct(part: usize, whole: usize) -> String {
    format!("{:.1}%", part as f64 / whole as f64 * 100.0)
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

fn fmt_psnr(p: f64) -> String {
    if p.is_finite() {
        format!("{p:.2} dB")
    } else {
        "lossless".to_string()
    }
}

fn fmt_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        format!("{ms:.1} ms")
    }
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}
