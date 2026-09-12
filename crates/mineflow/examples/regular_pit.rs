//! Solves an ultimate pit for a regular block model read from a text file.
//!
//! Usage:
//!     cargo run -p mineflow --release --example regular_pit -- <nx> <ny> <nz> <slope_deg> <values.dat> [out.dat]
//!
//! The values file holds one integer economic block value per line, in the
//! model's 1D block order. This mirrors the C++ command line utility, so the
//! two can be compared directly:
//!
//!     cargo run -p mineflow --release --example regular_pit -- 170 215 50 45 data/cucase.dat

use std::{
    env, fs,
    io::{BufWriter, Write},
};

use mineflow::{BlockModel, Pattern, Precedence, Slope, Solver, degrees};

/// The bench count used by the C++ utility for its --regular option.
const BENCHES: i64 = 9;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if !(5..=6).contains(&args.len()) {
        eprintln!("usage: regular_pit <nx> <ny> <nz> <slope_deg> <values.dat> [out.dat]");
        std::process::exit(1);
    }

    let model = BlockModel::new(args[0].parse()?, args[1].parse()?, args[2].parse()?);
    let count = model.checked_num_blocks()?;
    let slope = degrees(args[3].parse()?);

    let values = read_values(&args[4])?;
    if values.len() != count {
        return Err(format!("{} holds {} values but the model has {} blocks", args[4], values.len(), model.num_blocks()).into());
    }

    let pattern = Pattern::min_search(&model, &Slope::constant(slope)?, BENCHES)?;
    eprintln!("pattern: {} arcs per block", pattern.len()?);

    let precedence = Precedence::regular_3d(&model, &pattern)?;
    let mut solver = Solver::new(&precedence, &values)?;

    let info = solver.solve()?;
    eprintln!(
        "value {} over {}/{} blocks in {:.3}s ({} arcs used)",
        info.pit_value,
        info.num_blocks_in_pit,
        info.num_blocks,
        info.elapsed.as_secs_f64(),
        info.num_precedence_constraints_used,
    );

    if let Some(path) = args.get(5) {
        let pit = solver.pit()?;
        let mut out = BufWriter::new(fs::File::create(path)?);
        for mined in pit.as_slice() {
            writeln!(out, "{}", u8::from(*mined))?;
        }
    }

    Ok(())
}

fn read_values(path: &str) -> Result<Vec<i64>, Box<dyn std::error::Error>> {
    fs::read_to_string(path)?
        .split_ascii_whitespace()
        .map(|value| value.parse::<i64>().map_err(Into::into))
        .collect()
}
