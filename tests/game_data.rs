//! Tests against an installed copy of War Thunder. They are skipped unless `WT_GAME_DIR` points at the
//! game's `Resources/game` folder, e.g.
//! `WT_GAME_DIR=/Applications/WarThunderLauncher.app/Contents/WarThunder.app/Contents/Resources/game cargo test`.
//! No game data is stored in this repository.

use std::fs;
use std::path::PathBuf;

use wt_unpack::landclass::{self, GroundMaterial, LandClassKind};
use wt_unpack::level::Level;
use wt_unpack::rendinst::{is_facing_leaves, model_descs_where, ResourceIndex, RiModel, LAND_CLASS, REND_INST};
use wt_unpack::sound::{self, Codec, Fsb5};
use wt_unpack::terrain::HeightGrid;
use wt_unpack::collision::{Collision, NodeKind, COLLISION};
use wt_unpack::gear::{RadiusSource, RunningGear, Side, WheelKind};
use wt_unpack::rig::VehicleRig;
use wt_unpack::tank::Controls;
use wt_unpack::{blk, gltf, tank, vehicle, vromfs::Vromfs};

fn game() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("WT_GAME_DIR")?);
    assert!(dir.join("aces.vromfs.bin").exists(), "WT_GAME_DIR does not look like the game folder");
    Some(dir)
}

#[test]
fn data_archives_unpack_and_their_blks_parse() {
    let Some(game) = game() else { return };
    for name in ["char.vromfs.bin", "game.vromfs.bin", "wwdata.vromfs.bin"] {
        let vfs = Vromfs::parse(&fs::read(game.join(name)).unwrap()).unwrap();
        assert!(vfs.md5_ok, "{name}: md5 mismatch");
        assert!(!vfs.files.is_empty(), "{name}: no files");

        let shared = vfs.get(wt_unpack::vromfs::SHARED_NAMEMAP_NAME).map(|nm| {
            let (mut ctx, dict) = blk::SharedContext::from_namemap(&nm.data).unwrap();
            ctx.dict = vfs.get(&dict).map(|f| f.data.clone());
            ctx
        });
        let mut parsed = 0;
        for f in vfs.files.iter().filter(|f| f.name.ends_with(".blk") && blk::is_binary(&f.data)) {
            blk::parse(&f.data, shared.as_ref()).unwrap_or_else(|e| panic!("{name}:{}: {e:#}", f.name));
            parsed += 1;
        }
        assert!(parsed > 0, "{name}: no binary BLKs");
    }
}

#[test]
fn dyn_model_desc_has_the_sherman() {
    let Some(game) = game() else { return };
    let root = blk::parse(&fs::read(game.join("content/base/res/dynModelDesc.bin")).unwrap(), None).unwrap();
    let sherman = root.blocks.iter().find(|b| b.name == "m4a1_76w_sherman").unwrap();
    assert!(sherman.blocks.iter().any(|b| b.name == "matR"));
    assert!(sherman.blocks.iter().any(|b| b.name == "tex"));
}

#[test]
fn sherman_assembles_and_exports() {
    let Some(game) = game() else { return };
    let pack = game.join("content/base/res/tanks/usa_m4a1_76w_sherman.grp");
    let v = vehicle::load(&game, &pack, "m4a1_76w_sherman", 0).unwrap();

    // 126,900 rigid triangles (480 of them glass) plus 5,272 in the tracks and the flexible antenna
    let triangles: usize = v.meshes.iter().map(|m| m.indices.len() / 3).sum();
    assert_eq!(triangles, 132_172);
    assert_eq!(v.materials.len(), 20);
    for m in &v.meshes {
        assert_eq!(m.positions.len(), m.normals.len());
        assert_eq!(m.positions.len(), m.uvs.len());
        assert!(m.indices.iter().all(|&i| (i as usize) < m.positions.len()));
        assert!(m.material < v.materials.len());
    }
    // Roughly Sherman-sized: about 7 m long with the gun, under 4 m tall with the antenna
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in v.meshes.iter().flat_map(|m| &m.positions) {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let size: Vec<f32> = (0..3).map(|k| hi[k] - lo[k]).collect();
    assert!((6.5..7.5).contains(&size[0]) && (3.0..4.0).contains(&size[1]) && (2.5..3.0).contains(&size[2]), "{size:?}");
    // The hull's colour texture is found, with the olive camouflage baked in
    let hull = v.materials.iter().find(|m| m.color_key.as_deref().is_some_and(|k| k.contains("body_c__us_camo_olive"))).unwrap();
    assert!(hull.color.is_some() && hull.metal_rough.is_some() && hull.normal.is_some());

    let out = std::env::temp_dir().join(format!("wt-unpack-test-{}", std::process::id()));
    gltf::write(&v, &out).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&fs::read(out.join("m4a1_76w_sherman.gltf")).unwrap()).unwrap();
    assert_eq!(doc["meshes"][0]["primitives"].as_array().unwrap().len(), v.meshes.len());
    for image in doc["images"].as_array().unwrap() {
        assert!(out.join(image["uri"].as_str().unwrap()).exists());
    }
    fs::remove_dir_all(&out).unwrap();
}

#[test]
fn sherman_unit_names_its_turret_nodes_and_drive_train() {
    let Some(game) = game() else { return };
    let t = tank::load(&game, "m4a1_76w_sherman", None).unwrap().expect("a unit uses the Sherman model");
    assert!(t.unit.ends_with("_m4a1_76w_sherman"), "{}", t.unit);
    let turret = t.turret.unwrap();
    assert_eq!((turret.head.as_str(), turret.gun.as_str()), ("bone_turret", "bone_gun"));
    assert_eq!((turret.yaw, turret.pitch), ([-180.0, 180.0], [-10.0, 25.0]));
    // 400 hp, 2400 RPM, top gear 0.733: 38.6 km/h
    let d = t.drive.unwrap();
    assert_eq!((d.horse_powers, d.max_rpm), (400.0, 2400.0));
    assert!((d.top_speed() * 3.6 - 38.6).abs() < 0.1);

    // The skeleton carries the nodes the unit names, the wheels and the tracks
    let pack = game.join("content/base/res/tanks/usa_m4a1_76w_sherman.grp");
    let v = vehicle::load(&game, &pack, "m4a1_76w_sherman", 0).unwrap();
    let node = |name: &str| v.nodes.iter().position(|n| n.name == name).unwrap_or_else(|| panic!("no node {name}"));
    let (head, gun) = (node(&turret.head), node(&turret.gun));
    assert_eq!(v.nodes[gun].parent, Some(head));
    assert!(v.meshes.iter().any(|m| m.node == head) && v.meshes.iter().any(|m| m.node == gun));
    let wheels = v.nodes.iter().enumerate().filter(|(i, n)| n.name.starts_with("wheel_") && v.meshes.iter().any(|m| m.node == *i)).count();
    assert_eq!(wheels, 22);
    for side in ["track_l", "track_r"] {
        let track = v.meshes.iter().find(|m| m.node == node(side)).unwrap();
        assert!(vehicle::track_v_per_metre(track).is_some_and(|r| r.abs() > 0.1));
    }
    // The model has no unit
    assert!(tank::load(&game, "no_such_model", None).unwrap().is_none());
}

/// The Sherman's tracks are skinned with the game's bone table: every vertex follows the `root` node or
/// the suspension wheel node it wraps around, and moving one of those moves the track there
#[test]
fn sherman_track_skins_follow_the_wheel_bones() {
    let Some(game) = game() else { return };
    let pack = game.join("content/base/res/tanks/usa_m4a1_76w_sherman.grp");
    let v = vehicle::load(&game, &pack, "m4a1_76w_sherman", 0).unwrap();
    let node = |name: &str| v.nodes.iter().position(|n| n.name == name).unwrap_or_else(|| panic!("no node {name}"));

    let skinned: Vec<&vehicle::Mesh> = v.meshes.iter().filter(|m| m.skinned).collect();
    assert_eq!(skinned.len(), 3, "antenna and the two tracks");
    for m in &skinned {
        assert_eq!(m.joints.len(), m.positions.len());
        assert_eq!(m.weights.len(), m.positions.len());
        for (j, w) in m.joints.iter().zip(&m.weights) {
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-4, "{w:?}");
            assert!(j.iter().zip(w).all(|(&j, &w)| w == 0.0 || usize::from(j) < m.bones.len()));
        }
    }
    for (track, side) in [("track_l", "l"), ("track_r", "r")] {
        let m = skinned.iter().find(|m| m.node == node(track)).unwrap();
        // The root and the six suspension wheels of the side
        let mut names: Vec<&str> = m.bones.iter().map(|&b| v.nodes[b].name.as_str()).collect();
        names.sort_unstable();
        let wheels: Vec<String> = (1..=6).map(|i| format!("bone_wheel_{side}_0{i}")).collect();
        let mut expected: Vec<&str> = wheels.iter().map(String::as_str).chain(["root"]).collect();
        expected.sort_unstable();
        assert_eq!(names, expected);
        // One influence per vertex, with a real share on each wheel
        assert!(m.weights.iter().all(|w| w[0] > 0.99 && w[1] == 0.0));

        // Lifting a wheel bone 10 cm lifts exactly the vertices bound to it
        let lifted = m.bones.iter().position(|&b| v.nodes[b].name == format!("bone_wheel_{side}_03")).unwrap();
        let mut moves = vec![vehicle::IDENTITY; m.bones.len()];
        moves[lifted][10] = 0.1;
        let moved = m.skinned_positions(&moves);
        let bound = m.joints.iter().filter(|j| usize::from(j[0]) == lifted).count();
        assert!(bound > 50, "{bound}");
        for (i, (a, b)) in m.positions.iter().zip(&moved).enumerate() {
            let dy = b[1] - a[1];
            let expect = if usize::from(m.joints[i][0]) == lifted { 0.1 } else { 0.0 };
            assert!((dy - expect).abs() < 1e-5 && a[0] == b[0] && a[2] == b[2], "vertex {i}: {dy}");
        }
        // The vertices bound to a wheel bone are the ring of the track under that wheel's axle
        let wheel_x = v.nodes[m.bones[lifted]].pivot[0];
        let xs = m.joints.iter().zip(&m.positions).filter(|(j, _)| usize::from(j[0]) == lifted).map(|(_, p)| p[0]);
        let (lo, hi) = xs.fold((f32::MAX, f32::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)));
        assert!((lo - wheel_x).abs() < 0.01 && (hi - wheel_x).abs() < 0.01, "{lo}..{hi} under {wheel_x}");
    }
    // The antenna blends its two bones
    let antenna = skinned.iter().find(|m| m.node == node("antenna")).unwrap();
    let names: Vec<&str> = antenna.bones.iter().map(|&b| v.nodes[b].name.as_str()).collect();
    assert_eq!(names, ["bone_antenna_01", "bone_phx_antenna_01"]);
    assert!(antenna.weights.iter().any(|w| w[1] > 0.0));
}

/// Wheels and belts of the Sherman, with radii from its unit BLK and the track texture rate from its mesh
#[test]
fn sherman_running_gear_comes_from_the_game_data() {
    let Some(game) = game() else { return };
    let pack = game.join("content/base/res/tanks/usa_m4a1_76w_sherman.grp");
    let v = vehicle::load(&game, &pack, "m4a1_76w_sherman", 0).unwrap();
    let t = tank::load(&game, "m4a1_76w_sherman", None).unwrap().unwrap();
    let drive = t.drive.unwrap();
    let gear = RunningGear::new(&v, Some(&drive));
    let mesh_only = RunningGear::new(&v, None);

    // Six road wheels, three return rollers, a sprocket and an idler per side
    assert_eq!(gear.wheels.len(), 22);
    let count = |kind| gear.wheels.iter().filter(|w| w.kind == kind).count();
    assert_eq!((count(WheelKind::Road), count(WheelKind::Top), count(WheelKind::Drive), count(WheelKind::End)), (12, 6, 2, 2));
    assert!(gear.wheels.iter().all(|w| w.radius_source == RadiusSource::Unit));
    for (w, m) in gear.wheels.iter().zip(&mesh_only.wheels) {
        assert_eq!(w.node, m.node);
        assert_eq!(m.radius_source, RadiusSource::Mesh);
        // The BLK radii are the wheels' tread radii: the road wheels and idlers match their meshes to
        // 2 mm; the sprocket's is the pitch radius, inside the tips of its teeth; the rollers' mesh
        // is a little smaller than their rubber
        let name = &v.nodes[w.node].name;
        match w.kind {
            WheelKind::Road => assert!((w.radius - m.radius).abs() < 0.005, "{name}: {} vs {}", w.radius, m.radius),
            WheelKind::End => assert!((w.radius - m.radius).abs() < 0.01, "{name}"),
            WheelKind::Drive => assert!(w.radius < m.radius, "{name}"),
            WheelKind::Top => assert!((w.radius - m.radius).abs() < 0.03, "{name}"),
        }
    }
    let sprocket = gear.wheels.iter().find(|w| w.kind == WheelKind::Drive).unwrap();
    assert_eq!(sprocket.radius, drive.drive_gear_radius);
    assert!(gear.wheels.iter().filter(|w| w.kind == WheelKind::Road).all(|w| Some(w.radius) == drive.road_wheel_radius));
    assert!((gear.gauge.unwrap() - 2.163).abs() < 0.001, "{:?}", gear.gauge);

    // Both belts run the texture at the rate of the track mesh, which is a quarter of a V unit per link
    // of the pitch the unit BLK gives
    assert_eq!(gear.belts.len(), 2);
    assert_eq!(gear.belts.iter().map(|b| b.side).collect::<Vec<_>>(), [Side::Left, Side::Right]);
    let pitch = 0.154;
    for b in &gear.belts {
        assert!((b.v_per_metre * pitch - 0.25).abs() < 0.005, "{}", b.v_per_metre);
    }
    assert!((drive.track_length.unwrap() / 79.0 - pitch).abs() < 1e-4);
    assert_eq!(drive.track_animation_multiplier, Some(0.68));

    // Driving 10 m turns a road wheel 40 rad (clockwise), which is 6.37 turns, and runs the texture 16.1 V
    let mut state = gear.rest();
    gear.advance(&mut state, 10.0, 10.0);
    let road = gear.wheels.iter().position(|w| w.kind == WheelKind::Road).unwrap();
    assert!((state.wheel_angles[road] - (std::f32::consts::TAU * 7.0 - 40.0)).abs() < 1e-3, "{}", state.wheel_angles[road]);
    assert!((state.belt_offsets[0] - (gear.belts[0].v_per_metre * 10.0).fract()).abs() < 1e-3);
}

/// The Sherman's rig, from the unit file and the model, without the bridge: turret limits and speeds, the drive
/// train and its top speed, the wheels and belts, and the sockets
#[test]
fn sherman_rig_comes_from_the_unit_and_the_model() {
    let Some(game) = game() else { return };
    let pack = game.join("content/base/res/tanks/usa_m4a1_76w_sherman.grp");
    let v = vehicle::load(&game, &pack, "m4a1_76w_sherman", 0).unwrap();
    let tank = tank::load(&game, "m4a1_76w_sherman", None).unwrap().unwrap();
    let rig = VehicleRig::new(&v, Some(&tank));
    assert_eq!((rig.model.as_str(), rig.unit.as_deref()), ("m4a1_76w_sherman", Some("us_m4a1_76w_sherman")));

    // The main gun, and the coaxial and anti-aircraft machine guns after it
    let gun = rig.turret.as_ref().unwrap();
    assert_eq!((gun.head.as_str(), gun.gun.as_str(), gun.barrel.as_deref()), ("bone_turret", "bone_gun", Some("bone_gun_barrel")));
    assert_eq!((gun.emitter.as_deref(), gun.flash.as_deref()), (Some("bone_gun_barrel"), Some("emtr_gun_flame")));
    assert_eq!((gun.yaw, gun.pitch), ([-180.0, 180.0], [-10.0, 25.0]));
    assert_eq!((gun.speed_yaw, gun.speed_pitch), (24.0, 4.0));
    assert_eq!(gun.pitch_by_yaw.len(), 3);
    assert_eq!(rig.weapons.len(), 3);
    assert_eq!(rig.weapons[2].head, "bone_mg_aa_h_01");

    // 38.6 km/h forward and 5.0 reverse from the gearing; the BLK's own limits are kept as they are
    let drive = rig.drive.as_ref().unwrap();
    assert_eq!((drive.mass, drive.horse_powers, drive.max_rpm, drive.drive_gear_radius), (32673.0, 400.0, 2400.0, 0.305));
    assert_eq!((drive.max_accel, drive.max_ang_speed), (Some(4.0), Some(30.0)));
    let speeds = rig.speeds.unwrap();
    assert!((speeds.top_speed_kmh - 38.6).abs() < 0.1 && (speeds.reverse_speed_kmh - 5.0).abs() < 0.1, "{speeds:?}");

    let gear = rig.gear.as_ref().unwrap();
    assert_eq!((gear.wheels.len(), gear.belts.len()), (22, 2));
    assert!((gear.gauge_m.unwrap() - 2.163).abs() < 0.001);
    // Bridge axes: the left side is +X, the front +Z
    let wheel = |name: &str| gear.wheels.iter().find(|w| w.name == name).unwrap();
    assert!(wheel("wheel_l_01").pivot[0] > 1.0 && wheel("wheel_r_01").pivot[0] < -1.0);
    assert!(wheel("wheel_l_01").pivot[2] > wheel("wheel_l_06").pivot[2]);
    assert_eq!(wheel("wheel_l_01").radius_m, 0.25);
    assert!(gear.belts.iter().all(|b| (b.v_per_metre - 1.614).abs() < 0.01));

    // The sockets name the unit's nodes and the model's wheels, tracks, cameras and hatches
    let node = |s: &str| rig.socket(s).unwrap_or_else(|| panic!("no socket {s}")).node_name.as_str();
    assert_eq!((node("turret"), node("gun"), node("barrel"), node("muzzle"), node("emitter")), ("bone_turret", "bone_gun", "bone_gun_barrel", "emtr_gun_flame", "bone_gun_barrel"));
    assert_eq!((node("weapon_1_turret"), node("weapon_2_gun")), ("bone_mg_gun_twin", "bone_mg_aa_v_01"));
    assert_eq!((node("wheel_l_01"), node("track_r"), node("camera_driver"), node("hatch_01"), node("emtr_exhaust_01")), ("wheel_l_01", "track_r", "bone_camera_driver", "hatch_01", "emtr_exhaust_01"));
    assert_eq!(rig.sockets.iter().filter(|s| s.name.starts_with("wheel_")).count(), 22);
    assert!(rig.missing_nodes.is_empty(), "{:?}", rig.missing_nodes);
    // Every socket's node index is that node in the vehicle
    assert!(rig.sockets.iter().all(|s| v.nodes[s.node].name == s.node_name));

    // Driving: full throttle for a minute reaches the geared top speed, and the wheels have rolled forward
    let mut m = rig.driving_model().unwrap();
    for _ in 0..60 * 60 {
        m.step(Controls { throttle: 1.0, steer: 0.0 }, 1.0 / 60.0);
    }
    assert!((m.speed_ms() * 3.6 - speeds.top_speed_kmh).abs() < 0.5);
    assert_eq!(m.wheel_angles().len(), 22);
    assert!(m.belt_offsets().iter().all(|o| (0.0..1.0).contains(o)));
    // A model no unit uses has no turret or drive, but keeps its wheel and track sockets
    let bare = VehicleRig::new(&v, None);
    assert!(bare.turret.is_none() && bare.drive.is_none() && bare.socket("wheel_l_01").is_some() && bare.socket("turret").is_none());
}

/// Every collision resource in the game's packs decodes, with its vertices inside its nodes' boxes
#[test]
fn all_collision_resources_decode_and_stay_in_their_boxes() {
    let Some(game) = game() else { return };
    let mut packs: Vec<PathBuf> = fs::read_dir(game.join("content/base/res")).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "grp")).collect();
    packs.sort();
    let (mut resources, mut triangles, mut kinds) = (0usize, 0usize, std::collections::BTreeSet::new());
    for path in packs {
        let file = vehicle::map(&path).unwrap();
        let Ok(grp) = wt_blk::dxp_and_grp::grp_pack::GrpPack::parse(&file) else { continue };
        for r in grp.resources.iter().filter(|r| r.class_id == COLLISION) {
            let c = Collision::parse(&r.name, grp.data(r)).unwrap_or_else(|e| panic!("{}: {e:#}", r.name));
            for n in &c.nodes {
                kinds.insert(format!("{:?}", n.kind));
                assert_eq!(n.indices.len() % 3, 0);
                assert!(n.indices.iter().all(|&i| (i as usize) < n.positions.len()), "{}", r.name);
                for p in &n.positions {
                    assert!((0..3).all(|k| p[k] >= n.bbox[0][k] - 0.02 && p[k] <= n.bbox[1][k] + 0.02), "{}: {p:?} outside {:?}", r.name, n.bbox);
                }
            }
            resources += 1;
            triangles += c.triangles();
        }
    }
    // 19,799 resources in War Thunder 2.59, with 11.5 million triangles
    assert!(resources > 19_000 && triangles > 10_000_000, "{resources} resources, {triangles} triangles");
    assert_eq!(kinds.len(), 6, "{kinds:?}");
}

/// A building's collision is one box; a tree's is a capsule; both match the object they belong to
#[test]
fn berlin_objects_have_collision_resources() {
    let Some(game) = game() else { return };
    let index = ResourceIndex::build(&game, &[COLLISION]).unwrap();
    let load = |name: &str| {
        let path = index.find(COLLISION, name).unwrap_or_else(|| panic!("no {name}")).to_path_buf();
        let file = vehicle::map(&path).unwrap();
        let grp = wt_blk::dxp_and_grp::grp_pack::GrpPack::parse(&file).unwrap();
        let r = grp.find(name, COLLISION).unwrap();
        Collision::parse(name, grp.data(r)).unwrap()
    };
    let building = load("berlin_city_build_1_collision");
    assert_eq!(building.nodes.len(), 1);
    let n = &building.nodes[0];
    assert_eq!((n.kind, n.materials.as_slice()), (NodeKind::Box, ["concrete".to_string()].as_slice()));
    assert!(n.traceable() && n.physics_collidable());
    // 24 m wide and 22.5 m tall: the node's z axis is the object's up (its transform swaps y and z)
    assert!((n.bbox[1][0] - n.bbox[0][0] - 24.0).abs() < 0.01 && (n.bbox[1][2] - n.bbox[0][2] - 22.49).abs() < 0.01, "{:?}", n.bbox);
    assert_eq!(n.tm[1], [0.0, 0.0, 1.0]);
    let tree = load("liveoak_03_rendinst_collision");
    let cap = tree.nodes.iter().find(|n| n.kind == NodeKind::Capsule).unwrap().capsule.unwrap();
    assert!(cap.radius > 0.3 && cap.radius < 1.0, "{cap:?}");
}

#[test]
fn berlin_level_has_terrain_and_placed_objects() {
    let Some(game) = game() else { return };
    let raw = fs::read(game.join("levels/avg_berlin.bin")).unwrap();
    let level = Level::parse(&raw).unwrap();
    assert_eq!(level.blocks.last().unwrap().tag, "END");

    // The tank battle area: 2048 x 2048 samples 2 m apart, heights within 13..77 m
    let hm = level.heightmap().unwrap().unwrap();
    assert_eq!((hm.width, hm.height, hm.cell_size), (2048, 2048, 2.0));
    let grid = HeightGrid::from_heightmap(&hm);
    let (lo, hi) = grid.range().unwrap();
    assert!(lo >= 13.0 && hi <= 77.0 && hi - lo > 30.0, "{lo}..{hi}");

    // The landmesh around it, from the legacy ray tracer
    let lm = level.landmesh().unwrap().unwrap();
    assert_eq!(lm.cells, [16, 16]);
    let mesh = lm.tracer_mesh().unwrap().unwrap();
    assert!(mesh.indices.len() / 3 > 100_000);
    assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.positions.len()));

    // Placed objects: every counted instance unpacks, trees among them
    let layers = level.rendinst_layers().unwrap();
    assert!(!layers.is_empty());
    let mut total = 0;
    for layer in &layers {
        let instances = layer.instances().unwrap();
        assert_eq!(instances.len(), layer.counts().iter().sum::<usize>());
        total += instances.len();
    }
    assert!(total > 400_000, "{total}");
    assert!(layers[0].pools.iter().any(|p| p.name.starts_with("liveoak_")));
}

/// The ground's material data: the level's land classes by name, their weights per cell, and the
/// land class resources in the game's packs
#[test]
fn berlin_ground_material_comes_from_the_level_and_its_land_classes() {
    let Some(game) = game() else { return };
    let raw = fs::read(game.join("levels/avg_berlin.bin")).unwrap();
    let level = Level::parse(&raw).unwrap();
    let map = level.landmesh().unwrap().unwrap().detail_map().unwrap();
    // One weight texture of 128 x 128 per landmesh cell (4096 m), so a texel is 32 m
    assert_eq!((map.width, map.height, map.elem_size), (16, 16, 128));
    let names: Vec<&str> = map.land_classes.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["berlin_fields_a", "berlin_fields_b", "berlin_forest", "berlin_village", "berlin_city_b", "berlin_coast", "berlin_detailed_biome", "berlin_forest_b"]);

    let index = ResourceIndex::build(&game, &[LAND_CLASS]).unwrap();
    let ground = GroundMaterial::load(&level, &index).unwrap().unwrap();
    assert!(ground.errors.is_empty(), "{:?}", ground.errors);
    let class = |name: &str| ground.classes.iter().flatten().find(|c| c.name == name).unwrap_or_else(|| panic!("no land class {name}"));
    let city = class("berlin_city_b");
    assert_eq!((city.kind, city.colormap.as_deref(), city.size), (LandClassKind::Mega, Some("berlin_city_b*"), [2048.0, 2048.0]));
    assert_eq!(city.splat_map.as_deref(), Some("berlin_city_a_tex_m"));
    let red = &city.layers[0];
    assert_eq!((red.slot.as_str(), red.albedo.as_deref(), red.tile_m), ("Red", Some("detail_jungle_grass_fields_tex_d"), Some(31.0)));
    let biome = class("berlin_detailed_biome");
    assert_eq!((biome.kind, biome.shader.as_deref()), (LandClassKind::Indexed, Some("landmesh_indexed_landclass")));
    assert!(biome.layers.len() > 50);
    assert!(biome.extra_textures.contains(&("indices".to_string(), "berlin_detailed_biome_tex_b".to_string())));

    // The game's own copy of a land class holds the same kind of data
    let path = index.find(LAND_CLASS, "berlin_detailed_biome").expect("the biome land class is in a pack");
    let file = fs::read(path).unwrap();
    let pack = wt_blk::dxp_and_grp::grp_pack::GrpPack::parse(&file).unwrap();
    let res = pack.find("berlin_detailed_biome", LAND_CLASS).unwrap();
    let from_pack = landclass::LandClassMaterial::from_block("berlin_detailed_biome", &landclass::resource_detail(pack.data(res)).unwrap());
    assert_eq!((from_pack.kind, from_pack.size, from_pack.colormap.as_deref()), (LandClassKind::Indexed, [4096.0, 4096.0], Some("berlin_detailed_biome_tex_d")));

    // Weights blend to 1 everywhere, and the tank battle area (cell 7, 11) is the biome class's
    let weights = ground.weights.as_ref().unwrap();
    let mut blend = Vec::new();
    for (x, z) in [(2048.0, 2048.0), (100.0, 4000.0), (-20000.0, -30000.0), (30000.0, 15000.0)] {
        weights.at(x, z, &mut blend);
        let sum: f32 = blend.iter().map(|b| b.1).sum();
        assert!((sum - 1.0).abs() < 0.02, "({x}, {z}): {blend:?}");
    }
    weights.at(2048.0, 1500.0, &mut blend);
    assert!(blend.iter().any(|&(c, w)| c == 6 && w > 0.5), "{blend:?}");
}

/// Placed-object models are RendInst resources, and trees turn by their palette
#[test]
fn berlin_objects_resolve_to_rendinst_models_and_trees_have_rotation() {
    let Some(game) = game() else { return };
    let index = ResourceIndex::build(&game, &[REND_INST]).unwrap();
    let model = |name: &str| {
        let path = index.find(REND_INST, name).unwrap_or_else(|| panic!("{name} is in no pack"));
        let file = fs::read(path).unwrap();
        let pack = wt_blk::dxp_and_grp::grp_pack::GrpPack::parse(&file).unwrap();
        let res = pack.find(name, REND_INST).unwrap();
        RiModel::parse(pack.data(res)).unwrap()
    };
    // A house: 24 m tall, two LODs, the first used up to 100 m
    let house = model("berlin_city_build_1");
    assert_eq!(house.lods.iter().map(|l| l.range).collect::<Vec<_>>(), [100.0, 8000.0]);
    assert!(house.bbox[1][1] > 24.0 && house.bbox[0][1] < 0.0);
    assert!(house.triangles(0) > house.triangles(1));
    assert_eq!(house.lod_for_distance(50.0), 0);
    assert_eq!(house.lod_for_distance(250.0), 1);
    // A tree: its last LOD is the billboard impostor, which is never a mesh LOD
    let oak = model("liveoak_03_rendinst");
    assert!(oak.has_impostor && oak.lods.len() == 3);
    assert_eq!(oak.lod_for_distance(1.0e6), 1);

    let raw = fs::read(game.join("levels/avg_berlin.bin")).unwrap();
    let level = Level::parse(&raw).unwrap();
    let layers = level.rendinst_layers().unwrap();
    let layer = &layers[0];
    let pool = layer.pools.iter().position(|p| p.name == "liveoak_03_rendinst").unwrap();
    let p = &layer.pools[pool];
    assert!(p.pos_only && p.palette_rotation && p.palette_count > 1, "{p:?}");
    // The palette's rotations are turns of i / count about Y: every instance has a turned frame, with
    // as many distinct turns as the palette has presets
    let mut turns = std::collections::BTreeSet::new();
    for inst in layer.instances().unwrap().iter().filter(|i| i.pool == pool) {
        let [x, y, z, _] = inst.tm;
        let s = y[1];
        assert!(s > 0.0 && (x[0] * x[0] + x[2] * x[2]).sqrt() - s < 1e-3 * s.max(1.0), "{inst:?}");
        // x axis (cos, 0, sin) and z axis (-sin, 0, cos), times the scale
        assert!((x[0] - z[2]).abs() < 1e-4 && (x[2] + z[0]).abs() < 1e-4);
        turns.insert((x[2].atan2(x[0]) / std::f32::consts::TAU * p.palette_count as f32).round().rem_euclid(p.palette_count as f32) as usize);
    }
    assert_eq!(turns.len(), p.palette_count, "{turns:?}");
}

/// The material class of each material of a placed-object model, from riDesc.bin
fn material_classes(game: &std::path::Path, name: &str) -> Vec<String> {
    let descs = model_descs_where(game, |n| n == name).unwrap();
    let desc = descs.get(name).unwrap_or_else(|| panic!("{name} is not in riDesc.bin"));
    let mats = desc.blocks.iter().find(|b| b.name == "mat").unwrap();
    mats.blocks
        .iter()
        .map(|m| m.params.iter().find_map(|(n, v)| if n == "cls" { if let blk::Value::Str(s) = v { Some(s.clone()) } else { None } } else { None }).unwrap_or_default())
        .collect()
}

fn load_rendinst(game: &std::path::Path, name: &str) -> RiModel {
    let index = ResourceIndex::build(game, &[REND_INST]).unwrap();
    let file = fs::read(index.find(REND_INST, name).unwrap_or_else(|| panic!("{name} is in no pack"))).unwrap();
    let pack = wt_blk::dxp_and_grp::grp_pack::GrpPack::parse(&file).unwrap();
    RiModel::parse(pack.data(pack.find(name, REND_INST).unwrap())).unwrap()
}

/// The leaf cards of trees (`rendinst_facing_leaves`) are four vertices around one centre, moved by the
/// shader: their 24-byte vertices are not laid out as the buffer's declaration of the trunk says, and read
/// that way they land 1e11 m from the tree. Read as the shader's layout they sit inside the model's box and
/// are the very centres and corner offsets the file lists in its impostor data.
#[test]
fn facing_leaves_decode_to_the_centres_and_corners_the_file_lists() {
    let Some(game) = game() else { return };
    for name in ["liveoak_01_rendinst", "liveoak_03_rendinst", "bush3_r_rendinst"] {
        let model = load_rendinst(&game, name);
        let classes = material_classes(&game, name);
        let shadow = &model.impostor_data.as_ref().unwrap().shadow_points;
        let (mut leaf_elems, mut trunk_vertices) = (0, 0);
        for (l, lod) in model.lods.iter().enumerate() {
            for e in &lod.elems {
                let vd = &model.vdata[e.vdata];
                let range = &vd.indices[e.first_index..e.first_index + e.triangles * 3];
                let mut unique: Vec<usize> = range.iter().map(|&i| i as usize + e.base_vertex).collect();
                unique.sort_unstable();
                unique.dedup();
                if !is_facing_leaves(&classes[e.material]) {
                    // Everything else in a tree is the declaration's layout, and inside the box
                    for &v in &unique {
                        let p = model.vertex(vd, v).unwrap().pos;
                        assert!((0..3).all(|k| p[k] >= model.bbox[0][k] - 0.1 && p[k] <= model.bbox[1][k] + 0.1), "{name} lod {l}: {p:?}");
                    }
                    trunk_vertices += unique.len();
                    continue;
                }
                leaf_elems += 1;
                // Four vertices per card, every card's centre inside the model's box
                assert_eq!(unique.len() % 4, 0, "{name}");
                for &v in &unique {
                    let c = model.leaf_vertex(vd, v).unwrap().centre;
                    assert!((0..3).all(|k| c[k] >= model.bbox[0][k] - 0.1 && c[k] <= model.bbox[1][k] + 0.1), "{name} lod {l}: {c:?} outside {:?}", model.bbox);
                }
                if l == 0 {
                    // The file's list is made from LOD 0 with the centres' Y clamped to 0 and above. Each
                    // vertex is one of its records, to the precision of a half float, and none is left over.
                    assert_eq!(unique.len(), shadow.len(), "{name}");
                    let mut left: Vec<_> = shadow.iter().collect();
                    for &v in &unique {
                        let lv = model.leaf_vertex(vd, v).unwrap();
                        let c = [lv.centre[0], lv.centre[1].max(0.0), lv.centre[2]];
                        let near = |a: f32, b: f32| (a - b).abs() <= 0.0011 * b.abs() + 2e-4;
                        let at = left.iter().position(|b| (0..3).all(|k| near(c[k], b[0][k]) && near(lv.corner[k], b[1][k]))).unwrap_or_else(|| panic!("{name}: leaf vertex {v} {lv:?} is not in the impostor data"));
                        left.swap_remove(at);
                    }
                    assert!(left.is_empty());
                }
            }
        }
        assert!(leaf_elems >= 2 && trunk_vertices > 0, "{name}: {leaf_elems} leaf elements, {trunk_vertices} other vertices");
    }
    // The impostor data also says how far a card's corner reaches from its centre
    let oak = load_rendinst(&game, "liveoak_01_rendinst");
    let reach = oak.impostor_data.as_ref().unwrap().max_facing_leaves_delta;
    assert!((reach - 15.7074).abs() < 1e-3, "{reach}");
    // A layout the shader does not ask for is refused rather than read as leaves
    let trunk = &oak.vdata[0];
    assert!(oak.leaf_vertex(trunk, 0).is_err());
}

/// A level's placed objects stay near their model's origin, so the level's extent is the level's: the
/// leaf cards of trees used to put vertices 1e11 m out and its bounding box 2.4e11 m wide
#[cfg(feature = "bridge")]
#[test]
fn norway_plain_scene_has_no_vertices_far_from_the_level() {
    use rewrite_bridge::{Provider, SceneModel};
    let Some(game) = game() else { return };
    let wt = wt_unpack::bridge::WarThunder::new(&game);
    let scene = wt.load_scene("levels/arcade_norway_plain.bin").unwrap();
    scene.validate().unwrap();
    let radius = |m: &rewrite_bridge::Model| m.meshes.iter().flat_map(|x| &x.positions).map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt()).fold(0.0f32, f32::max);
    let (mut far_models, mut reach) = (Vec::new(), 0.0f32);
    for (i, m) in scene.models.iter().enumerate() {
        let SceneModel::Inline(m) = m else { continue };
        // The ground and the water are level-sized by nature; objects are not
        if m.name == "terrain" || m.name == "water" {
            continue;
        }
        let r = radius(m);
        if r > 1000.0 {
            far_models.push((m.name.clone(), r));
        }
        for inst in scene.instances.iter().filter(|x| x.model == i) {
            let t = inst.transform.translation;
            let s = inst.transform.scale.iter().cloned().fold(0.0f32, f32::max);
            reach = reach.max((t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt() + r * s);
        }
    }
    assert!(far_models.is_empty(), "objects with vertices more than a kilometre from their origin: {far_models:?}");
    assert!(reach < 100_000.0, "an instance reaches {reach} m from the origin");
    // The oaks are still there, as trunks and branches without their leaf cards
    assert!(scene.models.iter().any(|m| matches!(m, SceneModel::Inline(m) if m.name == "liveoak_01_rendinst")));
}

#[test]
fn kamchatka_terrain_comes_from_the_lts4_tracer() {
    let Some(game) = game() else { return };
    let raw = fs::read(game.join("levels/air_kamchatka.bin")).unwrap();
    let level = Level::parse(&raw).unwrap();
    assert!(level.heightmap().unwrap().is_none());
    let lm = level.landmesh().unwrap().unwrap();
    assert_eq!(lm.tracer_kind(), "LTS4");
    let mesh = lm.tracer_mesh().unwrap().unwrap();
    let grid = HeightGrid::rasterize(&mesh, 64.0).unwrap();
    assert_eq!((grid.width, grid.height), (2048, 2048));
    // Sea level up to the volcanoes, above 4 km
    let (lo, hi) = grid.range().unwrap();
    assert!(lo < 0.0 && hi > 4000.0, "{lo}..{hi}");
}

/// The Sherman through the rewrite-bridge provider: valid, facing +Z, with a turret and gun to turn
#[cfg(feature = "bridge")]
#[test]
fn sherman_loads_through_the_bridge() {
    use rewrite_bridge::{AssetKind, Provider};
    let Some(game) = game() else { return };
    let wt = wt_unpack::bridge::WarThunder::new(&game);
    let listed = wt.list("m4a1_76w_sherman").unwrap();
    let id = "tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman";
    assert!(listed.iter().any(|a| a.id == id && a.kind == AssetKind::Vehicle), "{listed:?}");
    // Damage and x-ray variants are not listed
    assert!(listed.iter().all(|a| !a.id.ends_with("_dmg") && !a.id.ends_with("_xray")));

    let m = wt.load(id).unwrap();
    m.validate().unwrap();
    // Node frames carry no rotation, so a node's place in the model is the sum of the translations
    let mut world = vec![[0f32; 3]; m.nodes.len()];
    for (i, n) in m.nodes.iter().enumerate() {
        let base = n.parent.map_or([0.0; 3], |p| world[p]);
        world[i] = [0, 1, 2].map(|k| base[k] + n.translation[k]);
        assert_eq!((n.rotation, n.scale), ([0.0, 0.0, 0.0, 1.0], [1.0; 3]));
    }
    let node = |name: &str| m.nodes.iter().position(|n| n.name == name).unwrap_or_else(|| panic!("no node {name}"));
    let (turret, gun) = (node("bone_turret"), node("bone_gun"));
    assert_eq!(m.nodes[gun].parent, Some(turret));
    assert!(m.meshes.iter().any(|x| x.node == turret) && m.meshes.iter().any(|x| x.node == gun));

    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for mesh in &m.meshes {
        let at = if mesh.skin.is_some() { [0.0; 3] } else { world[mesh.node] };
        for p in &mesh.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k] + at[k]);
                hi[k] = hi[k].max(p[k] + at[k]);
            }
        }
    }
    // About 7 m long along Z with the gun, about 2.7 m wide along X
    let size: Vec<f32> = (0..3).map(|k| hi[k] - lo[k]).collect();
    assert!((6.5..7.5).contains(&size[2]) && (2.5..3.0).contains(&size[0]), "{size:?}");
    // The front faces +Z: the muzzle is far forward, the gun points along +Z, and the left wheels
    // are at +X (glTF's front +Z with +Y up puts the model's left at +X)
    assert!(world[node("emtr_gun_flame")][2] > 3.5, "{:?}", world[node("emtr_gun_flame")]);
    assert!(world[node("emtr_gun_flame")][2] > world[gun][2] + 2.5);
    assert!(world[node("wheel_l_01")][0] > 1.0 && world[node("wheel_r_01")][0] < -1.0);
    // The tracks are skinned, the glass blends
    assert_eq!(m.skins.len(), 3, "track_l, track_r and the antenna");
    // The tracks follow the root and six suspension wheels each, with one influence per vertex
    let mut joints: Vec<usize> = m.skins.iter().map(|s| s.joints.len()).collect();
    joints.sort_unstable();
    assert_eq!(joints, [2, 7, 7]);
    for mesh in m.meshes.iter().filter(|x| x.skin.is_some()) {
        assert_eq!(mesh.joints.len(), mesh.positions.len());
        assert!(mesh.weights.iter().all(|w| (w.iter().sum::<f32>() - 1.0).abs() < 1e-4));
    }
    assert!(m.materials.iter().any(|x| x.name.contains("glass") && x.alpha == rewrite_bridge::AlphaMode::Blend));
}

#[test]
fn tank_engine_bank_lists_and_decodes() {
    let Some(game) = game() else { return };
    // The event bank holds no samples; its .assets.bank pair does
    let events = sound::map(&game.join("sound/tanks_engines.bank")).unwrap();
    assert!(sound::fsb5_ranges(&events).unwrap().is_empty());

    let map = sound::map(&game.join("sound/tanks_engines.assets.bank")).unwrap();
    let ranges = sound::fsb5_ranges(&map).unwrap();
    let fsbs: Vec<Fsb5> = ranges.iter().map(|r| Fsb5::parse(&map[r.clone()]).unwrap()).collect();
    let codecs: Vec<Codec> = fsbs.iter().map(|f| f.codec).collect();
    assert_eq!(codecs, [Codec::Vorbis, Codec::Fadpcm]);
    for fsb in &fsbs {
        assert!(fsb.samples.len() > 100);
        for s in &fsb.samples {
            assert!(s.name.is_some() && s.rate >= 8000 && (1..=2).contains(&s.channels) && s.frames > 0);
        }
    }
    let vorbis = &fsbs[0];
    assert!(vorbis.samples.iter().all(|s| s.vorbis_setup.is_some_and(|crc| sound::vorbis::setup_packet(crc).is_some())));
    let idle = vorbis.samples.iter().find(|s| s.name.as_deref() == Some("engine_00")).unwrap();

    // Decoding yields the header's length, with sound in it
    for (fsb, s) in [(vorbis, idle), (&fsbs[1], &fsbs[1].samples[0])] {
        let pcm = fsb.decode(s).unwrap();
        assert_eq!(pcm.samples.len(), s.frames as usize * usize::from(s.channels));
        let rms = (pcm.samples.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / pcm.samples.len() as f64).sqrt();
        assert!(rms > 100.0, "{:?} is silent", s.name);
    }

    // The Ogg repack opens with the identification page and closes with the end-of-stream page
    let mut ogg = Vec::new();
    sound::vorbis::write_ogg(idle, vorbis.data(idle), &mut ogg).unwrap();
    assert_eq!(&ogg[..4], b"OggS");
    assert_eq!(ogg[5], 2);
    let last = ogg.windows(4).rposition(|w| w == b"OggS").unwrap();
    assert_eq!(ogg[last + 5] & 4, 4);
    assert_eq!(u64::from_le_bytes(ogg[last + 6..last + 14].try_into().unwrap()), u64::from(idle.frames));
}

/// The Sherman through `load_animated`: the same model as `load`, plus the rig's sockets; and the rig by id
#[cfg(feature = "bridge")]
#[test]
fn sherman_sockets_come_through_load_animated() {
    use rewrite_bridge::Provider;
    let Some(game) = game() else { return };
    let wt = wt_unpack::bridge::WarThunder::new(&game);
    let id = "tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman";
    let a = wt.load_animated(id).unwrap();
    a.validate().unwrap();
    // `load` is the same model
    assert!(wt.load(id).unwrap() == a.model);
    let at = |name: &str| a.socket(name).unwrap_or_else(|| panic!("no socket {name}")).node;
    let name_of = |name: &str| a.model.nodes[at(name)].name.as_str();
    assert_eq!((name_of("turret"), name_of("gun"), name_of("barrel"), name_of("muzzle")), ("bone_turret", "bone_gun", "bone_gun_barrel", "emtr_gun_flame"));
    assert_eq!((name_of("wheel_r_top_02"), name_of("track_l"), name_of("camera_gunner")), ("wheel_r_top_02", "track_l", "bone_camera_gunner"));
    assert_eq!(a.model.nodes[at("gun")].parent, Some(at("turret")));
    assert_eq!(a.model.nodes[at("muzzle")].parent, Some(at("gun")));
    assert!(a.sockets.iter().all(|s| s.offset == rewrite_bridge::Transform::IDENTITY));

    // The rig by id agrees, without decoding textures
    let rig = wt.vehicle_rig(id).unwrap();
    assert_eq!(rig.sockets.len(), a.sockets.len());
    for s in &rig.sockets {
        assert_eq!(at(&s.name), s.node);
    }
    let gun = rig.turret.unwrap();
    assert_eq!((gun.yaw, gun.pitch), ([-180.0, 180.0], [-10.0, 25.0]));
    assert!((rig.speeds.unwrap().top_speed_kmh - 38.6).abs() < 0.1);
    // Wheels and belts index the bridge model's nodes and meshes
    let gear = rig.gear.unwrap();
    assert!(gear.wheels.iter().all(|w| a.model.nodes[w.node].name == w.name));
    assert!(gear.belts.iter().all(|b| a.model.meshes[b.mesh].skin.is_some() && a.model.nodes[b.node].name == b.name));

    // Clearing the caches changes nothing but memory
    wt.clear_caches();
    assert!(wt.load_animated(id).unwrap() == a);
    // Aircraft have no sockets
    let plane = wt.list("aircrafts/").unwrap().into_iter().next().unwrap();
    assert!(wt.load_animated(&plane.id).unwrap().sockets.is_empty());
}

/// Levels are scenes: listed as levels, validated, with their placed objects, ground and water
#[cfg(feature = "bridge")]
#[test]
fn berlin_loads_as_a_scene_through_the_bridge() {
    use rewrite_bridge::{AssetKind, Provider, SceneModel};
    let Some(game) = game() else { return };
    let wt = wt_unpack::bridge::WarThunder::new(&game);
    let listed = wt.list("levels/avg_berlin").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!((listed[0].id.as_str(), listed[0].kind), ("levels/avg_berlin.bin", AssetKind::Level));
    // A level is not a model
    assert!(wt.load("levels/avg_berlin.bin").is_err());

    let scene = wt.load_scene("levels/avg_berlin.bin").unwrap();
    scene.validate().unwrap();
    assert_eq!(scene.name, "avg_berlin");
    let inline = |i: usize| match &scene.models[i] {
        SceneModel::Inline(m) => m,
        other => panic!("model {i} is {other:?}"),
    };
    let by_name = |name: &str| (0..scene.models.len()).find(|&i| inline(i).name == name).unwrap_or_else(|| panic!("no model {name}"));
    let count = |model: usize| scene.instances.iter().filter(|i| i.model == model).count();

    // The placed objects: the level's 512,641 instances of 452 resolved models, one model each
    assert_eq!(count(by_name("liveoak_03_rendinst")), 321_161);
    assert_eq!(count(by_name("berlin_city_build_1")), 1_878);
    assert!(scene.instances.len() > 512_000 && scene.models.len() < 460, "{} instances of {} models", scene.instances.len(), scene.models.len());
    let house = inline(by_name("berlin_city_build_1"));
    assert!(house.materials.iter().any(|m| m.base_color.is_some()));

    // The ground: one tile per landmesh cell, textured, from -28672 to 36864 in the game's X (the bridge's Z)
    let terrain = inline(by_name("terrain"));
    assert_eq!(terrain.meshes.len(), 256);
    assert!(terrain.materials.iter().all(|m| m.base_color.is_some()));
    let z_max = terrain.meshes.iter().flat_map(|m| m.positions.iter().map(|p| p[2])).fold(f32::MIN, f32::max);
    assert!((z_max - 36864.0).abs() < 1.0, "{z_max}");
    let heights = terrain.meshes.iter().flat_map(|m| m.positions.iter().map(|p| p[1]));
    let (lo, hi) = heights.fold((f32::MAX, f32::MIN), |(l, h), y| (l.min(y), h.max(y)));
    assert!(lo > -20.0 && hi < 205.0 && hi - lo > 40.0, "{lo}..{hi}");

    // The water: the level's own water_level and colour
    let water = inline(by_name("water"));
    assert!(water.meshes[0].positions.iter().all(|p| p[1] == 35.0));
    assert_eq!(water.materials[0].base_color_factor, [0.82, 0.71, 0.58, 1.0]);
    // Sun, sky and lights are not in the level's data
    assert!(scene.lights.is_empty() && scene.environment.is_none());
}

/// The collision of the objects Berlin places, in the objects' own space: matches the render models
#[cfg(feature = "bridge")]
#[test]
fn berlin_collision_matches_the_placed_objects() {
    use rewrite_bridge::{Provider, SceneModel};
    let Some(game) = game() else { return };
    let wt = wt_unpack::bridge::WarThunder::new(&game);
    let collision = wt.load_level_collision("levels/avg_berlin.bin").unwrap();
    assert_eq!(collision.level, "avg_berlin");
    assert!(collision.failed.is_empty(), "{:?}", collision.failed);
    // 446 of the level's 476 objects have collision; the debris the destruction effects scatter do not
    assert!(collision.objects.len() > 400 && collision.triangles() > 200_000, "{} objects, {} triangles", collision.objects.len(), collision.triangles());
    assert!(collision.without_collision.iter().any(|n| n.starts_with("debris_")));

    let scene = wt.load_scene("levels/avg_berlin.bin").unwrap();
    let bounds = |m: &rewrite_bridge::Model| {
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in m.meshes.iter().flat_map(|x| &x.positions) {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        (lo, hi)
    };
    let (mut compared, mut outside) = (0, Vec::new());
    for m in &scene.models {
        let SceneModel::Inline(m) = m else { continue };
        let Some(model) = collision.object(&m.name).and_then(|o| o.model.as_ref()) else { continue };
        model.validate().unwrap();
        // The collision sits inside the render model's box (give or take a metre and a half of fitting) in the
        // same axes. The `_dp_` pieces of damaged buildings are drawn as fragments of the building but keep
        // the whole one's collision, so they are left out.
        if m.name.contains("_dp_") {
            continue;
        }
        let (r, c) = (bounds(m), bounds(model));
        if (0..3).any(|k| c.0[k] < r.0[k] - 1.5 || c.1[k] > r.1[k] + 1.5) {
            outside.push(m.name.clone());
        }
        compared += 1;
    }
    assert!(compared > 250, "{compared}");
    assert!(outside.is_empty(), "{} of {compared} outside: {outside:?}", outside.len());
    // The house is one box of concrete; the oaks have capsule trunks
    let house = collision.object("berlin_city_build_1").unwrap();
    assert_eq!(house.model.as_ref().unwrap().materials[0].name, "concrete");
    assert_eq!(house.triangles(), 12);
    assert!(collision.object("liveoak_03_rendinst").unwrap().capsules.len() == 1);
    // One object by name gives the same
    assert!(wt.load_object_collision("berlin_city_build_1").unwrap().unwrap() == *house);
    assert!(wt.load_object_collision("no_such_object").unwrap().is_none());
}

/// The conformance fixes: alpha modes from the shader data, winding that follows the node matrices, and no
/// textures for materials nothing uses
#[cfg(feature = "bridge")]
mod conformance {
    use rewrite_bridge::conformance::{check_animated, Expect};
    use rewrite_bridge::{AlphaMode, Provider};

    use super::*;

    const HE_115: &str = "aircrafts/germ_he_115.grp:he_115c_1";

    fn alpha_of(m: &rewrite_bridge::Model, suffix: &str) -> Vec<AlphaMode> {
        m.materials.iter().filter(|x| x.name.ends_with(suffix)).map(|x| x.alpha).collect()
    }

    #[test]
    fn alpha_modes_come_from_the_shader_class_and_atest() {
        let Some(game) = game() else { return };
        let wt = wt_unpack::bridge::WarThunder::new(&game);
        let m = wt.load(HE_115).unwrap();
        // The flash, tracer, propeller-disc and blend materials blend; so does glass
        for suffix in ["_dynamic_alpha_blend", "_aces_weapon_fire", "_propeller_side", "_dynamic_glass_chrome"] {
            let modes = alpha_of(&m, suffix);
            assert!(!modes.is_empty() && modes.iter().all(|a| *a == AlphaMode::Blend), "{suffix}: {modes:?}");
        }
        // Masked chrome carries a shine mask in its alpha: opaque, unless the material says atest=N
        let modes = alpha_of(&m, "_dynamic_masked_chrome_bump");
        assert!(modes.contains(&AlphaMode::Opaque), "{modes:?}");
        assert!(modes.iter().all(|a| matches!(a, AlphaMode::Opaque | AlphaMode::Mask(_))), "{modes:?}");
        // The Sherman's masked hull and ship materials stay opaque, its atest materials cut out
        let sherman = wt.load("tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman").unwrap();
        assert!(alpha_of(&sherman, "_dynamic_masked_tank").iter().all(|a| *a == AlphaMode::Opaque));
        assert!(alpha_of(&sherman, "_dynamic_tank_atest").iter().all(|a| matches!(a, AlphaMode::Mask(_))));
    }

    #[test]
    fn mirrored_and_scaled_parts_wind_with_their_normals() {
        let Some(game) = game() else { return };
        let wt = wt_unpack::bridge::WarThunder::new(&game);
        // radiator2_25 (768 triangles) and prop_side_2 (2) wound against their normals in the stored data; so do
        // the Ontos' left suspension arms bone_suspension_l_02 and _03 (886 each)
        let a = wt.load_animated(HE_115).unwrap();
        let report = check_animated(&a, &Expect::generic());
        assert!(!report.has("geometry.winding_flipped"), "{report}");
        for name in ["radiator2_25", "prop_side_2"] {
            let mesh = a.model.meshes.iter().find(|m| m.name.starts_with(name)).unwrap();
            let (mut agree, mut against) = (0, 0);
            for t in mesh.indices.chunks_exact(3) {
                let [p0, p1, p2] = [0, 1, 2].map(|k| mesh.positions[t[k] as usize]);
                let (e1, e2) = ([0, 1, 2].map(|k| p1[k] - p0[k]), [0, 1, 2].map(|k| p2[k] - p0[k]));
                let g = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
                let n: f32 = (0..3).map(|k| g[k] * (0..3).map(|c| mesh.normals[t[c] as usize][k]).sum::<f32>()).sum();
                if n > 0.0 {
                    agree += 1;
                } else if n < 0.0 {
                    against += 1;
                }
            }
            assert!(agree > 0 && against == 0, "{name}: {agree} with, {against} against");
        }
        // Mirrored tank nodes, skinned tracks and a plane of each kind still wind with their normals
        for id in ["tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman", "tanks/germ_pzkpfw_v_ausf_g_panther.grp:pzkpfw_V_ausf_g_panther", "aircrafts/usa_f_105.grp:f_105d", "aircrafts/fr_so_4050_vautour.grp:so_4050_vautour_2b", "tanks/usa_m50_ontos.grp:m50_ontos", "ships/usa_pc_451.grp:pc_451"] {
            let report = check_animated(&wt.load_animated(id).unwrap(), &Expect::generic());
            assert!(!report.has("geometry.winding_flipped"), "{id}: {report}");
        }
    }

    #[test]
    fn materials_no_mesh_uses_are_not_decoded() {
        let Some(game) = game() else { return };
        // The Chinese Sherman lists two alpha-tested materials its LOD 0 does not draw, and names textures for them
        let pack = game.join("content/base/res/tanks/usa_m4a1_76w_sherman.grp");
        let v = vehicle::load(&game, &pack, "cn_m4a1_76w_sherman", 0).unwrap();
        let used: std::collections::HashSet<usize> = v.meshes.iter().map(|m| m.material).collect();
        let unused: Vec<usize> = (0..v.materials.len()).filter(|i| !used.contains(i)).collect();
        assert!(unused.len() >= 2, "{unused:?}");
        for i in &unused {
            let m = &v.materials[*i];
            assert!(m.color.is_none() && m.metal_rough.is_none() && m.normal.is_none(), "{} was decoded", m.name);
        }
        assert!(used.iter().any(|i| v.materials[*i].color.is_some()));
        // The bridge model leaves them out altogether, and indices still match
        let wt = wt_unpack::bridge::WarThunder::new(&game);
        let a = wt.load_animated("tanks/usa_m4a1_76w_sherman.grp:cn_m4a1_76w_sherman").unwrap();
        assert_eq!(a.model.materials.len(), used.len());
        assert!(a.model.meshes.iter().all(|m| m.material < a.model.materials.len()));
        assert!(!check_animated(&a, &Expect::tank()).has("material.unused"));
        // Several vehicles at once: none has an unused material
        for id in [HE_115, "tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman", "ships/usa_aircraftcarrier_forrestal.grp:aircraftcarrier_forrestal"] {
            let report = check_animated(&wt.load_animated(id).unwrap(), &Expect::generic());
            assert!(!report.has("material.unused"), "{id}: {report}");
        }
    }
}
