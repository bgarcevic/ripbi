//! End-to-end tests of the `ripbi` and `rib` binaries themselves: the clap
//! surface (help, conflicts, exit codes) that in-process tests cannot reach.

use assert_cmd::Command;
use predicates::prelude::*;

fn ripbi() -> Command {
    Command::cargo_bin("ripbi").expect("the ripbi binary")
}

fn rib() -> Command {
    Command::cargo_bin("rib").expect("the rib alias binary")
}

fn mini_pbip() -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mini-pbip/Mini.pbip")
        .display()
        .to_string()
}

#[test]
fn help_lists_the_scan_subcommand() {
    ripbi()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("scan"));
}

#[test]
fn scan_help_leads_with_examples() {
    ripbi()
        .args(["scan", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("--report"));
}

#[test]
fn a_typo_subcommand_suggests_the_correction() {
    ripbi()
        .arg("scna")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "a similar subcommand exists: 'scan'",
        ));
}

#[test]
fn json_and_plain_flags_conflict() {
    ripbi()
        .args(["scan", &mini_pbip(), "--json", "--plain"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn summary_conflicts_with_the_finding_modes() {
    for mode in ["--json", "--plain"] {
        ripbi()
            .args(["scan", &mini_pbip(), "--summary", mode])
            .assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

#[test]
fn summary_mode_runs_the_binary_end_to_end() {
    ripbi()
        .args(["scan", &mini_pbip(), "--summary"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("Measures: 1"))
        .stdout(predicate::str::contains("Columns: 1"))
        .stdout(predicate::str::contains("'Sales'[Legacy Total]").not());
}

#[test]
fn scan_sets_the_documented_exit_codes() {
    // Unused objects found → 1.
    ripbi()
        .args(["scan", &mini_pbip(), "--plain"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("measure\t'Sales'[Legacy Total]"));

    // Usage error (no such path) → 2.
    ripbi()
        .args(["scan", "definitely/not/here.pbip"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("no such path"));
}

#[test]
fn bare_path_argument_no_longer_works_without_a_subcommand() {
    ripbi()
        .arg(mini_pbip())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn the_rib_alias_is_the_same_tool() {
    rib()
        .args(["scan", &mini_pbip(), "--plain"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("measure\t'Sales'[Legacy Total]"));
}

#[test]
fn rib_help_says_rib() {
    // clap derives the displayed name from argv[0], so the alias must never
    // print "Usage: ripbi".
    rib()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: rib"));
}
