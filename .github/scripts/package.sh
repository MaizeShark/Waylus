#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Assembles the release assets from the CI artifacts.
# Usage: package.sh <version> <artifacts dir> <output dir>
#   <artifacts dir>/waylus-windows-x64/{bin/,FFmpeg-LICENSE.txt}
#   <artifacts dir>/waylus-linux-x86_64/waylus
#   <artifacts dir>/waylus-apk/waylus.apk
# Run from the repository root (needs cargo, python3, zip).
set -euo pipefail

version=${1:?version}
art=$(realpath "${2:?artifacts dir}")
out=${3:?output dir}
mkdir -p "$out"
out=$(realpath "$out")
# shellcheck source=../ffmpeg-windows.env
source .github/ffmpeg-windows.env
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fill() { # template -> stdout, with the @VARIABLES@ filled in
    sed -e "s|@VERSION@|$version|g" \
        -e "s|@FFMPEG_VERSION@|$FFMPEG_VERSION|g" -e "s|@FFMPEG_COMMIT@|$FFMPEG_COMMIT|g" \
        -e "s|@FFMPEG_ASSET@|$FFMPEG_ASSET|g" -e "s|@FFMPEG_BUILD_TAG@|$FFMPEG_BUILD_TAG|g" \
        -e "s|@FFMPEG_SHA256@|$FFMPEG_SHA256|g" "$1"
}
crlf() { sed 's/$/\r/'; } # text files in the Windows zip

common_licenses() { # dir target
    mkdir -p "$1/licenses"
    python3 -I .github/scripts/third-party-licenses.py "$2" "$1/licenses/Rust-crates.txt"
    cp vendor/nusb/LICENSE-MIT "$1/licenses/nusb-LICENSE-MIT.txt"
    cp vendor/nusb/LICENSE-APACHE "$1/licenses/nusb-LICENSE-APACHE.txt"
    { cat packaging/android-app-notices.txt; cat vendor/nusb/LICENSE-APACHE; } > "$1/licenses/waylus-app-libraries.txt"
}

apk=$art/waylus-apk/waylus.apk
cp "$apk" "$out/waylus-$version.apk"

# Windows zip
name=waylus-$version-windows-x64
dir=$work/$name
mkdir -p "$dir"
cp "$art"/waylus-windows-x64/bin/* "$dir/"
cp "$apk" "$dir/waylus.apk"
crlf < LICENSE > "$dir/LICENSE.txt"
fill packaging/windows/README.txt | crlf > "$dir/README.txt"
common_licenses "$dir" x86_64-pc-windows-msvc
cp "$art/waylus-windows-x64/FFmpeg-LICENSE.txt" "$dir/licenses/"
fill packaging/FFmpeg-SOURCE.txt > "$dir/licenses/FFmpeg-SOURCE.txt"
for f in "$dir"/licenses/*.txt; do crlf < "$f" > "$f.tmp" && mv "$f.tmp" "$f"; done
(cd "$work" && zip -qr9 "$out/$name.zip" "$name")

# Linux tarball (built on Fedora 45)
name=waylus-$version-linux-x86_64-fedora45
dir=$work/$name
mkdir -p "$dir"
install -m 755 "$art/waylus-linux-x86_64/waylus" "$dir/waylus"
cp "$apk" "$dir/waylus.apk"
cp LICENSE "$dir/LICENSE"
fill packaging/linux/README.txt > "$dir/README.txt"
common_licenses "$dir" x86_64-unknown-linux-gnu
tar -C "$work" --owner=0 --group=0 -czf "$out/$name.tar.gz" "$name"

(cd "$out" && sha256sum -- * > SHA256SUMS.txt)
ls -l "$out"
cat "$out/SHA256SUMS.txt"
