//! Readers for War Thunder's Dagor Engine game data and FMOD sound banks, and a vehicle assembler
//! built on them.

#[cfg(test)]
mod alloc_probe;
pub mod blk;
#[cfg(feature = "bridge")]
pub mod bridge;
pub mod bytes;
pub mod camo;
pub mod collision;
pub mod gear;
pub mod gltf;
pub mod landclass;
pub mod level;
pub mod memory;
pub mod rendinst;
pub mod rig;
pub mod rodb;
#[cfg(feature = "bridge")]
pub mod level_collision;
#[cfg(feature = "bridge")]
pub mod scene;
pub mod live;
pub mod sound;
pub mod tank;
pub mod terrain;
pub mod texlib;
pub mod vehicle;
pub mod vromfs;

#[cfg(test)]
#[global_allocator]
static PROBE: alloc_probe::Probe = alloc_probe::Probe;
