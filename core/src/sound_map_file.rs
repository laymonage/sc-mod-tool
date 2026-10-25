//! The structure of the game's sound maps, `MAPS.SM0` and `MAPS.LM0`.
//!
//! A map is split into areas, one for each level plus `fix` and `menu`. Each
//! area has a list of events, whose keys are what the game's sound packages
//! store, and a list of resources. An event plays a resource by its index in
//! the area's list. A resource is a clip of audio, or a container that plays
//! other resources: one at random, one chosen by a switch, or several in
//! sequence. Containers list their resources in the area's extra section. See
//! `docs/sound-maps.md`.

use std::collections::HashMap;

/// The sizes of a map's area entries, events and resources.
pub(crate) const AREA_SIZE: usize = 0x30;
pub(crate) const EVENT_SIZE: usize = 0x58;
pub(crate) const RESOURCE_SIZE: usize = 0x80;
/// The top bits of a reference to a resource, from an event or a container.
/// The rest is the resource's index in its area. `MAPS.SM0` mostly leaves
/// them out, with the same meaning.
pub(crate) const RESOURCE_REFERENCE: u32 = 0x4000_0000;
const PLAIN_REFERENCE: u32 = 0;
/// The type of an event that plays its resource, rather than stopping it.
const PLAY: u32 = 1;
/// The types of resources.
pub(crate) const AUDIO: u32 = 1;
const SEQUENCE: u32 = 4;
const SWITCH: u32 = 8;
const RANDOM: u32 = 10;
/// An event's pitch when it plays its audio at its own sample rate: 1.0, in
/// 16.16 fixed point.
pub const NORMAL_PITCH: u32 = 0x1_0000;
/// The pitches an event is taken to set, from ×0.25 to ×4. Others are
/// garbage, and the audio plays at its own rate.
const PITCHES: std::ops::RangeInclusive<u32> = 0x4000..=0x4_0000;
/// The most resources a container is read for, in case a count is garbage.
const MAX_CONTAINER_ENTRIES: u32 = 1024;
/// How deeply containers are followed, in case they refer to each other.
const MAX_CONTAINER_DEPTH: usize = 8;
/// The most resources visited for an event, in case containers share their
/// resources so much that following every path would take too long.
const MAX_CONTAINER_VISITS: usize = 4096;

/// The fields of an audio resource that place its stream in its file.
pub(crate) const SIZE: usize = 12;
pub(crate) const OFFSET: usize = 20;
/// The sample count, or a looping resource's samples before its loop.
pub(crate) const SAMPLE_COUNT: usize = 48;
/// The size again, or a looping resource's bytes before its loop.
pub(crate) const SIZE_AGAIN: usize = 52;
/// The field of an audio resource whose bit 0 is set if it loops.
pub(crate) const LOOPS: usize = 44;
/// The fields of a looping resource with its loop's samples and bytes.
pub(crate) const LOOP_SAMPLE_COUNT: usize = 56;
pub(crate) const LOOP_SIZE: usize = 60;
/// The field of an audio resource with its sample rate.
pub(crate) const RATE: usize = 68;
/// The field of an audio resource with its codec.
pub(crate) const CODEC: usize = 76;
/// The codec of 16-bit PCM, which most sounds stored in maps use.
pub(crate) const PCM: u32 = 1;
/// The codec of Ubisoft ADPCM, which dialogue and Lambert's lines use.
pub(crate) const UBISOFT_ADPCM: u32 = 2;

/// A sound map's areas, events and resources.
pub(crate) struct SoundMapFile<'a> {
    data: &'a [u8],
    areas: Vec<Area>,
}

/// Where an area's lists are in its map.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Area {
    /// Where its entry is in the map's list of areas.
    entry: usize,
    header: usize,
    events_offset: usize,
    event_count: usize,
    resources_offset: usize,
    resource_count: usize,
    extra_offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Event {
    pub(crate) key: u32,
    /// Whether the event plays its resource, rather than, say, stopping it.
    pub(crate) plays: bool,
    /// The offset of the resource the event points at, if any.
    pub(crate) resource: Option<usize>,
    /// The pitch the event plays at, as a 16.16 fixed-point multiple of its
    /// audio's sample rate.
    pub(crate) pitch: u32,
}

impl<'a> SoundMapFile<'a> {
    pub(crate) fn parse(data: &'a [u8]) -> Result<Self, String> {
        let unreadable = || "isn't a sound map this tool can read".to_owned();
        let read = |position: usize| {
            read_u32(data, position)
                .map(|value| value as usize)
                .ok_or_else(unreadable)
        };
        if read_u32(data, 0) != Some(7) {
            return Err(unreadable());
        }
        let (areas_offset, area_count) = (read(4)?, read(8)?);

        let mut areas = Vec::new();
        for area in 0..area_count {
            let entry = area
                .checked_mul(AREA_SIZE)
                .and_then(|offset| offset.checked_add(areas_offset))
                .ok_or_else(unreadable)?;
            let header = read(entry + 8)?;
            let relative = |field: usize| -> Result<usize, String> {
                header
                    .checked_add(read(header + field)?)
                    .ok_or_else(unreadable)
            };
            let area = Area {
                entry,
                header,
                events_offset: relative(4)?,
                event_count: read(header + 8)?,
                resources_offset: relative(0xc)?,
                resource_count: read(header + 0x10)?,
                extra_offset: relative(0x1c)?,
            };
            let fits = |offset: usize, count: usize, size: usize| {
                count
                    .checked_mul(size)
                    .and_then(|length| length.checked_add(offset))
                    .is_some_and(|end| end <= data.len())
            };
            if !fits(area.events_offset, area.event_count, EVENT_SIZE)
                || !fits(area.resources_offset, area.resource_count, RESOURCE_SIZE)
            {
                return Err(unreadable());
            }
            areas.push(area);
        }
        Ok(Self { data, areas })
    }

    pub(crate) fn areas(&self) -> &[Area] {
        &self.areas
    }

    pub(crate) fn events(&self, area: &Area) -> impl Iterator<Item = Event> + '_ {
        let area = *area;
        (0..area.event_count).map_while(move |index| {
            let event = area.events_offset + index * EVENT_SIZE;
            Some(Event {
                key: read_u32(self.data, event)?,
                plays: read_u32(self.data, event + 4)? == PLAY,
                resource: self.resource(&area, read_u32(self.data, event + 8)?),
                pitch: read_u32(self.data, event + 0x10)?,
            })
        })
    }

    /// The area's name, such as `1_1_0Tbilisi`, which is null-terminated and
    /// at most 32 bytes.
    pub(crate) fn area_name(&self, area: &Area) -> &'a [u8] {
        let field = self.data.get(area.entry + 0x10..area.entry + AREA_SIZE);
        let field = field.unwrap_or_default();
        &field[..field
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(field.len())]
    }

    /// The offsets of the area's resources.
    pub(crate) fn resources(&self, area: &Area) -> impl Iterator<Item = usize> + use<> {
        let area = *area;
        (0..area.resource_count).map(move |index| area.resources_offset + index * RESOURCE_SIZE)
    }

    /// The 16-bit PCM clips stored in `area`, which areas each keep their own
    /// copy of. A resource that says its audio is in the map but whose audio
    /// can't be found is left out.
    ///
    /// The area's "section 3", at its header's `+0x14`, has one or more tables
    /// of where each resource's audio is in a subblock of the map, by the
    /// resource's index, and of where each subblock is. PCM is in subblock 0,
    /// as vgmstream reads it.
    pub(crate) fn stored_clips(&self, area: &Area) -> Vec<StoredClip<'a>> {
        let data = self.data;
        let read = |position: usize| read_u32(data, position).map(|value| value as usize);
        // Whether `count` entries of `size` bytes from `offset` are in the map,
        // so that a damaged count can't make this loop for long.
        let fits = |offset: usize, count: usize, size: usize| {
            count
                .checked_mul(size)
                .and_then(|length| length.checked_add(offset))
                .is_some_and(|end| end <= data.len())
        };
        let mut places = std::collections::HashMap::new();
        let (Some(section), Some(entries)) = (
            read(area.header + 0x14).and_then(|offset| offset.checked_add(area.header)),
            read(area.header + 0x18),
        ) else {
            return Vec::new();
        };
        if !fits(section, entries, 0x14) {
            return Vec::new();
        }
        for entry in 0..entries {
            let entry = section + entry * 0x14;
            let (Some(audio), Some(audio_count), Some(blocks), Some(block_count)) = (
                read(entry + 4).map(|offset| section + offset),
                read(entry + 8),
                read(entry + 0xc).map(|offset| section + offset),
                read(entry + 0x10),
            ) else {
                break;
            };
            if !fits(audio, audio_count, 8) || !fits(blocks, block_count, 0x10) {
                continue;
            }
            let block = (0..block_count)
                .map(|index| blocks + index * 0x10)
                .find(|&block| read(block) == Some(0));
            let Some((Some(size), Some(start))) =
                block.map(|block| (read(block + 8), read(block + 0xc)))
            else {
                continue;
            };
            for index in 0..audio_count {
                let position = audio + index * 8;
                if let (Some(resource), Some(offset)) = (read(position), read(position + 4)) {
                    places.insert(resource & 0x3FFF_FFFF, (start, offset, size));
                }
            }
        }

        let mut clips = Vec::new();
        for (index, resource) in self.resources(area).enumerate() {
            let Some(audio) = audio_resource(data, resource) else {
                continue;
            };
            let read16 = |field: usize| {
                let bytes = data.get(resource + field..resource + field + 2)?;
                Some(u16::from_le_bytes([bytes[0], bytes[1]]))
            };
            let stored = read(resource + 8).is_some_and(|internal| internal != 0)
                && read(resource + 0x28).is_some_and(|streamed| streamed & 1 == 0);
            let (Some(placement), Some(&(start, offset, size))) =
                (audio.placement, places.get(&index))
            else {
                continue;
            };
            let length = placement.length as usize;
            if !stored
                || audio.codec != Some(PCM)
                || read16(72) != Some(16)
                || offset.checked_add(length).is_none_or(|end| end > size)
                || start
                    .checked_add(offset + length)
                    .is_none_or(|end| end > data.len())
            {
                continue;
            }
            clips.push(StoredClip {
                resource,
                key: audio.key,
                name: audio.file_name,
                offset: (start + offset) as u64,
                length: placement.length,
                sample_count: placement.sample_count,
                channels: read16(74).unwrap_or(0),
                loop_start: audio.loop_start,
            });
        }
        clips
    }

    /// The offset of the resource `reference` points at in `area`.
    ///
    /// A few of `MAPS.SM0`'s references have `0x8000_0000` instead, which
    /// seem to point at the `fix` area's resources. None of those plays a
    /// clip at a pitch other than 1.0, so they aren't followed.
    fn resource(&self, area: &Area, reference: u32) -> Option<usize> {
        let index = (reference & 0x0FFF_FFFF) as usize;
        let tag = reference & 0xF000_0000;
        ((tag == RESOURCE_REFERENCE || tag == PLAIN_REFERENCE) && index < area.resource_count)
            .then(|| area.resources_offset + index * RESOURCE_SIZE)
    }

    /// The offsets of the audio the resource at `resource` plays: itself, if
    /// it's audio, or the audio its containers lead to.
    fn clips(&self, area: &Area, resource: usize) -> Vec<usize> {
        let mut clips = Vec::new();
        let mut visits_left = MAX_CONTAINER_VISITS;
        self.collect_clips(
            area,
            resource,
            &mut Vec::new(),
            &mut visits_left,
            &mut clips,
        );
        clips
    }

    fn collect_clips(
        &self,
        area: &Area,
        resource: usize,
        containers: &mut Vec<usize>,
        visits_left: &mut usize,
        clips: &mut Vec<usize>,
    ) {
        if *visits_left == 0
            || containers.contains(&resource)
            || containers.len() >= MAX_CONTAINER_DEPTH
        {
            return;
        }
        *visits_left -= 1;
        let read = |position| read_u32(self.data, position);
        // Each kind of container lists its count in a different place, and
        // gives each resource different details after its reference.
        let (count_field, entry_size) = match read(resource + 4) {
            Some(AUDIO) => {
                clips.push(resource);
                return;
            }
            Some(RANDOM) => (0x1c, 12),
            Some(SWITCH) => (0x1c, 8),
            Some(SEQUENCE) => (0x2c, 4),
            _ => return,
        };
        let (Some(entries), Some(count)) = (read(resource + 0x10), read(resource + count_field))
        else {
            return;
        };
        containers.push(resource);
        for entry in 0..count.min(MAX_CONTAINER_ENTRIES) as usize {
            let Some(reference) = read(area.extra_offset + entries as usize + entry * entry_size)
            else {
                break;
            };
            if let Some(child) = self.resource(area, reference) {
                self.collect_clips(area, child, containers, visits_left, clips);
            }
        }
        containers.pop();
    }

    /// The pitch each clip is played at, by its area's name and its index in
    /// the area's resources: that of the first event in its area that plays
    /// it, directly or through containers.
    ///
    /// The game pitches some characters' voices down: the Chinese and
    /// Georgian soldiers', the Elite Force's and the Ex-Spetsnaz's play at two
    /// thirds of their stored sample rate. A few of `MAPS.SM0`'s sound effects
    /// are pitched too, such as the laser alarm, at ×1.189.
    pub(crate) fn clip_pitches(&self) -> HashMap<(&'a [u8], usize), u32> {
        let mut pitches = HashMap::new();
        for area in &self.areas {
            let name = self.area_name(area);
            for event in self.events(area).filter(|event| event.plays) {
                let Some(resource) = event.resource else {
                    continue;
                };
                let pitch = if PITCHES.contains(&event.pitch) {
                    event.pitch
                } else {
                    NORMAL_PITCH
                };
                for clip in self.clips(area, resource) {
                    let index = (clip - area.resources_offset) / RESOURCE_SIZE;
                    pitches.entry((name, index)).or_insert(pitch);
                }
            }
        }
        pitches
    }
}

/// The sample rate audio stored at `sample_rate` plays at with `pitch`.
pub(crate) fn pitched_sample_rate(sample_rate: u32, pitch: u32) -> u32 {
    ((u64::from(sample_rate) * u64::from(pitch) + u64::from(NORMAL_PITCH / 2)) >> 16) as u32
}

/// Where a stream is in its file, and how many samples it has: what a sound
/// map's resource says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Placement {
    pub(crate) offset: u64,
    pub(crate) length: u64,
    pub(crate) sample_count: u32,
}

/// A clip of 16-bit PCM stored in a map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredClip<'a> {
    /// Where its resource is in the map.
    pub(crate) resource: usize,
    /// Its resource's key and file name, the name of the original recording.
    pub(crate) key: u32,
    pub(crate) name: &'a [u8],
    /// Where its audio is in the map file.
    pub(crate) offset: u64,
    pub(crate) length: u64,
    pub(crate) sample_count: u32,
    pub(crate) channels: u16,
    /// The sample its loop starts at, if it loops.
    pub(crate) loop_start: Option<u32>,
}

/// What an audio resource says about its audio.
pub(crate) struct AudioResource<'a> {
    pub(crate) key: u32,
    pub(crate) codec: Option<u32>,
    /// The rate it's stored at.
    pub(crate) rate: Option<u32>,
    pub(crate) file_name: &'a [u8],
    /// The sample its loop starts at, if it loops, which splits its sample
    /// count and size again in two. The loop runs to its end.
    pub(crate) loop_start: Option<u32>,
    /// Where its stream is, unless the resource's sizes disagree.
    pub(crate) placement: Option<Placement>,
}

/// The audio resource at `position`, or `None` for another kind of resource
/// or one without a file name.
pub(crate) fn audio_resource(data: &[u8], position: usize) -> Option<AudioResource<'_>> {
    let read = |field| read_u32(data, position + field);
    if read(4)? != AUDIO {
        return None;
    }
    let (offset, length) = (read(OFFSET)?, read(SIZE)?);
    // A looping resource has the samples and bytes before its loop where the
    // sample count and size again would be, and its loop's after them.
    let looping = read(LOOPS)? & 1 != 0;
    let before = (read(SAMPLE_COUNT)?, read(SIZE_AGAIN)?);
    let (sample_count, size_again) = if looping {
        (
            before.0.checked_add(read(LOOP_SAMPLE_COUNT)?)?,
            before.1.checked_add(read(LOOP_SIZE)?)?,
        )
    } else {
        before
    };
    Some(AudioResource {
        key: read(0)?,
        codec: read(CODEC),
        rate: read(RATE),
        file_name: file_name(data, position)?,
        loop_start: looping.then_some(before.0),
        placement: (size_again == length).then_some(Placement {
            offset: offset.into(),
            length: length.into(),
            sample_count,
        }),
    })
}

pub(crate) fn read_u32(data: &[u8], position: usize) -> Option<u32> {
    let bytes = data.get(position..position.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// The file name of the audio resource at `position`, which is
/// null-terminated and at most 32 bytes.
pub(crate) fn file_name(data: &[u8], position: usize) -> Option<&[u8]> {
    let start = position.checked_add(80)?;
    let field = data.get(start..start.checked_add(32)?)?;
    let name = &field[..field.iter().position(|byte| *byte == 0)?];
    (!name.is_empty() && name.is_ascii()).then_some(name)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A map with one area, whose events (key, resource index) play the
    /// given resources, each an audio clip with the given key in `0_0_2.LS0`
    /// at offset `key * 100`.
    pub(crate) fn test_map(events: &[(u32, u32)], resource_keys: &[u32]) -> Vec<u8> {
        let events: Vec<_> = events
            .iter()
            .map(|&(key, resource)| TestEvent {
                key,
                kind: PLAY,
                reference: RESOURCE_REFERENCE | resource,
                pitch: NORMAL_PITCH,
            })
            .collect();
        let resources: Vec<_> = resource_keys
            .iter()
            .map(|&key| TestResource::Audio(key))
            .collect();
        container_map(&events, &resources)
    }

    /// An event in a [`container_map`].
    pub(crate) struct TestEvent {
        pub(crate) key: u32,
        pub(crate) kind: u32,
        pub(crate) reference: u32,
        pub(crate) pitch: u32,
    }

    /// A resource in a [`container_map`].
    pub(crate) enum TestResource {
        /// A clip with this key, in `0_0_2.LS0` at offset `key * 100`.
        Audio(u32),
        /// A random container, playing the resources these refer to.
        Random(Vec<u32>),
        /// A switch, playing the resources these refer to.
        Switch(Vec<u32>),
    }

    /// The name of a [`container_map`]'s area.
    pub(crate) const TEST_AREA: &[u8] = b"1_1_0Tbilisi";

    /// A map with one area, [`TEST_AREA`], whose events play `resources`.
    pub(crate) fn container_map(events: &[TestEvent], resources: &[TestResource]) -> Vec<u8> {
        let area = 0x0c + AREA_SIZE;
        let events_offset = 0x24;
        let resources_offset = events_offset + events.len() * EVENT_SIZE;
        let extra_offset = resources_offset + resources.len() * RESOURCE_SIZE;
        let mut data = vec![0; area + extra_offset];
        for (position, value) in [(0, 7), (4, 0x0c), (8, 1), (0x0c + 8, area as u32)] {
            write_u32(&mut data, position, value);
        }
        data[0x0c + 0x10..0x0c + 0x10 + TEST_AREA.len()].copy_from_slice(TEST_AREA);
        for (field, value) in [
            (4, events_offset),
            (8, events.len()),
            (0xc, resources_offset),
            (0x10, resources.len()),
            (0x1c, extra_offset),
        ] {
            write_u32(&mut data, area + field, value as u32);
        }
        for (index, event) in events.iter().enumerate() {
            let position = area + events_offset + index * EVENT_SIZE;
            for (field, value) in [
                (0, event.key),
                (4, event.kind),
                (8, event.reference),
                (0x10, event.pitch),
            ] {
                write_u32(&mut data, position + field, value);
            }
        }
        for (index, resource) in resources.iter().enumerate() {
            let position = area + resources_offset + index * RESOURCE_SIZE;
            let (kind, children, entry_size) = match resource {
                TestResource::Audio(key) => {
                    for (field, value) in [
                        (0, *key),
                        (4, AUDIO),
                        (8, 1),
                        (12, 15_571),
                        (20, key * 100),
                        (48, 29_967),
                        (52, 15_571),
                        (68, 36_000),
                    ] {
                        write_u32(&mut data, position + field, value);
                    }
                    data[position + 80..position + 89].copy_from_slice(b"0_0_2.LS0");
                    continue;
                }
                TestResource::Random(children) => (RANDOM, children, 12),
                TestResource::Switch(children) => (SWITCH, children, 8),
            };
            let entries = data.len() - (area + extra_offset);
            write_u32(&mut data, position + 4, kind);
            write_u32(&mut data, position + 0x10, entries as u32);
            write_u32(&mut data, position + 0x1c, children.len() as u32);
            for child in children {
                let mut entry = vec![0; entry_size];
                entry[..4].copy_from_slice(&child.to_le_bytes());
                data.extend(entry);
            }
        }
        data
    }

    #[test]
    fn finds_the_pitch_of_clips_through_containers() {
        let play = |key, reference, pitch| TestEvent {
            key,
            kind: PLAY,
            reference,
            pitch,
        };
        // A soldier's bark is chosen by a switch, then at random from two
        // clips, and played at two thirds of its rate. `MAPS.SM0` refers to
        // resources by their plain index.
        let data = container_map(
            &[
                // Not a play event, so its pitch isn't the clips'.
                TestEvent {
                    kind: 10,
                    ..play(0x530000, RESOURCE_REFERENCE, 0x8000)
                },
                play(0x530000, RESOURCE_REFERENCE, 43_738),
                play(0x540000, 4, 0),
                play(0x550000, 5, 0x1_3061),
                // Another area's resource.
                play(0x560000, 0x8000_0006, 0x8000),
            ],
            &[
                TestResource::Switch(vec![RESOURCE_REFERENCE | 1]),
                TestResource::Random(vec![2, RESOURCE_REFERENCE | 3]),
                TestResource::Audio(0x530010),
                TestResource::Audio(0x530011),
                TestResource::Audio(0x540010),
                TestResource::Audio(0x550010),
                TestResource::Audio(0x560010),
            ],
        );
        let map = SoundMapFile::parse(&data).unwrap();

        assert_eq!(
            map.clip_pitches(),
            HashMap::from([
                ((TEST_AREA, 2), 43_738),
                ((TEST_AREA, 3), 43_738),
                // An event without a pitch plays at the clip's own rate.
                ((TEST_AREA, 4), NORMAL_PITCH),
                ((TEST_AREA, 5), 0x1_3061),
            ])
        );
        assert_eq!(pitched_sample_rate(16_000, 43_738), 10_678);
        assert_eq!(pitched_sample_rate(16_000, NORMAL_PITCH), 16_000);
    }

    #[test]
    fn stops_following_containers_that_share_their_resources_a_lot() {
        // Each container plays the next one 1,024 times, which would be
        // 1,024⁶ paths to the clip.
        let mut resources: Vec<_> = (1..7)
            .map(|next| TestResource::Random(vec![RESOURCE_REFERENCE | next; 1024]))
            .collect();
        resources.push(TestResource::Audio(1));
        let data = container_map(
            &[TestEvent {
                key: 1,
                kind: PLAY,
                reference: RESOURCE_REFERENCE,
                pitch: 0x8000,
            }],
            &resources,
        );
        let map = SoundMapFile::parse(&data).unwrap();
        assert_eq!(
            map.clip_pitches(),
            HashMap::from([((TEST_AREA, 6), 0x8000)])
        );
    }

    #[test]
    fn stops_at_containers_that_contain_themselves() {
        let data = container_map(
            &[TestEvent {
                key: 1,
                kind: PLAY,
                reference: RESOURCE_REFERENCE,
                pitch: NORMAL_PITCH,
            }],
            &[
                TestResource::Random(vec![RESOURCE_REFERENCE, RESOURCE_REFERENCE | 1]),
                TestResource::Audio(2),
            ],
        );
        let map = SoundMapFile::parse(&data).unwrap();
        assert_eq!(
            map.clip_pitches(),
            HashMap::from([((TEST_AREA, 1), NORMAL_PITCH)])
        );
    }

    /// A map whose areas each store the given mono 16-bit PCM clips, by key
    /// and name, with the clips' audio after each area's tables. The first
    /// area also has audio resources for `streams`, in other files.
    pub(crate) fn map_with_clips(
        areas: &[&[(u32, &str, &[i16])]],
        streams: &[(&str, Placement)],
    ) -> Vec<u8> {
        let mut data = vec![0; 0x0c + areas.len() * AREA_SIZE];
        for (position, value) in [(0, 7), (4, 0x0c), (8, areas.len() as u32)] {
            write_u32(&mut data, position, value);
        }
        for (index, clips) in areas.iter().enumerate() {
            let header = data.len();
            write_u32(&mut data, 0x0c + index * AREA_SIZE + 8, header as u32);
            let streams = if index == 0 { streams } else { &[] };
            let resources = 0x24;
            let section = resources + (clips.len() + streams.len()) * RESOURCE_SIZE;
            let blocks = 0x14 + clips.len() * 8;
            let audio = section + blocks + 0x10;
            data.resize(header + audio, 0);
            for (field, value) in [
                (4, resources),
                (0xc, resources),
                (0x10, clips.len() + streams.len()),
                (0x14, section),
                (0x18, 1),
            ] {
                write_u32(&mut data, header + field, value as u32);
            }
            let section = header + section;
            for (field, value) in [(0, u32::MAX), (4, 0x14), (8, clips.len() as u32)] {
                write_u32(&mut data, section + field, value);
            }
            write_u32(&mut data, section + 0xc, blocks as u32);
            write_u32(&mut data, section + 0x10, 1);
            let mut offset = 0;
            for (clip, &(key, name, samples)) in clips.iter().enumerate() {
                let resource = header + resources + clip * RESOURCE_SIZE;
                let length = samples.len() as u32 * 2;
                for (field, value) in [
                    (0, key),
                    (4, AUDIO),
                    (8, 1),
                    (SIZE, length),
                    (SAMPLE_COUNT, samples.len() as u32),
                    (SIZE_AGAIN, length),
                    (68, 22_050),
                    (72, 0x0001_0010),
                    (CODEC, PCM),
                ] {
                    write_u32(&mut data, resource + field, value);
                }
                data[resource + 80..resource + 80 + name.len()].copy_from_slice(name.as_bytes());
                write_u32(&mut data, section + 0x14 + clip * 8, clip as u32);
                write_u32(&mut data, section + 0x14 + clip * 8 + 4, offset);
                offset += length;
            }
            for (stream, (name, placement)) in streams.iter().enumerate() {
                let resource = header + resources + (clips.len() + stream) * RESOURCE_SIZE;
                for (field, value) in [
                    (0, 100 + stream as u32),
                    (4, AUDIO),
                    (SIZE, placement.length as u32),
                    (OFFSET, placement.offset as u32),
                    (SAMPLE_COUNT, placement.sample_count),
                    (SIZE_AGAIN, placement.length as u32),
                    (CODEC, UBISOFT_ADPCM),
                ] {
                    write_u32(&mut data, resource + field, value);
                }
                data[resource + 80..resource + 80 + name.len()].copy_from_slice(name.as_bytes());
            }
            let block = section + blocks;
            for (field, value) in [
                (0, 0),
                (4, offset),
                (8, offset),
                (0xc, (header + audio) as u32),
            ] {
                write_u32(&mut data, block + field, value);
            }
            for &(_, _, samples) in *clips {
                data.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
            }
        }
        data
    }

    #[test]
    fn finds_clips_stored_in_areas() {
        let map = map_with_clips(
            &[
                &[(1, "DOOR.wav", &[1, 2, 3]), (2, "STEP.wav", &[4, 5])],
                &[(2, "STEP.wav", &[4, 5])],
            ],
            &[],
        );
        let file = SoundMapFile::parse(&map).unwrap();
        let clips: Vec<_> = file
            .areas()
            .iter()
            .flat_map(|area| file.stored_clips(area))
            .map(|clip| {
                let start = clip.offset as usize;
                (
                    clip.key,
                    clip.name,
                    map[start..start + clip.length as usize].to_vec(),
                )
            })
            .collect();
        assert_eq!(
            clips,
            [
                (1, &b"DOOR.wav"[..], vec![1, 0, 2, 0, 3, 0]),
                (2, &b"STEP.wav"[..], vec![4, 0, 5, 0]),
                (2, &b"STEP.wav"[..], vec![4, 0, 5, 0]),
            ]
        );
    }

    #[test]
    fn counts_a_looping_clips_samples_before_and_in_its_loop() {
        let mut map = map_with_clips(&[&[(1, "FIRE.wav", &[1, 2, 3, 4, 5])]], &[]);
        let file = SoundMapFile::parse(&map).unwrap();
        let resource = file.resources(&file.areas()[0]).next().unwrap();
        for (field, value) in [
            (LOOPS, 1),
            (SAMPLE_COUNT, 2),
            (SIZE_AGAIN, 4),
            (LOOP_SAMPLE_COUNT, 3),
            (LOOP_SIZE, 6),
        ] {
            write_u32(&mut map, resource + field, value);
        }
        let file = SoundMapFile::parse(&map).unwrap();
        let clips = file.stored_clips(&file.areas()[0]);
        assert_eq!((clips[0].sample_count, clips[0].loop_start), (5, Some(2)));
    }

    #[test]
    fn places_no_looping_clip_whose_parts_dont_add_up_to_its_size() {
        let mut map = map_with_clips(&[&[(1, "FIRE.wav", &[1, 2, 3, 4, 5])]], &[]);
        let file = SoundMapFile::parse(&map).unwrap();
        let resource = file.resources(&file.areas()[0]).next().unwrap();
        // The size again at `+52`, as a clip that doesn't loop has it.
        for (field, value) in [(LOOPS, 1), (LOOP_SAMPLE_COUNT, 3), (LOOP_SIZE, 6)] {
            write_u32(&mut map, resource + field, value);
        }
        let audio = audio_resource(&map, resource).unwrap();
        assert_eq!((audio.loop_start, audio.placement), (Some(5), None));
        let file = SoundMapFile::parse(&map).unwrap();
        assert!(file.stored_clips(&file.areas()[0]).is_empty());
    }

    pub(crate) fn write_u32(data: &mut [u8], position: usize, value: u32) {
        data[position..position + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn rejects_maps_it_cant_read() {
        assert!(SoundMapFile::parse(&[]).is_err());
        let mut data = test_map(&[(1, 0)], &[1]);
        write_u32(&mut data, 0, 8);
        assert!(SoundMapFile::parse(&data).is_err());
        let mut data = test_map(&[(1, 0)], &[1]);
        data.truncate(0x0c + AREA_SIZE + 0x40);
        assert!(SoundMapFile::parse(&data).is_err());
    }
}
