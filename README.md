# SC Mod Tool

A read-only browser for Splinter Cell 1 sound banks, built with Rust and egui.
It lists the mono 4-bit Ubisoft ADPCM segments in each numeric `.LS0` bank,
plays them with vgmstream's decoder, and shows their subtitles. Game files are
never modified.

Subtitles come from direct `Localize`/`Sound` pairs in the game's compiled
UnrealScript, matched to segments through `.uax` sound keys and `MAPS.LM0`.
Sequence-based associations aren't inferred.

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
it. When redistributing, include vgmstream's license notices.

## Game-data test

An ignored test decodes every supported segment in a real installation and
checks the subtitle links. It uses `../Splinter Cell`, or `SC1_GAME_DIR`:

```sh
cargo test -p sc-mod-tool-core --test game_audio -- --ignored --nocapture
```
