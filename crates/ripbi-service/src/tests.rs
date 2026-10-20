//! End-to-end tests of [`crate::Service`] and [`crate::downstream`] over the
//! scripted transport. The fixtures mirror the shapes a real tenant returned
//! (see `FETCH_PLAN.md`), de-identified: `Model` as the model type,
//! `Association` edges, an org-app copy beside every workspace report, a
//! `202` + regional `Location` for every definition, `404` for anything the
//! caller may not read.

use std::rc::Rc;
use std::time::Duration;

use crate::Error;
use crate::downstream::{Access, ModelRef, discover, model_of_report, verify};
use crate::fake::{FABRIC, FakeClock, FakeTransport, POWER_BI, service};
use crate::service::{DefinitionKind, DefinitionRequest, FetchOptions};

const MODEL_WS: &str = "00000000-0000-0000-0000-0000000000a0";
const MARKETING: &str = "00000000-0000-0000-0000-0000000000a1";
const OPERATIONS: &str = "00000000-0000-0000-0000-0000000000a2";
const SECRET_WS: &str = "00000000-0000-0000-0000-0000000000a3";
const MODEL: &str = "00000000-0000-0000-0000-000000000001";
const PRICES: &str = "00000000-0000-0000-0000-000000000011";
const PRICES_APP: &str = "00000000-0000-0000-0000-000000000012";
const ROTA: &str = "00000000-0000-0000-0000-000000000021";
const HIDDEN: &str = "00000000-0000-0000-0000-000000000031";
const HIDDEN_APP: &str = "00000000-0000-0000-0000-000000000032";
const DASHBOARD: &str = "00000000-0000-0000-0000-000000000041";
const REGIONAL: &str = "https://wabi-test-redirect.analysis.windows.net/v1/operations";

fn model() -> ModelRef {
    ModelRef {
        id: MODEL.to_string(),
        name: "Sales".to_string(),
        workspace_id: MODEL_WS.to_string(),
        workspace_name: "Data".to_string(),
    }
}

fn item(id: &str, kind: &str, name: &str, workspace: &str) -> String {
    format!(r#"{{"id":"{id}","type":"{kind}","displayName":"{name}","workspaceId":"{workspace}"}}"#)
}

fn edge(from: &str, to: &str) -> String {
    format!(r#"{{"itemId":"{from}","dependentOnItemId":"{to}","relationType":"Association"}}"#)
}

fn workspace(id: &str, name: &str) -> String {
    format!(r#"{{"id":"{id}","displayName":"{name}","type":"Workspace"}}"#)
}

fn list(items: &[String]) -> String {
    format!(r#"{{"value":[{}]}}"#, items.join(","))
}

/// The tenant: the caller is Contributor in Data and Marketing, Viewer in
/// Operations, and not a member of the fourth workspace, whose app it can see.
fn script_tenant(transport: &FakeTransport) {
    script_tenant_with(transport, true);
}

/// [`script_tenant`], with or without access to the org-app listing.
fn script_tenant_with(transport: &FakeTransport, apps_visible: bool) {
    let items = [
        item(PRICES, "Report", "Prices", MARKETING),
        item(PRICES_APP, "Report", "Prices", MARKETING),
        item(ROTA, "Report", "Rota", OPERATIONS),
        item(HIDDEN, "Report", "Hidden", SECRET_WS),
        item(HIDDEN_APP, "Report", "Hidden", SECRET_WS),
        item(DASHBOARD, "Dashboard", "Overview", MARKETING),
        item(MODEL, "Model", "Sales", MODEL_WS),
    ];
    let edges = [
        edge(PRICES, MODEL),
        edge(PRICES_APP, MODEL),
        edge(ROTA, MODEL),
        edge(HIDDEN, MODEL),
        edge(HIDDEN_APP, MODEL),
        edge(DASHBOARD, PRICES),
    ];
    let workspaces = [
        workspace(MODEL_WS, "Data"),
        workspace(MARKETING, "Marketing"),
        workspace(OPERATIONS, "Operations"),
        workspace(SECRET_WS, "Board"),
    ];
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{MODEL_WS}/items/{MODEL}/relations/downstream?beta=true"),
        200,
        &format!(
            r#"{{"items":[{}],"relations":[{}],"workspaces":[{}]}}"#,
            items.join(","),
            edges.join(","),
            workspaces.join(",")
        ),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces"),
        200,
        &format!(
            r#"{{"value":[{},{}],"continuationUri":"{FABRIC}/workspaces?continuationToken=p2"}}"#,
            workspace(MODEL_WS, "Data"),
            workspace(MARKETING, "Marketing")
        ),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces?continuationToken=p2"),
        200,
        &list(&[workspace(OPERATIONS, "Operations")]),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces?roles=Admin,Member,Contributor"),
        200,
        &list(&[
            workspace(MODEL_WS, "Data"),
            workspace(MARKETING, "Marketing"),
        ]),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{MARKETING}/reports"),
        200,
        &list(&[item(PRICES, "Report", "Prices", MARKETING)]),
    );
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{OPERATIONS}/reports"),
        200,
        &list(&[item(ROTA, "Report", "Rota", OPERATIONS)]),
    );
    if !apps_visible {
        transport.reply(
            "GET",
            &format!("{POWER_BI}/apps"),
            403,
            r#"{"error":{"code":"Forbidden"}}"#,
        );
        return;
    }
    transport.reply(
        "GET",
        &format!("{POWER_BI}/apps"),
        200,
        r#"{"value":[{"id":"app-1","name":"Board pack"}]}"#,
    );
    transport.reply(
        "GET",
        &format!("{POWER_BI}/apps/app-1/reports"),
        200,
        &format!(
            r#"{{"value":[{{"id":"{HIDDEN_APP}","name":"Hidden","originalReportObjectId":"{HIDDEN}"}}]}}"#
        ),
    );
}

#[test]
fn discover_drops_app_copies_and_forecasts_every_report() {
    let transport = FakeTransport::new();
    let clock = Rc::new(FakeClock::default());
    script_tenant(&transport);
    let found = discover(&service(&transport, &clock), &model()).expect("discovers");

    assert_eq!(found.model_access, Access::Downloadable);
    let reports: Vec<(&str, &str, Access)> = found
        .reports
        .iter()
        .map(|r| (r.workspace_name.as_str(), r.name.as_str(), r.access))
        .collect();
    assert_eq!(
        reports,
        [
            ("Board", "Hidden", Access::NoAccess),
            ("Marketing", "Prices", Access::Downloadable),
            ("Operations", "Rota", Access::Viewer),
        ]
    );
    let copies: Vec<&str> = found.app_copies.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(copies, [PRICES_APP, HIDDEN_APP]);
    // The dashboard depends on a report, not the model: never a scan input.
    assert_eq!(found.other_items.len(), 1);
    assert_eq!(found.other_items[0].kind, "Dashboard");
    assert_eq!(found.unreadable().count(), 2);
    assert!(found.notes.is_empty(), "{:?}", found.notes);
}

#[test]
fn unlisted_workspaces_without_app_access_say_copies_may_hide() {
    let transport = FakeTransport::new();
    let clock = Rc::new(FakeClock::default());
    script_tenant_with(&transport, false);
    let found = discover(&service(&transport, &clock), &model()).expect("discovers");
    assert_eq!(
        found
            .reports
            .iter()
            .filter(|r| r.workspace_name == "Board")
            .count(),
        2
    );
    assert!(
        found
            .notes
            .iter()
            .any(|note| note.contains("could not list org apps"))
    );
    assert!(found.notes.iter().any(|note| note.contains("appear twice")));
}

#[test]
fn verify_reports_both_directions() {
    let transport = FakeTransport::new();
    let clock = Rc::new(FakeClock::default());
    script_tenant(&transport);
    let service = service(&transport, &clock);
    let found = discover(&service, &model()).expect("discovers");
    transport.reply(
        "GET",
        &format!("{POWER_BI}/groups/{MODEL_WS}/reports"),
        200,
        &format!(
            r#"{{"value":[{{"id":"00000000-0000-0000-0000-000000000099","name":"Missed","datasetId":"{MODEL}"}}]}}"#
        ),
    );
    transport.reply(
        "GET",
        &format!("{POWER_BI}/groups/{MARKETING}/reports"),
        200,
        &format!(
            r#"{{"value":[{{"id":"{PRICES}","name":"Prices","datasetId":"{MODEL}"}},{{"id":"x","name":"Other","datasetId":"y"}}]}}"#
        ),
    );
    // Operations' listing fails: counted, not fatal.
    let verification = verify(&service, &found).expect("verifies");
    assert_eq!(verification.workspaces_checked, 2);
    assert_eq!(verification.failures.len(), 1);
    assert_eq!(verification.only_in_listing.len(), 1);
    assert_eq!(verification.only_in_listing[0].name, "Missed");
    // Rota is in Operations, whose listing failed: it is reported as
    // downstream-only, since nothing listed it.
    assert_eq!(
        verification
            .only_in_downstream
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        ["Rota"]
    );
}

#[test]
fn a_report_link_resolves_to_its_direct_model() {
    let transport = FakeTransport::new();
    let clock = Rc::new(FakeClock::default());
    transport.reply(
        "GET",
        &format!("{FABRIC}/workspaces/{MARKETING}/items/{PRICES}/relations/upstream?beta=true"),
        200,
        &format!(
            r#"{{"items":[{},{},{}],"relations":[{},{}],"workspaces":[{}]}}"#,
            item(PRICES, "Report", "Prices", MARKETING),
            item(MODEL, "Model", "Sales", MODEL_WS),
            item("wh", "Warehouse", "Lake", MODEL_WS),
            edge(PRICES, MODEL),
            edge(MODEL, "wh"),
            workspace(MODEL_WS, "Data"),
        ),
    );
    let model = model_of_report(&service(&transport, &clock), MARKETING, PRICES).expect("found");
    assert_eq!(model, self::model());
}

fn definition_body(path: &str) -> String {
    format!(
        r#"{{"definition":{{"format":"PBIR","parts":[{{"path":"{path}","payload":"e30=","payloadType":"InlineBase64"}}]}}}}"#
    )
}

#[test]
fn definitions_are_fired_first_then_polled_together() {
    let transport = FakeTransport::new();
    let clock = Rc::new(FakeClock::default());
    for (id, op) in [(PRICES, "op-1"), (ROTA, "op-2")] {
        transport.reply_with(
            "POST",
            &format!("{FABRIC}/workspaces/{MARKETING}/reports/{id}/getDefinition"),
            202,
            &[
                ("Location", &format!("{REGIONAL}/{op}")),
                ("Retry-After", "20"),
            ],
            "",
        );
        transport.reply(
            "GET",
            &format!("{REGIONAL}/{op}"),
            200,
            r#"{"status":"Running"}"#,
        );
        transport.reply_with(
            "GET",
            &format!("{REGIONAL}/{op}"),
            200,
            &[("Retry-After", "2")],
            r#"{"status":"Succeeded"}"#,
        );
        transport.reply(
            "GET",
            &format!("{REGIONAL}/{op}/result"),
            200,
            &definition_body(&format!("{id}/definition.pbir")),
        );
    }
    transport.reply(
        "POST",
        &format!("{FABRIC}/workspaces/{OPERATIONS}/reports/{HIDDEN}/getDefinition"),
        404,
        r#"{"errorCode":"ItemNotFound"}"#,
    );
    let requests: Vec<DefinitionRequest> =
        [(MARKETING, PRICES), (OPERATIONS, HIDDEN), (MARKETING, ROTA)]
            .into_iter()
            .map(|(workspace_id, item_id)| DefinitionRequest {
                kind: DefinitionKind::Report,
                workspace_id: workspace_id.to_string(),
                item_id: item_id.to_string(),
                format: None,
            })
            .collect();
    let mut ticks = Vec::new();
    let results = service(&transport, &clock).fetch_definitions(
        &requests,
        FetchOptions::default(),
        &mut |done, total| ticks.push((done, total)),
    );

    assert_eq!(results.len(), 3);
    assert_eq!(
        results[0].as_ref().expect("first downloads").parts[0].path,
        format!("{PRICES}/definition.pbir")
    );
    assert!(results[1].as_ref().is_err_and(Error::is_not_found));
    assert_eq!(
        results[2].as_ref().expect("third downloads").parts[0].path,
        format!("{ROTA}/definition.pbir")
    );
    assert_eq!(ticks, [(1, 3), (2, 3), (3, 3)]);
    // All three POSTs go out before the first poll.
    let log = transport.requests();
    let first_poll = log
        .iter()
        .position(|r| r.starts_with("GET "))
        .expect("polled");
    assert_eq!(
        log[..first_poll]
            .iter()
            .filter(|r| r.starts_with("POST "))
            .count(),
        3
    );
    // The waits overlap: one 20 s wait and one 5 s re-poll (the Running
    // answer names no Retry-After), not 2 × 25 s.
    assert_eq!(clock.slept(), Duration::from_secs(25));
}

#[test]
fn throttled_calls_wait_and_retry() {
    let transport = FakeTransport::new();
    let clock = Rc::new(FakeClock::default());
    let url = format!("{FABRIC}/workspaces/{MODEL_WS}/semanticModels");
    transport.reply_with("GET", &url, 429, &[("Retry-After", "7")], "{}");
    transport.reply(
        "GET",
        &url,
        200,
        &list(&[item(MODEL, "SemanticModel", "Sales", MODEL_WS)]),
    );
    let models = service(&transport, &clock)
        .semantic_models(MODEL_WS)
        .expect("retried");
    assert_eq!(models.len(), 1);
    assert_eq!(clock.slept(), Duration::from_secs(7));
}

#[test]
fn tokens_never_travel_to_an_untrusted_location() {
    let transport = FakeTransport::new();
    let clock = Rc::new(FakeClock::default());
    transport.reply_with(
        "POST",
        &format!("{FABRIC}/workspaces/{MODEL_WS}/semanticModels/{MODEL}/getDefinition?format=TMDL"),
        202,
        &[("Location", "https://attacker.example/op")],
        "",
    );
    let results = service(&transport, &clock).fetch_definitions(
        &[DefinitionRequest {
            kind: DefinitionKind::SemanticModel,
            workspace_id: MODEL_WS.to_string(),
            item_id: MODEL.to_string(),
            format: Some("TMDL".to_string()),
        }],
        FetchOptions::default(),
        &mut |_, _| {},
    );
    assert!(
        matches!(results[0], Err(Error::UntrustedHost(ref host)) if host == "attacker.example")
    );
    assert!(!transport.requests().iter().any(|r| r.contains("attacker")));
}

#[test]
fn failed_operations_carry_the_service_message() {
    let transport = FakeTransport::new();
    let clock = Rc::new(FakeClock::default());
    transport.reply_with(
        "POST",
        &format!("{FABRIC}/workspaces/{MARKETING}/reports/{PRICES}/getDefinition"),
        202,
        &[("Location", &format!("{REGIONAL}/op-9"))],
        "",
    );
    transport.reply(
        "GET",
        &format!("{REGIONAL}/op-9"),
        200,
        r#"{"status":"Failed","error":{"errorCode":"OperationFailed","message":"Unsupported report"}}"#,
    );
    let results = service(&transport, &clock).fetch_definitions(
        &[DefinitionRequest {
            kind: DefinitionKind::Report,
            workspace_id: MARKETING.to_string(),
            item_id: PRICES.to_string(),
            format: None,
        }],
        FetchOptions::default(),
        &mut |_, _| {},
    );
    let message = results[0].as_ref().expect_err("failed").to_string();
    assert!(
        message.contains("OperationFailed: Unsupported report"),
        "{message}"
    );
}
