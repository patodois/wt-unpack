//! Live telemetry: parsing and the coordinate transform on hand-written samples, and (with the `live`
//! feature) the client and the CLI against the mock server. No game needed:
//! `cargo test --features live --test live`.

use serde_json::{json, Value};
use wt_unpack::live::{self, mock, Army, Indicators, MapBounds, MapInfo, MapObject, Snapshot, State, Status};

fn parse(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn close3(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter().zip(&b).all(|(x, y)| close(*x, *y))
}

fn sample_tank() -> Snapshot {
    Snapshot::new(
        Some(State::from_json(parse(mock::TANK_STATE))),
        Some(Indicators::from_json(parse(mock::TANK_INDICATORS))),
        Some(MapInfo::from_json(parse(mock::MAP_INFO))),
        live::map_objects_from_json(parse(mock::MAP_OBJ)),
        Vec::new(),
    )
}

#[test]
fn tank_indicators_parse_and_keep_unknown_keys() {
    let ind = Indicators::from_json(parse(mock::TANK_INDICATORS));
    assert!(ind.valid);
    assert_eq!(ind.army, Some(Army::Tank));
    assert_eq!(ind.unit.as_deref(), Some("tankModels/us_m4a1_76w_sherman"));
    assert_eq!(ind.unit_name(), Some("us_m4a1_76w_sherman"));
    assert_eq!((ind.speed, ind.gear, ind.rpm), (Some(23.4), Some(3.0), Some(2100.0)));
    assert_eq!(ind.compass_deg, None);
    assert_eq!(ind.raw["crew_total"], 5);
    assert_eq!(ind.raw.len(), 11);
}

#[test]
fn air_state_parses_keys_with_units() {
    let st = State::from_json(parse(mock::AIR_STATE));
    assert!(st.valid);
    assert_eq!(st.altitude_m, Some(1520.0));
    assert_eq!(st.tas_kmh, Some(512.0));
    assert_eq!(st.ias_kmh, Some(470.0));
    assert_eq!(st.mach, Some(0.43));
    assert_eq!(st.throttle_pct, Some(100.0));
    assert_eq!(st.rpm, Some(2950.0));
    assert_eq!(st.landing_gear_pct, Some(0.0));
    assert_eq!((st.fuel_kg, st.fuel_max_kg), (Some(310.0), Some(402.0)));
    assert_eq!(st.raw["pitch 1, deg"], 42.0);
}

#[test]
fn parsing_is_lenient() {
    // Numbers as strings, wrong types, missing keys, odd top-level values
    let ind = Indicators::from_json(json!({"valid": 1, "army": "tank", "speed": " 12.5 ", "gear": [1], "type": ""}));
    assert!(ind.valid);
    assert_eq!(ind.speed, Some(12.5));
    assert_eq!(ind.gear, None);
    assert_eq!(ind.unit, None);
    let st = State::from_json(json!(["not", "an", "object"]));
    assert!(!st.valid && st.raw.is_empty());
    let st = State::from_json(json!({"H, m": null, "TAS, km/h": "fast", "new key, %": 3}));
    assert_eq!((st.valid, st.altitude_m, st.tas_kmh), (false, None, None));
    assert_eq!(st.raw["new key, %"], 3);
    assert_eq!(Indicators::from_json(json!({"army": "submarine"})).army, Some(Army::Other("submarine".into())));
    // map_obj: a non-list gives nothing, non-objects in the list are skipped
    assert!(live::map_objects_from_json(json!({"valid": false})).is_empty());
    let objs = live::map_objects_from_json(json!([1, {"icon": "Player", "x": 0.5}, "x"]));
    assert_eq!(objs.len(), 1);
    assert!(objs[0].is_player());
    assert_eq!(objs[0].pos, None, "x without y is no position");
}

#[test]
fn map_objects_parse() {
    let objs = live::map_objects_from_json(parse(mock::MAP_OBJ));
    assert_eq!(objs.len(), 5);
    let me = objs.iter().find(|o| o.is_player()).unwrap();
    assert_eq!(me.kind, "ground_model");
    assert_eq!(me.rgb, Some([250, 200, 30]));
    assert_eq!(me.pos, Some([0.6, 0.25]));
    assert_eq!(me.dir, Some([0.6, -0.8]));
    assert!(!me.blink);
    let field = objs.iter().find(|o| o.kind == "airfield").unwrap();
    assert_eq!((field.pos, field.start, field.end), (None, Some([0.1, 0.95]), Some([0.3, 0.95])));
}

#[test]
fn map_info_bounds() {
    let info = MapInfo::from_json(parse(mock::MAP_INFO));
    assert!(info.valid);
    assert_eq!(info.map_generation, Some(7));
    assert_eq!(info.grid_zero, Some([-1500.0, 2300.0]));
    let b = info.bounds().unwrap();
    assert_eq!(b.size(), [4000.0, 4000.0]);
    // Missing, empty or inverted bounds give none
    assert!(MapInfo::from_json(json!({"valid": false})).bounds().is_none());
    assert!(MapInfo::from_json(json!({"map_min": [0, 0], "map_max": [0, 100]})).bounds().is_none());
    assert!(MapInfo::from_json(json!({"map_min": [10, 10], "map_max": [-10, 20]})).bounds().is_none());
    assert!(MapInfo::from_json(json!({"map_min": [0, 0], "map_max": [100, 100]})).bounds().is_some(), "integers are fine");
}

#[test]
fn transform_puts_north_on_z_and_west_on_x() {
    // A non-square, off-centre map catches swapped axes and offsets
    let b = MapBounds { min: [-1000.0, 200.0], max: [3000.0, 2200.0] };
    // Top-left of the image is the north-west corner, bottom-right the south-east
    assert_eq!(b.to_east_north([0.0, 0.0]), [-1000.0, 2200.0]);
    assert_eq!(b.to_east_north([1.0, 1.0]), [3000.0, 200.0]);
    assert!(close3(b.to_scene([0.0, 0.0], 5.0), [1000.0, 5.0, 2200.0]));
    assert!(close3(b.to_scene([1.0, 1.0], 0.0), [-3000.0, 0.0, 200.0]));
    assert!(close3(b.to_scene([0.5, 0.5], 0.0), [-1000.0, 0.0, 1200.0]));
    // Headings: up on the image is north, right is east
    assert!(close(b.heading_deg([0.0, -1.0]).unwrap(), 0.0));
    assert!(close(b.heading_deg([1.0, 0.0]).unwrap(), 90.0));
    assert!(close(b.heading_deg([0.0, 1.0]).unwrap(), 180.0));
    assert!(close(b.heading_deg([-1.0, 0.0]).unwrap(), 270.0));
    assert_eq!(b.heading_deg([0.0, 0.0]), None);
    // The axes form a right-handed basis: X x Y = Z with X = west, Y = up, Z = north
    let (west, up) = ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    let cross = [west[1] * up[2] - west[2] * up[1], west[2] * up[0] - west[0] * up[2], west[0] * up[1] - west[1] * up[0]];
    assert_eq!(cross, [0.0, 0.0, 1.0]);
    assert!(close3(live::heading_to_scene_dir(0.0), [0.0, 0.0, 1.0]));
    assert!(close3(live::heading_to_scene_dir(90.0), [-1.0, 0.0, 0.0]), "east is -X");
}

#[test]
fn heading_agrees_with_movement_on_the_map() {
    let b = MapBounds { min: [-1000.0, 200.0], max: [3000.0, 2200.0] };
    for d in [[0.3, -0.7], [-0.9, 0.1], [0.5, 0.5], [-0.2, -0.2]] {
        let p = [0.4, 0.6];
        let (a, z) = (b.to_scene(p, 0.0), b.to_scene([p[0] + d[0] * 1e-3, p[1] + d[1] * 1e-3], 0.0));
        let step = [z[0] - a[0], z[1] - a[1], z[2] - a[2]];
        let len = (step[0] * step[0] + step[2] * step[2]).sqrt();
        let h = b.heading_deg(d).unwrap();
        let dir = live::heading_to_scene_dir(h);
        assert!(close3([step[0] / len, 0.0, step[2] / len], dir), "{d:?}: moved {step:?}, heading {h} says {dir:?}");
        // A model facing +Z, turned by the yaw about +Y, faces the same way
        let yaw = live::heading_to_yaw(h);
        assert!(close3([yaw.sin(), 0.0, yaw.cos()], dir));
    }
}

#[test]
fn tank_snapshot_combines_endpoints() {
    let snap = sample_tank();
    assert_eq!(snap.status, Status::InMatch);
    let p = snap.player().unwrap();
    assert_eq!(p.army, Some(Army::Tank));
    assert_eq!(p.speed_kmh, Some(23.4));
    assert_eq!(p.gear, Some(3.0));
    assert_eq!(p.landing_gear_pct, None);
    assert_eq!(p.throttle_pct, None, "the sample tank reports no throttle");
    // x 0.6, y 0.25 on [-1500, -1700]..[2500, 2300]: 900 m east, 1300 m north
    assert!(close3(p.position.unwrap(), [-900.0, 0.0, 1300.0]));
    assert!(close(p.heading_deg.unwrap(), 0.6f64.atan2(0.8).to_degrees()));
    assert_eq!(p.line(), "tank us_m4a1_76w_sherman  23.4 km/h  hdg 037  pos -900.0 0.0 1300.0 m  thr -  gear 3");
    let log = snap.to_log_json(1.5);
    assert_eq!(log["status"], "in_match");
    assert_eq!(log["vehicle"]["position_m"], json!([-900.0, 0.0, 1300.0]));
    assert_eq!(log["player_map_obj"]["icon"], "Player");
    assert!(log.get("errors").is_none());
}

#[test]
fn air_snapshot_uses_state_and_compass() {
    let mut objs = live::map_objects_from_json(parse(mock::MAP_OBJ));
    objs[0].kind = "aircraft".into();
    let snap = Snapshot::new(
        Some(State::from_json(parse(mock::AIR_STATE))),
        Some(Indicators::from_json(parse(mock::AIR_INDICATORS))),
        Some(MapInfo::from_json(parse(mock::MAP_INFO))),
        objs,
        Vec::new(),
    );
    let p = snap.player().unwrap();
    assert_eq!(p.army, Some(Army::Air));
    assert_eq!(p.speed_kmh, Some(512.0), "true airspeed, not the indicator");
    assert_eq!(p.heading_deg, Some(271.5), "the compass wins over the map arrow");
    assert_eq!(p.throttle_pct, Some(100.0));
    assert_eq!((p.gear, p.landing_gear_pct), (None, Some(0.0)));
    assert!(close3(p.position.unwrap(), [-900.0, 1520.0, 1300.0]));
    assert_eq!(p.line(), "air p-51d-5  512.0 km/h  hdg 272  pos -900.0 1520.0 1300.0 m  thr 100%  ldg gear 0%");
}

#[test]
fn status_from_valid_flags() {
    let invalid = || Some(Indicators::from_json(parse(mock::NOT_IN_MATCH)));
    assert_eq!(Snapshot::new(None, None, None, vec![], vec![]).status, Status::GameNotRunning);
    let hangar = Snapshot::new(Some(State::from_json(parse(mock::NOT_IN_MATCH))), invalid(), None, vec![], vec![]);
    assert_eq!(hangar.status, Status::NotInMatch);
    assert!(hangar.player().is_none());
    // Only /state valid (an aircraft with /indicators failing) still counts as a match
    let only_state = Snapshot::new(Some(State::from_json(parse(mock::AIR_STATE))), None, None, vec![], vec!["indicators: HTTP 500".into()]);
    assert_eq!(only_state.status, Status::InMatch);
    let p = only_state.player().unwrap();
    assert_eq!((p.army, p.position, p.heading_deg), (None, None, None));
}

#[test]
fn partial_data_leaves_gaps_not_errors() {
    // No player icon (dead, or the minimap is off): everything but position and map heading remains
    let objs: Vec<MapObject> = live::map_objects_from_json(parse(mock::MAP_OBJ)).into_iter().filter(|o| !o.is_player()).collect();
    let snap = Snapshot::new(None, Some(Indicators::from_json(parse(mock::TANK_INDICATORS))), Some(MapInfo::from_json(parse(mock::MAP_INFO))), objs, vec![]);
    let p = snap.player().unwrap();
    assert_eq!((p.position, p.heading_deg, p.speed_kmh), (None, None, Some(23.4)));
    assert_eq!(p.line(), "tank us_m4a1_76w_sherman  23.4 km/h  hdg -  pos -  thr -  gear 3");
}

#[cfg(feature = "live")]
mod client {
    use super::*;
    use std::net::TcpListener;
    use std::time::{Duration, Instant};
    use wt_unpack::live::{mock::MockServer, Client, FetchError, Poller};

    fn client(server: &MockServer) -> Client {
        Client::new(&server.url(), Duration::from_millis(500))
    }

    #[test]
    fn tank_match_over_http() {
        let server = MockServer::start(mock::tank_match).unwrap();
        let c = client(&server);
        assert!(c.state().is_ok());
        let snap = c.snapshot(None);
        assert_eq!(snap.status, Status::InMatch);
        assert!(snap.errors.is_empty(), "{:?}", snap.errors);
        assert_eq!(snap.objects.len(), 5);
        assert!(close3(snap.player().unwrap().position.unwrap(), [-900.0, 0.0, 1300.0]));
        assert!(matches!(c.json("/nope"), Err(FetchError::Status(404))));
    }

    #[test]
    fn game_not_running() {
        // A port that was free a moment ago: nothing listens there now
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let c = Client::new(&format!("http://127.0.0.1:{port}"), Duration::from_millis(500));
        let t = Instant::now();
        let snap = c.snapshot(None);
        assert_eq!(snap.status, Status::GameNotRunning);
        assert!(snap.player().is_none());
        assert_eq!(snap.errors.len(), 1, "stops after the first unreachable endpoint: {:?}", snap.errors);
        assert!(snap.errors[0].starts_with("indicators: unreachable"));
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(matches!(c.state(), Err(FetchError::Unreachable(_))));
    }

    #[test]
    fn hung_server_times_out() {
        // Accepts connections and never answers
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let held = std::thread::spawn(move || {
            let conns: Vec<_> = listener.incoming().take(1).collect();
            std::thread::sleep(Duration::from_secs(2));
            drop(conns);
        });
        let t = Instant::now();
        let snap = Client::new(&url, Duration::from_millis(300)).snapshot(None);
        assert_eq!(snap.status, Status::GameNotRunning);
        assert!(t.elapsed() < Duration::from_millis(1500), "took {:?}", t.elapsed());
        drop(held);
    }

    #[test]
    fn hangar_is_not_a_match() {
        let server = MockServer::start(mock::hangar).unwrap();
        let snap = client(&server).snapshot(None);
        assert_eq!(snap.status, Status::NotInMatch);
        assert!(snap.player().is_none());
        assert!(snap.objects.is_empty() && snap.map_info.is_none(), "map endpoints are skipped outside a match");
    }

    #[test]
    fn partial_responses() {
        // map_obj missing, map_info garbled, /state an HTML error page
        let server = MockServer::start(|path: &str| match path {
            "/indicators" => mock::json(mock::TANK_INDICATORS),
            "/state" => Some((500, "text/html", b"<html>oops</html>".to_vec())),
            "/map_info.json" => Some((200, "application/json", b"{\"map_min\": [0, ".to_vec())),
            _ => None,
        })
        .unwrap();
        let snap = client(&server).snapshot(None);
        assert_eq!(snap.status, Status::InMatch);
        assert_eq!(snap.errors.len(), 3, "{:?}", snap.errors);
        assert!(snap.errors.iter().any(|e| e == "state: HTTP 500"));
        assert!(snap.errors.iter().any(|e| e.starts_with("map_info: bad JSON")));
        assert!(snap.errors.iter().any(|e| e == "map_obj: HTTP 404"));
        let p = snap.player().unwrap();
        assert_eq!((p.speed_kmh, p.gear, p.position), (Some(23.4), Some(3.0), None));
        assert_eq!(snap.to_log_json(0.0)["errors"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn poller_follows_the_game_in_and_out_of_a_match() {
        let server = MockServer::start(mock::tank_match).unwrap();
        let mut poller = Poller::new(client(&server), 50.0);
        let first = poller.next().unwrap();
        assert_eq!(first.status, Status::InMatch);
        // The cached map_info is reused even if the endpoint now fails
        server.set_handler(|p: &str| if p == "/map_info.json" { None } else { mock::tank_match(p) });
        let second = poller.next().unwrap();
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert!(second.player().unwrap().position.is_some());
        // Back to the hangar, then into a new match: map_info is fetched again
        server.set_handler(mock::hangar);
        assert_eq!(poller.next().unwrap().status, Status::NotInMatch);
        server.set_handler(|p: &str| if p == "/map_info.json" { None } else { mock::tank_match(p) });
        let fresh = poller.next().unwrap();
        assert_eq!(fresh.errors, vec!["map_info: HTTP 404".to_string()]);
        drop(server);
        assert_eq!(poller.next().unwrap().status, Status::GameNotRunning);
    }

    #[test]
    fn poller_keeps_its_rate() {
        let server = MockServer::start(mock::tank_match).unwrap();
        let t = Instant::now();
        let n = Poller::new(client(&server), 20.0).take(6).count();
        let took = t.elapsed();
        assert_eq!(n, 6);
        // Five waits of 50 ms between six polls
        assert!(took >= Duration::from_millis(250) && took < Duration::from_millis(1000), "took {took:?}");
    }

    #[test]
    fn map_image_bytes() {
        let server = MockServer::start(|p: &str| (p == "/map.img").then(|| (200, "image/jpeg", vec![0xFF, 0xD8, 0xFF, 0xD9]))).unwrap();
        assert_eq!(client(&server).map_image(Some(7)).unwrap(), vec![0xFF, 0xD8, 0xFF, 0xD9]);
    }

    #[test]
    fn driving_demo_moves() {
        let server = MockServer::start(mock::driving_demo()).unwrap();
        let c = client(&server);
        let a = c.snapshot(None).player().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let b = c.snapshot(None).player().unwrap();
        let (pa, pb) = (a.position.unwrap(), b.position.unwrap());
        assert!(pb[2] > pa[2], "starts heading north (+Z): {pa:?} -> {pb:?}");
        assert!(b.heading_deg.unwrap() > 300.0 || b.heading_deg.unwrap() < 1.0, "turning left from north");
    }

    #[test]
    fn cli_prints_lines_and_logs_json() {
        let dir = std::env::temp_dir().join(format!("wt-unpack-live-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("live.jsonl");
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_wt-unpack"))
            .args(["live", "--mock", "--hz", "20", "--count", "3", "--log"])
            .arg(&log)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8(out.stdout).unwrap();
        let lines: Vec<_> = stdout.lines().collect();
        assert_eq!(lines.len(), 3, "{stdout}");
        assert!(lines.iter().all(|l| l.starts_with("tank us_m4a1_76w_sherman ") && l.contains(" km/h  hdg ")));
        let logged: Vec<Value> = std::fs::read_to_string(&log).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(logged.len(), 3);
        assert!(logged.iter().all(|l| l["status"] == "in_match" && l["vehicle"]["position_m"].is_array()));
        std::fs::remove_dir_all(&dir).unwrap();

        // Nothing on the port: one waiting line per status, then it stops at the count
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_wt-unpack"))
            .args(["live", "--url", &format!("http://127.0.0.1:{port}"), "--hz", "20", "--count", "2"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8(out.stdout).unwrap(), format!("waiting for the game on http://127.0.0.1:{port} (not running)\n"));
    }
}
