# SC Mod Tool

Read-only Splinter Cell 1 sound-bank browser built with Rust and egui. It
discovers language folders from the selected installation, scans numeric
`.LS0` banks, lists supported mono 4-bit Ubisoft ADPCM segments, and plays a
selected segment using the upstream vgmstream decoder.

The MVP does not modify game files. It does not claim support for every `.LS0`
bank, Ubisoft codec, or Unreal `.uax` package. Banks without a recognized
stream include an explanation in the UI.

UI-independent parts are placed in `core` to allow for building CLIs.

## Windows development

Build from a Visual Studio developer shell with the x64 C++ tools available:

```powershell
git submodule update --init --recursive
cargo test --manifest-path sc-mod-tool\core\Cargo.toml
cargo test --manifest-path sc-mod-tool\Cargo.toml
cargo run --manifest-path sc-mod-tool\Cargo.toml
```

The vgmstream source is pinned as a Git submodule under `vendor/vgmstream`.
The core crate compiles only the upstream Ubisoft ADPCM decoder and the small
SC1 segment adapter. Include vgmstream's license and copyright notices when
redistributing the linked application.

Linux and macOS builds are planned for GitHub Actions and have not yet been
validated.
