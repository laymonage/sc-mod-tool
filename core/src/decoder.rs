use std::ffi::{c_int, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::Error;
use crate::sound_map_file::pitched_sample_rate;
use crate::vgmstream::Vgmstream;

unsafe extern "C" {
    fn decode_sc1_ubi_adpcm(
        input: *const u8,
        input_size: u32,
        output: *mut i16,
        output_samples: u32,
        output_sample_rate: *mut u32,
        is_cancelled: unsafe extern "C" fn(*mut c_void) -> c_int,
        cancel_context: *mut c_void,
    ) -> c_int;
}

unsafe extern "C" fn check_cancelled(context: *mut c_void) -> c_int {
    let cancelled = unsafe { &*context.cast::<AtomicBool>() };
    if cancelled.load(Ordering::Relaxed) {
        1
    } else {
        0
    }
}

#[derive(Debug)]
pub struct DecodedAudio {
    /// Interleaved samples.
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    /// One or two.
    pub channels: u16,
}

pub fn decode(
    input: &[u8],
    sample_count: u32,
    cancelled: &AtomicBool,
) -> Result<DecodedAudio, Error> {
    let input_size =
        u32::try_from(input.len()).map_err(|_| "Compressed segment is too large to decode.")?;
    let output_size =
        usize::try_from(sample_count).map_err(|_| "Sample count is too large to decode.")?;
    if output_size == 0 {
        return Err("The segment has no audio samples.".into());
    }

    let mut samples = vec![0_i16; output_size];
    let mut sample_rate = 0;
    let decoded = unsafe {
        decode_sc1_ubi_adpcm(
            input.as_ptr(),
            input_size,
            samples.as_mut_ptr(),
            sample_count,
            &mut sample_rate,
            check_cancelled,
            (cancelled as *const AtomicBool).cast_mut().cast::<c_void>(),
        )
    };
    if decoded == -2 {
        return Err(Error::Cancelled);
    }
    if decoded < 0 {
        return Err("vgmstream rejected the SC1 mono ADPCM segment.".into());
    }
    if decoded as u32 != sample_count {
        return Err(format!(
            "Expected {sample_count} decoded samples, but vgmstream returned {decoded}."
        )
        .into());
    }
    if sample_rate == 0 {
        return Err("vgmstream returned an invalid sample rate.".into());
    }

    Ok(DecodedAudio {
        samples,
        sample_rate,
        channels: 1,
    })
}

/// Decodes sound `subsong` of the sound map at `map_path` under `sounds`,
/// which must be canonical, to play at `pitch` (16.16 fixed point).
pub(crate) fn decode_map_sound(
    sounds: &Path,
    map_path: &str,
    subsong: u32,
    pitch: u32,
    cancelled: &AtomicBool,
) -> Result<DecodedAudio, Error> {
    let mut vgmstream = Vgmstream::new()?;
    vgmstream.open(sounds, map_path, subsong)?;
    let sound = vgmstream.decode(cancelled)?;
    let (samples, channels) = if sound.channels > 2 {
        (mix_layers(&sound.samples, sound.channels as usize), 2)
    } else {
        (sound.samples, sound.channels as u16)
    };
    if samples.is_empty() {
        return Err("The sound has no audio samples.".into());
    }
    Ok(DecodedAudio {
        samples,
        sample_rate: pitched_sample_rate(sound.sample_rate, pitch),
        channels,
    })
}

/// Mixes a sound in stereo layers, such as the three in `STREAM.SS0`'s
/// sounds, into one stereo pair, scaled down if the sum would clip.
fn mix_layers(samples: &[i16], channels: usize) -> Vec<i16> {
    let mixed: Vec<i32> = samples
        .chunks_exact(channels)
        .flat_map(|frame| {
            let mut pair = [0_i32; 2];
            for (channel, sample) in frame.iter().enumerate() {
                pair[channel % 2] += i32::from(*sample);
            }
            pair
        })
        .collect();
    let peak = mixed
        .iter()
        .map(|sample| sample.unsigned_abs())
        .max()
        .unwrap_or(0);
    let limit = i16::MAX as u32;
    mixed
        .into_iter()
        .map(|sample| {
            let sample = if peak > limit {
                (i64::from(sample) * i64::from(limit) / i64::from(peak)) as i32
            } else {
                sample
            };
            sample as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_minimal_mono_ubi_adpcm_segment() {
        let bytes = crate::tests::make_segment();

        let cancelled = AtomicBool::new(false);
        let decoded = decode(&bytes, 4, &cancelled).unwrap();
        assert_eq!(decoded.samples.len(), 4);
        assert_eq!(decoded.sample_rate, 36000);
    }

    #[test]
    fn decodes_a_minimal_6_bit_segment() {
        // Extra maps' banks use 6-bit codes.
        let bytes = crate::tests::make_segment_with_bits(6);

        let cancelled = AtomicBool::new(false);
        let decoded = decode(&bytes, 4, &cancelled).unwrap();
        assert_eq!(decoded.samples.len(), 4);
    }

    #[test]
    fn rejects_streams_outside_the_supported_shape() {
        let cancelled = AtomicBool::new(false);
        assert!(decode(&[0; 32], 4, &cancelled).is_err());
    }

    #[test]
    fn mixes_stereo_layers_without_clipping() {
        // Two frames of three stereo layers.
        let quiet = [1, 2, 3, 4, 5, 6, -1, -2, -3, -4, -5, -6];
        assert_eq!(mix_layers(&quiet, 6), [9, 12, -9, -12]);

        let loud = [i16::MAX, 0, i16::MAX, 0, 0, 0, 100, 0, 0, 0, 0, 0];
        let mixed = mix_layers(&loud, 6);
        assert_eq!(mixed[0], i16::MAX);
        assert_eq!(mixed[2], 50);
    }

    #[test]
    fn honours_cancellation_during_native_decode() {
        let bytes = crate::tests::make_segment();
        let cancelled = AtomicBool::new(true);

        assert_eq!(decode(&bytes, 4, &cancelled).unwrap_err(), Error::Cancelled);
    }
}
