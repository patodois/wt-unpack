#![no_main]
//! The telemetry API's JSON replies (`/state`, `/indicators`, `/map_info.json`, `/map_obj.json`) read into the
//! live types, and what the viewer derives from them.
use libfuzzer_sys::fuzz_target;
use wt_unpack::live::{self, Indicators, MapInfo, Snapshot, State};

fuzz_target!(|data: &[u8]| {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) else { return };
    let state = State::from_json(value.clone());
    let ind = Indicators::from_json(value.clone());
    let _ = (ind.gearbox(), ind.unit_name());
    let info = MapInfo::from_json(value.clone());
    let objects = live::map_objects_from_json(value);
    if let Some(b) = info.bounds() {
        let _ = (b.size(), b.to_east_north([1.0, 2.0]), b.to_scene([1.0, 2.0], 3.0), b.heading_deg([1.0, 1.0]));
    }
    for o in &objects {
        let _ = o.is_player();
    }
    let snap = Snapshot::new(Some(state), Some(ind), Some(info), objects, Vec::new());
    let _ = (snap.player_object(), snap.to_log_json(1.0));
    if let Some(p) = snap.player() {
        let _ = p.line();
    }
});
