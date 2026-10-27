use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use media_compress::{compress_video, compress_video_file, VideoCompressOptions};

fn is_mp4(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[4..8] == b"ftyp"
}

fn base_dir() -> PathBuf {
    let compile_time = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if compile_time.exists() {
        compile_time
    } else {
        std::env::current_dir().expect("cannot determine current directory")
    }
}

fn input_dir() -> PathBuf {
    let base = base_dir();
    let direct = base.join("test_videos");
    if direct.exists() {
        direct
    } else {
        base.join("test_data").join("test_videos")
    }
}

/// Integration test: compress every supported video under `test_videos/` to
/// H.264 MP4 and write results to `out_videos/`.
///
/// Unrecognised formats are skipped. The test fails only if a *recognised*
/// video container fails to compress.
#[test]
fn compress_all_test_videos() {
    let base_dir = base_dir();
    let input_dir = input_dir();
    let output_dir = base_dir.join("out_videos");

    if !input_dir.exists() {
        eprintln!("test_videos/ directory does not exist — skipping test");
        return;
    }

    fs::create_dir_all(&output_dir).expect("failed to create out_videos/");

    let entries: Vec<_> = fs::read_dir(&input_dir)
        .expect("failed to read test_videos/")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();

    assert!(
        !entries.is_empty(),
        "test_videos/ is empty — add some videos to test"
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

        let mut options = VideoCompressOptions::new(800);
        options.max_width = Some(1280);
        options.max_height = Some(720);

        let t0 = Instant::now();
        match compress_video(&data, options) {
            Ok(output_bytes) => {
                let elapsed = t0.elapsed();
                assert!(
                    !output_bytes.is_empty(),
                    "compress_video returned empty bytes for {}",
                    file_name
                );
                assert!(
                    is_mp4(&output_bytes),
                    "expected MP4 output for {}",
                    file_name
                );
                assert!(
                    output_bytes.len() <= original_size,
                    "output should not be larger than input for {} (got {} vs {})",
                    file_name,
                    output_bytes.len(),
                    original_size
                );

                let stem = path.file_stem().unwrap().to_string_lossy();
                let out_path = output_dir.join(format!("{}.mp4", stem));
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
            Err(media_compress::Error::UnsupportedFormat(msg)) => {
                eprintln!("  SKIP  {} — unsupported format: {}", file_name, msg);
                skipped += 1;
            }
            Err(e) => {
                panic!("compress_video failed for {}: {}", file_name, e);
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

/// Compress a source video through the file-path API and verify the output is
/// a valid, smaller MP4. This is the API used by hosts that must not load the
/// whole video into memory.
#[test]
fn compress_video_file_downscales() {
    let input_path = input_dir().join("sample.mp4");
    if !input_path.exists() {
        eprintln!("sample.mp4 does not exist — skipping test");
        return;
    }

    let output_dir = base_dir().join("out_videos");
    fs::create_dir_all(&output_dir).expect("failed to create out_videos/");
    let output_path = output_dir.join("sample_file_api.mp4");
    let _ = fs::remove_file(&output_path);

    let mut options = VideoCompressOptions::new(800);
    options.max_width = Some(640);
    options.max_height = Some(360);

    let compressed = compress_video_file(&input_path, &output_path, options)
        .expect("file api compression failed");
    assert!(
        compressed,
        "expected the file api to produce a smaller file"
    );

    let output = fs::read(&output_path).expect("failed to read file api output");
    assert!(is_mp4(&output), "expected MP4 output");
    assert!(!output.is_empty(), "output is empty");

    let input_len = fs::metadata(&input_path)
        .expect("failed to stat input")
        .len() as usize;
    assert!(
        output.len() < input_len,
        "file api output should be smaller than input ({} vs {})",
        output.len(),
        input_len
    );

    eprintln!(
        "file api downscaled: {} -> {} bytes ({:.1}%)",
        input_len,
        output.len(),
        output.len() as f64 / input_len as f64 * 100.0
    );
}

/// Downscale a source video to a smaller resolution cap and verify the output
/// is a valid, smaller MP4.
#[test]
fn compress_video_downscales() {
    let input_path = input_dir().join("sample.mp4");
    if !input_path.exists() {
        eprintln!("sample.mp4 does not exist — skipping test");
        return;
    }

    let input = fs::read(&input_path).expect("failed to read sample.mp4");

    let mut options = VideoCompressOptions::new(800);
    options.max_width = Some(640);
    options.max_height = Some(360);

    let out = compress_video(&input, options).expect("downscale failed");
    assert!(is_mp4(&out), "expected MP4 output");
    assert!(!out.is_empty(), "output is empty");
    assert!(out.len() < input.len(), "downscale should shrink the file");

    let output_dir = base_dir().join("out_videos");
    fs::create_dir_all(&output_dir).expect("failed to create out_videos/");
    fs::write(output_dir.join("sample_downscaled.mp4"), &out)
        .expect("failed to write downscaled output");

    eprintln!(
        "downscaled: {} -> {} bytes ({:.1}% of original)",
        input.len(),
        out.len(),
        out.len() as f64 / input.len() as f64 * 100.0
    );
}
