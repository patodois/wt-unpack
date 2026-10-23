# wt-unpack

Rust tools for reading War Thunder's game data (Dagor Engine) from your own installation:

- **Archives and configs:** `.vromfs.bin` archives (`VRFs`/`VRFx`, stored or zstd-packed) and binary BLK
  files in all six formats, including slim BLKs that rely on the archive's shared name map and zstd
  dictionary. BLKs convert to Dagor's text syntax or to JSON.
- **Vehicles:** tanks, aircraft and ships from `.grp` packs, assembled with their skeleton, tracks and
  materials. Textures come from `.dxp.bin` packs, with the camouflage and ambient occlusion baked in and
  Dagor's packed normal maps split into PBR metal/roughness and normal textures.
- **Levels:** the binary level dumps in `levels/` (`DBLD`): their blocks, the terrain (the `HM2`
  heightmap, or the landmesh's collision mesh), the ground's land classes and weights, and the placed
  objects (rendinst) with their transforms, rotations and real models. With the bridge feature a level
  loads as a whole scene (see "Levels as scenes").
- **Rigs and collision:** the data a game rewrite needs to drive a tank (turret limits, drive train, wheels,
  track belts, named attachment points) as plain structs, and the collision shapes of placed objects. See
  "Using wt-unpack from another game".
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

# The level as a rewrite-bridge scene (needs --features bridge): the models, the instances, the memory
# estimate, and what is not in the scene
wt-unpack level $GAME/levels/avg_berlin.bin --scene-stats

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
pivoting with one track in first gear at a standstill.

### Wheels and tracks

`wt_unpack::gear` holds what moves when a tracked vehicle drives, as data and pure functions that the
viewer and the bridge provider both use. All of it comes from the game files:

- **Wheels** are the `wheel_<l|r>_*` nodes. Each turns about the lateral axis through its pivot by
  `distance / radius` for the distance its track travels. Radii come from the unit BLK
  (`suspension.defaultGearRadius`, `topGearRadius`, `frontBackGearRadius` and
  `mechanics.driveGearRadius`), and from the wheel's mesh only when a unit does not give one. For the
  Sherman they agree with the meshes within 4 mm for road wheels and idlers; the sprocket's is its pitch
  radius, inside the tips of its teeth.
- **Track skin.** The tracks are skinned meshes. `wt_blk` reads the bone table in front of each one, which
  binds every track vertex to the `root` node or to one suspension wheel node (`bone_wheel_<l|r>_<NN>`):
  the ring of track under a road wheel follows that wheel, the rest stays with the hull. There are no
  bones along the links. `vehicle::Mesh` carries them as `bones`, `joints` and `weights`, the bridge
  provider as a `Skin` with per-vertex joints and weights, and the viewer skins them on the GPU.
  `Mesh::skinned_positions` does the blending on the CPU. The game places the wheel nodes with its
  suspension simulation (`PhysSys` in the unit BLK), which is not reproduced: on flat ground the bones sit
  at their bind pose. `wt-viewer --lift bone_wheel_l_03=0.15` raises a node to show the skin follow it.
- **Track motion** is a texture scroll along the static mesh. The rate is the change in V per metre on the
  mesh's bottom run: 1.614 for the Sherman, which is a quarter of a V unit per link of the unit's
  `trackLen`, as in the other tracked vehicles of the game. It is the rate at which the tread stays still on the
  ground.

Left out, because the code that uses them is not published: the unit's `tracks.animationMultiplier` (0.68
for the Sherman) and the `texcoord_anim=1` / `-1` parameter of the left and right track materials in
dynModelDesc.bin. `Drive::track_animation_multiplier` reads the first; neither is applied.


## Bridge to other game rewrites

With the `bridge` feature, `wt_unpack::bridge::WarThunder` implements the `Provider` of the
rewrite-bridge crate (checked out next to this repository, as `../rewrite-bridge`), so other Rust game rewrites can load War Thunder's
vehicles into their scenes. Ids look like `tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman`. Models
follow glTF's conventions: Dagor's `(x, y, z)` becomes `(z, y, x)`, so vehicles face +Z. The turret,
gun and wheels stay separate nodes, turning about their pivots in the vehicle's axes. Tracks and the
antenna are skins with the game's own bones and weights (a track follows its `root` node and suspension
wheel nodes, see above), and glass blends.

```sh
cargo build --release --features bridge
wt-unpack check-bridge              # load and validate every tank, aircraft and ship
wt-unpack rig tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman   # print a tank's rig
wt-unpack check-conformance -- --filter germ_he_115 -v          # rewrite-bridge's conformance checker over the vehicles
```

### Materials and winding

The bridge material of a vehicle part follows the shader class and parameters `dynModelDesc.bin` gives it
(`vehicle::material_alpha`): glass classes, `*alpha_blend`, `aces_weapon_fire` and `propeller_side` blend; an
`atest=N` parameter makes any class a cut-out at N/255 (a few `masked_chrome_bump` parts carry it); the
`*atest` classes cut out at 0.5 without it; everything else is opaque, including the `masked_*` classes whose
alpha is a paint or shine mask (their texels are transparent in 14 to 95% of cases and the hull is solid).
Materials no mesh of the LOD uses are not decoded and are left out of the model. The blend state itself is in
the compiled shader dumps, which are not decoded here. Triangles wind counter-clockwise around their vertex
normals; a handful of parts are stored the other way round (`germ_he_115`'s `radiator2_25`, the Ontos' left
suspension arms), which neither their node matrices nor their materials explain, so a mesh whose triangles
mostly face against its normals is turned over.

## Using wt-unpack from another game (rewrite-bridge)

A game rewrite that wants a War Thunder tank to drive, with its turret, wheels and tracks working from the
game's data, needs no knowledge of Dagor's formats. `WarThunder` gives it three things, all from the player's
installed copy:

- **The model and its sockets.** `load_animated(id)` returns the model plus a `Socket` for every node worth
  attaching to or turning: the turret, gun, barrel and muzzle, the wheels, the track belts, the crew cameras,
  the hatches and the effect emitters.
- **The rig**, `WarThunder::vehicle_rig(id)`: plain structs with the turret limits and speeds, the drive train,
  the track gauge, the wheel radii and the track texture rate, and a small driving model. Only the skeleton and
  meshes are read, so it is quick. `wt_unpack::rig` documents every field with its source key and unit.
- **Collision of level objects**, `WarThunder::load_level_collision(level_id)`, since `Scene` has no field for it.

```rust
use rewrite_bridge::Provider;
use wt_unpack::bridge::WarThunder;
use wt_unpack::rig::{wheel_rotation, TurretAim};
use wt_unpack::tank::Controls;

let wt = WarThunder::new("/Applications/WarThunderLauncher.app/Contents/WarThunder.app/Contents/Resources/game");
let id = "tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman";
let tank = wt.load_animated(id)?;               // Model + sockets; `load(id)` is the same model
let rig = wt.vehicle_rig(id)?;                  // limits, speeds, gearing, wheels, belts
let (turret, gun) = (tank.socket("turret").unwrap().node, tank.socket("gun").unwrap().node);
let mut aim = TurretAim::default();
let mut driving = rig.driving_model().unwrap(); // Sherman: tops out at 38.6 km/h from the gearing

// Every frame
aim.step(rig.turret.as_ref().unwrap(), yaw_input, pitch_input, dt); // at 24 and 4 deg/s, within -180..180 / -10..25
nodes[turret].rotation = aim.head_rotation();
nodes[gun].rotation = aim.gun_rotation();
driving.step(Controls { throttle, steer }, dt);
heading += driving.yaw_rate_rad_s() * dt;               // positive turns left (about +Y)
position += forward(heading) * driving.speed_ms() * dt; // the hull's pose is yours
for (wheel, angle) in rig.gear.as_ref().unwrap().wheels.iter().zip(driving.wheel_angles()) {
    nodes[wheel.node].rotation = wheel_rotation(*angle); // about +X; forward is positive
}
// driving.belt_offsets() are V offsets for the track meshes' UVs (rig.gear.belts[i].mesh)
```

`cargo run --release --features bridge --example drive_tank -- $GAME` runs this loop headless and prints the
state. The axes are the bridge's (front +Z, left +X, up +Y); `wt_unpack::rig` says which way each rotation
goes.

### Sockets

A socket is a node of the model's skeleton (its pivot, in the model's axes; identity offset). Names are the
game's node names, except that the turret group is named by role, as the unit BLK
(`gamedata/units/tankmodels/<unit>.blk` in `aces.vromfs.bin`) names its nodes:

| Socket | Node (Sherman) | Named by |
|---|---|---|
| `turret`, `gun`, `barrel` | `bone_turret`, `bone_gun`, `bone_gun_barrel` | `commonWeapons.Weapon.turret{head, gun, barrel}` |
| `muzzle` | `emtr_gun_flame` | `Weapon.flash` |
| `emitter` | `bone_gun_barrel` | `Weapon.emitter` (where the shell leaves) |
| `weapon_<i>_turret`, `_gun`, `_barrel`, `_muzzle`, `_emitter` | the coaxial (`i` = 1) and anti-aircraft (2) guns | the other `Weapon` blocks |
| `wheel_l_01`, `wheel_r_top_02`, `wheel_l_drive`, `wheel_r_back`, ... | the 22 wheels | the skeleton, verbatim |
| `track_l`, `track_r` | the track belts | the skeleton, verbatim |
| `camera_driver`, `camera_gunner`, `camera_commander` | `bone_camera_*`, the crew's eye points | the skeleton |
| `hatch_01`, ... | the hatches | the skeleton, verbatim |
| `emtr_*` | exhaust, lanterns, fires, track dust | the skeleton, verbatim |

The Sherman has 77 sockets. A node the unit names that the skeleton lacks gets none and is listed in
`VehicleRig::missing_nodes`. Sockets are for ground vehicles; aircraft and ships return none. Sockets carry a
position only: the game's own node axes are not carried (many are mirrored, and which way an emitter fires is
engine code).

### The rig

Everything below is read from the unit BLK unless noted; the units are those of the BLK. A field the unit does
not give is `None`, or the mount is left out; nothing is guessed.

- `turret`, `weapons` (`tank::Turret`): node names (`turret.head`, `gun`, `barrel`, `Weapon.emitter`, `flash`),
  traverse `limits.yaw` and elevation `limits.pitch` in degrees (Sherman: -180..180, -10..25), the elevation
  limits over parts of the traverse (`limitsTable`), and the speeds `speedYaw` and `speedPitch` in degrees per
  second.
- `drive` (`tank::Drive`): `VehiclePhys.Mass.TakeOff` (kg), `engine.horsePowers`, `engine.maxRPM`,
  `mechanics.gearRatios`, `mainGearRatio` and `sideGearRatio`, `driveGearRadius` (m), the brake force, the
  track width and length, the suspension's wheel radii, and the top-level `maxAccel` (m/s²), `maxDecel`,
  `maxAngSpeed` (deg/s) and `maxAngAccel`. `speeds` (`DriveSpeeds`) has the top speeds the gearing allows:
  38.6 km/h forward and 5.0 km/h in reverse for the Sherman.
- `gear` (`GearRig`): the track gauge in metres (2.163 for the Sherman), and for each wheel its node, side, kind
  (road, return roller, sprocket, idler), radius in metres (from the unit's suspension values, or measured from
  the wheel's mesh where the unit has none, which `radius_source` says) and pivot; for each belt its node, mesh
  and `v_per_metre`, the texture scroll rate (1.614 for the Sherman).
- `driving_model()`: the simple kinematic model of `tank::DriveState` plus the wheels and belts it turns
  (see "Wheels and tracks" above for what is left out).

Not available, and why: the crew's positions (the unit's `tank_crew` names damage parts such as `gunner_dm`,
which are geometry of the damage model, not points of the render skeleton), the cockpit's `headPos` (its frame is
not documented), the suspension simulation (`PhysSys`) and the engine's real vehicle physics (not public).

### Level collision

Every placed object has a collision resource in the game's `.grp` packs, `<object>_collision` (class
`ACE50000`, version 3, which `wt_unpack::collision` decodes: 19,799 resources, 57,415 nodes, 11.5 million
triangles, all of them). `Scene` has no collision field at the bridge revision this crate uses, so the shapes come
from `WarThunder::load_level_collision("levels/avg_berlin.bin")`, in the objects' own space and the bridge's axes:
`collision.object(&model.name)` is the collision of a scene model, to be placed by the same `Instance`
transform. Each `ObjectCollision` has a `model` with the triangle meshes, convex hulls and boxes (one mesh per
collision node, its material named after the node's physical material: `concrete`, `wood`...) and the capsules
(tree trunks) a triangle model cannot hold. Berlin: 446 of its 476 objects have collision, with 229,159 triangles and 11
capsules, loaded in about a second. The 30 without are debris pieces the destruction effects scatter (across
all 160 levels, 2,520 of 31,010 placed objects lack a resource).

The ground has no entry: the scene's `terrain` model is the landmesh's collision mesh (and the heightmap's grid
where it is finer). Not carried: point nodes (no geometry), the game's 8 sphere nodes, and the BVH's edge flags.

### Memory

Loading Berlin's scene (`load_scene("levels/avg_berlin.bin")`, 512,641 placements) peaked near 1.1 GB of live
allocations (the scene itself is 830 MB), and a program that dropped the scene still showed about 1.2 GB resident.
The loader keeps nothing alive for that: with the scene dropped, no allocation of the level load is left (the
allocator's statistics say 0 MB live). The memory is **retained by the system allocator**: freed pages are handed
back lazily, and on macOS they stay counted in the process's resident size and footprint until the system needs
them. What the provider does about it:

- it owns every cache it uses (the texture and pack indexes, the model descriptions of the vehicles, the unit
  index; about 26 MB after a level) and nothing is global; dropping the `WarThunder` value, or calling
  `clear_caches()`, frees them, and the pack indexes are shared between vehicle and level loads;
- `clear_caches()` and the end of every level load ask the allocator to release its free pages
  (`wt_unpack::memory::release_freed_memory`: `malloc_zone_pressure_relief` on macOS, `malloc_trim` on glibc).
  This lowers the resident size on a quiet system, but macOS can keep reporting the pages until it reclaims them,
  so the footprint of a process that loaded a level stays near its peak. `drop(scene); wt.clear_caches();` is
  what a consumer should do when it is done with a level;
- the peak is lower. The loader no longer keeps a second copy of every object texture (a texture is decoded once
  and the later materials copy it from the first that holds it), parses only the needed entries of `riDesc.bin`
  instead of holding its whole tree (about 200 MB), shares the paths of the texture index, and trims the meshes'
  spare capacity. The scene is byte for byte the same (checked with a hash of every mesh, image and
  transform).

Berlin, measured with `/usr/bin/time -l` on a Mac that was also busy with other work, so the OS-reported
numbers move between runs:

| Berlin | Before | After |
|---|---|---|
| Peak live allocations | 1,110 MB | 870 MB |
| Scene data at the end | 830 MB | 826 MB |
| Peak memory footprint | 1,590 to 1,610 MB | 1,150 to 1,370 MB |
| Maximum resident set size | 1,440 to 1,760 MB | 800 to 1,370 MB |
| Live after dropping the scene | 0 MB | 26 MB (the indexes), 0 MB after `clear_caches()` |

```sh
/usr/bin/time -l cargo run --release --features bridge --example scene_memory -- $GAME levels/avg_berlin.bin
```

`examples/scene_memory.rs` prints the live, resident and footprint sizes at each step (and a hash of the whole
scene with `WT_DIGEST=1`, to see that a change leaves it alone); `tests/memory.rs` checks the live numbers (macOS).
The peak is still the scene plus the allocator's fragmentation: the object textures are 256 px copies, one per
material, because a bridge `Material` owns its image.

## Levels as scenes

`WarThunder::load_scene("levels/avg_berlin.bin")` (and `wt-unpack level <file> --scene-stats`) turns a
level into a `rewrite_bridge::Scene`. `Provider::list` lists the levels as `AssetKind::Level`; `load`
of a level id says to use `load_scene`. Axes are the vehicles' (`(x, y, z)` becomes `(z, y, x)`).
Everything in the scene is read from the level, the game's resources or the level's settings; what
cannot be is left out and printed by `--scene-stats` (and listed in `wt_unpack::scene::NOT_APPLIED`).

| In the scene | Where it comes from |
|---|---|
| **Ground** (`terrain`, one mesh per landmesh cell) | The landmesh's collision mesh for the whole map and the `HM2` heightmap for the area it covers finer than the landmesh. Colours: the level's land classes (`lmap` detail data, from the level or from `03FB59C4` resources in the `.grp` packs) have colour maps over `size` metres; they are blended by the level's per-cell weights (`LMp2` is the *physics* material map: its header, the physical material names and the map's layout, is read by `wt-unpack level`, its bitmap is not decoded) and baked into one picture per cell. Weights come from the per-cell textures or the packed atlas, decoded as `land_weight_inc.dshl` reads them. |
| **Placed objects** (one model per distinct rendinst) | `RIGz` placements; each pool name resolves to a `RendInst` resource (class `77F8232F`, not a `DynModel`: LODs of rigid meshes with no skeleton), found by scanning the `.grp` headers; materials and textures from `riDesc.bin`. One LOD per model, chosen as the game's `getBestLodForDist` does from the model's own ranges at `SceneOptions::lod_distance_m` (250 m); the billboard impostor LOD is never chosen. Every placement is an `Instance`: no mesh is copied per instance. |
| **Rotations** | Matrix placements carry their own. Position-only placements (all trees) pick one of the pool's preset rotations by the low 3 bits of their scale: a turn about Y of `id / count` of a circle (`rendInstRotationPalette.cpp`), with `count` stored in the level's pool. Position-only pools without a palette have no rotation anywhere in the data and keep the identity. |
| **Water** (`water`) | One quad at the level settings' `water_level` (`levels/<name>.blk` in `aces.vromfs.bin`) over the landmesh, in the level's `water_aces_color_and_density` colour when it has one, else the bridge's default material. |

Not in the scene, and why:

- **The land shaders' detail layers.** Each class has a splat mask (mega classes) or an index map (the
  biome class) and tiled detail textures (`detailRed`.., `details{}`), which `LandClassMaterial` lists
  with their tile sizes, but the shaders that blend them are public only for the indexed class
  (`make_clipmap.dshl`) and not for the mega and detailed ones, so no blend is invented. The ground is the
  colour maps blended by the weights. Normals and roughness of the ground are likewise names only.
- **Water appearance.** The water shader is not public. 154 of the 160 levels have a `water_level`, but
  only 28 carry `water_aces_color_and_density` (the shader's default is not in the data), and the
  other 126 planes keep the bridge's default material.
- **Sun, sky, fog and lights.** The level stores its place and date (`stars{}`); the time of day and the
  weather come from the mission, and the sky from the atmosphere shader, so `Scene::lights` and
  `Scene::environment` are empty.
- **Object shaders.** Layered, perlin and decal materials use their first texture, decals and impostor
  billboards are left out, and wind, interior mapping and normal and reflectance maps are not applied.
- **Tree leaves.** The leaf cards of `rendinst_facing_leaves` materials are left out. A card is four vertices
  around one centre (`HALF4`, in the object's space) plus a corner offset the vertex shader applies in the
  camera's frame, so the data has no corners in the object's space to place; the trees keep their trunks and
  branch meshes. `RiModel::leaf_vertex` decodes the centres and offsets for readers that want them, and
  `cargo run --release --example check_rendinst_bounds` checks every placed-object model in the game.
- **Blocks not decoded:** `SCN`, `FRT`, `Obj`, `Wire`, splines, `wt3d`, `lmct`, and the scatter of
  objects by land class (`obj_plant_generate`), which the game generates at load time.

The conversion is bounded by budgets that are not game data and live in `SceneOptions`: the colour map
memory, the baked ground pictures (2 m per pixel where the heightmap is finer than the landmesh, 16 m
elsewhere, spacings growing together past 384 MB), the terrain vertex count, and the object textures
(256 px on the longest side). Berlin (512,641 placements of 452 models) loads in about 7 s with about 850 MB
of scene data and a peak near 1.3 GB (see "Memory" above); the collision of the objects is separate
(`load_level_collision`, see "Level collision"). `cargo run --release --features bridge --example render_scene -- $GAME levels/avg_berlin.bin
out.png --eye 2150,90,1380 --at 2400,40,1560` draws a scene from a camera with a small software
rasteriser (albedo with a headlight on a flat background, since the game's sun and sky are not in the
data).

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
an empty body is logged as a note, not an error. A connection that cannot be made ends the match at
once, as does `{"valid": false}`. On Windows a closed port is not refused straight away (the OS retries
for about 2 seconds), so connecting has its own 250 ms timeout, separate from the request's 500 ms.

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
  `compressedHeightmap.cpp`, `landRayTracerSoA4.cpp`, `bvhIO.cpp` and `rendInstGen.cpp`; the ground's
  materials `lmeshLandClasses.cpp`, `lmeshWeightAtlas*.cpp`, `landClsExp.cpp` and `binDumpUtil.cpp`
  (the read-only data block of land classes); placed objects `rendInstRes.cpp`,
  `rendInstRotationPalette.cpp` and `genObjUtil.h`. The `HM2`
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
| Vehicle rigs | see the `check-bridge` line below |
| Sound samples | 156,223 in 123 banks decoded: 148,743 Vorbis (91.7 hours) and 7,480 FADPCM |

## Disclaimer

Not affiliated with or endorsed by Gaijin Entertainment. War Thunder is a trademark of Gaijin
Entertainment. Use these tools to read and study game data you own. Do not use them for cheating, and do
not redistribute game assets.

## License

Licensed under either of MIT or Apache-2.0, at your option.
