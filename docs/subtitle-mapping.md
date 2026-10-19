# How segments are matched to subtitles

This explains how SC Mod Tool finds the subtitle text for each audio segment in
a Splinter Cell 1 sound bank, what the game files involved contain, and the
quirks in the shipped data that the matching has to work around.

Everything here was checked against the English and German PC release, and
confirmed by listening where noted. The numbers come from the game-data test
(see [Checking the results](#checking-the-results)).

## The short version

No file links an audio segment to its subtitle. The link has to be rebuilt from
four pieces:

1. **Scripts** (`system/*.u`) play each line by naming a `Sound` object, and for
   radio messages also name the subtitle cue.
2. **Sound packages** (`Sounds/*.uax`) give each `Sound` object a numeric key.
3. **The map** (`Sounds/<language>/MAPS.LM0`) has an event for each key, which
   points at a resource: a bank file, offset and length.
4. **Localization files** (`system/LOCALIZATION/*.int` and so on) hold the text.

Two kinds of subtitle come out of this:

- **Shown in game:** radio messages pass their cue alongside the sound, so the
  link is explicit.
- **Not shown in game:** conversations play the sound without a cue. Their text
  is the line at the same position in the localization section named after the
  scene. The app labels these "Not shown in game".

Result for English: 1085 of 1089 segments are matched to a line. 899 of them
have text; 186 are matched only to lines whose text is a `(null)` placeholder,
which the app shows as "No text". The remaining 4 segments have no line at all
(see [What's left](#whats-left)). These numbers are for the language's own
folder; the [extra maps](#extra-maps-soundsmaplanguage) and a few of the map's
own sounds add more.

## The files

### Sound banks: `Sounds/<language>/*.LS0`

Each numeric bank (`0_0_2.LS0`, `1_1_2.LS0`, …) is a sequence of mono 4-bit
Ubisoft ADPCM streams, found by scanning for a stream header (`0x30` bytes)
followed by frames (`0x34`-byte headers). Streams are padded to 4-byte
boundaries. A segment's `byte_length` is the stream itself, without that
padding, because `MAPS.LM0` records the unpadded size.

A level's lines are usually stored together, in roughly the order they're
spoken, which is a useful clue when checking a match by hand.

### Extra maps: `Sounds/<map>/<language>/`

Three extra levels keep their own sounds in folders beside the languages:
`1_6_1_1kolacell`, `1_7_1_1VselkaInfiltration` and `1_7_1_2Vselka`. Each has a
folder per language it ships (`English`, `French`, `Italien`; no German or
Spanish), with its own banks and `MAPS.LM0`. Their banks differ from the
language folder's in two ways:

- **6-bit codes.** Their streams use 6 bits per code instead of 4. The layout is
  otherwise the same, and vgmstream decodes both.
- **Reused names.** Both Vselka folders have a `3_1_1.LS0`, with different
  audio. A map's records only describe its own banks, so each folder's banks
  are matched against its own `MAPS.LM0`
  ([`resolve_segments`](../core/src/subtitles/map.rs)). A sound found in several
  maps gets its subtitle on each copy.

The app lists these banks under the language, grouped by map. They hold the
only copies of mission 3 audio (`S3_*` packages) in this release.

**Different resource keys.** The Vselka maps file `S3_1_1Voice`'s audio under
other resource keys than the language folder's map: `1_7_1_1VselkaInfiltration`
has 54 of its 60 resources for the package numbered past the package's last
sound, `0x450070`. Their events still use the package's keys, so matching
through the events finds them. Before events were read, these lines couldn't be
matched at all, and the keys that did overlap pointed at other lines.

### The map: `Sounds/<language>/MAPS.LM0`

The map is split into areas, one per level plus `fix` and `menu`. Each area has
a list of **events**, which the game plays, and a list of **resources**, which
describe the audio. The keys the sound packages store are event keys. An event
plays a resource by its index in the area's list, so the game never looks a
resource up by key.

All offsets are little-endian 32-bit values:

| Where | Offset | Field |
|---|---|---|
| File | `+0` | Version, `7` |
| | `+4`, `+8` | Offset and number of areas |
| Area entry (`0x30` bytes) | `+8` | Offset of the area's header |
| | `+16` | Area name, such as `1_1_0Tbilisi` |
| Area header | `+4`, `+8` | Offset (from the header) and number of events |
| | `+12`, `+16` | Offset (from the header) and number of resources |
| Event (`0x58` bytes) | `+0` | Key: `(package ID << 16) \| sound ID` |
| | `+4` | Type: `1` plays, `2` stops |
| | `+8` | `0x40000000` \| the resource's index in the area |
| | `+16` | Pitch, as a 16.16 fixed-point multiple of the sample rate |
| Resource (`0x80` bytes) | `+0` | The resource's own key |
| | `+4` | Type: `1` for a clip of audio |
| | `+12` | Stream size in bytes, repeated at `+52` |
| | `+20` | Offset of the stream in its bank |
| | `+48` | Sample count |
| | `+68` | Sample rate (36000) |
| | `+80` | Bank file name, such as `1_3_2.LS0`, null-terminated, up to 32 bytes |

Dialogue's events point straight at audio resources (type `1`), so only those
are used for subtitles. The other types are containers, which characters'
barks are chosen through: `10` plays one of its resources at random, `8` one
chosen by a switch, and `4` several in sequence, such as a radio click, a line
and a walkie-talkie burst. Each lists its resources in the area's extra section,
starting at its `+16`: random containers in 12-byte entries counted at `+28`,
switches in 8-byte entries counted at `+28`, and sequences in 4-byte entries
counted at `+44`. Each entry starts with a `0x40000000` reference.

Events' pitch matters for barks: the Chinese and Georgian soldiers', the Elite
Force's and the Ex-Spetsnaz's are stored at 16,000 Hz but played at two thirds
of that, 10,678 Hz, so the app plays them at that rate too. Everything else is
played at its stored rate.

Parsed in [`sound_map_file.rs`](../core/src/sound_map_file.rs), and for
subtitles in `parse_map_records` in [`map.rs`](../core/src/subtitles/map.rs).

An event's key is usually its resource's key too, but not always; see
[Events and resources](#events-and-resources).

**Use the bank name in the record.** A level can play a sound from another
level's package: level 1_3_2 plays `S1_3_3Voice.Play_13_21_01`, whose audio is
in `1_3_2.LS0`. Guessing the bank from the package name gets that wrong, so the
package name is only a fallback for records without a bank name.

### Sound packages: `Sounds/*.uax`

Each `Sound` export's serialized data is:

1. one zero byte (an empty property list)
2. the 32-bit key
3. for lip-synced sounds: a 32-bit `1`, then a length-prefixed, null-terminated
   path such as `\S0_0_2Voice\00_15_01.bin`

Parsed in [`SoundData.cs`](../core/uelib-bridge/SoundData.cs). Only the key
is used.

### Scripts: `system/*.u`

Scene logic lives in "pattern" classes in `EchelonPattern.u`, such as
`P_1_1_2_Tbilisi_Coronary`, decompiled with UELib through the
[bridge](../core/uelib-bridge/Exports.cs). Sounds are played through three
native functions (the decompiler shows only their numbers):

| Function | Used for | Example |
|---|---|---|
| `__NFUN_1407__` | Radio messages, with a subtitle | `__NFUN_1407__(Localize("P_0_0_2_Training_Comms", "Speech_0012L", "Localization\\P_0_0_2_Training"), Sound'S0_0_2Voice.Play_00_20_01', …)` |
| `__NFUN_1408__` | Conversation lines, without a subtitle | `__NFUN_1408__(Sound'S1_1_2Voice.Play_11_32_24', 0, …)` |
| `__NFUN_264__` | `PlaySound` on an actor: effects, PA announcements, the far end of phone calls | `SoundActors[0].__NFUN_264__(Sound'DestroyableObjet.Play_ExplosionBridge', 0)` |

[`DirectPairs.cs`](../core/uelib-bridge/DirectPairs.cs) finds `Localize` +
`Sound` pairs, and every sound call in script order with the function it's
passed to.

### Localization: `system/LOCALIZATION/*.<extension>`

INI-style files in Windows-1252. The extension is the language: `int`
(English), `fra`, `deu`, `ita`, `esp`. Each section is named after a pattern
class:

```ini
[P_1_1_2_Tbilisi_Coronary]
Speech_0020L=One false move and you're dead. I need information.
Speech_0021L=Um...Okay.
...
```

- `Speech_…` lines are spoken. `Note_…` and `Goal_…` lines are on-screen text
  and are never voiced.
- Cue numbers don't follow the order lines are spoken, or the sound numbers.
  `Play_11_32_24` is `Speech_0020L`.
- A section can repeat a cue name with different text (`Coronary` has two
  `Speech_0028L` lines), so lines must be read in file order, not looked up by
  name alone.
- A line whose text is `(null)` is a placeholder for a line without text. It
  still takes its place in the order and is matched to its audio, but its
  `Subtitle::text` is `None`, so the placeholder never reaches the app as
  dialogue.
- Other languages' sections usually have the same lines in the same order as
  English, but can keep extra leftover lines. German
  `[P_2_1_2_CIA_EndingP2]` has the 37 English lines plus 4 older drafts at the
  end.

## Matching

All of this happens once per game when subtitles are first loaded, in
[`load_verified_subtitles`](../core/src/subtitles.rs). The UELib analysis doesn't
depend on language, so it's cached and reused when switching languages.

### Subtitles shown in game

For each `Localize` + `Sound` pair: find the event with the sound's key in
`MAPS.LM0`, follow it to its resource, find the segment in the named bank with
the same offset, size and sample count, and read the cue's text from the
language's localization file. Audio outside the numeric banks, such as
Lambert's alarm warnings in `Music_Common.LS0`, isn't scanned, so it's matched
to the map sound with the resource's key instead (see the app's "Voices" banks).
421 of the 657 pairs match a segment in English. Most of the rest are mission 3
lines (`S3_*` packages) whose audio this release doesn't ship, or whose
localization files it doesn't ship (see [What's left](#whats-left)).

### Subtitles not shown in game

For each pattern that plays conversation lines without a cue
(`match_unshown_subtitles`):

1. Take the pattern's **lines of dialogue**: calls to `1407` or `1408`. Sound
   effects and background voices played through `264` don't have lines.
2. Take the **spoken lines** (`Speech_…`) of the English localization section
   with the pattern's name, in file order. Scenes are aligned against English
   even when another language is selected, because other languages can have
   leftover lines that would shift every position after them.
3. Line them up by position. The match is only made if every radio message
   (which names its cue) lands on its own cue's line, which checks the
   alignment.
4. If the section has **more** lines than the scene speaks (a line was cut, or
   is played some other way), try each way of skipping the extra lines. Keep the
   one where the speaking rate (characters per second of clip, ignoring speaker
   names and stage directions) is most even, but only if it's clearly better
   than the next best and there are at most 200 options.
5. Look each matched line up in the selected language by cue name, using the
   same occurrence when a cue name repeats.
6. If English doesn't line up, the selected language's own section is aligned
   the same way. If that fails too (fewer lines than calls, or an unclear
   choice), the scene is skipped with a warning.

Example: `[P_4_2_1_Abt_WaterDraining]` starts with "Whats that?", which no script
plays. Skipping it gives rates of 17, 13 and 16 characters per second; any other
choice puts "Whats that?" on a 2.2-second clip at 4.5 characters per second.

Every scene with a localization section now lines up, in both English and
German: 734 lines not shown in game are matched in each language's folder, and
the extra maps add 91 in English.

## Events and resources

An event's key is usually its resource's key too, but not always: in 21 of the
English map's packages, some events play a resource with another key. The game
always goes through the event, so this never shows in game, but matching a
sound's key straight to a resource key puts those lines on the wrong clips.
Some examples:

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
  language folder's map (see [Extra maps](#extra-maps-soundsmaplanguage)).

Before events were read, the tool matched keys straight to resources, and
guessed fixes for a few packages from lip-sync durations, which only some lines
have. Reading the events moved 14 more English lines onto other clips,
all in pairs or runs without lip-sync, including the Mafioso lines above. In
every pair whose lines differ in length, the clip lengths now fit the text.

## Known quirks

- **Re-recorded lines.** Some audio doesn't match its script text or lip-sync:
  `0_0_3.LS0` #1 says "Hi." but its text and lip-sync are "Good Lord! Where did
  you come from?"; `4_3_1.LS0` #2 ("Yes, sir.") and `5_1_2.LS0` #34 have
  lip-sync files of the wrong length. The app shows the game's text.
- **Shared clips.** `0_0.LS0` #1 ("Hi there.") is played by both the training
  comms (`Speech_0002L`, shown) and the interrogation scene (`Speech_0007L`, not
  shown), so it has two subtitles with the same text.
- **Sounds played twice.** A scene can play the same sound in two branches with
  a section line for each, such as "28469" in the interrogation scene.
- **Placeholder text.** Some unshown lines are placeholders, such as "Put Grinko
  on, etc" for a 27.7-second phone call (`1_1_1.LS0` #34). Many more are just
  `(null)`.
- **Untranslated lines.** German `[P_2_1_2_CIA_EndingP2]` keeps one line in
  English under its cue name (`Speech_0036L`, "Thank god, I though he would
  never go away.", `2_1_2.LS0` #55); its translation sits under the leftover
  cue names at the end of the section. Matching by cue name shows the English
  line, as the files say.

## What's left

Segments fall into three groups, which the app's segment filter ("All
segments", "With text", "Without text") and status bar distinguish:

| | English | German | In the app |
|---|---|---|---|
| Matched to a line with text | 899 | 901 | The text: normal for subtitles the game shows, dim italic for the rest |
| Matched only to lines without text | 186 | 184 | "No text" with the cue name; the inspector shows an empty "No text yet" box |
| No line at all | 4 | 4 | "—" |

The lines without text are still useful: each one is a known line in a known
scene, with its cue name and audio, ready for text to be written later.

The extra maps add 134 English segments: 31 with text, 90 with lines without
text, and 13 with no line. Those 13 are `1_7_1_1VselkaInfiltration/English/3_1_1.LS0`
#25–#37, which the map's events play but no script line or localization
section covers. German has no extra map folders.

The map's own sounds add Lambert's lines in `Music_Common.LS0`: the three alarm
warnings and "My God, Fisher! Have you gone insane?" in the language folder's
map, and the alarm warnings again in each extra map. That's 13 sounds with
subtitles in English and 4 in German.

Mission 3's own scenes (`P_3_4_1Severonickel` to `P_3_4_4Severonickel`) cue
radio messages from localization files this release doesn't ship, so each
language with extra maps gets four "Localization file … was not found"
warnings. Their 28 cues point at 16 segments. 15 of those are in
`1_6_1_1kolacell`, whose own scenes play the same sounds and do have
localization, so they get their lines from there. The one left,
`1_7_1_2Vselka/English/3_4_3.LS0` #1, gets its line from another scene: its
event is `Play_34_61_11` ("Watch out Fisher…"), not `Play_34_64_02`, the sound
whose key its resource has.

The 4 segments with no line in any localization file:

| Segment | What it is |
|---|---|
| `2_1.LS0` #2–#4 | PA announcements in the CIA building (`P_2_1_1_CIA_AmbientVoice`), which has no localization section |
| `2_1_1.LS0` #24 | The muffled far end of the CIA phone call. Its scene's section has one line, for the side you hear directly (#23). |

No scene is skipped in either language.

## Checking the results

The game-data test runs the whole pipeline on a real installation and checks
specific lines that were confirmed by listening, such as `Speech_0012L` on
`0_0_2.LS0` #6 and "Put Grinko on, etc" on `1_1_1.LS0` #34:

```sh
cargo test -p sc-mod-tool-core --test game_audio -- --ignored --nocapture
```

When changing the matching, run it and compare its summary line (currently
"943 segments with text and 276 with lines that have no text" for English,
including the extra maps and map sounds) and the warnings it prints. A new rule that
moves a confirmed line will fail the test.

To check a match by ear, open the bank in the app and play the segment. A
scene's lines usually sit next to each other in the bank in the order they're
spoken, so a misplaced line tends to stand out.
