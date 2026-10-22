//! The structure of the game's sound maps, `MAPS.SM0` and `MAPS.LM0`.
//!
//! A map is split into areas, one for each level plus `fix` and `menu`. Each
//! area has a list of events, whose keys are what the game's sound packages
//! store, and a list of resources. An event plays a resource by its index in
//! the area's list. A resource is a clip of audio, or a container that plays
//! other resources, which isn't followed here: dialogue's events point
//! straight at its audio. See `docs/sound-maps.md`.

/// The sizes of a map's area entries, events and resources.
pub(crate) const AREA_SIZE: usize = 0x30;
pub(crate) const EVENT_SIZE: usize = 0x58;
pub(crate) const RESOURCE_SIZE: usize = 0x80;
/// The top bits of an event's reference to a resource. The rest is the
/// resource's index in its area.
pub(crate) const RESOURCE_REFERENCE: u32 = 0x4000_0000;
/// The type of a resource that's a clip of audio.
pub(crate) const AUDIO: u32 = 1;

/// The fields of an audio resource that place its stream in its file.
pub(crate) const SIZE: usize = 12;
pub(crate) const OFFSET: usize = 20;
pub(crate) const SAMPLE_COUNT: usize = 48;
/// The size again.
pub(crate) const SIZE_AGAIN: usize = 52;
/// The field of an audio resource with its codec.
pub(crate) const CODEC: usize = 76;
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
    events_offset: usize,
    event_count: usize,
    resources_offset: usize,
    resource_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Event {
    pub(crate) key: u32,
    /// The offset of the resource the event points at, if any.
    pub(crate) resource: Option<usize>,
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
                resource: self.resource(&area, read_u32(self.data, event + 8)?),
            })
        })
    }

    /// The offsets of the area's resources.
    pub(crate) fn resources(&self, area: &Area) -> impl Iterator<Item = usize> + use<> {
        let area = *area;
        (0..area.resource_count).map(move |index| area.resources_offset + index * RESOURCE_SIZE)
    }

    /// The offset of the resource `reference` points at in `area`.
    fn resource(&self, area: &Area, reference: u32) -> Option<usize> {
        let index = (reference & 0x0FFF_FFFF) as usize;
        (reference & 0xF000_0000 == RESOURCE_REFERENCE && index < area.resource_count)
            .then(|| area.resources_offset + index * RESOURCE_SIZE)
    }
}

/// Where a stream is in its file, and how many samples it has: what a sound
/// map's resource says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Placement {
    pub(crate) offset: u64,
    pub(crate) length: u64,
    pub(crate) sample_count: u32,
}

/// What an audio resource says about its audio.
pub(crate) struct AudioResource<'a> {
    pub(crate) key: u32,
    pub(crate) codec: Option<u32>,
    pub(crate) file_name: &'a [u8],
    /// Where its stream is, unless the resource's two sizes disagree.
    pub(crate) placement: Option<Placement>,
}

/// The audio resource at `position`, or `None` for another kind of resource
/// or one without a file name.
pub(crate) fn audio_resource(data: &[u8], position: usize) -> Option<AudioResource<'_>> {
    let read = |field| read_u32(data, position + field);
    if read(4)? != AUDIO {
        return None;
    }
    let (offset, length, sample_count) = (read(OFFSET)?, read(SIZE)?, read(SAMPLE_COUNT)?);
    Some(AudioResource {
        key: read(0)?,
        codec: read(CODEC),
        file_name: file_name(data, position)?,
        placement: (read(SIZE_AGAIN)? == length).then_some(Placement {
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

/// [`file_name`], as a string.
pub(crate) fn read_file_name(data: &[u8], position: usize) -> Option<String> {
    file_name(data, position).map(|name| String::from_utf8_lossy(name).into_owned())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A map with one area, whose events (key, resource index) play the
    /// given resources, each an audio clip with the given key in `0_0_2.LS0`
    /// at offset `key * 100`.
    pub(crate) fn test_map(events: &[(u32, u32)], resource_keys: &[u32]) -> Vec<u8> {
        let area = 0x0c + AREA_SIZE;
        let events_offset = 0x24;
        let resources_offset = events_offset + events.len() * EVENT_SIZE;
        let mut data = vec![0; area + resources_offset + resource_keys.len() * RESOURCE_SIZE];
        for (position, value) in [(0, 7), (4, 0x0c), (8, 1), (0x0c + 8, area as u32)] {
            write_u32(&mut data, position, value);
        }
        for (field, value) in [
            (4, events_offset),
            (8, events.len()),
            (0xc, resources_offset),
            (0x10, resource_keys.len()),
        ] {
            write_u32(&mut data, area + field, value as u32);
        }
        for (index, (key, resource)) in events.iter().enumerate() {
            let event = area + events_offset + index * EVENT_SIZE;
            write_u32(&mut data, event, *key);
            write_u32(&mut data, event + 8, RESOURCE_REFERENCE | resource);
        }
        for (index, key) in resource_keys.iter().enumerate() {
            let resource = area + resources_offset + index * RESOURCE_SIZE;
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
                write_u32(&mut data, resource + field, value);
            }
            data[resource + 80..resource + 89].copy_from_slice(b"0_0_2.LS0");
        }
        data
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
