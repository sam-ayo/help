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

# dx runs the app from its own bundle, without the guest agent and the MCP bridge.
# Build them, and tell the app where they are.
cargo build -p guest_agent -p mcp_bridge
export HELP_HELPERS="$PWD/target/debug"

# After each patch, dx 0.7.10 leaves index.html in the bundle root and stub.o next to the
# app. codesign then refuses to sign the bundle on the next full build. Delete the files.
# dx writes stub.o again for each patch, so delete only an old one, not one in use.
bundles=target/dx/help/debug/macos
(while true; do
  rm -f $bundles/*.app/index.html(N) $bundles/*.app/Contents/MacOS/stub.o(N.ms+5)
  sleep 1
done) &
trap "kill $! 2>/dev/null" EXIT

# Sign the bundle with the virtualization entitlement, as scripts/sign-and-run.sh does.
# The team ID `-` gives an ad-hoc signature.
"$DX" serve --hot-patch --desktop -p app --bin help --features hotpatch \
  --apple-entitlements scripts/help.entitlements --apple-team-id - "$@"
