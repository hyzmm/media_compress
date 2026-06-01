const MIN_FRAME_DELAY_MS: i32 = 100;

/// Merge frames so that every output frame has at least `MIN_FRAME_DELAY_MS` delay.
pub fn merge_frames_min_delay(frames: Vec<(Vec<u8>, i32)>) -> Vec<(Vec<u8>, i32)> {
    if frames.is_empty() {
        return frames;
    }

    if frames.len() <= 3 {
        return frames
            .into_iter()
            .map(|(pixels, delay)| (pixels, delay.max(MIN_FRAME_DELAY_MS)))
            .collect();
    }

    let mut result: Vec<(Vec<u8>, i32)> = Vec::new();
    let mut accumulated: i32 = 0;
    let mut pending: Option<Vec<u8>> = None;

    for (pixels, delay) in frames {
        accumulated += delay.max(0);
        pending = Some(pixels);

        if accumulated >= MIN_FRAME_DELAY_MS {
            result.push((pending.take().expect("pending frame"), accumulated));
            accumulated = 0;
        }
    }

    if let Some(pixels) = pending {
        if let Some(last) = result.last_mut() {
            last.1 += accumulated;
        } else {
            result.push((pixels, accumulated.max(MIN_FRAME_DELAY_MS)));
        }
    }

    result
}
