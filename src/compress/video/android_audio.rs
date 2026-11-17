//! PCM conversion helpers for the Android audio path.
//!
//! MediaCodec decoders can emit 8-bit, 16-bit, float, 24-bit packed or 32-bit
//! PCM depending on the source codec and device. The AAC encoder used by the
//! Android backend consumes interleaved signed 16-bit little-endian samples,
//! so every other layout is converted instead of being copied as if it were
//! already 16-bit.

use std::borrow::Cow;

use crate::error::Error;

// Values from `android.media.AudioFormat`. MediaFormat's `pcm-encoding` key
// was added in API 28, so older devices simply omit it; that means the
// decoder's default, signed 16-bit PCM.
pub(crate) const PCM_ENCODING_DEFAULT: i32 = 1;
pub(crate) const PCM_ENCODING_16BIT: i32 = 2;
pub(crate) const PCM_ENCODING_8BIT: i32 = 3;
pub(crate) const PCM_ENCODING_FLOAT: i32 = 4;
pub(crate) const PCM_ENCODING_24BIT_PACKED: i32 = 21;
pub(crate) const PCM_ENCODING_32BIT: i32 = 22;

/// Convert interleaved PCM in the decoder's reported encoding to signed
/// 16-bit little-endian, the format the AAC encoder accepts.
///
/// A 16-bit buffer is returned borrowed so the common path does not allocate.
pub(crate) fn to_s16le<'a>(
    data: &'a [u8],
    encoding: i32,
    channels: i32,
) -> Result<Cow<'a, [u8]>, Error> {
    if channels <= 0 {
        return Err(Error::DecodeError(format!(
            "invalid PCM channel count: {channels}"
        )));
    }

    let bytes_per_sample: usize = match encoding {
        // `0` is AudioFormat.ENCODING_INVALID and `1` is ENCODING_DEFAULT;
        // both fall back to the decoder's documented 16-bit default.
        0 | PCM_ENCODING_DEFAULT | PCM_ENCODING_16BIT => 2,
        PCM_ENCODING_8BIT => 1,
        PCM_ENCODING_24BIT_PACKED => 3,
        PCM_ENCODING_FLOAT | PCM_ENCODING_32BIT => 4,
        other => {
            return Err(Error::DecodeError(format!(
                "unsupported decoder PCM encoding: {other}"
            )));
        }
    };

    let frame_size = bytes_per_sample
        .checked_mul(channels as usize)
        .ok_or_else(|| Error::DecodeError("PCM frame size overflow".into()))?;
    if frame_size == 0 || !data.len().is_multiple_of(frame_size) {
        return Err(Error::DecodeError(format!(
            "PCM buffer length {} is not a multiple of frame size {frame_size}",
            data.len()
        )));
    }

    if bytes_per_sample == 2 {
        return Ok(Cow::Borrowed(data));
    }

    let mut out = Vec::with_capacity(data.len() / bytes_per_sample * 2);
    for sample in data.chunks_exact(bytes_per_sample) {
        let value = match encoding {
            PCM_ENCODING_8BIT => (i16::from(sample[0]) - 128) << 8,
            PCM_ENCODING_24BIT_PACKED => {
                let value =
                    i32::from(sample[0]) | i32::from(sample[1]) << 8 | i32::from(sample[2]) << 16;
                ((value << 8) >> 16) as i16
            }
            PCM_ENCODING_32BIT => {
                (i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) >> 16) as i16
            }
            PCM_ENCODING_FLOAT => {
                let value = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
                let value = if value.is_nan() {
                    0.0
                } else {
                    value.clamp(-1.0, 1.0)
                };
                (value * 32767.0).round() as i16
            }
            _ => unreachable!("encoding was validated above"),
        };
        out.extend_from_slice(&value.to_le_bytes());
    }
    Ok(Cow::Owned(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(bytes: &[u8]) -> Vec<i16> {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect()
    }

    #[test]
    fn leaves_16_bit_pcm_borrowed() {
        let data = [0x34, 0x12, 0xfe, 0xff];
        let converted = to_s16le(&data, PCM_ENCODING_16BIT, 2).expect("16-bit PCM");
        assert!(matches!(converted, Cow::Borrowed(_)));
        assert_eq!(converted.as_ref(), data);
    }

    #[test]
    fn converts_unsigned_8_bit_pcm() {
        let converted = to_s16le(&[0, 128, 255], PCM_ENCODING_8BIT, 1).expect("8-bit PCM");
        assert_eq!(samples(&converted), vec![-32768, 0, 32512]);
    }

    #[test]
    fn converts_float_pcm_with_clamping() {
        let mut data = Vec::new();
        for value in [0.0f32, 0.5, -0.5, 1.0, -1.0, 2.0, -2.0, f32::NAN] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let converted = to_s16le(&data, PCM_ENCODING_FLOAT, 2).expect("float PCM");
        assert_eq!(
            samples(&converted),
            vec![0, 16384, -16384, 32767, -32767, 32767, -32767, 0]
        );
    }

    #[test]
    fn converts_24_bit_pcm_with_sign_extension() {
        let data = [
            0x00, 0x00, 0x80, // -8388608
            0xff, 0xff, 0x7f, // 8388607
            0x00, 0x01, 0x00, // 256
            0xff, 0xff, 0xff, // -1
        ];
        let converted = to_s16le(&data, PCM_ENCODING_24BIT_PACKED, 1).expect("24-bit PCM");
        assert_eq!(samples(&converted), vec![-32768, 32767, 1, -1]);
    }

    #[test]
    fn converts_32_bit_pcm() {
        let mut data = Vec::new();
        for value in [0i32, 32768, -32768, i32::MAX, i32::MIN] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let converted = to_s16le(&data, PCM_ENCODING_32BIT, 1).expect("32-bit PCM");
        assert_eq!(samples(&converted), vec![0, 0, -1, 32767, -32768]);
    }

    #[test]
    fn rejects_unaligned_and_unknown_pcm() {
        assert!(to_s16le(&[0, 0, 0], PCM_ENCODING_16BIT, 2).is_err());
        assert!(to_s16le(&[0], PCM_ENCODING_8BIT, 2).is_err());
        assert!(to_s16le(&[0, 0, 0, 0], 99, 1).is_err());
        assert!(to_s16le(&[0, 0], PCM_ENCODING_16BIT, 0).is_err());
    }
}
