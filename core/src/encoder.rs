//! An encoder for the mono Ubisoft ADPCM streams in numeric `.LS0` banks,
//! with 4-bit or 6-bit codes, the format [`crate::decoder::decode`] reads.
//!
//! A stream is a 0x30-byte header, then frames. Each frame holds the
//! decoder's state (0x34 bytes) and up to two subframes of 1536 codes, the
//! last of which may be shorter. The decoder is vgmstream's
//! `ubi_adpcm_decoder.c`, which this follows exactly; the code search is
//! adapted from previous research done in DecUbiSnd.

use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::Error;

pub const STREAM_HEADER_SIZE: usize = 0x30;
const STATE_SIZE: usize = 0x34;
const CODES_PER_SUBFRAME: u32 = 1536;
const SUBFRAMES_PER_FRAME: u32 = 2;
/// How many sequences of codes [`search`] keeps. Wider beams gain little more
/// on speech but take longer: this encodes 4-bit streams about 30 times faster
/// than they play, and 6-bit ones 15 times, in a release build.
const BEAM_WIDTH: usize = 4;

/// A stream's header, kept whole so that fields nobody understands, such as
/// `+0x1C`, are written back as they were.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamHeader([u8; STREAM_HEADER_SIZE]);

impl StreamHeader {
    /// Reads the header at the start of `stream`, which must be a mono
    /// stream with 4-bit or 6-bit codes.
    pub fn parse(stream: &[u8]) -> Result<Self, Error> {
        let bytes: [u8; STREAM_HEADER_SIZE] = stream
            .get(..STREAM_HEADER_SIZE)
            .and_then(|header| header.try_into().ok())
            .ok_or("The stream is too short to have a header.")?;
        let header = Self(bytes);
        if header.field(0x00) != 8 || header.field(0x14) != SUBFRAMES_PER_FRAME {
            return Err("The stream doesn't have a Ubisoft ADPCM header.".into());
        }
        if header.field(0x2C) != 1 {
            return Err("Only mono streams can be replaced.".into());
        }
        match header.field(0x24) {
            4 | 6 => Ok(header),
            bits => Err(format!("Streams with {bits}-bit codes aren't supported.").into()),
        }
    }

    pub fn sample_count(&self) -> u32 {
        self.field(0x04)
    }

    /// The stream's sample rate, 36,000 Hz unless the header says otherwise.
    pub fn sample_rate(&self) -> u32 {
        match self.field(0x18) {
            0 => 36000,
            rate => rate,
        }
    }

    /// The length of a stream like this one with `sample_count` samples,
    /// without the padding that banks add after it.
    pub fn stream_length(&self, sample_count: u32) -> usize {
        let width = self.width();
        let mut length = STREAM_HEADER_SIZE;
        for (index, codes) in subframe_code_counts(sample_count).enumerate() {
            if index % SUBFRAMES_PER_FRAME as usize == 0 {
                length += STATE_SIZE;
            }
            length += subframe_length(codes, width);
        }
        length
    }

    /// This header, describing a stream of `sample_count` samples.
    fn for_samples(&self, sample_count: u32) -> StreamHeader {
        let subframe_count = sample_count.div_ceil(CODES_PER_SUBFRAME);
        let mut header = self.clone();
        header.set_field(0x04, sample_count);
        header.set_field(0x08, subframe_count);
        header.set_field(
            0x0C,
            sample_count - (subframe_count - 1) * CODES_PER_SUBFRAME,
        );
        header.set_field(0x10, CODES_PER_SUBFRAME);
        header
    }

    fn width(&self) -> Width {
        if self.field(0x24) == 6 {
            Width::Six
        } else {
            Width::Four
        }
    }

    fn field(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.0[offset..offset + 4].try_into().unwrap())
    }

    fn set_field(&mut self, offset: usize, value: u32) {
        self.0[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}

/// The size of a stream's codes, each with its own decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Width {
    Four,
    Six,
}

impl Width {
    fn bits(self) -> u32 {
        match self {
            Width::Four => 4,
            Width::Six => 6,
        }
    }

    /// The codes the encoder may choose: those the game's files use, so that
    /// a replacement uses nothing the game's own audio doesn't.
    ///
    /// The codes left out of each width are those vgmstream marks as
    /// unexpected. 4-bit code 15 and 6-bit code 63 reach steps past the end
    /// of their decoder's tables. 6-bit codes 0 and 62 reach step 31, which
    /// makes the largest change but then shrinks the step. Choosing the
    /// nearest code for each sample in turn, the encoder kept picking them
    /// and fell behind the input; a beam search might use them well, but
    /// that isn't needed.
    fn codes(self) -> RangeInclusive<u8> {
        match self {
            Width::Four => 0..=14,
            Width::Six => 1..=61,
        }
    }
}

/// Encodes `samples` as a stream, with `template`'s header updated for their
/// number.
pub fn encode_stream(
    template: &StreamHeader,
    samples: &[i16],
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    encode(template, samples, cancelled).map(|(stream, _)| stream)
}

/// Encodes `samples`, and returns the stream with the samples a decoder will
/// read from it.
fn encode(
    template: &StreamHeader,
    samples: &[i16],
    cancelled: &AtomicBool,
) -> Result<(Vec<u8>, Vec<i16>), Error> {
    let sample_count = u32::try_from(samples.len())
        .ok()
        .filter(|count| *count > 0)
        .ok_or("A stream needs at least one sample, and fewer than 2³² of them.")?;
    let width = template.width();
    let header = template.for_samples(sample_count);
    let mut stream = Vec::with_capacity(header.stream_length(sample_count));
    stream.extend_from_slice(&header.0);
    let mut decoded = Vec::with_capacity(samples.len());
    let mut state = State::INITIAL;
    let mut remaining = samples;
    for (index, count) in subframe_code_counts(sample_count).enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if index % SUBFRAMES_PER_FRAME as usize == 0 {
            state.write(&mut stream);
        }
        let (subframe, rest) = remaining.split_at(count as usize);
        remaining = rest;
        let codes = search(&state, subframe, width);
        decoded.extend(codes.iter().map(|&code| state.expand(code, width)));
        pack_codes(&codes, width, &mut stream);
    }
    debug_assert_eq!(stream.len(), header.stream_length(sample_count));
    Ok((stream, decoded))
}

/// The number of codes in each subframe of a stream of `sample_count`
/// samples: all full but the last.
fn subframe_code_counts(sample_count: u32) -> impl Iterator<Item = u32> {
    let full = sample_count / CODES_PER_SUBFRAME;
    let last = sample_count % CODES_PER_SUBFRAME;
    std::iter::repeat_n(CODES_PER_SUBFRAME, full as usize).chain((last > 0).then_some(last))
}

/// A subframe's length, which the decoder works out as one byte more than
/// the whole bytes its codes fill.
fn subframe_length(code_count: u32, width: Width) -> usize {
    (code_count * width.bits() / 8 + 1) as usize
}

/// Chooses codes for `samples`, starting from `state`, with a beam search:
/// after each sample, it keeps the [`BEAM_WIDTH`] sequences of codes that
/// decode nearest the samples so far. Picking the nearest code for each
/// sample in turn does worse, as a code also changes how later ones decode:
/// on speech, by about 2 dB of signal-to-noise ratio for 4-bit codes and 7 dB
/// for 6-bit ones, whose decoder carries each change into the next sample.
fn search(state: &State, samples: &[i16], width: Width) -> Vec<u8> {
    struct Path {
        state: State,
        error: u64,
        node: u32,
    }
    struct Candidate {
        state: State,
        error: u64,
        path: usize,
        code: u8,
    }
    const ROOT: u32 = u32::MAX;

    // Each path's codes, as a tree of (parent, code) pairs.
    let mut nodes: Vec<(u32, u8)> = Vec::with_capacity(samples.len() * BEAM_WIDTH);
    let mut paths = vec![Path {
        state: *state,
        error: 0,
        node: ROOT,
    }];
    let mut next_paths = Vec::with_capacity(BEAM_WIDTH);
    let mut candidates = Vec::with_capacity(BEAM_WIDTH * width.codes().len());
    for &sample in samples {
        candidates.clear();
        for (index, path) in paths.iter().enumerate() {
            for code in width.codes() {
                let mut state = path.state;
                let output = state.expand(code, width);
                let difference = i64::from(sample) - i64::from(output);
                candidates.push(Candidate {
                    state,
                    error: path.error + difference.unsigned_abs().pow(2),
                    path: index,
                    code,
                });
            }
        }
        // Paths that reach the same state have the same future, so only the
        // better of them is kept, and a few spares are sorted in case.
        let shortlist = (BEAM_WIDTH * 2).min(candidates.len());
        if shortlist < candidates.len() {
            candidates.select_nth_unstable_by_key(shortlist, |candidate| candidate.error);
        }
        let shortlist = &mut candidates[..shortlist];
        shortlist.sort_unstable_by_key(|candidate| candidate.error);
        next_paths.clear();
        for candidate in shortlist.iter() {
            if next_paths
                .iter()
                .any(|path: &Path| path.state == candidate.state)
            {
                continue;
            }
            nodes.push((paths[candidate.path].node, candidate.code));
            next_paths.push(Path {
                state: candidate.state,
                error: candidate.error,
                node: nodes.len() as u32 - 1,
            });
            if next_paths.len() == BEAM_WIDTH {
                break;
            }
        }
        std::mem::swap(&mut paths, &mut next_paths);
    }

    let mut codes = vec![0; samples.len()];
    let mut node = paths[0].node;
    for code in codes.iter_mut().rev() {
        let (parent, value) = nodes[node as usize];
        *code = value;
        node = parent;
    }
    codes
}

/// Packs a subframe's codes into `output`.
///
/// The decoder reads the codes as one stream of bits, most significant first,
/// from 32-bit little-endian words; 6-bit codes can span two words. A
/// subframe that ends partway through a word only has [`subframe_length`]
/// bytes of it, and the game's files write those like this, every one
/// checked: when the whole word fits, it's written as usual with the unused
/// bits zero; otherwise the bits are written in order, high bits first.
/// vgmstream reads the latter as a whole word anyway, so the last few samples
/// of such a stream decode differently there.
fn pack_codes(codes: &[u8], width: Width, output: &mut Vec<u8>) {
    let bits = width.bits();
    let mut pending = 0_u64;
    let mut pending_bits = 0;
    for &code in codes {
        pending = (pending << bits) | u64::from(code);
        pending_bits += bits;
        if pending_bits >= 32 {
            pending_bits -= 32;
            output.extend_from_slice(&((pending >> pending_bits) as u32).to_le_bytes());
            pending &= (1 << pending_bits) - 1;
        }
    }
    let code_bits = codes.len() as u32 * bits;
    let tail_length = subframe_length(codes.len() as u32, width) - (code_bits / 32 * 4) as usize;
    // Without a partial word, this is just the padding byte.
    let word = (pending << (32 - pending_bits)) as u32;
    if tail_length == 4 {
        output.extend_from_slice(&word.to_le_bytes());
    } else {
        output.extend_from_slice(&word.to_be_bytes()[..tail_length]);
    }
}

/// The decoder's state for one channel, as stored at the start of each frame.
/// The 6-bit decoder only uses the step, `hist1` and the latest delta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct State {
    step1: i32,
    next1: i32,
    next2: i32,
    coef1: i16,
    coef2: i16,
    mods: [i16; 4],
    hist1: i16,
    hist2: i16,
    deltas: [i16; 5],
}

const TABLE1_4: [i32; 8] = [-100000000, 8, 269, 425, 545, 645, 745, 850];
const TABLE2_4: [i32; 8] = [-1536, 2314, 5243, 8192, 14336, 25354, 45445, 143626];
#[rustfmt::skip]
const TABLE1_6: [i32; 32] = [
    -100000000, -369, -245, -133, -33, 56, 135, 207,
    275, 338, 395, 448, 499, 548, 593, 635,
    676, 717, 755, 791, 825, 858, 889, 919,
    948, 975, 1003, 1029, 1054, 1078, 1103, 1132,
];
#[rustfmt::skip]
const TABLE2_6: [i32; 32] = [
    1800, 1800, 1800, 2048, 3072, 4096, 5000, 5056,
    5184, 5240, 6144, 6880, 9624, 12880, 14952, 18040,
    20480, 22920, 25600, 28040, 32560, 35840, 40960, 45832,
    51200, 56320, 63488, 67704, 75776, 89088, 102400, 0,
];
#[rustfmt::skip]
const DELTA_TABLE: [i32; 66] = [
    1024, 1031, 1053, 1076, 1099, 1123, 1148, 1172,
    1198, 1224, 1251, 1278, 1306, 1334, 1363, 1393,
    1423, 1454, 1485, 1518, 1551, 1584, 1619, 1654,
    1690, 1726, 1764, 1802, 1841, 1881, 1922, 1964,
    2007,
    -1024, -1031, -1053, -1076, -1099, -1123, -1148, -1172,
    -1198, -1224, -1251, -1278, -1306, -1334, -1363, -1393,
    -1423, -1454, -1485, -1518, -1551, -1584, -1619, -1654,
    -1690, -1726, -1764, -1802, -1841, -1881, -1922, -1964,
    -2007,
];

impl State {
    /// The state almost every stream in the game starts with.
    const INITIAL: State = State {
        step1: 0x500,
        next1: 0,
        next2: 0,
        coef1: 0,
        coef2: 0,
        mods: [0; 4],
        hist1: 0,
        hist2: 0,
        deltas: [0; 5],
    };

    /// Writes the state in its 0x34-byte form. The unused fields are zero,
    /// as in the game's files.
    fn write(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&2_u32.to_le_bytes());
        for value in [self.step1, self.next1, self.next2] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        let [mod1, mod2, mod3, mod4] = self.mods;
        let [delta1, delta2, delta3, delta4, delta5] = self.deltas;
        #[rustfmt::skip]
        let fields = [
            self.coef1, self.coef2, 0, 0,
            mod1, mod2, mod3, mod4,
            self.hist1, self.hist2, 0, 0,
            delta1, delta2, delta3, delta4,
            delta5, 0,
        ];
        for value in fields {
            output.extend_from_slice(&value.to_le_bytes());
        }
    }

    #[cfg(test)]
    fn read(bytes: &[u8]) -> State {
        let i32_at =
            |offset: usize| i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let i16_at =
            |offset: usize| i16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
        State {
            step1: i32_at(0x04),
            next1: i32_at(0x08),
            next2: i32_at(0x0C),
            coef1: i16_at(0x10),
            coef2: i16_at(0x12),
            mods: [i16_at(0x18), i16_at(0x1A), i16_at(0x1C), i16_at(0x1E)],
            hist1: i16_at(0x20),
            hist2: i16_at(0x22),
            deltas: [
                i16_at(0x28),
                i16_at(0x2A),
                i16_at(0x2C),
                i16_at(0x2E),
                i16_at(0x30),
            ],
        }
    }

    /// Decodes `code`, updating the state. This is vgmstream's
    /// `expand_code_4bit` or `expand_code_6bit`, with C's wrapping 32-bit
    /// arithmetic.
    ///
    /// vgmstream's tables have junk past step 7 for 4-bit codes and step 31
    /// for 6-bit ones, and these copies leave it out, so 4-bit code 15 and
    /// 6-bit code 63 can't be decoded.
    fn expand(&mut self, code: u8, width: Width) -> i16 {
        debug_assert!(code < (1 << width.bits()) - 1, "code {code} has no step");
        match width {
            Width::Four => self.expand_4bit(code),
            Width::Six => self.expand_6bit(code),
        }
    }

    fn expand_4bit(&mut self, code: u8) -> i16 {
        let code_signed = i32::from(code) - 7;
        let step_index = code_signed.unsigned_abs() as usize;
        let step_next = TABLE1_4[step_index].wrapping_add(self.step1);
        let step = self.next_step(TABLE2_4[step_index]);
        let delta = step_delta(step_next, code_signed < 0);

        let [mod1, mod2, mod3, mod4] = self.mods.map(i32::from);
        let [delta1, delta2, delta3, delta4, _] = self.deltas.map(i32::from);
        let next = ((mod1 * delta1)
            .wrapping_add(mod2 * delta2)
            .wrapping_add(mod3 * delta3)
            .wrapping_add(mod4 * delta4)
            >> 10) as i16;
        let prediction = (i32::from(self.coef1) * i32::from(self.hist1))
            .wrapping_add(i32::from(self.coef2) * i32::from(self.hist2))
            >> 10;
        let sample = delta.wrapping_add(i32::from(next)).wrapping_add(prediction) as i16;

        let mut coef1_next = i32::from(self.coef1) * 255;
        let mut coef2_next = i32::from(self.coef2) * 254;
        let delta = delta as i16;
        let change = i32::from(delta) + i32::from(next);
        if change != 0 {
            let sign1 = sign(change) * sign(delta1 + self.next1);
            let sign2 = sign(change) * sign(delta2 + self.next2);
            let mut coef_delta = i32::from(((((sign1 * 3072) + coef1_next) >> 6) & !0x3) as i16);
            coef_delta = clamp16(clamp16(coef_delta + 30719) - 30719);
            coef_delta = clamp16(clamp16(coef_delta - 30720) + 30720);
            coef_delta =
                (i32::from((sign2 * 1024) as i16) - i32::from((sign1 * coef_delta) as i16)) * 2;
            coef1_next += sign1 * 3072;
            coef2_next += coef_delta;
        }

        self.hist2 = self.hist1;
        self.hist1 = sample;
        self.coef2 = abs_max((coef2_next >> 8) as i16, 768);
        self.coef1 = abs_max((coef1_next >> 8) as i16, 960 - i32::from(self.coef2));
        self.next2 = self.next1;
        self.next1 = i32::from(next);
        self.step1 = step;
        self.deltas = [
            delta,
            delta1 as i16,
            delta2 as i16,
            delta3 as i16,
            delta4 as i16,
        ];

        let latest = sign(i32::from(delta));
        for (modulation, older) in self.mods.iter_mut().zip(&self.deltas[1..]) {
            let target = 2048 * latest * sign(i32::from(*older));
            *modulation = (clamp16(i32::from(*modulation) * 255 + target) >> 8) as i16;
        }
        sample
    }

    fn expand_6bit(&mut self, code: u8) -> i16 {
        let code_signed = i32::from(code) - 31;
        let step_index = code_signed.unsigned_abs() as usize;
        let step_next = TABLE1_6[step_index].wrapping_add(self.step1);
        let step = self.next_step(TABLE2_6[step_index]);
        let delta = step_delta(step_next, code_signed < 0);

        let sample = delta
            .wrapping_add(i32::from(self.deltas[0]))
            .wrapping_add(i32::from(self.hist1)) as i16;
        self.hist1 = sample;
        self.step1 = step;
        self.deltas[0] = delta as i16;
        sample
    }

    fn next_step(&self, adjustment: i32) -> i32 {
        (((self.step1 & 0xFFFF) * 246 + adjustment) >> 8).clamp(271, 2560)
    }
}

/// The change a code makes, from its step and sign.
fn step_delta(step_next: i32, negative: bool) -> i32 {
    if ((step_next as u32 & 0xFFFF_FF00).wrapping_sub(1) & 0x8000_0000) != 0 {
        return 0;
    }
    let index = ((step_next >> 3) & 0x1F) as usize + if negative { 33 } else { 0 };
    let shift = ((step_next >> 8) & 0xFF).clamp(0, 31) as u32;
    DELTA_TABLE[index].wrapping_shl(shift) >> 10
}

fn sign(value: i32) -> i32 {
    if value < 0 { -1 } else { 1 }
}

fn clamp16(value: i32) -> i32 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX))
}

fn abs_max(value: i16, limit: i32) -> i16 {
    let value = i32::from(value);
    (if value < -limit {
        -limit
    } else if value > limit {
        limit
    } else {
        value
    }) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder;
    use std::sync::atomic::AtomicUsize;

    const WIDTHS: [Width; 2] = [Width::Four, Width::Six];

    fn template(width: Width) -> StreamHeader {
        let mut bytes = [0; STREAM_HEADER_SIZE];
        let (bits, unknown28) = match width {
            Width::Four => (4, 1),
            Width::Six => (6, 3),
        };
        for (offset, value) in [
            (0x00, 8_u32),
            (0x14, 2),
            (0x18, 36000),
            (0x1C, 0x0123_4567),
            (0x24, bits),
            (0x28, unknown28),
            (0x2C, 1),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        StreamHeader::parse(&bytes).unwrap()
    }

    /// Deterministic test signals of `length` samples.
    fn signals(length: usize) -> Vec<(&'static str, Vec<i16>)> {
        let mut seed = 0x1234_5678_u32;
        let noise = (0..length)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 16) as i16 / 4
            })
            .collect();
        let sine = (0..length)
            .map(|index| {
                let phase = index as f64 * 440.0 * std::f64::consts::TAU / 36000.0;
                (phase.sin() * 12000.0) as i16
            })
            .collect();
        let square = (0..length)
            .map(|index| {
                if index / 40 % 2 == 0 {
                    i16::MAX
                } else {
                    i16::MIN
                }
            })
            .collect();
        vec![
            ("silence", vec![0; length]),
            ("noise", noise),
            ("sine", sine),
            ("square", square),
        ]
    }

    /// How many samples at the end of a stream of `sample_count` samples
    /// vgmstream reads differently from the game's layout: those with bits in
    /// a last partial word too short to be read as one (see [`pack_codes`]).
    fn unreliable_tail(sample_count: u32, width: Width) -> usize {
        let last = match sample_count % CODES_PER_SUBFRAME {
            0 => CODES_PER_SUBFRAME,
            last => last,
        };
        let bits = last * width.bits();
        let whole_words = bits / 32;
        if bits.is_multiple_of(32) || subframe_length(last, width) - whole_words as usize * 4 == 4 {
            0
        } else {
            (last - whole_words * 32 / width.bits()) as usize
        }
    }

    fn snr(reference: &[i16], decoded: &[i16]) -> f64 {
        let signal: f64 = reference
            .iter()
            .map(|&value| f64::from(value).powi(2))
            .sum();
        let noise: f64 = reference
            .iter()
            .zip(decoded)
            .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
            .sum();
        10.0 * (signal / noise.max(1.0)).log10()
    }

    #[test]
    fn vgmstream_decodes_what_the_encoder_expects() {
        let cancelled = AtomicBool::new(false);
        for width in WIDTHS {
            let template = template(width);
            for length in [
                1, 2, 5, 6, 7, 8, 9, 13, 16, 21, 22, 1535, 1536, 1537, 3072, 3073, 4609, 10_000,
            ] {
                for (name, samples) in signals(length) {
                    let context = format!("{width:?}, {name} × {length}");
                    let (stream, expected) = encode(&template, &samples, &cancelled).unwrap();
                    assert_eq!(
                        stream.len(),
                        template.stream_length(length as u32),
                        "{context}"
                    );
                    let decoded = decoder::decode(&stream, length as u32, &cancelled).unwrap();
                    let compared = length - unreliable_tail(length as u32, width);
                    assert_eq!(
                        decoded.samples[..compared],
                        expected[..compared],
                        "{context}"
                    );
                }
            }
        }
    }

    #[test]
    fn encoded_streams_sound_like_their_input() {
        let cancelled = AtomicBool::new(false);
        // A full-scale square wave is beyond what these codes can follow, so
        // it's only used to check that the encoder and decoder agree. The 6-bit
        // decoder carries each change into the next sample, which suits speech
        // but not white noise.
        for (width, name, minimum) in [
            (Width::Four, "sine", 45.0),
            (Width::Four, "noise", 15.0),
            (Width::Six, "sine", 50.0),
            (Width::Six, "noise", 6.0),
        ] {
            let samples = signals(36000)
                .into_iter()
                .find_map(|(signal, samples)| (signal == name).then_some(samples))
                .unwrap();
            let stream = encode_stream(&template(width), &samples, &cancelled).unwrap();
            let decoded = decoder::decode(&stream, 36000, &cancelled).unwrap();
            let quality = snr(&samples, &decoded.samples);
            assert!(quality > minimum, "{width:?}, {name}: SNR {quality:.1} dB");
        }
    }

    #[test]
    fn banks_find_encoded_streams() {
        let cancelled = AtomicBool::new(false);
        let samples = &signals(5000)[2].1;
        for width in WIDTHS {
            let stream = encode_stream(&template(width), samples, &cancelled).unwrap();
            let mut bank = stream.clone();
            bank.resize(bank.len().next_multiple_of(4), 0);
            bank.extend_from_slice(&stream);
            let segments = crate::banks::scan_ls0(&bank, "ENGLISH/1_1_0.LS0", &cancelled).unwrap();
            let second = bank.len() - stream.len();
            let ranges: Vec<_> = segments
                .iter()
                .map(|segment| segment.stream_range())
                .collect();
            assert_eq!(
                ranges,
                [
                    Some((0, stream.len() as u64)),
                    Some((second as u64, stream.len() as u64))
                ],
                "{width:?}"
            );
            assert!(segments.iter().all(|segment| segment.sample_count == 5000));
        }
    }

    #[test]
    fn headers_keep_the_template_and_describe_the_samples() {
        let cancelled = AtomicBool::new(false);
        for width in WIDTHS {
            let stream = encode_stream(&template(width), &[0; 3100], &cancelled).unwrap();
            let header = StreamHeader::parse(&stream).unwrap();
            assert_eq!(header.sample_count(), 3100);
            assert_eq!(header.field(0x08), 3);
            assert_eq!(header.field(0x0C), 28);
            assert_eq!(header.field(0x10), 1536);
            assert_eq!(header.sample_rate(), 36000);
            assert_eq!(header.field(0x1C), 0x0123_4567);
            assert_eq!(header.width(), width);
            assert_eq!(header.field(0x28), template(width).field(0x28));
            // Frames start with the state's signature.
            assert_eq!(
                stream[STREAM_HEADER_SIZE..STREAM_HEADER_SIZE + 4],
                2_u32.to_le_bytes()
            );
        }
    }

    #[test]
    fn partial_words_follow_the_games_layout() {
        let codes = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        let packed = |count: usize, width: Width| {
            let mut output = Vec::new();
            pack_codes(&codes[..count], width, &mut output);
            output
        };
        assert_eq!(packed(8, Width::Four), [0x78, 0x56, 0x34, 0x12, 0x00]);
        assert_eq!(packed(1, Width::Four), [0x10]);
        assert_eq!(packed(2, Width::Four), [0x12, 0x00]);
        assert_eq!(packed(3, Width::Four), [0x12, 0x30]);
        assert_eq!(packed(5, Width::Four), [0x12, 0x34, 0x50]);
        assert_eq!(packed(6, Width::Four), [0x00, 0x56, 0x34, 0x12]);
        assert_eq!(packed(7, Width::Four), [0x70, 0x56, 0x34, 0x12]);
        assert_eq!(packed(9, Width::Four), [0x78, 0x56, 0x34, 0x12, 0x90]);

        // 000001 000010 000011 000100 000101 000110 …
        assert_eq!(packed(1, Width::Six), [0b0000_0100]);
        assert_eq!(packed(2, Width::Six), [0b0000_0100, 0b0010_0000]);
        assert_eq!(packed(5, Width::Six), [0x14, 0xC4, 0x20, 0x04]);
        // The sixth code spans the first word and the next.
        assert_eq!(packed(6, Width::Six), [0x14, 0xC4, 0x20, 0x04, 0b0110_0000]);
    }

    #[test]
    fn rejects_streams_it_cannot_write() {
        let cancelled = AtomicBool::new(false);
        assert!(encode_stream(&template(Width::Four), &[], &cancelled).is_err());

        let mut five_bit = template(Width::Four).0;
        five_bit[0x24] = 5;
        assert!(StreamHeader::parse(&five_bit).is_err());
        let mut stereo = template(Width::Six).0;
        stereo[0x2C] = 2;
        assert!(StreamHeader::parse(&stereo).is_err());
        assert!(StreamHeader::parse(&[8, 0, 0, 0]).is_err());

        cancelled.store(true, Ordering::Relaxed);
        assert_eq!(
            encode_stream(&template(Width::Four), &[0; 10], &cancelled),
            Err(Error::Cancelled)
        );
    }

    /// Unpacks a subframe's codes the way the game's files lay them out.
    fn unpack_codes(bytes: &[u8], count: usize, width: Width) -> Vec<u8> {
        let bits = width.bits();
        let whole_words = count * bits as usize / 32;
        let mut words: Vec<u32> = bytes[..whole_words * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&word| u32::from_le_bytes(word))
            .collect();
        let tail = &bytes[whole_words * 4..];
        words.push(if tail.len() == 4 {
            u32::from_le_bytes(tail.try_into().unwrap())
        } else {
            let mut word = [0; 4];
            word[..tail.len()].copy_from_slice(tail);
            u32::from_be_bytes(word)
        });

        let mut words = words.into_iter();
        let mut codes = Vec::with_capacity(count);
        let mut pending = 0_u64;
        let mut pending_bits = 0;
        while codes.len() < count {
            if pending_bits < bits {
                pending = (pending << 32) | u64::from(words.next().unwrap());
                pending_bits += 32;
            }
            pending_bits -= bits;
            codes.push((pending >> pending_bits) as u8 & ((1 << bits) - 1));
            pending &= (1 << pending_bits) - 1;
        }
        codes
    }

    /// Checks the encoder against every mono stream in the game: decoding a
    /// stream's codes from its own states matches vgmstream, packing them and
    /// its states again gives the same bytes, and the encoder would give it the
    /// same header and length. Encoding is slow, so only every tenth 4-bit
    /// stream and every 6-bit one is encoded again, to check that it sounds
    /// nearly the same.
    #[test]
    #[ignore = "requires local Splinter Cell game files; set SC1_GAME_DIR"]
    fn matches_every_game_stream() {
        let game_root = std::env::var_os("SC1_GAME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Splinter Cell")
            });
        let cancelled = AtomicBool::new(false);
        let mut cache = crate::GameCache::default();
        let mut streams = Vec::new();
        for language in crate::discover_languages(&game_root).unwrap() {
            let banks = crate::discover_banks_with_progress(
                &game_root,
                &language.directory,
                &mut cache,
                &cancelled,
                |_, _, _| {},
            )
            .unwrap();
            for bank in banks.into_iter().filter(|bank| bank.sound_kind.is_none()) {
                for segment in bank.segments {
                    if let Some((offset, length)) = segment.stream_range() {
                        streams.push((bank.path.clone(), segment.id, offset, length));
                    }
                }
            }
        }

        let next = AtomicUsize::new(0);
        let checked = [AtomicUsize::new(0), AtomicUsize::new(0)];
        let threads = std::thread::available_parallelism().map_or(4, usize::from);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some((path, id, offset, length)) = streams.get(index) else {
                            break;
                        };
                        let original =
                            crate::read_segment(&game_root, path, *offset, *length).unwrap();
                        let width = check_game_stream(id, &original, index.is_multiple_of(10));
                        checked[(width == Width::Six) as usize].fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });
        let [four, six] = checked.map(AtomicUsize::into_inner);
        assert!(
            four > 1000 && six > 100,
            "only {four} 4-bit and {six} 6-bit streams were checked"
        );
        eprintln!("Checked {four} 4-bit and {six} 6-bit streams.");
    }

    /// Checks one of the game's streams, `original`, as
    /// [`matches_every_game_stream`] says. It's encoded again if it has 6-bit
    /// codes or `reencode` is set.
    fn check_game_stream(id: &str, original: &[u8], reencode: bool) -> Width {
        let cancelled = AtomicBool::new(false);
        let template = StreamHeader::parse(original).unwrap();
        let width = template.width();
        let sample_count = template.sample_count();
        assert_eq!(
            template.for_samples(sample_count),
            template,
            "{id}: header differs"
        );
        assert_eq!(
            template.stream_length(sample_count),
            original.len(),
            "{id}: length differs"
        );
        let reference = decoder::decode(original, sample_count, &cancelled)
            .unwrap()
            .samples;

        let mut repacked = original[..STREAM_HEADER_SIZE].to_vec();
        let mut decoded = Vec::new();
        let mut position = STREAM_HEADER_SIZE;
        let mut state = State::INITIAL;
        for (index, count) in subframe_code_counts(sample_count).enumerate() {
            if index % 2 == 0 {
                let bytes = &original[position..position + STATE_SIZE];
                state = State::read(bytes);
                state.write(&mut repacked);
                position += STATE_SIZE;
            }
            let size = subframe_length(count, width);
            let codes = unpack_codes(&original[position..position + size], count as usize, width);
            decoded.extend(codes.iter().map(|&code| state.expand(code, width)));
            pack_codes(&codes, width, &mut repacked);
            position += size;
        }
        assert_eq!(repacked, original, "{id}: repacked codes differ");
        let compared = reference.len() - unreliable_tail(sample_count, width);
        assert_eq!(
            decoded[..compared],
            reference[..compared],
            "{id}: decoded samples differ from vgmstream"
        );

        if reencode || width == Width::Six {
            let encoded = encode_stream(&template, &reference, &cancelled).unwrap();
            assert_eq!(
                encoded.len(),
                original.len(),
                "{id}: encoded length differs"
            );
            assert_eq!(
                encoded[..STREAM_HEADER_SIZE],
                original[..STREAM_HEADER_SIZE],
                "{id}: encoded header differs"
            );
            let reencoded = decoder::decode(&encoded, sample_count, &cancelled)
                .unwrap()
                .samples;
            let quality = snr(&reference, &reencoded);
            // The worst in the game are about 44 dB for 4-bit and 32 dB for
            // 6-bit.
            let minimum = match width {
                Width::Four => 40.0,
                Width::Six => 30.0,
            };
            assert!(quality > minimum, "{id}: re-encoded at {quality:.1} dB SNR");
        }
        width
    }
}
