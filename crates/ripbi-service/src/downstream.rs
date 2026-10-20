//! From one semantic model to the reports a scan needs: the downstream
//! relations, minus org-app copies, each with an access forecast.
//!
//! What the tenant taught us (see `FETCH_PLAN.md`):
//!
//! - Downstream crosses workspaces and does **not** hide reports the caller
//!   cannot read — so the forecast can name exactly what will be missing.
//! - Every report published in an org app comes back twice: the workspace
//!   report and its app copy, same name, same workspace id. The copy is not a
//!   workspace item and always `404`s on `getDefinition`; it is told apart by
//!   its absence from the workspace's report listing, or, where the caller
//!   cannot list the workspace, by the org-app listings.
//! - Workspace roles predict `getDefinition` exactly: Admin, Member, or
//!   Contributor downloads; Viewer and no access both answer `404`.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::api::{Item, Relation};
use crate::error::Result;
use crate::service::{Direction, Service};

/// The roles that may export a definition.
pub const WRITE_ROLES: [&str; 3] = ["Admin", "Member", "Contributor"];

/// A semantic model, located.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelRef {
    /// The model GUID.
    pub id: String,
    /// The model name.
    pub name: String,
    /// The owning workspace's GUID.
    pub workspace_id: String,
    /// The owning workspace's name.
    pub workspace_name: String,
}

/// What the caller may do with an item, forecast from workspace roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Access {
    /// Admin, Member, or Contributor: `getDefinition` works.
    Downloadable,
    /// Viewer: the item is visible, its definition is not.
    Viewer,
    /// Not a member of the workspace at all.
    NoAccess,
}

impl Access {
    /// The machine name used in `source.json`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Downloadable => "downloadable",
            Self::Viewer => "viewer",
            Self::NoAccess => "no_access",
        }
    }
}

/// A downstream item outside the scan: an app copy, a dashboard, a
/// paginated report, another model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemRef {
    /// The item GUID.
    pub id: String,
    /// The item name.
    pub name: String,
    /// The item type as the API spells it.
    pub kind: String,
    /// The owning workspace's GUID.
    pub workspace_id: String,
    /// The owning workspace's name, when known.
    pub workspace_name: String,
}

/// A workspace report bound to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRef {
    /// The report GUID.
    pub id: String,
    /// The report name.
    pub name: String,
    /// The owning workspace's GUID.
    pub workspace_id: String,
    /// The owning workspace's name.
    pub workspace_name: String,
    /// The forecast.
    pub access: Access,
}

/// Everything downstream of one model, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downstream {
    /// The model.
    pub model: ModelRef,
    /// The forecast for the model itself.
    pub model_access: Access,
    /// The workspace reports bound directly to the model, sorted by
    /// workspace then name.
    pub reports: Vec<ReportRef>,
    /// Org-app copies of those reports: skipped, never downloadable.
    pub app_copies: Vec<ItemRef>,
    /// Every other downstream item: dashboards, paginated reports, other
    /// models, and reports reached only through another item.
    pub other_items: Vec<ItemRef>,
    /// The raw edges, for provenance.
    pub relations: Vec<Relation>,
    /// Things the caller should know that are not errors, one sentence each.
    pub notes: Vec<String>,
}

impl Downstream {
    /// The reports the forecast says cannot be downloaded.
    pub fn unreadable(&self) -> impl Iterator<Item = &ReportRef> {
        self.reports
            .iter()
            .filter(|report| report.access != Access::Downloadable)
    }
}

/// Walks the model's downstream relations and classifies every item.
///
/// # Errors
///
/// Any [`crate::Error`] from the relations or workspace calls. Failures of
/// the best-effort calls (a workspace's report listing, the org-app
/// listings) become [`Downstream::notes`] instead.
pub fn discover(service: &Service, model: &ModelRef) -> Result<Downstream> {
    let relations = service.relations(&model.workspace_id, &model.id, Direction::Downstream)?;
    let member: HashSet<String> = service
        .workspaces()?
        .into_iter()
        .map(|workspace| workspace.id)
        .collect();
    let writable: HashSet<String> = service
        .workspaces_with_roles(&WRITE_ROLES)?
        .into_iter()
        .map(|workspace| workspace.id)
        .collect();
    let access = |workspace_id: &str| {
        if writable.contains(workspace_id) {
            Access::Downloadable
        } else if member.contains(workspace_id) {
            Access::Viewer
        } else {
            Access::NoAccess
        }
    };
    let names: HashMap<String, String> = relations
        .workspaces
        .iter()
        .map(|workspace| (workspace.id.clone(), workspace.display_name.clone()))
        .collect();
    let name_of = |workspace_id: &str| {
        names
            .get(workspace_id)
            .cloned()
            .or_else(|| (workspace_id == model.workspace_id).then(|| model.workspace_name.clone()))
            .unwrap_or_default()
    };
    let item_ref = |item: &Item| ItemRef {
        id: item.id.clone(),
        name: item.display_name.clone(),
        kind: item.kind.clone(),
        workspace_id: item.workspace_id.clone(),
        workspace_name: name_of(&item.workspace_id),
    };

    // Reports bound directly to the model. The API is transitive, so a
    // report on a model downstream of this one is listed too, but binds that
    // other model. With no edges at all (schema drift), trust the items.
    let direct: Option<HashSet<&str>> = (!relations.relations.is_empty()).then(|| {
        relations
            .relations
            .iter()
            .filter(|edge| edge.dependent_on_item_id.eq_ignore_ascii_case(&model.id))
            .map(|edge| edge.item_id.as_str())
            .collect()
    });
    let mut seen = HashSet::new();
    let mut candidates: BTreeMap<String, Vec<&Item>> = BTreeMap::new();
    let mut other_items = Vec::new();
    for item in &relations.items {
        if item.id.eq_ignore_ascii_case(&model.id) || !seen.insert(item.id.to_ascii_lowercase()) {
            continue;
        }
        let bound = direct
            .as_ref()
            .is_none_or(|direct| direct.contains(item.id.as_str()));
        if item.is_report() && bound {
            candidates
                .entry(item.workspace_id.clone())
                .or_default()
                .push(item);
        } else {
            other_items.push(item_ref(item));
        }
    }

    let mut notes = Vec::new();
    let mut reports = Vec::new();
    let mut app_copies = Vec::new();
    let mut unlisted: Vec<(&String, Vec<&Item>)> = Vec::new();
    for (workspace_id, items) in &candidates {
        let listing = if member.contains(workspace_id) {
            match service.reports(workspace_id) {
                Ok(listing) => Some(listing),
                Err(error) => {
                    notes.push(format!(
                        "could not list the reports in workspace '{}' to tell app copies apart: {error}",
                        name_of(workspace_id)
                    ));
                    None
                }
            }
        } else {
            None
        };
        match listing {
            Some(listing) => {
                let real: HashSet<String> = listing
                    .iter()
                    .map(|report| report.id.to_ascii_lowercase())
                    .collect();
                for item in items {
                    if real.contains(&item.id.to_ascii_lowercase()) {
                        reports.push(report_ref(
                            item,
                            name_of(workspace_id),
                            access(workspace_id),
                        ));
                    } else {
                        app_copies.push(item_ref(item));
                    }
                }
            }
            None => unlisted.push((workspace_id, items.clone())),
        }
    }

    // Workspaces the caller cannot list: ask the org apps which ids are
    // copies. Best effort — the caller may not see every app.
    if !unlisted.is_empty() {
        let copies = app_copy_ids(service, &mut notes);
        for (workspace_id, items) in unlisted {
            let mut names_seen: HashMap<String, usize> = HashMap::new();
            for item in items {
                if copies.contains(&item.id.to_ascii_lowercase()) {
                    app_copies.push(item_ref(item));
                    continue;
                }
                *names_seen
                    .entry(item.display_name.to_lowercase())
                    .or_default() += 1;
                reports.push(report_ref(
                    item,
                    name_of(workspace_id),
                    access(workspace_id),
                ));
            }
            let doubled = names_seen.values().filter(|count| **count > 1).count();
            if doubled > 0 {
                notes.push(format!(
                    "{doubled} report name(s) in workspace '{}' appear twice; one of each is \
                     likely an org-app copy ripbi cannot tell apart without access",
                    name_of(workspace_id)
                ));
            }
        }
    }

    reports.sort_by(|a, b| {
        (
            a.workspace_name.to_lowercase(),
            a.name.to_lowercase(),
            &a.id,
        )
            .cmp(&(
                b.workspace_name.to_lowercase(),
                b.name.to_lowercase(),
                &b.id,
            ))
    });
    Ok(Downstream {
        model_access: access(&model.workspace_id),
        model: model.clone(),
        reports,
        app_copies,
        other_items,
        relations: relations.relations,
        notes,
    })
}

fn report_ref(item: &Item, workspace_name: String, access: Access) -> ReportRef {
    ReportRef {
        id: item.id.clone(),
        name: item.display_name.clone(),
        workspace_id: item.workspace_id.clone(),
        workspace_name,
        access,
    }
}

/// Every app-copy report id the caller can see, lower-cased.
fn app_copy_ids(service: &Service, notes: &mut Vec<String>) -> HashSet<String> {
    let mut ids = HashSet::new();
    let apps = match service.apps() {
        Ok(apps) => apps,
        Err(error) => {
            notes.push(format!(
                "could not list org apps to tell app copies apart: {error}"
            ));
            return ids;
        }
    };
    for app in apps {
        match service.app_reports(&app.id) {
            Ok(reports) => ids.extend(reports.into_iter().map(|r| r.id.to_ascii_lowercase())),
            Err(error) => notes.push(format!(
                "could not list the reports in app '{}': {error}",
                app.name
            )),
        }
    }
    ids
}

/// The per-workspace listing compared with the downstream answer — the
/// safety net while the relations API is beta.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verification {
    /// Reports bound to the model that downstream did not return.
    pub only_in_listing: Vec<ItemRef>,
    /// Downstream reports in a member workspace that the listing lacks.
    pub only_in_downstream: Vec<ItemRef>,
    /// Workspaces checked.
    pub workspaces_checked: usize,
    /// Workspaces whose listing failed, with the reason.
    pub failures: Vec<String>,
}

/// Lists every member workspace's reports (Power BI API, which names each
/// report's model) and diffs the ones bound to the model against `found`.
///
/// # Errors
///
/// Any [`crate::Error`] from the workspace list; per-workspace failures are
/// collected in [`Verification::failures`].
pub fn verify(service: &Service, found: &Downstream) -> Result<Verification> {
    let workspaces = service.workspaces()?;
    let known: HashSet<String> = found
        .reports
        .iter()
        .map(|report| report.id.to_ascii_lowercase())
        .chain(
            found
                .app_copies
                .iter()
                .map(|copy| copy.id.to_ascii_lowercase()),
        )
        .collect();
    let mut verification = Verification::default();
    let mut listed = HashSet::new();
    let mut member = HashSet::new();
    for workspace in &workspaces {
        member.insert(workspace.id.to_ascii_lowercase());
        let reports = match service.group_reports(&workspace.id) {
            Ok(reports) => reports,
            Err(error) => {
                verification
                    .failures
                    .push(format!("{}: {error}", workspace.display_name));
                continue;
            }
        };
        verification.workspaces_checked += 1;
        for report in reports {
            let bound = report
                .dataset_id
                .as_deref()
                .is_some_and(|dataset| dataset.eq_ignore_ascii_case(&found.model.id));
            if !bound {
                continue;
            }
            let id = report.id.to_ascii_lowercase();
            if !known.contains(&id) {
                verification.only_in_listing.push(ItemRef {
                    id: report.id.clone(),
                    name: report.name.clone(),
                    kind: "Report".to_string(),
                    workspace_id: workspace.id.clone(),
                    workspace_name: workspace.display_name.clone(),
                });
            }
            listed.insert(id);
        }
    }
    for report in &found.reports {
        let id = report.id.to_ascii_lowercase();
        if member.contains(&report.workspace_id.to_ascii_lowercase()) && !listed.contains(&id) {
            verification.only_in_downstream.push(ItemRef {
                id: report.id.clone(),
                name: report.name.clone(),
                kind: "Report".to_string(),
                workspace_id: report.workspace_id.clone(),
                workspace_name: report.workspace_name.clone(),
            });
        }
    }
    Ok(verification)
}

/// The model a report is bound to, from the report's upstream relations.
///
/// # Errors
///
/// Any [`crate::Error`] from the call; [`crate::Error::Response`] when the
/// answer names no semantic model.
pub fn model_of_report(service: &Service, workspace_id: &str, report_id: &str) -> Result<ModelRef> {
    let relations = service.relations(workspace_id, report_id, Direction::Upstream)?;
    let models: HashMap<&str, &Item> = relations
        .items
        .iter()
        .filter(|item| item.is_semantic_model())
        .map(|item| (item.id.as_str(), item))
        .collect();
    // Upstream is transitive (report → model → warehouse): prefer the model
    // the report itself depends on.
    let direct = relations
        .relations
        .iter()
        .filter(|edge| edge.item_id.eq_ignore_ascii_case(report_id))
        .find_map(|edge| models.get(edge.dependent_on_item_id.as_str()).copied());
    let model = direct
        .or_else(|| {
            (models.len() == 1)
                .then(|| models.values().copied().next())
                .flatten()
        })
        .ok_or_else(|| crate::Error::Response {
            endpoint: format!("relations/upstream of report {report_id}"),
            detail: "no semantic model found upstream of the report".to_string(),
        })?;
    let workspace_name = relations
        .workspaces
        .iter()
        .find(|workspace| workspace.id.eq_ignore_ascii_case(&model.workspace_id))
        .map(|workspace| workspace.display_name.clone())
        .unwrap_or_default();
    Ok(ModelRef {
        id: model.id.clone(),
        name: model.display_name.clone(),
        workspace_id: model.workspace_id.clone(),
        workspace_name,
    })
}
