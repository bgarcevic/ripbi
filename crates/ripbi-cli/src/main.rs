#![forbid(unsafe_code)]

use std::path::PathBuf;

use clap::Parser;

/// Static analysis, linting, and tree-shaking for Power BI semantic models and DAX.
#[derive(Parser)]
#[command(name = "ripbi", version)]
struct Cli {
    /// Path to a .pbix/.pbit file, .pbip folder, TMDL folder, or model.bim
    path: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    println!(
        "ripbi: analysis of {} not yet implemented",
        cli.path.display()
    );
    Ok(())
}
