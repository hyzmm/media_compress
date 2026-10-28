// Native-only integration tests: H.264/HEVC detection and transcoding go
// through the platform media stack, which does not exist on wasm32.
#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use media_compress::{
    convert_video_file, VideoCodec, VideoConvertOptions, VideoConvertOutcome,
};

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

/// Convert every video under `test_videos/` and write results to
/// `out_videos/converted/`.
///
/// H.264/HEVC sources must be reported as already compliant without creating
/// an output; every other recognised source must produce an MP4. Fixtures are
/// not committed, so the test only asserts that at least one file was handled.
#[test]
fn convert_all_test_videos() {
    let input_dir = input_dir();
    if !input_dir.exists() {
        eprintln!("test_videos/ directory does not exist — skipping test");
        return;
    }

    let output_dir = base_dir().join("out_videos").join("converted");
    fs::create_dir_all(&output_dir).expect("failed to create out_videos/converted/");

    let entries: Vec<_> = fs::read_dir(&input_dir)
        .expect("failed to read test_videos/")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .collect();

    let mut converted = 0usize;
    let mut compliant = 0usize;

    for entry in &entries {
        let path = entry.path();
        let file_name = path.file_name().unwrap().to_string_lossy();
        if file_name == ".DS_Store" {
            continue;
        }

        let stem = path.file_stem().unwrap().to_string_lossy();
        let out_path = output_dir.join(format!("{stem}.mp4"));
        let _ = fs::remove_file(&out_path);

        let started = Instant::now();
        match convert_video_file(&path, &out_path, VideoConvertOptions::new()) {
            Ok(VideoConvertOutcome::AlreadyCompliant(codec)) => {
                assert!(
                    !out_path.exists(),
                    "compliant input {file_name} must not create an output"
                );
                eprintln!("  SKIP     {file_name} — already {codec:?}");
                compliant += 1;
            }
            Ok(VideoConvertOutcome::Converted) => {
                let bytes = fs::read(&out_path)
                    .unwrap_or_else(|e| panic!("cannot read {}: {e}", out_path.display()));
                assert!(!bytes.is_empty(), "convert returned an empty {file_name}");
                assert!(
                    is_mp4(&bytes),
                    "expected MP4 output for {file_name}, got {} bytes",
                    bytes.len()
                );
                eprintln!(
                    "  CONVERT  {file_name} -> {} ({} bytes)  time: {:.2?}",
                    out_path.display(),
                    bytes.len(),
                    started.elapsed()
                );
                converted += 1;
            }
            Err(media_compress::Error::UnsupportedFormat(msg)) => {
                eprintln!("  SKIP     {file_name} — unsupported format: {msg}");
            }
            Err(media_compress::Error::PlatformNotSupported(msg)) => {
                eprintln!("  SKIP     {file_name} — platform not supported: {msg}");
            }
            // Some containers/codecs (e.g. VP9 WebM on AVFoundation) are not
            // decodable by every platform; that is a source limitation, not a
            // conversion regression.
            Err(media_compress::Error::DecodeError(msg)) => {
                eprintln!("  SKIP     {file_name} — platform cannot decode: {msg}");
            }
            Err(e) => panic!("convert_video_file failed for {file_name}: {e}"),
        }
    }

    assert!(converted + compliant > 0, "no video fixtures were handled");
    eprintln!("converted: {converted}, already compliant: {compliant}");
}

/// A compliant codec still needs a re-encode when the frame exceeds the size
/// limit; a compliant input already within the limit is skipped.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "android",
    target_os = "windows"
))]
#[test]
fn max_size_forces_conversion_of_large_compliant_input() {
    let input_dir = input_dir();
    let large = input_dir.join("landscape_1080p.mp4");
    let small = input_dir.join("small_360p.mp4");
    if !large.exists() || !small.exists() {
        eprintln!("video fixtures not present — skipping test");
        return;
    }

    let dir = temp_dir("max_size");
    let limits = VideoConvertOptions::with_max_size(1280, 720);

    let large_out = dir.join("large.mp4");
    assert_eq!(
        convert_video_file(&large, &large_out, limits).unwrap(),
        VideoConvertOutcome::Converted
    );
    assert!(
        is_mp4(&fs::read(&large_out).unwrap()),
        "downscaled output must be MP4"
    );

    let small_out = dir.join("small.mp4");
    assert_eq!(
        convert_video_file(&small, &small_out, limits).unwrap(),
        VideoConvertOutcome::AlreadyCompliant(VideoCodec::H264)
    );
    assert!(!small_out.exists(), "in-limit input must not be rewritten");

    // 最长边 1920 的方框同时容纳横屏 1080p 和竖屏 1080x1920
    let wide_limits = VideoConvertOptions::with_max_size(1920, 1920);
    let wide_out = dir.join("wide.mp4");
    assert_eq!(
        convert_video_file(&large, &wide_out, wide_limits).unwrap(),
        VideoConvertOutcome::AlreadyCompliant(VideoCodec::H264)
    );
    assert!(!wide_out.exists());

    let portrait = input_dir.join("portrait_physical.mp4");
    if portrait.exists() {
        let portrait_out = dir.join("portrait.mp4");
        assert_eq!(
            convert_video_file(&portrait, &portrait_out, wide_limits).unwrap(),
            VideoConvertOutcome::AlreadyCompliant(VideoCodec::H264),
            "portrait 1080x1920 must fit the 1920 square"
        );
        assert!(!portrait_out.exists());
    }

    let _ = fs::remove_dir_all(&dir);
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "media_compress_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "android",
    target_os = "windows"
))]
#[test]
fn rejects_unrecognized_container() {
    let dir = std::env::temp_dir().join(format!(
        "media_compress_convert_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&dir).expect("failed to create temp dir");

    let input = dir.join("input.bin");
    let output = dir.join("output.mp4");
    fs::write(&input, b"this is not a video container").expect("failed to write temp input");

    match convert_video_file(&input, &output, VideoConvertOptions::new()) {
        Err(media_compress::Error::UnsupportedFormat(_)) => {}
        other => panic!("expected UnsupportedFormat, got {other:?}"),
    }
    assert!(
        !output.exists(),
        "failed conversion must not leave an output"
    );

    let _ = fs::remove_dir_all(&dir);
}
