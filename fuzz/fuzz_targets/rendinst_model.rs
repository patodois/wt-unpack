#![no_main]
//! `RiModel::parse`: a RendInst resource, and reading the vertices and leaf cards its tables point at.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(m) = wt_unpack::rendinst::RiModel::parse(data) else { return };
    let _ = m.lod_for_distance(100.0);
    for (l, lod) in m.lods.iter().enumerate().take(4) {
        let _ = m.triangles(l);
        for e in lod.elems.iter().take(8) {
            let _ = m.leaf_cards(e);
        }
    }
    for v in m.vdata.iter().take(4) {
        for i in [0usize, 1, 7, 1000, usize::MAX] {
            let _ = m.vertex(v, i);
            let _ = m.leaf_vertex(v, i);
        }
    }
});
