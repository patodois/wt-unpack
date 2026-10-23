# Levels as scenes

How a level becomes a `rewrite_bridge::Scene`. Part of the [wt-unpack README](../README.md).

`WarThunder::load_scene("levels/avg_berlin.bin")` (and `wt-unpack level <file> --scene-stats`) turns a
level into a `rewrite_bridge::Scene`. `Provider::list` lists the levels as `AssetKind::Level`; `load`
of a level id says to use `load_scene`. Axes are the vehicles' (`(x, y, z)` becomes `(z, y, x)`).
Everything in the scene is read from the level, the game's resources or the level's settings; what
cannot be is left out and printed by `--scene-stats` (and listed in `wt_unpack::scene::NOT_APPLIED`).

| In the scene | Where it comes from |
|---|---|
| **Ground** (`terrain`, one mesh per landmesh cell) | The landmesh's collision mesh for the whole map and the `HM2` heightmap for the area it covers finer than the landmesh. Colours: the level's land classes (`lmap` detail data, from the level or from `03FB59C4` resources in the `.grp` packs) have colour maps over `size` metres; they are blended by the level's per-cell weights (`LMp2` is the *physics* material map: its header, the physical material names and the map's layout, is read by `wt-unpack level`, its bitmap is not decoded) and baked into one picture per cell. Weights come from the per-cell textures or the packed atlas, decoded as `land_weight_inc.dshl` reads them. |
| **Placed objects** (one model per distinct rendinst) | `RIGz` placements; each pool name resolves to a `RendInst` resource (class `77F8232F`, not a `DynModel`: LODs of rigid meshes with no skeleton), found by scanning the `.grp` headers; materials and textures from `riDesc.bin`. One LOD per model, chosen as the game's `getBestLodForDist` does from the model's own ranges at `SceneOptions::lod_distance_m` (250 m); the billboard impostor LOD is never chosen. Every placement is an `Instance`: no mesh is copied per instance. |
| **Rotations** | Matrix placements carry their own. Position-only placements (all trees) pick one of the pool's preset rotations by the low 3 bits of their scale: a turn about Y of `id / count` of a circle (`rendInstRotationPalette.cpp`), with `count` stored in the level's pool. Position-only pools without a palette have no rotation anywhere in the data and keep the identity. |
| **Water** (`water`) | One quad at the level settings' `water_level` (`levels/<name>.blk` in `aces.vromfs.bin`) over the landmesh, in the level's `water_aces_color_and_density` colour when it has one, else the bridge's default material. |

Not in the scene, and why:

- **The land shaders' detail layers.** Each class has a splat mask (mega classes) or an index map (the
  biome class) and tiled detail textures (`detailRed`.., `details{}`), which `LandClassMaterial` lists
  with their tile sizes, but the shaders that blend them are public only for the indexed class
  (`make_clipmap.dshl`) and not for the mega and detailed ones, so no blend is invented. The ground is the
  colour maps blended by the weights. Normals and roughness of the ground are likewise names only.
- **Water appearance.** The water shader is not public. 154 of the 160 levels have a `water_level`, but
  only 28 carry `water_aces_color_and_density` (the shader's default is not in the data), and the
  other 126 planes keep the bridge's default material.
- **Sun, sky, fog and lights.** The level stores its place and date (`stars{}`); the time of day and the
  weather come from the mission, and the sky from the atmosphere shader, so `Scene::lights` and
  `Scene::environment` are empty.
- **Object shaders.** Layered, perlin and decal materials use their first texture, decals and impostor
  billboards are left out, and wind, interior mapping and normal and reflectance maps are not applied.
- **Tree leaves.** The leaf cards of `rendinst_facing_leaves` materials are left out. A card is four vertices
  around one centre (`HALF4`, in the object's space) plus a corner offset the vertex shader applies in the
  camera's frame, so the data has no corners in the object's space to place; the trees keep their trunks and
  branch meshes. `RiModel::leaf_vertex` decodes the centres and offsets for readers that want them, and
  `cargo run --release --example check_rendinst_bounds` checks every placed-object model in the game.
- **Blocks not decoded:** `SCN`, `FRT`, `Obj`, `Wire`, splines, `wt3d`, `lmct`, and the scatter of
  objects by land class (`obj_plant_generate`), which the game generates at load time.

The conversion is bounded by budgets that are not game data and live in `SceneOptions`: the colour map
memory, the baked ground pictures (2 m per pixel where the heightmap is finer than the landmesh, 16 m
elsewhere, spacings growing together past 384 MB), the terrain vertex count, and the object textures
(256 px on the longest side). Berlin (512,641 placements of 452 models) loads in about 7 s with about 850 MB
of scene data and a peak near 1.3 GB (see [MEMORY.md](MEMORY.md)); the collision of the objects is separate
(`load_level_collision`, see "Level collision" in [BRIDGE.md](BRIDGE.md#level-collision)). `cargo run --release --features bridge --example render_scene -- $GAME levels/avg_berlin.bin
out.png --eye 2150,90,1380 --at 2400,40,1560` draws a scene from a camera with a small software
rasteriser (albedo with a headlight on a flat background, since the game's sun and sky are not in the
data).
