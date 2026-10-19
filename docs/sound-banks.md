# Sound banks

The files that hold the game's audio. None of them has an index of its own:
the [sound maps](sound-maps.md) say what's in them and where, except in the
numeric `.LS0` banks, which the app scans. vgmstream decodes everything.

## Numeric banks

`Sounds/<language>/<code>.LS0`, such as `0_0_2.LS0` and `1_1_2.LS0`: a level's
voice lines, in the language's own folder.

Each bank is a sequence of mono 4-bit Ubisoft ADPCM streams, found by scanning
for a stream header (`0x30` bytes) followed by frames (`0x34`-byte headers). A
segment is one stream, identified by the bank's path and the stream's offset,
such as `ENGLISH/2_2_1.LS0:1912308`. Streams are padded to 4-byte boundaries. A
segment's `byte_length` is the stream itself, without that padding, because the
maps record the unpadded size. Scanned in
[`ls0.rs`](../core/src/banks/ls0.rs).

A level's lines are usually stored together, in roughly the order they're
spoken, which is a useful clue when checking a match by hand.

The [extra maps](README.md#extra-maps)' banks differ in two ways:

- **6-bit codes.** Their streams use 6 bits per code instead of 4. The layout is
  otherwise the same.
- **Reused names.** Both Vselka folders have a `3_1_1.LS0`, with different
  audio. Each map's [sound map](sound-maps.md) only describes its own banks.

## `Music_*.LS0`

`Sounds/<language>/Music_*.LS0` aren't music, and aren't scanned: the
[sound maps](sound-maps.md) list their sounds, and the app shows them in the
"Voices" banks.

| File | Holds | Codec |
|---|---|---|
| `Music_Common.LS0` | Lambert's three alarm warnings, and "My God, Fisher! Have you gone insane?" | Mono Ubisoft ADPCM, 36,000 Hz |
| `Music_Birmanie.LS0`, `Music_Georgie.LS0`, `Music_Usa.LS0` | Guards' alerts, such as on finding a body, one set per region | Mono Ubisoft IMA ADPCM, 36,000 Hz |

The extra maps' language folders have their own `Music_Common.LS0` and
`Music_Georgie.LS0`.

## Stream files: `.SS0`

| File | Holds |
|---|---|
| `Music_Birmanie.SS0`, `Music_Common.SS0`, `Music_Georgie.SS0`, `Music_Usa.SS0` | Music, in stereo 4-bit Ubisoft ADPCM at 36,000 Hz. |
| `STREAM.SS0` | Long streams of up to about 8 minutes, in Ubisoft IMA ADPCM with 6 channels, as three stereo layers. Whether the game plays the layers together isn't known; the app mixes them into stereo, scaled down if the sum would clip. |
| `1_3_3.SS0` | One mono Ubisoft IMA ADPCM stream. |
| `Common/Music_Common.SS0`, `Common/Music_Georgie.SS0` | The extra maps' music. |

Each extra map has its own `STREAM.SS0`.
