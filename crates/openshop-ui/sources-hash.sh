#!/bin/sh
# Prints a SHA-256 of what the committed admin UI bundle (crates/openshop/ui-dist)
# is built from: the code of the admin UI and of openshop-shared, index.html and
# Trunk.toml. `build-dist.sh` writes it to `ui-dist.sha256`, CI computes it again:
# a change to those files without a new bundle fails there. The bytes of the
# bundle cannot be compared instead, they depend on the platform it is built on
# (see CONTRIBUTING.md). The dependencies are not part of it: after updating
# them, build the bundle again.
set -eu

crates=$(cd "$(dirname "$0")/.." && pwd)
cd "$crates"

if command -v sha256sum >/dev/null 2>&1; then
  sum() { sha256sum; }
else
  sum() { shasum -a 256; }
fi

{
  find openshop-ui/src openshop-shared/src -type f
  echo openshop-ui/index.html
  echo openshop-ui/Trunk.toml
} | LC_ALL=C sort | while read -r file; do
  printf '%s  %s\n' "$(sum < "$file" | cut -d' ' -f1)" "$file"
done | sum | cut -d' ' -f1
