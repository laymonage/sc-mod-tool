# SC Mod Tool

Read-only Splinter Cell 1 sound-bank browser built with Rust and egui. It
discovers language folders from the selected installation, scans numeric
`.LS0` banks, lists supported mono 4-bit Ubisoft ADPCM segments, and plays a
selected segment using the upstream vgmstream decoder.

The MVP does not modify game files. It does not claim support for every `.LS0`
bank, Ubisoft codec, or Unreal `.uax` package. Banks without a recognized
stream include an explanation in the UI.

Subtitle links are derived from the selected game's compiled UnrealScript
references, sound-object map keys in `.uax` packages, and locale-specific
`MAPS.LM0` records. Cue text is read from the game's localization files.
Only direct compiled `Localize`/`Sound` pairs are shown; sequence-based
associations are not inferred. When one sound is referenced by multiple cues,
the UI shows each possible subtitle with its provenance.

UI-independent parts are placed in `core` to allow for building CLIs.

## Windows development

Build from a Visual Studio developer shell with the x64 C++ tools available:

The Windows x64 build also requires the .NET 10 SDK and MSVC tools for
publishing the bundled Native AOT UELib bridge. The resulting bridge DLL is
self-contained and does not require a .NET runtime when the app runs.

The bridge is built by the default `uelib-bridge` feature. To build without
the .NET SDK, pass `--no-default-features`; the app still browses and plays
sound banks, but subtitle matching is unavailable.

```powershell
git submodule update --init --recursive
cargo test
cargo run
```

To open a game folder straight away, pass it as an argument:
`cargo run -- "C:\Games\Splinter Cell"`.

The vgmstream source is pinned as a Git submodule under `vendor/vgmstream`.
The core crate compiles only the upstream Ubisoft ADPCM decoder and the small
SC1 segment adapter. Include vgmstream's license and copyright notices when
redistributing the linked application.

## Local game-data integration test

The full game-data decoder test is ignored during ordinary test runs and in CI.
Run it explicitly from PowerShell. By default it looks for `../Splinter Cell`
relative to the app repository root. Set `SC1_GAME_DIR` to override this with
the game installation or its `Sounds` directory:

```powershell
cargo test -p sc-mod-tool-core --test game_audio -- --ignored --nocapture
```

It scans each discovered language and decodes every segment currently
recognized as supported, reading each bank once. It verifies decoded sample
counts and sample rates against the scanner metadata, and verifies dynamically
derived subtitle links against discovered segment IDs and localized cue text.
