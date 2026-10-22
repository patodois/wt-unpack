//! Opens a War Thunder vehicle straight from the game files in a Bevy window.
//!
//! Usage: `wt-viewer [pack.grp model_name] [--game <game folder>] [--screenshot <file.png>]`
//! Drag with the mouse to orbit, scroll to zoom. With `--screenshot` the window saves one frame and
//! closes, which is handy for checking a model without looking at the screen.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use wt_unpack::vehicle::{self, Vehicle};

const DEFAULT_GAME: &str = "/Applications/WarThunderLauncher.app/Contents/WarThunder.app/Contents/Resources/game";

/// The vehicle loaded before the window opens, consumed by the startup system
#[derive(Resource)]
struct Loaded(Mutex<Option<Vehicle>>);

/// Where to save a single frame before exiting
#[derive(Resource)]
struct ScreenshotTo(PathBuf);

/// Orbit camera state: angles in radians, distance in metres
#[derive(Resource)]
struct Orbit {
    target: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let option = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let game = option("--game").map_or_else(|| PathBuf::from(DEFAULT_GAME), PathBuf::from);
    let screenshot = option("--screenshot").map(PathBuf::from);
    // Positional arguments are everything that is neither a flag nor a flag's value
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i].starts_with("--") {
            i += 2;
        } else {
            positional.push(&args[i]);
            i += 1;
        }
    }
    let (pack, name) = match positional.as_slice() {
        [pack, name, ..] => (PathBuf::from(pack.as_str()), name.to_string()),
        _ => (game.join("content/base/res/tanks/usa_m4a1_76w_sherman.grp"), "m4a1_76w_sherman".to_string()),
    };

    eprintln!("loading {name} from {}", pack.display());
    let vehicle = vehicle::load(&game, &pack, &name, 0).map_err(|e| e.context(format!("loading {name}")))?;

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window { title: format!("wt-viewer — {name}"), ..default() }),
        ..default()
    }));
    if let Some(path) = screenshot {
        app.insert_resource(ScreenshotTo(path)).add_systems(Update, screenshot_and_exit);
    }
    app
        .insert_resource(Loaded(Mutex::new(Some(vehicle))))
        .insert_resource(ClearColor(Color::srgb(0.11, 0.12, 0.14)))
        .insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 600.0, affects_lightmapped_meshes: true })
        .add_systems(Startup, spawn_vehicle)
        .add_systems(Update, orbit_camera)
        .run();
    Ok(())
}

fn to_bevy_image(img: &vehicle::Image, srgb: bool) -> Image {
    let format = if srgb { TextureFormat::Rgba8UnormSrgb } else { TextureFormat::Rgba8Unorm };
    let mut image = Image::new(
        Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        img.rgba.clone(),
        format,
        RenderAssetUsages::RENDER_WORLD,
    );
    // Dagor's UVs tile beyond 0..1
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        anisotropy_clamp: 8,
        ..ImageSamplerDescriptor::linear()
    });
    image
}

fn spawn_vehicle(
    mut commands: Commands,
    loaded: Res<Loaded>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let vehicle = loaded.0.lock().unwrap().take().expect("vehicle is spawned once");

    let mats: Vec<Handle<StandardMaterial>> = vehicle
        .materials
        .iter()
        .map(|m| {
            let mut mat = StandardMaterial {
                base_color_texture: m.color.as_ref().map(|i| images.add(to_bevy_image(i, true))),
                perceptual_roughness: 0.85,
                metallic: 0.0,
                double_sided: m.double_sided,
                cull_mode: if m.double_sided { None } else { Some(bevy::render::render_resource::Face::Back) },
                alpha_mode: if m.alpha_mask { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque },
                ..default()
            };
            if let (Some(mr), Some(n)) = (&m.metal_rough, &m.normal) {
                mat.metallic_roughness_texture = Some(images.add(to_bevy_image(mr, false)));
                mat.normal_map_texture = Some(images.add(to_bevy_image(n, false)));
                mat.metallic = 1.0;
                mat.perceptual_roughness = 1.0;
            }
            materials.add(mat)
        })
        .collect();

    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for m in &vehicle.meshes {
        for p in &m.positions {
            lo = lo.min(Vec3::from(*p));
            hi = hi.max(Vec3::from(*p));
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.clone())
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals.clone())
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, m.uvs.clone())
            .with_inserted_indices(Indices::U32(m.indices.clone()));
        // Normal maps need tangents
        if let Err(e) = mesh.generate_tangents() {
            warn!("no tangents for material {}: {e}", m.material);
        }
        commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(mats[m.material].clone())));
    }

    let size = (hi - lo).max_element();
    commands.insert_resource(Orbit { target: (lo + hi) / 2.0, yaw: 0.8, pitch: 0.35, distance: size * 1.3 });
    commands.spawn((Camera3d::default(), Transform::default()));
    commands.spawn((
        DirectionalLight { illuminance: 9000.0, shadow_maps_enabled: true, ..default() },
        Transform::from_xyz(4.0, 8.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    // Ground plane under the tracks
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(size * 4.0, size * 4.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.23, 0.24, 0.22), perceptual_roughness: 1.0, ..default() })),
        Transform::from_xyz((lo.x + hi.x) / 2.0, lo.y, (lo.z + hi.z) / 2.0),
    ));
    info!("{}: {} meshes, {} materials", vehicle.name, vehicle.meshes.len(), vehicle.materials.len());
}

fn orbit_camera(
    mut orbit: ResMut<Orbit>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut camera: Query<&mut Transform, With<Camera3d>>,
) {
    if buttons.pressed(MouseButton::Left) {
        orbit.yaw -= motion.delta.x * 0.005;
        orbit.pitch = (orbit.pitch + motion.delta.y * 0.005).clamp(-1.4, 1.4);
    }
    orbit.distance = (orbit.distance * (1.0 - scroll.delta.y * 0.1)).clamp(1.0, 200.0);
    let offset = Vec3::new(orbit.yaw.sin() * orbit.pitch.cos(), orbit.pitch.sin(), orbit.yaw.cos() * orbit.pitch.cos()) * orbit.distance;
    for mut t in &mut camera {
        *t = Transform::from_translation(orbit.target + offset).looking_at(orbit.target, Vec3::Y);
    }
}

/// Waits for textures to upload, saves one frame, then exits
fn screenshot_and_exit(
    mut commands: Commands,
    target: Res<ScreenshotTo>,
    mut frame: Local<u32>,
    mut exit: MessageWriter<AppExit>,
) {
    *frame += 1;
    if *frame == 60 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(target.0.clone()));
    }
    if *frame == 120 {
        exit.write(AppExit::Success);
    }
}
