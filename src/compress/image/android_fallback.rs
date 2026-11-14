//! Android image decoder fallback policy.
//!
//! Kept free of JNI/FFI so the decisions can be unit-tested on the host.
//! `AImageDecoder` is the primary decoder on API 30+, but it can reject images
//! that the JNI `BitmapFactory` path still handles (vendor codec gaps, unusual
//! color formats, malformed-but-decodable files).

use crate::error::Error;

/// `ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT` from `<android/imagedecoder.h>`.
///
/// The value is -9. -6 is `ANDROID_IMAGE_DECODER_INVALID_INPUT`, so using it
/// here classified unsupported formats as decode errors and skipped the JNI
/// fallback.
pub(crate) const ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT: i32 = -9;

/// Whether a failure from `AImageDecoder` is worth retrying with the JNI
/// `BitmapFactory` decoder.
///
/// Encoding happens only after a successful decode, so an `EncodeError` will
/// not improve on retry. Other errors (`NativeError`, `UnsupportedFormat`)
/// indicate a caller/runtime problem rather than a decoder limitation.
pub(crate) fn should_try_jni_fallback(error: &Error) -> bool {
    matches!(
        error,
        Error::PlatformNotSupported(_) | Error::DecodeError(_)
    )
}

/// Resolve the result of the JNI fallback while keeping the primary error
/// useful when both decoders fail.
///
/// If `AImageDecoder` reported `PlatformNotSupported`, its message only means
/// "try the other decoder", so the fallback error is the actionable one. For a
/// genuine decode failure, keep the `DecodeError` classification and append
/// the fallback error for diagnostics.
pub(crate) fn resolve_fallback(
    primary: Error,
    fallback: Result<Vec<u8>, Error>,
) -> Result<Vec<u8>, Error> {
    match fallback {
        Ok(output) => Ok(output),
        Err(fallback_error) => match primary {
            Error::PlatformNotSupported(_) => Err(fallback_error),
            Error::DecodeError(message) => Err(Error::DecodeError(format!(
                "{message}; JNI BitmapFactory fallback also failed: {fallback_error}"
            ))),
            other => Err(other),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_fallback, should_try_jni_fallback, ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT,
    };
    use crate::error::Error;

    #[test]
    fn unsupported_format_is_minus_nine_not_invalid_input() {
        assert_eq!(ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT, -9);
        assert_ne!(ANDROID_IMAGE_DECODER_UNSUPPORTED_FORMAT, -6);
    }

    #[test]
    fn decoder_failures_trigger_the_jni_fallback() {
        assert!(should_try_jni_fallback(&Error::PlatformNotSupported(
            "AImageDecoder unavailable".into()
        )));
        assert!(should_try_jni_fallback(&Error::DecodeError(
            "AImageDecoder_decodeImage failed: -6".into()
        )));
    }

    #[test]
    fn encode_and_runtime_errors_do_not_trigger_the_jni_fallback() {
        assert!(!should_try_jni_fallback(&Error::EncodeError(
            "WebP encode failed".into()
        )));
        assert!(!should_try_jni_fallback(&Error::NativeError(
            "JavaVM attach failed".into()
        )));
        assert!(!should_try_jni_fallback(&Error::UnsupportedFormat(
            "unknown".into()
        )));
    }

    #[test]
    fn successful_fallback_wins() {
        let result = resolve_fallback(
            Error::DecodeError("AImageDecoder failed".into()),
            Ok(b"jni output".to_vec()),
        );
        assert_eq!(result.unwrap(), b"jni output");
    }

    #[test]
    fn decode_error_keeps_primary_classification_when_fallback_fails() {
        let error = resolve_fallback(
            Error::DecodeError("AImageDecoder failed: -6".into()),
            Err(Error::PlatformNotSupported(
                "JavaVM was not initialized".into(),
            )),
        )
        .expect_err("both decoders failed");

        match error {
            Error::DecodeError(message) => {
                assert!(message.contains("AImageDecoder failed: -6"), "{message}");
                assert!(message.contains("JavaVM was not initialized"), "{message}");
            }
            other => panic!("expected DecodeError, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_primary_returns_the_actionable_fallback_error() {
        let error = resolve_fallback(
            Error::PlatformNotSupported("format not supported by AImageDecoder".into()),
            Err(Error::PlatformNotSupported(
                "JavaVM was not initialized".into(),
            )),
        )
        .expect_err("both decoders failed");

        match error {
            Error::PlatformNotSupported(message) => {
                assert!(message.contains("JavaVM was not initialized"), "{message}");
            }
            other => panic!("expected PlatformNotSupported, got {other:?}"),
        }
    }
}
