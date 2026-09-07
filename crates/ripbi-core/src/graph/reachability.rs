//! Reachability: the two-pass traversal that separates live objects from dead.
//!
//! Two plain passes encode the relationship policy without any special-cased
//! machinery (the policy itself is documented in [`super`]):
//!
//! 1. **Strong pass** — from the roots (report bindings and roles), over every
//!    edge *except* relationship endpoints. Containment fires: a used column
//!    keeps its table alive, a used table keeps its partitions and
//!    relationships alive.
//! 2. **Weak pass** — extends the strong set over every edge *except*
//!    containment. A live table pulls in its relationships and both their key
//!    columns, but a key column that is only alive this way can no longer keep
//!    its own table alive — so a table referenced by nothing but a
//!    relationship is still reported unused.
//!
//! A consumer annotation therefore never lies: for any unused object, every
//! referencing object is either itself unused, or live only through a
//! relationship endpoint (its `also_unused` flag is `false`).

use std::collections::HashSet;

use super::{DependencyGraph, ObjectId, Provenance};

/// The liveness verdict for one graph: the set of live objects.
pub(super) struct Reachability {
    live: HashSet<ObjectId>,
}

impl Reachability {
    /// Runs both passes over the finished graph.
    pub(super) fn compute(graph: &DependencyGraph) -> Self {
        // Pass 1 reaches everything except relationship-endpoint targets; pass 2
        // extends that set without letting containment fire again, so weakly
        // alive key columns never drag their tables along.
        let strong = graph.reach(graph.seed_indices(), Provenance::is_strong_pass_edge);
        let live = graph.reach(strong.iter().copied(), Provenance::is_weak_pass_edge);
        Self {
            live: live
                .into_iter()
                .map(|idx| graph.object_at(idx).clone())
                .collect(),
        }
    }

    /// True when the object is live at all.
    pub(super) fn is_live(&self, id: &ObjectId) -> bool {
        self.live.contains(id)
    }
}

/// One object reachability never reached — a `scan` finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnusedObject {
    /// The unused object.
    pub id: ObjectId,
    /// Every graph edge pointing at it. Empty means nothing references the
    /// object at all: the root cause of its dead chain, deletable outright.
    pub used_by: Vec<UsedBy>,
}

/// One referencing object behind an [`UnusedObject`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedBy {
    /// The referencing object.
    pub id: ObjectId,
    /// What kind of use the edge records.
    pub provenance: Provenance,
    /// True when the referencing object is itself unused — the "also unused"
    /// of the `← only used by X (also unused)` annotation. False means the
    /// referencing object is live but its use could not keep this one alive:
    /// a key column kept alive only as a relationship endpoint, holding its
    /// table's only reference.
    pub also_unused: bool,
}
