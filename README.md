# wt-unpack

Rust tools for reading War Thunder's game data (Dagor Engine) from your own installation:

- **Archives and configs:** `.vromfs.bin` archives (`VRFs`/`VRFx`, stored or zstd-packed) and binary BLK
  files in all six formats, including slim BLKs that rely on the archive's shared name map and zstd
  dictionary. BLKs convert to Dagor's text syntax or to JSON.
- **Vehicles:** tanks, aircraft and ships from `.grp` packs, assembled with their skeleton, tracks and
  materials. Textures come from `.dxp.bin` packs, with the camouflage and ambient occlusion baked in and
  Dagor's packed normal maps split into PBR metal/roughness and normal textures.
- **Levels:** the binary level dumps in `levels/` (`DBLD`): their blocks, the terrain (the `HM2`
  heightmap, or the landmesh's collision mesh) and the placed objects (rendinst) with their transforms.
- **Export:** vehicles and terrain to glTF 2.0, which opens in Blender, three.js, Godot and most 3D tools;
  terrain heights to 16-bit PNG.
- **Viewer:** `wt-viewer`, a [Bevy](https://bevyengine.org) app that opens a vehicle straight from the
  game files, with no export step. Tanks are posable (turret, gun, wheels, tracks) within the limits in
  their unit file, and a drive mode puts them on flat ground with a simple driving model.

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

# What a .grp pack holds, and the LODs, vertex layouts and skeleton of one model
wt-unpack grp $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp
wt-unpack grp $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp --model m4a1_76w_sherman

# Export a vehicle to glTF with its textures
wt-unpack model $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp m4a1_76w_sherman -o out/sherman

# Parse every model in a folder and summarise failures (a regression check)
wt-unpack check $GAME/content/base/res/tanks

# The blocks of a level, its terrain as a 16-bit PNG (plus a JSON with its world placement) and as glTF,
# and its placed objects
wt-unpack level $GAME/levels/avg_berlin.bin
wt-unpack level $GAME/levels/arcade_alps.bin --heightmap out/alps.png --gltf out/alps
wt-unpack level $GAME/levels/avg_berlin.bin --terrain landmesh --heightmap out/berlin_wide.png
wt-unpack level $GAME/levels/avg_berlin.bin --objects

# Parse every level and summarise what decodes
wt-unpack check-levels $GAME/levels

# Open a vehicle in 3D: drag to orbit, scroll to zoom, Q/E turn the turret, R/F elevate the gun
wt-viewer $GAME/content/base/res/tanks/germ_pzkpfw_v_ausf_g_panther.grp pzkpfw_V_ausf_g_panther --game $GAME

# Drive it on flat ground with WASD; the camera follows and the speed shows in a corner
wt-viewer $GAME/content/base/res/tanks/usa_m4a1_76w_sherman.grp m4a1_76w_sherman --drive

# Save a picture and close: turret 60 degrees left, gun 10 degrees up; or after a scripted drive
wt-viewer --turret 60 --gun 10 --screenshot sherman.png
wt-viewer --drive --auto W:30,WA:3 --turret 90 --screenshot drive.png
```

The viewer finds the tank's unit file (`gamedata/units/tankmodels/<unit>.blk` in `aces.vromfs.bin`) by
its model name, or takes `--unit <name>`. From it come the turret and gun nodes with their traverse and
elevation limits and speeds, and the mass, engine power, gearing, sprocket and wheel radii and braking
force. Glass materials are drawn see-through.

The driving model is a simple kinematic one, not the game's (which is not public): engine power and
the unit's acceleration limit set the acceleration, the gearing at maximum RPM sets the top speeds (38.6
km/h forward and 5.0 km/h reverse for the M4A1 (76) W), and the hull turns by running one track slower,
pivoting with one track in first gear at a standstill. The wheels roll with their track, and the track
texture runs along the static track mesh.

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
- Level files follow Dagor's `loadLevel.cpp`, `lmeshManager.cpp`, `heightmapPhysHandler.cpp`,
  `compressedHeightmap.cpp`, `landRayTracerSoA4.cpp`, `bvhIO.cpp` and `rendInstGen.cpp`. The `HM2`
  heightmap covers the playable area (2 m per sample on tank maps); the landmesh's ray tracer
  covers the whole map at its own resolution and is rasterised into the same kind of grid.
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
| Levels | 160/160 parse fully: terrain from `HM2` in 86, from a land ray tracer in 158; 8,717,713 placed objects |

## Disclaimer

Not affiliated with or endorsed by Gaijin Entertainment. War Thunder is a trademark of Gaijin
Entertainment. Use these tools to read and study game data you own. Do not use them for cheating, and do
not redistribute game assets.

## License

Licensed under either of MIT or Apache-2.0, at your option.
