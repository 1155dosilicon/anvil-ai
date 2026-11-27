#!/bin/sh
# Anvil installer for Linux x86_64.
# Usage: sh ./install.sh [extra `anvil install` flags]
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
bin="$here/anvil"
if [ ! -x "$bin" ]; then
    # FAT/exFAT drives are often mounted without exec permission: run a copy.
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/anvil.XXXXXX")
    cp "$bin" "$tmp/anvil"
    chmod +x "$tmp/anvil"
    bin="$tmp/anvil"
fi
exec "$bin" install --from "$here" --yes "$@"
