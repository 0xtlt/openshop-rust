#!/bin/sh
# Prints a SHA-256 of what the committed admin UI bundle (crates/openshop/ui-dist)
# is built from: the code of the admin UI and of openshop-shared, index.html,
# Trunk.toml, the manifests and the lockfile (build profile and dependencies
# included), the script that builds it, and the versions of Rust and trunk of
# mise.toml. `build-dist.sh` writes it to `ui-dist.sha256`, CI computes it
# again: a change to any of them without a new bundle fails there. The bytes of
# the bundle cannot be compared instead, they depend on the platform it is
# built on (see CONTRIBUTING.md).
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

if command -v sha256sum >/dev/null 2>&1; then
  sum() { sha256sum; }
else
  sum() { shasum -a 256; }
fi

{
  {
    find crates/openshop-ui/src crates/openshop-shared/src -type f
    echo crates/openshop-ui/Cargo.toml
    echo crates/openshop-ui/Cargo.lock
    echo crates/openshop-ui/index.html
    echo crates/openshop-ui/Trunk.toml
    echo crates/openshop-ui/build-dist.sh
    echo crates/openshop-shared/Cargo.toml
  } | LC_ALL=C sort | while read -r file; do
    printf '%s  %s\n' "$(sum < "$file" | cut -d' ' -f1)" "$file"
  done
  # The toolchains the bundle is built with.
  grep -E '^(rust|"github:trunk-rs/trunk") = ' mise.toml
} | sum | cut -d' ' -f1
