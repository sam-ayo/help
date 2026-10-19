#!/bin/zsh
# Start the app with hot patching. A change to a function in crates/app shows in the
# running app without a restart. See crates/app/src/hotpatch.rs.
#
# Install the Dioxus CLI first. Homebrew's Deno also has a `dx` command, so install it
# in its own folder:
#   cargo install dioxus-cli --version 0.7.10 --locked --root ~/.local/dioxus-cli
set -e
DX=${DX:-$HOME/.local/dioxus-cli/bin/dx}
cd "${0:A:h}/.."
exec "$DX" serve --hot-patch --desktop -p app --bin agent-desk --features hotpatch "$@"
