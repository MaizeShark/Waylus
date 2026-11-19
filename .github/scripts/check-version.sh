#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Checks that a release tag (v1.2.3) matches the host's Cargo version and the app's
# versionName, and prints the version. Fails if they differ. Usage: check-version.sh v1.2.3
set -euo pipefail

tag=${1:?usage: check-version.sh <tag>}
case $tag in
    v[0-9]*) version=${tag#v} ;;
    *) echo "::error::tag '$tag' does not look like v<version>"; exit 1 ;;
esac

host=$(cargo metadata --no-deps --format-version 1 --manifest-path Cargo.toml |
    jq -r '.packages[] | select(.name == "waylus") | .version')
mapfile -t app < <(sed -nE 's/^[[:space:]]*versionName[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/p' android/app/build.gradle.kts)
if [ "${#app[@]}" -ne 1 ]; then
    echo "::error::expected one versionName in android/app/build.gradle.kts, found ${#app[@]}"
    exit 1
fi

echo "tag:         $version"
echo "host/Cargo:  $host"
echo "versionName: ${app[0]}"
status=0
[ "$host" = "$version" ] || { echo "::error::host version $host (Cargo.toml) does not match tag $tag"; status=1; }
[ "${app[0]}" = "$version" ] || { echo "::error::app versionName ${app[0]} does not match tag $tag"; status=1; }
[ $status -eq 0 ] || exit 1

if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "version=$version" >> "$GITHUB_OUTPUT"
fi
