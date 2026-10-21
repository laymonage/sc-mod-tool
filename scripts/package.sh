#!/usr/bin/env bash
# Packages a release build with its bridge library, FFmpeg and license
# notices, as dist/sc-mod-tool-<version>-<platform>.zip for Windows or .tar.gz
# otherwise.
#
# Usage: scripts/package.sh <version> <platform> <target>
# For example, after `cargo build --release --target aarch64-apple-darwin`
# and `scripts/ffmpeg.sh build aarch64-apple-darwin`:
#   scripts/package.sh v0.1.0 macos-arm64 aarch64-apple-darwin
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "Usage: $0 <version> <platform> <target>" >&2
  exit 2
fi
version=$1
platform=$2
target=$3

case "$target" in
  *-windows-*) files=(sc-mod-tool.exe SCModUELibBridge.dll) ffmpeg=ffmpeg.exe ;;
  *-apple-*) files=(sc-mod-tool libSCModUELibBridge.dylib) ffmpeg=ffmpeg ;;
  *-linux-*) files=(sc-mod-tool libSCModUELibBridge.so) ffmpeg=ffmpeg ;;
  *)
    echo "Unsupported target: $target" >&2
    exit 2
    ;;
esac

cd "$(dirname "$0")/.."
name="sc-mod-tool-$version-$platform"
build="${CARGO_TARGET_DIR:-target}/$target/release"
ffmpeg_build="build/ffmpeg/$target"
package="dist/$name"
rm -rf "$package" "$package.zip" "$package.tar.gz"
mkdir -p "$package/licenses"
if [[ "$target" == *-apple-* ]]; then
  # Finder opens a bare executable in Terminal, so macOS gets an app bundle.
  # The app loads the bridge and finds FFmpeg beside it, in Contents/MacOS.
  app="$package/SC Mod Tool.app"
  mkdir -p "$app/Contents/MacOS"
  for file in "${files[@]}"; do
    cp "$build/$file" "$app/Contents/MacOS/"
  done
  cp "$ffmpeg_build/$ffmpeg" "$app/Contents/MacOS/"
  plist="$app/Contents/Info.plist"
  cp scripts/macos/Info.plist "$plist"
  plutil -replace CFBundleShortVersionString -string "${version#v}" "$plist"
  plutil -replace CFBundleVersion -string "${version#v}" "$plist"
  # The linker signs the executable on its own, which leaves the bundle
  # looking damaged once its Info.plist is added. Signing the bridge and
  # FFmpeg and then the bundle, ad hoc, makes it valid. It still isn't
  # notarized.
  codesign --force --sign - "$app/Contents/MacOS/libSCModUELibBridge.dylib"
  codesign --force --sign - "$app/Contents/MacOS/$ffmpeg"
  codesign --force --sign - "$app"
else
  for file in "${files[@]}"; do
    cp "$build/$file" "$package/"
  done
  cp "$ffmpeg_build/$ffmpeg" "$package/"
fi
cp README.md LICENSE "$package/"

# The notices of the third-party code built into the app and the bridge, and
# of FFmpeg, as the README asks. Every version of the runtime package has the
# same ones.
packages="${NUGET_PACKAGES:-$HOME/.nuget/packages}"
runtime=$(find "$packages" -mindepth 2 -maxdepth 2 -type d \
  -path "*/microsoft.netcore.app.runtime.nativeaot.*/*" | sort | tail -n 1)
if [ -z "$runtime" ]; then
  echo "The .NET runtime package was not found in $packages." >&2
  exit 1
fi
cp vendor/vgmstream/COPYING "$package/licenses/vgmstream.txt"
cp "$ffmpeg_build/LICENSE.txt" "$package/licenses/ffmpeg.txt"
cp "$ffmpeg_build/SOURCE.txt" "$package/licenses/ffmpeg-source.txt"
cp "$packages"/eliot.uelib/*/LICENSE "$package/licenses/UELib.txt"
cp "$runtime/LICENSE.TXT" "$package/licenses/dotnet-runtime.txt"
cp "$runtime/THIRD-PARTY-NOTICES.TXT" "$package/licenses/dotnet-runtime-third-party-notices.txt"

cd dist
case "$target" in
  *-windows-*) 7z a "$name.zip" "$name" > /dev/null && echo "dist/$name.zip" ;;
  *) tar -czf "$name.tar.gz" "$name" && echo "dist/$name.tar.gz" ;;
esac
