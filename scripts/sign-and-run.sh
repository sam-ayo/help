#!/bin/zsh
# Cargo runner: sign the binary with the virtualization entitlement, then run it.
# Virtualization.framework refuses to create a VM without this entitlement.
set -e
codesign --force --sign - --entitlements "${0:A:h}/help.entitlements" "$1" 2>/dev/null
exec "$@"
