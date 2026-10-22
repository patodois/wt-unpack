# wt-unpack

Rust tools for reading War Thunder's game data (Dagor Engine) from your own installation:

- **Archives and configs:** `.vromfs.bin` archives (`VRFs`/`VRFx`, stored or zstd-packed) and binary BLK
  files in all six formats, including slim BLKs that rely on the archive's shared name map and zstd
  dictionary. BLKs convert to Dagor's text syntax or to JSON.
- **Vehicles:** tanks, aircraft and ships from `.grp` packs, assembled with their skeleton, tracks and
  materials. Textures come from `.dxp.bin` packs, with the camouflage and ambient occlusion baked in and
  Dagor's packed normal maps split into PBR metal/roughness and normal textures.
- **Export:** vehicles to glTF 2.0, which opens in Blender, three.js, Godot and most 3D tools.
- **Viewer:** `wt-viewer`, a [Bevy](https://bevyengine.org) app that opens a vehicle straight from the
  game files, with no export step.

This repository contains no game code or game data, and the tools never modify the game. Everything is
read from a copy of the game you already have installed.

## Build

```sh
cargo build --release              # wt-unpack CLI and library
cargo build --release -p wt-viewer # the 3D viewer
```

## Usage

The examples use `$GAME` for the game folder. On macOS that is
`/Applications/WarThunderLauncher.app/Contents/WarThunder.app/Contents/Resources/game`.

```sh
# Archives and BLKs
wt-unpack list $GAME/char.vromfs.bin
wt-unpack extract $GAME/aces.vromfs.bin -o out/aces --blk json --filter tankmodels
wt-unpack cat $GAME/char.vromfs.bin:config/wpcost.blk

# What a .grp pack holds, and the LODs and vertex layouts of one model
wt-unpack grp $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp
wt-unpack grp $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp --model m4a1_76w_sherman

# Export a vehicle to glTF with its textures
wt-unpack model $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp m4a1_76w_sherman -o out/sherman

# Parse every model in a folder and summarise failures (a regression check)
wt-unpack check $GAME/content/base/res/tanks

# Open a vehicle in 3D: drag to orbit, scroll to zoom
wt-viewer $GAME/content/base/res/tanks/germ_pzkpfw_v_ausf_g_panther.grp pzkpfw_V_ausf_g_panther --game $GAME
```

## How it works

- The vromfs and BLK readers in `src/` are written from the open-source
  [Dagor Engine](https://github.com/GaijinEntertainment/DagorEngine) (BSD-3-Clause), mainly
  `vromfsLoad.cpp`, `dag_zstdObfuscate.h` and `blk_serialize.cpp`. No code was copied; the module docs
  describe the layouts.
- Texture (DDSx/DXP) and model (DynModel, GeomNodeTree, GRP) reading comes from
  [wt_blk](https://github.com/Warthunder-Open-Source-Foundation/wt_blk). Those readers were contributed
  there (Warthunder-Open-Source-Foundation/wt_blk#28 and #29), and this crate uses them from a fork
  until they are merged.
- Oodle-compressed data is decoded by [oozextract](https://github.com/lvlvllvlvllvlvl/oozextract), pure
  Rust with no proprietary library.
- Materials and texture names come from `dynModelDesc.bin`.
- The camouflage blend is an approximation, because the game's shaders are not public. The base
  texture's alpha marks painted areas, and the camouflage and occlusion maps are sampled through the
  mesh's second UV set.

## Testing

```sh
cargo test                                     # unit tests; no game files needed
WT_GAME_DIR=$GAME cargo test --release         # also runs the tests that read your installed game
```

Checked against War Thunder 2.59.0.80 on macOS:

| What | Result |
|---|---|
| Files in the nine data archives | 38,737 extracted |
| Textures | 148,335, all decoded except 375 stored as deltas to a paired base texture |
| Models | tanks 3,656/3,656, aircraft 1,695/1,695, ships 1,925/1,925, with every skeleton |

## Disclaimer

Not affiliated with or endorsed by Gaijin Entertainment. War Thunder is a trademark of Gaijin
Entertainment. Use these tools to read and study game data you own. Do not use them for cheating, and do
not redistribute game assets.

## License

Licensed under either of MIT or Apache-2.0, at your option.
