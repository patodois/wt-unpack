# Memory of loading levels and vehicles

Why a process that loaded a level still shows its memory, and what the provider does about it. Part of the [wt-unpack README](../README.md).

Loading Berlin's scene (`load_scene("levels/avg_berlin.bin")`, 512,641 placements) peaked near 1.1 GB of live
allocations (the scene itself is 830 MB), and a program that dropped the scene still showed about 1.2 GB resident.
The loader keeps nothing alive for that: with the scene dropped, no allocation of the level load is left (the
allocator's statistics say 0 MB live). The memory is **retained by the system allocator**: freed pages are handed
back lazily, and on macOS they stay counted in the process's resident size and footprint until the system needs
them. What the provider does about it:

- it owns every cache it uses (the texture and pack indexes, the model descriptions of the vehicles, the unit
  index; about 26 MB after a level) and nothing is global; dropping the `WarThunder` value, or calling
  `clear_caches()`, frees them, and the pack indexes are shared between vehicle and level loads;
- `clear_caches()` and the end of every level load ask the allocator to release its free pages
  (`wt_unpack::memory::release_freed_memory`: `malloc_zone_pressure_relief` on macOS, `malloc_trim` on glibc).
  This lowers the resident size on a quiet system, but macOS can keep reporting the pages until it reclaims them,
  so the footprint of a process that loaded a level stays near its peak. `drop(scene); wt.clear_caches();` is
  what a consumer should do when it is done with a level;
- the peak is lower. The loader no longer keeps a second copy of every object texture (a texture is decoded once
  and the later materials copy it from the first that holds it), parses only the needed entries of `riDesc.bin`
  instead of holding its whole tree (about 200 MB), shares the paths of the texture index, and trims the meshes'
  spare capacity. The scene is byte for byte the same (checked with a hash of every mesh, image and
  transform).

Berlin, measured with `/usr/bin/time -l` on a Mac that was also busy with other work, so the OS-reported
numbers move between runs:

| Berlin | Before | After |
|---|---|---|
| Peak live allocations | 1,110 MB | 870 MB |
| Scene data at the end | 830 MB | 826 MB |
| Peak memory footprint | 1,590 to 1,610 MB | 1,150 to 1,370 MB |
| Maximum resident set size | 1,440 to 1,760 MB | 800 to 1,370 MB |
| Live after dropping the scene | 0 MB | 26 MB (the indexes), 0 MB after `clear_caches()` |

```sh
/usr/bin/time -l cargo run --release --features bridge --example scene_memory -- $GAME levels/avg_berlin.bin
```

`examples/scene_memory.rs` prints the live, resident and footprint sizes at each step (and a hash of the whole
scene with `WT_DIGEST=1`, to see that a change leaves it alone); `tests/memory.rs` checks the live numbers (macOS).
The peak is still the scene plus the allocator's fragmentation: the object textures are 256 px copies, one per
material, because a bridge `Material` owns its image.

## Vehicles (`check-bridge`)

`wt-unpack check-bridge` loads vehicles one after another, which looked like a leak: a full run showed the
resident size climbing from 0.6 to 3.0 GB. It is not one. Measured with `examples/vehicle_memory.rs`, which prints
the allocator's live bytes next to the resident size and the footprint after every vehicle:

- the live bytes are 140 MB after every vehicle (the provider's indexes) and 0 after `clear_caches()`: nothing
  accumulates;
- the resident size follows the **heaviest vehicle loaded recently**. Textures are baked at full size, camouflage
  included, and a vehicle holds them all until it is dropped: the B-52H (869,000 triangles) takes 2.2 GB, a
  large ship 1.5 to 3 GB. macOS keeps the freed pages counted as resident until it needs them (they fell back to
  250 to 600 MB some seconds later), and `release_freed_memory()` after every vehicle, or `clear_caches()`
  every 10 vehicles, made no difference to the peak (2.5 to 2.9 GB over 350 aircraft, in all three variants).

So the fix is not a cache: `check-bridge` now checks geometry, skeleton and materials **without baking textures**
unless `--textures` is given (`WarThunder::with_textures(false)` in the library), and it releases the allocator's
free pages after every vehicle and the provider's caches every 250.

| `check-bridge` | Before (always textures) | After (default) | After with `--textures` |
|---|---|---|---|
| 350 aircraft (numbers 600 to 950, with the B-52) | 88 s, peak 2.5 GB resident, 3.2 GB footprint | 14 s, peak 0.6 GB, 0.5 GB footprint | 94 s, peak 3.1 GB, 3.4 GB footprint |
| All 3,245 vehicles | more than an hour, 0.6 to 3.5 GB | 3 min 22 s, 0.3 to 0.9 GB (peak 0.99 GB, footprint 0.84 GB) | not run in full; 1,300 vehicles took 20 minutes |

Measured with `/usr/bin/time -l` on a Mac with 16 GB.
