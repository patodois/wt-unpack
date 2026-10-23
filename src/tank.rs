//! What a tank's unit BLK says about moving it: the turret and gun nodes with their limits, and the
//! drive train (mass, engine, gearing, tracks), plus a simple driving model built on it.
//!
//! Units live in aces.vromfs.bin as `gamedata/units/tankmodels/<unit>.blk`, and each names its 3D model
//! with `model:t` (`us_m4a1_76w_sherman` uses `m4a1_76w_sherman`). The fields used:
//!
//! ```text
//! commonWeapons{ Weapon{ speedYaw speedPitch turret{ head gun barrel } limits{ yaw pitch } limitsTable{ lim... } } }
//! VehiclePhys{ Mass{ TakeOff Empty } engine{ horsePowers maxRPM } tracks{ width trackLen trackCount }
//!   mechanics{ maxBrakeForce driveGearRadius mainGearRatio sideGearRatio gearRatios{ ratio... } }
//!   suspension{ defaultGearRadius topGearRadius frontBackGearRadius } }
//! maxAccel maxDecel maxAngSpeed maxAngAccel maxFwdSpeed maxRevSpeed   (top level)
//! ```
//!
//! The game's vehicle physics is not public. The driving model here is kinematic: engine power and the
//! BLK's acceleration limits set the acceleration, the gearing sets the top speeds, and the hull turns by
//! running its tracks at different speeds.

use std::f32::consts::PI;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::blk::{self, Block, Value};
use crate::vromfs::{self, Vromfs};

/// A weapon mount: a `Weapon` block of the unit's `commonWeapons` that has a `turret` block, with the
/// nodes that carry it and the limits it moves within. Angles are degrees, as in the BLK; the Sherman's
/// elevation of `-10..25` shows that positive pitch is up. Positive yaw is taken as towards the left, the
/// way the viewer turns the node.
#[derive(Debug, Clone, PartialEq)]
pub struct Turret {
    /// Skeleton node that traverses the mount. `commonWeapons.Weapon.turret.head`
    pub head: String,
    /// Skeleton node that elevates the gun. `Weapon.turret.gun`
    pub gun: String,
    /// Skeleton node of the recoiling barrel, when the unit names one. `Weapon.turret.barrel`
    pub barrel: Option<String>,
    /// Node the shell leaves from, when named. `Weapon.emitter`
    pub emitter: Option<String>,
    /// Node of the muzzle flash, when named (a few launchers have none). `Weapon.flash`
    pub flash: Option<String>,
    /// Traverse limits, degrees: `[min, max]`. `Weapon.limits.yaw`. A range of 360 or more is a full circle.
    pub yaw: [f32; 2],
    /// Elevation limits, degrees: `[min, max]`. `Weapon.limits.pitch`
    pub pitch: [f32; 2],
    /// Elevation limits over parts of the traverse (e.g. over the engine deck), where the unit has them:
    /// `[yaw_min, yaw_max, pitch_min, pitch_max]`, degrees. `Weapon.limitsTable`, one `p4` per row
    pub pitch_by_yaw: Vec<[f32; 4]>,
    /// Traverse speed, degrees per second. `Weapon.speedYaw`
    pub speed_yaw: f32,
    /// Elevation speed, degrees per second. `Weapon.speedPitch`
    pub speed_pitch: f32,
}

impl Turret {
    /// Clamps a traverse angle to the limits; a turret with a full circle wraps around instead
    pub fn clamp_yaw(&self, yaw: f32) -> f32 {
        if self.yaw[1] - self.yaw[0] >= 360.0 {
            (yaw + 180.0).rem_euclid(360.0) - 180.0
        } else {
            yaw.clamp(self.yaw[0], self.yaw[1])
        }
    }

    /// Elevation limits at a traverse angle
    pub fn pitch_limits(&self, yaw: f32) -> [f32; 2] {
        self.pitch_by_yaw
            .iter()
            .find(|l| (l[0]..=l[1]).contains(&yaw))
            .map_or(self.pitch, |l| [l[2], l[3]])
    }

    pub fn clamp_pitch(&self, yaw: f32, pitch: f32) -> f32 {
        let [lo, hi] = self.pitch_limits(yaw);
        pitch.clamp(lo, hi)
    }
}

/// Drive train and running gear of a unit, from the unit BLK's `VehiclePhys` block and the limits at the
/// top level of the file. Every field names its key; `Option` ones are absent from some units.
#[derive(Debug, Clone, PartialEq)]
pub struct Drive {
    /// Loaded mass, kg. `VehiclePhys.Mass.TakeOff`, else `Mass.Empty`, else the top-level `mass`
    pub mass: f32,
    /// Engine power, horsepower (converted at 745.7 W each by [`Drive::power_watts`]). `VehiclePhys.engine.horsePowers`
    pub horse_powers: f32,
    /// Engine speed at full throttle, revolutions per minute. `VehiclePhys.engine.maxRPM`
    pub max_rpm: f32,
    /// Drive sprocket radius, m. `VehiclePhys.mechanics.driveGearRadius`
    pub drive_gear_radius: f32,
    /// Final drive ratio. `VehiclePhys.mechanics.mainGearRatio`
    pub main_gear_ratio: f32,
    /// Side gear ratio. `VehiclePhys.mechanics.sideGearRatio`
    pub side_gear_ratio: f32,
    /// Gearbox ratios, in the BLK's order: negative ones are reverse gears, 0 is neutral.
    /// `mechanics.gearRatios.ratio`, one `ratio` per gear
    pub gear_ratios: Vec<f32>,
    /// Braking force, N. `mechanics.maxBrakeForce`
    pub max_brake_force: Option<f32>,
    /// Width of one track, m. `VehiclePhys.tracks.width`
    pub track_width: Option<f32>,
    /// Length of the track belt (links times link length), m. `VehiclePhys.tracks.trackLen * trackCount`
    pub track_length: Option<f32>,
    /// `VehiclePhys.tracks.animationMultiplier` (negative on some units), read as the game has it. The
    /// engine code that uses it is not public, so nothing here applies it; see [`crate::gear`].
    pub track_animation_multiplier: Option<f32>,
    /// Radius of the road wheels, m. `VehiclePhys.suspension.defaultGearRadius`
    pub road_wheel_radius: Option<f32>,
    /// Radius of the return rollers, m. `suspension.topGearRadius`
    pub top_wheel_radius: Option<f32>,
    /// Radius of the idler at the end opposite the sprocket, m. `suspension.frontBackGearRadius`
    pub end_wheel_radius: Option<f32>,
    /// Acceleration limit, m/s². Top-level `maxAccel`
    pub max_accel: Option<f32>,
    /// Deceleration limit, m/s². Top-level `maxDecel`
    pub max_decel: Option<f32>,
    /// Turn rate limit, degrees per second. Top-level `maxAngSpeed`
    pub max_ang_speed: Option<f32>,
    /// Turn acceleration limit, degrees per second². Top-level `maxAngAccel`
    pub max_ang_accel: Option<f32>,
    /// Speed limits at the top level, km/h (`maxFwdSpeed`, `maxRevSpeed`). They do not match the gearing
    /// (the Sherman says 75 km/h) and seem to be for AI-driven units, so the model uses the gearing instead.
    pub max_fwd_speed: Option<f32>,
    pub max_rev_speed: Option<f32>,
}

impl Drive {
    /// Speed with the engine at its maximum RPM in a gear, m/s
    pub fn gear_speed(&self, ratio: f32) -> f32 {
        let wheel_rpm = self.max_rpm / (ratio.abs() * self.main_gear_ratio * self.side_gear_ratio);
        wheel_rpm / 60.0 * 2.0 * PI * self.drive_gear_radius
    }

    /// Top speed in the highest forward gear, m/s
    pub fn top_speed(&self) -> f32 {
        let ratio = self.gear_ratios.iter().copied().filter(|&r| r > 0.0).fold(f32::MAX, f32::min);
        if ratio == f32::MAX { 0.0 } else { self.gear_speed(ratio) }
    }

    /// Top speed in the fastest reverse gear, m/s
    pub fn reverse_speed(&self) -> f32 {
        let ratio = self.gear_ratios.iter().copied().filter(|&r| r < 0.0).fold(f32::MIN, f32::max);
        if ratio == f32::MIN { 0.0 } else { self.gear_speed(ratio) }
    }

    /// Top speed in first gear, m/s
    pub fn first_gear_speed(&self) -> f32 {
        let ratio = self.gear_ratios.iter().copied().fold(0.0, f32::max);
        if ratio > 0.0 { self.gear_speed(ratio) } else { 0.0 }
    }

    pub fn power_watts(&self) -> f32 {
        self.horse_powers * 745.7
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tank {
    /// Unit name, e.g. `us_m4a1_76w_sherman`
    pub unit: String,
    /// The first weapon with a turret: the main gun comes first in `commonWeapons`
    pub turret: Option<Turret>,
    /// Every weapon mount with a turret, in the BLK's order (the main gun first, then the machine guns)
    pub weapons: Vec<Turret>,
    pub drive: Option<Drive>,
}

fn child<'a>(b: &'a Block, name: &str) -> Option<&'a Block> {
    b.blocks.iter().find(|c| c.name == name)
}

fn param<'a>(b: &'a Block, name: &str) -> Option<&'a Value> {
    b.params.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

fn real(b: &Block, name: &str) -> Option<f32> {
    match param(b, name)? {
        Value::Real(x) => Some(*x),
        Value::Int(x) => Some(*x as f32),
        _ => None,
    }
}

fn text<'a>(b: &'a Block, name: &str) -> Option<&'a str> {
    match param(b, name)? {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

fn point2(b: &Block, name: &str) -> Option<[f32; 2]> {
    match param(b, name)? {
        Value::Point2(p) => Some(*p),
        _ => None,
    }
}

/// The weapon mounts, in BLK order. A `Weapon` counts when its `turret` block names a `head` and a `gun` and
/// the weapon has `limits` (with `yaw` and `pitch`), `speedYaw` and `speedPitch`: nothing is made up for
/// a mount that lacks them (all 1,308 turreted units of the game have them).
fn weapons(root: &Block) -> Vec<Turret> {
    let Some(common) = child(root, "commonWeapons") else { return Vec::new() };
    common
        .blocks
        .iter()
        .filter(|w| w.name == "Weapon")
        .filter_map(|w| {
            let t = child(w, "turret")?;
            let limits = child(w, "limits")?;
            let pitch_by_yaw = child(w, "limitsTable").map_or_else(Vec::new, |table| {
                table
                    .params
                    .iter()
                    .filter_map(|(_, v)| match v {
                        Value::Point4(p) => Some(*p),
                        _ => None,
                    })
                    .collect()
            });
            Some(Turret {
                head: text(t, "head")?.to_string(),
                gun: text(t, "gun")?.to_string(),
                barrel: text(t, "barrel").map(str::to_string),
                emitter: text(w, "emitter").map(str::to_string),
                flash: text(w, "flash").map(str::to_string),
                yaw: point2(limits, "yaw")?,
                pitch: point2(limits, "pitch")?,
                pitch_by_yaw,
                speed_yaw: real(w, "speedYaw")?,
                speed_pitch: real(w, "speedPitch")?,
            })
        })
        .collect()
}

fn drive(root: &Block) -> Option<Drive> {
    let phys = child(root, "VehiclePhys")?;
    let mass = child(phys, "Mass").and_then(|m| real(m, "TakeOff").or_else(|| real(m, "Empty"))).or_else(|| real(root, "mass"))?;
    let engine = child(phys, "engine")?;
    let mech = child(phys, "mechanics")?;
    let tracks = child(phys, "tracks");
    let susp = child(phys, "suspension");
    let gear_ratios = child(mech, "gearRatios")
        .map(|g| {
            g.params
                .iter()
                .filter(|(n, _)| n == "ratio")
                .filter_map(|(_, v)| match v {
                    Value::Real(x) => Some(*x),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let track_length = tracks.and_then(|t| Some(real(t, "trackLen")? * real(t, "trackCount")?));
    Some(Drive {
        mass,
        horse_powers: real(engine, "horsePowers")?,
        max_rpm: real(engine, "maxRPM")?,
        drive_gear_radius: real(mech, "driveGearRadius")?,
        main_gear_ratio: real(mech, "mainGearRatio")?,
        side_gear_ratio: real(mech, "sideGearRatio")?,
        gear_ratios,
        max_brake_force: real(mech, "maxBrakeForce"),
        track_width: tracks.and_then(|t| real(t, "width")),
        track_length,
        track_animation_multiplier: tracks.and_then(|t| real(t, "animationMultiplier")),
        road_wheel_radius: susp.and_then(|s| real(s, "defaultGearRadius")),
        top_wheel_radius: susp.and_then(|s| real(s, "topGearRadius")),
        end_wheel_radius: susp.and_then(|s| real(s, "frontBackGearRadius")),
        max_accel: real(root, "maxAccel"),
        max_decel: real(root, "maxDecel"),
        max_ang_speed: real(root, "maxAngSpeed"),
        max_ang_accel: real(root, "maxAngAccel"),
        max_fwd_speed: real(root, "maxFwdSpeed"),
        max_rev_speed: real(root, "maxRevSpeed"),
    })
}

pub fn from_blk(unit: &str, root: &Block) -> Tank {
    let weapons = weapons(root);
    Tank { unit: unit.to_string(), turret: weapons.first().cloned(), weapons, drive: drive(root) }
}

const UNIT_DIR: &str = "gamedata/units/tankmodels/";

/// `aces.vromfs.bin` opened for reading unit files
struct Units {
    vfs: Vromfs,
    shared: Option<blk::SharedContext>,
}

impl Units {
    fn open(game: &Path) -> Result<Self> {
        let path = game.join("aces.vromfs.bin");
        let raw = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let vfs = Vromfs::parse(&raw)?;
        drop(raw);
        let shared = vromfs::shared_context(&vfs)?;
        Ok(Self { vfs, shared })
    }

    fn names(&self) -> Vec<String> {
        let mut units: Vec<String> = self
            .vfs
            .files
            .iter()
            .filter_map(|f| f.name.strip_prefix(UNIT_DIR).and_then(|n| n.strip_suffix(".blk")).filter(|n| !n.contains('/')).map(str::to_string))
            .collect();
        units.sort();
        units
    }

    fn parse(&self, name: &str) -> Result<Block> {
        let file = self.vfs.get(&format!("{UNIT_DIR}{name}.blk")).with_context(|| format!("no unit {name} in aces.vromfs.bin"))?;
        blk::parse(&file.data, self.shared.as_ref()).with_context(|| format!("parsing unit {name}"))
    }
}

/// The unit that uses a model: units are usually named after their model with a country prefix, so those
/// come first; ties go alphabetically
fn pick_unit<'a>(model: &str, candidates: impl Iterator<Item = (&'a str, &'a str)>) -> Option<&'a str> {
    let model_lc = model.to_lowercase();
    let mut best: Option<(&str, bool)> = None;
    for (unit, unit_model) in candidates {
        if !unit_model.eq_ignore_ascii_case(model) {
            continue;
        }
        let likely = unit.to_lowercase().ends_with(&model_lc);
        // `names()` is sorted, so the first of each kind is the alphabetical one
        if best.is_none_or(|(_, l)| likely && !l) {
            best = Some((unit, likely));
        }
    }
    best.map(|b| b.0)
}

/// Which unit uses which 3D model, read once from `aces.vromfs.bin`: a few kilobytes, so a program that
/// looks up many vehicles scans the units once instead of for every lookup.
#[derive(Debug, Clone, Default)]
pub struct UnitIndex {
    /// `(unit, model)`, sorted by unit
    units: Vec<(String, String)>,
}

impl UnitIndex {
    pub fn build(game: &Path) -> Result<Self> {
        let units = Units::open(game)?;
        let mut out = Vec::new();
        for name in units.names() {
            let Ok(root) = units.parse(&name) else { continue };
            if let Some(model) = text(&root, "model") {
                out.push((name, model.to_string()));
            }
        }
        Ok(Self { units: out })
    }

    /// The unit that uses a 3D model (the one named after it when several do)
    pub fn unit_of_model(&self, model: &str) -> Option<&str> {
        pick_unit(model, self.units.iter().map(|(u, m)| (u.as_str(), m.as_str())))
    }

    /// Every unit, with its model
    pub fn units(&self) -> &[(String, String)] {
        &self.units
    }
}

/// Loads the named unit's data
pub fn load_unit(game: &Path, unit: &str) -> Result<Tank> {
    let units = Units::open(game)?;
    Ok(from_blk(unit, &units.parse(unit)?))
}

/// Finds the unit that uses a model, or loads the named unit. Returns None when no unit uses the model
/// (e.g. aircraft and ships, or models only used as parts).
pub fn load(game: &Path, model: &str, unit: Option<&str>) -> Result<Option<Tank>> {
    let units = Units::open(game)?;
    if let Some(name) = unit {
        return Ok(Some(from_blk(name, &units.parse(name)?)));
    }
    // Units are usually named after their model with a country prefix; try those first
    let model_lc = model.to_lowercase();
    let (likely, rest): (Vec<_>, Vec<_>) = units.names().into_iter().partition(|u| u.to_lowercase().ends_with(&model_lc));
    for name in likely.iter().chain(&rest) {
        let Ok(root) = units.parse(name) else { continue };
        if text(&root, "model").is_some_and(|m| m.eq_ignore_ascii_case(model)) {
            return Ok(Some(from_blk(name, &root)));
        }
    }
    Ok(None)
}

/// Driver input: throttle forward/back and steering (positive turns left), each -1..1
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Controls {
    pub throttle: f32,
    pub steer: f32,
}

/// Rolling resistance of tracks on firm ground, as a fraction of the weight
const ROLLING_RESISTANCE: f32 = 0.04;
/// Share of the engine power that reaches the tracks
const TRANSMISSION_EFFICIENCY: f32 = 0.8;
/// Sideways grip that limits turning at speed, in g
const LATERAL_GRIP: f32 = 0.5;
const G: f32 = 9.81;

/// Speed and turn rate of the hull
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DriveState {
    /// m/s, positive forward
    pub speed: f32,
    /// rad/s, positive turning left (counter-clockwise seen from above)
    pub yaw_rate: f32,
}

/// -1, 0 or 1 (`f32::signum` gives 1 for 0)
fn sign(x: f32) -> f32 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

impl DriveState {
    /// Advances by `dt` seconds. `gauge` is the distance between the track centres in metres.
    pub fn step(&mut self, d: &Drive, gauge: f32, c: Controls, dt: f32) {
        let throttle = c.throttle.clamp(-1.0, 1.0);
        let v = self.speed;
        let max_accel = d.max_accel.unwrap_or(3.0);
        let brake = d.max_brake_force.map_or(max_accel * 2.0, |f| f / d.mass).min(d.max_decel.unwrap_or(f32::MAX));
        let driving = throttle != 0.0 && (v == 0.0 || sign(throttle) == sign(v));
        let accel = if driving {
            // Power-limited, and no faster than the BLK allows; below 1 m/s the force is that at 1 m/s
            let power = TRANSMISSION_EFFICIENCY * d.power_watts() / (d.mass * v.abs().max(1.0));
            sign(throttle) * power.min(max_accel) * throttle.abs()
        } else {
            // Throttle against the motion brakes
            -sign(v) * brake * throttle.abs()
        };
        let mut next = v + (accel - sign(v) * ROLLING_RESISTANCE * G) * dt;
        if driving {
            // The engine cannot turn faster than its maximum RPM in the top gear
            let limit = if throttle > 0.0 { d.top_speed() } else { d.reverse_speed() } * throttle.abs();
            if next.abs() > limit.max(v.abs()) {
                next = sign(next) * limit.max(v.abs());
            }
        } else if sign(next) != sign(v) {
            // Brakes and resistance stop the tank; they do not reverse it
            next = 0.0;
        }
        self.speed = next;

        // Turning: at a standstill one track runs at first-gear speed while the other is braked; on the
        // move the inner track slows down, up to what sideways grip allows
        let gauge = gauge.max(0.5);
        let pivot = d.first_gear_speed() / gauge;
        let moving = (self.speed.abs() / gauge).min(LATERAL_GRIP * G / self.speed.abs().max(0.1));
        let mut max_rate = pivot.max(moving);
        if let Some(cap) = d.max_ang_speed {
            max_rate = max_rate.min(cap.to_radians());
        }
        let target = c.steer.clamp(-1.0, 1.0) * max_rate;
        let step = d.max_ang_accel.map_or(1.0, f32::to_radians) * dt;
        self.yaw_rate += (target - self.yaw_rate).clamp(-step, step);
    }

    /// Speeds of the left and right tracks, m/s
    pub fn track_speeds(&self, gauge: f32) -> (f32, f32) {
        let half = self.yaw_rate * gauge / 2.0;
        (self.speed - half, self.speed + half)
    }
}

/// A scripted drive: `W:20,WA:3,S:2` holds W for 20 s, then W and A for 3 s, then S for 2 s. W/S are
/// throttle, A/D steer left/right, and `-` (or nothing) is no input.
pub fn parse_script(s: &str) -> Result<Vec<(Controls, f32)>> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (keys, secs) = part.split_once(':').with_context(|| format!("{part}: expected KEYS:seconds"))?;
        let secs: f32 = secs.trim().parse().with_context(|| format!("{part}: bad duration"))?;
        let mut c = Controls::default();
        for k in keys.trim().chars() {
            match k.to_ascii_uppercase() {
                'W' => c.throttle += 1.0,
                'S' => c.throttle -= 1.0,
                'A' => c.steer += 1.0,
                'D' => c.steer -= 1.0,
                '-' => {}
                other => bail!("{part}: unknown key {other}"),
            }
        }
        out.push((c, secs));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(name: &str, params: Vec<(&str, Value)>, blocks: Vec<Block>) -> Block {
        Block { name: name.to_string(), params: params.into_iter().map(|(n, v)| (n.to_string(), v)).collect(), blocks }
    }

    /// A unit BLK shaped like the Sherman's, with the fields this module reads
    fn sherman() -> Block {
        let s = |x: &str| Value::Str(x.to_string());
        let weapon = block(
            "Weapon",
            vec![("emitter", s("bone_gun_barrel")), ("flash", s("emtr_gun_flame")), ("speedYaw", Value::Real(24.0)), ("speedPitch", Value::Real(4.0))],
            vec![
                block("turret", vec![("head", s("bone_turret")), ("gun", s("bone_gun")), ("barrel", s("bone_gun_barrel"))], vec![]),
                block("limits", vec![("yaw", Value::Point2([-180.0, 180.0])), ("pitch", Value::Point2([-10.0, 25.0]))], vec![]),
                block(
                    "limitsTable",
                    vec![
                        ("lim1", Value::Point4([-180.0, -160.0, -7.0, 25.0])),
                        ("lim2", Value::Point4([-160.0, 160.0, -10.0, 25.0])),
                        ("lim3", Value::Point4([160.0, 180.0, -7.0, 25.0])),
                    ],
                    vec![],
                ),
            ],
        );
        // A coaxial machine gun without its own turret block comes second and is ignored
        let mg = block("Weapon", vec![("speedYaw", Value::Real(5.5))], vec![]);
        let ratios = [-5.64, 0.0, 7.55, 3.1, 1.795, 1.111, 0.733].map(|r| ("ratio", Value::Real(r))).to_vec();
        let phys = block(
            "VehiclePhys",
            vec![],
            vec![
                block("Mass", vec![("Empty", Value::Real(32373.0)), ("TakeOff", Value::Real(32673.0))], vec![]),
                block(
                    "tracks",
                    vec![("width", Value::Real(0.4)), ("trackLen", Value::Real(0.154)), ("trackCount", Value::Int(79)), ("animationMultiplier", Value::Real(0.68))],
                    vec![],
                ),
                block("engine", vec![("horsePowers", Value::Real(400.0)), ("maxRPM", Value::Real(2400.0))], vec![]),
                block(
                    "mechanics",
                    vec![
                        ("maxBrakeForce", Value::Real(130000.0)),
                        ("driveGearRadius", Value::Real(0.305)),
                        ("mainGearRatio", Value::Real(3.615)),
                        ("sideGearRatio", Value::Real(2.7)),
                    ],
                    vec![block("gearRatios", ratios, vec![])],
                ),
                block(
                    "suspension",
                    vec![("defaultGearRadius", Value::Real(0.25)), ("topGearRadius", Value::Real(0.13)), ("frontBackGearRadius", Value::Real(0.28))],
                    vec![],
                ),
            ],
        );
        block(
            "",
            vec![
                ("model", s("m4a1_76w_sherman")),
                ("maxFwdSpeed", Value::Real(75.0)),
                ("maxAccel", Value::Real(4.0)),
                ("maxDecel", Value::Real(8.0)),
                ("maxAngSpeed", Value::Real(30.0)),
                ("maxAngAccel", Value::Real(40.0)),
                ("mass", Value::Real(31800.0)),
            ],
            vec![block("commonWeapons", vec![], vec![weapon, mg]), phys],
        )
    }

    #[test]
    fn reads_the_turret_and_its_limits() {
        let t = from_blk("us_m4a1_76w_sherman", &sherman()).turret.unwrap();
        assert_eq!((t.head.as_str(), t.gun.as_str(), t.barrel.as_deref()), ("bone_turret", "bone_gun", Some("bone_gun_barrel")));
        assert_eq!((t.speed_yaw, t.speed_pitch), (24.0, 4.0));
        // A full circle wraps; elevation is clamped, with less depression over the engine deck
        assert_eq!(t.clamp_yaw(200.0), -160.0);
        assert_eq!(t.clamp_pitch(0.0, -20.0), -10.0);
        assert_eq!(t.clamp_pitch(170.0, -20.0), -7.0);
        assert_eq!(t.clamp_pitch(-170.0, 40.0), 25.0);
        let limited = Turret { yaw: [-15.0, 15.0], pitch_by_yaw: vec![], ..t };
        assert_eq!(limited.clamp_yaw(60.0), 15.0);
        assert_eq!(limited.pitch_limits(0.0), [-10.0, 25.0]);
    }

    #[test]
    fn every_weapon_with_a_turret_is_listed_and_the_first_is_the_main_gun() {
        let t = from_blk("us_m4a1_76w_sherman", &sherman());
        // The coaxial machine gun in the fixture has no turret block, so only the main gun counts
        assert_eq!(t.weapons.len(), 1);
        assert_eq!(t.turret.as_ref(), t.weapons.first());
        let main = t.turret.unwrap();
        // The muzzle flash and the shell emitter are named by the weapon
        assert_eq!((main.emitter.as_deref(), main.flash.as_deref()), (Some("bone_gun_barrel"), Some("emtr_gun_flame")));
    }

    #[test]
    fn a_mount_without_limits_or_speeds_is_not_made_up() {
        let mut root = sherman();
        let weapons = root.blocks.iter_mut().find(|b| b.name == "commonWeapons").unwrap();
        let main = &mut weapons.blocks[0];
        main.params.retain(|(n, _)| n != "speedYaw");
        assert!(from_blk("x", &root).turret.is_none());
        let mut root = sherman();
        let weapons = root.blocks.iter_mut().find(|b| b.name == "commonWeapons").unwrap();
        weapons.blocks[0].blocks.retain(|b| b.name != "limits");
        assert!(from_blk("x", &root).weapons.is_empty());
    }

    #[test]
    fn units_are_picked_by_model_name_first() {
        let units = [("ussr_t_34", "t_34_1942"), ("germ_pzkpfw_iv", "pzkpfw_iv"), ("us_m4_event", "m4_sherman"), ("us_m4_sherman", "m4_sherman")];
        let index = UnitIndex { units: units.iter().map(|(u, m)| (u.to_string(), m.to_string())).collect() };
        // Two units share the model; the one named after it wins, not the alphabetically first
        assert_eq!(index.unit_of_model("m4_sherman"), Some("us_m4_sherman"));
        assert_eq!(index.unit_of_model("M4_SHERMAN"), Some("us_m4_sherman"));
        assert_eq!(index.unit_of_model("t_34_1942"), Some("ussr_t_34"));
        assert_eq!(index.unit_of_model("none"), None);
        // Without a likely name, the alphabetical first
        let only = UnitIndex { units: vec![("a_unit".into(), "m".into()), ("b_unit".into(), "m".into())] };
        assert_eq!(only.unit_of_model("m"), Some("a_unit"));
    }

    #[test]
    fn reads_the_drive_train() {
        let d = from_blk("us_m4a1_76w_sherman", &sherman()).drive.unwrap();
        assert_eq!(d.mass, 32673.0);
        assert_eq!(d.gear_ratios.len(), 7);
        assert_eq!(d.track_width, Some(0.4));
        assert_eq!(d.track_animation_multiplier, Some(0.68));
        assert!((d.track_length.unwrap() - 12.166).abs() < 1e-3);
        assert_eq!(d.road_wheel_radius, Some(0.25));
        // 2400 RPM through 0.733 x 3.615 x 2.7 on a 0.305 m sprocket: 38.6 km/h; reverse 5.0 km/h
        assert!((d.top_speed() * 3.6 - 38.6).abs() < 0.1, "{}", d.top_speed() * 3.6);
        assert!((d.reverse_speed() * 3.6 - 5.02).abs() < 0.05, "{}", d.reverse_speed() * 3.6);
        assert!((d.first_gear_speed() * 3.6 - 3.75).abs() < 0.05, "{}", d.first_gear_speed() * 3.6);
    }

    #[test]
    fn units_without_physics_have_no_drive() {
        let mut root = sherman();
        root.blocks.retain(|b| b.name != "VehiclePhys");
        assert!(from_blk("x", &root).drive.is_none());
        root.blocks.clear();
        assert_eq!(from_blk("x", &root), Tank { unit: "x".into(), turret: None, weapons: Vec::new(), drive: None });
    }

    #[test]
    fn drives_up_to_the_geared_top_speed_and_stops() {
        let d = from_blk("x", &sherman()).drive.unwrap();
        let mut s = DriveState::default();
        let dt = 1.0 / 60.0;
        let full = Controls { throttle: 1.0, steer: 0.0 };
        // Never faster than the BLK's acceleration limit
        s.step(&d, 2.16, full, dt);
        assert!(s.speed > 0.0 && s.speed <= 4.0 * dt + 1e-6);
        for _ in 0..60 * 60 {
            s.step(&d, 2.16, full, dt);
        }
        assert!((s.speed - d.top_speed()).abs() < 0.01, "{} vs {}", s.speed, d.top_speed());
        // Holding S brakes to a stop within a few seconds, landing on 0, and only then reverses
        let back = Controls { throttle: -1.0, steer: 0.0 };
        let mut steps = 0;
        while s.speed > 0.0 {
            s.step(&d, 2.16, back, dt);
            steps += 1;
        }
        assert!(steps < 60 * 4 && s.speed == 0.0, "{steps} steps, {}", s.speed);
        for _ in 0..60 * 10 {
            s.step(&d, 2.16, back, dt);
        }
        assert!((s.speed + d.reverse_speed()).abs() < 0.01, "{}", s.speed);
        // Coasting stops too
        s.speed = 3.0;
        for _ in 0..60 * 20 {
            s.step(&d, 2.16, Controls::default(), dt);
        }
        assert_eq!(s.speed, 0.0);
    }

    #[test]
    fn turns_by_track_speed_difference() {
        let d = from_blk("x", &sherman()).drive.unwrap();
        let mut s = DriveState::default();
        for _ in 0..60 * 3 {
            s.step(&d, 2.16, Controls { throttle: 0.0, steer: 1.0 }, 1.0 / 60.0);
        }
        // Pivoting left: one track at first-gear speed, under the BLK's 30 deg/s
        let (left, right) = s.track_speeds(2.16);
        assert!(s.yaw_rate > 0.0 && s.yaw_rate <= 30f32.to_radians());
        assert!((right - left - s.yaw_rate * 2.16).abs() < 1e-5 && left < 0.0 && right > 0.0);
        assert!((right - left - d.first_gear_speed()).abs() < 1e-3);
    }

    #[test]
    fn parses_scripts() {
        let s = parse_script("W:20, wa:3,S:1.5,-:2").unwrap();
        let c = |throttle, steer| Controls { throttle, steer };
        assert_eq!(s, vec![(c(1.0, 0.0), 20.0), (c(1.0, 1.0), 3.0), (c(-1.0, 0.0), 1.5), (c(0.0, 0.0), 2.0)]);
        assert!(parse_script("W").is_err());
        assert!(parse_script("X:1").is_err());
    }
}
