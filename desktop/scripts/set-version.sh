#!/bin/zsh
# Write a version into [workspace.package] in Cargo.toml. All of the crates use this
# version. The release workflow uses this script to set the version from the tag.
#
# Usage: scripts/set-version.sh <version>
# Example: scripts/set-version.sh 0.2.0
set -euo pipefail
version="${1:-}"
if ! [[ "$version" =~ '^[0-9]+\.[0-9]+\.[0-9]+$' ]]; then
  echo "The version must have the form 1.2.3, not \"$version\"." >&2
  exit 1
fi
cd "${0:A:h}/.."
# Change only the first version line, which is in [workspace.package]. The -i.bak form
# works with BSD sed and GNU sed.
sed -i.bak "1,/^version = /s/^version = \".*\"/version = \"$version\"/" Cargo.toml
rm Cargo.toml.bak
actual=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [[ "$actual" != "$version" ]]; then
  echo "Cargo.toml has version $actual, not $version." >&2
  exit 1
fi
echo "set version $version"
