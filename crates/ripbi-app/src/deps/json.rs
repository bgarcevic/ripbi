//! The `deps` JSON contract, schema v1 (`crates/ripbi-cli/docs/deps.md`):
//! what `ripbi deps --json` prints — the graph itself, never truncated.

use std::collections::HashSet;
use std::io;

use serde::Serialize;

use ripbi_core::{BindingSite, ObjectId};

use super::{ByType, DepsOutput};
use crate::error::ScanError;
use crate::output::kind_of;

/// Writes `--json`: the graph itself, `schema_version` 1. Never truncated.
///
/// # Errors
/// Propagates serialization or stream write failures.
pub fn write(out: &mut dyn io::Write, output: &DepsOutput) -> Result<(), ScanError> {
    match output {
        DepsOutput::Overview(overview) => write_json(
            out,
            &JsonOverview {
                schema_version: 1,
                objects: overview.objects,
                edges: overview.edges,
                bindings: overview.bindings,
                by_type: overview.by_type,
            },
        ),
        DepsOutput::Focused(focused) => {
            let single_root = focused.roots.len() == 1;
            let mut node_ids: Vec<&ObjectId> = Vec::new();
            for slice in focused.dependencies.iter().flatten() {
                node_ids.extend(slice.nodes.iter());
            }
            for slice in focused.impact.iter().flatten() {
                node_ids.extend(slice.nodes.iter());
            }
            node_ids.sort();
            node_ids.dedup();
            let nodes: Vec<JsonNode> = node_ids
                .into_iter()
                .map(|id| JsonNode {
                    id: id.to_string(),
                    kind: kind_of(id),
                })
                .collect();

            // Dependency edges keep the graph orientation (consumer →
            // producer); edges only the impact slices reached are flipped so
            // `from` is always the queried-object side, and `kind` says
            // which. An edge several roots share is emitted once, as a
            // dependency.
            let mut edges: Vec<JsonEdge> = Vec::new();
            let mut seen: HashSet<(String, String, &'static str)> = HashSet::new();
            for slice in focused.dependencies.iter().flatten() {
                for edge in &slice.edges {
                    let key = (
                        edge.from.to_string(),
                        edge.to.to_string(),
                        edge.provenance.key(),
                    );
                    if seen.insert(key.clone()) {
                        edges.push(JsonEdge {
                            from: edge.from.to_string(),
                            to: edge.to.to_string(),
                            kind: "dependency",
                            provenance: edge.provenance.key(),
                        });
                    }
                }
            }
            for slice in focused.impact.iter().flatten() {
                for edge in &slice.edges {
                    let forward = (
                        edge.from.to_string(),
                        edge.to.to_string(),
                        edge.provenance.key(),
                    );
                    if seen.contains(&forward) {
                        continue;
                    }
                    seen.insert(forward);
                    edges.push(JsonEdge {
                        from: edge.to.to_string(),
                        to: edge.from.to_string(),
                        kind: "impact",
                        provenance: edge.provenance.key(),
                    });
                }
            }
            edges.sort_by(|a, b| {
                (&a.from, &a.to, a.kind, a.provenance).cmp(&(&b.from, &b.to, b.kind, b.provenance))
            });

            let bindings: Vec<JsonBinding> = focused
                .bindings
                .iter()
                .flatten()
                .map(|(id, edge)| JsonBinding {
                    object: id.to_string(),
                    report: edge.report.as_ref().map(|r| r.as_str().to_string()),
                    page: edge.page.as_ref().map(|p| p.as_str().to_string()),
                    visual: edge.visual.as_ref().map(|v| v.as_str().to_string()),
                    bookmark: edge.bookmark.as_ref().map(|b| b.as_str().to_string()),
                    mobile: edge.mobile,
                    binding: match &edge.kind {
                        BindingSite::FieldWell { role } => Some(role.clone()),
                        _ => None,
                    },
                    kind: edge.kind.key(),
                })
                .collect();

            let mut kept: Vec<JsonKept> = Vec::new();
            let mut seen: HashSet<&ObjectId> = HashSet::new();
            for (id, by) in focused.kept.iter().flatten() {
                if seen.insert(id) {
                    kept.push(JsonKept {
                        object: id.to_string(),
                        annotated: by.id.to_string(),
                        reason: by.reason.clone(),
                    });
                }
            }

            let mut queried: Vec<JsonQueried> = Vec::new();
            let mut seen: HashSet<&ObjectId> = HashSet::new();
            for by in focused.queried.iter().flatten() {
                if seen.insert(&by.id) {
                    queried.push(JsonQueried {
                        object: by.id.to_string(),
                        count: by.count,
                        last_seen: by.last_seen.clone(),
                        users: by.users,
                        applications: by.applications.clone(),
                        reports: by.reports.clone(),
                    });
                }
            }

            let root = single_root.then(|| {
                let id = &focused.roots[0];
                JsonRoot {
                    id: id.to_string(),
                    kind: kind_of(id),
                }
            });
            write_json(
                out,
                &JsonFocused {
                    schema_version: 1,
                    root,
                    nodes,
                    edges,
                    bindings,
                    kept,
                    queried,
                },
            )
        }
    }
}

fn write_json<T: Serialize>(out: &mut dyn io::Write, payload: &T) -> Result<(), ScanError> {
    serde_json::to_writer_pretty(&mut *out, payload)
        .map_err(|error| ScanError::new(format!("cannot serialize JSON: {error}")))?;
    writeln!(out).map_err(ScanError::from)?;
    Ok(())
}

/// The JSON document of one focused view: the graph itself, versioned.
/// `root` is `null` when selectors chose several roots.
#[derive(Serialize)]
struct JsonFocused {
    schema_version: u8,
    root: Option<JsonRoot>,
    nodes: Vec<JsonNode>,
    edges: Vec<JsonEdge>,
    bindings: Vec<JsonBinding>,
    kept: Vec<JsonKept>,
    queried: Vec<JsonQueried>,
}

/// One object on an impact slice that logged queries reference directly.
/// User identities never leave the log: `users` is a distinct count.
#[derive(Serialize)]
struct JsonQueried {
    object: String,
    count: usize,
    last_seen: Option<String>,
    users: usize,
    applications: Vec<String>,
    /// Service report ids from the queries' `ApplicationContext`.
    reports: Vec<String>,
}

/// One object on an impact slice a `ripbi_keep` annotation keeps.
#[derive(Serialize)]
struct JsonKept {
    object: String,
    /// The annotated object: `object` itself, or its kept table.
    annotated: String,
    reason: String,
}

#[derive(Serialize)]
struct JsonRoot {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct JsonNode {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct JsonEdge {
    from: String,
    to: String,
    /// `dependency` for upstream edges (graph orientation), `impact` for
    /// edges reached only downstream (flipped, so `from` is the
    /// queried-object side).
    kind: &'static str,
    provenance: &'static str,
}

#[derive(Serialize)]
struct JsonBinding {
    object: String,
    report: Option<String>,
    page: Option<String>,
    visual: Option<String>,
    bookmark: Option<String>,
    mobile: bool,
    /// The field-well role (`Values`), when the binding is a field well.
    binding: Option<String>,
    /// The binding kind, snake_case.
    kind: &'static str,
}

/// The JSON document of the overview.
#[derive(Serialize)]
struct JsonOverview {
    schema_version: u8,
    objects: usize,
    edges: usize,
    bindings: usize,
    by_type: ByType,
}
