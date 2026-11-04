# Fuzzing the wt-unpack readers

cargo-fuzz targets, one per entry point that takes untrusted bytes (`fuzz_targets/`; `drive_script` takes text). This
crate has its own workspace and lock file, needs a nightly toolchain and is not part of the published package.

```sh
cargo install cargo-fuzz
cd fuzz
# a corpus directory of your own, seeded from the tracked seeds (a run writes the inputs it finds into it)
mkdir -p /tmp/wtu-corpus-level_landmesh && cp -R corpus/level_landmesh/. /tmp/wtu-corpus-level_landmesh/
cargo +nightly fuzz run level_landmesh /tmp/wtu-corpus-level_landmesh -- -max_len=16384 -rss_limit_mb=2048 -malloc_limit_mb=1536 -timeout=10 -max_total_time=300
```

Without the directory argument cargo-fuzz uses `corpus/<target>` itself and writes the new inputs next to the seeds
(they are git-ignored, but they mix with the seeds).

## Flags

Run every target with these, written out on the command line (a shell variable that holds them is not split by zsh and
the run then silently uses libFuzzer's defaults):

- `-max_len=16384`: inputs up to 16 KiB.
- `-rss_limit_mb=2048`: stop when the process holds more than 2 GiB.
- `-malloc_limit_mb=1536`: stop on a single allocation above 1.5 GiB. This has to stay above 1 GiB: `HeightGrid::rasterize`
  allocates the whole grid its mesh spans in one piece, and a side may be up to 16384 samples (16384 x 16384 x 4 bytes =
  1 GiB). That is real, accepted input (`level air_denmark.bin --terrain landmesh --cell 16 --heightmap out.png` rasterises 8191 x 8191
  and works), so a lower limit reports it as a finding. Only that heights buffer is allocated fallibly (`try_reserve_exact`: a
  refused allocation is an error). `write_png`, `HeightGrid::from_heightmap` and the heightmap decoder allocate infallibly, and
  abort when the system refuses.
- `-timeout=10`: seconds for one input. A tiny input can legitimately exceed it: `HeightGrid::rasterize` costs triangles times
  the samples they cover (as on main; the loop is unchanged). A 189-byte `lmap` with 128 triangles over a 16384 x 16384 grid took
  12 s under the fuzzer in the second review, and 128 triangles spanning that grid took 22 s in a direct call in a release build.
  That is a slow input, not a hang; when `-timeout` reports one, look at the grid size before looking for a bug.
- `-max_total_time=300`: seconds for the whole run (leave it out to run until a finding).

ASan and debug assertions are on (cargo-fuzz defaults).

## Corpus

`corpus/` holds the seeds and nothing else: the 52 files that `src/make_seeds.rs` writes, built from the layouts in the
module docs plus the repository's own `samples/live/*.json` (no game file is used). The directory is git-ignored so that a run
does not leave the tree dirty, and the seeds are tracked with `git add -f`. To rebuild them:

```sh
cargo run --release --bin make_seeds -- corpus
```

A run writes the inputs it finds into the corpus directory it is given, so run on a directory of your own, seeded from this one
(the first command above), and do not commit its output.

A crash is saved under `artifacts/<target>/` (ignored). Once fixed, pin it with a unit test in the crate that has the bug;
add the input to `corpus/` only if it still fails when the fix is taken out.

## What the targets do with a parsed value

`src/lib.rs` follows the CLI on the landmesh path only (`level --terrain landmesh --heightmap out.png`): `HeightGrid::rasterize`
at the file's own cell size, `fill_from` the level's heightmap, `write_png`. `fsb5_bank` slices `&data[range]` of each bank as the
CLI does. Beyond or short of the CLI:

- `use_heightmap` builds the grid from the heightmap and calls `write_png` (the CLI's `--terrain hm2` does too), but also
  `grid.mesh(64)` and `rasterize(&mesh, 8.0)`, which the CLI does not.
- `terrain::to_model` and `gltf::write` (the glTF export) are not fuzzed.
- The steps after the rasterising run only on a grid of a few million samples, so the harness itself is not what allocates
  gigabytes; the grid itself is whatever size the mesh asks for.
