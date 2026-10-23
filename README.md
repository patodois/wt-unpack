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
- **Sounds:** FMOD Studio sound banks (`sound/*.assets.bank`). Samples come out as Ogg Vorbis, copied
  without re-encoding, or as WAV, including FMOD's own FADPCM codec.
- **Export:** vehicles and terrain to glTF 2.0, which opens in Blender, three.js, Godot and most 3D tools;
  terrain heights to 16-bit PNG.
- **Viewer:** `wt-viewer`, a [Bevy](https://bevyengine.org) app that opens a vehicle straight from the
  game files, with no export step. Tanks are posable (turret, gun, wheels, tracks) within the limits in
  their unit file, and a drive mode puts them on flat ground with a simple driving model.
- **Live telemetry:** `wt-unpack live` follows a running game through its own read-only HTTP API on
  `localhost:8111` (see below).

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

# Sound banks: list the samples, then extract some (Vorbis to .ogg, FADPCM to .wav; --format wav decodes all)
wt-unpack sound list $GAME/sound/tanks_engines.assets.bank
wt-unpack sound extract $GAME/sound/tanks_weapons.assets.bank --filter dshk_close -o out/dshk
wt-unpack sound check $GAME/sound --decode

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

## Live telemetry (official localhost:8111 API)

While a match or test drive runs, the War Thunder client itself serves read-only JSON on
`http://localhost:8111`, the same data its in-game browser pages and community overlays use.
`wt-unpack live` polls it and prints one line about the player's vehicle; `src/live.rs` is the library
side, for feeding the live state into a scene.

It only reads what the game publishes on that port: the player's own gauges and the objects already drawn
on the player's minimap. It never reads the game's memory, injects anything or modifies the client, and
it adds no markers or information the player does not already have.

| Endpoint | Used for |
|---|---|
| `/indicators` | Vehicle kind (`army`) and unit (`type`), speed, gearbox (`gear`, `gear_neutral`), RPM, crew; throttle and compass for aircraft |
| `/state` | Aircraft flight model: altitude, true airspeed, throttle, landing gear (always `{"valid": false}` for ground vehicles) |
| `/map_info.json` | The minimap's bounds in metres (`map_min`, `map_max`), read once per match |
| `/map_obj.json` | The player's icon (`"icon": "Player"`): position and facing on the minimap |
| `/map.img` | The minimap image (library only) |

Positions come out in metres in the crate's glTF convention, +Y up and right-handed, with map north on
+Z, so +X points west:

```text
east  = map_min[0] + x * (map_max[0] - map_min[0])       # x, y: map_obj position, 0..1, y downwards
north = map_max[1] - y * (map_max[1] - map_min[1])
[X, Y, Z] = [-east, altitude, north]                     # altitude: "H, m" for aircraft, else 0
```

Headings are compass degrees, clockwise from north; heading `h` points along `[-sin h, 0, cos h]`. The
module docs in `src/live.rs` have the details. Every field the game sends is kept as raw JSON next to the
typed ones, so new or renamed fields survive a game update.

```sh
cargo build --release --features live

# Start War Thunder, open a test drive (or a battle), then:
wt-unpack live                        # one line per poll, 5 per second
wt-unpack live --hz 10 --log run.jsonl # also append one JSON line per poll to a file

# Without the game: a built-in mock server with a tank driving in a circle
wt-unpack live --mock --count 20
```

A line looks like `tank us_m4a1_76w_sherman  23.4 km/h  hdg 037  pos -900.0 0.0 1300.0 m  thr -  gear 3`.
Gears show as `R`, `N` and `1`..`n`; speed is negative in reverse, and tanks report no throttle. With the
game closed it says it is waiting; in the hangar it says the game is not in a match, and it polls once a
second until a match starts. The JSON log holds the combined values, the raw `/state` and `/indicators`,
the player's own minimap entry, and `map_info` on the first poll of each match.

About 2% of polls hiccup: `/map_obj.json` answers with an empty body, or a request times out. In a match
the poller bridges these for up to 2 seconds with the last good answer (the log lists them under `held`);
an empty body is logged as a note, not an error. A refused connection or `{"valid": false}` ends the
match at once.

A test drive with a T-26 confirmed the units (speed in km/h, positions in metres: the speed derived from
the positions matches the speedometer within about 1 km/h), the heading, and the gear numbering. Not
yet checked: whether map north is +Z of the level geometry exported from the game files.

The field names and units come from open-source community clients, chiefly the
[WarThunder](https://github.com/PowerBroker2/WarThunder) Python package, and the samples the tests serve
(`samples/live/`) are written by hand in that shape. They are not recordings of the game.

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
- The sound bank and FSB5 readers follow the layouts documented by the community in
  [vgmstream](https://github.com/vgmstream/vgmstream) (ISC) and
  [python-fsb5](https://github.com/HearthSim/python-fsb5) (MIT); no code was copied. FSB5 Vorbis drops
  the stream headers, so they are rebuilt from FMOD's fixed set of setup headers, which python-fsb5
  extracted and vgmstream ships (`src/sound/vorbis_setups.bin.zst`, under vgmstream's ISC licence in
  `src/sound/LICENSE-vgmstream`).
  Vorbis decoding to WAV uses [lewton](https://github.com/RustAudio/lewton).
- The camouflage blend is an approximation, because the game's shaders are not public. The base
  texture's alpha marks painted areas, and the camouflage and occlusion maps are sampled through the
  mesh's second UV set.

## Testing

```sh
cargo test                                     # unit tests; no game files needed
cargo test --features live                     # also the live telemetry client, against a mock server
WT_GAME_DIR=$GAME cargo test --release         # also runs the tests that read your installed game
```

Checked against War Thunder 2.59.0.80 on macOS:

| What | Result |
|---|---|
| Files in the nine data archives | 38,737 extracted |
| Textures | 148,335, all decoded except 375 stored as deltas to a paired base texture |
| Models | tanks 3,656/3,656, aircraft 1,695/1,695, ships 1,925/1,925, with every skeleton |
| Levels | 160/160 parse fully: terrain from `HM2` in 86, from a land ray tracer in 158; 8,717,713 placed objects |
| Sound samples | 156,223 in 123 banks decoded: 148,743 Vorbis (91.7 hours) and 7,480 FADPCM |

## Disclaimer

Not affiliated with or endorsed by Gaijin Entertainment. War Thunder is a trademark of Gaijin
Entertainment. Use these tools to read and study game data you own. Do not use them for cheating, and do
not redistribute game assets.

## License

Licensed under either of MIT or Apache-2.0, at your option.
