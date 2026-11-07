//! Looks textures up by name in the game's `.dxp.bin` packs and decodes them, optionally shrunk.
//!
//! A name like `berlin_city_b*` stands for the texture `berlin_city_b` (the part before the `*`),
//! preferring the high-quality copy (`berlin_city_b$hq`) when a pack has one.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use wt_formats::dxp::DxpPack;

use crate::vehicle::{self, TextureIndex};

/// An 8-bit RGBA image, rows top to bottom
#[derive(Clone)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct TextureLibrary {
    index: Arc<TextureIndex>,
}

/// The name a texture is filed under: no `*` suffix, no `?options`
pub fn texture_key(name: &str) -> &str {
    name.split(['*', '?']).next().unwrap_or(name)
}

impl TextureLibrary {
    /// Indexes every pack under the game folder (patched packs win)
    pub fn open(game: &Path) -> Result<Self> {
        Ok(TextureLibrary { index: Arc::new(vehicle::texture_index(game)?) })
    }

    /// A library over an index that is already built, e.g. the one a [`vehicle::Library`] uses
    pub fn from_index(index: Arc<TextureIndex>) -> Self {
        TextureLibrary { index }
    }

    /// The index this library looks textures up in
    pub fn index(&self) -> &Arc<TextureIndex> {
        &self.index
    }

    pub fn contains(&self, name: &str) -> bool {
        self.pack_of(name).is_some()
    }

    fn pack_of(&self, name: &str) -> Option<(String, &Path)> {
        let key = texture_key(name);
        [format!("{key}$hq"), key.to_string()].into_iter().find_map(|n| self.index.get(&n).map(|p| (n, &**p)))
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
    if img.width == 0 || img.height == 0 { return img; }
    while img.width.max(img.height) > max_side.max(1) {
        let (w, h) = ((img.width / 2).max(1) as usize, (img.height / 2).max(1) as usize);
        let src_w = img.width as usize;
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                for c in 0..4 {
                    let at = |dx: usize, dy: usize| u32::from(img.rgba[(((2 * y + dy).min(img.height as usize - 1)) * src_w + (2 * x + dx).min(src_w - 1)) * 4 + c]);
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

#[cfg(test)]
mod thin_texture_regression {
    use super::*;
    #[test]
    fn single_pixel_dimensions_still_obey_max_side() {
        for (width, height) in [(8, 1), (1, 8), (16, 2), (2, 16)] {
            let image = Rgba {
                width,
                height,
                rgba: vec![100; (width * height * 4) as usize],
            };
            let small = shrink(image, 2);
            assert!(small.width.max(small.height) <= 2);
            assert!(small.rgba.iter().all(|&b| b == 100));
        }
    }
}

#[cfg(test)]
mod empty_texture_compatibility {
    use super::*;
    #[test]
    fn empty_images_keep_their_dimensions() {
        for (width,height) in [(0,8),(8,0),(0,0)] {
            let out=shrink(Rgba {width,height,rgba:vec![]},2);
            assert_eq!((out.width,out.height),(width,height));
            assert!(out.rgba.is_empty());
        }
    }
}

#[cfg(test)]
mod minimum_extent_contract {
    use super::*;
    #[test]
    fn a_single_pixel_axis_survives_every_reduction() {
        for (width,height) in [(1,8),(8,1)] {
            let image=Rgba{width,height,rgba:vec![17;32]};let small=shrink(image,1);
            assert_eq!((small.width,small.height),(1,1));assert_eq!(small.rgba,[17;4]);
        }
    }
    #[test]
    fn a_zero_limit_is_a_no_op_for_a_single_pixel() {
        const CHILD:&str="SINGLE_PIXEL_LIMIT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let result=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","texlib::minimum_extent_contract::a_zero_limit_is_a_no_op_for_a_single_pixel","--nocapture"]).env(CHILD,"1").output().unwrap();
            assert!(result.status.success(),"child status {}: {}",result.status,String::from_utf8_lossy(&result.stderr));return;
        }
        // A missing clamp must fail in bounded time rather than hang the test process.
        std::thread::spawn(||{std::thread::sleep(std::time::Duration::from_secs(3));std::process::exit(124);});
        let image=Rgba{width:1,height:1,rgba:vec![17;4]};let result=shrink(image,0);
        assert_eq!((result.width,result.height),(1,1));assert_eq!(result.rgba,[17;4]);
    }

}
