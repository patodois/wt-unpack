#![no_main]
//! `Collision::parse`: a collision resource.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(c) = wt_unpack::collision::Collision::parse("fuzz", data) {
        let _ = c.triangles();
        for n in &c.nodes {
            let _ = (n.traceable(), n.physics_collidable(), n.to_object([1.0, 2.0, 3.0]));
        }
    }
});
