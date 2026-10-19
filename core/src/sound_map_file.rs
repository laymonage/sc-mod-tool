//! The structure of the game's sound maps, `MAPS.SM0` and `MAPS.LM0`.
//!
//! A map is split into areas, one for each level plus `fix` and `menu`. Each
//! area has a list of events, whose keys are what the game's sound packages
//! store, and a list of resources. An event plays a resource by its index in
//! the area's list. A resource is a clip of audio, or a container that plays
//! other resources: one at random, one chosen by a switch, or several in
//! sequence. Containers list their resources in the area's extra section.

use std::collections::HashMap;

/// The sizes of a map's area entries, events and resources.
pub(crate) const AREA_SIZE: usize = 0x30;
pub(crate) const EVENT_SIZE: usize = 0x58;
pub(crate) const RESOURCE_SIZE: usize = 0x80;
/// The top bits of a reference to a resource, from an event or a container.
/// The rest is the resource's index in its area.
pub(crate) const RESOURCE_REFERENCE: u32 = 0x4000_0000;
/// The type of an event that plays its resource, rather than stopping it.
pub(crate) const PLAY: u32 = 1;
/// The types of resources.
pub(crate) const AUDIO: u32 = 1;
const SEQUENCE: u32 = 4;
const SWITCH: u32 = 8;
const RANDOM: u32 = 10;
/// An event's pitch when it plays its audio at its own sample rate: 1.0, in
/// 16.16 fixed point.
pub(crate) const NORMAL_PITCH: u32 = 0x1_0000;
/// The most resources a container is read for, in case a count is garbage.
const MAX_CONTAINER_ENTRIES: u32 = 1024;
/// How deeply containers are followed, in case they refer to each other.
const MAX_CONTAINER_DEPTH: usize = 8;

/// A sound map's areas, events and resources.
pub(crate) struct SoundMapFile<'a> {
    data: &'a [u8],
    areas: Vec<Area>,
}

/// Where an area's lists are in its map.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Area {
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
                key: self.read(event)?,
                plays: self.read(event + 4)? == PLAY,
                resource: self.resource(&area, self.read(event + 8)?),
                pitch: self.read(event + 0x10)?,
            })
        })
    }

    pub(crate) fn read(&self, position: usize) -> Option<u32> {
        read_u32(self.data, position)
    }

    /// The offset of the resource `reference` points at in `area`.
    fn resource(&self, area: &Area, reference: u32) -> Option<usize> {
        let index = (reference & 0x0FFF_FFFF) as usize;
        (reference & 0xF000_0000 == RESOURCE_REFERENCE && index < area.resource_count)
            .then(|| area.resources_offset + index * RESOURCE_SIZE)
    }

    /// The offsets of the audio the resource at `resource` plays: itself, if
    /// it's audio, or the audio its containers lead to.
    pub(crate) fn clips(&self, area: &Area, resource: usize) -> Vec<usize> {
        let mut clips = Vec::new();
        self.collect_clips(area, resource, &mut Vec::new(), &mut clips);
        clips
    }

    fn collect_clips(
        &self,
        area: &Area,
        resource: usize,
        containers: &mut Vec<usize>,
        clips: &mut Vec<usize>,
    ) {
        if containers.contains(&resource) || containers.len() >= MAX_CONTAINER_DEPTH {
            return;
        }
        // Each kind of container lists its count in a different place, and
        // gives each resource different details after its reference.
        let (count_field, entry_size) = match self.read(resource + 4) {
            Some(AUDIO) => {
                clips.push(resource);
                return;
            }
            Some(RANDOM) => (0x1c, 12),
            Some(SWITCH) => (0x1c, 8),
            Some(SEQUENCE) => (0x2c, 4),
            _ => return,
        };
        let (Some(entries), Some(count)) = (
            self.read(resource + 0x10),
            self.read(resource + count_field),
        ) else {
            return;
        };
        containers.push(resource);
        for entry in 0..count.min(MAX_CONTAINER_ENTRIES) as usize {
            let Some(reference) =
                self.read(area.extra_offset + entries as usize + entry * entry_size)
            else {
                break;
            };
            if let Some(child) = self.resource(area, reference) {
                self.collect_clips(area, child, containers, clips);
            }
        }
        containers.pop();
    }

    /// The pitch the map's events play each clip at, by the key of the clip's
    /// resource. The game pitches some characters' voices down: the Chinese
    /// and Georgian soldiers', the Elite Force's and the Ex-Spetsnaz's play at
    /// two thirds of their stored sample rate.
    pub(crate) fn clip_pitches(&self) -> HashMap<u32, u32> {
        let mut pitches = HashMap::new();
        for area in &self.areas {
            for event in self.events(area).filter(|event| event.plays) {
                let Some(resource) = event.resource else {
                    continue;
                };
                let pitch = if event.pitch == 0 {
                    NORMAL_PITCH
                } else {
                    event.pitch
                };
                for clip in self.clips(area, resource) {
                    if let Some(key) = self.read(clip) {
                        pitches.entry(key).or_insert(pitch);
                    }
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

pub(crate) fn read_u32(data: &[u8], position: usize) -> Option<u32> {
    let bytes = data.get(position..position.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A resource in a [`test_map`].
    pub(crate) enum TestResource {
        /// A clip with this key, in `0_0_2.LS0` at offset `key * 100`.
        Audio(u32),
        /// A random container, playing the resources at these indexes.
        Random(Vec<u32>),
        /// A switch, playing the resources at these indexes.
        Switch(Vec<u32>),
    }

    /// A map with one area, whose events (key, resource index, pitch) play
    /// `resources`.
    pub(crate) fn test_map(events: &[(u32, u32, u32)], resources: &[TestResource]) -> Vec<u8> {
        let area = 0x0c + AREA_SIZE;
        let events_offset = 0x24;
        let resources_offset = events_offset + events.len() * EVENT_SIZE;
        let extra_offset = resources_offset + resources.len() * RESOURCE_SIZE;
        let mut data = vec![0; area + extra_offset];
        for (position, value) in [(0, 7), (4, 0x0c), (8, 1), (0x0c + 8, area as u32)] {
            write_u32(&mut data, position, value);
        }
        for (field, value) in [
            (4, events_offset),
            (8, events.len()),
            (0xc, resources_offset),
            (0x10, resources.len()),
            (0x1c, extra_offset),
        ] {
            write_u32(&mut data, area + field, value as u32);
        }
        for (index, (key, resource, pitch)) in events.iter().enumerate() {
            let event = area + events_offset + index * EVENT_SIZE;
            for (field, value) in [
                (0, *key),
                (4, PLAY),
                (8, RESOURCE_REFERENCE | resource),
                (0x10, *pitch),
            ] {
                write_u32(&mut data, event + field, value);
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
                entry[..4].copy_from_slice(&(RESOURCE_REFERENCE | child).to_le_bytes());
                data.extend(entry);
            }
        }
        data
    }

    pub(crate) fn write_u32(data: &mut [u8], position: usize, value: u32) {
        data[position..position + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn finds_the_pitch_of_clips_through_containers() {
        // A soldier's bark is chosen by a switch, then at random from two
        // clips, and played at two thirds of its rate.
        let data = test_map(
            &[(0x530000, 0, 43_738), (0x540000, 4, 0)],
            &[
                TestResource::Switch(vec![1]),
                TestResource::Random(vec![2, 3]),
                TestResource::Audio(0x530010),
                TestResource::Audio(0x530011),
                TestResource::Audio(0x540010),
            ],
        );
        let map = SoundMapFile::parse(&data).unwrap();

        assert_eq!(
            map.clip_pitches(),
            HashMap::from([
                (0x530010, 43_738),
                (0x530011, 43_738),
                // An event without a pitch plays at the clip's own rate.
                (0x540010, NORMAL_PITCH),
            ])
        );
        assert_eq!(pitched_sample_rate(16_000, 43_738), 10_678);
        assert_eq!(pitched_sample_rate(16_000, NORMAL_PITCH), 16_000);
    }

    #[test]
    fn stops_at_containers_that_contain_themselves() {
        let data = test_map(
            &[(1, 0, NORMAL_PITCH)],
            &[TestResource::Random(vec![0, 1]), TestResource::Audio(2)],
        );
        let map = SoundMapFile::parse(&data).unwrap();
        assert_eq!(map.clip_pitches(), HashMap::from([(2, NORMAL_PITCH)]));
    }

    #[test]
    fn rejects_maps_it_cant_read() {
        assert!(SoundMapFile::parse(&[]).is_err());
        let mut data = test_map(&[(1, 0, NORMAL_PITCH)], &[TestResource::Audio(1)]);
        write_u32(&mut data, 0, 8);
        assert!(SoundMapFile::parse(&data).is_err());
        let mut data = test_map(&[(1, 0, NORMAL_PITCH)], &[TestResource::Audio(1)]);
        data.truncate(0x0c + AREA_SIZE + 0x40);
        assert!(SoundMapFile::parse(&data).is_err());
    }
}
