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
  # libavutil/aarch64/crc.S is under the GPL. Without these, it's empty.
  --disable-arm-crc
  --disable-pmull
  --disable-eor3
  # The video DCTs, which configure enables on its own. Nothing here uses
  # them, but they'd still be compiled, with their notices.
  --disable-faan
  --enable-protocol=fd,file,pipe
  --enable-demuxer=aac,aiff,asf,caf,flac,matroska,mov,mp3,ogg,pcm_s16le,w64,wav
  --enable-decoder='aac,aac_latm,adpcm_*,alac,flac,mp1*,mp2*,mp3*,opus,pcm_*,vorbis,wma*'
  --enable-parser=aac,aac_latm,flac,mpegaudio,opus,vorbis
  --enable-filter=aresample,atempo
  --enable-encoder=pcm_s16le
  --enable-muxer=pcm_s16le
)

if [ $# -ne 2 ]; then
  echo "Usage: $0 build <target> | source <dir>" >&2
  exit 2
fi
# The folder for `source` is relative to where the script is run from.
if [ "$1" = source ]; then
  mkdir -p "$2"
  destination=$(cd "$2" && pwd)
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

# The files from the source release that went into FFmpeg, from the
# compiler's dependency files, except `*_host.d`, which are for programs only
# run during the build.
built_sources() {
  (cd "$work" && find . -name '*.d' ! -name '*_host.d' -exec cat {} +) |
    tr ' \\' '\n\n' |
    sed -e 's/:$//' -e 's|^\./||' -e ':a' -e 's|[^/]*/\.\./||' -e 'ta' |
    LC_ALL=C sort -u |
    LC_ALL=C comm -12 - <(tar -tJf "$build/$tarball" | sed 's|^[^/]*/||' | LC_ALL=C sort)
}

# The first comment in $1 that names a copyright or license, without its
# comment marks, its indent and any notes after the license.
license_comment() {
  awk '
    !inside && /\/\*/ { inside = 1; n = 0; block = "" }
    inside {
      lines[n++] = $0
      block = block $0 "\n"
      if ($0 ~ /\*\//) {
        inside = 0
        if (tolower(block) ~ /copyright|licen[cs]e|permission|public domain/) {
          found = 1
          exit
        }
      }
    }
    END {
      if (!found) exit
      for (i = 0; i < n; i++) {
        line = lines[i]
        if (i == 0) sub(/^.*\/\*+/, "", line)
        if (i == n - 1) sub(/\*+\/.*$/, "", line)
        if (line ~ /^[ \t]*\*/) {
          sub(/^[ \t]*\*+/, "", line)
          sub(/^ /, "", line)
        }
        sub(/[ \t]+$/, "", line)
        lines[i] = line
      }
      first = 0
      while (first < n && lines[first] == "") first++
      # Notes on the code can follow the license, as in the IJG files, so the
      # comment ends with the last paragraph about copyright or licensing.
      last = first
      for (i = first; i < n; i++) {
        if (tolower(lines[i]) !~ /copyright|licen[cs]e|permi|warrant|liab|notice|redistribut|rights reserved/) continue
        last = i
        while (last + 1 < n && lines[last + 1] != "") last++
      }
      indent = -1
      for (i = first; i <= last; i++) {
        if (lines[i] == "") continue
        match(lines[i], /^[ \t]*/)
        if (indent < 0 || RLENGTH < indent) indent = RLENGTH
      }
      for (i = first; i <= last; i++) print substr(lines[i], indent + 1)
    }
  ' "$1"
}

# Writes to $1 the notices of the files built into FFmpeg that aren't under
# the LGPL, from their own headers, as their licenses ask. A file that names
# no license is under the LGPL, as FFmpeg's LICENSE.md says. A license not
# known to allow this fails the build, and so does GPL code, unless it's built
# into an object with no global symbols, which the rest of FFmpeg can't call.
write_notices() {
  local groups="$work/notices" file text key object failed=0
  rm -rf "$groups"
  mkdir "$groups"
  while read -r file; do
    text=$(license_comment "$work/$file")
    if [ -z "$text" ]; then
      if head -n 60 "$work/$file" |
        grep -q -i -E 'copyright|licen[cs]e|permission|public domain'; then
        echo "The license of $file couldn't be read." >&2
        failed=1
      fi
      continue
    fi
    case "$(tr -s ' \t\n' ' ' <<< "$text")" in
      *"Lesser General Public"*) continue ;;
      *"General Public License"*)
        object="$work/${file%.*}.o"
        [ -f "$object" ] && [ -z "$(nm -g "$object" 2> /dev/null)" ] && continue
        echo "$file is under the GPL." >&2
        failed=1
        continue
        ;;
      *"Independent JPEG Group"*) echo "$file" >> "$groups/ijg" ;;
      *"Permission is hereby granted, free of charge, to any person"* | \
        *"Permission to use, copy, modify, and"*"distribute this software for any purpose"* | \
        *"Permission is granted to anyone to use this software for any purpose"* | \
        *"Redistribution and use in source and binary forms"*) ;;
      *)
        echo "$file is under a license this script doesn't know." >&2
        failed=1
        continue
        ;;
    esac
    # Files with the same notice, apart from spacing, share it.
    key=$(tr -d ' \t\n' <<< "$text" | cksum | cut -d ' ' -f 1)
    if [ ! -f "$groups/$key.txt" ]; then
      echo "$key" >> "$groups/order"
      echo "$text" > "$groups/$key.txt"
    fi
    echo "$file" >> "$groups/$key.files"
  done < <(built_sources)
  [ "$failed" = 0 ] || return 1

  {
    echo "Most of FFmpeg is under the GNU Lesser General Public License (see"
    echo "ffmpeg.txt), but a few of the files built into this FFmpeg are under"
    echo "other licenses. Their notices follow, each after the files it covers,"
    echo "as paths in FFmpeg's source release."
    # What the IJG's license asks of a program shipped without its source.
    if [ -f "$groups/ijg" ]; then
      printf '\n%s\n\n' "$rule"
      cat "$groups/ijg"
      echo
      echo "This software is based in part on the work of the Independent JPEG Group."
    fi
    while read -r key; do
      printf '\n%s\n\n' "$rule"
      cat "$groups/$key.files"
      echo
      cat "$groups/$key.txt"
    done < "$groups/order"
  } > "$1"
}

case "$1" in
  source)
    fetch
    cp "$build/$tarball" "$destination/"
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

rule=$(printf '%078d' 0 | tr 0 -)
write_notices "$out/NOTICES.txt"
if [[ "$target" == *-windows-* ]]; then
  # Linking statically also builds in parts of MinGW-w64, whose licenses ask
  # for their notices too.
  {
    printf '\n%s\n\n' "$rule"
    echo "ffmpeg.exe also includes the MinGW-w64 runtime and its winpthreads"
    echo "library, whose notices follow."
    for license in crt/COPYING.MinGW-w64-runtime.txt winpthreads/COPYING; do
      printf '\n%s\n\n' "$rule"
      cat "$MINGW_PREFIX/share/licenses/$license"
    done
  } >> "$out/NOTICES.txt"
fi

# One quoted option per line, so it can be pasted into a shell. None of them
# has a quote of its own.
configure_line=$(printf " \\\\\n  '%s'" "${configure_options[@]}")
year=$(sed -n 's/^#define CONFIG_THIS_YEAR //p' "$work/config.h")
cat > "$out/SOURCE.txt" << EOF
This package includes FFmpeg $version (https://ffmpeg.org), copyright (c)
2000-$year the FFmpeg developers, licensed under the GNU Lesser General Public
License, version 2.1 or later; see ffmpeg.txt. Some of its files are under
other licenses; see ffmpeg-notices.txt.

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
