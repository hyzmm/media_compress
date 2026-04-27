use crate::error::Error;

pub fn compress(_input: &[u8], _quality: f32) -> Result<Vec<u8>, Error> {
    Err(Error::PlatformNotSupported(
        "Web: image decoding must be performed on the JavaScript side".into(),
    ))
}
