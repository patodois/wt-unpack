# How it works

Where the formats come from. Part of the [wt-unpack README](../README.md).

- The vromfs and BLK readers in `src/` are written from the open-source
  [Dagor Engine](https://github.com/GaijinEntertainment/DagorEngine) (BSD-3-Clause), mainly
  `vromfsLoad.cpp`, `dag_zstdObfuscate.h` and `blk_serialize.cpp`. No code was copied; the module docs
  describe the layouts.
- Texture (DDSx/DXP) and model (DynModel, GeomNodeTree, GRP) reading comes from
  [wt_blk](https://github.com/Warthunder-Open-Source-Foundation/wt_blk). Those readers were contributed
  there (Warthunder-Open-Source-Foundation/wt_blk#28 and #29), and this crate uses them from a fork
  until they are merged.
- Oodle-compressed data is decoded by [oozextract](https://github.com/lvlvllvlvllvlvl/oozextract), pure
  Rust with no proprietary library.
- Materials and texture names come from `dynModelDesc.bin`.
- Level files follow Dagor's `loadLevel.cpp`, `lmeshManager.cpp`, `heightmapPhysHandler.cpp`,
  `compressedHeightmap.cpp`, `landRayTracerSoA4.cpp`, `bvhIO.cpp` and `rendInstGen.cpp`; the ground's
  materials `lmeshLandClasses.cpp`, `lmeshWeightAtlas*.cpp`, `landClsExp.cpp` and `binDumpUtil.cpp`
  (the read-only data block of land classes); placed objects `rendInstRes.cpp`,
  `rendInstRotationPalette.cpp` and `genObjUtil.h`. The `HM2`
  heightmap covers the playable area (2 m per sample on tank maps); the landmesh's ray tracer
  covers the whole map at its own resolution and is rasterised into the same kind of grid.
- The sound bank and FSB5 readers follow the layouts documented by the community in
  [vgmstream](https://github.com/vgmstream/vgmstream) (ISC) and
  [python-fsb5](https://github.com/HearthSim/python-fsb5) (MIT); no code was copied. FSB5 Vorbis drops
  the stream headers, so they are rebuilt from FMOD's fixed set of setup headers, which python-fsb5
  extracted and vgmstream ships (`src/sound/vorbis_setups.bin.zst`, under vgmstream's ISC licence in
  `src/sound/LICENSE-vgmstream`).
  Vorbis decoding to WAV uses [lewton](https://github.com/RustAudio/lewton).
- The camouflage blend is an approximation, because the game's shaders are not public. The base
  texture's alpha marks painted areas, and the camouflage and occlusion maps are sampled through the
  mesh's second UV set.
