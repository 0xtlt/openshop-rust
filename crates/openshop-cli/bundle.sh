#!/bin/sh
# Copies into `bundle/` what the CLI embeds from outside its package, which
# `cargo package` leaves out: the templates, the manifest of the workspace and
# the Admin schema of `graphql!`. Built from its package, as crates.io builds
# it, the CLI reads them there (`build.rs`). Run it before `cargo package` or
# `cargo publish` of `openshop-cli`; `mise run package` does.
set -eu

cli=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$cli/../.." && pwd)
bundle="$cli/bundle"

rm -rf "$bundle"
mkdir -p "$bundle"
# The files git has, not what a build left in `templates/`.
(cd "$root" && git ls-files templates | tar -cf - -T -) | tar -xf - -C "$bundle"
# `graphql/admin.schema.json` of a template links to the schema of the
# framework, outside the package: the bundle has the schema itself.
rm -r "$bundle"/templates/*/graphql
# Cargo leaves out of a package every directory with a `Cargo.toml`.
find "$bundle/templates" -name Cargo.toml -exec sh -c 'mv "$1" "$1.template"' sh {} \;
cp "$root/Cargo.toml" "$bundle/workspace.toml"
cp "$root/crates/openshop/graphql/admin.schema.json" "$bundle/admin.schema.json"
