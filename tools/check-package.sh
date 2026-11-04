#!/bin/sh
# The files of the published crate must be exactly the ones in tools/package-files.txt: a file that
# should not ship (a process note, a review, the fuzz crate) fails here, and so does a file that
# disappears. Change the allowlist in the same commit as the file.
set -eu
cd "$(dirname "$0")/.."
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
cargo package --list --allow-dirty 2>/dev/null | LC_ALL=C sort > "$tmp"
diff -u tools/package-files.txt "$tmp"
echo "Package contents match tools/package-files.txt."
