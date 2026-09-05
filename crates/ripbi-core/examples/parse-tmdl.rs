//! Eyeball a parsed semantic model from the terminal, until the real CLI
//! (#8) lands. This is a binary target, so printing here is fine — the
//! library itself stays silent and returns its findings as data.
//!
//! Usage:
//!
//! ```text
//! cargo run -p ripbi-core --example parse-tmdl -- <folder> [--expressions]
//! ```
//!
//! `<folder>` is a `.SemanticModel` item folder or its `definition/`
//! directory. Exit codes: `0` parsed with no skips, `1` parsed with skips,
//! `2` usage error, `3` parse error.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use ripbi_core::ingest::{SkipNotice, semantic_model};
use ripbi_core::model::{PartitionSource, TabularDatabase};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(path) = args.iter().find(|arg| !arg.starts_with("--")) else {
        eprintln!("usage: parse-tmdl <.SemanticModel folder | definition folder> [--expressions]");
        return ExitCode::from(2);
    };
    let dump_expressions = args.iter().any(|arg| arg == "--expressions");

    let ingested = match semantic_model(Path::new(path)) {
        Ok(ingested) => ingested,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(3);
        }
    };

    print_summary(&ingested.value);
    print_skips(&ingested.skips);
    if dump_expressions {
        print_expressions(&ingested.value);
    }

    if ingested.skips.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn print_summary(db: &TabularDatabase) {
    let measures: usize = db.tables.iter().map(|t| t.measures.len()).sum();
    let columns: usize = db.tables.iter().map(|t| t.columns.len()).sum();
    let inactive = db.relationships.iter().filter(|r| !r.is_active).count();

    println!("Model: {}", db.name.as_deref().unwrap_or("(unnamed)"));
    println!(
        "  tables: {}  measures: {}  columns: {}  relationships: {} ({} inactive)  roles: {}  shared expressions: {}  functions: {}",
        db.tables.len(),
        measures,
        columns,
        db.relationships.len(),
        inactive,
        db.roles.len(),
        db.expressions.len(),
        db.functions.len(),
    );

    println!();
    println!("Tables (in ref-table order):");
    for (index, table) in db.tables.iter().enumerate() {
        let partitions: Vec<&str> = table
            .partitions
            .iter()
            .map(|partition| match &partition.source {
                PartitionSource::M { .. } => "M",
                PartitionSource::Calculated { .. } => "DAX",
                PartitionSource::Query { .. } => "query",
                PartitionSource::Other { kind } => kind.as_deref().unwrap_or("other"),
            })
            .collect();
        let calc_group = table
            .calculation_group
            .as_ref()
            .map_or(String::new(), |group| {
                format!("  +{} calc items", group.items.len())
            });
        println!(
            "{:2}. {:24} {:2} cols {:2} meas {:2} hier [{}]{}",
            index + 1,
            table.name,
            table.columns.len(),
            table.measures.len(),
            table.hierarchies.len(),
            partitions.join(", "),
            calc_group,
        );
    }
}

fn print_skips(skips: &[SkipNotice]) {
    println!();
    println!("Skips ({}):", skips.len());
    // Details are written to be self-describing, so no kind label is needed.
    for skip in skips {
        let location = skip
            .location
            .as_deref()
            .map_or(String::new(), |location| format!(" {location}"));
        println!("  {}{}: {}", skip.path.display(), location, skip.detail);
    }
}

fn print_expressions(db: &TabularDatabase) {
    println!();
    println!("DAX expressions ({}):", db.dax_expressions().len());
    for expression in db.dax_expressions() {
        println!(
            "\n{:?} · {} · home table: {}",
            expression.kind,
            expression.owner.to_object_id(),
            expression.home_table.unwrap_or("—"),
        );
        for line in expression.text.lines() {
            println!("  | {line}");
        }
    }
    println!();
    println!("M expressions ({}):", db.m_expressions().len());
    for expression in db.m_expressions() {
        println!("\n{}", expression.owner.to_object_id());
        for line in expression.text.lines().take(3) {
            println!("  | {line}");
        }
        let remaining = expression.text.lines().count().saturating_sub(3);
        if remaining > 0 {
            println!("  | … (+{remaining} lines)");
        }
    }
}
