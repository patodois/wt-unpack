#![no_main]
//! `blk::parse`, with no shared context and with one (the first byte picks; the dictionary is the second half
//! of the input), and every consumer of a parsed block that takes it as it comes.
use libfuzzer_sys::fuzz_target;
use wt_unpack::blk::{self, SharedContext};

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else { return };
    let block = match mode & 3 {
        0 => blk::parse(rest, None),
        mode => {
            let names = (0..32).map(|i| format!("name{i}")).collect();
            let (body, dict) = if mode == 1 { (rest, &rest[..0]) } else { rest.split_at(rest.len() / 2) };
            let ctx = SharedContext { names, dict: (mode != 1).then(|| dict.to_vec()) };
            blk::parse(body, Some(&ctx))
        }
    };
    if let Ok(b) = block {
        let _ = b.to_text();
        let _ = b.to_json();
        let _ = wt_unpack::tank::from_blk("unit", &b);
        let _ = wt_unpack::landclass::LandClassMaterial::from_block("class", &b);
    }
});
