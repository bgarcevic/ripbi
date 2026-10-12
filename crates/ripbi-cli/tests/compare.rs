//! Integration tests for `scan --compare-root` (issue #141): the same scan
//! reruns in another checkout, only findings that did not exist there are
//! reported and gate the exit code, and findings gone since are listed as
//! fixed.

// Uses only part of `common`; `expect` (not `allow`) fails this build if
// that stops being true. Contract: common/mod.rs.
#[expect(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use ripbi_cli::cli::ScanArgs;

use common::{TempDir, auto_datetime_pbip, broken_visual_pbip, json_payload, mini_pbip, run_scan};

/// Two checkouts of one fixture: `base/` (the comparison) and `head/` (the
/// change under test). Scans run in `head/` by discovery, exactly like CI
/// running `rib scan --compare-root ../base` from the repository root.
struct Checkouts {
    temp: TempDir,
}

impl Checkouts {
    fn of(name: &str, fixture: &Path) -> Self {
        let temp = TempDir::new(name);
        for side in ["base", "head"] {
            let dir = temp.mkdir(side);
            copy(fixture, &dir);
        }
        Self { temp }
    }

    fn base(&self) -> PathBuf {
        self.temp.0.join("base")
    }

    fn head(&self) -> PathBuf {
        self.temp.0.join("head")
    }

    /// Rewrites one file in `side`, replacing `from` with `to` exactly once.
    fn edit(&self, side: &str, relative: &str, from: &str, to: &str) {
        let path = self.temp.0.join(side).join(relative);
        let text = fs::read_to_string(&path).expect("read fixture file");
        assert_eq!(text.matches(from).count(), 1, "{from:?} in {relative}");
        fs::write(&path, text.replace(from, to)).expect("write fixture file");
    }

    fn scan(&self, extra: ScanArgs) -> (i32, String, String) {
        let args = ScanArgs {
            compare_root: Some(self.base()),
            ..extra
        };
        run_scan(&args, &self.head(), "")
    }
}

fn copy(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("read fixture") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            fs::create_dir_all(&target).expect("mkdir");
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

const SALES: &str = "Mini.SemanticModel/definition/tables/Sales.tmdl";
const VISUAL: &str = "Mini.Report/definition/pages/P1/visuals/V1/visual.json";

/// A measure no visual binds and no expression names.
const DEAD_MEASURE: &str = "\tmeasure 'Draft KPI' = 1\n\t\tlineageTag: 99999999-9999-9999-9999-999999999990\n\n\tcolumn Amount";

#[test]
fn identical_checkouts_report_nothing_new() {
    let checkouts = Checkouts::of("compare-same", &mini_pbip());
    let (code, stdout, stderr) = checkouts.scan(ScanArgs::default());

    assert_eq!(code, 0, "nothing new:\n{stdout}\n{stderr}");
    assert!(stdout.contains(", 0 unused"), "{stdout}");
    assert!(stdout.contains("(2 findings already in"), "{stdout}");
    assert!(stdout.contains("No unused objects."), "{stdout}");
    assert!(!stdout.contains("Fixed since"), "{stdout}");
}

#[test]
fn a_new_dead_measure_is_the_only_finding_and_gates() {
    let checkouts = Checkouts::of("compare-new", &mini_pbip());
    checkouts.edit("head", SALES, "\tcolumn Amount", DEAD_MEASURE);

    let (code, stdout, stderr) = checkouts.scan(ScanArgs::default());
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains(", 1 unused"), "{stdout}");
    assert!(stdout.contains("'Sales'[Draft KPI]"), "{stdout}");
    assert!(!stdout.contains("'Sales'[Legacy Total]"), "{stdout}");
    assert!(
        stderr.contains("hint: Remove the new unused objects") && stderr.contains("ripbi_keep"),
        "the gate points at the annotation:\n{stderr}"
    );
}

/// The keep hint belongs to a failing comparison only: not to a clean one,
/// and not to a plain scan whose findings are not "new".
#[test]
fn the_keep_hint_needs_new_unused_findings() {
    let checkouts = Checkouts::of("compare-hint", &mini_pbip());
    let (code, _, stderr) = checkouts.scan(ScanArgs::default());
    assert_eq!(code, 0);
    assert!(!stderr.contains("ripbi_keep"), "{stderr}");

    let (code, _, stderr) = run_scan(&ScanArgs::default(), &checkouts.head(), "");
    assert_eq!(code, 1);
    assert!(!stderr.contains("ripbi_keep"), "{stderr}");
}

/// Issue #151: the same new measure, annotated as deliberately kept, is a
/// root rather than a finding — the PR passes the gate.
#[test]
fn a_new_measure_kept_by_annotation_passes_the_gate() {
    let checkouts = Checkouts::of("compare-kept", &mini_pbip());
    let kept = DEAD_MEASURE.replace(
        "\n\n\tcolumn Amount",
        "\n\t\tannotation ripbi_keep = Used by the Finance Excel pivot\n\n\tcolumn Amount",
    );
    checkouts.edit("head", SALES, "\tcolumn Amount", &kept);

    let (code, stdout, _) = checkouts.scan(ScanArgs::default());
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains(", 0 unused"), "{stdout}");
    assert!(!stdout.contains("Draft KPI"), "{stdout}");
}

/// Report changes count too: rebinding the card from `Total` to `Legacy
/// Total` kills `Total` and `Amount` (new) and revives the legacy pair (fixed).
#[test]
fn report_changes_are_compared_across_both_sides() {
    let checkouts = Checkouts::of("compare-report", &mini_pbip());
    checkouts.edit(
        "head",
        VISUAL,
        "\"Property\": \"Total\"",
        "\"Property\": \"Legacy Total\"",
    );

    let (code, stdout, _) = checkouts.scan(ScanArgs::default());
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("'Sales'[Total]"), "{stdout}");
    assert!(stdout.contains("'Sales'[Amount]"), "{stdout}");
    assert!(stdout.contains("Fixed since"), "{stdout}");
    assert!(
        stdout.contains("'Sales'[Legacy Total]  measure"),
        "{stdout}"
    );
    assert!(stdout.contains("'Sales'[Legacy]  column"), "{stdout}");
}

#[test]
fn findings_removed_since_the_base_are_listed_as_fixed() {
    let checkouts = Checkouts::of("compare-fixed", &mini_pbip());
    checkouts.edit("base", SALES, "\tcolumn Amount", DEAD_MEASURE);

    let (code, stdout, _) = checkouts.scan(ScanArgs::default());
    assert_eq!(code, 0, "a fixed finding never fails the run:\n{stdout}");
    assert!(stdout.contains("Fixed since"), "{stdout}");
    assert!(stdout.contains("'Sales'[Draft KPI]  measure"), "{stdout}");

    let (_, plain, _) = checkouts.scan(ScanArgs {
        plain: true,
        ..ScanArgs::default()
    });
    assert_eq!(plain, "fixed:measure\t'Sales'[Draft KPI]\n");

    let (_, summary, _) = checkouts.scan(ScanArgs {
        summary: true,
        ..ScanArgs::default()
    });
    assert!(summary.contains(": 1\n"), "{summary}");
    assert!(summary.contains("Fixed since"), "{summary}");
}

/// A finding hidden by type selection is still detected: it must not read as
/// fixed just because this run did not report it.
#[test]
fn filtered_findings_are_not_fixed() {
    let checkouts = Checkouts::of("compare-filtered", &mini_pbip());
    let (code, stdout, _) = checkouts.scan(ScanArgs {
        types: vec!["measure".to_string()],
        ..ScanArgs::default()
    });
    assert_eq!(code, 0, "{stdout}");
    assert!(!stdout.contains("Fixed since"), "{stdout}");
}

#[test]
fn json_carries_the_comparison_only_when_asked() {
    let checkouts = Checkouts::of("compare-json", &mini_pbip());
    checkouts.edit("head", SALES, "\tcolumn Amount", DEAD_MEASURE);
    let json = || ScanArgs {
        json: true,
        ..ScanArgs::default()
    };

    let (code, stdout, _) = checkouts.scan(json());
    assert_eq!(code, 1);
    let payload = json_payload(&stdout);
    assert_eq!(payload["summary"]["unused"], 1);
    assert_eq!(payload["unused"][0]["id"], "'Sales'[Draft KPI]");
    assert_eq!(payload["compare"]["existing"], 2);
    assert!(
        payload["compare"]["fixed"]
            .as_array()
            .expect("fixed")
            .is_empty()
    );

    let (_, stdout, _) = run_scan(&json(), &checkouts.head(), "");
    assert!(
        json_payload(&stdout).get("compare").is_none(),
        "no --compare-root, no field: other scans' JSON is unchanged"
    );
}

#[test]
fn a_missing_compare_root_is_an_error_with_a_hint() {
    let checkouts = Checkouts::of("compare-missing", &mini_pbip());
    let args = ScanArgs {
        compare_root: Some(checkouts.temp.0.join("nope")),
        ..ScanArgs::default()
    };
    let (code, _, stderr) = run_scan(&args, &checkouts.head(), "");
    assert_eq!(code, 2);
    assert!(stderr.contains("is not a folder"), "{stderr}");
    assert!(
        stderr.contains("hint: check out the base revision"),
        "{stderr}"
    );
}

/// A model this change adds has nothing to compare against: a note, and every
/// finding is new.
#[test]
fn a_base_without_the_model_makes_every_finding_new() {
    let checkouts = Checkouts::of("compare-empty", &mini_pbip());
    fs::remove_dir_all(checkouts.base()).expect("clear base");
    fs::create_dir_all(checkouts.base()).expect("empty base");

    let (code, stdout, stderr) = checkouts.scan(ScanArgs::default());
    assert_eq!(code, 1, "{stdout}");
    assert!(stderr.contains("Note: nothing to compare in"), "{stderr}");
    assert!(stdout.contains(", 2 unused"), "{stdout}");
    assert!(stdout.contains("(0 findings already in"), "{stdout}");
}

#[test]
fn unchanged_breakage_does_not_gate_under_broken() {
    let checkouts = Checkouts::of("compare-broken", &broken_visual_pbip());
    let broken = || ScanArgs {
        broken: true,
        ..ScanArgs::default()
    };

    let (code, stdout, _) = run_scan(&broken(), &checkouts.head(), "");
    assert_eq!(code, 1, "the fixture has breakage:\n{stdout}");

    let (code, stdout, _) = checkouts.scan(broken());
    assert_eq!(
        code, 0,
        "breakage the base already had no longer gates:\n{stdout}"
    );
}

/// Auto date/time rows compare per table; a dead table's own finding goes
/// with its row instead of resurfacing as a generic table finding.
#[test]
fn unchanged_auto_date_time_rows_do_not_gate() {
    let checkouts = Checkouts::of("compare-adt", &auto_datetime_pbip());
    let (code, stdout, _) = checkouts.scan(ScanArgs {
        json: true,
        ..ScanArgs::default()
    });
    assert_eq!(code, 0, "{stdout}");
    let payload = json_payload(&stdout);
    assert!(
        payload["unused"].as_array().expect("unused").is_empty(),
        "{stdout}"
    );
    assert!(
        payload["auto_date_time"]
            .as_array()
            .expect("rows")
            .iter()
            .all(|row| row["verdict"] == "in_use"),
        "only informational rows remain:\n{stdout}"
    );
}

/// The hand-edit that first exposed this: a measure indented with spaces. It
/// must cost nothing else in its table, so the comparison reports exactly the
/// new measure — never the rest of the table as "fixed".
#[test]
fn a_space_indented_measure_is_new_and_breaks_nothing_else() {
    let checkouts = Checkouts::of("compare-spaces", &mini_pbip());
    checkouts.edit(
        "head",
        SALES,
        "\tcolumn Amount",
        "    measure 'Try Me' = 1\n\n\tcolumn Amount",
    );

    let (code, stdout, stderr) = checkouts.scan(ScanArgs::default());
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains(", 1 unused"), "{stdout}");
    assert!(stdout.contains("'Sales'[Try Me]"), "{stdout}");
    assert!(!stdout.contains("Fixed since"), "{stdout}");
    assert!(stderr.contains("indented with spaces"), "{stderr}");
}

/// A skip that drops an object makes every "fixed" claim suspect: the list
/// carries a caveat, and JSON says so.
#[test]
fn fixed_findings_carry_a_caveat_when_the_parse_dropped_objects() {
    let checkouts = Checkouts::of("compare-damage", &mini_pbip());
    checkouts.edit("base", SALES, "\tcolumn Amount", DEAD_MEASURE);
    checkouts.edit(
        "head",
        SALES,
        "\tcolumn Amount",
        "    isHidden\n\tcolumn Amount",
    );

    let (code, stdout, stderr) = checkouts.scan(ScanArgs::default());
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("'Sales'[Draft KPI]  measure"), "{stdout}");
    assert!(stdout.contains("may be parse damage"), "{stdout}");
    assert!(stderr.contains("could not be placed"), "{stderr}");

    let (_, json, _) = checkouts.scan(ScanArgs {
        json: true,
        ..ScanArgs::default()
    });
    assert_eq!(json_payload(&json)["compare"]["fixed_uncertain"], true);

    let (_, clean, _) = Checkouts::of("compare-damage-clean", &mini_pbip()).scan(ScanArgs {
        json: true,
        ..ScanArgs::default()
    });
    assert_eq!(json_payload(&clean)["compare"]["fixed_uncertain"], false);
}
