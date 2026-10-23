# War Thunder as a rewrite-bridge provider

The `bridge` feature: ids, conventions, sockets, the rig and level collision. Part of the [wt-unpack README](../README.md). Wheels, tracks and materials are in [VEHICLES.md](VEHICLES.md), memory in [MEMORY.md](MEMORY.md), levels in [LEVELS.md](LEVELS.md).

## Bridge to other game rewrites

With the `bridge` feature, `wt_unpack::bridge::WarThunder` implements the `Provider` of the
[rewrite-bridge](https://github.com/patodois/rewrite-bridge) crate, so other Rust game rewrites can load War Thunder's
vehicles into their scenes. Ids look like `tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman`. Models
follow glTF's conventions: Dagor's `(x, y, z)` becomes `(z, y, x)`, so vehicles face +Z. The turret,
gun and wheels stay separate nodes, turning about their pivots in the vehicle's axes. Tracks and the
antenna are skins with the game's own bones and weights (a track follows its `root` node and suspension
wheel nodes, see [VEHICLES.md](VEHICLES.md#wheels-and-tracks)), and glass blends.

```toml
[dependencies]
wt-unpack = { git = "https://github.com/patodois/wt-unpack", features = ["bridge"] }
# every provider in one program must name the same rewrite-bridge tag
rewrite-bridge = { git = "https://github.com/patodois/rewrite-bridge", tag = "v0.1.0" }
```

```sh
cargo build --release --features bridge
wt-unpack check-bridge --limit 100                              # load and validate vehicles: see the README
wt-unpack rig tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman   # print a tank's rig
wt-unpack check-conformance --filter germ_he_115 -v             # rewrite-bridge's conformance checker over the vehicles
```

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
  (see "Wheels and tracks" in [VEHICLES.md](VEHICLES.md#wheels-and-tracks) for what is left out).

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
