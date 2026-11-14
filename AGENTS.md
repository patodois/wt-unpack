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

The package gate compares `cargo package --list` with `tools/package-files.txt`; update the list with package changes
(a new file under `src/`, `tests/`, `examples/`, `docs/` or `tools/` needs its line there).
Game-data tests use `WT_GAME_DIR` and skip without the installed game.
The viewer has tests of its own that CI does not run: `cargo test -p wt-viewer` (arguments and `--help`; no window opens).
Release CI builds with `cargo build --release --locked --target <target> -p wt-unpack --features live,bridge`.
Its targets are `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`.

## Code map

Every file of `src/` (the library is `lib.rs`, the CLI `main.rs`):

- `vromfs.rs`, `blk.rs`, `blk_output.rs`, `unzstd.rs`, `bytes.rs`: archives, binary BLK and its streamed text/JSON
  output, zstd with output limits, the little-endian cursor every reader uses.
- `rodb.rs`: read-only data block dumps, the form game resources such as land classes keep their BLK in.
- `vehicle.rs`, `camo.rs`, `texlib.rs`: vehicle assembly from `.grp` packs (using wt-formats), camouflage and
  occlusion baking, texture lookup in `.dxp.bin` packs.
- `tank.rs`, `rig.rs`, `gear.rs`: unit files (turret limits, drive train, the simple driving model), the rig a
  rewrite drives a tank with, the running gear (wheels, track belts).
- `level.rs`, `terrain.rs`, `landclass.rs`: level containers (`DBLD`) with heightmap, landmesh and placed-object
  layers; height grids and terrain meshes; the ground's land classes, weights and baked colour.
- `rendinst.rs`, `collision.rs`: placed-object models and `riDesc.bin`; collision resources.
- `scene.rs`, `level_collision.rs`, `bridge.rs` (all three need the `bridge` feature): a level as a
  rewrite-bridge scene, the collision of its objects, and the provider `WarThunder` with its caches.
- `gltf.rs`, `paths.rs`: glTF export; output file names made from game data (see the rules below).
- `sound.rs`, `sound/vorbis.rs`: FMOD banks and FSB5 samples, PCM and FADPCM to WAV; Vorbis to Ogg and to PCM
  (`sound/vorbis_setups.bin.zst` and its `LICENSE-vgmstream` belong together).
- `live.rs`: the telemetry client (`live` feature for the HTTP part) and its mock server.
- `memory.rs`: handing freed memory back to the system after a load.
- `alloc_probe.rs`: test builds only; the allocator that lets tests measure allocations.
- `main.rs`: every command. Its own `println!` ends the program quietly when standard output is closed; the
  check commands end through `check_outcome` (exit status 1 on a failure).

Beside `src/`:

- `viewer/`: `wt-viewer`, a Bevy app (`src/main.rs`, `args.rs` for the command line, `drive.rs` for drive mode).
- `tests/`: `conversion.rs` (CLI on synthetic files: extraction, names, pipes, exit statuses), `cli.rs` (needs
  the `bridge` feature), `live.rs` (its client tests need `live`), and `game_data.rs`, `memory.rs`,
  `memory_vehicles.rs`, which need `WT_GAME_DIR`.
- `examples/`: `drive_tank.rs` is the usage example; `render_scene.rs`, `scene_memory.rs`, `vehicle_memory.rs`
  and `check_rendinst_bounds.rs` are measuring and audit tools. All but the last need `--features bridge`.
- `fuzz/`: a separate nightly-only crate with one target per reader; not in the workspace or the package.
- `tools/`: `check-package.sh` with `package-files.txt`, `check_public_tree.py`, `verify-release.sh`.
- `packaging/package.sh`: builds a release archive; used by `.github/workflows/release.yml`.
- `samples/live/`: JSON answers of the game's telemetry API, built into the mock server (`live --mock`, tests).
- `docs/`: `BRIDGE.md`, `VEHICLES.md`, `LEVELS.md`, `MEMORY.md`, `LIVE.md`, `HOW_IT_WORKS.md`.

## Project rules

- Read the user's game folder; never commit game archives, exports or extracted fixtures.
- Keep lztans's clean-room provenance separate from the Dagor layouts credited by wt-formats.
- Retain the vgmstream sound-table credit and its adjacent license.
- Treat all archive lengths, counts and paths as untrusted and preserve the parser's allocation checks.
- A reader that follows offsets needs a total as well: VROMFS, BLK and rodb each charge what they copy,
  because nothing stops two entries from naming the same bytes. The `RIGz` layers of a level have no such
  total yet.
- A name read from game data never names an output file directly: it goes through `paths::stem` or
  `paths::file_name`, which leave ordinary names exactly as they are. `extract` keeps the folders of an archive
  and checks each member's path itself (`safe_join` in `main.rs`).
- A fix comes with a test that fails without it; synthetic inputs are built in the tests (see the builders in
  `tests/conversion.rs`), never copied from the game.
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
