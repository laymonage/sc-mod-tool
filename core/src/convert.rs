//! Converting any audio file into samples a segment can be encoded from,
//! using an `ffmpeg` executable.

use std::env;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::Error;

/// Sources longer than this are refused, so that choosing a film by mistake
/// doesn't decode hours of audio.
const MAX_SOURCE_SECONDS: u32 = 600;
/// `atempo` drops its last partial window, so this much silence, in seconds,
/// goes after the audio being stretched and is trimmed off again.
const STRETCH_LEAD_OUT_SECONDS: f64 = 0.25;

/// An `ffmpeg` executable that answered `-version`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ffmpeg {
    path: PathBuf,
}

/// How a source is made to fit a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitMode {
    /// Cut the source at the segment's length, or pad it with silence.
    TrimPad,
    /// Speed the source up or slow it down to the segment's length, keeping
    /// its pitch.
    Stretch,
    /// Keep the source's own length.
    KeepLength,
}

/// Which part of the source to use, and how to fit it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FitOptions {
    pub mode: FitMode,
    /// The first sample used, at the segment's rate.
    pub start: usize,
    /// The sample after the last one used, or `None` for the end.
    pub end: Option<usize>,
}

impl Default for FitOptions {
    fn default() -> Self {
        Self {
            mode: FitMode::TrimPad,
            start: 0,
            end: None,
        }
    }
}

impl Ffmpeg {
    /// Finds `ffmpeg`: at `configured` if it's given, otherwise beside this
    /// program, then on the `PATH`.
    pub fn locate(configured: Option<&Path>) -> Result<Self, Error> {
        if let Some(path) = configured {
            return Self::probe(path).ok_or_else(|| {
                format!(
                    "{} isn't an FFmpeg program that can be run.",
                    path.display()
                )
                .into()
            });
        }
        candidates()
            .into_iter()
            .filter(|path| path.is_file())
            .find_map(|path| Self::probe(&path))
            .ok_or_else(|| {
                "FFmpeg wasn't found. Install it and add it to the PATH, or choose where it is."
                    .into()
            })
    }

    /// `path` as an `Ffmpeg`, if it runs and says it's FFmpeg.
    fn probe(path: &Path) -> Option<Self> {
        let ffmpeg = Self {
            path: path.to_owned(),
        };
        let output = ffmpeg
            .run(
                "start",
                [OsString::from("-version")],
                None,
                &AtomicBool::new(false),
            )
            .ok()?;
        output.starts_with(b"ffmpeg version").then_some(ffmpeg)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Decodes `input` to mono samples at `sample_rate`.
    pub fn decode(
        &self,
        input: &Path,
        sample_rate: u32,
        cancelled: &AtomicBool,
    ) -> Result<Vec<i16>, Error> {
        let mut source = OsString::from("file:");
        source.push(input.as_os_str());
        let mut arguments = os_strings(&["-nostdin", "-protocol_whitelist", "file", "-i"]);
        arguments.push(source);
        // A little past the limit, to tell a source that's too long from one
        // that's just long enough.
        let limit = (MAX_SOURCE_SECONDS + 1).to_string();
        arguments.extend(os_strings(&["-t", &limit]));
        arguments.extend(arguments_for_output(sample_rate));
        let output = self.run("read the audio", arguments, None, cancelled)?;
        let samples = samples_from_bytes(&output);
        if samples.len() > MAX_SOURCE_SECONDS as usize * sample_rate as usize {
            return Err(format!(
                "The audio is longer than {} minutes.",
                MAX_SOURCE_SECONDS / 60
            )
            .into());
        }
        if samples.is_empty() {
            return Err("The file has no audio.".into());
        }
        Ok(samples)
    }

    /// Changes the length of `samples` by `tempo`, keeping their pitch, with
    /// FFmpeg's `atempo`. The result is only roughly `samples.len() / tempo`
    /// long.
    fn stretch(
        &self,
        samples: &[i16],
        sample_rate: u32,
        tempo: f64,
        cancelled: &AtomicBool,
    ) -> Result<Vec<i16>, Error> {
        let rate = sample_rate.to_string();
        let mut arguments = os_strings(&["-f", "s16le", "-ar", &rate, "-ac", "1", "-i", "-"]);
        arguments.extend(os_strings(&["-af", &atempo_filter(tempo)]));
        arguments.extend(arguments_for_output(sample_rate));
        let lead_out = (f64::from(sample_rate) * STRETCH_LEAD_OUT_SECONDS) as usize;
        let mut input = Vec::with_capacity((samples.len() + lead_out) * 2);
        for sample in samples {
            input.extend_from_slice(&sample.to_le_bytes());
        }
        input.resize(input.len() + lead_out * 2, 0);
        let output = self.run("stretch the audio", arguments, Some(input), cancelled)?;
        Ok(samples_from_bytes(&output))
    }

    /// Runs FFmpeg with `arguments`, giving it `input`, and returns what it
    /// writes. `action` completes "FFmpeg couldn't …" in errors.
    fn run(
        &self,
        action: &str,
        arguments: impl IntoIterator<Item = OsString>,
        input: Option<Vec<u8>>,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        let mut command = Command::new(&self.path);
        command
            .args(["-hide_banner", "-loglevel", "error"])
            .args(arguments)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console_window(&mut command);
        let mut child = command.spawn().map_err(|error| {
            format!("Could not start FFmpeg at {}: {error}", self.path.display())
        })?;

        // FFmpeg reads and writes at the same time, so each pipe needs its
        // own thread.
        let writer = input.and_then(|bytes| {
            let mut stdin = child.stdin.take()?;
            // FFmpeg may stop reading early, such as when it fails.
            Some(thread::spawn(move || {
                let _ = stdin.write_all(&bytes);
            }))
        });
        let stdout = child.stdout.take().map(read_all);
        let stderr = child.stderr.take().map(read_all);

        // If waiting fails, such as on a cancel, the threads are left to
        // finish on their own: a launcher, such as a Chocolatey shim, can be
        // killed while the FFmpeg it started keeps the pipes open.
        let status = wait(&mut child, cancelled)?;
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        let output = stdout.map(join_reader).unwrap_or_default();
        let messages = stderr.map(join_reader).unwrap_or_default();
        if !status.success() {
            let messages = String::from_utf8_lossy(&messages);
            let reason = failure_reason(&messages).unwrap_or("it gave no reason");
            return Err(format!("FFmpeg couldn't {action}: {reason}").into());
        }
        Ok(output)
    }
}

/// Waits for `child`, killing it if `cancelled` is set.
fn wait(child: &mut std::process::Child, cancelled: &AtomicBool) -> Result<ExitStatus, Error> {
    loop {
        if cancelled.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Could not wait for FFmpeg: {error}").into());
            }
        }
    }
}

fn read_all(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

fn join_reader(reader: thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    reader.join().unwrap_or_default()
}

#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

/// Why FFmpeg failed, from the errors it wrote. Its last lines only sum up,
/// such as "Error opening output files: Invalid argument", so the last message
/// from one of its parts, such as "[aist#0:0/adpcm_ima_qt @ 0x…] Decoding
/// requested, but no decoder found for: adpcm_ima_qt", is used without its
/// prefix, if there is one.
fn failure_reason(messages: &str) -> Option<&str> {
    let lines = messages
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let last = lines.clone().next_back()?;
    let from_part = lines.rev().find_map(|line| {
        let (prefix, message) = line.strip_prefix('[')?.split_once("] ")?;
        prefix.contains(" @ ").then_some(message.trim())
    });
    Some(from_part.unwrap_or(last))
}

fn os_strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

/// The arguments for writing mono 16-bit samples at `sample_rate` to the
/// standard output.
fn arguments_for_output(sample_rate: u32) -> Vec<OsString> {
    let rate = sample_rate.to_string();
    os_strings(&[
        "-vn",
        "-sn",
        "-dn",
        "-ac",
        "1",
        "-ar",
        &rate,
        "-c:a",
        "pcm_s16le",
        "-f",
        "s16le",
        // The standard output.
        "-",
    ])
}

fn samples_from_bytes(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| i16::from_le_bytes(pair))
        .collect()
}

/// Where to look for `ffmpeg` when no path is configured.
fn candidates() -> Vec<PathBuf> {
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let mut directories = Vec::new();
    if let Some(directory) = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_owned))
    {
        directories.push(directory);
    }
    if let Some(path) = env::var_os("PATH") {
        directories.extend(env::split_paths(&path));
    }
    // Apps opened from the Finder don't get the shell's PATH, so Homebrew's
    // folders are checked too, with its `ffmpeg-full`, which isn't linked
    // into them.
    if cfg!(target_os = "macos") {
        directories.extend(
            [
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/opt/homebrew/opt/ffmpeg-full/bin",
                "/usr/local/opt/ffmpeg-full/bin",
            ]
            .map(PathBuf::from),
        );
    }
    directories
        .into_iter()
        .map(|directory| directory.join(name))
        .collect()
}

/// A chain of `atempo` filters that changes speed by `tempo` overall. Each
/// stays between 0.5 and 2, which every FFmpeg version accepts.
fn atempo_filter(mut tempo: f64) -> String {
    debug_assert!(tempo.is_finite() && tempo > 0.0, "tempo {tempo}");
    let mut filters = Vec::new();
    while tempo > 2.0 {
        filters.push("atempo=2".to_owned());
        tempo /= 2.0;
    }
    while tempo < 0.5 {
        filters.push("atempo=0.5".to_owned());
        tempo /= 0.5;
    }
    filters.push(format!("atempo={tempo}"));
    filters.join(",")
}

/// How much faster than the source a stretched replacement plays: above 1 it's
/// sped up, below 1 slowed down.
pub fn stretch_factor(source_length: usize, target_length: usize) -> f64 {
    source_length as f64 / target_length as f64
}

/// The part of `source` that `options` choose.
fn selection<'a>(source: &'a [i16], options: &FitOptions) -> Result<&'a [i16], Error> {
    let end = options.end.unwrap_or(source.len()).min(source.len());
    source
        .get(options.start..end)
        .filter(|selected| !selected.is_empty())
        .ok_or_else(|| "The chosen part of the audio is empty.".into())
}

/// Makes `source`, decoded at `sample_rate`, fit a segment of `target_length`
/// samples as `options` say. Only [`FitMode::Stretch`] runs `ffmpeg`.
pub fn fit(
    ffmpeg: &Ffmpeg,
    source: &[i16],
    sample_rate: u32,
    target_length: usize,
    options: &FitOptions,
    cancelled: &AtomicBool,
) -> Result<Vec<i16>, Error> {
    let selected = selection(source, options)?;
    if target_length == 0 && options.mode != FitMode::KeepLength {
        return Err("There's no length to fit the audio to.".into());
    }
    let mut samples = match options.mode {
        FitMode::KeepLength => return Ok(selected.to_vec()),
        FitMode::TrimPad => selected.to_vec(),
        FitMode::Stretch if selected.len() == target_length => selected.to_vec(),
        FitMode::Stretch => {
            let tempo = stretch_factor(selected.len(), target_length);
            ffmpeg.stretch(selected, sample_rate, tempo, cancelled)?
        }
    };
    samples.resize(target_length, 0);
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::DecodedAudio;

    fn unused_ffmpeg() -> Ffmpeg {
        Ffmpeg {
            path: PathBuf::from("ffmpeg that isn't run"),
        }
    }

    /// FFmpeg from the `PATH`, or `None` to skip a test where it's missing,
    /// unless `SC_REQUIRE_FFMPEG` is set, as in CI.
    fn installed_ffmpeg() -> Option<Ffmpeg> {
        let found = Ffmpeg::locate(None);
        if std::env::var_os("SC_REQUIRE_FFMPEG").is_some() {
            return Some(found.unwrap());
        }
        if found.is_err() {
            eprintln!("Skipped: FFmpeg isn't installed.");
        }
        found.ok()
    }

    fn ramp(length: usize) -> Vec<i16> {
        (0..length).map(|index| index as i16).collect()
    }

    #[test]
    fn atempo_chains_stay_in_range() {
        assert_eq!(atempo_filter(1.25), "atempo=1.25");
        assert_eq!(atempo_filter(5.0), "atempo=2,atempo=2,atempo=1.25");
        assert_eq!(atempo_filter(0.2), "atempo=0.5,atempo=0.5,atempo=0.8");
        assert_eq!(atempo_filter(2.0), "atempo=2");
    }

    #[test]
    fn trimming_and_padding_reach_the_target_exactly() {
        let cancelled = AtomicBool::new(false);
        let ffmpeg = unused_ffmpeg();
        let options = FitOptions::default();
        assert_eq!(
            fit(&ffmpeg, &ramp(10), 36000, 4, &options, &cancelled).unwrap(),
            [0, 1, 2, 3]
        );
        assert_eq!(
            fit(&ffmpeg, &ramp(3), 36000, 5, &options, &cancelled).unwrap(),
            [0, 1, 2, 0, 0]
        );

        let selected = FitOptions {
            start: 2,
            end: Some(5),
            ..options
        };
        assert_eq!(
            fit(&ffmpeg, &ramp(10), 36000, 4, &selected, &cancelled).unwrap(),
            [2, 3, 4, 0]
        );
    }

    #[test]
    fn an_empty_target_is_refused() {
        let cancelled = AtomicBool::new(false);
        for mode in [FitMode::TrimPad, FitMode::Stretch] {
            let options = FitOptions {
                mode,
                ..FitOptions::default()
            };
            assert!(fit(&unused_ffmpeg(), &ramp(4), 36000, 0, &options, &cancelled).is_err());
        }
    }

    #[test]
    fn keeping_the_length_only_selects() {
        let cancelled = AtomicBool::new(false);
        let options = FitOptions {
            mode: FitMode::KeepLength,
            start: 1,
            end: Some(100),
        };
        assert_eq!(
            fit(&unused_ffmpeg(), &ramp(4), 36000, 2, &options, &cancelled).unwrap(),
            [1, 2, 3]
        );
    }

    #[test]
    fn empty_selections_are_refused() {
        let cancelled = AtomicBool::new(false);
        for (start, end) in [(4, None), (2, Some(2)), (3, Some(1))] {
            let options = FitOptions {
                start,
                end,
                ..FitOptions::default()
            };
            assert!(fit(&unused_ffmpeg(), &ramp(4), 36000, 2, &options, &cancelled).is_err());
        }
    }

    #[test]
    fn stretching_to_the_same_length_doesnt_run_ffmpeg() {
        let options = FitOptions {
            mode: FitMode::Stretch,
            ..FitOptions::default()
        };
        let cancelled = AtomicBool::new(false);
        assert_eq!(
            fit(&unused_ffmpeg(), &ramp(3), 36000, 3, &options, &cancelled).unwrap(),
            [0, 1, 2]
        );
    }

    #[test]
    fn a_configured_path_must_be_ffmpeg() {
        let folder = tempfile::tempdir().unwrap();
        let missing = folder.path().join("ffmpeg");
        assert!(Ffmpeg::locate(Some(&missing)).is_err());
        assert!(Ffmpeg::locate(Some(folder.path())).is_err());
    }

    /// Writes a stereo 44.1 kHz WAV file of a tone, `seconds` long.
    fn write_tone(path: &Path, seconds: usize) {
        let frames = 44100 * seconds;
        let samples = (0..frames)
            .flat_map(|index| {
                let value = ((index as f64 * 440.0 * std::f64::consts::TAU / 44100.0).sin()
                    * 8000.0) as i16;
                [value, value]
            })
            .collect();
        let audio = DecodedAudio {
            samples,
            sample_rate: 44100,
            channels: 2,
        };
        audio
            .write_wav(std::fs::File::create(path).unwrap())
            .unwrap();
    }

    #[test]
    fn decodes_files_to_mono_at_the_segments_rate() {
        let Some(ffmpeg) = installed_ffmpeg() else {
            return;
        };
        let folder = tempfile::tempdir().unwrap();
        // On Unix, a name FFmpeg would otherwise take for a protocol.
        let name = if cfg!(windows) {
            "tone.wav"
        } else {
            "concat:tone.wav"
        };
        let path = folder.path().join(name);
        write_tone(&path, 2);
        let cancelled = AtomicBool::new(false);
        let samples = ffmpeg.decode(&path, 36000, &cancelled).unwrap();
        assert_eq!(samples.len(), 72000);
        let peak = samples
            .iter()
            .map(|sample| sample.unsigned_abs())
            .max()
            .unwrap();
        assert!((7500..=8500).contains(&peak), "peak {peak}");
    }

    #[test]
    fn stretches_to_the_target_without_losing_the_end() {
        let Some(ffmpeg) = installed_ffmpeg() else {
            return;
        };
        let cancelled = AtomicBool::new(false);
        let source = vec![6000_i16; 36000];
        for target in [24000, 36000 * 3 / 2] {
            let tempo = stretch_factor(source.len(), target);
            let stretched = ffmpeg.stretch(&source, 36000, tempo, &cancelled).unwrap();
            assert!(stretched.len() >= target, "{} < {target}", stretched.len());
            let options = FitOptions {
                mode: FitMode::Stretch,
                ..FitOptions::default()
            };
            let fitted = fit(&ffmpeg, &source, 36000, target, &options, &cancelled).unwrap();
            assert_eq!(fitted.len(), target);
            // The end of the source is still there, rather than the silence
            // that pads a stretch FFmpeg cut short.
            let tail = &fitted[target - 1500..target - 500];
            assert!(
                tail.iter().all(|sample| sample.unsigned_abs() > 3000),
                "{tail:?}"
            );
        }
    }

    #[test]
    fn failures_are_explained_by_ffmpegs_parts() {
        let unsupported = "\
[aist#0:0/adpcm_ima_qt @ 0x7995010180] Decoding requested, but no decoder found for: adpcm_ima_qt
Error opening output file -.
Error opening output files: Invalid argument
";
        assert_eq!(
            failure_reason(unsupported),
            Some("Decoding requested, but no decoder found for: adpcm_ima_qt")
        );
        let not_audio = "\
[in#0 @ 0x7a26c14000] Error opening input: Invalid data found when processing input
Error opening input file file:/tmp/t.xyz.
Error opening input files: Invalid data found when processing input
";
        assert_eq!(
            failure_reason(not_audio),
            Some("Error opening input: Invalid data found when processing input")
        );
        assert_eq!(
            failure_reason("file:/tmp/missing.wav: No such file or directory\n\n"),
            Some("file:/tmp/missing.wav: No such file or directory")
        );
        assert_eq!(failure_reason(" \n"), None);
    }

    #[test]
    fn reports_files_ffmpeg_cannot_read() {
        let Some(ffmpeg) = installed_ffmpeg() else {
            return;
        };
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("not audio.wav");
        std::fs::write(&path, b"not audio").unwrap();
        let error = ffmpeg
            .decode(&path, 36000, &AtomicBool::new(false))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("FFmpeg couldn't read the audio: "),
            "{error}"
        );
    }

    #[test]
    fn cancelling_stops_ffmpeg() {
        let Some(ffmpeg) = installed_ffmpeg() else {
            return;
        };
        let cancelled = AtomicBool::new(true);
        let source = vec![0_i16; 36000 * 60];
        assert_eq!(
            ffmpeg.stretch(&source, 36000, 0.5, &cancelled),
            Err(Error::Cancelled)
        );
    }
}
