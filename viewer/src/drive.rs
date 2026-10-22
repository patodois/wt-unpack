//! Drive mode: the tank on a flat grid, driven with WASD (or a script), with the camera following and
//! the speed in a corner. The driving model is `wt_unpack::tank::DriveState`, stepped at a fixed rate.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::Affine2;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use wt_unpack::tank::{Controls, Drive, DriveState};

use crate::{Bounds, TrackTravel, VehicleRoot};

/// Physics step, s
const STEP: f32 = 1.0 / 120.0;
/// Side of the ground square, m
const GROUND: f32 = 4000.0;
/// One grid texture covers this many metres, with a line every metre
const GRID_TILE: f32 = 10.0;

pub struct DrivePlugin;

impl Plugin for DrivePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_ground).add_systems(Update, (drive, show_speed).chain().before(crate::roll_running_gear));
    }
}

#[derive(Resource)]
pub struct Driving {
    drive: Drive,
    state: DriveState,
    /// Hull heading, radians (0 faces +X), and position on the ground
    heading: f32,
    position: Vec2,
    /// Scripted input: (controls, seconds), and the time driven so far
    script: Vec<(Controls, f32)>,
    clock: f32,
    unspent: f32,
    top_speed: f32,
}

impl Driving {
    pub fn new(drive: Drive, script: Vec<(Controls, f32)>) -> Self {
        Self { drive, state: DriveState::default(), heading: 0.0, position: Vec2::ZERO, script, clock: 0.0, unspent: 0.0, top_speed: 0.0 }
    }

    fn script_length(&self) -> f32 {
        self.script.iter().map(|s| s.1).sum()
    }

    /// True when the script has run out (or there is none)
    pub fn script_done(&self) -> bool {
        self.clock >= self.script_length()
    }

    /// The scripted controls at the current time
    fn scripted(&self) -> Option<Controls> {
        let mut t = self.clock;
        for (c, secs) in &self.script {
            if t < *secs {
                return Some(*c);
            }
            t -= secs;
        }
        None
    }

    pub fn report(&self) -> String {
        let d = &self.drive;
        format!(
            "drove {:.1} s and {:.0} m; top speed reached {:.1} km/h; gearing allows {:.1} km/h forward, {:.1} km/h reverse; BLK maxFwdSpeed {:?} km/h",
            self.clock,
            self.position.length(),
            self.top_speed * 3.6,
            d.top_speed() * 3.6,
            d.reverse_speed() * 3.6,
            d.max_fwd_speed,
        )
    }
}

#[derive(Component)]
struct SpeedText;

/// A grid texture with mipmaps, so the lines fade out in the distance instead of shimmering
fn grid_image() -> Image {
    const SIZE: usize = 512;
    let cell = SIZE / GRID_TILE as usize;
    let mut levels = vec![Vec::with_capacity(SIZE * SIZE * 4)];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let line = |i: usize| i % cell < 2;
            let edge = |i: usize| i < 4;
            let c: [u8; 3] = if edge(x) || edge(y) {
                [150, 152, 140]
            } else if line(x) || line(y) {
                [105, 108, 98]
            } else {
                [70, 74, 66]
            };
            levels[0].extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    // Box-filtered mip chain down to 1x1
    let mut size = SIZE;
    while size > 1 {
        let prev = levels.last().unwrap();
        let half = size / 2;
        let mut next = Vec::with_capacity(half * half * 4);
        for y in 0..half {
            for x in 0..half {
                for k in 0..4 {
                    let at = |dx: usize, dy: usize| u32::from(prev[((2 * y + dy) * size + 2 * x + dx) * 4 + k]);
                    next.push(((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8);
                }
            }
        }
        levels.push(next);
        size = half;
    }
    let mip_count = levels.len() as u32;
    let mut image = Image::new(
        Extent3d { width: SIZE as u32, height: SIZE as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        levels[0].clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(levels.concat());
    image.texture_descriptor.mip_level_count = mip_count;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 16,
        ..ImageSamplerDescriptor::linear()
    });
    image
}

fn spawn_ground(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>, mut images: ResMut<Assets<Image>>) {
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(GROUND, GROUND))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color_texture: Some(images.add(grid_image())),
            uv_transform: Affine2::from_scale(Vec2::splat(GROUND / GRID_TILE)),
            perceptual_roughness: 1.0,
            ..default()
        })),
        Transform::default(),
    ));
    commands.spawn((
        Text::new(""),
        TextFont { font_size: FontSize::Px(20.0), ..default() },
        Node { position_type: PositionType::Absolute, top: px(12), left: px(12), ..default() },
        SpeedText,
    ));
}

/// Steps the driving model with the keys (or the script) and moves the vehicle
fn drive(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    bounds: Res<Bounds>,
    mut driving: ResMut<Driving>,
    mut travel: ResMut<TrackTravel>,
    mut root: Query<&mut Transform, With<VehicleRoot>>,
) {
    let axis = |plus: KeyCode, minus: KeyCode| f32::from(u8::from(keys.pressed(plus))) - f32::from(u8::from(keys.pressed(minus)));
    let keyboard = Controls { throttle: axis(KeyCode::KeyW, KeyCode::KeyS), steer: axis(KeyCode::KeyA, KeyCode::KeyD) };
    let scripted = !driving.script.is_empty();
    let d = &mut *driving;
    // A slow frame (start-up, texture upload) is not allowed to jump the tank far ahead
    d.unspent += time.delta_secs().min(0.1);
    *travel = TrackTravel::default();
    while d.unspent >= STEP {
        d.unspent -= STEP;
        let controls = if scripted { d.scripted().unwrap_or_default() } else { keyboard };
        d.state.step(&d.drive, bounds.gauge, controls, STEP);
        d.heading += d.state.yaw_rate * STEP;
        let forward = Vec2::new(d.heading.cos(), -d.heading.sin());
        d.position += forward * d.state.speed * STEP;
        let (left, right) = d.state.track_speeds(bounds.gauge);
        travel.left += left * STEP;
        travel.right += right * STEP;
        d.clock += STEP;
        d.top_speed = d.top_speed.max(d.state.speed.abs());
    }
    for mut t in &mut root {
        t.translation = Vec3::new(d.position.x, -bounds.lo.y, d.position.y);
        t.rotation = Quat::from_rotation_y(d.heading);
    }
}

fn show_speed(driving: Res<Driving>, mut text: Query<&mut Text, With<SpeedText>>) {
    let d = &driving;
    for mut t in &mut text {
        t.0 = format!(
            "{:.1} km/h\ntop {:.1} km/h, gearing {:.1} km/h\nWASD drive, Q/E turret, R/F gun",
            d.state.speed * 3.6,
            d.top_speed * 3.6,
            d.drive.top_speed() * 3.6
        );
    }
}
