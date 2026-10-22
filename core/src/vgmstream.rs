//! Bindings to vgmstream's library, which reads the game's sound maps
//! (`MAPS.SM0`, `MAPS.LM0`) and the streams their sounds point at.
//!
//! vgmstream opens files through callbacks here, by names relative to
//! `Sounds`, so it can't read outside that folder.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use memmap2::Mmap;

use crate::Error;

/// `libstreamfile_t`
#[repr(C)]
struct LibStreamFile {
    user_data: *mut c_void,
    read: unsafe extern "C" fn(*mut c_void, *mut u8, i64, c_int) -> c_int,
    get_size: unsafe extern "C" fn(*mut c_void) -> i64,
    get_name: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    open: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut LibStreamFile,
    close: unsafe extern "C" fn(*mut LibStreamFile),
}

/// `libvgmstream_format_t`
#[repr(C)]
struct Format {
    channels: c_int,
    sample_rate: c_int,
    sample_format: c_int,
    sample_size: c_int,
    channel_layout: u32,
    subsong_index: c_int,
    subsong_count: c_int,
    input_channels: c_int,
    stream_samples: i64,
    loop_start: i64,
    loop_end: i64,
    loop_flag: bool,
    play_forever: bool,
    play_samples: i64,
    stream_bitrate: c_int,
    codec_name: [c_char; 128],
    layout_name: [c_char; 128],
    meta_name: [c_char; 128],
    stream_name: [c_char; 256],
    format_id: c_int,
}

/// `libvgmstream_decoder_t`
#[repr(C)]
struct DecoderState {
    buf: *mut c_void,
    buf_samples: c_int,
    buf_bytes: c_int,
    done: bool,
}

/// `libvgmstream_t`
#[repr(C)]
struct LibVgmstream {
    private: *mut c_void,
    format: *const Format,
    decoder: *mut DecoderState,
}

/// `libvgmstream_config_t`
#[repr(C)]
#[derive(Default)]
struct Config {
    disable_config_override: bool,
    allow_play_forever: bool,
    play_forever: bool,
    ignore_loop: bool,
    force_loop: bool,
    really_force_loop: bool,
    ignore_fade: bool,
    loop_count: f64,
    fade_time: f64,
    fade_delay: f64,
    stereo_track: c_int,
    auto_downmix_channels: c_int,
    force_sfmt: c_int,
}

/// `LIBVGMSTREAM_SFMT_PCM16`
const SAMPLE_FORMAT_PCM16: c_int = 1;

unsafe extern "C" {
    fn libvgmstream_init() -> *mut LibVgmstream;
    fn libvgmstream_free(lib: *mut LibVgmstream);
    fn libvgmstream_setup(lib: *mut LibVgmstream, config: *mut Config);
    fn libvgmstream_open_stream(
        lib: *mut LibVgmstream,
        libsf: *mut LibStreamFile,
        subsong: c_int,
    ) -> c_int;
    fn libvgmstream_render(lib: *mut LibVgmstream) -> c_int;
    fn libstreamfile_open_buffered(inner: *mut LibStreamFile) -> *mut LibStreamFile;
    fn libstreamfile_close(libsf: *mut LibStreamFile);
}

/// A sound vgmstream opened.
#[derive(Clone, Debug)]
pub(crate) struct SoundInfo {
    /// How many sounds the file has, numbered from 1.
    pub(crate) subsong_count: u32,
    pub(crate) channels: u32,
    pub(crate) sample_rate: u32,
    pub(crate) sample_count: u32,
    pub(crate) codec: String,
    /// vgmstream's description of where the sound is, such as
    /// `1_1_0Tbilisi/0012/01-00130086/TAWCHI02.wav` for a sound map.
    pub(crate) stream_name: String,
}

/// Interleaved 16-bit samples.
pub(crate) struct DecodedSound {
    pub(crate) samples: Vec<i16>,
    pub(crate) channels: u32,
    pub(crate) sample_rate: u32,
}

/// A vgmstream instance, which opens one sound at a time and plays it once,
/// without looping.
pub(crate) struct Vgmstream {
    lib: NonNull<LibVgmstream>,
    /// A file this instance reads through a [`SharedFile`], whenever it's
    /// opened by that file's name.
    shared: Option<Arc<SharedFile>>,
}

impl Vgmstream {
    pub(crate) fn new() -> Result<Self, String> {
        let lib = NonNull::new(unsafe { libvgmstream_init() })
            .ok_or_else(|| "Could not start vgmstream.".to_owned())?;
        Ok(Self { lib, shared: None })
    }

    /// Reads `file` through the copy other instances share, instead of
    /// opening it again.
    pub(crate) fn sharing(mut self, file: Arc<SharedFile>) -> Self {
        self.shared = Some(file);
        self
    }

    /// Opens sound `subsong` (from 1) of the file at `relative_path` under
    /// `sounds`, which must be canonical.
    pub(crate) fn open(
        &mut self,
        sounds: &Path,
        relative_path: &str,
        subsong: u32,
    ) -> Result<SoundInfo, String> {
        let subsong =
            c_int::try_from(subsong).map_err(|_| format!("Sound {subsong} is out of range."))?;
        // vgmstream makes many small reads, so they're buffered. Files it
        // opens from this one are buffered too.
        let file = SoundFile::open(sounds, relative_path, self.shared.as_ref())
            .map(|file| unsafe { libstreamfile_open_buffered(file) })
            .filter(|file| !file.is_null())
            .ok_or_else(|| format!("Could not open {relative_path}."))?;

        let mut config = Config {
            ignore_loop: true,
            force_sfmt: SAMPLE_FORMAT_PCM16,
            ..Config::default()
        };
        let lib = self.lib.as_ptr();
        // vgmstream reopens the file by name as it needs it, so it's closed
        // once the sound is open.
        let opened = unsafe {
            libvgmstream_setup(lib, &mut config);
            let opened = libvgmstream_open_stream(lib, file, subsong);
            libstreamfile_close(file);
            opened
        };
        if opened < 0 {
            return Err(format!(
                "vgmstream could not open sound {subsong} of {relative_path}."
            ));
        }

        let format = unsafe { &*(*lib).format };
        let positive = |value: c_int| u32::try_from(value).ok().filter(|value| *value > 0);
        let (Some(channels), Some(sample_rate)) =
            (positive(format.channels), positive(format.sample_rate))
        else {
            return Err(format!(
                "vgmstream returned an invalid format for sound {subsong} of {relative_path}."
            ));
        };
        Ok(SoundInfo {
            subsong_count: u32::try_from(format.subsong_count).unwrap_or(0),
            channels,
            sample_rate,
            sample_count: u32::try_from(format.play_samples).unwrap_or(0),
            codec: c_string(&format.codec_name),
            stream_name: c_string(&format.stream_name),
        })
    }

    /// Decodes the open sound to its end.
    pub(crate) fn decode(&mut self, cancelled: &AtomicBool) -> Result<DecodedSound, Error> {
        let lib = self.lib.as_ptr();
        let format = unsafe { &*(*lib).format };
        let channels = u32::try_from(format.channels).unwrap_or(0);
        let sample_rate = u32::try_from(format.sample_rate).unwrap_or(0);
        if channels == 0 || sample_rate == 0 || format.sample_format != SAMPLE_FORMAT_PCM16 {
            return Err("vgmstream returned an unsupported sample format.".into());
        }

        let expected = usize::try_from(format.play_samples).unwrap_or(0) * channels as usize;
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(expected)
            .map_err(|error| format!("Could not allocate the decoded sound: {error}"))?;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if unsafe { libvgmstream_render(lib) } < 0 {
                return Err("vgmstream could not decode the sound.".into());
            }
            let decoder = unsafe { &*(*lib).decoder };
            let count = usize::try_from(decoder.buf_samples).unwrap_or(0) * channels as usize;
            if count > 0 && !decoder.buf.is_null() {
                let buffer =
                    unsafe { std::slice::from_raw_parts(decoder.buf.cast::<i16>(), count) };
                samples.extend_from_slice(buffer);
            }
            if decoder.done {
                break;
            }
        }
        Ok(DecodedSound {
            samples,
            channels,
            sample_rate,
        })
    }
}

impl Drop for Vgmstream {
    fn drop(&mut self) {
        unsafe { libvgmstream_free(self.lib.as_ptr()) };
    }
}

fn c_string(characters: &[c_char]) -> String {
    let bytes: Vec<u8> = characters
        .iter()
        .take_while(|character| **character != 0)
        // `c_char` is `i8` on some targets and `u8` on others.
        .map(|character| u8::from_ne_bytes(character.to_ne_bytes()))
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A file under `Sounds` that vgmstream reads. `api` comes first, so vgmstream's
/// pointer to it is also a pointer to the whole struct.
#[repr(C)]
struct SoundFile {
    api: LibStreamFile,
    sounds: PathBuf,
    /// The path relative to `Sounds`, with `/` separators.
    name: CString,
    source: Source,
    size: i64,
    /// The shared file to read through when vgmstream opens it from this
    /// one.
    shared: Option<Arc<SharedFile>>,
}

/// Where a [`SoundFile`] reads from.
enum Source {
    File(File),
    Shared(Arc<SharedFile>),
}

impl SoundFile {
    /// Opens `relative_path` under `sounds`, or `shared` when that's the
    /// file, returning a pointer vgmstream owns.
    fn open(
        sounds: &Path,
        relative_path: &str,
        shared: Option<&Arc<SharedFile>>,
    ) -> Option<*mut LibStreamFile> {
        // vgmstream reopens files by the name this gives it, so a shared
        // file is recognized by its name without looking for it again.
        let (name, source, size) = match shared.filter(|shared| shared.is_named(relative_path)) {
            Some(shared) => (
                shared.name.clone(),
                Source::Shared(Arc::clone(shared)),
                shared.size,
            ),
            None => {
                let (path, name) = resolve_sound_path(sounds, relative_path)?;
                let file = File::open(&path).ok()?;
                let size = i64::try_from(file.metadata().ok()?.len()).ok()?;
                (CString::new(name).ok()?, Source::File(file), size)
            }
        };
        let sound_file = Box::new(SoundFile {
            api: LibStreamFile {
                user_data: std::ptr::null_mut(),
                read: sound_file_read,
                get_size: sound_file_get_size,
                get_name: sound_file_get_name,
                open: sound_file_open,
                close: sound_file_close,
            },
            sounds: sounds.to_path_buf(),
            name,
            source,
            size,
            shared: shared.cloned(),
        });
        let pointer = Box::into_raw(sound_file);
        unsafe { (*pointer).api.user_data = pointer.cast() };
        Some(pointer.cast())
    }

    fn read_at(&mut self, offset: u64, destination: &mut [u8]) -> usize {
        match &mut self.source {
            Source::File(file) => read_file_at(file, offset, destination),
            Source::Shared(shared) => shared.read_at(offset, destination),
        }
    }
}

/// Reads into `destination` from `offset`, returning how many bytes were read,
/// which is fewer at the end of the file or on an error.
fn read_file_at(file: &mut File, offset: u64, destination: &mut [u8]) -> usize {
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return 0;
    }
    let mut read = 0;
    while read < destination.len() {
        match file.read(&mut destination[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    read
}

/// A file several vgmstream instances read at once, such as a sound map whose
/// sounds are being listed, which is mapped into memory once for all of them.
///
/// vgmstream reads a map's whole index again for each sound it opens, through
/// a new 32 KiB buffer each time. Listing all of `MAPS.SM0`'s sounds that way
/// reads about 25 GB from the file, all from the same 4 MiB, which the
/// operating system keeps in memory for the mapping.
pub(crate) struct SharedFile {
    /// The path relative to `Sounds`, with `/` separators.
    name: CString,
    size: i64,
    map: Mmap,
}

impl SharedFile {
    /// Opens `relative_path` under `sounds`, which must be canonical.
    pub(crate) fn open(sounds: &Path, relative_path: &str) -> Result<Arc<Self>, String> {
        let could_not_open = || format!("Could not open {relative_path}.");
        let (path, name) = resolve_sound_path(sounds, relative_path).ok_or_else(could_not_open)?;
        let file = File::open(&path).map_err(|_| could_not_open())?;
        // SAFETY: the mapping is only sound while nothing changes the file.
        // The game's files don't change while their sounds are listed, which
        // is as long as a shared file lives. If one were truncated meanwhile,
        // reading past its new end would crash the app.
        let map = unsafe { Mmap::map(&file) }.map_err(|_| could_not_open())?;
        Ok(Arc::new(Self {
            name: CString::new(name).map_err(|_| could_not_open())?,
            size: i64::try_from(map.len()).map_err(|_| could_not_open())?,
            map,
        }))
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.map
    }

    fn is_named(&self, relative_path: &str) -> bool {
        self.name.to_str() == Ok(relative_path)
    }

    fn read_at(&self, offset: u64, destination: &mut [u8]) -> usize {
        let start =
            usize::try_from(offset).map_or(self.map.len(), |offset| offset.min(self.map.len()));
        let end = start.saturating_add(destination.len()).min(self.map.len());
        destination[..end - start].copy_from_slice(&self.map[start..end]);
        end - start
    }
}

unsafe extern "C" fn sound_file_read(
    user_data: *mut c_void,
    destination: *mut u8,
    offset: i64,
    length: c_int,
) -> c_int {
    let sound_file = unsafe { &mut *user_data.cast::<SoundFile>() };
    let (Ok(offset), Ok(length)) = (u64::try_from(offset), usize::try_from(length)) else {
        return 0;
    };
    if destination.is_null() {
        return 0;
    }
    let destination = unsafe { std::slice::from_raw_parts_mut(destination, length) };
    sound_file.read_at(offset, destination) as c_int
}

unsafe extern "C" fn sound_file_get_size(user_data: *mut c_void) -> i64 {
    unsafe { (*user_data.cast::<SoundFile>()).size }
}

unsafe extern "C" fn sound_file_get_name(user_data: *mut c_void) -> *const c_char {
    unsafe { (*user_data.cast::<SoundFile>()).name.as_ptr() }
}

unsafe extern "C" fn sound_file_open(
    user_data: *mut c_void,
    name: *const c_char,
) -> *mut LibStreamFile {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    let sound_file = unsafe { &*user_data.cast::<SoundFile>() };
    let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else {
        return std::ptr::null_mut();
    };
    SoundFile::open(&sound_file.sounds, name, sound_file.shared.as_ref())
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn sound_file_close(libsf: *mut LibStreamFile) {
    if !libsf.is_null() {
        drop(unsafe { Box::from_raw(libsf.cast::<SoundFile>()) });
    }
}

/// Finds `relative_path` under `sounds`, ignoring case as Windows does, and
/// returns its path and its name relative to `sounds`.
///
/// The extra maps' `MAPS.SM0` point at music that's in `Sounds/Common`, not
/// beside them, so a missing file is looked for there too.
fn resolve_sound_path(sounds: &Path, relative_path: &str) -> Option<(PathBuf, String)> {
    let mut parts = Vec::new();
    for part in relative_path.split(['/', '\\']) {
        let mut components = Path::new(part).components();
        match (components.next(), components.next()) {
            (None | Some(Component::CurDir), None) => {}
            (Some(Component::Normal(_)), None) => parts.push(part),
            _ => return None,
        }
    }
    let file_name = *parts.last()?;
    let path = find_path(sounds, &parts).or_else(|| find_path(sounds, &["Common", file_name]))?;
    let path = path.canonicalize().ok()?;
    if !path.starts_with(sounds) || !path.is_file() {
        return None;
    }
    let name = path
        .strip_prefix(sounds)
        .ok()?
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    Some((path, name))
}

/// Follows `parts` down from `directory`, matching each name exactly or, if
/// that's missing, ignoring case.
fn find_path(directory: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut path = directory.to_path_buf();
    for part in parts {
        let exact = path.join(part);
        path = if exact.exists() {
            exact
        } else {
            std::fs::read_dir(&path)
                .ok()?
                .filter_map(Result::ok)
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(part)
                })?
                .path()
        };
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn resolves_sound_paths_inside_sounds_ignoring_case() {
        let game = tempfile::tempdir().unwrap();
        let sounds = game.path().join("Sounds");
        fs::create_dir_all(sounds.join("ENGLISH")).unwrap();
        fs::create_dir_all(sounds.join("Common")).unwrap();
        fs::create_dir_all(sounds.join("1_6_1_1kolacell")).unwrap();
        fs::write(sounds.join("ENGLISH/MAPS.LM0"), [0]).unwrap();
        fs::write(sounds.join("Common/Music_Common.SS0"), [0]).unwrap();
        fs::write(game.path().join("outside.SS0"), [0]).unwrap();
        let sounds = sounds.canonicalize().unwrap();

        let name = |path| resolve_sound_path(&sounds, path).map(|(_, name)| name);
        assert_eq!(
            name("english/maps.lm0").as_deref(),
            Some("ENGLISH/MAPS.LM0")
        );
        assert_eq!(
            name("./ENGLISH\\MAPS.LM0").as_deref(),
            Some("ENGLISH/MAPS.LM0")
        );
        assert_eq!(
            name("1_6_1_1kolacell/Music_Common.SS0").as_deref(),
            Some("Common/Music_Common.SS0")
        );
        assert_eq!(name("../outside.SS0"), None);
        assert_eq!(name("ENGLISH/../../outside.SS0"), None);
        assert_eq!(name("ENGLISH"), None);
        assert_eq!(name("missing.SS0"), None);
    }

    #[test]
    fn reads_files_at_any_offset() {
        let game = tempfile::tempdir().unwrap();
        // vgmstream's buffer is 0x8000 bytes.
        let block = 0x8000;
        let bytes: Vec<u8> = (0..block * 2 + 10)
            .map(|index| (index % 251) as u8)
            .collect();
        fs::write(game.path().join("MAPS.SM0"), &bytes).unwrap();
        let sounds = game.path().canonicalize().unwrap();

        let shared = SharedFile::open(&sounds, "MAPS.SM0").unwrap();
        for shared in [None, Some(&shared)] {
            let file = SoundFile::open(&sounds, "MAPS.SM0", shared).unwrap();
            let file = unsafe { libstreamfile_open_buffered(file) };
            let read = |offset: usize, length: usize| {
                let mut destination = vec![0; length];
                let count = unsafe {
                    ((*file).read)(
                        (*file).user_data,
                        destination.as_mut_ptr(),
                        offset as i64,
                        length as c_int,
                    )
                };
                destination.truncate(count as usize);
                destination
            };
            for (offset, length) in [(0x1000, 16), (0, 16), (block - 4, 8), (5, 0x10000)] {
                assert_eq!(
                    read(offset, length),
                    bytes[offset..(offset + length).min(bytes.len())]
                );
            }
            assert!(read(bytes.len(), 4).is_empty());
            unsafe { libstreamfile_close(file) };
        }

        fs::write(game.path().join("EMPTY.SM0"), []).unwrap();
        let empty = SharedFile::open(&sounds, "EMPTY.SM0").unwrap();
        assert_eq!(empty.read_at(0, &mut [0; 4]), 0);
    }

    #[test]
    fn rejects_files_vgmstream_does_not_recognize() {
        let game = tempfile::tempdir().unwrap();
        let sounds = game.path().join("Sounds");
        fs::create_dir_all(&sounds).unwrap();
        fs::write(sounds.join("MAPS.SM0"), [0; 64]).unwrap();
        let sounds = sounds.canonicalize().unwrap();

        let mut vgmstream = Vgmstream::new().unwrap();
        assert!(vgmstream.open(&sounds, "MAPS.SM0", 1).is_err());
        assert!(vgmstream.open(&sounds, "missing.SM0", 1).is_err());
    }
}
