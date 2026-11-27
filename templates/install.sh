#!/bin/sh
# Anvil installer for Linux x86_64.
# Usage: sh ./install.sh [extra `anvil install` flags]
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
bin="$here/anvil"
if [ -x "$bin" ]; then
    exec "$bin" install --from "$here" --yes "$@"
fi
# FAT/exFAT drives are often mounted without exec permission: run a copy
# (`anvil install` copies itself into ANVIL_HOME, so the copy can go afterwards).
tmp=$(mktemp -d "${TMPDIR:-/tmp}/anvil.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
cp "$bin" "$tmp/anvil"
chmod +x "$tmp/anvil"
status=0
"$tmp/anvil" install --from "$here" --yes "$@" || status=$?
exit "$status"
