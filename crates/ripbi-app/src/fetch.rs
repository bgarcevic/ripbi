//! Fetching from a Power BI / Fabric tenant: resolve one semantic model,
//! forecast which of its downstream reports the caller may read, download the
//! model and every readable report, and lay them out so a scan runs on the
//! folder with no flags.
//!
//! A partial report set is the most dangerous input ripbi can get — objects
//! only the missing reports use come back as unused — so the forecast always
//! reaches the [`Observer`], an interactive run asks before downloading an
//! incomplete set, and a non-interactive one refuses unless
//! [`FetchRequest::allow_incomplete`]. All network work lives in
//! `ripbi-service`; this module decides, and the front end presents.

pub mod layout;

use std::collections::HashMap;
use std::path::PathBuf;

use ripbi_service::api::{Definition, Workspace, is_guid};
use ripbi_service::auth::{AzureCli, Secret, ServicePrincipal, StaticTokens, TokenSource};
use ripbi_service::downstream::{self, Access, Downstream, ModelRef, ReportRef, Verification};
use ripbi_service::link::{LinkKind, parse_link};
use ripbi_service::service::{DefinitionKind, DefinitionRequest, FetchOptions};
use ripbi_service::{Error as ServiceError, Service};

use crate::error::ScanError;

pub use layout::{MANIFEST_PATH, Manifest, ReportStatus, Written};

/// Where the tokens come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AuthMode {
    /// `RIPBI_FABRIC_TOKEN`, else a service principal, else the Azure CLI.
    #[default]
    Auto,
    /// The Azure CLI's signed-in account.
    Az,
    /// `RIPBI_FABRIC_TOKEN` (and `RIPBI_POWERBI_TOKEN`, if set).
    Env,
    /// A service principal: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`,
    /// `AZURE_CLIENT_SECRET`.
    Sp,
}

/// Picks the token source: explicit `mode`, else the first configured of
/// env token → service principal → Azure CLI. `env` reads one variable.
///
/// # Errors
///
/// When `Env` or `Sp` names variables that are unset.
pub fn token_source(
    mode: AuthMode,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Box<dyn TokenSource>, ScanError> {
    let from_env = || {
        env("RIPBI_FABRIC_TOKEN").map(|fabric| StaticTokens {
            fabric: Secret::new(fabric),
            power_bi: env("RIPBI_POWERBI_TOKEN").map(Secret::new),
            origin: "RIPBI_FABRIC_TOKEN".to_string(),
        })
    };
    let principal = || match (
        env("AZURE_TENANT_ID"),
        env("AZURE_CLIENT_ID"),
        env("AZURE_CLIENT_SECRET"),
    ) {
        (Some(tenant_id), Some(client_id), Some(secret)) => Some(ServicePrincipal {
            tenant_id,
            client_id,
            client_secret: Secret::new(secret),
            authority: env("AZURE_AUTHORITY_HOST")
                .unwrap_or_else(|| ServicePrincipal::AUTHORITY.to_string()),
        }),
        _ => None,
    };
    match mode {
        AuthMode::Auto => Ok(if let Some(tokens) = from_env() {
            Box::new(tokens)
        } else if let Some(principal) = principal() {
            Box::new(principal)
        } else {
            Box::new(AzureCli)
        }),
        AuthMode::Az => Ok(Box::new(AzureCli)),
        AuthMode::Env => from_env()
            .map(|tokens| Box::new(tokens) as Box<dyn TokenSource>)
            .ok_or_else(|| {
                ScanError::new("--auth env needs RIPBI_FABRIC_TOKEN, which is not set")
                    .with_hint("export a Fabric API token, e.g. from `az account get-access-token`")
            }),
        AuthMode::Sp => principal()
            .map(|principal| Box::new(principal) as Box<dyn TokenSource>)
            .ok_or_else(|| {
                ScanError::new(
                    "--auth sp needs AZURE_TENANT_ID, AZURE_CLIENT_ID, and AZURE_CLIENT_SECRET",
                )
                .with_hint(
                    "set all three for the service principal; ripbi never reads them from files",
                )
            }),
    }
}

/// Everything one fetch is asked to do.
#[derive(Debug, Clone, Default)]
pub struct FetchRequest {
    /// The working directory the output folder resolves against.
    pub cwd: PathBuf,
    /// A Power BI link to the model or one of its reports.
    pub link: Option<String>,
    /// The workspace, by name or id.
    pub workspace: Option<String>,
    /// The model, by name or id.
    pub model_name: Option<String>,
    /// The output folder; `./<model name>` when unset.
    pub out: Option<PathBuf>,
    /// Cross-check the downstream listing against each workspace's items.
    pub verify: bool,
    /// Forecast only; download nothing.
    pub dry_run: bool,
    /// Download the readable reports even when some cannot be read.
    pub allow_incomplete: bool,
    /// Write into a folder fetch did not write, or that holds another model.
    pub force: bool,
    /// Whether the front end can ask: the pickers and the incomplete-set
    /// question go to the [`Observer`] only when set.
    pub interactive: bool,
}

/// How a front end follows and steers a fetch. Calls arrive in order:
/// `signed_in`, `pick` (only when interactive and nothing named the model),
/// `forecast`, `confirm_incomplete`, `exporting` (repeatedly), `writing`.
pub trait Observer {
    /// The sign-in that will be used, and the tenant its token names.
    ///
    /// # Errors
    /// Whatever the front end fails with; the fetch stops.
    fn signed_in(&mut self, _auth: &str, _tenant: Option<&str>) -> Result<(), ScanError> {
        Ok(())
    }

    /// Choose one of `labels` (never empty). `title` heads the list, `prompt`
    /// asks for the choice.
    ///
    /// # Errors
    /// Whatever the front end fails with; the fetch stops.
    fn pick(&mut self, title: &str, _prompt: &str, _labels: &[String]) -> Result<usize, ScanError> {
        Err(ScanError::new(format!("nothing to pick from: {title}")))
    }

    /// The downstream forecast, before anything downloads.
    ///
    /// # Errors
    /// Whatever the front end fails with; the fetch stops.
    fn forecast(
        &mut self,
        _found: &Downstream,
        _verification: Option<&Verification>,
    ) -> Result<(), ScanError> {
        Ok(())
    }

    /// `unreadable` of the `total` downstream reports cannot be read: fetch
    /// the readable ones anyway? Asked only when interactive.
    ///
    /// # Errors
    /// Whatever the front end fails with; the fetch stops.
    fn confirm_incomplete(&mut self, _unreadable: usize, _total: usize) -> Result<bool, ScanError> {
        Ok(false)
    }

    /// `done` of `total` definitions exported; first called with `done == 0`.
    fn exporting(&mut self, _done: usize, _total: usize) {}

    /// Every export finished; the project is being written.
    fn writing(&mut self) {}
}

impl Observer for () {}

/// What a successful fetch produced.
#[derive(Debug)]
pub enum FetchOutcome {
    /// [`FetchRequest::dry_run`]: the forecast only.
    Forecast,
    /// A project on disk.
    Written {
        /// The project folder.
        root: PathBuf,
        /// What was written.
        written: Box<Written>,
        /// More reports failed than the forecast predicted and nobody accepted
        /// an incomplete set: the project is on disk, but the run failed.
        refused: bool,
    },
}

/// Runs one fetch against `service`.
///
/// # Errors
///
/// Sign-in, lookup, and network failures; a model that cannot be exported; an
/// incomplete set not allowed; a cancelled confirmation; an output folder
/// that is not fetch's to write; I/O.
pub fn run(
    request: &FetchRequest,
    service: &Service,
    observer: &mut dyn Observer,
) -> Result<FetchOutcome, ScanError> {
    let tenant = service.tenant_id().map_err(service_error)?;
    observer.signed_in(&service.auth_description(), tenant.as_deref())?;

    let model = resolve_model(request, service, observer)?;
    let found = downstream::discover(service, &model).map_err(service_error)?;
    let verification = if request.verify {
        Some(downstream::verify(service, &found).map_err(service_error)?)
    } else {
        None
    };
    observer.forecast(&found, verification.as_ref())?;

    if found.model_access != Access::Downloadable {
        return Err(ScanError::new(format!(
            "cannot download the model '{}': {} in workspace '{}'",
            model.name,
            access_phrase(found.model_access),
            model.workspace_name
        ))
        .with_hint("exporting a definition needs the Admin, Member, or Contributor role"));
    }
    if request.dry_run {
        return Ok(FetchOutcome::Forecast);
    }

    let unreadable = found.unreadable().count();
    let mut accepted_incomplete = request.allow_incomplete;
    if unreadable > 0 && !accepted_incomplete {
        if request.interactive {
            if !observer.confirm_incomplete(unreadable, found.reports.len())? {
                return Err(ScanError::new("cancelled; nothing was downloaded"));
            }
            accepted_incomplete = true;
        } else {
            return Err(ScanError::new(format!(
                "{unreadable} of {} downstream reports cannot be read, so a scan would be incomplete",
                found.reports.len()
            ))
            .with_hint(
                "pass --allow-incomplete to fetch the readable ones, or get Contributor on the workspaces listed above",
            ));
        }
    }

    let root = layout::output_dir(&request.cwd, request.out.as_deref(), &model);
    let plan = layout::prepare(&root, &model, request.force)?;
    let downloads = download(service, &found, observer)?;
    observer.writing();
    let manifest = layout::manifest(&found, tenant, &downloads, verification.as_ref());
    let written = layout::write(&plan, &found, &downloads, manifest)?;

    let refused = written.manifest.unreadable_count() > unreadable && !accepted_incomplete;
    Ok(FetchOutcome::Written {
        root,
        written: Box::new(written),
        refused,
    })
}

/// How a forecast reads in a sentence.
#[must_use]
pub fn access_phrase(access: Access) -> &'static str {
    match access {
        Access::Downloadable => "you can download it",
        Access::Viewer => "you are a Viewer",
        Access::NoAccess => "you are not a member",
    }
}

/// The downloads, keyed for the layout writer.
pub struct Downloads {
    /// The model's definition.
    pub model: Definition,
    /// Each downloadable report's outcome, by report id.
    pub reports: HashMap<String, Result<Definition, ServiceError>>,
}

/// Fires every export, then polls them together.
fn download(
    service: &Service,
    found: &Downstream,
    observer: &mut dyn Observer,
) -> Result<Downloads, ScanError> {
    let readable: Vec<&ReportRef> = found
        .reports
        .iter()
        .filter(|report| report.access == Access::Downloadable)
        .collect();
    let mut requests = vec![DefinitionRequest {
        kind: DefinitionKind::SemanticModel,
        workspace_id: found.model.workspace_id.clone(),
        item_id: found.model.id.clone(),
        format: Some("TMDL".to_string()),
    }];
    requests.extend(readable.iter().map(|report| DefinitionRequest {
        kind: DefinitionKind::Report,
        workspace_id: report.workspace_id.clone(),
        item_id: report.id.clone(),
        format: None,
    }));
    observer.exporting(0, requests.len());
    let mut results = service
        .fetch_definitions(&requests, FetchOptions::default(), &mut |done, total| {
            observer.exporting(done, total);
        })
        .into_iter();
    let model = results
        .next()
        .unwrap_or_else(|| Err(ServiceError::Timeout("the model".to_string())))
        .map_err(|error| {
            service_error(error).with_hint(
                "the model must export as TMDL; check the workspace role and that the model is not a Direct Lake or push dataset",
            )
        })?;
    let reports = readable
        .iter()
        .map(|report| report.id.clone())
        .zip(results)
        .collect();
    Ok(Downloads { model, reports })
}

/// Resolves the model from a link, from the workspace/model names, or from
/// the observer's pickers.
fn resolve_model(
    request: &FetchRequest,
    service: &Service,
    observer: &mut dyn Observer,
) -> Result<ModelRef, ScanError> {
    let interactive = request.interactive;
    if let Some(text) = &request.link {
        let link = parse_link(text).ok_or_else(|| {
            ScanError::new(format!(
                "'{text}' is not a Power BI link to a semantic model or report"
            ))
            .with_hint(
                "copy the address of the model's details page: https://app.powerbi.com/groups/<workspace>/datasets/<model>/details",
            )
        })?;
        let mut model = match link.kind {
            LinkKind::SemanticModel => {
                let item = service
                    .semantic_model(&link.workspace_id, &link.item_id)
                    .map_err(service_error)?;
                ModelRef {
                    id: item.id,
                    name: item.display_name,
                    workspace_id: link.workspace_id,
                    workspace_name: String::new(),
                }
            }
            LinkKind::Report => {
                downstream::model_of_report(service, &link.workspace_id, &link.item_id)
                    .map_err(service_error)?
            }
        };
        if model.workspace_name.is_empty() {
            model.workspace_name = service
                .workspace(&model.workspace_id)
                .map(|workspace| workspace.display_name)
                .unwrap_or_default();
        }
        return Ok(model);
    }

    let workspace = match &request.workspace {
        Some(wanted) => Some(find_workspace(service, wanted)?),
        None if request.model_name.is_some() => None,
        None if interactive => {
            let mut workspaces = service.workspaces().map_err(service_error)?;
            workspaces.sort_by_key(|workspace| workspace.display_name.to_lowercase());
            let labels: Vec<String> = workspaces
                .iter()
                .map(|workspace| workspace.display_name.clone())
                .collect();
            let choice = pick(observer, "Workspaces:", "Select a workspace", &labels)?;
            Some(workspaces.swap_remove(choice))
        }
        None => {
            return Err(
                ScanError::new("no semantic model given").with_hint(
                    "pass a link, or --workspace and --model (see ripbi fetch --help); --no-input disables the pickers",
                ),
            );
        }
    };

    match (workspace, &request.model_name) {
        (Some(workspace), Some(wanted)) => {
            let models = service
                .semantic_models(&workspace.id)
                .map_err(service_error)?;
            let matches: Vec<ModelRef> = models
                .into_iter()
                .filter(|item| matches_name(&item.id, &item.display_name, wanted))
                .map(|item| model_ref(item.id, item.display_name, &workspace))
                .collect();
            one_model(matches, wanted, Some(&workspace.display_name))
        }
        (Some(workspace), None) => {
            if !interactive {
                return Err(ScanError::new("no semantic model given")
                    .with_hint("pass --model NAME; --no-input disables the picker"));
            }
            let mut models = service
                .semantic_models(&workspace.id)
                .map_err(service_error)?;
            if models.is_empty() {
                return Err(ScanError::new(format!(
                    "workspace '{}' has no semantic models",
                    workspace.display_name
                )));
            }
            models.sort_by_key(|item| item.display_name.to_lowercase());
            let labels: Vec<String> = models
                .iter()
                .map(|item| item.display_name.clone())
                .collect();
            let choice = pick(
                observer,
                &format!("Semantic models in {}:", workspace.display_name),
                "Select a model",
                &labels,
            )?;
            let item = models.swap_remove(choice);
            Ok(model_ref(item.id, item.display_name, &workspace))
        }
        (None, Some(wanted)) => {
            // No workspace: search every workspace the caller can see.
            let mut matches = Vec::new();
            for workspace in service.workspaces().map_err(service_error)? {
                let Ok(models) = service.semantic_models(&workspace.id) else {
                    continue;
                };
                matches.extend(
                    models
                        .into_iter()
                        .filter(|item| matches_name(&item.id, &item.display_name, wanted))
                        .map(|item| model_ref(item.id, item.display_name, &workspace)),
                );
            }
            one_model(matches, wanted, None)
        }
        (None, None) => unreachable!("a missing workspace without a model name returned above"),
    }
}

/// The observer's pick, refused for an empty list and checked against it.
fn pick(
    observer: &mut dyn Observer,
    title: &str,
    prompt: &str,
    labels: &[String],
) -> Result<usize, ScanError> {
    if labels.is_empty() {
        return Err(ScanError::new(format!("nothing to pick from: {title}")));
    }
    let choice = observer.pick(title, prompt, labels)?;
    if choice >= labels.len() {
        return Err(ScanError::new(format!("no such choice: {}", choice + 1)));
    }
    Ok(choice)
}

fn model_ref(id: String, name: String, workspace: &Workspace) -> ModelRef {
    ModelRef {
        id,
        name,
        workspace_id: workspace.id.clone(),
        workspace_name: workspace.display_name.clone(),
    }
}

fn matches_name(id: &str, name: &str, wanted: &str) -> bool {
    if is_guid(wanted) {
        id.eq_ignore_ascii_case(wanted)
    } else {
        name.to_lowercase() == wanted.to_lowercase()
    }
}

fn find_workspace(service: &Service, wanted: &str) -> Result<Workspace, ScanError> {
    let mut matches: Vec<Workspace> = service
        .workspaces()
        .map_err(service_error)?
        .into_iter()
        .filter(|workspace| matches_name(&workspace.id, &workspace.display_name, wanted))
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 if is_guid(wanted) => {
            // Not a member, perhaps, but the id may still resolve.
            service.workspace(wanted).map_err(|error| {
                service_error(error).with_hint("check the workspace id, and that you are a member")
            })
        }
        0 => Err(ScanError::new(format!("no workspace named '{wanted}'"))
            .with_hint("workspace names match case-insensitively; an id always works")),
        _ => Err(ScanError::new(format!(
            "{} workspaces are named '{wanted}':{}",
            matches.len(),
            matches
                .iter()
                .map(|workspace| format!("\n  - {}", workspace.id))
                .collect::<String>()
        ))
        .with_hint("pass the workspace id instead")),
    }
}

fn one_model(
    mut matches: Vec<ModelRef>,
    wanted: &str,
    workspace: Option<&str>,
) -> Result<ModelRef, ScanError> {
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => {
            let place = workspace.map_or_else(
                || "in any workspace you can see".to_string(),
                |workspace| format!("in workspace '{workspace}'"),
            );
            Err(
                ScanError::new(format!("no semantic model '{wanted}' {place}"))
                    .with_hint("model names match case-insensitively; an id always works"),
            )
        }
        _ => Err(ScanError::new(format!(
            "{} semantic models match '{wanted}':{}",
            matches.len(),
            matches
                .iter()
                .map(|model| format!(
                    "\n  - {} / {} ({})",
                    model.workspace_name, model.name, model.id
                ))
                .collect::<String>()
        ))
        .with_hint("pass --workspace, or the model id")),
    }
}

/// A service error worded for people, with the conventional fix.
#[must_use]
pub fn service_error(error: ServiceError) -> ScanError {
    let hint = match &error {
        ServiceError::Auth(_) => Some(
            "run `az login`, or set RIPBI_FABRIC_TOKEN, or AZURE_TENANT_ID + AZURE_CLIENT_ID + AZURE_CLIENT_SECRET",
        ),
        ServiceError::Http { status: 401, .. } => {
            Some("the token was rejected: sign in again (az login) or check the tenant")
        }
        ServiceError::Http { status: 403, .. } => Some(
            "a service principal also needs the tenant setting 'Service principals can use Fabric APIs'",
        ),
        ServiceError::Http { status: 404, .. } => {
            Some("the item does not exist, or you are not allowed to see it")
        }
        _ => None,
    };
    let message = ScanError::new(error.to_string());
    match hint {
        Some(hint) => message.with_hint(hint),
        None => message,
    }
}
