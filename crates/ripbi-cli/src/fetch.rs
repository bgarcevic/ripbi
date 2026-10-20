//! `ripbi fetch`: sign in to a Power BI / Fabric tenant, resolve one semantic
//! model, forecast which of its downstream reports the caller may read,
//! download the model and every readable report, and lay them out so
//! `ripbi scan` runs on the folder with no flags.
//!
//! A partial report set is the most dangerous input ripbi can get — objects
//! only the missing reports use come back as unused — so the forecast always
//! prints, an interactive run asks before downloading an incomplete set, and
//! a non-interactive one refuses (exit `2`) unless `--allow-incomplete`.
//! All network work lives in `ripbi-service`; this module decides and prints.

mod layout;
mod render;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ripbi_service::api::{Definition, Workspace, is_guid};
use ripbi_service::auth::{AzureCli, Secret, ServicePrincipal, StaticTokens, TokenSource};
use ripbi_service::downstream::{self, Access, Downstream, ModelRef, ReportRef};
use ripbi_service::http::UreqTransport;
use ripbi_service::link::{LinkKind, parse_link};
use ripbi_service::service::{DefinitionKind, DefinitionRequest, FetchOptions};
use ripbi_service::{Error as ServiceError, Service};

use crate::cli::{AuthMode, FetchArgs, ScanArgs};
use crate::error::ScanError;
use crate::scan::{self, Streams};
use crate::style::Palette;

pub use layout::{MANIFEST_PATH, Manifest, ReportStatus};

/// Exit code: fetched (or forecast) successfully.
pub const EXIT_OK: i32 = 0;
/// Exit code: usage, sign-in, network, or an incomplete set not allowed.
pub const EXIT_ERROR: i32 = 2;

/// One request's budget. A definition export answers within seconds; the
/// long wait is polled, never held open.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs `fetch` against the real tenant, from the process's working directory.
#[must_use = "the return value is the process exit code"]
pub fn run(args: &FetchArgs, streams: &mut Streams<'_>) -> i32 {
    let palette = Palette::detect(streams.stderr_is_tty, args.no_color);
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            return report_error(
                args,
                streams,
                &palette,
                &ScanError::new(format!("cannot read the current directory: {error}")),
            );
        }
    };
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let tokens = match token_source(args.auth, &env) {
        Ok(tokens) => tokens,
        Err(error) => return report_error(args, streams, &palette, &error),
    };
    let service = Service::new(Box::new(UreqTransport::new(REQUEST_TIMEOUT)), tokens);
    run_with(args, &service, &cwd, streams)
}

/// Runs `fetch` with the service and working directory injected (tests).
#[must_use = "the return value is the process exit code"]
pub fn run_with(args: &FetchArgs, service: &Service, cwd: &Path, streams: &mut Streams<'_>) -> i32 {
    let palette = Palette::detect(streams.stderr_is_tty, args.no_color);
    match fetch(args, service, cwd, streams, &palette) {
        Ok(Fetched::Forecast) => EXIT_OK,
        Ok(Fetched::Written { root, exit }) => {
            if args.scan && exit == EXIT_OK {
                scan_fetched(args, &root, streams)
            } else {
                exit
            }
        }
        Err(error) => report_error(args, streams, &palette, &error),
    }
}

/// Picks the token source: explicit `--auth`, else the first configured of
/// env token → service principal → Azure CLI. `env` reads one variable.
///
/// # Errors
///
/// When `--auth env` or `--auth sp` names variables that are unset.
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

/// What a successful run produced.
enum Fetched {
    /// `--dry-run`: the forecast only.
    Forecast,
    /// A project on disk, and the exit code it earned.
    Written { root: PathBuf, exit: i32 },
}

/// The whole flow, errors left to the caller.
fn fetch(
    args: &FetchArgs,
    service: &Service,
    cwd: &Path,
    streams: &mut Streams<'_>,
    palette: &Palette,
) -> Result<Fetched, ScanError> {
    let started = Instant::now();
    let interactive = streams.stdin_is_tty && !args.no_input && !args.quiet;
    let tenant = service.tenant_id().map_err(service_error)?;
    if !args.quiet {
        let tenant = tenant
            .as_deref()
            .map(|tenant| format!(" (tenant {tenant})"))
            .unwrap_or_default();
        writeln!(
            streams.err,
            "{}",
            palette.info_note(&format!(
                "Signed in via {}{tenant}",
                service.auth_description()
            ))
        )?;
    }

    let model = resolve_model(args, service, interactive, streams, palette)?;
    let found = downstream::discover(service, &model).map_err(service_error)?;
    let verification = if args.verify {
        Some(downstream::verify(service, &found).map_err(service_error)?)
    } else {
        None
    };
    if !args.quiet {
        render::forecast(streams.err, palette, &found, verification.as_ref())?;
    }

    if found.model_access != Access::Downloadable {
        return Err(ScanError::new(format!(
            "cannot download the model '{}': {} in workspace '{}'",
            model.name,
            render::access_phrase(found.model_access),
            model.workspace_name
        ))
        .with_hint("exporting a definition needs the Admin, Member, or Contributor role"));
    }
    if args.dry_run {
        return Ok(Fetched::Forecast);
    }

    let unreadable = found.unreadable().count();
    let mut accepted_incomplete = args.allow_incomplete;
    if unreadable > 0 && !accepted_incomplete {
        if interactive {
            if !confirm(streams, "Continue with the readable reports? [y/N] ")? {
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

    let root = layout::output_dir(cwd, args.out.as_deref(), &model);
    let plan = layout::prepare(&root, &model, args.force)?;

    let downloads = download(service, &found, streams, args.quiet)?;
    let manifest = layout::manifest(&found, tenant, &downloads, verification.as_ref());
    let written = layout::write(&plan, &found, &downloads, manifest)?;

    let failed = written.manifest.unreadable_count();
    let exit = if failed > unreadable && !accepted_incomplete {
        EXIT_ERROR
    } else {
        EXIT_OK
    };
    if !args.quiet {
        render::summary(
            streams.err,
            palette,
            &written,
            &display_path(cwd, &root),
            started.elapsed(),
            exit == EXIT_ERROR,
        )?;
        if !written.config_written {
            writeln!(
                streams.err,
                "{}",
                palette.warn_note(
                    "kept the existing ripbi.toml (not written by fetch); point its target and reports at the fetched folders"
                )
            )?;
        }
        writeln!(streams.out, "{}", display_path(cwd, &root))?;
    }
    Ok(Fetched::Written { root, exit })
}

/// The downloads, keyed for the layout writer.
pub(crate) struct Downloads {
    /// The model's definition.
    pub model: Definition,
    /// Each downloadable report's outcome, by report id.
    pub reports: HashMap<String, Result<Definition, ServiceError>>,
}

/// Fires every export, then polls them together.
fn download(
    service: &Service,
    found: &Downstream,
    streams: &mut Streams<'_>,
    quiet: bool,
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
    let live = streams.stderr_is_tty && !quiet;
    let err = &mut *streams.err;
    let mut results = service
        .fetch_definitions(&requests, FetchOptions::default(), &mut |done, total| {
            if live {
                let _ = write!(err, "\r  Downloading definitions {done}/{total}…");
                let _ = err.flush();
            }
        })
        .into_iter();
    if live {
        // Clear the counter line: the summary replaces it.
        write!(streams.err, "\r\x1b[2K")?;
    }
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

/// Resolves the model from a link, from `--workspace`/`--model`, or from
/// the interactive pickers.
fn resolve_model(
    args: &FetchArgs,
    service: &Service,
    interactive: bool,
    streams: &mut Streams<'_>,
    palette: &Palette,
) -> Result<ModelRef, ScanError> {
    if let Some(text) = &args.link {
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

    let workspace = match &args.workspace {
        Some(wanted) => Some(find_workspace(service, wanted)?),
        None if args.model_name.is_some() => None,
        None if interactive => {
            let mut workspaces = service.workspaces().map_err(service_error)?;
            workspaces.sort_by_key(|workspace| workspace.display_name.to_lowercase());
            let labels: Vec<String> = workspaces
                .iter()
                .map(|workspace| workspace.display_name.clone())
                .collect();
            let choice = pick(
                "Workspaces:",
                "Select a workspace",
                &labels,
                streams,
                palette,
            )?;
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

    match (workspace, &args.model_name) {
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
                &format!("Semantic models in {}:", workspace.display_name),
                "Select a model",
                &labels,
                streams,
                palette,
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
        (None, None) => unreachable!("a missing workspace without --model returned above"),
    }
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

/// A numbered list on stderr, a choice read from stdin.
fn pick(
    title: &str,
    prompt: &str,
    labels: &[String],
    streams: &mut Streams<'_>,
    palette: &Palette,
) -> Result<usize, ScanError> {
    if labels.is_empty() {
        return Err(ScanError::new(format!("nothing to pick from: {title}")));
    }
    writeln!(streams.err, "{}", palette.bold(title))?;
    for (index, label) in labels.iter().enumerate() {
        writeln!(streams.err, "  {}. {label}", index + 1)?;
    }
    loop {
        write!(streams.err, "{prompt} [1-{}]: ", labels.len())?;
        streams.err.flush()?;
        let Some(line) = read_line(streams.input)? else {
            return Err(ScanError::new("nothing selected"));
        };
        match line.trim().parse::<usize>() {
            Ok(choice) if (1..=labels.len()).contains(&choice) => return Ok(choice - 1),
            _ => writeln!(
                streams.err,
                "Enter a number between 1 and {}.",
                labels.len()
            )?,
        }
    }
}

/// A `[y/N]` question; anything but y/yes is no.
fn confirm(streams: &mut Streams<'_>, question: &str) -> Result<bool, ScanError> {
    write!(streams.err, "{question}")?;
    streams.err.flush()?;
    let line = read_line(streams.input)?.unwrap_or_default();
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// One line from `input`, without reading past it: a buffered reader would
/// swallow the answer to the next prompt. `None` at end of input.
fn read_line(input: &mut dyn io::Read) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match input.read(&mut byte)? {
            0 if bytes.is_empty() => return Ok(None),
            0 => break,
            _ if byte[0] == b'\n' => break,
            _ => bytes.push(byte[0]),
        }
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// `--scan`: the bare `ripbi scan` a user would type next, run from inside
/// the fetched folder so its `ripbi.toml` supplies the model and reports.
fn scan_fetched(args: &FetchArgs, root: &Path, streams: &mut Streams<'_>) -> i32 {
    let scan_args = ScanArgs {
        quiet: args.quiet,
        no_color: args.no_color,
        no_input: true,
        ..ScanArgs::default()
    };
    if !args.quiet {
        let _ = writeln!(streams.err);
    }
    scan::run_in(&scan_args, root, streams)
}

/// The root as the user should type it: relative to the cwd when inside it.
fn display_path(cwd: &Path, root: &Path) -> String {
    let shown = root.strip_prefix(cwd).map_or_else(
        |_| root.to_path_buf(),
        |relative| Path::new(".").join(relative),
    );
    shown.display().to_string()
}

/// Rewrites a service error for humans, with the conventional fix.
fn service_error(error: ServiceError) -> ScanError {
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

fn report_error(
    args: &FetchArgs,
    streams: &mut Streams<'_>,
    palette: &Palette,
    error: &ScanError,
) -> i32 {
    if !args.quiet {
        let _ = writeln!(streams.err, "{} {}", palette.alert("error:"), error.message);
        if let Some(hint) = &error.hint {
            let _ = writeln!(streams.err, "hint: {hint}");
        }
    }
    EXIT_ERROR
}
