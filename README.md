# wt-unpack

A small Rust tool that reads the data archives of War Thunder and other
Dagor Engine games:

- `.vromfs.bin` archives (`VRFs` / `VRFx`, stored or zstd-packed)
- binary BLK files (all six format bytes, including slim BLKs that use the
  archive's shared name map and zstd dictionary)

Binary BLKs are converted to readable Dagor text syntax or to JSON.

This project contains no game code or game data. It only reads files from a
copy of the game you already have installed.

## Build

```sh
cargo build --release
```

## Usage

```sh
# Header info and file list
wt-unpack list game/char.vromfs.bin

# Extract everything, converting binary BLKs to text (default) or JSON
wt-unpack extract game/aces.vromfs.bin -o out/aces
wt-unpack extract game/aces.vromfs.bin -o out/aces --blk json --filter tankmodels

# Print a single BLK, loose or from inside an archive
wt-unpack cat game/char.vromfs.bin:config/wpcost.blk
wt-unpack cat game/char.vromfs.bin:config/wpcost.blk --blk json
```

On macOS the game files live under
`/Applications/WarThunderLauncher.app/Contents/WarThunder.app/Contents/Resources/game`.

## Format notes

The formats are implemented from the open-source
[Dagor Engine](https://github.com/GaijinEntertainment/DagorEngine)
(BSD-3-Clause), mainly `prog/engine/ioSys/vromfsLoad.cpp`,
`prog/dagorInclude/supp/dag_zstdObfuscate.h` and
`prog/engine/ioSys/dataBlock/blk_serialize.cpp`. No code was copied; the
module docs in `src/vromfs.rs` and `src/blk.rs` describe the layouts.

Tested against War Thunder 2.59.0.80 (macOS): 38,737 files across nine
archives extracted with no conversion errors.

## Disclaimer

Not affiliated with or endorsed by Gaijin Entertainment. War Thunder is a
trademark of Gaijin Entertainment. Use this tool for reading and studying
game data only; do not use it for cheating or to redistribute game assets.

## License

Licensed under either of MIT or Apache-2.0, at your option.
