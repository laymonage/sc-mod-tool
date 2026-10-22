# Editing sounds: notes

**Status:** SC Mod Tool replaces dialogue in the numbered `.LS0` banks, at
the original length or at another. Sounds stored in maps, and the other
codecs, can't be replaced yet. These notes collect what's known about
replacing sounds. They build on [sound maps](sound-maps.md) and
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

## What the game showed

Both were tested in the game, on English data:

- **A replacement of the same length plays,** in several levels. It's spliced
  into its bank, and nothing else changes.
- **The game follows an edited `MAPS.LM0`.** A line's resource was pointed at
  another, shorter stream in its bank: its offset, both sizes and its sample
  count. The game played the other stream in the line's place, and showed the
  line's subtitle only for the new stream's length, 0.4 s instead of 8.8 s.
  So nothing checks the map, and a subtitle lasts as long as the audio the
  map gives its line.

That's what replacing at another length relies on. The new stream goes where
the old one was, the streams after it move, and every resource pointing into
the bank is changed to match, in every area that has it.

## Still to find out

- Does a line of another length cause problems with English lip-sync files,
  which record each line's original duration? Lips may just stop early or
  late.
- Does it accept a resource pointing at a file it didn't ship with?
- Does it play PCM from a resource that was ADPCM?

## Then

1. **Sounds in other files.** `Music_Common.LS0`, Lambert's alarm lines, uses
   the same codec as dialogue, so it can be replaced the same way.
2. **Sounds inside maps.** A clip of the same length can be spliced in.
   Another length means rewriting the map's area offsets and sizes, or moving
   the clip out to a separate file.
3. **Ubisoft IMA ADPCM**, for barks and guards' alerts, needs its own
   encoder.
4. **Replacing many segments at once.** Before a replacement of another
   length is accepted, the app checks that its stream can move, which reads
   the bank and the `MAPS.LM0` beside it, 35 MB for a language. That's fine
   for one replacement at a time, but a bulk replacement should keep each
   folder's map in memory between checks.

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
