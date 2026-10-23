//! The rig of a vehicle: everything a game rewrite needs to drive a War Thunder tank with its turret, wheels
//! and tracks working from the game's data, without knowing Dagor's formats. All of it is plain structs.
//!
//! A [`VehicleRig`] gathers, for one 3D model:
//!
//! - the **weapon mounts** ([`Turret`]): the nodes of the turret head, gun, barrel, emitter and muzzle flash,
//!   the traverse and elevation limits and speeds (unit BLK `commonWeapons`);
//! - the **drive train** ([`Drive`]) and its computed speeds ([`DriveSpeeds`]): mass, power, RPM, gearing, sprocket
//!   radius, acceleration limits (unit BLK `VehiclePhys` and the limits at the top level);
//! - the **running gear** ([`GearRig`]): the track gauge, each wheel's node, side, radius and pivot, and each
//!   track belt's node and texture rate (the unit BLK's radii and the model's skeleton and meshes);
//! - the **sockets**: the data-named nodes worth attaching things to (see below), which the bridge provider
//!   returns from `load_animated`.
//!
//! Every field of the data structs says which BLK key it comes from and its unit. Anything the game's data
//! does not give is `None` or left out; nothing is filled in with a guess.
//!
//! # Axes
//!
//! Positions and rotations here follow the bridge (glTF): metres, +Y up, right-handed, the vehicle faces +Z and
//! its left side is +X. Dagor's `(x, y, z)` becomes `(z, y, x)`. Angles in the BLK are degrees; the helpers on
//! [`TurretAim`] and [`DrivingModel`] return the rotations to put on the nodes.
//!
//! # Moving the parts
//!
//! - **Turret head** (`socket "turret"`): rotate about +Y by the traverse angle, positive towards the left.
//! - **Gun** (`"gun"`): rotate about -X by the elevation (positive raises the muzzle, which is on +Z).
//! - **Wheels**: rotate about +X; a wheel rolling forward turns by `distance / radius`, positive about +X.
//! - **Track belts**: add [`DrivingModel::belt_offsets`] to the V coordinate of the track mesh's UVs (a texture
//!   scroll; the tracks have no links to move).
//!
//! # Sockets
//!
//! A socket is a node of the model's skeleton (the node's pivot, with the model's axes). Names are the game's
//! node names, except for the turret group and the crew cameras, where the game's name says what the node is
//! for in the unit BLK:
//!
//! | socket | node | where the node is named |
//! |---|---|---|
//! | `turret` | the main gun's traverse node (Sherman: `bone_turret`) | unit `commonWeapons.Weapon.turret.head` |
//! | `gun` | the elevating node (`bone_gun`) | `Weapon.turret.gun` |
//! | `barrel` | the recoiling barrel (`bone_gun_barrel`), when named | `Weapon.turret.barrel` |
//! | `muzzle` | the muzzle flash emitter (`emtr_gun_flame`), when named | `Weapon.flash` |
//! | `emitter` | the node the shell leaves from (`bone_gun_barrel`), when named | `Weapon.emitter` |
//! | `weapon_<i>_turret` .. `_gun`, `_barrel`, `_muzzle`, `_emitter` | the same for the other mounts, `i` from 1 (the coaxial and anti-aircraft guns) | the other `Weapon` blocks |
//! | `wheel_l_01`, `wheel_r_top_02`, `wheel_l_drive`, `wheel_r_back`, .. | each wheel that has a mesh | model skeleton, verbatim |
//! | `track_l`, `track_r` | the track belts | model skeleton, verbatim |
//! | `camera_driver`, `camera_gunner`, `camera_commander` | the crew's eye points (`bone_camera_*`) | model skeleton |
//! | `hatch_01`.. | the hatches | model skeleton, verbatim |
//! | `emtr_*` | the effect emitters (exhaust, lanterns, fires, track dust), verbatim | model skeleton |
//!
//! A node the unit names but the skeleton lacks (the Sherman's `bone_mg_aa_v_01`) gets no socket and is listed in
//! [`VehicleRig::missing_nodes`]. Sockets carry a position only: the game's own node axes are not carried
//! (many are mirrored, and which axis an emitter fires along is engine code).
//!
//! # Left out
//!
//! The crew's positions (the unit's `tank_crew` names damage parts such as `gunner_dm`, which are geometry of
//! the damage model, not points of the render skeleton), the cockpit's `headPos` (its frame is not documented),
//! the suspension simulation (`PhysSys`) and the engine's real vehicle physics are not public or not
//! unambiguous, so they are not here. Ground vehicles only: aircraft and ships have no unit file in
//! `gamedata/units/tankmodels`.

use std::collections::{HashMap, HashSet};
use std::f32::consts::TAU;

use crate::gear::{self, GearState, RadiusSource, RunningGear, Side, WheelKind};
use crate::tank::{Controls, Drive, DriveState, Tank, Turret};
use crate::vehicle::Vehicle;

/// From the vehicle's space of [`crate::vehicle`] (front +X, right +Z) to the bridge's (front +Z, left +X)
pub fn to_bridge_axes(p: [f32; 3]) -> [f32; 3] {
    [-p[2], p[1], p[0]]
}

/// Speeds the gearing allows, computed from [`Drive`]: the engine at its maximum RPM in a gear, through
/// `main_gear_ratio * side_gear_ratio` and the sprocket radius
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriveSpeeds {
    /// Top speed in the highest forward gear, km/h
    pub top_speed_kmh: f32,
    /// Top speed in the fastest reverse gear, km/h
    pub reverse_speed_kmh: f32,
    /// Top speed in first gear, km/h
    pub first_gear_speed_kmh: f32,
    /// Engine power, watts (`horse_powers` times 745.7)
    pub power_watts: f32,
}

impl DriveSpeeds {
    pub fn of(d: &Drive) -> Self {
        Self { top_speed_kmh: d.top_speed() * 3.6, reverse_speed_kmh: d.reverse_speed() * 3.6, first_gear_speed_kmh: d.first_gear_speed() * 3.6, power_watts: d.power_watts() }
    }
}

/// A wheel that turns as the vehicle drives
#[derive(Debug, Clone, PartialEq)]
pub struct WheelRig {
    /// Index in the bridge model's `nodes` (the same as in the model `load` returns for the id)
    pub node: usize,
    /// The skeleton node's name, e.g. `wheel_l_01`, which is also the socket's name
    pub name: String,
    pub side: Side,
    pub kind: WheelKind,
    /// m. The unit BLK's `VehiclePhys.suspension` / `mechanics` radius for this kind of wheel, or the mesh's
    /// half height where the unit has none (see `radius_source`)
    pub radius_m: f32,
    pub radius_source: RadiusSource,
    /// The point the wheel turns about, in the model's space (bridge axes). The axis is +X
    pub pivot: [f32; 3],
}

/// A track belt: a skinned mesh whose texture runs along the track
#[derive(Debug, Clone, PartialEq)]
pub struct BeltRig {
    /// Index in the bridge model's `nodes`, the belt's node (`track_l`, `track_r`)
    pub node: usize,
    pub name: String,
    /// Index in the bridge model's `meshes`
    pub mesh: usize,
    pub side: Side,
    /// Change of the mesh's V coordinate per metre the belt moves relative to the hull, measured on the
    /// mesh's bottom run (the rate at which the tread stays still on the ground)
    pub v_per_metre: f32,
}

/// Wheels and track belts
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GearRig {
    /// Distance between the two tracks' centres, m: between the mean of the right and of the left wheels' pivots
    pub gauge_m: Option<f32>,
    pub wheels: Vec<WheelRig>,
    pub belts: Vec<BeltRig>,
}

/// A named attachment point: a node of the skeleton (see the module docs for the naming)
#[derive(Debug, Clone, PartialEq)]
pub struct RigSocket {
    pub name: String,
    /// Index in the bridge model's `nodes`
    pub node: usize,
    /// The skeleton node's name
    pub node_name: String,
}

/// Everything about one vehicle's moving parts, from its unit file and its model
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VehicleRig {
    /// The 3D model, e.g. `m4a1_76w_sherman`
    pub model: String,
    /// The unit BLK that uses the model, e.g. `us_m4a1_76w_sherman`; `None` when no unit does (aircraft, ships)
    pub unit: Option<String>,
    /// The main gun: the first weapon with a turret
    pub turret: Option<Turret>,
    /// All weapon mounts with a turret, the main gun first
    pub weapons: Vec<Turret>,
    pub drive: Option<Drive>,
    pub speeds: Option<DriveSpeeds>,
    pub gear: Option<GearRig>,
    pub sockets: Vec<RigSocket>,
    /// Nodes the unit names that the model's skeleton does not have
    pub missing_nodes: Vec<String>,
}

impl VehicleRig {
    /// Builds the rig of an assembled vehicle with its unit data (`None` for models no unit uses). Indices of
    /// nodes and meshes are those of `v`, which the bridge model keeps.
    pub fn new(v: &Vehicle, tank: Option<&Tank>) -> Self {
        let drive = tank.and_then(|t| t.drive.clone());
        let running = RunningGear::new(v, drive.as_ref());
        let gear = (!running.wheels.is_empty() || !running.belts.is_empty()).then(|| GearRig {
            gauge_m: running.gauge,
            wheels: running
                .wheels
                .iter()
                .map(|w| WheelRig {
                    node: w.node,
                    name: v.nodes[w.node].name.clone(),
                    side: w.side,
                    kind: w.kind,
                    radius_m: w.radius,
                    radius_source: w.radius_source,
                    pivot: to_bridge_axes(v.nodes[w.node].pivot),
                })
                .collect(),
            belts: running
                .belts
                .iter()
                .map(|b| {
                    let node = v.meshes[b.mesh].node;
                    BeltRig { node, name: v.nodes[node].name.clone(), mesh: b.mesh, side: b.side, v_per_metre: b.v_per_metre }
                })
                .collect(),
        });
        let weapons = tank.map(|t| t.weapons.clone()).unwrap_or_default();
        let mut rig = VehicleRig {
            model: v.name.clone(),
            unit: tank.map(|t| t.unit.clone()),
            turret: weapons.first().cloned(),
            speeds: drive.as_ref().map(DriveSpeeds::of),
            drive,
            gear,
            weapons,
            ..VehicleRig::default()
        };
        rig.find_sockets(v);
        rig
    }

    fn find_sockets(&mut self, v: &Vehicle) {
        let mut by_name: HashMap<&str, usize> = HashMap::new();
        for (i, n) in v.nodes.iter().enumerate() {
            by_name.entry(n.name.as_str()).or_insert(i);
        }
        let mut used: HashSet<String> = HashSet::new();
        let mut sockets = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        let mut add = |socket: String, node_name: &str, named_by_unit: bool| {
            match by_name.get(node_name) {
                Some(&node) => {
                    if used.insert(socket.clone()) {
                        sockets.push(RigSocket { name: socket, node, node_name: node_name.to_string() });
                    }
                }
                None if named_by_unit && !missing.iter().any(|m| m == node_name) => missing.push(node_name.to_string()),
                None => {}
            }
        };

        // The weapon mounts the unit names
        for (i, w) in self.weapons.iter().enumerate() {
            let prefix = if i == 0 { String::new() } else { format!("weapon_{i}_") };
            add(format!("{prefix}turret"), &w.head, true);
            add(format!("{prefix}gun"), &w.gun, true);
            for (suffix, node) in [("barrel", &w.barrel), ("muzzle", &w.flash), ("emitter", &w.emitter)] {
                if let Some(n) = node {
                    add(format!("{prefix}{suffix}"), n, true);
                }
            }
        }
        // Wheels and belts, by the skeleton's own names
        if let Some(g) = &self.gear {
            for w in &g.wheels {
                add(w.name.clone(), &w.name, false);
            }
            for b in &g.belts {
                add(b.name.clone(), &b.name, false);
            }
        }
        // Crew cameras, hatches and effect emitters that the skeleton carries
        for n in &v.nodes {
            if let Some(role) = n.name.strip_prefix("bone_camera_") {
                add(format!("camera_{role}"), &n.name, false);
            } else if n.name.strip_prefix("hatch_").is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit())) || n.name.starts_with("emtr_") {
                add(n.name.clone(), &n.name, false);
            }
        }
        self.sockets = sockets;
        self.missing_nodes = missing;
    }

    /// The socket with this name
    pub fn socket(&self, name: &str) -> Option<&RigSocket> {
        self.sockets.iter().find(|s| s.name == name)
    }

    /// A driving model that moves the hull, wheels and belts together, from the unit's drive train and the
    /// running gear. `None` when the unit has no drive train or the model no track gauge.
    pub fn driving_model(&self) -> Option<DrivingModel> {
        let drive = self.drive.clone()?;
        let gear = self.gear.as_ref()?;
        let gauge = gear.gauge_m?;
        Some(DrivingModel {
            drive,
            gauge,
            state: DriveState::default(),
            wheels: gear.wheels.iter().map(|w| (w.side, w.radius_m)).collect(),
            belts: gear.belts.iter().map(|b| (b.side, b.v_per_metre)).collect(),
            gear_state: GearState { wheel_angles: vec![0.0; gear.wheels.len()], belt_offsets: vec![0.0; gear.belts.len()] },
            wheel_x: vec![0.0; gear.wheels.len()],
        })
    }
}

/// The simple driving model of [`crate::tank::DriveState`] together with the wheels and belts it turns.
/// Kinematic, not the game's physics (which is not public): engine power and the unit's acceleration limits
/// set the acceleration, the gearing the top speeds, and the hull turns by running one track slower.
///
/// Each step gives the hull's speed and turn rate for the consumer to integrate into a pose, and the angles
/// of the wheels and offsets of the belts for the nodes and meshes of the rig.
#[derive(Debug, Clone)]
pub struct DrivingModel {
    drive: Drive,
    gauge: f32,
    state: DriveState,
    wheels: Vec<(Side, f32)>,
    belts: Vec<(Side, f32)>,
    gear_state: GearState,
    wheel_x: Vec<f32>,
}

impl DrivingModel {
    /// Advances by `dt` seconds. Throttle and steering are -1..1; positive steering turns left.
    pub fn step(&mut self, controls: Controls, dt: f32) {
        self.state.step(&self.drive, self.gauge, controls, dt);
        let (left, right) = self.state.track_speeds(self.gauge);
        self.advance_gear(left * dt, right * dt);
    }

    fn advance_gear(&mut self, left: f32, right: f32) {
        for (angle, (side, radius)) in self.gear_state.wheel_angles.iter_mut().zip(&self.wheels) {
            *angle = gear::wheel_angle_after(*angle, side.pick(left, right), *radius);
        }
        for (offset, (side, rate)) in self.gear_state.belt_offsets.iter_mut().zip(&self.belts) {
            *offset = gear::belt_offset_after(*offset, *rate, side.pick(left, right));
        }
        // The game-space angle turns about the vehicle's right side (+Z there), clockwise when positive
        // rolls backwards; about the bridge's +X a forward roll is positive
        for (x, a) in self.wheel_x.iter_mut().zip(&self.gear_state.wheel_angles) {
            *x = (TAU - a).rem_euclid(TAU);
        }
    }

    /// Hull speed, m/s; positive forward (+Z)
    pub fn speed_ms(&self) -> f32 {
        self.state.speed
    }

    /// Hull turn rate, rad/s; positive turns left, which is a rotation about +Y
    pub fn yaw_rate_rad_s(&self) -> f32 {
        self.state.yaw_rate
    }

    /// The speeds of the left and right tracks, m/s
    pub fn track_speeds_ms(&self) -> (f32, f32) {
        self.state.track_speeds(self.gauge)
    }

    /// Angles of the wheels in radians in 0..TAU, parallel to [`GearRig::wheels`]: the rotation about the
    /// wheel node's +X axis (a wheel rolling forward turns by distance / radius, positive)
    pub fn wheel_angles(&self) -> &[f32] {
        &self.wheel_x
    }

    /// V offsets in 0..1 to add to the track meshes' texture coordinates, parallel to [`GearRig::belts`]
    pub fn belt_offsets(&self) -> &[f32] {
        &self.gear_state.belt_offsets
    }

    /// Back to standing still with the wheels and belts at rest
    pub fn reset(&mut self) {
        self.state = DriveState::default();
        self.gear_state.wheel_angles.iter_mut().for_each(|a| *a = 0.0);
        self.gear_state.belt_offsets.iter_mut().for_each(|a| *a = 0.0);
        self.wheel_x.iter_mut().for_each(|a| *a = 0.0);
    }
}

/// Where a weapon mount is pointing, moved at the BLK's speeds within its limits
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TurretAim {
    /// Traverse, degrees; positive towards the left
    pub yaw_deg: f32,
    /// Elevation, degrees; positive raises the muzzle
    pub pitch_deg: f32,
}

impl TurretAim {
    /// Moves by `yaw_input` and `pitch_input` (each -1..1: positive turns left and raises the muzzle) for `dt`
    /// seconds at the mount's `speed_yaw` and `speed_pitch`, then applies its limits
    pub fn step(&mut self, turret: &Turret, yaw_input: f32, pitch_input: f32, dt: f32) {
        self.yaw_deg += yaw_input.clamp(-1.0, 1.0) * turret.speed_yaw * dt;
        self.pitch_deg += pitch_input.clamp(-1.0, 1.0) * turret.speed_pitch * dt;
        self.yaw_deg = turret.clamp_yaw(self.yaw_deg);
        self.pitch_deg = turret.clamp_pitch(self.yaw_deg, self.pitch_deg);
    }

    /// Rotation of the head node, a quaternion (x, y, z, w): about +Y by the traverse
    pub fn head_rotation(&self) -> [f32; 4] {
        let h = self.yaw_deg.to_radians() / 2.0;
        [0.0, h.sin(), 0.0, h.cos()]
    }

    /// Rotation of the gun node: about -X by the elevation, which raises a muzzle on +Z
    pub fn gun_rotation(&self) -> [f32; 4] {
        let h = -self.pitch_deg.to_radians() / 2.0;
        [h.sin(), 0.0, 0.0, h.cos()]
    }
}

/// Rotation of a wheel node for an angle from [`DrivingModel::wheel_angles`]: a quaternion about +X
pub fn wheel_rotation(angle: f32) -> [f32; 4] {
    let h = angle / 2.0;
    [h.sin(), 0.0, 0.0, h.cos()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vehicle::{Mesh, Node};

    fn turret(head: &str, gun: &str) -> Turret {
        Turret {
            head: head.into(),
            gun: gun.into(),
            barrel: Some("bone_gun_barrel".into()),
            emitter: Some("bone_gun_barrel".into()),
            flash: Some("emtr_gun_flame".into()),
            yaw: [-180.0, 180.0],
            pitch: [-10.0, 25.0],
            pitch_by_yaw: vec![],
            speed_yaw: 24.0,
            speed_pitch: 4.0,
        }
    }

    fn drive() -> Drive {
        Drive {
            mass: 32673.0,
            horse_powers: 400.0,
            max_rpm: 2400.0,
            drive_gear_radius: 0.305,
            main_gear_ratio: 3.615,
            side_gear_ratio: 2.7,
            gear_ratios: vec![-5.64, 0.0, 7.55, 3.1, 1.795, 1.111, 0.733],
            max_brake_force: Some(130000.0),
            track_width: None,
            track_length: None,
            track_animation_multiplier: None,
            road_wheel_radius: Some(0.25),
            top_wheel_radius: None,
            end_wheel_radius: None,
            max_accel: Some(4.0),
            max_decel: Some(8.0),
            max_ang_speed: Some(30.0),
            max_ang_accel: Some(40.0),
            max_fwd_speed: None,
            max_rev_speed: None,
        }
    }

    fn quad(node: usize, skinned: bool, y: [f32; 2], v_per_metre: f32) -> Mesh {
        let mut m = Mesh { node, skinned, ..Mesh::default() };
        for i in 0..4 {
            let x = i as f32 * 0.5;
            m.positions.extend([[x, y[0], 0.0], [x, y[0], 0.4], [x, y[1], 0.0]]);
            m.uvs.extend([[0.0, x * v_per_metre], [1.0, x * v_per_metre], [0.0, 0.0]]);
            m.normals.extend([[0.0, 1.0, 0.0]; 3]);
        }
        for i in 0..3u32 {
            let (a, b) = (i * 3, i * 3 + 3);
            m.indices.extend([a, b, a + 1, a + 1, b, b + 1]);
        }
        m
    }

    /// Dagor-space vehicle: front +X, right +Z; the left wheel at z = -1 (so +X in the bridge's axes)
    fn vehicle() -> Vehicle {
        let n = |name: &str, parent, pivot| Node { name: name.into(), parent, pivot };
        let nodes = vec![
            n("", None, [0.0; 3]),
            n("bone_turret", Some(0), [0.0, 1.8, 0.0]),
            n("bone_gun", Some(1), [0.9, 2.2, 0.0]),
            n("emtr_gun_flame", Some(2), [3.9, 2.2, 0.0]),
            n("wheel_l_01", Some(0), [1.0, 0.3, -1.0]),
            n("wheel_r_01", Some(0), [1.0, 0.3, 1.0]),
            n("track_l", Some(0), [0.0; 3]),
            n("track_r", Some(0), [0.0; 3]),
            n("bone_camera_driver", Some(0), [1.3, 1.9, 0.5]),
            n("hatch_01", Some(1), [0.0, 2.7, 0.0]),
            n("hatch_x", Some(1), [0.0, 2.7, 0.0]),
            n("bone_gun_barrel", Some(2), [2.2, 2.2, 0.0]),
            n("emtr_exhaust_01", Some(0), [-2.6, 1.0, 0.1]),
        ];
        Vehicle {
            name: "tank".into(),
            meshes: vec![quad(4, false, [0.0, 0.5], 0.0), quad(5, false, [0.0, 0.5], 0.0), quad(6, true, [0.0, 0.8], 1.6), quad(7, true, [0.0, 0.8], 1.6)],
            materials: Vec::new(),
            nodes,
        }
    }

    fn tank() -> Tank {
        let mut coax = turret("bone_mg_gun_twin", "bone_mg_gun_twin");
        coax.flash = Some("emtr_mg_flame_01".into());
        coax.barrel = None;
        coax.emitter = Some("bone_mg_aa_v_01".into());
        Tank { unit: "us_x".into(), turret: Some(turret("bone_turret", "bone_gun")), weapons: vec![turret("bone_turret", "bone_gun"), coax], drive: Some(drive()) }
    }

    #[test]
    fn sockets_are_the_data_named_nodes() {
        let rig = VehicleRig::new(&vehicle(), Some(&tank()));
        let names: Vec<&str> = rig.sockets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["turret", "gun", "barrel", "muzzle", "emitter", "wheel_l_01", "wheel_r_01", "track_l", "track_r", "emtr_gun_flame", "camera_driver", "hatch_01", "emtr_exhaust_01"]
        );
        let node = |s: &str| rig.socket(s).unwrap().node;
        assert_eq!((node("turret"), node("gun"), node("muzzle"), node("barrel"), node("emitter")), (1, 2, 3, 11, 11));
        assert_eq!(rig.socket("camera_driver").unwrap().node_name, "bone_camera_driver");
        // The coaxial gun's nodes are not in this skeleton; the unit names them, so they are listed
        assert!(rig.socket("weapon_1_turret").is_none());
        assert_eq!(rig.missing_nodes, ["bone_mg_gun_twin", "emtr_mg_flame_01", "bone_mg_aa_v_01"]);
        // A hatch-like name without a number is not a hatch
        assert!(rig.socket("hatch_x").is_none());
    }

    #[test]
    fn a_model_without_a_unit_has_only_its_named_nodes() {
        let rig = VehicleRig::new(&vehicle(), None);
        assert!(rig.unit.is_none() && rig.turret.is_none() && rig.drive.is_none() && rig.speeds.is_none());
        assert!(rig.socket("turret").is_none() && rig.socket("wheel_l_01").is_some());
        assert!(rig.missing_nodes.is_empty());
        assert!(rig.driving_model().is_none());
    }

    #[test]
    fn the_running_gear_is_in_the_bridge_axes() {
        let rig = VehicleRig::new(&vehicle(), Some(&tank()));
        let gear = rig.gear.as_ref().unwrap();
        // Dagor z = -1 is the left side: +X in the bridge's axes; the front (+X there) is +Z here
        let left = gear.wheels.iter().find(|w| w.name == "wheel_l_01").unwrap();
        assert_eq!(left.pivot, [1.0, 0.3, 1.0]);
        assert_eq!((left.radius_m, left.radius_source), (0.25, RadiusSource::Unit));
        assert_eq!(gear.gauge_m, Some(2.0));
        assert_eq!(gear.belts.iter().map(|b| (b.name.as_str(), b.side)).collect::<Vec<_>>(), [("track_l", Side::Left), ("track_r", Side::Right)]);
        let speeds = rig.speeds.unwrap();
        assert!((speeds.top_speed_kmh - 38.6).abs() < 0.1 && (speeds.reverse_speed_kmh - 5.02).abs() < 0.05);
    }

    #[test]
    fn the_driving_model_rolls_wheels_forward_about_plus_x() {
        let rig = VehicleRig::new(&vehicle(), Some(&tank()));
        let mut m = rig.driving_model().unwrap();
        let dt = 1.0 / 60.0;
        for _ in 0..120 {
            m.step(Controls { throttle: 1.0, steer: 0.0 }, dt);
        }
        assert!(m.speed_ms() > 0.0);
        // Both tracks have moved the same distance; a road wheel (0.25 m) has turned distance / 0.25 forward
        let x = m.wheel_angles()[0];
        assert!(x > 0.0 && x < TAU && (m.wheel_angles()[0] - m.wheel_angles()[1]).abs() < 1e-4);
        assert!(m.belt_offsets().iter().all(|o| (0.0..1.0).contains(o)));
        let travelled: f32 = (m.wheel_angles()[0] * 0.25).rem_euclid(TAU * 0.25);
        assert!(travelled > 0.0);
        // Steering left makes the left track the slower one
        m.reset();
        for _ in 0..60 {
            m.step(Controls { throttle: 1.0, steer: 1.0 }, dt);
        }
        let (l, r) = m.track_speeds_ms();
        assert!(l < r && m.yaw_rate_rad_s() > 0.0);
        m.reset();
        assert_eq!((m.speed_ms(), m.wheel_angles()[0], m.belt_offsets()[0]), (0.0, 0.0, 0.0));
    }

    #[test]
    fn turret_aim_follows_the_limits_and_the_rotation_axes() {
        let t = turret("a", "b");
        let mut aim = TurretAim::default();
        aim.step(&t, 1.0, 1.0, 1.0);
        assert_eq!((aim.yaw_deg, aim.pitch_deg), (24.0, 4.0));
        aim.step(&t, 0.0, 1.0, 100.0);
        assert_eq!(aim.pitch_deg, 25.0);
        aim.step(&t, 0.0, -1.0, 100.0);
        assert_eq!(aim.pitch_deg, -10.0);
        // Positive yaw turns the front (+Z) towards the left (+X): rotation about +Y
        let apply = |q: [f32; 4], p: [f32; 3]| {
            let [x, y, z, w] = q;
            let u = [x, y, z];
            let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
            let uv = cross(u, p);
            let uuv = cross(u, uv);
            [0, 1, 2].map(|k| p[k] + 2.0 * (w * uv[k] + uuv[k]))
        };
        let turned = apply(TurretAim { yaw_deg: 90.0, pitch_deg: 0.0 }.head_rotation(), [0.0, 0.0, 1.0]);
        assert!((turned[0] - 1.0).abs() < 1e-5 && turned[2].abs() < 1e-5);
        // Positive pitch raises the muzzle; a wheel rolling forward moves its top towards +Z
        let raised = apply(TurretAim { yaw_deg: 0.0, pitch_deg: 30.0 }.gun_rotation(), [0.0, 0.0, 1.0]);
        assert!(raised[1] > 0.49 && raised[2] > 0.86);
        let top = apply(wheel_rotation(0.3), [0.0, 1.0, 0.0]);
        assert!(top[2] > 0.0);
    }
}
