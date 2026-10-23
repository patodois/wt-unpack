//! Looks textures up by name in the game's `.dxp.bin` packs and decodes them, optionally shrunk.
//!
//! A name like `berlin_city_b*` stands for the texture `berlin_city_b` (the part before the `*`),
//! preferring the high-quality copy (`berlin_city_b$hq`) when a pack has one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use wt_blk::dxp_and_grp::dxp::DxpPack;

use crate::vehicle;

/// An 8-bit RGBA image, rows top to bottom
#[derive(Clone)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct TextureLibrary {
    index: HashMap<String, PathBuf>,
}

/// The name a texture is filed under: no `*` suffix, no `?options`
pub fn texture_key(name: &str) -> &str {
    name.split(['*', '?']).next().unwrap_or(name)
}

impl TextureLibrary {
    /// Indexes every pack under the game folder (patched packs win)
    pub fn open(game: &Path) -> Result<Self> {
        Ok(TextureLibrary { index: vehicle::texture_index(game)? })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.pack_of(name).is_some()
    }

    fn pack_of(&self, name: &str) -> Option<(String, &PathBuf)> {
        let key = texture_key(name);
        [format!("{key}$hq"), key.to_string()].into_iter().find_map(|n| self.index.get(&n).map(|p| (n, p)))
    }

    /// The size of a texture in pixels, from its header; `None` when no pack has it
    pub fn size(&self, name: &str) -> Result<Option<(u32, u32)>> {
        let Some((full, path)) = self.pack_of(name) else { return Ok(None) };
        let file = vehicle::map(path)?;
        let pack = DxpPack::parse(&file).map_err(|e| anyhow::anyhow!("{}: {e:?}", path.display()))?;
        Ok(pack.entries.iter().find(|e| e.name.split('*').next() == Some(full.as_str()) && !e.header.is_empty()).map(|e| (u32::from(e.header.width), u32::from(e.header.height))))
    }

    /// Decodes the top mip of a texture, shrunk by a power of two (box filter) until neither side exceeds
    /// `max_side`. `None` when no pack has the texture.
    pub fn decode(&self, name: &str, max_side: u32) -> Result<Option<Rgba>> {
        let Some((full, path)) = self.pack_of(name) else { return Ok(None) };
        let file = vehicle::map(path)?;
        let pack = DxpPack::parse(&file).map_err(|e| anyhow::anyhow!("{}: {e:?}", path.display()))?;
        let i = pack
            .entries
            .iter()
            .position(|e| e.name.split('*').next() == Some(full.as_str()) && !e.header.is_empty())
            .with_context(|| format!("texture {full} vanished from {}", path.display()))?;
        let texture = pack.texture(i).map_err(|e| anyhow::anyhow!("{name}: {e:?}"))?;
        let img = Rgba { width: texture.width(), height: texture.height(), rgba: texture.to_rgba().map_err(|e| anyhow::anyhow!("{name}: {e:?}"))? };
        Ok(Some(shrink(img, max_side)))
    }
}

/// Halves an image (2 x 2 box filter) until neither side exceeds `max_side`
pub fn shrink(mut img: Rgba, max_side: u32) -> Rgba {
    while img.width.max(img.height) > max_side.max(1) && img.width >= 2 && img.height >= 2 {
        let (w, h) = ((img.width / 2) as usize, (img.height / 2) as usize);
        let src_w = img.width as usize;
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                for c in 0..4 {
                    let at = |dx: usize, dy: usize| u32::from(img.rgba[((2 * y + dy) * src_w + 2 * x + dx) * 4 + c]);
                    out[(y * w + x) * 4 + c] = ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8;
                }
            }
        }
        img = Rgba { width: w as u32, height: h as u32, rgba: out };
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_drop_the_star_and_options() {
        assert_eq!(texture_key("berlin_city_b*"), "berlin_city_b");
        assert_eq!(texture_key("avg_berlin_lightmap*?AmQ0-0-0"), "avg_berlin_lightmap");
        assert_eq!(texture_key("plain"), "plain");
    }

    #[test]
    fn shrinking_averages_blocks() {
        let img = Rgba { width: 4, height: 2, rgba: (0..8).flat_map(|i| [i * 10, 0, 0, 255]).collect() };
        let s = shrink(img, 2);
        assert_eq!((s.width, s.height), (2, 1));
        // The left 2 x 2 block holds reds 0, 10, 40, 50 (row-major pixels 0, 1, 4, 5)
        assert_eq!(s.rgba[0], 25);
        assert_eq!(s.rgba[4], 45);
    }
}
