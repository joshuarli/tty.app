#!/bin/sh
set -eu

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  echo "Usage: ./bump-version.sh <version>  (e.g. 0.2.0)" >&2
  exit 1
fi

OLD=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)
if [ -z "$OLD" ]; then
  echo "Error: could not read current version from Cargo.toml" >&2
  exit 1
fi

echo "$OLD -> $VERSION"

sd "^version = \"$OLD\"" "version = \"$VERSION\"" Cargo.toml
sd "<string>$OLD</string>" "<string>$VERSION</string>" Info.plist

cargo check

git add Cargo.toml Cargo.lock Info.plist
git commit -m "v$VERSION"
git tag "v$VERSION"
git push --follow-tags
