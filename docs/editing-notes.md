# Editing sounds: notes

**Status: not implemented.** SC Mod Tool only reads the game's files. These
notes collect what's known so far about replacing sounds, for when that work
starts. They build on [sound maps](sound-maps.md) and
[sound banks](sound-banks.md).

## What the file formats allow

**Every sound is reached through a map.** An event points at a resource, and
the resource gives the file holding the audio, its offset and size, sample
count, sample rate and codec. So a replacement doesn't have to overwrite the
original audio: it can be written somewhere new, with the resource updated to
point there.

**Resources name their file.** Replacements could go in a new bank file, such
as a hypothetical `MOD.LS0`, with only the map changed. The original banks stay
as they are.

**Three codecs are used:**

| Codec | Used for | Writing it |
|---|---|---|
| 16-bit PCM | Sound effects; the extra maps' voices | Easy: no encoding |
| Ubisoft IMA ADPCM | Barks, guards' alerts | A known IMA variant, so an encoder is manageable |
| Ubisoft 4/6-bit ADPCM | Dialogue, Lambert's lines, music | Hardest: vgmstream only decodes it, so an encoder would have to be written, with its decoder as the specification |

If the game plays PCM from any resource, which the extra maps' PCM voices
suggest, early replacements could be PCM, at the cost of larger files.

**Sounds stored inside a map are harder** than those in separate files.
Changing their size shifts everything after them, so the map's area offsets
and sizes would need rewriting. Pointing their resources at a separate file
would avoid that, if the game allows it.

## What only the game can answer

- Does it check maps or banks for tampering, such as checksums, or sizes in
  `Echelon.SP0`?
- Does it accept a resource pointing at a file it didn't ship with?
- Does it play PCM from a resource that was ADPCM?
- Does a line of a different length cause problems, for example with English
  lip-sync files, which record each line's original duration?

## First experiment: an edited map

The cheapest test answers the first question: does the game accept an edited
`MAPS.LM0`? It needs no new audio, only a change to which existing clip an
event plays.

In `Sounds/ENGLISH/MAPS.LM0`, the Training area (`0_0_2_Training`, the first
area) has Lambert's three alarm warnings:

| Event | Reference at the event's `+8` | Resource | Line |
|---|---|---|---|
| `0x00660005` (`Lambert.Play_AlarmStage1Opt1`) | `0x40000000` | `0x00660002` | "This operation is based around stealth, watch the alarms." |
| `0x00660003` (`Play_AlarmStage1Opt2`) | `0x40000001` | `0x00660001` | "That's our limit, one more alarm and I'm aborting the mission." |
| `0x00660004` (`Play_AlarmStage1Opt3`) | `0x40000002` | `0x00660000` | "That's one alarm too many, the mission's over." |

On a copy of the map, change event `0x00660005`'s reference from `0x40000000`
to `0x40000002`, then back up the original, put the copy in its place, and play
Training. If the first alarm plays "That's one alarm too many…" with the first
warning's subtitle, the game accepts edited maps. If it plays the original line
or refuses to load, something checks the map.

Each area has its own events, so this only changes Training.

## Then

1. **One replacement clip in a new file.** Write a PCM clip to a new file, and
   point one resource at it, with its offset, size, sample count, sample rate,
   codec (`1` for PCM) and file name updated.
2. **Encoders.** Ubisoft IMA ADPCM, then Ubisoft ADPCM, to keep replacements
   in the original codecs and sizes.
3. **Sounds inside maps.** Rewriting maps whose sizes change, or moving those
   sounds out to separate files.
4. **The app.** An "Export" and "Replace" action for each segment. The
   inspector already reserves a place for them.

## What the app keeps while it runs

Two things in the app assume the game's files don't change while it's open,
and editing will break both:

- **The game cache.** `GameCache` keeps the banks read from each sound map
  and UELib's analysis of the scripts, so switching languages doesn't read
  them again. Only Rescan or opening another game folder clears it. Saving a
  map, a bank or a script package has to clear it too, or the app keeps
  showing what was there before.
- **Mapped files.** While a scan lists a map's sounds, the map is mapped into
  memory. Truncating it then would crash the app, and Windows won't let a
  mapped file be replaced. A save shouldn't run during a scan, and writing to
  a new file and renaming it over the old one is safer than writing in place.
