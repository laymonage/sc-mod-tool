# SC Mod Tool

A read-only browser for Splinter Cell 1 sound banks, built with Rust and egui.
It lists the mono 4-bit and 6-bit Ubisoft ADPCM segments in each numeric `.LS0` bank
and shows their subtitles. It also lists the sound effects, character voices,
music and streams in the game's sound maps (`MAPS.SM0` and each language's
`MAPS.LM0`), each sound once with the levels that use it. vgmstream decodes
everything. Game files are never modified.

Subtitles come from the game's compiled UnrealScript and localization files,
matched to segments through `.uax` sound keys and `MAPS.LM0`. This includes
lines the game plays without showing a subtitle. See
[docs/subtitle-mapping.md](docs/subtitle-mapping.md) for how the matching works
and the quirks in the game data it works around, and [docs](docs/README.md) for
the game's sound files.

The `core` crate has no UI code, so it can back CLIs too.

## Building

You need Rust ([rustup](https://rustup.rs)), the
[.NET 10 SDK](https://dotnet.microsoft.com/download), and a C toolchain:

- **Windows:** Visual Studio Build Tools with "Desktop development with C++"
- **macOS:** `xcode-select --install`
- **Linux:** `sudo apt install build-essential clang zlib1g-dev pkg-config libasound2-dev`
  (under WSL, install the Linux .NET SDK inside the distribution)

```sh
git submodule update --init --recursive
cargo test
dotnet test core/uelib-bridge-tests    # the bridge's script matcher
cargo run -- "/path/to/Splinter Cell"  # the folder argument is optional
```

The .NET SDK builds the UELib bridge used for subtitles. It can't be
cross-compiled to another OS. To build without it, pass
`--no-default-features`; everything except subtitles still works.

The bridge library (`SCModUELibBridge.dll`, `libSCModUELibBridge.dylib` or
`libSCModUELibBridge.so`) is copied next to the executable and must ship with
it.

When redistributing, include the license notices of the third-party code built
into the app and the bridge:

- **vgmstream**, which decodes the audio, is built into the app:
  [`vendor/vgmstream/COPYING`](vendor/vgmstream/COPYING).
- **UELib**, which reads the game's scripts, is built into the bridge: MIT,
  [license](https://github.com/EliotVU/Unreal-Library/blob/master/LICENSE).
- **The .NET runtime** is compiled into the bridge by Native AOT: MIT, with its
  [license](https://github.com/dotnet/runtime/blob/main/LICENSE.TXT) and
  [third-party notices](https://github.com/dotnet/runtime/blob/main/THIRD-PARTY-NOTICES.TXT).

## Game-data test

An ignored test decodes every supported segment in a real installation and
checks the subtitle links. It uses `../Splinter Cell`, or `SC1_GAME_DIR`:

```sh
cargo test -p sc-mod-tool-core --test game_audio -- --ignored --nocapture
```
