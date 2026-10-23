# Editing sounds: notes

**Status:** SC Mod Tool replaces dialogue in the numbered `.LS0` banks, and
Lambert's alarm warnings in `Music_Common.LS0`, at the original length or at
another. Sounds stored in the maps as mono 16-bit PCM, most sound effects and
the extra maps' voices, can be replaced at their own length. Other sounds
stored in maps, and the IMA ADPCM codec, can't be replaced yet. These notes collect what's known about
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

These were tested in the game, on English data:

- **A replacement of the same length plays,** in several levels.
- **The game follows an edited `MAPS.LM0`.** A line's resource was pointed at
  another, shorter stream in its bank: its offset, both sizes and its sample
  count. The game played the other stream in the line's place, and showed the
  line's subtitle only for the new stream's length, 0.4 s instead of 8.8 s.
  So nothing checks the map, and a subtitle lasts as long as the audio the
  map gives its line.
- **A replacement of another length plays too,** saved by the app, with the
  bank laid out again and its map changed to match.

## How saving works

A replacement of the same length is spliced into its bank, and nothing else
changes. One of another length goes where the old stream was, followed by
fresh padding to the next 4 bytes, and the streams after it move. Every
resource pointing into the bank is changed to match, in every area of the
`MAPS.LM0` beside it: its offset, both sizes and its sample count. No
`MAPS.SM0` points into a numbered bank, so that's the only map that changes.

`Music_Common.LS0` is saved the same way. Its lines are sounds in the map,
which vgmstream finds by number, so the app reads where each one's stream is
from the map's resources: its key, file, offset and size. Every resource with
its key and file must be Ubisoft ADPCM audio and agree on where the stream is,
and no other sound may play the same stream, or it can't be replaced.

A sound stored in a map as 16-bit PCM is spliced in, at the same length:
another length would move everything after it in the map, which would need its
area offsets and tables rewriting. Each area that plays the sound has its own
copy ([sound maps](sound-maps.md#audio-stored-in-a-map)), so every copy is
replaced. Only mono sounds can be: a few in the extra maps are stereo. The
conversion targets the rate the game plays the sound at, which an event's
pitch can make lower or higher than the rate it's stored at, so that it
plays at the right speed. Saving writes the whole map, which is 218 MB for
`MAPS.SM0`, and saving in place copies it to `MAPS.SM0.orig` first. The
game's maps meet that: in each language's, the audio resources in the file
have 4 keys, one for each stream, repeated across areas. The 36 other keys
naming it are switches' and random choices', whose file name is left over.

This relies on the banks' simple layout ([sound banks](sound-banks.md)). So
before the app accepts a replacement of another length, it checks that only
padding follows the stream, that the map is there, and that every resource
in the bank is at one of its streams. Saving checks again.

Saving builds and checks every file before writing any:

- Each rebuilt bank is scanned again. It must have the same streams, at the
  places expected, and the new ones must decode.
- Each changed map is read back, and every resource must be at its stream in
  the new layout.
- Files are written to a temporary file, then renamed over the old one.
- A map and the banks it describes are written together. If one can't be
  written, those already written are put back as they were.

**Save** writes over the game's files, asking first: once, and again the first
time a save changes a map. Before a file is first changed, it's copied beside
itself with `.orig` added to its name, and later saves keep that copy.
**Save to folder** writes only the changed files, into a folder laid out like
`Sounds`, and leaves the game alone. Exported files from an earlier export
stay in the folder, even if their replacements were reverted since.

## Still to find out

- Does a line of another length cause problems with English lip-sync files,
  which record each line's original duration? Lips may just stop early or
  late.
- Does it accept a resource pointing at a file it didn't ship with?
- Does it play PCM from a resource that was ADPCM?

## Then

1. **Sounds inside maps at another length.** That means rewriting the map's
   area offsets and section 3, or moving the clip out to a separate file.
2. **Ubisoft IMA ADPCM**, for barks, guards' alerts and the other
   `Music_*.LS0` files, needs its own encoder.
3. **Replacing many segments at once.** Before a replacement of another
   length is accepted, the app checks that its stream can move, which reads
   the bank and the `MAPS.LM0` beside it, 35 MB for a language. That's fine
   for one replacement at a time, but a bulk replacement should keep each
   folder's map in memory between checks.

## What the app keeps while it runs

Two things in the app assume the game's files don't change while it's open,
so saving has to work around them:

- **The game cache.** `GameCache` keeps the banks read from each sound map
  and UELib's analysis of the scripts, so switching languages doesn't read
  them again. Saving in place replaces the cache and scans again. A segment
  that moved has a new ID, as IDs hold the stream's offset, so it may not be
  selected again afterwards.
- **Mapped files.** While a scan lists a map's sounds, the map is mapped into
  memory. Truncating it then would crash the app, and Windows won't let a
  mapped file be replaced. So saving waits for scans, scanning waits for
  saves, and files are renamed over, never written in place.
