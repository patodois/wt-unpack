# Cloud report: "fresh person" test of the README

Clean Linux x86_64 container (Ubuntu 24.04, glibc 2.39, OpenSSL 3.0.13, Rust 1.97.0 stable, no game files). I followed
the README top to bottom. Commands that need game files were run against my own synthetic inputs where one was cheap to
build (a hand-made `.vromfs.bin`, a loose text BLK, missing and empty folders); the rest I only compared with `--help`
and the source. No game file or game data was downloaded, built or committed.

**Branch.** The task text names `cloud/readme-wt-unpack`; this session's instructions allow pushes only to
`claude/readme-wt-unpack-n6cdtx`, so the work is on that branch. Nothing was pushed anywhere else, no PR, no tags, no release.

## What I did

1. Read the release page through the GitHub API: v0.3.0 (and v0.2.0, v0.1.0) each have 4 archives, `SHA256SUMS`,
   `SHA256SUMS.sig`. Downloaded the Linux archive, `SHA256SUMS`, `SHA256SUMS.sig` of v0.3.0.
2. Checked the checksum, the signature (`tools/verify-release.sh`), extracted the archive, ran `--help`, `--version`,
   `live --help`, `live --mock --count 3`, `rig --help`: the release binary has the `live` and `bridge` commands as the README says.
   The release archive contains only `wt-unpack`, README.md (the v0.3.0 one, older than the README on `main`), two licences, `INSTALL.txt`.
3. Built from source: `cargo build --release` (CLI, ~40 s warm registry), `--features live,bridge` (~20 s more), and
   `cargo build --release -p wt-viewer` (8 min 33 s). Built the CLI with Rust 1.90.0 (`--features live,bridge`): works, so the README's 1.90 is right for the CLI.
4. Compared every `--help` with the README tables and examples; ran each command against missing/empty/synthetic inputs.
5. Checked local links and anchors in the README (all exist), the DER hash of `RELEASE-KEY.pem` (matches the README).

## Bugs fixed (each with a test)

### 1. `tools/verify-release.sh` fails on OpenSSL 3.0.x
- **Input shape:** any valid release folder, on OpenSSL 3.0.x (Ubuntu 22.04/24.04, Debian 12; here 3.0.13). The README says "needs OpenSSL 3".
- **Failure:** `pkeyutl: Could not allocate 0 bytes for oneshot sign/verify buffer`, exit 1, for a correct signature. The script fed the signed message to `openssl pkeyutl -verify -rawin -in /dev/stdin` from a pipe; Ed25519 is a one-shot algorithm and OpenSSL 3.0 needs a file of known size (with `-in` omitted it says "unable to determine file size for oneshot operation").
- **Fix:** the message is written to a private `mktemp -d` file, verified, and removed by a `trap` on exit. The key still goes through a file descriptor. The script's output and arguments are unchanged.
- **Test:** `tests/verify_release.rs::verify_release_accepts_a_signed_release_and_rejects_a_changed_one`. It makes a throwaway Ed25519 key with `openssl genpkey`, puts its public half in a copy of the script in place of the release key, signs a synthetic `SHA256SUMS`, and checks: the release verifies, another tag does not, a changed archive does not. Skipped when OpenSSL 3 is not on `PATH`. Confirmed failing without the fix (the error above) and passing with it. I also ran the fixed script against the real v0.3.0 download: `Signature Verified Successfully`, `OK: SHA256SUMS signed by the release key; 1 file(s) match`.

## README problems fixed (documentation only)

| # | Problem a fresh person hits | Fix |
|---|---|---|
| 1 | `sha256sum -c SHA256SUMS` after downloading one archive prints three `FAILED open or read` lines and exits 1 | Linux command is now `sha256sum -c --ignore-missing SHA256SUMS`, with the reason |
| 2 | `sh tools/verify-release.sh ...` assumes the repo; the archives do not contain `tools/`. No hint how to check OpenSSL version or what success looks like | Says the script is in the repository, not the archive; how to check OpenSSL; what `OK:` means; what the three arguments are |
| 3 | No example of unpacking and running the Linux archive (the folder name, `./wt-unpack`) | Added `tar -xzf ... && cd ... && ./wt-unpack --help` and the note about `./` vs `PATH` |
| 4 | "Rust 1.90 or newer ... tested with 1.99": the viewer does not build with a stable older than 1.97.1 (`bevy@0.20.0 requires rustc 1.97.1`; I hit it on 1.97.0). 1.99 is not a verifiable claim | Prerequisites now say 1.90 for `wt-unpack` (checked), 1.97.1 for `wt-viewer` (checked), plus `git` and a C linker |
| 5 | Viewer build on a clean Linux box stops at `wayland-sys` ("Package wayland-client was not found"); README only says "extra system libraries" | Added the `apt install` line I used (`pkg-config libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev`) after which the build succeeded |
| 6 | `$GAME` was unquoted in every example, but Steam's Linux and Windows folder is `War Thunder` (a space) | All `$GAME/...` uses are quoted; a note says why |
| 7 | `--game` defaults to the macOS launcher path. The "Try it" `model` command has no `--game`, so on Linux it cannot find the game folder; the same for the viewer, `rig`, `check-bridge`, `check-conformance` examples | `--game "$GAME"` added to those examples; a Prerequisites bullet explains the default and what happens when it is wrong |
| 8 | "Try it" only showed a macOS `GAME=` | Linux/Steam example added as a comment |
| 9 | `... \| head` examples print a Rust panic (`failed printing to stdout: Broken pipe`, exit 101) | One-sentence note (the behaviour itself is listed below) |

## Behaviour problems found, NOT changed (report only)

1. **Broken pipe panic.** `wt-unpack list X | head -1` (any output larger than what `head` reads) panics in `std::io::stdio::_print` and exits 101. The README's own examples pipe to `head`. Needs SIGPIPE handling or `writeln!` with error ignoring in `src/main.rs`.
2. **A missing or wrong `--game` folder is not an error** for `check-bridge`, `check-conformance`: `check-bridge --game /nonexistent` prints `0/0 vehicles loaded and valid (0 failed)` and exits 0; `check-conformance` prints `no asset matches` and exits 0; `check-bridge --list` prints nothing and exits 0. On Linux without `--game` this is the first thing a new user sees. `check`, `check-levels` and `sound check` on an empty folder also report `0/0` with status 0.
3. **Error messages without a path.** `check-levels /nonexistent` and `sound check /nonexistent` print only `Error: No such file or directory (os error 2)`; `list` and `model` do name the file.
4. **Backtraces on ordinary errors.** Errors print a `Stack backtrace:` block. This container has `RUST_BACKTRACE=1` set, so it may be my environment, not the program; I did not check with it unset.
5. **`cat` of a text BLK ignores `--blk json`**: `wt-unpack cat t.blk --blk json` prints the text unchanged (a loose text file). The README does not promise otherwise; mentioned only because `--blk` is documented for `extract`.
6. `cargo clippy --all-targets -- -D warnings` fails on existing code (9 errors in the lib tests, e.g. `identity_op`, `needless_borrow`) and `cargo fmt --check` reports differences in existing files. CI runs neither, and the repository does not follow default rustfmt, so I did not touch them.
7. `model --help` shows `--lod` (not in the README) and `level --help` shows `--textures` and `--cell` (not in the README). Not wrong, just undocumented.

## Fuzzing

None. I changed no parser or decoder (only a shell script, a test and docs), so there was nothing to fuzz. Time per target: 0 s for every target.

## What I left out, and why

- **Every command that needs game files** (`grp`, `model`, `level`, `sound ...`, `check` with models, `rig`, `check-bridge` on real vehicles, `wt-viewer` windows and screenshots, `WT_GAME_DIR` tests): I have no game and must not download one. Verified only that they parse their arguments, that their `--help` matches the README, and how they fail on missing or empty inputs. I built a synthetic stored `VRFs` archive by hand; `list`, `extract --filter`, `cat archive:path` worked on it. The numbers in the README tables (3,245 vehicles, 160 levels, times, memory) are therefore **unverified** and I left them as they are.
- **External links** (crates.io, docs.rs, GitHub repos of rewrite-bridge, bridge-testbed, wf-unpack, etc., Bevy site): the sandbox proxy answers 403 or fails for them, so I could not check them. Only `rustup.rs` and the releases page returned 200.
- macOS and Windows instructions (Gatekeeper, SmartScreen, `Get-FileHash`, `shasum -c`): not testable here, left unchanged. I did not add `--ignore-missing` to the macOS `shasum` line because I could not check it.
- The Windows/macOS archives were not downloaded or run.
- The apt package list for the viewer: I installed the four packages together, so I do not know which of them are strictly required, and I did not run the viewer (no display, no game). `libx11-dev`/X11 headers were already present on this image; a fresh Ubuntu may need more.

## Uncertain

- The README claims "tested with 1.99"; I replaced it with what I could verify (1.90 and 1.97.x). If 1.99 is a real toolchain the owner uses, that is not contradicted, only unchecked.
- `Cargo.toml` has no `rust-version`, so the stated minimums are documentation only; Cargo will not enforce 1.90 for the CLI (it does enforce 1.97.1 for the viewer via its dependencies).
- Whether the `Broken pipe` message also happens on the release binary on other platforms: only seen on Linux with a debug-free release build.
- Tests: `cargo test -p wt-unpack --release` and `--features live,bridge` both pass here (116 and 139 library tests plus integration tests); the CI also runs Windows and macOS, which I could not.
