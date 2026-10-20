use std::time::Duration;

use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::{OutputStream, OutputStreamHandle, Sink, buffer::SamplesBuffer, cpal};
use sc_mod_tool_core::decoder::DecodedAudio;

/// A segment that is playing, paused, or stopped.
pub struct ActivePlayback {
    pub sink: Sink,
    pub segment_id: String,
    pub duration: Duration,
    /// Whether playback has stopped, at the end or by the user, leaving the
    /// segment loaded at its start.
    pub stopped: bool,
    /// Where in the segment the sink's audio starts, after a seek.
    start: Duration,
    /// Kept to load the segment again once it has stopped, or from where it
    /// was sought.
    audio: DecodedAudio,
}

impl ActivePlayback {
    /// How far playback has got, never past the end.
    pub fn position(&self) -> Duration {
        (self.start + self.sink.get_pos()).min(self.duration)
    }

    pub fn toggle_pause(&mut self) {
        if self.sink.is_paused() {
            self.stopped = false;
            self.sink.play();
        } else {
            self.sink.pause();
        }
    }
}

/// The audio output stream, kept open between playbacks because opening a
/// device is slow on some platforms.
#[derive(Default)]
pub struct AudioOutput {
    stream: Option<OpenStream>,
}

struct OpenStream {
    /// The default output device when the stream was opened, so the stream can
    /// be reopened if the user switches devices.
    device_name: Option<String>,
    _stream: OutputStream,
    handle: OutputStreamHandle,
}

impl AudioOutput {
    pub fn play(
        &mut self,
        audio: DecodedAudio,
        segment_id: String,
    ) -> Result<ActivePlayback, String> {
        let sink = self.load(&audio, Duration::ZERO)?;
        sink.play();
        let frames = audio.samples.len() / usize::from(audio.channels.max(1));
        let duration = Duration::from_secs_f64(frames as f64 / f64::from(audio.sample_rate.max(1)));
        Ok(ActivePlayback {
            sink,
            segment_id,
            duration,
            stopped: false,
            start: Duration::ZERO,
            audio,
        })
    }

    /// Stops playback, keeping the segment loaded at its start so it can be
    /// played again. Rodio can't rewind a sound that has ended, so the segment
    /// is loaded again.
    pub fn stop(&mut self, playback: &mut ActivePlayback) -> Result<(), String> {
        playback.sink = self.load(&playback.audio, Duration::ZERO)?;
        playback.start = Duration::ZERO;
        playback.stopped = true;
        Ok(())
    }

    /// Moves playback to `position`, leaving it paused or playing as it was.
    ///
    /// The segment is loaded again from there instead of seeking the sink.
    /// Rodio's seek waits for the output stream to take it, and a stream
    /// whose device has gone away never does, which would freeze the app.
    /// Loading again also moves playback to a new default device.
    pub fn seek(
        &mut self,
        playback: &mut ActivePlayback,
        position: Duration,
    ) -> Result<(), String> {
        let position = position.min(playback.duration);
        let paused = playback.sink.is_paused();
        playback.sink = self.load(&playback.audio, position)?;
        playback.start = position;
        playback.stopped = false;
        if !paused {
            playback.sink.play();
        }
        Ok(())
    }

    /// A paused sink with the audio queued from `start`.
    fn load(&mut self, audio: &DecodedAudio, start: Duration) -> Result<Sink, String> {
        let device_name = cpal::default_host()
            .default_output_device()
            .and_then(|device| device.name().ok());
        let stream = match &self.stream {
            Some(stream) if stream.device_name == device_name => stream,
            _ => {
                // Close the old stream before opening the new device.
                self.stream = None;
                let (stream, handle) =
                    OutputStream::try_default().map_err(|error| error.to_string())?;
                self.stream.insert(OpenStream {
                    device_name,
                    _stream: stream,
                    handle,
                })
            }
        };
        let sink = Sink::try_new(&stream.handle).map_err(|error| error.to_string())?;
        sink.pause();
        let channels = usize::from(audio.channels.max(1));
        let frame = (start.as_secs_f64() * f64::from(audio.sample_rate)) as usize;
        let first = frame.saturating_mul(channels).min(audio.samples.len());
        sink.append(SamplesBuffer::new(
            audio.channels,
            audio.sample_rate,
            audio.samples[first..].to_vec(),
        ));
        Ok(sink)
    }
}
