use std::num::NonZero;
use std::time::Duration;

use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player, Source, buffer::SamplesBuffer, cpal};
use sc_mod_tool_core::decoder::DecodedAudio;

/// A decoded segment, ready to play.
pub struct PlayableAudio(SamplesBuffer);

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
        let samples: Vec<f32> = audio
            .samples
            .iter()
            .map(|&sample| f32::from(sample) / 32768.0)
            .collect();
        Ok(Self(SamplesBuffer::new(channels, sample_rate, samples)))
    }
}

/// A segment that is playing, paused, or stopped.
pub struct ActivePlayback {
    pub player: Player,
    pub segment_id: String,
    pub duration: Duration,
    /// Whether playback has stopped, at the end or by the user, leaving the
    /// segment loaded at its start.
    pub stopped: bool,
    /// Where in the segment the player's audio starts, after a seek.
    start: Duration,
    /// Kept to load the segment again once it has stopped, or from where it
    /// was sought. Copies of it share its samples.
    audio: SamplesBuffer,
}

impl ActivePlayback {
    /// How far playback has got, never past the end.
    pub fn position(&self) -> Duration {
        (self.start + self.player.get_pos()).min(self.duration)
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
    sink: MixerDeviceSink,
}

impl AudioOutput {
    pub fn play(
        &mut self,
        PlayableAudio(audio): PlayableAudio,
        segment_id: String,
    ) -> Result<ActivePlayback, String> {
        let player = self.load(&audio, Duration::ZERO)?;
        player.play();
        Ok(ActivePlayback {
            player,
            segment_id,
            duration: audio.total_duration().unwrap_or_default(),
            stopped: false,
            start: Duration::ZERO,
            audio,
        })
    }

    /// Stops playback, keeping the segment loaded at its start so it can be
    /// played again. Rodio can't rewind a sound that has ended, so the segment
    /// is loaded again.
    pub fn stop(&mut self, playback: &mut ActivePlayback) -> Result<(), String> {
        playback.player = self.load(&playback.audio, Duration::ZERO)?;
        playback.start = Duration::ZERO;
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
        playback.player = self.load(&playback.audio, position)?;
        playback.start = position;
        playback.stopped = false;
        if !paused {
            playback.player.play();
        }
        Ok(())
    }

    /// A paused player with the audio queued from `start`.
    fn load(&mut self, audio: &SamplesBuffer, start: Duration) -> Result<Player, String> {
        let device = cpal::default_host().default_output_device();
        let device_id = device.as_ref().and_then(|device| device.id().ok());
        let sink = match &self.sink {
            Some(open) if open.device_id == device_id => &open.sink,
            _ => {
                // Close the old output before opening the new device.
                self.sink = None;
                let device = device.ok_or("No audio output device was found.")?;
                let mut sink = DeviceSinkBuilder::from_device(device)
                    .and_then(|builder| builder.open_sink_or_fallback())
                    .map_err(|error| error.to_string())?;
                // The output is replaced whenever the device changes, which
                // rodio would otherwise report on stderr.
                sink.log_on_drop(false);
                &self.sink.insert(OpenSink { device_id, sink }).sink
            }
        };
        let player = Player::connect_new(sink.mixer());
        player.pause();
        // Seeking the buffer before it plays is immediate: it only moves its
        // position, and copies of it share their samples.
        let mut source = audio.clone();
        source.try_seek(start).map_err(|error| error.to_string())?;
        player.append(source);
        Ok(player)
    }
}
