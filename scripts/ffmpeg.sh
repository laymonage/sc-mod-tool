#!/usr/bin/env bash
# Builds the FFmpeg that release packages ship, from a pinned source release
# checked against its SHA-256. It's audio only, with just what replacing a
# segment needs, and has no external libraries, so it stays LGPL.
#
# Usage:
#   scripts/ffmpeg.sh build <target>   # into build/ffmpeg/<target>
#   scripts/ffmpeg.sh source <dir>     # copies the checked source release there
#
# Windows builds need MSYS2's UCRT64 shell with gcc and make. A macOS build
# for the other architecture is cross-compiled.
set -euo pipefail

version=9.0.2
# The release's signature checks out with FFmpeg's release signing key,
# FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8.
sha256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
tarball="ffmpeg-$version.tar.xz"
url="https://ffmpeg.org/releases/$tarball"

# What src/app/edits.rs offers to open, decoded and written as mono 16-bit
# PCM at the segment's rate, with atempo for stretching. FFmpeg reads `-`
# through `fd` and writes it through `pipe`.
configure_options=(
  --disable-everything
  --disable-autodetect
  --disable-ffplay
  --disable-ffprobe
  --disable-avdevice
  --disable-network
  --disable-doc
  --disable-debug
  --disable-x86asm
  --enable-protocol=fd,file,pipe
  --enable-demuxer=aac,aiff,asf,caf,flac,matroska,mov,mp3,ogg,pcm_s16le,w64,wav
  --enable-decoder='aac,aac_latm,adpcm_ima_wav,adpcm_ms,alac,flac,mp1*,mp2*,mp3*,opus,pcm_*,vorbis,wma*'
  --enable-parser=aac,aac_latm,flac,mpegaudio,opus,vorbis
  --enable-filter=aresample,atempo
  --enable-encoder=pcm_s16le
  --enable-muxer=pcm_s16le
)

if [ $# -ne 2 ]; then
  echo "Usage: $0 build <target> | source <dir>" >&2
  exit 2
fi
cd "$(dirname "$0")/.."
build=build/ffmpeg

# Downloads the source release into $build, once, and checks it.
fetch() {
  mkdir -p "$build"
  if [ ! -f "$build/$tarball" ]; then
    curl -fsSL --retry 3 -o "$build/$tarball.part" "$url"
    mv "$build/$tarball.part" "$build/$tarball"
  fi
  local actual
  if command -v sha256sum > /dev/null; then
    actual=$(sha256sum "$build/$tarball" | cut -d ' ' -f 1)
  else
    actual=$(shasum -a 256 "$build/$tarball" | cut -d ' ' -f 1)
  fi
  if [ "$actual" != "$sha256" ]; then
    rm -f "$build/$tarball"
    echo "$tarball has SHA-256 $actual, not $sha256." >&2
    exit 1
  fi
}

case "$1" in
  source)
    fetch
    mkdir -p "$2"
    cp "$build/$tarball" "$2/"
    echo "$2/$tarball"
    exit 0
    ;;
  build) target=$2 ;;
  *)
    echo "Usage: $0 build <target> | source <dir>" >&2
    exit 2
    ;;
esac

exe=ffmpeg
case "$target" in
  *-windows-*)
    exe=ffmpeg.exe
    # The ffmpeg program needs threads. Linking statically leaves only system
    # DLLs to load.
    configure_options+=(--enable-w32threads --extra-ldflags=-static)
    ;;
  *-apple-*)
    arch=${target%%-*}
    clang_arch=$arch
    [ "$arch" = aarch64 ] && clang_arch=arm64
    # The same minimum as scripts/macos/Info.plist.
    configure_options+=(
      --enable-cross-compile --target-os=darwin --arch="$arch"
      --cc="clang -arch $clang_arch"
      --extra-cflags=-mmacosx-version-min=12.0
      --extra-ldflags=-mmacosx-version-min=12.0
    )
    ;;
  *-linux-*) ;;
  *)
    echo "Unsupported target: $target" >&2
    exit 2
    ;;
esac

fetch
out="$build/$target"
work="$build/work-$target"
rm -rf "$out" "$work"
mkdir -p "$work"
tar -xJf "$build/$tarball" -C "$work" --strip-components 1
(
  cd "$work"
  ./configure "${configure_options[@]}"
  make -j"$(getconf _NPROCESSORS_ONLN 2> /dev/null || sysctl -n hw.ncpu)" "$exe"
)
mkdir -p "$out"
# `ffmpeg` is the stripped copy of `ffmpeg_g`.
cp "$work/$exe" "$out/"
cp "$work/COPYING.LGPLv2.1" "$out/LICENSE.txt"

# One quoted option per line, so it can be pasted into a shell. None of them
# has a quote of its own.
configure_line=$(printf " \\\\\n  '%s'" "${configure_options[@]}")
cat > "$out/SOURCE.txt" << EOF
This package includes FFmpeg $version (https://ffmpeg.org), licensed under the
GNU Lesser General Public License, version 2.1 or later; see ffmpeg.txt.

It was built without changes from the source release
$url
SHA-256 $sha256
which is also attached to the release this package comes from, with:

./configure$configure_line
make $exe
EOF

case "$target" in
  *-windows-*)
    if objdump -p "$out/$exe" | grep -i "DLL Name: lib"; then
      echo "$exe needs a DLL that isn't part of Windows." >&2
      exit 1
    fi
    ;;
  *-apple-*) otool -L "$out/$exe" ;;
  *-linux-*) ldd "$out/$exe" ;;
esac

# Stretching a second of silence uses the pipe, the raw PCM demuxer and
# muxer, resampling and atempo. A cross-compiled build is left alone.
if "$out/$exe" -version > /dev/null 2>&1; then
  bytes=$(head -c 72000 /dev/zero |
    "$out/$exe" -hide_banner -loglevel error -f s16le -ar 36000 -ac 1 -i - \
      -af atempo=1.25 -ac 1 -ar 22050 -c:a pcm_s16le -f s16le - | wc -c)
  if [ "$bytes" -lt 30000 ]; then
    echo "$exe made $bytes bytes of audio from a second of silence." >&2
    exit 1
  fi
fi
rm -rf "$work"
echo "$out/$exe"
