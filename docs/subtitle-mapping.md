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
3. **The map** (`Sounds/<language>/MAPS.LM0`) gives each key a bank file, offset
   and length.
4. **Localization files** (`system/LOCALIZATION/*.int` and so on) hold the text.

Two kinds of subtitle come out of this:

- **Shown in game:** radio messages pass their cue alongside the sound, so the
  link is explicit.
- **Not shown in game:** conversations play the sound without a cue. Their text
  is the line at the same position in the localization section named after the
  scene. The app labels these "Not shown in game".

On top of that, a few sound packages store wrong keys, which are corrected using
lip-sync data.

Result for English: 1085 of 1089 segments are matched to a line. 899 of them
have text; 186 are matched only to lines whose text is a `(null)` placeholder,
which the app shows as "No text". The remaining 4 segments have no line at all
(see [What's left](#whats-left)). These numbers are for the language's own
folder; the [extra maps](#extra-maps-soundsmaplanguage) add more.

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
  ([`resolve_segments`](../core/src/subtitles.rs)). A sound found in several
  maps gets its subtitle on each copy.

The app lists these banks under the language, grouped by map. They hold the
only copies of mission 3 audio (`S3_*` packages) in this release.

**Another version of `S3_1_1Voice`.** The Vselka maps were made with a
different version of `S3_1_1Voice` than the game ships. The game's
`S3_1_1Voice.uax` has sounds `0x450000`–`0x450070`, but
`1_7_1_1VselkaInfiltration`'s map files 60 sounds under the package's ID, 54 of
them numbered past the end of that range. The keys that do overlap point at
other lines: the map's `0x45006F` clip is 0.69 s, but the lip-sync file for
`Play_31_38_02`, which the game's package gives that key, is 1.74 s. Matching
those keys put Lambert's "GPS shows you're within a stone's throw…" on a 3-second
clip.

So when most of a map's records under a package's ID are for sounds the package
doesn't have, that package isn't matched in that map, with a warning
(`mark_foreign_packages`). This only affects `S3_1_1Voice` in the two Vselka
maps, and `Lambert`, which doesn't match in any map. Every other match in the
extra maps that has a lip-sync file agrees with it.

### The map: `Sounds/<language>/MAPS.LM0`

A table of 128-byte records, one per sound. The fields used:

| Offset | Field |
|---|---|
| `+0` | Key: `(package ID << 16) \| sound ID` |
| `+4`, `+8` | Both `1` in every record used |
| `+12` | Stream size in bytes, repeated at `+52` |
| `+20` | Offset of the stream in its bank |
| `+48` | Sample count |
| `+68` | Sample rate (36000) |
| `+80` | Bank file name, such as `1_3_2.LS0`, null-terminated, up to 32 bytes |

The parser finds records by scanning every 4-byte position for this shape
(`parse_map_records` in [`subtitles.rs`](../core/src/subtitles.rs)).

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

Parsed in [`SoundData.cs`](../core/uelib-bridge/SoundData.cs).

### Lip-sync files: `LIPSYNCH/int/<package>/<line>.bin`

One file per lip-synced sound, named after it without the `Play_` prefix
(`Play_00_15_01` → `00_15_01.bin`). The 32-bit value at `+8` is the line's
duration in milliseconds. Only English (`int`) lip-sync data ships, so it's
compared against English clip lengths.

These durations are the only independent record of which audio belongs to which
sound, which is what makes them useful for correcting keys. They're not
perfect: some lines were re-recorded after the lip-sync was made (see
[Known quirks](#known-quirks)).

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
[`load_verified_subtitles`](../core/src/subtitles.rs). The UELib analysis and key
corrections don't depend on language, so they're cached and reused when
switching languages.

### Subtitles shown in game

For each `Localize` + `Sound` pair: correct the sound's key if needed, look the
key up in `MAPS.LM0`, find the segment in the named bank with the same offset,
size and sample count, and read the cue's text from the language's localization
file. 390 of the 657 pairs match a segment in English: 381 in the language
folder and 9 in the extra maps. Almost all of the rest are mission 3 lines
(`S3_*` packages). The extra maps only ship the mission 3 sounds their own
levels use, and the Vselka maps' `S3_1_1Voice` sounds can't be matched (see
[Extra maps](#extra-maps-soundsmaplanguage)).

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
German: 734 lines not shown in game are matched in each.

## Correcting scrambled sound keys

In a few packages, the key stored in a `Sound` object points at the wrong audio.
The names and lip-sync paths in those objects are right; only the keys are
wrong, and the game must work around it internally. Lip-sync durations, bank
order and listening all agree on the correct keys.

[`sound_keys.rs`](../core/src/sound_keys.rs) applies four corrections, in this
order. The first three need lip-sync durations (within 0.15 s) to clearly
favour the change over the stored keys:

| Kind | What's wrong | Found in |
|---|---|---|
| Reversed run | A run of consecutive keys was assigned in reverse name order | `S0_0_2Voice` (`0x00`–`0x29`, 41 sounds), `S2_2_2Voice` (`0x5D`–`0x62`) |
| Swapped pair | Two neighbouring exports have each other's keys | `S1_1_2Voice` (`Play_11_28_09` ↔ `Play_11_32_27`), `S2_1_1Voice` (`Play_21_15_01` ↔ `Play_21_16_01`) |
| Shifted onto a free key | A sound's audio is under the next key, which no sound owns | `S2_1_2Voice` (`Play_21_96_01`: `0x6D` → `0x6E`) |
| Late addition | A sound added last points at a key with no audio, while its audio sits under the package's only key that no sound owns. Applied only when there's exactly one of each. | `S1_1_1Voice` (`Play_11_10_01`: `0x41` → `0x10`), `S2_1_2Voice` (`Play_21_10_18`: `0x6C` → `0x6D`) |

Each correction is listed in the app's warnings window. Without them, for
example, the stored keys of the training lines "All right, Fisher, we'll get
through this…" and "We'll let you do your thing here…" point at the intro
cutscene's "I like to be early. Hello Colonel." and "Sam Fisher. I can't
believe you beat me here.", so those subtitles landed on the wrong clips.

Lip-sync alone isn't trusted for single sounds: in three places the lip-sync file
is simply out of date (below), and the stored key is right.

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

The extra maps add 134 English segments: 10 with text, 47 with lines without
text, and 77 with no line. Most of the 77 are the Vselka maps' `S3_1_1Voice`
sounds, which use another version of the package's keys. German has no extra
map folders.

The 4 segments with no line in any localization file:

| Segment | What it is |
|---|---|
| `2_1.LS0` #2–#4 | PA announcements in the CIA building (`P_2_1_1_CIA_AmbientVoice`), which has no localization section |
| `2_1_1.LS0` #24 | The muffled far end of the CIA phone call. Its scene's section has one line, for the side you hear directly (#23). |

No scene is skipped in either language.

The key corrections only work where lip-sync data or the "one of each" rule
applies. A package with scrambled keys and no lip-synced sounds nearby would go
unnoticed, though none is known.

## Checking the results

The game-data test runs the whole pipeline on a real installation and checks
specific lines that were confirmed by listening, such as `Speech_0012L` on
`0_0_2.LS0` #6 and "Put Grinko on, etc" on `1_1_1.LS0` #34:

```sh
cargo test -p sc-mod-tool-core --test game_audio -- --ignored --nocapture
```

When changing the matching, run it and compare its summary line (currently
"899 segments with text and 186 with lines that have no text" for English) and the warnings it prints. A new rule that
moves a confirmed line will fail the test.

To check a match by ear, open the bank in the app and play the segment. A
scene's lines usually sit next to each other in the bank in the order they're
spoken, so a misplaced line tends to stand out.
