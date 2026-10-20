//! `ripbi fetch` end to end over the scripted tenant from
//! `ripbi_service::fake`: resolution, the forecast and its gate, the
//! download, the project layout, and a scan of the result. The "tenant"
//! serves the mini PBIP fixture, with the report's `definition.pbir` bound
//! `byConnection` — the shape a real export has.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::{TempDir, mini_pbip, run_scan};
use ripbi_cli::cli::{AuthMode, FetchArgs, ScanArgs};
use ripbi_cli::fetch::{self, Manifest, ReportStatus};
use ripbi_cli::scan::Streams;
use ripbi_service::fake::{FABRIC, FakeClock, FakeTransport, definition_json, service};

const DATA: &str = "00000000-0000-0000-0000-0000000000a0";
const MARKETING: &str = "00000000-0000-0000-0000-0000000000a1";
const OPERATIONS: &str = "00000000-0000-0000-0000-0000000000a2";
const MODEL: &str = "00000000-0000-0000-0000-000000000001";
const REPORT: &str = "00000000-0000-0000-0000-000000000011";
const REPORT_APP: &str = "00000000-0000-0000-0000-000000000012";
const ROTA: &str = "00000000-0000-0000-0000-000000000021";
const DASHBOARD: &str = "00000000-0000-0000-0000-000000000041";
const REGIONAL: &str = "https://wabi-test-redirect.analysis.windows.net/v1/operations";

fn item(id: &str, kind: &str, name: &str, workspace: &str) -> String {
    format!(r#"{{"id":"{id}","type":"{kind}","displayName":"{name}","workspaceId":"{workspace}"}}"#)
}

fn workspace(id: &str, name: &str) -> String {
    format!(r#"{{"id":"{id}","displayName":"{name}","type":"Workspace"}}"#)
}

fn list(items: &[String]) -> String {
    format!(r#"{{"value":[{}]}}"#, items.join(","))
}

/// Every file under `dir` as a definition part, `/`-separated.
fn parts_of(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, parts: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(base, &path, parts);
            } else {
                let relative = path
                    .strip_prefix(base)
                    .expect("inside")
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                parts.push((relative, fs::read(&path).expect("read file")));
            }
        }
    }
    let mut parts = Vec::new();
    walk(dir, dir, &mut parts);
    parts.sort();
    parts
}

fn model_parts() -> Vec<(String, Vec<u8>)> {
    let mut parts = parts_of(&mini_pbip().join("Mini.SemanticModel"));
    parts.push((
        "definition.pbism".to_string(),
        br#"{"version":"4.2","settings":{}}"#.to_vec(),
    ));
    parts
}

fn report_parts() -> Vec<(String, Vec<u8>)> {
    let pbir = format!(
        r#"{{"version":"4.0","datasetReference":{{"byConnection":{{"connectionString":"Data Source=\"powerbi://api.powerbi.com/v1.0/myorg/Data\";initial catalog=Mini;integrated security=ClaimsToken;semanticmodelid={MODEL}"}}}}}}"#
    );
    let mut parts: Vec<(String, Vec<u8>)> = parts_of(&mini_pbip().join("Mini.Report"))
        .into_iter()
        .filter(|(path, _)| path != "definition.pbir")
        .collect();
    parts.push(("definition.pbir".to_string(), pbir.into_bytes()));
    parts.push((
        "StaticResources/RegisteredResources/logo.png".to_string(),
        vec![0x89, b'P', b'N', b'G'],
    ));
    parts
}

/// The tenant: Contributor in Data and Marketing; with `viewer_report`, a
/// second report lives in Operations, where the caller is only a Viewer.
fn tenant(viewer_report: bool) -> Rc<FakeTransport> {
    let transport = FakeTransport::new();
    let mut items = vec![
        item(REPORT, "Report", "Mini", MARKETING),
        item(REPORT_APP, "Report", "Mini", MARKETING),
        item(DASHBOARD, "Dashboard", "Overview", MARKETING),
        item(MODEL, "Model", "Mini", DATA),
    ];
    let mut edges = vec![(REPORT, MODEL), (REPORT_APP, MODEL), (DASHBOARD, REPORT)];
    if viewer_report {
        items.push(item(ROTA, "Report", "Rota", OPERATIONS));
        edges.push((ROTA, MODEL));
    }
    let edges: Vec<String> = edges
        .iter()
        .map(|(from, to)| {
            format!(
                r#"{{"itemId":"{from}","dependentOnItemId":"{to}","relationType":"Association"}}"#
            )
        })
        .collect();
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{DATA}/items/{MODEL}/relations/downstream?beta=true"),
        200,
        &format!(
            r#"{{"items":[{}],"relations":[{}],"workspaces":[{},{},{}]}}"#,
            items.join(","),
            edges.join(","),
            workspace(DATA, "Data"),
            workspace(MARKETING, "Marketing"),
            workspace(OPERATIONS, "Operations"),
        ),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces"),
        200,
        &list(&[
            workspace(DATA, "Data"),
            workspace(MARKETING, "Marketing"),
            workspace(OPERATIONS, "Operations"),
        ]),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces?roles=Admin,Member,Contributor"),
        200,
        &list(&[workspace(DATA, "Data"), workspace(MARKETING, "Marketing")]),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{DATA}/semanticModels"),
        200,
        &list(&[item(MODEL, "SemanticModel", "Mini", DATA)]),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{MARKETING}/semanticModels"),
        200,
        &list(&[]),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{OPERATIONS}/semanticModels"),
        200,
        &list(&[]),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{MARKETING}/reports"),
        200,
        &list(&[item(REPORT, "Report", "Mini", MARKETING)]),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{OPERATIONS}/reports"),
        200,
        &list(&[item(ROTA, "Report", "Rota", OPERATIONS)]),
    );
    // The model: the long-running path.
    transport.reply_with(
        "POST",
        &format!("{FABRIC}/workspaces/{DATA}/semanticModels/{MODEL}/getDefinition?format=TMDL"),
        202,
        &[
            ("Location", &format!("{REGIONAL}/model-op")),
            ("Retry-After", "20"),
        ],
        "",
    );
    transport.reply(
        "GET",
        &format!("{REGIONAL}/model-op"),
        200,
        r#"{"status":"Succeeded"}"#,
    );
    transport.reply(
        "GET",
        &format!("{REGIONAL}/model-op/result"),
        200,
        &definition_json("TMDL", &model_parts()),
    );
    // The report: answered at once.
    transport.reply(
        "POST",
        &format!("{FABRIC}/workspaces/{MARKETING}/reports/{REPORT}/getDefinition"),
        200,
        &definition_json("PBIR", &report_parts()),
    );
    transport
}

fn args() -> FetchArgs {
    FetchArgs {
        workspace: Some("data".to_string()),
        model_name: Some("Mini".to_string()),
        no_color: true,
        ..FetchArgs::default()
    }
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

fn run(args: &FetchArgs, transport: &Rc<FakeTransport>, cwd: &Path, stdin: &str, tty: bool) -> Run {
    let clock = Rc::new(FakeClock::default());
    let service = service(transport, &clock);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut input = stdin.as_bytes();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        input: &mut input,
        stdin_is_tty: tty,
        stdout_is_tty: false,
        stderr_is_tty: false,
    };
    let code = fetch::run_with(args, &service, cwd, &mut streams);
    Run {
        code,
        out: String::from_utf8(out).expect("utf8 stdout"),
        err: String::from_utf8(err).expect("utf8 stderr"),
    }
}

fn posts(transport: &FakeTransport) -> usize {
    transport
        .requests()
        .iter()
        .filter(|request| request.starts_with("POST "))
        .count()
}

#[test]
fn fetches_the_model_and_its_reports_into_a_scannable_project() {
    let dir = TempDir::new("fetch-complete");
    let transport = tenant(false);
    let run = run(&args(), &transport, &dir.0, "", false);
    assert_eq!(run.code, 0, "{}", run.err);

    let root = dir.0.join("Mini");
    assert_eq!(PathBuf::from(run.out.trim()), Path::new(".").join("Mini"));
    assert!(
        root.join("Mini.SemanticModel/definition/tables/Sales.tmdl")
            .is_file()
    );
    assert!(root.join("Mini.SemanticModel/definition.pbism").is_file());
    let report = root.join("reports/Marketing/Mini.Report");
    assert!(report.join("definition/report.json").is_file());
    assert!(
        !report.join("StaticResources").exists(),
        "static resources are skipped"
    );
    let config = fs::read_to_string(root.join("ripbi.toml")).expect("config");
    assert!(config.starts_with("# Written by `ripbi fetch`"));
    assert!(config.contains("target = \"Mini.SemanticModel\""));
    assert!(config.contains("\"reports/Marketing/Mini.Report\","));

    let manifest = Manifest::load(&root).expect("manifest");
    assert_eq!(manifest.schema_version, 1);
    assert!(manifest.complete);
    assert_eq!(manifest.model.id, MODEL);
    assert_eq!(manifest.model.path, "Mini.SemanticModel");
    assert_eq!(manifest.reports.len(), 1);
    assert_eq!(manifest.reports[0].status, ReportStatus::Downloaded);
    assert_eq!(
        manifest.reports[0].path.as_deref(),
        Some("reports/Marketing/Mini.Report")
    );
    assert_eq!(manifest.app_copies.len(), 1);
    assert_eq!(manifest.app_copies[0].id, REPORT_APP);
    assert_eq!(manifest.other_items.len(), 1);

    assert!(
        run.err
            .contains("Mini · Data · 1 downstream report (+1 app copy skipped)"),
        "{}",
        run.err
    );
    assert!(run.err.contains("✓ 1 downloadable"), "{}", run.err);
    assert!(
        run.err
            .contains("1 other downstream item not scanned: 1 Dashboard"),
        "{}",
        run.err
    );
    assert!(
        run.err.contains("Fetched Mini and 1 report into"),
        "{}",
        run.err
    );
    // The app copy is never requested: it would only 404.
    assert!(
        !transport
            .requests()
            .iter()
            .any(|r| r.contains(REPORT_APP) && r.starts_with("POST"))
    );
}

#[test]
fn fetch_then_scan_runs_on_the_folder_with_no_flags() {
    let dir = TempDir::new("fetch-scan");
    let transport = tenant(false);
    let args = FetchArgs {
        scan: true,
        ..args()
    };
    let run = run(&args, &transport, &dir.0, "", false);
    // The mini fixture keeps one dead chain: the scan finds it (exit 1).
    assert_eq!(run.code, 1, "{}", run.err);
    assert!(run.out.contains("Legacy Total"), "{}", run.out);
    assert!(!run.err.contains("error:"), "{}", run.err);
}

#[test]
fn an_incomplete_set_is_refused_without_allow_incomplete() {
    let dir = TempDir::new("fetch-refuse");
    let transport = tenant(true);
    let run = run(&args(), &transport, &dir.0, "", false);
    assert_eq!(run.code, 2);
    assert!(
        run.err
            .contains("✗ 1 not readable   Operations (1, viewer)"),
        "{}",
        run.err
    );
    assert!(
        run.err
            .contains("Scan will be incomplete: 1 report can't be read.")
    );
    assert!(
        run.err.contains("1 of 2 downstream reports cannot be read"),
        "{}",
        run.err
    );
    assert!(run.err.contains("--allow-incomplete"));
    assert_eq!(posts(&transport), 0, "nothing is downloaded");
    assert!(!dir.0.join("Mini").exists());
}

#[test]
fn allow_incomplete_fetches_the_readable_reports_and_records_the_rest() {
    let dir = TempDir::new("fetch-allow");
    let transport = tenant(true);
    let args = FetchArgs {
        allow_incomplete: true,
        ..args()
    };
    let run = run(&args, &transport, &dir.0, "", false);
    assert_eq!(run.code, 0, "{}", run.err);
    let manifest = Manifest::load(&dir.0.join("Mini")).expect("manifest");
    assert!(!manifest.complete);
    let statuses: Vec<(&str, ReportStatus)> = manifest
        .reports
        .iter()
        .map(|r| (r.name.as_str(), r.status))
        .collect();
    assert_eq!(
        statuses,
        [
            ("Mini", ReportStatus::Downloaded),
            ("Rota", ReportStatus::Viewer)
        ]
    );
    assert!(run.err.contains("1 report is missing"), "{}", run.err);
}

#[test]
fn scanning_an_incomplete_fetch_says_so_and_strict_fails_on_it() {
    let dir = TempDir::new("fetch-scan-incomplete");
    let args = FetchArgs {
        allow_incomplete: true,
        ..args()
    };
    assert_eq!(run(&args, &tenant(true), &dir.0, "", false).code, 0);
    let root = dir.0.join("Mini");

    let (code, _, err) = run_scan(&ScanArgs::default(), &root, "");
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains(
            "[incomplete_report_set] 1 of 2 downstream reports could not be fetched, so objects \
             only they use show as unused: 'Rota' (Operations)"
        ),
        "{err}"
    );
    let strict = ScanArgs {
        strict: true,
        ..ScanArgs::default()
    };
    assert_eq!(run_scan(&strict, &root, "").0, 2);

    // A complete fetch carries no notice.
    let complete = TempDir::new("fetch-scan-complete");
    assert_eq!(
        run(&self::args(), &tenant(false), &complete.0, "", false).code,
        0
    );
    let (code, _, err) = run_scan(&strict, &complete.0.join("Mini"), "");
    assert_eq!(code, 1, "{err}");
    assert!(!err.contains("incomplete_report_set"), "{err}");
}

#[test]
fn dry_run_forecasts_and_downloads_nothing() {
    let dir = TempDir::new("fetch-dry");
    let transport = tenant(true);
    let args = FetchArgs {
        dry_run: true,
        ..args()
    };
    let run = run(&args, &transport, &dir.0, "", false);
    assert_eq!(run.code, 0, "{}", run.err);
    assert!(run.err.contains("Scan will be incomplete"));
    assert!(run.out.is_empty());
    assert_eq!(posts(&transport), 0);
    assert!(!dir.0.join("Mini").exists());
}

#[test]
fn an_interactive_run_asks_before_fetching_an_incomplete_set() {
    let dir = TempDir::new("fetch-confirm");
    let declined = run(&args(), &tenant(true), &dir.0, "n\n", true);
    assert_eq!(declined.code, 2);
    assert!(
        declined
            .err
            .contains("Continue with the readable reports? [y/N]")
    );
    assert!(declined.err.contains("cancelled; nothing was downloaded"));

    let accepted = run(&args(), &tenant(true), &dir.0, "y\n", true);
    assert_eq!(accepted.code, 0, "{}", accepted.err);
    assert!(dir.0.join("Mini/reports/Marketing/Mini.Report").is_dir());
}

#[test]
fn the_pickers_choose_workspace_then_model() {
    let dir = TempDir::new("fetch-pick");
    let args = FetchArgs {
        dry_run: true,
        no_color: true,
        ..FetchArgs::default()
    };
    // Workspaces sort by name: Data, Marketing, Operations.
    let run = run(&args, &tenant(false), &dir.0, "1\n1\n", true);
    assert_eq!(run.code, 0, "{}", run.err);
    assert!(
        run.err
            .contains("Workspaces:\n  1. Data\n  2. Marketing\n  3. Operations"),
        "{}",
        run.err
    );
    assert!(
        run.err.contains("Semantic models in Data:\n  1. Mini"),
        "{}",
        run.err
    );
}

#[test]
fn without_a_model_or_a_tty_fetch_says_what_to_pass() {
    let dir = TempDir::new("fetch-nothing");
    let args = FetchArgs {
        no_color: true,
        ..FetchArgs::default()
    };
    let run = run(&args, &tenant(false), &dir.0, "", false);
    assert_eq!(run.code, 2);
    assert!(run.err.contains("error: no semantic model given"));
    assert!(
        run.err
            .contains("hint: pass a link, or --workspace and --model")
    );
}

#[test]
fn a_model_name_alone_searches_every_workspace() {
    let dir = TempDir::new("fetch-search");
    let args = FetchArgs {
        workspace: None,
        dry_run: true,
        ..args()
    };
    let run = run(&args, &tenant(false), &dir.0, "", false);
    assert_eq!(run.code, 0, "{}", run.err);
    assert!(run.err.contains("Mini · Data"));
}

#[test]
fn links_to_a_model_or_a_report_resolve() {
    let dir = TempDir::new("fetch-link");
    let transport = tenant(false);
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{DATA}/semanticModels/{MODEL}"),
        200,
        &item(MODEL, "SemanticModel", "Mini", DATA),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{DATA}"),
        200,
        &workspace(DATA, "Data"),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{MARKETING}/items/{REPORT}/relations/upstream?beta=true"),
        200,
        &format!(
            r#"{{"items":[{},{}],"relations":[{{"itemId":"{REPORT}","dependentOnItemId":"{MODEL}","relationType":"Association"}}],"workspaces":[{}]}}"#,
            item(REPORT, "Report", "Mini", MARKETING),
            item(MODEL, "Model", "Mini", DATA),
            workspace(DATA, "Data"),
        ),
    );
    for link in [
        format!(
            "https://app.powerbi.com/groups/{DATA}/datasets/{MODEL}/details?experience=power-bi"
        ),
        format!("https://app.powerbi.com/groups/{MARKETING}/reports/{REPORT}/ReportSection"),
    ] {
        let args = FetchArgs {
            link: Some(link.clone()),
            dry_run: true,
            no_color: true,
            ..FetchArgs::default()
        };
        let run = run(&args, &transport, &dir.0, "", false);
        assert_eq!(run.code, 0, "{link}: {}", run.err);
        assert!(
            run.err.contains("Mini · Data · 1 downstream report"),
            "{link}: {}",
            run.err
        );
    }
    let args = FetchArgs {
        link: Some("https://example.com/not-a-model".to_string()),
        ..FetchArgs::default()
    };
    let run = run(&args, &transport, &dir.0, "", false);
    assert_eq!(run.code, 2);
    assert!(
        run.err
            .contains("is not a Power BI link to a semantic model or report")
    );
}

#[test]
fn a_refetch_replaces_its_own_folders_but_a_foreign_directory_is_refused() {
    let dir = TempDir::new("fetch-refetch");
    let first = run(&args(), &tenant(false), &dir.0, "", false);
    assert_eq!(first.code, 0, "{}", first.err);
    // A stale report from an earlier fetch disappears; the user's own file stays.
    dir.write("Mini/reports/Gone/Old.Report/definition.pbir", "{}");
    dir.write("Mini/notes.md", "mine");
    let second = run(&args(), &tenant(false), &dir.0, "", false);
    assert_eq!(second.code, 0, "{}", second.err);
    assert!(!dir.0.join("Mini/reports/Gone").exists());
    assert!(dir.0.join("Mini/notes.md").is_file());

    dir.write("Other/readme.txt", "not a fetch");
    let args = FetchArgs {
        out: Some(PathBuf::from("Other")),
        ..args()
    };
    let refused = run(&args, &tenant(false), &dir.0, "", false);
    assert_eq!(refused.code, 2);
    assert!(
        refused
            .err
            .contains("is not empty and was not written by ripbi fetch")
    );
    let forced = run(
        &FetchArgs {
            force: true,
            ..args
        },
        &tenant(false),
        &dir.0,
        "",
        false,
    );
    assert_eq!(forced.code, 0, "{}", forced.err);
    assert!(dir.0.join("Other/readme.txt").is_file());
    assert!(dir.0.join("Other/Mini.SemanticModel").is_dir());
}

#[test]
fn a_report_the_forecast_called_readable_can_still_404() {
    let dir = TempDir::new("fetch-404");
    let transport = tenant(false);
    // Item-level permissions: the workspace role says yes, the report says no.
    transport.replace(
        "POST",
        &format!("{FABRIC}/workspaces/{MARKETING}/reports/{REPORT}/getDefinition"),
        404,
        r#"{"errorCode":"ItemNotFound"}"#,
    );
    let run = run(&args(), &transport, &dir.0, "", false);
    assert_eq!(run.code, 2, "{}", run.err);
    let manifest = Manifest::load(&dir.0.join("Mini")).expect("written anyway");
    assert_eq!(manifest.reports[0].status, ReportStatus::NoAccess);
    assert_eq!(manifest.reports[0].http_status, Some(404));
    assert!(
        run.err
            .contains("more reports failed than the forecast predicted")
    );
}

#[test]
fn a_viewer_of_the_model_workspace_cannot_fetch_it() {
    let dir = TempDir::new("fetch-model-viewer");
    let transport = tenant(false);
    transport.replace(
        "GET",
        &format!("{FABRIC}/workspaces?roles=Admin,Member,Contributor"),
        200,
        &list(&[workspace(MARKETING, "Marketing")]),
    );
    let run = run(&args(), &transport, &dir.0, "", false);
    assert_eq!(run.code, 2);
    assert!(
        run.err
            .contains("cannot download the model 'Mini': you are a Viewer in workspace 'Data'"),
        "{}",
        run.err
    );
}

#[test]
fn the_token_source_follows_flag_then_environment() {
    let env = |vars: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    };
    const SP: &[(&str, &str)] = &[
        ("AZURE_TENANT_ID", "t"),
        ("AZURE_CLIENT_ID", "client-1"),
        ("AZURE_CLIENT_SECRET", "s"),
    ];
    let describe = |mode, vars: &'static [(&'static str, &'static str)]| {
        fetch::token_source(mode, &env(vars)).map(|tokens| tokens.describe())
    };
    assert_eq!(
        describe(AuthMode::Auto, &[]).ok().as_deref(),
        Some("Azure CLI")
    );
    assert_eq!(
        describe(AuthMode::Auto, SP).ok().as_deref(),
        Some("service principal client-1")
    );
    assert_eq!(
        describe(
            AuthMode::Auto,
            &[("RIPBI_FABRIC_TOKEN", "x"), ("AZURE_CLIENT_SECRET", "s")]
        )
        .ok()
        .as_deref(),
        Some("RIPBI_FABRIC_TOKEN")
    );
    assert_eq!(
        describe(AuthMode::Az, SP).ok().as_deref(),
        Some("Azure CLI")
    );
    assert!(describe(AuthMode::Env, SP).is_err());
    assert!(describe(AuthMode::Sp, &[("AZURE_CLIENT_ID", "c")]).is_err());
}
