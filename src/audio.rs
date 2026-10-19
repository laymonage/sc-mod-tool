use std::time::Duration;

use rodio::cpal::traits::{DeviceTrait, HostTrait};
use rodio::{OutputStream, OutputStreamHandle, Sink, buffer::SamplesBuffer, cpal};
use sc_mod_tool_core::decoder::DecodedAudio;

/// A segment that is currently playing.
pub struct ActivePlayback {
    pub sink: Sink,
    pub segment_id: String,
    pub duration: Duration,
}

impl ActivePlayback {
    /// How far playback has got, never past the end.
    pub fn position(&self) -> Duration {
        self.sink.get_pos().min(self.duration)
    }

    pub fn seek(&self, position: Duration) -> Result<(), String> {
        self.sink
            .try_seek(position.min(self.duration))
            .map_err(|error| error.to_string())
    }

    pub fn toggle_pause(&self) {
        if self.sink.is_paused() {
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
        let frames = audio.samples.len() / usize::from(audio.channels.max(1));
        let duration = Duration::from_secs_f64(frames as f64 / f64::from(audio.sample_rate.max(1)));
        sink.append(SamplesBuffer::new(
            audio.channels,
            audio.sample_rate,
            audio.samples,
        ));
        Ok(ActivePlayback {
            sink,
            segment_id,
            duration,
        })
    }
}
