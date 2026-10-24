use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::{
    ChannelCount, CpalErrorKind, DeviceSinkBuilder, MixerDeviceSink, Player, Sample, SampleRate,
    Source, cpal,
};
use sc_mod_tool_core::decoder::DecodedAudio;

/// A decoded segment, ready to play. Copies share their samples.
#[derive(Clone)]
pub struct PlayableAudio {
    /// Interleaved samples.
    samples: Arc<[Sample]>,
    channels: ChannelCount,
    sample_rate: SampleRate,
}

impl PlayableAudio {
    /// Converts the samples to rodio's format. For a long stream this takes a
    /// while, so it's done with the decoding, off the UI thread.
    pub fn new(audio: DecodedAudio) -> Result<Self, String> {
        let (Some(channels), Some(sample_rate)) = (
            NonZero::new(audio.channels),
            NonZero::new(audio.sample_rate),
        ) else {
            return Err("The segment has no channels or sample rate.".to_owned());
        };
        let samples = audio
            .samples
            .iter()
            .map(|&sample| Sample::from(sample) / 32768.0)
            .collect();
        Ok(Self {
            samples,
            channels,
            sample_rate,
        })
    }

    fn frame_count(&self) -> usize {
        self.samples.len() / usize::from(self.channels.get())
    }

    /// How long `frames` frames play for.
    fn time_of(&self, frames: usize) -> Duration {
        Duration::from_secs_f64(frames as f64 / f64::from(self.sample_rate.get()))
    }

    fn duration(&self) -> Duration {
        self.time_of(self.frame_count())
    }

    /// The frame playing at `time`, never past the end.
    fn frame_at(&self, time: Duration) -> usize {
        ((time.as_secs_f64() * f64::from(self.sample_rate.get())).round() as usize)
            .min(self.frame_count())
    }
}

/// Plays audio to its end and then, while `repeat` is set, again from its loop
/// start, with no gap between them, as the game loops a sound.
struct LoopingSource {
    audio: PlayableAudio,
    /// The next sample.
    position: usize,
    /// The sample the loop starts at.
    loop_start: usize,
    /// Checked at the end, so that turning it off lets the audio finish.
    repeat: Arc<AtomicBool>,
    /// Set once it has gone back to its loop start.
    looped: Arc<AtomicBool>,
}

impl LoopingSource {
    /// A source of `audio` from `start`, which loops from frame `loop_start`.
    fn new(
        audio: &PlayableAudio,
        start: Duration,
        loop_start: usize,
        repeat: &Arc<AtomicBool>,
    ) -> Self {
        let channels = usize::from(audio.channels.get());
        Self {
            audio: audio.clone(),
            position: audio.frame_at(start) * channels,
            loop_start: loop_start * channels,
            repeat: Arc::clone(repeat),
            looped: Arc::new(AtomicBool::new(false)),
        }
    }

    fn is_at_end(&self) -> bool {
        self.position >= self.audio.samples.len()
    }
}

impl Iterator for LoopingSource {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        if self.is_at_end() {
            if self.audio.samples.is_empty() || !self.repeat.load(Ordering::Relaxed) {
                return None;
            }
            self.position = self.loop_start;
            self.looped.store(true, Ordering::Relaxed);
        }
        let sample = self.audio.samples[self.position];
        self.position += 1;
        Some(sample)
    }
}

impl Source for LoopingSource {
    // Its channels and rate never change, so it plays as one span. Rodio
    // tracks its position across loops by counting its samples.
    fn current_span_len(&self) -> Option<usize> {
        (self.is_at_end() && !self.repeat.load(Ordering::Relaxed)).then_some(0)
    }

    fn channels(&self) -> ChannelCount {
        self.audio.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.audio.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// Where a sound that's played for `elapsed` is, if it has looped from
/// `loop_start` to its end, `duration`, since.
fn looped_position(elapsed: Duration, duration: Duration, loop_start: Duration) -> Duration {
    let loop_length = duration.saturating_sub(loop_start);
    if elapsed <= duration || loop_length.is_zero() {
        return elapsed.min(duration);
    }
    let into_loop = (elapsed - loop_start).as_secs_f64() % loop_length.as_secs_f64();
    loop_start + Duration::from_secs_f64(into_loop)
}

/// A segment that is playing, paused, or stopped.
pub struct ActivePlayback {
    pub player: Player,
    pub segment_id: String,
    /// Whether it's the game's own audio for a replaced segment.
    pub original: bool,
    pub duration: Duration,
    /// Whether playback has stopped, at the end or by the user, leaving the
    /// segment loaded at its start.
    pub stopped: bool,
    /// Where in the segment the player's audio starts, after a seek.
    start: Duration,
    /// Kept to load the segment again once it has stopped, or from where it
    /// was sought. Copies of it share its samples.
    audio: PlayableAudio,
    /// The frame it repeats from, while `repeat` is set.
    loop_start: usize,
    repeat: Arc<AtomicBool>,
    /// Whether the player's audio has gone back to its loop start.
    looped: Arc<AtomicBool>,
}

impl ActivePlayback {
    /// How far playback has got, never past the end, and within the loop once
    /// it has repeated.
    pub fn position(&self) -> Duration {
        let elapsed = self.start + self.player.get_pos();
        if self.looped.load(Ordering::Relaxed) {
            looped_position(elapsed, self.duration, self.audio.time_of(self.loop_start))
        } else {
            elapsed.min(self.duration)
        }
    }

    /// Whether to play the audio again from its loop start when it ends. It
    /// can change while it plays.
    pub fn set_repeat(&self, repeat: bool) {
        self.repeat.store(repeat, Ordering::Relaxed);
    }

    pub fn toggle_pause(&mut self) {
        if self.player.is_paused() {
            self.stopped = false;
            self.player.play();
        } else {
            self.player.pause();
        }
    }
}

/// The audio output, kept open between playbacks because opening a device is
/// slow on some platforms.
#[derive(Default)]
pub struct AudioOutput {
    sink: Option<OpenSink>,
}

struct OpenSink {
    /// The default output device when the sink was opened, so it can be
    /// opened again if the user switches devices.
    device_id: Option<cpal::DeviceId>,
    /// Set when the sink reports that it stopped working, such as when its
    /// device was disconnected, so it's opened again for the next segment
    /// even if the device comes back under the same ID.
    failed: Arc<AtomicBool>,
    sink: MixerDeviceSink,
}

impl AudioOutput {
    /// Plays `audio`, which, if `repeat` is set, plays again from frame
    /// `loop_start` each time it ends. A loop start past the end is the start.
    pub fn play(
        &mut self,
        audio: PlayableAudio,
        segment_id: String,
        loop_start: Option<u32>,
        repeat: bool,
    ) -> Result<ActivePlayback, String> {
        let loop_start = loop_start
            .map(|frame| frame as usize)
            .filter(|&frame| frame < audio.frame_count())
            .unwrap_or(0);
        let repeat = Arc::new(AtomicBool::new(repeat));
        let source = LoopingSource::new(&audio, Duration::ZERO, loop_start, &repeat);
        let looped = Arc::clone(&source.looped);
        let player = self.load(source)?;
        player.play();
        Ok(ActivePlayback {
            player,
            segment_id,
            original: false,
            duration: audio.duration(),
            stopped: false,
            start: Duration::ZERO,
            audio,
            loop_start,
            repeat,
            looped,
        })
    }

    /// Stops playback, keeping the segment loaded at its start so it can be
    /// played again. Rodio can't rewind a sound that has ended, so the segment
    /// is loaded again.
    pub fn stop(&mut self, playback: &mut ActivePlayback) -> Result<(), String> {
        self.reload(playback, Duration::ZERO)?;
        playback.stopped = true;
        Ok(())
    }

    /// Moves playback to `position`, leaving it paused or playing as it was.
    ///
    /// The segment is loaded again from there instead of seeking the player.
    /// Rodio's seek waits for the output to take it, and an output whose
    /// device has gone away never does, which would freeze the app. Loading
    /// again also moves playback to a new default device.
    pub fn seek(
        &mut self,
        playback: &mut ActivePlayback,
        position: Duration,
    ) -> Result<(), String> {
        let position = position.min(playback.duration);
        let paused = playback.player.is_paused();
        self.reload(playback, position)?;
        playback.stopped = false;
        if !paused {
            playback.player.play();
        }
        Ok(())
    }

    /// Gives `playback` a paused player with its audio queued from `start`.
    fn reload(&mut self, playback: &mut ActivePlayback, start: Duration) -> Result<(), String> {
        let source = LoopingSource::new(
            &playback.audio,
            start,
            playback.loop_start,
            &playback.repeat,
        );
        let looped = Arc::clone(&source.looped);
        playback.player = self.load(source)?;
        playback.start = start;
        playback.looped = looped;
        Ok(())
    }

    /// A paused player with `source` queued.
    fn load(&mut self, source: LoopingSource) -> Result<Player, String> {
        let device = cpal::default_host().default_output_device();
        let device_id = device.as_ref().and_then(|device| device.id().ok());
        let sink = match &self.sink {
            Some(open) if open.device_id == device_id && !open.failed.load(Ordering::Relaxed) => {
                &open.sink
            }
            _ => {
                // Close the old output before opening the new device.
                self.sink = None;
                let device = device.ok_or("No audio output device was found.")?;
                let failed = Arc::new(AtomicBool::new(false));
                let on_error = {
                    let failed = Arc::clone(&failed);
                    move |error: cpal::Error| {
                        // These leave the output working: it followed a new
                        // default device, or briefly fell behind. Opening it
                        // again is cheap, so anything else counts as failed.
                        if !matches!(
                            error.kind(),
                            CpalErrorKind::DeviceChanged
                                | CpalErrorKind::Xrun
                                | CpalErrorKind::RealtimeDenied
                        ) {
                            failed.store(true, Ordering::Relaxed);
                        }
                    }
                };
                let mut sink = DeviceSinkBuilder::from_device(device)
                    .and_then(|builder| {
                        builder
                            .with_error_callback(on_error)
                            .open_sink_or_fallback()
                    })
                    .map_err(|error| error.to_string())?;
                // The output is replaced whenever the device changes, which
                // rodio would otherwise report on stderr.
                sink.log_on_drop(false);
                &self
                    .sink
                    .insert(OpenSink {
                        device_id,
                        failed,
                        sink,
                    })
                    .sink
            }
        };
        let player = Player::connect_new(sink.mixer());
        player.pause();
        player.append(source);
        Ok(player)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audio of `samples` at 10 Hz.
    fn audio(samples: &[i16], channels: u16) -> PlayableAudio {
        PlayableAudio::new(DecodedAudio {
            samples: samples.to_vec(),
            sample_rate: 10,
            channels,
        })
        .unwrap()
    }

    fn take(source: &mut LoopingSource, count: usize) -> Vec<i16> {
        source
            .by_ref()
            .take(count)
            .map(|sample| (sample * 32768.0).round() as i16)
            .collect()
    }

    #[test]
    fn repeats_from_its_loop_start_with_no_gap() {
        let repeat = Arc::new(AtomicBool::new(true));
        let mut source = LoopingSource::new(&audio(&[1, 2, 3, 4], 1), Duration::ZERO, 2, &repeat);
        assert_eq!(take(&mut source, 7), [1, 2, 3, 4, 3, 4, 3]);
        assert!(source.looped.load(Ordering::Relaxed));

        // Turned off, it finishes the loop and ends.
        repeat.store(false, Ordering::Relaxed);
        assert_eq!(take(&mut source, 7), [4]);
        assert_eq!(source.current_span_len(), Some(0));
    }

    #[test]
    fn loops_whole_frames_from_where_it_starts() {
        let repeat = Arc::new(AtomicBool::new(true));
        let stereo = audio(&[1, 2, 3, 4, 5, 6], 2);
        let mut source = LoopingSource::new(&stereo, Duration::from_millis(200), 1, &repeat);
        assert_eq!(take(&mut source, 6), [5, 6, 3, 4, 5, 6]);
    }

    #[test]
    fn plays_once_without_repeat() {
        let repeat = Arc::new(AtomicBool::new(false));
        let mut source = LoopingSource::new(&audio(&[1, 2, 3], 1), Duration::ZERO, 0, &repeat);
        assert_eq!(take(&mut source, 5), [1, 2, 3]);
        assert!(!source.looped.load(Ordering::Relaxed));

        let repeat = Arc::new(AtomicBool::new(true));
        let mut empty = LoopingSource::new(&audio(&[], 1), Duration::ZERO, 0, &repeat);
        assert_eq!(empty.next(), None);
    }

    #[test]
    fn keeps_a_looped_position_within_the_loop() {
        let seconds = Duration::from_secs;
        let position =
            |elapsed, loop_start| looped_position(seconds(elapsed), seconds(4), loop_start);
        assert_eq!(position(3, seconds(2)), seconds(3));
        assert_eq!(position(4, seconds(2)), seconds(4));
        assert_eq!(position(5, seconds(2)), seconds(3));
        assert_eq!(position(7, seconds(2)), seconds(3));
        assert_eq!(position(9, seconds(0)), seconds(1));
        // A loop of no length stays at the end.
        assert_eq!(position(9, seconds(4)), seconds(4));
    }
}
