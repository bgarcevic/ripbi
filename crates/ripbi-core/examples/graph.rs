//! Eyeball the dependency graph from the terminal, until the real CLI (#8)
//! lands. This is a binary target, so printing here is fine — the library
//! itself stays silent and returns its findings as data.
//!
//! Usage:
//!
//! ```text
//! cargo run -p ripbi-core --example graph -- <model> [<report> ...] [--deps <name>]
//! ```
//!
//! `<model>` is a `.SemanticModel` item folder; each `<report>` is a `.Report`
//! item folder. Without `--deps`, the findings are printed: every unused
//! object with its "used by" annotations. With `--deps <name>` (a
//! case-insensitive substring of an object's display form, e.g. `--deps
//! Amount`), every matching object's consumers, report bindings, and
//! producers are printed instead. Exit codes: `0` ok, `2` usage error, `3`
//! ingest error.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use ripbi_core::graph::DependencyGraph;
use ripbi_core::ingest::{report, semantic_model};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut paths: Vec<&String> = Vec::new();
    let mut deps_query: Option<&String> = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--deps" {
            deps_query = args.get(index + 1);
            index += 2;
        } else {
            paths.push(&args[index]);
            index += 1;
        }
    }
    let Some(model_path) = paths.first() else {
        eprintln!("usage: graph <.SemanticModel folder> [<.Report folder> ...] [--deps <name>]");
        return ExitCode::from(2);
    };
    let report_paths = &paths[1..];

    let db = match semantic_model(Path::new(model_path)) {
        Ok(ingested) => {
            for skip in &ingested.skips {
                eprintln!("skip: {}", skip.detail);
            }
            ingested.value
        }
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(3);
        }
    };
    let mut reports = Vec::new();
    for path in report_paths {
        match report(Path::new(path)) {
            Ok(ingested) => {
                for skip in &ingested.skips {
                    eprintln!("skip: {}", skip.detail);
                }
                reports.push(ingested.value);
            }
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(3);
            }
        }
    }
    let report_refs: Vec<&ripbi_core::ReportModel> = reports.iter().collect();
    let graph = DependencyGraph::build(&db, &report_refs);

    match deps_query {
        Some(query) => print_deps(&graph, query),
        None => print_findings(&graph),
    }
    ExitCode::SUCCESS
}

/// The `scan` preview: every unreachable object, annotated with who still
/// references it.
fn print_findings(graph: &DependencyGraph) {
    if graph.roots().is_empty() {
        println!("warning: no reachability roots (no report bindings, no roles)\n");
    }
    let unused = graph.unused_objects();
    println!(
        "nodes: {}  roots: {}  unused: {}",
        graph.object_ids().count(),
        graph.roots().len(),
        unused.len(),
    );
    for finding in &unused {
        println!("\nUNUSED {}", finding.id);
        if finding.used_by.is_empty() {
            println!("  ← nothing references it (orphan — delete freely)");
        }
        for used in &finding.used_by {
            let also = if used.also_unused {
                " (also unused)"
            } else {
                ""
            };
            println!("  ← used by {} — {}{also}", used.id, used.provenance);
        }
    }
}

/// The `deps` preview: who uses each matching object, and what it uses.
fn print_deps(graph: &DependencyGraph, query: &str) {
    let needle = query.to_lowercase();
    let mut matches = 0;
    for id in graph.object_ids() {
        if !id.to_string().to_lowercase().contains(&needle) {
            continue;
        }
        matches += 1;
        println!("{id}");
        let consumers = graph.consumers_of(id);
        if consumers.is_empty() {
            println!("  used by: —");
        } else {
            println!("  used by:");
            for (consumer, provenance) in &consumers {
                println!("    ← {consumer} — {provenance}");
            }
        }
        for provenance in graph.roots_of(id) {
            println!("    ← report binding — {provenance}");
        }
        let producers = graph.producers_of(id);
        if producers.is_empty() {
            println!("  uses: —");
        } else {
            println!("  uses:");
            for (producer, provenance) in &producers {
                println!("    → {producer} — {provenance}");
            }
        }
        println!();
    }
    if matches == 0 {
        println!("no object matches {query:?}");
    }
}
