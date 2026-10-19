#!/bin/sh
# Builds the admin UI bundle into crates/openshop/ui-dist, or into the
# directory given as the argument. Run it with the Rust and the trunk of
# mise.toml, from anywhere; `mise run ui:build` does.
#
# The same sources give the same bytes, wherever the repository is checked out
# and whoever builds, on the same platform (not from macOS to Linux, nor from
# x86_64 to aarch64: the compiler orders the functions differently). Paths of
# the machine would reach the WebAssembly otherwise, so the build runs on a
# copy of the sources, in a fixed place, with its own cargo home:
# - the path of the sources goes into the hash of the crates, which orders the
#   functions, and the path of every source file into the panic messages;
# - the path of the registry goes into the name of a symbol (wasm_split_macros
#   hashes `CARGO_MANIFEST_PATH`), which orders them again.
# The crates are downloaded into that cargo home, a few seconds.
#
# Building the committed bundle also writes ui-dist.sha256, the hash of the
# sources it is built from (sources-hash.sh), which CI checks.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
dist=${1:-$root/crates/openshop/ui-dist}
case $dist in /*) ;; *) dist=$PWD/$dist ;; esac

work=/tmp/openshop-ui-build
sysroot=$(rustc --print sysroot)
commit=$(rustc -vV | sed -n 's/^commit-hash: //p')

# The sources the bundle is made of: the admin UI and what it depends on.
mkdir -p "$work/crates"
rm -rf "$work/crates/openshop-ui" "$work/crates/openshop-shared"
cp "$root/Cargo.toml" "$work/Cargo.toml"
for crate in openshop-ui openshop-shared; do
  (cd "$root/crates" && tar -c --exclude=target --exclude=dist "$crate") | tar -x -C "$work/crates"
done

# rustup keeps the toolchains where they are, whatever CARGO_HOME says, and
# the cargo of PATH may look for its programs in `$CARGO_HOME/bin`.
RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}
tools=${CARGO_HOME:-$HOME/.cargo}/bin
CARGO_HOME=$work/cargo-home
CARGO_TARGET_DIR=$work/target
mkdir -p "$CARGO_HOME"
if [ -d "$tools" ] && [ ! -e "$CARGO_HOME/bin" ]; then ln -s "$tools" "$CARGO_HOME/bin"; fi
export RUSTUP_HOME CARGO_HOME CARGO_TARGET_DIR

# The separator of CARGO_ENCODED_RUSTFLAGS is \037: a path may hold a space.
CARGO_ENCODED_RUSTFLAGS=$(printf '%s\037%s\037%s\037%s' \
  "--remap-path-prefix=$work=/openshop" \
  "--remap-path-prefix=$CARGO_HOME=/cargo" \
  "--remap-path-prefix=$sysroot/lib/rustlib/src/rust=/rustc/$commit" \
  "--remap-path-prefix=$sysroot=/rustup")
export CARGO_ENCODED_RUSTFLAGS

cd "$work/crates/openshop-ui"
trunk build --release --locked --dist "$dist"
if [ $# -eq 0 ]; then "$root/crates/openshop-ui/sources-hash.sh" > "$root/crates/openshop-ui/ui-dist.sha256"; fi
