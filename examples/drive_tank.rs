//! Drives a War Thunder tank headlessly through the bridge, the way a game rewrite would: load the model with
//! its sockets, read the rig, then each frame aim the turret and drive, and put the results on the nodes.
//!
//! ```sh
//! cargo run --release --features bridge --example drive_tank -- $GAME [tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman]
//! ```

use rewrite_bridge::{Node, Provider};
use wt_unpack::bridge::WarThunder;
use wt_unpack::rig::{wheel_rotation, TurretAim};
use wt_unpack::tank::Controls;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let game = args.next().ok_or_else(|| anyhow::anyhow!("usage: drive_tank <game folder> [vehicle id]"))?;
    let id = args.next().unwrap_or_else(|| "tanks/usa_m4a1_76w_sherman.grp:m4a1_76w_sherman".to_string());

    let wt = WarThunder::new(game);
    // The model with the sockets for its turret, gun, muzzle, wheels and tracks, and the rig behind them
    let tank = wt.load_animated(&id).map_err(|e| anyhow::anyhow!("{e}"))?;
    let rig = wt.vehicle_rig(&id)?;
    let (turret, gun, muzzle) = (tank.socket("turret"), tank.socket("gun"), tank.socket("muzzle"));
    let main_gun = rig.turret.as_ref().ok_or_else(|| anyhow::anyhow!("{id} has no turret"))?;
    let mut driving = rig.driving_model().ok_or_else(|| anyhow::anyhow!("{id} has no drive train or track gauge"))?;
    let gear = rig.gear.as_ref().expect("a driving model needs the gear");

    // The scene graph a renderer would hold: the model's nodes, whose rotations we set each frame
    let mut nodes: Vec<Node> = tank.model.nodes.clone();
    let mut aim = TurretAim::default();
    let (mut position, mut heading) = ([0.0f32; 2], 0.0f32);
    let dt = 1.0 / 60.0;
    for frame in 0..60 * 20 {
        let t = frame as f32 * dt;
        // 20 seconds: full throttle, steering left for the middle five, while the turret traverses left and the gun lifts
        let steer = if (8.0..13.0).contains(&t) { 1.0 } else { 0.0 };
        driving.step(Controls { throttle: 1.0, steer }, dt);
        aim.step(main_gun, 1.0, 0.5, dt);

        // The hull: speed along the model's +Z, heading about +Y (left positive)
        heading += driving.yaw_rate_rad_s() * dt;
        position[0] += driving.speed_ms() * dt * heading.sin();
        position[1] += driving.speed_ms() * dt * heading.cos();
        // The parts
        if let (Some(t), Some(g)) = (turret, gun) {
            nodes[t.node].rotation = aim.head_rotation();
            nodes[g.node].rotation = aim.gun_rotation();
        }
        for (wheel, angle) in gear.wheels.iter().zip(driving.wheel_angles()) {
            nodes[wheel.node].rotation = wheel_rotation(*angle);
        }
        if frame % 300 == 0 {
            // A renderer adds the belt offsets to the V of the track meshes' UVs
            let belts: Vec<String> = gear.belts.iter().zip(driving.belt_offsets()).map(|(b, o)| format!("{} {o:.2}", b.name)).collect();
            println!(
                "t {t:5.1} s  {:5.1} km/h  heading {:6.1} deg  at ({:6.1}, {:6.1}) m  turret {:5.1} deg  gun {:4.1} deg  belts {}",
                driving.speed_ms() * 3.6,
                heading.to_degrees(),
                position[0],
                position[1],
                aim.yaw_deg,
                aim.pitch_deg,
                belts.join(", ")
            );
        }
    }
    println!("muzzle socket: node {:?}; top speed from the gearing {:.1} km/h", muzzle.map(|m| &tank.model.nodes[m.node].name), rig.speeds.map_or(0.0, |s| s.top_speed_kmh));
    Ok(())
}
