# Agent guide

wt-unpack reads War Thunder assets for developers studying, exporting or loading their own installation in another game engine.
Its optional `bridge` feature supplies a rewrite-bridge provider; the workspace includes a Bevy viewer and optional local telemetry.

## Toolchain, build and CI

- The README requires Rust 1.90+ and reports testing with 1.99; CI uses stable Rust.
- Library CI runs on Linux, macOS and Windows; viewer compilation runs on macOS.
- `bridge` and `live` are optional, off by default; `live` reads localhost telemetry.
- Exact CI commands:

```sh
cargo build -p wt-unpack --release
cargo test -p wt-unpack
cargo test -p wt-unpack --features live,bridge
cargo build -p wt-viewer
sh tools/check-package.sh
```

The package gate compares `cargo package --list` with `tools/package-files.txt`; update the list with package changes.
Game-data tests use `WT_GAME_DIR` and skip without the installed game.
Release CI builds with `cargo build --release --locked --target <target> -p wt-unpack --features live,bridge`.
Its targets are `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`.

## Code map

- `src/vromfs.rs`, `blk.rs`, `blk_output.rs`, `unzstd.rs`: archives, binary BLK and streamed output.
- `src/vehicle.rs`, `rig.rs`, `tank.rs`, `texlib.rs`: vehicles, rigs and textures, using wt-formats.
- `src/level.rs`, `terrain.rs`, `rendinst.rs`, `collision.rs`: levels and collision data.
- `src/scene.rs`, `bridge.rs`, `gltf.rs`: provider scenes and export.
- `src/sound/`, `live.rs`, `main.rs`: sound banks, telemetry and CLI.
- `viewer/`, `tests/`, `fuzz/`, `docs/`: viewer, regression/memory tests and format guides.

## Project rules

- Read the user's game folder; never commit game archives, exports or extracted fixtures.
- Keep lztans's clean-room provenance separate from the Dagor layouts credited by wt-formats.
- Retain the vgmstream sound-table credit and its adjacent license.
- Treat all archive lengths, counts and paths as untrusted and preserve the parser's allocation checks.
- Allocation quotas are not RSS limits: accepted hostile BLKs can still use several GB (README 0.5.1 notes).
- Prefer `Block::write_text_to` / `write_json_to` for bounded output buffering; legacy conversion methods allocate the full result.
- Large scenes and textured vehicles need substantial memory; see `docs/MEMORY.md` and the memory tests.
- Drop finished scenes and call `clear_caches()` when done; allocator retention can outlast live scene allocations.

## Public files

- Never commit game files, extracted assets, renders, decompiled code or secrets.
- The public-tree gate needs Python 3.9+ and Git; run it from the repository root:
  `python3 tools/check_public_tree.py` (checks Git's index, including staged new files).
- CI checks committed changes with `python3 tools/check_public_tree.py --range BASE..HEAD`.

## Contributing, license and AI

- Open an issue to discuss a problem or send a pull request with the change.
- Before a pull request, run the applicable CI commands above and the public-tree gate.
- License: MIT OR Apache-2.0, at your option; see `LICENSE-MIT` and `LICENSE-APACHE`.

The code was written with AI coding agents (Anthropic's Claude and OpenAI's GPT models) under the maintainer's direction.
AI-assisted contributions are welcome when their use is disclosed and the changes are reviewed.
