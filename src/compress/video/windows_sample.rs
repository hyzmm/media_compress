//! Windows Media Foundation sample-read flags and state machine.
//!
//! `IMFSourceReader::ReadSample` can succeed while returning a null sample:
//! stream ticks, stream/media-type changes, and end-of-stream all do this.
//! Treating every null sample as EOS truncates the output and can also discard
//! the final sample when the EOS flag is set together with a sample.
//!
//! This module has no COM/FFI dependencies so the decision logic can be
//! unit-tested on the host.

// Retry-only flags are kept documented and are exercised by the tests; they do
// not need an explicit production branch.
#![allow(dead_code)]

/// `MF_SOURCE_READERF_ERROR` from `mfreadwrite.h`.
pub(crate) const MF_SOURCE_READERF_ERROR: u32 = 0x1;
/// `MF_SOURCE_READERF_ENDOFSTREAM` from `mfreadwrite.h`.
pub(crate) const MF_SOURCE_READERF_ENDOFSTREAM: u32 = 0x2;
/// `MF_SOURCE_READERF_NEWSTREAM` from `mfreadwrite.h`.
pub(crate) const MF_SOURCE_READERF_NEWSTREAM: u32 = 0x4;
/// `MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED` from `mfreadwrite.h`.
pub(crate) const MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED: u32 = 0x10;
/// `MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED` from `mfreadwrite.h`.
pub(crate) const MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED: u32 = 0x20;
/// `MF_SOURCE_READERF_STREAMTICK` from `mfreadwrite.h`.
pub(crate) const MF_SOURCE_READERF_STREAMTICK: u32 = 0x100;
/// `MF_SOURCE_READERF_ALLEFFECTSREMOVED` from `mfreadwrite.h`.
pub(crate) const MF_SOURCE_READERF_ALLEFFECTSREMOVED: u32 = 0x200;

/// What `read_sample` should do with one `ReadSample` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadSampleAction {
    /// A sample is present and must be processed, even if EOS is also flagged.
    UseSample,
    /// No sample and the stream really reached its end.
    EndOfStream,
    /// No sample yet, but this is a stream event rather than EOS. Read again.
    Retry,
    /// The source reader reported an unrecoverable error.
    Fail,
}

/// Classify one successful `ReadSample` call.
///
/// Error takes precedence because Media Foundation says not to call the source
/// reader again after `MF_SOURCE_READERF_ERROR`. A non-null sample wins over
/// EOS so the final sample is not dropped.
pub(crate) fn classify_read_sample(has_sample: bool, flags: u32) -> ReadSampleAction {
    if flags & MF_SOURCE_READERF_ERROR != 0 {
        ReadSampleAction::Fail
    } else if has_sample {
        ReadSampleAction::UseSample
    } else if flags & MF_SOURCE_READERF_ENDOFSTREAM != 0 {
        ReadSampleAction::EndOfStream
    } else {
        ReadSampleAction::Retry
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_read_sample, ReadSampleAction, MF_SOURCE_READERF_ALLEFFECTSREMOVED,
        MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
        MF_SOURCE_READERF_ERROR, MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED,
        MF_SOURCE_READERF_NEWSTREAM, MF_SOURCE_READERF_STREAMTICK,
    };

    #[test]
    fn eos_and_error_values_match_mfreadwrite_header() {
        assert_eq!(MF_SOURCE_READERF_ERROR, 0x1);
        assert_eq!(MF_SOURCE_READERF_ENDOFSTREAM, 0x2);
        assert_eq!(MF_SOURCE_READERF_NEWSTREAM, 0x4);
        assert_eq!(MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED, 0x10);
        assert_eq!(MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, 0x20);
        assert_eq!(MF_SOURCE_READERF_STREAMTICK, 0x100);
        assert_eq!(MF_SOURCE_READERF_ALLEFFECTSREMOVED, 0x200);
    }

    #[test]
    fn null_sample_without_eos_is_retried() {
        for flags in [
            0,
            MF_SOURCE_READERF_NEWSTREAM,
            MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED,
            MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED,
            MF_SOURCE_READERF_STREAMTICK,
            MF_SOURCE_READERF_ALLEFFECTSREMOVED,
        ] {
            assert_eq!(
                classify_read_sample(false, flags),
                ReadSampleAction::Retry,
                "flags 0x{flags:08x} must not be treated as EOS"
            );
        }
    }

    #[test]
    fn null_sample_with_eos_is_end_of_stream() {
        assert_eq!(
            classify_read_sample(false, MF_SOURCE_READERF_ENDOFSTREAM),
            ReadSampleAction::EndOfStream
        );
    }

    #[test]
    fn sample_is_used_even_when_eos_is_also_flagged() {
        assert_eq!(
            classify_read_sample(true, MF_SOURCE_READERF_ENDOFSTREAM),
            ReadSampleAction::UseSample
        );
        assert_eq!(classify_read_sample(true, 0), ReadSampleAction::UseSample);
    }

    #[test]
    fn error_flag_is_not_treated_as_eos() {
        assert_eq!(
            classify_read_sample(false, MF_SOURCE_READERF_ERROR),
            ReadSampleAction::Fail
        );
        assert_eq!(
            classify_read_sample(true, MF_SOURCE_READERF_ERROR),
            ReadSampleAction::Fail
        );
    }
}
