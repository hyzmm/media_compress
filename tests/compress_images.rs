use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use media_compress::{compress_image, CompressOptions};

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\xff\xd8\xff")
}

fn is_gif(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

fn gif_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 10 || !is_gif(bytes) {
        return None;
    }
    let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
    let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
    Some((w, h))
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !is_jpeg(bytes) || bytes.len() < 4 {
        return None;
    }

    let mut i = 2usize;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }

        let marker = bytes[i + 1];
        i += 2;

        if marker == 0xD8 || marker == 0xD9 {
            continue;
        }
        if i + 2 > bytes.len() {
            return None;
        }

        let seg_len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
        if seg_len < 2 || i + seg_len > bytes.len() {
            return None;
        }

        let is_sof = matches!(
            marker,
            0xC0 | 0xC1
                | 0xC2
                | 0xC3
                | 0xC5
                | 0xC6
                | 0xC7
                | 0xC9
                | 0xCA
                | 0xCB
                | 0xCD
                | 0xCE
                | 0xCF
        );

        if is_sof && seg_len >= 7 {
            let h = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            return Some((w, h));
        }

        i += seg_len;
    }

    None
}

fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    jpeg_dimensions(bytes).or_else(|| gif_dimensions(bytes))
}

#[test]
fn compress_with_min_1080_dimensions() {
    let input = include_bytes!("../test_images/test_image.bmp");

    let original =
        compress_image(input, CompressOptions::new(75.0)).expect("compress_image baseline failed");
    assert!(is_jpeg(&original), "expected JPEG output for BMP input");
    let (ow, oh) = image_dimensions(&original).expect("failed to parse baseline image dimensions");
    assert!(
        ow > 1080 && oh > 1080,
        "fixture should be larger than 1080x1080, got {ow}x{oh}"
    );

    let mut options = CompressOptions::new(75.0);
    options.min_width = Some(1080);
    options.min_height = Some(1080);

    let constrained =
        compress_image(input, options).expect("compress_image with min 1080x1080 failed");
    assert!(is_jpeg(&constrained), "expected JPEG output for BMP input");
    let (cw, ch) =
        image_dimensions(&constrained).expect("failed to parse constrained image dimensions");

    assert!(
        cw >= 1080 && ch >= 1080,
        "expected output >=1080x1080, got {cw}x{ch}"
    );
    assert!(
        cw <= ow && ch <= oh,
        "expected output no larger than baseline {ow}x{oh}, got {cw}x{ch}"
    );
    assert!(
        cw < ow || ch < oh,
        "expected at least one dimension to shrink from baseline {ow}x{oh}, got {cw}x{ch}"
    );
}

#[test]
fn compress_test_image_gif_with_min_1080() {
    let input = include_bytes!("../test_images/test_image.gif");

    let mut options = CompressOptions::new(75.0);
    options.min_width = Some(1080);
    options.min_height = Some(1080);

    let err = compress_image(input, options).expect_err("GIF input should return an error");
    match err {
        media_compress::Error::UnsupportedFormat(msg) => {
            assert!(
                msg.contains("GIF compression is not supported"),
                "unexpected unsupported message: {msg}"
            );
        }
        other => panic!("expected UnsupportedFormat for GIF input, got {other}"),
    }
}

#[test]
fn compress_exif_rotate_90_jpg_to_out_images() {
    let base_dir = {
        let compile_time = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if compile_time.exists() {
            compile_time
        } else {
            std::env::current_dir().expect("cannot determine current directory")
        }
    };

    let input_path = {
        let direct = base_dir.join("test_images").join("portrait_2.jpg");
        if direct.exists() {
            direct
        } else {
            base_dir
                .join("test_data")
                .join("test_images")
                .join("portrait_2.jpg")
        }
    };
    let output_dir = base_dir.join("out_images");

    assert!(
        input_path.exists(),
        "missing test fixture: {}",
        input_path.display()
    );

    fs::create_dir_all(&output_dir).expect("failed to create out_images/");

    let input = fs::read(&input_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", input_path.display(), e));

    let output_bytes = compress_image(&input, CompressOptions::new(75.0))
        .expect("compress_image failed for portrait_2.jpg");
    assert!(
        is_jpeg(&output_bytes),
        "expected JPEG output for JPEG input"
    );
    assert!(!output_bytes.is_empty(), "compressed output is empty");

    let (w, h) = image_dimensions(&output_bytes).expect("failed to parse output dimensions");
    assert!(
        h > w,
        "expected EXIF-rotated output to be portrait, got {w}x{h}"
    );

    let out_path = output_dir.join("portrait_2.jpg.jpeg");
    fs::write(&out_path, &output_bytes)
        .unwrap_or_else(|e| panic!("cannot write {}: {}", out_path.display(), e));
}

/// Integration test: compress every supported file under `test_images/` to JPEG and
/// write results to `out_images/`.
///
/// Unsupported or unrecognised formats are silently skipped.
/// The test only fails if a *recognised* format fails to compress.
#[test]
fn compress_all_test_images() {
    // On host / iOS simulator: CARGO_MANIFEST_DIR (compile-time) points to the
    // project root which is accessible directly.
    // On Android via dinghy: the host path does not exist on the device; fall
    // back to the current working directory (the dinghy bundle root).
    // Dinghy copies test_data entries into <bundle_root>/test_data/<id>/,
    // so we probe both locations.
    let base_dir = {
        let compile_time = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if compile_time.exists() {
            compile_time
        } else {
            std::env::current_dir().expect("cannot determine current directory")
        }
    };

    let input_dir = {
        let direct = base_dir.join("test_images");
        if direct.exists() {
            direct
        } else {
            // dinghy test_data layout
            base_dir.join("test_data").join("test_images")
        }
    };

    let output_dir = base_dir.join("out_images");

    if !input_dir.exists() {
        eprintln!("test_images/ directory does not exist — skipping test");
        return;
    }

    fs::create_dir_all(&output_dir).expect("failed to create out_images/");

    let entries: Vec<_> = fs::read_dir(&input_dir)
        .expect("failed to read test_images/")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();

    assert!(
        !entries.is_empty(),
        "test_images/ is empty — add some images to test"
    );

    let mut compressed = 0usize;
    let mut skipped = 0usize;

    for entry in &entries {
        let path = entry.path();
        let file_name = path.file_name().unwrap().to_string_lossy();
        if file_name == ".DS_Store" {
            continue;
        }

        let data = fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {}", file_name, e));

        let original_size = data.len();
        eprintln!("  COMPRESS  {}  size: {} bytes", file_name, original_size);

        let t0 = Instant::now();
        match compress_image(&data, CompressOptions::new(75.0)) {
            Ok(output_bytes) => {
                let elapsed = t0.elapsed();
                assert!(
                    !output_bytes.is_empty(),
                    "compress_image returned empty bytes for {}",
                    file_name
                );

                // Write output: append the original extension before final extension so
                // files with the same stem (e.g. test_image.jpg / test_image.png)
                // don't overwrite each other in out_images/.
                let stem = path.file_stem().unwrap().to_string_lossy();
                let orig_ext = path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let out_ext = if is_gif(&output_bytes) { "gif" } else { "jpg" };
                let out_name = format!("{}{}.{}", stem, orig_ext, out_ext);
                let out_path = output_dir.join(&out_name);
                fs::write(&out_path, &output_bytes)
                    .unwrap_or_else(|e| panic!("cannot write {}: {}", out_path.display(), e));

                let ratio = output_bytes.len() as f64 / original_size as f64 * 100.0;
                println!(
                    "    -> {} bytes ({:.1}% of original)  time: {:.2?}  saved to {}",
                    output_bytes.len(),
                    ratio,
                    elapsed,
                    out_path.file_name().unwrap().to_string_lossy()
                );
                compressed += 1;
            }
            Err(media_compress::Error::PlatformNotSupported(msg)) => {
                eprintln!("  SKIP  {} — platform not supported: {}", file_name, msg);
                skipped += 1;
            }
            Err(media_compress::Error::UnsupportedFormat(msg))
                if is_gif(&data) && msg.contains("GIF compression is not supported") =>
            {
                eprintln!("  SKIP  {} — GIF compression disabled: {}", file_name, msg);
                skipped += 1;
            }
            Err(e) => {
                panic!("compress_image failed for {}: {}", file_name, e);
            }
        }
    }

    eprintln!(
        "\nDone: {} compressed, {} skipped  (total: {})",
        compressed,
        skipped,
        entries.len()
    );
}
