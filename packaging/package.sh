#!/usr/bin/env bash
# Packs one release binary into dist/<bin>-<version>-<platform>.(tar.gz|zip), next to the README, the licenses and
# an INSTALL.txt. Used by .github/workflows/release.yml, and runnable by hand:
#
#   BIN=wt-unpack VERSION=v0.1.0 PLATFORM=macos-arm64 BIN_PATH=target/release/wt-unpack packaging/package.sh
#
# PLATFORM is macos-arm64, macos-x86_64, linux-x86_64 or windows-x86_64. Optional: OUT_DIR (default dist),
# PKG_GAME (what the program reads, for INSTALL.txt) and PKG_EXTRA (more files to include, space separated).
set -euo pipefail

: "${BIN:?}" "${VERSION:?}" "${PLATFORM:?}" "${BIN_PATH:?}"
OUT_DIR="${OUT_DIR:-dist}"
PKG_GAME="${PKG_GAME:-the game it reads}"
PKG_EXTRA="${PKG_EXTRA:-}"

name="$BIN-$VERSION-$PLATFORM"
case "$PLATFORM" in
  windows-*) exe="$BIN.exe" ;;
  *) exe="$BIN" ;;
esac

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
root="$stage/$name"
mkdir -p "$root" "$OUT_DIR"
cp "$BIN_PATH" "$root/$exe"
chmod +x "$root/$exe"
cp README.md LICENSE-APACHE LICENSE-MIT "$root/"
for f in $PKG_EXTRA; do cp "$f" "$root/"; done

{
  echo "$BIN $VERSION ($PLATFORM)"
  echo
  echo "This archive holds the $BIN program, the README and the two license files. It contains no"
  echo "game data: $BIN reads the files of $PKG_GAME that you already have installed."
  echo "The README explains what it can do and how to point it at your install."
  echo
  case "$PLATFORM" in
    windows-*)
      echo "Run it"
      echo "------"
      echo "Open PowerShell in this folder and run:"
      echo
      echo "    .\\$exe --help"
      echo
      echo "To use it from anywhere, copy $exe into a folder that is on your PATH."
      echo
      echo "SmartScreen"
      echo "-----------"
      echo "The program is not code-signed (that needs a paid certificate). Windows may show"
      echo "\"Windows protected your PC\" the first time. Choose \"More info\", then \"Run anyway\". To avoid"
      echo "the warning, right-click the downloaded .zip, open Properties, tick \"Unblock\" and press OK"
      echo "before you extract it."
      echo
      echo "Check the download"
      echo "------------------"
      echo "Compare the output of this with the line for the archive in SHA256SUMS:"
      echo
      echo "    Get-FileHash -Algorithm SHA256 $name.zip"
      ;;
    *)
      echo "Run it"
      echo "------"
      echo "From a terminal, in this folder:"
      echo
      echo "    ./$exe --help"
      echo
      echo "To use it from anywhere, copy it into a folder that is on your PATH, for example:"
      echo
      echo "    mkdir -p ~/.local/bin && cp $exe ~/.local/bin/"
      echo
      case "$PLATFORM" in
        macos-*)
          echo "macOS: Gatekeeper"
          echo "-----------------"
          echo "The program is not signed or notarized (that needs a paid Apple developer account). If the"
          echo "archive came from a browser, macOS marks the program as quarantined and refuses to open it"
          echo "(\"Apple could not verify ... is free of malware\"). Remove the mark once:"
          echo
          echo "    xattr -d com.apple.quarantine ./$exe"
          echo
          echo "Or run it once, then open System Settings > Privacy & Security and press \"Open Anyway\"."
          echo "Files downloaded with curl or the gh command line tool are not quarantined and need none of this."
          echo
          ;;
        linux-*)
          echo "Linux"
          echo "-----"
          echo "Built on Ubuntu 22.04 for x86_64, so it needs glibc 2.35 or newer (Ubuntu 22.04, Debian 12,"
          echo "Fedora 36 and later). On an older system, build from source (see the README)."
          echo
          ;;
      esac
      echo "Check the download"
      echo "------------------"
      echo "Compare the output of this with the line for the archive in SHA256SUMS:"
      echo
      if [ "${PLATFORM%%-*}" = macos ]; then
        echo "    shasum -a 256 $name.tar.gz"
      else
        echo "    sha256sum $name.tar.gz"
      fi
      ;;
  esac
} > "$root/INSTALL.txt"

case "$PLATFORM" in
  windows-*)
    archive="$name.zip"
    rm -f "$OUT_DIR/$archive"
    out_abs="$(cd "$OUT_DIR" && pwd)"
    if command -v 7z >/dev/null 2>&1; then
      (cd "$stage" && 7z a -tzip -bso0 -bsp0 "$out_abs/$archive" "$name")
    elif command -v zip >/dev/null 2>&1; then
      (cd "$stage" && zip -qr "$out_abs/$archive" "$name")
    else
      echo "no 7z or zip on this machine" >&2
      exit 1
    fi
    ;;
  *)
    archive="$name.tar.gz"
    # COPYFILE_DISABLE keeps macOS from adding ._ files to the archive
    COPYFILE_DISABLE=1 tar -C "$stage" --numeric-owner -czf "$OUT_DIR/$archive" "$name"
    ;;
esac
echo "$OUT_DIR/$archive"
