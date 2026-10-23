//! Opens a War Thunder vehicle straight from the game files in a Bevy window.
//!
//! Usage: `wt-viewer [pack.grp model_name] [--game <game folder>] [--unit <unit>] [--screenshot <file.png>]
//! [--turret <deg>] [--gun <deg>] [--orbit <yaw,pitch,distance>] [--target <x,y,z>] [--drive] [--auto <script>]
//! [--lift <node=metres>]`
//!
//! Drag with the mouse to orbit, scroll to zoom. Q/E traverse the turret and R/F elevate the gun, within
//! the limits of the tank's unit BLK (found by its model name, or named with `--unit`). `--drive` puts
//! the tank on flat ground to drive with WASD (see `drive.rs`); `--auto W:20,WA:3` drives by script.
//! `--turret`/`--gun` set the starting aim in degrees (positive: left and up), `--orbit` the camera angles
//! in degrees and distance in metres, and `--target` the point it looks at (model space, metres). With
//! `--screenshot` the window saves one frame and closes (after the script, if there is one), which is
//! handy for checking a model without looking at the screen.
//!
//! The wheels and tracks move by `wt_unpack::gear`: wheels turn by the distance their track travels and the
//! track texture runs at the rate of the track mesh, while the track mesh itself is skinned with the
//! game's own bone table, so it follows its skeleton nodes (`root` and the suspension wheel nodes).
//! `--lift bone_wheel_l_03=0.15` raises a node by that many metres, to see the skin follow it; the game
//! moves those nodes with a suspension simulation that the viewer does not have.

mod drive;

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::math::Affine2;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use wt_unpack::gear::{GearState, RunningGear};
use wt_unpack::tank::{self, Tank};
use wt_unpack::vehicle::{self, Vehicle};

const DEFAULT_GAME: &str = "/Applications/WarThunderLauncher.app/Contents/WarThunder.app/Contents/Resources/game";

/// The vehicle loaded before the window opens, consumed by the startup system
#[derive(Resource)]
struct Loaded(Mutex<Option<Vehicle>>);

/// What the unit BLK says about the vehicle, if a unit uses it
#[derive(Resource)]
struct Spec(Option<Tank>);

/// Where to save a single frame before exiting
#[derive(Resource)]
struct ScreenshotTo(PathBuf);

/// Screenshots are rendered off screen: a window that other windows cover stops being drawn on macOS,
/// and a capture of it after a few seconds comes out black
#[derive(Resource)]
struct ShotImage(Handle<Image>);

const SHOT_SIZE: (u32, u32) = (2560, 1440);

/// Orbit camera state: angles in radians, distance in metres. When following, the target and the yaw
/// move with the vehicle.
#[derive(Resource)]
struct Orbit {
    target: Vec3,
    yaw: f32,
    pitch: f32,
    distance: f32,
    follow: bool,
}

/// Camera placement asked for on the command line, applied once the vehicle's size is known
#[derive(Resource, Default)]
struct CameraRequest {
    orbit: Option<[f32; 3]>,
    target: Option<Vec3>,
}

/// Turret traverse and gun elevation, degrees: positive is to the left and up
#[derive(Resource, Default)]
struct Aim {
    yaw: f32,
    pitch: f32,
}

/// The entity that places the whole vehicle in the world
#[derive(Component)]
struct VehicleRoot;

#[derive(Component)]
struct TurretHead;

#[derive(Component)]
struct GunMount;

/// A wheel that rolls with its side's track: an index into the running gear's wheels
#[derive(Component)]
struct Wheel(usize);

/// A track whose texture runs with its speed: an index into the running gear's belts. Each track has its
/// own copy of the material.
#[derive(Component)]
struct Track {
    belt: usize,
    material: Handle<StandardMaterial>,
}

/// The vehicle's wheels and belts, from the game's data, and where they are now
#[derive(Resource)]
struct Gear {
    gear: RunningGear,
    state: GearState,
}

/// Nodes to raise, from `--lift`
#[derive(Resource, Default)]
struct Lift(Vec<(String, f32)>);

/// How far each track has moved this frame, m (set by the drive mode)
#[derive(Resource, Default)]
struct TrackTravel {
    left: f32,
    right: f32,
}

/// The vehicle's lowest point in model space, and the distance between the track centres
#[derive(Resource)]
struct Bounds {
    lo: Vec3,
    gauge: f32,
}

/// `N` comma-separated numbers
fn floats<const N: usize>(s: &str) -> Result<[f32; N]> {
    let v: Vec<f32> = s.split(',').map(|x| x.trim().parse::<f32>()).collect::<Result<_, _>>().map_err(|e| anyhow!("{s}: {e}"))?;
    v.try_into().map_err(|_| anyhow!("expected {N} comma-separated numbers: {s}"))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let option = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let game = option("--game").map_or_else(|| PathBuf::from(DEFAULT_GAME), PathBuf::from);
    let screenshot = option("--screenshot").map(PathBuf::from);
    // Flags that take no value
    const SWITCHES: [&str; 1] = ["--drive"];
    let driving = args.iter().any(|a| a == "--drive");
    // Positional arguments are everything that is neither a flag nor a flag's value
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if SWITCHES.contains(&args[i].as_str()) {
            i += 1;
        } else if args[i].starts_with("--") {
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
    let script = option("--auto").map(|s| tank::parse_script(&s)).transpose()?;
    let lift = Lift(
        option("--lift")
            .map(|s| {
                s.split(',')
                    .map(|part| {
                        let (node, metres) = part.split_once('=').ok_or_else(|| anyhow!("--lift expects node=metres: {part}"))?;
                        Ok((node.trim().to_string(), metres.trim().parse::<f32>()?))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default(),
    );
    let camera = CameraRequest {
        orbit: option("--orbit").map(|s| floats::<3>(&s)).transpose()?,
        target: option("--target").map(|s| floats::<3>(&s).map(Vec3::from)).transpose()?,
    };

    eprintln!("loading {name} from {}", pack.display());
    let vehicle = vehicle::load(&game, &pack, &name, 0).map_err(|e| e.context(format!("loading {name}")))?;
    let spec = tank::load(&game, &name, option("--unit").as_deref()).unwrap_or_else(|e| {
        eprintln!("warning: no unit BLK: {e:#}");
        None
    });
    match &spec {
        Some(t) => eprintln!("unit {}: turret {:?}, drive {:?}", t.unit, t.turret, t.drive),
        None => eprintln!("no unit uses {name}; turret limits and driving are not available"),
    }

    // Starting aim, within the limits
    let turret = spec.as_ref().and_then(|t| t.turret.as_ref());
    let degrees = |flag: &str| option(flag).map_or(Ok(0.0), |s| floats::<1>(&s).map(|[x]| x));
    let mut aim = Aim { yaw: degrees("--turret")?, pitch: 0.0 };
    let pitch = degrees("--gun")?;
    if let Some(t) = turret {
        aim.yaw = t.clamp_yaw(aim.yaw);
        aim.pitch = t.clamp_pitch(aim.yaw, pitch);
    } else {
        aim.pitch = pitch;
    }

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window { title: format!("wt-viewer — {name}"), ..default() }),
        ..default()
    }));
    if driving {
        let Some(d) = spec.as_ref().and_then(|t| t.drive.clone()) else { bail!("--drive needs the unit's VehiclePhys block") };
        app.insert_resource(drive::Driving::new(d, script.unwrap_or_default()));
        app.add_plugins(drive::DrivePlugin);
    }
    if let Some(path) = screenshot {
        app.insert_resource(ScreenshotTo(path)).add_systems(Update, screenshot_and_exit);
    }
    app
        .insert_resource(Loaded(Mutex::new(Some(vehicle))))
        .insert_resource(Spec(spec))
        .insert_resource(aim)
        .insert_resource(camera)
        .insert_resource(lift)
        .init_resource::<TrackTravel>()
        .insert_resource(ClearColor(Color::srgb(0.11, 0.12, 0.14)))
        .insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 600.0, affects_lightmapped_meshes: true })
        .add_systems(Startup, spawn_vehicle)
        .add_systems(Update, (aim_turret, roll_running_gear, orbit_camera).chain())
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
    spec: Res<Spec>,
    request: Res<CameraRequest>,
    lift: Res<Lift>,
    driving: Option<Res<drive::Driving>>,
    screenshot: Option<Res<ScreenshotTo>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut inverse_bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
) {
    let vehicle = loaded.0.lock().unwrap().take().expect("vehicle is spawned once");
    let turret = spec.0.as_ref().and_then(|t| t.turret.as_ref());
    let drive = spec.0.as_ref().and_then(|t| t.drive.as_ref());

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
                alpha_mode: match m.alpha {
                    wt_unpack::vehicle::Alpha::Opaque => AlphaMode::Opaque,
                    wt_unpack::vehicle::Alpha::Mask(cutoff) => AlphaMode::Mask(cutoff),
                    wt_unpack::vehicle::Alpha::Blend => AlphaMode::Blend,
                },
                ..default()
            };
            if let (Some(mr), Some(n)) = (&m.metal_rough, &m.normal) {
                mat.metallic_roughness_texture = Some(images.add(to_bevy_image(mr, false)));
                mat.normal_map_texture = Some(images.add(to_bevy_image(n, false)));
                mat.metallic = 1.0;
                mat.perceptual_roughness = 1.0;
            }
            // Glass (vision blocks, lights): see-through and smooth, keeping its tint and normal map
            if m.name.contains("glass") {
                mat.alpha_mode = AlphaMode::Blend;
                mat.base_color = Color::srgba(1.0, 1.0, 1.0, 0.3);
                mat.metallic_roughness_texture = None;
                mat.metallic = 0.0;
                mat.perceptual_roughness = 0.05;
                mat.reflectance = 0.6;
            }
            materials.add(mat)
        })
        .collect();
    for m in &vehicle.materials {
        info!("material {}{}", m.name, if m.name.contains("glass") { " (glass)" } else { "" });
    }

    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in vehicle.meshes.iter().flat_map(|m| &m.positions) {
        lo = lo.min(Vec3::from(*p));
        hi = hi.max(Vec3::from(*p));
    }

    // The vehicle stands on y = 0. Every node is an entity placed at its pivot relative to its parent,
    // without rotation, so turning a node turns its subtree about the pivot in the hull's axes.
    let root = commands.spawn((Name::new(vehicle.name.clone()), VehicleRoot, Transform::from_xyz(0.0, -lo.y, 0.0), Visibility::default())).id();
    let pivot = |i: usize| Vec3::from(vehicle.nodes[i].pivot);
    let mut nodes = Vec::with_capacity(vehicle.nodes.len());
    for (i, n) in vehicle.nodes.iter().enumerate() {
        let (parent, origin) = n.parent.map_or((root, Vec3::ZERO), |p| (nodes[p], pivot(p)));
        let raised = lift.0.iter().filter(|(name, _)| *name == n.name).map(|(_, metres)| *metres).sum::<f32>();
        let at = pivot(i) - origin + Vec3::Y * raised;
        nodes.push(commands.spawn((Name::new(n.name.clone()), Transform::from_translation(at), Visibility::default(), ChildOf(parent))).id());
    }

    // Wheels and belts, from the game's data
    let gear = RunningGear::new(&vehicle, drive);
    for (i, w) in gear.wheels.iter().enumerate() {
        commands.entity(nodes[w.node]).insert(Wheel(i));
    }
    info!("{} wheels, track gauge {:?} m", gear.wheels.len(), gear.gauge);

    for (mesh_index, m) in vehicle.meshes.iter().enumerate() {
        let node_name = vehicle.nodes[m.node].name.as_str();
        let skinned = m.skinned && !m.bones.is_empty() && m.joints.len() == m.positions.len();
        // Rigid parts are relative to their node; skinned vertices stay in the vehicle's space, bound to
        // the bones' nodes
        let at = if skinned { Vec3::ZERO } else { pivot(m.node) };
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.iter().map(|p| (Vec3::from(*p) - at).to_array()).collect::<Vec<_>>())
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals.clone())
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, m.uvs.clone())
            .with_inserted_indices(Indices::U32(m.indices.clone()));
        if skinned {
            mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(m.joints.clone()));
            mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, m.weights.clone());
        }
        // Normal maps need tangents
        if let Err(e) = mesh.generate_tangents() {
            warn!("no tangents for material {}: {e}", m.material);
        }
        let mut material = mats[m.material].clone();
        let parent = if skinned { root } else { nodes[m.node] };
        let mut entity = commands.spawn((Mesh3d(meshes.add(mesh)), ChildOf(parent)));
        if skinned {
            // Each bone's inverse bind matrix undoes its node's place (node frames carry no axes)
            let bindposes = m.bones.iter().map(|&b| Mat4::from_translation(-pivot(b))).collect::<Vec<_>>();
            entity.insert(SkinnedMesh {
                inverse_bindposes: inverse_bindposes.add(SkinnedMeshInverseBindposes::from(bindposes)),
                joints: m.bones.iter().map(|&b| nodes[b]).collect(),
            });
        }
        if let Some(belt) = gear.belts.iter().position(|b| b.mesh == mesh_index) {
            // Its own material, so the two tracks can run at different speeds
            let own = materials.get(&material).cloned().expect("material was just added");
            material = materials.add(own);
            entity.insert(Track { belt, material: material.clone() });
            info!("{node_name}: texture runs {:.3} V per metre", gear.belts[belt].v_per_metre);
        }
        entity.insert(MeshMaterial3d(material));
    }
    for (i, n) in vehicle.nodes.iter().enumerate() {
        let name = n.name.as_str();
        if turret.map_or(name == "bone_turret", |t| t.head == name) {
            commands.entity(nodes[i]).insert(TurretHead);
        }
        if turret.map_or(name == "bone_gun", |t| t.gun == name) {
            commands.entity(nodes[i]).insert(GunMount);
        }
    }
    let glass: Vec<&str> = vehicle.meshes.iter().filter(|m| vehicle.materials[m.material].name.contains("glass")).map(|m| vehicle.nodes[m.node].name.as_str()).collect();
    info!("glass on nodes {glass:?}");
    // Without wheels to measure, a typical gauge
    let gauge = gear.gauge.unwrap_or(2.5);
    commands.insert_resource(Bounds { lo, gauge });
    let state = gear.rest();
    commands.insert_resource(Gear { gear, state });

    let size = (hi - lo).max_element();
    let center = (lo + hi) / 2.0 - Vec3::Y * lo.y;
    let [yaw, pitch, distance] = request.orbit.map_or([0.8, 0.35, size * 1.3], |[y, p, d]| [y.to_radians(), p.to_radians(), d]);
    let target = request.target.map_or(center, |t| t - Vec3::Y * lo.y);
    // Driving: the camera follows from behind unless told otherwise
    let (yaw, follow) = match driving {
        Some(_) if request.orbit.is_none() => (-std::f32::consts::FRAC_PI_2 + 0.5, true),
        Some(_) => (yaw, true),
        None => (yaw, false),
    };
    commands.insert_resource(Orbit { target, yaw, pitch, distance, follow });
    // The HUD (drive mode) goes wherever this camera renders
    let mut camera = commands.spawn((Camera3d::default(), IsDefaultUiCamera, Transform::default()));
    if screenshot.is_some() {
        let target = images.add(Image::new_target_texture(SHOT_SIZE.0, SHOT_SIZE.1, TextureFormat::Rgba8UnormSrgb, None));
        camera.insert(RenderTarget::Image(target.clone().into()));
        commands.insert_resource(ShotImage(target));
    }
    commands.spawn((
        DirectionalLight { illuminance: 9000.0, shadow_maps_enabled: true, ..default() },
        Transform::from_xyz(4.0, 8.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    if driving.is_none() {
        // Ground plane under the tracks
        commands.spawn((
            Mesh3d(meshes.add(Plane3d::default().mesh().size(size * 4.0, size * 4.0))),
            MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.23, 0.24, 0.22), perceptual_roughness: 1.0, ..default() })),
            Transform::from_xyz((lo.x + hi.x) / 2.0, 0.0, (lo.z + hi.z) / 2.0),
        ));
    }
    info!("{}: {} meshes, {} materials, {} nodes", vehicle.name, vehicle.meshes.len(), vehicle.materials.len(), vehicle.nodes.len());
}

/// Q/E traverse the turret and R/F elevate the gun, at the BLK's speeds and within its limits
fn aim_turret(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    spec: Res<Spec>,
    mut aim: ResMut<Aim>,
    mut heads: Query<&mut Transform, (With<TurretHead>, Without<GunMount>)>,
    mut guns: Query<&mut Transform, (With<GunMount>, Without<TurretHead>)>,
) {
    let turret = spec.0.as_ref().and_then(|t| t.turret.as_ref());
    let (speed_yaw, speed_pitch) = turret.map_or((20.0, 5.0), |t| (t.speed_yaw, t.speed_pitch));
    let axis = |plus: KeyCode, minus: KeyCode| f32::from(u8::from(keys.pressed(plus))) - f32::from(u8::from(keys.pressed(minus)));
    let dt = time.delta_secs();
    aim.yaw += axis(KeyCode::KeyQ, KeyCode::KeyE) * speed_yaw * dt;
    aim.pitch += axis(KeyCode::KeyR, KeyCode::KeyF) * speed_pitch * dt;
    if let Some(t) = turret {
        aim.yaw = t.clamp_yaw(aim.yaw);
        aim.pitch = t.clamp_pitch(aim.yaw, aim.pitch);
    }
    // Y is up and Z points right, so turning about +Y goes left and about +Z lifts the muzzle (+X)
    for mut t in &mut heads {
        t.rotation = Quat::from_rotation_y(aim.yaw.to_radians());
    }
    for mut t in &mut guns {
        t.rotation = Quat::from_rotation_z(aim.pitch.to_radians());
    }
}

/// Rolls the wheels and runs the track textures by how far each track moved (see `wt_unpack::gear`)
fn roll_running_gear(
    travel: Res<TrackTravel>,
    mut gear: ResMut<Gear>,
    mut wheels: Query<(&Wheel, &mut Transform)>,
    tracks: Query<&Track>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Gear { gear, state } = &mut *gear;
    gear.advance(state, travel.left, travel.right);
    for (w, mut t) in &mut wheels {
        t.rotation = Quat::from_rotation_z(state.wheel_angles[w.0]);
    }
    for track in &tracks {
        if let Some(mut m) = materials.get_mut(&track.material) {
            m.uv_transform = Affine2::from_translation(Vec2::new(0.0, state.belt_offsets[track.belt]));
        }
    }
}

fn orbit_camera(
    mut orbit: ResMut<Orbit>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    root: Query<&Transform, (With<VehicleRoot>, Without<Camera3d>)>,
    mut camera: Query<&mut Transform, With<Camera3d>>,
) {
    if buttons.pressed(MouseButton::Left) {
        orbit.yaw -= motion.delta.x * 0.005;
        orbit.pitch = (orbit.pitch + motion.delta.y * 0.005).clamp(-1.4, 1.4);
    }
    orbit.distance = (orbit.distance * (1.0 - scroll.delta.y * 0.1)).clamp(1.0, 200.0);
    // A following camera keeps its angle to the vehicle's heading and looks at the vehicle
    let (target, yaw) = match root.single() {
        Ok(r) if orbit.follow => {
            let heading = r.rotation.to_euler(EulerRot::YXZ).0;
            (r.translation + Vec3::Y * 1.5, orbit.yaw + heading)
        }
        _ => (orbit.target, orbit.yaw),
    };
    let offset = Vec3::new(yaw.sin() * orbit.pitch.cos(), orbit.pitch.sin(), yaw.cos() * orbit.pitch.cos()) * orbit.distance;
    for mut t in &mut camera {
        *t = Transform::from_translation(target + offset).looking_at(target, Vec3::Y);
    }
}

/// Waits for textures to upload (and for the drive script to finish), saves one frame, then exits
fn screenshot_and_exit(
    mut commands: Commands,
    target: Res<ScreenshotTo>,
    image: Option<Res<ShotImage>>,
    driving: Option<Res<drive::Driving>>,
    mut frame: Local<u32>,
    mut shot_at: Local<Option<u32>>,
    mut exit: MessageWriter<AppExit>,
) {
    *frame += 1;
    let ready = *frame >= 60 && driving.as_ref().is_none_or(|d| d.script_done());
    if let (true, None, Some(image)) = (ready, *shot_at, image) {
        *shot_at = Some(*frame);
        commands.spawn(Screenshot::image(image.0.clone())).observe(save_to_disk(target.0.clone()));
        if let Some(d) = &driving {
            eprintln!("{}", d.report());
        }
    }
    if shot_at.is_some_and(|f| *frame == f + 60) {
        exit.write(AppExit::Success);
    }
}
