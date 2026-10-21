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

### Stream layout

What's below holds for every stream in the game's numeric banks, and the
app's [encoder](../core/src/encoder.rs) writes streams the same way. A
stream's layout depends only on its sample count, so a replacement with the
same number of samples has the same size.

The header's fields are little-endian 32-bit:

| Offset | Field |
|---|---|
| `+0x00` | `8` |
| `+0x04` | Sample count, which is also the number of codes |
| `+0x08` | Number of subframes: the sample count divided by 1,536, rounded up |
| `+0x0C` | Codes in the last subframe |
| `+0x10` | Codes in the other subframes, always 1,536 |
| `+0x14` | Subframes per frame, `2` |
| `+0x18` | Sample rate, always 36,000 |
| `+0x1C` | Usually `1`; otherwise values that look like leftover memory addresses, unrelated to the stream |
| `+0x20` | `0` |
| `+0x24` | Bits per code, `4` or `6` |
| `+0x28` | `1` for 4-bit streams, `3` for 6-bit |
| `+0x2C` | Channels, `1` |

Each frame starts with the decoder's state (`0x34` bytes, signature `2`, with
its unused fields zero), then up to two subframes. A subframe of `n` codes
takes `n × bits / 8 + 1` bytes. Codes are one stream of bits, most
significant first, stored as 32-bit little-endian words, so a 6-bit code can
span two words. The spare byte is zero.

When a stream's last subframe ends partway through a word, its last bytes
may not hold the whole word. If they do, the word is written as usual, with
the unused bits zero. Otherwise the bits are written in order, high bits
first. vgmstream reads a whole word there anyway, so it decodes the last few
samples of such a stream from the bytes after it.

Almost every stream starts with the same state: a step of `0x500` and
everything else zero. The decoder reloads the state at every frame, so the
encoder writes its own state there as it goes.

The 6-bit decoder only uses the state's step, `hist1` and latest delta. The
other fields are zero, except `+0x08`: 35 of the 6-bit streams set it to a
small number of their own, such as `2` or `-3`, in every frame. The decoder
ignores it, and the encoder writes `0`.

4-bit streams use codes 0 to 14, and 6-bit streams codes 1 to 61. The codes
left out reach steps past the decoders' tables, except 6-bit codes 0 and 62.
Those make the largest change but then shrink the step, so an encoder that
used them would fall behind a rising or falling sound.

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
