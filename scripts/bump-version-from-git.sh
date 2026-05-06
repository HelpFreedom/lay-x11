#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

head_count="$(git rev-list --count HEAD)"
next_count="$((head_count + 1))"
version="0.1.${next_count}"
release_date="$(date +%F)"

perl -0pi -e 's/^version = "[^"]+"/version = "'"${version}"'"/m' Cargo.toml
perl -0pi -e 's/"version-name": "[^"]+"/"version-name": "'"${version}"'"/' \
  extension/lay@radislabus-star.github.io/metadata.json
perl -0pi -e "s/const APP_VERSION = '[^']+';/const APP_VERSION = '${version}';/" \
  extension/lay@radislabus-star.github.io/lay-impl.js
perl -0pi -e "s/const APP_RELEASE_DATE = '[^']+';/const APP_RELEASE_DATE = '${release_date}';/" \
  extension/lay@radislabus-star.github.io/lay-impl.js

cargo check --quiet

printf 'lay version -> %s\n' "$version"
