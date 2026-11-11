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

        let options = VideoCompressOptions::new(28.0);

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
/// a valid MP4. This is the API used by hosts that must not load the whole
/// video into memory.
#[test]
fn compress_video_file_crf() {
    let input_path = input_dir().join("short_landscape_1080p.mp4");
    if !input_path.exists() {
        eprintln!("short_landscape_1080p.mp4 does not exist — skipping test");
        return;
    }

    let output_dir = base_dir().join("out_videos");
    fs::create_dir_all(&output_dir).expect("failed to create out_videos/");
    let output_path = output_dir.join("short_landscape_1080p_file_api.mp4");
    let _ = fs::remove_file(&output_path);

    let options = VideoCompressOptions::new(30.0);

    compress_video_file(&input_path, &output_path, options).expect("file api compression failed");

    let output = fs::read(&output_path).expect("failed to read file api output");
    assert!(is_mp4(&output), "expected MP4 output");
    assert!(!output.is_empty(), "output is empty");

    let input_len = fs::metadata(&input_path)
        .expect("failed to stat input")
        .len() as usize;

    eprintln!(
        "file api crf: {} -> {} bytes ({:.1}%)",
        input_len,
        output.len(),
        output.len() as f64 / input_len as f64 * 100.0
    );
}

/// A re-encode that ends up larger than the source is still kept: the API
/// targets H.264 output rather than a guaranteed size reduction.
#[test]
fn keeps_reencode_larger_than_source() {
    let input_path = input_dir().join("convert_hevc.mp4");
    if !input_path.exists() {
        eprintln!("convert_hevc.mp4 does not exist — skipping test");
        return;
    }

    let input = fs::read(&input_path).expect("failed to read convert_hevc.mp4");
    let options = VideoCompressOptions::new(0.0);

    let output = compress_video(&input, options).expect("crf 0 compression failed");
    assert!(is_mp4(&output), "expected MP4 output");
    assert!(
        output.len() > input.len(),
        "CRF 0 on an already-compressed source should be larger ({} vs {})",
        output.len(),
        input.len()
    );

    let output_dir = base_dir().join("out_videos");
    fs::create_dir_all(&output_dir).expect("failed to create out_videos/");
    let output_path = output_dir.join("convert_hevc_crf0_file_api.mp4");
    let _ = fs::remove_file(&output_path);

    compress_video_file(&input_path, &output_path, options).expect("file api compression failed");

    let file_output = fs::read(&output_path).expect("failed to read file api output");
    assert!(is_mp4(&file_output), "expected MP4 output");
    assert!(
        file_output.len() > input.len(),
        "file api must keep a re-encode larger than the source ({} vs {})",
        file_output.len(),
        input.len()
    );
}

/// Compress a source video by CRF and verify the output is a valid MP4.
#[test]
fn compress_video_crf() {
    let input_path = input_dir().join("sample.mp4");
    if !input_path.exists() {
        eprintln!("sample.mp4 does not exist — skipping test");
        return;
    }

    let input = fs::read(&input_path).expect("failed to read sample.mp4");

    let out =
        compress_video(&input, VideoCompressOptions::new(30.0)).expect("crf compression failed");
    assert!(is_mp4(&out), "expected MP4 output");
    assert!(!out.is_empty(), "output is empty");

    let output_dir = base_dir().join("out_videos");
    fs::create_dir_all(&output_dir).expect("failed to create out_videos/");
    fs::write(output_dir.join("sample_crf.mp4"), &out).expect("failed to write compressed output");

    eprintln!(
        "crf compressed: {} -> {} bytes ({:.1}% of original)",
        input.len(),
        out.len(),
        out.len() as f64 / input.len() as f64 * 100.0
    );
}

/// A lower CRF must keep more data than a higher one.
#[test]
fn lower_crf_produces_larger_output() {
    let input_path = input_dir().join("short_landscape_1080p.mp4");
    if !input_path.exists() {
        eprintln!("short_landscape_1080p.mp4 does not exist — skipping test");
        return;
    }

    let input = fs::read(&input_path).expect("failed to read short_landscape_1080p.mp4");
    let high_quality =
        compress_video(&input, VideoCompressOptions::new(24.0)).expect("crf 24 compression failed");
    let low_quality =
        compress_video(&input, VideoCompressOptions::new(35.0)).expect("crf 35 compression failed");

    assert!(is_mp4(&high_quality), "expected MP4 output for CRF 24");
    assert!(is_mp4(&low_quality), "expected MP4 output for CRF 35");
    assert!(
        high_quality.len() > low_quality.len(),
        "CRF 24 should keep more data than CRF 35 ({} vs {})",
        high_quality.len(),
        low_quality.len()
    );

    eprintln!(
        "crf sizes: 24 -> {} bytes, 35 -> {} bytes",
        high_quality.len(),
        low_quality.len()
    );
}
