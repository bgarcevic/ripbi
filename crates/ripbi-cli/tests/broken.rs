//! Integration tests for `ripbi scan` against the broken-visual PBIP fixture
//! (issue #60): field bindings that no longer resolve in the model surface as
//! `broken_visual` findings — advisory by default (they never change the exit
//! code), gating under `--broken` exactly the way the type flags gate their
//! kinds. The fixture carries one healthy card (V1 → `Total`), one card on a
//! dropped column (V2 → `Sales.Color`), one card on a measure whose own DAX
//! is broken (V3 → `Broken Total`), and one KPI-style card (V4 → `Total
//! Goal`), which must resolve rather than flag.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::path::PathBuf;

use ripbi_cli::cli::ScanArgs;

use common::{
    TempDir, auto_datetime_pbip, broken_visual_pbip, field_parameters_pbip, json_payload, run_scan,
    scan_path,
};

fn fixture_args(path: impl Into<PathBuf>) -> ScanArgs {
    ScanArgs {
        path: Some(path.into()),
        ..ScanArgs::default()
    }
}

fn json_args(path: impl Into<PathBuf>) -> ScanArgs {
    ScanArgs {
        json: true,
        path: Some(path.into()),
        ..ScanArgs::default()
    }
}

fn broken_targets(payload: &serde_json::Value) -> Vec<&str> {
    payload["broken"]
        .as_array()
        .expect("broken array")
        .iter()
        .map(|binding| binding["target"].as_str().expect("broken target"))
        .collect()
}

/// Exactly the two broken bindings, with their reasons: the dropped column,
/// and the measure whose expression names a dropped column (the visual
/// inherits the breakage with the artifact named). The KPI-style `Total
/// Goal` binding resolves and flags nothing; the healthy card is untouched.
#[test]
fn the_fixture_flags_exactly_the_two_broken_bindings() {
    let args = json_args(broken_visual_pbip());
    let (code, stdout, _) = run_scan(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "breakage is advisory by default (issue #60)");
    let payload = json_payload(&stdout);
    assert_eq!(
        broken_targets(&payload),
        ["'Sales'[Color]", "'Sales'[Broken Total]"]
    );

    let color = &payload["broken"][0];
    assert_eq!(color["reason"], "field_not_found");
    assert!(color["bound_artifact"].is_null());
    assert!(
        color["provenance"]
            .as_str()
            .expect("provenance")
            .contains("field well 'Values' — visual 'V2' on page 'P1'"),
        "the site names the visual:\n{color}"
    );

    let broken_total = &payload["broken"][1];
    assert_eq!(broken_total["reason"], "bound_artifact_broken");
    assert_eq!(broken_total["bound_artifact"], "'Sales'[Broken Total]");

    // The KPI variant and the healthy card flag nothing; the summary counts
    // what is reported, and the model's objects are all live. The fixture's
    // database carries the standard Fabric metadata keys, so a zero skip
    // count also locks the whitelist end to end.
    assert_eq!(payload["summary"]["broken"], 2);
    assert_eq!(payload["summary"]["broken_total"], 2);
    assert_eq!(payload["summary"]["unused"], 0);
    assert_eq!(
        payload["skips"]["count"], 0,
        "standard export metadata must parse silently"
    );
}

/// The human section renders the field, the reason phrase, and the binding
/// site — the provenance the issue promises: page, visual, field, reason.
#[test]
fn the_human_output_names_field_reason_and_site() {
    let (_, stdout, _) = scan_path(&broken_visual_pbip(), &std::env::temp_dir());

    assert!(
        stdout.contains("Broken visual bindings (2)"),
        "section header:\n{stdout}"
    );
    assert!(
        stdout.contains("  'Sales'[Color]\n    ← field not found in the model — field well 'Values' — visual 'V2' on page 'P1'\n"),
        "the dropped-column binding:\n{stdout}"
    );
    assert!(
        stdout.contains("← bound artifact 'Sales'[Broken Total] has unresolvable references — field well 'Values' — visual 'V3' on page 'P1'\n"),
        "the inherited breakage names the artifact:\n{stdout}"
    );
    assert!(
        !stdout.contains("Total Goal"),
        "the KPI variant resolves, not flags:\n{stdout}"
    );
}

/// `--broken` scopes the run to breakage and gates on it: unused findings
/// are filter-hidden and the exit code follows the broken bindings alone —
/// the mirror image of the type flags.
#[test]
fn broken_scopes_the_run_and_gates_the_exit_code() {
    let args = ScanArgs {
        json: true,
        broken: true,
        path: Some(broken_visual_pbip()),
        ..ScanArgs::default()
    };
    let (code, stdout, _) = run_scan(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 1, "two broken bindings gate the run");
    let payload = json_payload(&stdout);
    assert_eq!(payload["summary"]["broken"], 2);
    assert_eq!(payload["summary"]["unused"], 0);

    let args = ScanArgs {
        plain: true,
        broken: true,
        path: Some(broken_visual_pbip()),
        ..ScanArgs::default()
    };
    let (_, stdout, _) = run_scan(&args, &std::env::temp_dir(), "");
    assert!(
        stdout.contains("broken_visual:field_not_found\t'Sales'[Color]\n"),
        "plain carries the broken records:\n{stdout}"
    );
    assert!(
        stdout.contains("broken_visual:bound_artifact_broken\t'Sales'[Broken Total]\n"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("\nmeasure\t"),
        "--broken hides the unused findings:\n{stdout}"
    );
}

/// In human output a lone `--broken` speaks for its scope: the bindings are
/// listed without a `No unused objects.` line stacked above them (the empty
/// findings list is the filter's doing, not a result), and a run that flags
/// nothing reads `No broken reports.` — the placeholder the flag's consumer
/// actually asked about.
#[test]
fn the_broken_only_human_output_speaks_for_its_scope() {
    let args = ScanArgs {
        broken: true,
        path: Some(broken_visual_pbip()),
        ..ScanArgs::default()
    };
    let (code, stdout, _) = run_scan(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 1, "the two broken bindings gate the run");
    assert!(
        stdout.contains("Broken visual bindings (2)"),
        "the bindings are listed:\n{stdout}"
    );
    assert!(
        !stdout.contains("No unused objects."),
        "no reachability verdict in a breakage-scoped run:\n{stdout}"
    );

    let args = ScanArgs {
        broken: true,
        path: Some(field_parameters_pbip()),
        ..ScanArgs::default()
    };
    let (code, stdout, _) = run_scan(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0, "nothing broken in the machinery fixture");
    assert!(
        stdout.contains("No broken reports."),
        "the clean placeholder names the scope:\n{stdout}"
    );
    assert!(!stdout.contains("No unused objects."), ":\n{stdout}");
}

/// The advisory default: a `--broken`-less run over the same fixture exits
/// clean because nothing is *unused* — a repo that was clean before the
/// feature must not start failing (issue #84's constraint, this side).
#[test]
fn breakage_alone_does_not_fail_the_default_exit_code() {
    let args = ScanArgs {
        path: Some(broken_visual_pbip()),
        ..ScanArgs::default()
    };
    let (code, stdout, _) = run_scan(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0);
    assert!(
        stdout.contains("Broken visual bindings (2)"),
        "the findings still show:\n{stdout}"
    );
}

/// A type flag without `--broken` hides the broken bindings — an
/// unused-only gate never sees them (issue #84's constraint, the other
/// side) — and the JSON summary keeps the totals honest.
#[test]
fn a_type_flag_hides_breakage_into_its_own_count() {
    let args = ScanArgs {
        json: true,
        measures: true,
        path: Some(broken_visual_pbip()),
        ..ScanArgs::default()
    };
    let (code, stdout, _) = run_scan(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 0);
    let payload = json_payload(&stdout);
    assert_eq!(payload["summary"]["broken"], 0);
    assert_eq!(payload["summary"]["broken_total"], 2);
    assert!(
        stdout.contains("\"broken\": []"),
        "the (present, empty) array:\n{stdout}"
    );

    // In human modes the hidden breakage is accounted for like every other
    // summary-arithmetic gap.
    let args = ScanArgs {
        measures: true,
        path: Some(broken_visual_pbip()),
        ..ScanArgs::default()
    };
    let (_, stdout, _) = run_scan(&args, &std::env::temp_dir(), "");
    assert!(
        stdout.contains("(2 broken-visual bindings hidden by type filters)"),
        "the hidden breakage is accounted for:\n{stdout}"
    );
}

/// Combining `--broken` with a type flag gates on the union, the same
/// reported-only rule the type flags obey.
#[test]
fn broken_and_a_type_flag_gate_on_their_union() {
    let args = ScanArgs {
        json: true,
        broken: true,
        measures: true,
        path: Some(broken_visual_pbip()),
        ..ScanArgs::default()
    };
    let (code, stdout, _) = run_scan(&args, &std::env::temp_dir(), "");

    assert_eq!(code, 1);
    let payload = json_payload(&stdout);
    assert_eq!(payload["summary"]["broken"], 2);
    assert_eq!(
        payload["broken"][1]["reason"], "bound_artifact_broken",
        "the broken records survive the type flag's scoping:\n{stdout}"
    );
}

/// The #52 fixtures pin the resolution side of the precision bar: field
/// parameters and auto date/time hierarchies resolve through their
/// machinery, so neither fixture may grow a broken finding now that misses
/// are reported (issue #60, detection notes).
#[test]
fn the_field_parameter_and_auto_datetime_fixtures_flag_nothing_broken() {
    for (name, path) in [
        ("field parameters", field_parameters_pbip()),
        ("auto date/time", auto_datetime_pbip()),
    ] {
        let args = json_args(path);
        let (_, stdout, _) = run_scan(&args, &std::env::temp_dir(), "");
        let payload = json_payload(&stdout);
        assert_eq!(
            payload["summary"]["broken_total"], 0,
            "{name} machinery must resolve, not flag:\n{stdout}"
        );
    }
}

/// Issue #60's precision bar, narrowed to the drift that can actually hide a
/// name: an `unknown_property` skip means the object was parsed with its
/// name regardless, so breakage stays reported. The findings still show, and
/// `--strict` still fails on the drift itself.
#[test]
fn an_unknown_property_does_not_suppress_breakage() {
    let temp = TempDir::new("broken-property-drift");
    temp.copy_tree(&broken_visual_pbip());
    // An unknown database property: real drift, recorded as a notice — but
    // it names no object and hides no name.
    let database_tmdl = temp.0.join("Broken.SemanticModel/definition/database.tmdl");
    let text = std::fs::read_to_string(&database_tmdl).expect("read database.tmdl");
    std::fs::write(&database_tmdl, format!("{text}\tfrobnicate: true\n"))
        .expect("write drifted database.tmdl");

    let args = json_args(temp.0.join("Broken.pbip"));
    let (code, stdout, _) = run_scan(&args, &temp.0, "");

    let payload = json_payload(&stdout);
    assert_eq!(code, 0, "nothing unused, breakage advisory");
    assert_eq!(payload["skips"]["count"], 1, "the drift is still recorded");
    assert_eq!(
        payload["summary"]["broken"], 2,
        "property drift does not suppress:\n{stdout}"
    );
    assert_eq!(payload["summary"]["broken_total"], 2);

    // --broken still gates on the reported breakage; --strict still fails on
    // the drift notice itself.
    let args = ScanArgs {
        json: true,
        broken: true,
        path: Some(temp.0.join("Broken.pbip")),
        ..ScanArgs::default()
    };
    let (broken_code, _, _) = run_scan(&args, &temp.0, "");
    assert_eq!(broken_code, 1, "the two broken bindings gate");

    let args = ScanArgs {
        strict: true,
        path: Some(temp.0.join("Broken.pbip")),
        ..ScanArgs::default()
    };
    let (strict_code, _, _) = run_scan(&args, &temp.0, "");
    assert_eq!(strict_code, 2, "the drift notice is still strict-fatal");
}

/// Issue #60's precision bar: a "broken" claim fires only when the model
/// ingest was clean. A model whose TMDL carries an unknown property records
/// a skip, and the same broken bindings are suppressed with the count
/// explained — while `--strict` still fails the run on the skips themselves.
#[test]
fn skips_in_the_model_ingest_suppress_breakage() {
    let temp = TempDir::new("broken-suppressed");
    temp.copy_tree(&broken_visual_pbip());
    // An unknown table property: parse drift the engine of issue #60 cannot
    // reason past, recorded as a skip notice.
    let model_tmdl = temp.0.join("Broken.SemanticModel/definition/model.tmdl");
    let text = std::fs::read_to_string(&model_tmdl).expect("read model.tmdl");
    let drifted = text.replace("ref table Sales", "ref table Sales\n\nunknownProp: true");
    std::fs::write(&model_tmdl, drifted).expect("write drifted model.tmdl");

    let args = json_args(temp.0.join("Broken.pbip"));
    let (code, stdout, _) = run_scan(&args, &temp.0, "");

    let payload = json_payload(&stdout);
    assert_eq!(payload["summary"]["broken"], 0, "nothing is claimed");
    assert_eq!(payload["summary"]["broken_total"], 2, "detection still ran");
    assert_eq!(code, 0, "suppressed breakage gates nothing");

    // Human modes explain the suppression, like every summary-arithmetic gap.
    let args = fixture_args(temp.0.join("Broken.pbip"));
    let (_, stdout, _) = run_scan(&args, &temp.0, "");
    assert!(
        stdout.contains("(2 possible broken-visual bindings suppressed — the model ingest reported skips, listed on stderr; --strict fails on those skips)"),
        "the suppression is explained:\n{stdout}"
    );

    // --strict surfaces the skips behind the suppression as the error they
    // are, independent of the breakage machinery.
    let args = ScanArgs {
        strict: true,
        path: Some(temp.0.join("Broken.pbip")),
        ..ScanArgs::default()
    };
    let (strict_code, _, _) = run_scan(&args, &temp.0, "");
    assert_eq!(strict_code, 2);
}
