#!/bin/sh
# Checks a downloaded release of one of patodois's projects.
#
#   verify-release.sh <repo> <tag> <folder>
#
# <folder> holds the files you downloaded from https://github.com/patodois/<repo>/releases/tag/<tag>:
# SHA256SUMS, SHA256SUMS.sig and any of the archives. The script checks the Ed25519 signature of
# SHA256SUMS with the release key below, then the SHA-256 of every archive present in the folder.
# The signed message is "osp release patodois/<repo> <tag>\n" followed by SHA256SUMS, so a
# signature from another release or another project does not verify.
#
# Needs OpenSSL 3 (on macOS: brew install openssl@3, or set OPENSSL to its path).
set -eu
repo=$1
tag=$2
folder=$3
openssl=${OPENSSL:-openssl}
if [ -x /opt/homebrew/opt/openssl@3/bin/openssl ] && [ -z "${OPENSSL:-}" ]; then
    openssl=/opt/homebrew/opt/openssl@3/bin/openssl
fi

# Release key, SHA-256 of its DER form:
# 26c35ed9930be42fd266852135e2e81eb74c36ed9a2d0c527e86210a3dcb0946
key='-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA8MKbcbjNbRSBQPWhWHtT8YYexfkRFDM0PCpfjDxLAaE=
-----END PUBLIC KEY-----'

# The key and the signed message reach OpenSSL through file descriptors; nothing is written to disk
{ printf 'osp release patodois/%s %s\n' "$repo" "$tag"; cat "$folder/SHA256SUMS"; } |
    "$openssl" pkeyutl -verify -rawin -pubin -inkey /dev/fd/3 -in /dev/stdin \
        -sigfile "$folder/SHA256SUMS.sig" 3<<KEY
$key
KEY

if command -v sha256sum >/dev/null 2>&1; then
    sum="sha256sum"
else
    sum="shasum -a 256"
fi
cd "$folder"
checked=0
while read -r hash name; do
    name=${name#\*}
    [ -f "$name" ] || continue
    echo "$hash  $name" | $sum -c -
    checked=$((checked + 1))
done < SHA256SUMS
if [ "$checked" -eq 0 ]; then
    echo "SHA256SUMS is signed, but none of its files are in $folder" >&2
    exit 1
fi
echo "OK: SHA256SUMS signed by the release key; $checked file(s) match"
