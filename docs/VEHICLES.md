# Vehicles: wheels, tracks, materials and winding

How a vehicle is assembled and what moves when it drives. Part of the [wt-unpack README](../README.md); the bridge side is in [BRIDGE.md](BRIDGE.md).

## Wheels and tracks

`wt_unpack::gear` holds what moves when a tracked vehicle drives, as data and pure functions that the
viewer and the bridge provider both use. All of it comes from the game files:

- **Wheels** are the `wheel_<l|r>_*` nodes. Each turns about the lateral axis through its pivot by
  `distance / radius` for the distance its track travels. Radii come from the unit BLK
  (`suspension.defaultGearRadius`, `topGearRadius`, `frontBackGearRadius` and
  `mechanics.driveGearRadius`), and from the wheel's mesh only when a unit does not give one. For the
  Sherman they agree with the meshes within 4 mm for road wheels and idlers; the sprocket's is its pitch
  radius, inside the tips of its teeth.
- **Track skin.** The tracks are skinned meshes. `wt_blk` reads the bone table in front of each one, which
  binds every track vertex to the `root` node or to one suspension wheel node (`bone_wheel_<l|r>_<NN>`):
  the ring of track under a road wheel follows that wheel, the rest stays with the hull. There are no
  bones along the links. `vehicle::Mesh` carries them as `bones`, `joints` and `weights`, the bridge
  provider as a `Skin` with per-vertex joints and weights, and the viewer skins them on the GPU.
  `Mesh::skinned_positions` does the blending on the CPU. The game places the wheel nodes with its
  suspension simulation (`PhysSys` in the unit BLK), which is not reproduced: on flat ground the bones sit
  at their bind pose. `wt-viewer --lift bone_wheel_l_03=0.15` raises a node to show the skin follow it.
- **Track motion** is a texture scroll along the static mesh. The rate is the change in V per metre on the
  mesh's bottom run: 1.614 for the Sherman, which is a quarter of a V unit per link of the unit's
  `trackLen`, as in the other tracked vehicles of the game. It is the rate at which the tread stays still on the
  ground.

Left out, because the code that uses them is not published: the unit's `tracks.animationMultiplier` (0.68
for the Sherman) and the `texcoord_anim=1` / `-1` parameter of the left and right track materials in
dynModelDesc.bin. `Drive::track_animation_multiplier` reads the first; neither is applied.


## Materials and winding

The bridge material of a vehicle part follows the shader class and parameters `dynModelDesc.bin` gives it
(`vehicle::material_alpha`): glass classes, `*alpha_blend`, `aces_weapon_fire` and `propeller_side` blend; an
`atest=N` parameter makes any class a cut-out at N/255 (a few `masked_chrome_bump` parts carry it); the
`*atest` classes cut out at 0.5 without it; everything else is opaque, including the `masked_*` classes whose
alpha is a paint or shine mask (their texels are transparent in 14 to 95% of cases and the hull is solid).
Materials no mesh of the LOD uses are not decoded and are left out of the model. The blend state itself is in
the compiled shader dumps, which are not decoded here. Triangles wind counter-clockwise around their vertex
normals; a handful of parts are stored the other way round (`germ_he_115`'s `radiator2_25`, the Ontos' left
suspension arms), which neither their node matrices nor their materials explain, so a mesh whose triangles
mostly face against its normals is turned over.
