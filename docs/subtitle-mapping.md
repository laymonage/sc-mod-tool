# How segments are matched to subtitles

This explains how SC Mod Tool finds the subtitle text for each audio segment,
and the quirks in the shipped data that the matching has to work around. The
files involved are described in [the other docs](README.md).

Everything here was checked against the English and German PC release, and
confirmed by listening where noted. The numbers come from the game-data test
(see [Checking the results](#checking-the-results)).

## The short version

No file links an audio segment to its subtitle. The link has to be rebuilt from
four pieces:

1. **Scripts** (`system/*.u`) play each line by naming a `Sound` object, and for
   radio messages also name the subtitle cue
   ([scripts](scripts-and-localization.md#scripts-systemu)).
2. **Sound packages** (`Sounds/*.uax`) give each `Sound` object a numeric key
   ([packages](scripts-and-localization.md#sound-packages-soundsuax)).
3. **The map** (`Sounds/<language>/MAPS.LM0`) has an event for each key, which
   points at a resource: a bank file, offset and length
   ([sound maps](sound-maps.md)).
4. **Localization files** (`system/LOCALIZATION/*.int` and so on) hold the text
   ([localization](scripts-and-localization.md#localization-systemlocalizationextension)).

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
folder; the [extra maps](README.md#extra-maps) and a few of the map's own
sounds add more.

## Matching

All of this happens once per game when subtitles are first loaded, in
[`load_verified_subtitles`](../core/src/subtitles.rs). The UELib analysis doesn't
depend on language, so it's cached and reused when switching languages.

### Subtitles shown in game

For each `Localize` + `Sound` pair: find the event with the sound's key in
`MAPS.LM0`, follow it to its resource (an event's key
[isn't always its resource's](sound-maps.md#event-keys-arent-always-resource-keys)),
find the segment in the named bank with the same offset, size and sample count,
and read the cue's text from the language's localization file.

Audio outside the numeric banks, such as Lambert's alarm warnings in
`Music_Common.LS0`, isn't scanned, so it's matched to the map sound with the
resource's key instead (see the app's "Voices" banks). Each extra map's banks
are matched against its own `MAPS.LM0`
([`resolve_segments`](../core/src/subtitles/map.rs)), and a sound found in
several maps gets its subtitle on each copy.

421 of the 657 pairs match a segment in English. Most of the rest are
[mission 3](README.md#mission-codes) lines (`S3_*` packages) whose audio this
release doesn't ship, or whose localization files it doesn't ship (see
[What's left](#whats-left)).

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

### Matching through events

Before events were read, the tool matched keys straight to resources, and
guessed fixes for a few packages from lip-sync durations, which only some lines
have. Reading the events moved 14 more English lines onto other clips, all in
pairs or runs without lip-sync, including `S2_2_1Voice`'s Mafioso lines
([examples](sound-maps.md#event-keys-arent-always-resource-keys)). In every
pair whose lines differ in length, the clip lengths now fit the text.

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
