//! The running gear of a tracked vehicle: which parts of the model move when the vehicle drives, and by
//! how much. Pure data and functions, so any app can animate a [`Vehicle`] (the viewer and the bridge
//! provider both start from this).
//!
//! What the game's data says, and what this module takes from it:
//!
//! - **Wheels.** The visible wheels are skeleton nodes named `wheel_<l|r>_<NN>` (road wheels),
//!   `wheel_<l|r>_top_<NN>` (return rollers), `wheel_<l|r>_drive` (sprocket) and `wheel_<l|r>_back` /
//!   `_front` (idlers). A wheel turns about the lateral axis through its node's pivot. Its radius comes
//!   from the unit BLK: `VehiclePhys.suspension.defaultGearRadius`, `topGearRadius`,
//!   `frontBackGearRadius` and `VehiclePhys.mechanics.driveGearRadius`. Without a BLK value the radius
//!   is measured from the wheel's mesh. A wheel that rolls without slipping on its track turns by
//!   `travel / radius`.
//! - **Track skin.** Tracks are skinned meshes (`track_l`, `track_r`) whose vertices are bound, by the
//!   bone table `wt_blk` reads, to the `root` bone or to the suspension wheel bones
//!   (`bone_wheel_<l|r>_<NN>`): the stretch of track around a road wheel follows that wheel's bone, the
//!   rest of the loop stays with the hull. There are no bones along the links. Moving a bone moves the
//!   track vertices bound to it ([`vehicle::Mesh::skinned_positions`]), which is how the game bends the
//!   track over terrain. Each bone's pose comes from the game's suspension simulation (`PhysSys` in the
//!   unit BLK, solved by the engine's physics), which is not reproduced here: on flat ground the bones
//!   sit at their bind pose.
//! - **Track texture.** The track's links are painted on the skinned mesh, with U across the track and V
//!   along it, and the mesh stays put: the tread moves because the texture scrolls along V. The
//!   mesh's texture mapping gives the exact no-slip rate (the change in V per metre of belt, see
//!   [`vehicle::track_v_per_metre`]), which in the game's meshes is a quarter of a V unit per link
//!   (`trackLen` in the unit BLK). A belt moves relative to the hull at the track's ground speed.
//!
//! Left out because the data to do it 1:1 is not public: the unit BLK's `tracks.animationMultiplier`
//! (see [`crate::tank::Drive::track_animation_multiplier`]) and the `texcoord_anim=1` / `-1` parameter
//! of the left and right track materials in dynModelDesc.bin are read by shader and engine code that
//! is not published, so their scaling is unknown. They are not applied; the rate above is the one
//! that keeps the tread still on the ground.

use std::f32::consts::TAU;

use crate::tank::Drive;
use crate::vehicle::{self, Vehicle};

/// Which side of the vehicle a part is on
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    /// For a node named like `wheel_l_03` or `track_r` (with `prefix` `"wheel_"` or `"track_"`)
    pub fn of_node(name: &str, prefix: &str) -> Option<Side> {
        match name.strip_prefix(prefix)?.split('_').next()? {
            "l" => Some(Side::Left),
            "r" => Some(Side::Right),
            _ => None,
        }
    }

    /// The distance travelled by this side's track out of a pair
    pub fn pick(self, left: f32, right: f32) -> f32 {
        match self {
            Side::Left => left,
            Side::Right => right,
        }
    }
}

/// What a wheel is, which decides the radius the unit BLK gives it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelKind {
    Road,
    /// A return roller carrying the upper run
    Top,
    /// The drive sprocket
    Drive,
    /// An idler at the front or the back
    End,
}

impl WheelKind {
    pub fn of_node(name: &str) -> WheelKind {
        if name.contains("_top") {
            WheelKind::Top
        } else if name.contains("_drive") {
            WheelKind::Drive
        } else if name.contains("_back") || name.contains("_front") {
            WheelKind::End
        } else {
            WheelKind::Road
        }
    }

    /// The radius the unit BLK gives wheels of this kind, m
    pub fn blk_radius(self, drive: &Drive) -> Option<f32> {
        match self {
            WheelKind::Road => drive.road_wheel_radius,
            WheelKind::Top => drive.top_wheel_radius,
            WheelKind::Drive => Some(drive.drive_gear_radius),
            WheelKind::End => drive.end_wheel_radius,
        }
    }
}

/// Where a wheel's radius came from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadiusSource {
    /// The unit BLK
    Unit,
    /// Measured from the wheel's mesh (half its height), for lack of a value in the BLK
    Mesh,
}

/// A wheel that turns as the vehicle drives
#[derive(Debug, Clone, PartialEq)]
pub struct Wheel {
    /// Skeleton node (an index into `Vehicle::nodes`); it turns about the axis through its pivot, parallel
    /// to the vehicle's Z
    pub node: usize,
    pub side: Side,
    pub kind: WheelKind,
    /// m
    pub radius: f32,
    pub radius_source: RadiusSource,
}

/// A track belt: a skinned mesh whose texture runs along the track
#[derive(Debug, Clone, PartialEq)]
pub struct Belt {
    /// Index into `Vehicle::meshes`
    pub mesh: usize,
    pub side: Side,
    /// Change in V per metre the belt moves relative to the hull, forward positive: add
    /// `v_per_metre * distance` to the texture's V offset for the tread to stay still on the ground
    pub v_per_metre: f32,
}

/// Wheels and belts of a vehicle
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunningGear {
    pub wheels: Vec<Wheel>,
    pub belts: Vec<Belt>,
    /// Distance between the two tracks' centres, m: between the mean of the right and left wheels' pivots
    pub gauge: Option<f32>,
}

/// The wheels' angles and the belts' texture offsets at a moment
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GearState {
    /// Radians, parallel to [`RunningGear::wheels`]. Positive turns counter-clockwise seen from +Z (the
    /// vehicle's right side), so a wheel rolling forward has a negative rate
    pub wheel_angles: Vec<f32>,
    /// Texture V offsets in 0..1, parallel to [`RunningGear::belts`]
    pub belt_offsets: Vec<f32>,
}

/// A wheel's angle after the track beneath it moved `travel` metres forward, radians in 0..TAU
pub fn wheel_angle_after(angle: f32, travel: f32, radius: f32) -> f32 {
    // Rolling forward (+X) turns a wheel clockwise seen from the right (+Z), i.e. negatively about Z
    (angle - travel / radius).rem_euclid(TAU)
}

/// A belt's texture offset after the belt moved `travel` metres forward on the hull. The bottom run stays
/// put on the ground, so relative to the hull the tread runs backwards; the offset follows the mesh's
/// own texture direction.
pub fn belt_offset_after(offset: f32, v_per_metre: f32, travel: f32) -> f32 {
    (offset + v_per_metre * travel).rem_euclid(1.0)
}

impl RunningGear {
    /// Finds the wheels and belts of a vehicle. Wheels take their radius from the unit's drive data when
    /// there is one.
    pub fn new(v: &Vehicle, drive: Option<&Drive>) -> Self {
        let mut wheels = Vec::new();
        let (mut left_z, mut right_z) = (Vec::new(), Vec::new());
        for (node, n) in v.nodes.iter().enumerate() {
            let Some(side) = Side::of_node(&n.name, "wheel_") else { continue };
            // Height of the wheel's meshes; nodes without any are not wheels you can see
            let (mut bottom, mut top) = (f32::MAX, f32::MIN);
            for m in v.meshes.iter().filter(|m| m.node == node) {
                for p in &m.positions {
                    bottom = bottom.min(p[1]);
                    top = top.max(p[1]);
                }
            }
            if bottom > top {
                continue;
            }
            let kind = WheelKind::of_node(&n.name);
            let (radius, radius_source) = match drive.and_then(|d| kind.blk_radius(d)) {
                Some(r) => (r, RadiusSource::Unit),
                None => ((top - bottom) / 2.0, RadiusSource::Mesh),
            };
            wheels.push(Wheel { node, side, kind, radius: radius.max(0.05), radius_source });
            match side {
                Side::Left => left_z.push(n.pivot[2]),
                Side::Right => right_z.push(n.pivot[2]),
            }
        }
        let mean = |z: &[f32]| z.iter().sum::<f32>() / z.len() as f32;
        let gauge = (!left_z.is_empty() && !right_z.is_empty()).then(|| mean(&right_z) - mean(&left_z));

        let belts = v
            .meshes
            .iter()
            .enumerate()
            .filter(|(_, m)| m.skinned)
            .filter_map(|(i, m)| {
                let side = Side::of_node(&v.nodes[m.node].name, "track_")?;
                Some(Belt { mesh: i, side, v_per_metre: vehicle::track_v_per_metre(m)? })
            })
            .collect();
        Self { wheels, belts, gauge }
    }

    /// Everything at rest
    pub fn rest(&self) -> GearState {
        GearState { wheel_angles: vec![0.0; self.wheels.len()], belt_offsets: vec![0.0; self.belts.len()] }
    }

    /// Moves the state on by how far the left and right tracks travelled, m (forward positive)
    pub fn advance(&self, state: &mut GearState, left: f32, right: f32) {
        for (angle, w) in state.wheel_angles.iter_mut().zip(&self.wheels) {
            *angle = wheel_angle_after(*angle, w.side.pick(left, right), w.radius);
        }
        for (offset, b) in state.belt_offsets.iter_mut().zip(&self.belts) {
            *offset = belt_offset_after(*offset, b.v_per_metre, b.side.pick(left, right));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vehicle::{Mesh, Node};

    fn node(name: &str, parent: Option<usize>, pivot: [f32; 3]) -> Node {
        Node { name: name.into(), parent, pivot }
    }

    /// A strip of mesh (a quad per metre) with the given heights, for a wheel or a track
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

    fn vehicle() -> Vehicle {
        let nodes = vec![
            node("", None, [0.0; 3]),
            node("wheel_l_01", Some(0), [1.0, 0.3, -1.0]),
            node("wheel_r_01", Some(0), [1.0, 0.3, 1.0]),
            node("wheel_l_drive", Some(0), [2.0, 0.7, -1.0]),
            node("wheel_r_top_01", Some(0), [1.0, 0.9, 1.0]),
            node("wheel_l_hidden", Some(0), [0.0; 3]),
            node("track_l", Some(0), [0.0; 3]),
            node("track_r", Some(0), [0.0; 3]),
            node("bone_turret", Some(0), [0.0; 3]),
        ];
        Vehicle {
            name: "t".into(),
            // A 0.6 m road wheel on each side (0.3 high, centred on y = 0.3), a sprocket 0.9 m high,
            // a return roller 0.2 m high, and the tracks
            meshes: vec![
                quad(1, false, [0.0, 0.6], 0.0),
                quad(2, false, [0.0, 0.6], 0.0),
                quad(3, false, [0.25, 1.15], 0.0),
                quad(4, false, [0.8, 1.0], 0.0),
                quad(6, true, [0.0, 0.8], 1.6),
                quad(7, true, [0.0, 0.8], 1.6),
                // A skinned mesh on a node that is not a track
                quad(8, true, [0.0, 0.8], 1.6),
            ],
            materials: Vec::new(),
            nodes,
        }
    }

    fn drive() -> Drive {
        Drive {
            mass: 30000.0,
            horse_powers: 400.0,
            max_rpm: 2400.0,
            drive_gear_radius: 0.305,
            main_gear_ratio: 3.6,
            side_gear_ratio: 2.7,
            gear_ratios: vec![],
            max_brake_force: None,
            track_width: None,
            track_length: None,
            track_animation_multiplier: Some(0.68),
            road_wheel_radius: Some(0.25),
            top_wheel_radius: None,
            end_wheel_radius: None,
            max_accel: None,
            max_decel: None,
            max_ang_speed: None,
            max_ang_accel: None,
            max_fwd_speed: None,
            max_rev_speed: None,
        }
    }

    #[test]
    fn wheels_and_belts_are_found_by_name() {
        let gear = RunningGear::new(&vehicle(), Some(&drive()));
        let names: Vec<(usize, Side, WheelKind)> = gear.wheels.iter().map(|w| (w.node, w.side, w.kind)).collect();
        // wheel_l_hidden has no mesh, so it is not listed
        assert_eq!(
            names,
            [
                (1, Side::Left, WheelKind::Road),
                (2, Side::Right, WheelKind::Road),
                (3, Side::Left, WheelKind::Drive),
                (4, Side::Right, WheelKind::Top)
            ]
        );
        // Both tracks, with the V rate of their meshes; the skinned mesh on the turret is no belt
        assert_eq!(gear.belts.iter().map(|b| (b.mesh, b.side)).collect::<Vec<_>>(), [(4, Side::Left), (5, Side::Right)]);
        assert!(gear.belts.iter().all(|b| (b.v_per_metre - 1.6).abs() < 1e-5));
        // Wheels at z = -1 (left) and +1 (right): 2 m apart
        assert_eq!(gear.gauge, Some(2.0));
    }

    #[test]
    fn radii_come_from_the_unit_else_from_the_mesh() {
        let v = vehicle();
        let with_unit = RunningGear::new(&v, Some(&drive()));
        let radius = |g: &RunningGear, node| {
            let w = g.wheels.iter().find(|w| w.node == node).unwrap();
            (w.radius, w.radius_source)
        };
        assert_eq!(radius(&with_unit, 1), (0.25, RadiusSource::Unit));
        assert_eq!(radius(&with_unit, 3), (0.305, RadiusSource::Unit));
        // The unit has no return roller radius: half the mesh's 0.2 m height
        let (r, source) = radius(&with_unit, 4);
        assert!((r - 0.1).abs() < 1e-6 && source == RadiusSource::Mesh);
        // No unit at all: everything is measured (a 0.6 m road wheel, a 0.9 m sprocket)
        let measured = RunningGear::new(&v, None);
        assert!((radius(&measured, 1).0 - 0.3).abs() < 1e-6);
        assert!((radius(&measured, 3).0 - 0.45).abs() < 1e-6);
        assert_eq!(measured.wheels.iter().filter(|w| w.radius_source == RadiusSource::Mesh).count(), 4);
    }

    #[test]
    fn wheels_turn_with_the_distance_their_track_moves() {
        let gear = RunningGear::new(&vehicle(), Some(&drive()));
        let mut state = gear.rest();
        // One metre forward on the left track only: the left road wheel (0.25 m) turns 4 rad, clockwise
        gear.advance(&mut state, 1.0, 0.0);
        assert!((state.wheel_angles[0] - (TAU - 4.0)).abs() < 1e-5, "{}", state.wheel_angles[0]);
        assert_eq!(state.wheel_angles[1], 0.0);
        // The sprocket's radius is bigger, so it turns less
        assert!((state.wheel_angles[2] - (TAU - 1.0 / 0.305)).abs() < 1e-5);
        // Rolling a full circumference returns to the start
        let mut around = gear.rest();
        for _ in 0..8 {
            gear.advance(&mut around, TAU * 0.25 / 8.0, 0.0);
        }
        assert!(around.wheel_angles[0] < 1e-4 || (TAU - around.wheel_angles[0]) < 1e-4, "{}", around.wheel_angles[0]);
        // Backwards turns the other way
        let mut back = gear.rest();
        gear.advance(&mut back, -0.5, -0.5);
        assert!((back.wheel_angles[0] - 2.0).abs() < 1e-5);
    }

    #[test]
    fn belts_run_the_texture_by_the_mesh_rate() {
        let gear = RunningGear::new(&vehicle(), None);
        let mut state = gear.rest();
        gear.advance(&mut state, 0.5, -0.25);
        // 1.6 V per metre: 0.8 forward on the left, -0.4 (wrapped) on the right
        assert!((state.belt_offsets[0] - 0.8).abs() < 1e-5);
        assert!((state.belt_offsets[1] - 0.6).abs() < 1e-5);
        assert!((belt_offset_after(0.9, 1.0, 0.25) - 0.15).abs() < 1e-6);
    }
}
