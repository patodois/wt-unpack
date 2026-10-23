//! Client for War Thunder's read-only telemetry API on `localhost:8111`.
//!
//! While a match or test drive runs, the game client serves JSON on port 8111 for in-game browser pages
//! and community overlays. This module only reads what the game publishes there; it never touches the
//! game process. The endpoints and their fields, as documented by community clients (for example the
//! `WarThunder` Python package) and checked against nothing else:
//!
//! | Endpoint | Content |
//! |---|---|
//! | `/state` | Flight model of the player's aircraft, keys with units: `"H, m"`, `"TAS, km/h"`, `"IAS, km/h"`, `"M"`, `"AoA, deg"`, `"Vy, m/s"`, `"Mfuel, kg"`, `"Mfuel0, kg"`, `"throttle 1, %"`, `"RPM 1"`, `"gear, %"` (landing gear), `"flaps, %"`, ... |
//! | `/indicators` | Cockpit and HUD gauges: `"army"` (`"air"`, `"tank"`, ...), `"type"` (the unit, e.g. `tankModels/us_m4a1_76w_sherman`), `"speed"`, `"compass"`, `"throttle"` (0..1, aircraft), `"gear"` and `"gear_neutral"` (gearbox, ground vehicles), `"rpm"`, `"crew_current"`, `"crew_total"`, ... The set depends on the vehicle |
//! | `/map_info.json` | The minimap's world bounds: `"map_min"`, `"map_max"` (metres), `"grid_zero"`, `"grid_steps"`, `"grid_size"`, `"map_generation"`, `"hud_type"` |
//! | `/map_obj.json` | The objects drawn on the player's minimap: `"type"`, `"icon"`, `"icon_bg"`, `"color"`, `"color[]"`, `"blink"`, position `"x"`/`"y"` and direction `"dx"`/`"dy"` normalised to the map, or a segment `"sx"`/`"sy"`..`"ex"`/`"ey"` (airfields) |
//! | `/map.img` | The minimap image (`?gen=<map_generation>`) |
//!
//! Every endpoint answers `{"valid": false}` (or an empty list) outside a match; for ground vehicles
//! `/state` stays `{"valid": false}` in a match too, so a match is detected from either endpoint. Each
//! struct keeps the
//! whole JSON object in `raw`, so fields the game adds later are not lost, and reads the known keys
//! leniently: a missing or oddly typed key becomes `None` instead of failing the parse.
//!
//! A real test drive (a T-26, about 1,100 polls in a match) confirmed the speed unit (km/h, negative
//! in reverse), the metre scale of the bounds, the sign of `dx`/`dy`, and the gear numbering (see
//! [`Gear`]). About 2% of polls fail on their own: an empty `/map_obj.json` body or a timeout. The
//! [`Poller`] rides over those for a short grace period instead of leaving the match.
//!
//! # Coordinates
//!
//! `map_obj.json` positions are normalised to the minimap image: `x` grows to the right (east) and `y`
//! grows downwards (south), `0..1` across the map. `map_info.json` gives the bounds in metres, with
//! `map_min = [west, south]` and `map_max = [east, north]`. So
//!
//! ```text
//! east  = map_min[0] + x * (map_max[0] - map_min[0])
//! north = map_max[1] - y * (map_max[1] - map_min[1])
//! ```
//!
//! and the scene position, in the crate's glTF convention (metres, +Y up, right-handed, +Z forward), puts
//! map north on +Z. Right-handedness then makes +X point west:
//!
//! ```text
//! [X, Y, Z] = [-east, altitude, north]
//! ```
//!
//! `altitude` is `"H, m"` from `/state` when the game reports it (aircraft) and 0 otherwise: the API
//! gives no height for ground vehicles. Headings are compass degrees, clockwise from north seen from
//! above; heading `h` points along `[-sin h, 0, cos h]`, which is a rotation of `-h` about +Y applied
//! to a model facing +Z. Vehicles assembled by [`crate::vehicle`] face +X instead and need `-h - 90°`.
//! `grid_zero` is the corner the in-game grid letters count from and is not added to positions.
//! Not yet checked: whether map north is +Z of the level geometry exported from the game files.

#[cfg(feature = "live")]
use std::time::Duration;

use serde_json::{Map, Value};

/// The game's telemetry server. `127.0.0.1` rather than `localhost` so no lookup can pick IPv6 first.
pub const DEFAULT_URL: &str = "http://127.0.0.1:8111";

fn num(raw: &Map<String, Value>, key: &str) -> Option<f64> {
    match raw.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

fn text(raw: &Map<String, Value>, key: &str) -> Option<String> {
    raw.get(key)?.as_str().map(|s| s.replace('\u{a0}', " ").trim().to_string())
}

fn flag(raw: &Map<String, Value>, key: &str) -> bool {
    num(raw, key).is_some_and(|v| v != 0.0)
}

fn pair(raw: &Map<String, Value>, key: &str) -> Option<[f64; 2]> {
    match raw.get(key)?.as_array()?.as_slice() {
        [a, b, ..] => Some([a.as_f64()?, b.as_f64()?]),
        _ => None,
    }
}

fn two(raw: &Map<String, Value>, a: &str, b: &str) -> Option<[f64; 2]> {
    Some([num(raw, a)?, num(raw, b)?])
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// `/state`: the flight model of the player's aircraft. Ground vehicles get few or none of these.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub valid: bool,
    /// `"H, m"`: altitude above sea level
    pub altitude_m: Option<f64>,
    /// `"TAS, km/h"`, true airspeed
    pub tas_kmh: Option<f64>,
    /// `"IAS, km/h"`, indicated airspeed
    pub ias_kmh: Option<f64>,
    /// `"M"`, Mach number
    pub mach: Option<f64>,
    /// `"AoA, deg"`, angle of attack
    pub aoa_deg: Option<f64>,
    /// `"Vy, m/s"`, vertical speed
    pub vy_ms: Option<f64>,
    /// `"Mfuel, kg"` and `"Mfuel0, kg"`: fuel left and the full load
    pub fuel_kg: Option<f64>,
    pub fuel_max_kg: Option<f64>,
    /// `"throttle 1, %"`, first engine
    pub throttle_pct: Option<f64>,
    /// `"RPM 1"`, first engine
    pub rpm: Option<f64>,
    /// `"gear, %"`: landing gear extension, not a gearbox
    pub landing_gear_pct: Option<f64>,
    /// `"flaps, %"`
    pub flaps_pct: Option<f64>,
    /// The whole object, including keys not listed above
    pub raw: Map<String, Value>,
}

impl State {
    pub fn from_json(value: Value) -> Self {
        let raw = object(value);
        Self {
            valid: flag(&raw, "valid"),
            altitude_m: num(&raw, "H, m"),
            tas_kmh: num(&raw, "TAS, km/h"),
            ias_kmh: num(&raw, "IAS, km/h"),
            mach: num(&raw, "M"),
            aoa_deg: num(&raw, "AoA, deg"),
            vy_ms: num(&raw, "Vy, m/s"),
            fuel_kg: num(&raw, "Mfuel, kg"),
            fuel_max_kg: num(&raw, "Mfuel0, kg"),
            throttle_pct: num(&raw, "throttle 1, %"),
            rpm: num(&raw, "RPM 1"),
            landing_gear_pct: num(&raw, "gear, %"),
            flaps_pct: num(&raw, "flaps, %"),
            raw,
        }
    }
}

/// What `"army"` in `/indicators` says the player is driving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Army {
    Air,
    Tank,
    Ship,
    Other(String),
}

impl Army {
    fn parse(s: &str) -> Self {
        match s {
            "air" => Self::Air,
            "tank" => Self::Tank,
            "ship" | "naval" => Self::Ship,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Air => "air",
            Self::Tank => "tank",
            Self::Ship => "ship",
            Self::Other(s) => s,
        }
    }
}

/// `/indicators`: the gauges of the player's vehicle. Which keys appear depends on the vehicle.
#[derive(Clone, Debug, Default)]
pub struct Indicators {
    pub valid: bool,
    /// `"army"`
    pub army: Option<Army>,
    /// `"type"`: the unit, e.g. `tankModels/us_m4a1_76w_sherman` or `p-51d-5`
    pub unit: Option<String>,
    /// `"speed"`: km/h for ground vehicles (the HUD speedometer), negative in reverse
    pub speed: Option<f64>,
    /// `"compass"`: heading in degrees (aircraft)
    pub compass_deg: Option<f64>,
    /// `"throttle"`, 0..1; ground vehicles do not report it
    pub throttle: Option<f64>,
    /// `"gear"`: raw gearbox index of ground vehicles, counted from the lowest reverse gear. Use
    /// [`Indicators::gearbox`] for the gear as the HUD shows it
    pub gear: Option<f64>,
    /// `"gear_neutral"`: the index of neutral in `gear`
    pub gear_neutral: Option<f64>,
    /// `"rpm"`
    pub rpm: Option<f64>,
    /// `"crew_current"` and `"crew_total"`: crew members left and the full crew (ground vehicles)
    pub crew_current: Option<f64>,
    pub crew_total: Option<f64>,
    /// The whole object, including keys not listed above
    pub raw: Map<String, Value>,
}

impl Indicators {
    pub fn from_json(value: Value) -> Self {
        let raw = object(value);
        Self {
            valid: flag(&raw, "valid"),
            army: text(&raw, "army").map(|s| Army::parse(&s)),
            unit: text(&raw, "type").filter(|s| !s.is_empty()),
            speed: num(&raw, "speed"),
            compass_deg: num(&raw, "compass").or_else(|| num(&raw, "compass1")),
            throttle: num(&raw, "throttle").or_else(|| num(&raw, "throttle1")),
            gear: num(&raw, "gear"),
            gear_neutral: num(&raw, "gear_neutral"),
            rpm: num(&raw, "rpm"),
            crew_current: num(&raw, "crew_current"),
            crew_total: num(&raw, "crew_total"),
            raw,
        }
    }

    /// The gearbox position as the HUD shows it, from `gear` and `gear_neutral`.
    pub fn gearbox(&self) -> Option<Gear> {
        let gear = self.gear?.round() as i64;
        Some(match self.gear_neutral {
            Some(n) => Gear::from_index(gear, n.round() as i64),
            None => Gear::Index(gear),
        })
    }

    /// The unit name without its folder, e.g. `us_m4a1_76w_sherman`.
    pub fn unit_name(&self) -> Option<&str> {
        self.unit.as_deref().map(|u| u.rsplit('/').next().unwrap_or(u))
    }
}

/// A ground vehicle's gearbox position. The game counts `gear` up from the lowest reverse gear and gives
/// the index of neutral in `gear_neutral`; on the T-26 (one reverse, five forward gears) neutral is 1,
/// so `gear` 0 is reverse and 2..6 are first to fifth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gear {
    /// Reverse gear `n`, 1 being the one next to neutral
    Reverse(u32),
    Neutral,
    /// Forward gear `n`, from 1
    Forward(u32),
    /// The raw index, when the game sends no `gear_neutral`. This was the only output before the
    /// numbering was confirmed and is superseded by the variants above
    Index(i64),
}

impl Gear {
    pub fn from_index(gear: i64, neutral: i64) -> Self {
        match gear - neutral {
            0 => Self::Neutral,
            d if d > 0 => Self::Forward(d as u32),
            d => Self::Reverse(d.unsigned_abs() as u32),
        }
    }
}

impl std::fmt::Display for Gear {
    /// `R`, `N`, `1`..`n`; `R2` for a second reverse gear; a raw index as `#3`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reverse(1) => write!(f, "R"),
            Self::Reverse(n) => write!(f, "R{n}"),
            Self::Neutral => write!(f, "N"),
            Self::Forward(n) => write!(f, "{n}"),
            Self::Index(i) => write!(f, "#{i}"),
        }
    }
}

/// `/map_info.json`: where the minimap sits in the world.
#[derive(Clone, Debug, Default)]
pub struct MapInfo {
    pub valid: bool,
    /// `"map_min"`, `[west, south]` in metres
    pub map_min: Option<[f64; 2]>,
    /// `"map_max"`, `[east, north]` in metres
    pub map_max: Option<[f64; 2]>,
    /// `"grid_zero"`: the corner the grid labels count from, in metres
    pub grid_zero: Option<[f64; 2]>,
    /// `"grid_steps"`: grid cell size in metres
    pub grid_steps: Option<[f64; 2]>,
    /// `"grid_size"`
    pub grid_size: Option<[f64; 2]>,
    /// `"map_generation"`: changes when the map image changes; pass it to `/map.img?gen=`
    pub map_generation: Option<i64>,
    /// `"hud_type"`
    pub hud_type: Option<i64>,
    pub raw: Map<String, Value>,
}

impl MapInfo {
    pub fn from_json(value: Value) -> Self {
        let raw = object(value);
        let int = |k: &str| num(&raw, k).map(|v| v as i64);
        Self {
            valid: flag(&raw, "valid"),
            map_min: pair(&raw, "map_min"),
            map_max: pair(&raw, "map_max"),
            grid_zero: pair(&raw, "grid_zero"),
            grid_steps: pair(&raw, "grid_steps"),
            grid_size: pair(&raw, "grid_size"),
            map_generation: int("map_generation"),
            hud_type: int("hud_type"),
            raw,
        }
    }

    /// The bounds, when both corners are present and span a non-empty area.
    pub fn bounds(&self) -> Option<MapBounds> {
        let (min, max) = (self.map_min?, self.map_max?);
        (max[0] > min[0] && max[1] > min[1] && min.iter().chain(&max).all(|v| v.is_finite())).then_some(MapBounds { min, max })
    }
}

/// The minimap's extent in metres; converts its normalised coordinates to the scene. See the module
/// docs for the convention.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapBounds {
    /// `[west, south]`
    pub min: [f64; 2],
    /// `[east, north]`
    pub max: [f64; 2],
}

impl MapBounds {
    pub fn size(&self) -> [f64; 2] {
        [self.max[0] - self.min[0], self.max[1] - self.min[1]]
    }

    /// Normalised map coordinates (`x` right, `y` down) to `[east, north]` metres.
    pub fn to_east_north(&self, [x, y]: [f64; 2]) -> [f64; 2] {
        let [w, h] = self.size();
        [self.min[0] + x * w, self.max[1] - y * h]
    }

    /// Normalised map coordinates to a scene position: glTF axes, +Y up, map north on +Z, +X west.
    pub fn to_scene(&self, xy: [f64; 2], altitude: f64) -> [f64; 3] {
        let [east, north] = self.to_east_north(xy);
        [-east, altitude, north]
    }

    /// A direction in normalised map units (`dx` right, `dy` down) to a compass heading in degrees,
    /// scaling by the map's size in case it is not square.
    pub fn heading_deg(&self, [dx, dy]: [f64; 2]) -> Option<f64> {
        let [w, h] = self.size();
        let (east, north) = (dx * w, -dy * h);
        (east != 0.0 || north != 0.0).then(|| east.atan2(north).to_degrees().rem_euclid(360.0))
    }
}

/// Unit vector of a compass heading in the scene: `[-sin h, 0, cos h]`.
pub fn heading_to_scene_dir(heading_deg: f64) -> [f64; 3] {
    let h = heading_deg.to_radians();
    [-h.sin(), 0.0, h.cos()]
}

/// Rotation about +Y, in radians, that turns a model facing +Z (glTF forward) to a compass heading.
pub fn heading_to_yaw(heading_deg: f64) -> f64 {
    -heading_deg.to_radians()
}

/// One entry of `/map_obj.json`: something drawn on the player's minimap.
#[derive(Clone, Debug, Default)]
pub struct MapObject {
    /// `"type"`: `aircraft`, `ground_model`, `airfield`, `capture_zone`, `respawn_base_tank`, ...
    pub kind: String,
    /// `"icon"`: `Player` for the player, else a class such as `MediumTank` or `Fighter`
    pub icon: String,
    pub icon_bg: Option<String>,
    /// `"color"`, e.g. `#fa0C00`
    pub color: Option<String>,
    /// `"color[]"`
    pub rgb: Option<[u8; 3]>,
    pub blink: bool,
    /// `"x"`, `"y"`: position, normalised to the map
    pub pos: Option<[f64; 2]>,
    /// `"dx"`, `"dy"`: facing, in normalised map units
    pub dir: Option<[f64; 2]>,
    /// `"sx"`, `"sy"` and `"ex"`, `"ey"`: ends of a segment (airfield runways)
    pub start: Option<[f64; 2]>,
    pub end: Option<[f64; 2]>,
    pub raw: Map<String, Value>,
}

impl MapObject {
    pub fn from_json(value: Value) -> Self {
        let raw = object(value);
        let rgb = raw.get("color[]").and_then(Value::as_array).and_then(|a| match a.as_slice() {
            [r, g, b, ..] => Some([r, g, b].map(|c| c.as_f64().unwrap_or(0.0).clamp(0.0, 255.0) as u8)),
            _ => None,
        });
        Self {
            kind: text(&raw, "type").unwrap_or_default(),
            icon: text(&raw, "icon").unwrap_or_default(),
            icon_bg: text(&raw, "icon_bg"),
            color: text(&raw, "color"),
            rgb,
            blink: flag(&raw, "blink"),
            pos: two(&raw, "x", "y"),
            dir: two(&raw, "dx", "dy"),
            start: two(&raw, "sx", "sy"),
            end: two(&raw, "ex", "ey"),
            raw,
        }
    }

    pub fn is_player(&self) -> bool {
        self.icon == "Player"
    }
}

/// Parses `/map_obj.json`. Anything but a list (the game sends one even when empty) gives no objects.
pub fn map_objects_from_json(value: Value) -> Vec<MapObject> {
    match value {
        Value::Array(items) => items.into_iter().filter(Value::is_object).map(MapObject::from_json).collect(),
        _ => Vec::new(),
    }
}

/// Where the game is, as far as the API can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Nothing answers on the port: the game is closed (or the URL is wrong)
    GameNotRunning,
    /// The game answers but reports no vehicle: hangar, loading screen, or spectating
    NotInMatch,
    InMatch,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::GameNotRunning => "game_not_running",
            Self::NotInMatch => "not_in_match",
            Self::InMatch => "in_match",
        }
    }
}

/// One poll of every endpoint. Endpoints fail independently, so any part can be missing; `errors`
/// says why.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub status: Status,
    pub state: Option<State>,
    pub indicators: Option<Indicators>,
    pub map_info: Option<MapInfo>,
    pub objects: Vec<MapObject>,
    /// `endpoint: reason` for each endpoint that could not be read
    pub errors: Vec<String>,
    /// Hiccups that are routine and not worth an error, e.g. `map_obj: empty body`
    pub notes: Vec<String>,
    /// Endpoints that gave nothing usable this poll (errors and notes alike)
    pub failed: Vec<&'static str>,
    /// `/indicators` failed in a way that says nothing about the match (a timeout, a dropped
    /// connection, an empty or cut-off body), so `status` alone may be wrong; [`Poller`] handles it
    pub inconclusive: bool,
    /// Parts the [`Poller`] carried over from an earlier poll to bridge a hiccup
    pub held: Vec<&'static str>,
    /// `map_info` was fetched in this poll rather than reused from the match's first poll
    pub map_info_fresh: bool,
}

/// The player's vehicle, combined from the endpoints.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerVehicle {
    pub army: Option<Army>,
    pub unit: Option<String>,
    /// Ground speed of ground vehicles (negative in reverse), true airspeed of aircraft
    pub speed_kmh: Option<f64>,
    /// Compass heading, degrees clockwise from north
    pub heading_deg: Option<f64>,
    /// Scene position in metres (see the module docs); `None` without the map bounds or the player icon
    pub position: Option<[f64; 3]>,
    pub throttle_pct: Option<f64>,
    /// Gearbox position (ground vehicles)
    pub gear: Option<Gear>,
    /// Landing gear extension (aircraft)
    pub landing_gear_pct: Option<f64>,
}

impl Snapshot {
    /// Builds a snapshot from what each endpoint returned, deciding the status from the `valid` flags.
    pub fn new(state: Option<State>, indicators: Option<Indicators>, map_info: Option<MapInfo>, objects: Vec<MapObject>, errors: Vec<String>) -> Self {
        let answered = state.is_some() || indicators.is_some() || map_info.is_some();
        let valid = state.as_ref().is_some_and(|s| s.valid) || indicators.as_ref().is_some_and(|i| i.valid);
        let status = match (answered, valid) {
            (false, _) => Status::GameNotRunning,
            (true, false) => Status::NotInMatch,
            (true, true) => Status::InMatch,
        };
        let map_info_fresh = map_info.is_some();
        Self { status, state, indicators, map_info, objects, errors, notes: Vec::new(), failed: Vec::new(), inconclusive: false, held: Vec::new(), map_info_fresh }
    }

    pub fn player_object(&self) -> Option<&MapObject> {
        self.objects.iter().find(|o| o.is_player())
    }

    /// The player's vehicle, when in a match.
    pub fn player(&self) -> Option<PlayerVehicle> {
        if self.status != Status::InMatch {
            return None;
        }
        let state = self.state.as_ref().filter(|s| s.valid);
        let ind = self.indicators.as_ref().filter(|i| i.valid);
        let army = ind.and_then(|i| i.army.clone());
        let bounds = self.map_info.as_ref().and_then(MapInfo::bounds);
        let me = self.player_object();
        let map_heading = bounds.zip(me.and_then(|o| o.dir)).and_then(|(b, d)| b.heading_deg(d));
        let air = army == Some(Army::Air);
        Some(PlayerVehicle {
            unit: ind.and_then(|i| i.unit.clone()),
            speed_kmh: if air { state.and_then(|s| s.tas_kmh).or(ind.and_then(|i| i.speed)) } else { ind.and_then(|i| i.speed) },
            heading_deg: ind.and_then(|i| i.compass_deg).map(|h| h.rem_euclid(360.0)).or(map_heading),
            position: bounds.zip(me.and_then(|o| o.pos)).map(|(b, p)| b.to_scene(p, state.and_then(|s| s.altitude_m).unwrap_or(0.0))),
            throttle_pct: state.and_then(|s| s.throttle_pct).or(ind.and_then(|i| i.throttle).map(|t| t * 100.0)),
            gear: if air { None } else { ind.and_then(Indicators::gearbox) },
            landing_gear_pct: if air { state.and_then(|s| s.landing_gear_pct) } else { None },
            army,
        })
    }

    /// A JSON line for logs: status, the combined vehicle, and the raw `/state`, `/indicators` and the
    /// player's own map entry; `map_info` when it was fetched (once per match under the [`Poller`]).
    /// Other minimap objects are left out.
    pub fn to_log_json(&self, t: f64) -> Value {
        let mut out = Map::new();
        out.insert("t".into(), serde_json::json!((t * 1000.0).round() / 1000.0));
        out.insert("status".into(), self.status.label().into());
        if let Some(p) = self.player() {
            out.insert(
                "vehicle".into(),
                serde_json::json!({
                    "army": p.army.as_ref().map(Army::label),
                    "unit": p.unit,
                    "speed_kmh": p.speed_kmh,
                    "heading_deg": p.heading_deg,
                    "position_m": p.position,
                    "throttle_pct": p.throttle_pct,
                    "gear": p.gear.map(|g| g.to_string()),
                    "landing_gear_pct": p.landing_gear_pct,
                }),
            );
        }
        if let Some(s) = &self.state {
            out.insert("state".into(), Value::Object(s.raw.clone()));
        }
        if let Some(i) = &self.indicators {
            out.insert("indicators".into(), Value::Object(i.raw.clone()));
        }
        if let Some(o) = self.player_object() {
            out.insert("player_map_obj".into(), Value::Object(o.raw.clone()));
        }
        if let Some(m) = self.map_info.as_ref().filter(|_| self.map_info_fresh) {
            out.insert("map_info".into(), Value::Object(m.raw.clone()));
        }
        if !self.held.is_empty() {
            out.insert("held".into(), self.held.clone().into());
        }
        if !self.errors.is_empty() {
            out.insert("errors".into(), self.errors.clone().into());
        }
        if !self.notes.is_empty() {
            out.insert("notes".into(), self.notes.clone().into());
        }
        Value::Object(out)
    }
}

impl PlayerVehicle {
    /// One compact line: kind, unit, speed, heading, position, throttle and gear.
    pub fn line(&self) -> String {
        let opt = |v: Option<f64>, f: &dyn Fn(f64) -> String| v.map_or_else(|| "-".to_string(), f);
        let kind = self.army.as_ref().map_or("?", Army::label);
        let unit = self.unit.as_deref().map(|u| u.rsplit('/').next().unwrap_or(u)).unwrap_or("?");
        let pos = self.position.map_or_else(|| "-".to_string(), |[x, y, z]| format!("{x:.1} {y:.1} {z:.1} m"));
        let gear = match (self.gear, self.landing_gear_pct) {
            (Some(g), _) => format!("gear {g}"),
            (None, Some(lg)) => format!("ldg gear {lg:.0}%"),
            (None, None) => "gear -".to_string(),
        };
        format!(
            "{kind} {unit}  {} km/h  hdg {}  pos {pos}  thr {}  {gear}",
            opt(self.speed_kmh, &|v| format!("{v:.1}")),
            opt(self.heading_deg, &|v| format!("{:03}", (v.round() as i64).rem_euclid(360))),
            opt(self.throttle_pct, &|v| format!("{v:.0}%")),
        )
    }
}

/// Why an endpoint could not be read.
#[derive(Debug)]
pub enum FetchError {
    /// No connection (refused): the game is not running
    Unreachable(String),
    /// A timeout or a connection dropped midway: the game is busy, not necessarily gone
    Transient(String),
    /// The server answered with an error status
    Status(u16),
    /// A 200 with an empty body, which the game sends now and then
    Empty,
    /// The body is not JSON
    BadJson(String),
}

impl FetchError {
    /// Whether the failure says nothing about the game's state
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_) | Self::Empty | Self::BadJson(_))
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "unreachable ({e})"),
            Self::Transient(e) => write!(f, "no answer ({e})"),
            Self::Status(code) => write!(f, "HTTP {code}"),
            Self::Empty => write!(f, "empty body"),
            Self::BadJson(e) => write!(f, "bad JSON ({e})"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Refused connections mean no game; anything else (timeouts, connections dropped midway) is a hiccup.
#[cfg(feature = "live")]
fn classify(e: ureq::Error) -> FetchError {
    use std::io::ErrorKind;
    let refused = match &e {
        ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => true,
        ureq::Error::Io(io) => matches!(io.kind(), ErrorKind::ConnectionRefused | ErrorKind::AddrNotAvailable),
        _ => false,
    };
    if refused {
        FetchError::Unreachable(e.to_string())
    } else {
        FetchError::Transient(e.to_string())
    }
}

/// A blocking client for the telemetry API.
#[cfg(feature = "live")]
pub struct Client {
    base: String,
    agent: ureq::Agent,
}

#[cfg(feature = "live")]
impl Client {
    /// `base` is e.g. [`DEFAULT_URL`]; `timeout` bounds each request, so a hung game cannot stall the
    /// poller.
    pub fn new(base: &str, timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build();
        Self { base: base.trim_end_matches('/').to_string(), agent: ureq::Agent::new_with_config(config) }
    }

    fn get(&self, path: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
        let url = format!("{}{path}", self.base);
        let mut resp = self.agent.get(&url).call().map_err(classify)?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(FetchError::Status(status));
        }
        resp.body_mut().with_config().limit(limit).read_to_vec().map_err(classify)
    }

    /// GET a JSON endpoint, e.g. `/state`.
    pub fn json(&self, path: &str) -> Result<Value, FetchError> {
        let body = self.get(path, 4 << 20)?;
        if body.iter().all(u8::is_ascii_whitespace) {
            return Err(FetchError::Empty);
        }
        serde_json::from_slice(&body).map_err(|e| FetchError::BadJson(e.to_string()))
    }

    pub fn state(&self) -> Result<State, FetchError> {
        self.json("/state").map(State::from_json)
    }

    pub fn indicators(&self) -> Result<Indicators, FetchError> {
        self.json("/indicators").map(Indicators::from_json)
    }

    pub fn map_info(&self) -> Result<MapInfo, FetchError> {
        self.json("/map_info.json").map(MapInfo::from_json)
    }

    pub fn map_objects(&self) -> Result<Vec<MapObject>, FetchError> {
        self.json("/map_obj.json").map(map_objects_from_json)
    }

    /// The minimap image as served (an encoded picture; JPEG in community reports).
    pub fn map_image(&self, generation: Option<i64>) -> Result<Vec<u8>, FetchError> {
        let path = generation.map_or_else(|| "/map.img".to_string(), |g| format!("/map.img?gen={g}"));
        self.get(&path, 64 << 20)
    }

    /// Reads every endpoint once. `map_info` can be passed in to skip refetching it, since it only
    /// changes between matches. If `/indicators` refuses the connection the game is taken as closed
    /// and the other endpoints are not tried. The map endpoints are read in a match, or when
    /// `/indicators` hiccuped but the server still answers.
    pub fn snapshot(&self, map_info: Option<&MapInfo>) -> Snapshot {
        #[derive(Default)]
        struct Outcome {
            errors: Vec<String>,
            notes: Vec<String>,
            failed: Vec<&'static str>,
        }
        impl Outcome {
            fn keep<T>(&mut self, name: &'static str, r: Result<T, FetchError>) -> Option<T> {
                match r {
                    Ok(v) => Some(v),
                    Err(e) => {
                        let line = format!("{name}: {e}");
                        if matches!(e, FetchError::Empty) { self.notes.push(line) } else { self.errors.push(line) }
                        self.failed.push(name);
                        None
                    }
                }
            }
        }
        let mut out = Outcome::default();
        let indicators_result = self.indicators();
        let unreachable = matches!(indicators_result, Err(FetchError::Unreachable(_)));
        let inconclusive = indicators_result.as_ref().err().is_some_and(FetchError::is_transient);
        let indicators = out.keep("indicators", indicators_result);
        let (mut state, mut info, mut objects, mut fresh) = (None, None, Vec::new(), false);
        if !unreachable {
            state = out.keep("state", self.state());
            let in_match = state.as_ref().is_some_and(|s| s.valid) || indicators.as_ref().is_some_and(|i| i.valid);
            if in_match || (inconclusive && state.is_some()) {
                info = match map_info {
                    Some(m) => Some(m.clone()),
                    None => {
                        fresh = true;
                        out.keep("map_info", self.map_info())
                    }
                };
                objects = out.keep("map_obj", self.map_objects()).unwrap_or_default();
            }
        }
        let mut snap = Snapshot::new(state, indicators, info, objects, out.errors);
        snap.notes = out.notes;
        snap.failed = out.failed;
        snap.inconclusive = inconclusive;
        snap.map_info_fresh = fresh && snap.map_info.is_some();
        snap
    }
}

/// Polls the API at a fixed rate, as an iterator of snapshots. It slows to `idle_interval` while the
/// game is closed or out of a match, and caches `/map_info.json` for the length of a match.
///
/// In a match it bridges hiccups for up to `grace`: when `/indicators` times out or `/map_obj.json`
/// comes back empty, the last good answer stands in (listed in [`Snapshot::held`]) and the status stays
/// [`Status::InMatch`]. A refused connection or an answer of `{"valid": false}` ends the match at once.
#[cfg(feature = "live")]
pub struct Poller {
    pub client: Client,
    pub interval: Duration,
    pub idle_interval: Duration,
    pub grace: Duration,
    map_info: Option<MapInfo>,
    good_indicators: Option<(std::time::Instant, Indicators, Option<State>)>,
    good_objects: Option<(std::time::Instant, Vec<MapObject>)>,
    next: Option<std::time::Instant>,
}

#[cfg(feature = "live")]
impl Poller {
    /// `rate_hz` polls per second in a match.
    pub fn new(client: Client, rate_hz: f64) -> Self {
        let interval = Duration::from_secs_f64(1.0 / rate_hz.clamp(0.1, 100.0));
        Self {
            client,
            interval,
            idle_interval: interval.max(Duration::from_secs(1)),
            grace: Duration::from_secs(2),
            map_info: None,
            good_indicators: None,
            good_objects: None,
            next: None,
        }
    }

    /// Takes one snapshot now, without waiting.
    pub fn poll(&mut self) -> Snapshot {
        let mut snap = self.client.snapshot(self.map_info.as_ref().filter(|m| m.bounds().is_some()));
        let now = std::time::Instant::now();
        let grace = self.grace;
        let recent = |at: &std::time::Instant| now.duration_since(*at) <= grace;
        if snap.inconclusive && snap.indicators.is_none() {
            if let Some((_, ind, state)) = self.good_indicators.as_ref().filter(|g| recent(&g.0)) {
                snap.indicators = Some(ind.clone());
                snap.held.push("indicators");
                if snap.state.as_ref().is_none_or(|s| !s.valid) && state.is_some() {
                    snap.state = state.clone();
                    snap.held.push("state");
                }
                snap.status = Status::InMatch;
            }
        }
        if snap.status != Status::InMatch {
            self.map_info = None;
            self.good_indicators = None;
            self.good_objects = None;
            return snap;
        }
        if snap.map_info.is_none() {
            snap.map_info = self.map_info.clone();
        }
        if snap.failed.contains(&"map_obj") {
            if let Some((_, objects)) = self.good_objects.as_ref().filter(|g| recent(&g.0)) {
                snap.objects = objects.clone();
                snap.held.push("map_obj");
            }
        } else if snap.player_object().is_some() {
            self.good_objects = Some((now, snap.objects.clone()));
        }
        if !snap.held.contains(&"indicators") {
            if let Some(ind) = snap.indicators.clone().filter(|i| i.valid) {
                self.good_indicators = Some((now, ind, snap.state.clone().filter(|s| s.valid)));
            }
        }
        if snap.map_info.as_ref().is_some_and(|m| m.bounds().is_some()) {
            self.map_info = snap.map_info.clone();
        }
        snap
    }
}

#[cfg(feature = "live")]
impl Iterator for Poller {
    type Item = Snapshot;

    fn next(&mut self) -> Option<Snapshot> {
        if let Some(at) = self.next {
            std::thread::sleep(at.saturating_duration_since(std::time::Instant::now()));
        }
        let start = std::time::Instant::now();
        let snap = self.poll();
        let wait = if snap.status == Status::InMatch { self.interval } else { self.idle_interval };
        self.next = Some(start + wait);
        Some(snap)
    }
}

/// A stand-in for the game's server, for tests and demos: serves canned or computed JSON on a local
/// port. The sample JSON is written by hand in the shape the community clients document; it holds no
/// game data.
pub mod mock {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    pub const TANK_STATE: &str = include_str!("../samples/live/tank_state.json");
    pub const TANK_INDICATORS: &str = include_str!("../samples/live/tank_indicators.json");
    pub const AIR_STATE: &str = include_str!("../samples/live/air_state.json");
    pub const AIR_INDICATORS: &str = include_str!("../samples/live/air_indicators.json");
    pub const MAP_INFO: &str = include_str!("../samples/live/map_info.json");
    pub const MAP_OBJ: &str = include_str!("../samples/live/map_obj.json");
    /// What every endpoint answers in the hangar
    pub const NOT_IN_MATCH: &str = r#"{"valid": false}"#;

    /// A response: status code, content type and body.
    pub type Response = (u16, &'static str, Vec<u8>);

    pub fn json(body: &str) -> Option<Response> {
        Some((200, "application/json", body.as_bytes().to_vec()))
    }

    type Handler = dyn Fn(&str) -> Option<Response> + Send + Sync;

    /// A small HTTP/1.1 server on `127.0.0.1` at a free port. The handler gets the request path without
    /// the query and returns `None` for 404. It stops when dropped.
    pub struct MockServer {
        pub addr: SocketAddr,
        handler: Arc<Mutex<Arc<Handler>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl MockServer {
        pub fn start(handler: impl Fn(&str) -> Option<Response> + Send + Sync + 'static) -> std::io::Result<Self> {
            Self::bind("127.0.0.1:0", handler)
        }

        pub fn bind(addr: &str, handler: impl Fn(&str) -> Option<Response> + Send + Sync + 'static) -> std::io::Result<Self> {
            let listener = TcpListener::bind(addr)?;
            let addr = listener.local_addr()?;
            let handler: Arc<Mutex<Arc<Handler>>> = Arc::new(Mutex::new(Arc::new(handler)));
            let stop = Arc::new(AtomicBool::new(false));
            let (h, s) = (handler.clone(), stop.clone());
            let thread = std::thread::spawn(move || {
                for conn in listener.incoming() {
                    if s.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(conn) = conn else { continue };
                    let handler = h.lock().unwrap().clone();
                    std::thread::spawn(move || {
                        let _ = serve(conn, &*handler);
                    });
                }
            });
            Ok(Self { addr, handler, stop, thread: Some(thread) })
        }

        pub fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        /// Swaps the handler, e.g. to go from a match back to the hangar.
        pub fn set_handler(&self, handler: impl Fn(&str) -> Option<Response> + Send + Sync + 'static) {
            *self.handler.lock().unwrap() = Arc::new(handler);
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Wake the accept loop so it sees the flag
            let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn serve(mut conn: TcpStream, handler: &Handler) -> std::io::Result<()> {
        conn.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut reader = BufReader::new(conn.try_clone()?);
        let mut request = String::new();
        reader.read_line(&mut request)?;
        // Skip the headers; requests are GETs without a body
        let mut line = String::new();
        while reader.read_line(&mut line)? > 2 {
            line.clear();
        }
        let target = request.split_whitespace().nth(1).unwrap_or("/");
        let path = target.split('?').next().unwrap_or(target);
        let (code, ctype, body) = handler(path).unwrap_or((404, "text/plain", b"not found".to_vec()));
        let reason = if code == 200 { "OK" } else { "Error" };
        write!(conn, "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
        conn.write_all(&body)?;
        conn.flush()
    }

    /// The samples as recorded: a tank in a match.
    pub fn tank_match(path: &str) -> Option<Response> {
        match path {
            "/state" => json(TANK_STATE),
            "/indicators" => json(TANK_INDICATORS),
            "/map_info.json" => json(MAP_INFO),
            "/map_obj.json" => json(MAP_OBJ),
            _ => None,
        }
    }

    /// The hangar: the server is up, nothing is valid.
    pub fn hangar(path: &str) -> Option<Response> {
        match path {
            "/state" | "/indicators" | "/map_info.json" => json(NOT_IN_MATCH),
            "/map_obj.json" => json("[]"),
            _ => None,
        }
    }

    /// A demo: the sample tank drives a 300 m circle at 30 km/h around the map centre, changing gear
    /// with speed. Everything is computed from the clock, so each request sees a new position.
    pub fn driving_demo() -> impl Fn(&str) -> Option<Response> + Send + Sync + 'static {
        let start = Instant::now();
        let info: serde_json::Value = serde_json::from_str(MAP_INFO).expect("sample map_info parses");
        let size = info["map_max"][0].as_f64().unwrap_or(2048.0) - info["map_min"][0].as_f64().unwrap_or(-2048.0);
        move |path| {
            let t = start.elapsed().as_secs_f64();
            let (radius, speed_ms) = (300.0, 30.0 / 3.6);
            // Counter-clockwise seen from above, starting due east of the centre and heading north
            let a = speed_ms * t / radius;
            let (east, north) = (radius * a.cos(), radius * a.sin());
            let (dx, dy) = (-a.sin(), -a.cos());
            match path {
                "/indicators" => {
                    let mut v: serde_json::Value = serde_json::from_str(TANK_INDICATORS).ok()?;
                    let ramp = (t / 4.0).min(1.0);
                    v["speed"] = (30.0 * ramp).into();
                    // gear_neutral is 1 in the sample, so 2..4 show as first to third
                    v["gear"] = (if ramp < 0.3 { 2.0 } else if ramp < 0.7 { 3.0 } else { 4.0 }).into();
                    v["rpm"] = (900.0 + 1500.0 * ramp).into();
                    json(&v.to_string())
                }
                "/map_obj.json" => {
                    let mut v: serde_json::Value = serde_json::from_str(MAP_OBJ).ok()?;
                    if let Some(me) = v.as_array_mut()?.iter_mut().find(|o| o["icon"] == "Player") {
                        me["x"] = (0.5 + east / size).into();
                        me["y"] = (0.5 - north / size).into();
                        me["dx"] = dx.into();
                        me["dy"] = dy.into();
                    }
                    json(&v.to_string())
                }
                other => tank_match(other),
            }
        }
    }
}
