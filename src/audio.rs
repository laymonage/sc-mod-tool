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
    /// Kept to load the segment again once it has stopped.
    audio: DecodedAudio,
}

impl ActivePlayback {
    /// How far playback has got, never past the end.
    pub fn position(&self) -> Duration {
        self.sink.get_pos().min(self.duration)
    }

    pub fn seek(&mut self, position: Duration) -> Result<(), String> {
        self.stopped = false;
        self.sink
            .try_seek(position.min(self.duration))
            .map_err(|error| error.to_string())
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
        let sink = self.load(&audio)?;
        sink.play();
        let frames = audio.samples.len() / usize::from(audio.channels.max(1));
        let duration = Duration::from_secs_f64(frames as f64 / f64::from(audio.sample_rate.max(1)));
        Ok(ActivePlayback {
            sink,
            segment_id,
            duration,
            stopped: false,
            audio,
        })
    }

    /// Stops playback, keeping the segment loaded at its start so it can be
    /// played again. Rodio can't rewind a sound that has ended, so the segment
    /// is loaded again.
    pub fn stop(&mut self, playback: &mut ActivePlayback) -> Result<(), String> {
        playback.sink = self.load(&playback.audio)?;
        playback.stopped = true;
        Ok(())
    }

    /// A paused sink with the audio queued at its start.
    fn load(&mut self, audio: &DecodedAudio) -> Result<Sink, String> {
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
        sink.append(SamplesBuffer::new(
            audio.channels,
            audio.sample_rate,
            audio.samples.clone(),
        ));
        Ok(sink)
    }
}
