# Sound maps

`MAPS.SM0` and `MAPS.LM0` list every sound the game plays: what plays it, and
where its audio is.

- **`Sounds/MAPS.SM0`** is shared by every language: sound effects, music and
  long streams.
- **`Sounds/<language>/MAPS.LM0`** is the language's own: characters' barks,
  voice lines and the alerts in `Music_*.LS0`.

Each [extra map](README.md#extra-maps) has its own of both, which only describe
its own files.

## Structure

A map is split into areas, one per level plus `fix` and `menu`. Each area has
a list of **events**, which the game plays, and a list of **resources**, which
describe the audio. The keys the sound packages store are event keys. An event
plays a resource by its index in the area's list, so the game never looks a
resource up by key.

All values are little-endian 32-bit unless noted:

| Where | Offset | Field |
|---|---|---|
| File | `+0` | Version, `7` |
| | `+4`, `+8` | Offset and number of areas |
| Area entry (`0x30` bytes) | `+8` | Offset of the area's header |
| | `+16` | Area name, such as `1_1_0Tbilisi` |
| Area header | `+4`, `+8` | Offset (from the header) and number of events |
| | `+12`, `+16` | Offset (from the header) and number of resources |
| | `+20`, `+24` | Offset (from the header) and number of entries of [section 3](#audio-stored-in-a-map) |
| | `+28`, `+32` | Offset (from the header) and size of the extra section |
| Event (`0x58` bytes) | `+0` | Key: `(package ID << 16) \| sound ID` |
| | `+4` | Type: `1` plays, `2` stops |
| | `+8` | `0x40000000` \| the resource's index in the area |
| | `+16` | [Pitch](#pitch), as a 16.16 fixed-point multiple of the sample rate |
| Resource (`0x80` bytes) | `+0` | The resource's own key |
| | `+4` | [Type](#resource-types) |

An audio resource (type `1`) continues:

| Offset | Field |
|---|---|
| `+8` | `1`, in every audio resource in the game's maps. vgmstream reads `0` as audio in another file |
| `+12` | Stream size in bytes, repeated at `+52` unless it loops |
| `+20` | Offset of the stream in its file, `0` for audio stored in the map |
| `+40` | Bit 0 set if it's streamed from another file, clear if it's stored in the map |
| `+44` | Bit 0 set if it [loops](#looping-clips) |
| `+48` | Sample count, unless it loops |
| `+68` | Sample rate |
| `+72`, `+74` | Bits per decoded sample (16) and channels, 16-bit each |
| `+76` | Codec: `1` 16-bit PCM, `2` Ubisoft ADPCM, `4` Ubisoft IMA ADPCM |
| `+80` | File name, null-terminated, up to 32 bytes |

The file name is the [stream file](sound-banks.md) holding the audio, such as
`1_3_2.LS0` or `Music_Common.SS0`, or, for audio stored in the map itself, the
original recording's name, such as `DOOJAM_1.wav` or `CSOGAZ_1.ima`.

### Looping clips

A looping clip, such as an automatic weapon's fire (`AK47IS_1.wav`) or a
fire's crackle (`FIRE_1.wav`), splits its sample count and size in two: the
part before its loop, and the loop, from there to the clip's end, which the
game repeats.

| Offset | Field |
|---|---|
| `+48`, `+52` | Samples and bytes before the loop, usually `0` |
| `+56`, `+60` | Samples and bytes in the loop |

The clip's sample count is the sum of the two, which is how vgmstream reads
it, and its size at `+12` is the sum of the two sizes. vgmstream reads the
size from `+12` alone. `MAPS.SM0` has 80 looping clips, and the extra maps'
own have 13 (`1_6_1_1kolacell`), 30 (`1_7_1_1VselkaInfiltration`) and 10
(`1_7_1_2Vselka`), all stored in the map. No `MAPS.LM0` has any. A
replacement keeps the loop where it was, so it should loop cleanly from there.
The app shows where a sound it can replace loops from.

Non-audio resources keep leftover bytes from the previous resource in fields
they don't use, as vgmstream's notes warn, so a container can seem to have a
file name.

The app reads events and resources in
[`sound_map_file.rs`](../core/src/sound_map_file.rs), for subtitles. vgmstream
reads the rest.

## Audio stored in a map

Audio stored in the map isn't found from its resource's offset, which is `0`.
Each area keeps its own copy of the audio its resources play, in subblocks
after its tables, and "section 3" says where. Each of its entries (`0x14`
bytes) is:

| Offset | Field |
|---|---|
| `+0` | An ID, `-1` for the first |
| `+4`, `+8` | Offset (from section 3) and number of entries of the audio table |
| `+12`, `+16` | Offset (from section 3) and number of entries of the subblock table |

The audio table's entries (8 bytes) are a resource's index in the area, in its
low 30 bits, and where its audio starts in its subblock. The subblock table's
(16 bytes) are a subblock's ID, its size with and without padding, and its
offset in the map file. 16-bit PCM is in subblock `0`, and other codecs in
subblock `1`, as vgmstream reads them.

Every area that plays a sound has a copy, and in the game's maps the copies
are identical: `MAPS.SM0`'s 868 PCM sounds have 6,058 copies, for example. The
app replaces a sound by writing every copy
([editing notes](editing-notes.md#how-saving-works)). It reads the tables in
[`sound_map_file.rs`](../core/src/sound_map_file.rs).

## Resource types

| Type | What it is |
|---|---|
| `1` | A clip of audio |
| `4` | A sequence: several resources in a row |
| `8` | A switch: one resource, chosen by a value |
| `10` | Random: one resource, chosen at random |
| `15` | Silence |

Containers (`4`, `8` and `10`) list their resources in the area's extra
section, from the offset at their `+16`. Each entry starts with a
`0x40000000`-style reference:

| Type | Entries | Counted at | Entry size |
|---|---|---|---|
| Sequence (`4`) | References | `+44` | 4 bytes |
| Switch (`8`) | Reference, then the value that chooses it | `+28` | 8 bytes |
| Random (`10`) | Reference, then the chance out of `0x10000` | `+28` | 12 bytes |

Dialogue's events point straight at audio. Characters' barks go through
containers: some barks' events play a random container, and others a switch,
which picks a random container, which picks a clip.

## Pitch

An event plays its audio at its pitch times the audio's stored sample rate.
Almost every event's pitch is 1.0 (`0x10000`), but the Chinese and Georgian
soldiers', the Elite Force's and the Ex-Spetsnaz's barks (`CSO*`, `GSO*`,
`EFO*`, `EXS*`) are stored at 16,000 Hz and played at two thirds of that
(`0xAADA`), 10,678 Hz. Played at their stored rate, they sound too fast and
too high. A few of [`MAPS.SM0`'s](#maps-sm0s-events) sound effects have a
pitch too. Following each event through its containers gives every clip a
single pitch.

vgmstream plays each clip at its stored rate: it reads maps as banks of
audio, and leaves what events do with it to other tools, as for other formats
with events ("cue formats" in its usage notes). The app applies the pitch
itself. For each clip, it finds the first play event in the clip's area that
reaches it, directly or through containers, and scales the sample rate
vgmstream gives by that event's pitch (`clip_pitches` in
[`sound_map_file.rs`](../core/src/sound_map_file.rs)). A pitch outside ×0.25
to ×4 is taken as garbage, and the clip plays at its stored rate. In
`MAPS.LM0`, every play event uses a `0x40000000` reference. Its five events
with a [plain reference](#maps-sm0s-events) are of type `10`, which aren't
play events, and point at a container.

## Event keys aren't always resource keys

An event's key is usually its resource's key too, but not always: in 21 of the
English map's packages, some events play a resource with another key. The game
always goes through the event, so this never shows in game, but matching a
sound's key straight to a resource key puts lines on the wrong clips. Some
examples:

- `S0_0_2Voice`'s events `0x00`–`0x29` play their resources in reverse order,
  so the training lines "All right, Fisher, we'll get through this…" and "We'll
  let you do your thing here…" would land on the intro cutscene's "I like to
  be early. Hello Colonel." and "Sam Fisher. I can't believe you beat me here."
- `S1_1_1Voice`'s `Play_11_10_01` was added late: its event, `0x41`, plays
  resource `0x10`.
- `S2_2_1Voice`'s Mafioso lines play each other's resources in pairs, so
  "Of course." would be shown on a 4.7-second clip, and "Thank god. Make sure
  you leave…" on a 1.7-second one.
- `S2_1_2Voice`'s events `0x68`–`0x6D` play their resources in a small
  rotation, such as `0x6C` playing `0x68`.
- The Vselka maps number `S3_1_1Voice`'s resources differently from the
  language folder's map: `1_7_1_1VselkaInfiltration` has 54 of its 60
  resources for the package numbered past the package's last sound,
  `0x450070`, while its events use the package's keys.

**Use the file name in the resource.** A level can play a sound from another
level's package: level 1_3_2 plays `S1_3_3Voice.Play_13_21_01`, whose audio is
in `1_3_2.LS0`. Guessing the bank from the package name gets that wrong.

## `MAPS.SM0`'s events

`MAPS.SM0`'s resources have the same layout, but most of its events and
containers don't use `0x40000000` references. Their references have no top
bits at all, and are a plain resource index. A few have `0x80000000` instead,
which seem to point at the `fix` area's resources. None of those plays a clip
at a pitch other than 1.0. Its events of types `4` and `10` aren't play or stop
events, and their `+16` isn't a pitch.

A few of its play events set a pitch other than 1.0, and the game applies it:

| Sound | Clips | Pitch |
|---|---|---|
| Laser and metal detector alarm (`Electronic.Play_AlarmeDetecteurLaser`) | `ALALAS_1.wav` | ×1.189 |
| Makarov and VZ61 shots | `MAKASS_*`, `VZ61S*` | ×0.891 |
| Uzi shots | `UZIIS*` | ×0.944 |

This was tested by replacing `ALALAS_1.wav` with a 1,000 Hz tone and tripping
a laser in the Defense Ministry: the game played it at 1,189 Hz, for 0.63 s of
its 0.74 s. The app follows plain references too, so it previews and converts
these clips at the rates the game plays them at.

## How the app lists them

vgmstream lists a map's audio resources, sequences and layers as numbered
entries, named like `1_1_0Tbilisi/0012/01-00130086/TAWCHI02.wav`: the area, the
entry, its type and resource key, and its file name. A map sound's ID is the
map's path and entry number, such as `ENGLISH/MAPS.LM0#1`.

The app sorts each map's sounds into four banks: **Sound effects** (audio stored
in `MAPS.SM0`), **Voices** (audio stored in `MAPS.LM0`, and its `Music_*.LS0`
alerts), **Music** (`Music_*.SS0` and the sequences that play it) and
**Streams** (`STREAM.SS0` and other multi-channel or layered sounds). Entries
whose audio is in a numeric `.LS0` bank are left out, since those banks are
[scanned](sound-banks.md#numeric-banks) and listed on their own.

Each area lists every sound it plays, so most sounds are listed by several
areas. Entries with the same key, name and length are listed once, with every
area that plays them ("Used in"), under the first entry's number. Sounds are
sorted by name. Those whose audio isn't in the map, which only have their
stream file's name, and sequences go last. Listed in
[`sound_maps.rs`](../core/src/banks/sound_maps.rs).
