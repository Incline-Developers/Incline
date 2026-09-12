# MineFlow for Rust

A pure Rust ultimate pit optimization library in the Incline Cargo workspace.
Incline remains the root application; this crate lives in `crates/mineflow` and
can be built independently with `cargo check -p mineflow`.

The library ports MineFlow's highest-label pseudoflow solver and mining
precedence models. It contains no FFI, C++, build script, or unsafe Rust. Its
only direct dependency is `web-time`, for timing on native and WebAssembly.

## Usage

```rust
use mineflow::{Precedence, Solver};

fn main() -> mineflow::Result<()> {
    // Mining block a requires mining b for each edge (a, b).
    let precedence = Precedence::explicit(5, &[(0, 2), (0, 3), (1, 3), (1, 4)])?;
    let mut solver = Solver::new(&precedence, &[7, 2, -2, -2, -2])?;
    let info = solver.solve()?;
    assert_eq!(info.pit_value, 3);
    assert_eq!(solver.pit()?.as_slice(), &[true, false, true, true, false]);

    solver.solve_largest()?;
    assert_eq!(solver.largest_pit()?.num_selected(), 5);
    Ok(())
}
```

For a regular model:

```rust
use mineflow::{BlockModel, UltimatePit, degrees};

fn main() -> mineflow::Result<()> {
    let model = BlockModel::new(11, 11, 4).with_block_size(10.0, 10.0, 10.0);
    let mut values = vec![-1; model.checked_num_blocks()?];
    values[model.grid_index(5, 5, 0) as usize] = 500;
    let solution = UltimatePit::new(model)
        .constant_slope(degrees(45.0))
        .benches(4)
        .solve(&values)?;
    println!("{} blocks, value {}", solution.pit.num_selected(), solution.info.pit_value);
    Ok(())
}
```

Block indices increase in x, then y, then z; z increases upward. Slope azimuths
are clockwise from north. Geometric coordinates and angles use f64. Economic
values, costs and prices are supplied by the caller, not inferred by MineFlow.

The `regular_pit` example reads integer economic values in this order and can
write one selected/not-selected flag per block:

```sh
cargo run -p mineflow --release --example regular_pit -- \
    120 120 26 45 /path/to/bauxitemed.dat /path/to/pit-flags.dat
```

## Supported library functionality

- Smallest and largest optimal pits, repeated solves with updated block values,
  solve timing, selected block counts and exact total value.
- Explicit graphs, regular 2D 45-degree grids, regular 3D pattern precedence,
  and keyed patterns for spatially and directionally varying slopes.
- Custom `PrecedenceSource` and `BlockValueSource` implementations for irregular
  models. Only antecedents are needed to solve; successor queries are optional.
- Constant and circular directional slopes, linear lookup, and sampled cosine
  and cubic interpolation.
- One-five, one-nine and knight's-move templates; naive, less-naive and minimum
  search pattern generation, plus whole-model and cumulative bench accuracy.
- Graph enumeration, consistency checks and reusable reachability searches.
- Explicit floating-point quantization through `ValueScale` and `solve_f64`.

Regular precedence generates neighbours on demand. The smallest-cut solver
materializes the outgoing neighbours of nodes it visits and keeps an indexed
forest of active tree arcs. Largest-cut selection additionally builds reverse
component dependencies; its worst-case storage is proportional to the examined
precedence edges. Updating values retains the precedence model but rebuilds the
flow forest, matching upstream's actual update behavior.

`solve_with_cancel` and `solve_largest_with_cancel` accept a cheap callback that
returns true to cancel. Cancellation invalidates results and a later solve can
restart. Provider calls and pattern construction are synchronous. Custom
providers must be immutable and deterministic; their panics propagate normally
(and abort on the project's WebAssembly target). Run expensive work through
Incline's job system when adding an application command. No application UI is
connected to the solver yet.

## Port decisions

This ports the library, not the C++ command-line argument interface or MATLAB
MEX bindings. It retains the useful public API from Incline's former Rust
bindings, with these intentional differences:

- Input values are i64; excess, flow and `SolveInfo::pit_value` are i128. This
  exactly covers sums of addressable i64 block values without GMP. Arbitrary
  precision input values and the old `gmp` feature are not provided.
- `PrecedenceSource` is `Send + Sync`; models are shared using `Arc`. FFI error
  variants and panic-catching trampolines are removed.
- The solver addresses at most 4,294,967,294 blocks, so its flow forest keeps
  32 bit node links. `Solver::solve` reports a larger model as invalid input.
  Block indices remain i64 throughout the public API.
- Slope azimuths are normalized before lookup, including negative angles.
  Duplicate azimuths are rejected. Slopes lie in (0, 90] degrees. Cubic
  interpolation that overshoots that range returns an error.
- Largest-cut selection uses residual reachability, contracting only tree arcs
  with positive flow. Upstream instead groups by whole flow-tree branch, which
  lumps together nodes joined by zero-flow arcs; such an arc is residual in only
  one direction, so its endpoints can belong to different optimal cuts, and
  forcing them to one answer drops break-even blocks. For values `[0, 0, -2]`
  and edges `[(0, 1), (0, 2)]`, the largest optimal pit is
  `[false, true, false]`; upstream returns `[false, false, false]`. On the
  upstream bauxite dataset this port selects 124,742 blocks where upstream
  selects 76,927, at the same objective value.
- Grid offset checks validate each coordinate, preventing row/bench wrapping.
  Empty explicit graphs are supported; graph/pattern constructors and graph
  queries reject invalid counts and indices. Grid indexing helpers expect a
  validated model and in-range coordinates.
- Accuracy field names retain upstream's convention: `false_positive` counts
  geometric cone blocks missed by the pattern, and `false_negative` counts
  extras. Rates with zero denominators are defined as zero.

## Benchmarks

Measured against the original C++ on the five upstream datasets, pinned to one
core, median of seven runs: both solver modes are 20-34% faster on the four
larger datasets, in 29-41% less memory. In largest-pit mode the port is also
doing strictly more work, because it corrects the upstream result described
above.

Several things in the solver exist only for that margin and would otherwise look
arbitrary: the flow forest's 32 bit node links and cached parent index, which
halve the node to 48 bytes and remove a dependent load from the hottest loop;
the precomputed per-offset index deltas and the branch-free path for blocks
interior to a pattern; and the backwards walk in `Forest::largest`, which spreads
exclusion using successor queries rather than reconstructing reversed edges.

The benchmark and profiling harnesses were removed once that work was done. They
compared against a clean checkout of upstream at
`6f5dd7091070b1bd85457b3ce9fd95c4e345c983`, driven by `perf` and a C++17 build of
`mineflow.cpp`, so reproducing the figures needs a C++ toolchain and the upstream
datasets.

## Port validation

Validated with the repository nightly toolchain:

- 39 temporary behavior tests, including all 512 directed three-node graphs
  across 125 value assignments (64,000 combinations), 3,000 random cyclic
  graphs checked by exhaustive subset enumeration, and a 100,000-node chain.
- 500 random graphs compared with upstream C++ for exact smallest cuts and
  objective values. The largest-cut discrepancy was independently checked
  by brute force.
- 240 generated patterns across uniform/scaled/anisotropic blocks, multiple
  bench counts and constant/directional slopes. Constant slopes match upstream;
  directional cases match a reference copy with azimuth normalization fixed.
- The upstream 120 × 120 × 26 bauxite dataset: identical C++/Rust selection of
  74,587 out of 374,400 blocks, with objective value 28,288,679.
- Native and WebAssembly compilation of both the crate and Incline, Clippy,
  formatting, example compilation and documentation examples. Browser runtime
  and interactive application behavior were not exercised.

The later optimization pass that produced the figures above was validated
separately:

- 9 temporary tests, including 600 random explicit graphs, half of them cyclic,
  each solved through all three precedence paths - the crate's own graphs, a
  foreign source with successors, and a foreign source without - for 1,800 solves
  checked against exhaustive enumeration of every closed subset, confirming both
  that the smallest pit is the intersection of all optimal closures and that the
  largest is their union. 150 random regular models were checked the same way.
- 66 generated patterns whose antecedents and successors were compared block by
  block against a direct coordinate walk, covering models small enough that every
  block is on the pattern boundary and large enough to exercise the interior path.
- Every objective value, selected count and selection-mask hash on all five
  upstream datasets in both modes, unchanged from before the pass, with the two
  smaller datasets also re-run with debug assertions enabled.

Temporary unit tests were removed after passing, following the repository's
`AGENTS.md` instructions. Usage examples remain in the API documentation.

## Sources and license

Algorithm and geometry reference:
[MineFlowCSM/MineFlow](https://github.com/MineFlowCSM/MineFlow), revision
`6f5dd7091070b1bd85457b3ce9fd95c4e345c983`.
Rust API and supporting value/model helpers:
[Incline-Developers/MineFlow-rs](https://github.com/Incline-Developers/MineFlow-rs),
revision `3a6a3b541aa55a50eebd5efadea4943c82353a08`.
The upstream MIT copyright notice is retained in [LICENSE](LICENSE).

Research citation: Deutsch, M., Dağdelen, K. & Johnson, T. (2022),
[An Open-Source Program for Efficiently Computing Ultimate Pit Limits: MineFlow](https://doi.org/10.1007/s11053-022-10035-w).
