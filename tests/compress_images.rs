use std::fs;
use std::path::Path;
use std::time::Instant;

use media_compress::{compress_image, ImageFormat};

/// Integration test: compress every file under `test_images/` to WebP and
/// write results to `out_images/`.
///
/// Unsupported or unrecognised formats are silently skipped.
/// The test only fails if a *recognised* format fails to compress.
#[test]
fn compress_all_test_images() {
    let input_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_images");
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("out_images");

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

        let data = fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {}", file_name, e));

        // Detect format from magic bytes; skip if unknown.
        let format = match ImageFormat::detect(&data) {
            Some(f) => f,
            None => {
                eprintln!("  SKIP  {} — unrecognised format", file_name);
                skipped += 1;
                continue;
            }
        };

        let original_size = data.len();
        eprintln!(
            "  COMPRESS  {}  (format: {:?}, size: {} bytes)",
            file_name, format, original_size
        );

        let t0 = Instant::now();
        match compress_image(&data, Some(format), 75.0) {
            Ok(webp_bytes) => {
                let elapsed = t0.elapsed();
                assert!(
                    !webp_bytes.is_empty(),
                    "compress_image returned empty bytes for {}",
                    file_name
                );

                // Write output: append the original extension before .webp so
                // files with the same stem (e.g. test_image.jpg / test_image.png)
                // don't overwrite each other in out_images/.
                let stem = path.file_stem().unwrap().to_string_lossy();
                let orig_ext = path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let out_name = format!("{}{}.webp", stem, orig_ext);
                let out_path = output_dir.join(&out_name);
                fs::write(&out_path, &webp_bytes)
                    .unwrap_or_else(|e| panic!("cannot write {}: {}", out_path.display(), e));

                let ratio = webp_bytes.len() as f64 / original_size as f64 * 100.0;
                eprintln!(
                    "    -> {} bytes ({:.1}% of original)  time: {:.2?}  saved to {}",
                    webp_bytes.len(),
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
