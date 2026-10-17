//! Bindings to vgmstream's library, which reads the game's sound maps
//! (`MAPS.SM0`, `MAPS.LM0`) and the streams their sounds point at.
//!
//! vgmstream opens files through callbacks here, by names relative to
//! `Sounds`, so it can't read outside that folder.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    cache: Rc<RefCell<BlockCache>>,
}

impl Vgmstream {
    pub(crate) fn new() -> Result<Self, String> {
        let lib = NonNull::new(unsafe { libvgmstream_init() })
            .ok_or_else(|| "Could not start vgmstream.".to_owned())?;
        Ok(Self {
            lib,
            cache: Rc::default(),
        })
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
        let file = SoundFile::open(sounds, relative_path, &self.cache)
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
        .map(|character| *character as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The size of the blocks files are read in. vgmstream makes many small
/// reads.
const BLOCK_SIZE: u64 = 0x8000;
/// How many blocks a [`BlockCache`] keeps: 32 MiB.
const CACHED_BLOCKS: usize = 1024;

/// The blocks of files vgmstream has read, shared by every file one
/// [`Vgmstream`] opens. vgmstream reopens a sound map for each sound it opens
/// and reads its index again, so this saves most of the reading when listing a
/// map's sounds.
#[derive(Default)]
struct BlockCache {
    files: HashMap<PathBuf, HashMap<u64, Rc<[u8]>>>,
    /// The cached blocks, oldest first.
    order: VecDeque<(PathBuf, u64)>,
}

impl BlockCache {
    fn get(&self, path: &Path, index: u64) -> Option<Rc<[u8]>> {
        self.files.get(path)?.get(&index).cloned()
    }

    fn insert(&mut self, path: &Path, index: u64, block: Rc<[u8]>) {
        if self.order.len() == CACHED_BLOCKS
            && let Some((oldest_path, oldest_index)) = self.order.pop_front()
            && let Some(blocks) = self.files.get_mut(&oldest_path)
        {
            blocks.remove(&oldest_index);
        }
        self.files
            .entry(path.to_path_buf())
            .or_default()
            .insert(index, block);
        self.order.push_back((path.to_path_buf(), index));
    }
}

/// A file under `Sounds` that vgmstream reads. `api` comes first, so vgmstream's
/// pointer to it is also a pointer to the whole struct.
///
/// vgmstream's own `libstreamfile_open_buffered` isn't used, because it
/// returns data from the previous read's offset rather than the requested one.
#[repr(C)]
struct SoundFile {
    api: LibStreamFile,
    sounds: PathBuf,
    path: PathBuf,
    /// The path relative to `Sounds`, with `/` separators.
    name: CString,
    file: File,
    size: i64,
    cache: Rc<RefCell<BlockCache>>,
    /// The block read last, and its index.
    block: Option<(u64, Rc<[u8]>)>,
}

impl SoundFile {
    /// Opens `relative_path` under `sounds`, returning a pointer vgmstream
    /// owns.
    fn open(
        sounds: &Path,
        relative_path: &str,
        cache: &Rc<RefCell<BlockCache>>,
    ) -> Option<*mut LibStreamFile> {
        let (path, name) = resolve_sound_path(sounds, relative_path)?;
        let file = File::open(&path).ok()?;
        let size = i64::try_from(file.metadata().ok()?.len()).ok()?;
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
            path,
            name: CString::new(name).ok()?,
            file,
            size,
            cache: Rc::clone(cache),
            block: None,
        });
        let pointer = Box::into_raw(sound_file);
        unsafe { (*pointer).api.user_data = pointer.cast() };
        Some(pointer.cast())
    }

    /// Reads into `destination` from `offset`, returning how many bytes were
    /// read, which is fewer at the end of the file or on an error.
    fn read_at(&mut self, offset: u64, destination: &mut [u8]) -> usize {
        // vgmstream mostly reads a few bytes at a time from the last block.
        if let Some((index, block)) = &self.block {
            let start = offset.wrapping_sub(index * BLOCK_SIZE);
            if start < block.len() as u64 && destination.len() as u64 <= block.len() as u64 - start
            {
                let start = start as usize;
                destination.copy_from_slice(&block[start..start + destination.len()]);
                return destination.len();
            }
        }

        let mut read = 0;
        while read < destination.len() {
            let position = offset + read as u64;
            let index = position / BLOCK_SIZE;
            let Some(block) = self.read_block(index) else {
                break;
            };
            let start = (position - index * BLOCK_SIZE) as usize;
            if start >= block.len() {
                break;
            }
            let count = (block.len() - start).min(destination.len() - read);
            destination[read..read + count].copy_from_slice(&block[start..start + count]);
            read += count;
        }
        read
    }

    /// The block at `index`, or `None` past the end of the file or on an
    /// error.
    fn read_block(&mut self, index: u64) -> Option<Rc<[u8]>> {
        if let Some((current, block)) = &self.block
            && *current == index
        {
            return Some(Rc::clone(block));
        }
        let cached = self.cache.borrow().get(&self.path, index);
        let block = match cached {
            Some(block) => block,
            None => {
                self.file.seek(SeekFrom::Start(index * BLOCK_SIZE)).ok()?;
                let mut bytes = Vec::new();
                (&mut self.file)
                    .take(BLOCK_SIZE)
                    .read_to_end(&mut bytes)
                    .ok()?;
                if bytes.is_empty() {
                    return None;
                }
                let block: Rc<[u8]> = bytes.into();
                self.cache
                    .borrow_mut()
                    .insert(&self.path, index, Rc::clone(&block));
                block
            }
        };
        self.block = Some((index, Rc::clone(&block)));
        Some(block)
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
    SoundFile::open(&sound_file.sounds, name, &sound_file.cache).unwrap_or(std::ptr::null_mut())
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
        let block = BLOCK_SIZE as usize;
        let bytes: Vec<u8> = (0..block * 2 + 10).map(|index| index as u8).collect();
        fs::write(game.path().join("MAPS.SM0"), &bytes).unwrap();
        let sounds = game.path().canonicalize().unwrap();

        let file = SoundFile::open(&sounds, "MAPS.SM0", &Rc::default()).unwrap();
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
