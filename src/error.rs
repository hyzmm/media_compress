use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Unsupported image format: {0}")]
    UnsupportedFormat(String),

    #[error("Decode error: {0}")]
    DecodeError(String),

    #[error("Encode error: {0}")]
    EncodeError(String),

    #[error("Native API error: {0}")]
    NativeError(String),

    #[error("Platform not supported for format: {0}")]
    PlatformNotSupported(String),
}
