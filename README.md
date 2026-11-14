# wt-unpack

[![rewrite-bridge on crates.io](https://img.shields.io/crates/v/rewrite-bridge.svg?label=rewrite-bridge)](https://crates.io/crates/rewrite-bridge)
[![rewrite-bridge docs](https://img.shields.io/docsrs/rewrite-bridge?label=docs.rs)](https://docs.rs/rewrite-bridge)

wt-unpack provides Rust tools for asset-tool and game-rewrite developers to read War Thunder's Dagor Engine data from their own installation and export models, textures, level scenes and sounds.

It is for people who want to look at, convert or study War Thunder's assets, and for people who write a **game
rewrite**: a new engine for an old game that loads the original's assets, possibly mixed with another game's
(a mash-up). For the second kind, wt-unpack is a *provider* for [rewrite-bridge](https://github.com/patodois/rewrite-bridge),
a small crate of model, animation and scene types that rewrites share (see [Using it from another game](#using-it-from-another-game)).

**Adding this to a game.** Read the [guide to adding a game](https://github.com/patodois/rewrite-bridge/blob/main/docs/INTEGRATING.md) first. Here: build with the `bridge` feature and open the provider with `wt_unpack::bridge::WarThunder::new(game_folder)`; you pass the War Thunder folder, nothing searches for it.

- **Archives and configs:** `.vromfs.bin` archives (`VRFs`/`VRFx`, stored or zstd-packed) and binary BLK
  files in all six formats, including slim BLKs that rely on the archive's shared name map and zstd
  dictionary. BLKs convert to Dagor's text syntax or to JSON.
- **Vehicles:** tanks, aircraft and ships from `.grp` packs, assembled with their skeleton, tracks and
  materials. Textures come from `.dxp.bin` packs, with the camouflage and ambient occlusion baked in and
  Dagor's packed normal maps split into PBR metal/roughness and normal textures.
- **Levels:** the binary level dumps in `levels/` (`DBLD`): their blocks, the terrain (the `HM2`
  heightmap, or the landmesh's collision mesh), the ground's land classes and weights, and the placed
  objects (rendinst) with their transforms, rotations and real models. With the bridge feature a level
  loads as a whole scene ([docs/LEVELS.md](docs/LEVELS.md)); the camera-facing leaf cards of its trees,
  which a `Scene` cannot hold, come beside it (`WarThunder::load_level_with_leaves`).
- **Rigs and collision:** the data a game rewrite needs to drive a tank (turret limits, drive train, wheels,
  track belts, named attachment points) as plain structs, and the collision shapes of placed objects
  ([docs/BRIDGE.md](docs/BRIDGE.md)).
- **Sounds:** FMOD Studio sound banks (`sound/*.assets.bank`). Samples come out as Ogg Vorbis, copied
  without re-encoding, or as WAV, including FMOD's own FADPCM codec.
- **Export:** vehicles and terrain to glTF 2.0, which opens in Blender, three.js, Godot and most 3D tools;
  terrain heights to 16-bit PNG.
- **Viewer:** `wt-viewer`, a [Bevy](https://bevyengine.org) app that opens a vehicle straight from the
  game files, with no export step. Tanks are posable (turret, gun, wheels, tracks) within the limits in
  their unit file, and a drive mode puts them on flat ground with a simple driving model.
- **Live telemetry:** `wt-unpack live` follows a running game through its own read-only HTTP API on
  `localhost:8111` ([docs/LIVE.md](docs/LIVE.md)).

This repository contains no game code or game data, and the tools never modify the game. Everything is
read from a copy of the game you already have installed. Words you may not know are explained in the
[glossary](#glossary) at the end.

## Download

Programs built for each release are on the [Releases page](https://github.com/patodois/wt-unpack/releases). Pick the archive for your system:

| System | Archive |
|--------|---------|
| macOS, Apple silicon | `wt-unpack-<tag>-macos-arm64.tar.gz` |
| macOS, Intel | `wt-unpack-<tag>-macos-x86_64.tar.gz` |
| Linux, x86_64 (glibc 2.35 or newer) | `wt-unpack-<tag>-linux-x86_64.tar.gz` |
| Windows, x86_64 | `wt-unpack-<tag>-windows-x86_64.zip` |

Each archive holds the program, this README, both license files and an `INSTALL.txt`. `SHA256SUMS` on the release page has
the checksum of every archive (`shasum -a 256 -c SHA256SUMS` on macOS, `sha256sum -c SHA256SUMS` on Linux,
`Get-FileHash` on Windows). They are built with the `live` and `bridge` features, so `wt-unpack live` and the rewrite-bridge commands are included. The 3D viewer (`wt-viewer`) is not in the archives: it needs extra system libraries to build on Linux, so build it from source (see [Build](#build)).

`SHA256SUMS.sig` on the same release signs `SHA256SUMS` with the project's Ed25519 release key, so you can also check
that the list itself came from here. With the downloads in one folder (needs OpenSSL 3):

```sh
sh tools/verify-release.sh wt-unpack v0.5.1 path/to/folder
```

The signed text is `osp release patodois/wt-unpack <tag>`, a newline, then `SHA256SUMS`, so a signature from another release
does not verify. The public key is in `RELEASE-KEY.pem` (SHA-256 of its DER form
`26c35ed9930be42fd266852135e2e81eb74c36ed9a2d0c527e86210a3dcb0946`), and the script carries the same key.

**The binaries contain no game data.** They are the program only; it reads the files of a copy of War Thunder that you already have
installed.

They are not code-signed, so the first run may be blocked:

- **macOS (Gatekeeper)**: a file downloaded in a browser is quarantined, and macOS refuses to open it ("Apple could not
  verify ..."). Remove the mark once, from the folder you extracted to: `xattr -d com.apple.quarantine ./wt-unpack`. Or open
  System Settings > Privacy & Security and press "Open Anyway" after the first refusal.
- **Windows (SmartScreen)**: "Windows protected your PC" appears the first time. Choose "More info", then "Run anyway". Or
  right-click the downloaded `.zip`, open Properties and tick "Unblock" before you extract it.

To build it yourself instead, see [Build](#build).

## Prerequisites

- **Rust 1.90 or newer** ([rustup.rs](https://rustup.rs)); the code is written and tested with 1.99.
- **War Thunder, installed.** The tools read the game's own folder, the one that holds `aces.vromfs.bin`,
  `levels/`, `content/` and `sound/`. Call it `$GAME`:
  - **macOS** (the launcher's app bundle): `/Applications/WarThunderLauncher.app/Contents/WarThunder.app/Contents/Resources/game`.
    This is also the default of `--game`.
  - **Windows**: the install folder; with Steam usually `C:\Program Files (x86)\Steam\steamapps\common\War Thunder`.
  - **Linux**: with Steam usually `~/.steam/steam/steamapps/common/War Thunder`.
  - Any other place works too; give it to commands that take `--game`, or write it into the paths you pass.
- **Memory and disk.** Most commands need well under 1 GB of RAM; the exceptions are said next to the command
  below. Output is small except for glTF exports of whole terrains.

## Build

```sh
git clone https://github.com/patodois/wt-unpack && cd wt-unpack
cargo build --release              # wt-unpack CLI and library: about a minute on a recent machine, plus fetching the dependencies
cargo build --release -p wt-viewer # the 3D viewer: several minutes the first time (Bevy)
export PATH=$PWD/target/release:$PATH   # so that `wt-unpack ...` below finds the program
```

Without the `export` line, write `cargo run --release -- list ...` or `target/release/wt-unpack list ...`
instead of `wt-unpack list ...`.

The commands that talk to rewrite-bridge (`check-bridge`, `check-conformance`, `rig`, `level --scene-stats`) only
exist when built with the `bridge` feature, which downloads [rewrite-bridge](https://crates.io/crates/rewrite-bridge) from crates.io; `live` needs the `live` feature:

```sh
cargo build --release --features bridge        # or: --features live,bridge
```

## Try it

A first look takes a few seconds:

```sh
GAME="/Applications/WarThunderLauncher.app/Contents/WarThunder.app/Contents/Resources/game"   # your game folder

wt-unpack list $GAME/char.vromfs.bin | head
wt-unpack grp $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp
wt-unpack model $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp m4a1_76w_sherman -o out/sherman
```

`-o` is a **folder**: `out/sherman` is created and gets the glTF, its `.bin` and the PNG textures (about 40
files). The same holds for `extract -o` and `sound extract -o`. Open `out/sherman/*.gltf` in Blender, three.js or
Godot. (`level --heightmap` takes a file name, `level --gltf` a folder.)

## Usage

`$GAME` is the game folder (see Prerequisites). Commands marked **heavy** read a lot of the game; times are
from a Mac with an SSD and 16 GB, on War Thunder 2.59.0.80.

```sh
# Archives and BLKs
wt-unpack list $GAME/char.vromfs.bin
wt-unpack extract $GAME/aces.vromfs.bin -o out/aces --blk json --filter tankmodels
wt-unpack cat $GAME/char.vromfs.bin:config/wpcost.blk | head   # it is 1.16 million lines: pipe it

# What a .grp pack holds, and the LODs, vertex layouts and skeleton of one model
wt-unpack grp $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp
wt-unpack grp $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp --model m4a1_76w_sherman

# Export a vehicle to glTF with its textures, into the folder given with -o
wt-unpack model $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp m4a1_76w_sherman -o out/sherman

# HEAVY: parse every model in a folder and summarise failures (a regression check): the tanks take 40 s and 0.14 GB
wt-unpack check $GAME/content/base/res/tanks

# The blocks of a level, its terrain as a 16-bit PNG (plus a JSON with its world placement) and as glTF,
# and its placed objects
wt-unpack level $GAME/levels/avg_berlin.bin
wt-unpack level $GAME/levels/arcade_alps.bin --heightmap out/alps.png --gltf out/alps
wt-unpack level $GAME/levels/avg_berlin.bin --terrain landmesh --heightmap out/berlin_wide.png
wt-unpack level $GAME/levels/avg_berlin.bin --objects

# The level as a rewrite-bridge scene (needs --features bridge): the models, the instances, the memory
# estimate, and what is not in the scene. Berlin takes about 7 s and 1.3 GB.
wt-unpack level $GAME/levels/avg_berlin.bin --scene-stats

# HEAVY: parse every level and summarise what decodes: 10 s, 0.4 GB
wt-unpack check-levels $GAME/levels

# Sound banks: list the samples, then extract some (Vorbis to .ogg, FADPCM to .wav; --format wav decodes all)
wt-unpack sound list $GAME/sound/tanks_engines.assets.bank
wt-unpack sound extract $GAME/sound/tanks_weapons.assets.bank --filter dshk_close -o out/dshk
wt-unpack sound check $GAME/sound            # headers of every bank: under a second
wt-unpack sound check $GAME/sound --decode   # HEAVY: decodes all 156,223 samples: 10 to 12 minutes, about 0.7 GB

# Open a vehicle in 3D: drag to orbit, scroll to zoom, Q/E turn the turret, R/F elevate the gun
wt-viewer $GAME/content/base/res/tanks/germ_pzkpfw_v_ausf_g_panther.grp pzkpfw_V_ausf_g_panther --game $GAME

# Drive it on flat ground with WASD; the camera follows and the speed shows in a corner
wt-viewer $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp m4a1_76w_sherman --drive

# Save a picture and close: turret 60 degrees left, gun 10 degrees up; or after a scripted drive
wt-viewer --turret 60 --gun 10 --screenshot sherman.png
wt-viewer --drive --auto W:30,WA:3 --turret 90 --screenshot drive.png
```

`wt-viewer --help` prints the options; a mistyped option is refused with the same text, and nothing opens. They are:

| Option | Meaning |
|---|---|
| `PACK.grp MODEL` | the model to open (a `.grp` of `content/base/res/tanks`, `aircrafts` or `ships`, and a model in it); with neither it opens the Sherman |
| `--game <folder>` | the game folder `$GAME` (default: the macOS launcher's) |
| `--unit <name>` | the unit BLK for the turret limits and drive train, when it is not found from the model's name |
| `--turret <deg>`, `--gun <deg>` | starting traverse (positive left) and elevation (positive up), within the unit's limits |
| `--orbit <yaw,pitch,dist>`, `--target <x,y,z>` | starting camera: angles in degrees and distance in metres; the point it looks at (model space, metres) |
| `--lift <node=metres>` | raise a skeleton node, to see the skinned tracks follow it (comma-separate several) |
| `--drive` | drive on flat ground with WASD |
| `--auto <script>` | drive by script (with `--drive`): `W:20,WA:3,S:2` holds W for 20 s, then W and A for 3 s, then S for 2 s |
| `--screenshot <file.png>` | save one frame (after the script, if any) and close; a relative path lands in the folder you ran it from |
| `-h`, `--help` | print this list |

The viewer finds the tank's unit file (`gamedata/units/tankmodels/<unit>.blk` in `aces.vromfs.bin`) by
its model name, or takes `--unit <name>`. From it come the turret and gun nodes with their traverse and
elevation limits and speeds, and the mass, engine power, gearing, sprocket and wheel radii and braking
force. Glass materials are drawn see-through.

The driving model is a simple kinematic one, not the game's (which is not public): engine power and
the unit's acceleration limit set the acceleration, the gearing at maximum RPM sets the top speeds (38.6
km/h forward and 5.0 km/h reverse for the M4A1 (76) W), and the hull turns by running one track slower,
pivoting with one track in first gear at a standstill. How wheels, tracks and materials are put together is in
[docs/VEHICLES.md](docs/VEHICLES.md).

### What the check commands should print

`check`, `check-levels` and `sound check` are regression checks for the readers; on War Thunder 2.59.0.80:

| Command | Expected |
|---|---|
| `check <folder>/tanks` | 3,656 of 3,656 models parse (aircraft 1,695 of 1,695, ships 1,925 of 1,925, with every skeleton) |
| `check-levels $GAME/levels` | 160 of 160 levels parse fully; terrain from `HM2` in 86, from a land ray tracer in 158; 8,717,713 placed objects |
| `sound check --decode` | 156,223 samples decode (148,743 Vorbis, 7,480 FADPCM) |

If your numbers are far from these, the game has been updated; open an issue with the version.

## Using it from another game

With the `bridge` feature, `wt_unpack::bridge::WarThunder` implements rewrite-bridge's `Provider`: vehicles
(with sockets), rigs, levels as scenes and level collision, so another Rust game rewrite can load War Thunder's
tanks into its scenes without knowing Dagor's formats.

```toml
[dependencies]
wt-unpack = { git = "https://github.com/patodois/wt-unpack", features = ["bridge"] }
# crates.io gives every crate in the build the same rewrite-bridge: all 0.4.x requirements share one copy
rewrite-bridge = "0.4"
```

Every crate in one build has to share one copy of rewrite-bridge, or `WarThunder` implements a `Provider` that
your code's `Provider` is not ("multiple different versions of crate `rewrite_bridge`"). From crates.io Cargo does
that for you: this crate asks for `0.4`, your `"0.4"` accepts it, and both resolve to one 0.4.x release. Another
minor line (`"0.3"`) would be a second copy. `cargo tree -i rewrite-bridge` must show a single copy.

See [docs/BRIDGE.md](docs/BRIDGE.md) for the ids, axes, sockets, the rig and level collision,
[docs/LEVELS.md](docs/LEVELS.md) for scenes, [docs/MEMORY.md](docs/MEMORY.md) for what a consumer should do about
memory, and rewrite-bridge's [guide to writing a provider](https://github.com/patodois/rewrite-bridge/blob/main/docs/WRITING_A_PROVIDER.md)
for what a provider is and how the pieces fit.

The commands that exercise the provider (build with `--features bridge`):

```sh
wt-unpack rig tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman   # print a tank's rig
wt-unpack check-bridge --list --filter tanks/germ_       # only print the ids of the vehicles that match
wt-unpack check-bridge --filter tanks/germ_ --limit 20   # load and validate those
wt-unpack check-bridge                                   # HEAVY: every tank, aircraft and ship
wt-unpack check-conformance --filter germ_he_115 -v      # rewrite-bridge's conformance checker
```

### check-bridge

Loads each vehicle through the provider, validates it and prints a summary per folder (`tanks`, `aircrafts`,
`ships`). With no arguments it checks all 3,245 vehicles, and says so first:

```text
checking 3245 vehicles (geometry and materials, no textures: add --textures to bake them; listed in 0.2 s); one progress line per 50, Ctrl-C stops
  250/3245  elapsed 0:06  343 MB resident (peak 488 MB)  about 1:13 left
```

The time left is a rough guess from the speed so far (ships take longer than aircraft).

- `--filter TEXT` keeps the vehicles whose id contains the text (an id is `tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman`,
  so `tanks/`, `usa_`, `germ_he_` all work); `--list` only prints the ids; `--limit N` stops after N;
  `--skip N` starts later; `--progress N` sets the interval of the progress lines.
- **Without textures** (the default) it checks geometry, skeleton, materials and sockets: all 3,245 vehicles in
  **3 minutes 20 seconds**, between 0.3 and 0.9 GB of resident memory, never above **1 GB**.
- **`--textures`** also decodes and bakes every texture, camouflage included. That is the full load a game
  would do, but a large ship needs 2 to 3 GB for itself and one vehicle takes 0.3 to 6 seconds, so a full run takes
  **more than an hour** (the first 1,300 vehicles took 20 minutes). Try it with `--filter` and `--limit`.
- Expected: 3,245 of 3,245 load and validate (1,122 aircraft, 718 ships, 1,405 tanks), and a few
  `warning: parts without a skeleton node, left untransformed: [...]` lines, which are information about
  decorative parts of the game's data, not failures. With `--textures`, lines such as
  `texture bf_109e_1_n: Texture is not supported: stored as a difference to a paired base texture` are expected too:
  375 textures are stored as deltas to a paired base texture and are not decoded.

Memory is flat because nothing is kept: the provider holds about 140 MB of indexes and a vehicle is dropped before
the next one loads. With textures the resident size still follows the heaviest vehicles (a B-52 at 869,000 triangles
takes 2 GB to bake), and macOS gives the freed pages back lazily, so it falls again some seconds later; see
[docs/MEMORY.md](docs/MEMORY.md).

### check-conformance

Runs rewrite-bridge's conformance checker over the vehicles (and levels) the provider lists: scale, orientation,
winding, skins, materials. `--help` lists the checker's options: `--id` (repeatable), `--filter`, `--kind`,
`--limit`, `--compare-load`, `--allow`, `-v`, `--strict`. They go straight after the command, with no `--` needed
(`--game` and `--no-textures` also work anywhere). `--no-textures` skips texture baking, which is much faster.

```sh
wt-unpack check-conformance --filter usa_m4a1_76w_sherman
```

checks the Sherman and its `cn_` variant and prints two warnings per model,
`material.opaque_transparent_texture`: a few parts of the game use materials whose alpha is a paint or shine mask,
not transparency, so the bridge's material stays opaque. This is deliberate; see "Materials and winding" in
[docs/VEHICLES.md](docs/VEHICLES.md#materials-and-winding). Exit status 1 means an asset has an error; warnings do
not change it unless `--strict`.

## Live telemetry

`wt-unpack live` (build with `--features live`) follows a running game through the read-only API the game itself
serves on `localhost:8111`, and prints one line about the player's vehicle. It never reads the game's memory.
Details and the JSON log are in [docs/LIVE.md](docs/LIVE.md).

## Testing

```sh
cargo test                                     # unit tests; no game files needed
cargo test --features live,bridge              # also the live telemetry client (against a mock server) and the bridge
WT_GAME_DIR=$GAME cargo test --release         # also runs the tests that read your installed game
WT_GAME_DIR=$GAME cargo test --release --features live,bridge   # all of it: the Sherman's rig and sockets, every
                                               # collision resource, Berlin's scene and collision, and the
                                               # memory check (tests/memory.rs)
```

Checked against War Thunder 2.59.0.80 on macOS:

| What | Result |
|---|---|
| Files in the nine data archives | 38,737 extracted |
| Textures | 148,335, all decoded except 375 stored as deltas to a paired base texture |
| Models | tanks 3,656/3,656, aircraft 1,695/1,695, ships 1,925/1,925, with every skeleton |
| Levels | 160/160 parse fully: terrain from `HM2` in 86, from a land ray tracer in 158; 8,717,713 placed objects |
| Level scenes | 160/160 load and validate through the bridge in 0.8 to 33 s (two runs at once), peak RSS 0.60 to 1.79 GB (avg_breslau), where it was 0.65 to 1.96 GB; every placed object's pool resolves to a model, and 225 models that are only decals have nothing to draw |
| Level collision | 160/160 levels: 28,490 placed objects have collision resources and 0 fail to decode; 2,520 have none (debris) |
| Collision resources | 19,799/19,799 decode (57,415 nodes, 11.5 million triangles), every vertex inside its node's box |
| Vehicle rigs and bridge | `check-bridge`: 3,245/3,245 vehicles load and validate (see "check-bridge" above) |
| Sound samples | 156,223 in 123 banks decoded: 148,743 Vorbis (91.7 hours) and 7,480 FADPCM |

## More documentation

- [docs/BRIDGE.md](docs/BRIDGE.md): the rewrite-bridge provider: ids, sockets, the rig, level collision
- [docs/VEHICLES.md](docs/VEHICLES.md): wheels, tracks, materials and winding
- [docs/LEVELS.md](docs/LEVELS.md): levels as scenes
- [docs/MEMORY.md](docs/MEMORY.md): memory of loading levels and vehicles, and the allocator
- [docs/LIVE.md](docs/LIVE.md): the live telemetry client
- [docs/HOW_IT_WORKS.md](docs/HOW_IT_WORKS.md): where the formats come from
- [rewrite-bridge](https://github.com/patodois/rewrite-bridge): the shared types and the conformance checker
- [bridge-testbed](https://github.com/patodois/bridge-testbed): a Bevy app that loads the assets of every provider
  side by side, to compare scale, orientation, animation and materials across games (it needs your own installs)
- The other providers of rewrite-bridge, for other games: [wf-unpack](https://github.com/patodois/wf-unpack) (Warframe),
  [hytale-unpack](https://github.com/patodois/hytale-unpack) (Hytale) and
  [nocturne-unpack](https://github.com/patodois/nocturne-unpack) (Shin Megami Tensei III Nocturne HD Remaster)

## Glossary

- **Game rewrite**: a new engine that loads an old game's original assets. A **mash-up** loads assets of
  several games into one.
- **rewrite-bridge**, **Provider**: the shared crate, and the trait a game implements to hand out its
  assets as the crate's `Model`, `AnimatedModel` and `Scene` types. wt-unpack is the War Thunder provider; the
  others are listed under "More documentation". **Socket**: a named node of a model to attach things to (a
  turret, a muzzle).
- **bridge-testbed**: the app that loads the models of several providers into one scene to check that scales,
  axes and animations agree; it is how a provider is looked at before a game rewrite relies on it.
- **vromfs** (`.vromfs.bin`): Dagor's archive format. **BLK**: Dagor's configuration file format, text or binary.
- **grp**: a pack of models and their skeletons. **DynModel**: a movable model (a vehicle), as opposed to a
  **rendinst** ("render instance"), a static object placed in a level (a tree, a house).
- **dxp**: a texture pack (`.dxp.bin`). **DDSx**: Dagor's texture format.
- **landmesh**: the level's ground as a mesh and its collision. **HM2**: the compressed heightmap of the
  playable area of a level. **DBLD**: the container format of the files in `levels/`.
- **glTF**: the open 3D file format the exports use. **PBR**: physically based materials (base colour,
  metal and roughness, normal map).
- **RSS**: resident set size, the memory a process holds in RAM. **Footprint** is macOS's stricter count.

## Disclaimer

Not affiliated with or endorsed by Gaijin Entertainment. War Thunder is a trademark of Gaijin
Entertainment. Use these tools to read and study game data you own. Do not use them for cheating, and do
not redistribute game assets.

## Dependencies and credit

Every dependency comes from [crates.io](https://crates.io): the build has no git dependency, and the format
decoders are pure Rust.

- [wt-formats](https://crates.io/crates/wt-formats) (ours) reads GRP packs, DXP and DDSx textures, DynModels and
  skeletons. Its README credits [wt_blk](https://github.com/Warthunder-Open-Source-Foundation/wt_blk), which
  documented these formats first.
- The Oodle decoder is [lztans](https://crates.io/crates/lztans) (ours), written in a clean room: from a prose
  specification of the format, by an implementer who read no existing decoder.
- Releases v0.1.0 and v0.2.0 (source and binaries) used [oozextract](https://crates.io/crates/oozextract) 0.5
  for Oodle. Its license field says MIT, but its code appears to be a port of [ooz](https://github.com/powzix/ooz),
  which is GPL-3.0, so those two releases may carry GPL-3.0 obligations. v0.3.0 and later contain none of that
  code: use them.
- Block-compressed textures are decoded by [bcn-decode](https://crates.io/crates/bcn-decode) (ours), written from
  the public specifications; the earlier build used texture2ddecoder, which is no longer a dependency.
- zstd is decoded by [ruzstd](https://crates.io/crates/ruzstd); the C zstd library is used only by the tests, to
  build test data.
- The Vorbis setup table in `src/sound/vorbis_setups.bin.zst` comes from [vgmstream](https://github.com/vgmstream/vgmstream)
  (ISC, `src/sound/LICENSE-vgmstream`). Optional features add [rewrite-bridge](https://crates.io/crates/rewrite-bridge)
  (`bridge`) and [ureq](https://crates.io/crates/ureq) (`live`); the viewer is built on [Bevy](https://bevyengine.org).
- The layouts follow the open-source [Dagor Engine](https://github.com/GaijinEntertainment/DagorEngine)
  (BSD-3-Clause), the sound formats the community documentation of vgmstream and python-fsb5; no code was copied
  (see [docs/HOW_IT_WORKS.md](docs/HOW_IT_WORKS.md)).

## Changes since 0.5.1

- Sounds: an `.ogg` page that begins exactly at a packet boundary is no longer flagged as continuing a packet.
  The wrong flag appeared after 255 packets of 16 bytes or less in a row (a long silence) and made players that
  follow the Ogg rules drop one packet. Extract again any `.ogg` you keep from an earlier version.

## Changes in 0.5.1

- Archives and BLK files have accounting limits for parsing: names and file ranges copied out of a
  VROMFS table, BLK parameter storage, block tables and copied text are checked before reservation.
  A zstd window is limited to the greater of 8 MiB or twice the remaining decoded-output limit.
  These are allocation accounting limits, not a bound on process RSS or a guarantee against running
  out of memory. Decoder buffers, the decoded dump, templates and the result can overlap in memory;
  allocator overhead and the caller's input are additional costs.
  BLK text and storage have separate quotas. Name-table slots have a 64 MiB ceiling and block-table storage a 256 MiB ceiling checked before reservation; compressing a block tree better does not lower its limit.
  The block ceiling allows 1,398,101 records on 64-bit targets, 8.45 times the largest measured real table (165,374).
  Parameter templates, use counters, name storage and all output fields share a quota of 32 times
  the decoded dump size, with a 64 KiB floor and a 1 GiB ceiling. Copied strings have a separate
  16-times quota, with a 64 KiB floor and 256 MiB ceiling. A compressed dump can reach 1 GiB before
  its counts can be checked. zstd output drains directly into its destination without an
  intermediate full-output vector.
  On macOS arm64, a synthetic 32,865-byte BLK with a 1 GiB zstd window and 1,073,741,815 decoded
  bytes used 3,810,164,736 bytes peak RSS before direct draining and 1,818,935,296 after it for
  decompression alone. Parsing that accepted dump (6,381,900 parameters, 349,525 block records)
  peaked at 3,780,804,608 bytes; a related dump refused at 12,183,668 parameter templates still
  peaked at 3,223,437,312 bytes. These are measurements of those cases, not universal maxima. Other synthetic inputs exceeded 4 GB RSS
  before the measurement was stopped, including a 101,514-byte stream with a 2 GiB window that reached
  at least 4.6 GB before being refused.
- `cat` and `extract` stream converted BLK text and pretty JSON through a buffered writer. They keep
  the parsed BLK but no complete encoded document or JSON value tree. The additive library methods
  `Block::write_text_to` and `Block::write_json_to` expose the same behavior. JSON still groups borrowed
  entries by name to preserve ordering and repeated-name arrays: its auxiliary index grows with the
  number of entries in each open block. Text needs only the block traversal and bounded numeric
  formatting temporaries. There is no encoded-byte, elapsed-time or disk-space quota on either writer;
  for example, one Matrix parameter can produce 527 text bytes from an 8-byte table entry. The legacy
  `to_text()` and `to_json()` APIs still allocate their entire result, without an extra conversion quota;
  callers needing bounded output buffering should use the writer methods. Manually constructed
  `Block` trees are not subject to the parser's depth or storage limits.
- `extract` retains its existing success status (0) when all selected safe files have been written,
  including binary BLKs that could not be converted and were saved unchanged under their original names.
  This preserves the raw-fallback behavior of existing extraction scripts. Besides each file's diagnostic,
  stderr ends with `kept raw: N BLK files` and counts grouped by refusal reason, including the requested
  count and applicable limit for quota errors. Scripts requiring conversion of every BLK should check
  that summary for zero. Unsafe paths are skipped, as before. Archive/read/write failures exit nonzero;
  streamed write failures can leave a partial output file. `cat` exits nonzero on a refused binary BLK
  and does not substitute raw data. An invalid shared name map continues to abort either command before
  extraction starts, even with `extract --blk raw` or a filter: the archive-wide context is validated once,
  rather than silently hiding a damaged shared map in some modes.
- The BLK and level readers refuse damaged counts earlier, and lztans 0.1.1 is the minimum accepted version.

## Changes in 0.5.0

- The `bridge` feature now uses rewrite-bridge 0.4. Nothing in the provider changed; a program that also depends on
  rewrite-bridge has to ask for `"0.4"` so the build holds one copy.
- `blk::SharedContext` is built with `SharedContext::new(names, dict)`, and its name list is read through `names()`.
  The `names` field is no longer public: the context adds up the bytes of the names once, and that total would be wrong
  if the list could change afterwards.
- Archives and BLK files are read with total limits: the names and file ranges copied out of a VROMFS table, the text
  copied out of a BLK, and the window of a zstd stream (at most the greater of 8 MiB or twice the output) are checked
  before they are allocated, so a damaged file fails with an error instead of exhausting memory.
- Textures that are reduced to fit a size limit keep shrinking after one side reaches a single pixel: a texture of
  256 by 1 limited to 128 now comes out 128 by 1 instead of staying 256 by 1. A stored Oodle block shorter than six
  bytes is accepted when its declared sizes are right.

## License

Licensed under either of MIT or Apache-2.0, at your option.
