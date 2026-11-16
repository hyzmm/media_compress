//! Rational frame-rate helpers shared by the platform backends.
//!
//! Container frame rates are rational, not integers: 23.976 fps is
//! 24000/1001, 29.97 fps is 30000/1001, and so on. Rounding those values to
//! 24/30 changes the declared cadence and the fallback sample duration, which
//! makes video timing drift away from audio. Keep the exact ratio wherever the
//! platform accepts one; use [`ceil_fps`] only for integer-only hints and
//! bitrate estimates.

/// Media Foundation / CoreMedia use 100 ns ticks.
const HNS_PER_SECOND: i64 = 10_000_000;

/// Round a rational frame rate up to the nearest whole frame per second.
///
/// This is only for APIs that cannot represent a rational rate (for example
/// codec rate-control hints). It must not be used to derive timestamps.
pub(crate) fn ceil_fps(fps_num: u32, fps_den: u32) -> u32 {
    if fps_num == 0 || fps_den == 0 {
        return 30;
    }
    fps_num.div_ceil(fps_den).max(1)
}

/// Duration of one frame in 100 ns units, rounded to the nearest tick.
///
/// Used when a decoded sample does not carry a duration. At 30000/1001 this
/// yields 333667 ticks (33.3667 ms), not the 333333 ticks of 30/1.
pub(crate) fn frame_duration_100ns(fps_num: u32, fps_den: u32) -> i64 {
    if fps_num == 0 || fps_den == 0 {
        return frame_duration_100ns(30, 1);
    }
    let num = fps_num as u64;
    let den = fps_den as u64;
    let ticks = (HNS_PER_SECOND as u64 * den + num / 2) / num;
    ticks.min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::{ceil_fps, frame_duration_100ns};

    #[test]
    fn ceil_fps_keeps_ntsc_rates_in_range() {
        assert_eq!(ceil_fps(24_000, 1001), 24);
        assert_eq!(ceil_fps(30_000, 1001), 30);
        assert_eq!(ceil_fps(60_000, 1001), 60);
        assert_eq!(ceil_fps(120_000, 1001), 120);
    }

    #[test]
    fn integer_rates_stay_unchanged() {
        assert_eq!(ceil_fps(25, 1), 25);
        assert_eq!(ceil_fps(50, 1), 50);
        assert_eq!(ceil_fps(1, 2), 1);
    }

    #[test]
    fn sample_duration_uses_the_exact_ratio() {
        assert_eq!(frame_duration_100ns(30, 1), 333_333);
        // 30000/1001 must not collapse to the 30/1 duration.
        assert_eq!(frame_duration_100ns(30_000, 1001), 333_667);
        assert_eq!(frame_duration_100ns(24_000, 1001), 417_083);
        assert_eq!(frame_duration_100ns(60_000, 1001), 166_833);
    }

    #[test]
    fn invalid_rates_do_not_panic_or_divide_by_zero() {
        assert_eq!(ceil_fps(0, 1001), 30);
        assert_eq!(ceil_fps(30_000, 0), 30);
        assert_eq!(frame_duration_100ns(0, 1001), 333_333);
        assert_eq!(frame_duration_100ns(30_000, 0), 333_333);
    }
}
